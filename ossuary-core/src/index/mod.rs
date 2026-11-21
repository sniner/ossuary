//! The index: a fold over the log, and never the truth.
//!
//! Query answering is a fold of the claim log into `SQLite`. The index is
//! rebuilt from the log at any time and lost without loss — delete the file
//! and fold again — which is exactly why it may live outside the archive's
//! promises, in `cache/`, with whatever schema today's questions want.
//! Nothing here is authoritative; the segments are.
//!
//! The fold is incremental, and the log's own shape is what makes that
//! cheap: a sealed segment never changes, so a segment folded once is
//! folded forever, and a new one only ever joins the chain at its end.
//! The head only grows until it is sealed: it is a directory of bundles,
//! each written once. The index keeps the sealed segment the head
//! followed when the index last read it (`open_read`), and the bundles it
//! has read (`bundles`). A fold lists the head and reads only the bundles
//! it has not read; when there are none and no seal was made since, it
//! reads and writes nothing. When the head follows another segment, the
//! fold walks back to the one it knows and folds the segments passed on
//! the way, oldest first. A claim of theirs that the index already holds,
//! read from the head before the seal, only gets its new segment and
//! position. Then the bundles of the new head are read. The fold reads
//! the log in its order, and what it arrives at does not depend on that
//! order: a retraction names its claims by id, and marks them whether it
//! is read before or after them. An index built anew therefore answers
//! as one that was folded on.
//!
//! The fold keeps one table of substance and five of names. `claims` has
//! every claim as a row, retractions included, and a retraction marks
//! the claims it names as `retracted`. The standing claims are the
//! assertions without that mark, `retract = 0 AND retracted = 0`. They
//! are a set: a value recorded twice is two claims in it, and none of
//! them is preferred. [`about`](Index::about) reads every row,
//! [`find`](Index::find) the standing ones unless it is asked for the
//! record. There is no table of the standing values beside `claims`: it
//! would repeat nearly every claim.
//!
//! With the cache on a network share every page read is a round trip,
//! so the indexes are made for `find` to read few pages. `claims_value`
//! has every column a term on an attribute compares. A term on run,
//! source or time has an index of its own, and beside another term it is
//! read from that index as a set of claim ids, not from the table rows.
//!
//! Subjects, attributes, sources, runs and segments stand in tables of
//! their own and appear in `claims` as integer ids: a digest is 64 bytes
//! and a segment name the same, and either repeated a quarter of a
//! million times is most of a file. Four views are for a look with
//! `sqlite3`, and nothing here reads them: `v_claims` shows the claims
//! with the names in place of the ids, `v_standing` the standing values,
//! `v_places` every standing `file:path` unquoted, `v_runs` what each
//! run wrote. Which of several standing values a reader prefers is the
//! reader's policy at query time.
//!
//! The index follows the log forward, the only direction a log moves. A
//! log changed in any other way is found where it shows: claims in the
//! index but no head in the log, claims but no record of what was read
//! of the head, a chain that does not lead back to the segment the index
//! followed. The index is then emptied and folded anew, which is what
//! deleting the cache does. So is an index that has read a bundle the
//! open segment no longer holds while it follows the same segment: the
//! index of another copy of the archive, or a log set back.
//!
//! The code is in five files: `schema` has the tables, `fold` reads the
//! log into them and is the only one that knows the log, `as_of` replays
//! the record up to an earlier moment, `query` and `find` answer from
//! the tables. The types and opening the file are here.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, params};

use self::schema::{HEAD, HEAD_SEQ, SCHEMA, ddl};
use crate::claim::{Attribute, ClaimId, Run, Source, Subject, Value};
use crate::error::{Error, Result};
use crate::part::Part;
use crate::vocabulary::{FILE_NAME, FILE_PATH, MAILBOX_PLACE};

mod as_of;
mod find;
mod fold;
mod query;
mod schema;
#[cfg(test)]
mod tests;

/// What one fold did: how much was new.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Folded {
    /// Sealed segments that were new to the index.
    pub segments: usize,
    /// Claims those segments brought that the index did not hold yet
    /// as claims of the open head.
    pub claims: usize,
    /// Claims read from the open head, appended since the last fold.
    pub head: usize,
    /// Whether the index did not match the log and was emptied and
    /// folded anew; the counts are then those of the whole log.
    pub rebuilt: bool,
}

/// One standing claim: an assertion that no retraction names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Standing {
    /// The file the claim is about.
    pub subject: Subject,
    /// The value it asserts.
    pub value: Value,
    /// When it was made, in claim time's own spelling.
    pub asserted: String,
    /// Its id. `(asserted, claim)` is an order of the claims that
    /// follows from the log alone, whatever order an index read it in.
    pub claim: ClaimId,
}

