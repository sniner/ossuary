//! `ossuary maintain`: repairs that add to the archive and rewrite
//! nothing, and the one taking-out that loses nothing.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;
use ossuary_core::{Break, Cause, Fixity, Timestamp, Twin, Weeded};

use crate::{open, say};

/// The maintenance commands. `mend` only adds to the archive and
/// rewrites nothing; `weed` removes copies in `derived/` of files that
/// `content/` also contains.
#[derive(Subcommand)]
pub(crate) enum Maintenance {
    /// Join a broken chain of sealed segments and record each break
    ///
    /// Where `audit` finds the chain of sealed segments in more than one
    /// piece, because an open segment was lost and started again or a
    /// sealed segment is missing, mend closes each break with a mend: a
    /// segment without claims that names the two segments it joins.
    /// Sealed segments are not changed, and the segment after the break
    /// still names its original predecessor, so the record still shows
    /// what was lost and where. Lost claims are not recovered; record
    /// them again first if possible. A break after a segment that is
    /// present but damaged is left open. Reads the whole log, like
    /// `audit`. Exits 1 if a break was left open.
    Mend {
        /// Show what would be mended, write nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove files from derived/ that are also in content/
    ///
    /// A file first extracted as a derived file (an attachment from a
    /// mail, for example) and later ingested as an original is stored in
    /// both content/ and derived/ under the same name. Only the copy in
    /// content/ is used. weed checks both copies against their hash
    /// before removing anything. If both are sound, or only the copy in
    /// derived/ is damaged, the copy in derived/ is removed. If the copy
    /// in content/ is damaged and the one in derived/ is sound, both are
    /// kept; --repair moves the damaged file aside, keeping all its
    /// bytes, stores the sound copy in its place and removes the copy in
    /// derived/. If both are damaged, or either cannot be read, both are
    /// kept: restore the file from a backup of the archive. No recorded
    /// file is lost at any step. Exits 1 if a file was left in both.
    Weed {
        /// Show what would be removed, write nothing
        #[arg(long)]
        dry_run: bool,
        /// If the copy in content/ is damaged and the one in derived/ is
        /// sound, move the damaged file aside and store the sound copy
        /// in its place
        #[arg(long)]
        repair: bool,
    },
}

/// Run one maintenance task.
pub(crate) fn run(root: &Path, task: &Maintenance, verbose: bool, quiet: bool) -> Result<ExitCode> {
    match task {
        Maintenance::Mend { dry_run } => mend(root, *dry_run, quiet),
        Maintenance::Weed { dry_run, repair } => weed(root, *dry_run, *repair, verbose, quiet),
    }
}

/// `maintain mend`: every break the audit finds in the chain, closed
/// with a mend where one can stand.
fn mend(root: &Path, dry_run: bool, quiet: bool) -> Result<ExitCode> {
    let archive = open(root)?;
    if !quiet {
        eprintln!("archive {}", archive.root().display());
        eprintln!("reading every sealed segment and the open segment");
    }
    let log = ossuary_core::audit_log(archive.log())?;
    if log.chains.len() <= 1 {
        let line = "the chain is complete, nothing to mend";
        if log.mended.is_empty() {
            println!("{line}");
        } else {
            println!("{line}; {} break(s) mended earlier", log.mended.len());
        }
        return Ok(ExitCode::SUCCESS);
    }
    if log.breaks.is_empty() {
        println!(
            "the chain is in {} pieces with no identifiable break; `ossuary audit` lists the pieces",
            log.chains.len()
        );
        return Ok(ExitCode::FAILURE);
    }
    let mut left_open = 0;
    for (index, brk) in log.breaks.iter().enumerate() {
        println!("break {}: {}", index + 1, describe(brk));
        if !brk.sure {
            println!("  left open; the chain before this one cannot be determined");
            left_open += 1;
            continue;
        }
        if !brk.mendable() {
            println!(
                "  left open; the segment is present but damaged; restore it from a backup of the archive"
            );
            left_open += 1;
            continue;
        }
        if dry_run {
            println!("  would be mended; --dry-run, nothing written");
            continue;
        }
        match ossuary_core::mend(archive.log(), brk)? {
            Some(segment) if brk.before.is_none() => println!(
                "  mended as {}; the open segment now follows the mend",
                segment.digest()
            ),
            Some(segment) => println!("  mended as {}", segment.digest()),
            None => {
                // `mendable` said yes a moment ago; the archive did not
                // change under this run's feet in a way that turns a
                // break unmendable, but the report is honest either way.
                println!("  left open; the break could not be mended");
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
    let before = brk.before.as_deref().unwrap_or("the open segment");
    match &brk.cause {
        Cause::HeadLost => format!(
            "after {} and before {before}, an open segment was lost with its claims; the claims recorded between {} and {} are lost",
            brk.after,
            when(brk.from.as_ref()),
            when(brk.to.as_ref()),
        ),
        Cause::SegmentLost(segment) => format!(
            "after {} and before {before}, segment {segment} is missing; the mend records its name",
            brk.after,
        ),
        Cause::SegmentUnreadable(segment) => format!(
            "after {} and before {before}, segment {segment} is present but unreadable",
            brk.after,
        ),
    }
}

/// A claim time for a line, or the word for none.
fn when(time: Option<&Timestamp>) -> &str {
    time.map_or("no claim", Timestamp::as_str)
}

/// `maintain weed`: every file held by both stores, proved on both sides
/// and let go of where that loses nothing. The taken-out are counted and
/// named under --verbose; whatever stays standing is named, with the way
/// forward.
#[allow(
    clippy::fn_params_excessive_bools,
    reason = "two switches of the verb and two of the run, each one flag the user set"
)]
fn weed(root: &Path, dry_run: bool, repair: bool, verbose: bool, quiet: bool) -> Result<ExitCode> {
    let archive = open(root)?;
    if !quiet {
        eprintln!("archive {}", archive.root().display());
        eprintln!(
            "looking for files in both content/ and derived/, checking both copies against their hash"
        );
    }
    let twins = ossuary_core::twins(&archive)?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if twins.is_empty() {
        say(
            &mut out,
            "no file is in both content/ and derived/; nothing to weed",
        )?;
        return Ok(ExitCode::SUCCESS);
    }
    let mut released = 0;
    let mut repaired = 0;
    let mut standing = 0;
    for twin in &twins {
        let digest = twin.digest.as_str();
        let outcome = if dry_run {
            if twin.releasable() {
                Weeded::Released
            } else if repair && twin.repairable() {
                Weeded::Repaired {
                    aside: std::path::PathBuf::new(),
                }
            } else {
                Weeded::Standing
            }
        } else {
            ossuary_core::weed(&archive, twin, repair)?
        };
        let line = match outcome {
            Weeded::Released => {
                released += 1;
                if !verbose {
                    continue;
                }
                let how = if twin.derived == Fixity::Damaged {
                    "damaged in derived/, sound in content/"
                } else {
                    "both copies sound"
                };
                if dry_run {
                    format!("{digest}: {how}; would be removed from derived/")
                } else {
                    format!("{digest}: {how}; removed from derived/")
                }
            }
            Weeded::Repaired { aside } => {
                repaired += 1;
                if dry_run {
                    format!(
                        "{digest}: damaged in content/, sound in derived/; the damaged copy would be moved aside and replaced with the sound one"
                    )
                } else {
                    // The run named the archive once; a file inside it
                    // reads as it reads inside it.
                    let aside = aside.strip_prefix(archive.root()).unwrap_or(&aside);
                    format!(
                        "{digest}: damaged in content/, sound in derived/; damaged copy moved to {}, replaced with the sound copy, copy in derived/ removed",
                        aside.display()
                    )
                }
            }
            Weeded::Standing => {
                standing += 1;
                format!("{digest}: {}", standing_reason(twin, repair))
            }
        };
        if !say(&mut out, &line)? {
            return Ok(ExitCode::SUCCESS);
        }
    }
    say(
        &mut out,
        &weed_verdict(twins.len(), released, repaired, standing, dry_run),
    )?;
    if standing > 0 {
        Ok(ExitCode::FAILURE)
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

/// The last line of a weeding: the counts, and which clean outcome it
/// is — nothing written, or `content/` now answering alone.
fn weed_verdict(
    twins: usize,
    released: usize,
    repaired: usize,
    standing: usize,
    dry_run: bool,
) -> String {
    let would = if dry_run { "would be " } else { "" };
    let mut clauses = vec![
        format!("{twins} file(s) in both content/ and derived/"),
        format!("{released} {would}removed from derived/"),
    ];
    if repaired > 0 {
        clauses.push(format!(
            "{repaired} {would}repaired from the copy in derived/"
        ));
    }
    if standing > 0 {
        clauses.push(format!("{standing} left unchanged"));
    }
    let mut verdict = clauses.join(", ");
    if dry_run {
        verdict.push_str("; --dry-run, nothing written");
    } else if released + repaired > 0 {
        verdict.push_str("; every file is still in content/");
    }
    verdict
}

/// Why a file held by both stores was left as it is, and what leads on
/// from there.
fn standing_reason(twin: &Twin, repair: bool) -> String {
    match (&twin.content, &twin.derived) {
        (Fixity::Unreadable(error), _) => {
            format!("could not read the copy in content/: {error}; left unchanged")
        }
        (_, Fixity::Unreadable(error)) => {
            format!("could not read the copy in derived/: {error}; left unchanged")
        }
        (Fixity::Damaged, Fixity::Damaged) => {
            "damaged in both content/ and derived/; left unchanged, restore it from a backup of the archive"
                .to_string()
        }
        (Fixity::Damaged, Fixity::Sound) if repair => {
            "damaged in content/, sound in derived/; left unchanged, the copy in derived/ disappeared during this run"
                .to_string()
        }
        (Fixity::Damaged, Fixity::Sound) => {
            "damaged in content/, sound in derived/; left unchanged, `ossuary maintain weed --repair` replaces the damaged copy with the sound one"
                .to_string()
        }
        // A sound original is releasable whatever the derived copy is,
        // short of unreadable, and those cases are answered above.
        (Fixity::Sound, _) => "left unchanged".to_string(),
    }
}
