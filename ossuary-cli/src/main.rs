//! ossuary: the command line onto an archive.
//!
//! Thin on purpose: parsing, wording and exit codes live here, and nothing
//! else does — every decision about the archive itself is `ossuary-core`'s.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result, anyhow};
use clap::{Parser, Subcommand};
use ossuary_core::{
    Algorithm, Archive, Attribute, Error, Field, Index, Scope, Subject, Sweep, Term, Value,
};

mod audit;
mod browse;
mod export;
mod extract;
mod maintain;
mod output;

#[derive(Parser)]
#[command(
    name = "ossuary",
    version,
    about = "A personal archive that keeps files permanently and records what is known about them",
    after_help = "Commands by group:
  archive    init, audit, maintain
  record     ingest, extract, annotate, retract, seal
  query      about, standing, find, attributes, history, ls, tree, id
  retrieve   get, export

Any other command NAME runs the program ossuary-NAME from the PATH:
`ossuary mount` runs ossuary-mount, `ossuary mailvault` runs ossuary-mailvault."
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

    /// Print only results and errors
    #[arg(short, long, global = true)]
    quiet: bool,

    /// List every item where the output would otherwise show only a
    /// count
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create an empty archive, or add missing files to an existing one
    ///
    /// On an existing archive, init changes nothing that is there and
    /// adds what is missing (currently config.toml, for archives created
    /// before it existed).
    Init {
        /// The hash algorithm that names the files, fixed when the
        /// archive is created. Default: sha256; blake3 is faster on CPUs
        /// without SHA instructions
        #[arg(long, value_name = "NAME")]
        algorithm: Option<String>,
    },
    /// Add files and directory trees to the archive
    ///
    /// Every regular file under the given paths is added, except paths
    /// that the archive's config.toml excludes; a file named directly is
    /// always added. For each file, ingest records its path, name, host,
    /// size, MIME type and modification time. All files of one call form
    /// one run. The files are only read. Files unchanged since the last
    /// run are skipped, so a repeated ingest reads only new and changed
    /// files. Archives found under the paths are skipped; naming an
    /// archive, or a path inside one, is an error.
    ///
    /// Recorded paths under a named directory where the file no longer
    /// exists are retracted. The content and its other claims are kept,
    /// and --as-of a time before the run still shows the path. A named
    /// directory in which no file is found is left alone unless
    /// --emptied is given. --collect retracts nothing.
    Ingest {
        /// Files and directories to add
        #[arg(value_name = "PATH", required = true)]
        paths: Vec<PathBuf>,

        /// Tag every file this run records with user:tag; may be
        /// repeated. Files skipped as unchanged are not tagged; --full
        /// includes them
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,

        /// Read every file, also those unchanged since the last run
        #[arg(long)]
        full: bool,

        /// Retract no paths; for a directory that is emptied after each
        /// ingest
        #[arg(long, conflicts_with = "emptied")]
        collect: bool,

        /// Retract recorded paths without a file under the named
        /// directories, also when a directory is empty or no longer
        /// exists
        #[arg(long)]
        emptied: bool,

        /// Show how many files and bytes would be added and how many
        /// paths retracted, write nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Run extractors on files they have not examined yet
    ///
    /// NAME selects the program: `ossuary extract image` runs
    /// `ossuary-extract-image` from the PATH with every contract it
    /// offers; NAME:CONTRACT runs one contract, as in `ossuary extract
    /// packed:list`. Without NAME, the extractors listed under
    /// `[extract] run` in the archive's config.toml run. Extraction
    /// repeats in rounds until a round finds nothing new to examine, so
    /// a chain like mail, attachment, text is processed in one call.
    /// Every file is recorded as examined, whether anything was found or
    /// not, so a repeated call examines only new files; a new extractor
    /// version examines all files again. Files an extractor derives (an
    /// attachment, extracted text) are added to the archive and linked
    /// to their origin.
    ///
    /// SUBJECTs limit the call to these files, examined once, in a
    /// single round; a named file is passed to the extractor even if it
    /// is of a type the extractor does not read. A run id selects every
    /// file of that run, and files and run ids can be mixed. --dry-run
    /// shows what would be recorded for the named files (claims, and
    /// each derived file with name, type and size) and writes nothing;
    /// it requires SUBJECTs.
    Extract {
        /// The extractor to run: the part of the program name after
        /// `ossuary-extract-`, optionally with `:CONTRACT`. Default: the
        /// archive's `[extract] run` list
        #[arg(value_name = "NAME")]
        name: Option<String>,

        /// Examine only these files: a hex name or a prefix of it, or a
        /// run id
        #[arg(value_name = "SUBJECT")]
        subjects: Vec<String>,

        /// Examine files again even if they were examined before
        #[arg(long)]
        full: bool,

        /// Show what would be recorded for the named files, write
        /// nothing
        #[arg(long)]
        dry_run: bool,

        /// Directory for derived files before they are stored (default:
        /// cache/tmp in the archive)
        #[arg(long, value_name = "DIR")]
        temp_dir: Option<PathBuf>,
    },
    /// Add tags and comments to recorded files
    ///
    /// Each --comment and --tag is recorded as a user:comment or user:tag
    /// claim with source user on every named file. Comments and tags add
    /// up: a second comment does not replace the first, and `about`
    /// shows all of them with their time. All names are resolved before
    /// anything is written, so a mistyped name writes nothing. To tag
    /// files while ingesting them, use `ingest --tag`; to annotate the
    /// files a query finds: `ossuary find --id … | xargs ossuary
    /// annotate --tag …`
    Annotate {
        /// Files to annotate, by hex name or a unique prefix of it
        #[arg(value_name = "SUBJECT", required = true)]
        subjects: Vec<String>,

        /// Add a comment (user:comment); may be repeated
        #[arg(long = "comment", value_name = "TEXT")]
        comments: Vec<String>,

        /// Add a tag (user:tag); may be repeated
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,
    },
    /// Retract attribute values from files
    ///
    /// TARGETs are files and attribute=value pairs, in any order. A file
    /// is given by its hex name or a prefix of it. attribute=value
    /// retracts that value from every named file; attribute=.. retracts
    /// every standing value of the attribute. Values are literal, in the
    /// form `standing` prints them: globs and ranges are rejected, and a
    /// value that looks like one is written in double quotes. To retract
    /// from the files a query finds: `ossuary find --id … | xargs
    /// ossuary retract user:tag=old`.
    ///
    /// Everything is resolved before anything is written: if a pair is
    /// not standing on one of the named files, nothing is retracted. A
    /// retraction is a claim with source user, so `about` still shows
    /// the retracted value, --as-of a time before the retraction still
    /// returns it, and exporting a run still uses the paths the run
    /// recorded. Any claim can be retracted, including an extractor's; a
    /// later `extract --full` or a new extractor version may record it
    /// again. To restore a value, record it again: your own tags and
    /// comments with `annotate`, an extractor's claims with `extract
    /// NAME FILE --full`.
    Retract {
        /// Files and attribute=value pairs, in any order
        #[arg(value_name = "TARGET", required = true)]
        targets: Vec<String>,

        /// Show what would be retracted, write nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Close the open segment; its claims become part of the sealed log
    Seal,
    /// Show all claims about a file, oldest first
    About {
        /// The file's hex name, or a unique prefix of it
        #[arg(value_name = "SUBJECT")]
        subject: String,

        /// Show only these attributes; a name ending in `:` selects a
        /// namespace, like `exif:`
        #[arg(value_name = "ATTRIBUTE")]
        attributes: Vec<String>,

        /// Print each claim as a JSON line, as stored in the log
        #[arg(short, long)]
        json: bool,

        /// Use only claims recorded up to TIME: an RFC 3339 time, a date
        /// (up to the end of that day), or a run id (up to its last
        /// claim)
        #[arg(long, value_name = "TIME")]
        as_of: Option<String>,
    },
    /// Show a file's standing values
    ///
    /// `about` shows every claim, retractions included; `standing` shows
    /// the result: retractions applied, duplicates removed. Without
    /// ATTRIBUTEs, every standing value is printed as an attribute=value
    /// pair, one per line, in the form `find` takes as a term.
    /// ATTRIBUTEs restrict the output, and a name ending in `:` selects a
    /// namespace, like `exif:`. With exactly one attribute, only the
    /// values are printed, one per line, strings without quotes; several
    /// lines mean the attribute has several values. If there is no
    /// standing value, nothing is printed and the exit code is 1.
    Standing {
        /// The file's hex name, or a unique prefix of it
        #[arg(value_name = "SUBJECT")]
        subject: String,

        /// Show only these attributes; a name ending in `:` selects a
        /// namespace. With exactly one attribute, print only its values
        #[arg(value_name = "ATTRIBUTE")]
        attributes: Vec<String>,

        /// Print one JSON object with each attribute's values as a list
        #[arg(short, long)]
        json: bool,

        /// Use only claims recorded up to TIME: an RFC 3339 time, a date
        /// (up to the end of that day), or a run id (up to its last
        /// claim)
        #[arg(long, value_name = "TIME")]
        as_of: Option<String>,
    },
    /// Find files that match all terms
    ///
    /// A term is attribute=value, and a file must match every term:
    /// `find file:mime=image/jpeg exif:make=Google` finds files that are
    /// both. In text values, `*` and `?` are wildcards: `find
    /// file:path=*crete*` finds files from that folder. low..high matches
    /// a value in the range, and either end may be left open:
    /// `file:modified=2026-09-01..` is "changed since September",
    /// `file:size=..4096` "at most 4 KiB". A bare `..` matches any value,
    /// so attribute=.. requires the attribute to be present. A value in
    /// double quotes is literal: no wildcards, no range. --missing
    /// selects files without an attribute (or, ending in `:`, without
    /// any attribute of the namespace): `find file:mime=image/jpeg
    /// --missing exif:` finds photos without EXIF data. Only standing
    /// values are searched.
    ///
    /// A name without a colon is a field of the claim: subject,
    /// attribute, value, time, source, run, retract. A field term
    /// applies to the claims of all attribute terms: `find run=RUN
    /// file:name=*` finds the files a run recorded, `find source=user
    /// user:tag` the tags you set yourself, `find time=2026-09-01..`
    /// what was recorded since September. A date covers the whole day,
    /// as with --as-of. Field values take the same patterns and ranges
    /// as attribute values.
    ///
    /// Each match is printed as its short name (the shortest unique
    /// prefix) with the shown attributes indented below it, one
    /// attribute=value pair per line in the same form as a term. The
    /// attributes of the terms are shown, unless bare attributes are
    /// given; then only those are shown: `find file:name=*.pdf
    /// file:modified` shows the modification times. A namespace like
    /// `exif:` shows all its attributes and a bare field name shows the
    /// field; a field term shows nothing. With only bare attributes,
    /// every file is listed. --id prints only the full names, one per
    /// line, for `about`, `standing`, `get` or `export`; --json prints
    /// one JSON object per match, values as lists.
    ///
    /// --with-derived also shows the files derived from each match, as a
    /// tree below it, each with its `file:mime`. --with-origin also shows
    /// the files each match was derived from, as a tree with the
    /// original at the top and the match at the bottom; a file derived
    /// from several origins (the same attachment in two mails) appears
    /// once per origin. With --id the names are printed as a flat list,
    /// each origin before the files derived from it, the order `export`
    /// needs; with --json, derived files are nested under `derived`.
    ///
    /// Only files that are still present are listed: files with a
    /// standing `file:path`, or files derived from one. --all searches
    /// every claim ever written, including retracted values and files
    /// no longer present, and is required for the retract field: `find
    /// --all retract=true file:path=*` finds retracted paths. Under
    /// --all, a shown value may no longer be standing; `about` shows
    /// which.
    Find {
        /// attribute=value or field=value; all terms must match. A bare
        /// attribute, namespace: or field selects what is shown
        #[arg(value_name = "TERM")]
        terms: Vec<String>,

        /// Only files without this attribute (or, ending in `:`, without
        /// any attribute of this namespace); may be repeated
        #[arg(long, value_name = "ATTRIBUTE")]
        missing: Vec<String>,

        /// Print only the full names, one per line
        #[arg(long)]
        id: bool,

        /// Print one JSON object per match: the full name and each shown
        /// attribute's values as a list
        #[arg(short, long)]
        json: bool,

        /// Use only claims recorded up to TIME: an RFC 3339 time, a date
        /// (up to the end of that day), or a run id (up to its last
        /// claim)
        #[arg(long, value_name = "TIME")]
        as_of: Option<String>,

        /// Search all claims ever written, including retracted values
        /// and files no longer present
        #[arg(long)]
        all: bool,

        /// Also show the files derived from each match, as a tree
        #[arg(long)]
        with_derived: bool,

        /// Also show the files each match was derived from, as a tree
        #[arg(long)]
        with_origin: bool,
    },
    /// List all attributes with standing values
    ///
    /// One attribute per line, sorted, without values, in the form
    /// `find` takes: `ossuary find $(ossuary attributes mail:)` shows
    /// everything recorded about mail. NAMESPACEs, written with their
    /// colon like `exif:`, restrict the list. An attribute whose values
    /// were all retracted is not listed; `about` still shows its claims.
    /// --count prefixes each attribute with the number of files that
    /// have it, so `sort -rn` ranks them; --json prints one object per
    /// attribute with name and count.
    Attributes {
        /// Only these namespaces, written with their colon, like
        /// `exif:`; may be repeated
        #[arg(value_name = "NAMESPACE:")]
        namespaces: Vec<String>,

        /// Prefix each attribute with the number of files that have it
        #[arg(long)]
        count: bool,

        /// Print one JSON object per attribute: its name and the number
        /// of files that have it
        #[arg(short, long)]
        json: bool,

        /// Use only claims recorded up to TIME: an RFC 3339 time, a date
        /// (up to the end of that day), or a run id (up to its last
        /// claim)
        #[arg(long, value_name = "TIME")]
        as_of: Option<String>,
    },
    /// List all runs, oldest first
    ///
    /// One line per run, that is, per call that wrote claims: ingest,
    /// extract, annotate, retract, and external commands such as
    /// `ossuary mailvault`. Each line shows when the run ended, in the
    /// format --as-of takes; the run id, which export, extract and
    /// --as-of take; the number of files and claims written, and how
    /// many of the claims are retractions; and the sources of the
    /// claims. `find run=ID …` shows what a run recorded. Claims written
    /// before runs were recorded belong to no run and are not listed.
    /// --json prints one object per run, with its first and last time.
    History {
        /// Print one JSON object per run: id, first and last time,
        /// counts, sources
        #[arg(short, long)]
        json: bool,

        /// Only runs finished by TIME: an RFC 3339 time, a date (up to
        /// the end of that day), or a run id (up to its last claim)
        #[arg(long, value_name = "TIME")]
        as_of: Option<String>,
    },
    /// List the recorded files and folders at a path
    ///
    /// PLACE is an absolute path as ingest recorded it, from whichever
    /// machine the file was on; without PLACE, the top level is listed.
    /// Folders end with a slash. Each file is on a line of its own, its
    /// short name first (the name `about`, `get` and `export` take), as
    /// in `export --dry-run`. The listing comes from the record, not
    /// from a disk: a path is listed until it is retracted, even if the
    /// disk is gone. If different files were recorded at the same path
    /// over time, each gets its own line.
    Ls {
        /// An absolute path (default: /)
        #[arg(value_name = "PLACE")]
        place: Option<String>,

        /// Print one JSON object per entry: a file with the full names
        /// of all files recorded at it, or a folder
        #[arg(short, long)]
        json: bool,

        /// Use only claims recorded up to TIME: an RFC 3339 time, a date
        /// (up to the end of that day), or a run id (up to its last
        /// claim)
        #[arg(long, value_name = "TIME")]
        as_of: Option<String>,
    },
    /// Show the recorded files and folders below a path as a tree
    ///
    /// The same listing as `ls`, for everything below PLACE: one branch
    /// per folder, each file with its short name in brackets. Like `ls`,
    /// it lists paths from every machine files were ingested from, until
    /// they are retracted.
    Tree {
        /// An absolute path (default: /)
        #[arg(value_name = "PLACE")]
        place: Option<String>,

        /// Use only claims recorded up to TIME: an RFC 3339 time, a date
        /// (up to the end of that day), or a run id (up to its last
        /// claim)
        #[arg(long, value_name = "TIME")]
        as_of: Option<String>,
    },
    /// Print the name a file would have in the archive
    ///
    /// Hashes the file the way the archive names files, without adding
    /// it, and reports whether the archive already contains it. The name
    /// depends only on the file's content, so this works before and
    /// after an ingest.
    Id {
        /// The file to hash
        #[arg(value_name = "FILE")]
        path: PathBuf,
    },
    /// Write a file's content to stdout
    ///
    /// The content is written unchanged; --output writes it to a file
    /// instead.
    Get {
        /// The file's hex name, or a unique prefix of it
        #[arg(value_name = "SUBJECT")]
        subject: String,

        /// Write to FILE instead of stdout
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,
    },
    /// Copy files out of the archive under their recorded paths
    ///
    /// Each ID is a file (its hex name as find, id and get print it, or
    /// a unique prefix) or a run (its full run id as ingest and extract
    /// print it); both can be mixed. A run exports every file it
    /// recorded, under the path it recorded. Paths are made relative:
    /// folders shared by all exported files are left out, so files that
    /// were side by side are exported side by side. If the files come
    /// from unrelated folders, each group gets its own folder under
    /// PATH, named after the deepest folder its files share. A file
    /// given by name is exported to every standing path recorded for
    /// it, a derived file under its recorded names. Content recorded at
    /// several paths is exported to each of them. Existing files in PATH
    /// are never overwritten: a file with the same content counts as
    /// exported, a file with different content is reported as a failure
    /// and left unchanged. A destination inside the archive is refused.
    /// `ossuary find --id … | xargs ossuary export PATH` exports the
    /// files a query finds.
    Export {
        /// Directory to export into; created if missing
        #[arg(value_name = "PATH")]
        destination: PathBuf,

        /// A file, by hex name or a unique prefix, or a run, by its run
        /// id; several may be given
        #[arg(value_name = "ID", required = true)]
        ids: Vec<String>,

        /// Show where each file would be written, write nothing
        #[arg(long)]
        dry_run: bool,

        /// Use only claims recorded up to TIME: an RFC 3339 time, a date
        /// (up to the end of that day), or a run id (up to its last
        /// claim)
        #[arg(long, value_name = "TIME")]
        as_of: Option<String>,
    },
    /// Verify file hashes, claims and the chain of sealed segments
    ///
    /// Reads the whole archive, without using the cache. Three checks:
    /// every file in content/ and derived/ is re-hashed and compared
    /// with its name; every sealed segment and the open segment in
    /// claims/ must be readable claim by claim, and every segment named
    /// as a predecessor must be present; every file the claims refer
    /// to, as subject or as origin of a derived file, must be present in
    /// content/ or derived/.
    ///
    /// A chain of sealed segments in more than one piece is also a
    /// finding: a segment without a predecessor, other than the first,
    /// shows that the open segment was lost with its claims and started
    /// again. The report lists every piece with its time span, so the
    /// lost period can be narrowed down and recorded again; `ossuary
    /// maintain mend` then joins the pieces. A mended break is reported,
    /// not counted as a finding. Also reported and not counted: files
    /// without claims (adding the same file again records them) and
    /// files in both content/ and derived/ (`ossuary maintain weed`
    /// removes the copy in derived/).
    ///
    /// The report counts what it finds and lists up to five names per
    /// finding; --verbose lists all of them, --json prints one object
    /// per finding. Exits 0 if the archive is sound, 1 if there are
    /// findings.
    Audit {
        /// Print one JSON object per finding or observation
        #[arg(short, long)]
        json: bool,
    },
    /// Repair and clean up the archive
    #[command(subcommand)]
    Maintain(maintain::Maintenance),
    // An outside verb: `ossuary NAME …` becomes `ossuary-NAME …` from
    // the PATH, the way `mount` arrives without weighing this tool
    // down. The resolved archive travels in the environment; the rest
    // of the line goes through word for word.
    #[command(external_subcommand)]
    Outside(Vec<OsString>),
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

#[allow(
    clippy::too_many_lines,
    reason = "one arm per verb — the table is the point, and splitting it hides the map"
)]
fn run(cli: Cli) -> Result<ExitCode> {
    let quiet = cli.quiet;
    match cli.command {
        Command::Init { algorithm } => init(&cli.archive, algorithm.as_deref()),
        Command::Ingest {
            paths,
            tags,
            full,
            collect,
            emptied,
            dry_run,
        } => ingest(
            &cli.archive,
            &paths,
            &tags,
            Switches {
                full,
                collect,
                emptied,
                dry_run,
            },
            quiet,
        ),
        Command::Extract {
            name,
            subjects,
            full,
            dry_run,
            temp_dir,
        } => extract::extract(
            &cli.archive,
            name.as_deref(),
            &subjects,
            full,
            dry_run,
            temp_dir.as_deref(),
            quiet,
        ),
        Command::Annotate {
            subjects,
            comments,
            tags,
        } => annotate(&cli.archive, &subjects, &comments, &tags, quiet),
        Command::Retract { targets, dry_run } => retract(&cli.archive, &targets, dry_run, quiet),
        Command::Seal => seal(&cli.archive),
        Command::About {
            subject,
            attributes,
            json,
            as_of,
        } => about(
            &cli.archive,
            &subject,
            &attributes,
            json,
            as_of.as_deref(),
            quiet,
        ),
        Command::Standing {
            subject,
            attributes,
            json,
            as_of,
        } => standing(
            &cli.archive,
            &subject,
            &attributes,
            json,
            as_of.as_deref(),
            quiet,
        ),
        Command::Find {
            terms,
            missing,
            id,
            json,
            as_of,
            all,
            with_derived,
            with_origin,
        } => find(
            &cli.archive,
            &terms,
            &missing,
            id,
            json,
            as_of.as_deref(),
            if all { Scope::Record } else { Scope::Present },
            Along {
                derived: with_derived,
                origin: with_origin,
            },
            quiet,
        ),
        Command::Attributes {
            namespaces,
            count,
            json,
            as_of,
        } => attributes(
            &cli.archive,
            &namespaces,
            count,
            json,
            as_of.as_deref(),
            quiet,
        ),
        Command::History { json, as_of } => history(&cli.archive, json, as_of.as_deref(), quiet),
        Command::Ls { place, json, as_of } => browse::ls(
            &cli.archive,
            place.as_deref(),
            json,
            as_of.as_deref(),
            quiet,
        ),
        Command::Tree { place, as_of } => {
            browse::tree(&cli.archive, place.as_deref(), as_of.as_deref(), quiet)
        }
        Command::Id { path } => id(&cli.archive, &path, quiet),
        Command::Get { subject, output } => get(&cli.archive, &subject, output.as_deref(), quiet),
        Command::Export {
            destination,
            ids,
            dry_run,
            as_of,
        } => export::export(
            &cli.archive,
            &destination,
            &ids,
            dry_run,
            as_of.as_deref(),
            quiet,
        ),
        Command::Audit { json } => audit::audit(&cli.archive, json, cli.verbose, quiet),
        Command::Maintain(task) => maintain::run(&cli.archive, &task, cli.verbose, quiet),
        Command::Outside(pieces) => outside(&cli.archive, &pieces),
    }
}

