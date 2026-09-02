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
//! The fold keeps two tables. `claims` is the history, verbatim: every
//! claim a row, retractions included, in log order — what
//! [`about`](Index::about) reads. `standing` is the folded answer: one row
//! per standing (subject, attribute, value), the set semantics as a
//! primary key — an assertion is `INSERT OR IGNORE`, a retraction a
//! `DELETE` — and what [`find`](Index::find) reads. What stays deliberately
//! un-baked is *narrowing*: which of several standing values a reader
//! prefers is query-time policy, and the sets carry them all.
//!
//! Standing follows the log forward, the only direction a log moves; a
//! `head.jsonl` edited backwards leaves it stale until the cache is
//! deleted and refolded — the cure every cache here has.

use std::path::Path;

use rusqlite::{Connection, params};

use crate::claim::{Attribute, Claim, Source, Subject, Timestamp, Value};
use crate::error::{Error, Result};
use crate::log::Log;

/// The cache schema's generation, kept in `PRAGMA user_version`: bumped
/// whenever the tables change shape, so an older file is recognised and
/// emptied instead of half-understood.
const SCHEMA: i64 = 2;

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

/// A disposable query index over a claim log.
#[derive(Debug)]
pub struct Index {
    connection: Connection,
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
        // The cache's own generation. A file from an older schema is not
        // migrated but emptied — it is a cache, and the next fold rebuilds
        // it from the log for the cost of one slow first answer.
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != SCHEMA {
            connection.execute_batch(&format!(
                "DROP TABLE IF EXISTS claims;
                 DROP TABLE IF EXISTS segments;
                 DROP TABLE IF EXISTS standing;
                 PRAGMA user_version = {SCHEMA};"
            ))?;
        }
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS claims (
                 subject   TEXT NOT NULL,
                 attribute TEXT NOT NULL,
                 value     TEXT,
                 time      TEXT NOT NULL,
                 source    TEXT NOT NULL,
                 retract   INTEGER NOT NULL DEFAULT 0,
                 segment   TEXT NOT NULL,
                 position  INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS claims_subject
                 ON claims (subject, attribute);
             CREATE TABLE IF NOT EXISTS segments (
                 digest TEXT PRIMARY KEY,
                 first  TEXT
             );
             CREATE TABLE IF NOT EXISTS standing (
                 subject   TEXT NOT NULL,
                 attribute TEXT NOT NULL,
                 value     TEXT NOT NULL,
                 PRIMARY KEY (subject, attribute, value)
             );
             CREATE INDEX IF NOT EXISTS standing_lookup
                 ON standing (attribute, value);",
        )?;
        Ok(Index { connection })
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
        let mut folded = Folded::default();

        for segment in log.segments()? {
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
            insert(&transaction, &digest, &claims)?;
            transaction.execute(
                "INSERT INTO segments (digest, first) VALUES (?1, ?2)",
                params![digest, segment.first_claim_at().map(Timestamp::as_str)],
            )?;
            transaction.commit()?;
            folded.segments += 1;
            folded.claims += claims.len();
        }

        let head = log.head()?;
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM claims WHERE segment = 'head'", [])?;
        insert(&transaction, "head", &head)?;
        transaction.commit()?;
        folded.head = head.len();

        Ok(folded)
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
            "SELECT c.subject, c.attribute, c.value, c.time, c.source, c.retract
             FROM claims c LEFT JOIN segments s ON c.segment = s.digest
             WHERE c.subject = ?1
             ORDER BY c.time,
                      c.segment = 'head',
                      s.first,
                      s.digest,
                      c.position",
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

