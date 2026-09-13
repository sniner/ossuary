//! `ossuary maintain`: repairs that add to the archive and rewrite
//! nothing.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;
use ossuary_core::{Break, Cause, Timestamp};

use crate::open;

/// What `ossuary maintain` can do. Every repair adds to the archive
/// and rewrites nothing — the record of what went wrong stays.
#[derive(Subcommand)]
pub(crate) enum Maintenance {
    /// Join the pieces of a broken chain, and keep the break on the
    /// record
    ///
    /// Where `audit` finds the chain of sealed segments in more than
    /// one piece — a head lost and begun anew, or a sealed segment
    /// gone — this closes each break with a mend: a segment of no
    /// claims that names the two ends it joins, stored like any other.
    /// Nothing already sealed is touched, and the segment after the
    /// break still names what it named, so the record keeps saying
    /// what was lost and where. What was lost stays lost: take it in
    /// again first, if it can be had. A break behind a segment that is
    /// held but damaged is left alone — a mend stands in for what is
    /// gone, not for what is damaged. Reads the whole log, as the audit
    /// does. Exits 1 when a break was left open.
    Mend {
        /// Say what would be mended and write nothing
        #[arg(long)]
        dry_run: bool,
    },
}

/// Run one maintenance task.
pub(crate) fn run(root: &Path, task: &Maintenance, quiet: bool) -> Result<ExitCode> {
    match task {
        Maintenance::Mend { dry_run } => mend(root, *dry_run, quiet),
    }
}

/// `maintain mend`: every break the audit finds in the chain, closed
/// with a mend where one can stand.
fn mend(root: &Path, dry_run: bool, quiet: bool) -> Result<ExitCode> {
    let archive = open(root)?;
    if !quiet {
        eprintln!("archive {}", archive.root().display());
        eprintln!(
            "reading every sealed segment and the open head — the chain is proved from the archive itself, never from the cache"
        );
    }
    let log = ossuary_core::audit_log(archive.log())?;
    if log.chains.len() <= 1 {
        let line = "the chain is whole — nothing to mend";
        if log.mended.is_empty() {
            println!("{line}");
        } else {
            println!(
                "{line}; {} mended break(s) stand on the record",
                log.mended.len()
            );
        }
        return Ok(ExitCode::SUCCESS);
    }
    if log.breaks.is_empty() {
        println!(
            "the chain is in {} pieces that no break explains — a segment follows one that is on no chain; `ossuary audit` lists the pieces",
            log.chains.len()
        );
        return Ok(ExitCode::FAILURE);
    }
    let mut left_open = 0;
    for (index, brk) in log.breaks.iter().enumerate() {
        println!("break {}: {}", index + 1, describe(brk));
        if !brk.sure {
            println!(
                "  left open — which chain stands right before this one is not certain, their first claims share a second; a mend between the wrong ends would be worse than the break"
            );
            left_open += 1;
            continue;
        }
        if !brk.mendable() {
            println!(
                "  left open — a mend stands in for what is gone, not for what is held and damaged; restore the segment from a copy of the archive"
            );
            left_open += 1;
            continue;
        }
        if dry_run {
            println!("  would be mended — --dry-run, nothing written");
            continue;
        }
        match ossuary_core::mend(archive.log(), brk)? {
            Some(segment) if brk.before.is_none() => println!(
                "  mended as {} — the open head now follows the mend",
                segment.digest()
            ),
            Some(segment) => println!("  mended as {}", segment.digest()),
            None => {
                // `mendable` said yes a moment ago; the archive did not
                // change under this run's feet in a way that turns a
                // break unmendable, but the report is honest either way.
                println!("  left open — the break could not be mended");
                left_open += 1;
            }
        }
    }
    if left_open > 0 {
        Ok(ExitCode::FAILURE)
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

/// A break in a line: its two ends, and what the record says happened
/// between them.
fn describe(brk: &Break) -> String {
    let before = brk.before.as_deref().unwrap_or("the open head");
    match &brk.cause {
        Cause::HeadLost => format!(
            "after {} and before {before}, a head was lost, its claims with it — nothing recorded between {} and {} survived",
            brk.after,
            when(brk.from.as_ref()),
            when(brk.to.as_ref()),
        ),
        Cause::SegmentLost(segment) => format!(
            "after {} and before {before}, segment {segment} is not held — the mend keeps its name on the record",
            brk.after,
        ),
        Cause::SegmentUnreadable(segment) => format!(
            "after {} and before {before}, segment {segment} is held but will not read back",
            brk.after,
        ),
    }
}

/// A claim time for a line, or the word for none.
fn when(time: Option<&Timestamp>) -> &str {
    time.map_or("no claim", Timestamp::as_str)
}
