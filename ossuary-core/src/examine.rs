//! Running extractors over what they have not examined — the orchestrator.
//!
//! The extractor is its own program, found on PATH and spoken to over
//! pipes — the protocol is `docs/extractors.md`. This side owns the
//! archive: it asks the extractor who it is, folds the log for the
//! worklist, hands each file's bytes over — and, for an extractor that
//! derives files, a fresh directory to write them into — and funnels
//! what comes back through the claim grammar before anything is
//! recorded. A file whose answer does not parse, or whose announced
//! files do not add up, fails whole — nothing half-recorded, no
//! receipt, offered again next run. The directory is the orchestrator's
//! to make and to sweep, announced files and workspace leavings alike.
//!
//! One program may carry several contracts — separately named,
//! separately sourced, separately receipted capabilities, announced as
//! one identify line each. Downstream nothing changes: the worklist and
//! the record see only sources. `extract NAME` runs every contract the
//! program announces; `extract NAME:CONTRACT` runs one.
//!
//! The orchestration speaks facts, not lines: what it does and finds
//! along the way goes to an [`Observer`] as [`Event`]s, and what the
//! whole call came to is the [`Settlement`] it returns. Which of it is
//! said, where, and how loudly is the observer's business alone.

use std::collections::HashSet;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::archive::Archive;
use crate::claim::{Attribute, Source, Subject, Value};
use crate::error::{Error, Result};
use crate::extract::{Derivation, Examined, record_examination, run_id};
use crate::index::Index;

/// One thing the orchestration did or found, told as it happens. The
/// observer decides what becomes of it; the variants only say what is.
#[derive(Debug)]
pub enum Event<'a> {
    /// The index caught up on sealed segments it had not seen.
    CaughtUp {
        /// How many segments the fold brought in.
        segments: usize,
    },
    /// A listed entry did not identify; the rest of the list still runs.
    Skipped {
        /// The entry as the archive's list spelled it.
        listed: &'a str,
        /// Why it sat out.
        trouble: &'a Error,
    },
    /// The first round found nothing for this source — the calm answer.
    /// Later rounds finding nothing are only the fixpoint being reached
    /// and go unsaid.
    Idle {
        source: &'a Source,
        /// Whether receipts were being ignored: "nothing of this kind at
        /// all" against "nothing of this kind unexamined".
        full: bool,
    },
    /// A pass over the worklist begins.
    Waiting {
        source: &'a Source,
        count: usize,
        /// 1 on the first round; later rounds are the news that a feeder
        /// produced fresh work.
        round: usize,
    },
    /// A pass over named files begins.
    Named { source: &'a Source, count: usize },
    /// A pass ended: the verdict's material, what failed included.
    Verdict {
        source: &'a Source,
        tally: &'a Tally,
        /// Named files that sat out because their receipt already stands.
        already: usize,
        /// What could not be examined — no receipt, offered again.
        failures: &'a [(Subject, Error)],
    },
}

/// Where the orchestration tells its [`Event`]s. Any `FnMut(Event)`
/// serves; the trait exists so the plug has a name.
pub trait Observer {
    /// One event, as it happens.
    fn event(&mut self, event: Event<'_>);
}

impl<F: FnMut(Event<'_>)> Observer for F {
    fn event(&mut self, event: Event<'_>) {
        self(event);
    }
}

/// What one call to [`examine`] came to, across all its rounds.
#[derive(Debug)]
pub struct Settlement {
    /// No pass failed a file and no listed entry was skipped.
    pub clean: bool,
    /// Contracts that ran.
    pub ran: usize,
    /// Working rounds — the round that only confirmed nothing was left
    /// is not counted, and named files are one pass, never rounds.
    pub rounds: usize,
    /// Files examined, across all rounds.
    pub examinations: usize,
}

/// One line of the answer to `--identify` — one contract. Unknown keys
/// are tolerated on purpose: the protocol number is the gate, and it is
/// checked before anything else is believed.
#[derive(Deserialize)]
struct Identity {
    #[serde(rename = "ossuary-extractor")]
    protocol: u32,
    /// The contract's name, when the program carries several — passed
    /// back as the first argument so the program knows which of its
    /// trades is meant. A single-line answer may leave it out.
    #[serde(default)]
    contract: Option<String>,
    source: String,
    mimes: Vec<String>,
    #[serde(default)]
    derives: bool,
}

/// One answer line, in any of its three shapes — a finding about the
/// examined file, the announcement of a derived file, a finding about
/// one. Strict: protocol 1 says these fields, and a misspelled key must
/// not pass as an empty answer.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Line {
    file: Option<String>,
    mime: Option<String>,
    attribute: Option<String>,
    value: Option<Value>,
}