/// What a question reads: the present, the standing set whole, or the
/// record.
///
/// The standing set is the claims that no retraction names. A file is
/// *placed* while a place stands on it — a
/// `file:path` from a walk, a `mailbox:place` from a fetch — or, for
/// what a tool won out of another file, while its origin is placed,
/// along `prov:origin` as far as it goes. A `file:path` led by `@`
/// names a place inside another content and places nothing by itself:
/// an unpacked entry is present through its archive. A file whose every
/// place was taken back is still held, and still answers `--as-of` a
/// day it lay somewhere, but it is not part of the present. The record
/// is the history itself: every claim ever written, retractions
/// included, so what was said and since taken back answers too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The standing set, files at a place of their own or won out of
    /// one that is.
    Present,
    /// The standing set, every file the archive holds.
    Held,
    /// Every claim on the record, standing or not.
    Record,
}

/// A field of the claim, named in a term without a colon — the way an
/// attribute is named with one. What a claim is made of, as
/// `docs/format.md` lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// The file the claim is about.
    Subject,
    /// What the claim says something about.
    Attribute,
    /// What it says, in the log's own JSON spelling.
    Value,
    /// When it was written.
    Time,
    /// Who wrote it.
    Source,
    /// In which call it was written.
    Run,
    /// Whether it takes back rather than asserts: `true` or `false`.
    Retract,
}

impl Field {
    /// The field a term names, by its spelling in the format document.
    ///
    /// # Errors
    ///
    /// [`Error::Field`] for any other word.
    pub fn parse(name: &str) -> Result<Self> {
        Ok(match name {
            "subject" => Field::Subject,
            "attribute" => Field::Attribute,
            "value" => Field::Value,
            "time" => Field::Time,
            "source" => Field::Source,
            "run" => Field::Run,
            "retract" => Field::Retract,
            other => return Err(Error::Field(other.to_string())),
        })
    }

    /// The field's name, as the format document spells it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Field::Subject => "subject",
            Field::Attribute => "attribute",
            Field::Value => "value",
            Field::Time => "time",
            Field::Source => "source",
            Field::Run => "run",
            Field::Retract => "retract",
        }
    }
}

/// One narrowing term of a [`find`](Index::find): what is asked of, and
/// the pattern asked. An attribute term speaks about a standing value or
/// a claim's value; a field term about the claim that carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    /// `attribute=pattern`.
    Attribute(Attribute, String),
    /// `attribute.path=pattern`: a part of a value. All part terms on
    /// the same attribute must match the same value.
    Part(Part, String),
    /// `field=pattern`.
    Field(Field, String),
}

/// Where a file stood, as far as the record tells: a full path some
/// sighting recorded, a mailbox folder a message was fetched from, or a
/// bare name (a derived file that never sat anywhere).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Placement {
    /// A recorded `file:path` — absolute, in its host's own spelling,
    /// or led by `@`: the path an unpacked entry had inside its
    /// archive, which lays out like any other path with the `@` taken
    /// off.
    Path(String),
    /// A recorded `file:name` — all a derived file without a path has.
    Name(String),
    /// A recorded `mailbox:place`, `account:folder`. A message has no
    /// file name of its own, so it is exported as `<digest>.eml`.
    Mailbox(String),
}

/// The attributes that record where a file stood, in the order a file
/// answers with them: all values of the first kind it has.
const PLACES: [&str; 3] = [FILE_PATH, MAILBOX_PLACE, FILE_NAME];

impl Placement {
    /// A value of the attribute at `rank` in [`PLACES`].
    fn ranked(rank: usize, spelled: String) -> Placement {
        match rank {
            0 => Placement::Path(spelled),
            1 => Placement::Mailbox(spelled),
            _ => Placement::Name(spelled),
        }
    }
}

/// One run as the record tells it: what one call wrote, from its first
/// claim to its last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Episode {
    /// The run's id.
    pub run: Run,
    /// The moment of its first claim, in claim time's own spelling.
    pub first: String,
    /// The moment of its last claim.
    pub last: String,
    /// Who spoke in it, sorted — an ingest with tags speaks as `ingest`
    /// and `user`, a bare `extract` as every extractor that ran.
    pub sources: Vec<Source>,
    /// Files it wrote something about.
    pub files: u64,
    /// Claims it wrote.
    pub claims: u64,
    /// Of them, retractions.
    pub retractions: u64,
}

/// A disposable query index over a claim log.
#[derive(Debug)]
pub struct Index {
    connection: Connection,
    ids: Ids,
    /// Whether the file is known to belong to the log it is folded
    /// from: it lies in the archive's `cache/`, or a fold of this
    /// instance found that it does. See [`open_outside`](Index::open_outside).
    proved: bool,
}

/// The name tables, as far as this instance has met them: a digest, an
/// attribute or a source seen once is looked up once. Filled lazily by
/// the fold, so a CLI call that folds nothing new pays for nothing.
#[derive(Debug, Default)]
struct Ids {
    subjects: HashMap<String, i64>,
    attributes: HashMap<String, i64>,
    sources: HashMap<String, i64>,
    runs: HashMap<String, i64>,
}

