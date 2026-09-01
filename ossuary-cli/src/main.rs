//! ossuary: the command line onto an archive.
//!
//! Thin on purpose: parsing, wording and exit codes live here, and nothing
//! else does — every decision about the archive itself is `ossuary-core`'s.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result, anyhow};
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
    /// Begin an empty archive — or complete one already standing
    ///
    /// Running init on an archive is safe: nothing standing is remade or
    /// edited, and what is missing is added — today that is config.toml,
    /// for archives begun before it existed.
    Init {
        /// The hash that names everything taken in — for good, chosen only
        /// when the archive begins. sha256 unless this machine lacks SHA
        /// instructions; then blake3 hashes faster
        #[arg(long, value_name = "NAME")]
        algorithm: Option<String>,
    },
    /// Take files in: a directory tree, or one file
    ///
    /// Every regular file goes in — minus what the archive's config.toml
    /// excludes; a file named outright goes in regardless — and six claims
    /// go on the record for each: where it came from, on which machine,
    /// with which run, how large, what kind, and when it last changed.
    /// What is taken in is only read. Taking the same files in again
    /// stores nothing twice — it records that they also sat here.
    Ingest {
        /// What to take in
        #[arg(value_name = "PATH")]
        path: PathBuf,
    },
    /// Close the open segment; its claims become part of the sealed log
    Seal,
    /// Everything on the record about one file, oldest first
    About {
        /// The file's name in the archive: as `sha256:…`, or the bare hex —
        /// a beginning of it is enough while it names only one file
        #[arg(value_name = "SUBJECT")]
        subject: String,
    },
    /// One file's bytes, back out of the archive
    ///
    /// The bytes come out exactly as they went in, to stdout, ready to
    /// pipe; --output writes them to a file instead.
    Get {
        /// The file's name in the archive: as `sha256:…`, or the bare hex —
        /// a beginning of it is enough while it names only one file
        #[arg(value_name = "SUBJECT")]
        subject: String,

        /// Write the bytes to FILE instead of stdout
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,
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
        Command::Init { algorithm } => init(&cli.archive, algorithm.as_deref()),
        Command::Ingest { path } => ingest(&cli.archive, &path),
        Command::Seal => seal(&cli.archive),
        Command::About { subject } => about(&cli.archive, &subject),
        Command::Get { subject, output } => get(&cli.archive, &subject, output.as_deref()),
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

fn init(root: &Path, algorithm: Option<&str>) -> Result<ExitCode> {
    let requested = algorithm.map(str::parse::<Algorithm>).transpose()?;
    match Archive::create(root, requested.unwrap_or(Algorithm::Sha256)) {
        Ok(archive) => {
            println!(
                "{}: an empty archive — its settings stand in config.toml; take files in with `ossuary ingest DIR`",
                archive.root().display()
            );
            Ok(ExitCode::SUCCESS)
        }
        // Standing in an archive, init completes instead: what is there is
        // never remade, what is missing appears. Only remaking would need
        // refusing, and only choosing an algorithm anew would be remaking.
        Err(Error::AlreadyArchive(_)) => {
            let archive = open(root)?;
            let standing = archive.content().algorithm();
            if let Some(asked) = requested {
                if asked != standing {
                    return Err(anyhow!(
                        "{}: named by {}, for good — an algorithm is chosen when an archive begins, not after",
                        archive.root().display(),
                        standing.name()
                    ));
                }
            }
            if archive.complete()? {
                println!(
                    "{}: already an archive — config.toml was missing and now spells the defaults",
                    archive.root().display()
                );
            } else {
                println!(
                    "{}: already an archive, and whole — nothing to add",
                    archive.root().display()
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(error) => Err(error.into()),
    }
}

fn ingest(root: &Path, path: &Path) -> Result<ExitCode> {
    let archive = open(root)?;
    let host = gethostname::gethostname().to_string_lossy().into_owned();

    eprintln!("archive {}", archive.root().display());
    eprintln!("taking in {}", path.display());
    let run = ossuary_core::ingest(
        archive.content(),
        archive.log(),
        path,
        &host,
        archive.config().excludes(),
    )?;

    let excluded = if run.excluded > 0 {
        format!(", {} path(s) left out as config.toml asks", run.excluded)
    } else {
        String::new()
    };
    println!(
        "{} file(s) new to the archive, {} already held — every place they sat is on the record{excluded}; \
         {} claim(s) written as run {}",
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
        None => println!("nothing to seal — the open segment holds no claims"),
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

fn get(root: &Path, subject: &str, output: Option<&Path>) -> Result<ExitCode> {
    let archive = open(root)?;
    let content = archive.content();

    // The store resolves a beginning by a shard listing, the way `about`'s
    // index resolves one by a range scan — this is the content-facing door,
    // so the store's own answer is the one that counts.
    let algorithm = content.algorithm().name();
    let bare = subject
        .strip_prefix(algorithm)
        .and_then(|rest| rest.strip_prefix(':'))
        .unwrap_or(subject);
    if bare.contains(':') {
        return Err(anyhow!(
            "content here answers to {algorithm}:… — for what the log knows about {subject}, ask `ossuary about`"
        ));
    }
    if !bare.bytes().all(|byte| byte.is_ascii_hexdigit()) || bare.is_empty() {
        return Err(anyhow!(
            "{bare:?} is not hex — a file's name is {algorithm}:… or the bare hex of it"
        ));
    }
    let needed = content.min_prefix();
    if bare.len() < needed {
        return Err(anyhow!(
            "{bare:?} is too short to look up — the store is filed by the first {needed} characters, give at least that many"
        ));
    }
    let digest = match content.matching(bare)?.as_slice() {
        [] => return Err(anyhow!("nothing in the store begins with {bare:?}")),
        [one] => one.clone(),
        many => {
            return Err(anyhow!(
                "{bare:?} begins {} names — give more of it to name only one",
                many.len()
            ));
        }
    };
    let mut reader = content
        .reader(&digest)?
        .ok_or_else(|| anyhow!("{algorithm}:{digest}: gone between naming and reading"))?;

    if let Some(path) = output {
        let mut file =
            std::fs::File::create(path).with_context(|| format!("{}: writing", path.display()))?;
        let bytes = std::io::copy(&mut reader, &mut file)
            .with_context(|| format!("{}: writing", path.display()))?;
        println!(
            "{}: {} byte(s) of {algorithm}:{digest}",
            path.display(),
            bytes
        );
    } else {
        // A beginning was given; say what it named, where the bytes
        // will not drown it out.
        if bare.len() < digest.as_str().len() {
            eprintln!("{algorithm}:{digest}");
        }
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        if let Err(error) = std::io::copy(&mut reader, &mut lock) {
            // The reader closed the pipe: it has all it wanted. That is
            // its business going well, not this run going badly.
            if error.kind() == std::io::ErrorKind::BrokenPipe {
                return Ok(ExitCode::SUCCESS);
            }
            return Err(anyhow::Error::new(error).context("writing to stdout"));
        }
    }
    Ok(ExitCode::SUCCESS)
}
