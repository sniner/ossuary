//! `ossuary maintain`: `weed` removes copies in `derived/` that
//! `content/` also has, `describe` records the size and type of files
//! that lack them, `tidy` removes temporary files of interrupted commands,
//! `set-aside` moves damaged files aside so that they can be stored again.

use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;
use ossuary_core::{Aside, Fixity, TEMP_MIN_AGE, Twin, Undescribing, Weeded};

use crate::audit::{listing, since_time};
use crate::output::{errln, human_bytes, say};
use crate::{Location, catch_up, finish_interrupted_seal, open};

/// The maintenance commands.
#[derive(Subcommand)]
pub(crate) enum Maintenance {
    /// Record the size and type of files that lack them
    ///
    /// A file can lack them after an interrupted run. Such files are left
    /// out of `ossuary extract`, and `ossuary audit` lists them. describe
    /// reads each of these files and records what is missing.
    Describe {
        /// Show which files would be described, write nothing
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
    /// kept: restore the file from a copy of the archive. No recorded
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
    /// Remove temporary files that interrupted commands left behind
    ///
    /// A command that is stopped before it has recorded its last batch of
    /// files, with Ctrl-C for example, leaves the files of that batch in
    /// the tmp/ directories of the archive. They are not part of the
    /// archive and only take space. tidy removes those older than 24
    /// hours; a younger one may belong to a command that is still running.
    Tidy {
        /// Show how many files would be removed, remove nothing
        #[arg(long)]
        dry_run: bool,
        /// Also remove temporary files younger than 24 hours; only when
        /// no other command is writing to the archive
        #[arg(long)]
        all: bool,
    },
    /// Move damaged files aside so that they can be stored again
    ///
    /// A file that is empty or cut short, after a crash of the computer
    /// or a power loss, still has its place in the archive, and adding
    /// the original again stores nothing. set-aside checks the files in
    /// content/ and derived/ against their hashes and renames each
    /// damaged one in place, with .corrupt added to its name. Nothing is
    /// deleted, and what is recorded about the file stays. Then it names
    /// the command that stores each file again: `ossuary ingest` for a
    /// file with a recorded path, `ossuary mailvault fetch --compare` for
    /// a mail message, `ossuary extract --full` for a derived file.
    SetAside {
        /// Show which files would be moved aside, write nothing
        #[arg(long)]
        dry_run: bool,
        /// Check only the files written since TIME: an RFC 3339 time
        /// like 2026-01-01T12:00:00Z, or a date
        #[arg(long, value_name = "TIME")]
        since: Option<String>,
    },
}

/// Run one maintenance task.
pub(crate) fn run(
    location: &Location,
    task: &Maintenance,
    verbose: bool,
    quiet: bool,
) -> Result<ExitCode> {
    match task {
        Maintenance::Describe { dry_run } => describe(location, *dry_run, verbose, quiet),
        Maintenance::Weed { dry_run, repair } => weed(location, *dry_run, *repair, verbose, quiet),
        Maintenance::Tidy { dry_run, all } => tidy(location, *dry_run, *all, quiet),
        Maintenance::SetAside { dry_run, since } => {
            set_aside(location, *dry_run, since.as_deref(), verbose, quiet)
        }
    }
}

/// `maintain set-aside`: move every damaged file aside, and name the
/// command that stores it again.
fn set_aside(
    location: &Location,
    dry_run: bool,
    since: Option<&str>,
    verbose: bool,
    quiet: bool,
) -> Result<ExitCode> {
    let since = since.map(since_time).transpose()?;
    let archive = open(location)?;
    if !quiet {
        errln!("archive {}", archive.root().display());
    }
    let mut index = archive.index()?;
    catch_up(&mut index, &archive, quiet)?;
    if !quiet {
        match &since {
            Some(since) => errln!(
                "checking the files in content/ and derived/ written since {} against their hashes",
                since.as_str()
            ),
            None => errln!("checking every file in content/ and derived/ against its hash"),
        }
    }
    let done = ossuary_core::set_aside(&archive, &index, since.as_ref(), dry_run)?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut unreadable = 0;
    for (place, store) in [("content/", &done.content), ("derived/", &done.derived)] {
        for (digest, error) in &store.unreadable {
            unreadable += 1;
            if !say(
                &mut out,
                &format!("{place}: could not read {digest}: {error}"),
            )? {
                return Ok(ExitCode::FAILURE);
            }
        }
    }
    let exit = if unreadable == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    };
    let read = done.content.verified + done.derived.verified;
    if done.files.is_empty() {
        say(
            &mut out,
            &format!("{read} file(s) checked, none damaged; nothing moved aside"),
        )?;
        return Ok(exit);
    }
    let all: Vec<&Aside> = done.files.iter().collect();
    let heading = if dry_run {
        format!(
            "{read} file(s) checked, {} damaged; would be moved aside, nothing written",
            all.len()
        )
    } else {
        format!(
            "{read} file(s) checked, {} damaged and moved aside; each is kept in its directory with .corrupt added to its name",
            all.len()
        )
    };
    if !listing(&mut out, &heading, &names(&all), verbose)? {
        return Ok(exit);
    }
    if !ways_back(&mut out, &all, verbose)? {
        return Ok(exit);
    }
    let next = if dry_run {
        "run `ossuary maintain set-aside` without --dry-run first"
    } else {
        "after that, `ossuary audit --fast` lists the files that are still missing"
    };
    say(&mut out, next)?;
    Ok(exit)
}

/// The names of `files`, for a listing.
fn names(files: &[&Aside]) -> Vec<String> {
    files
        .iter()
        .map(|file| file.digest.as_str().to_string())
        .collect()
}