/// Hand the line to an outside verb: `ossuary NAME …` becomes
/// `ossuary-NAME …` found on the PATH and *becomes* this process —
/// signals, exit code and all. The archive travels resolved: however
/// it was named — flag, environment, or standing in it — the child
/// sees one absolute `OSSUARY_ARCHIVE` and resolves nothing itself.
fn outside(root: &Path, pieces: &[OsString]) -> Result<ExitCode> {
    use std::os::unix::process::CommandExt as _;
    let Some((name, rest)) = pieces.split_first() else {
        return Err(anyhow!(
            "no command given; `ossuary --help` lists the commands"
        ));
    };
    let name = name.to_string_lossy();
    let program = format!("ossuary-{name}");
    let archive = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let error = std::process::Command::new(&program)
        .args(rest)
        .env("OSSUARY_ARCHIVE", &archive)
        .exec();
    if error.kind() == std::io::ErrorKind::NotFound {
        return Err(anyhow!(
            "unknown command `{name}`, and no `{program}` on the PATH; `ossuary --help` lists the commands"
        ));
    }
    Err(anyhow::Error::new(error).context(format!("running {program}")))
}

/// One answer line onto stdout. `Ok(true)` means written; `Ok(false)`
/// means the reader closed the pipe — it has all it wanted, so the
/// answer ends there and the run counts as a success, the way `get`
/// has always read it. Any other write trouble is real.
pub(crate) fn say(out: &mut impl std::io::Write, line: &str) -> Result<bool> {
    match writeln!(out, "{line}") {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(false),
        Err(error) => Err(anyhow::Error::new(error).context("writing to stdout")),
    }
}

