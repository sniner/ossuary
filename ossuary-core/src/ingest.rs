//! Ingest: dumb, cheap, and never waiting to understand.
//!
//! A walk over directory trees and single files, taken at their word: every
//! regular file goes into the content store, and the day-one facts — the
//! ones any format has — go into the log. Nothing here looks *inside* a format; understanding is the
//! extractors' job, later and repeatedly, and a blob carrying nothing but
//! these claims is a queue entry, not a failure.
//!
//! Re-ingesting is harmless and useful: the same bytes dedup to the same
//! blob, and the new provenance — another path, another host, another run —
//! is recorded as the new facts they are. Facts accrete; sorting them out
//! is the fold's business, at query time. What keeps a repeated sweep from
//! drowning the log in re-run provenance is the walk's [`IngestMemory`]:
//! a file whose place, size and mtime the last run already saw is not
//! read, not hashed, and gets no claims. The memory only ever informs the
//! effort, never the truth — "I did not look again" is always allowed.
//!
//! A walk also notices what is gone. The record stands by every place a
//! file was once seen at, and a place the walk covered whole and did not
//! meet the file at is a place the file no longer lies at: the run takes
//! that sighting back — a retraction of `file:path`, under the run's own
//! source, one more claim in the log and never an erasure. The bytes stay,
//! every other claim stays, and `--as-of` before the run still shows the
//! file where it was. What the walk did not cover it does not judge: a
//! directory that would not open, a path the excludes leave out, a root
//! that is a single file, a root the walk met not one file under while
//! the record stands by places there (what a mount point looks like
//! with nothing mounted, unless the caller says the directory was
//! emptied), and a place only another host ever saw the file at — not
//! seen is not gone.

use std::fs;
use std::path::{Path, PathBuf};

use immure::Store;
use rusqlite::{Connection, params};
use serde_json::json;

use crate::accession::{Sighting, admit, known_attribute, record};
use crate::claim::{Claim, Run, Source, Subject, Timestamp, Value};
use crate::config::Excludes;
use crate::error::{Error, Result};
use crate::index::Index;
use crate::log::Log;

/// What one ingest run did.
#[derive(Debug)]
pub struct Ingested {
    /// The run's id, as every claim of it carries in its `run` field:
    /// what "arrived together" means, made exact.
    pub run: Run,
    /// Blobs this run added to the store.
    pub stored: usize,
    /// Files whose bytes the store already held. Their provenance was
    /// recorded all the same — "it also sat here" is a new fact about old
    /// content.
    pub known: usize,
    /// Claims appended to the log.
    pub claims: usize,
    /// Files the memory knew unchanged — not read, nothing recorded.
    pub unchanged: usize,
    /// Paths the excludes left out — a directory counts once, unwalked.
    pub excluded: usize,
    /// Files no longer at a place the record stood by: their sightings
    /// taken back, one retraction each.
    pub gone: usize,
    /// Directory roots the walk met no file under while the record
    /// stands by places there — a mount point with nothing mounted,
    /// most likely. Nothing under them was judged.
    pub empty: Vec<PathBuf>,
    /// Archives the walk met and left whole, each counted at its root —
    /// an archive never takes in an archive.
    pub archives: Vec<PathBuf>,
    /// What could not be taken in, and why. A walk over a million files
    /// does not forfeit the rest to one unreadable one; what failed is
    /// named here instead.
    pub failed: Vec<(PathBuf, Error)>,
}

/// What a run knows besides its roots: whose machine this is, what the
/// user said about the batch, what never goes in, what earlier runs
/// remember, and what the record stands by.
#[derive(Debug, Clone, Copy)]
pub struct Sweep<'a> {
    /// The machine the roots are on, as `prov:host` will say.
    pub host: &'a str,
    /// The user's word on the whole batch, a `user:tag` claim each.
    pub tags: &'a [String],
    /// What never goes in.
    pub excludes: &'a Excludes,
    /// The walk's memory of earlier runs — usually
    /// [`Archive::ingest_memory`](crate::Archive::ingest_memory). A file
    /// whose place, size and mtime it knows is not read, not hashed, and
    /// gets no claims. `None` observes everything anew, and so does a
    /// lost memory — the cost is a noisy run, never a wrong claim.
    pub memory: Option<&'a IngestMemory>,
    /// The record's standing places, caught up to the log — usually
    /// [`Archive::index`](crate::Archive::index) after a fold. What
    /// stands under a walked root and was not met is gone, and its
    /// sighting is taken back. `None` looks for nothing gone: the run
    /// only collects, the way a directory emptied after every run
    /// wants it.
    pub record: Option<&'a Index>,
    /// The caller's word that the roots were emptied on purpose: every
    /// place on record under them is taken back, however little the
    /// walk meets — the veto against leaving a root met empty unjudged.
    pub emptied: bool,
}

