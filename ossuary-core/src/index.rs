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
//! [`find`](Index::find) reads. Subjects, attributes, sources and
//! segments stand in tables of their own and appear in the two big
//! tables as integer ids: a digest is 64 bytes and a segment name the
//! same, and either repeated a quarter of a million times is most of a
//! file. Four views are for a look with `sqlite3`, and nothing here
//! reads them: `v_claims` and `v_standing` show both tables with the
//! names in place of the ids, `v_places` every standing `file:path`
//! unquoted, `v_arrivals` what each `prov:run` took in. What stays deliberately un-baked is *narrowing*:
//! which of several standing values a reader prefers is query-time
//! policy, and the sets carry them all.
//!
//! Standing follows the log forward, the only direction a log moves; a
//! `head.jsonl` edited backwards leaves it stale until the cache is
//! deleted and refolded — the cure every cache here has.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, params};

use crate::claim::{Attribute, Claim, Source, Subject, Timestamp, Value};
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
                so.name AS source, c.retract, s.digest AS segment, s.seq, c.position
         FROM claims c
         JOIN subjects su ON su.id = c.subject
         JOIN attributes a ON a.id = c.attribute
         JOIN sources so ON so.id = c.source
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
     CREATE VIEW IF NOT EXISTS v_arrivals AS
         SELECT json_extract(c.value, '$') AS run, so.name AS source,
                COUNT(DISTINCT c.subject) AS files, MIN(c.time) AS first, MAX(c.time) AS last
         FROM claims c
         JOIN sources so ON so.id = c.source
         WHERE c.attribute = (SELECT id FROM attributes WHERE name = 'prov:run')
           AND c.retract = 0
         GROUP BY c.value, c.source;";

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

/// Which files a question is about: those still lying somewhere, or
/// every file the archive holds.
///
/// A file is *placed* while a place stands on it — a `file:path` from a
/// walk, a `mailbox:place` from a fetch — or, for what a tool won out of
/// another file, while its origin is placed, along `derive:derived-from`
/// as far as it goes. A file whose every place was taken back is still
/// held, and still answers `--as-of` a day it lay somewhere, but it is
/// not part of the present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// Files at a place of their own, or won out of one that is.
    Placed,
    /// Every file the archive holds, at a place or not.
    Held,
}

