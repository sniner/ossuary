//! Claims held back until the files they name are flushed.
//!
//! A writer that stores files and records claims about them (ingest, an
//! extractor run, a fetch) hands each item's claims to a [`Recorder`]. The
//! recorder writes them to the log as one bundle per batch, after the
//! stores have flushed the files those claims name: a claim is never on
//! the device before the file it is about, and the log gets one bundle per
//! batch instead of one per file. A [`Batch`] says when a batch ends: after
//! a number of items, after a number of stored bytes, or after a time,
//! whichever comes first.
//!
//! The stores of an archive do not flush by themselves (see
//! [`Archive`](crate::Archive)), so the recorder decides when a batch ends,
//! and a flush that fails is reported by [`Recorder::write`] and nowhere
//! else. After such a failure the recorder writes nothing more: the files
//! of its claims may be missing, and a caller that goes on to save a resume
//! point gets the error again instead.
//!
//! A crash of the system, or a process that is killed, loses the batch
//! that was not written: its files have no names yet and its claims were
//! never in the log. The next run does that batch again.

use std::time::{Duration, Instant};

use immure::Store;

use crate::archive::Archive;
use crate::claim::Claim;
use crate::error::{Error, Result};
use crate::log::Log;

/// When a [`Recorder`] writes what it holds: at the first of the limits
/// reached. Larger batches cost fewer flushes of the device, and a run
/// that is interrupted repeats more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Batch {
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
    /// What an archive uses unless told otherwise.
    pub const DEFAULT: Batch = Batch {
        items: 1000,
        bytes: 256 << 20,
        age: Some(Duration::from_secs(30)),
    };

    /// For a run that writes very many files (see
    /// [`Archive::lazy_flush`](crate::Archive::lazy_flush)).
    pub const LAZY: Batch = Batch {
        items: 10_000,
        bytes: 1 << 30,
        age: None,
    };

    fn is_full(&self, items: usize, bytes: u64, since: Option<Instant>) -> bool {
        items >= self.items
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
    /// as the log says ([`Log::batch`]).
    #[must_use]
    pub fn new(log: &'a Log, stores: &[&'a Store]) -> Self {
        Recorder {
            log,
            stores: stores.to_vec(),
            batch: log.batch(),
            claims: Vec::new(),
            items: 0,
            stored: 0,
            since: None,
            failed: false,
        }
    }

    /// A recorder writing to the archive's log, about files in its content
    /// and derived stores.
    #[must_use]
    pub fn for_archive(archive: &'a Archive) -> Self {
        Recorder::new(archive.log(), &[archive.content(), archive.derived()])
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
        self.claims.extend(claims);
        self.items += 1;
        self.stored = self.stored.saturating_add(stored);
        self.since.get_or_insert_with(Instant::now);
        if !self.batch.is_full(self.items, self.stored, self.since) {
            return Ok(false);
        }
        self.write()?;
        Ok(true)
    }

    /// Write the claims held: the stores flush the files written so far,
    /// then the claims go into the log as one bundle, flushed. Nothing to
    /// do when nothing is held.
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
    use crate::claim::{Attribute, Run, Source, Subject, Timestamp};

    fn archive(dir: &TempDir) -> Archive {
        Archive::create(dir.path().join("archive"), Algorithm::Sha256).unwrap()
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
        let archive = archive(&dir);
        let mut recorder = Recorder::for_archive(&archive);

        let full = Batch::DEFAULT.items;
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
        let archive = archive(&dir);
        let mut recorder = Recorder::for_archive(&archive);
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
        let archive = archive(&dir);
        let mut recorder = Recorder::for_archive(&archive);

        let (_, claims) = item(&archive, b"a large file");
        assert!(!recorder.push(claims, Batch::DEFAULT.bytes - 1).unwrap());
        let (path, claims) = item(&archive, b"one byte more");
        assert!(recorder.push(claims, 1).unwrap());

        assert!(path.is_file());
        assert_eq!(archive.log().head().unwrap().len(), 2);
    }

    #[test]
    fn the_stores_of_an_archive_wait_for_the_recorder() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
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
        let archive = archive(&dir);
        let mut recorder = Recorder::for_archive(&archive);
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
        let archive = archive(&dir);
        let mut recorder = Recorder::for_archive(&archive);
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
    fn a_lazy_archive_holds_larger_batches() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir).lazy_flush();
        let mut recorder = Recorder::for_archive(&archive);

        for n in 0..Batch::DEFAULT.items {
            let (_, claims) = item(&archive, format!("file {n}").as_bytes());
            assert!(!recorder.push(claims, 0).unwrap());
        }

        assert!(archive.log().head().unwrap().is_empty());
        recorder.write().unwrap();
        assert_eq!(archive.log().head().unwrap().len(), Batch::DEFAULT.items);
    }
}
