//! The index: a fold over the log, and never the truth.
//!
//! Query answering is a fold of the claim log into `SQLite`. The index is
//! rebuilt from the log at any time and lost without loss — delete the file
//! and fold again — which is exactly why it may live outside the archive's
//! promises, in `cache/`, with whatever schema today's questions want.
//! Nothing here is authoritative; the segments are.
//!
//! The fold is incremental, and the log's own immutability is what makes
//! that cheap: a sealed segment never changes, so a segment folded once is
//! folded forever, and "what is new" is the set difference of digests. Only
//! the open head is folded afresh each time, because only the head moves.
//!
//! The fold keeps two tables of substance and four of names. `claims` is
//! the history, verbatim: every claim a row, retractions included, in
//! fold order — what [`about`](Index::about) reads. `standing` is the
//! folded answer: one row per standing (subject, attribute, value), the
//! set semantics as a primary key — an assertion is an upsert that
//! renews the row's moment, a retraction a `DELETE` — and what
//! [`find`](Index::find) reads. Subjects, attributes, sources, runs and
//! segments stand in tables of their own and appear in the two big
//! tables as integer ids: a digest is 64 bytes and a segment name the
//! same, and either repeated a quarter of a million times is most of a
//! file. Four views are for a look with `sqlite3`, and nothing here
//! reads them: `v_claims` and `v_standing` show both tables with the
//! names in place of the ids, `v_places` every standing `file:path`
//! unquoted, `v_runs` what each run wrote. What stays deliberately
//! un-baked is *narrowing*: which of several standing values a reader
//! prefers is query-time policy, and the sets carry them all.
//!
//! Standing follows the log forward, the only direction a log moves; a
//! `head.jsonl` edited backwards leaves it stale until the cache is
//! deleted and refolded — the cure every cache here has.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension as _, params};

use crate::claim::{Attribute, Claim, Run, Source, Subject, Timestamp, Value};
use crate::error::{Error, Result};
use crate::export::Placement;
use crate::log::Log;

/// The cache schema's generation, kept in `PRAGMA user_version`: a file
/// carrying any other number is emptied instead of half-understood.
const SCHEMA: i64 = 1;

/// The open head's row in `segments`: id 0, named `head`, ranked past
/// every sealed segment so it sorts last wherever log order is asked.
const HEAD: i64 = 0;
const HEAD_SEQ: i64 = i64::MAX;

/// The tables, indexes and views, created where missing.
const DDL: &str = "CREATE TABLE IF NOT EXISTS subjects (
         id     INTEGER PRIMARY KEY,
         digest TEXT NOT NULL UNIQUE
     );
     CREATE TABLE IF NOT EXISTS attributes (
         id   INTEGER PRIMARY KEY,
         name TEXT NOT NULL UNIQUE
     );
     CREATE TABLE IF NOT EXISTS sources (
         id   INTEGER PRIMARY KEY,
         name TEXT NOT NULL UNIQUE
     );
     CREATE TABLE IF NOT EXISTS runs (
         id   INTEGER PRIMARY KEY,
         name TEXT NOT NULL UNIQUE
     );
     CREATE TABLE IF NOT EXISTS segments (
         id     INTEGER PRIMARY KEY,
         digest TEXT NOT NULL UNIQUE,
         first  TEXT,
         seq    INTEGER NOT NULL
     );
     CREATE TABLE IF NOT EXISTS claims (
         id        INTEGER PRIMARY KEY,
         subject   INTEGER NOT NULL,
         attribute INTEGER NOT NULL,
         value     TEXT,
         time      TEXT NOT NULL,
         source    INTEGER NOT NULL,
         run       INTEGER,
         retract   INTEGER NOT NULL DEFAULT 0,
         segment   INTEGER NOT NULL,
         position  INTEGER NOT NULL
     );
     CREATE INDEX IF NOT EXISTS claims_subject
         ON claims (subject, attribute);
     CREATE INDEX IF NOT EXISTS claims_attribute
         ON claims (attribute, time);
     CREATE INDEX IF NOT EXISTS claims_segment
         ON claims (segment, position);
     CREATE INDEX IF NOT EXISTS claims_run
         ON claims (run);
     CREATE TABLE IF NOT EXISTS standing (
         subject   INTEGER NOT NULL,
         attribute INTEGER NOT NULL,
         value     TEXT NOT NULL,
         time      TEXT NOT NULL,
         claim     INTEGER NOT NULL,
         PRIMARY KEY (subject, attribute, value)
     ) WITHOUT ROWID;
     CREATE INDEX IF NOT EXISTS standing_lookup
         ON standing (attribute, value);
     CREATE VIEW IF NOT EXISTS v_claims AS
         SELECT c.id, su.digest AS subject, a.name AS attribute, c.value, c.time,
                so.name AS source, r.name AS run, c.retract, s.digest AS segment, s.seq, c.position
         FROM claims c
         JOIN subjects su ON su.id = c.subject
         JOIN attributes a ON a.id = c.attribute
         JOIN sources so ON so.id = c.source
         LEFT JOIN runs r ON r.id = c.run
         JOIN segments s ON s.id = c.segment;
     CREATE VIEW IF NOT EXISTS v_standing AS
         SELECT su.digest AS subject, a.name AS attribute, st.value, st.time, st.claim
         FROM standing st
         JOIN subjects su ON su.id = st.subject
         JOIN attributes a ON a.id = st.attribute;
     CREATE VIEW IF NOT EXISTS v_places AS
         SELECT su.digest AS subject, json_extract(st.value, '$') AS path, st.time
         FROM standing st
         JOIN subjects su ON su.id = st.subject
         WHERE st.attribute = (SELECT id FROM attributes WHERE name = 'file:path')
           AND json_type(st.value) = 'text';
     CREATE VIEW IF NOT EXISTS v_runs AS
         SELECT r.name AS run, MIN(c.time) AS first, MAX(c.time) AS last,
                COUNT(DISTINCT c.subject) AS files, COUNT(*) AS claims,
                SUM(c.retract) AS retractions
         FROM claims c
         JOIN runs r ON r.id = c.run
         GROUP BY c.run;";

/// What one fold did: how much was new.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Folded {
    /// Sealed segments that were new to the index.
    pub segments: usize,
    /// Claims those segments brought.
    pub claims: usize,
    /// Claims in the open head, folded afresh.
    pub head: usize,
}

/// One value standing at a closing time: whose record it is on, what
/// stands, when its newest surviving assertion was made, and that
/// assertion's place in fold order — the tie-breaker claim time cannot
/// be, since many claims share a second.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Standing {
    /// The file the value stands on.
    pub subject: Subject,
    /// The value itself.
    pub value: Value,
    /// When the newest assertion still standing was made, in claim
    /// time's own spelling.
    pub asserted: String,
    /// That assertion's row in the index, which counts up in fold
    /// order: larger is later in the log.
    pub order: u64,
}

/// One row of `segments`: id, digest, first claim's time, rank.
type SegmentRow = (i64, String, Option<String>, i64);

/// What a question reads: the present, the standing set whole, or the
/// record.
///
/// The standing set is the outcome — retractions applied, repeats
/// collapsed. A file is *placed* while a place stands on it — a
/// `file:path` from a walk, a `mailbox:place` from a fetch — or, for
/// what a tool won out of another file, while its origin is placed,
/// along `derive:derived-from` as far as it goes. A file whose every
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
    /// `field=pattern`.
    Field(Field, String),
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

/// The recursive table of placed subjects, for a query to open with:
/// every subject a place stands on, and every subject derived from one
/// of those.
///
/// The step walks from a placed subject to what was derived from it:
/// its digest, spelled as the JSON string a standing value is, looked
/// up under `derive:derived-from` through `standing_lookup`. Joining the
/// other way round, on the value unquoted, has no index to use and reads
/// every derivation once per placed subject.
const PLACED: &str = "WITH RECURSIVE placed(subject) AS (
        SELECT subject FROM standing
         WHERE attribute IN (SELECT id FROM attributes WHERE name IN ('file:path', 'mailbox:place'))
        UNION
        SELECT st.subject FROM placed
          JOIN subjects su ON su.id = placed.subject
          JOIN standing st ON st.attribute = (SELECT id FROM attributes WHERE name = 'derive:derived-from')
                          AND st.value = json_quote(su.digest)
    ) ";

