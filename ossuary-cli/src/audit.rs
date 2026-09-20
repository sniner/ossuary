//! `ossuary audit`: the archive held against its own record.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Result, anyhow};
use ossuary_core::{Audit, Cause, Chain, Fixity, LogAudit, StoreAudit, Timestamp};

use crate::{open, say};

/// How many names still fit in a plain answer. Up to a handful stands
/// right there; more turns into a count, and --verbose names them all.
const HANDFUL: usize = 5;

pub(crate) fn audit(root: &Path, json: bool, verbose: bool, quiet: bool) -> Result<ExitCode> {
    if json && verbose {
        return Err(anyhow!(
            "--json always names every finding; --verbose adds nothing; drop one of them"
        ));
    }
    let archive = open(root)?;
    if !quiet {
        eprintln!("archive {}", archive.root().display());
        eprintln!(
            "step 1 of 3: content/, every file read whole, its bytes proved against its name"
        );
    }
    let content = ossuary_core::audit_store(archive.content())?;
    if !quiet {
        eprintln!("step 2 of 3: derived/, the same, for what tools made");
    }
    let derived = ossuary_core::audit_store(archive.derived())?;
    if !quiet {
        eprintln!("step 3 of 3: claims/, every sealed segment and the open head read back");
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
    if !chain_block(out, &audit.log, verbose)? {
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
            "{} file(s) held in {place} that no claim speaks of; the next arrival records them",
            unrecorded.len()
        );
        if !listing(out, &heading, unrecorded, verbose)? {
            return Ok(());
        }
    }
    if !twin_block(out, audit, verbose)? {
        return Ok(());
    }
    say(out, &verdict(audit))?;
    Ok(())
}

/// Files held by both stores: an observation with the way to tidy it,
/// and — where the original is the damaged copy and the derived one
/// sound — the way to make the original good again.
fn twin_block(out: &mut impl Write, audit: &Audit, verbose: bool) -> Result<bool> {
    if audit.twins.is_empty() {
        return Ok(true);
    }
    let names: Vec<String> = audit
        .twins
        .iter()
        .map(|twin| twin.digest.as_str().to_string())
        .collect();
    let heading = format!(
        "{} file(s) held by both stores; `ossuary maintain weed` takes the copy in derived/ out",
        names.len()
    );
    if !listing(out, &heading, &names, verbose)? {
        return Ok(false);
    }
    let repairable: Vec<String> = audit
        .twins
        .iter()
        .filter(|twin| twin.repairable())
        .map(|twin| twin.digest.as_str().to_string())
        .collect();
    if repairable.is_empty() {
        return Ok(true);
    }
    let heading = format!(
        "{} of them damaged in content/ and sound in derived/; `ossuary maintain weed --repair` stores the sound bytes in the original's place",
        repairable.len()
    );
    listing(out, &heading, &repairable, verbose)
}

/// A copy's fixity as the one word the JSON answer uses for it.
fn fixity_word(fixity: &Fixity) -> &'static str {
    match fixity {
        Fixity::Sound => "sound",
        Fixity::Damaged => "damaged",
        Fixity::Unreadable(_) => "unreadable",
    }
}