/// The archive, or the way to one.
pub(crate) fn open(root: &Path) -> Result<Archive> {
    Archive::open(root).map_err(|error| match error {
        Error::NoArchive(path) => anyhow!(
            "{}: not an ossuary archive; run in an archive, name one with --archive, or create one with `ossuary init`",
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
                "{}: empty archive created, settings in config.toml; add files with `ossuary ingest DIR`",
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
                        "{}: already an archive using {}; the algorithm cannot be changed after creation",
                        archive.root().display(),
                        standing.name()
                    ));
                }
            }
            if archive.complete()? {
                println!(
                    "{}: already an archive; config.toml was missing and has been created with the defaults",
                    archive.root().display()
                );
            } else {
                println!(
                    "{}: already an archive, nothing missing",
                    archive.root().display()
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(error) => Err(error.into()),
    }
}

/// The switches of `ingest`, as the user set them.
#[derive(Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "four flags of one verb, carried together as they were set"
)]
struct Switches {
    full: bool,
    collect: bool,
    emptied: bool,
    dry_run: bool,
}

fn ingest(
    root: &Path,
    paths: &[PathBuf],
    tags: &[String],
    switches: Switches,
    quiet: bool,
) -> Result<ExitCode> {
    let Switches {
        full,
        collect,
        emptied,
        dry_run,
    } = switches;
    if let Some(empty) = tags.iter().find(|tag| tag.trim().is_empty()) {
        return Err(anyhow!(
            "--tag {empty:?} is empty; give it a value or leave it out"
        ));
    }
    let archive = open(root)?;
    let host = gethostname::gethostname().to_string_lossy().into_owned();

    if !quiet {
        eprintln!("archive {}", archive.root().display());
        // One path is named; many — a glob's expansion — are counted,
        // the verdict tells how they fared.
        match paths {
            [one] => eprintln!("ingesting {}", one.display()),
            many => eprintln!("ingesting {} paths", many.len()),
        }
    }
    let memory = if full {
        None
    } else {
        Some(archive.ingest_memory()?)
    };
    // What the record stands by, caught up to the log: the places the
    // walk holds what it met against. Collecting holds nothing against
    // anything, and needs no index for it.
    let record = if collect {
        None
    } else {
        let mut record = archive.index()?;
        catch_up(&mut record, &archive, quiet)?;
        Some(record)
    };
    let sweep = Sweep {
        host: &host,
        tags,
        excludes: archive.config().excludes(),
        memory: memory.as_ref(),
        record: record.as_ref(),
        emptied,
    };
    if dry_run {
        return previewed(paths, &sweep, quiet);
    }
    let run = ossuary_core::ingest(archive.content(), archive.log(), paths, &sweep)?;

    // Each archive met is named where the run talks; the verdict keeps
    // the count. So is each root met empty.
    if !quiet {
        for path in &run.archives {
            eprintln!("{}: ossuary archive, skipped", path.display());
        }
        for path in &run.empty {
            eprintln!(
                "{}: no files found, recorded paths under it kept; if it was emptied on purpose, `ossuary ingest --emptied` retracts them",
                path.display()
            );
        }
    }
    println!("{}", ingest_verdict(&run, tags));
    if run.failed.is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("{} file(s) failed:", run.failed.len());
        for (path, error) in &run.failed {
            eprintln!("  {}: {}", path.display(), error.spelled());
        }
        Ok(ExitCode::FAILURE)
    }
}

