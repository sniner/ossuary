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
    after_help = "Each fix reads the log and writes only the claims that are missing.
Running a fix again writes nothing."
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

    /// Show what would be written, write nothing
    #[arg(long, global = true)]
    dry_run: bool,

    #[command(subcommand)]
    command: Command,
}

/// The fixes, one per breaking change. The help text of each says what
/// changed, what the fix writes, and what it leaves unchanged.
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
            "{}: not an ossuary archive; run this in an archive or name one with --archive",
            path.display()
        ),
        other => other.into(),
    })
}
