//! ossuary-mailvault: mail into the archive.
//!
//! `ossuary mailvault` fetches whole mailboxes over IMAP or MS Graph
//! into an ossuary archive. Every message goes in through the
//! archive's own two-step accession — the bytes into the content
//! store, once, however many folders and accounts carry them — and
//! onto the record goes where it was seen: the account and the folder,
//! as one `mailbox:place` claim per place. Where each folder's fetch
//! carries on is this program's own memory in `cache/`, the way
//! mailvault kept it: over IMAP the folder's UIDVALIDITY and the
//! highest UID fetched under it, over Graph the server's delta link.
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
use clap::{Parser, Subcommand};
use ossuary_core::{Archive, Error};

mod config;
mod fetch;
mod graph;
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
use tally::Tally;

/// What every claim of this program says as its source.
pub const SOURCE: &str = "mailvault";

/// What a fetched message is, in this program's own words.
pub const MESSAGE: &str = "message/rfc822";

#[derive(Parser)]
#[command(
    name = "ossuary-mailvault",
    version,
    about = "Fetch mail from IMAP and Microsoft 365 mailboxes into an ossuary archive",
    max_term_width = 100
)]
struct Cli {
    /// The ossuary archive to work in
    #[arg(
        long,
        global = true,
        value_name = "DIR",
        env = "OSSUARY_ARCHIVE",
        default_value = "."
    )]
    archive: PathBuf,

    /// Print only results and errors
    #[arg(short, long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
#[allow(
    clippy::doc_markdown,
    reason = "help texts; backticks would show in --help"
)]
enum Command {
    /// Write a mailvault.toml with an example of each kind of account
    ///
    /// The examples are commented out. An existing mailvault.toml is not
    /// overwritten.
    Init,
    /// Fetch new mail from the accounts in mailvault.toml
    ///
    /// Only messages that arrived since the last run are fetched, unless
    /// --full is given.
    Fetch {
        /// Fetch only these accounts, by their name in mailvault.toml; all
        /// when none is given
        #[arg(value_name = "ACCOUNT")]
        accounts: Vec<String>,

        /// Fetch all messages, not only those since the last run. Messages
        /// already in the archive are not stored again
        #[arg(long)]
        full: bool,

        /// Run the commands given as *_cmd keys in mailvault.toml, such as
        /// password_cmd
        #[arg(long)]
        allow_exec: bool,

        /// Show what would be fetched, write nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Import an archive of the Python tool mailvault (github.com/sniner/mailvault)
    ///
    /// Each message is stored with the mailboxes and folders the Python
    /// mailvault recorded for it.
    Import {
        /// The directory of the Python mailvault archive
        #[arg(value_name = "DIR")]
        vault: PathBuf,

        /// Import only these mailboxes, by their name in the Python
        /// mailvault; all when none is given
        #[arg(value_name = "MAILBOX")]
        mailboxes: Vec<String>,

        /// Read all messages again, including those already imported
        #[arg(long)]
        full: bool,

        /// Show what would be imported, write nothing
        #[arg(long)]
        dry_run: bool,
    },
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
        quiet,
        command,
    } = cli;
    let say = Say::new(quiet);
    let archive = open(&archive)?;
    let memo_path = archive.root().join("cache").join(memo::FILE_NAME);
    // Everything that can say no before the archive is touched says it
    // before the memo is opened — the memo is a file, and a refused
    // call should make none.
    match command {
        Command::Init => {
            init(&archive)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Fetch {
            accounts,
            full,
            allow_exec,
            dry_run,
        } => {
            say.line(format_args!("archive {}", archive.root().display()));
            let config = Config::load(archive.root())?;
            let chosen = config.chosen(&accounts)?;
            if chosen.is_empty() {
                bail!(
                    "{}: no accounts in {}; add or uncomment an [[account]] table for each mailbox",
                    archive.root().display(),
                    config::FILE_NAME
                );
            }
            let memo = Memo::open(&memo_path)?;
            let tally = fetch::run(
                &archive,
                &chosen,
                &memo,
                &fetch::Options {
                    full,
                    allow_exec,
                    dry_run,
                },
                say,
            )?;
            Ok(finish(&tally, dry_run))
        }
        Command::Import {
            vault,
            mailboxes,
            full,
            dry_run,
        } => {
            say.line(format_args!("archive {}", archive.root().display()));
            vault::verify(&vault)?;
            let memo = Memo::open(&memo_path)?;
            say.line(format_args!(
                "importing the Python mailvault archive at {}",
                vault.display()
            ));
            let tally = vault::run(
                &archive,
                &vault,
                &mailboxes,
                &memo,
                &vault::Options { full, dry_run },
                say,
            )?;
            Ok(finish(&tally, dry_run))
        }
    }
}

/// A `mailvault.toml` to fill in, unless one stands already.
fn init(archive: &Archive) -> Result<()> {
    let root = archive.root().display();
    let name = config::FILE_NAME;
    if config::begin(archive.root())? {
        println!(
            "{root}: {name} written with commented-out examples; fill in your mailboxes, then run `ossuary mailvault fetch`"
        );
    } else {
        println!("{root}: {name} already exists, not overwritten");
    }
    Ok(())
}

/// The verdict on stdout, what failed on stderr, and the exit code
/// that says which of the two it was.
fn finish(tally: &Tally, dry_run: bool) -> ExitCode {
    println!("{}", tally.verdict(dry_run));
    if tally.failed.is_empty() {
        return ExitCode::SUCCESS;
    }
    for failure in &tally.failed {
        eprintln!("{failure}");
    }
    if dry_run {
        eprintln!("{} failed, listed above", tally.failed.len());
    } else {
        eprintln!(
            "{} failed, listed above; everything else was recorded, and the next run tries the failed ones again",
            tally.failed.len()
        );
    }
    ExitCode::FAILURE
}

/// The archive, or the way to one — `ossuary`'s own wording.
fn open(root: &Path) -> Result<Archive> {
    Archive::open(root).map_err(|error| match error {
        Error::NoArchive(path) => anyhow!(
            "{}: not an ossuary archive; run in an archive, name one with --archive, or create one with `ossuary init`",
            path.display()
        ),
        other => other.into(),
    })
}
