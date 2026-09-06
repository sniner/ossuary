//! `ossuary audit`: the archive held against its own record.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Result, anyhow};
use ossuary_core::{Audit, LogAudit, StoreAudit};

use crate::{open, say};

/// How many names still fit in a plain answer. Up to a handful stands
/// right there; more turns into a count, and --verbose names them all.
const HANDFUL: usize = 5;

pub(crate) fn audit(root: &Path, json: bool, verbose: bool, quiet: bool) -> Result<ExitCode> {
    if json && verbose {
        return Err(anyhow!(
            "--json always names every finding — --verbose adds nothing; drop one of them"
        ));
    }
    let archive = open(root)?;
    if !quiet {
        eprintln!("archive {}", archive.root().display());
        eprintln!(
            "step 1 of 3: content/ — every file read whole, its bytes proved against its name"
        );
    }
    let content = ossuary_core::audit_store(archive.content())?;
    if !quiet {
        eprintln!("step 2 of 3: derived/ — the same, for what tools made");
    }
    let derived = ossuary_core::audit_store(archive.derived())?;
    if !quiet {
        eprintln!("step 3 of 3: claims/ — every sealed segment and the open head read back");
    }
    let log = ossuary_core::audit_log(archive.log())?;
    let audit = Audit::assemble(content, derived, log);

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if json {
        render_json(&mut out, &audit)?;
        // Under --json stdout is findings and nothing else; the verdict
        // is the run's word on how it went.
        if !quiet {
            eprintln!("{}", verdict(&audit));
        }
    } else {
        render(&mut out, &audit, verbose)?;
    }
    if audit.is_sound() {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

/// The whole report, for a reader. A closed pipe ends the answer early
/// and calmly — the exit code still carries the verdict.
fn render(out: &mut impl Write, audit: &Audit, verbose: bool) -> Result<()> {
    if !store_block(out, "content/", &audit.content, verbose)? {
        return Ok(());
    }
    if !store_block(out, "derived/", &audit.derived, verbose)? {
        return Ok(());
    }
    if !log_block(out, &audit.log, verbose)? {
        return Ok(());
    }
    let missing = if audit.missing.is_empty() {
        say(out, "every file the claims speak of is held")?
    } else {
        listing(
            out,
            &format!(
                "{} file(s) the claims speak of, held by no store",
                audit.missing.len()
            ),
            &audit.missing,
            verbose,
        )?
    };
    if !missing {
        return Ok(());
    }
    for (place, unrecorded) in [
        ("content/", &audit.unrecorded_content),
        ("derived/", &audit.unrecorded_derived),
    ] {
        if unrecorded.is_empty() {
            continue;
        }
        let heading = format!(
            "{} file(s) held in {place} that no claim speaks of — not a finding; an interrupted run leaves these, and the next arrival records them",
            unrecorded.len()
        );
        if !listing(out, &heading, unrecorded, verbose)? {
            return Ok(());
        }
    }
    say(out, &verdict(audit))?;
    Ok(())
}

/// The last line: which clean outcome it is, or how much is wrong.
fn verdict(audit: &Audit) -> String {
    if audit.is_sound() {
        "sound — every file re-hashed and true to its name, every claim read back, nothing spoken of is missing".to_string()
    } else {
        format!("not sound: {} finding(s)", audit.findings())
    }
}

/// One store's line: what could not even be read, always named — the
/// error is the finding — then the count of files with damage listed
/// under the handful rule.
fn store_block(
    out: &mut impl Write,
    name: &str,
    store: &StoreAudit,
    verbose: bool,
) -> Result<bool> {
    if store.checked == 0 {
        return say(out, &format!("{name}: holds nothing"));
    }
    for (digest, error) in &store.unreadable {
        if !say(out, &format!("{name}: could not read {digest}: {error}"))? {
            return Ok(false);
        }
    }
    let mut clauses = vec![format!("{} file(s)", store.checked)];
    if !store.unreadable.is_empty() {
        clauses.push(format!("{} not read", store.unreadable.len()));
    }
    if !store.damaged.is_empty() {
        clauses.push(format!(
            "{} damaged — bytes no longer what the name says",
            store.damaged.len()
        ));
    } else if store.unreadable.is_empty() {
        clauses.push("every one still what its name says".to_string());
    }
    listing(
        out,
        &format!("{name}: {}", clauses.join(", ")),
        &store.damaged,
        verbose,
    )
}

/// The log's lines: whatever will not read back is named whole — those
/// errors are the findings — then the count of segments, claims, and
/// damage listed under the handful rule.
fn log_block(out: &mut impl Write, log: &LogAudit, verbose: bool) -> Result<bool> {
    for (digest, error) in &log.unreadable {
        if !say(
            out,
            &format!("claims: could not read segment {digest}: {error}"),
        )? {
            return Ok(false);
        }
    }
    for (digest, error) in &log.broken {
        if !say(
            out,
            &format!("claims: segment {digest} will not read back — {error}"),
        )? {
            return Ok(false);
        }
    }
    if let Some(error) = &log.head_broken {
        if !say(
            out,
            &format!("claims: the open head will not read back — {error}"),
        )? {
            return Ok(false);
        }
    }
    let mut clauses = vec![
        if log.head_broken.is_none() {
            format!("{} sealed segment(s) and the open head", log.segments)
        } else {
            format!("{} sealed segment(s)", log.segments)
        },
        format!("{} claim(s)", log.claims),
    ];
    if !log.unreadable.is_empty() {
        clauses.push(format!("{} not read", log.unreadable.len()));
    }
    if !log.broken.is_empty() {
        clauses.push(format!("{} broken", log.broken.len()));
    }
    if !log.damaged.is_empty() {
        clauses.push(format!("{} damaged", log.damaged.len()));
    } else if log.unreadable.is_empty() && log.broken.is_empty() && log.head_broken.is_none() {
        clauses.push("read back whole".to_string());
    }
    listing(
        out,
        &format!("claims: {}", clauses.join(", ")),
        &log.damaged,
        verbose,
    )
}

/// A heading and the names beneath it: all of them under --verbose or
/// when a handful fits, the way to them otherwise. `Ok(false)` means the
/// reader left.
fn listing(out: &mut impl Write, heading: &str, ids: &[String], verbose: bool) -> Result<bool> {
    if ids.is_empty() {
        return say(out, heading);
    }
    if verbose || ids.len() <= HANDFUL {
        if !say(out, &format!("{heading}:"))? {
            return Ok(false);
        }
        for id in ids {
            if !say(out, &format!("  {id}"))? {
                return Ok(false);
            }
        }
        Ok(true)
    } else {
        say(out, &format!("{heading} — --verbose names them"))
    }
}

/// One JSON object per finding or observation, ready for jq — a sound,
/// fully recorded archive answers an empty stream.
fn render_json(out: &mut impl Write, audit: &Audit) -> Result<()> {
    use serde_json::json;

    let mut lines: Vec<serde_json::Value> = Vec::new();
    for (place, store) in [("content", &audit.content), ("derived", &audit.derived)] {
        for subject in &store.damaged {
            lines.push(json!({"finding": "damaged", "store": place, "subject": subject}));
        }
        for (subject, error) in &store.unreadable {
            lines.push(json!({"finding": "unreadable", "store": place, "subject": subject, "error": error}));
        }
    }
    for segment in &audit.log.damaged {
        lines.push(json!({"finding": "damaged", "store": "claims", "segment": segment}));
    }
    for (segment, error) in &audit.log.unreadable {
        lines.push(
            json!({"finding": "unreadable", "store": "claims", "segment": segment, "error": error}),
        );
    }
    for (segment, error) in &audit.log.broken {
        lines.push(json!({"finding": "broken-segment", "segment": segment, "error": error}));
    }
    if let Some(error) = &audit.log.head_broken {
        lines.push(json!({"finding": "broken-head", "error": error}));
    }
    for subject in &audit.missing {
        lines.push(json!({"finding": "missing", "subject": subject}));
    }
    for (place, unrecorded) in [
        ("content", &audit.unrecorded_content),
        ("derived", &audit.unrecorded_derived),
    ] {
        for subject in unrecorded {
            lines.push(json!({"observation": "unrecorded", "store": place, "subject": subject}));
        }
    }
    for line in &lines {
        if !say(out, &line.to_string())? {
            return Ok(());
        }
    }
    Ok(())
}
