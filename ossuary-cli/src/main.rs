//! ossuary: the command line onto an archive.
//!
//! Thin on purpose: parsing, wording and exit codes live here, and nothing
//! else does — every decision about the archive itself is `ossuary-core`'s.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Result, anyhow};
use clap::{Parser, Subcommand};
use ossuary_core::{Algorithm, Archive, Error, Subject};

mod output;

#[derive(Parser)]
#[command(
    name = "ossuary",
    version,
    about = "A personal archive: files kept for good, with everything known about them"
)]
struct Cli {
    /// The archive to work in; standing in it is enough.
    #[arg(long, global = true, value_name = "DIR", default_value = ".")]
    archive: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Begin an empty archive
    Init {
        /// The hash that names everything taken in — for good. sha256 unless
        /// this machine lacks SHA instructions; then blake3 hashes faster.
        #[arg(long, default_value = "sha256", value_name = "NAME")]
        algorithm: String,
    },
    /// Take a directory tree in: its files, and their day-one facts
    ///
    /// Every regular file goes in, and six facts go on the record for each:
    /// where it came from, on which machine, with which run, how large, what
    /// kind, and when it last changed. The tree itself is only read. Taking
    /// the same files in again stores nothing twice — it records that they
    /// also sat here.
    Ingest {
        /// What to take in
        #[arg(value_name = "DIR")]
        tree: PathBuf,
    },
    /// Close the open segment; its facts become part of the sealed log
    Seal,
    /// Everything on the record about one file, oldest first
    About {
        /// The file's name in the archive: as `sha256:…`, or the bare hex —
        /// a beginning of it is enough while it names only one file
        #[arg(value_name = "SUBJECT")]
        subject: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("ossuary: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Command::Init { algorithm } => init(&cli.archive, &algorithm),
        Command::Ingest { tree } => ingest(&cli.archive, &tree),
        Command::Seal => seal(&cli.archive),
        Command::About { subject } => about(&cli.archive, &subject),
    }
}

/// The archive, or the way to one.
fn open(root: &Path) -> Result<Archive> {
    Archive::open(root).map_err(|error| match error {
        Error::NoArchive(path) => anyhow!(
            "{}: not an ossuary archive — stand in one, name it with --archive, or begin one with `ossuary init`",
            path.display()
        ),
        other => other.into(),
    })
}

fn init(root: &Path, algorithm: &str) -> Result<ExitCode> {
    let algorithm: Algorithm = algorithm.parse()?;
    let archive = Archive::create(root, algorithm)?;
    println!(
        "{}: an empty archive — take files in with `ossuary ingest DIR`",
        archive.root().display()
    );
    Ok(ExitCode::SUCCESS)
}

fn ingest(root: &Path, tree: &Path) -> Result<ExitCode> {
    let archive = open(root)?;
    let host = gethostname::gethostname().to_string_lossy().into_owned();

    eprintln!("archive {}", archive.root().display());
    eprintln!("taking in {}", tree.display());
    let run = ossuary_core::ingest(archive.content(), archive.log(), tree, &host)?;

    println!(
        "{} file(s) new to the archive, {} already held — every place they sat is on the record; \
         {} fact(s) written as run {}",
        run.stored, run.known, run.claims, run.run
    );
    if run.failed.is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("{} could not be taken in:", run.failed.len());
        for (path, error) in &run.failed {
            eprintln!("  {}: {error}", path.display());
        }
        Ok(ExitCode::FAILURE)
    }
}

fn seal(root: &Path) -> Result<ExitCode> {
    let archive = open(root)?;
    match archive.log().seal()? {
        Some(segment) => println!(
            "sealed as {} — the open segment starts afresh",
            segment.digest()
        ),
        None => println!("nothing to seal — the open segment holds no facts"),
    }
    Ok(ExitCode::SUCCESS)
}

fn about(root: &Path, subject: &str) -> Result<ExitCode> {
    let archive = open(root)?;
    let mut index = archive.index()?;
    let folded = index.fold(archive.log())?;
    if folded.segments > 0 {
        eprintln!(
            "catching the index up: {} sealed segment(s) it had not seen",
            folded.segments
        );
    }

    // The bare hex is enough — the archive knows its own algorithm — and so
    // is a beginning of it: the index resolves it, like a short commit hash.
    let algorithm = archive.content().algorithm().name();
    let bare = subject
        .strip_prefix(algorithm)
        .and_then(|rest| rest.strip_prefix(':'))
        .unwrap_or(subject);
    let subject = if bare.contains(':') {
        // Another algorithm's name in full: taken at its word.
        Subject::parse(bare)?
    } else if let Ok(whole) = Subject::parse(&format!("{algorithm}:{bare}")) {
        whole
    } else {
        match index.matching(algorithm, bare)?.as_slice() {
            [] => {
                println!("nothing on the record begins with {bare:?}");
                return Ok(ExitCode::SUCCESS);
            }
            [one] => one.clone(),
            many => {
                return Err(anyhow!(
                    "{bare:?} begins {} names — give more of it to name only one",
                    many.len()
                ));
            }
        }
    };

    let claims = index.about(&subject)?;
    if claims.is_empty() {
        println!("nothing on the record about {subject}");
    } else {
        for claim in &claims {
            println!("{}", output::line(claim));
        }
    }
    Ok(ExitCode::SUCCESS)
}