/// A disposable query index over a claim log.
#[derive(Debug)]
pub struct Index {
    connection: Connection,
    ids: Ids,
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
        if version != SCHEMA {
            connection.execute_batch(&format!(
                "DROP VIEW IF EXISTS v_claims;
                 DROP VIEW IF EXISTS v_standing;
                 DROP VIEW IF EXISTS v_places;
                 DROP VIEW IF EXISTS v_runs;
                 DROP TABLE IF EXISTS standing;
                 DROP TABLE IF EXISTS claims;
                 DROP TABLE IF EXISTS segments;
                 DROP TABLE IF EXISTS subjects;
                 DROP TABLE IF EXISTS attributes;
                 DROP TABLE IF EXISTS sources;
                 DROP TABLE IF EXISTS runs;
                 PRAGMA user_version = {SCHEMA};"
            ))?;
        }
        // The integer columns of `claims` and `standing` are ids into the
        // five name tables; `standing.claim` is a row of `claims`. None of
        // it is declared a foreign key on purpose: the head's claim rows
        // are deleted and rewritten each fold while standing rows still
        // point at the old ones, until the same assertion, folded again,
        // points them at the new.
        connection.execute_batch(DDL)?;
        connection.execute(
            "INSERT OR IGNORE INTO segments (id, digest, first, seq) VALUES (?1, 'head', NULL, ?2)",
            params![HEAD, HEAD_SEQ],
        )?;
        Ok(Index {
            connection,
            ids: Ids::default(),
        })
    }

    /// Fold the log in: new segments once, the head afresh.
    ///
    /// Safe to call as often as wanted — a segment already folded is
    /// recognised by its digest and skipped, and each segment lands in one
    /// transaction, so an interrupted fold left nothing half-indexed.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`, and everything reading the log can
    /// answer.
    pub fn fold(&mut self, log: &Log) -> Result<Folded> {
        let folded = self.fold_in(log);
        if folded.is_err() {
            // A transaction rolled back may have taken names with it
            // that the cache still knows ids for.
            self.ids = Ids::default();
        }
        folded
    }

    fn fold_in(&mut self, log: &Log) -> Result<Folded> {
        let mut folded = Folded::default();

        let segments = log.segments()?;
        for (rank, segment) in segments.iter().enumerate() {
            let digest = segment.digest().to_string();
            let known: bool = self.connection.query_row(
                "SELECT EXISTS (SELECT 1 FROM segments WHERE digest = ?1)",
                params![digest],
                |row| row.get(0),
            )?;
            if known {
                continue;
            }
            let claims = log.read(segment.digest())?;
            let transaction = self.connection.transaction()?;
            transaction.execute(
                "INSERT INTO segments (digest, first, seq) VALUES (?1, ?2, ?3)",
                params![
                    digest,
                    segment.first_claim_at().map(Timestamp::as_str),
                    rank_of(rank)
                ],
            )?;
            let id = transaction.last_insert_rowid();
            insert(&transaction, &mut self.ids, id, &claims)?;
            transaction.commit()?;
            folded.segments += 1;
            folded.claims += claims.len();
        }
        if folded.segments > 0 {
            // A new segment may sort before ones already folded, so every
            // sealed segment takes its rank from the log's own order
            // again — a few hundred rows at most. Whatever order the log
            // answers in, the index inherits without knowing why.
            let transaction = self.connection.transaction()?;
            for (rank, segment) in segments.iter().enumerate() {
                transaction.execute(
                    "UPDATE segments SET seq = ?1 WHERE digest = ?2",
                    params![rank_of(rank), segment.digest().to_string()],
                )?;
            }
            transaction.commit()?;
        }

        let head = log.head()?;
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM claims WHERE segment = ?1", params![HEAD])?;
        insert(&transaction, &mut self.ids, HEAD, &head)?;
        transaction.commit()?;
        folded.head = head.len();

        Ok(folded)
    }

    /// The record as it stood at `cutoff`, as an index of its own: every
    /// claim recorded by then — assertions and retractions alike, in log
    /// order — replayed into a throwaway in-memory index, so every
    /// question this type answers can be asked of that day instead of
    /// today. The cutoff compares in claim time's own spelling, RFC 3339
    /// UTC. Call [`fold`](Index::fold) first: the replay reads this
    /// index, not the log.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; the row-to-claim errors cannot
    /// happen for rows a fold wrote, but are propagated rather than
    /// sworn away.
    pub fn as_of(&self, cutoff: &str) -> Result<Index> {
        use rusqlite::types::Value as Sql;
        self.replay_until("c.time <= ?1", &[Sql::Text(cutoff.to_string())])
    }

    /// The archive's knowledge as of the end of one run: every claim up
    /// to and including the run's last, in log order — a
    /// [`as_of`](Index::as_of) that cuts at a claim instead of a second,
    /// so two runs within one second still come apart. `None` when no
    /// claim carries the run.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`, as for [`as_of`](Index::as_of).
    pub fn as_of_run(&self, run: &Run) -> Result<Option<Index>> {
        use rusqlite::types::Value as Sql;
        let last = self
            .connection
            .query_row(
                "SELECT c.time, s.seq, c.position
                 FROM claims c JOIN segments s ON s.id = c.segment
                 WHERE c.run = (SELECT id FROM runs WHERE name = ?1)
                 ORDER BY c.time DESC, s.seq DESC, c.position DESC
                 LIMIT 1",
                params![run.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((time, seq, position)) = last else {
            return Ok(None);
        };
        let replayed = self.replay_until(
            "c.time < ?1 OR (c.time = ?1 AND (s.seq < ?2 OR (s.seq = ?2 AND c.position <= ?3)))",
            &[Sql::Text(time), Sql::Integer(seq), Sql::Integer(position)],
        )?;
        Ok(Some(replayed))
    }

    /// The record as it stood when `given` names, and the moment that
    /// is: a time in the friendlier spellings — a date alone closes at
    /// that day's end, "as of the first" meaning the first has
    /// happened — or a run id, closing after that run's last claim.
    /// The one door `--as-of` goes through, whichever program holds it.
    /// `None` for a run the record has no claim of.
    ///
    /// # Errors
    ///
    /// [`Error::Timestamp`] when `given` is neither a run id nor a
    /// moment; [`Error::Index`] from `SQLite`.
    pub fn at(&self, given: &str) -> Result<Option<(Index, Timestamp)>> {
        if Run::spelled(given) {
            let run = Run::parse(given)?;
            let Some(replayed) = self.as_of_run(&run)? else {
                return Ok(None);
            };
            let closed: String = self.connection.query_row(
                "SELECT MAX(c.time) FROM claims c
                 WHERE c.run = (SELECT id FROM runs WHERE name = ?1)",
                params![run.as_str()],
                |row| row.get(0),
            )?;
            return Ok(Some((replayed, Timestamp::parse(&closed)?)));
        }
        let closing = Timestamp::closing(given)?;
        Ok(Some((self.as_of(closing.as_str())?, closing)))
    }

    /// Every claim the `until` clause admits — spoken of `c`, the claim,
    /// and `s`, its segment — replayed in log order into a throwaway
    /// in-memory index.
    fn replay_until(&self, until: &str, params: &[rusqlite::types::Value]) -> Result<Index> {
        let mut replayed = Index::open(":memory:")?;
        // The segments' own order rides along: `about` breaks time ties
        // by it, and the replayed record must read like the original.
        let segments: std::result::Result<Vec<SegmentRow>, _> = self
            .connection
            .prepare("SELECT id, digest, first, seq FROM segments WHERE id != ?1")?
            .query_map(params![HEAD], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect();
        let mut statement = self.connection.prepare(&format!(
            "SELECT su.digest, a.name, c.value, c.time, so.name, r.name, c.retract,
                    c.segment, c.position
             FROM claims c
             JOIN subjects su ON su.id = c.subject
             JOIN attributes a ON a.id = c.attribute
             JOIN sources so ON so.id = c.source
             LEFT JOIN runs r ON r.id = c.run
             JOIN segments s ON s.id = c.segment
             WHERE {until}
             ORDER BY c.time, s.seq, c.position"
        ))?;
        let rows = statement.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok((
                (
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, bool>(6)?,
                ),
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
            ))
        })?;
        let Index { connection, ids } = &mut replayed;
        let transaction = connection.transaction()?;
        for (id, digest, first, seq) in segments? {
            transaction.execute(
                "INSERT INTO segments (id, digest, first, seq) VALUES (?1, ?2, ?3, ?4)",
                params![id, digest, first, seq],
            )?;
        }
        for row in rows {
            let (fields, segment, position) = row?;
            replay(&transaction, ids, &claim(fields)?, segment, position)?;
        }
        transaction.commit()?;
        Ok(replayed)
    }

    /// Everything the log says about one subject, in log order: by time,
    /// ties broken by the segments' own order, the head last.
    ///
    /// Retractions come back as the claims they are — interpreting them is
    /// the caller's policy, not the index's.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; the row-to-claim errors cannot happen
    /// for rows a fold wrote, but are propagated rather than sworn away.
    pub fn about(&self, subject: &Subject) -> Result<Vec<Claim>> {
        let mut statement = self.connection.prepare(
            "SELECT su.digest, a.name, c.value, c.time, so.name, r.name, c.retract
             FROM claims c
             JOIN subjects su ON su.id = c.subject
             JOIN attributes a ON a.id = c.attribute
             JOIN sources so ON so.id = c.source
             LEFT JOIN runs r ON r.id = c.run
             JOIN segments s ON s.id = c.segment
             WHERE su.digest = ?1
             ORDER BY c.time, s.seq, c.position",
        )?;
        let rows = statement.query_map(params![subject.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, bool>(6)?,
            ))
        })?;
        let mut claims = Vec::new();
        for row in rows {
            claims.push(claim(row?)?);
        }
        Ok(claims)
    }

    /// One subject's values for one attribute, sorted by their stored
    /// spelling — as of the last [`fold`](Index::fold), the open head
    /// included.
    ///
    /// Under [`Scope::Present`] and [`Scope::Held`] the standing values:
    /// where [`about`](Index::about) answers with the history, this
    /// answers with the outcome, retractions already applied, repeats
    /// already collapsed. Which of several standing values a reader
    /// prefers stays query-time policy, so they all come back. Under
    /// [`Scope::Record`] every value ever said, asserted or taken back,
    /// each once.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; a value that does not parse back
    /// cannot happen for rows a fold wrote, but is propagated rather than
    /// sworn away.
    pub fn values(
        &self,
        subject: &Subject,
        attribute: &Attribute,
        scope: Scope,
    ) -> Result<Vec<Value>> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT DISTINCT value FROM {}
             WHERE subject = (SELECT id FROM subjects WHERE digest = ?1)
               AND attribute = (SELECT id FROM attributes WHERE name = ?2)
               AND value IS NOT NULL
             ORDER BY value",
            rows_of(scope)
        ))?;
        let rows = statement.query_map(params![subject.as_str(), attribute.as_str()], |row| {
            row.get::<_, String>(0)
        })?;
        let mut values = Vec::new();
        for row in rows {
            values.push(serde_json::from_str(&row?)?);
        }
        Ok(values)
    }

    /// What one subject holds across a whole namespace: every
    /// `(attribute, value)` whose attribute begins `namespace:`, ordered
    /// by attribute and value — as of the last [`fold`](Index::fold), the
    /// open head included. The namespace comes bare, without its colon.
    /// The [`Scope`] reads as in [`values`](Index::values): the standing
    /// pairs, or every pair ever said.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; a row that does not parse back
    /// cannot happen for rows a fold wrote, but is propagated rather than
    /// sworn away.
    pub fn values_in(
        &self,
        subject: &Subject,
        namespace: &str,
        scope: Scope,
    ) -> Result<Vec<(Attribute, Value)>> {
        let mut statement = self.connection.prepare(&format!(
            // ';' is the character after ':', and no attribute contains
            // one: the half-open range is the namespace.
            "SELECT DISTINCT a.name, st.value
             FROM {} st JOIN attributes a ON a.id = st.attribute
             WHERE st.subject = (SELECT id FROM subjects WHERE digest = ?1)
               AND a.name >= ?2 AND a.name < ?3
               AND st.value IS NOT NULL
             ORDER BY a.name, st.value",
            rows_of(scope)
        ))?;
        let rows = statement.query_map(
            params![
                subject.as_str(),
                format!("{namespace}:"),
                format!("{namespace};")
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        let mut standing = Vec::new();
        for row in rows {
            let (attribute, value) = row?;
            standing.push((Attribute::parse(&attribute)?, serde_json::from_str(&value)?));
        }
        Ok(standing)
    }

    /// Everything currently standing on one subject: every standing
    /// `(attribute, value)` pair, ordered by attribute and value — as of
    /// the last [`fold`](Index::fold), the open head included. The whole
    /// outcome where [`values`](Index::values) answers one attribute and
    /// [`values_in`](Index::values_in) one namespace.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; a row that does not parse back
    /// cannot happen for rows a fold wrote, but is propagated rather than
    /// sworn away.
    pub fn standing(&self, subject: &Subject) -> Result<Vec<(Attribute, Value)>> {
        let mut statement = self.connection.prepare(
            "SELECT a.name, st.value
             FROM standing st JOIN attributes a ON a.id = st.attribute
             WHERE st.subject = (SELECT id FROM subjects WHERE digest = ?1)
             ORDER BY a.name, st.value",
        )?;
        let rows = statement.query_map(params![subject.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut standing = Vec::new();
        for row in rows {
            let (attribute, value) = row?;
            standing.push((Attribute::parse(&attribute)?, serde_json::from_str(&value)?));
        }
        Ok(standing)
    }

    /// Every subject the record speaks about, sorted — as of the last
    /// [`fold`](Index::fold), the open head included. The answer to a
    /// question that names no terms at all: show me something about
    /// every file.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; the row-to-subject errors cannot
    /// happen for rows a fold wrote, but are propagated rather than
    /// sworn away.
    pub fn subjects(&self, scope: Scope) -> Result<Vec<Subject>> {
        let mut sql = String::new();
        if scope == Scope::Present {
            sql.push_str(PLACED);
        }
        let _ = write!(
            sql,
            "SELECT digest FROM subjects su
             WHERE EXISTS (SELECT 1 FROM {} st WHERE st.subject = su.id)",
            rows_of(scope)
        );
        if scope == Scope::Present {
            sql.push_str(" AND su.id IN (SELECT subject FROM placed)");
        }
        sql.push_str(" ORDER BY digest");
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut subjects = Vec::new();
        for row in rows {
            subjects.push(Subject::parse(&row?)?);
        }
        Ok(subjects)
    }

    /// Every attribute standing on the record, each with the number of
    /// files it stands on — sorted by attribute, as of the last
    /// [`fold`](Index::fold), the open head included. The words a
    /// question can be asked in: what [`find`](Index::find) can name is
    /// exactly what answers here, and an attribute every value of which
    /// was retracted is not among them.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; the row-to-attribute errors cannot
    /// happen for rows a fold wrote, but are propagated rather than
    /// sworn away.
    pub fn attributes(&self) -> Result<Vec<(Attribute, u64)>> {
        let mut statement = self.connection.prepare(
            "SELECT a.name, COUNT(DISTINCT st.subject)
             FROM standing st JOIN attributes a ON a.id = st.attribute
             GROUP BY a.id ORDER BY a.name",
        )?;
        let rows = statement.query_map([], |row| {
            // A COUNT is never negative; the conversion is the type's
            // formality, and its failure would be SQLite's own error.
            let files: i64 = row.get(1)?;
            let files = u64::try_from(files)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, files))?;
            Ok((row.get::<_, String>(0)?, files))
        })?;
        let mut attributes = Vec::new();
        for row in rows {
            let (attribute, files) = row?;
            attributes.push((Attribute::parse(&attribute)?, files));
        }
        Ok(attributes)
    }

    /// Everything standing under one place: every standing `file:path`
    /// that names `place` itself or anything below it, as (path, subject)
    /// pairs sorted by path then subject — as of the last
    /// [`fold`](Index::fold), the open head included.
    ///
    /// Below means component-wise, the way folders nest: `/a/b` lies
    /// under `/a`, `/a/bc` does not. `place` comes without a trailing
    /// slash, the root as `/`. Only string values name places; a
    /// `file:path` standing as any other type is passed over.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; the row-to-subject errors cannot
    /// happen for rows a fold wrote, but are propagated rather than
    /// sworn away.
    pub fn under(&self, place: &str) -> Result<Vec<(String, Subject)>> {
        let mut statement = self.connection.prepare(
            "SELECT st.value, su.digest
             FROM standing st JOIN subjects su ON su.id = st.subject
             WHERE st.attribute = (SELECT id FROM attributes WHERE name = 'file:path')",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut pairs = Vec::new();
        for row in rows {
            let (value, subject) = row?;
            let Value::String(path) = serde_json::from_str(&value)? else {
                continue;
            };
            let below = match place {
                "/" => path.starts_with('/'),
                _ => {
                    path == place
                        || path
                            .strip_prefix(place)
                            .is_some_and(|rest| rest.starts_with('/'))
                }
            };
            if below {
                pairs.push((path, Subject::parse(&subject)?));
            }
        }
        pairs.sort();
        Ok(pairs)
    }

    /// One attribute's standing values across the whole record as it
    /// stood at `cutoff` — each with the moment of its newest surviving
    /// assertion.
    ///
    /// With a cutoff this is the fold run once more with a closing time:
    /// every claim of `attribute` up to and including `cutoff` —
    /// assertions and retractions alike, in log order — and what stands
    /// when the replay ends is the answer. `None` closes nowhere and
    /// answers for today, straight from the standing set, which carries
    /// each row's newest moment already. The cutoff compares in claim
    /// time's own spelling, RFC 3339 UTC whole seconds.
    ///
    /// The answer carries every standing value, times attached; a
    /// reader narrowing them to one — a mounted view's newest-wins —
    /// decides by those times, and the narrowing stays the reader's
    /// policy, as everywhere.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; the row-to-subject and
    /// row-to-value errors cannot happen for rows a fold wrote, but are
    /// propagated rather than sworn away.
    pub fn standing_as_of(&self, attribute: &str, cutoff: Option<&str>) -> Result<Vec<Standing>> {
        use std::collections::BTreeMap;

        let Some(cutoff) = cutoff else {
            let mut statement = self.connection.prepare(
                "SELECT su.digest, st.value, st.time, st.claim
                 FROM standing st JOIN subjects su ON su.id = st.subject
                 WHERE st.attribute = (SELECT id FROM attributes WHERE name = ?1)
                 ORDER BY su.digest, st.value",
            )?;
            let rows = statement.query_map(params![attribute], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?;
            let mut standing = Vec::new();
            for row in rows {
                let (subject, value, asserted, claim) = row?;
                standing.push(Standing {
                    subject: Subject::parse(&subject)?,
                    value: serde_json::from_str(&value)?,
                    asserted,
                    order: order_of(claim),
                });
            }
            return Ok(standing);
        };

        let mut statement = self.connection.prepare(
            "SELECT su.digest, c.value, c.time, c.retract, c.id
             FROM claims c
             JOIN subjects su ON su.id = c.subject
             JOIN segments s ON s.id = c.segment
             WHERE c.attribute = (SELECT id FROM attributes WHERE name = ?1)
               AND c.time <= ?2
             ORDER BY c.time, s.seq, c.position",
        )?;
        let rows = statement.query_map(params![attribute, cutoff], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;

        let mut folded: BTreeMap<String, BTreeMap<String, (String, u64)>> = BTreeMap::new();
        for row in rows {
            let (subject, value, time, retract, claim) = row?;
            match (retract, value) {
                // A repeated assertion collapses in the set but renews
                // the moment: the newest surviving assertion is the one
                // that answers for when.
                (false, Some(value)) => {
                    folded
                        .entry(subject)
                        .or_default()
                        .insert(value, (time, order_of(claim)));
                }
                (true, Some(value)) => {
                    if let Some(values) = folded.get_mut(&subject) {
                        values.remove(&value);
                    }
                }
                (true, None) => {
                    folded.remove(&subject);
                }
                // An assertion always carries a value; rows a fold
                // wrote cannot lack one.
                (false, None) => {}
            }
        }

        let mut standing = Vec::new();
        for (subject, values) in folded {
            if values.is_empty() {
                continue;
            }
            let subject = Subject::parse(&subject)?;
            for (value, (asserted, order)) in values {
                standing.push(Standing {
                    subject: subject.clone(),
                    value: serde_json::from_str(&value)?,
                    asserted,
                    order,
                });
            }
        }
        Ok(standing)
    }

    /// Every subject on which all `terms` hold and none of `missing` does,
    /// sorted — as of the last [`fold`](Index::fold), the open head
    /// included.
    ///
    /// An attribute term is an attribute and a pattern, and the pattern
    /// is read in this order: wrapped in double quotes it is *literal* —
    /// exactly that string, the way to name a value that looks like a
    /// glob or a range; with `*` or `?` it is a glob, matching within
    /// string values only; with `..` it is a range, `low..high` with
    /// either side open — bounds compare in the attribute's own spelling,
    /// lexicographically for strings and numerically for numbers, the low
    /// end inclusive, and a bare `..` asks only that the attribute stands
    /// at all; otherwise it is exact — a string, or the bare JSON scalar
    /// for numbers and booleans, either spelling answering. Every term
    /// must hold, each on *some* value: two ranged terms on one attribute
    /// may be satisfied by two different values, where one `low..high`
    /// term speaks about a single value lying between.
    ///
    /// A field term speaks about the claim that carries a value — who
    /// wrote it, when, in which run — and holds for every attribute term
    /// at once: `run=X file:name=abc` is a name said in run X, and with
    /// a second attribute term both must have been said in X, by two
    /// claims of the same run. Field patterns read like attribute
    /// patterns, in the field's own spelling: a run id, a source, a
    /// digest, a time to the second; `retract` is `true` or `false`.
    /// A claim from before runs were written has none, and no `run`
    /// term reaches it.
    ///
    /// Each entry of `missing` names an attribute the subject must lack;
    /// ending in `:` it names a whole namespace. With no terms at all,
    /// `missing` is asked of every subject the log speaks about.
    ///
    /// Under [`Scope::Present`] and [`Scope::Held`] only *standing*
    /// values answer, and a field term speaks about the claim that
    /// stands for the value — the newest one to say it: a retracted
    /// value finds nothing, however long its claim stays in the log,
    /// and a value said again answers for its latest run only. Under
    /// [`Scope::Record`] every claim ever written answers, retractions
    /// included, which is the one way to `retract=true`.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; an entry of `missing` that fits
    /// neither the attribute grammar nor `namespace:` is refused with the
    /// grammar's own error; [`Error::Retract`] for a `retract` pattern
    /// that is neither true nor false.
    pub fn find(&self, terms: &[Term], missing: &[String], scope: Scope) -> Result<Vec<Subject>> {
        use rusqlite::types::Value as Sql;
        if terms.is_empty() && missing.is_empty() {
            return Ok(Vec::new());
        }
        let attributes: Vec<(&Attribute, &str)> = terms
            .iter()
            .filter_map(|term| match term {
                Term::Attribute(attribute, pattern) => Some((attribute, pattern.as_str())),
                Term::Field(..) => None,
            })
            .collect();
        let fields: Vec<(Field, &str)> = terms
            .iter()
            .filter_map(|term| match term {
                Term::Field(field, pattern) => Some((*field, pattern.as_str())),
                Term::Attribute(..) => None,
            })
            .collect();
        // Under the standing scopes a row is a standing value with the
        // claim that stands for it alongside; under the record it is the
        // claim itself. Either way `c` is the claim a field term asks
        // about and `st` the row an attribute term asks about.
        let rows = match scope {
            Scope::Record => "claims c",
            Scope::Present | Scope::Held => "standing st JOIN claims c ON c.id = st.claim",
        };
        let row = match scope {
            Scope::Record => "c",
            Scope::Present | Scope::Held => "st",
        };
        // The terms meet as sets of subject ids; the names come last.
        let mut sql = String::new();
        if scope == Scope::Present {
            sql.push_str(PLACED);
        }
        sql.push_str("SELECT digest FROM subjects WHERE id IN (");
        let mut params: Vec<Sql> = Vec::new();
        if attributes.is_empty() {
            let _ = write!(sql, "SELECT c.subject FROM {rows} WHERE 1");
            for (field, pattern) in &fields {
                field_clause(&mut sql, &mut params, *field, pattern)?;
            }
        }
        for (position, (attribute, pattern)) in attributes.iter().enumerate() {
            if position > 0 {
                sql.push_str(" INTERSECT ");
            }
            let _ = write!(
                sql,
                "SELECT c.subject FROM {rows}
                 WHERE {row}.attribute = (SELECT id FROM attributes WHERE name = ?)"
            );
            params.push(Sql::Text(attribute.as_str().to_string()));
            value_clause(&mut sql, &mut params, &format!("{row}.value"), pattern);
            for (field, pattern) in &fields {
                field_clause(&mut sql, &mut params, *field, pattern)?;
            }
        }
        lacking(&mut sql, &mut params, missing, rows_of(scope))?;
        if scope == Scope::Present {
            sql.push_str(" INTERSECT SELECT subject FROM placed");
        }
        sql.push_str(") ORDER BY digest");
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            row.get::<_, String>(0)
        })?;
        let mut subjects = Vec::new();
        for row in rows {
            subjects.push(Subject::parse(&row?)?);
        }
        Ok(subjects)
    }

    /// The distinct values one field takes over one subject's claims,
    /// sorted — as of the last [`fold`](Index::fold), the open head
    /// included. Under the standing scopes the claims that stand for
    /// the subject's standing values; under [`Scope::Record`] every
    /// claim about it. A string field answers strings, `value` the
    /// values themselves, `retract` booleans; a claim from before runs
    /// contributes nothing to `run`.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; a value that does not parse back
    /// cannot happen for rows a fold wrote, but is propagated rather than
    /// sworn away.
    pub fn field_values(
        &self,
        subject: &Subject,
        field: Field,
        scope: Scope,
    ) -> Result<Vec<Value>> {
        let rows = match scope {
            Scope::Record => "claims c",
            Scope::Present | Scope::Held => "claims c JOIN standing st ON st.claim = c.id",
        };
        let (expression, join) = match field {
            Field::Subject => ("su.digest", " JOIN subjects su ON su.id = c.subject"),
            Field::Attribute => ("a.name", " JOIN attributes a ON a.id = c.attribute"),
            Field::Value => ("c.value", ""),
            Field::Time => ("c.time", ""),
            Field::Source => ("so.name", " JOIN sources so ON so.id = c.source"),
            Field::Run => ("r.name", " JOIN runs r ON r.id = c.run"),
            Field::Retract => ("c.retract", ""),
        };
        let mut statement = self.connection.prepare(&format!(
            "SELECT DISTINCT {expression} FROM {rows}{join}
             WHERE c.subject = (SELECT id FROM subjects WHERE digest = ?1)
               AND {expression} IS NOT NULL
             ORDER BY 1"
        ))?;
        let rows = statement.query_map(params![subject.as_str()], |row| {
            row.get::<_, rusqlite::types::Value>(0)
        })?;
        let mut values = Vec::new();
        for row in rows {
            values.push(match (field, row?) {
                (Field::Value, rusqlite::types::Value::Text(json)) => serde_json::from_str(&json)?,
                (Field::Retract, rusqlite::types::Value::Integer(flag)) => Value::Bool(flag != 0),
                (_, rusqlite::types::Value::Text(text)) => Value::String(text),
                // The columns above are text or the retract flag; another
                // storage class would be SQLite's own surprise.
                (_, other) => Value::String(format!("{other:?}")),
            });
        }
        Ok(values)
    }

    /// Every run on the record, in log order: what each call wrote, from
    /// its first claim to its last — as of the last [`fold`](Index::fold),
    /// the open head included. Claims from before runs were written
    /// belong to no episode.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; the row-to-run errors cannot
    /// happen for rows a fold wrote, but are propagated rather than
    /// sworn away.
    pub fn history(&self) -> Result<Vec<Episode>> {
        let mut statement = self.connection.prepare(
            // Runs in order of their first claim: the lowest segment
            // and the lowest position taken apart would put a run that
            // went on past a seal before one that began earlier.
            "WITH first AS (
                 SELECT run, seq, position FROM (
                     SELECT c.run, s.seq, c.position,
                            ROW_NUMBER() OVER (
                                PARTITION BY c.run ORDER BY c.time, s.seq, c.position
                            ) AS rank
                     FROM claims c JOIN segments s ON s.id = c.segment
                     WHERE c.run IS NOT NULL)
                 WHERE rank = 1)
             SELECT r.name, MIN(c.time), MAX(c.time), COUNT(DISTINCT c.subject), COUNT(*),
                    SUM(c.retract)
             FROM claims c
             JOIN runs r ON r.id = c.run
             JOIN first f ON f.run = c.run
             GROUP BY c.run
             ORDER BY MIN(c.time), f.seq, f.position",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        let mut episodes = Vec::new();
        for row in rows {
            let (run, first, last, files, claims, retractions) = row?;
            episodes.push(Episode {
                run: Run::parse(&run)?,
                first,
                last,
                sources: Vec::new(),
                files: counted(files)?,
                claims: counted(claims)?,
                retractions: counted(retractions)?,
            });
        }
        let mut statement = self.connection.prepare(
            "SELECT DISTINCT r.name, so.name
             FROM claims c
             JOIN runs r ON r.id = c.run
             JOIN sources so ON so.id = c.source
             ORDER BY so.name",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (run, source) = row?;
            if let Some(episode) = episodes
                .iter_mut()
                .find(|episode| episode.run.as_str() == run)
            {
                episode.sources.push(Source::parse(&source)?);
            }
        }
        Ok(episodes)
    }

    /// Every subject still waiting for an extractor: standing `file:mime`
    /// among `mimes`, and no standing [`prov:examined`](crate::EXAMINED)
    /// receipt naming `source` — as of the last [`fold`](Index::fold), the
    /// open head included. Sorted, so a run walks the same order twice.
    ///
    /// This is the log informing *effort*, never truth: whether a file is
    /// offered again is decided here, what an extractor says about it never
    /// is. Both sides of the test read the standing set, so a retracted
    /// kind takes a file off the list and a retracted receipt puts it back.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; the row-to-subject errors cannot
    /// happen for rows a fold wrote, but are propagated rather than sworn
    /// away.
    pub fn worklist(&self, mimes: &[String], source: &Source) -> Result<Vec<Subject>> {
        if mimes.is_empty() {
            return Ok(Vec::new());
        }
        let holes = vec!["?"; mimes.len()].join(", ");
        let mut statement = self.connection.prepare(&format!(
            "SELECT digest FROM subjects WHERE id IN (
                 SELECT subject FROM standing
                  WHERE attribute = (SELECT id FROM attributes WHERE name = 'file:mime')
                    AND value IN ({holes})
                 EXCEPT
                 SELECT subject FROM standing
                  WHERE attribute = (SELECT id FROM attributes WHERE name = 'prov:examined')
                    AND value = ?)
             ORDER BY digest"
        ))?;
        // The value column holds values as JSON, so a MIME type and the
        // receipt's source are compared in their stored spelling: quoted.
        let quoted: Vec<String> = mimes
            .iter()
            .map(|mime| Value::String(mime.clone()).to_string())
            .collect();
        let mut params: Vec<&dyn rusqlite::ToSql> = Vec::new();
        for mime in &quoted {
            params.push(mime);
        }
        let receipt = Value::String(source.as_str().to_string()).to_string();
        params.push(&receipt);
        let rows = statement.query_map(params.as_slice(), |row| row.get::<_, String>(0))?;
        let mut subjects = Vec::new();
        for row in rows {
            subjects.push(Subject::parse(&row?)?);
        }
        Ok(subjects)
    }

    /// Every subject whose standing `file:mime` is among `mimes`,
    /// receipts notwithstanding — the [`worklist`](Index::worklist)
    /// before anything is subtracted: what `extract --full` examines
    /// anew. Sorted, like the worklist.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; the row-to-subject errors cannot
    /// happen for rows a fold wrote, but are propagated rather than sworn
    /// away.
    pub fn of_kind(&self, mimes: &[String]) -> Result<Vec<Subject>> {
        if mimes.is_empty() {
            return Ok(Vec::new());
        }
        let holes = vec!["?"; mimes.len()].join(", ");
        let mut statement = self.connection.prepare(&format!(
            "SELECT digest FROM subjects WHERE id IN (
                 SELECT subject FROM standing
                  WHERE attribute = (SELECT id FROM attributes WHERE name = 'file:mime')
                    AND value IN ({holes}))
             ORDER BY digest"
        ))?;
        // The value column holds values as JSON, so a MIME type is
        // compared in its stored spelling: quoted.
        let quoted: Vec<String> = mimes
            .iter()
            .map(|mime| Value::String(mime.clone()).to_string())
            .collect();
        let rows = statement.query_map(rusqlite::params_from_iter(quoted.iter()), |row| {
            row.get::<_, String>(0)
        })?;
        let mut subjects = Vec::new();
        for row in rows {
            subjects.push(Subject::parse(&row?)?);
        }
        Ok(subjects)
    }

    /// Whether `source` has already examined `subject`: a standing
    /// [`prov:examined`](crate::EXAMINED) receipt naming exactly this
    /// source as its value. What the worklist subtracts wholesale, asked
    /// about one file — for a run that was handed names instead of a kind.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`.
    pub fn examined(&self, subject: &Subject, source: &Source) -> Result<bool> {
        let mut statement = self.connection.prepare_cached(
            "SELECT 1 FROM standing
              WHERE subject = (SELECT id FROM subjects WHERE digest = ?1)
                AND attribute = (SELECT id FROM attributes WHERE name = 'prov:examined')
                AND value = ?2
              LIMIT 1",
        )?;
        let receipt = Value::String(source.as_str().to_string()).to_string();
        let found = statement.exists(rusqlite::params![subject.as_str(), receipt])?;
        Ok(found)
    }

    /// Whether any claim on the record carries `run`. A run of
    /// retractions or tags alone is on the record and names no file,
    /// which [`run_sightings`](Index::run_sightings) cannot tell from a
    /// run that never was.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`.
    pub fn has_run(&self, run: &Run) -> Result<bool> {
        let found: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM claims c
                           WHERE c.run = (SELECT id FROM runs WHERE name = ?1))",
            params![run.as_str()],
            |row| row.get(0),
        )?;
        Ok(found)
    }

    /// Every sighting one run put on the record, with where it saw the
    /// file: one pair per place, the same content seen at two places in
    /// one run answering twice. An ingest sighting answers with its
    /// `file:path`; a sighting without one — a derived file never sat
    /// anywhere — answers with its `file:name`, every name the run
    /// spelled. Empty when no claim carries the run.
    ///
    /// This asks the history, not the standing set: a run's record stays
    /// its record, later retractions notwithstanding.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; the row-to-subject errors cannot
    /// happen for rows a fold wrote, but are propagated rather than
    /// sworn away.
    pub fn run_sightings(&self, run: &Run) -> Result<Vec<(Subject, Placement)>> {
        let mut statement = self.connection.prepare(
            "SELECT DISTINCT su.digest, a.name, json_extract(c.value, '$')
             FROM claims c
             JOIN subjects su ON su.id = c.subject
             JOIN attributes a ON a.id = c.attribute
             WHERE c.run = (SELECT id FROM runs WHERE name = ?1)
               AND c.retract = 0
               AND a.name IN ('file:path', 'file:name')
               AND json_type(c.value) = 'text'
             ORDER BY su.digest, a.name DESC, c.value",
        )?;
        let rows = statement.query_map(params![run.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut sightings: Vec<(Subject, Placement)> = Vec::new();
        let mut current: Option<(Subject, bool)> = None;
        for row in rows {
            let (digest, attribute, spelled) = row?;
            let subject = match &current {
                Some((subject, _)) if subject.as_str() == digest => subject.clone(),
                _ => {
                    let subject = Subject::parse(&digest)?;
                    current = Some((subject.clone(), false));
                    subject
                }
            };
            // Paths are ordered before names, so the first path of a
            // subject is met before any of its names; a subject with a
            // path answers with its paths alone.
            let placed = current.as_ref().is_some_and(|(_, placed)| *placed);
            if attribute == "file:path" {
                current = Some((subject.clone(), true));
                sightings.push((subject, Placement::Path(spelled)));
            } else if !placed {
                sightings.push((subject, Placement::Name(spelled)));
            }
        }
        Ok(sightings)
    }

    /// Every subject the log speaks about — as of the last
    /// [`fold`](Index::fold), the open head included — whose digest begins
    /// with `hex`, sorted. Case does not matter, the way [`Subject`] itself
    /// normalises; a `hex` that is a whole digest names at most itself.
    ///
    /// This is where an abbreviated name is resolved, and deliberately not
    /// in the content store: the log may speak about subjects no store
    /// holds, and the index already has them all in one indexed range.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; the row-to-subject errors cannot
    /// happen for rows a fold wrote, but are propagated rather than sworn
    /// away.
    pub fn matching(&self, hex: &str) -> Result<Vec<Subject>> {
        let low = hex.to_ascii_lowercase();
        // The half-open range [low, low + "g") holds every digest that
        // begins with `low` and nothing else: 'g' is the first character
        // past the hex digits, and a range beats LIKE here — it uses the
        // index, and a stray '%' in the input stays a character.
        let high = format!("{low}g");
        let mut statement = self.connection.prepare(
            "SELECT digest FROM subjects
             WHERE digest >= ?1 AND digest < ?2
             ORDER BY digest",
        )?;
        let rows = statement.query_map(params![low, high], |row| row.get::<_, String>(0))?;
        let mut subjects = Vec::new();
        for row in rows {
            subjects.push(Subject::parse(&row?)?);
        }
        Ok(subjects)
    }

    /// The subject as the log spells it, from whatever the caller was
    /// given: a whole name taken as it is, a beginning resolved against
    /// [`matching`](Index::matching), like a short commit hash. `None`
    /// when nothing on the record begins that way — whether that is calm
    /// or a refusal is the caller's question.
    ///
    /// # Errors
    ///
    /// [`Error::Ambiguous`] on a beginning that starts several names, and
    /// whatever [`matching`](Index::matching) can answer.
    pub fn resolve(&self, given: &str) -> Result<Option<Subject>> {
        if let Ok(whole) = Subject::parse(given) {
            return Ok(Some(whole));
        }
        match self.matching(given)?.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some(one.clone())),
            many => Err(Error::Ambiguous {
                given: given.to_string(),
                count: many.len(),
            }),
        }
    }
}