    /// Every subject on which all `terms` stand and none of `missing` does,
    /// sorted — as of the last [`fold`](Index::fold), the open head
    /// included.
    ///
    /// A term is an attribute and a value: the value matches the standing
    /// spelling — a string, or the bare JSON scalar for numbers and
    /// booleans — and may carry `*` and `?`, which match within string
    /// values only. Every term must hold. Each entry of `missing` names an
    /// attribute the subject must lack; ending in `:` it names a whole
    /// namespace. With no terms at all, `missing` is asked of every
    /// subject the log speaks about.
    ///
    /// Only *standing* values answer: a retracted value finds nothing,
    /// however long its claim stays in the log — this is where the set
    /// semantics first faces a reader.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; an entry of `missing` that fits
    /// neither the attribute grammar nor `namespace:` is refused with the
    /// grammar's own error.
    pub fn find(&self, terms: &[(Attribute, String)], missing: &[String]) -> Result<Vec<Subject>> {
        if terms.is_empty() && missing.is_empty() {
            return Ok(Vec::new());
        }
        let mut sql = String::new();
        let mut params: Vec<String> = Vec::new();
        if terms.is_empty() {
            sql.push_str("SELECT DISTINCT subject FROM standing");
        }
        for (position, (attribute, pattern)) in terms.iter().enumerate() {
            if position > 0 {
                sql.push_str(" INTERSECT ");
            }
            if pattern.contains('*') || pattern.contains('?') {
                // The stored string spelling is quoted JSON, so a pattern
                // globbed inside quotes matches string values and nothing
                // else — numbers were promised no wildcards.
                sql.push_str("SELECT subject FROM standing WHERE attribute = ? AND value GLOB ?");
                params.push(attribute.as_str().to_string());
                params.push(format!("\"{pattern}\""));
            } else {
                match serde_json::from_str::<Value>(pattern) {
                    // The bare word is a JSON scalar — a number, a
                    // boolean: it may stand as itself or as a string, and
                    // either spelling answers.
                    Ok(scalar) if !scalar.is_string() => {
                        sql.push_str(
                            "SELECT subject FROM standing WHERE attribute = ? AND value IN (?, ?)",
                        );
                        params.push(attribute.as_str().to_string());
                        params.push(scalar.to_string());
                        params.push(Value::String(pattern.clone()).to_string());
                    }
                    _ => {
                        sql.push_str(
                            "SELECT subject FROM standing WHERE attribute = ? AND value = ?",
                        );
                        params.push(attribute.as_str().to_string());
                        params.push(Value::String(pattern.clone()).to_string());
                    }
                }
            }
        }
        for absent in missing {
            sql.push_str(" EXCEPT SELECT subject FROM standing WHERE ");
            if let Some(namespace) = absent.strip_suffix(':') {
                // The grammar has one door; a prefix walks through it
                // wearing a dummy name.
                Attribute::parse(&format!("{namespace}:a"))?;
                // ';' is the character after ':', and no attribute
                // contains one: the half-open range is the namespace.
                sql.push_str("attribute >= ? AND attribute < ?");
                params.push(format!("{namespace}:"));
                params.push(format!("{namespace};"));
            } else {
                let attribute = Attribute::parse(absent)?;
                sql.push_str("attribute = ?");
                params.push(attribute.as_str().to_string());
            }
        }
        sql.push_str(" ORDER BY subject");
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
    /// among `mimes`, and no [`prov:examined`](crate::EXAMINED)
    /// receipt from `source` — as of the last [`fold`](Index::fold), the
    /// open head included. Sorted, so a run walks the same order twice.
    ///
    /// This is the log informing *effort*, never truth: whether a file is
    /// offered again is decided here, what an extractor says about it never
    /// is. Retracted claims do not count on either side of the test.
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
            "SELECT DISTINCT subject FROM claims
             WHERE attribute = 'file:mime' AND retract = 0
               AND value IN ({holes})
               AND subject NOT IN (
                   SELECT subject FROM claims
                    WHERE attribute = 'prov:examined'
                      AND source = ? AND retract = 0)
             ORDER BY subject"
        ))?;
        // The value column holds values as JSON, so a MIME type is
        // compared in its stored spelling: quoted.
        let quoted: Vec<String> = mimes
            .iter()
            .map(|mime| Value::String(mime.clone()).to_string())
            .collect();
        let mut params: Vec<&dyn rusqlite::ToSql> = Vec::new();
        for mime in &quoted {
            params.push(mime);
        }
        let source = source.as_str().to_string();
        params.push(&source);
        let rows = statement.query_map(params.as_slice(), |row| row.get::<_, String>(0))?;
        let mut subjects = Vec::new();
        for row in rows {
            subjects.push(Subject::parse(&row?)?);
        }
        Ok(subjects)
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
    pub fn matching(&self, algorithm: &str, hex: &str) -> Result<Vec<Subject>> {
        let low = format!("{algorithm}:{}", hex.to_ascii_lowercase());
        // The half-open range [low, low + "g") holds every digest that
        // begins with `low` and nothing else: 'g' is the first character
        // past the hex digits, and a range beats LIKE here — it uses the
        // index, and a stray '%' in the input stays a character.
        let high = format!("{low}g");
        let mut statement = self.connection.prepare(
            "SELECT DISTINCT subject FROM claims
             WHERE subject >= ?1 AND subject < ?2
             ORDER BY subject",
        )?;
        let rows = statement.query_map(params![low, high], |row| row.get::<_, String>(0))?;
        let mut subjects = Vec::new();
        for row in rows {
            subjects.push(Subject::parse(&row?)?);
        }
        Ok(subjects)
    }
}