/// Run extractors until everything they read is examined.
///
/// `name` is one extractor — `NAME`, or `NAME:CONTRACT` for one contract
/// of it — and it answers `--identify` or the call is an error. Left
/// out, the archive's own list under `[extract]` runs, and there a
/// broken entry is only [`Event::Skipped`] — a nightly sweep does not
/// forfeit exif to a missing poppler.
///
/// With no `subjects`, rounds over the worklist until a full round
/// examines nothing — the fixpoint; named `subjects` are the surgical
/// grip: resolved the way `about` resolves a name, one pass, no rounds.
/// `full` ignores standing receipts; `temp_dir` is where a deriving
/// extractor's files wait, `cache/tmp` in the archive when unnamed.
///
/// # Errors
///
/// Naming trouble — [`Error::Unknown`], [`Error::Ambiguous`] — and
/// [`Error::Extract`] for a named extractor that will not identify, a
/// loop that never runs dry, or `temp_dir` handed to a run that derives
/// nothing. A file the extractor fails is not an error here: it stands
/// in [`Event::Verdict`] and costs the settlement its `clean`.
pub fn examine(
    archive: &Archive,
    name: Option<&str>,
    subjects: &[String],
    full: bool,
    temp_dir: Option<&Path>,
    observer: &mut dyn Observer,
) -> Result<Settlement> {
    let names: Vec<String> = match name {
        Some(name) => vec![name.to_string()],
        None => archive.config().extractors().to_vec(),
    };
    if names.is_empty() {
        return Err(Error::Extract(
            "no extractors to run — name one, like `ossuary extract text`, or list this archive's own under [extract] in config.toml".to_string(),
        ));
    }

    // Everyone answers --identify before anyone runs. A single named
    // extractor that cannot is this run's error; in the archive's own
    // list, the broken one is reported and the healthy ones still run.
    let mut runs = Vec::new();
    let mut skipped = 0usize;
    for listed in &names {
        match prepare(listed) {
            Ok(mut prepared) => runs.append(&mut prepared),
            Err(error) => {
                if name.is_some() {
                    return Err(error);
                }
                skipped += 1;
                observer.event(Event::Skipped {
                    listed,
                    trouble: &error,
                });
            }
        }
    }
    if temp_dir.is_some() && !runs.is_empty() && !runs.iter().any(|run| run.identity.derives) {
        return Err(Error::Extract(match runs.as_slice() {
            [only] => match &only.identity.contract {
                Some(contract) => format!(
                    "`{}` hands no files back under its {contract} contract — --temp-dir is where derived files would wait; run this without it",
                    only.program
                ),
                None => format!(
                    "`{}` hands no files back — --temp-dir is where derived files would wait; run this without it",
                    only.program
                ),
            },
            _ => "no files would come back from this run — --temp-dir is where derived files wait; run this without it".to_string(),
        }));
    }

    let mut index = archive.index()?;
    let mut troubled = skipped > 0;
    let mut invocation = Invocation {
        archive,
        run_id: run_id(),
        full,
        temp_dir,
        observer,
        examined: HashSet::new(),
    };

    let mut rounds = 1;
    let mut examinations = 0;
    if subjects.is_empty() {
        let settled = settle(&mut invocation, &mut index, &runs)?;
        troubled |= settled.troubled;
        rounds = settled.rounds;
        examinations = settled.examinations;
    } else {
        // Named files are the surgical grip: one pass, no rounds.
        for run in &runs {
            catch_up(&mut index, invocation.archive, invocation.observer)?;
            let outcome = run_one(&mut invocation, &index, run, subjects, 1)?;
            troubled |= !outcome.clean;
            examinations += outcome.examined;
        }
    }
    Ok(Settlement {
        clean: !troubled,
        ran: runs.len(),
        rounds,
        examinations,
    })
}