/// A segment's rank in log order, as the row stores it.
fn rank_of(rank: usize) -> i64 {
    i64::try_from(rank).expect("fewer segments than i64 can count")
}

/// A claim's row id as a [`Standing`] order: ids count up from one.
fn order_of(claim: i64) -> u64 {
    u64::try_from(claim).expect("row ids are positive")
}

/// One segment's claims into the tables, in their order: every claim a
/// history row, and each one folded into `standing` — an assertion puts
/// the value in and renews its moment, a retraction takes it out, a
/// valueless retraction empties the attribute. All three are idempotent,
/// which is what lets the head be applied afresh each fold.
fn insert(
    transaction: &rusqlite::Transaction<'_>,
    ids: &mut Ids,
    segment: i64,
    claims: &[Claim],
) -> Result<()> {
    for (position, claim) in claims.iter().enumerate() {
        let position = i64::try_from(position).expect("fewer claims than i64 can count");
        replay(transaction, ids, claim, segment, position)?;
    }
    Ok(())
}

/// One claim into the tables: into the history as it is, onto the
/// standing what it asserts or takes away.
fn replay(
    transaction: &rusqlite::Transaction<'_>,
    ids: &mut Ids,
    claim: &Claim,
    segment: i64,
    position: i64,
) -> Result<()> {
    let subject = ids.subject(transaction, claim.subject().as_str())?;
    let attribute = ids.attribute(transaction, claim.attribute().as_str())?;
    let source = ids.source(transaction, claim.source().as_str())?;
    let run = match claim.run() {
        Some(run) => Some(ids.run(transaction, run.as_str())?),
        None => None,
    };
    let value = claim.value().map(Value::to_string);
    let mut history = transaction.prepare_cached(
        "INSERT INTO claims
             (subject, attribute, value, time, source, run, retract, segment, position)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    history.execute(params![
        subject,
        attribute,
        value,
        claim.time().as_str(),
        source,
        run,
        claim.is_retraction(),
        segment,
        position,
    ])?;
    let id = transaction.last_insert_rowid();
    match (value, claim.is_retraction()) {
        (Some(value), false) => {
            // Said again, the value stands as before, but the moment and
            // the row that answer for it are the newest.
            let mut put = transaction.prepare_cached(
                "INSERT INTO standing (subject, attribute, value, time, claim)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (subject, attribute, value)
                 DO UPDATE SET time = excluded.time, claim = excluded.claim",
            )?;
            put.execute(params![
                subject,
                attribute,
                value,
                claim.time().as_str(),
                id
            ])?;
        }
        (Some(value), true) => {
            let mut take = transaction.prepare_cached(
                "DELETE FROM standing WHERE subject = ?1 AND attribute = ?2 AND value = ?3",
            )?;
            take.execute(params![subject, attribute, value])?;
        }
        (None, true) => {
            let mut empty = transaction
                .prepare_cached("DELETE FROM standing WHERE subject = ?1 AND attribute = ?2")?;
            empty.execute(params![subject, attribute])?;
        }
        // A claim without value and without retract cannot be built.
        (None, false) => {}
    }
    Ok(())
}

/// A row back into the claim it was — through the validating constructors,
/// so the index cannot smuggle in what the log could not have held.
fn claim(
    (subject, attribute, value, time, source, run, retract): (
        String,
        String,
        Option<String>,
        String,
        String,
        Option<String>,
        bool,
    ),
) -> Result<Claim> {
    let subject = Subject::parse(&subject)?;
    let attribute = Attribute::parse(&attribute)?;
    let time = Timestamp::parse(&time)?;
    let source = Source::parse(&source)?;
    let run = run.as_deref().map(Run::parse).transpose()?;
    let value = value
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()?;
    Claim::recorded(subject, attribute, value, time, source, run, retract)
}

/// The rows a [`Scope`] reads: the standing set, or the claims.
fn rows_of(scope: Scope) -> &'static str {
    match scope {
        Scope::Present | Scope::Held => "standing",
        Scope::Record => "claims",
    }
}

