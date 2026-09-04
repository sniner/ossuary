//! Ingest: dumb, cheap, and never waiting to understand.
//!
//! A walk over a directory tree — or one file, taken at its word: every
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

use std::fs;
use std::path::{Path, PathBuf};

use immure::{Status, Store};
use rusqlite::{Connection, params};
use serde_json::json;
use uuid::Uuid;

use crate::claim::{Attribute, Claim, Source, Subject, Timestamp};
use crate::config::Excludes;
use crate::error::{Error, Result};
use crate::log::Log;

/// What one ingest run did.
#[derive(Debug)]
pub struct Ingested {
    /// The run's id, as it stands in every `prov:run` claim: what
    /// "arrived together" means, made exact.
    pub run: String,
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
    /// What could not be taken in, and why. A walk over a million files
    /// does not forfeit the rest to one unreadable one; what failed is
    /// named here instead.
    pub failed: Vec<(PathBuf, Error)>,
}

/// Take a directory tree — or a single file — into the archive: blobs
/// into `content`, day-one claims into `log`.
///
/// A blob new to the store gets the seven day-one claims, available for
/// any format on its first day: `file:path` (the real place: absolute,
/// `..` and symlinks resolved — claims are forever, so they name the
/// place, not the way it was typed), `file:name` (the path's last
/// element, so a name is askable without string surgery), `prov:host`,
/// `prov:run`, `file:size`, `file:mime` — by magic bytes, with a
/// UTF-8 look for plain text and `application/octet-stream` when nothing
/// answers — and `file:modified` where the filesystem has an mtime to
/// tell — repeated at the precision it was observed, fractional seconds
/// and all.
///
/// Bytes the store already holds get their sighting only: place, name,
/// host, run, mtime. Another place they sat is new knowledge; their size
/// and kind describe the content, the log has them from the first
/// sighting, and saying a deterministic thing twice adds nothing. Symlinks are not followed,
/// hidden files are files, and the walk is sorted at every level, so the
/// same tree ingests in the same order twice.
///
/// `host` is who this machine says it is — an FQDN where there is one; the
/// caller knows, this crate does not ask around.
///
/// `excludes` is the archive's word on what never goes in — usually
/// [`Config::excludes`](crate::Config::excludes). What they match is
/// counted in [`Ingested::excluded`] and otherwise left in peace: an
/// excluded directory is not even walked. They speak about trees, though:
/// a file named outright as `root` goes in regardless — naming it is more
/// deliberate than a pattern is.
///
/// `memory` is the walk's memory of earlier runs — usually
/// [`Archive::ingest_memory`](crate::Archive::ingest_memory). A file whose
/// place, size and mtime it knows is counted in [`Ingested::unchanged`]
/// and otherwise skipped whole: not read, not hashed, no claims. `None`
/// observes everything anew, and so does a lost memory — the cost is a
/// noisy run, never a wrong claim.
///
/// # Errors
///
/// Whatever building the first claims can answer, and the memory refusing
/// to read or write. Per-file trouble is not an error here, and neither is
/// a root that will not resolve: both are collected in
/// [`Ingested::failed`] while the walk goes on.
pub fn ingest(
    content: &Store,
    log: &Log,
    root: impl AsRef<Path>,
    host: &str,
    excludes: &Excludes,
    memory: Option<&IngestMemory>,
) -> Result<Ingested> {
    let source = Source::parse("ingest")?;
    let mut result = Ingested {
        run: Uuid::new_v4().to_string(),
        stored: 0,
        known: 0,
        claims: 0,
        unchanged: 0,
        excluded: 0,
        failed: Vec::new(),
    };
    // The failure list names the path beside each error, so the contexts
    // here say only what was being done when it went wrong.
    let root = match fs::canonicalize(root.as_ref()) {
        Ok(root) => root,
        Err(error) => {
            result.failed.push((
                root.as_ref().to_path_buf(),
                Error::Io {
                    context: "resolving".to_string(),
                    source: error,
                },
            ));
            return Ok(result);
        }
    };

    let mut walker = Walk {
        root: &root,
        excludes,
        files: Vec::new(),
        failed: Vec::new(),
        excluded: 0,
    };
    match fs::metadata(&root) {
        Ok(metadata) if metadata.is_file() => walker.files.push(root.clone()),
        Ok(metadata) if metadata.is_dir() => walker.walk(&root),
        // A socket, a pipe, a device: silently passed by in a walk, but a
        // run that was told to take one in must not look like it did.
        Ok(_) => walker.failed.push((
            root.clone(),
            Error::Io {
                context: "taking in".to_string(),
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
    result.excluded = walker.excluded;
    result.failed.extend(walker.failed);
    if let Some(memory) = memory {
        memory.begin()?;
    }
    for path in walker.files {
        // What the memory compares is what the last run wrote into it:
        // the size and mtime read just before the file was, so a change
        // mid-read surfaces as a mismatch on the next sweep.
        let seen = memory
            .map(|memory| observe(memory, host, &path))
            .transpose()?;
        if let Some(Observation::Unchanged) = seen {
            result.unchanged += 1;
            continue;
        }
        match take(content, log, &path, host, &result.run, &source) {
            Ok((status, claims)) => {
                if status.is_new() {
                    result.stored += 1;
                } else {
                    result.known += 1;
                }
                result.claims += claims;
                if let (Some(memory), Some(Observation::Changed(size, mtime))) = (memory, seen) {
                    memory.record(host, &path, size, mtime)?;
                }
            }
            Err(error) => result.failed.push((path, error)),
        }
    }
    if let Some(memory) = memory {
        memory.commit()?;
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

/// One file: bytes into the store, facts into the log.
fn take(
    content: &Store,
    log: &Log,
    path: &Path,
    host: &str,
    run: &str,
    source: &Source,
) -> Result<(Status, usize)> {
    let bytes = fs::read(path).map_err(|source| Error::Io {
        context: "reading".to_string(),
        source,
    })?;
    let modified = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(modified_value);

    let (status, entry) = content.add(&bytes)?;
    let subject = Subject::parse(&format!(
        "{}:{}",
        content.algorithm().name(),
        entry.digest()
    ))?;
    let time = Timestamp::now();

    let mut claims = vec![fact(
        &subject,
        "file:path",
        json!(path.to_string_lossy()),
        &time,
        source,
    )?];
    // A canonicalized file path always ends in a name; asking spares the
    // unwrap, not a real case.
    if let Some(name) = path.file_name() {
        claims.push(fact(
            &subject,
            "file:name",
            json!(name.to_string_lossy()),
            &time,
            source,
        )?);
    }
    claims.push(fact(&subject, "prov:host", json!(host), &time, source)?);
    claims.push(fact(&subject, "prov:run", json!(run), &time, source)?);
    // Size and kind describe the content, not the sighting: the log has
    // them from the blob's first day, and they never say anything new.
    if status.is_new() {
        claims.push(fact(
            &subject,
            "file:size",
            json!(bytes.len()),
            &time,
            source,
        )?);
        claims.push(fact(
            &subject,
            "file:mime",
            json!(mime(&bytes)),
            &time,
            source,
        )?);
    }
    if let Some(modified) = modified {
        claims.push(fact(
            &subject,
            "file:modified",
            json!(modified),
            &time,
            source,
        )?);
    }
    for claim in &claims {
        log.append(claim)?;
    }
    Ok((status, claims.len()))
}

/// One day-one fact, spelled out.
fn fact(
    subject: &Subject,
    attribute: &'static str,
    value: serde_json::Value,
    time: &Timestamp,
    source: &Source,
) -> Result<Claim> {
    let attribute = Attribute::parse(attribute).expect("a known attribute");
    Claim::assert(
        subject.clone(),
        attribute,
        value,
        time.clone(),
        source.clone(),
    )
}

/// What the bytes say they are: magic bytes first, a UTF-8 look for plain
/// text second, and the honest shrug when nothing answers.
fn mime(bytes: &[u8]) -> String {
    infer::get(bytes)
        .map(|kind| kind.mime_type().to_string())
        .or_else(|| {
            (!bytes.is_empty() && std::str::from_utf8(bytes).is_ok())
                .then(|| "text/plain".to_string())
        })
        .unwrap_or_else(|| "application/octet-stream".to_string())
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
    fn begin(&self) -> Result<()> {
        self.connection.execute_batch("BEGIN IMMEDIATE")?;
        Ok(())
    }

    fn commit(&self) -> Result<()> {
        self.connection.execute_batch("COMMIT")?;
        Ok(())
    }

    /// Whether this place was last seen with exactly this size and mtime.
    fn unchanged(&self, host: &str, path: &Path, size: i64, mtime: i64) -> Result<bool> {
        let mut statement = self.connection.prepare_cached(
            "SELECT 1 FROM seen WHERE host = ?1 AND path = ?2 AND size = ?3 AND mtime = ?4",
        )?;
        let found = statement.exists(params![host, path.to_string_lossy(), size, mtime])?;
        Ok(found)
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
        format!("sha256:{}", Algorithm::Sha256.hash(b"hello world"))
    }

    fn none() -> Excludes {
        Excludes::none()
    }

    #[test]
    fn a_tree_goes_in_with_its_seven_facts_each() {
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

        let result = ingest(&content, &log, &tree, "atlas.example.net", &none(), None).unwrap();

        assert_eq!(result.stored, 2);
        assert_eq!(result.known, 0);
        assert_eq!(result.claims, 14, "seven facts per file, mtime included");
        assert!(result.failed.is_empty());

        let head = log.head().unwrap();
        assert_eq!(head.len(), 14);
        let about_a: Vec<_> = head
            .iter()
            .filter(|claim| claim.subject().as_str() == hello_subject())
            .collect();
        assert_eq!(about_a.len(), 7);
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
        assert_eq!(value("prov:run"), Some(json!(result.run)));
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
    fn known_bytes_still_get_their_provenance() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let tree = dir.path().join("tree");
        fs::create_dir_all(&tree).unwrap();
        fs::write(tree.join("first.txt"), b"hello world").unwrap();
        fs::write(tree.join("second.txt"), b"hello world").unwrap();

        let result = ingest(&content, &log, &tree, "atlas.example.net", &none(), None).unwrap();

        assert_eq!(result.stored, 1, "one content");
        assert_eq!(result.known, 1, "met again under the second name");
        assert_eq!(
            result.claims, 12,
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

        let result = ingest(&content, &log, &tree, "atlas.example.net", &none(), None).unwrap();

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
            tree.join("sub").join(".."),
            "atlas.example.net",
            &none(),
            None,
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

        let result = ingest(&content, &log, &tree, "atlas.example.net", &excludes, None).unwrap();

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

        let result = ingest(&content, &log, &tree, "atlas.example.net", &excludes, None).unwrap();

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

        let result = ingest(&content, &log, &tree, "atlas.example.net", &excludes, None).unwrap();

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

        let result = ingest(&content, &log, &file, "atlas.example.net", &none(), None).unwrap();

        assert_eq!(result.stored, 1, "a file is not a tree, and goes in");
        assert_eq!(result.claims, 7);
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

        ingest(&content, &log, &file, "atlas.example.net", &none(), None).unwrap();

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

        let result = ingest(&content, &log, &file, "atlas.example.net", &excludes, None).unwrap();

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
            &tree,
            "atlas.example.net",
            &none(),
            Some(&memory),
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
            &tree,
            "atlas.example.net",
            &none(),
            Some(&memory),
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
            &tree,
            "atlas.example.net",
            &none(),
            Some(&memory),
        )
        .unwrap();
        // A different size settles "changed" whatever the clock says.
        fs::write(tree.join("a.txt"), b"hello world, grown").unwrap();

        let second = ingest(
            &content,
            &log,
            &tree,
            "atlas.example.net",
            &none(),
            Some(&memory),
        )
        .unwrap();

        assert_eq!(second.stored, 1, "the new bytes go in");
        assert_eq!(second.unchanged, 1, "the untouched neighbour does not");
        assert_eq!(second.claims, 7, "and only the change is on the record");
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
            &tree,
            "atlas.example.net",
            &none(),
            Some(&memory),
        )
        .unwrap();
        let second = ingest(
            &content,
            &log,
            &tree,
            "rhea.example.net",
            &none(),
            Some(&memory),
        )
        .unwrap();

        assert_eq!(second.unchanged, 0, "what atlas saw, rhea has not");
        assert_eq!(second.known, 1, "and rhea's sighting goes on the record");
    }

    #[test]
    fn a_root_that_is_not_there_is_a_named_failure_not_a_crash() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);

        let result = ingest(
            &content,
            &log,
            dir.path().join("no-such-tree"),
            "atlas",
            &none(),
            None,
        )
        .unwrap();

        assert_eq!(result.stored + result.known, 0);
        assert_eq!(result.failed.len(), 1);
    }
}
