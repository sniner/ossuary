//! Claims held back until the files they name are flushed.
//!
//! A writer that stores files and records claims about them (ingest, an
//! extractor run, a fetch) hands each item's claims to a [`Recorder`]. The
//! recorder writes them to the log as one bundle for every [`BATCH`] items,
//! or sooner when the items stored [`BATCH_BYTES`], after the stores have
//! flushed the files those claims name: a claim is never on the device
//! before the file it is about, and the log gets one bundle per batch
//! instead of one per file.
//!
//! The stores of an archive do not flush by themselves (see
//! [`Archive`](crate::Archive)), so the recorder decides when a batch ends,
//! and a flush that fails is reported by [`Recorder::write`] and nowhere
//! else. After such a failure the recorder writes nothing more: the files
//! of its claims may be missing, and a caller that goes on to save a resume
//! point gets the error again instead.
//!
//! An archive that flushes only at the end (see
//! [`Archive::flush_at_end`](crate::Archive::flush_at_end)) writes the
//! bundles without flushing anything. [`Recorder::durable`] says so, for a
//! caller that keeps a resume point of its own and must not move it past
//! claims a crash of the system can still take.

use immure::{Flush, Store};

use crate::archive::Archive;
use crate::claim::Claim;
use crate::error::{Error, Result};
use crate::log::Log;

/// How many items a [`Recorder`] holds before it writes them.
pub const BATCH: usize = 100;

/// How many bytes the items held by a [`Recorder`] may have stored before
/// it writes them, however few they are: a batch of large files does not
/// wait unflushed for its hundredth.
pub const BATCH_BYTES: u64 = 64 << 20;

/// Claims of items not written yet, and where they go.
#[derive(Debug)]
pub struct Recorder<'a> {
    log: &'a Log,
    /// The stores the files named by the claims lie in.
    stores: Vec<&'a Store>,
    claims: Vec<Claim>,
    items: usize,
    /// The bytes the items held stored.
    stored: u64,
    /// A write failed. The files of the claims held then may be missing.
    failed: bool,
}

impl<'a> Recorder<'a> {
    /// A recorder writing to `log`, about files in `stores`.
    #[must_use]
    pub fn new(log: &'a Log, stores: &[&'a Store]) -> Self {
        Recorder {
            log,
            stores: stores.to_vec(),
            claims: Vec::new(),
            items: 0,
            stored: 0,
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
    /// it added none. Once [`BATCH`] items are held, or they stored
    /// [`BATCH_BYTES`], they are written. Answers whether that happened.
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
        if self.items < BATCH && self.stored < BATCH_BYTES {
            return Ok(false);
        }
        self.write()?;
        Ok(true)
    }

    /// Write the claims held: the stores flush the files written so far,
    /// then the claims go into the log as one bundle. Nothing to do when
    /// nothing is held.
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
        Ok(())
    }

    fn flush_and_append(&self) -> Result<()> {
        for store in &self.stores {
            // A store that flushes at the end is flushed there.
            if store.flushes() != Flush::OnRequest {
                store.flush()?;
            }
        }
        self.log.append_all(&self.claims)
    }

    /// How many items are held and not written yet.
    #[must_use]
    pub fn held(&self) -> usize {
        self.items
    }

    /// Whether what [`write`](Recorder::write) wrote is on the device when
    /// it returns: not when the archive flushes only at the end.
    #[must_use]
    pub fn durable(&self) -> bool {
        !self.log.flushes_at_end()
            && self
                .stores
                .iter()
                .all(|store| store.flushes() != Flush::OnRequest)
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

        for n in 1..BATCH {
            let (_, claims) = item(&archive, format!("file {n}").as_bytes());
            assert!(!recorder.push(claims, 0).unwrap());
        }
        assert!(
            archive.log().head().unwrap().is_empty(),
            "held, not written"
        );
        assert_eq!(recorder.held(), BATCH - 1);

        let (path, claims) = item(&archive, b"the last of the batch");
        assert!(recorder.push(claims, 0).unwrap(), "written with the batch");

        assert_eq!(recorder.held(), 0);
        assert_eq!(archive.log().head().unwrap().len(), BATCH);
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

        assert!(recorder.durable());
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
        assert!(!recorder.push(claims, BATCH_BYTES - 1).unwrap());
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
    fn an_archive_flushing_at_the_end_writes_without_flushing() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir).flush_at_end();
        let mut recorder = Recorder::for_archive(&archive);
        let (path, claims) = item(&archive, b"named at once");

        assert!(archive.flushes_at_end());
        assert!(!recorder.durable());
        assert!(path.is_file(), "under its name at once");
        recorder.push(claims, 0).unwrap();
        recorder.write().unwrap();
        assert_eq!(archive.log().head().unwrap().len(), 1);
        archive.flush().unwrap();
    }
}