/// A count out of `SQLite` as the unsigned number it is.
fn counted(count: i64) -> Result<u64> {
    u64::try_from(count)
        .map_err(|_| Error::Index(rusqlite::Error::IntegralValueOutOfRange(0, count)))
}

/// The pattern of an attribute term, appended to `sql` as a condition
/// on `column`, a value in the log's JSON spelling: literal in quotes,
/// glob, range, or exact.
fn value_clause(
    sql: &mut String,
    params: &mut Vec<rusqlite::types::Value>,
    column: &str,
    pattern: &str,
) {
    use rusqlite::types::Value as Sql;
    let text = |s: &str| Sql::Text(s.to_string());
    let quoted = |s: &str| text(&Value::String(s.to_string()).to_string());
    // A bound spelled as a JSON number, typed so SQLite compares
    // numerically instead of by storage class.
    let number = |bound: &str| -> Option<Sql> {
        let value: Value = serde_json::from_str(bound).ok()?;
        if let Some(whole) = value.as_i64() {
            return Some(Sql::Integer(whole));
        }
        value.as_f64().map(Sql::Real)
    };
    if let Some(literal) = pattern
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        // Wrapped in quotes: exactly this string, nothing read into it
        // — the door for values that look like globs or ranges.
        let _ = write!(sql, " AND {column} = ?");
        params.push(quoted(literal));
    } else if pattern.contains('*') || pattern.contains('?') {
        // The stored string spelling is quoted JSON, so a pattern
        // globbed inside quotes matches string values and nothing else
        // — numbers were promised no wildcards.
        let _ = write!(sql, " AND {column} GLOB ?");
        params.push(text(&format!("\"{pattern}\"")));
    } else if let Some((low, high)) = pattern.split_once("..") {
        let numeric = [low, high]
            .iter()
            .filter(|bound| !bound.is_empty())
            .all(|bound| number(bound).is_some());
        if low.is_empty() && high.is_empty() {
            // A bare "..": any value at all.
            let _ = write!(sql, " AND {column} IS NOT NULL");
        } else if numeric {
            let _ = write!(sql, " AND json_type({column}) IN ('integer', 'real')");
            if let Some(bound) = number(low) {
                let _ = write!(sql, " AND json_extract({column}, '$') >= ?");
                params.push(bound);
            }
            if let Some(bound) = number(high) {
                let _ = write!(sql, " AND json_extract({column}, '$') <= ?");
                params.push(bound);
            }
        } else {
            // Bounds compare in the value's own spelling; the type guard
            // keeps numbers out, whose storage sorts below every quoted
            // string.
            let _ = write!(sql, " AND json_type({column}) = 'text'");
            if !low.is_empty() {
                let _ = write!(sql, " AND {column} >= ?");
                params.push(quoted(low));
            }
            if !high.is_empty() {
                let _ = write!(sql, " AND {column} <= ?");
                params.push(quoted(high));
            }
        }
    } else {
        match serde_json::from_str::<Value>(pattern) {
            // The bare word is a JSON scalar — a number, a boolean: it
            // may stand as itself or as a string, and either spelling
            // answers.
            Ok(scalar) if !scalar.is_string() => {
                let _ = write!(sql, " AND {column} IN (?, ?)");
                params.push(text(&scalar.to_string()));
                params.push(quoted(pattern));
            }
            _ => {
                let _ = write!(sql, " AND {column} = ?");
                params.push(quoted(pattern));
            }
        }
    }
}

/// The pattern of a field term over a plain text column — a digest, a
/// name, a time, a run id — appended to `sql` as a condition on
/// `column`: literal in quotes, glob, range in the column's own
/// spelling, or exact.
fn text_clause(
    sql: &mut String,
    params: &mut Vec<rusqlite::types::Value>,
    column: &str,
    pattern: &str,
) {
    use rusqlite::types::Value as Sql;
    let text = |s: &str| Sql::Text(s.to_string());
    if let Some(literal) = pattern
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        let _ = write!(sql, " AND {column} = ?");
        params.push(text(literal));
    } else if pattern.contains('*') || pattern.contains('?') {
        let _ = write!(sql, " AND {column} GLOB ?");
        params.push(text(pattern));
    } else if let Some((low, high)) = pattern.split_once("..") {
        let _ = write!(sql, " AND {column} IS NOT NULL");
        if !low.is_empty() {
            let _ = write!(sql, " AND {column} >= ?");
            params.push(text(low));
        }
        if !high.is_empty() {
            let _ = write!(sql, " AND {column} <= ?");
            params.push(text(high));
        }
    } else {
        let _ = write!(sql, " AND {column} = ?");
        params.push(text(pattern));
    }
}

/// The pattern of a time term, appended to `sql` as a condition on
/// `c.time`: in claim time's friendlier spellings, the ones `--as-of`
/// takes. A date alone is the whole day, a range from a date opens with
/// the day, one up to a date closes with it, and a moment without its
/// `Z` is the moment. A quoted literal and a glob read as written.
fn time_clause(
    sql: &mut String,
    params: &mut Vec<rusqlite::types::Value>,
    pattern: &str,
) -> Result<()> {
    use rusqlite::types::Value as Sql;
    let quoted = pattern.starts_with('"') && pattern.ends_with('"') && pattern.len() >= 2;
    if quoted || pattern.contains('*') || pattern.contains('?') {
        text_clause(sql, params, "c.time", pattern);
    } else if let Some((low, high)) = pattern.split_once("..") {
        if !low.is_empty() {
            sql.push_str(" AND c.time >= ?");
            params.push(Sql::Text(Timestamp::opening(low)?.as_str().to_string()));
        }
        if !high.is_empty() {
            sql.push_str(" AND c.time <= ?");
            params.push(Sql::Text(Timestamp::closing(high)?.as_str().to_string()));
        }
    } else if Timestamp::is_date(pattern) {
        sql.push_str(" AND c.time BETWEEN ? AND ?");
        params.push(Sql::Text(Timestamp::opening(pattern)?.as_str().to_string()));
        params.push(Sql::Text(Timestamp::closing(pattern)?.as_str().to_string()));
    } else {
        sql.push_str(" AND c.time = ?");
        params.push(Sql::Text(Timestamp::closing(pattern)?.as_str().to_string()));
    }
    Ok(())
}

