//! The ingest memory: which files earlier runs already recorded.
//!
//! A cache in `cache/ingest.sqlite`, opened by
//! [`Archive::ingest_memory`](crate::Archive::ingest_memory) and read and
//! written by `ingest`. It knows nothing of the walk: one row per host and
//! path, and the sealed segment that shows which archive it belongs to.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};

use crate::error::Result;

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
    /// [`Error::Index`](crate::Error::Index) when `SQLite` cannot open or prepare it — and like
    /// the index, a memory broken rather than merely refusing may simply
    /// be deleted.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let connection = Connection::open(path)?;
        // In one transaction, or each table costs a commit of its own.
        connection.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS seen (
                 host  TEXT NOT NULL,
                 path  TEXT NOT NULL,
                 size  INTEGER NOT NULL,
                 mtime INTEGER NOT NULL,
                 PRIMARY KEY (host, path)
             );
             CREATE TABLE IF NOT EXISTS archive (
                 id      INTEGER PRIMARY KEY CHECK (id = 0),
                 segment TEXT NOT NULL
             );
             COMMIT;",
        )?;
        Ok(IngestMemory { connection })
    }

    /// The sealed segment the memory recorded when it was last written:
    /// what shows a memory kept outside the archive to belong to it (see
    /// [`Archive::accepts_cache`](crate::Archive::accepts_cache)). `None`
    /// when nothing was sealed then.
    ///
    /// # Errors
    ///
    /// [`Error::Index`](crate::Error::Index) from `SQLite`.
    pub fn proof(&self) -> Result<Option<String>> {
        use rusqlite::OptionalExtension as _;
        let segment = self
            .connection
            .query_row("SELECT segment FROM archive WHERE id = 0", [], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(segment)
    }

    /// Forget every sighting and the proof: the next run reads every
    /// file again.
    ///
    /// # Errors
    ///
    /// [`Error::Index`](crate::Error::Index) from `SQLite`.
    pub fn clear(&self) -> Result<()> {
        self.connection
            .execute_batch("DELETE FROM seen; DELETE FROM archive;")?;
        Ok(())
    }

    fn prove(&self, segment: &str) -> Result<()> {
        self.connection.execute(
            "INSERT OR REPLACE INTO archive (id, segment) VALUES (0, ?1)",
            params![segment],
        )?;
        Ok(())
    }
}

impl IngestMemory {
    /// Whether this place was last seen with exactly this size and mtime.
    pub(crate) fn unchanged(&self, host: &str, path: &Path, size: i64, mtime: i64) -> Result<bool> {
        let mut statement = self.connection.prepare_cached(
            "SELECT 1 FROM seen WHERE host = ?1 AND path = ?2 AND size = ?3 AND mtime = ?4",
        )?;
        let found = statement.exists(params![host, path.to_string_lossy(), size, mtime])?;
        Ok(found)
    }

    /// Forget a place whose sighting was just retracted, so that a file
    /// that returns there unchanged is read again.
    fn forget(&self, host: &str, path: &Path) -> Result<()> {
        let mut statement = self
            .connection
            .prepare_cached("DELETE FROM seen WHERE host = ?1 AND path = ?2")?;
        statement.execute(params![host, path.to_string_lossy()])?;
        Ok(())
    }

    /// Forget these paths, on whichever host they were seen: the next run
    /// reads the files there again.
    ///
    /// # Errors
    ///
    /// [`Error::Index`](crate::Error::Index) from `SQLite`.
    pub fn forget_paths(&self, paths: &[&str]) -> Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        {
            let mut statement = transaction.prepare_cached("DELETE FROM seen WHERE path = ?1")?;
            for path in paths {
                statement.execute(params![path])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Remember a sighting that just went on the record.
    pub(crate) fn record(&self, host: &str, path: &Path, size: i64, mtime: i64) -> Result<()> {
        let mut statement = self.connection.prepare_cached(
            "INSERT INTO seen (host, path, size, mtime) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (host, path) DO UPDATE
             SET size = excluded.size, mtime = excluded.mtime",
        )?;
        statement.execute(params![host, path.to_string_lossy(), size, mtime])?;
        Ok(())
    }

    /// Write one step of a run in one transaction: remember the
    /// sightings `seen` (path, size, mtime), forget the places `gone`,
    /// and record `proof`, the segment sealed last, when there is one.
    ///
    /// # Errors
    ///
    /// [`Error::Index`](crate::Error::Index) from `SQLite`.
    pub(crate) fn write<'p>(
        &self,
        host: &str,
        seen: &[(PathBuf, i64, i64)],
        gone: impl Iterator<Item = &'p Path>,
        proof: Option<&str>,
    ) -> Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        for (path, size, mtime) in seen {
            self.record(host, path, *size, *mtime)?;
        }
        for path in gone {
            self.forget(host, path)?;
        }
        if let Some(proof) = proof {
            self.prove(proof)?;
        }
        transaction.commit()?;
        Ok(())
    }
}