/// How the fixpoint loop went, for the settlement.
struct Settled {
    troubled: bool,
    rounds: usize,
    examinations: usize,
}

/// Rounds over the list until a full round examines nothing — the
/// fixpoint. The "queue" is the worklist itself, refolded from log and
/// receipts before every extractor: Ctrl-C in round three loses
/// nothing, the next call carries on where the receipts end. List
/// order is only a performance hint — an extractor listed before its
/// feeder costs one more round, never a second command.
fn settle(invocation: &mut Invocation, index: &mut Index, runs: &[Run]) -> Result<Settled> {
    let mut troubled = false;
    let mut round = 0usize;
    let mut total = 0usize;
    loop {
        round += 1;
        let mut examined = 0usize;
        let mut busy: Vec<String> = Vec::new();
        for run in runs {
            catch_up(index, invocation.archive, invocation.observer)?;
            let outcome = run_one(invocation, index, run, &[], round)?;
            troubled |= !outcome.clean;
            if outcome.examined > 0 {
                examined += outcome.examined;
                busy.push(run.source.to_string());
            }
        }
        if examined == 0 {
            break;
        }
        total += examined;
        if round == MAX_ROUNDS {
            return Err(Error::Extract(format!(
                "round {MAX_ROUNDS} still examined files — {} never runs dry; an extractor whose output bytes differ every round cannot settle. What is recorded so far stands; fix the extractor, then run this again",
                busy.join(", ")
            )));
        }
    }
    // The last round is the measuring stick — it only confirmed there
    // was nothing left — so it is not counted as work.
    Ok(Settled {
        troubled,
        rounds: round - 1,
        examinations: total,
    })
}

/// The lid on the fixpoint loop. Honest extractors settle in a handful
/// of rounds — content nests mails-in-mails deep, not thirty-two deep —
/// so a loop still busy here is an extractor minting fresh bytes every
/// round, and the verdict names it. A constant, not a knob, until a
/// real archive proves the need.
const MAX_ROUNDS: usize = 32;

/// Fold the log in and, when that was real work, say so.
fn catch_up(index: &mut Index, archive: &Archive, observer: &mut dyn Observer) -> Result<()> {
    let folded = index.fold(archive.log())?;
    if folded.segments > 0 {
        observer.event(Event::CaughtUp {
            segments: folded.segments,
        });
    }
    Ok(())
}

/// One extractor's contract, identified and ready to run.
struct Run {
    program: String,
    identity: Identity,
    source: Source,
}

/// A listed entry, taken at its word: `NAME` is the program — every
/// contract it announces runs — and `NAME:CONTRACT` is one contract of
/// it. The same spelling serves the command line and the archive's own
/// list, so "inventory the archives, never unpack them" can stand in
/// config.toml as `"packed:list"`.
fn parse_listed(listed: &str) -> Result<(&str, Option<&str>)> {
    let (name, contract) = match listed.split_once(':') {
        Some((name, contract)) => (name, Some(contract)),
        None => (listed, None),
    };
    if !well_formed(name) || contract.is_some_and(|contract| !well_formed(contract)) {
        return Err(Error::Extract(format!(
            "{listed:?} does not name an extractor — the form is NAME or NAME:CONTRACT, lowercase letters, digits and dashes"
        )));
    }
    Ok((name, contract))
}