/// One field term as a condition on the claim `c`, appended to `sql`.
fn field_clause(
    sql: &mut String,
    params: &mut Vec<rusqlite::types::Value>,
    field: Field,
    pattern: &str,
) -> Result<()> {
    use rusqlite::types::Value as Sql;
    match field {
        Field::Subject => {
            sql.push_str(" AND c.subject IN (SELECT id FROM subjects WHERE 1");
            text_clause(sql, params, "digest", pattern);
            sql.push(')');
        }
        Field::Attribute => {
            sql.push_str(" AND c.attribute IN (SELECT id FROM attributes WHERE 1");
            text_clause(sql, params, "name", pattern);
            sql.push(')');
        }
        Field::Source => {
            sql.push_str(" AND c.source IN (SELECT id FROM sources WHERE 1");
            text_clause(sql, params, "name", pattern);
            sql.push(')');
        }
        Field::Run => {
            sql.push_str(" AND c.run IN (SELECT id FROM runs WHERE 1");
            text_clause(sql, params, "name", pattern);
            sql.push(')');
        }
        Field::Time => time_clause(sql, params, pattern)?,
        Field::Value => value_clause(sql, params, "c.value", pattern),
        Field::Retract => {
            let flag = match pattern.trim_matches('"') {
                "true" => 1,
                "false" => 0,
                _ => return Err(Error::Retract(pattern.to_string())),
            };
            sql.push_str(" AND c.retract = ?");
            params.push(Sql::Integer(flag));
        }
    }
    Ok(())
}

