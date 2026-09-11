//! ossuary-mailvault: mail into the archive.
//!
//! `ossuary mailvault` fetches whole mailboxes over IMAP into an
//! ossuary archive. Every message goes in through the archive's own
//! two-step accession — the bytes into the content store, once, however
//! many folders and accounts carry them — and onto the record goes
//! where it was seen: the account and the folder, as one
//! `mailbox:place` claim per place. Where each folder's fetch carries
//! on is this program's own memory in `cache/`, the way mailvault kept
//! it: the folder's UIDVALIDITY and the highest UID fetched under it.
//!
//! Everything after that is ossuary's: `extract mail` reads the
//! headers and unpacks the attachments, `find` asks, `get` answers.
//!
//! An outside verb of the trusted family, like `ossuary-mount`: found
//! on the PATH, handed the archive in `OSSUARY_ARCHIVE`, linking the
//! core. Its mailboxes stand in `mailvault.toml` in the archive root.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Result, anyhow, bail};
use clap::Parser;
use ossuary_core::{Archive, Error};

mod config;
mod fetch;
mod memo;
mod output;
mod place;
mod remote;
mod tally;
mod utf7;
mod vault;

use config::Config;
use memo::Memo;
use output::Say;

/// What every claim of this program says as its source.
pub const SOURCE: &str = "mailvault";

/// What a fetched message is, in this program's own words.
pub const MESSAGE: &str = "message/rfc822";

#[derive(Parser)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the command line's switches, one field each — a mode enum would only rename them"
)]
#[command(
    name = "ossuary-mailvault",
    version,
    about = "Mail into the archive: whole mailboxes fetched over IMAP, every message on the record with the place it was seen in",
    max_term_width = 100
)]
struct Cli {
    /// The archive to fill; standing in it is enough
    #[arg(long, value_name = "DIR", env = "OSSUARY_ARCHIVE", default_value = ".")]
    archive: PathBuf,

    /// Only these accounts, by the name mailvault.toml gives them; every
    /// configured one otherwise. With --from-vault: only these
    /// mailboxes of the vault
    #[arg(value_name = "ACCOUNT")]
    accounts: Vec<String>,

    /// Fetch every folder whole, wherever the last run left off. Bytes
    /// the archive has are not stored twice; their place is said again
    #[arg(long)]
    full: bool,

    /// Run the *_cmd fields of mailvault.toml — the password from a
    /// password manager
    #[arg(long)]
    allow_exec: bool,

    /// Say what would be fetched and write nothing
    #[arg(long)]
    dry_run: bool,

    /// Take over a mailvault archive standing at DIR instead of asking
    /// any server: every message it holds, with the mailbox and folder
    /// it was seen in. Interrupted, the next call carries on
    #[arg(long, value_name = "DIR")]
    from_vault: Option<PathBuf>,

    /// The verdict and errors only — the run keeps its narration to
    /// itself
    #[arg(short, long)]
    quiet: bool,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("ossuary-mailvault: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let Cli {
        archive,
        accounts,
        full,
        allow_exec,
        dry_run,
        from_vault,
        quiet,
    } = cli;
    if allow_exec && from_vault.is_some() {
        bail!("--allow-exec runs commands to reach mailboxes, and --from-vault asks none");
    }
    let say = Say::new(quiet);
    let archive = open(&archive)?;
    say.line(format_args!("archive {}", archive.root().display()));
    // Everything that can say no before the archive is touched says it
    // here — the memo is a file, and a refused call should make none.
    if let Some(vault) = &from_vault {
        vault::verify(vault)?;
    }
    let memo = Memo::open(&archive.root().join("cache").join(memo::FILE_NAME))?;

    let tally = if let Some(vault) = from_vault {
        say.line(format_args!("taking over the vault at {}", vault.display()));
        vault::run(
            &archive,
            &vault,
            &accounts,
            &memo,
            &vault::Options { full, dry_run },
            say,
        )?
    } else {
        let config = Config::load(archive.root())?;
        let chosen = config.chosen(&accounts)?;
        if chosen.is_empty() {
            bail!(
                "{}: no accounts in {} — one [[account]] table per mailbox; see the README for the shape",
                archive.root().display(),
                config::FILE_NAME
            );
        }
        fetch::run(
            &archive,
            &chosen,
            &memo,
            &fetch::Options {
                full,
                allow_exec,
                dry_run,
            },
            say,
        )?
    };

    println!("{}", tally.verdict(dry_run));
    if tally.failed.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    for failure in &tally.failed {
        eprintln!("{failure}");
    }
    if dry_run {
        eprintln!("{} failed, as named above", tally.failed.len());
    } else {
        eprintln!(
            "{} failed, as named above — the rest is on the record, and the next run tries them again",
            tally.failed.len()
        );
    }
    Ok(ExitCode::FAILURE)
}

/// The archive, or the way to one — `ossuary`'s own wording.
fn open(root: &Path) -> Result<Archive> {
    Archive::open(root).map_err(|error| match error {
        Error::NoArchive(path) => anyhow!(
            "{}: not an ossuary archive — stand in one, name it with --archive, or begin one with `ossuary init`",
            path.display()
        ),
        other => other.into(),
    })
}