/// The grammar an extractor's name and a contract's name share: what
/// fits after `ossuary-extract-` fits behind the colon.
fn well_formed(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// One listed entry into its runs: the program identified, its
/// contracts selected — all of them, or the one named after the colon.
fn prepare(listed: &str) -> Result<Vec<Run>> {
    let (name, wanted) = parse_listed(listed)?;
    let program = format!("ossuary-extract-{name}");
    let identities = identify(&program)?;
    let selected = match wanted {
        None => identities,
        Some(wanted) => {
            let offered: Vec<String> = identities
                .iter()
                .filter_map(|identity| identity.contract.clone())
                .collect();
            match identities
                .into_iter()
                .find(|identity| identity.contract.as_deref() == Some(wanted))
            {
                Some(identity) => vec![identity],
                None if offered.is_empty() => {
                    return Err(Error::Extract(format!(
                        "`{program}` names no contracts — run it whole, as `ossuary extract {name}`"
                    )));
                }
                None => {
                    return Err(Error::Extract(format!(
                        "`{program}` offers no contract named {wanted:?} — it offers {}",
                        offered.join(", ")
                    )));
                }
            }
        }
    };
    selected
        .into_iter()
        .map(|identity| {
            Ok(Run {
                program: program.clone(),
                source: Source::parse(&identity.source)?,
                identity,
            })
        })
        .collect()
}

/// What one [`examine`] call carries through all its rounds.
struct Invocation<'a> {
    archive: &'a Archive,
    /// The run anchor: stamped as `prov:run` on every derived file this
    /// call takes in, rounds included — they are the call's insides.
    run_id: String,
    full: bool,
    temp_dir: Option<&'a Path>,
    observer: &'a mut dyn Observer,
    /// Who examined what within THIS call. Under --full, receipts from
    /// before the call are ignored and this memo stands in for them —
    /// otherwise every round would offer everything anew and the loop
    /// could never settle. A private, disposable note of the call's own
    /// writes: a crash loses it and costs at most a few double
    /// examinations, which dedup in the set.
    examined: HashSet<String>,
}

/// How one extractor's pass went: whether it ran clean, and how many
/// files it examined — the loop's fixpoint question.
struct Outcome {
    clean: bool,
    examined: usize,
}

/// The memo's spelling of "this source examined this file".
fn memo_key(subject: &Subject, source: &Source) -> String {
    format!("{}|{}", source.as_str(), subject.as_str())
}

/// One extractor over its worklist — or over the named files. Answers
/// whether the pass went clean and how many files it examined; what
/// failed stands in the pass's [`Event::Verdict`].
///
/// `round` is 1 on the first round and for named files. There an empty
/// worklist is an answer worth telling; in later rounds it is only the
/// fixpoint being reached, and saying so per extractor per round would
/// bury the run's real news. Work in a later round names its round
/// instead — that is what says why an extractor that just had nothing
/// suddenly has something.
fn run_one(
    invocation: &mut Invocation,
    index: &Index,
    run: &Run,
    subjects: &[String],
    round: usize,
) -> Result<Outcome> {
    let Run {
        program,
        identity,
        source,
    } = run;
    let archive = invocation.archive;
    let scratch = identity
        .derives
        .then(|| scratch_parent(archive, invocation.temp_dir))
        .transpose()?;

    // What this pass examines: the worklist, unless files were named.
    // Named files skip the mime dispatch — naming is more deliberate
    // than a pattern — and, without --full, sit out when their receipt
    // already stands. Under --full the receipts of earlier calls are
    // ignored and the invocation's own memo takes their place.
    let mut already = 0usize;
    let worklist: Vec<Subject> = if subjects.is_empty() {
        let waiting: Vec<Subject> = if invocation.full {
            index
                .of_kind(&identity.mimes)?
                .into_iter()
                .filter(|subject| !invocation.examined.contains(&memo_key(subject, source)))
                .collect()
        } else {
            index.worklist(&identity.mimes, source)?
        };
        if waiting.is_empty() {
            if round == 1 {
                invocation.observer.event(Event::Idle {
                    source,
                    full: invocation.full,
                });
            }
            return Ok(Outcome {
                clean: true,
                examined: 0,
            });
        }
        invocation.observer.event(Event::Waiting {
            source,
            count: waiting.len(),
            round,
        });
        waiting
    } else {
        let examinees;
        (examinees, already) = named(index, subjects, source, invocation.full)?;
        if !examinees.is_empty() {
            invocation.observer.event(Event::Named {
                source,
                count: examinees.len(),
            });
        }
        examinees
    };

    let mut tally = Tally::default();
    let mut failures: Vec<(Subject, Error)> = Vec::new();
    for subject in worklist {
        match examine_one(
            archive,
            program,
            identity.contract.as_deref(),
            &subject,
            source,
            &invocation.run_id,
            scratch.as_deref(),
        ) {
            Ok(written) => {
                tally.add(&written);
                invocation.examined.insert(memo_key(&subject, source));
            }
            Err(error) => failures.push((subject, error)),
        }
    }

    invocation.observer.event(Event::Verdict {
        source,
        tally: &tally,
        already,
        failures: &failures,
    });
    Ok(Outcome {
        clean: failures.is_empty(),
        examined: tally.examined,
    })
}