impl Ids {
    fn subject(&mut self, connection: &Connection, digest: &str) -> Result<i64> {
        intern(connection, &mut self.subjects, "subjects", "digest", digest)
    }

    fn attribute(&mut self, connection: &Connection, name: &str) -> Result<i64> {
        intern(connection, &mut self.attributes, "attributes", "name", name)
    }

    fn source(&mut self, connection: &Connection, name: &str) -> Result<i64> {
        intern(connection, &mut self.sources, "sources", "name", name)
    }

    fn run(&mut self, connection: &Connection, name: &str) -> Result<i64> {
        intern(connection, &mut self.runs, "runs", "name", name)
    }
}

/// The id of `name` in `table`, given one if it has none yet, and
/// remembered.
fn intern(
    connection: &Connection,
    cache: &mut HashMap<String, i64>,
    table: &str,
    column: &str,
    name: &str,
) -> Result<i64> {
    if let Some(&id) = cache.get(name) {
        return Ok(id);
    }
    let mut find =
        connection.prepare_cached(&format!("SELECT id FROM {table} WHERE {column} = ?1"))?;
    let id = match find.query_row(params![name], |row| row.get::<_, i64>(0)) {
        Ok(id) => id,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            let mut add = connection
                .prepare_cached(&format!("INSERT INTO {table} ({column}) VALUES (?1)"))?;
            add.execute(params![name])?;
            connection.last_insert_rowid()
        }
        Err(error) => return Err(error.into()),
    };
    cache.insert(name.to_string(), id);
    Ok(id)
}

impl Index {
    /// Open the index at `path`, creating file and schema as needed.
    ///
    /// The path belongs in `cache/`: everything here is derived, and
    /// deleting it loses nothing that [`fold`](Index::fold) does not
    /// restore.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] when `SQLite` cannot open or prepare it.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let connection = Connection::open(path)?;
        // A mount and a CLI call may hold the same file; a fold on one
        // side makes the other wait a moment instead of failing at once.
        connection.busy_timeout(Duration::from_secs(5))?;
        // A cache may lose its last writes to a crash and be none the
        // worse: the next fold restores them.
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        // The cache's own generation. A file from an older schema is not
        // migrated but emptied — it is a cache, and the next fold rebuilds
        // it from the log for the cost of one slow first answer.
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        // One transaction for the whole schema. Without it every statement
        // commits on its own, and a new file costs a flush of the device
        // per table, index and view.
        let schema = connection.unchecked_transaction()?;
        if version != SCHEMA {
            connection.execute_batch(&format!(
                "DROP VIEW IF EXISTS v_claims;
                 DROP VIEW IF EXISTS v_standing;
                 DROP VIEW IF EXISTS v_places;
                 DROP VIEW IF EXISTS v_runs;
                 DROP TABLE IF EXISTS retractions;
                 DROP TABLE IF EXISTS claims;
                 DROP TABLE IF EXISTS segments;
                 DROP TABLE IF EXISTS open_read;
                 DROP TABLE IF EXISTS bundles;
                 DROP TABLE IF EXISTS subjects;
                 DROP TABLE IF EXISTS attributes;
                 DROP TABLE IF EXISTS sources;
                 DROP TABLE IF EXISTS runs;
                 PRAGMA user_version = {SCHEMA};"
            ))?;
        }
        // The integer columns of `claims` are ids into the five name
        // tables. None of them is declared a foreign key. `open_read` is a single row, the
        // segment the open segment followed when it was last read;
        // `bundles` the bundles of it read so far (see the module notes).
        connection.execute_batch(&ddl())?;
        // Asked first: an insert that is ignored still needs write access,
        // and a current index on a read-only archive is to be read.
        let head_row: bool = connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM segments WHERE id = ?1)",
            params![HEAD],
            |row| row.get(0),
        )?;
        if !head_row {
            connection.execute(
                "INSERT INTO segments (id, digest, seq) VALUES (?1, 'head', ?2)",
                params![HEAD, HEAD_SEQ],
            )?;
        }
        schema.commit()?;
        Ok(Index {
            connection,
            ids: Ids::default(),
            proved: true,
        })
    }

    /// Open an index kept outside the archive it is folded from.
    ///
    /// Such a file may be another archive's. A fold tells by the sealed
    /// segment the index followed: the chain of this log must lead back
    /// to it. An index that followed none has nothing to tell by, and
    /// the first fold of this instance empties it and folds the log
    /// anew. That is the open segment at most, since a log with a sealed
    /// segment gives the index one to follow.
    ///
    /// # Errors
    ///
    /// As [`open`](Index::open).
    pub fn open_outside(path: impl AsRef<Path>) -> Result<Self> {
        let mut index = Index::open(path)?;
        index.proved = false;
        Ok(index)
    }
}