/// The recursive table of placed subjects, for a query to open with:
/// every subject a place stands on, and every subject derived from one
/// of those.
const PLACED: &str = "WITH RECURSIVE placed(subject) AS (
        SELECT subject FROM standing
         WHERE attribute IN (SELECT id FROM attributes WHERE name IN ('file:path', 'mailbox:place'))
        UNION
        SELECT st.subject FROM standing st
          JOIN subjects su ON su.digest = json_extract(st.value, '$')
          JOIN placed ON placed.subject = su.id
         WHERE st.attribute = (SELECT id FROM attributes WHERE name = 'derive:derived-from')
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
                 DROP VIEW IF EXISTS v_arrivals;
                 DROP TABLE IF EXISTS standing;
                 DROP TABLE IF EXISTS claims;
                 DROP TABLE IF EXISTS segments;
                 DROP TABLE IF EXISTS subjects;
                 DROP TABLE IF EXISTS attributes;
                 DROP TABLE IF EXISTS sources;
                 PRAGMA user_version = {SCHEMA};"
            ))?;
        }
        // The integer columns of `claims` and `standing` are ids into the
        // four name tables; `standing.claim` is a row of `claims`. None of
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
        let mut statement = self.connection.prepare(
            "SELECT su.digest, a.name, c.value, c.time, so.name, c.retract, c.segment, c.position
             FROM claims c
             JOIN subjects su ON su.id = c.subject
             JOIN attributes a ON a.id = c.attribute
             JOIN sources so ON so.id = c.source
             JOIN segments s ON s.id = c.segment
             WHERE c.time <= ?1
             ORDER BY c.time, s.seq, c.position",
        )?;
        let rows = statement.query_map(params![cutoff], |row| {
            Ok((
                (
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, bool>(5)?,
                ),
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
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
            "SELECT su.digest, a.name, c.value, c.time, so.name, c.retract
             FROM claims c
             JOIN subjects su ON su.id = c.subject
             JOIN attributes a ON a.id = c.attribute
             JOIN sources so ON so.id = c.source
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
                row.get::<_, bool>(5)?,
            ))
        })?;
        let mut claims = Vec::new();
        for row in rows {
            claims.push(claim(row?)?);
        }
        Ok(claims)
    }

    /// One subject's standing values for one attribute, sorted by their
    /// stored spelling — as of the last [`fold`](Index::fold), the open
    /// head included.
    ///
    /// Where [`about`](Index::about) answers with the history, this
    /// answers with the outcome: retractions already applied, repeats
    /// already collapsed. Which of several standing values a reader
    /// prefers stays query-time policy, so they all come back.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; a value that does not parse back
    /// cannot happen for rows a fold wrote, but is propagated rather than
    /// sworn away.
    pub fn values(&self, subject: &Subject, attribute: &Attribute) -> Result<Vec<Value>> {
        let mut statement = self.connection.prepare(
            "SELECT value FROM standing
             WHERE subject = (SELECT id FROM subjects WHERE digest = ?1)
               AND attribute = (SELECT id FROM attributes WHERE name = ?2)
             ORDER BY value",
        )?;
        let rows = statement.query_map(params![subject.as_str(), attribute.as_str()], |row| {
            row.get::<_, String>(0)
        })?;
        let mut values = Vec::new();
        for row in rows {
            values.push(serde_json::from_str(&row?)?);
        }
        Ok(values)
    }

    /// What currently stands on one subject across a whole namespace:
    /// every standing `(attribute, value)` whose attribute begins
    /// `namespace:`, ordered by attribute and value — as of the last
    /// [`fold`](Index::fold), the open head included. The namespace comes
    /// bare, without its colon.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; a row that does not parse back
    /// cannot happen for rows a fold wrote, but is propagated rather than
    /// sworn away.
    pub fn values_in(&self, subject: &Subject, namespace: &str) -> Result<Vec<(Attribute, Value)>> {
        let mut statement = self.connection.prepare(
            // ';' is the character after ':', and no attribute contains
            // one: the half-open range is the namespace.
            "SELECT a.name, st.value
             FROM standing st JOIN attributes a ON a.id = st.attribute
             WHERE st.subject = (SELECT id FROM subjects WHERE digest = ?1)
               AND a.name >= ?2 AND a.name < ?3
             ORDER BY a.name, st.value",
        )?;
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
    pub fn subjects(&self, presence: Presence) -> Result<Vec<Subject>> {
        let mut sql = String::new();
        if presence == Presence::Placed {
            sql.push_str(PLACED);
        }
        sql.push_str(
            "SELECT digest FROM subjects su
             WHERE EXISTS (SELECT 1 FROM standing st WHERE st.subject = su.id)",
        );
        if presence == Presence::Placed {
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

    /// Every subject on which all `terms` stand and none of `missing` does,
    /// sorted — as of the last [`fold`](Index::fold), the open head
    /// included.
    ///
    /// A term is an attribute and a value, and the value is read in this
    /// order: wrapped in double quotes it is *literal* — exactly that
    /// string, the way to name a value that looks like a glob or a range;
    /// with `*` or `?` it is a glob, matching within string values only;
    /// with `..` it is a range, `low..high` with either side open —
    /// bounds compare in the attribute's own spelling, lexicographically
    /// for strings and numerically for numbers, the low end inclusive,
    /// and a bare `..` asks only that the attribute stands at all;
    /// otherwise it is exact — a string, or the bare JSON scalar for
    /// numbers and booleans, either spelling answering. Every term must
    /// hold, each on *some* standing value: two ranged terms on one
    /// attribute may be satisfied by two different values, where one
    /// `low..high` term speaks about a single value lying between.
    ///
    /// Each entry of `missing` names an attribute the subject must lack;
    /// ending in `:` it names a whole namespace. With no terms at all,
    /// `missing` is asked of every subject the log speaks about.
    ///
    /// Only *standing* values answer: a retracted value finds nothing,
    /// however long its claim stays in the log — this is where the set
    /// semantics first faces a reader. And only files of the asked
    /// [`Presence`] answer: [`Presence::Placed`] leaves out every file
    /// no place stands on any more.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; an entry of `missing` that fits
    /// neither the attribute grammar nor `namespace:` is refused with the
    /// grammar's own error.
    pub fn find(
        &self,
        terms: &[(Attribute, String)],
        missing: &[String],
        presence: Presence,
    ) -> Result<Vec<Subject>> {
        use rusqlite::types::Value as Sql;
        if terms.is_empty() && missing.is_empty() {
            return Ok(Vec::new());
        }
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
        // The terms meet as sets of subject ids; the names come last.
        let mut sql = String::new();
        if presence == Presence::Placed {
            sql.push_str(PLACED);
        }
        sql.push_str("SELECT digest FROM subjects WHERE id IN (");
        let mut params: Vec<Sql> = Vec::new();
        if terms.is_empty() {
            sql.push_str("SELECT DISTINCT subject FROM standing");
        }
        for (position, (attribute, pattern)) in terms.iter().enumerate() {
            if position > 0 {
                sql.push_str(" INTERSECT ");
            }
            sql.push_str(
                "SELECT subject FROM standing
                 WHERE attribute = (SELECT id FROM attributes WHERE name = ?)",
            );
            params.push(text(attribute.as_str()));
            if let Some(literal) = pattern
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
            {
                // Wrapped in quotes: exactly this string, nothing read
                // into it — the door for values that look like globs or
                // ranges.
                sql.push_str(" AND value = ?");
                params.push(quoted(literal));
            } else if pattern.contains('*') || pattern.contains('?') {
                // The stored string spelling is quoted JSON, so a pattern
                // globbed inside quotes matches string values and nothing
                // else — numbers were promised no wildcards.
                sql.push_str(" AND value GLOB ?");
                params.push(text(&format!("\"{pattern}\"")));
            } else if let Some((low, high)) = pattern.split_once("..") {
                let numeric = [low, high]
                    .iter()
                    .filter(|bound| !bound.is_empty())
                    .all(|bound| number(bound).is_some());
                if low.is_empty() && high.is_empty() {
                    // A bare "..": any standing value at all.
                } else if numeric {
                    sql.push_str(" AND json_type(value) IN ('integer', 'real')");
                    if let Some(bound) = number(low) {
                        sql.push_str(" AND json_extract(value, '$') >= ?");
                        params.push(bound);
                    }
                    if let Some(bound) = number(high) {
                        sql.push_str(" AND json_extract(value, '$') <= ?");
                        params.push(bound);
                    }
                } else {
                    // Bounds compare in the value's own spelling; the
                    // type guard keeps numbers out, whose storage sorts
                    // below every quoted string.
                    sql.push_str(" AND json_type(value) = 'text'");
                    if !low.is_empty() {
                        sql.push_str(" AND value >= ?");
                        params.push(quoted(low));
                    }
                    if !high.is_empty() {
                        sql.push_str(" AND value <= ?");
                        params.push(quoted(high));
                    }
                }
            } else {
                match serde_json::from_str::<Value>(pattern) {
                    // The bare word is a JSON scalar — a number, a
                    // boolean: it may stand as itself or as a string, and
                    // either spelling answers.
                    Ok(scalar) if !scalar.is_string() => {
                        sql.push_str(" AND value IN (?, ?)");
                        params.push(text(&scalar.to_string()));
                        params.push(quoted(pattern));
                    }
                    _ => {
                        sql.push_str(" AND value = ?");
                        params.push(quoted(pattern));
                    }
                }
            }
        }
        lacking(&mut sql, &mut params, missing)?;
        if presence == Presence::Placed {
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

    /// Every sighting one run put on the record, with where it saw the
    /// file: one pair per place, the same content seen at two places in
    /// one run answering twice. An ingest sighting answers with its
    /// `file:path`; a sighting without one — a derived file never sat
    /// anywhere — answers with its `file:name`, every name the sighting
    /// spelled.
    ///
    /// This asks the history, not the standing set: which place belongs
    /// to which run is told by the claims written together — a sighting
    /// speaks with one moment and one source — and a run's record stays
    /// its record, later retractions notwithstanding.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; the row-to-subject errors cannot
    /// happen for rows a fold wrote, but are propagated rather than
    /// sworn away.
    pub fn run_sightings(&self, run: &str) -> Result<Vec<(Subject, Placement)>> {
        let quoted = Value::String(run.to_string()).to_string();
        let mut statement = self.connection.prepare(
            "SELECT su.digest, a.name, c.value, c.time, so.name
             FROM claims c
             JOIN subjects su ON su.id = c.subject
             JOIN attributes a ON a.id = c.attribute
             JOIN sources so ON so.id = c.source
             WHERE c.retract = 0
               AND a.name IN ('prov:run', 'file:path', 'file:name')
               AND c.subject IN (
                   SELECT subject FROM claims
                    WHERE attribute = (SELECT id FROM attributes WHERE name = 'prov:run')
                      AND value = ?1 AND retract = 0)
             ORDER BY su.digest, c.time, so.name",
        )?;
        let rows = statement.query_map(params![quoted], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut claims = Vec::new();
        for row in rows {
            claims.push(row?);
        }
        let mut sightings: Vec<(Subject, Placement)> = Vec::new();
        let mut start = 0;
        while start < claims.len() {
            // One sighting: the rows sharing subject, moment and source.
            let key = |row: &(String, String, String, String, String)| {
                (row.0.clone(), row.3.clone(), row.4.clone())
            };
            let opening = key(&claims[start]);
            let mut end = start;
            while end < claims.len() && key(&claims[end]) == opening {
                end += 1;
            }
            let group = &claims[start..end];
            start = end;
            if !group
                .iter()
                .any(|(_, attribute, value, _, _)| attribute == "prov:run" && *value == quoted)
            {
                continue;
            }
            let subject = Subject::parse(&group[0].0)?;
            let spelled = |wanted: &str| -> Vec<String> {
                group
                    .iter()
                    .filter(|(_, attribute, _, _, _)| attribute == wanted)
                    .filter_map(|(_, _, value, _, _)| {
                        serde_json::from_str::<Value>(value)
                            .ok()
                            .as_ref()
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .collect()
            };
            let paths = spelled("file:path");
            if paths.is_empty() {
                for name in spelled("file:name") {
                    sightings.push((subject.clone(), Placement::Name(name)));
                }
            } else {
                for path in paths {
                    sightings.push((subject.clone(), Placement::Path(path)));
                }
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
    let value = claim.value().map(Value::to_string);
    let mut history = transaction.prepare_cached(
        "INSERT INTO claims
             (subject, attribute, value, time, source, retract, segment, position)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    history.execute(params![
        subject,
        attribute,
        value,
        claim.time().as_str(),
        source,
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
    (subject, attribute, value, time, source, retract): (
        String,
        String,
        Option<String>,
        String,
        String,
        bool,
    ),
) -> Result<Claim> {
    let subject = Subject::parse(&subject)?;
    let attribute = Attribute::parse(&attribute)?;
    let time = Timestamp::parse(&time)?;
    let source = Source::parse(&source)?;
    let value = value
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()?;
    match (value, retract) {
        (Some(value), false) => Claim::assert(subject, attribute, value, time, source),
        (Some(value), true) => Claim::retract_value(subject, attribute, value, time, source),
        (None, true) => Ok(Claim::retract_attribute(subject, attribute, time, source)),
        (None, false) => Err(Error::ValueRequired),
    }
}

/// The clause that takes every subject lacking nothing of `missing` out
/// of a `find`: one `EXCEPT` per entry, an attribute by name or, ending
/// in `:`, a whole namespace.
fn lacking(
    sql: &mut String,
    params: &mut Vec<rusqlite::types::Value>,
    missing: &[String],
) -> Result<()> {
    use rusqlite::types::Value as Sql;
    let text = |s: &str| Sql::Text(s.to_string());
    for absent in missing {
        sql.push_str(" EXCEPT SELECT subject FROM standing WHERE attribute IN (SELECT id FROM attributes WHERE ");
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
        )
        .unwrap()
    }

    fn term(attribute: &str, pattern: &str) -> (Attribute, String) {
        (Attribute::parse(attribute).unwrap(), pattern.to_string())
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
                .find(&[term("user:tag", "holiday")], &[], Presence::Held)
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
            )
            .unwrap(),
        )
        .unwrap();
        index.fold(&log).unwrap();

        assert_eq!(
            index
                .find(&[term("user:tag", "holiday")], &[], Presence::Held)
                .unwrap(),
            []
        );
        assert_eq!(
            index
                .find(&[term("user:tag", "crete")], &[], Presence::Held)
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
        ))
        .unwrap();
        index.fold(&log).unwrap();

        assert_eq!(
            index
                .find(&[term("user:tag", "crete")], &[], Presence::Held)
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
            )
            .unwrap(),
        )
        .unwrap();

        index.fold(&log).unwrap();
        index.fold(&log).unwrap();
        assert_eq!(
            index
                .find(&[term("user:tag", "holiday")], &[], Presence::Held)
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
            )
            .unwrap(),
        )
        .unwrap();
        index.fold(&log).unwrap();

        let tags = Attribute::parse("user:tag").unwrap();
        assert_eq!(
            index.values(&subject(), &tags).unwrap(),
            [json!("beach"), json!("crete")],
            "said twice stands once, retracted stands not at all; sorted by spelling"
        );
        assert_eq!(
            index
                .values(&subject(), &Attribute::parse("exif:model").unwrap())
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
                .find(&[term("file:name", "*.jpg")], &[], Presence::Held)
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
    fn places_and_arrivals_read_as_tables() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        for (attribute, value) in [
            ("file:path", json!("/home/john/a.txt")),
            ("prov:run", json!("run-a")),
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

        let run: (String, String, i64) = index
            .connection
            .query_row("SELECT run, source, files FROM v_arrivals", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap();
        assert_eq!(run, ("run-a".to_string(), "user".to_string(), 1));
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
                )
                .unwrap(),
            )
            .unwrap();
        }
        index.fold(&log).unwrap();

        assert_eq!(
            index.values(&subject(), &mime).unwrap(),
            [json!("message/rfc822")],
            "who says it is the claim's business; the standing set holds the value once"
        );
        assert_eq!(
            index
                .find(&[term("file:mime", "message/rfc822")], &[], Presence::Held)
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
            )
            .unwrap(),
        )
        .unwrap();
        log.append(&tag("beach", "2026-09-04T10:00:00Z")).unwrap();
        index.fold(&log).unwrap();

        let tags = Attribute::parse("user:tag").unwrap();
        let early = index.as_of("2026-09-02T00:00:00Z").unwrap();
        assert_eq!(
            early.values(&subject(), &tags).unwrap(),
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
            late.values(&subject(), &tags).unwrap(),
            Vec::<Value>::new(),
            "after the retraction nothing stands, and beach has not arrived yet"
        );
        assert_eq!(
            index.values(&subject(), &tags).unwrap(),
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
                    Presence::Held
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
                .find(&[term("file:path", "*crete*")], &[], Presence::Held)
                .unwrap(),
            [subject()]
        );
        assert_eq!(
            index
                .find(&[term("file:size", "20*")], &[], Presence::Held)
                .unwrap(),
            [],
            "numbers were promised no wildcards"
        );
        assert_eq!(
            index
                .find(&[term("file:size", "2019")], &[], Presence::Held)
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
            .values_in(&subject(), "exif")
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
            index.values_in(&subject(), "user").unwrap(),
            [],
            "a namespace nothing stands in answers empty"
        );
        assert_eq!(
            index.subjects(Presence::Held).unwrap(),
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
                    Presence::Held
                )
                .unwrap(),
            std::slice::from_ref(&bare),
            "the namespace prefix names the lack"
        );
        assert_eq!(
            index
                .find(&[], &["exif:make".to_string()], Presence::Held)
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
                .find(
                    &[term("file:modified", "2026-09-01..")],
                    &[],
                    Presence::Held
                )
                .unwrap(),
            [december.clone(), both.clone()],
            "since: one standing value past the bound suffices"
        );
        assert_eq!(
            index
                .find(
                    &[term("file:modified", "2026-09-01..2026-10-01")],
                    &[],
                    Presence::Held
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
                    Presence::Held
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
                .find(&[term("user:rating", "1..10")], &[], Presence::Held)
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
                    Presence::Held
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
                    Presence::Held
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
                .find(&[term("user:note", "\"see 3..4\"")], &[], Presence::Held)
                .unwrap(),
            [subject()],
            "quoted, the dots are just dots"
        );
        assert_eq!(
            index
                .find(&[term("user:note", "\"see 3\"")], &[], Presence::Held)
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
                .find(&[term("user:tag", "..")], &[], Presence::Held)
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
    fn sight(log: &Log, subject: &Subject, run: &str, path: &str, time: &str) {
        let time = Timestamp::parse(time).unwrap();
        let source = Source::parse("ingest").unwrap();
        let name = path.rsplit('/').next().unwrap();
        for (attribute, value) in [
            ("file:path", json!(path)),
            ("file:name", json!(name)),
            ("prov:run", json!(run)),
        ] {
            log.append(
                &Claim::assert(
                    subject.clone(),
                    Attribute::parse(attribute).unwrap(),
                    value,
                    time.clone(),
                    source.clone(),
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
            "run-a",
            "/home/s/a.txt",
            "2026-09-01T10:00:00Z",
        );
        sight(
            &log,
            &moved,
            "run-a",
            "/home/s/b.txt",
            "2026-09-01T10:00:01Z",
        );
        // The same content, met again by a later run somewhere else: its
        // path belongs to that run, not to run-a.
        sight(
            &log,
            &subject(),
            "run-b",
            "/mnt/nas/a.txt",
            "2026-09-02T10:00:00Z",
        );
        index.fold(&log).unwrap();

        assert_eq!(
            index.run_sightings("run-a").unwrap(),
            [
                (moved, Placement::Path("/home/s/b.txt".to_string())),
                (subject(), Placement::Path("/home/s/a.txt".to_string())),
            ],
            "each run answers with the places it recorded itself, by subject"
        );
        assert_eq!(
            index.run_sightings("run-b").unwrap(),
            [(subject(), Placement::Path("/mnt/nas/a.txt".to_string()))]
        );
        assert_eq!(index.run_sightings("run-c").unwrap(), []);
    }

    #[test]
    fn one_run_seeing_two_places_answers_twice() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        sight(
            &log,
            &subject(),
            "run-a",
            "/home/s/a.txt",
            "2026-09-01T10:00:00Z",
        );
        sight(
            &log,
            &subject(),
            "run-a",
            "/home/s/copy/a.txt",
            "2026-09-01T10:00:02Z",
        );
        index.fold(&log).unwrap();

        assert_eq!(
            index.run_sightings("run-a").unwrap(),
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
        for (attribute, value) in [
            ("file:name", json!("invoice.pdf")),
            ("prov:run", json!("run-x")),
        ] {
            log.append(
                &Claim::assert(
                    subject(),
                    Attribute::parse(attribute).unwrap(),
                    value,
                    time.clone(),
                    source.clone(),
                )
                .unwrap(),
            )
            .unwrap();
        }
        index.fold(&log).unwrap();

        assert_eq!(
            index.run_sightings("run-x").unwrap(),
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
                .find(&[term("user:tag", "holiday")], &[], Presence::Held)
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
            index.find(&holiday, &[], Presence::Placed).unwrap(),
            vec![placed.clone()],
            "no place stands on the other"
        );
        assert_eq!(
            index.find(&holiday, &[], Presence::Held).unwrap(),
            vec![placed.clone(), placeless.clone()]
        );
        assert_eq!(
            index.subjects(Presence::Placed).unwrap(),
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
                .find(&holiday, &[], Presence::Placed)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            index.find(&holiday, &[], Presence::Held).unwrap(),
            vec![placed.clone(), placeless],
            "held all the same"
        );
        let before = index.as_of("2026-09-01T23:59:59Z").unwrap();
        assert_eq!(
            before.find(&holiday, &[], Presence::Placed).unwrap(),
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
            index.find(&invoice, &[], Presence::Placed).unwrap(),
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
                .find(&invoice, &[], Presence::Placed)
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
                .find(&[term("user:tag", "holiday")], &[], Presence::Placed)
                .unwrap(),
            vec![message]
        );
    }
}