/// What the pass did, counted for the verdict.
#[derive(Debug, Default)]
pub struct Tally {
    /// Files examined.
    pub examined: usize,
    /// Claims written, receipts included.
    pub claims: usize,
    /// Derived files whose bytes were new to the derived store.
    pub stored: usize,
    /// Derived files whose bytes the archive already held.
    pub known: usize,
    /// Examinations whose whole harvest was the receipt.
    pub nothing: usize,
}

impl Tally {
    fn add(&mut self, written: &Examined) {
        self.examined += 1;
        self.claims += written.claims;
        self.stored += written.stored;
        self.known += written.known;
        if written.claims == 1 {
            self.nothing += 1;
        }
    }
}

/// The files a pass that was handed names examines: resolved the way
/// `about` resolves a name, deduplicated, and — without `--full` — minus
/// those whose receipt from this source already stands. Answers the
/// examinees and how many sat out; an unknown name refuses the whole run
/// before any work is done.
fn named(
    index: &Index,
    subjects: &[String],
    source: &Source,
    full: bool,
) -> Result<(Vec<Subject>, usize)> {
    let mut examinees: Vec<Subject> = Vec::new();
    let mut already = 0usize;
    for given in subjects {
        let Some(subject) = index.resolve(given)? else {
            return Err(Error::Unknown(given.clone()));
        };
        if examinees.contains(&subject) {
            continue;
        }
        if !full && index.examined(&subject, source)? {
            already += 1;
            continue;
        }
        examinees.push(subject);
    }
    Ok((examinees, already))
}

/// Who is this extractor, and what does it read? One line per
/// contract: a single line may leave the contract unnamed — the
/// program of one trade, spoken to as it always was — while several
/// lines must each name theirs, no name twice.
fn identify(program: &str) -> Result<Vec<Identity>> {
    let output = Command::new(program)
        .arg("--identify")
        .output()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::Extract(format!(
                    "no `{program}` on PATH — an extractor is its own program; install it, then run this again"
                ))
            } else {
                Error::Io {
                    context: format!("running `{program} --identify`"),
                    source: error,
                }
            }
        })?;
    if !output.status.success() {
        // The extractor's stderr is its own account of what is wrong —
        // "no pdftotext on PATH" beats "it failed".
        let complaint = String::from_utf8_lossy(&output.stderr);
        let complaint = complaint.trim();
        if !complaint.is_empty() {
            return Err(Error::Extract(format!(
                "`{program} --identify` failed: {complaint}"
            )));
        }
        return Err(Error::Extract(format!(
            "`{program} --identify` failed — an extractor answers it before anything is examined"
        )));
    }
    read_identities(program, &String::from_utf8_lossy(&output.stdout))
}

/// The identify answer read whole and held to its rules — apart from
/// running the program, the whole of `identify`.
fn read_identities(program: &str, answer: &str) -> Result<Vec<Identity>> {
    let mut identities = Vec::new();
    for line in answer.lines().filter(|line| !line.trim().is_empty()) {
        let identity: Identity = serde_json::from_str(line.trim()).map_err(|error| {
            Error::Extract(format!(
                "`{program} --identify` did not answer in the extractor protocol: {error}"
            ))
        })?;
        if identity.protocol != 1 {
            return Err(Error::Extract(format!(
                "`{program}` speaks extractor protocol {}, this build speaks 1 — upgrade ossuary",
                identity.protocol
            )));
        }
        if let Some(contract) = &identity.contract
            && !well_formed(contract)
        {
            return Err(Error::Extract(format!(
                "`{program}` announces a contract named {contract:?} — a contract's name is lowercase letters, digits and dashes"
            )));
        }
        identities.push(identity);
    }
    if identities.is_empty() {
        return Err(Error::Extract(format!(
            "`{program} --identify` answered nothing — an extractor announces itself in at least one line"
        )));
    }
    if identities.len() > 1 {
        let mut seen = HashSet::new();
        for identity in &identities {
            let Some(contract) = &identity.contract else {
                return Err(Error::Extract(format!(
                    "`{program}` announces {} contracts and one line names none — with several, every line says which it is",
                    identities.len()
                )));
            };
            if !seen.insert(contract.as_str()) {
                return Err(Error::Extract(format!(
                    "`{program}` announces the contract {contract:?} twice"
                )));
            }
        }
    }
    Ok(identities)
}

