//! Claims held in memory and written to the log in bundles.
//!
//! A writer that stores files and records claims about them (ingest, an
//! extractor run, a fetch) hands each item's claims to a [`Recorder`]. The
//! recorder writes them to the log as one bundle per batch, flushed, and
//! the log gets one bundle per batch instead of one per file. A [`Batch`]
//! says when a batch ends.
//!
//! An archive does not flush the files in its content and derived stores
//! (see [`Archive`](crate::Archive)): they have their names at once, and
//! only the claims are flushed. A crash of the system can then leave a
//! file that is empty or cut short under a claim that is on the device.
//! An audit finds it, and the same bytes stored again repair it. A
//! process that is killed loses only the claims it held, and the next run
//! does that batch again.
//!
//! An archive that flushes its files ([`Archive::flush_files`](crate::Archive::flush_files))
//! holds them without a name until the recorder ends the batch: the
//! stores flush, then the claims are written, so a claim is never on the
//! device before the file it is about. A flush that fails is reported by
//! [`Recorder::write`] and nowhere else. After such a failure the recorder
//! writes nothing more: the files of its claims may be missing, and a
//! caller that goes on to save a resume point gets the error again
//! instead.

use std::time::{Duration, Instant};

use immure::Store;

use crate::claim::Claim;
use crate::error::{Error, Result};
use crate::log::Log;

/// When a [`Recorder`] writes what it holds: at the first of the limits
/// reached. Larger batches cost fewer flushes of the device, and a run
/// that is interrupted repeats more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Batch {
    /// Bytes of the claims held, as the lines the log stores.
    pub claims: u64,
    /// Items held.
    pub items: usize,
    /// Bytes the items held added to the stores: a batch of large files
    /// does not wait unflushed for its last item.
    pub bytes: u64,
    /// Time since the first item of the batch was held: a run that gets
    /// its items slowly, from a mail server, still writes regularly.
    /// Checked when an item is held, not in between.
    pub age: Option<Duration>,
}

impl Batch {
    /// What an archive uses unless told otherwise. No file waits for the
    /// end of the batch, so only the claims and the time count.
    pub const DEFAULT: Batch = Batch {
        claims: 4 << 20,
        items: usize::MAX,
        bytes: u64::MAX,
        age: Some(Duration::from_secs(30)),
    };

    /// For an archive that flushes its files (see
    /// [`Archive::flush_files`](crate::Archive::flush_files)): the files
    /// of a batch have no names until it ends.
    pub const FLUSH: Batch = Batch {
        claims: u64::MAX,
        items: 1000,
        bytes: 256 << 20,
        age: Some(Duration::from_secs(30)),
    };

    fn is_full(&self, claims: u64, items: usize, bytes: u64, since: Option<Instant>) -> bool {
        claims >= self.claims
            || items >= self.items
            || bytes >= self.bytes
            || matches!((self.age, since), (Some(age), Some(since)) if since.elapsed() >= age)
    }
}

/// Claims of items not written yet, and where they go.
#[derive(Debug)]
pub struct Recorder<'a> {
    log: &'a Log,
    /// The stores the files named by the claims lie in.
    stores: Vec<&'a Store>,
    batch: Batch,
    claims: Vec<Claim>,
    /// The bytes of the claims held, as lines.
    lines: u64,
    items: usize,
    /// The bytes the items held stored.
    stored: u64,
    /// When the first of the items held was handed over.
    since: Option<Instant>,
    /// A write failed. The files of the claims held then may be missing.
    failed: bool,
}