/// Take directory trees and single files — as many roots as named, in
/// one run — into the archive: blobs into `content`, day-one claims
/// into `log`. Every file's places are recorded, its bytes' size and
/// kind on their first arrival, and the caller's tags on every file the
/// run records. Then what is gone is noticed: every place the record
/// stands by under a walked root, where the walk met no file, has its
/// `file:path` sighting taken back — see [`Sweep::record`]. What
/// [`Ingested`] counts is what happened.
///
/// Excludes are the archive's, from its config: a pattern matched
/// against every path relative to its root, and a directory matched is
/// left whole, unwalked. A named root is never excluded — naming is more
/// deliberate than a pattern is.
///
/// # Errors
///
/// Whatever building the first claims can answer, the memory or the
/// record refusing to read or write, and [`Error::IngestsArchive`] when
/// a named root is an archive or lies inside one. Per-file trouble is
/// not an error here, and neither is a root that will not resolve: both
/// are collected in [`Ingested::failed`] while the walk goes on.
pub fn ingest<I>(content: &Store, log: &Log, roots: I, sweep: &Sweep<'_>) -> Result<Ingested>
where
    I: IntoIterator,
    I::Item: AsRef<Path>,
{
    let source = Source::parse("ingest")?;
    let mut result = Ingested {
        run: Run::new(),
        stored: 0,
        known: 0,
        claims: 0,
        unchanged: 0,
        excluded: 0,
        gone: 0,
        empty: Vec::new(),
        archives: Vec::new(),
        failed: Vec::new(),
    };
    // Every root is gathered before anything is read: one run, one
    // sweep, and a root that will not resolve costs only itself. The
    // failure list names the path beside each error, so the contexts
    // here say only what was being done when it went wrong.
    let gathered = gather(roots, sweep.excludes, sweep.emptied)?;
    result.excluded = gathered.excluded;
    let Judged { gone, empty } = match sweep.record {
        Some(record) => judge(record, &gathered, sweep)?,
        None => Judged::default(),
    };
    result.empty = empty;
    let remembering = sweep.memory.map(IngestMemory::begin).transpose()?;
    for path in &gathered.files {
        // What the memory compares is what the last run wrote into it:
        // the size and mtime read just before the file was, so a change
        // mid-read surfaces as a mismatch on the next sweep.
        let seen = sweep
            .memory
            .map(|memory| observe(memory, sweep.host, path))
            .transpose()?;
        if let Some(Observation::Unchanged) = seen {
            result.unchanged += 1;
            continue;
        }
        match take(
            content,
            log,
            path,
            sweep.host,
            &result.run,
            &source,
            sweep.tags,
        ) {
            Ok((new, claims)) => {
                if new {
                    result.stored += 1;
                } else {
                    result.known += 1;
                }
                result.claims += claims;
                if let (Some(memory), Some(Observation::Changed(size, mtime))) =
                    (sweep.memory, seen)
                {
                    memory.record(sweep.host, path, size, mtime)?;
                }
            }
            Err(error) => result.failed.push((path.clone(), error)),
        }
    }
    // The sightings taken back, one claim each — and the memory forgets
    // the place with them, so a file that comes back unchanged is
    // observed anew and gets its place back on the record.
    let time = Timestamp::now();
    for (path, subject) in gone {
        let claim = Claim::retract_value(
            subject,
            known_attribute("file:path"),
            Value::String(path.to_string_lossy().into_owned()),
            time.clone(),
            source.clone(),
            result.run.clone(),
        )?;
        log.append(&claim)?;
        result.claims += 1;
        result.gone += 1;
        if let Some(memory) = sweep.memory {
            memory.forget(sweep.host, &path)?;
        }
    }
    if let Some(remembering) = remembering {
        remembering.commit()?;
    }
    result.archives = gathered.archives;
    result.failed.extend(gathered.failed);
    Ok(result)
}

/// What holding the record against a walk found: the places to take
/// back, and the roots that could not be judged.
#[derive(Debug, Default)]
struct Judged {
    /// Places no file was met at, each with the file that stood there.
    gone: Vec<(PathBuf, Subject)>,
    /// Walked roots with places on record and not one file met.
    empty: Vec<PathBuf>,
}

/// Every place the record stands by under a walked root where the walk
/// met no file: what a run takes back. A place is left alone — not
/// seen is not gone — when it lies under anything the walk could not
/// read, under a path the excludes leave out, or when the record never
/// saw the file on this host at all. And a root the walk met not one
/// file under, while the record stands by places there, is not judged
/// at all: that is what a mount point looks like with nothing mounted,
/// and a directory truly emptied is told apart from it by the next run
/// that meets a file, or by the caller's word ([`Sweep::emptied`]).
fn judge(record: &Index, gathered: &Gathered, sweep: &Sweep<'_>) -> Result<Judged> {
    let met: std::collections::BTreeSet<&Path> =
        gathered.files.iter().map(PathBuf::as_path).collect();
    let host = Value::String(sweep.host.to_string());
    let mut judged = Judged::default();
    for root in &gathered.walked {
        let Some(place) = root.to_str() else {
            continue;
        };
        let standing = record.under(place)?;
        if !sweep.emptied && !standing.is_empty() && !met.iter().any(|path| path.starts_with(root))
        {
            judged.empty.push(root.clone());
            continue;
        }
        for (path, subject) in standing {
            let path = PathBuf::from(path);
            if met.contains(path.as_path()) {
                continue;
            }
            if gathered
                .failed
                .iter()
                .any(|(unread, _)| path.starts_with(unread))
            {
                continue;
            }
            let relative = path.strip_prefix(root).unwrap_or(&path);
            if relative.ancestors().any(|ancestor| {
                !ancestor.as_os_str().is_empty() && sweep.excludes.excluded(ancestor)
            }) {
                continue;
            }
            // The same absolute path on another machine is another place;
            // only a file this host ever saw can be gone from here.
            if !record
                .values(
                    &subject,
                    &known_attribute("prov:host"),
                    crate::index::Scope::Held,
                )?
                .contains(&host)
            {
                continue;
            }
            judged.gone.push((path, subject));
        }
    }
    Ok(judged)
}

/// Every named root gathered, before anything is read: the walks done,
/// the archives met set aside, the troubles collected.
struct Gathered {
    files: Vec<PathBuf>,
    excluded: usize,
    archives: Vec<PathBuf>,
    failed: Vec<(PathBuf, Error)>,
    /// Directory roots the walk went into, resolved — where what the
    /// record stands by can be held against what was met.
    walked: Vec<PathBuf>,
}

fn gather<I>(roots: I, excludes: &Excludes, emptied: bool) -> Result<Gathered>
where
    I: IntoIterator,
    I::Item: AsRef<Path>,
{
    let mut gathered = Gathered {
        files: Vec::new(),
        excluded: 0,
        archives: Vec::new(),
        failed: Vec::new(),
        walked: Vec::new(),
    };
    for given in roots {
        let root = match fs::canonicalize(given.as_ref()) {
            Ok(root) => root,
            // A directory that is no more is the plainest case of the
            // word emptied: the walk meets nothing under it, and every
            // place on record there is taken back all the same.
            Err(error) if emptied && error.kind() == std::io::ErrorKind::NotFound => {
                if let Some(root) = absent(given.as_ref()) {
                    if let Some(found) = enclosing_archive(&root) {
                        return Err(Error::IngestsArchive(found));
                    }
                    gathered.walked.push(root);
                    continue;
                }
                gathered.failed.push((
                    given.as_ref().to_path_buf(),
                    Error::Io {
                        context: "resolving".to_string(),
                        source: error,
                    },
                ));
                continue;
            }
            Err(error) => {
                gathered.failed.push((
                    given.as_ref().to_path_buf(),
                    Error::Io {
                        context: "resolving".to_string(),
                        source: error,
                    },
                ));
                continue;
            }
        };
        // Named outright is refused, not skipped: whoever points ingest
        // at an archive — or into one — is standing somewhere they did
        // not mean to be, and no half of the call should proceed on that.
        if let Some(found) = enclosing_archive(&root) {
            return Err(Error::IngestsArchive(found));
        }
        let mut walker = Walk {
            root: &root,
            excludes,
            files: Vec::new(),
            failed: Vec::new(),
            excluded: 0,
            archives: Vec::new(),
        };
        match fs::metadata(&root) {
            Ok(metadata) if metadata.is_file() => walker.files.push(root.clone()),
            Ok(metadata) if metadata.is_dir() => {
                walker.walk(&root);
                gathered.walked.push(root.clone());
            }
            // A socket, a pipe, a device: silently passed by in a walk, but a
            // run that was told to take one in must not look like it did.
            Ok(_) => walker.failed.push((
                root.clone(),
                Error::Io {
                    context: "ingesting".to_string(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "not a file or a directory",
                    ),
                },
            )),
            Err(source) => walker.failed.push((
                root.clone(),
                Error::Io {
                    context: "resolving".to_string(),
                    source,
                },
            )),
        }
        gathered.excluded += walker.excluded;
        gathered.archives.extend(walker.archives);
        gathered.failed.extend(walker.failed);
        gathered.files.extend(walker.files);
    }
    Ok(gathered)
}

/// What an ingest would do, told without doing it.
#[derive(Debug)]
pub struct Previewed {
    /// Files that would be read and taken in.
    pub files: usize,
    /// Their sizes as the filesystem reports them, summed — the number
    /// that makes a forgotten ISO visible before it is hashed.
    pub bytes: u64,
    /// Files the memory knows unchanged — the run would leave them in
    /// peace.
    pub unchanged: usize,
    /// Paths the excludes would leave out.
    pub excluded: usize,
    /// Files no longer at a place the record stands by — the sightings
    /// the run would take back.
    pub gone: Vec<PathBuf>,
    /// Directory roots the walk met no file under while the record
    /// stands by places there; nothing under them would be judged.
    pub empty: Vec<PathBuf>,
    /// Archives the walk met — left whole, run or rehearsal alike.
    pub archives: Vec<PathBuf>,
    /// What could not even be looked at, and why.
    pub failed: Vec<(PathBuf, Error)>,
}

/// What [`ingest`] would do with these roots, without reading a byte of
/// them: the same walk, the same excludes, the same memory, the same
/// look for what is gone — and the sizes the filesystem reports where
/// the real run would hash. The answer to `--dry-run`.
///
/// # Errors
///
/// [`Error::IngestsArchive`] when a named root is an archive or lies
/// inside one, and the memory or the record refusing to read. Per-file
/// trouble is collected in [`Previewed::failed`], the way the real run
/// collects it.
pub fn preview<I>(roots: I, sweep: &Sweep<'_>) -> Result<Previewed>
where
    I: IntoIterator,
    I::Item: AsRef<Path>,
{
    let gathered = gather(roots, sweep.excludes, sweep.emptied)?;
    let Judged { gone, empty } = match sweep.record {
        Some(record) => judge(record, &gathered, sweep)?,
        None => Judged::default(),
    };
    let mut result = Previewed {
        files: 0,
        bytes: 0,
        unchanged: 0,
        excluded: gathered.excluded,
        gone: gone.into_iter().map(|(path, _)| path).collect(),
        empty,
        archives: gathered.archives,
        failed: gathered.failed,
    };
    for path in gathered.files {
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(source) => {
                result.failed.push((
                    path,
                    Error::Io {
                        context: "reading metadata".to_string(),
                        source,
                    },
                ));
                continue;
            }
        };
        let seen = sweep
            .memory
            .map(|memory| observe(memory, sweep.host, &path))
            .transpose()?;
        if let Some(Observation::Unchanged) = seen {
            result.unchanged += 1;
            continue;
        }
        result.files += 1;
        result.bytes += metadata.len();
    }
    Ok(result)
}

/// What the memory has to say about a file, asked before it is read.
#[derive(Debug, Clone, Copy)]
enum Observation {
    /// Same place, same size, same mtime as when a sighting last went on
    /// the record: nothing to do.
    Unchanged,
    /// Worth reading — and these are the size and mtime to remember once
    /// it was.
    Changed(i64, i64),
    /// No mtime to compare by: observed every time, remembered never.
    Undated,
}

/// Ask the memory about one file.
fn observe(memory: &IngestMemory, host: &str, path: &Path) -> Result<Observation> {
    let Ok(metadata) = fs::metadata(path) else {
        // Whatever is wrong surfaces when the file is read, with the
        // failure list to hold it; the memory just has nothing to say.
        return Ok(Observation::Undated);
    };
    let Some(mtime) = metadata.modified().ok().and_then(unix_nanos) else {
        return Ok(Observation::Undated);
    };
    let size = i64::try_from(metadata.len()).unwrap_or(i64::MAX);
    if memory.unchanged(host, path, size, mtime)? {
        Ok(Observation::Unchanged)
    } else {
        Ok(Observation::Changed(size, mtime))
    }
}

/// One file: bytes into the store, facts into the log — and the
/// caller's tags beside them, under their own source. The walk's own
/// facts are the file's places and its mtime; everything a format has on
/// day one, the record says from what the admission saw.
fn take(
    content: &Store,
    log: &Log,
    path: &Path,
    host: &str,
    run: &Run,
    source: &Source,
    tags: &[String],
) -> Result<(bool, usize)> {
    let file = fs::File::open(path).map_err(|source| Error::Io {
        context: "reading".to_string(),
        source,
    })?;
    let modified = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(modified_value);

    let mut facts = vec![(known_attribute("file:path"), json!(path.to_string_lossy()))];
    // A canonicalized file path always ends in a name; asking spares the
    // unwrap, not a real case.
    if let Some(name) = path.file_name() {
        facts.push((known_attribute("file:name"), json!(name.to_string_lossy())));
    }
    facts.push((known_attribute("prov:host"), json!(host)));
    if let Some(modified) = modified {
        facts.push((known_attribute("file:modified"), json!(modified)));
    }
    let admitted = admit(content, file)?;
    let claims = record(
        log,
        &admitted,
        &Sighting {
            source,
            run,
            mime: None,
            facts: &facts,
            tags,
        },
    )?;
    Ok((admitted.is_new(), claims))
}

/// The moment the filesystem reported, spelled as the RFC 3339 instant it
/// is — at the precision it was observed: every fractional digit the mtime
/// carries, trailing zeros trimmed, no fraction at all on a whole second.
/// APFS speaks in nanoseconds and FAT in whole seconds, and the claim
/// repeats what was said instead of rounding it into a shape. This is what
/// lets the walk's memory, which compares nanoseconds, be rebuilt from the
/// log without losing a digit on the way.
///
/// Deliberately not a [`Timestamp`]: that type is claim time — when
/// something was *said*, whole seconds, because sub-second wallclock
/// across hosts is precision that does not exist. An mtime is an observed
/// fact about a file, and observation is repeated verbatim.
///
/// `None` when the year falls outside 0000–9999, like
/// [`Timestamp::from_unix`]: a clock that broken tells no mtime.
fn modified_value(time: std::time::SystemTime) -> Option<String> {
    let (seconds, nanos) = match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(elapsed) => (
            i64::try_from(elapsed.as_secs()).ok()?,
            elapsed.subsec_nanos(),
        ),
        Err(before) => {
            let before = before.duration();
            let seconds = i64::try_from(before.as_secs()).ok()?;
            if before.subsec_nanos() == 0 {
                (-seconds, 0)
            } else {
                // 0.3s before the epoch is 23:59:59.7 the second before.
                (-seconds - 1, 1_000_000_000 - before.subsec_nanos())
            }
        }
    };
    let whole = Timestamp::from_unix(seconds).ok()?;
    let whole = whole.as_str();
    if nanos == 0 {
        return Some(whole.to_string());
    }
    let fraction = format!("{nanos:09}");
    let fraction = fraction.trim_end_matches('0');
    Some(format!("{}.{fraction}Z", &whole[..whole.len() - 1]))
}