/// The clause that takes every subject lacking nothing of `missing` out
/// of a `find`: one `EXCEPT` per entry, an attribute by name or, ending
/// in `:`, a whole namespace — asked of `rows`, the standing set or the
/// claims.
fn lacking(
    sql: &mut String,
    params: &mut Vec<rusqlite::types::Value>,
    missing: &[String],
    rows: &str,
) -> Result<()> {
    use rusqlite::types::Value as Sql;
    let text = |s: &str| Sql::Text(s.to_string());
    for absent in missing {
        let _ = write!(
            sql,
            " EXCEPT SELECT subject FROM {rows} WHERE attribute IN (SELECT id FROM attributes WHERE "
        );
        if let Some(namespace) = absent.strip_suffix(':') {
            // The grammar has one door; a prefix walks through it
            // wearing a dummy name.
            Attribute::parse(&format!("{namespace}:a"))?;
            // ';' is the character after ':', and no attribute
            // contains one: the half-open range is the namespace.
            sql.push_str("name >= ? AND name < ?)");
            params.push(text(&format!("{namespace}:")));
            params.push(text(&format!("{namespace};")));
        } else {
            let attribute = Attribute::parse(absent)?;
            sql.push_str("name = ?)");
            params.push(text(attribute.as_str()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use immure::Store;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    fn log_in(dir: &TempDir) -> Log {
        let store = Store::builder(dir.path().join("claims"))
            .suffix(".seg")
            .depth(1)
            .create()
            .unwrap();
        Log::new(store, dir.path().join("head.jsonl"))
    }

    fn index_in(dir: &TempDir) -> Index {
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        Index::open(cache.join("index.sqlite")).unwrap()
    }

    fn subject() -> Subject {
        Subject::parse("9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e").unwrap()
    }

    fn tag(tag: &str, time: &str) -> Claim {
        tag_about(subject(), tag, time)
    }

    fn tag_about(subject: Subject, tag: &str, time: &str) -> Claim {
        Claim::assert(
            subject,
            Attribute::parse("user:tag").unwrap(),
            json!(tag),
            Timestamp::parse(time).unwrap(),
            Source::parse("user").unwrap(),
            Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn a_fold_is_incremental_because_segments_never_change() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);

        log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
        log.seal().unwrap().unwrap();
        log.append(&tag("beach", "2026-09-01T21:14:04Z")).unwrap();

        let first = index.fold(&log).unwrap();
        assert_eq!(
            first,
            Folded {
                segments: 1,
                claims: 1,
                head: 1
            }
        );

        let again = index.fold(&log).unwrap();
        assert_eq!(
            again,
            Folded {
                segments: 0,
                claims: 0,
                head: 1
            },
            "the sealed segment is folded forever; only the head moves"
        );
    }

    #[test]
    fn about_answers_in_log_order_with_the_head_last() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);

        log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
        log.seal().unwrap().unwrap();
        log.append(&tag("beach", "2026-09-01T21:14:03Z")).unwrap();
        index.fold(&log).unwrap();

        let claims = index.about(&subject()).unwrap();
        assert_eq!(
            claims
                .iter()
                .map(|claim| claim.value().unwrap().as_str().unwrap())
                .collect::<Vec<_>>(),
            ["holiday", "beach"],
            "same second, and the sealed segment still comes before the head"
        );
    }

    #[test]
    fn retractions_come_back_as_the_claims_they_are() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);

        log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
        log.append(
            &Claim::retract_value(
                subject(),
                Attribute::parse("user:tag").unwrap(),
                json!("holiday"),
                Timestamp::parse("2030-04-01T10:00:00Z").unwrap(),
                Source::parse("user").unwrap(),
                Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        index.fold(&log).unwrap();

        let claims = index.about(&subject()).unwrap();
        assert_eq!(claims.len(), 2, "nothing is interpreted away");
        assert!(claims[1].is_retraction());
        assert_eq!(claims[1].value(), Some(&json!("holiday")));
    }

    #[test]
    fn a_beginning_names_the_subjects_it_begins() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        let near =
            Subject::parse("9f2ac41edd00000000000000000000000000000000000000000000000000ffff")
                .unwrap();
        log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
        log.seal().unwrap().unwrap();
        log.append(&tag_about(near.clone(), "beach", "2026-09-01T21:14:04Z"))
            .unwrap();
        index.fold(&log).unwrap();

        assert_eq!(
            index.matching("9f2ac41e").unwrap(),
            [subject(), near.clone()],
            "sealed or still in the head, each once, sorted"
        );
        assert_eq!(
            index.matching("9F2AC41ED").unwrap(),
            [near],
            "case falls away, the way subjects themselves are spelled"
        );
        assert_eq!(index.matching("ffff").unwrap(), Vec::new());
        assert_eq!(
            index.matching("9f2ac41e%").unwrap(),
            Vec::new(),
            "a wildcard is a character, and no digest contains one"
        );
    }

    fn say(subject: &Subject, attribute: &str, value: serde_json::Value, time: &str) -> Claim {
        Claim::assert(
            subject.clone(),
            Attribute::parse(attribute).unwrap(),
            value,
            Timestamp::parse(time).unwrap(),
            Source::parse("user").unwrap(),
            Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        )
        .unwrap()
    }

    fn term(attribute: &str, pattern: &str) -> Term {
        Term::Attribute(Attribute::parse(attribute).unwrap(), pattern.to_string())
    }

    /// A run id per letter, so a test can tell its runs apart.
    fn run_x(letter: char) -> Run {
        Run::parse(&format!("315e360b-020e-48be-8f2d-f2002a2ea9b{letter}")).unwrap()
    }

    /// One claim with its run chosen, the way a writer of this version
    /// writes one.
    fn said_in(subject: &Subject, attribute: &str, value: Value, time: &str, run: &Run) -> Claim {
        Claim::assert(
            subject.clone(),
            Attribute::parse(attribute).unwrap(),
            value,
            Timestamp::parse(time).unwrap(),
            Source::parse("ingest").unwrap(),
            run.clone(),
        )
        .unwrap()
    }

    fn field(field: Field, pattern: &str) -> Term {
        Term::Field(field, pattern.to_string())
    }

    #[test]
    #[allow(clippy::too_many_lines, reason = "one scene, asked seven ways")]
    fn a_field_term_asks_about_the_claim_behind_a_value() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        let (a, b) = (subject(), other());
        // Run a names file a; run b names file b and tags file a.
        log.append(&said_in(
            &a,
            "file:name",
            json!("abc"),
            "2026-09-01T10:00:00Z",
            &run_x('a'),
        ))
        .unwrap();
        log.append(&said_in(
            &b,
            "file:name",
            json!("abc"),
            "2026-09-02T10:00:00Z",
            &run_x('b'),
        ))
        .unwrap();
        log.append(&said_in(
            &a,
            "user:tag",
            json!("beach"),
            "2026-09-02T10:00:00Z",
            &run_x('b'),
        ))
        .unwrap();
        index.fold(&log).unwrap();

        let name = || term("file:name", "abc");
        assert_eq!(
            index
                .find(
                    &[field(Field::Run, run_x('a').as_str()), name()],
                    &[],
                    Scope::Held
                )
                .unwrap(),
            std::slice::from_ref(&a),
            "the name claim itself must come from the run"
        );
        assert_eq!(
            index
                .find(
                    &[field(Field::Run, run_x('b').as_str()), name()],
                    &[],
                    Scope::Held
                )
                .unwrap(),
            std::slice::from_ref(&b),
            "file a was touched by run b, but its name was not said there"
        );
        assert_eq!(
            index
                .find(&[field(Field::Run, run_x('b').as_str())], &[], Scope::Held)
                .unwrap(),
            [a.clone(), b.clone()],
            "a field term alone: every file with a standing value from that run"
        );
        assert_eq!(
            index
                .find(&[field(Field::Time, "2026-09-02..")], &[], Scope::Held)
                .unwrap(),
            [a.clone(), b.clone()],
            "time ranges read like attribute ranges, in claim time's spelling"
        );
        assert_eq!(
            index
                .find(
                    &[field(Field::Time, "..2026-09-01T23:59:59Z"), name()],
                    &[],
                    Scope::Held
                )
                .unwrap(),
            std::slice::from_ref(&a)
        );
        assert_eq!(
            index
                .find(&[field(Field::Source, "ing*")], &[], Scope::Held)
                .unwrap(),
            [a.clone(), b.clone()]
        );
        assert_eq!(
            index
                .find(
                    &[field(Field::Time, "..2026-09-01"), name()],
                    &[],
                    Scope::Held
                )
                .unwrap(),
            std::slice::from_ref(&a),
            "a range up to a date closes with the day, as --as-of does"
        );
        assert_eq!(
            index
                .find(&[field(Field::Time, "2026-09-02")], &[], Scope::Held)
                .unwrap(),
            [a.clone(), b.clone()],
            "a date alone is the whole day"
        );
        assert_eq!(
            index
                .find(
                    &[field(Field::Time, "2026-09-01T10:00:00")],
                    &[],
                    Scope::Held
                )
                .unwrap(),
            std::slice::from_ref(&a),
            "a moment without its Z is the moment"
        );
        assert!(
            matches!(
                index.find(&[field(Field::Time, "..yesterday")], &[], Scope::Held),
                Err(Error::Timestamp(given)) if given == "yesterday"
            ),
            "a bound that names no moment is refused, not compared as text"
        );
        assert_eq!(
            index
                .find(&[field(Field::Subject, "aa*"), name()], &[], Scope::Held)
                .unwrap(),
            std::slice::from_ref(&b),
            "subject is a field like any other, a beginning globbed"
        );
        assert_eq!(
            index.field_values(&a, Field::Run, Scope::Held).unwrap(),
            [json!(run_x('a').as_str()), json!(run_x('b').as_str())],
            "a bare field shows every run that stands behind a value"
        );
        assert!(matches!(
            index.find(&[field(Field::Retract, "maybe")], &[], Scope::Record),
            Err(Error::Retract(_))
        ));
    }

    #[test]
    fn a_value_said_again_answers_for_the_run_that_said_it_last() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        log.append(&said_in(
            &subject(),
            "file:name",
            json!("abc"),
            "2026-09-01T10:00:00Z",
            &run_x('a'),
        ))
        .unwrap();
        log.append(&said_in(
            &subject(),
            "file:name",
            json!("abc"),
            "2026-09-02T10:00:00Z",
            &run_x('b'),
        ))
        .unwrap();
        index.fold(&log).unwrap();

        let name = term("file:name", "abc");
        assert_eq!(
            index
                .find(
                    &[field(Field::Run, run_x('a').as_str()), name.clone()],
                    &[],
                    Scope::Held
                )
                .unwrap(),
            [],
            "the standing row points at the newest sayer"
        );
        assert_eq!(
            index
                .find(
                    &[field(Field::Run, run_x('a').as_str()), name],
                    &[],
                    Scope::Record
                )
                .unwrap(),
            [subject()],
            "the record still knows who said it first"
        );
    }

    #[test]
    fn the_record_answers_for_what_was_taken_back() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        log.append(&said_in(
            &subject(),
            "file:path",
            json!("/home/s/a.txt"),
            "2026-09-01T10:00:00Z",
            &run_x('a'),
        ))
        .unwrap();
        log.append(
            &Claim::retract_value(
                subject(),
                Attribute::parse("file:path").unwrap(),
                json!("/home/s/a.txt"),
                Timestamp::parse("2026-09-03T10:00:00Z").unwrap(),
                Source::parse("ingest").unwrap(),
                run_x('c'),
            )
            .unwrap(),
        )
        .unwrap();
        index.fold(&log).unwrap();

        let path = term("file:path", "*");
        assert_eq!(
            index
                .find(std::slice::from_ref(&path), &[], Scope::Present)
                .unwrap(),
            []
        );
        assert_eq!(
            index
                .find(std::slice::from_ref(&path), &[], Scope::Held)
                .unwrap(),
            []
        );
        assert_eq!(
            index
                .find(std::slice::from_ref(&path), &[], Scope::Record)
                .unwrap(),
            [subject()],
            "the record keeps the path, standing or not"
        );
        assert_eq!(
            index
                .find(
                    &[field(Field::Retract, "true"), path.clone()],
                    &[],
                    Scope::Record
                )
                .unwrap(),
            [subject()],
            "what was ever taken back, by the retraction claim itself"
        );
        assert_eq!(
            index
                .find(&[field(Field::Retract, "true"), path], &[], Scope::Held)
                .unwrap(),
            [],
            "a retraction never stands"
        );
        assert_eq!(
            index
                .values(
                    &subject(),
                    &Attribute::parse("file:path").unwrap(),
                    Scope::Record
                )
                .unwrap(),
            [json!("/home/s/a.txt")],
            "under the record a value said and taken back shows once"
        );
        assert_eq!(index.subjects(Scope::Record).unwrap(), [subject()]);
        assert_eq!(index.subjects(Scope::Held).unwrap(), []);
    }

    #[test]
    fn history_orders_runs_by_their_first_claim_across_a_seal() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        // Three runs within one second, begun c, b, a — and a goes on
        // past a seal, so its lowest position lies in a later segment.
        let when = "2026-09-01T10:00:00Z";
        for (letter, value) in [('c', "c"), ('b', "b"), ('a', "a")] {
            log.append(&said_in(
                &subject(),
                "user:tag",
                json!(value),
                when,
                &run_x(letter),
            ))
            .unwrap();
        }
        log.seal().unwrap().unwrap();
        log.append(&said_in(
            &subject(),
            "user:tag",
            json!("a again"),
            when,
            &run_x('a'),
        ))
        .unwrap();
        index.fold(&log).unwrap();

        let runs: Vec<Run> = index
            .history()
            .unwrap()
            .into_iter()
            .map(|episode| episode.run)
            .collect();
        assert_eq!(
            runs,
            [run_x('c'), run_x('b'), run_x('a')],
            "log order of the first claim, not the lowest segment and position apart"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one record, three runs and a runless claim"
    )]
    fn history_tells_each_run_from_its_first_claim_to_its_last() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        log.append(&said_in(
            &subject(),
            "file:name",
            json!("a.txt"),
            "2026-09-01T10:00:00Z",
            &run_x('a'),
        ))
        .unwrap();
        log.append(&said_in(
            &other(),
            "file:name",
            json!("b.txt"),
            "2026-09-01T10:00:07Z",
            &run_x('a'),
        ))
        .unwrap();
        log.append(
            &Claim::assert(
                subject(),
                Attribute::parse("user:tag").unwrap(),
                json!("beach"),
                Timestamp::parse("2026-09-01T10:00:07Z").unwrap(),
                Source::parse("user").unwrap(),
                run_x('a'),
            )
            .unwrap(),
        )
        .unwrap();
        log.append(
            &Claim::retract_value(
                other(),
                Attribute::parse("file:name").unwrap(),
                json!("b.txt"),
                Timestamp::parse("2026-09-02T10:00:00Z").unwrap(),
                Source::parse("ingest").unwrap(),
                run_x('b'),
            )
            .unwrap(),
        )
        .unwrap();
        // A claim from before runs were written belongs to no run.
        log.append(
            &Claim::recorded(
                subject(),
                Attribute::parse("user:tag").unwrap(),
                Some(json!("old")),
                Timestamp::parse("2026-08-01T10:00:00Z").unwrap(),
                Source::parse("user").unwrap(),
                None,
                false,
            )
            .unwrap(),
        )
        .unwrap();
        index.fold(&log).unwrap();

        assert_eq!(
            index.history().unwrap(),
            [
                Episode {
                    run: run_x('a'),
                    first: "2026-09-01T10:00:00Z".to_string(),
                    last: "2026-09-01T10:00:07Z".to_string(),
                    sources: vec![
                        Source::parse("ingest").unwrap(),
                        Source::parse("user").unwrap()
                    ],
                    files: 2,
                    claims: 3,
                    retractions: 0,
                },
                Episode {
                    run: run_x('b'),
                    first: "2026-09-02T10:00:00Z".to_string(),
                    last: "2026-09-02T10:00:00Z".to_string(),
                    sources: vec![Source::parse("ingest").unwrap()],
                    files: 1,
                    claims: 1,
                    retractions: 1,
                },
            ],
            "in log order, the runless claim in no episode"
        );
        assert_eq!(
            index
                .find(&[field(Field::Run, "*")], &[], Scope::Held)
                .unwrap(),
            [subject()],
            "every file with a standing value from a run; file b's name was taken back, and the runless tag counts for nothing"
        );
        assert_eq!(
            index
                .field_values(&subject(), Field::Run, Scope::Held)
                .unwrap(),
            [json!(run_x('a').as_str())],
            "the runless tag contributes nothing to run"
        );
        assert_eq!(
            index
                .field_values(&subject(), Field::Attribute, Scope::Held)
                .unwrap(),
            [json!("file:name"), json!("user:tag")]
        );
    }

    #[test]
    fn as_of_a_run_closes_after_its_last_claim() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        // Two runs within one second: a time cutoff cannot tell them apart.
        let when = "2026-09-01T10:00:00Z";
        log.append(&said_in(
            &subject(),
            "user:tag",
            json!("first"),
            when,
            &run_x('a'),
        ))
        .unwrap();
        log.append(&said_in(
            &subject(),
            "user:tag",
            json!("second"),
            when,
            &run_x('b'),
        ))
        .unwrap();
        index.fold(&log).unwrap();

        let tag = Attribute::parse("user:tag").unwrap();
        let after_a = index.as_of_run(&run_x('a')).unwrap().unwrap();
        assert_eq!(
            after_a.values(&subject(), &tag, Scope::Held).unwrap(),
            [json!("first")],
            "the view closes after run a's last claim, run b is not yet"
        );
        let after_b = index.as_of_run(&run_x('b')).unwrap().unwrap();
        assert_eq!(
            after_b.values(&subject(), &tag, Scope::Held).unwrap(),
            [json!("first"), json!("second")]
        );
        assert_eq!(
            index
                .as_of(when)
                .unwrap()
                .values(&subject(), &tag, Scope::Held)
                .unwrap(),
            [json!("first"), json!("second")],
            "the second holds both; only the run tells them apart"
        );
        assert!(
            index.as_of_run(&run_x('c')).unwrap().is_none(),
            "a run no claim carries is no cutoff"
        );

        // The one door: a run id or a moment, and the moment it closes at.
        let (view, closed) = index.at(run_x('a').as_str()).unwrap().unwrap();
        assert_eq!(
            view.values(&subject(), &tag, Scope::Held).unwrap(),
            [json!("first")]
        );
        assert_eq!(closed.as_str(), when, "a run closes at its last claim");
        let (view, closed) = index.at("2026-09-01").unwrap().unwrap();
        assert_eq!(
            view.values(&subject(), &tag, Scope::Held).unwrap(),
            [json!("first"), json!("second")]
        );
        assert_eq!(
            closed.as_str(),
            "2026-09-01T23:59:59Z",
            "a date alone closes at the day's end"
        );
        assert!(index.at(run_x('c').as_str()).unwrap().is_none());
        assert!(matches!(
            index.at("last week"),
            Err(Error::Timestamp(given)) if given == "last week"
        ));
    }

    #[test]
    fn a_value_said_twice_stands_once() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
        log.append(&tag("holiday", "2026-09-02T09:00:00Z")).unwrap();
        index.fold(&log).unwrap();

        assert_eq!(
            index
                .find(&[term("user:tag", "holiday")], &[], Scope::Held)
                .unwrap(),
            [subject()],
            "the set holds it once, however often it was said"
        );
        assert_eq!(
            index.about(&subject()).unwrap().len(),
            2,
            "while the history keeps every word"
        );
    }

    #[test]
    fn a_retracted_value_no_longer_answers() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
        log.append(&tag("crete", "2026-09-01T21:14:04Z")).unwrap();
        log.append(
            &Claim::retract_value(
                subject(),
                Attribute::parse("user:tag").unwrap(),
                json!("holiday"),
                Timestamp::parse("2026-09-02T10:00:00Z").unwrap(),
                Source::parse("user").unwrap(),
                Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        index.fold(&log).unwrap();

        assert_eq!(
            index
                .find(&[term("user:tag", "holiday")], &[], Scope::Held)
                .unwrap(),
            []
        );
        assert_eq!(
            index
                .find(&[term("user:tag", "crete")], &[], Scope::Held)
                .unwrap(),
            [subject()],
            "the neighbour value stands untouched"
        );
    }

    #[test]
    fn a_valueless_retraction_empties_the_attribute() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
        log.append(&tag("crete", "2026-09-01T21:14:04Z")).unwrap();
        log.append(&Claim::retract_attribute(
            subject(),
            Attribute::parse("user:tag").unwrap(),
            Timestamp::parse("2026-09-02T10:00:00Z").unwrap(),
            Source::parse("user").unwrap(),
            Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        ))
        .unwrap();
        index.fold(&log).unwrap();

        assert_eq!(
            index
                .find(&[term("user:tag", "crete")], &[], Scope::Held)
                .unwrap(),
            []
        );
    }

    #[test]
    fn a_retraction_seen_once_holds_across_refolds() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        // The assertion seals; the retraction stays in the open head,
        // which every fold applies afresh.
        log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
        log.seal().unwrap().unwrap();
        log.append(
            &Claim::retract_value(
                subject(),
                Attribute::parse("user:tag").unwrap(),
                json!("holiday"),
                Timestamp::parse("2026-09-02T10:00:00Z").unwrap(),
                Source::parse("user").unwrap(),
                Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
            )
            .unwrap(),
        )
        .unwrap();

        index.fold(&log).unwrap();
        index.fold(&log).unwrap();
        assert_eq!(
            index
                .find(&[term("user:tag", "holiday")], &[], Scope::Held)
                .unwrap(),
            [],
            "the sealed assertion is folded once and must not resurface"
        );
    }

    #[test]
    fn values_answers_the_standing_set_and_nothing_more() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
        log.append(&tag("crete", "2026-09-01T21:14:04Z")).unwrap();
        log.append(&tag("crete", "2026-09-02T08:00:00Z")).unwrap();
        log.append(&tag("beach", "2026-09-02T08:00:01Z")).unwrap();
        log.append(
            &Claim::retract_value(
                subject(),
                Attribute::parse("user:tag").unwrap(),
                json!("holiday"),
                Timestamp::parse("2026-09-03T10:00:00Z").unwrap(),
                Source::parse("user").unwrap(),
                Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        index.fold(&log).unwrap();

        let tags = Attribute::parse("user:tag").unwrap();
        assert_eq!(
            index.values(&subject(), &tags, Scope::Held).unwrap(),
            [json!("beach"), json!("crete")],
            "said twice stands once, retracted stands not at all; sorted by spelling"
        );
        assert_eq!(
            index
                .values(
                    &subject(),
                    &Attribute::parse("exif:model").unwrap(),
                    Scope::Held
                )
                .unwrap(),
            Vec::<Value>::new(),
            "an attribute never claimed has nothing standing"
        );
    }

    #[test]
    fn a_file_with_two_matching_values_answers_once() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        for (name, time) in [
            ("rebar1.jpg", "2026-09-01T10:00:00Z"),
            ("rebar4.jpg", "2026-09-01T10:00:01Z"),
        ] {
            log.append(&say(&subject(), "file:name", json!(name), time))
                .unwrap();
        }
        index.fold(&log).unwrap();

        assert_eq!(
            index
                .find(&[term("file:name", "*.jpg")], &[], Scope::Held)
                .unwrap(),
            [subject()],
            "one term, two values matching it, one file: the answer is a set of files"
        );
    }

    #[test]
    fn the_views_show_the_tables_with_names_in_place_of_ids() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
        log.seal().unwrap().unwrap();
        log.append(&tag("beach", "2026-09-01T21:14:04Z")).unwrap();
        index.fold(&log).unwrap();

        let rows: Vec<(String, String, String, String, String)> = index
            .connection
            .prepare("SELECT subject, attribute, value, source, segment FROM v_claims ORDER BY id")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, subject().as_str());
        assert_eq!(rows[0].1, "user:tag");
        assert_eq!(rows[0].2, "\"holiday\"", "the value as stored: JSON text");
        assert_eq!(rows[0].3, "user");
        assert_eq!(rows[0].4.len(), 64, "the sealed segment by its digest");
        assert_eq!(rows[1].4, "head");

        let standing: Vec<(String, String, String)> = index
            .connection
            .prepare("SELECT subject, attribute, value FROM v_standing ORDER BY value")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(
            standing,
            [
                (
                    subject().as_str().to_string(),
                    "user:tag".to_string(),
                    "\"beach\"".to_string()
                ),
                (
                    subject().as_str().to_string(),
                    "user:tag".to_string(),
                    "\"holiday\"".to_string()
                ),
            ]
        );
    }

    #[test]
    fn places_and_runs_read_as_tables() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        for (attribute, value) in [
            ("file:path", json!("/home/john/a.txt")),
            ("file:name", json!("a.txt")),
        ] {
            log.append(&say(&subject(), attribute, value, "2026-09-01T10:00:00Z"))
                .unwrap();
        }
        index.fold(&log).unwrap();

        let path: String = index
            .connection
            .query_row("SELECT path FROM v_places", [], |row| row.get(0))
            .unwrap();
        assert_eq!(path, "/home/john/a.txt", "the path bare, not as JSON text");

        let run: (String, i64, i64) = index
            .connection
            .query_row("SELECT run, files, claims FROM v_runs", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap();
        assert_eq!(
            run,
            ("315e360b-020e-48be-8f2d-f2002a2ea9b4".to_string(), 1, 2)
        );
    }

    #[test]
    fn two_sources_saying_one_value_stand_as_one() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        let mime = Attribute::parse("file:mime").unwrap();
        for (source, time) in [
            ("ingest", "2026-09-01T10:00:00Z"),
            ("extractor:mail/1", "2026-09-01T10:00:05Z"),
        ] {
            log.append(
                &Claim::assert(
                    subject(),
                    mime.clone(),
                    json!("message/rfc822"),
                    Timestamp::parse(time).unwrap(),
                    Source::parse(source).unwrap(),
                    Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
        }
        index.fold(&log).unwrap();

        assert_eq!(
            index.values(&subject(), &mime, Scope::Held).unwrap(),
            [json!("message/rfc822")],
            "who says it is the claim's business; the standing set holds the value once"
        );
        assert_eq!(
            index
                .find(&[term("file:mime", "message/rfc822")], &[], Scope::Held)
                .unwrap(),
            [subject()],
            "and a search finds the file once"
        );
    }

    #[test]
    fn as_of_answers_with_the_knowledge_of_that_day() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        log.append(&tag("holiday", "2026-09-01T10:00:00Z")).unwrap();
        log.seal().unwrap().unwrap();
        log.append(
            &Claim::retract_value(
                subject(),
                Attribute::parse("user:tag").unwrap(),
                json!("holiday"),
                Timestamp::parse("2026-09-03T10:00:00Z").unwrap(),
                Source::parse("user").unwrap(),
                Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        log.append(&tag("beach", "2026-09-04T10:00:00Z")).unwrap();
        index.fold(&log).unwrap();

        let tags = Attribute::parse("user:tag").unwrap();
        let early = index.as_of("2026-09-02T00:00:00Z").unwrap();
        assert_eq!(
            early.values(&subject(), &tags, Scope::Held).unwrap(),
            [json!("holiday")],
            "on the second, holiday stood and nothing had been taken back"
        );
        assert_eq!(
            early.about(&subject()).unwrap().len(),
            1,
            "the story as well ends at the cutoff"
        );

        let late = index.as_of("2026-09-03T12:00:00Z").unwrap();
        assert_eq!(
            late.values(&subject(), &tags, Scope::Held).unwrap(),
            Vec::<Value>::new(),
            "after the retraction nothing stands, and beach has not arrived yet"
        );
        assert_eq!(
            index.values(&subject(), &tags, Scope::Held).unwrap(),
            [json!("beach")],
            "the index itself keeps answering for today"
        );
    }

    #[test]
    fn standing_answers_the_whole_outcome_of_one_subject() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        log.append(&tag("crete", "2026-09-01T21:14:03Z")).unwrap();
        log.append(&tag("beach", "2026-09-01T21:14:04Z")).unwrap();
        log.append(&say(
            &subject(),
            "file:name",
            json!("dscn0042.jpg"),
            "2026-09-01T21:14:05Z",
        ))
        .unwrap();
        log.append(&tag("holiday", "2026-09-01T21:14:06Z")).unwrap();
        log.append(
            &Claim::retract_value(
                subject(),
                Attribute::parse("user:tag").unwrap(),
                json!("holiday"),
                Timestamp::parse("2026-09-02T10:00:00Z").unwrap(),
                Source::parse("user").unwrap(),
                Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        let other =
            Subject::parse("1111111111111111111111111111111111111111111111111111111111111111")
                .unwrap();
        log.append(&tag_about(other, "noise", "2026-09-02T10:00:01Z"))
            .unwrap();
        index.fold(&log).unwrap();

        let pairs = index.standing(&subject()).unwrap();
        assert_eq!(
            pairs,
            [
                (
                    Attribute::parse("file:name").unwrap(),
                    json!("dscn0042.jpg")
                ),
                (Attribute::parse("user:tag").unwrap(), json!("beach")),
                (Attribute::parse("user:tag").unwrap(), json!("crete")),
            ],
            "every attribute of this subject and no other's, retracted values gone, ordered by attribute then value"
        );
    }

    #[test]
    fn find_needs_every_term_to_stand() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        let other =
            Subject::parse("1111111111111111111111111111111111111111111111111111111111111111")
                .unwrap();
        log.append(&say(
            &subject(),
            "file:mime",
            json!("image/jpeg"),
            "2026-09-01T21:14:03Z",
        ))
        .unwrap();
        log.append(&tag("holiday", "2026-09-01T21:14:04Z")).unwrap();
        log.append(&say(
            &other,
            "file:mime",
            json!("image/jpeg"),
            "2026-09-01T21:14:05Z",
        ))
        .unwrap();
        index.fold(&log).unwrap();

        assert_eq!(
            index
                .find(
                    &[term("file:mime", "image/jpeg"), term("user:tag", "holiday")],
                    &[],
                    Scope::Held
                )
                .unwrap(),
            [subject()],
            "both terms, one subject; the untagged jpeg is not it"
        );
    }

    #[test]
    fn a_pattern_matches_within_string_values_only() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        log.append(&say(
            &subject(),
            "file:path",
            json!("/photos/2019/crete/beach.jpg"),
            "2026-09-01T21:14:03Z",
        ))
        .unwrap();
        log.append(&say(
            &subject(),
            "file:size",
            json!(2019),
            "2026-09-01T21:14:03Z",
        ))
        .unwrap();
        index.fold(&log).unwrap();

        assert_eq!(
            index
                .find(&[term("file:path", "*crete*")], &[], Scope::Held)
                .unwrap(),
            [subject()]
        );
        assert_eq!(
            index
                .find(&[term("file:size", "20*")], &[], Scope::Held)
                .unwrap(),
            [],
            "numbers were promised no wildcards"
        );
        assert_eq!(
            index
                .find(&[term("file:size", "2019")], &[], Scope::Held)
                .unwrap(),
            [subject()],
            "while the exact number answers"
        );
    }

    #[test]
    fn a_namespace_answers_whole_and_the_record_names_its_subjects() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        let when = "2026-09-01T21:14:03Z";
        log.append(&say(&subject(), "exif:model", json!("Pixel 7"), when))
            .unwrap();
        log.append(&say(&subject(), "exif:make", json!("Google"), when))
            .unwrap();
        log.append(&say(&subject(), "file:size", json!(2019), when))
            .unwrap();
        index.fold(&log).unwrap();

        let spelled: Vec<(String, Value)> = index
            .values_in(&subject(), "exif", Scope::Held)
            .unwrap()
            .into_iter()
            .map(|(attribute, value)| (attribute.as_str().to_string(), value))
            .collect();
        assert_eq!(
            spelled,
            [
                ("exif:make".to_string(), json!("Google")),
                ("exif:model".to_string(), json!("Pixel 7")),
            ],
            "the namespace whole, ordered by attribute"
        );
        assert_eq!(
            index.values_in(&subject(), "user", Scope::Held).unwrap(),
            [],
            "a namespace nothing stands in answers empty"
        );
        assert_eq!(
            index.subjects(Scope::Held).unwrap(),
            [subject()],
            "the record names every subject it speaks about"
        );
    }

    #[test]
    fn attributes_names_the_standing_words_and_counts_their_files() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        let other =
            Subject::parse("1111111111111111111111111111111111111111111111111111111111111111")
                .unwrap();
        let when = "2026-09-01T21:14:03Z";
        log.append(&say(&subject(), "file:mime", json!("image/jpeg"), when))
            .unwrap();
        log.append(&say(&other, "file:mime", json!("image/jpeg"), when))
            .unwrap();
        log.append(&say(&subject(), "user:tag", json!("holiday"), when))
            .unwrap();
        log.append(&say(&subject(), "user:tag", json!("beach"), when))
            .unwrap();
        log.append(&say(&other, "exif:make", json!("Google"), when))
            .unwrap();
        log.append(
            &Claim::retract_value(
                other.clone(),
                Attribute::parse("exif:make").unwrap(),
                json!("Google"),
                Timestamp::parse("2026-09-02T10:00:00Z").unwrap(),
                Source::parse("user").unwrap(),
                Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        index.fold(&log).unwrap();

        let spelled: Vec<(String, u64)> = index
            .attributes()
            .unwrap()
            .into_iter()
            .map(|(attribute, files)| (attribute.as_str().to_string(), files))
            .collect();
        assert_eq!(
            spelled,
            [("file:mime".to_string(), 2), ("user:tag".to_string(), 1)],
            "sorted by attribute; files are counted, not values — two tags on \
             one file are one file; a word retracted everywhere is gone"
        );
    }

    #[test]
    fn missing_names_what_a_subject_lacks() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        let bare =
            Subject::parse("1111111111111111111111111111111111111111111111111111111111111111")
                .unwrap();
        log.append(&say(
            &subject(),
            "file:mime",
            json!("image/jpeg"),
            "2026-09-01T21:14:03Z",
        ))
        .unwrap();
        log.append(&say(
            &subject(),
            "exif:make",
            json!("Google"),
            "2026-09-01T21:14:03Z",
        ))
        .unwrap();
        log.append(&say(
            &bare,
            "file:mime",
            json!("image/jpeg"),
            "2026-09-01T21:14:04Z",
        ))
        .unwrap();
        index.fold(&log).unwrap();

        assert_eq!(
            index
                .find(
                    &[term("file:mime", "image/jpeg")],
                    &["exif:".to_string()],
                    Scope::Held
                )
                .unwrap(),
            std::slice::from_ref(&bare),
            "the namespace prefix names the lack"
        );
        assert_eq!(
            index
                .find(&[], &["exif:make".to_string()], Scope::Held)
                .unwrap(),
            [bare],
            "with no terms, missing is asked of every subject"
        );
    }

    #[test]
    fn a_range_speaks_about_one_standing_value() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        let august =
            Subject::parse("1111111111111111111111111111111111111111111111111111111111111111")
                .unwrap();
        let december =
            Subject::parse("2222222222222222222222222222222222222222222222222222222222222222")
                .unwrap();
        let both =
            Subject::parse("3333333333333333333333333333333333333333333333333333333333333333")
                .unwrap();
        let when = "2026-09-01T00:00:00Z";
        log.append(&say(
            &august,
            "file:modified",
            json!("2026-08-15T10:00:00Z"),
            when,
        ))
        .unwrap();
        log.append(&say(
            &december,
            "file:modified",
            json!("2026-12-24T10:00:00Z"),
            when,
        ))
        .unwrap();
        log.append(&say(
            &both,
            "file:modified",
            json!("2026-08-15T10:00:00Z"),
            when,
        ))
        .unwrap();
        log.append(&say(
            &both,
            "file:modified",
            json!("2026-12-24T10:00:00Z"),
            when,
        ))
        .unwrap();
        index.fold(&log).unwrap();

        assert_eq!(
            index
                .find(&[term("file:modified", "2026-09-01..")], &[], Scope::Held)
                .unwrap(),
            [december.clone(), both.clone()],
            "since: one standing value past the bound suffices"
        );
        assert_eq!(
            index
                .find(
                    &[term("file:modified", "2026-09-01..2026-10-01")],
                    &[],
                    Scope::Held
                )
                .unwrap(),
            [],
            "one range term wants a single value inside — nobody has one"
        );
        assert_eq!(
            index
                .find(
                    &[
                        term("file:modified", "2026-09-01.."),
                        term("file:modified", "..2026-10-01"),
                    ],
                    &[],
                    Scope::Held
                )
                .unwrap(),
            [both],
            "two terms may be satisfied by two different values"
        );
    }

    #[test]
    fn a_numeric_range_reads_numbers_only() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        let worded =
            Subject::parse("1111111111111111111111111111111111111111111111111111111111111111")
                .unwrap();
        log.append(&say(
            &subject(),
            "user:rating",
            json!(7),
            "2026-09-01T00:00:00Z",
        ))
        .unwrap();
        log.append(&say(
            &worded,
            "user:rating",
            json!("7ish"),
            "2026-09-01T00:00:00Z",
        ))
        .unwrap();
        index.fold(&log).unwrap();

        assert_eq!(
            index
                .find(&[term("user:rating", "1..10")], &[], Scope::Held)
                .unwrap(),
            [subject()],
            "numeric bounds speak about numbers; the worded rating is not seven"
        );
    }

    #[test]
    fn a_verbatim_spelling_ranges_in_its_own_order() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        log.append(&say(
            &subject(),
            "exif:date-time-original",
            json!("2026:07:25 23:09:09"),
            "2026-09-01T00:00:00Z",
        ))
        .unwrap();
        index.fold(&log).unwrap();

        assert_eq!(
            index
                .find(
                    &[term("exif:date-time-original", "2026:07:01..2026:08:01")],
                    &[],
                    Scope::Held
                )
                .unwrap(),
            [subject()],
            "EXIF's own spelling is fixed-width and sorts chronologically"
        );
        assert_eq!(
            index
                .find(
                    &[term("exif:date-time-original", "2026:08:01..")],
                    &[],
                    Scope::Held
                )
                .unwrap(),
            []
        );
    }

    #[test]
    fn quotes_take_a_value_literally() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        log.append(&say(
            &subject(),
            "user:note",
            json!("see 3..4"),
            "2026-09-01T00:00:00Z",
        ))
        .unwrap();
        index.fold(&log).unwrap();

        assert_eq!(
            index
                .find(&[term("user:note", "\"see 3..4\"")], &[], Scope::Held)
                .unwrap(),
            [subject()],
            "quoted, the dots are just dots"
        );
        assert_eq!(
            index
                .find(&[term("user:note", "\"see 3\"")], &[], Scope::Held)
                .unwrap(),
            [],
            "and quoted means whole, not prefix"
        );
    }

    #[test]
    fn a_bare_range_asks_only_that_the_attribute_stands() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        let bare =
            Subject::parse("1111111111111111111111111111111111111111111111111111111111111111")
                .unwrap();
        log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
        log.append(&say(
            &bare,
            "file:mime",
            json!("image/jpeg"),
            "2026-09-01T21:14:04Z",
        ))
        .unwrap();
        index.fold(&log).unwrap();

        assert_eq!(
            index
                .find(&[term("user:tag", "..")], &[], Scope::Held)
                .unwrap(),
            [subject()],
            "the presence question, --missing turned around"
        );
    }

    #[test]
    fn under_answers_component_wise() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        let neighbour =
            Subject::parse("1111111111111111111111111111111111111111111111111111111111111111")
                .unwrap();
        let deeper =
            Subject::parse("2222222222222222222222222222222222222222222222222222222222222222")
                .unwrap();
        let when = "2026-09-01T21:14:03Z";
        log.append(&say(
            &subject(),
            "file:path",
            json!("/home/john/a.txt"),
            when,
        ))
        .unwrap();
        log.append(&say(
            &neighbour,
            "file:path",
            json!("/home/johnny/b.txt"),
            when,
        ))
        .unwrap();
        log.append(&say(
            &deeper,
            "file:path",
            json!("/home/john/photos/c.jpg"),
            when,
        ))
        .unwrap();
        index.fold(&log).unwrap();

        assert_eq!(
            index.under("/home/john").unwrap(),
            [
                ("/home/john/a.txt".to_string(), subject()),
                ("/home/john/photos/c.jpg".to_string(), deeper.clone()),
            ],
            "a place bounds by component: johnny is a neighbour, not a child"
        );
        assert_eq!(
            index.under("/home/john/a.txt").unwrap(),
            [("/home/john/a.txt".to_string(), subject())],
            "a place that is a file answers with itself"
        );
        assert_eq!(
            index.under("/").unwrap().len(),
            3,
            "the root is over everything"
        );
        assert_eq!(index.under("/mnt").unwrap(), []);
    }

    #[test]
    fn under_reads_standing_string_places_only() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        let when = "2026-09-01T21:14:03Z";
        log.append(&say(
            &subject(),
            "file:path",
            json!("/home/john/a.txt"),
            when,
        ))
        .unwrap();
        log.append(&say(&subject(), "file:path", json!(42), when))
            .unwrap();
        log.append(
            &Claim::retract_value(
                subject(),
                Attribute::parse("file:path").unwrap(),
                json!("/home/john/a.txt"),
                Timestamp::parse("2026-09-02T10:00:00Z").unwrap(),
                Source::parse("user").unwrap(),
                Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        index.fold(&log).unwrap();

        assert_eq!(
            index.under("/").unwrap(),
            [],
            "the retracted place no longer stands, and a number names none"
        );
    }

    #[test]
    fn standing_as_of_replays_to_a_closing_time() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        let place = json!("/home/john/a.txt");
        log.append(&say(
            &subject(),
            "file:path",
            place.clone(),
            "2026-01-01T00:00:00Z",
        ))
        .unwrap();
        log.append(
            &Claim::retract_value(
                subject(),
                Attribute::parse("file:path").unwrap(),
                place.clone(),
                Timestamp::parse("2026-02-01T00:00:00Z").unwrap(),
                Source::parse("user").unwrap(),
                Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        log.append(&say(
            &subject(),
            "file:path",
            place.clone(),
            "2026-03-01T00:00:00Z",
        ))
        .unwrap();
        index.fold(&log).unwrap();

        let at = |cutoff| index.standing_as_of("file:path", cutoff).unwrap();
        assert_eq!(
            (
                at(Some("2026-01-15T00:00:00Z"))[0].value.clone(),
                at(Some("2026-01-15T00:00:00Z"))[0].asserted.clone()
            ),
            (place.clone(), "2026-01-01T00:00:00Z".to_string()),
            "before the retraction the first assertion stands"
        );
        assert_eq!(
            at(Some("2026-02-15T00:00:00Z")),
            [],
            "at a cutoff behind the retraction nothing stands"
        );
        let today = at(None);
        assert_eq!(
            (
                today[0].value.clone(),
                today[0].asserted.clone(),
                today[0].order
            ),
            (place, "2026-03-01T00:00:00Z".to_string(), 3),
            "no cutoff answers for today, and the surviving assertion names its moment and its row, the third claim folded"
        );
    }

    #[test]
    fn standing_as_of_renews_the_moment_and_empties_whole_attributes() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        log.append(&tag("holiday", "2026-01-01T00:00:00Z")).unwrap();
        log.append(&tag("beach", "2026-01-02T00:00:00Z")).unwrap();
        log.append(&tag("holiday", "2026-01-03T00:00:00Z")).unwrap();
        log.append(&Claim::retract_attribute(
            subject(),
            Attribute::parse("user:tag").unwrap(),
            Timestamp::parse("2026-01-04T00:00:00Z").unwrap(),
            Source::parse("user").unwrap(),
            Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        ))
        .unwrap();
        index.fold(&log).unwrap();

        let before = index
            .standing_as_of("user:tag", Some("2026-01-03T00:00:00Z"))
            .unwrap();
        assert_eq!(
            before
                .iter()
                .map(|standing| (standing.value.clone(), standing.asserted.as_str()))
                .collect::<Vec<_>>(),
            [
                (json!("beach"), "2026-01-02T00:00:00Z"),
                (json!("holiday"), "2026-01-03T00:00:00Z"),
            ],
            "a repeated assertion collapses in the set but renews its moment"
        );
        assert_eq!(
            index.standing_as_of("user:tag", None).unwrap(),
            [],
            "a valueless retraction empties the attribute in the replay too"
        );
    }

    /// One sighting the way ingest writes one: place, name and run
    /// with one moment and one source.
    fn sight(log: &Log, subject: &Subject, run: &Run, path: &str, time: &str) {
        let time = Timestamp::parse(time).unwrap();
        let source = Source::parse("ingest").unwrap();
        let name = path.rsplit('/').next().unwrap();
        for (attribute, value) in [("file:path", json!(path)), ("file:name", json!(name))] {
            log.append(
                &Claim::assert(
                    subject.clone(),
                    Attribute::parse(attribute).unwrap(),
                    value,
                    time.clone(),
                    source.clone(),
                    run.clone(),
                )
                .unwrap(),
            )
            .unwrap();
        }
    }

    #[test]
    fn run_sightings_answer_with_the_runs_own_places() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        let moved =
            Subject::parse("1111111111111111111111111111111111111111111111111111111111111111")
                .unwrap();
        sight(
            &log,
            &subject(),
            &run_x('a'),
            "/home/s/a.txt",
            "2026-09-01T10:00:00Z",
        );
        sight(
            &log,
            &moved,
            &run_x('a'),
            "/home/s/b.txt",
            "2026-09-01T10:00:01Z",
        );
        // The same content, met again by a later run somewhere else: its
        // path belongs to that run, not to run-a.
        sight(
            &log,
            &subject(),
            &run_x('b'),
            "/mnt/nas/a.txt",
            "2026-09-02T10:00:00Z",
        );
        index.fold(&log).unwrap();

        assert_eq!(
            index.run_sightings(&run_x('a')).unwrap(),
            [
                (moved, Placement::Path("/home/s/b.txt".to_string())),
                (subject(), Placement::Path("/home/s/a.txt".to_string())),
            ],
            "each run answers with the places it recorded itself, by subject"
        );
        assert_eq!(
            index.run_sightings(&run_x('b')).unwrap(),
            [(subject(), Placement::Path("/mnt/nas/a.txt".to_string()))]
        );
        assert_eq!(index.run_sightings(&run_x('c')).unwrap(), []);

        // A run that only took something back is on the record and names
        // no file; only has_run tells it from a run that never was.
        let run = run_x('d');
        log.append(
            &Claim::retract_value(
                subject(),
                Attribute::parse("file:path").unwrap(),
                json!("/home/s/a.txt"),
                Timestamp::parse("2026-09-03T10:00:00Z").unwrap(),
                Source::parse("ingest").unwrap(),
                run.clone(),
            )
            .unwrap(),
        )
        .unwrap();
        index.fold(&log).unwrap();
        assert_eq!(index.run_sightings(&run).unwrap(), []);
        assert!(index.has_run(&run).unwrap());
        assert!(!index.has_run(&run_x('c')).unwrap());
    }

    #[test]
    fn one_run_seeing_two_places_answers_twice() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        sight(
            &log,
            &subject(),
            &run_x('a'),
            "/home/s/a.txt",
            "2026-09-01T10:00:00Z",
        );
        sight(
            &log,
            &subject(),
            &run_x('a'),
            "/home/s/copy/a.txt",
            "2026-09-01T10:00:02Z",
        );
        index.fold(&log).unwrap();

        assert_eq!(
            index.run_sightings(&run_x('a')).unwrap(),
            [
                (subject(), Placement::Path("/home/s/a.txt".to_string())),
                (subject(), Placement::Path("/home/s/copy/a.txt".to_string())),
            ],
            "the run's reality had two, and the record says so"
        );
    }

    #[test]
    fn a_sighting_without_a_path_answers_with_its_names() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        let time = Timestamp::parse("2026-09-01T10:00:00Z").unwrap();
        let source = Source::parse("extractor:mail/0.1.0").unwrap();
        log.append(
            &Claim::assert(
                subject(),
                Attribute::parse("file:name").unwrap(),
                json!("invoice.pdf"),
                time.clone(),
                source.clone(),
                run_x('e'),
            )
            .unwrap(),
        )
        .unwrap();
        index.fold(&log).unwrap();

        assert_eq!(
            index.run_sightings(&run_x('e')).unwrap(),
            [(subject(), Placement::Name("invoice.pdf".to_string()))],
            "a derived file never sat anywhere, so its name answers"
        );
    }

    #[test]
    fn an_older_cache_is_emptied_and_refolds() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let path = {
            let cache = dir.path().join("cache");
            std::fs::create_dir_all(&cache).unwrap();
            cache.join("index.sqlite")
        };
        log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
        log.seal().unwrap().unwrap();
        {
            let mut index = Index::open(&path).unwrap();
            index.fold(&log).unwrap();
        }
        // An index file from before this schema announces an older
        // generation; opening it starts over instead of guessing.
        Connection::open(&path)
            .unwrap()
            .execute_batch("PRAGMA user_version = 0")
            .unwrap();

        let mut index = Index::open(&path).unwrap();
        let folded = index.fold(&log).unwrap();
        assert_eq!(folded.segments, 1, "the emptied cache folds from scratch");
        assert_eq!(
            index
                .find(&[term("user:tag", "holiday")], &[], Scope::Held)
                .unwrap(),
            [subject()]
        );
    }

    #[test]
    fn a_subject_the_log_never_mentioned_has_nothing_to_say() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        index.fold(&log).unwrap();

        let unknown =
            Subject::parse("00000000000000000000000000000000000000000000000000000000000000ff")
                .unwrap();
        assert_eq!(index.about(&unknown).unwrap(), Vec::new());
    }

    fn other() -> Subject {
        Subject::parse("aa2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e").unwrap()
    }

    fn said(subject: Subject, attribute: &str, value: Value, time: &str) -> Claim {
        Claim::assert(
            subject,
            Attribute::parse(attribute).unwrap(),
            value,
            Timestamp::parse(time).unwrap(),
            Source::parse("test").unwrap(),
            Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        )
        .unwrap()
    }

    fn taken_back(subject: Subject, attribute: &str, value: Value, time: &str) -> Claim {
        Claim::retract_value(
            subject,
            Attribute::parse(attribute).unwrap(),
            value,
            Timestamp::parse(time).unwrap(),
            Source::parse("test").unwrap(),
            Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn a_file_answers_only_while_a_place_stands_on_it() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        let placed = subject();
        let placeless = other();
        log.append(&tag_about(
            placed.clone(),
            "holiday",
            "2026-09-01T10:00:00Z",
        ))
        .unwrap();
        log.append(&said(
            placed.clone(),
            "file:path",
            json!("/x/a"),
            "2026-09-01T10:00:00Z",
        ))
        .unwrap();
        log.append(&tag_about(
            placeless.clone(),
            "holiday",
            "2026-09-01T10:00:00Z",
        ))
        .unwrap();
        index.fold(&log).unwrap();

        let holiday = [term("user:tag", "holiday")];
        assert_eq!(
            index.find(&holiday, &[], Scope::Present).unwrap(),
            vec![placed.clone()],
            "no place stands on the other"
        );
        assert_eq!(
            index.find(&holiday, &[], Scope::Held).unwrap(),
            vec![placed.clone(), placeless.clone()]
        );
        assert_eq!(
            index.subjects(Scope::Present).unwrap(),
            vec![placed.clone()]
        );

        log.append(&taken_back(
            placed.clone(),
            "file:path",
            json!("/x/a"),
            "2026-09-02T10:00:00Z",
        ))
        .unwrap();
        index.fold(&log).unwrap();

        assert!(
            index
                .find(&holiday, &[], Scope::Present)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            index.find(&holiday, &[], Scope::Held).unwrap(),
            vec![placed.clone(), placeless],
            "held all the same"
        );
        let before = index.as_of("2026-09-01T23:59:59Z").unwrap();
        assert_eq!(
            before.find(&holiday, &[], Scope::Present).unwrap(),
            vec![placed],
            "as of the day it lay there, it did"
        );
    }

    #[test]
    fn a_derived_file_is_placed_while_its_origin_is() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        let origin = subject();
        let derived = other();
        log.append(&said(
            origin.clone(),
            "file:path",
            json!("/x/mail.eml"),
            "2026-09-01T10:00:00Z",
        ))
        .unwrap();
        log.append(&said(
            derived.clone(),
            "derive:derived-from",
            json!(origin.as_str()),
            "2026-09-01T10:00:00Z",
        ))
        .unwrap();
        log.append(&tag_about(
            derived.clone(),
            "invoice",
            "2026-09-01T10:00:00Z",
        ))
        .unwrap();
        index.fold(&log).unwrap();

        let invoice = [term("user:tag", "invoice")];
        assert_eq!(
            index.find(&invoice, &[], Scope::Present).unwrap(),
            vec![derived.clone()],
            "an attachment lies where its mail lies"
        );

        log.append(&taken_back(
            origin,
            "file:path",
            json!("/x/mail.eml"),
            "2026-09-02T10:00:00Z",
        ))
        .unwrap();
        index.fold(&log).unwrap();
        assert!(
            index
                .find(&invoice, &[], Scope::Present)
                .unwrap()
                .is_empty(),
            "and goes where its mail goes"
        );
    }

    #[test]
    fn a_message_at_a_mailbox_place_is_placed() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        let message = subject();
        log.append(&said(
            message.clone(),
            "mailbox:place",
            json!("example.org/INBOX"),
            "2026-09-01T10:00:00Z",
        ))
        .unwrap();
        log.append(&tag_about(
            message.clone(),
            "holiday",
            "2026-09-01T10:00:00Z",
        ))
        .unwrap();
        index.fold(&log).unwrap();

        assert_eq!(
            index
                .find(&[term("user:tag", "holiday")], &[], Scope::Present)
                .unwrap(),
            vec![message]
        );
    }
}