/// For the files set aside, the command that stores each again: by where
/// the record says it came from. `Ok(false)` means the reader left.
fn ways_back(out: &mut impl std::io::Write, all: &[&Aside], verbose: bool) -> Result<bool> {
    let ingested: Vec<String> = all
        .iter()
        .filter(|file| !file.derived)
        .flat_map(|file| file.paths.iter().cloned())
        .collect();
    let from_files = all
        .iter()
        .filter(|file| !file.derived && !file.paths.is_empty())
        .count();
    if from_files > 0 {
        let heading = format!(
            "{from_files} of them added with `ossuary ingest`; `ossuary ingest DIR` on the directories they are in stores them again. Recorded at"
        );
        if !listing(out, &heading, &ingested, verbose)? {
            return Ok(false);
        }
    }
    let mail = all.iter().filter(|file| !file.derived && file.mail).count();
    if mail > 0
        && !say(
            out,
            &format!(
                "{mail} of them mail message(s); `ossuary mailvault fetch --compare` fetches them again"
            ),
        )?
    {
        return Ok(false);
    }
    let derived = all.iter().filter(|file| file.derived).count();
    if derived > 0
        && !say(
            out,
            &format!(
                "{derived} of them derived file(s); `ossuary extract --full` creates them again"
            ),
        )?
    {
        return Ok(false);
    }
    let other: Vec<&Aside> = all
        .iter()
        .copied()
        .filter(|file| !file.derived && !file.mail && file.paths.is_empty())
        .collect();
    if !other.is_empty() {
        let heading = format!(
            "{} of them without a recorded path or mailbox; the command that added them stores them again",
            other.len()
        );
        if !listing(out, &heading, &names(&other), verbose)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// `maintain tidy`: remove the temporary files of interrupted commands.
fn tidy(location: &Location, dry_run: bool, all: bool, quiet: bool) -> Result<ExitCode> {
    let archive = open(location)?;
    if !quiet {
        errln!("archive {}", archive.root().display());
    }
    let min_age = (!all).then_some(TEMP_MIN_AGE);
    let tidied = ossuary_core::tidy(&archive, min_age, dry_run)?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let size = human_bytes(tidied.bytes);
    let verdict = match (tidied.removed, dry_run) {
        (0, _) if tidied.young == 0 => "no temporary files in the archive".to_string(),
        (0, _) => "no temporary files older than 24 hours".to_string(),
        (count, true) => format!("would remove {count} temporary file(s), {size}; nothing removed"),
        (count, false) => format!("{count} temporary file(s) removed, {size}"),
    };
    say(&mut out, &verdict)?;
    if tidied.young > 0 {
        say(
            &mut out,
            &format!(
                "{} temporary file(s) younger than 24 hours kept; a running command may be writing them. If no command is writing to the archive, pass --all to remove them",
                tidied.young
            ),
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `maintain describe`: find the files recorded without size or type,
/// read each one and record what it lacks.
fn describe(location: &Location, dry_run: bool, verbose: bool, quiet: bool) -> Result<ExitCode> {
    let archive = open(location)?;
    if !quiet {
        errln!("archive {}", archive.root().display());
    }
    if !dry_run {
        finish_interrupted_seal(&archive)?;
    }
    if !quiet {
        errln!(
            "step 1 of 2: reading every claim and listing content/ and derived/, to find files without size or type"
        );
    }
    let files = match ossuary_core::undescribed(&archive)? {
        Undescribing::Found(files) => files,
        Undescribing::Refused(log) => {
            errln!(
                "no files described: {} finding(s) in claims/; `ossuary audit` lists them",
                log.findings()
            );
            return Ok(ExitCode::FAILURE);
        }
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if files.is_empty() {
        say(
            &mut out,
            "every recorded file has its size and type; nothing to describe",
        )?;
        return Ok(ExitCode::SUCCESS);
    }
    let names: Vec<String> = files
        .iter()
        .map(|file| file.digest.as_str().to_string())
        .collect();
    if dry_run {
        let heading = format!(
            "would record the size or type of {} file(s); nothing written",
            files.len()
        );
        listing(&mut out, &heading, &names, verbose)?;
        return Ok(ExitCode::SUCCESS);
    }
    if !quiet {
        errln!(
            "step 2 of 2: reading {} file(s) in the archive",
            files.len()
        );
    }
    let described = ossuary_core::describe(&archive, &files)?;
    if !described.gone.is_empty() {
        let heading = format!(
            "{} file(s) no longer in content/ or derived/; `ossuary audit` shows what is missing",
            described.gone.len()
        );
        if !listing(&mut out, &heading, &described.gone, true)? {
            return Ok(ExitCode::FAILURE);
        }
    }
    if described.claims == 0 {
        say(&mut out, "0 file(s) described, nothing written")?;
    } else {
        say(
            &mut out,
            &format!(
                "{} file(s) described, {} claim(s) written, run {}; `ossuary extract` now examines them",
                described.files, described.claims, described.run
            ),
        )?;
    }
    if described.gone.is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
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
fn weed(
    location: &Location,
    dry_run: bool,
    repair: bool,
    verbose: bool,
    quiet: bool,
) -> Result<ExitCode> {
    let archive = open(location)?;
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
        verdict.push_str("; nothing written");
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
        (Fixity::Unchecked, _) | (_, Fixity::Unchecked) => {
            "not checked against its hash; left unchanged".to_string()
        }
        (Fixity::Damaged, Fixity::Damaged) => {
            "damaged in both content/ and derived/; left unchanged, restore it from a copy of the archive"
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