/// The last line of a run: what it did, count by count, and what went
/// on the record.
fn ingest_verdict(run: &ossuary_core::Ingested, tags: &[String]) -> String {
    let mut verdict = vec![format!("{} file(s) stored", run.stored)];
    if run.known > 0 {
        verdict.push(format!("{} already stored", run.known));
    }
    if run.unchanged > 0 {
        verdict.push(if tags.is_empty() {
            format!("{} unchanged since the last run", run.unchanged)
        } else {
            // A tag rides only on what the run records; saying so here
            // beats a tree that looks tagged and is not.
            format!(
                "{} unchanged since the last run, not tagged (--full tags them too)",
                run.unchanged
            )
        });
    }
    if run.excluded > 0 {
        verdict.push(format!("{} path(s) excluded by config.toml", run.excluded));
    }
    if run.gone > 0 {
        verdict.push(format!("{} path(s) no longer found, retracted", run.gone));
    }
    if !run.empty.is_empty() {
        verdict.push(format!(
            "{} director{} without files, recorded paths kept",
            run.empty.len(),
            if run.empty.len() == 1 { "y" } else { "ies" }
        ));
    }
    if !run.archives.is_empty() {
        verdict.push(format!("{} archive(s) skipped", run.archives.len()));
    }
    let record = if run.claims > 0 {
        format!("{} claim(s) written, run {}", run.claims, run.run)
    } else {
        "0 claims written".to_string()
    };
    format!("{}; {record}", verdict.join(", "))
}

/// The --dry-run answer: [`ossuary_core::preview`]'s findings, worded
/// like the run they spare.
fn previewed(paths: &[PathBuf], sweep: &Sweep<'_>, quiet: bool) -> Result<ExitCode> {
    let run = ossuary_core::preview(paths, sweep)?;
    if !quiet {
        for path in &run.archives {
            eprintln!("{}: ossuary archive, skipped", path.display());
        }
        for path in &run.empty {
            eprintln!(
                "{}: no files found, recorded paths under it would be kept; if it was emptied on purpose, `ossuary ingest --emptied` retracts them",
                path.display()
            );
        }
    }
    let mut verdict = vec![format!(
        "would ingest {} file(s), {}",
        run.files,
        output::human_bytes(run.bytes)
    )];
    if run.unchanged > 0 {
        verdict.push(format!("{} unchanged since the last run", run.unchanged));
    }
    if run.excluded > 0 {
        verdict.push(format!("{} path(s) excluded by config.toml", run.excluded));
    }
    if !run.gone.is_empty() {
        verdict.push(format!(
            "{} path(s) no longer found, would be retracted",
            run.gone.len()
        ));
    }
    if !run.empty.is_empty() {
        verdict.push(format!(
            "{} director{} without files, recorded paths would be kept",
            run.empty.len(),
            if run.empty.len() == 1 { "y" } else { "ies" }
        ));
    }
    if !run.archives.is_empty() {
        verdict.push(format!("{} archive(s) skipped", run.archives.len()));
    }
    println!("{}; nothing written", verdict.join(", "));
    if run.failed.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    eprintln!("{} could not be read:", run.failed.len());
    for (path, error) in &run.failed {
        eprintln!("  {}: {}", path.display(), error.spelled());
    }
    Ok(ExitCode::FAILURE)
}

fn annotate(
    root: &Path,
    given: &[String],
    comments: &[String],
    tags: &[String],
    quiet: bool,
) -> Result<ExitCode> {
    if comments.is_empty() && tags.is_empty() {
        return Err(anyhow!(
            "nothing to annotate; give --comment TEXT, --tag TAG, or both"
        ));
    }
    if comments.iter().any(|text| text.trim().is_empty()) {
        return Err(anyhow!("--comment is empty; give it text or leave it out"));
    }
    if let Some(empty) = tags.iter().find(|tag| tag.trim().is_empty()) {
        return Err(anyhow!(
            "--tag {empty:?} is empty; give it a value or leave it out"
        ));
    }
    let archive = open(root)?;
    let mut index = archive.index()?;
    catch_up(&mut index, &archive, quiet)?;

    // Every name resolves before anything is written: a mistake in the
    // third of five must not leave the first two half-annotated.
    let mut subjects: Vec<Subject> = Vec::new();
    for name in given {
        let Some(subject) = resolve(&index, name)? else {
            return Err(anyhow!("no subject begins with {name:?}; nothing written"));
        };
        if !subjects.contains(&subject) {
            subjects.push(subject);
        }
    }
    let written = ossuary_core::annotate(archive.log(), &subjects, comments, tags)?;
    println!(
        "{} file(s) annotated, {} claim(s) written, run {}",
        subjects.len(),
        written.claims,
        written.run
    );
    Ok(ExitCode::SUCCESS)
}

/// What one pair asks to take back: the exact value, or all of them.
#[derive(Debug, PartialEq)]
enum Asked {
    Literal(String),
    All,
}

/// A pair's value part, read the way the answers spell one: double
/// quotes mean the characters themselves, `..` alone means all of it,
/// and anything that reads like a glob or a range is refused — retract
/// takes back what is named, and what matches is find's business.
fn asked(attribute: &Attribute, raw: &str) -> Result<Asked> {
    if raw == ".." {
        return Ok(Asked::All);
    }
    if let Some(inner) = raw
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        return Ok(Asked::Literal(inner.to_string()));
    }
    let name = attribute.as_str();
    if raw.is_empty() {
        return Err(anyhow!(
            "{name}= has no value; give a value, or {name}=.. to retract all values"
        ));
    }
    if raw.contains("..") {
        return Err(anyhow!(
            "{name}={raw} looks like a range, which retract does not accept; for the literal value, quote it: {name}=\"{raw}\""
        ));
    }
    if raw.contains('*') || raw.contains('?') {
        return Err(anyhow!(
            "{name}={raw} looks like a pattern, which retract does not accept; to retract from matching files: `ossuary find --id {name}={raw} | xargs ossuary retract …`; for the literal value, quote it: {name}=\"{raw}\""
        ));
    }
    Ok(Asked::Literal(raw.to_string()))
}