impl<'a> Recorder<'a> {
    /// A recorder writing to `log`, about files in `stores`, in batches
    /// of `batch`. [`Archive::recorder`](crate::Archive::recorder) makes
    /// the one that fits an archive.
    pub(crate) fn new(log: &'a Log, stores: &[&'a Store], batch: Batch) -> Self {
        Recorder {
            log,
            stores: stores.to_vec(),
            batch,
            claims: Vec::new(),
            lines: 0,
            items: 0,
            stored: 0,
            since: None,
            failed: false,
        }
    }

    /// Hold one item's claims: a file's, an examination's, a message's.
    /// `stored` is the number of bytes the item added to the stores, 0 when
    /// it added none. Once the batch is full (see [`Batch`]), it is
    /// written. Answers whether that happened.
    ///
    /// # Errors
    ///
    /// As [`write`](Recorder::write).
    pub fn push(&mut self, claims: Vec<Claim>, stored: u64) -> Result<bool> {
        if self.failed {
            return Err(Error::NotRecorded);
        }
        for claim in claims {
            // Serialised here once; the log writes the same line.
            let claim = claim.with_line();
            self.lines = self.lines.saturating_add(claim.line().len() as u64 + 1);
            self.claims.push(claim);
        }
        self.items += 1;
        self.stored = self.stored.saturating_add(stored);
        self.since.get_or_insert_with(Instant::now);
        if !self
            .batch
            .is_full(self.lines, self.items, self.stored, self.since)
        {
            return Ok(false);
        }
        self.write()?;
        Ok(true)
    }

    /// Write the claims held: the stores flush the files written so far,
    /// where the archive flushes its files, then the claims go into the
    /// log as one bundle, flushed. Nothing to do when nothing is held.
    ///
    /// # Errors
    ///
    /// [`Error::Store`](crate::Error::Store) when a store cannot flush, and
    /// whatever appending to the log can answer. The claims held are not
    /// written then, and not later either: every call after a failure
    /// answers [`Error::NotRecorded`].
    pub fn write(&mut self) -> Result<()> {
        if self.failed {
            return Err(Error::NotRecorded);
        }
        if self.items == 0 {
            return Ok(());
        }
        let written = self.flush_and_append();
        if written.is_err() {
            self.failed = true;
            return written;
        }
        self.claims.clear();
        self.lines = 0;
        self.items = 0;
        self.stored = 0;
        self.since = None;
        Ok(())
    }

    fn flush_and_append(&self) -> Result<()> {
        for store in &self.stores {
            store.flush()?;
        }
        self.log.append_all(&self.claims)
    }

    /// How many items are held and not written yet.
    #[must_use]
    pub fn held(&self) -> usize {
        self.items
    }
}

#[cfg(test)]
mod tests {
    use immure::Algorithm;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::archive::Archive;
    use crate::claim::{Attribute, Run, Source, Subject, Timestamp};

    fn archive(dir: &TempDir) -> Archive {
        Archive::create(dir.path().join("archive"), Algorithm::Sha256).unwrap()
    }

    /// An archive that flushes its files, as under `--flush`.
    fn flushing(dir: &TempDir) -> Archive {
        archive(dir).flush_files()
    }

    /// Bytes into the content store and the one claim about them.
    fn item(archive: &Archive, bytes: &[u8]) -> (std::path::PathBuf, Vec<Claim>) {
        let (_, entry) = archive.content().add(bytes).unwrap();
        let claim = Claim::assert(
            Subject::parse(entry.digest().as_str()).unwrap(),
            Attribute::parse("file:size").unwrap(),
            json!(bytes.len()),
            Timestamp::now(),
            Source::parse("test").unwrap(),
            Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        )
        .unwrap();
        (entry.path().to_path_buf(), vec![claim])
    }

    #[test]
    fn claims_wait_for_a_full_batch() {
        let dir = TempDir::new().unwrap();
        let archive = flushing(&dir);
        let mut recorder = archive.recorder();

        let full = Batch::FLUSH.items;
        for n in 1..full {
            let (_, claims) = item(&archive, format!("file {n}").as_bytes());
            assert!(!recorder.push(claims, 0).unwrap());
        }
        assert!(
            archive.log().head().unwrap().is_empty(),
            "held, not written"
        );
        assert_eq!(recorder.held(), full - 1);

        let (path, claims) = item(&archive, b"the last of the batch");
        assert!(recorder.push(claims, 0).unwrap(), "written with the batch");

        assert_eq!(recorder.held(), 0);
        assert_eq!(archive.log().head().unwrap().len(), full);
        assert!(path.is_file(), "the files are in place");
    }

    #[test]
    fn the_files_are_in_place_before_their_claims_are_written() {
        let dir = TempDir::new().unwrap();
        let archive = flushing(&dir);
        let mut recorder = archive.recorder();
        let (path, claims) = item(&archive, b"named with its batch");
        assert!(!path.exists());

        recorder.push(claims, 0).unwrap();
        recorder.write().unwrap();

        assert!(path.is_file());
        assert_eq!(archive.log().head().unwrap().len(), 1);
        recorder.write().unwrap();
        assert_eq!(archive.log().head().unwrap().len(), 1, "nothing twice");
    }

