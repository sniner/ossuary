//! The tables, indexes and views of the index file.

use crate::vocabulary::FILE_PATH;

/// The cache schema's generation, kept in `PRAGMA user_version`: a file
/// carrying any other number is emptied instead of half-understood.
pub(super) const SCHEMA: i64 = 2;

/// The open head's row in `segments`: id 0, named `head`, ranked past
/// every sealed segment so it sorts last wherever log order is asked.
pub(super) const HEAD: i64 = 0;
pub(super) const HEAD_SEQ: i64 = i64::MAX;

/// The tables, indexes and views, created where missing.
pub(super) fn ddl() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS subjects (
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
         seq    INTEGER NOT NULL
     );
     CREATE TABLE IF NOT EXISTS open_read (
         id       INTEGER PRIMARY KEY CHECK (id = 0),
         previous TEXT
     );
     CREATE TABLE IF NOT EXISTS bundles (
         digest TEXT PRIMARY KEY
     ) WITHOUT ROWID;
     CREATE TABLE IF NOT EXISTS claims (
         id        INTEGER PRIMARY KEY,
         cid       TEXT NOT NULL UNIQUE,
         subject   INTEGER NOT NULL,
         attribute INTEGER NOT NULL,
         value     TEXT,
         time      TEXT NOT NULL,
         source    INTEGER NOT NULL,
         run       INTEGER,
         retract   INTEGER NOT NULL DEFAULT 0,
         retracted INTEGER NOT NULL DEFAULT 0,
         segment   INTEGER NOT NULL,
         position  INTEGER NOT NULL
     );
     CREATE INDEX IF NOT EXISTS claims_subject
         ON claims (subject, attribute);
     CREATE TABLE IF NOT EXISTS retractions (
         target TEXT NOT NULL,
         claim  INTEGER NOT NULL
     );
     CREATE INDEX IF NOT EXISTS retractions_target
         ON retractions (target);
     CREATE INDEX IF NOT EXISTS retractions_claim
         ON retractions (claim);
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
         SELECT c.id, c.cid, su.digest AS subject, a.name AS attribute, c.value, c.time,
                so.name AS source, r.name AS run, c.retract, c.retracted,
                s.digest AS segment, s.seq, c.position
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
         WHERE st.attribute = (SELECT id FROM attributes WHERE name = '{FILE_PATH}')
           AND json_type(st.value) = 'text';
     CREATE VIEW IF NOT EXISTS v_runs AS
         SELECT r.name AS run, MIN(c.time) AS first, MAX(c.time) AS last,
                COUNT(DISTINCT c.subject) AS files, COUNT(*) AS claims,
                SUM(c.retract) AS retractions
         FROM claims c
         JOIN runs r ON r.id = c.run
         GROUP BY c.run;"
    )
}
