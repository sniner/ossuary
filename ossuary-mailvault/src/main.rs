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
//! core. Its mailboxes stand in `mailvault.toml` in the archive root,
//! or in a file named with `--accounts`.

// `assert!(x.is_empty())` reads as the statement it makes; the form
// clippy suggests, `assert_eq!(x, [] as [T; 0])`, says the same and
// names the type for nothing.
#![cfg_attr(test, allow(clippy::assert_is_empty))]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use ossuary_core::{Archive, Error};

mod add;
mod compare;
mod config;
mod fetch;
mod graph;
mod import;
mod memo;
mod output;
mod place;
mod proton;
mod remote;
mod tally;
mod utf7;

use config::{Account, Config, Reach, Source};
use graph::Graph;
use memo::Memo;
use output::Say;
use remote::{Mailbox as _, Remote};
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

    /// Keep the caches in DIR, not in the archive
    ///
    /// The caches store the resume point of each folder and the index of
    /// the archive. Use it to keep them off a network share. DIR is
    /// created if missing. Use one DIR per archive. A cache outside the
    /// archive is only used once the archive has a sealed segment; until
    /// then every folder is fetched in full.
    #[arg(long, global = true, value_name = "DIR", env = "OSSUARY_CACHE")]
    cache: Option<PathBuf>,

    /// Read the accounts from FILE instead of mailvault.toml in the
    /// archive
    // Not `accounts`: that id is the ACCOUNT positional of fetch and
    // folders, and a global option of the same id is not propagated.
    #[arg(long = "accounts", global = true, value_name = "FILE")]
    accounts_file: Option<PathBuf>,

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
    /// the name to use in the account's folders or skip list.
    Folders {
        /// List only these accounts, by their name in mailvault.toml; all
        /// when none is given
        #[arg(value_name = "ACCOUNT")]
        accounts: Vec<String>,
    },
    /// Fetch new mail from the accounts in mailvault.toml
    ///
    /// Only messages that arrived since the last run are fetched, unless
    /// --full or --compare is given.
    Fetch {
        /// Fetch only these accounts, by their name in mailvault.toml; all
        /// when none is given
        #[arg(value_name = "ACCOUNT")]
        accounts: Vec<String>,

        /// Fetch all messages, not only those since the last run. Messages
        /// already in the archive are not stored again
        #[arg(long)]
        full: bool,

        /// List all messages and fetch only those whose Message-ID is not
        /// in the archive. Run ossuary extract mail first
        #[arg(long, conflicts_with = "full")]
        compare: bool,

        /// Show what would be fetched, write nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Import an archive of the Python tool mailvault (github.com/sniner/mailvault)
    ///
    /// Each message is stored with the mailboxes and folders the Python
    /// mailvault recorded for it. Each mailbox needs an account of the
    /// same name in mailvault.toml. A folder that is not in the
    /// account's folders list is recorded as a label (mailbox:tag): on a
    /// Gmail account every such folder, on any other IMAP account those
    /// under Labels/, where Proton Mail Bridge shows labels.
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

        /// Record every folder as the Python mailvault named it, labels
        /// included, without reading mailvault.toml
        #[arg(long)]
        no_auto_labels: bool,
    },
    /// Add the messages in a directory of files, one message per file
    ///
    /// For messages from a backup or an export of a mailbox. Each file
    /// below DIR is stored as a fetched message is. Its place
    /// (mailbox:place) is the account name given and, as the folder, the
    /// directory the file is in below DIR: a file in DIR/Archive/2010
    /// gets ACCOUNT:Archive/2010, a file in DIR itself gets ACCOUNT. The
    /// account needs no entry in mailvault.toml.
    ///
    /// A file that the mail extractor would not read as a message is not
    /// added and is listed. Files whose names begin with a dot are
    /// skipped. A second run over the same directory stores nothing new
    /// and records each place again.
    Add(AddArgs),
}

/// What `add` takes.
#[derive(clap::Args)]
#[allow(
    clippy::doc_markdown,
    reason = "help texts; backticks would show in --help"
)]
struct AddArgs {
    /// The directory with the message files
    #[arg(value_name = "DIR")]
    dir: PathBuf,

    /// The account name for the places of the messages: letters,
    /// digits, `.`, `_` and `-`
    #[arg(long, value_name = "ACCOUNT")]
    account: String,

