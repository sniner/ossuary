//! The fixer: the one program that changes what an archive has sealed.
//!
//! `ossuary` never changes a digest: it only adds to an archive. Some
//! repairs cannot be made that way. A word in the vocabulary changed, a
//! header member changed its form, a segment was lost and the chain has
//! to be closed over it, the archive is to move to another hash. Those
//! are done here. A fix that adds claims is a function from the log to a
//! [`Plan`], the claims it would append and the words for saying so;
//! [`record`] reads the log whole and replays an attribute's standing
//! set, and [`plan`] appends what a plan holds. A fix that changes what
//! is sealed is a function from the archive to a
//! [`Rewrite`](rewrite::Rewrite); [`rewrite`] reads every segment, works
//! out the new names along the chain, and writes the result. Its result
//! is in effect a new archive: every seal given out for a renamed
//! segment no longer checks. This file maps each subcommand to its fix.

// `assert!(x.is_empty())` reads as the statement it makes; the form
// clippy suggests, `assert_eq!(x, [] as [T; 0])`, says the same and
// names the type for nothing.
#![cfg_attr(test, allow(clippy::assert_is_empty))]

mod fixes;
mod plan;
mod record;
mod rewrite;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Result, anyhow, bail};
use clap::{Parser, Subcommand};
use ossuary_core::{Algorithm, Archive, Error};

use crate::plan::Plan;

#[derive(Parser)]
#[command(
    name = "ossuary-fix",
    version,
    about = "Repair ossuary archives and convert archives of older versions to the current format",
    after_help = "Each fix reads the whole log and writes only what is missing or in an old form.
Running a fix again writes nothing. A fix that rewrites sealed segments gives
them new names, so seal ids issued before no longer pass `ossuary seal --check`."
)]
struct Cli {
    /// The archive to work in
    #[arg(
        long,
        global = true,
        value_name = "DIR",
        env = "OSSUARY_ARCHIVE",
        default_value = "."
    )]
    archive: PathBuf,

    /// The directory of the archive's caches, when they are not kept in
    /// the archive
    #[arg(long, global = true, value_name = "DIR", env = "OSSUARY_CACHE")]
    cache: Option<PathBuf>,

    /// Show what would be written, write nothing
    #[arg(long, global = true)]
    dry_run: bool,

    #[command(subcommand)]
    command: Command,
}

