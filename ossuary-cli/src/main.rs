//! ossuary: the command line onto an archive.
//!
//! Thin on purpose: parsing, wording and exit codes live here, and nothing
//! else does — every decision about the archive itself is `ossuary-core`'s.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result, anyhow};
use clap::{Parser, Subcommand};
use ossuary_core::{Algorithm, Archive, Attribute, Error, Index, Subject, Value};

mod extract;
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

    /// Answers and errors only — the run keeps its narration to itself
    #[arg(short, long, global = true)]
    quiet: bool,

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
    /// Take files in: directory trees and single files, any mix
    ///
    /// Every regular file goes in — minus what the archive's config.toml
    /// excludes; a file named outright goes in regardless — and seven
    /// claims go on the record for each: where it came from, what it is
    /// called, on which machine, with which run, how large, what kind,
    /// and when it last changed. Everything of one call arrives in one
    /// run — `ossuary ingest *.pdf` keeps what a glob matched together,
    /// and "arrived together" stays an askable fact.
    /// What is taken in is only read. A repeated run remembers what it
    /// already observed and leaves unchanged files in peace, so pouring
    /// the same directory in again costs only what is new or changed.
    Ingest {
        /// What to take in; several may be named
        #[arg(value_name = "PATH", required = true)]
        paths: Vec<PathBuf>,

        /// Look at every file anew, remembered or not
        #[arg(long)]
        full: bool,
    },
    /// Run extractors over every file they have not yet examined
    ///
    /// NAME names the program: `ossuary extract exif` runs
    /// `ossuary-extract-exif` from PATH, and every contract it offers —
    /// some programs carry several, each examined and receipted on its
    /// own. NAME:CONTRACT runs one of them: `ossuary extract
    /// packed:list` inventories archives without unpacking them, and
    /// the same spelling holds in the config's list. Without a NAME,
    /// the archive's own list runs — `[extract] run` in its
    /// config.toml — in rounds,
    /// until a whole round finds nothing left: what one extractor
    /// hands back, the next round offers to whichever extractor reads
    /// it, so one call drives a chain like mail, attachment, text to
    /// its end. Each extractor reads each file's bytes and tells the
    /// archive what it found; every finding goes on the record under
    /// the extractor's own name, and every examined file gets a
    /// receipt — found something or not — so a repeated run costs only
    /// what is new, and a call cut short simply continues next time.
    /// An extractor may hand back files as well as findings — an
    /// unpacked attachment, extracted text — and each goes into the
    /// archive as content of its own, named, typed, tied to its origin
    /// and stamped with this call's run id on the record. Files of
    /// kinds an extractor does not read are never touched, and a new
    /// extractor version looks at everything again. A NAME alone still
    /// runs rounds, over that one extractor; naming files runs none —
    /// the named files are examined once, now, and a named file is
    /// handed over even when its kind is not one the extractor reads,
    /// because naming is more deliberate than a pattern.
    Extract {
        /// Which extractor to run: the part after `ossuary-extract-`,
        /// with `:CONTRACT` picking one contract of a program that
        /// offers several. Left out, the archive's own list runs
        #[arg(value_name = "NAME")]
        name: Option<String>,

        /// Only these files, named the way the archive names them —
        /// sha256:… or a beginning of it — instead of everything that
        /// waits
        #[arg(value_name = "SUBJECT")]
        subjects: Vec<String>,

        /// Examine anew, receipted or not: the named files, or — with
        /// none named — everything of a kind the extractor reads
        #[arg(long)]
        full: bool,

        /// Where derived files wait on their way into the archive —
        /// cache/tmp in the archive unless this names another place; a
        /// fast local disk pays off when the archive sits on a share
        #[arg(long, value_name = "DIR")]
        temp_dir: Option<PathBuf>,
    },
    /// Close the open segment; its claims become part of the sealed log
    Seal,
    /// Everything on the record about one file, oldest first
    About {
        /// The file's name in the archive: as `sha256:…`, or the bare hex —
        /// a beginning of it is enough while it names only one file
        #[arg(value_name = "SUBJECT")]
        subject: String,

        /// Only these attributes; a name ending in `:` means the whole
        /// namespace, like `exif:`
        #[arg(value_name = "ATTRIBUTE")]
        attributes: Vec<String>,

        /// Each claim as the JSON line the log holds, ready for jq
        #[arg(short, long)]
        json: bool,
    },
    /// What stands: one attribute's current values, ready for a script
    ///
    /// Where `about` answers with the history — every claim, retractions
    /// included — this answers with the outcome: the values standing after
    /// retractions are applied and repeats collapsed, one per line,
    /// strings bare. Several lines mean the attribute honestly holds
    /// several values; choosing among them is the caller's business, and
    /// with --json the values keep their JSON spelling so jq can do the
    /// choosing. When nothing stands, nothing comes and the exit code
    /// says 1, so a script can test for it.
    Value {
        /// The file's name in the archive: as `sha256:…`, or the bare hex —
        /// a beginning of it is enough while it names only one file
        #[arg(value_name = "SUBJECT")]
        subject: String,

        /// The attribute asked about, like exif:model
        #[arg(value_name = "ATTRIBUTE")]
        attribute: String,

        /// Values in their JSON spelling — strings keep their quotes
        #[arg(short, long)]
        json: bool,
    },
    /// Every file on which all the terms stand
    ///
    /// A term is attribute=value, and every term must hold: `find
    /// file:mime=image/jpeg exif:make=Google` names the files that are
    /// both. `*` and `?` match within text values — `find
    /// file:path=*crete*` answers "what came from that folder".
    /// A value low..high asks for one value inside the range, either
    /// side open: `file:modified=2026-09-01..` is "changed since
    /// September", `file:size=..4096` "at most 4 KiB". Bounds compare in
    /// the attribute's own spelling, a bare `..` asks only that the
    /// attribute stands at all, and two ranged terms on one attribute
    /// may be satisfied by two different values — one low..high term
    /// speaks about a single one. A value wrapped in double quotes is
    /// literal: no glob, no range. --missing turns the question around:
    /// files that lack an attribute (or, ending in `:`, a whole
    /// namespace) — `find file:mime=image/jpeg --missing exif:` is
    /// "which photos have no EXIF on record". Only standing values
    /// answer; what was retracted no longer counts. The matches come out
    /// as names, one per line, ready for `about`, `value` or `get`.
    Find {
        /// attribute=value; repeat to demand all of them at once
        #[arg(value_name = "TERM")]
        terms: Vec<String>,

        /// Only files lacking this attribute (with `:` at the end: this
        /// namespace); may be repeated
        #[arg(long, value_name = "ATTRIBUTE")]
        missing: Vec<String>,
    },
    /// The name a file answers to in the archive
    ///
    /// Hashes the file the way the archive names content — the file is
    /// only read, never taken in — and says whether the archive already
    /// holds those bytes. Works before an ingest as well as after: the
    /// name is the bytes' own, not something an ingest hands out.
    Id {
        /// The file to name
        #[arg(value_name = "FILE")]
        path: PathBuf,
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
    let quiet = cli.quiet;
    match cli.command {
        Command::Init { algorithm } => init(&cli.archive, algorithm.as_deref()),
        Command::Ingest { paths, full } => ingest(&cli.archive, &paths, full, quiet),
        Command::Extract {
            name,
            subjects,
            full,
            temp_dir,
        } => extract::extract(
            &cli.archive,
            name.as_deref(),
            &subjects,
            full,
            temp_dir.as_deref(),
            quiet,
        ),
        Command::Seal => seal(&cli.archive),
        Command::About {
            subject,
            attributes,
            json,
        } => about(&cli.archive, &subject, &attributes, json, quiet),
        Command::Value {
            subject,
            attribute,
            json,
        } => value(&cli.archive, &subject, &attribute, json, quiet),
        Command::Find { terms, missing } => find(&cli.archive, &terms, &missing, quiet),
        Command::Id { path } => id(&cli.archive, &path, quiet),
        Command::Get { subject, output } => get(&cli.archive, &subject, output.as_deref(), quiet),
    }
}

