//! The fixer: a repair tool for 0.x archives after a breaking change.
//!
//! A 0.x archive collects scars: a word in the vocabulary that changed,
//! a claim an older version should have said. The programs stay free of
//! migration code; this one knows each scar by name and closes it. So
//! far every fix closes its scar the way the record closes everything,
//! by adding. A scar that can only be closed by rewriting what is sealed
//! is not ruled out; a segment is named by its bytes and chained by that
//! name, so such a fix re-seals the chain from there on. That is not a
//! clean job, which is why it lives here and not in the archive.
//!
//! The shape: a fix is one module under [`fixes`], a function from the
//! log to a [`Plan`], the claims it would append and the words for
//! saying so. Everything else is shared: [`record`] reads the log whole
//! and replays an attribute's standing set, [`plan`] appends what a
//! plan holds and speaks the sentence, and this file maps each
//! subcommand to its fix. Adding a fix is one file, one variant of
//! [`Command`] with its help text, and one arm of the match in [`run`].

mod fixes;
mod plan;
mod record;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Result, anyhow};
use clap::{Parser, Subcommand};
use ossuary_core::{Archive, Error};

use crate::plan::Plan;

#[derive(Parser)]
#[command(
    name = "ossuary-fix",
    version,
    about = "Repair tool for 0.x archives after a breaking change",
    after_help = "Every fix reads the whole log, works out what is missing, and writes
exactly that. Run a fix twice and the second run finds nothing to do."
)]
struct Cli {
    /// The archive to work in; standing in it is enough
    #[arg(
        long,
        global = true,
        value_name = "DIR",
        env = "OSSUARY_ARCHIVE",
        default_value = "."
    )]
    archive: PathBuf,

    /// Say what would be written, and write nothing
    #[arg(long, global = true)]
    dry_run: bool,

    #[command(subcommand)]
    command: Command,
}

/// The fixes, one per scar. The help text of each says what the scar
/// is, what the fix writes, and what it leaves alone.
#[derive(Subcommand)]
enum Command {
    /// Say every standing `derive:derived-from` again as `prov:origin`
    ///
    /// Until 0.6.3 a derived file's origin was recorded as
    /// `derive:derived-from`; the word is `prov:origin` now, and the
    /// present is asked in the new word, so a derived file whose origin
    /// stands only under the old one is held but not placed: `find`,
    /// `ls` and the mount no longer reach it. This fix says each origin
    /// that still stands under the old word again under the new one,
    /// with the moment, source and run of the old claim, so the record
    /// reads as of any day as if the new word had always been used. The
    /// old claims stay as they were said. An origin already standing
    /// as `prov:origin` is left alone.
    Origin,
    /// Say every standing `zip:entry` and `zip:path` again as an inner place
    ///
    /// Until 0.7.0 the packed extractor spoke in a namespace named
    /// after the one format it read: an archive's inventory stood on
    /// it as `zip:entry`, an unpacked entry's path inside the archive
    /// stood on the entry as `zip:path`. A place inside another
    /// content is now spelled with a leading `@` and stands as
    /// `packed:path` on the archive and as `file:path` on the entry,
    /// whatever the archive's format, so one `find` term reaches both.
    /// This fix says each such place that still stands only under an
    /// old word again under the new one, `@` in front, with the
    /// moment, source and run of the old claim. The old claims stay as
    /// they were said; a place already standing in the new word is
    /// left alone.
    Packed,
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

/// The one path every fix takes: open, plan, say, and append unless
/// rehearsing.
fn run(cli: &Cli) -> Result<ExitCode> {
    let archive = open(&cli.archive)?;
    let log = archive.log();
    let plan: Plan = match cli.command {
        Command::Origin => fixes::origin::plan(log)?,
        Command::Packed => fixes::packed::plan(log)?,
    };
    if !cli.dry_run {
        plan.apply(log)?;
    }
    println!("{}", plan.sentence(cli.dry_run));
    Ok(ExitCode::SUCCESS)
}

fn open(root: &Path) -> Result<Archive> {
    Archive::open(root).map_err(|error| match error {
        Error::NoArchive(path) => anyhow!(
            "{}: not an ossuary archive; stand in one, or name it with --archive",
            path.display()
        ),
        other => other.into(),
    })
}
