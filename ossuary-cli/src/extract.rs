//! `ossuary extract`: one extractor over everything it has not examined.
//!
//! The extractor is its own program, found on PATH and spoken to over
//! pipes — the protocol is `docs/extractors.md`. This side owns the
//! archive: it asks the extractor who it is, folds the log for the
//! worklist, hands each file's bytes over, and funnels what comes back
//! through the claim grammar before anything is recorded. A file whose
//! answer does not parse fails whole — nothing half-recorded, no
//! receipt, offered again next run.

use std::io::{Read, Write as _};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context as _, Result, anyhow};
use ossuary_core::{Archive, Attribute, Source, Subject, Value, record_examination};
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
}

/// One finding line. Strict: protocol 1 says these two fields, and a
/// misspelled key must not pass as an empty answer.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Finding {
    attribute: String,
    value: Value,
}

pub fn extract(root: &Path, name: &str) -> Result<ExitCode> {
    let archive = crate::open(root)?;
    let program = format!("ossuary-extract-{name}");
    let identity = identify(&program)?;
    let source = Source::parse(&identity.source)?;

    eprintln!("archive {}", archive.root().display());
    let mut index = archive.index()?;
    let folded = index.fold(archive.log())?;
    if folded.segments > 0 {
        eprintln!(
            "catching the index up: {} sealed segment(s) it had not seen",
            folded.segments
        );
    }
    let worklist = index.worklist(&identity.mimes, &source)?;
    if worklist.is_empty() {
        println!("nothing waiting for {source} — no file of a kind it reads stands unexamined");
        return Ok(ExitCode::SUCCESS);
    }
    eprintln!("{} file(s) waiting for {source}", worklist.len());

    let mut examined = 0usize;
    let mut claims = 0usize;
    let mut quiet = 0usize;
    let mut failed: Vec<(Subject, anyhow::Error)> = Vec::new();
    for subject in worklist {
        match examine(&archive, &program, &subject, &source) {
            Ok(written) => {
                examined += 1;
                claims += written;
                if written == 1 {
                    quiet += 1;
                }
            }
            Err(error) => failed.push((subject, error)),
        }
    }

    let mut verdict = vec![format!(
        "{examined} file(s) examined by {source}, {claims} claim(s) written"
    )];
    if quiet > 0 {
        verdict.push(format!("{quiet} had nothing to tell"));
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

/// One file through the extractor and onto the record.
fn examine(archive: &Archive, program: &str, subject: &Subject, source: &Source) -> Result<usize> {
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

    let mut child = Command::new(program)
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

    let mut findings = Vec::new();
    for line in answer.lines().filter(|line| !line.trim().is_empty()) {
        let finding: Finding = serde_json::from_str(line)
            .with_context(|| format!("`{program}` answered outside the protocol: {line:?}"))?;
        let attribute = Attribute::parse(&finding.attribute)?;
        findings.push((attribute, finding.value));
    }
    Ok(record_examination(
        archive.log(),
        subject,
        &findings,
        source,
    )?)
}