/// The archive, or the way to one.
pub(crate) fn open(root: &Path) -> Result<Archive> {
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

fn ingest(root: &Path, paths: &[PathBuf], full: bool, quiet: bool) -> Result<ExitCode> {
    let archive = open(root)?;
    let host = gethostname::gethostname().to_string_lossy().into_owned();

    if !quiet {
        eprintln!("archive {}", archive.root().display());
        // One path is named; many — a glob's expansion — are counted,
        // the verdict tells how they fared.
        match paths {
            [one] => eprintln!("taking in {}", one.display()),
            many => eprintln!("taking in {} paths", many.len()),
        }
    }
    let memory = if full {
        None
    } else {
        Some(archive.ingest_memory()?)
    };
    let run = ossuary_core::ingest(
        archive.content(),
        archive.log(),
        paths,
        &host,
        archive.config().excludes(),
        memory.as_ref(),
    )?;

    let mut verdict = vec![format!("{} file(s) new to the archive", run.stored)];
    if run.known > 0 {
        verdict.push(format!(
            "{} already held — every place they sat is on the record",
            run.known
        ));
    }
    if run.unchanged > 0 {
        verdict.push(format!(
            "{} unchanged since the last run and left in peace",
            run.unchanged
        ));
    }
    if run.excluded > 0 {
        verdict.push(format!(
            "{} path(s) left out as config.toml asks",
            run.excluded
        ));
    }
    let record = if run.claims > 0 {
        format!("{} claim(s) written as run {}", run.claims, run.run)
    } else {
        "nothing new to record".to_string()
    };
    println!("{}; {record}", verdict.join(", "));
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

/// Fold the log in and, unless quiet, say when that was real work.
pub(crate) fn catch_up(index: &mut Index, archive: &Archive, quiet: bool) -> Result<()> {
    let folded = index.fold(archive.log())?;
    if folded.segments > 0 && !quiet {
        eprintln!(
            "catching the index up: {} sealed segment(s) it had not seen",
            folded.segments
        );
    }
    Ok(())
}

/// The subject as the log spells it, from whatever the user typed: the
/// archive's own algorithm filled in — the bare hex is enough — and a
/// beginning resolved against the index, like a short commit hash.
/// `None` when nothing on the record begins that way; a beginning that
/// names several files is refused.
pub(crate) fn resolve(archive: &Archive, index: &Index, given: &str) -> Result<Option<Subject>> {
    let algorithm = archive.content().algorithm().name();
    let bare = given
        .strip_prefix(algorithm)
        .and_then(|rest| rest.strip_prefix(':'))
        .unwrap_or(given);
    if bare.contains(':') {
        // Another algorithm's name in full: taken at its word.
        return Ok(Some(Subject::parse(bare)?));
    }
    if let Ok(whole) = Subject::parse(&format!("{algorithm}:{bare}")) {
        return Ok(Some(whole));
    }
    match index.matching(algorithm, bare)?.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one.clone())),
        many => Err(anyhow!(
            "{bare:?} begins {} names — give more of it to name only one",
            many.len()
        )),
    }
}