/// The chain, when it is not one: every piece named with its ends and
/// its span, then each break with what the record says of it and what
/// to do; and the mends that stand, whole chain or not.
fn chain_block(out: &mut impl Write, log: &LogAudit, verbose: bool) -> Result<bool> {
    if log.chains.len() > 1 {
        if !say(
            out,
            &format!(
                "the chain of sealed segments is broken: {} chains where there should be one",
                log.chains.len()
            ),
        )? {
            return Ok(false);
        }
        for (index, chain) in log.chains.iter().enumerate() {
            if !say(out, &format!("  chain {}: {}", index + 1, describe(chain)))? {
                return Ok(false);
            }
        }
        for (index, brk) in log.breaks.iter().enumerate() {
            let line = match &brk.cause {
                Cause::HeadLost => format!(
                    "  chain {} begins where a head was lost, its claims with it: nothing recorded between {} and {} survived; take that in again, then `ossuary maintain mend` joins the chains",
                    index + 2,
                    when(brk.from.as_ref()),
                    when(brk.to.as_ref()),
                ),
                Cause::SegmentLost(segment) => format!(
                    "  chain {} begins after segment {segment}, which is not held; restore it from a copy of the archive, or `ossuary maintain mend` joins the chains and keeps its name on the record",
                    index + 2,
                ),
                Cause::SegmentUnreadable(segment) => format!(
                    "  chain {} begins after segment {segment}, which is held but will not read back; restore it from a copy of the archive",
                    index + 2,
                ),
            };
            if !say(out, &line)? {
                return Ok(false);
            }
            if !brk.sure
                && !say(
                    out,
                    &format!(
                        "  which chain stands right before chain {} is not certain; `maintain mend` leaves this break alone",
                        index + 2
                    ),
                )?
            {
                return Ok(false);
            }
        }
    }
    for mend in &log.looped {
        if !say(
            out,
            &format!(
                "claims: the chain runs in a circle; mend {mend} joins two ends that were not a break's; take its file out of claims/, then `ossuary maintain mend` again"
            ),
        )? {
            return Ok(false);
        }
    }
    mend_block(out, log, verbose)
}

