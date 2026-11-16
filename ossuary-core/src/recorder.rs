//! Claims held back until the files they name are flushed.
//!
//! A writer that stores files and records claims about them (ingest, an
//! extractor run, a fetch) hands each item's claims to a [`Recorder`]. The
//! recorder writes them to the log as one bundle for every [`BATCH`] items,
//! after the stores have flushed the files those claims name: a claim is
//! never on the device before the file it is about, and the log gets one
//! bundle per batch instead of one per file.
//!
//! An archive that flushes only at the end (see
//! [`Archive::flush_at_end`](crate::Archive::flush_at_end)) writes the
//! bundles without flushing anything. [`Recorder::durable`] says so, for a
//! caller that keeps a resume point of its own and must not move it past
//! claims a crash of the system can still take.

use immure::{Flush, Store};

use crate::archive::Archive;
use crate::claim::Claim;
use crate::error::Result;
use crate::log::Log;

/// How many items a [`Recorder`] holds before it writes them: as many as
/// the stores flush together by default.
pub const BATCH: usize = immure::DEFAULT_BATCH.get();

/// Claims of items not written yet, and where they go.
#[derive(Debug)]
pub struct Recorder<'a> {
    log: &'a Log,
    /// The stores the files named by the claims lie in.
    stores: Vec<&'a Store>,
    claims: Vec<Claim>,
    items: usize,
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
        }
    }

    /// A recorder writing to the archive's log, about files in its content
    /// and derived stores.
    #[must_use]
    pub fn for_archive(archive: &'a Archive) -> Self {
        Recorder::new(archive.log(), &[archive.content(), archive.derived()])
    }

    /// Hold one item's claims: a file's, an examination's, a message's.
    /// Once [`BATCH`] items are held, they are written. Answers whether
    /// that happened.
    ///
    /// # Errors
    ///
    /// As [`write`](Recorder::write).
    pub fn push(&mut self, claims: Vec<Claim>) -> Result<bool> {
        self.claims.extend(claims);
        self.items += 1;
        if self.items < BATCH {
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
    /// whatever appending to the log can answer. The claims stay held then.
    pub fn write(&mut self) -> Result<()> {
        if self.items == 0 {
            return Ok(());
        }
        for store in &self.stores {
            // A store that flushes at the end is flushed there.
            if store.flushes() != Flush::OnRequest {
                store.flush()?;
            }
        }
        self.log.append_all(&self.claims)?;
        self.claims.clear();
        self.items = 0;
        Ok(())
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
            assert!(!recorder.push(claims).unwrap());
        }
        assert!(
            archive.log().head().unwrap().is_empty(),
            "held, not written"
        );
        assert_eq!(recorder.held(), BATCH - 1);

        let (path, claims) = item(&archive, b"the last of the batch");
        assert!(recorder.push(claims).unwrap(), "written with the batch");

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

        recorder.push(claims).unwrap();
        recorder.write().unwrap();

        assert!(recorder.durable());
        assert!(path.is_file());
        assert_eq!(archive.log().head().unwrap().len(), 1);
        recorder.write().unwrap();
        assert_eq!(archive.log().head().unwrap().len(), 1, "nothing twice");
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
        recorder.push(claims).unwrap();
        recorder.write().unwrap();
        assert_eq!(archive.log().head().unwrap().len(), 1);
        archive.flush().unwrap();
    }
}
