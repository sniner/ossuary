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
//! is the fold's business, at query time.

use std::fs;
use std::path::{Path, PathBuf};

use immure::{Status, Store};
use serde_json::json;
use uuid::Uuid;

use crate::claim::{Attribute, Claim, Source, Subject, Timestamp};
use crate::config::Excludes;
use crate::error::{Error, Result};
use crate::log::Log;

/// What one ingest run did.
#[derive(Debug)]
pub struct Ingested {
    /// The run's id, as it stands in every `prov:ingest-run` claim: what
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
/// Six facts per file, available for any format on its first day:
/// `prov:ingest-path` (the real path: absolute, `..` and symlinks resolved —
/// claims are forever, so they name the place, not the way it was typed),
/// `prov:host`, `prov:ingest-run`,
/// `file:size`, `file:mime` — by magic bytes, with a UTF-8 look for plain
/// text and `application/octet-stream` when nothing answers — and
/// `file:modified` where the filesystem has an mtime to tell. Symlinks are
/// not followed, hidden files are files, and the walk is sorted at every
/// level, so the same tree ingests in the same order twice.
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
/// # Errors
///
/// Whatever building the first claims can answer. Per-file trouble is not
/// an error here, and neither is a root that will not resolve: both are
/// collected in [`Ingested::failed`] while the walk goes on.
pub fn ingest(
    content: &Store,
    log: &Log,
    root: impl AsRef<Path>,
    host: &str,
    excludes: &Excludes,
) -> Result<Ingested> {
    let source = Source::parse("ingest")?;
    let mut result = Ingested {
        run: Uuid::new_v4().to_string(),
        stored: 0,
        known: 0,
        claims: 0,
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
    for path in walker.files {
        match take(content, log, &path, host, &result.run, &source) {
            Ok((status, claims)) => {
                if status.is_new() {
                    result.stored += 1;
                } else {
                    result.known += 1;
                }
                result.claims += claims;
            }
            Err(error) => result.failed.push((path, error)),
        }
    }
    Ok(result)
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
        .map(unix_seconds)
        .and_then(|seconds| Timestamp::from_unix(seconds).ok());
    let mime = mime(&bytes);

    let (status, entry) = content.add(&bytes)?;
    let subject = Subject::parse(&format!(
        "{}:{}",
        content.algorithm().name(),
        entry.digest()
    ))?;
    let time = Timestamp::now();

    let mut claims = vec![
        fact(
            &subject,
            "prov:ingest-path",
            json!(path.to_string_lossy()),
            &time,
            source,
        )?,
        fact(&subject, "prov:host", json!(host), &time, source)?,
        fact(&subject, "prov:ingest-run", json!(run), &time, source)?,
        fact(&subject, "file:size", json!(bytes.len()), &time, source)?,
        fact(&subject, "file:mime", json!(mime), &time, source)?,
    ];
    if let Some(modified) = modified {
        claims.push(fact(
            &subject,
            "file:modified",
            json!(modified.as_str()),
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

/// Unix time of a moment the filesystem reported, negative before 1970.
fn unix_seconds(time: std::time::SystemTime) -> i64 {
    match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(elapsed) => i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_secs()).unwrap_or(i64::MAX),
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

        let result = ingest(&content, &log, &tree, "atlas.example.net", &none()).unwrap();

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
        assert_eq!(value("prov:host"), Some(json!("atlas.example.net")));
        assert_eq!(value("prov:ingest-run"), Some(json!(result.run)));
        let path = value("prov:ingest-path").unwrap();
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

        let result = ingest(&content, &log, &tree, "atlas.example.net", &none()).unwrap();

        assert_eq!(result.stored, 1, "one content");
        assert_eq!(result.known, 1, "met again under the second name");
        assert_eq!(
            result.claims, 12,
            "and both places it sat are on the record"
        );
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

        let result = ingest(&content, &log, &tree, "atlas.example.net", &none()).unwrap();

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
        )
        .unwrap();

        assert_eq!(result.stored, 1);
        let path = log
            .head()
            .unwrap()
            .iter()
            .find(|claim| claim.attribute().as_str() == "prov:ingest-path")
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

        let result = ingest(&content, &log, &tree, "atlas.example.net", &excludes).unwrap();

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

        let result = ingest(&content, &log, &tree, "atlas.example.net", &excludes).unwrap();

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

        let result = ingest(&content, &log, &tree, "atlas.example.net", &excludes).unwrap();

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

        let result = ingest(&content, &log, &file, "atlas.example.net", &none()).unwrap();

        assert_eq!(result.stored, 1, "a file is not a tree, and goes in");
        assert_eq!(result.claims, 6);
        assert!(result.failed.is_empty());
        let path = log
            .head()
            .unwrap()
            .iter()
            .find(|claim| claim.attribute().as_str() == "prov:ingest-path")
            .and_then(|claim| claim.value())
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap();
        assert!(path.ends_with("/solo.txt"));
    }

    #[test]
    fn a_file_named_outright_beats_the_excludes() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let file = dir.path().join(".DS_Store");
        fs::write(&file, b"junk, but asked for").unwrap();
        let excludes = Excludes::compile([".DS_Store"]).unwrap();

        let result = ingest(&content, &log, &file, "atlas.example.net", &excludes).unwrap();

        assert_eq!(
            result.stored, 1,
            "the excludes speak about trees; naming a file is more deliberate"
        );
        assert_eq!(result.excluded, 0);
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
        )
        .unwrap();

        assert_eq!(result.stored + result.known, 0);
        assert_eq!(result.failed.len(), 1);
    }
}