/// Whether a standing value is the one a literal names: a string by
/// its text, anything else by its JSON spelling — the way `standing`
/// and `find` print them, so what the answer showed is what the
/// taking names.
fn named_value(value: &Value, literal: &str) -> bool {
    match value {
        Value::String(text) => text == literal,
        #[allow(
            clippy::cmp_owned,
            reason = "a number's JSON spelling exists only once rendered — Value's own == would compare types, not spellings"
        )]
        other => other.to_string() == literal,
    }
}

/// What the pairs take back on one subject, checked against what
/// stands: the takings to write, and — grouped for showing — every
/// value that would fall. A pair standing nowhere on the subject is an
/// error: a taking that changes nothing must not look like one that did.
#[allow(
    clippy::type_complexity,
    reason = "the two halves of one answer — splitting them would only rename the tuple"
)]
fn matched(
    index: &Index,
    subject: &Subject,
    pairs: &[(Attribute, Asked)],
) -> Result<(
    Vec<(Subject, Attribute, ossuary_core::Taking)>,
    Vec<(String, Vec<Value>)>,
)> {
    let mut takings = Vec::new();
    let mut falling: Vec<(String, Vec<Value>)> = Vec::new();
    for (attribute, what) in pairs {
        let standing = index.values(subject, attribute, Scope::Held)?;
        match what {
            Asked::All => {
                if standing.is_empty() {
                    return Err(anyhow!(
                        "no standing value for {} on {subject}; nothing retracted; `ossuary about {}` shows all claims",
                        attribute.as_str(),
                        shorten(index, subject)?
                    ));
                }
                falling.push((attribute.as_str().to_string(), standing));
                takings.push((
                    subject.clone(),
                    attribute.clone(),
                    ossuary_core::Taking::All,
                ));
            }
            Asked::Literal(literal) => {
                let taken: Vec<Value> = standing
                    .into_iter()
                    .filter(|value| named_value(value, literal))
                    .collect();
                if taken.is_empty() {
                    return Err(anyhow!(
                        "{}={literal} is not a standing value on {subject}; nothing retracted; `ossuary standing {}` lists the standing values",
                        attribute.as_str(),
                        shorten(index, subject)?
                    ));
                }
                for value in taken {
                    takings.push((
                        subject.clone(),
                        attribute.clone(),
                        ossuary_core::Taking::Value(value.clone()),
                    ));
                    match falling
                        .iter_mut()
                        .find(|(known, _)| known == attribute.as_str())
                    {
                        Some((_, seen)) => seen.push(value),
                        None => falling.push((attribute.as_str().to_string(), vec![value])),
                    }
                }
            }
        }
    }
    Ok((takings, falling))
}

fn retract(root: &Path, targets: &[String], dry_run: bool, quiet: bool) -> Result<ExitCode> {
    // Shapes first, before the archive opens: a call that cannot mean
    // anything is refused without touching a thing.
    let mut names: Vec<&str> = Vec::new();
    let mut pairs: Vec<(Attribute, Asked)> = Vec::new();
    for target in targets {
        if let Some((name, raw)) = target.split_once('=') {
            let attribute = Attribute::parse(name)?;
            let asked = asked(&attribute, raw)?;
            if !pairs.contains(&(attribute.clone(), Asked::All))
                && !pairs
                    .iter()
                    .any(|(known, what)| *known == attribute && *what == asked)
            {
                pairs.push((attribute, asked));
            }
        } else if target.contains(':') {
            return Err(anyhow!(
                "{target} is an attribute without a value; use {target}=VALUE to retract one value, {target}=.. to retract all"
            ));
        } else if !names.contains(&target.as_str()) {
            names.push(target);
        }
    }
    if pairs.is_empty() {
        return Err(anyhow!(
            "nothing to retract; add attribute=value, or attribute=.. for all values"
        ));
    }
    if names.is_empty() {
        return Err(anyhow!(
            "no file given; add its hex name or a prefix of it (`ossuary find --id` prints names)"
        ));
    }

    let archive = open(root)?;
    let mut index = archive.index()?;
    catch_up(&mut index, &archive, quiet)?;

    // Everything resolves before anything is written: a mistake in the
    // third of five must not leave the first two half-retracted.
    let mut subjects: Vec<Subject> = Vec::new();
    for name in &names {
        let Some(subject) = resolve(&index, name)? else {
            return Err(anyhow!(
                "no subject begins with {name:?}; nothing retracted"
            ));
        };
        if !subjects.contains(&subject) {
            subjects.push(subject);
        }
    }
    let mut takings: Vec<(Subject, Attribute, ossuary_core::Taking)> = Vec::new();
    let mut told: Vec<String> = Vec::new();
    let mut values = 0usize;
    for subject in &subjects {
        let (subject_takings, falling) = matched(&index, subject, &pairs)?;
        takings.extend(subject_takings);
        values += falling.iter().map(|(_, seen)| seen.len()).sum::<usize>();
        let mut block = shorten(&index, subject)?;
        for line in output::pairs(&falling).lines() {
            block.push_str("\n  ");
            block.push_str(line);
        }
        told.push(block);
    }

    if dry_run {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        for block in &told {
            if !say(&mut out, block)? {
                break;
            }
        }
        if !quiet {
            eprintln!(
                "would retract {values} value(s) from {} file(s); nothing written",
                subjects.len()
            );
        }
        return Ok(ExitCode::SUCCESS);
    }
    let written = ossuary_core::retract(archive.log(), &takings)?;
    println!(
        "{values} value(s) retracted from {} file(s); {} retraction(s) written, run {}",
        subjects.len(),
        written.claims,
        written.run
    );
    Ok(ExitCode::SUCCESS)
}

fn seal(root: &Path) -> Result<ExitCode> {
    let archive = open(root)?;
    match archive.log().seal()? {
        Some(segment) => println!(
            "sealed as {}; the open segment is now empty",
            segment.digest()
        ),
        None => println!("nothing to seal; the open segment contains no claims"),
    }
    Ok(ExitCode::SUCCESS)
}

/// Fold the log in and, unless quiet, say when that was real work.
pub(crate) fn catch_up(index: &mut Index, archive: &Archive, quiet: bool) -> Result<()> {
    let folded = index.fold(archive.log())?;
    if folded.segments > 0 && !quiet {
        eprintln!("index updated: {} new log segment(s)", folded.segments);
    }
    Ok(())
}

/// The index a question asks: the archive's cache caught up — or,
/// under --as-of, a throwaway replay of everything recorded by then,
/// so the same door answers with that day's knowledge. A run id in
/// place of a time closes after that run's last claim.
pub(crate) fn index_at(archive: &Archive, as_of: Option<&str>, quiet: bool) -> Result<Index> {
    let mut index = archive.index()?;
    catch_up(&mut index, archive, quiet)?;
    let Some(given) = as_of else {
        return Ok(index);
    };
    match index.at(given) {
        Ok(Some((view, _))) => Ok(view),
        Ok(None) => Err(anyhow!(
            "run {given} not found; `ossuary history` lists the runs"
        )),
        Err(Error::Timestamp(_)) => Err(anyhow!(
            "{given:?} is not a valid time; give an RFC 3339 time like 2026-01-01T12:00:00Z, a date, or a run id"
        )),
        Err(error) => Err(error.into()),
    }
}

/// The subject as the log spells it, from whatever the user typed —
/// [`Index::resolve`], with a refused beginning worded by core. `None`
/// when nothing on the record begins that way.
pub(crate) fn resolve(index: &Index, given: &str) -> Result<Option<Subject>> {
    Ok(index.resolve(given)?)
}

