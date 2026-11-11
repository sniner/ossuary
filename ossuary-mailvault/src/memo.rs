//! This program's memory in `cache/`: where each folder's fetch carries
//! on, what a listing left to fetch, and which messages an import has
//! already recorded.
//!
//! All of it is cache by the archive's own rule: losing the file costs
//! time, never a claim, and no claim is ever built from what stands
//! here. Five tables:
//!
//! - `resume` (account, folder, state): one row per folder, `state`
//!   is a [`Resume`] as JSON, tagged with the backend that wrote it.
//!   IMAP keeps the folder's UIDVALIDITY and the highest UID fetched
//!   under it; the next run asks the server only for what lies above.
//!   MS Graph keeps the delta link the server handed out at the end of
//!   the last listing, and when, so an expired link can be reported
//!   with its age. A point written by the other backend, because the
//!   account was switched, counts as none.
//! - `pending` (account, folder, id, detail, attempts): the messages a
//!   listing reported that are not in the archive yet. `detail` is
//!   JSON of the backend's choosing; Graph keeps the categories there.
//!   `attempts` counts the runs that failed to fetch the message. A
//!   message is removed when it is stored, or when it is given up. For a
//!   label folder of Proton Mail Bridge the rows are the UIDs whose
//!   Proton id is not in the archive yet; they are never given up, and
//!   a row is removed when the id is found or the server no longer lists
//!   the UID.
//! - `imported` (`store_id`, place): for the takeover of a Python
//!   mailvault archive, which places of each of its messages are on
//!   the record, so an interrupted import carries on and a message
//!   recorded with every place is not read again.
//! - `compared` (account, folder, id, subject): the messages `fetch
//!   --compare` fetched, by server id, and the subject each turned out
//!   to be. A message the Message-ID cannot settle, such as one of two
//!   copies on the server, would otherwise be fetched by every
//!   `--compare`. `id` is the Graph id, or UIDVALIDITY and UID as
//!   `uidvalidity/uid`, so a renumbered folder matches no row. Each
//!   `--compare` removes the rows of messages no longer listed, no
//!   longer at the place, or settled by the Message-ID.
//! - `archive` (id, segment): one row, the sealed segment the archive
//!   held last when the memo was committed. A memo kept outside the
//!   archive is used only when the archive holds that segment (see
//!   [`Memo::proof`]).
//!
//! Server-side numbering like a UID belongs here and nowhere on a
//! message's record: it means nothing once the UIDVALIDITY changes.
//!
//! A run writes in batches: between [`Memo::begin`] and [`Memo::commit`]
//! the writes are kept in memory, and the commit applies them in one
//! transaction. The file is locked only for that moment and not while
//! the run downloads, so a second run on the same archive is not kept
//! from its own commits.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use rusqlite::{Connection, OptionalExtension as _, Transaction, TransactionBehavior, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// The memo's file name in `cache/`.
pub const FILE_NAME: &str = "mailvault.sqlite";

/// How much space deleted rows must take up before [`Memo::compact`]
/// rewrites the file.
const COMPACT_FROM: i64 = 1024 * 1024;

/// Every table of the memo.
const TABLES: [&str; 5] = ["resume", "pending", "imported", "compared", "archive"];

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

/// A folder's resume point, as the backend that fetched it wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "lowercase")]
pub enum Resume {
    Imap(ImapResume),
    Graph(GraphResume),
}

/// One entry of a folder's pending list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending<T> {
    pub id: String,
    /// What the listing reported about the message, as the backend
    /// wrote it.
    pub detail: T,
    /// How many runs failed to fetch it.
    pub attempts: u32,
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

/// The folder a write is about.
struct At {
    account: String,
    folder: String,
}

impl At {
    fn new(account: &str, folder: &str) -> Self {
        Self {
            account: account.to_string(),
            folder: folder.to_string(),
        }
    }
}