fn about(
    root: &Path,
    subject: &str,
    attributes: &[String],
    json: bool,
    quiet: bool,
) -> Result<ExitCode> {
    let archive = open(root)?;
    let mut index = archive.index()?;
    catch_up(&mut index, &archive, quiet)?;

    // Under --json, stdout is claims and nothing else; the calm zero
    // answers move over to where the run talks.
    let calm = |sentence: String| {
        if json {
            if !quiet {
                eprintln!("{sentence}");
            }
        } else {
            println!("{sentence}");
        }
    };

    let Some(subject) = resolve(&archive, &index, subject)? else {
        calm(format!("nothing on the record begins with {subject:?}"));
        return Ok(ExitCode::SUCCESS);
    };

    let mut claims = index.about(&subject)?;
    if claims.is_empty() {
        calm(format!("nothing on the record about {subject}"));
        return Ok(ExitCode::SUCCESS);
    }
    if !attributes.is_empty() {
        claims.retain(|claim| {
            attributes.iter().any(|wanted| {
                if wanted.ends_with(':') {
                    claim.attribute().as_str().starts_with(wanted.as_str())
                } else {
                    claim.attribute().as_str() == wanted
                }
            })
        });
        if claims.is_empty() {
            calm(format!(
                "nothing of that on the record about {subject} — plain `ossuary about` shows all of it"
            ));
            return Ok(ExitCode::SUCCESS);
        }
    }
    for claim in &claims {
        if json {
            println!("{}", claim.to_line());
        } else {
            println!("{}", output::line(claim));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn value(root: &Path, subject: &str, attribute: &str, json: bool, quiet: bool) -> Result<ExitCode> {
    let attribute = Attribute::parse(attribute)?;
    let archive = open(root)?;
    let mut index = archive.index()?;
    catch_up(&mut index, &archive, quiet)?;

    // Nothing standing is a testable answer, not a broken run: the exit
    // code carries it, and the sentence is for a reader wondering why
    // nothing came.
    let Some(subject) = resolve(&archive, &index, subject)? else {
        if !quiet {
            eprintln!("nothing on the record begins with {subject:?}");
        }
        return Ok(ExitCode::FAILURE);
    };
    let standing = index.values(&subject, &attribute)?;
    if standing.is_empty() {
        if !quiet {
            eprintln!(
                "nothing stands for {} on {subject} — `ossuary about` shows what was ever said",
                attribute.as_str()
            );
        }
        return Ok(ExitCode::FAILURE);
    }
    for value in &standing {
        match value {
            Value::String(text) if !json => println!("{text}"),
            other => println!("{other}"),
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn find(root: &Path, terms: &[String], missing: &[String], quiet: bool) -> Result<ExitCode> {
    if terms.is_empty() && missing.is_empty() {
        return Err(anyhow!(
            "nothing asked — name at least one TERM as attribute=value, or --missing ATTRIBUTE"
        ));
    }
    let mut parsed = Vec::new();
    for word in terms {
        let Some((attribute, value)) = word.split_once('=') else {
            return Err(anyhow!(
                "{word:?} is not a term — a term is attribute=value, like user:tag=holiday"
            ));
        };
        parsed.push((Attribute::parse(attribute)?, value.to_string()));
    }
    let archive = open(root)?;
    let mut index = archive.index()?;
    catch_up(&mut index, &archive, quiet)?;
    let subjects = index.find(&parsed, missing)?;
    for subject in &subjects {
        println!("{subject}");
    }
    // The names alone stay on stdout, ready to pipe; the count is the
    // run's word on how it went.
    if !quiet {
        match subjects.len() {
            0 => eprintln!("nothing standing matches"),
            n => eprintln!("{n} file(s)"),
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn id(root: &Path, path: &Path, quiet: bool) -> Result<ExitCode> {
    let archive = open(root)?;
    let algorithm = archive.content().algorithm();
    let bytes = std::fs::read(path).with_context(|| format!("{}: reading", path.display()))?;
    let digest = algorithm.hash(&bytes);
    println!("{}:{digest}", algorithm.name());
    // The name is the answer and stays alone on stdout; whether the
    // archive holds the bytes is the run talking. Held means held —
    // taken in or derived, either store counts.
    if !quiet {
        let held = !archive.content().matching(digest.as_str())?.is_empty()
            || !archive.derived().matching(digest.as_str())?.is_empty();
        if held {
            eprintln!("the archive holds these bytes — `ossuary about` says what is on the record");
        } else {
            eprintln!("not in the archive — `ossuary ingest` takes it in");
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn get(root: &Path, subject: &str, output: Option<&Path>, quiet: bool) -> Result<ExitCode> {
    let archive = open(root)?;
    let content = archive.content();
    let derived = archive.derived();

    // The stores resolve a beginning by a shard listing, the way `about`'s
    // index resolves one by a range scan — this is the content-facing
    // door, so the stores' own answer is the one that counts. content/
    // and derived/ share one name space: the same bytes may sit in both,
    // so a beginning must be unique across their union, not in each.
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
    let needed = content.min_prefix().max(derived.min_prefix());
    if bare.len() < needed {
        return Err(anyhow!(
            "{bare:?} is too short to look up — the stores are filed by the first {needed} characters, give at least that many"
        ));
    }
    let mut candidates = content.matching(bare)?;
    for extra in derived.matching(bare)? {
        if !candidates.iter().any(|held| held == &extra) {
            candidates.push(extra);
        }
    }
    let digest = match candidates.as_slice() {
        [] => return Err(anyhow!("the archive holds nothing beginning with {bare:?}")),
        [one] => one.clone(),
        many => {
            return Err(anyhow!(
                "{bare:?} begins {} names — give more of it to name only one",
                many.len()
            ));
        }
    };
    let mut reader = match content.reader(&digest)? {
        Some(reader) => reader,
        None => derived
            .reader(&digest)?
            .ok_or_else(|| anyhow!("{algorithm}:{digest}: gone between naming and reading"))?,
    };

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
        if bare.len() < digest.as_str().len() && !quiet {
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