/// The same moment in nanoseconds — the finest comparison the filesystem
/// offers, so "unchanged" means as much as it can. `None` when it will not
/// fit, which reads as "no mtime": observed every time, never wrongly
/// skipped.
fn unix_nanos(time: std::time::SystemTime) -> Option<i64> {
    match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(elapsed) => i64::try_from(elapsed.as_nanos()).ok(),
        Err(before) => i64::try_from(before.duration().as_nanos())
            .ok()
            .and_then(i64::checked_neg),
    }
}

/// The walk's memory: which sightings past runs already put on the record.
///
/// One row per place — host and path — holding the size and mtime the file
/// had when it was last taken in. A file that still matches is left in
/// peace: not read, not hashed, no claims — which is what keeps "pour the
/// whole directory in again" from writing thousands of re-run claims when
/// ten files are new.
///
/// It lives in `cache/` and is pure economy, never truth. The log does not
/// depend on it, no claim's content comes from it, and deleting it merely
/// makes the next sweep observe — and possibly re-record — everything: the
/// behaviour every run had before it existed.
#[derive(Debug)]
pub struct IngestMemory {
    connection: Connection,
}

impl IngestMemory {
    /// Open the memory at `path`, creating file and schema as needed. The
    /// path belongs in `cache/`.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] when `SQLite` cannot open or prepare it — and like
    /// the index, a memory broken rather than merely refusing may simply
    /// be deleted.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS seen (
                 host  TEXT NOT NULL,
                 path  TEXT NOT NULL,
                 size  INTEGER NOT NULL,
                 mtime INTEGER NOT NULL,
                 PRIMARY KEY (host, path)
             );",
        )?;
        Ok(IngestMemory { connection })
    }

    /// One transaction around a whole run: thousands of sightings, one
    /// sync. A run that dies on the way rolls back whole, and its files
    /// are merely observed again next time.
    fn begin(&self) -> Result<Remembering<'_>> {
        self.connection.execute_batch("BEGIN IMMEDIATE")?;
        Ok(Remembering {
            memory: self,
            done: false,
        })
    }
}

/// The memory's transaction for one run: committed when the run gets to
/// the end, rolled back when it leaves early — so a memory used for a
/// second run is not still inside the first.
struct Remembering<'a> {
    memory: &'a IngestMemory,
    done: bool,
}

impl Remembering<'_> {
    fn commit(mut self) -> Result<()> {
        self.memory.connection.execute_batch("COMMIT")?;
        self.done = true;
        Ok(())
    }
}

impl Drop for Remembering<'_> {
    fn drop(&mut self) {
        if !self.done {
            // Nothing to answer with from a drop; a rollback that fails
            // leaves the transaction to the connection's own end.
            let _ = self.memory.connection.execute_batch("ROLLBACK");
        }
    }
}

