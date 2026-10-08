//! This program's memory in `cache/`: where each folder's fetch carries
//! on, what a listing left to fetch, and which messages an import has
//! already recorded.
//!
//! All of it is cache by the archive's own rule: losing the file costs
//! time, never a claim, and no claim is ever built from what stands
//! here. Three tables:
//!
//! - `resume` (account, folder, state): one row per folder, `state`
//!   is JSON whose shape the backend decides. IMAP keeps the folder's
//!   UIDVALIDITY and the highest UID fetched under it; the next run
//!   asks the server only for what lies above. MS Graph keeps the
//!   delta link the server handed out at the end of the last listing,
//!   and when, so an expired link can be reported with its age. A
//!   state the backend cannot read, because the account was switched
//!   to another backend, counts as none.
//! - `pending` (account, folder, id, detail): the messages a listing
//!   reported that are not in the archive yet, `detail` again JSON of
//!   the backend's choosing. Graph keeps the categories there. A
//!   message is removed when it is stored.
//! - `imported` (`store_id`, place): for the takeover of a Python
//!   mailvault archive, which places of each of its messages are on
//!   the record, so an interrupted import carries on and a message
//!   recorded with every place is not read again.
//!
//! Server-side numbering like a UID belongs here and nowhere on a
//! message's record: it means nothing once the UIDVALIDITY changes.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension as _, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::graph::Offered;

/// The memo's file name in `cache/`.
pub const FILE_NAME: &str = "mailvault.sqlite";

/// An IMAP folder's resume point: what the server promised about its
/// UIDs, and the highest one fetched under that promise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImapResume {
    pub uidvalidity: u32,
    pub uid: u32,
}

/// An MS Graph folder's resume point: the delta link the server handed
/// out at the end of the last listing, and when, in seconds since the
/// epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphResume {
    pub link: String,
    pub issued: i64,
}

impl GraphResume {
    /// How old the link is, for the line that reports the server
    /// refusing it: the one way to learn how long these live.
    #[must_use]
    pub fn age(&self) -> String {
        let seconds = now().saturating_sub(self.issued).max(0);
        let hours = seconds / 3600;
        if hours < 48 {
            format!("{hours}h")
        } else {
            format!("{}d", hours / 24)
        }
    }
}