/// One segment's claims into the tables, in their order: every claim a
/// history row, and each one folded into `standing` — an assertion puts
/// the value in (the primary key deduplicates), a retraction takes it
/// out, a valueless retraction empties the attribute. All three are
/// idempotent, which is what lets the head be applied afresh each fold.
fn insert(transaction: &rusqlite::Transaction<'_>, segment: &str, claims: &[Claim]) -> Result<()> {
    let mut history = transaction.prepare(
        "INSERT INTO claims
             (subject, attribute, value, time, source, retract, segment, position)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    let mut put = transaction.prepare(
        "INSERT OR IGNORE INTO standing (subject, attribute, value) VALUES (?1, ?2, ?3)",
    )?;
    let mut take = transaction
        .prepare("DELETE FROM standing WHERE subject = ?1 AND attribute = ?2 AND value = ?3")?;
    let mut empty =
        transaction.prepare("DELETE FROM standing WHERE subject = ?1 AND attribute = ?2")?;
    for (position, claim) in claims.iter().enumerate() {
        let position = i64::try_from(position).expect("fewer claims than i64 can count");
        history.execute(params![
            claim.subject().as_str(),
            claim.attribute().as_str(),
            claim.value().map(Value::to_string),
            claim.time().as_str(),
            claim.source().as_str(),
            claim.is_retraction(),
            segment,
            position,
        ])?;
        let subject = claim.subject().as_str();
        let attribute = claim.attribute().as_str();
        match (claim.value(), claim.is_retraction()) {
            (Some(value), false) => {
                put.execute(params![subject, attribute, value.to_string()])?;
            }
            (Some(value), true) => {
                take.execute(params![subject, attribute, value.to_string()])?;
            }
            (None, true) => {
                empty.execute(params![subject, attribute])?;
            }
            // A claim without value and without retract cannot be built.
            (None, false) => {}
        }
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
        Subject::parse("sha256:9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e")
            .unwrap()
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
        let near = Subject::parse(
            "sha256:9f2ac41edd00000000000000000000000000000000000000000000000000ffff",
        )
        .unwrap();
        log.append(&tag("holiday", "2026-09-01T21:14:03Z")).unwrap();
        log.seal().unwrap().unwrap();
        log.append(&tag_about(near.clone(), "beach", "2026-09-01T21:14:04Z"))
            .unwrap();
        index.fold(&log).unwrap();

        assert_eq!(
            index.matching("sha256", "9f2ac41e").unwrap(),
            [subject(), near.clone()],
            "sealed or still in the head, each once, sorted"
        );
        assert_eq!(
            index.matching("sha256", "9F2AC41ED").unwrap(),
            [near],
            "case falls away, the way subjects themselves are spelled"
        );
        assert_eq!(index.matching("sha256", "ffff").unwrap(), Vec::new());
        assert_eq!(
            index.matching("sha256", "9f2ac41e%").unwrap(),
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
            index.find(&[term("user:tag", "holiday")], &[]).unwrap(),
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

        assert_eq!(index.find(&[term("user:tag", "holiday")], &[]).unwrap(), []);
        assert_eq!(
            index.find(&[term("user:tag", "crete")], &[]).unwrap(),
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

        assert_eq!(index.find(&[term("user:tag", "crete")], &[]).unwrap(), []);
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
            index.find(&[term("user:tag", "holiday")], &[]).unwrap(),
            [],
            "the sealed assertion is folded once and must not resurface"
        );
    }

    #[test]
    fn find_needs_every_term_to_stand() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        let other = Subject::parse(
            "sha256:1111111111111111111111111111111111111111111111111111111111111111",
        )
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
                    &[]
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
            "prov:ingest-path",
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
                .find(&[term("prov:ingest-path", "*crete*")], &[])
                .unwrap(),
            [subject()]
        );
        assert_eq!(
            index.find(&[term("file:size", "20*")], &[]).unwrap(),
            [],
            "numbers were promised no wildcards"
        );
        assert_eq!(
            index.find(&[term("file:size", "2019")], &[]).unwrap(),
            [subject()],
            "while the exact number answers"
        );
    }

    #[test]
    fn missing_names_what_a_subject_lacks() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        let bare = Subject::parse(
            "sha256:1111111111111111111111111111111111111111111111111111111111111111",
        )
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
                .find(&[term("file:mime", "image/jpeg")], &["exif:".to_string()])
                .unwrap(),
            std::slice::from_ref(&bare),
            "the namespace prefix names the lack"
        );
        assert_eq!(
            index.find(&[], &["exif:make".to_string()]).unwrap(),
            [bare],
            "with no terms, missing is asked of every subject"
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
            index.find(&[term("user:tag", "holiday")], &[]).unwrap(),
            [subject()]
        );
    }

    #[test]
    fn a_subject_the_log_never_mentioned_has_nothing_to_say() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let mut index = index_in(&dir);
        index.fold(&log).unwrap();

        let unknown = Subject::parse(
            "sha256:00000000000000000000000000000000000000000000000000000000000000ff",
        )
        .unwrap();
        assert_eq!(index.about(&unknown).unwrap(), Vec::new());
    }
}
