//! Ingest: dumb, cheap, and never waiting to understand.
//!
//! A walk over a directory tree: every regular file goes into the content
//! store, and the day-one facts — the ones any format has — go into the
//! log. Nothing here looks *inside* a format; understanding is the
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
    /// What could not be taken in, and why. A walk over a million files
    /// does not forfeit the rest to one unreadable one; what failed is
    /// named here instead.
    pub failed: Vec<(PathBuf, Error)>,
}

/// Take a directory tree into the archive: blobs into `content`, day-one
/// claims into `log`.
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
/// # Errors
///
/// Whatever building the first claims can answer. Per-file trouble is not
/// an error here, and neither is a root that will not resolve: both are
/// collected in [`Ingested::failed`] while the walk goes on.
pub fn ingest(content: &Store, log: &Log, root: impl AsRef<Path>, host: &str) -> Result<Ingested> {
    let source = Source::parse("ingest")?;
    let mut result = Ingested {
        run: Uuid::new_v4().to_string(),
        stored: 0,
        known: 0,
        claims: 0,
        failed: Vec::new(),
    };
    let root = match fs::canonicalize(root.as_ref()) {
        Ok(root) => root,
        Err(error) => {
            result.failed.push((
                root.as_ref().to_path_buf(),
                Error::Io {
                    context: format!("{}: resolving", root.as_ref().display()),
                    source: error,
                },
            ));
            return Ok(result);
        }
    };

    let mut files = Vec::new();
    walk(&root, &mut files, &mut result.failed);
    for path in files {
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
        context: format!("{}: reading", path.display()),
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

/// Every regular file under `dir`, sorted by name at every level. Symlinks
/// are skipped, and a directory that will not open is recorded and passed
/// by rather than ending the walk.
fn walk(dir: &Path, files: &mut Vec<PathBuf>, failed: &mut Vec<(PathBuf, Error)>) {
    let trouble = |source| Error::Io {
        context: format!("{}: walking", dir.display()),
        source,
    };
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(source) => {
            failed.push((dir.to_path_buf(), trouble(source)));
            return;
        }
    };
    let mut children = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => children.push(entry),
            Err(source) => failed.push((dir.to_path_buf(), trouble(source))),
        }
    }
    children.sort_by_key(std::fs::DirEntry::path);
    for child in children {
        let path = child.path();
        match child.file_type() {
            Ok(kind) if kind.is_symlink() => {}
            Ok(kind) if kind.is_dir() => walk(&path, files, failed),
            Ok(kind) if kind.is_file() => files.push(path),
            // Sockets, pipes, devices: not content, not an error.
            Ok(_) => {}
            Err(source) => failed.push((
                path.clone(),
                Error::Io {
                    context: format!("{}: walking", path.display()),
                    source,
                },
            )),
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

        let result = ingest(&content, &log, &tree, "atlas.example.net").unwrap();

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

        let result = ingest(&content, &log, &tree, "atlas.example.net").unwrap();

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

        let result = ingest(&content, &log, &tree, "atlas.example.net").unwrap();

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
    fn a_root_that_is_not_there_is_a_named_failure_not_a_crash() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);

        let result = ingest(&content, &log, dir.path().join("no-such-tree"), "atlas").unwrap();

        assert_eq!(result.stored + result.known, 0);
        assert_eq!(result.failed.len(), 1);
    }
}
