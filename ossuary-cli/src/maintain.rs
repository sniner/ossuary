//! `ossuary maintain`: the one taking-out that loses nothing.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;
use ossuary_core::{Fixity, Twin, Weeded};

use crate::open;
use crate::output::{errln, say};

/// The maintenance commands. `weed` removes copies in `derived/` of
/// files that `content/` also contains.
#[derive(Subcommand)]
pub(crate) enum Maintenance {
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
        Maintenance::Weed { dry_run, repair } => weed(root, *dry_run, *repair, verbose, quiet),
    }
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
        errln!("archive {}", archive.root().display());
        errln!(
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
