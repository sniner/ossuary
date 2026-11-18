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
//! Only trouble that belongs to one file lets the run go on: the
//! extractor refusing it or answering outside the protocol, or the file
//! not reading. Anything else stops the run at that file, writing to the
//! archive above all, because every file after it would fail the same
//! way, and a run that keeps going past such trouble hides it.
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
use std::fs;
use std::path::{Path, PathBuf};

use crate::archive::Archive;
use crate::claim::{Attribute, Run, Source, Subject, Value};
use crate::error::{Error, Result};
use crate::extract::{Examined, record_examination};
use crate::extractor::{Failure, Harvest, Identity, exchange, identify, well_formed};
use crate::index::{Index, Scope};
use crate::recorder::Recorder;
use crate::vocabulary::{FILE_NAME, known_attribute};

/// One thing the orchestration did or found, told as it happens. The
/// observer decides what becomes of it; the variants only say what is.
#[derive(Debug)]
pub enum Event<'a> {
    /// The index caught up on sealed segments it had not seen, or was
    /// built anew.
    CaughtUp {
        /// How many segments the fold brought in.
        segments: usize,
        /// Whether the index did not match the log and was built anew
        /// from all of it.
        rebuilt: bool,
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
    /// One file rehearsed under --dry-run: what the extractor would
    /// record, shown and then dropped.
    Rehearsed {
        source: &'a Source,
        subject: &'a Subject,
        /// The claims that would go on the examined file.
        findings: &'a [(Attribute, Value)],
        /// The files that would be taken in: name, MIME type, size.
        derived: &'a [(String, String, u64)],
    },
    /// The extractor spoke on stderr about one file: narration or a
    /// complaint, examination happened all the same. The lines come
    /// without the program's own name in front, and with the file the
    /// extractor could not know it was speaking about.
    Remarked {
        source: &'a Source,
        subject: &'a Subject,
        /// The file's standing `file:name`, when the record has one.
        name: Option<&'a str>,
        /// What was said, one or more lines.
        remark: &'a str,
    },
    /// A file could not be examined: it gets no receipt and is offered
    /// again on the next run. Told as it happens; the pass goes on.
    Failed {
        source: &'a Source,
        subject: &'a Subject,
        /// The file's standing `file:name`, when the record has one.
        name: Option<&'a str>,
        error: &'a Error,
    },
    /// A pass ended, or stopped at a file: the verdict's material.
    Verdict {
        source: &'a Source,
        tally: &'a Tally,
        /// Named files that sat out because their receipt already stands.
        already: usize,
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
    /// Derived files taken in, across all rounds — bytes new to the
    /// archive and bytes it already held alike.
    pub derived: usize,
    /// The call's run id: what every claim this call wrote carries,
    /// findings, receipts and derived files' sightings alike.
    pub run: Run,
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
/// `dry_run` runs the extractors and records nothing: each named file's
/// would-be harvest is told as [`Event::Rehearsed`] and dropped — and
/// because a rehearsal over everything that waits would examine the
/// whole archive and keep none of it, it demands named `subjects`.
///
/// # Errors
///
/// [`Error::HeadLost`] before any extractor runs when the log's head was
/// lost and the run would record, naming trouble — [`Error::Unknown`],
/// [`Error::Ambiguous`] — and [`Error::Extract`] for a named extractor
/// that will not identify, a loop that never runs dry, or `temp_dir`
/// handed to a run that derives nothing. [`Error::Extract`] as well when
/// the run stops at a file because the archive could not be written, or
/// for any other trouble that is not the file's own; the files examined
/// before it stay recorded. A file the extractor fails, or that will not
/// read, is not an error here: it is told as [`Event::Failed`], the run
/// goes on, and the settlement loses its `clean`.
pub fn examine(
    archive: &Archive,
    name: Option<&str>,
    subjects: &[String],
    full: bool,
    dry_run: bool,
    temp_dir: Option<&Path>,
    observer: &mut dyn Observer,
) -> Result<Settlement> {
    if dry_run && subjects.is_empty() {
        return Err(Error::Extract(
            "a dry run needs named files; name subjects or a run id".to_string(),
        ));
    }
    if !dry_run {
        archive.log().ready()?;
    }
    let names: Vec<String> = match name {
        Some(name) => vec![name.to_string()],
        None => archive.config().extractors().to_vec(),
    };
    if names.is_empty() {
        return Err(Error::Extract(
            "no extractors to run; name one, as in `ossuary extract pdf`, or list them under [extract] in config.toml".to_string(),
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
                    "`{}` writes no derived files under its {contract} contract; run without --temp-dir",
                    only.program
                ),
                None => format!(
                    "`{}` writes no derived files; run without --temp-dir",
                    only.program
                ),
            },
            _ => {
                "no extractor in this run writes derived files; run without --temp-dir".to_string()
            }
        }));
    }

    let mut index = archive.index()?;
    let mut troubled = skipped > 0;
    let mut invocation = Invocation {
        archive,
        run_id: Run::new(),
        full,
        dry_run,
        temp_dir,
        observer,
        examined: HashSet::new(),
    };

    let mut rounds = 1;
    let mut examinations = 0;
    let mut derived = 0;
    if subjects.is_empty() {
        let settled = settle(&mut invocation, &mut index, &runs)?;
        troubled |= settled.troubled;
        rounds = settled.rounds;
        examinations = settled.examinations;
        derived = settled.derived;
    } else {
        // Named files are the surgical grip: one pass, no rounds.
        for run in &runs {
            catch_up(&mut index, invocation.archive, invocation.observer)?;
            let outcome = run_one(&mut invocation, &index, run, subjects, 1)?;
            troubled |= !outcome.clean;
            examinations += outcome.examined;
            derived += outcome.derived;
        }
    }
    if !dry_run {
        archive.flush()?;
    }
    Ok(Settlement {
        clean: !troubled,
        ran: runs.len(),
        rounds,
        examinations,
        derived,
        run: invocation.run_id,
    })
}