/// Seconds since the epoch, as the memo stamps them.
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
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
            .with_context(|| format!("{}: could not be opened", path.display()))?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS resume (
                 account TEXT NOT NULL,
                 folder  TEXT NOT NULL,
                 state   TEXT NOT NULL,
                 PRIMARY KEY (account, folder)
             );
             CREATE TABLE IF NOT EXISTS pending (
                 account TEXT NOT NULL,
                 folder  TEXT NOT NULL,
                 id      TEXT NOT NULL,
                 detail  TEXT NOT NULL,
                 PRIMARY KEY (account, folder, id)
             );
             CREATE TABLE IF NOT EXISTS imported (
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

    /// Give the space of deleted rows back to the file system. The
    /// pending list of a large folder is written and deleted again
    /// within one run, and the file would otherwise keep that size.
    /// Not inside a transaction.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn compact(&self) -> Result<()> {
        self.connection.execute_batch("VACUUM")?;
        Ok(())
    }

    /// The folder's resume point read as `T`: `None` for a folder
    /// never fetched, and for a state written by another backend.
    fn resume<T: DeserializeOwned>(&self, account: &str, folder: &str) -> Result<Option<T>> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT state FROM resume WHERE account = ?1 AND folder = ?2")?;
        let state: Option<String> = statement
            .query_row(params![account, folder], |row| row.get(0))
            .optional()?;
        Ok(state.and_then(|state| serde_json::from_str(&state).ok()))
    }

    fn set_resume<T: Serialize>(&self, account: &str, folder: &str, state: &T) -> Result<()> {
        let mut statement = self.connection.prepare_cached(
            "INSERT INTO resume (account, folder, state) VALUES (?1, ?2, ?3)
             ON CONFLICT (account, folder) DO UPDATE SET state = excluded.state",
        )?;
        statement.execute(params![account, folder, serde_json::to_string(state)?])?;
        Ok(())
    }

    /// Where an IMAP folder's fetch carries on.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn imap_resume(&self, account: &str, folder: &str) -> Result<Option<ImapResume>> {
        self.resume(account, folder)
    }

    /// A message of the IMAP folder is in: the fetch carries on above
    /// it. Under the same UIDVALIDITY the point only ever moves up,
    /// because the server returns a batch in its own order and the
    /// highest UID seen is the one to carry on from. A new UIDVALIDITY
    /// starts over.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn advance_imap(&self, account: &str, folder: &str, resume: ImapResume) -> Result<()> {
        let next = match self.imap_resume(account, folder)? {
            Some(current) if current.uidvalidity == resume.uidvalidity => ImapResume {
                uid: current.uid.max(resume.uid),
                ..resume
            },
            _ => resume,
        };
        self.set_resume(account, folder, &next)
    }

    /// Where an MS Graph folder's listings carry on.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn graph_resume(&self, account: &str, folder: &str) -> Result<Option<GraphResume>> {
        self.resume(account, folder)
    }

    /// A listing of the Graph folder is complete: the next run carries
    /// on from this link. What the listing reported goes on the pending
    /// list separately, so the link can be saved before any of it is
    /// fetched.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn advance_graph(&self, account: &str, folder: &str, link: &str) -> Result<()> {
        let resume = GraphResume {
            link: link.to_string(),
            issued: now(),
        };
        self.set_resume(account, folder, &resume)
    }

    /// The messages of the folder an earlier listing reported that are
    /// not stored yet, with the categories it reported, in the order
    /// they were listed.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing, or a detail column that is not the JSON this
    /// program writes.
    pub fn pending(&self, account: &str, folder: &str) -> Result<Vec<Offered>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT id, detail FROM pending WHERE account = ?1 AND folder = ?2 ORDER BY rowid",
        )?;
        let rows = statement.query_map(params![account, folder], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut pending = Vec::new();
        for row in rows {
            let (id, detail) = row?;
            let tags = serde_json::from_str(&detail)
                .with_context(|| format!("pending message {id}: detail is not a JSON list"))?;
            pending.push(Offered { id, tags });
        }
        Ok(pending)
    }

    /// Put the messages a listing reported on the folder's pending
    /// list. A message already on the list gets the categories of the
    /// newer listing.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn add_pending(&self, account: &str, folder: &str, offered: &[Offered]) -> Result<()> {
        let mut statement = self.connection.prepare_cached(
            "INSERT INTO pending (account, folder, id, detail) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (account, folder, id) DO UPDATE SET detail = excluded.detail",
        )?;
        for message in offered {
            let detail = serde_json::to_string(&message.tags)?;
            statement.execute(params![account, folder, message.id, detail])?;
        }
        Ok(())
    }

    /// The message is stored, or the server no longer has it: take it
    /// off the folder's pending list.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn remove_pending(&self, account: &str, folder: &str, id: &str) -> Result<()> {
        let mut statement = self
            .connection
            .prepare_cached("DELETE FROM pending WHERE account = ?1 AND folder = ?2 AND id = ?3")?;
        statement.execute(params![account, folder, id])?;
        Ok(())
    }

    /// Empty the folder's pending list: a listing from scratch reports
    /// the folder whole and replaces whatever an earlier one left.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn clear_pending(&self, account: &str, folder: &str) -> Result<()> {
        let mut statement = self
            .connection
            .prepare_cached("DELETE FROM pending WHERE account = ?1 AND folder = ?2")?;
        statement.execute(params![account, folder])?;
        Ok(())
    }

    /// The places of a vault's message that are on the record: `None`
    /// for a message never imported, an empty set for one imported
    /// without a place.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn imported(&self, store_id: &str) -> Result<Option<BTreeSet<String>>> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT place FROM imported WHERE store_id = ?1")?;
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

    /// Remember that these places of this vault message are recorded.
    /// A message with no place at all is remembered under an empty
    /// one, so it is not read again either.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn mark_imported(&self, store_id: &str, places: &[&String]) -> Result<()> {
        let mut statement = self
            .connection
            .prepare_cached("INSERT OR IGNORE INTO imported (store_id, place) VALUES (?1, ?2)")?;
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

    fn at(uidvalidity: u32, uid: u32) -> ImapResume {
        ImapResume { uidvalidity, uid }
    }

    #[test]
    fn an_imap_resume_point_is_remembered_per_folder_and_moves_forward() {
        let (_dir, memo) = memo();

        assert_eq!(memo.imap_resume("a", "INBOX").unwrap(), None);
        memo.advance_imap("a", "INBOX", at(7, 12)).unwrap();
        memo.advance_imap("a", "INBOX", at(7, 40)).unwrap();
        assert_eq!(memo.imap_resume("a", "INBOX").unwrap(), Some(at(7, 40)));
        assert_eq!(
            memo.imap_resume("a", "Sent").unwrap(),
            None,
            "another folder is another point"
        );
    }

    #[test]
    fn an_imap_resume_point_never_moves_back_under_the_same_promise() {
        let (_dir, memo) = memo();
        memo.advance_imap("a", "INBOX", at(7, 40)).unwrap();
        memo.advance_imap("a", "INBOX", at(7, 30)).unwrap();
        assert_eq!(
            memo.imap_resume("a", "INBOX").unwrap(),
            Some(at(7, 40)),
            "a batch returned out of order does not lower the point"
        );
        memo.advance_imap("a", "INBOX", at(8, 3)).unwrap();
        assert_eq!(
            memo.imap_resume("a", "INBOX").unwrap(),
            Some(at(8, 3)),
            "a new UIDVALIDITY starts over"
        );
    }

    #[test]
    fn a_graph_resume_point_is_remembered_per_folder_and_replaced_whole() {
        let (_dir, memo) = memo();
        assert_eq!(memo.graph_resume("a", "Inbox").unwrap(), None);
        memo.advance_graph("a", "Inbox", "https://graph.example/one")
            .unwrap();
        memo.advance_graph("a", "Inbox", "https://graph.example/two")
            .unwrap();
        let point = memo.graph_resume("a", "Inbox").unwrap().unwrap();
        assert_eq!(point.link, "https://graph.example/two");
        assert!(point.issued > 0);
        assert_eq!(point.age(), "0h", "just issued");
        assert_eq!(memo.graph_resume("a", "Sent").unwrap(), None);
    }

    #[test]
    fn a_resume_point_of_the_other_backend_counts_as_none() {
        let (_dir, memo) = memo();
        memo.advance_graph("a", "Inbox", "https://graph.example/one")
            .unwrap();
        assert_eq!(
            memo.imap_resume("a", "Inbox").unwrap(),
            None,
            "the account was switched to IMAP: a first fetch"
        );
        memo.advance_imap("a", "Inbox", at(7, 40)).unwrap();
        assert_eq!(memo.graph_resume("a", "Inbox").unwrap(), None);
        assert_eq!(memo.imap_resume("a", "Inbox").unwrap(), Some(at(7, 40)));
    }

    #[test]
    fn the_pending_list_is_kept_per_folder_in_listing_order() {
        let (_dir, memo) = memo();
        let offered = |id: &str, tags: &[&str]| Offered {
            id: id.to_string(),
            tags: tags.iter().map(ToString::to_string).collect(),
        };
        assert!(memo.pending("a", "Inbox").unwrap().is_empty());

        memo.add_pending("a", "Inbox", &[offered("M2", &["Red"]), offered("M1", &[])])
            .unwrap();
        memo.add_pending("a", "Sent", &[offered("S1", &[])])
            .unwrap();
        assert_eq!(
            memo.pending("a", "Inbox").unwrap(),
            [offered("M2", &["Red"]), offered("M1", &[])]
        );

        memo.add_pending("a", "Inbox", &[offered("M2", &["Blue"])])
            .unwrap();
        assert_eq!(
            memo.pending("a", "Inbox").unwrap(),
            [offered("M2", &["Blue"]), offered("M1", &[])],
            "a newer listing replaces the categories"
        );

        memo.remove_pending("a", "Inbox", "M2").unwrap();
        assert_eq!(memo.pending("a", "Inbox").unwrap(), [offered("M1", &[])]);

        memo.clear_pending("a", "Inbox").unwrap();
        assert!(memo.pending("a", "Inbox").unwrap().is_empty());
        assert_eq!(
            memo.pending("a", "Sent").unwrap(),
            [offered("S1", &[])],
            "another folder's list is untouched"
        );
        memo.compact().unwrap();
    }

    #[test]
    fn a_links_age_reads_in_hours_and_then_in_days() {
        let hours = |h: i64| GraphResume {
            link: String::new(),
            issued: now() - h * 3600,
        };
        assert_eq!(hours(5).age(), "5h");
        assert_eq!(hours(47).age(), "47h");
        assert_eq!(hours(48).age(), "2d");
        assert_eq!(hours(24 * 30).age(), "30d");
    }

    #[test]
    fn what_an_import_recorded_is_remembered_by_place() {
        let (_dir, memo) = memo();
        assert_eq!(memo.imported("abc").unwrap(), None, "never imported");

        memo.mark_imported("abc", &[]).unwrap();
        assert_eq!(
            memo.imported("abc").unwrap(),
            Some(BTreeSet::new()),
            "imported without a place is still imported"
        );

        let inbox = "x:INBOX".to_string();
        let sent = "x:Sent".to_string();
        memo.mark_imported("abc", &[&inbox]).unwrap();
        memo.mark_imported("abc", &[&inbox, &sent]).unwrap();
        assert_eq!(
            memo.imported("abc").unwrap(),
            Some(BTreeSet::from([inbox, sent])),
            "places accrete, recorded twice counts once"
        );
    }
}