/// Where the per-file scratch directories go: what `temp_dir` named, or
/// `cache/tmp` in the archive — the disposable tier, on a disk sized for
/// content, where a system /tmp may be a small ramdisk.
fn scratch_parent(archive: &Archive, temp_dir: Option<&Path>) -> Result<PathBuf> {
    let parent = match temp_dir {
        Some(dir) => dir.to_path_buf(),
        None => archive.root().join("cache").join("tmp"),
    };
    std::fs::create_dir_all(&parent).map_err(|error| Error::Io {
        context: format!("{}: creating the place for derived files", parent.display()),
        source: error,
    })?;
    // The extractor takes the path as its argument; handed over absolute,
    // it holds whatever the extractor's working directory turns out to be.
    std::path::absolute(&parent).map_err(|error| Error::Io {
        context: format!(
            "{}: resolving the place for derived files",
            parent.display()
        ),
        source: error,
    })
}

/// One file through the extractor and onto the record. A contract that
/// was announced by name is named back: first argument the contract,
/// then — when this contract derives — the directory.
fn examine_one(
    archive: &Archive,
    program: &str,
    contract: Option<&str>,
    subject: &Subject,
    source: &Source,
    run_id: &str,
    scratch_parent: Option<&Path>,
) -> Result<Examined> {
    let content = archive.content();
    // The examinee may be an original or itself derived — the PDF out of
    // a mail: content/ is asked first, derived/ second.
    let (store, digest) = match content.matching(subject.as_str())?.as_slice() {
        [one] => (content, one.clone()),
        _ => match archive.derived().matching(subject.as_str())?.as_slice() {
            [one] => (archive.derived(), one.clone()),
            _ => {
                return Err(Error::Extract(
                    "not held — the log speaks of it, neither store holds its bytes".to_string(),
                ));
            }
        },
    };
    let mut bytes = Vec::new();
    store
        .reader(&digest)?
        .ok_or_else(|| Error::Extract("gone between naming and reading".to_string()))?
        .read_to_end(&mut bytes)
        .map_err(|error| Error::Io {
            context: "reading from the store".to_string(),
            source: error,
        })?;

    // A fresh directory per file: names cannot collide across files, and
    // dropping it sweeps everything — announced files once they are taken
    // in, workspace leavings and half-written attempts always.
    let scratch = scratch_parent
        .map(|parent| {
            tempfile::TempDir::new_in(parent).map_err(|error| Error::Io {
                context: format!("{}: making a directory for derived files", parent.display()),
                source: error,
            })
        })
        .transpose()?;
    let mut command = Command::new(program);
    if let Some(contract) = contract {
        command.arg(contract);
    }
    if let Some(scratch) = &scratch {
        command.arg(scratch.path());
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| Error::Io {
            context: format!("running `{program}`"),
            source: error,
        })?;
    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(&bytes)
        .map_err(|error| Error::Io {
            context: "handing the bytes over".to_string(),
            source: error,
        })?;
    let mut answer = String::new();
    child
        .stdout
        .take()
        .expect("stdout was piped")
        .read_to_string(&mut answer)
        .map_err(|error| Error::Io {
            context: "reading the findings".to_string(),
            source: error,
        })?;
    let status = child.wait().map_err(|error| Error::Io {
        context: "waiting for the extractor".to_string(),
        source: error,
    })?;
    if !status.success() {
        return Err(Error::Extract(format!(
            "`{program}` gave up on it ({status})"
        )));
    }

    let Harvest { findings, derived } = harvest(
        program,
        &answer,
        scratch.as_ref().map(tempfile::TempDir::path),
    )?;
    record_examination(archive, subject, &findings, &derived, source, run_id)
}

