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
    Archive, Attribute, Derivation, Examined, Source, Subject, Value, record_examination,
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

pub fn extract(root: &Path, name: &str, temp_dir: Option<&Path>, quiet: bool) -> Result<ExitCode> {
    let archive = crate::open(root)?;
    let program = format!("ossuary-extract-{name}");
    let identity = identify(&program)?;
    let source = Source::parse(&identity.source)?;
    if temp_dir.is_some() && !identity.derives {
        return Err(anyhow!(
            "`{program}` hands no files back — --temp-dir is where derived files would wait; run this without it"
        ));
    }
    let scratch = identity
        .derives
        .then(|| scratch_parent(&archive, temp_dir))
        .transpose()?;

    if !quiet {
        eprintln!("archive {}", archive.root().display());
    }
    let mut index = archive.index()?;
    crate::catch_up(&mut index, &archive, quiet)?;
    let worklist = index.worklist(&identity.mimes, &source)?;
    if worklist.is_empty() {
        println!("nothing waiting for {source} — no file of a kind it reads stands unexamined");
        return Ok(ExitCode::SUCCESS);
    }
    if !quiet {
        eprintln!("{} file(s) waiting for {source}", worklist.len());
    }

    let mut examined = 0usize;
    let mut claims = 0usize;
    let mut stored = 0usize;
    let mut known = 0usize;
    let mut quiet_files = 0usize;
    let mut failed: Vec<(Subject, anyhow::Error)> = Vec::new();
    for subject in worklist {
        match examine(&archive, &program, &subject, &source, scratch.as_deref()) {
            Ok(written) => {
                examined += 1;
                claims += written.claims;
                stored += written.stored;
                known += written.known;
                if written.claims == 1 {
                    quiet_files += 1;
                }
            }
            Err(error) => failed.push((subject, error)),
        }
    }

    let mut verdict = vec![format!(
        "{examined} file(s) examined by {source}, {claims} claim(s) written"
    )];
    if stored + known > 0 {
        let taken = stored + known;
        verdict.push(if known > 0 {
            format!(
                "{taken} derived file(s) taken in, {known} of them bytes the archive already held"
            )
        } else {
            format!("{taken} derived file(s) taken in")
        });
    }
    if quiet_files > 0 {
        verdict.push(format!("{quiet_files} had nothing to tell"));
    }
    println!("{}", verdict.join("; "));
    if failed.is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!(
            "{} could not be examined — offered again next run:",
            failed.len()
        );
        for (subject, error) in &failed {
            eprintln!("  {subject}: {error:#}");
        }
        Ok(ExitCode::FAILURE)
    }
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
            "named by {algorithm}, the content store answers to {} — nothing to hand over",
            content.algorithm().name()
        ));
    }
    let digest = match content.matching(hex)?.as_slice() {
        [one] => one.clone(),
        _ => {
            return Err(anyhow!(
                "not in the content store — the log speaks of it, the store does not hold it"
            ));
        }
    };
    let mut bytes = Vec::new();
    content
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
