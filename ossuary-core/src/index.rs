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
//! What the fold deliberately does *not* do is interpret: retractions are
//! rows like any other, and no supersession policy is baked in. Which claim
//! wins is a question for query time — policies change, and a policy in the
//! schema would make the cache authoritative in the one way that matters.

use std::path::Path;

use rusqlite::{Connection, params};

use crate::claim::{Attribute, Claim, Source, Subject, Timestamp, Value};
use crate::error::{Error, Result};
use crate::log::Log;

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
             );",
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

/// One segment's claims into the table, in their order.
fn insert(transaction: &rusqlite::Transaction<'_>, segment: &str, claims: &[Claim]) -> Result<()> {
    let mut statement = transaction.prepare(
        "INSERT INTO claims
             (subject, attribute, value, time, source, retract, segment, position)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for (position, claim) in claims.iter().enumerate() {
        let position = i64::try_from(position).expect("fewer claims than i64 can count");
        statement.execute(params![
            claim.subject().as_str(),
            claim.attribute().as_str(),
            claim.value().map(Value::to_string),
            claim.time().as_str(),
            claim.source().as_str(),
            claim.is_retraction(),
            segment,
            position,
        ])?;
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