/// The fixes. The help text of each says what it is for, what it
/// writes, and what it leaves unchanged.
#[derive(Subcommand)]
enum Command {
    /// Copy standing `derive:derived-from` to `prov:origin`
    ///
    /// Until 0.6.3 the origin of a derived file was recorded as
    /// `derive:derived-from`; it is now `prov:origin`. A derived file
    /// whose origin is recorded only under the old attribute is not
    /// shown by `find`, `ls` or `ossuary-mount`. This fix copies each
    /// standing `derive:derived-from` value to `prov:origin`, with the
    /// time, source and run of the old claim. The old claims are not
    /// changed. Origins already recorded as `prov:origin` are skipped.
    Origin,
    /// Copy standing `zip:entry` and `zip:path` to `packed:path` and `file:path`
    ///
    /// Until 0.7.0 the packed extractor read only zip files and used the
    /// `zip` namespace: the entries of a zip file were recorded on it as
    /// `zip:entry`, the path of an unpacked entry was recorded on the
    /// entry as `zip:path`. Paths inside a packed file now begin with
    /// `@` and are recorded as `packed:path` on the packed file and as
    /// `file:path` on the entry, for every format, so one `find` term
    /// matches both. This fix copies each standing `zip:entry` value to
    /// `packed:path` and each standing `zip:path` value to `file:path`,
    /// with a leading `@` and the time, source and run of the old claim.
    /// The old claims are not changed. Paths already recorded under the
    /// new attribute are skipped.
    Packed,
    /// Rewrite an archive of 0.14 or earlier into the current form
    ///
    /// Until 0.14 a segment header named the segments sealed before it
    /// as a list, so that a segment could follow several: after `ossuary
    /// merge`, or after `ossuary maintain mend` closed a break. Both
    /// commands were removed, and the current version reads only a
    /// header that names one segment: `"previous":"3c1e…"`. Such an
    /// archive was read in the order of its segments' first claims; this
    /// fix links the segments in that order, so the claims are read in
    /// the same order as before. Segments without claims, the mends and
    /// the segments that joined merged archives, are removed. An archive
    /// in the current form is left as it is.
    ///
    /// A segment that is missing and that no mend records as lost, and a
    /// missing open segment, are lost with their claims; the fix goes on
    /// only with --accept-loss. Copy a missing file back from a copy of
    /// the archive instead, if there is one.
    ///
    /// A sealed segment is named by its hash, so each rewritten segment
    /// gets a new name, and every segment after it is rewritten to name
    /// it. Claims are not changed. The query index in cache/ is removed
    /// and rebuilt by the next command. Make a copy of the archive first.
    List {
        /// Continue although a segment or the open segment is missing;
        /// their claims are lost
        #[arg(long)]
        accept_loss: bool,
    },
    /// Bring an archive of 0.18 or earlier into the current form
    ///
    /// Until 0.17 the open segment was one file, head.jsonl, and a
    /// retraction contained the value it retracted; 0.18 kept the open
    /// segment as a directory in the archive root. The open segment is
    /// now a directory under open/, and a retraction names the claims it
    /// retracts by id. This fix moves the open segment into open/,
    /// rewrites every retraction to name the claims that were standing
    /// when it was written, leaves out retractions that had nothing to
    /// retract, and seals the open segment. Assertions are not changed.
    /// Other commands do not open an archive in an older format; run
    /// this fix first.
    ///
    /// The segments from the first rewritten retraction on get new
    /// names, so seal ids issued for them no longer pass
    /// `ossuary seal --check`. The query index in cache/ is removed and
    /// rebuilt by the next command. Make a copy of the archive first.
    Open,
    /// Close a broken chain of sealed segments
    ///
    /// If a sealed segment is missing and no copy of the archive has it,
    /// the segment after the gap is linked to the one before it. A
    /// missing open segment is replaced by a new, empty one after the
    /// last sealed segment. Their claims are lost, so the fix continues
    /// only with --accept-loss; if a copy of the archive has the file,
    /// copy it back instead. Segments that are still linked keep their
    /// order. Segments not linked to the rest are moved to the end of the
    /// chain and listed. If the chain is broken in more than one place
    /// and the order of the pieces is unknown, the fix stops.
    ///
    /// The segments after a gap get new names, so seal ids issued for
    /// them no longer pass `ossuary seal --check`. The query index in
    /// cache/ is removed and rebuilt by the next command. Make a copy of
    /// the archive first.
    Chain {
        /// Continue although a segment or the open segment is missing;
        /// their claims are lost
        #[arg(long)]
        accept_loss: bool,
    },
    /// Copy the archive into a new one that uses another hash algorithm
    ///
    /// Every file in content/ and derived/ is hashed again with the
    /// given algorithm and stored in a new archive at TARGET, which must
    /// be empty or not exist. Every claim is written there with the new
    /// names, in the order of the log, with its time, source and run
    /// unchanged. config.toml, mailvault.toml and the fetch state of
    /// ossuary-mailvault in cache/ are copied. The archive itself is only
    /// read. A damaged file, or a claim about a file that is not in the
    /// archive, stops the run; --force leaves them out.
    Rehash {
        /// The hash algorithm of the new archive, one that `ossuary init
        /// --algorithm` takes
        #[arg(long, value_name = "NAME")]
        algorithm: Algorithm,
        /// Where the new archive is created
        #[arg(value_name = "TARGET")]
        target: PathBuf,
        /// Leave out damaged files and claims about files not in the
        /// archive
        #[arg(long)]
        force: bool,
    },
    /// Remove the claims of extractors and the derived files only they recorded
    ///
    /// Removes every claim written by `ossuary extract`: what the
    /// extractors found in the files, the records that a file was
    /// examined, and the claims about derived files. A derived file that
    /// no other claim names is removed from derived/. Afterwards the
    /// archive is as it was before `ossuary extract` first ran, and the
    /// next run examines every file again. Use it when an extractor
    /// writes its values in another form than before and the old values
    /// are not to stay in the archive.
    ///
    /// Claims of other sources are not changed. A retraction by another
    /// source is removed when every claim it names is removed. A derived
    /// file that another claim names stays in derived/, without its
    /// recorded size and type; `ossuary maintain describe` records both
    /// again. Files in content/ are not touched.
    ///
    /// The segments from the first one with a removed claim on get new
    /// names, so seal ids issued for them no longer pass
    /// `ossuary seal --check`. The query index in cache/ is removed and
    /// rebuilt by the next command. Make a copy of the archive first.
    Unextract {
        /// Remove only the claims of these extractors: a source as the
        /// claims name it (extractor:mail/1), or without its generation
        /// (extractor:mail) for every generation. `--dry-run` lists the
        /// sources in the archive
        #[arg(value_name = "SOURCE")]
        sources: Vec<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("ossuary-fix: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// The one path every fix takes: open, plan, write unless rehearsing,
/// say.
fn run(cli: &Cli) -> Result<ExitCode> {
    let archive = match cli.command {
        // The one fix that reads the form before 0.18.
        Command::Open => Archive::open_unchecked(&cli.archive).map_err(|error| match error {
            Error::NoArchive(path) => anyhow!(
                "{}: not an ossuary archive; run this in an archive or name one with --archive",
                path.display()
            ),
            other => other.into(),
        })?,
        _ => open(&cli.archive)?,
    };
    let archive = match &cli.cache {
        Some(dir) => archive.with_cache(dir),
        None => archive,
    };
    let (sentence, seals) = match cli.command {
        Command::Origin => (
            claims(cli, &archive, &fixes::origin::plan(archive.log())?)?,
            None,
        ),
        Command::Packed => (
            claims(cli, &archive, &fixes::packed::plan(archive.log())?)?,
            None,
        ),
        Command::List { accept_loss } => {
            let (rewrite, found) = fixes::list::plan(&archive)?;
            lossy(cli, &archive, &rewrite, &found, accept_loss)?
        }
        Command::Chain { accept_loss } => {
            let (rewrite, found) = fixes::chain::plan(&archive)?;
            lossy(cli, &archive, &rewrite, &found, accept_loss)?
        }
        Command::Open => {
            let (rewrite, converted) = fixes::open::plan(&archive)?;
            let mut sentence = segments(cli, &archive, &rewrite)?;
            if let Some(words) = converted.sentence(cli.dry_run) {
                sentence.push_str("; ");
                sentence.push_str(&words);
            }
            // The moved open segment is one bundle of everything it held,
            // which is a segment's worth: sealed here rather than left for
            // the next writer.
            if rewrite.changes() {
                if cli.dry_run {
                    sentence.push_str("; the open segment would be sealed");
                } else if archive.log().seal()?.is_some() {
                    sentence.push_str("; the open segment sealed");
                }
            }
            (sentence, rewrite.seals(cli.dry_run))
        }
        Command::Rehash {
            algorithm,
            ref target,
            force,
        } => (rehash(cli, &archive, target, algorithm, force)?, None),
        Command::Unextract { ref sources } => {
            let (rewrite, removed) = fixes::unextract::plan(&archive, sources)?;
            for line in removed.lines() {
                println!("{line}");
            }
            if !cli.dry_run {
                removed.apply(&archive)?;
            }
            let mut sentence = segments(cli, &archive, &rewrite)?;
            if rewrite.changes() && !cli.dry_run {
                // A file without size or type is skipped by `ossuary extract`.
                sentence.push_str(if removed.kept > 0 {
                    "; run `ossuary maintain describe`, then `ossuary extract` to examine the files again"
                } else {
                    "; run `ossuary extract` to examine the files again"
                });
            }
            (sentence, rewrite.seals(cli.dry_run))
        }
    };
    println!("{sentence}");
    if let Some(seals) = seals {
        eprintln!("{seals}");
    }
    Ok(ExitCode::SUCCESS)
}

/// A fix that adds claims: appended unless rehearsing.
fn claims(cli: &Cli, archive: &Archive, plan: &Plan) -> Result<String> {
    if !cli.dry_run
        && let Some(note) = plan.apply(archive.log())?.note()
    {
        eprintln!("{note}");
    }
    Ok(plan.sentence(cli.dry_run))
}

/// A rewrite that may give up claims: what was found said first, and
/// nothing written for a loss the caller did not accept.
fn lossy(
    cli: &Cli,
    archive: &Archive,
    rewrite: &rewrite::Rewrite,
    found: &fixes::found::Found,
    accept_loss: bool,
) -> Result<(String, Option<String>)> {
    for line in found.lines() {
        println!("{line}");
    }
    if found.loses_claims() && !accept_loss {
        bail!(
            "nothing was written: copy the missing files back from a copy of the archive and run again, or pass --accept-loss if there is no copy"
        );
    }
    Ok((segments(cli, archive, rewrite)?, rewrite.seals(cli.dry_run)))
}

/// The archive rehashed into a new one, and the line that says so.
fn rehash(
    cli: &Cli,
    archive: &Archive,
    target: &Path,
    algorithm: Algorithm,
    force: bool,
) -> Result<String> {
    let done = fixes::rehash::rehash(archive, target, algorithm, force, cli.dry_run)?;
    if cli.dry_run {
        return Ok(format!(
            "would rehash {} file(s) in content/ and {} in derived/ with {algorithm} and write {} claim(s) to a new archive at {}; nothing written",
            done.content,
            done.derived,
            done.claims,
            target.display()
        ));
    }
    let mut sentence = format!(
        "{} file(s) in content/ and {} in derived/ rehashed with {algorithm}, {} claim(s) written; the new archive is at {}",
        done.content,
        done.derived,
        done.claims,
        target.display()
    );
    // Writing to a String does not fail.
    if !done.damaged.is_empty() {
        let _ = write!(
            sentence,
            "; {} damaged file(s) left out, the first is {}",
            done.damaged.len(),
            done.damaged[0]
        );
    }
    if done.dropped > 0 {
        let _ = write!(
            sentence,
            "; {} claim(s) about files not in the archive left out",
            done.dropped
        );
    }
    Ok(sentence)
}

/// A fix that rewrites segments: written unless rehearsing.
fn segments(cli: &Cli, archive: &Archive, rewrite: &rewrite::Rewrite) -> Result<String> {
    if !cli.dry_run {
        rewrite.apply(archive)?;
    }
    Ok(rewrite.sentence(cli.dry_run))
}

fn open(root: &Path) -> Result<Archive> {
    Archive::open(root).map_err(|error| match error {
        Error::NoArchive(path) => anyhow!(
            "{}: not an ossuary archive; run this in an archive or name one with --archive",
            path.display()
        ),
        other => other.into(),
    })
}