/// How the fixpoint loop went, for the settlement.
struct Settled {
    troubled: bool,
    rounds: usize,
    examinations: usize,
    derived: usize,
}

/// Rounds over the list until a full round examines nothing — the
/// fixpoint. The "queue" is the worklist itself, refolded from log and
/// receipts before every extractor: Ctrl-C in round three loses
/// nothing, the next call carries on where the receipts end. List
/// order is only a performance hint — an extractor listed before its
/// feeder costs one more round, never a second command.
fn settle(invocation: &mut Invocation, index: &mut Index, runs: &[Contract]) -> Result<Settled> {
    let mut troubled = false;
    let mut round = 0usize;
    let mut total = 0usize;
    let mut derived = 0usize;
    loop {
        round += 1;
        let mut examined = 0usize;
        let mut busy: Vec<String> = Vec::new();
        for run in runs {
            catch_up(index, invocation.archive, invocation.observer)?;
            let outcome = run_one(invocation, index, run, &[], round)?;
            troubled |= !outcome.clean;
            derived += outcome.derived;
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
                "stopped after {MAX_ROUNDS} rounds with files still waiting for {}; the claims recorded so far are kept; fix the extractor, then run again",
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
        derived,
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
    if folded.segments > 0 || folded.rebuilt {
        observer.event(Event::CaughtUp {
            segments: folded.segments,
            rebuilt: folded.rebuilt,
        });
    }
    Ok(())
}

/// One extractor's contract, identified and ready to run.
struct Contract {
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
            "{listed:?} is not an extractor name; use NAME or NAME:CONTRACT, with lowercase letters, digits and dashes"
        )));
    }
    Ok((name, contract))
}