    #[test]
    fn large_items_are_written_before_the_batch_is_full() {
        let dir = TempDir::new().unwrap();
        let archive = flushing(&dir);
        let mut recorder = archive.recorder();

        let (_, claims) = item(&archive, b"a large file");
        assert!(!recorder.push(claims, Batch::FLUSH.bytes - 1).unwrap());
        let (path, claims) = item(&archive, b"one byte more");
        assert!(recorder.push(claims, 1).unwrap());

        assert!(path.is_file());
        assert_eq!(archive.log().head().unwrap().len(), 2);
    }

    #[test]
    fn the_stores_of_a_flushing_archive_wait_for_the_recorder() {
        let dir = TempDir::new().unwrap();
        let archive = flushing(&dir);
        let mut paths = Vec::new();
        for n in 0..=immure::DEFAULT_BATCH.get() {
            let (path, _) = item(&archive, format!("file {n}").as_bytes());
            paths.push(path);
        }
        assert!(
            paths.iter().all(|path| !path.exists()),
            "no batch of the store ends by itself"
        );
        archive.flush().unwrap();
        assert!(paths.iter().all(|path| path.is_file()));
    }

    /// A flush that fails must not be followed by a write of the claims:
    /// with immure 0.4.0 the files of the failed batch were gone, the next
    /// flush had nothing to do, and 99 claims named files the archive did
    /// not have. Found in the review of 0.25.0.
    #[cfg(unix)]
    #[test]
    fn nothing_is_written_after_a_flush_that_failed() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = TempDir::new().unwrap();
        let archive = flushing(&dir);
        let mut recorder = archive.recorder();
        let (path, claims) = item(&archive, b"its shard refuses the rename");
        recorder.push(claims, 0).unwrap();
        let shard = path.parent().unwrap();
        std::fs::set_permissions(shard, std::fs::Permissions::from_mode(0o555)).unwrap();
        if std::fs::File::create(shard.join("probe")).is_ok() {
            // Root writes where the mode forbids it; nothing to test then.
            return;
        }

        assert!(matches!(recorder.write(), Err(Error::Store(_))));
        std::fs::set_permissions(shard, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(matches!(recorder.write(), Err(Error::NotRecorded)));
        let (_, claims) = item(&archive, b"after the failure");
        assert!(matches!(recorder.push(claims, 0), Err(Error::NotRecorded)));
        assert!(archive.log().head().unwrap().is_empty());
    }

    #[test]
    fn a_batch_that_is_old_enough_is_written_with_the_next_item() {
        let dir = TempDir::new().unwrap();
        let archive = flushing(&dir);
        let mut recorder = archive.recorder();
        recorder.batch.age = Some(Duration::from_millis(20));

        let (_, claims) = item(&archive, b"the first of a slow run");
        assert!(!recorder.push(claims, 0).unwrap());
        std::thread::sleep(Duration::from_millis(30));
        let (path, claims) = item(&archive, b"the second, much later");
        assert!(recorder.push(claims, 0).unwrap());

        assert!(path.is_file());
        assert_eq!(archive.log().head().unwrap().len(), 2);
    }

    #[test]
    fn a_file_has_its_name_before_its_claims_are_written() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let mut recorder = archive.recorder();

        let (path, claims) = item(&archive, b"named at once");
        assert!(path.is_file());
        assert!(!recorder.push(claims, 13).unwrap());

        assert!(archive.log().head().unwrap().is_empty(), "held");
        recorder.write().unwrap();
        assert_eq!(archive.log().head().unwrap().len(), 1);
    }

    #[test]
    fn claims_are_written_once_their_lines_reach_the_limit() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let mut recorder = archive.recorder();
        let (_, claims) = item(&archive, b"one claim");
        let line = claims[0].to_line().len() as u64 + 1;
        recorder.batch.claims = 3 * line;

        // The number of items and the bytes they stored do not count.
        assert!(!recorder.push(claims.clone(), 1 << 40).unwrap());
        assert!(!recorder.push(claims.clone(), 1 << 40).unwrap());
        assert!(recorder.push(claims.clone(), 0).unwrap());

        assert_eq!(recorder.held(), 0);
        assert!(
            !recorder.push(claims, 0).unwrap(),
            "counted from zero again"
        );
    }
}