/// An answer sorted: what stands on the examined file, and what became
/// files of its own.
struct Harvest {
    findings: Vec<(Attribute, Value)>,
    derived: Vec<Derivation>,
}

/// The whole answer sorted into findings and derived files — or refused
/// whole. The stream is read to its end before anything is judged, so
/// the order of its lines never matters: announcing a file after
/// speaking about it is legal.
fn harvest(program: &str, answer: &str, scratch: Option<&Path>) -> Result<Harvest> {
    let mut findings = Vec::new();
    let mut derived: Vec<Derivation> = Vec::new();
    let mut announced: HashSet<String> = HashSet::new();
    let mut spoken: Vec<(String, Attribute, Value)> = Vec::new();
    for line in answer.lines().filter(|line| !line.trim().is_empty()) {
        let parsed: Line = serde_json::from_str(line).map_err(|error| {
            Error::Extract(format!(
                "`{program}` answered outside the protocol: {line:?}: {error}"
            ))
        })?;
        match parsed {
            Line {
                file: Some(name),
                mime: Some(mime),
                attribute: None,
                value: None,
            } => {
                let Some(scratch) = scratch else {
                    return Err(Error::Extract(format!(
                        "`{program}` announced {name:?}, but its identify line does not say it derives — no directory was handed over"
                    )));
                };
                if !bare(&name) {
                    return Err(Error::Extract(format!(
                        "`{program}` announced {name:?} — a derived file's name is bare, no path in it"
                    )));
                }
                if !announced.insert(name.clone()) {
                    return Err(Error::Extract(format!(
                        "`{program}` announced {name:?} twice"
                    )));
                }
                let path = scratch.join(&name);
                if !path.is_file() {
                    return Err(Error::Extract(format!(
                        "`{program}` announced {name:?} but wrote no such file"
                    )));
                }
                derived.push(Derivation {
                    name,
                    mime,
                    path,
                    findings: Vec::new(),
                });
            }
            Line {
                file: None,
                mime: None,
                attribute: Some(attribute),
                value: Some(value),
            } => findings.push((Attribute::parse(&attribute)?, value)),
            Line {
                file: Some(name),
                mime: None,
                attribute: Some(attribute),
                value: Some(value),
            } => spoken.push((name, Attribute::parse(&attribute)?, value)),
            _ => {
                return Err(Error::Extract(format!(
                    "`{program}` answered outside the protocol: {line:?}"
                )));
            }
        }
    }
    for (name, attribute, value) in spoken {
        let Some(derivation) = derived
            .iter_mut()
            .find(|derivation| derivation.name == name)
        else {
            return Err(Error::Extract(format!(
                "`{program}` spoke about {name:?}, a file it never announced"
            )));
        };
        derivation.findings.push((attribute, value));
    }
    Ok(Harvest { findings, derived })
}