/// The mends: those that hold a break closed, those whose loss was made
/// good since, and those that close nothing.
fn mend_block(out: &mut impl Write, log: &LogAudit, verbose: bool) -> Result<bool> {
    if !log.mended.is_empty() {
        let mends: Vec<String> = log
            .mended
            .iter()
            .map(|mended| {
                let mut line = format!(
                    "mend {} joins {} to {}",
                    mended.mend,
                    mended.previous,
                    mended.before.as_deref().unwrap_or("the open head")
                );
                if let Some(replaces) = &mended.replaces {
                    line = format!("{line} in place of {replaces}, which is gone");
                }
                line
            })
            .collect();
        let heading = format!("{} mended break(s) in the chain", mends.len());
        if !listing(out, &heading, &mends, verbose)? {
            return Ok(false);
        }
    }
    if !log.restored.is_empty() {
        let mends: Vec<String> = log
            .restored
            .iter()
            .map(|mended| {
                format!(
                    "mend {} stood in for {}",
                    mended.mend,
                    mended.replaces.as_deref().unwrap_or("nothing named")
                )
            })
            .collect();
        let heading = format!(
            "{} mend(s) for a loss since made good; the segment each stood in for is held again",
            mends.len()
        );
        if !listing(out, &heading, &mends, verbose)? {
            return Ok(false);
        }
    }
    if !log.idle_mends.is_empty() {
        let heading = format!(
            "{} mend(s) that close no break; the segment each stands in front of needs none, or is not held",
            log.idle_mends.len()
        );
        if !listing(out, &heading, &log.idle_mends, verbose)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// One chain in a line: how much it holds, when, and its two ends.
fn describe(chain: &Chain) -> String {
    let mut clauses = Vec::new();
    let span = |clauses: &mut Vec<String>| {
        if chain.from.is_some() {
            clauses.push(format!(
                "{} to {}",
                when(chain.from.as_ref()),
                when(chain.to.as_ref())
            ));
        }
    };
    if let (Some(first), Some(last)) = (chain.segments.first(), chain.segments.last()) {
        clauses.push(format!("{} segment(s)", chain.segments.len()));
        clauses.push(format!("{} claim(s)", chain.claims));
        span(&mut clauses);
        if chain.open_head {
            clauses.push(format!("{first} to the open head"));
        } else if first == last {
            clauses.push(format!("{first} alone"));
        } else {
            clauses.push(format!("{first} to {last}"));
        }
    } else {
        clauses.push("the open head alone".to_string());
        clauses.push(format!("{} claim(s)", chain.claims));
        span(&mut clauses);
    }
    clauses.join(", ")
}

/// A claim time for a line, or the word for none.
fn when(time: Option<&Timestamp>) -> &str {
    time.map_or("no claim", Timestamp::as_str)
}

/// The last line: which clean outcome it is, or how much is wrong.
fn verdict(audit: &Audit) -> String {
    if audit.is_sound() {
        "sound; every file re-hashed and true to its name, every claim read back, every sealed segment named still held, nothing spoken of is missing".to_string()
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
        clauses.push(format!("{} damaged", store.damaged.len()));
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
/// errors are the findings — and so is every sealed segment that is gone,
/// with who names it; then the count of segments, claims, and damage
/// listed under the handful rule.
fn log_block(out: &mut impl Write, log: &LogAudit, verbose: bool) -> Result<bool> {
    for (successor, digest) in &log.predecessor_missing {
        if !say(
            out,
            &format!(
                "claims: segment {digest} is not held; {successor} names it as the one sealed before it"
            ),
        )? {
            return Ok(false);
        }
    }
    if let Some(digest) = &log.head_predecessor_missing {
        if !say(
            out,
            &format!(
                "claims: segment {digest} is not held; the open head names it as the last one sealed"
            ),
        )? {
            return Ok(false);
        }
    }
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
            &format!("claims: segment {digest} will not read back: {error}"),
        )? {
            return Ok(false);
        }
    }
    if let Some(error) = &log.head_broken {
        if !say(
            out,
            &format!("claims: the open head will not read back: {error}"),
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
    let lost = log.predecessor_missing.len() + usize::from(log.head_predecessor_missing.is_some());
    if lost > 0 {
        clauses.push(format!("{lost} lost"));
    }
    if !log.damaged.is_empty() {
        clauses.push(format!("{} damaged", log.damaged.len()));
    } else if log.unreadable.is_empty()
        && log.broken.is_empty()
        && log.head_broken.is_none()
        && lost == 0
    {
        clauses.push("read back whole".to_string());
        if log.segments > 0 && log.chains.len() <= 1 {
            clauses.push("chained from the open head back to the first".to_string());
        }
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
        say(out, &format!("{heading}; --verbose names them"))
    }
}

/// One JSON object per finding or observation, ready for jq — a sound,
/// fully recorded archive answers an empty stream.
#[allow(
    clippy::too_many_lines,
    reason = "one push per kind of finding, in the order the report reads; splitting it would only hide that order"
)]
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
    for (successor, segment) in &audit.log.predecessor_missing {
        lines.push(
            json!({"finding": "missing-segment", "segment": segment, "successor": successor}),
        );
    }
    if let Some(segment) = &audit.log.head_predecessor_missing {
        lines.push(json!({"finding": "missing-segment", "segment": segment, "successor": "head"}));
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
    for twin in &audit.twins {
        lines.push(json!({
            "observation": "twin",
            "subject": twin.digest.as_str(),
            "content": fixity_word(&twin.content),
            "derived": fixity_word(&twin.derived),
        }));
    }
    if audit.log.chains.len() > 1 {
        for (index, chain) in audit.log.chains.iter().enumerate() {
            lines.push(json!({
                "observation": "chain",
                "index": index + 1,
                "first": chain.segments.first(),
                "last": chain.segments.last(),
                "segments": chain.segments.len(),
                "claims": chain.claims,
                "open_head": chain.open_head,
                "from": chain.from,
                "to": chain.to,
            }));
        }
    }
    for brk in &audit.log.breaks {
        if brk.cause == Cause::HeadLost {
            lines.push(json!({
                "finding": "head-lost",
                "after": brk.after,
                "before": brk.before.as_deref().unwrap_or("head"),
                "from": brk.from,
                "to": brk.to,
                "sure": brk.sure,
            }));
        }
    }
    for mend in &audit.log.looped {
        lines.push(json!({"finding": "chain-loop", "mend": mend}));
    }
    for mended in &audit.log.restored {
        lines.push(json!({
            "observation": "restored",
            "mend": mended.mend,
            "replaces": mended.replaces,
        }));
    }
    for mended in &audit.log.mended {
        lines.push(json!({
            "observation": "mended",
            "mend": mended.mend,
            "previous": mended.previous,
            "before": mended.before.as_deref().unwrap_or("head"),
            "replaces": mended.replaces,
        }));
    }
    for mend in &audit.log.idle_mends {
        lines.push(json!({"observation": "idle-mend", "mend": mend}));
    }
    for line in &lines {
        if !say(out, &line.to_string())? {
            return Ok(());
        }
    }
    Ok(())
}
