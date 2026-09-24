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

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use ossuary_core::{Archive, Error};

mod config;
mod fetch;
mod graph;
mod import;
mod memo;
mod output;
mod place;
mod remote;
mod tally;
mod utf7;

use config::{Account, Config, Reach};
use graph::Graph;
use memo::Memo;
use output::Say;
use remote::Remote;
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
    /// List the folders of the accounts in mailvault.toml
    ///
    /// One line per folder, as ACCOUNT:FOLDER. The part after the colon is
    /// the name to use in the account's folders list.
    Folders {
        /// List only these accounts, by their name in mailvault.toml; all
        /// when none is given
        #[arg(value_name = "ACCOUNT")]
        accounts: Vec<String>,

        /// Run the commands given as *_cmd keys in mailvault.toml, such as
        /// password_cmd
        #[arg(long)]
        allow_exec: bool,
    },
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
        Command::Folders {
            accounts,
            allow_exec,
        } => {
            let config = Config::load(archive.root())?;
            let chosen = chosen(&archive, &config, &accounts)?;
            folders(&chosen, allow_exec)
        }
        Command::Fetch {
            accounts,
            full,
            allow_exec,
            dry_run,
        } => {
            say.line(format_args!("archive {}", archive.root().display()));
            let config = Config::load(archive.root())?;
            let chosen = chosen(&archive, &config, &accounts)?;
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
            import::verify(&vault)?;
            let memo = Memo::open(&memo_path)?;
            say.line(format_args!(
                "importing the Python mailvault archive at {}",
                vault.display()
            ));
            let tally = import::run(
                &archive,
                &vault,
                &mailboxes,
                &memo,
                &import::Options { full, dry_run },
                say,
            )?;
            Ok(finish(&tally, dry_run))
        }
    }
}

/// The accounts named, or all of them; an error when there are none at
/// all.
fn chosen<'a>(archive: &Archive, config: &'a Config, names: &[String]) -> Result<Vec<&'a Account>> {
    let chosen = config.chosen(names)?;
    if chosen.is_empty() {
        bail!(
            "{}: no accounts in {}; add or uncomment an [[account]] table for each mailbox",
            archive.root().display(),
            config::FILE_NAME
        );
    }
    Ok(chosen)
}

/// Every folder of each account on stdout. An account that cannot be
/// reached is reported on stderr, and the others are still listed.
fn folders(accounts: &[&Account], allow_exec: bool) -> Result<ExitCode> {
    let mut out = std::io::stdout().lock();
    let mut failed = 0;
    for account in accounts {
        let names = match account_folders(account, allow_exec) {
            Ok(names) => names,
            Err(error) => {
                eprintln!("{error:#}");
                failed += 1;
                continue;
            }
        };
        for name in names {
            match writeln!(out, "{}", place::folder(&account.name, &name)) {
                Ok(()) => {}
                // The reader closed the pipe (`| head`) and has what it wanted.
                Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {
                    return Ok(ExitCode::SUCCESS);
                }
                Err(error) => return Err(error).context("writing to stdout"),
            }
        }
    }
    if failed == 0 {
        return Ok(ExitCode::SUCCESS);
    }
    eprintln!("{failed} account(s) failed, listed above");
    Ok(ExitCode::FAILURE)
}

/// The folders of one account, as the server names them.
fn account_folders(account: &Account, allow_exec: bool) -> Result<Vec<String>> {
    match account.reach(allow_exec)? {
        Reach::Imap(imap) => {
            let password = imap.password(&account.name)?;
            let mut remote = Remote::connect(&account.name, &imap, password)?;
            let folders = remote.folders().with_context(|| account.name.clone());
            remote.logout();
            folders
        }
        Reach::Graph(graph) => {
            let secret = graph.secret(&account.name)?.to_string();
            Ok(Graph::connect(&account.name, &graph, secret)?.folders())
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