/// One listed entry into its runs: the program identified, its
/// contracts selected — all of them, or the one named after the colon.
fn prepare(listed: &str) -> Result<Vec<Contract>> {
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
                        "`{program}` has no named contracts; run it as `ossuary extract {name}`"
                    )));
                }
                None => {
                    return Err(Error::Extract(format!(
                        "`{program}` has no contract named {wanted:?}; available contracts: {}",
                        offered.join(", ")
                    )));
                }
            }
        }
    };
    selected
        .into_iter()
        .map(|identity| {
            Ok(Contract {
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
    /// The run: what every claim of this call carries, rounds included
    /// — they are the call's insides.
    run_id: Run,
    full: bool,
    /// Run the extractors, record nothing: harvests become
    /// [`Event::Rehearsed`] and are dropped.
    dry_run: bool,
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
    derived: usize,
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
    run: &Contract,
    subjects: &[String],
    round: usize,
) -> Result<Outcome> {
    let Contract {
        identity, source, ..
    } = run;
    let archive = invocation.archive;
    let scratch = identity
        .derives
        .then(|| scratch_parent(archive, invocation.temp_dir))
        .transpose()?;

    let Some((worklist, already)) = examinees(invocation, index, run, subjects, round)? else {
        return Ok(Outcome {
            clean: true,
            examined: 0,
            derived: 0,
        });
    };

    let mut tally = Tally::default();
    // The examinations of the pass are written in batches, the last one
    // before the verdict and before the run stops at a file: what the
    // verdict counts and what the stop says is recorded is in the log.
    let mut recorder = archive.recorder();
    for subject in worklist {
        let attempted = attempt(
            invocation,
            &mut recorder,
            run,
            &subject,
            scratch.as_deref(),
            &mut tally,
        );
        let failure = match attempted {
            Ok(remarks) => {
                relay(invocation, index, source, &subject, &remarks)?;
                continue;
            }
            Err(failure) => failure,
        };
        let name = name_of(index, &subject)?;
        match failure {
            Failure::Skip(error) => {
                tally.failed += 1;
                invocation.observer.event(Event::Failed {
                    source,
                    subject: &subject,
                    name: name.as_deref(),
                    error: &error,
                });
            }
            Failure::Stop(error) => {
                // Writing the batch failing is the trouble to report: the
                // examinations before this file are not recorded then.
                recorder.write()?;
                invocation.observer.event(Event::Verdict {
                    source,
                    tally: &tally,
                    already,
                });
                return Err(stopped(
                    &subject,
                    name.as_deref(),
                    &error,
                    invocation.dry_run,
                ));
            }
        }
    }

    recorder.write()?;
    invocation.observer.event(Event::Verdict {
        source,
        tally: &tally,
        already,
    });
    Ok(Outcome {
        clean: tally.failed == 0,
        examined: tally.examined,
        derived: tally.stored + tally.known,
    })
}

/// What one pass examines, and how many named files sat out: the
/// worklist, unless files were named. `None` when the worklist is empty.
///
/// Named files skip the mime dispatch — naming is more deliberate than a
/// pattern — and, without --full, sit out when their receipt already
/// stands. Under --full the receipts of earlier calls are ignored and the
/// invocation's own memo takes their place.
fn examinees(
    invocation: &mut Invocation,
    index: &Index,
    run: &Contract,
    subjects: &[String],
    round: usize,
) -> Result<Option<(Vec<Subject>, usize)>> {
    let Contract {
        identity, source, ..
    } = run;
    if !subjects.is_empty() {
        let (examinees, already) = named(index, subjects, source, invocation.full)?;
        if !examinees.is_empty() {
            invocation.observer.event(Event::Named {
                source,
                count: examinees.len(),
            });
        }
        return Ok(Some((examinees, already)));
    }
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
        return Ok(None);
    }
    invocation.observer.event(Event::Waiting {
        source,
        count: waiting.len(),
        round,
    });
    Ok(Some((waiting, 0)))
}

/// One file through the extractor, rehearsed under --dry-run and recorded
/// otherwise, and counted in the tally. Answers what the extractor said on
/// stderr.
fn attempt(
    invocation: &mut Invocation,
    recorder: &mut Recorder<'_>,
    run: &Contract,
    subject: &Subject,
    scratch: Option<&Path>,
    tally: &mut Tally,
) -> std::result::Result<String, Failure> {
    if invocation.dry_run {
        let (nothing, remarks) = rehearse_one(invocation, run, subject, scratch)?;
        tally.examined += 1;
        if nothing {
            tally.nothing += 1;
        }
        return Ok(remarks);
    }
    let (written, remarks) = examine_one(
        invocation.archive,
        recorder,
        run,
        subject,
        &invocation.run_id,
        scratch,
    )?;
    tally.add(&written);
    invocation.examined.insert(memo_key(subject, &run.source));
    Ok(remarks)
}

/// The error that ends a run at `subject`: what went wrong, and that
/// what was recorded before it stays.
fn stopped(subject: &Subject, name: Option<&str>, error: &Error, dry_run: bool) -> Error {
    let file = match name {
        Some(name) => format!("{subject} ({name})"),
        None => subject.to_string(),
    };
    Error::Extract(if dry_run {
        format!("dry run stopped at {file}: {}", error.spelled())
    } else {
        format!(
            "stopped at {file}: {}; the files examined before it are recorded, and the next run starts again with this one",
            error.spelled()
        )
    })
}

/// The file's standing `file:name`, when the record has one.
fn name_of(index: &Index, subject: &Subject) -> Result<Option<String>> {
    let names = index.values(subject, &known_attribute(FILE_NAME), Scope::Held)?;
    Ok(names.iter().find_map(Value::as_str).map(str::to_string))
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
    /// Files that could not be examined, each told as [`Event::Failed`].
    pub failed: usize,
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

/// Where the per-file scratch directories go: what `temp_dir` named, or
/// `tmp` in the archive's cache directory, which lies on a disk sized
/// for content where a system /tmp may be a small ramdisk.
fn scratch_parent(archive: &Archive, temp_dir: Option<&Path>) -> Result<PathBuf> {
    let parent = match temp_dir {
        Some(dir) => dir.to_path_buf(),
        None => archive.cache_dir()?.join("tmp"),
    };
    std::fs::create_dir_all(&parent).map_err(|error| Error::Io {
        context: format!(
            "{}: creating the directory for derived files",
            parent.display()
        ),
        source: error,
    })?;
    // The extractor takes the path as its argument; handed over absolute,
    // it holds whatever the extractor's working directory turns out to be.
    std::path::absolute(&parent).map_err(|error| Error::Io {
        context: format!(
            "{}: resolving the directory for derived files",
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
    recorder: &mut Recorder<'_>,
    run: &Contract,
    subject: &Subject,
    run_id: &Run,
    scratch_parent: Option<&Path>,
) -> std::result::Result<(Examined, String), Failure> {
    let (Harvest { findings, derived }, _scratch, remarks) = try_one(
        archive,
        &run.program,
        run.identity.contract.as_deref(),
        subject,
        scratch_parent,
    )?;
    // Everything the extractor said has passed the grammar by now, so
    // what fails from here on is the archive being written.
    let written = record_examination(
        archive,
        recorder,
        subject,
        &findings,
        &derived,
        &run.source,
        run_id,
    )?;
    Ok((written, remarks))
}

/// What the extractor said on stderr about one file, told to the
/// observer with the file named — the extractor never knew which file
/// it had; the orchestrator does. Silence is nothing to tell.
fn relay(
    invocation: &mut Invocation,
    index: &Index,
    source: &Source,
    subject: &Subject,
    remarks: &str,
) -> Result<()> {
    if remarks.trim().is_empty() {
        return Ok(());
    }
    let name = name_of(index, subject)?;
    invocation.observer.event(Event::Remarked {
        source,
        subject,
        name: name.as_deref(),
        remark: remarks,
    });
    Ok(())
}

/// One file under --dry-run: same run, same harvest, and the harvest
/// is shown as [`Event::Rehearsed`] instead of recorded — dropped with
/// the scratch. Answers whether the whole harvest was empty, and what
/// the extractor said on stderr.
fn rehearse_one(
    invocation: &mut Invocation,
    run: &Contract,
    subject: &Subject,
    scratch: Option<&Path>,
) -> std::result::Result<(bool, String), Failure> {
    let (Harvest { findings, derived }, _scratch, remarks) = try_one(
        invocation.archive,
        &run.program,
        run.identity.contract.as_deref(),
        subject,
        scratch,
    )?;
    let derived: Vec<(String, String, u64)> = derived
        .iter()
        .map(|derivation| {
            let bytes = fs::metadata(&derivation.path).map_or(0, |metadata| metadata.len());
            (derivation.name.clone(), derivation.mime.clone(), bytes)
        })
        .collect();
    let nothing = findings.is_empty() && derived.is_empty();
    invocation.observer.event(Event::Rehearsed {
        source: &run.source,
        subject,
        findings: &findings,
        derived: &derived,
    });
    Ok((nothing, remarks))
}

/// The examination itself, short of the record: the bytes handed over,
/// the answer harvested, the extractor's stderr kept as its remarks.
/// The scratch directory rides along so announced files still stand
/// when the caller reads or records them.
fn try_one(
    archive: &Archive,
    program: &str,
    contract: Option<&str>,
    subject: &Subject,
    scratch_parent: Option<&Path>,
) -> std::result::Result<(Harvest, Option<tempfile::TempDir>, String), Failure> {
    let content = archive.content();
    // The examinee may be an original or itself derived — the PDF out of
    // a mail: content/ is asked first, derived/ second.
    let digest = immure::Digest::parse(subject.as_str())?;
    let store = if content.contains(&digest)? {
        content
    } else if archive.derived().contains(&digest)? {
        archive.derived()
    } else {
        return Err(Failure::Skip(Error::Extract(
            "recorded, but neither content/ nor derived/ contains it".to_string(),
        )));
    };
    // One file that will not open is that file's trouble; `ossuary audit`
    // is what finds out why.
    let mut reader = store
        .reader(&digest)
        .map_err(|error| Failure::Skip(error.into()))?
        .ok_or_else(|| {
            Failure::Skip(Error::Extract(
                "no longer present in content/ or derived/".to_string(),
            ))
        })?;

    // A fresh directory per file: names cannot collide across files, and
    // dropping it sweeps everything — announced files once they are taken
    // in, workspace leavings and half-written attempts always.
    let scratch = scratch_parent
        .map(|parent| {
            tempfile::TempDir::new_in(parent).map_err(|error| Error::Io {
                context: format!(
                    "{}: creating a directory for derived files",
                    parent.display()
                ),
                source: error,
            })
        })
        .transpose()?;
    let (harvest, remarks) = exchange(
        program,
        contract,
        scratch.as_ref().map(tempfile::TempDir::path),
        &mut reader,
    )?;
    Ok((harvest, scratch, remarks))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_closure_serves_as_an_observer() {
        let mut seen = Vec::new();
        let mut observer = |event: Event<'_>| {
            if let Event::CaughtUp { segments, .. } = event {
                seen.push(segments);
            }
        };
        let observer: &mut dyn Observer = &mut observer;
        observer.event(Event::CaughtUp {
            segments: 3,
            rebuilt: false,
        });
        assert_eq!(seen, [3]);
    }
}
