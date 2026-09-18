//! `ossuary maintain`: repairs that add to the archive and rewrite
//! nothing, and the one taking-out that loses nothing.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;
use ossuary_core::{Break, Cause, Fixity, Timestamp, Twin, Weeded};

use crate::{open, say};

/// What `ossuary maintain` can do. Every repair adds to the archive
/// and rewrites nothing — the record of what went wrong stays. The one
/// thing that goes is a copy in `derived/` of what `content/` holds
/// too, which no claim and no reader ever reaches.
#[derive(Subcommand)]
pub(crate) enum Maintenance {
    /// Join the pieces of a broken chain, and keep the break on the
    /// record
    ///
    /// Where `audit` finds the chain of sealed segments in more than one
    /// piece, a head lost and begun anew or a sealed segment gone, this
    /// closes each break with a mend: a segment of no claims that names
    /// the two ends it joins, stored like any other. Nothing already
    /// sealed is touched, and the segment after the break still names
    /// what it named, so the record keeps saying what was lost and where.
    /// What was lost stays lost: take it in again first, if it can be
    /// had. A break behind a segment that is held but damaged is left
    /// alone; a mend stands in for what is gone, not for what is damaged.
    /// Reads the whole log, as the audit does. Exits 1 when a break was
    /// left open.
    Mend {
        /// Say what would be mended and write nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Take out of derived/ what content/ holds as well
    ///
    /// A file won as a derived file, an attachment out of a mail say,
    /// and later taken in as an original stands in both stores under
    /// the same name, and content/ answers for it first: the copy in
    /// derived/ answers nothing, and no claim names a store. Both
    /// copies are read whole and proved against their name before
    /// anything goes. Both sound, the copy in derived/ is taken out;
    /// damaged in derived/ and sound in content/, taken out as well.
    /// Damaged in content/ and sound in derived/, it stays and says so;
    /// --repair sets the damaged original aside under a name of its
    /// own, every byte kept, and stores the sound bytes in its place.
    /// Damaged in both stores, or unreadable in either, it stays:
    /// restore it from a copy of the archive. Nothing the claims speak
    /// of goes missing at any step. Exits 1 when a file was left
    /// standing.
    Weed {
        /// Say what would be taken out and write nothing
        #[arg(long)]
        dry_run: bool,
        /// Where the original is damaged and the copy in derived/ sound,
        /// set the original aside and store the sound bytes in its place
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
        eprintln!("reading every sealed segment and the open head");
    }
    let log = ossuary_core::audit_log(archive.log())?;
    if log.chains.len() <= 1 {
        let line = "the chain is whole, nothing to mend";
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
            "the chain is in {} pieces that no break explains; `ossuary audit` lists the pieces",
            log.chains.len()
        );
        return Ok(ExitCode::FAILURE);
    }
    let mut left_open = 0;
    for (index, brk) in log.breaks.iter().enumerate() {
        println!("break {}: {}", index + 1, describe(brk));
        if !brk.sure {
            println!("  left open; which chain stands right before this one is not certain");
            left_open += 1;
            continue;
        }
        if !brk.mendable() {
            println!(
                "  left open; the segment is held but damaged; restore it from a copy of the archive"
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
                "  mended as {}; the open head now follows the mend",
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
    let before = brk.before.as_deref().unwrap_or("the open head");
    match &brk.cause {
        Cause::HeadLost => format!(
            "after {} and before {before}, a head was lost, its claims with it; nothing recorded between {} and {} survived",
            brk.after,
            when(brk.from.as_ref()),
            when(brk.to.as_ref()),
        ),
        Cause::SegmentLost(segment) => format!(
            "after {} and before {before}, segment {segment} is not held; the mend keeps its name on the record",
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
            "looking through derived/ for files content/ holds too, both copies read whole and proved against their name"
        );
    }
    let twins = ossuary_core::twins(&archive)?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if twins.is_empty() {
        say(&mut out, "no file is held by both stores; nothing to weed")?;
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
                    "both copies true to their names"
                };
                if dry_run {
                    format!("{digest}: {how}; would be taken out of derived/")
                } else {
                    format!("{digest}: {how}; taken out of derived/")
                }
            }
            Weeded::Repaired { aside } => {
                repaired += 1;
                if dry_run {
                    format!(
                        "{digest}: damaged in content/, sound in derived/; would be set aside and the sound bytes stored in its place"
                    )
                } else {
                    // The run named the archive once; a file inside it
                    // reads as it reads inside it.
                    let aside = aside.strip_prefix(archive.root()).unwrap_or(&aside);
                    format!(
                        "{digest}: damaged in content/, sound in derived/; the original set aside as {}, the sound bytes stored in its place, the copy in derived/ taken out",
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
        format!("{twins} file(s) held by both stores"),
        format!("{released} {would}taken out of derived/"),
    ];
    if repaired > 0 {
        clauses.push(format!(
            "{repaired} {would}repaired from the copy in derived/"
        ));
    }
    if standing > 0 {
        clauses.push(format!("{standing} left standing"));
    }
    let mut verdict = clauses.join(", ");
    if dry_run {
        verdict.push_str("; --dry-run, nothing written");
    } else if released + repaired > 0 {
        verdict.push_str("; content/ answers for every one of them");
    }
    verdict
}

/// Why a file held by both stores was left as it is, and what leads on
/// from there.
fn standing_reason(twin: &Twin, repair: bool) -> String {
    match (&twin.content, &twin.derived) {
        (Fixity::Unreadable(error), _) => {
            format!("could not read the copy in content/: {error}; left standing")
        }
        (_, Fixity::Unreadable(error)) => {
            format!("could not read the copy in derived/: {error}; left standing")
        }
        (Fixity::Damaged, Fixity::Damaged) => {
            "damaged in both stores; left standing, restore it from a copy of the archive"
                .to_string()
        }
        (Fixity::Damaged, Fixity::Sound) if repair => {
            "damaged in content/, sound in derived/; left standing, the copy in derived/ went away under this run"
                .to_string()
        }
        (Fixity::Damaged, Fixity::Sound) => {
            "damaged in content/, sound in derived/; left standing, `ossuary maintain weed --repair` stores the sound bytes in the original's place"
                .to_string()
        }
        // A sound original is releasable whatever the derived copy is,
        // short of unreadable, and those cases are answered above.
        (Fixity::Sound, _) => "left standing".to_string(),
    }
}