    /// Record the claims with this time, not the time of the run, such
    /// as the day the backup was made: an RFC 3339 time, or a date for
    /// the start of that day in UTC
    #[arg(long, value_name = "TIME")]
    seen: Option<String>,

    /// Show how many messages would be added, write nothing
    #[arg(long)]
    dry_run: bool,
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

/// Ready the log before a run that writes, and say so when a seal that
/// stopped before its last step was finished. A lost head is refused here,
/// before a server is asked for anything.
fn finish_interrupted_seal(archive: &Archive) -> Result<()> {
    if let Some(note) = archive.log().writable()?.note() {
        eprintln!("{note}");
    }
    Ok(())
}
fn run(cli: Cli) -> Result<ExitCode> {
    let Cli {
        archive: root,
        cache,
        accounts_file: named,
        quiet,
        command,
    } = cli;
    let say = Say::new(quiet);
    // Everything that can say no before the archive is touched says it
    // before the memo is opened — the memo is a file, and a refused
    // call should make none.
    match command {
        Command::Init => {
            init(&accounts_alone(named, &root)?)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Folders { accounts } => {
            let config = accounts_alone(named, &root)?.load()?;
            let chosen = chosen(&config, &accounts)?;
            folders(&chosen, say)
        }
        Command::Fetch {
            accounts,
            full,
            compare,
            dry_run,
        } => {
            let (archive, source) = archive_and_accounts(&root, cache, named)?;
            say.line(format_args!("archive {}", archive.root().display()));
            if !dry_run {
                finish_interrupted_seal(&archive)?;
            }
            let config = source.load()?;
            let chosen = chosen(&config, &accounts)?;
            let memo = opened(&archive, say, "every folder is fetched in full", dry_run)?;
            let tally = fetch::run(
                &archive,
                &source,
                &chosen,
                &memo,
                &fetch::Options {
                    full,
                    compare,
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
            no_auto_labels,
        } => {
            if no_auto_labels && let Some(path) = &named {
                bail!(
                    "--accounts {}: --no-auto-labels reads no accounts; drop one of the two",
                    path.display()
                );
            }
            let (archive, source) = archive_and_accounts(&root, cache, named)?;
            say.line(format_args!("archive {}", archive.root().display()));
            if !dry_run {
                finish_interrupted_seal(&archive)?;
            }
            import::verify(&vault)?;
            let config = if no_auto_labels {
                None
            } else {
                Some(source.load().map_err(|error| {
                    anyhow!("{error:#}; or pass --no-auto-labels to import without the accounts")
                })?)
            };
            let memo = opened(
                &archive,
                say,
                "the import reads every message again",
                dry_run,
            )?;
            say.line(format_args!(
                "importing the Python mailvault archive at {}",
                vault.display()
            ));
            let tally = import::run(
                &archive,
                &vault,
                &mailboxes,
                config.as_ref(),
                &memo,
                &import::Options { full, dry_run },
                say,
            )?;
            Ok(finish(&tally, dry_run))
        }
        Command::Add(args) => add(&root, cache, named.as_deref(), &args, say),
    }
}

/// `add`: the options checked before the archive is written to, then
/// the run.
fn add(
    root: &Path,
    cache: Option<PathBuf>,
    named: Option<&Path>,
    args: &AddArgs,
    say: Say,
) -> Result<ExitCode> {
    let AddArgs {
        dir,
        account,
        seen,
        dry_run,
    } = args;
    if let Some(path) = named {
        bail!(
            "--accounts {}: add reads no accounts; the account name is given with --account",
            path.display()
        );
    }
    if !config::valid_name(account) {
        bail!("--account {account}: an account name is made of letters, digits, `.`, `_` and `-`");
    }
    let seen = seen
        .as_deref()
        .map(|given| {
            ossuary_core::Timestamp::opening(given).map_err(|_| {
                anyhow!(
                    "--seen {given}: not a time; give an RFC 3339 time (2019-10-05T12:00:00Z) or a date (2019-10-05)"
                )
            })
        })
        .transpose()?;
    let archive = open(root)?;
    let archive = match cache {
        Some(dir) => archive.with_cache(dir),
        None => archive,
    };
    say.line(format_args!("archive {}", archive.root().display()));
    if !dry_run {
        finish_interrupted_seal(&archive)?;
    }
    say.line(format_args!(
        "adding the messages in {} as account {account}",
        dir.display()
    ));
    let tally = add::run(
        &archive,
        dir,
        &add::Options {
            account,
            seen,
            dry_run: *dry_run,
        },
        say,
    )?;
    Ok(finish(&tally, *dry_run))
}

/// The accounts file for a command that works on it alone, `init` and
/// `folders`. A file named with `--accounts` need not lie in an archive,
/// so none is opened for it.
fn accounts_alone(named: Option<PathBuf>, root: &Path) -> Result<Source> {
    Ok(match named {
        Some(path) => Source::Named(path),
        None => Source::Archive(open(root)?.root().to_path_buf()),
    })
}

/// The archive a command fetches or imports into, and its accounts file.
fn archive_and_accounts(
    root: &Path,
    cache: Option<PathBuf>,
    named: Option<PathBuf>,
) -> Result<(Archive, Source)> {
    let archive = open(root)?;
    let archive = match cache {
        Some(dir) => archive.with_cache(dir),
        None => archive,
    };
    let source = match named {
        Some(path) => Source::Named(path),
        None => Source::Archive(archive.root().to_path_buf()),
    };
    Ok((archive, source))
}

/// The memo, opened. A memo kept outside the archive that does not
/// name a sealed segment the archive holds is emptied, and a line says
/// so and what that costs this command: it may be another archive's,
/// and a resume point from there would pass by messages this archive
/// never got.
///
/// A dry run makes no memo file and empties none. Where a run would do
/// either, it gets a memo in memory that holds nothing, and so counts
/// what the run would fetch.
fn opened(archive: &Archive, say: Say, costs: &str, dry_run: bool) -> Result<Memo> {
    let path = archive.cache_dir()?.join(memo::FILE_NAME);
    if dry_run && !path.exists() {
        return Memo::empty();
    }
    let memo = Memo::open(&path)?;
    if archive.accepts_cache(memo.proof()?.as_deref())? {
        return Ok(memo);
    }
    let held = if dry_run {
        !memo.is_empty()?
    } else {
        memo.clear()?
    };
    if held {
        say.line(format_args!(
            "{}: does not belong to this archive, not used; {costs}",
            path.display()
        ));
    }
    if dry_run { Memo::empty() } else { Ok(memo) }
}

/// The accounts named, or all of them; an error when there are none at
/// all.
fn chosen<'a>(config: &'a Config, names: &[String]) -> Result<Vec<&'a Account>> {
    let chosen = config.chosen(names)?;
    if chosen.is_empty() {
        bail!(
            "{}: no accounts; add or uncomment an [[account]] table for each mailbox",
            config.path.display()
        );
    }
    Ok(chosen)
}

/// Every folder of each account on stdout. An account that cannot be
/// reached is reported on stderr, and the others are still listed.
fn folders(accounts: &[&Account], say: Say) -> Result<ExitCode> {
    let mut out = std::io::stdout().lock();
    let mut failed = 0;
    for account in accounts {
        let names = match account_folders(account, say) {
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
fn account_folders(account: &Account, say: Say) -> Result<Vec<String>> {
    match account.reach()? {
        Reach::Imap(imap) => {
            let password = imap.password(&account.name)?;
            let mut remote = Remote::connect(&account.name, &imap, password)?;
            let folders = remote.folders().with_context(|| account.name.clone());
            remote.logout();
            folders
        }
        Reach::Graph(graph) => {
            let secret = graph.secret(&account.name)?.to_string();
            Ok(Graph::connect(&account.name, &graph, secret, say)?.folders())
        }
    }
}

/// A `mailvault.toml` to fill in, unless one stands already.
fn init(source: &Source) -> Result<()> {
    let path = source.path();
    if source.begin()? {
        println!(
            "{}: written with commented-out examples; fill in your mailboxes, then run `{}`",
            path.display(),
            source.command("fetch")
        );
    } else {
        println!("{}: already exists, not overwritten", path.display());
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
    eprintln!("{}", closing(tally, dry_run));
    ExitCode::FAILURE
}

/// The line under the list of failures: how many, and what becomes of
/// them. The next run tries a failure again, except a message given up.
fn closing(tally: &Tally, dry_run: bool) -> String {
    let failed = tally.failed.len();
    if dry_run {
        return format!("{failed} failed, listed above");
    }
    let recorded = format!("{failed} failed, listed above; everything else was recorded");
    match tally.given_up {
        0 => format!("{recorded}, and the next run tries the failed ones again"),
        given_up if given_up == failed => {
            format!("{recorded}; what failed was given up, and only --full fetches it again")
        }
        given_up => format!(
            "{recorded}; the next run tries them again, except {given_up} given up, which only --full fetches"
        ),
    }
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

#[cfg(test)]
mod tests {
    use ossuary_core::Algorithm;

    use super::*;

    /// An archive with its caches outside it, and the path of its memo.
    fn outside(dir: &Path) -> (Archive, PathBuf) {
        let archive = Archive::create(dir.join("archive"), Algorithm::Sha256)
            .unwrap()
            .with_cache(dir.join("elsewhere"));
        let path = dir.join("elsewhere").join(memo::FILE_NAME);
        (archive, path)
    }

    /// A memo file with one resume point and no proof of its archive.
    fn unproven(path: &Path) {
        let memo = Memo::open(path).unwrap();
        memo.begin().unwrap();
        memo.advance_imap(
            "john",
            "INBOX",
            memo::ImapResume {
                uidvalidity: 1,
                uid: 7,
            },
        )
        .unwrap();
        memo.commit().unwrap();
    }

    fn point(memo: &Memo) -> Option<memo::ImapResume> {
        memo.imap_resume("john", "INBOX").unwrap()
    }

    #[test]
    fn init_with_a_named_accounts_file_needs_no_archive() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("mail.toml");
        let cli = Cli::try_parse_from([
            "ossuary-mailvault".as_ref(),
            "--archive".as_ref(),
            dir.path().join("no-archive").as_os_str(),
            "--accounts".as_ref(),
            file.as_os_str(),
            "init".as_ref(),
        ])
        .unwrap();

        assert_eq!(run(cli).unwrap(), ExitCode::SUCCESS);

        assert_eq!(std::fs::read_to_string(&file).unwrap(), config::STARTER);
    }

    #[test]
    fn init_without_a_named_accounts_file_needs_the_archive() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::try_parse_from([
            "ossuary-mailvault".as_ref(),
            "--archive".as_ref(),
            dir.path().as_os_str(),
            "init".as_ref(),
        ])
        .unwrap();

        let refused = run(cli).err().unwrap();

        assert!(
            refused.to_string().contains("not an ossuary archive"),
            "{refused:#}"
        );
        assert!(!dir.path().join(config::FILE_NAME).exists());
    }

    #[test]
    fn the_closing_line_does_not_promise_another_try_for_what_was_given_up() {
        let mut tally = Tally::new("fetch");
        tally.failed = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            closing(&tally, false),
            "2 failed, listed above; everything else was recorded, and the next run tries the failed ones again"
        );
        tally.given_up = 1;
        assert_eq!(
            closing(&tally, false),
            "2 failed, listed above; everything else was recorded; the next run tries them again, except 1 given up, which only --full fetches"
        );
        tally.given_up = 2;
        assert_eq!(
            closing(&tally, false),
            "2 failed, listed above; everything else was recorded; what failed was given up, and only --full fetches it again"
        );
        assert_eq!(closing(&tally, true), "2 failed, listed above");
    }

    #[test]
    fn a_dry_run_makes_no_memo_file() {
        let dir = tempfile::tempdir().unwrap();
        let (archive, path) = outside(dir.path());

        let memo = opened(&archive, Say::new(true), "", true).unwrap();

        assert!(memo.is_empty().unwrap());
        assert!(!path.exists());
    }

    #[test]
    fn a_dry_run_does_not_use_a_memo_without_proof_and_leaves_it_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let (archive, path) = outside(dir.path());
        unproven(&path);

        let memo = opened(&archive, Say::new(true), "", true).unwrap();

        assert_eq!(point(&memo), None, "the run starts from nothing");
        assert!(
            point(&Memo::open(&path).unwrap()).is_some(),
            "the file still has the point"
        );
    }

    #[test]
    fn a_run_empties_a_memo_without_proof() {
        let dir = tempfile::tempdir().unwrap();
        let (archive, path) = outside(dir.path());
        unproven(&path);

        let memo = opened(&archive, Say::new(true), "", false).unwrap();

        assert_eq!(point(&memo), None);
        assert_eq!(point(&Memo::open(&path).unwrap()), None);
    }
}
