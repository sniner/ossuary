//! `ossuary extract`: one extractor over everything it has not examined.
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

use std::collections::HashSet;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context as _, Result, anyhow};
use ossuary_core::{
    Archive, Attribute, Derivation, Examined, Index, Source, Subject, Value, record_examination,
};
use serde::Deserialize;

/// The answer to `--identify`. Unknown keys are tolerated on purpose:
/// the protocol number is the gate, and it is checked before anything
/// else is believed.
#[derive(Deserialize)]
struct Identity {
    #[serde(rename = "ossuary-extractor")]
    protocol: u32,
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

pub fn extract(
    root: &Path,
    name: Option<&str>,
    subjects: &[String],
    full: bool,
    temp_dir: Option<&Path>,
    quiet: bool,
) -> Result<ExitCode> {
    let archive = crate::open(root)?;
    let names: Vec<String> = match name {
        Some(name) => vec![name.to_string()],
        None => archive.config().extractors().to_vec(),
    };
    if names.is_empty() {
        return Err(anyhow!(
            "no extractors to run — name one, like `ossuary extract text`, or list this archive's own under [extract] in config.toml"
        ));
    }
    if !quiet {
        eprintln!("archive {}", archive.root().display());
    }

    // Everyone answers --identify before anyone runs. A single named
    // extractor that cannot is this run's error; in the archive's own
    // list, the broken one is reported and the healthy ones still run —
    // a nightly sweep does not forfeit exif to a missing poppler.
    let mut runs = Vec::new();
    let mut broken = 0usize;
    for listed in &names {
        let program = format!("ossuary-extract-{listed}");
        let identified = identify(&program)
            .and_then(|identity| Ok((Source::parse(&identity.source)?, identity)));
        match identified {
            Ok((source, identity)) => runs.push(Run {
                program,
                identity,
                source,
            }),
            Err(error) => {
                if name.is_some() {
                    return Err(error);
                }
                broken += 1;
                eprintln!("skipped: {error:#}");
            }
        }
    }
    if temp_dir.is_some() && !runs.is_empty() && !runs.iter().any(|run| run.identity.derives) {
        return Err(match runs.as_slice() {
            [only] => anyhow!(
                "`{}` hands no files back — --temp-dir is where derived files would wait; run this without it",
                only.program
            ),
            _ => anyhow!(
                "no files would come back from this run — --temp-dir is where derived files wait; run this without it"
            ),
        });
    }

    let mut index = archive.index()?;
    let mut troubled = broken > 0;
    for run in &runs {
        // Folded before every extractor, not once: what one derives, the
        // next one's worklist already sees.
        crate::catch_up(&mut index, &archive, quiet)?;
        troubled |= !run_one(&archive, &index, run, subjects, full, temp_dir, quiet)?;
    }
    if runs.is_empty() {
        println!("nothing ran — no extractor in the archive's list answered --identify");
    }
    if troubled {
        Ok(ExitCode::FAILURE)
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

/// One extractor, identified and ready to run.
struct Run {
    program: String,
    identity: Identity,
    source: Source,
}

/// One extractor over its worklist — or over the named files. Answers
/// whether the run went clean; what failed is already on stderr.
fn run_one(
    archive: &Archive,
    index: &Index,
    run: &Run,
    subjects: &[String],
    full: bool,
    temp_dir: Option<&Path>,
    quiet: bool,
) -> Result<bool> {
    let Run {
        program,
        identity,
        source,
    } = run;
    let scratch = identity
        .derives
        .then(|| scratch_parent(archive, temp_dir))
        .transpose()?;

    // What this run examines: the worklist, unless files were named.
    // Named files skip the mime dispatch — naming is more deliberate
    // than a pattern — and, without --full, sit out when their receipt
    // already stands.
    let mut already = 0usize;
    let worklist: Vec<Subject> = if subjects.is_empty() {
        let waiting = if full {
            index.of_kind(&identity.mimes)?
        } else {
            index.worklist(&identity.mimes, source)?
        };
        if waiting.is_empty() {
            if full {
                println!("nothing of a kind {source} reads is on the record");
            } else {
                println!(
                    "nothing waiting for {source} — no file of a kind it reads stands unexamined"
                );
            }
            return Ok(true);
        }
        if !quiet {
            eprintln!("{} file(s) waiting for {source}", waiting.len());
        }
        waiting
    } else {
        let examinees;
        (examinees, already) = named(archive, index, subjects, source, full)?;
        if !quiet && !examinees.is_empty() {
            eprintln!("{} file(s) named for {source}", examinees.len());
        }
        examinees
    };

    let mut tally = Tally::default();
    let mut failed: Vec<(Subject, anyhow::Error)> = Vec::new();
    for subject in worklist {
        match examine(archive, program, &subject, source, scratch.as_deref()) {
            Ok(written) => tally.add(&written),
            Err(error) => failed.push((subject, error)),
        }
    }

    println!("{}", tally.verdict(source, already));
    if failed.is_empty() {
        Ok(true)
    } else {
        eprintln!(
            "{} could not be examined — offered again next run:",
            failed.len()
        );
        for (subject, error) in &failed {
            eprintln!("  {subject}: {error:#}");
        }
        Ok(false)
    }
}

/// What the run did, counted for the verdict.
#[derive(Default)]
struct Tally {
    examined: usize,
    claims: usize,
    stored: usize,
    known: usize,
    nothing: usize,
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

    /// The run's verdict, in one line: what happened, and which parts of
    /// "nothing" are the calm kind.
    fn verdict(&self, source: &Source, already: usize) -> String {
        let mut verdict = vec![format!(
            "{} file(s) examined by {source}, {} claim(s) written",
            self.examined, self.claims
        )];
        let taken = self.stored + self.known;
        if taken > 0 {
            verdict.push(if self.known > 0 {
                format!(
                    "{taken} derived file(s) taken in, {} of them bytes the archive already held",
                    self.known
                )
            } else {
                format!("{taken} derived file(s) taken in")
            });
        }
        if self.nothing > 0 {
            verdict.push(format!("{} had nothing to tell", self.nothing));
        }
        if already > 0 {
            verdict.push(format!(
                "{already} already examined — --full examines them anew"
            ));
        }
        verdict.join("; ")
    }
}

/// The files a run that was handed names examines: resolved the way
/// `about` resolves a name, deduplicated, and — without `--full` — minus
/// those whose receipt from this source already stands. Answers the
/// examinees and how many sat out; an unknown name refuses the whole run
/// before any work is done.
fn named(
    archive: &Archive,
    index: &Index,
    subjects: &[String],
    source: &Source,
    full: bool,
) -> Result<(Vec<Subject>, usize)> {
    let mut examinees: Vec<Subject> = Vec::new();
    let mut already = 0usize;
    for given in subjects {
        let Some(subject) = crate::resolve(archive, index, given)? else {
            return Err(anyhow!("nothing on the record begins with {given:?}"));
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

/// Who is this extractor, and what does it read?
fn identify(program: &str) -> Result<Identity> {
    let output = Command::new(program)
        .arg("--identify")
        .output()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                anyhow!(
                    "no `{program}` on PATH — an extractor is its own program; install it, then run this again"
                )
            } else {
                anyhow::Error::new(error).context(format!("running `{program} --identify`"))
            }
        })?;
    if !output.status.success() {
        // The extractor's stderr is its own account of what is wrong —
        // "no pdftotext on PATH" beats "it failed".
        let complaint = String::from_utf8_lossy(&output.stderr);
        let complaint = complaint.trim();
        if !complaint.is_empty() {
            return Err(anyhow!("`{program} --identify` failed: {complaint}"));
        }
        return Err(anyhow!(
            "`{program} --identify` failed — an extractor answers it before anything is examined"
        ));
    }
    let line = String::from_utf8_lossy(&output.stdout);
    let identity: Identity = serde_json::from_str(line.trim()).with_context(|| {
        format!("`{program} --identify` did not answer in the extractor protocol")
    })?;
    if identity.protocol != 1 {
        return Err(anyhow!(
            "`{program}` speaks extractor protocol {}, this build speaks 1 — upgrade ossuary",
            identity.protocol
        ));
    }
    Ok(identity)
}

/// Where the per-file scratch directories go: what --temp-dir named, or
/// `cache/tmp` in the archive — the disposable tier, on a disk sized for
/// content, where a system /tmp may be a small ramdisk.
fn scratch_parent(archive: &Archive, temp_dir: Option<&Path>) -> Result<PathBuf> {
    let parent = match temp_dir {
        Some(dir) => dir.to_path_buf(),
        None => archive.root().join("cache").join("tmp"),
    };
    std::fs::create_dir_all(&parent)
        .with_context(|| format!("{}: creating the place for derived files", parent.display()))?;
    // The extractor takes the path as its argument; handed over absolute,
    // it holds whatever the extractor's working directory turns out to be.
    std::path::absolute(&parent).with_context(|| {
        format!(
            "{}: resolving the place for derived files",
            parent.display()
        )
    })
}

/// One file through the extractor and onto the record.
fn examine(
    archive: &Archive,
    program: &str,
    subject: &Subject,
    source: &Source,
    scratch_parent: Option<&Path>,
) -> Result<Examined> {
    let content = archive.content();
    let (algorithm, hex) = subject
        .as_str()
        .split_once(':')
        .expect("a subject carries its algorithm");
    if algorithm != content.algorithm().name() {
        return Err(anyhow!(
            "named by {algorithm}, this archive answers to {} — nothing to hand over",
            content.algorithm().name()
        ));
    }
    // The examinee may be an original or itself derived — the PDF out of
    // a mail: content/ is asked first, derived/ second.
    let (store, digest) = match content.matching(hex)?.as_slice() {
        [one] => (content, one.clone()),
        _ => match archive.derived().matching(hex)?.as_slice() {
            [one] => (archive.derived(), one.clone()),
            _ => {
                return Err(anyhow!(
                    "not held — the log speaks of it, neither store holds its bytes"
                ));
            }
        },
    };
    let mut bytes = Vec::new();
    store
        .reader(&digest)?
        .ok_or_else(|| anyhow!("gone between naming and reading"))?
        .read_to_end(&mut bytes)
        .context("reading from the store")?;

    // A fresh directory per file: names cannot collide across files, and
    // dropping it sweeps everything — announced files once they are taken
    // in, workspace leavings and half-written attempts always.
    let scratch = scratch_parent
        .map(|parent| {
            tempfile::TempDir::new_in(parent).with_context(|| {
                format!("{}: making a directory for derived files", parent.display())
            })
        })
        .transpose()?;
    let mut command = Command::new(program);
    if let Some(scratch) = &scratch {
        command.arg(scratch.path());
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| format!("running `{program}`"))?;
    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(&bytes)
        .context("handing the bytes over")?;
    let mut answer = String::new();
    child
        .stdout
        .take()
        .expect("stdout was piped")
        .read_to_string(&mut answer)
        .context("reading the findings")?;
    let status = child.wait().context("waiting for the extractor")?;
    if !status.success() {
        return Err(anyhow!("`{program}` gave up on it ({status})"));
    }

    let Harvest { findings, derived } = harvest(
        program,
        &answer,
        scratch.as_ref().map(tempfile::TempDir::path),
    )?;
    Ok(record_examination(
        content,
        archive.derived(),
        archive.log(),
        subject,
        &findings,
        &derived,
        source,
    )?)
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
        let parsed: Line = serde_json::from_str(line)
            .with_context(|| format!("`{program}` answered outside the protocol: {line:?}"))?;
        match parsed {
            Line {
                file: Some(name),
                mime: Some(mime),
                attribute: None,
                value: None,
            } => {
                let Some(scratch) = scratch else {
                    return Err(anyhow!(
                        "`{program}` announced {name:?}, but its identify line does not say it derives — no directory was handed over"
                    ));
                };
                if !bare(&name) {
                    return Err(anyhow!(
                        "`{program}` announced {name:?} — a derived file's name is bare, no path in it"
                    ));
                }
                if !announced.insert(name.clone()) {
                    return Err(anyhow!("`{program}` announced {name:?} twice"));
                }
                let path = scratch.join(&name);
                if !path.is_file() {
                    return Err(anyhow!(
                        "`{program}` announced {name:?} but wrote no such file"
                    ));
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
                return Err(anyhow!(
                    "`{program}` answered outside the protocol: {line:?}"
                ));
            }
        }
    }
    for (name, attribute, value) in spoken {
        let Some(derivation) = derived
            .iter_mut()
            .find(|derivation| derivation.name == name)
        else {
            return Err(anyhow!(
                "`{program}` spoke about {name:?}, a file it never announced"
            ));
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
}