/// One write to the memo: what a public method of [`Memo`] asks for,
/// kept so that a batch can apply it later.
enum Write {
    /// Move an IMAP folder's point up to this one, or start over under
    /// a new UIDVALIDITY.
    AdvanceImap(At, ImapResume),
    SetResume(At, Resume),
    /// Ids for the folder's pending list, each with its detail as JSON.
    AddPending(At, Vec<(String, String)>),
    FailPending(At, String),
    RemovePending(At, String),
    ClearPending(At),
    AddCompared {
        at: At,
        id: String,
        subject: String,
    },
    RemoveCompared(At, Vec<String>),
    MarkImported {
        store_id: String,
        places: Vec<String>,
    },
    /// The sealed segment that shows which archive the memo belongs to.
    Proof(String),
}

pub struct Memo {
    connection: Connection,
    /// The file, for messages; `None` for a memo in memory.
    path: Option<PathBuf>,
    /// The writes of the batch that is open, in the order they were
    /// asked for; `None` when no batch is open.
    batch: RefCell<Option<Vec<Write>>>,
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
                .with_context(|| format!("{}: could not be created", dir.display()))?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("{}: could not be opened", path.display()))?;
        Self::with(connection, Some(path.to_path_buf()))
    }

    /// A memo that holds nothing and is kept in memory, never in a file:
    /// for a dry run that may not use the memo in `cache/`, or would have
    /// to make it.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn empty() -> Result<Self> {
        let connection =
            Connection::open_in_memory().context("opening a temporary database in memory")?;
        Self::with(connection, None)
    }

    /// The memo on `connection`, its tables made where they are missing.
    fn with(connection: Connection, path: Option<PathBuf>) -> Result<Self> {
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS resume (
                 account TEXT NOT NULL,
                 folder  TEXT NOT NULL,
                 state   TEXT NOT NULL,
                 PRIMARY KEY (account, folder)
             );
             CREATE TABLE IF NOT EXISTS pending (
                 account  TEXT NOT NULL,
                 folder   TEXT NOT NULL,
                 id       TEXT NOT NULL,
                 detail   TEXT NOT NULL,
                 attempts INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY (account, folder, id)
             );
             CREATE TABLE IF NOT EXISTS imported (
                 store_id TEXT NOT NULL,
                 place    TEXT NOT NULL,
                 PRIMARY KEY (store_id, place)
             );
             CREATE TABLE IF NOT EXISTS compared (
                 account TEXT NOT NULL,
                 folder  TEXT NOT NULL,
                 id      TEXT NOT NULL,
                 subject TEXT NOT NULL,
                 PRIMARY KEY (account, folder, id)
             );
             CREATE TABLE IF NOT EXISTS archive (
                 id      INTEGER PRIMARY KEY CHECK (id = 0),
                 segment TEXT NOT NULL
             );",
        )?;
        Ok(Self {
            connection,
            path,
            batch: RefCell::new(None),
        })
    }

    /// The memo's file as a message names it, with a stand-in for a
    /// memo in memory.
    #[must_use]
    pub fn file(&self) -> String {
        self.path.as_ref().map_or_else(
            || "the temporary database in memory".to_string(),
            |path| path.display().to_string(),
        )
    }

    /// The sealed segment the memo recorded when it was last committed:
    /// what shows a memo kept outside the archive to belong to it (see
    /// `Archive::accepts_cache`). `None` when nothing was sealed then.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn proof(&self) -> Result<Option<String>> {
        Ok(self
            .connection
            .query_row("SELECT segment FROM archive WHERE id = 0", [], |row| {
                row.get(0)
            })
            .optional()?)
    }

    /// Empty the memo: every folder is fetched in full again, and an
    /// import starts over. Returns whether it held anything. Applied at
    /// once, not as part of a batch.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn clear(&self) -> Result<bool> {
        let mut removed = 0;
        for table in TABLES {
            removed += self
                .connection
                .execute(&format!("DELETE FROM {table}"), [])?;
        }
        Ok(removed > 0)
    }

    /// Whether the memo holds nothing: what [`clear`](Self::clear)
    /// leaves.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn is_empty(&self) -> Result<bool> {
        for table in TABLES {
            let held: bool = self.connection.query_row(
                &format!("SELECT EXISTS (SELECT 1 FROM {table})"),
                [],
                |row| row.get(0),
            )?;
            if held {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// [`commit`](Self::commit), and with it the proof that the memo
    /// belongs to `archive` when it is kept outside it: the segment
    /// sealed last.
    ///
    /// # Errors
    ///
    /// As `commit`, or the archive's open segment not answering.
    pub fn commit_for(&self, archive: &ossuary_core::Archive) -> Result<()> {
        if let Some(sealed) = archive.cache_proof()? {
            self.write(Write::Proof(sealed.as_str().to_string()))?;
        }
        self.commit()
    }

    /// Begin a batch of writes. Until [`commit`](Self::commit) every
    /// write is kept in memory and the file is not touched, so a read
    /// does not see the writes of the open batch. Without a batch a
    /// write is applied at once.
    ///
    /// # Errors
    ///
    /// A batch that is already open.
    pub fn begin(&self) -> Result<()> {
        let mut batch = self.batch.borrow_mut();
        if batch.is_some() {
            bail!("{}: a batch of writes is already open", self.file());
        }
        *batch = Some(Vec::new());
        Ok(())
    }

    /// Apply the writes of the open batch in one transaction, in the
    /// order they were asked for, and close the batch. When a write
    /// fails, none of them is applied.
    ///
    /// # Errors
    ///
    /// No batch open, or `SQLite` refusing: the file locked by another
    /// program for longer than the connection waits, most often.
    pub fn commit(&self) -> Result<()> {
        let Some(batch) = self.batch.borrow_mut().take() else {
            bail!("{}: no batch of writes is open", self.file());
        };
        let transaction =
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
        for write in &batch {
            self.apply(write)?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Keep the write for the open batch, or apply it at once when there
    /// is none.
    fn write(&self, write: Write) -> Result<()> {
        if let Some(batch) = self.batch.borrow_mut().as_mut() {
            batch.push(write);
            return Ok(());
        }
        self.apply(&write)
    }

    /// Carry out one write on the file.
    fn apply(&self, write: &Write) -> Result<()> {
        let connection = &self.connection;
        match write {
            Write::AdvanceImap(at, resume) => {
                let next = match self.imap_resume(&at.account, &at.folder)? {
                    Some(current) if current.uidvalidity == resume.uidvalidity => ImapResume {
                        uid: current.uid.max(resume.uid),
                        ..*resume
                    },
                    _ => *resume,
                };
                self.set_resume(at, &Resume::Imap(next))?;
            }
            Write::SetResume(at, resume) => self.set_resume(at, resume)?,
            Write::AddPending(at, listed) => {
                let mut statement = connection.prepare_cached(
                    "INSERT INTO pending (account, folder, id, detail) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (account, folder, id) DO UPDATE SET detail = excluded.detail",
                )?;
                for (id, detail) in listed {
                    statement.execute(params![at.account, at.folder, id, detail])?;
                }
            }
            Write::FailPending(at, id) => {
                connection
                    .prepare_cached(
                        "UPDATE pending SET attempts = attempts + 1
                         WHERE account = ?1 AND folder = ?2 AND id = ?3",
                    )?
                    .execute(params![at.account, at.folder, id])?;
            }
            Write::RemovePending(at, id) => {
                connection
                    .prepare_cached(
                        "DELETE FROM pending WHERE account = ?1 AND folder = ?2 AND id = ?3",
                    )?
                    .execute(params![at.account, at.folder, id])?;
            }
            Write::ClearPending(at) => {
                connection
                    .prepare_cached("DELETE FROM pending WHERE account = ?1 AND folder = ?2")?
                    .execute(params![at.account, at.folder])?;
            }
            Write::AddCompared { at, id, subject } => {
                connection
                    .prepare_cached(
                        "INSERT INTO compared (account, folder, id, subject)
                         VALUES (?1, ?2, ?3, ?4)
                         ON CONFLICT (account, folder, id)
                         DO UPDATE SET subject = excluded.subject",
                    )?
                    .execute(params![at.account, at.folder, id, subject])?;
            }
            Write::RemoveCompared(at, ids) => {
                let mut statement = connection.prepare_cached(
                    "DELETE FROM compared WHERE account = ?1 AND folder = ?2 AND id = ?3",
                )?;
                for id in ids {
                    statement.execute(params![at.account, at.folder, id])?;
                }
            }
            Write::MarkImported { store_id, places } => {
                let mut statement = connection.prepare_cached(
                    "INSERT OR IGNORE INTO imported (store_id, place) VALUES (?1, ?2)",
                )?;
                if places.is_empty() {
                    statement.execute(params![store_id, ""])?;
                }
                for place in places {
                    statement.execute(params![store_id, place])?;
                }
            }
            Write::Proof(segment) => {
                connection.execute(
                    "INSERT OR REPLACE INTO archive (id, segment) VALUES (0, ?1)",
                    params![segment],
                )?;
            }
        }
        Ok(())
    }

    /// Give the space of deleted rows back to the file system, when
    /// there is [`COMPACT_FROM`] or more of it. The pending list of a
    /// large folder is written and deleted again within one run, and
    /// the file would otherwise keep that size. Applied at once, not
    /// as part of a batch. Returns whether the file was rewritten.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn compact(&self) -> Result<bool> {
        let pragma = |name: &str| -> Result<i64> {
            Ok(self
                .connection
                .query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))?)
        };
        if pragma("freelist_count")? * pragma("page_size")? < COMPACT_FROM {
            return Ok(false);
        }
        self.connection.execute_batch("VACUUM")?;
        Ok(true)
    }

    /// The folder's resume point: `None` for a folder never fetched.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing, or a state column that is not the JSON this
    /// program writes.
    pub fn resume(&self, account: &str, folder: &str) -> Result<Option<Resume>> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT state FROM resume WHERE account = ?1 AND folder = ?2")?;
        let state: Option<String> = statement
            .query_row(params![account, folder], |row| row.get(0))
            .optional()?;
        state
            .map(|state| {
                serde_json::from_str(&state).with_context(|| {
                    format!(
                        "{account}:{folder}: the resume point in {} is not readable; delete the file",
                        self.file()
                    )
                })
            })
            .transpose()
    }

    fn set_resume(&self, at: &At, state: &Resume) -> Result<()> {
        let mut statement = self.connection.prepare_cached(
            "INSERT INTO resume (account, folder, state) VALUES (?1, ?2, ?3)
             ON CONFLICT (account, folder) DO UPDATE SET state = excluded.state",
        )?;
        statement.execute(params![
            at.account,
            at.folder,
            serde_json::to_string(state)?
        ])?;
        Ok(())
    }

    /// Where an IMAP folder's fetch carries on: `None` for a folder
    /// never fetched, and for one last fetched over Graph.
    ///
    /// # Errors
    ///
    /// As [`resume`](Self::resume).
    pub fn imap_resume(&self, account: &str, folder: &str) -> Result<Option<ImapResume>> {
        Ok(match self.resume(account, folder)? {
            Some(Resume::Imap(point)) => Some(point),
            _ => None,
        })
    }

    /// A message of the IMAP folder is in: the fetch carries on above
    /// it. Under the same UIDVALIDITY the point only ever moves up,
    /// because the server returns a batch in its own order and the
    /// highest UID seen is the one to carry on from. A new UIDVALIDITY
    /// starts over.
    ///
    /// The point it is compared with is read when the write is applied:
    /// in a batch that is at the commit, after the batch's earlier
    /// writes.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn advance_imap(&self, account: &str, folder: &str, resume: ImapResume) -> Result<()> {
        self.write(Write::AdvanceImap(At::new(account, folder), resume))
    }

    /// Where an MS Graph folder's listings carry on: `None` for a
    /// folder never listed, and for one last fetched over IMAP.
    ///
    /// # Errors
    ///
    /// As [`resume`](Self::resume).
    pub fn graph_resume(&self, account: &str, folder: &str) -> Result<Option<GraphResume>> {
        Ok(match self.resume(account, folder)? {
            Some(Resume::Graph(point)) => Some(point),
            _ => None,
        })
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
        self.write(Write::SetResume(
            At::new(account, folder),
            Resume::Graph(resume),
        ))
    }

    /// The messages of the folder an earlier listing reported that are
    /// not stored yet, each with its detail read as `T`. The order is
    /// the listing's as far as `SQLite` keeps it, which a compaction
    /// need not; nothing depends on it.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing, or a detail column that is not the JSON this
    /// program writes.
    pub fn pending<T: DeserializeOwned>(
        &self,
        account: &str,
        folder: &str,
    ) -> Result<Vec<Pending<T>>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT id, detail, attempts FROM pending WHERE account = ?1 AND folder = ?2
             ORDER BY rowid",
        )?;
        let rows = statement.query_map(params![account, folder], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u32>(2)?,
            ))
        })?;
        let mut pending = Vec::new();
        for row in rows {
            let (id, detail, attempts) = row?;
            let detail = serde_json::from_str(&detail).with_context(|| {
                format!(
                    "pending message {id}: unreadable entry in {}; delete the file",
                    self.file()
                )
            })?;
            pending.push(Pending {
                id,
                detail,
                attempts,
            });
        }
        Ok(pending)
    }

    /// Put the messages a listing reported on the folder's pending
    /// list, each with its detail written as JSON. A message already on
    /// the list gets the detail of the newer listing and keeps its
    /// attempts.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn add_pending<'a, T: Serialize>(
        &self,
        account: &str,
        folder: &str,
        listed: impl IntoIterator<Item = (&'a str, T)>,
    ) -> Result<()> {
        let listed = listed
            .into_iter()
            .map(|(id, detail)| Ok((id.to_string(), serde_json::to_string(&detail)?)))
            .collect::<Result<Vec<_>>>()?;
        self.write(Write::AddPending(At::new(account, folder), listed))
    }

    /// One more run failed to fetch the message.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn fail_pending(&self, account: &str, folder: &str, id: &str) -> Result<()> {
        self.write(Write::FailPending(At::new(account, folder), id.to_string()))
    }

    /// The message is stored, the server no longer has it, or it is
    /// given up: take it off the folder's pending list.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn remove_pending(&self, account: &str, folder: &str, id: &str) -> Result<()> {
        self.write(Write::RemovePending(
            At::new(account, folder),
            id.to_string(),
        ))
    }

    /// Empty the folder's pending list: a listing from scratch reports
    /// the folder whole and replaces whatever an earlier one left.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn clear_pending(&self, account: &str, folder: &str) -> Result<()> {
        self.write(Write::ClearPending(At::new(account, folder)))
    }

    /// The messages `fetch --compare` fetched from the folder: the
    /// subject of each, by server id.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn compared(&self, account: &str, folder: &str) -> Result<HashMap<String, String>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT id, subject FROM compared WHERE account = ?1 AND folder = ?2",
        )?;
        let rows = statement.query_map(params![account, folder], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Remember that the message fetched under the server's `id` is
    /// `subject`.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn add_compared(&self, account: &str, folder: &str, id: &str, subject: &str) -> Result<()> {
        self.write(Write::AddCompared {
            at: At::new(account, folder),
            id: id.to_string(),
            subject: subject.to_string(),
        })
    }

    /// Forget these messages of the folder.
    ///
    /// # Errors
    ///
    /// `SQLite` refusing.
    pub fn remove_compared<'a>(
        &self,
        account: &str,
        folder: &str,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<()> {
        let ids = ids.into_iter().map(ToString::to_string).collect();
        self.write(Write::RemoveCompared(At::new(account, folder), ids))
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
        self.write(Write::MarkImported {
            store_id: store_id.to_string(),
            places: places.iter().map(|place| (*place).clone()).collect(),
        })
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
    fn a_memo_outside_the_archive_records_the_segment_sealed_last() {
        use ossuary_core::{Algorithm, Archive, Attribute, Claim, Run, Source, Subject, Timestamp};

        let dir = tempfile::tempdir().unwrap();
        let archive = Archive::create(dir.path().join("archive"), Algorithm::Sha256)
            .unwrap()
            .with_cache(dir.path().join("elsewhere"));
        let memo = Memo::open(&archive.cache_dir().unwrap().join(FILE_NAME)).unwrap();
        let accepted = |memo: &Memo| {
            archive
                .accepts_cache(memo.proof().unwrap().as_deref())
                .unwrap()
        };
        memo.begin().unwrap();
        memo.advance_imap("john", "INBOX", at(1, 7)).unwrap();
        memo.commit_for(&archive).unwrap();
        assert_eq!(memo.proof().unwrap(), None, "nothing is sealed yet");
        assert!(!accepted(&memo));

        let claim = Claim::assert(
            Subject::parse(&"ab".repeat(32)).unwrap(),
            Attribute::parse("user:tag").unwrap(),
            serde_json::json!("x"),
            Timestamp::now(),
            Source::parse("user").unwrap(),
            Run::new(),
        )
        .unwrap();
        archive.log().append(&claim).unwrap();
        let sealed = archive.log().seal().unwrap().unwrap();
        memo.begin().unwrap();
        memo.commit_for(&archive).unwrap();
        assert_eq!(
            memo.proof().unwrap().as_deref(),
            Some(sealed.digest().as_str())
        );
        assert!(accepted(&memo));

        assert!(!memo.is_empty().unwrap());
        assert!(memo.clear().unwrap(), "it held a resume point");
        assert_eq!(memo.imap_resume("john", "INBOX").unwrap(), None);
        assert_eq!(memo.proof().unwrap(), None);
        assert!(memo.is_empty().unwrap());
        assert!(!memo.clear().unwrap(), "and holds nothing now");
    }

    #[test]
    fn a_batch_is_applied_at_the_commit_in_the_order_of_its_writes() {
        let (_dir, memo) = memo();
        memo.begin().unwrap();
        memo.advance_imap("john", "INBOX", at(1, 5)).unwrap();
        memo.advance_imap("john", "INBOX", at(1, 9)).unwrap();
        memo.advance_imap("john", "INBOX", at(1, 3)).unwrap();
        memo.add_pending("john", "INBOX", [("4", ()), ("6", ())])
            .unwrap();
        memo.fail_pending("john", "INBOX", "4").unwrap();
        memo.remove_pending("john", "INBOX", "6").unwrap();
        assert_eq!(
            memo.imap_resume("john", "INBOX").unwrap(),
            None,
            "a read does not see the open batch"
        );

        memo.commit().unwrap();

        assert_eq!(memo.imap_resume("john", "INBOX").unwrap(), Some(at(1, 9)));
        let pending: Vec<(String, u32)> = memo
            .pending::<()>("john", "INBOX")
            .unwrap()
            .into_iter()
            .map(|message| (message.id, message.attempts))
            .collect();
        assert_eq!(pending, [("4".to_string(), 1)]);
    }

    #[test]
    fn a_second_memo_on_the_file_commits_while_a_batch_is_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let one = Memo::open(&path).unwrap();
        let two = Memo::open(&path).unwrap();
        one.begin().unwrap();
        one.advance_imap("john", "INBOX", at(1, 7)).unwrap();

        two.begin().unwrap();
        two.advance_imap("john", "Sent", at(2, 3)).unwrap();
        two.commit().unwrap();
        one.commit().unwrap();

        assert_eq!(two.imap_resume("john", "INBOX").unwrap(), Some(at(1, 7)));
        assert_eq!(one.imap_resume("john", "Sent").unwrap(), Some(at(2, 3)));
    }

    #[test]
    fn a_batch_is_opened_once_and_committed_once() {
        let (_dir, memo) = memo();
        assert!(memo.commit().is_err(), "no batch is open");
        memo.begin().unwrap();
        assert!(memo.begin().is_err(), "one is open already");
        memo.commit().unwrap();
        assert!(memo.commit().is_err());
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
        assert!(matches!(
            memo.resume("a", "Inbox").unwrap(),
            Some(Resume::Graph(_))
        ));
        memo.advance_imap("a", "Inbox", at(7, 40)).unwrap();
        assert_eq!(memo.graph_resume("a", "Inbox").unwrap(), None);
        assert_eq!(memo.imap_resume("a", "Inbox").unwrap(), Some(at(7, 40)));
    }

    #[test]
    fn a_resume_point_that_is_not_json_is_an_error_not_a_first_fetch() {
        let (dir, memo) = memo();
        memo.connection
            .execute("INSERT INTO resume VALUES ('a', 'Inbox', 'not json')", [])
            .unwrap();
        let error = memo.imap_resume("a", "Inbox").unwrap_err();
        let path = dir.path().join("cache").join(FILE_NAME);
        assert!(
            format!("{error:#}").contains(&format!(
                "a:Inbox: the resume point in {} is not readable; delete the file",
                path.display()
            )),
            "{error:#}"
        );
    }

    /// A pending entry as the memo hands it back.
    fn entry(id: &str, tags: &[&str], attempts: u32) -> Pending<Vec<String>> {
        Pending {
            id: id.to_string(),
            detail: tags.iter().map(ToString::to_string).collect(),
            attempts,
        }
    }

    /// A message as a listing reports it, for `add_pending`.
    fn listed(id: &'static str, tags: &[&str]) -> (&'static str, Vec<String>) {
        (id, tags.iter().map(ToString::to_string).collect())
    }

    #[test]
    fn the_pending_list_is_kept_per_folder() {
        let (_dir, memo) = memo();
        let pending = |folder: &str| memo.pending::<Vec<String>>("a", folder).unwrap();
        assert!(pending("Inbox").is_empty());

        memo.add_pending("a", "Inbox", [listed("M2", &["Red"]), listed("M1", &[])])
            .unwrap();
        memo.add_pending("a", "Sent", [listed("S1", &[])]).unwrap();
        assert_eq!(
            pending("Inbox"),
            [entry("M2", &["Red"], 0), entry("M1", &[], 0)]
        );

        memo.fail_pending("a", "Inbox", "M2").unwrap();
        memo.add_pending("a", "Inbox", [listed("M2", &["Blue"])])
            .unwrap();
        assert_eq!(
            pending("Inbox"),
            [entry("M2", &["Blue"], 1), entry("M1", &[], 0)],
            "a newer listing replaces the detail and keeps the attempts"
        );

        memo.remove_pending("a", "Inbox", "M2").unwrap();
        assert_eq!(pending("Inbox"), [entry("M1", &[], 0)]);

        memo.clear_pending("a", "Inbox").unwrap();
        assert!(pending("Inbox").is_empty());
        assert_eq!(
            pending("Sent"),
            [entry("S1", &[], 0)],
            "another folder's list is untouched"
        );
    }

    #[test]
    fn the_file_is_compacted_once_the_deleted_rows_amount_to_something() {
        let (dir, memo) = memo();
        let path = dir.path().join("cache").join(FILE_NAME);
        assert!(!memo.compact().unwrap(), "nothing deleted, nothing to do");

        let detail = "x".repeat(200);
        let ids: Vec<String> = (0..10_000).map(|n| format!("M{n}")).collect();
        memo.begin().unwrap();
        memo.add_pending("a", "Inbox", ids.iter().map(|id| (id.as_str(), &detail)))
            .unwrap();
        memo.commit().unwrap();
        let full = fs::metadata(&path).unwrap().len();
        memo.clear_pending("a", "Inbox").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().len(),
            full,
            "deleted, still that size"
        );

        assert!(memo.compact().unwrap());
        assert!(fs::metadata(&path).unwrap().len() < full / 4);
        assert!(
            !memo.compact().unwrap(),
            "and now there is nothing to give back"
        );
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
    fn what_compare_fetched_is_remembered_per_folder() {
        let (_dir, memo) = memo();
        assert!(memo.compared("a", "Inbox").unwrap().is_empty());

        memo.add_compared("a", "Inbox", "M1", "s1").unwrap();
        memo.add_compared("a", "Inbox", "M2", "s2").unwrap();
        memo.add_compared("a", "Inbox", "M1", "s3").unwrap();
        memo.add_compared("a", "Sent", "M1", "s1").unwrap();
        let known = |folder: &str| {
            let mut rows: Vec<_> = memo.compared("a", folder).unwrap().into_iter().collect();
            rows.sort();
            rows
        };
        let row = |id: &str, subject: &str| (id.to_string(), subject.to_string());
        assert_eq!(
            known("Inbox"),
            [row("M1", "s3"), row("M2", "s2")],
            "fetched again, the newer subject counts"
        );

        memo.remove_compared("a", "Inbox", ["M1", "M9"]).unwrap();
        assert_eq!(known("Inbox"), [row("M2", "s2")]);
        assert_eq!(
            known("Sent"),
            [row("M1", "s1")],
            "another folder's rows are untouched"
        );
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