/// A derived file's announced name: the key the stream and the directory
/// share. Bare means it names a file, not a place — nothing empty, no
/// dot-directories, no separators of either persuasion.
fn bare(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_name_names_a_file_and_nothing_else() {
        assert!(bare("report.pdf"));
        assert!(bare("no extension"));
        assert!(!bare(""));
        assert!(!bare("."));
        assert!(!bare(".."));
        assert!(!bare("sub/report.pdf"));
        assert!(!bare("..\\report.pdf"));
    }

    #[test]
    fn the_three_line_shapes_sort_themselves() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("report.pdf"), b"%PDF").unwrap();
        let answer = concat!(
            "{\"attribute\":\"mail:subject\",\"value\":\"Re: the plan\"}\n",
            "{\"file\":\"report.pdf\",\"attribute\":\"mail:content-id\",\"value\":\"<p2@example.com>\"}\n",
            "{\"file\":\"report.pdf\",\"mime\":\"application/pdf\"}\n",
        );

        let Harvest { findings, derived } = harvest("x", answer, Some(dir.path())).unwrap();

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].0.as_str(), "mail:subject");
        assert_eq!(derived.len(), 1);
        assert_eq!(derived[0].name, "report.pdf");
        assert_eq!(derived[0].mime, "application/pdf");
        assert_eq!(
            derived[0].findings.len(),
            1,
            "spoken about before it was announced, and sorted out all the same"
        );
    }

    #[test]
    fn an_answer_that_does_not_add_up_is_refused_whole() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("real.txt"), b"present").unwrap();
        let refused = [
            // A shape the protocol does not know.
            "{\"file\":\"real.txt\",\"mime\":\"text/plain\",\"value\":1}",
            // Announced, but never written.
            "{\"file\":\"ghost.txt\",\"mime\":\"text/plain\"}",
            // Spoken about, but never announced.
            "{\"file\":\"real.txt\",\"attribute\":\"a:b\",\"value\":1}",
            // A path where a bare name belongs.
            "{\"file\":\"sub/real.txt\",\"mime\":\"text/plain\"}",
            // Announced twice.
            "{\"file\":\"real.txt\",\"mime\":\"text/plain\"}\n{\"file\":\"real.txt\",\"mime\":\"text/plain\"}",
        ];
        for answer in refused {
            assert!(
                harvest("x", answer, Some(dir.path())).is_err(),
                "{answer} should have been refused"
            );
        }
    }

    #[test]
    fn without_a_directory_an_announcement_is_a_breach() {
        assert!(harvest("x", "{\"file\":\"a.txt\",\"mime\":\"text/plain\"}", None).is_err());
    }

    #[test]
    fn a_listed_entry_splits_into_program_and_contract() {
        assert_eq!(parse_listed("packed").unwrap(), ("packed", None));
        assert_eq!(
            parse_listed("packed:list").unwrap(),
            ("packed", Some("list"))
        );
        for wrong in ["", ":", "packed:", ":list", "Packed:list", "a:b:c", "a b"] {
            assert!(parse_listed(wrong).is_err(), "{wrong:?} should be refused");
        }
    }

    #[test]
    fn one_identify_line_may_stay_nameless_but_several_may_not() {
        let single =
            r#"{"ossuary-extractor":1,"source":"extractor:exif/0.1.0","mimes":["image/jpeg"]}"#;
        let read = read_identities("x", single).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].contract, None);

        let pair = concat!(
            r#"{"ossuary-extractor":1,"contract":"list","source":"extractor:packed-list/0.1.0","mimes":["application/zip"]}"#,
            "\n",
            r#"{"ossuary-extractor":1,"contract":"unpack","source":"extractor:packed-unpack/0.1.0","mimes":["application/zip"],"derives":true}"#,
            "\n",
        );
        let read = read_identities("x", pair).unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].contract.as_deref(), Some("list"));
        assert!(read[1].derives);

        let refused = [
            // Nothing at all.
            "",
            // Two lines, one nameless.
            concat!(
                r#"{"ossuary-extractor":1,"contract":"list","source":"extractor:a/1","mimes":[]}"#,
                "\n",
                r#"{"ossuary-extractor":1,"source":"extractor:b/1","mimes":[]}"#,
            ),
            // The same contract twice.
            concat!(
                r#"{"ossuary-extractor":1,"contract":"list","source":"extractor:a/1","mimes":[]}"#,
                "\n",
                r#"{"ossuary-extractor":1,"contract":"list","source":"extractor:b/1","mimes":[]}"#,
            ),
            // A contract outside the name grammar.
            r#"{"ossuary-extractor":1,"contract":"List","source":"extractor:a/1","mimes":[]}"#,
            // A protocol this build does not speak.
            r#"{"ossuary-extractor":2,"contract":"list","source":"extractor:a/1","mimes":[]}"#,
        ];
        for answer in refused {
            assert!(
                read_identities("x", answer).is_err(),
                "{answer:?} should be refused"
            );
        }
    }

    #[test]
    fn a_closure_serves_as_an_observer() {
        let mut seen = Vec::new();
        let mut observer = |event: Event<'_>| {
            if let Event::CaughtUp { segments } = event {
                seen.push(segments);
            }
        };
        let observer: &mut dyn Observer = &mut observer;
        observer.event(Event::CaughtUp { segments: 3 });
        assert_eq!(seen, [3]);
    }
}
