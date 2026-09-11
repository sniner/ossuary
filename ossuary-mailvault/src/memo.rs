//! This program's memory in `cache/`: where each folder's fetch carries
//! on, and which sightings a takeover has already said.
//!
//! Both are cache by the archive's own rule: the memory informs the
//! effort, never the truth. Losing the file costs time, never a claim —
//! and no claim is ever built from what stands here. The takeover memo
//! in particular holds no subject: a message whose record needs a new
//! line is read again, and the store says what it is.
//!
//! The **resume point** of a folder is what mailvault kept in `heads/`:
//! the folder's UIDVALIDITY and the highest UID fetched under it. The
//! next run asks the server only for what lies above. A UID is the
//! server's temporary numbering — it means nothing once the UIDVALIDITY
//! changes — so it belongs here and nowhere on a message's record.
//! Without a resume point a folder is fetched whole; the bytes dedup in
//! the store, and the places are said again.
//!
//! The **takeover memo** remembers which places of a mailvault
//! archive's messages are on the record, so an interrupted takeover
//! carries on and a message already said with every place is not read
//! again.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension as _, params};

/// The memo's file name in `cache/`.
pub const FILE_NAME: &str = "mailvault.sqlite";

/// A folder's resume point: what the server promised about its UIDs,
/// and the highest one fetched under that promise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resume {
    pub uidvalidity: u32,
    pub uid: u32,
}

pub struct Memo {
    connection: Connection,
}

impl Memo {
    /// Open the memo at `path`, making file and schema as needed.
    ///
    /// # Errors
    ///
    /// The directory or the file refusing.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)
                .with_context(|| format!("{}: creating cache/", dir.display()))?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("{}: the memo would not open", path.display()))?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS resume (
                 account     TEXT NOT NULL,
                 folder      TEXT NOT NULL,
                 uidvalidity INTEGER NOT NULL,
                 uid         INTEGER NOT NULL,
                 PRIMARY KEY (account, folder)
             );
             CREATE TABLE IF NOT EXISTS said (
                 store_id TEXT NOT NULL,
                 place    TEXT NOT NULL,
                 PRIMARY KEY (store_id, place)
             );",
        )?;
        Ok(Self { connection })
    }

    /// Begin a transaction: many small writes, one sync.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn begin(&self) -> Result<()> {
        self.connection.execute_batch("BEGIN IMMEDIATE")?;
        Ok(())
    }

    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn commit(&self) -> Result<()> {
        self.connection.execute_batch("COMMIT")?;
        Ok(())
    }

    /// Where a folder's fetch carries on — `None` for a folder never
    /// fetched.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn resume(&self, account: &str, folder: &str) -> Result<Option<Resume>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT uidvalidity, uid FROM resume WHERE account = ?1 AND folder = ?2",
        )?;
        let found = statement
            .query_row(params![account, folder], |row| {
                Ok(Resume {
                    uidvalidity: row.get(0)?,
                    uid: row.get(1)?,
                })
            })
            .optional()?;
        Ok(found)
    }

    /// A message of the folder is in: the fetch carries on above it.
    /// Under the same UIDVALIDITY the point only ever moves up — the
    /// server hands a batch back in its own order, and the highest UID
    /// seen is the one to carry on from. A new UIDVALIDITY starts over.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn advance(&self, account: &str, folder: &str, resume: Resume) -> Result<()> {
        let mut statement = self.connection.prepare_cached(
            "INSERT INTO resume (account, folder, uidvalidity, uid) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (account, folder) DO UPDATE SET
                 uid = CASE WHEN uidvalidity = excluded.uidvalidity
                            THEN MAX(uid, excluded.uid)
                            ELSE excluded.uid END,
                 uidvalidity = excluded.uidvalidity",
        )?;
        statement.execute(params![account, folder, resume.uidvalidity, resume.uid])?;
        Ok(())
    }

    /// The places of a vault's message that are on the record — `None`
    /// for a message never taken in, an empty set for one taken in
    /// without a place.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn said(&self, store_id: &str) -> Result<Option<BTreeSet<String>>> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT place FROM said WHERE store_id = ?1")?;
        let rows = statement.query_map(params![store_id], |row| row.get::<_, String>(0))?;
        let mut taken = false;
        let mut places = BTreeSet::new();
        for row in rows {
            taken = true;
            let place = row?;
            if !place.is_empty() {
                places.insert(place);
            }
        }
        Ok(taken.then_some(places))
    }

    /// Remember: these places of this vault message are said. A message
    /// with no place at all is remembered under an empty one, so it is
    /// not read again either.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn say(&self, store_id: &str, places: &[&String]) -> Result<()> {
        let mut statement = self
            .connection
            .prepare_cached("INSERT OR IGNORE INTO said (store_id, place) VALUES (?1, ?2)")?;
        if places.is_empty() {
            statement.execute(params![store_id, ""])?;
        }
        for place in places {
            statement.execute(params![store_id, place])?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memo() -> (tempfile::TempDir, Memo) {
        let dir = tempfile::tempdir().unwrap();
        let memo = Memo::open(&dir.path().join("cache").join(FILE_NAME)).unwrap();
        (dir, memo)
    }

    fn at(uidvalidity: u32, uid: u32) -> Resume {
        Resume { uidvalidity, uid }
    }

    #[test]
    fn a_resume_point_is_remembered_per_folder_and_moves_forward() {
        let (_dir, memo) = memo();

        assert_eq!(memo.resume("a", "INBOX").unwrap(), None);
        memo.advance("a", "INBOX", at(7, 12)).unwrap();
        memo.advance("a", "INBOX", at(7, 40)).unwrap();
        assert_eq!(memo.resume("a", "INBOX").unwrap(), Some(at(7, 40)));
        assert_eq!(
            memo.resume("a", "Sent").unwrap(),
            None,
            "another folder is another point"
        );
    }

    #[test]
    fn a_resume_point_never_moves_back_under_the_same_promise() {
        let (_dir, memo) = memo();
        memo.advance("a", "INBOX", at(7, 40)).unwrap();
        memo.advance("a", "INBOX", at(7, 30)).unwrap();
        assert_eq!(
            memo.resume("a", "INBOX").unwrap(),
            Some(at(7, 40)),
            "a batch handed back out of order does not lower the point"
        );
        memo.advance("a", "INBOX", at(8, 3)).unwrap();
        assert_eq!(
            memo.resume("a", "INBOX").unwrap(),
            Some(at(8, 3)),
            "a new UIDVALIDITY starts over"
        );
    }

    #[test]
    fn what_a_takeover_said_is_remembered_by_place() {
        let (_dir, memo) = memo();
        assert_eq!(memo.said("abc").unwrap(), None, "never taken in");

        memo.say("abc", &[]).unwrap();
        assert_eq!(
            memo.said("abc").unwrap(),
            Some(BTreeSet::new()),
            "taken in without a place is still taken in"
        );

        let inbox = "x/INBOX".to_string();
        let sent = "x/Sent".to_string();
        memo.say("abc", &[&inbox]).unwrap();
        memo.say("abc", &[&inbox, &sent]).unwrap();
        assert_eq!(
            memo.said("abc").unwrap(),
            Some(BTreeSet::from([inbox, sent])),
            "places accrete, said twice counts once"
        );
    }
}