fn about(
    root: &Path,
    subject: &str,
    attributes: &[String],
    json: bool,
    as_of: Option<&str>,
    quiet: bool,
) -> Result<ExitCode> {
    let archive = open(root)?;
    let index = index_at(&archive, as_of, quiet)?;

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

    let Some(subject) = resolve(&index, subject)? else {
        calm(format!("no subject begins with {subject:?}"));
        return Ok(ExitCode::SUCCESS);
    };

    let mut claims = index.about(&subject)?;
    if claims.is_empty() {
        calm(format!("no claims recorded about {subject}"));
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
                "no claims about {subject} for these attributes; leave out the attributes to see all claims"
            ));
            return Ok(ExitCode::SUCCESS);
        }
    }
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for claim in &claims {
        let line = if json {
            claim.to_line()
        } else {
            output::line(claim)
        };
        if !say(&mut out, &line)? {
            break;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn standing(
    root: &Path,
    subject: &str,
    attributes: &[String],
    json: bool,
    as_of: Option<&str>,
    quiet: bool,
) -> Result<ExitCode> {
    // Every spelling is checked before the archive opens: a mistyped
    // attribute refuses the call, not the middle of an answer.
    let mut projections: Vec<Projection> = Vec::new();
    for word in attributes {
        let projection = if let Some(namespace) = word.strip_suffix(':') {
            // The grammar has one door; a prefix walks through it
            // wearing a dummy name.
            Attribute::parse(&format!("{namespace}:a"))?;
            Projection::Namespace(namespace.to_string())
        } else {
            Projection::Attribute(Attribute::parse(word)?)
        };
        if !projections.contains(&projection) {
            projections.push(projection);
        }
    }
    // Exactly one attribute named: the asker knows what they asked, so
    // the label would be an echo — the values come bare, the way a
    // script wants them.
    let bare =
        attributes.len() == 1 && matches!(projections.as_slice(), [Projection::Attribute(_)]);

    let archive = open(root)?;
    let index = index_at(&archive, as_of, quiet)?;

    // Nothing standing is a testable answer, not a broken run: the exit
    // code carries it, and the sentence is for a reader wondering why
    // nothing came.
    let Some(subject) = resolve(&index, subject)? else {
        if !quiet {
            eprintln!("no subject begins with {subject:?}");
        }
        return Ok(ExitCode::FAILURE);
    };
    let shown = if projections.is_empty() {
        grouped(index.standing(&subject)?)
    } else {
        gather(&index, &subject, &projections, Scope::Held)?
    };
    if shown.is_empty() {
        if !quiet {
            eprintln!(
                "no standing values{} on {subject}; `ossuary about` shows every claim, retractions included",
                if attributes.is_empty() {
                    ""
                } else {
                    " for these attributes"
                },
            );
        }
        return Ok(ExitCode::FAILURE);
    }
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if json {
        say(&mut out, &output::json_line(subject.as_str(), &shown))?;
    } else if bare {
        for (_, values) in &shown {
            for value in values {
                let line = match value {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                if !say(&mut out, &line)? {
                    return Ok(ExitCode::SUCCESS);
                }
            }
        }
    } else {
        say(&mut out, &output::pairs(&shown))?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Rows ordered by attribute, folded into the shape an answer shows:
/// each attribute once, its standing values together.
fn grouped(rows: Vec<(Attribute, Value)>) -> Vec<(String, Vec<Value>)> {
    let mut shown: Vec<(String, Vec<Value>)> = Vec::new();
    for (attribute, value) in rows {
        match shown.last_mut() {
            Some((known, values)) if *known == attribute.as_str() => values.push(value),
            _ => shown.push((attribute.as_str().to_string(), vec![value])),
        }
    }
    shown
}

/// One thing a `find` match shows: an attribute, a whole namespace, or
/// a field of the claims. The question is the projection — the filters
/// show themselves until a bare name stands among the terms; then
/// explicit beats implicit, and only the bare ones show.
#[derive(Debug, Clone, PartialEq)]
enum Projection {
    Attribute(Attribute),
    Namespace(String),
    Field(Field),
}

/// The question taken apart: what narrows, and what is shown. Filter
/// terms show their own attributes only while no bare attribute stands
/// among the terms — naming one takes the showing over. A name without
/// a colon is a field of the claim.
fn question(terms: &[String], id_only: bool) -> Result<(Vec<Term>, Vec<Projection>)> {
    let mut filters = Vec::new();
    let mut asked: Vec<Projection> = Vec::new();
    let mut implied: Vec<Projection> = Vec::new();
    let remember = |projection: Projection, projections: &mut Vec<Projection>| {
        if !projections.contains(&projection) {
            projections.push(projection);
        }
    };
    for word in terms {
        if let Some((name, value)) = word.split_once('=') {
            if name.contains(':') {
                let attribute = Attribute::parse(name)?;
                remember(Projection::Attribute(attribute.clone()), &mut implied);
                filters.push(Term::Attribute(attribute, value.to_string()));
            } else {
                // A field term shows nothing of itself: it asks about the
                // claim, and the answer would only echo the question. A
                // bare field name is what shows a field.
                let field = Field::parse(name)?;
                filters.push(Term::Field(field, value.to_string()));
            }
        } else if let Some(namespace) = word.strip_suffix(':') {
            // The grammar has one door; a prefix walks through it
            // wearing a dummy name.
            Attribute::parse(&format!("{namespace}:a"))?;
            if id_only {
                return Err(anyhow!(
                    "{word:?} selects what to show, and --id prints only names; drop one of them"
                ));
            }
            remember(Projection::Namespace(namespace.to_string()), &mut asked);
        } else if word.contains(':') {
            let attribute = Attribute::parse(word)?;
            if id_only {
                return Err(anyhow!(
                    "{word:?} selects what to show, and --id prints only names; drop one of them"
                ));
            }
            remember(Projection::Attribute(attribute), &mut asked);
        } else {
            let field = Field::parse(word)?;
            if id_only {
                return Err(anyhow!(
                    "{word:?} selects what to show, and --id prints only names; drop one of them"
                ));
            }
            remember(Projection::Field(field), &mut asked);
        }
    }
    let projections = if asked.is_empty() { implied } else { asked };
    Ok((filters, projections))
}

#[allow(
    clippy::too_many_arguments,
    reason = "every one is a switch the user set on the command line, passed through as it came"
)]
fn find(
    root: &Path,
    terms: &[String],
    missing: &[String],
    id_only: bool,
    json: bool,
    as_of: Option<&str>,
    scope: Scope,
    along: Along,
    quiet: bool,
) -> Result<ExitCode> {
    if id_only && json {
        return Err(anyhow!(
            "--id and --json exclude each other; drop one of them"
        ));
    }
    let (filters, projections) = question(terms, id_only)?;
    if filters.is_empty() && missing.is_empty() && projections.is_empty() {
        return Err(anyhow!(
            "no query given; give a TERM (attribute=value), an attribute to show, or --missing ATTRIBUTE"
        ));
    }
    // A retraction never stands, so only the record can answer for it.
    let asks_retract = filters
        .iter()
        .any(|term| matches!(term, Term::Field(Field::Retract, _)))
        || projections.contains(&Projection::Field(Field::Retract));
    if asks_retract && scope != Scope::Record {
        return Err(anyhow!(
            "the retract field works only with --all; add --all"
        ));
    }
    let archive = open(root)?;
    let index = index_at(&archive, as_of, quiet)?;
    // Only bare names asked: nothing narrows, every file answers.
    let subjects = if filters.is_empty() && missing.is_empty() {
        index.subjects(scope)?
    } else {
        index
            .find(&filters, missing, scope)
            .map_err(|error| match error {
                Error::Timestamp(given) => anyhow!(
                    "{given:?} is not a valid time; time= takes an RFC 3339 time like 2026-01-01T12:00:00Z, or a date"
                ),
                other => other.into(),
            })?
    };
    let answering = Answering {
        index: &index,
        projections: &projections,
        scope,
        id_only,
        json,
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut origins = 0;
    let mut derived = 0;
    for subject in &subjects {
        let line = if along.origin {
            let mut blocks = Vec::new();
            for (above, found) in answering.descents(subject, along.derived)? {
                origins += above;
                derived += flat(&found).len() - above - 1;
                blocks.push(answering.render(&found));
            }
            blocks.join("\n")
        } else if along.derived {
            let found = answering.tree(subject)?;
            derived += flat(&found).len() - 1;
            answering.render(&found)
        } else if id_only {
            subject.as_str().to_string()
        } else {
            let shown = gather(&index, subject, &projections, scope)?;
            if json {
                output::json_line(subject.as_str(), &shown)
            } else {
                output::match_block(&shorten(&index, subject)?, &shown)
            }
        };
        if !say(&mut out, &line)? {
            return Ok(ExitCode::SUCCESS);
        }
    }
    // The matches alone stay on stdout, ready to pipe; the count is the
    // run's word on how it went.
    if !quiet {
        match (subjects.len(), scope) {
            (0, Scope::Record) => eprintln!("no matching files, retracted claims included"),
            (0, _) => eprintln!("no matching files"),
            (n, _) => {
                let mut count = format!("{n} file(s)");
                if along.origin {
                    write!(count, ", {origins} origin(s)").expect("writing to a String");
                }
                if along.derived {
                    write!(count, ", {derived} derived").expect("writing to a String");
                }
                eprintln!("{count}");
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Which way a `find` follows `prov:origin` past its matches.
#[derive(Clone, Copy)]
struct Along {
    /// Down: what was won out of each match.
    derived: bool,
    /// Up: where each match came from.
    origin: bool,
}

/// How one `find` answers for a match, held together so the walk to
/// what was derived can ask the same way at every step.
struct Answering<'a> {
    index: &'a Index,
    projections: &'a [Projection],
    scope: Scope,
    /// Names alone: nothing gathered, nothing shortened.
    id_only: bool,
    /// Full names head the answer, so no short name is looked up.
    json: bool,
}

impl Answering<'_> {
    /// What a file that came along unasked shows: its kind first, then
    /// what the question shows.
    fn kind_first(&self) -> Result<Vec<Projection>> {
        Ok(
            std::iter::once(Projection::Attribute(Attribute::parse("file:mime")?))
                .chain(self.projections.iter().cloned())
                .collect(),
        )
    }

    /// One tree the way the question asked: the names flat under --id,
    /// one JSON object under --json, the indented block otherwise.
    fn render(&self, found: &output::Found) -> String {
        if self.id_only {
            flat(found).join("\n")
        } else if self.json {
            output::json_tree(found)
        } else {
            output::match_tree(found)
        }
    }

    /// One match with everything won out of it, as far as the
    /// derivations go. A derived file shows its kind first, then what
    /// the question shows; the walk visits each file once, so a
    /// derivation that leads back up cannot run in circles.
    fn tree(&self, subject: &Subject) -> Result<output::Found> {
        let mut seen = std::collections::HashSet::new();
        seen.insert(subject.clone());
        self.found(subject, self.projections, &self.kind_first()?, &mut seen)
    }

    /// One match with where it came from: one tree per line of descent,
    /// the farthest origin at its head and the match beneath the last
    /// of them, each origin showing its kind first. With `derived`,
    /// what was won out of the match follows beneath it. Each tree
    /// comes with the number of origins above the match.
    fn descents(&self, subject: &Subject, derived: bool) -> Result<Vec<(usize, output::Found)>> {
        let kind_first = self.kind_first()?;
        let mut trees = Vec::new();
        for line in self.ancestry(subject)? {
            let mut found = if derived {
                let mut seen: std::collections::HashSet<Subject> = line.iter().cloned().collect();
                self.found(subject, self.projections, &kind_first, &mut seen)?
            } else {
                self.alone(subject, self.projections)?
            };
            for origin in line.iter().rev().skip(1) {
                let mut above = self.alone(origin, &kind_first)?;
                above.derived.push(found);
                found = above;
            }
            trees.push((line.len() - 1, found));
        }
        Ok(trees)
    }

    /// Every line of descent that ends at the subject, the farthest
    /// origin first and the subject last; a file that came from nowhere
    /// is a line of its own. A line visits each file once, so an origin
    /// that leads back down cannot run in circles.
    fn ancestry(&self, subject: &Subject) -> Result<Vec<Vec<Subject>>> {
        let mut lines = Vec::new();
        let mut path = vec![subject.clone()];
        self.climb(&mut path, &mut lines)?;
        Ok(lines)
    }

    fn climb(&self, path: &mut Vec<Subject>, lines: &mut Vec<Vec<Subject>>) -> Result<()> {
        let top = path.last().cloned().expect("a path begins at the match");
        let origins: Vec<Subject> = self
            .index
            .origins(&top, self.scope)?
            .into_iter()
            .filter(|origin| !path.contains(origin))
            .collect();
        if origins.is_empty() {
            lines.push(path.iter().rev().cloned().collect());
            return Ok(());
        }
        for origin in origins {
            path.push(origin);
            self.climb(path, lines)?;
            path.pop();
        }
        Ok(())
    }

    /// One file by itself, showing `projections`, nothing beneath it.
    fn alone(&self, subject: &Subject, projections: &[Projection]) -> Result<output::Found> {
        let mut found = output::Found {
            subject: subject.as_str().to_string(),
            ..output::Found::default()
        };
        if !self.id_only {
            found.shown = gather(self.index, subject, projections, self.scope)?;
            if !self.json {
                found.name = shorten(self.index, subject)?;
            }
        }
        Ok(found)
    }

    fn found(
        &self,
        subject: &Subject,
        projections: &[Projection],
        below: &[Projection],
        seen: &mut std::collections::HashSet<Subject>,
    ) -> Result<output::Found> {
        let mut found = self.alone(subject, projections)?;
        for derived in self.index.derived(subject, self.scope)? {
            if !seen.insert(derived.clone()) {
                continue;
            }
            found
                .derived
                .push(self.found(&derived, below, below, seen)?);
        }
        Ok(found)
    }
}

/// The tree's full names in reading order, the origin ahead of what
/// was derived from it: what `--id --with-derived` prints.
fn flat(found: &output::Found) -> Vec<String> {
    let mut names = vec![found.subject.clone()];
    for derived in &found.derived {
        names.extend(flat(derived));
    }
    names
}

fn history(root: &Path, json: bool, as_of: Option<&str>, quiet: bool) -> Result<ExitCode> {
    let archive = open(root)?;
    let index = index_at(&archive, as_of, quiet)?;
    let episodes = index.history()?;
    if episodes.is_empty() {
        // The calm zero answer: under --json it leaves stdout an empty
        // stream, the way about's do.
        let sentence = if as_of.is_some() {
            "no runs recorded by then"
        } else {
            "no runs recorded; add files with `ossuary ingest DIR`"
        };
        if json {
            if !quiet {
                eprintln!("{sentence}");
            }
        } else {
            println!("{sentence}");
        }
        return Ok(ExitCode::SUCCESS);
    }
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for episode in &episodes {
        let line = if json {
            output::episode_line(episode)
        } else {
            output::episode(episode)
        };
        if !say(&mut out, &line)? {
            break;
        }
    }
    if !quiet {
        eprintln!("{} run(s)", episodes.len());
    }
    Ok(ExitCode::SUCCESS)
}

fn attributes(
    root: &Path,
    namespaces: &[String],
    count: bool,
    json: bool,
    as_of: Option<&str>,
    quiet: bool,
) -> Result<ExitCode> {
    // Every spelling is checked before the archive opens: a mistyped
    // namespace refuses the call, not the middle of an answer.
    let mut wanted: Vec<&str> = Vec::new();
    for word in namespaces {
        let Some(namespace) = word.strip_suffix(':') else {
            return Err(anyhow!(
                "{word:?} is not a namespace; write a namespace with its colon, like exif:"
            ));
        };
        // The grammar has one door; a prefix walks through it wearing
        // a dummy name.
        Attribute::parse(&format!("{namespace}:a"))?;
        if !wanted.contains(&namespace) {
            wanted.push(namespace);
        }
    }
    let archive = open(root)?;
    let index = index_at(&archive, as_of, quiet)?;
    let mut words = index.attributes()?;
    if !wanted.is_empty() {
        words.retain(|(attribute, _)| wanted.contains(&attribute.namespace()));
    }
    if words.is_empty() {
        // The way forward differs by what was asked: an empty record
        // wants an ingest, an empty namespace wants the plain call —
        // and a moment past wants neither, the record has moved on.
        if !quiet {
            let named = wanted
                .iter()
                .map(|namespace| format!("{namespace}:"))
                .collect::<Vec<_>>()
                .join(" or ");
            match (as_of, wanted.is_empty()) {
                (Some(time), true) => eprintln!("no standing attributes as of {time}"),
                (Some(time), false) => eprintln!("no standing attributes in {named} as of {time}"),
                (None, true) => {
                    eprintln!("no standing attributes; add files with `ossuary ingest DIR`");
                }
                (None, false) => eprintln!(
                    "no standing attributes in {named}; `ossuary attributes` without a namespace lists all"
                ),
            }
        }
        return Ok(ExitCode::SUCCESS);
    }
    // The count stands first, right-aligned to the widest, the way
    // `uniq -c` speaks — so `sort -rn` ranks the answer.
    let width = words
        .iter()
        .map(|(_, files)| files.to_string().len())
        .max()
        .unwrap_or(0);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for (attribute, files) in &words {
        let line = if json {
            output::attribute_line(attribute, *files)
        } else if count {
            format!("{files:>width$}  {}", attribute.as_str())
        } else {
            attribute.as_str().to_string()
        };
        if !say(&mut out, &line)? {
            return Ok(ExitCode::SUCCESS);
        }
    }
    // The words alone stay on stdout, ready to pipe; the count is the
    // run's word on how it went.
    if !quiet {
        let namespaces: std::collections::BTreeSet<&str> = words
            .iter()
            .map(|(attribute, _)| attribute.namespace())
            .collect();
        eprintln!(
            "{} attribute(s) in {} namespace(s)",
            words.len(),
            namespaces.len()
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// What one match shows: the projected attributes and fields with
/// their values under the scope, in the question's order, each name
/// once and only when something is there to show.
fn gather(
    index: &Index,
    subject: &Subject,
    projections: &[Projection],
    scope: Scope,
) -> Result<Vec<(String, Vec<ossuary_core::Value>)>> {
    let mut shown: Vec<(String, Vec<ossuary_core::Value>)> = Vec::new();
    for projection in projections {
        match projection {
            Projection::Attribute(attribute) => {
                if shown.iter().any(|(known, _)| known == attribute.as_str()) {
                    continue;
                }
                let values = index.values(subject, attribute, scope)?;
                if !values.is_empty() {
                    shown.push((attribute.as_str().to_string(), values));
                }
            }
            Projection::Namespace(namespace) => {
                // An attribute an earlier projection already shows would
                // repeat its whole set — its namespace rows are skipped.
                let already: Vec<String> = shown.iter().map(|(known, _)| known.clone()).collect();
                for (attribute, value) in index.values_in(subject, namespace, scope)? {
                    if already.contains(&attribute.as_str().to_string()) {
                        continue;
                    }
                    match shown
                        .iter_mut()
                        .find(|(known, _)| *known == attribute.as_str())
                    {
                        Some((_, values)) => values.push(value),
                        None => shown.push((attribute.as_str().to_string(), vec![value])),
                    }
                }
            }
            Projection::Field(field) => {
                if shown.iter().any(|(known, _)| known == field.as_str()) {
                    continue;
                }
                let values = index.field_values(subject, *field, scope)?;
                if !values.is_empty() {
                    shown.push((field.as_str().to_string(), values));
                }
            }
        }
    }
    Ok(shown)
}

/// The name shortened the way git shortens a hash: the shortest prefix,
/// eight characters at least, that names only this file on the record —
/// it grows by itself as the archive does, and resolves wherever a
/// SUBJECT is taken.
pub(crate) fn shorten(index: &Index, subject: &Subject) -> Result<String> {
    let hex = subject.as_str();
    let mut length = 8;
    while length < hex.len() {
        // The prefix matches at least this subject itself; matching only
        // one name means naming it alone.
        if index.matching(&hex[..length])?.len() == 1 {
            return Ok(hex[..length].to_string());
        }
        length += 1;
    }
    Ok(hex.to_string())
}

fn id(root: &Path, path: &Path, quiet: bool) -> Result<ExitCode> {
    let archive = open(root)?;
    let algorithm = archive.content().algorithm();
    let mut file =
        std::fs::File::open(path).with_context(|| format!("{}: reading", path.display()))?;
    let mut hasher = algorithm.hasher();
    std::io::copy(&mut file, &mut hasher)
        .with_context(|| format!("{}: reading", path.display()))?;
    let digest = hasher.finish();
    println!("{digest}");
    // The name is the answer and stays alone on stdout; whether the
    // archive holds the bytes is the run talking. Held means held —
    // taken in or derived, either store counts.
    if !quiet {
        let held = !archive.content().matching(digest.as_str())?.is_empty()
            || !archive.derived().matching(digest.as_str())?.is_empty();
        if held {
            eprintln!("already in the archive; `ossuary about` shows its claims");
        } else {
            eprintln!("not in the archive; `ossuary ingest` adds it");
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
    let bare = subject;
    if !bare.bytes().all(|byte| byte.is_ascii_hexdigit()) || bare.is_empty() {
        return Err(anyhow!(
            "{bare:?} is not a hex name; give the file's hex digest as `ossuary find` prints it"
        ));
    }
    let needed = content.min_prefix().max(derived.min_prefix());
    if bare.len() < needed {
        return Err(anyhow!(
            "{bare:?} is too short; give at least {needed} characters"
        ));
    }
    let mut candidates = content.matching(bare)?;
    for extra in derived.matching(bare)? {
        if !candidates.iter().any(|held| held == &extra) {
            candidates.push(extra);
        }
    }
    let digest = match candidates.as_slice() {
        [] => return Err(anyhow!("no subject begins with {bare:?}")),
        [one] => one.clone(),
        many => {
            return Err(anyhow!(
                "{bare:?} matches {} subjects; give a longer prefix",
                many.len()
            ));
        }
    };
    let mut reader = match content.reader(&digest)? {
        Some(reader) => reader,
        None => derived
            .reader(&digest)?
            .ok_or_else(|| anyhow!("{digest}: no longer present in content/ or derived/"))?,
    };

    if let Some(path) = output {
        let mut file =
            std::fs::File::create(path).with_context(|| format!("{}: writing", path.display()))?;
        let bytes = std::io::copy(&mut reader, &mut file)
            .with_context(|| format!("{}: writing", path.display()))?;
        println!("{}: {} byte(s) of {digest}", path.display(), bytes);
    } else {
        // A beginning was given; say what it named, where the bytes
        // will not drown it out.
        if bare.len() < digest.as_str().len() && !quiet {
            eprintln!("{digest}");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn words(terms: &[&str]) -> Vec<String> {
        terms.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn the_filters_show_themselves_while_nothing_is_named() {
        let (filters, projections) = question(
            &words(&["file:mime=application/pdf", "user:tag=crete"]),
            false,
        )
        .unwrap();
        assert_eq!(filters.len(), 2);
        assert_eq!(
            projections,
            [
                Projection::Attribute(Attribute::parse("file:mime").unwrap()),
                Projection::Attribute(Attribute::parse("user:tag").unwrap()),
            ],
            "no bare attribute among the terms: the filters are the projection"
        );
    }

    #[test]
    fn a_name_without_a_colon_is_a_field_of_the_claim() {
        let (filters, projections) = question(
            &words(&["run=315e360b-020e-48be-8f2d-f2002a2ea9b4", "file:name"]),
            false,
        )
        .unwrap();
        assert_eq!(
            filters,
            [Term::Field(
                Field::Run,
                "315e360b-020e-48be-8f2d-f2002a2ea9b4".to_string()
            )]
        );
        assert_eq!(
            projections,
            [Projection::Attribute(
                Attribute::parse("file:name").unwrap()
            )],
            "the bare attribute takes the showing over, from a field term too"
        );
        let (filters, projections) = question(&words(&["source=user", "run"]), false).unwrap();
        assert_eq!(filters, [Term::Field(Field::Source, "user".to_string())]);
        assert_eq!(
            projections,
            [Projection::Field(Field::Run)],
            "a bare field name shows the field"
        );
        let (filters, projections) = question(&words(&["retract=true"]), false).unwrap();
        assert_eq!(filters.len(), 1);
        assert_eq!(
            projections,
            [],
            "a field term shows nothing of itself; the answer would echo the question"
        );
        assert!(
            question(&words(&["bogus=1"]), false).is_err(),
            "a word without a colon must be one of the seven fields"
        );
    }

    #[test]
    fn a_bare_attribute_takes_the_showing_over() {
        let (filters, projections) =
            question(&words(&["file:path=*crete*", "file:name", "exif:"]), false).unwrap();
        assert_eq!(filters.len(), 1, "the filter still narrows");
        assert_eq!(
            projections,
            [
                Projection::Attribute(Attribute::parse("file:name").unwrap()),
                Projection::Namespace("exif".to_string()),
            ],
            "explicit beats implicit: only the bare ones show, in the question's order"
        );
    }

    #[test]
    fn a_projection_named_twice_shows_once() {
        let (_, projections) = question(
            &words(&["file:name", "file:name", "file:name=*.pdf"]),
            false,
        )
        .unwrap();
        assert_eq!(
            projections,
            [Projection::Attribute(
                Attribute::parse("file:name").unwrap()
            )]
        );
    }

    /// A reader that left: every write answers with a closed pipe.
    struct Gone;
    impl std::io::Write for Gone {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_closed_pipe_ends_the_answer_instead_of_the_run() {
        let mut held = Vec::new();
        assert!(say(&mut held, "an answer").unwrap(), "written and told so");
        assert_eq!(held, b"an answer\n");
        assert!(
            !say(&mut Gone, "unwanted").unwrap(),
            "the reader gone is a calm false, not an error"
        );
    }
}