impl IngestMemory {
    /// Whether this place was last seen with exactly this size and mtime.
    fn unchanged(&self, host: &str, path: &Path, size: i64, mtime: i64) -> Result<bool> {
        let mut statement = self.connection.prepare_cached(
            "SELECT 1 FROM seen WHERE host = ?1 AND path = ?2 AND size = ?3 AND mtime = ?4",
        )?;
        let found = statement.exists(params![host, path.to_string_lossy(), size, mtime])?;
        Ok(found)
    }

    /// Forget a place whose sighting was just taken back, so a file that
    /// returns there unchanged is observed anew rather than passed by.
    fn forget(&self, host: &str, path: &Path) -> Result<()> {
        let mut statement = self
            .connection
            .prepare_cached("DELETE FROM seen WHERE host = ?1 AND path = ?2")?;
        statement.execute(params![host, path.to_string_lossy()])?;
        Ok(())
    }

    /// Remember a sighting that just went on the record.
    fn record(&self, host: &str, path: &Path, size: i64, mtime: i64) -> Result<()> {
        let mut statement = self.connection.prepare_cached(
            "INSERT INTO seen (host, path, size, mtime) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (host, path) DO UPDATE
             SET size = excluded.size, mtime = excluded.mtime",
        )?;
        statement.execute(params![host, path.to_string_lossy(), size, mtime])?;
        Ok(())
    }
}

/// The archive root at or above a path, if any: the mark is looked for
/// at the path itself first, then upward.
/// Where a path that is no more would stand, resolved as far as it
/// exists: the nearest ancestor that does, canonical, with the rest of
/// the path beneath it — so the place it names is the one the record
/// spells, symlinked temp directories and all. `None` when not even
/// the root of the filesystem answers.
fn absent(given: &Path) -> Option<PathBuf> {
    let given = std::path::absolute(given).ok()?;
    given.ancestors().skip(1).find_map(|ancestor| {
        let base = fs::canonicalize(ancestor).ok()?;
        let rest = given.strip_prefix(ancestor).ok()?;
        Some(base.join(rest))
    })
}

fn enclosing_archive(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|dir| crate::archive::is_archive(dir))
        .map(Path::to_path_buf)
}

/// The walk in progress: every regular file under the root, sorted by name
/// at every level, minus what the excludes say never goes in.
struct Walk<'a> {
    /// Where the walk began — what the exclude patterns' paths are
    /// relative to.
    root: &'a Path,
    excludes: &'a Excludes,
    files: Vec<PathBuf>,
    failed: Vec<(PathBuf, Error)>,
    excluded: usize,
    /// Archive roots met on the walk and left whole.
    archives: Vec<PathBuf>,
}

impl Walk<'_> {
    /// Walk `dir`. Symlinks are skipped, an excluded path is counted and
    /// left alone — a directory unwalked — and a directory that will not
    /// open is recorded and passed by rather than ending the walk.
    fn walk(&mut self, dir: &Path) {
        let trouble = |source| Error::Io {
            context: "walking".to_string(),
            source,
        };
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(source) => {
                self.failed.push((dir.to_path_buf(), trouble(source)));
                return;
            }
        };
        let mut children = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => children.push(entry),
                Err(source) => self.failed.push((dir.to_path_buf(), trouble(source))),
            }
        }
        children.sort_by_key(std::fs::DirEntry::path);
        for child in children {
            let path = child.path();
            let relative = path.strip_prefix(self.root).unwrap_or(&path);
            if self.excludes.excluded(relative) {
                self.excluded += 1;
                continue;
            }
            match child.file_type() {
                Ok(kind) if kind.is_symlink() => {}
                // An archive met on the walk is left whole: an archive
                // never takes in an archive.
                Ok(kind) if kind.is_dir() && crate::archive::is_archive(&path) => {
                    self.archives.push(path);
                }
                Ok(kind) if kind.is_dir() => self.walk(&path),
                Ok(kind) if kind.is_file() => self.files.push(path),
                // Sockets, pipes, devices: not content, not an error.
                Ok(_) => {}
                Err(source) => self.failed.push((
                    path.clone(),
                    Error::Io {
                        context: "walking".to_string(),
                        source,
                    },
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use immure::Algorithm;

    use crate::accession::SNIFF;
    use crate::claim::Claim;
    use tempfile::TempDir;

    use super::*;

    fn archive(dir: &TempDir) -> (Store, Log) {
        let content = Store::builder(dir.path().join("content"))
            .suffix("")
            .depth(2)
            .create()
            .unwrap();
        let claims = Store::builder(dir.path().join("claims"))
            .suffix(".seg")
            .depth(1)
            .create()
            .unwrap();
        (content, Log::new(claims, dir.path().join("head.jsonl")))
    }

    fn hello_subject() -> String {
        Algorithm::Sha256.hash(b"hello world").to_string()
    }

    fn none() -> Excludes {
        Excludes::none()
    }

    /// A generation-1 mark, the way `Archive::create` writes one.
    const MARK_LINE: &str = "{\"ossuary-archive\":1,\"algorithm\":\"sha256\",\"content-depth\":2,\"derived-depth\":2,\"claims-depth\":1}\n";

    #[test]
    fn a_preview_measures_without_reading() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let tree = dir.path().join("tree");
        fs::create_dir_all(&tree).unwrap();
        fs::write(tree.join("a.txt"), b"hello world").unwrap();
        fs::write(tree.join("b.bin"), [0u8; 4096]).unwrap();
        let memory = IngestMemory::open(dir.path().join("memory.sqlite")).unwrap();

        let before = preview(
            [&tree],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &none(),
                memory: Some(&memory),
                record: None,
                emptied: false,
            },
        )
        .unwrap();
        assert_eq!(before.files, 2);
        assert_eq!(before.bytes, 11 + 4096);
        assert_eq!(before.unchanged, 0);

        assert!(
            log.head().unwrap().is_empty()
                && preview(
                    [&tree],
                    &Sweep {
                        host: "atlas.example.net",
                        tags: &[],
                        excludes: &none(),
                        memory: Some(&memory),
                        record: None,
                        emptied: false,
                    }
                )
                .unwrap()
                .files
                    == 2,
            "a preview leaves no trace — not in the log, not in the memory"
        );

        ingest(
            &content,
            &log,
            [&tree],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &none(),
                memory: Some(&memory),
                record: None,
                emptied: false,
            },
        )
        .unwrap();
        let after = preview(
            [&tree],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &none(),
                memory: Some(&memory),
                record: None,
                emptied: false,
            },
        )
        .unwrap();
        assert_eq!(
            (after.files, after.bytes, after.unchanged),
            (0, 0, 2),
            "what the run remembered, the preview leaves in peace"
        );
    }

    #[test]
    fn an_archive_met_on_the_walk_is_left_whole() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let tree = dir.path().join("tree");
        fs::create_dir_all(tree.join("vault")).unwrap();
        fs::write(tree.join("a.txt"), b"hello world").unwrap();
        fs::write(tree.join("vault").join("FORMAT"), MARK_LINE).unwrap();
        fs::write(tree.join("vault").join("head.jsonl"), b"claims").unwrap();

        let result = ingest(
            &content,
            &log,
            [&tree],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &none(),
                memory: None,
                record: None,
                emptied: false,
            },
        )
        .unwrap();

        assert_eq!(result.stored, 1, "only a.txt went in");
        assert_eq!(
            result.archives,
            [fs::canonicalize(tree.join("vault")).unwrap()],
            "the archive is named at its root, once, and nothing under it was read"
        );
        assert!(result.failed.is_empty(), "left whole is not a failure");
    }

    #[test]
    fn an_archive_named_outright_refuses_the_call() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let vault = dir.path().join("vault");
        fs::create_dir_all(&vault).unwrap();
        fs::write(vault.join("FORMAT"), MARK_LINE).unwrap();
        fs::write(vault.join("notes.txt"), b"inside").unwrap();

        let named_whole = ingest(
            &content,
            &log,
            [&vault],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &none(),
                memory: None,
                record: None,
                emptied: false,
            },
        );
        assert!(matches!(named_whole, Err(Error::IngestsArchive(_))));

        let named_inside = ingest(
            &content,
            &log,
            [vault.join("notes.txt")],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &none(),
                memory: None,
                record: None,
                emptied: false,
            },
        );
        match named_inside {
            Err(Error::IngestsArchive(found)) => assert_eq!(
                found,
                fs::canonicalize(&vault).unwrap(),
                "pointing inside an archive names the archive"
            ),
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(
            log.head().unwrap().is_empty(),
            "a refused call wrote nothing"
        );
    }

    #[test]
    fn a_tree_goes_in_with_its_six_facts_each() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let tree = dir.path().join("tree");
        fs::create_dir_all(tree.join("sub")).unwrap();
        fs::write(tree.join("a.txt"), b"hello world").unwrap();
        fs::write(
            tree.join("sub").join("b.jpg"),
            [0xFF, 0xD8, 0xFF, 0xE0, 0x00],
        )
        .unwrap();

        let result = ingest(
            &content,
            &log,
            [&tree],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &none(),
                memory: None,
                record: None,
                emptied: false,
            },
        )
        .unwrap();

        assert_eq!(result.stored, 2);
        assert_eq!(result.known, 0);
        assert_eq!(result.claims, 12, "six facts per file, mtime included");
        assert!(result.failed.is_empty());

        let head = log.head().unwrap();
        assert_eq!(head.len(), 12);
        let about_a: Vec<_> = head
            .iter()
            .filter(|claim| claim.subject().as_str() == hello_subject())
            .collect();
        assert_eq!(about_a.len(), 6);
        let value = |attribute: &str| {
            about_a
                .iter()
                .find(|claim| claim.attribute().as_str() == attribute)
                .and_then(|claim| claim.value())
                .cloned()
        };
        assert_eq!(value("file:mime"), Some(json!("text/plain")));
        assert_eq!(value("file:size"), Some(json!(11)));
        assert_eq!(value("file:name"), Some(json!("a.txt")));
        assert_eq!(value("prov:host"), Some(json!("atlas.example.net")));
        assert!(
            head.iter().all(|claim| claim.run() == Some(&result.run)),
            "every claim of the call carries its run"
        );
        let path = value("file:path").unwrap();
        assert!(path.as_str().unwrap().ends_with("/tree/a.txt"));

        let jpeg = head
            .iter()
            .find(|claim| {
                claim.attribute().as_str() == "file:mime"
                    && claim.subject().as_str() != hello_subject()
            })
            .unwrap();
        assert_eq!(
            jpeg.value(),
            Some(&json!("image/jpeg")),
            "magic bytes, not the file name"
        );
    }

    #[test]
    fn tags_ride_on_what_the_run_records_and_only_that() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let tree = dir.path().join("tree");
        fs::create_dir_all(&tree).unwrap();
        fs::write(tree.join("photo.jpg"), b"hello world").unwrap();
        let memory = IngestMemory::open(dir.path().join("ingest.sqlite")).unwrap();
        let tags = ["holiday".to_string(), "beach".to_string()];

        ingest(
            &content,
            &log,
            [&tree],
            &Sweep {
                host: "atlas.example.net",
                tags: &tags,
                excludes: &none(),
                memory: Some(&memory),
                record: None,
                emptied: false,
            },
        )
        .unwrap();

        let tagged: Vec<Claim> = log
            .head()
            .unwrap()
            .into_iter()
            .filter(|claim| claim.attribute().as_str() == "user:tag")
            .collect();
        assert_eq!(tagged.len(), 2, "one claim per tag");
        assert!(
            tagged.iter().all(|claim| claim.source().as_str() == "user"),
            "the human asserts, the walk is only the pen"
        );

        // The same tree again: the memory skips it whole, tags included.
        let again = ingest(
            &content,
            &log,
            [&tree],
            &Sweep {
                host: "atlas.example.net",
                tags: &["latergreat".to_string()],
                excludes: &none(),
                memory: Some(&memory),
                record: None,
                emptied: false,
            },
        )
        .unwrap();
        assert_eq!(again.unchanged, 1);
        assert_eq!(
            again.claims, 0,
            "a skipped file gets no claims, tags among them"
        );

        // Known bytes at a new place: the sighting is recorded, and the
        // tag rides on it.
        fs::write(tree.join("copy.jpg"), b"hello world").unwrap();
        ingest(
            &content,
            &log,
            [&tree],
            &Sweep {
                host: "atlas.example.net",
                tags: &["copies".to_string()],
                excludes: &none(),
                memory: Some(&memory),
                record: None,
                emptied: false,
            },
        )
        .unwrap();
        let copies = log
            .head()
            .unwrap()
            .iter()
            .filter(|claim| {
                claim.attribute().as_str() == "user:tag" && claim.value() == Some(&json!("copies"))
            })
            .count();
        assert_eq!(copies, 1, "a sighting of known bytes carries the tag");
    }

    #[test]
    fn known_bytes_still_get_their_provenance() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let tree = dir.path().join("tree");
        fs::create_dir_all(&tree).unwrap();
        fs::write(tree.join("first.txt"), b"hello world").unwrap();
        fs::write(tree.join("second.txt"), b"hello world").unwrap();

        let result = ingest(
            &content,
            &log,
            [&tree],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &none(),
                memory: None,
                record: None,
                emptied: false,
            },
        )
        .unwrap();

        assert_eq!(result.stored, 1, "one content");
        assert_eq!(result.known, 1, "met again under the second name");
        assert_eq!(
            result.claims, 10,
            "both places it sat are on the record; size and kind only once"
        );
        let described = log
            .head()
            .unwrap()
            .iter()
            .filter(|claim| matches!(claim.attribute().as_str(), "file:size" | "file:mime"))
            .count();
        assert_eq!(described, 2, "the content is described exactly once");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_content() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let tree = dir.path().join("tree");
        fs::create_dir_all(&tree).unwrap();
        fs::write(tree.join("real.txt"), b"content").unwrap();
        std::os::unix::fs::symlink(tree.join("real.txt"), tree.join("alias.txt")).unwrap();

        let result = ingest(
            &content,
            &log,
            [&tree],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &none(),
                memory: None,
                record: None,
                emptied: false,
            },
        )
        .unwrap();

        assert_eq!(result.stored, 1, "the file, not its alias");
        assert!(result.failed.is_empty());
    }

    #[test]
    fn the_recorded_path_is_the_real_one() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let tree = dir.path().join("tree");
        fs::create_dir_all(tree.join("sub")).unwrap();
        fs::write(tree.join("a.txt"), b"hello world").unwrap();

        let result = ingest(
            &content,
            &log,
            [tree.join("sub").join("..")],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &none(),
                memory: None,
                record: None,
                emptied: false,
            },
        )
        .unwrap();

        assert_eq!(result.stored, 1);
        let path = log
            .head()
            .unwrap()
            .iter()
            .find(|claim| claim.attribute().as_str() == "file:path")
            .and_then(|claim| claim.value())
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap();
        assert!(
            !path.contains(".."),
            "claims are forever, and {path:?} is not the place itself"
        );
        assert!(path.ends_with("/tree/a.txt"));
    }

    #[test]
    fn what_the_excludes_name_stays_out_and_is_counted() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let tree = dir.path().join("tree");
        fs::create_dir_all(tree.join("sub")).unwrap();
        fs::write(tree.join("a.txt"), b"content").unwrap();
        fs::write(tree.join(".DS_Store"), b"junk").unwrap();
        fs::write(tree.join("sub").join(".DS_Store"), b"junk below").unwrap();
        let excludes = Excludes::compile([".DS_Store"]).unwrap();

        let result = ingest(
            &content,
            &log,
            [&tree],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &excludes,
                memory: None,
                record: None,
                emptied: false,
            },
        )
        .unwrap();

        assert_eq!(result.stored, 1, "the content, not the junk");
        assert_eq!(result.excluded, 2, "at every level, and on the record");
        assert!(result.failed.is_empty());
    }

    #[test]
    fn an_excluded_directory_is_not_walked() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let tree = dir.path().join("tree");
        fs::create_dir_all(tree.join("node_modules").join("deep")).unwrap();
        fs::write(tree.join("a.txt"), b"content").unwrap();
        fs::write(
            tree.join("node_modules").join("deep").join("b.txt"),
            b"dependency",
        )
        .unwrap();
        let excludes = Excludes::compile(["node_modules"]).unwrap();

        let result = ingest(
            &content,
            &log,
            [&tree],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &excludes,
                memory: None,
                record: None,
                emptied: false,
            },
        )
        .unwrap();

        assert_eq!(result.stored, 1);
        assert_eq!(
            result.excluded, 1,
            "the directory counts once; what is under it was never seen"
        );
    }

    #[test]
    fn a_path_pattern_is_relative_to_the_ingested_tree() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let tree = dir.path().join("tree");
        fs::create_dir_all(tree.join("build")).unwrap();
        fs::create_dir_all(tree.join("src").join("build")).unwrap();
        fs::write(tree.join("build").join("out"), b"artifact").unwrap();
        fs::write(tree.join("src").join("build").join("keep"), b"source").unwrap();
        let excludes = Excludes::compile(["build/**"]).unwrap();

        let result = ingest(
            &content,
            &log,
            [&tree],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &excludes,
                memory: None,
                record: None,
                emptied: false,
            },
        )
        .unwrap();

        assert_eq!(
            result.stored, 1,
            "build/ at the top is out, src/build/ is not it"
        );
        assert_eq!(result.excluded, 1);
    }

    #[test]
    fn a_single_file_goes_in_with_its_facts() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let file = dir.path().join("solo.txt");
        fs::write(&file, b"hello world").unwrap();

        let result = ingest(
            &content,
            &log,
            [&file],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &none(),
                memory: None,
                record: None,
                emptied: false,
            },
        )
        .unwrap();

        assert_eq!(result.stored, 1, "a file is not a tree, and goes in");
        assert_eq!(result.claims, 6);
        assert!(result.failed.is_empty());
        let path = log
            .head()
            .unwrap()
            .iter()
            .find(|claim| claim.attribute().as_str() == "file:path")
            .and_then(|claim| claim.value())
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap();
        assert!(path.ends_with("/solo.txt"));
    }

    #[test]
    fn a_modified_value_keeps_the_observed_precision() {
        let at = |seconds, nanos| std::time::UNIX_EPOCH + std::time::Duration::new(seconds, nanos);
        assert_eq!(
            modified_value(at(1_700_000_000, 0)).unwrap(),
            "2023-11-14T22:13:20Z",
            "a whole second carries no fraction"
        );
        assert_eq!(
            modified_value(at(1_700_000_000, 500_000_000)).unwrap(),
            "2023-11-14T22:13:20.5Z",
            "trailing zeros are trimmed, not padded"
        );
        assert_eq!(
            modified_value(at(1_700_000_000, 123_456_789)).unwrap(),
            "2023-11-14T22:13:20.123456789Z",
            "every observed digit survives"
        );
    }

    #[test]
    fn a_moment_before_the_epoch_still_tells_its_fraction() {
        let time = std::time::UNIX_EPOCH - std::time::Duration::new(0, 300_000_000);
        assert_eq!(modified_value(time).unwrap(), "1969-12-31T23:59:59.7Z");
    }

    #[test]
    fn a_clock_beyond_the_four_digits_tells_no_mtime() {
        // 10000-01-01T00:00:00Z: the year the format's four digits run out.
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(253_402_300_800);
        assert_eq!(modified_value(time), None);
    }

    #[test]
    fn the_recorded_mtime_is_the_one_observed() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let file = dir.path().join("dated.txt");
        fs::write(&file, b"dated content").unwrap();
        let mtime = std::time::UNIX_EPOCH + std::time::Duration::new(1_700_000_000, 123_456_000);
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(mtime)
            .unwrap();

        ingest(
            &content,
            &log,
            [&file],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &none(),
                memory: None,
                record: None,
                emptied: false,
            },
        )
        .unwrap();

        let value = log
            .head()
            .unwrap()
            .iter()
            .find(|claim| claim.attribute().as_str() == "file:modified")
            .and_then(|claim| claim.value())
            .cloned()
            .unwrap();
        assert_eq!(
            value,
            json!("2023-11-14T22:13:20.123456Z"),
            "the claim repeats the filesystem verbatim"
        );
    }

    #[test]
    fn a_file_named_outright_beats_the_excludes() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let file = dir.path().join(".DS_Store");
        fs::write(&file, b"junk, but asked for").unwrap();
        let excludes = Excludes::compile([".DS_Store"]).unwrap();

        let result = ingest(
            &content,
            &log,
            [&file],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &excludes,
                memory: None,
                record: None,
                emptied: false,
            },
        )
        .unwrap();

        assert_eq!(
            result.stored, 1,
            "the excludes speak about trees; naming a file is more deliberate"
        );
        assert_eq!(result.excluded, 0);
    }

    #[test]
    fn a_second_sweep_leaves_unchanged_files_in_peace() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let tree = dir.path().join("tree");
        fs::create_dir_all(&tree).unwrap();
        fs::write(tree.join("a.txt"), b"hello world").unwrap();
        fs::write(tree.join("b.txt"), b"more content").unwrap();
        let memory = IngestMemory::open(dir.path().join("ingest.sqlite")).unwrap();

        let first = ingest(
            &content,
            &log,
            [&tree],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &none(),
                memory: Some(&memory),
                record: None,
                emptied: false,
            },
        )
        .unwrap();
        assert_eq!(first.stored, 2);
        assert_eq!(first.unchanged, 0);

        // The memory outlives its handle, like the file it is.
        drop(memory);
        let memory = IngestMemory::open(dir.path().join("ingest.sqlite")).unwrap();
        let second = ingest(
            &content,
            &log,
            [&tree],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &none(),
                memory: Some(&memory),
                record: None,
                emptied: false,
            },
        )
        .unwrap();

        assert_eq!(second.unchanged, 2, "nothing changed, nothing observed");
        assert_eq!(second.stored + second.known, 0);
        assert_eq!(second.claims, 0, "a quiet sweep writes nothing at all");
    }

    #[test]
    fn a_changed_file_is_observed_again() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let tree = dir.path().join("tree");
        fs::create_dir_all(&tree).unwrap();
        fs::write(tree.join("a.txt"), b"hello world").unwrap();
        fs::write(tree.join("b.txt"), b"more content").unwrap();
        let memory = IngestMemory::open(dir.path().join("ingest.sqlite")).unwrap();

        ingest(
            &content,
            &log,
            [&tree],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &none(),
                memory: Some(&memory),
                record: None,
                emptied: false,
            },
        )
        .unwrap();
        // A different size settles "changed" whatever the clock says.
        fs::write(tree.join("a.txt"), b"hello world, grown").unwrap();

        let second = ingest(
            &content,
            &log,
            [&tree],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &none(),
                memory: Some(&memory),
                record: None,
                emptied: false,
            },
        )
        .unwrap();

        assert_eq!(second.stored, 1, "the new bytes go in");
        assert_eq!(second.unchanged, 1, "the untouched neighbour does not");
        assert_eq!(second.claims, 6, "and only the change is on the record");
    }

    #[test]
    fn another_host_is_another_sighting() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let tree = dir.path().join("tree");
        fs::create_dir_all(&tree).unwrap();
        fs::write(tree.join("a.txt"), b"hello world").unwrap();
        let memory = IngestMemory::open(dir.path().join("ingest.sqlite")).unwrap();

        ingest(
            &content,
            &log,
            [&tree],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &none(),
                memory: Some(&memory),
                record: None,
                emptied: false,
            },
        )
        .unwrap();
        let second = ingest(
            &content,
            &log,
            [&tree],
            &Sweep {
                host: "rhea.example.net",
                tags: &[],
                excludes: &none(),
                memory: Some(&memory),
                record: None,
                emptied: false,
            },
        )
        .unwrap();

        assert_eq!(second.unchanged, 0, "what atlas saw, rhea has not");
        assert_eq!(second.known, 1, "and rhea's sighting goes on the record");
    }

    #[test]
    fn several_roots_arrive_in_one_run() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let tree = dir.path().join("tree");
        fs::create_dir_all(&tree).unwrap();
        fs::write(tree.join("a.txt"), b"first").unwrap();
        let single = dir.path().join("single.txt");
        fs::write(&single, b"second").unwrap();
        let gone = dir.path().join("no-such-place");

        let result = ingest(
            &content,
            &log,
            [&tree, &single, &gone],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &none(),
                memory: None,
                record: None,
                emptied: false,
            },
        )
        .unwrap();

        assert_eq!(result.stored, 2, "both healthy roots went in");
        assert_eq!(
            result.failed.len(),
            1,
            "the root that is not there is named, and costs only itself"
        );
        let runs: std::collections::HashSet<Run> = log
            .head()
            .unwrap()
            .iter()
            .filter_map(|claim| claim.run().cloned())
            .collect();
        assert_eq!(
            runs,
            std::collections::HashSet::from([result.run.clone()]),
            "everything of one call arrived in one run"
        );
    }

    #[test]
    fn a_root_that_is_not_there_is_a_named_failure_not_a_crash() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);

        let result = ingest(
            &content,
            &log,
            [dir.path().join("no-such-tree")],
            &Sweep {
                host: "atlas",
                tags: &[],
                excludes: &none(),
                memory: None,
                record: None,
                emptied: false,
            },
        )
        .unwrap();

        assert_eq!(result.stored + result.known, 0);
        assert_eq!(result.failed.len(), 1);
    }

    fn recorded_mime(dir: &TempDir, bytes: &[u8]) -> serde_json::Value {
        let (content, log) = archive(dir);
        let file = dir.path().join("specimen");
        fs::write(&file, bytes).unwrap();
        ingest(
            &content,
            &log,
            [&file],
            &Sweep {
                host: "atlas.example.net",
                tags: &[],
                excludes: &none(),
                memory: None,
                record: None,
                emptied: false,
            },
        )
        .unwrap();
        log.head()
            .unwrap()
            .iter()
            .find(|claim| claim.attribute().as_str() == "file:mime")
            .and_then(|claim| claim.value())
            .cloned()
            .unwrap()
    }

    #[test]
    fn the_text_fallback_judges_the_whole_file_not_the_sniff_head() {
        // Text for longer than the sniff head sees, then one raw byte:
        // only a whole-stream look can refuse the text/plain fallback.
        let dir = TempDir::new().unwrap();
        let mut bytes = vec![b'a'; SNIFF + 1024];
        bytes.push(0xFF);
        assert_eq!(
            recorded_mime(&dir, &bytes),
            json!("application/octet-stream")
        );
    }

    #[test]
    fn a_text_larger_than_the_sniff_head_is_still_text() {
        // A multi-byte character straddling the head's edge: the truncated
        // head is no longer valid UTF-8, the whole stream is.
        let dir = TempDir::new().unwrap();
        let mut bytes = vec![b'a'; SNIFF - 1];
        bytes.extend_from_slice("ä und noch viel mehr Text".as_bytes());
        assert_eq!(recorded_mime(&dir, &bytes), json!("text/plain"));
    }

    /// The record as it stands: an index folded from the log, thrown
    /// away with the test.
    fn record_of(log: &Log) -> Index {
        let mut index = Index::open(":memory:").unwrap();
        index.fold(log).unwrap();
        index
    }

    fn places_under(log: &Log, root: &Path) -> Vec<String> {
        record_of(log)
            .under(root.to_str().unwrap())
            .unwrap()
            .into_iter()
            .map(|(path, _)| path)
            .collect()
    }

    fn sweep<'a>(
        host: &'a str,
        excludes: &'a Excludes,
        memory: Option<&'a IngestMemory>,
        record: Option<&'a Index>,
    ) -> Sweep<'a> {
        Sweep {
            host,
            tags: &[],
            excludes,
            memory,
            record,
            emptied: false,
        }
    }

    #[test]
    fn a_file_gone_from_a_walked_tree_has_its_place_taken_back() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        // The walk resolves its roots; the test asks by the resolved name.
        let tree = dir.path().join("tree");
        fs::create_dir_all(&tree).unwrap();
        let tree = tree.canonicalize().unwrap();
        fs::write(tree.join("a.txt"), b"hello world").unwrap();
        fs::write(tree.join("b.txt"), b"more content").unwrap();
        let memory = IngestMemory::open(dir.path().join("ingest.sqlite")).unwrap();
        let host = "atlas.example.net";
        let excludes = none();
        ingest(
            &content,
            &log,
            [&tree],
            &sweep(host, &excludes, Some(&memory), None),
        )
        .unwrap();
        let a = tree.join("a.txt");
        let metadata = fs::metadata(&a).unwrap();
        let size = i64::try_from(metadata.len()).unwrap();
        let mtime = unix_nanos(metadata.modified().unwrap()).unwrap();
        assert!(memory.unchanged(host, &a, size, mtime).unwrap());
        assert_eq!(places_under(&log, &tree).len(), 2);

        fs::remove_file(&a).unwrap();
        let record = record_of(&log);
        let second = ingest(
            &content,
            &log,
            [&tree],
            &sweep(host, &excludes, Some(&memory), Some(&record)),
        )
        .unwrap();

        assert_eq!(second.gone, 1);
        assert_eq!(second.claims, 1, "one retraction, nothing else");
        assert_eq!(second.unchanged, 1);
        assert_eq!(
            places_under(&log, &tree),
            vec![tree.join("b.txt").to_string_lossy().into_owned()],
            "the place is taken back; the bytes and the rest stay"
        );
        assert!(
            content
                .contains(&Algorithm::Sha256.hash(b"hello world"))
                .unwrap()
        );
        assert!(
            !memory.unchanged(host, &a, size, mtime).unwrap(),
            "the memory forgets the place with the sighting"
        );

        // Back where it was: observed anew, its place on the record again.
        fs::write(&a, b"hello world").unwrap();
        let record = record_of(&log);
        let third = ingest(
            &content,
            &log,
            [&tree],
            &sweep(host, &excludes, Some(&memory), Some(&record)),
        )
        .unwrap();
        assert_eq!(third.gone, 0);
        assert_eq!(third.known, 1);
        assert_eq!(places_under(&log, &tree).len(), 2);
    }

    #[test]
    fn a_place_under_an_unreadable_directory_is_not_gone() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let tree = dir.path().join("tree");
        fs::create_dir_all(tree.join("sub")).unwrap();
        let tree = tree.canonicalize().unwrap();
        let sub = tree.join("sub");
        fs::write(sub.join("a.txt"), b"hello world").unwrap();
        let host = "atlas.example.net";
        let excludes = none();
        ingest(&content, &log, [&tree], &sweep(host, &excludes, None, None)).unwrap();

        fs::set_permissions(&sub, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_dir(&sub).is_ok() {
            // Root reads everything; the case cannot be played here.
            fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        let record = record_of(&log);
        let again = ingest(
            &content,
            &log,
            [&tree],
            &sweep(host, &excludes, None, Some(&record)),
        );
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();
        let again = again.unwrap();

        assert_eq!(again.failed.len(), 1, "the directory that would not open");
        assert_eq!(again.gone, 0, "not seen is not gone");
        assert_eq!(places_under(&log, &tree).len(), 1);
    }

    #[test]
    fn a_place_the_excludes_leave_out_is_not_gone() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let tree = dir.path().join("tree");
        fs::create_dir_all(tree.join("skip")).unwrap();
        let tree = tree.canonicalize().unwrap();
        fs::write(tree.join("skip").join("a.txt"), b"hello world").unwrap();
        let host = "atlas.example.net";
        ingest(&content, &log, [&tree], &sweep(host, &none(), None, None)).unwrap();

        let excludes = Excludes::compile(["skip"]).unwrap();
        let record = record_of(&log);
        let again = ingest(
            &content,
            &log,
            [&tree],
            &sweep(host, &excludes, None, Some(&record)),
        )
        .unwrap();

        assert_eq!(again.excluded, 1);
        assert_eq!(again.gone, 0, "left out is not looked at");
        assert_eq!(places_under(&log, &tree).len(), 1);
    }

    #[test]
    fn a_place_on_another_host_is_not_gone() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        // The walk resolves its roots; the test asks by the resolved name.
        let tree = dir.path().join("tree");
        fs::create_dir_all(&tree).unwrap();
        let tree = tree.canonicalize().unwrap();
        fs::write(tree.join("a.txt"), b"hello world").unwrap();
        let excludes = none();
        ingest(
            &content,
            &log,
            [&tree],
            &sweep("atlas.example.net", &excludes, None, None),
        )
        .unwrap();

        fs::remove_file(tree.join("a.txt")).unwrap();
        let record = record_of(&log);
        let elsewhere = ingest(
            &content,
            &log,
            [&tree],
            &sweep("borea.example.net", &excludes, None, Some(&record)),
        )
        .unwrap();

        assert_eq!(
            elsewhere.gone, 0,
            "the same path on another machine is another place"
        );
        assert_eq!(places_under(&log, &tree).len(), 1);
    }

    #[test]
    fn a_preview_names_what_would_be_gone_and_takes_nothing_back() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        // The walk resolves its roots; the test asks by the resolved name.
        let tree = dir.path().join("tree");
        fs::create_dir_all(&tree).unwrap();
        let tree = tree.canonicalize().unwrap();
        fs::write(tree.join("a.txt"), b"hello world").unwrap();
        fs::write(tree.join("b.txt"), b"more content").unwrap();
        let host = "atlas.example.net";
        let excludes = none();
        ingest(&content, &log, [&tree], &sweep(host, &excludes, None, None)).unwrap();
        fs::remove_file(tree.join("a.txt")).unwrap();
        let record = record_of(&log);

        let rehearsal = preview([&tree], &sweep(host, &excludes, None, Some(&record))).unwrap();

        assert_eq!(rehearsal.gone, vec![tree.join("a.txt")]);
        assert!(rehearsal.empty.is_empty());
        assert_eq!(places_under(&log, &tree).len(), 2, "nothing written");
    }

    #[test]
    fn a_root_met_empty_judges_nothing() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        // The walk resolves its roots; the test asks by the resolved name.
        let tree = dir.path().join("tree");
        fs::create_dir_all(&tree).unwrap();
        let tree = tree.canonicalize().unwrap();
        fs::write(tree.join("a.txt"), b"hello world").unwrap();
        fs::write(tree.join("b.txt"), b"more content").unwrap();
        let host = "atlas.example.net";
        let excludes = none();
        ingest(&content, &log, [&tree], &sweep(host, &excludes, None, None)).unwrap();

        // What a mount point looks like with nothing mounted.
        fs::remove_file(tree.join("a.txt")).unwrap();
        fs::remove_file(tree.join("b.txt")).unwrap();
        let record = record_of(&log);
        let rehearsal = preview([&tree], &sweep(host, &excludes, None, Some(&record))).unwrap();
        assert!(rehearsal.gone.is_empty());
        assert_eq!(rehearsal.empty, vec![tree.clone()]);
        let again = ingest(
            &content,
            &log,
            [&tree],
            &sweep(host, &excludes, None, Some(&record)),
        )
        .unwrap();

        assert_eq!(again.gone, 0, "not one file met: nothing judged");
        assert_eq!(again.empty, vec![tree.clone()]);
        assert_eq!(places_under(&log, &tree).len(), 2);

        // One file back: the walk met something, and judges the rest.
        fs::write(tree.join("b.txt"), b"more content").unwrap();
        let record = record_of(&log);
        let later = ingest(
            &content,
            &log,
            [&tree],
            &sweep(host, &excludes, None, Some(&record)),
        )
        .unwrap();
        assert_eq!(later.gone, 1);
        assert!(later.empty.is_empty());
        assert_eq!(places_under(&log, &tree).len(), 1);
    }

    #[test]
    fn the_word_emptied_takes_back_every_place_under_a_root_met_empty() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        // The walk resolves its roots; the test asks by the resolved name.
        let tree = dir.path().join("tree");
        fs::create_dir_all(&tree).unwrap();
        let tree = tree.canonicalize().unwrap();
        fs::write(tree.join("a.txt"), b"hello world").unwrap();
        fs::write(tree.join("b.txt"), b"more content").unwrap();
        let host = "atlas.example.net";
        let excludes = none();
        ingest(&content, &log, [&tree], &sweep(host, &excludes, None, None)).unwrap();
        fs::remove_file(tree.join("a.txt")).unwrap();
        fs::remove_file(tree.join("b.txt")).unwrap();
        let record = record_of(&log);
        let mut emptied = sweep(host, &excludes, None, Some(&record));
        emptied.emptied = true;

        let rehearsal = preview([&tree], &emptied).unwrap();
        assert_eq!(rehearsal.gone.len(), 2);
        assert!(rehearsal.empty.is_empty());

        let run = ingest(&content, &log, [&tree], &emptied).unwrap();

        assert_eq!(run.gone, 2);
        assert!(run.empty.is_empty());
        assert!(places_under(&log, &tree).is_empty());
    }

    #[test]
    fn the_word_emptied_takes_back_every_place_under_a_root_that_is_gone() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let given = dir.path().join("inbox");
        fs::create_dir_all(&given).unwrap();
        // The record spells the resolved place; the call names the
        // directory as the shell would, symlinked temp directory and all.
        let tree = given.canonicalize().unwrap();
        fs::write(tree.join("a.txt"), b"hello world").unwrap();
        let host = "atlas.example.net";
        let excludes = none();
        ingest(
            &content,
            &log,
            [&given],
            &sweep(host, &excludes, None, None),
        )
        .unwrap();
        fs::remove_dir_all(&given).unwrap();
        let record = record_of(&log);

        let mut plain = sweep(host, &excludes, None, Some(&record));
        let run = ingest(&content, &log, [&given], &plain).unwrap();
        assert_eq!(
            run.gone, 0,
            "without the word, a directory that is no more is a failure"
        );
        assert_eq!(run.failed.len(), 1);
        assert_eq!(places_under(&log, &tree).len(), 1);

        plain.emptied = true;
        let rehearsal = preview([&given], &plain).unwrap();
        assert_eq!(rehearsal.gone.len(), 1);
        assert!(rehearsal.failed.is_empty());
        let run = ingest(&content, &log, [&given], &plain).unwrap();
        assert_eq!(run.gone, 1);
        assert!(run.failed.is_empty(), "{:?}", run.failed);
        assert!(places_under(&log, &tree).is_empty());
    }
}
