//! Folding the log into the index: sealed segments once, the open
//! segment bundle by bundle. The only part of the index that reads the
//! log.

use std::collections::HashSet;

use immure::Digest;
use rusqlite::{Connection, OptionalExtension as _, TransactionBehavior, params};

use super::schema::HEAD;
use super::{Folded, Ids, Index};
use crate::claim::{Claim, Value};
use crate::error::{Error, Result};
use crate::log::{Log, Open, sort_as_sealed};

impl Index {
    /// Fold the log in: new segments once, and of the open head what was
    /// appended since the last fold.
    ///
    /// Safe to call as often as wanted. When every bundle in the head was
    /// read before and nothing was sealed since, the head is listed and
    /// nothing is read or written. A segment already folded is recognised
    /// by its digest and skipped, and each segment lands in one
    /// transaction, so an interrupted fold left nothing half-indexed. An
    /// index that does not match the log is emptied and folded anew, and
    /// [`Folded::rebuilt`] says so.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`, [`Error::IndexMismatch`] when the
    /// index still does not match the log after it was folded anew, and
    /// everything reading the log can answer.
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
        if let Some(folded) = self.follow(log)? {
            self.proved = true;
            return Ok(folded);
        }
        self.empty()?;
        match self.follow(log)? {
            Some(folded) => {
                self.proved = true;
                Ok(Folded {
                    rebuilt: true,
                    ..folded
                })
            }
            None => Err(Error::IndexMismatch),
        }
    }

    /// Fold in what the log holds beyond the index. `None` when the index
    /// does not match the log and has to be emptied first.
    ///
    /// Every step that writes runs in a transaction that takes the write
    /// lock before it reads `open_read`: a mount and a CLI call can fold
    /// into the same file, and both reading the same state would fold the
    /// same claims twice. A claim is known by its id, so a bundle read
    /// twice, or a segment that holds what was read from bundles, costs
    /// nothing but the reading.
    fn follow(&mut self, log: &Log) -> Result<Option<Folded>> {
        let read = open_read(&self.connection)?;
        let Some(open) = log.open()? else {
            // No open segment, so nothing sealed either.
            return Ok((!holds_claims(&self.connection)?).then(Folded::default));
        };
        if read == OpenRead::Never && holds_claims(&self.connection)? {
            // Claims without a record of what was read: not this build's.
            return Ok(None);
        }
        let followed = read.followed();
        if !self.proved && followed.is_none() && holds_claims(&self.connection)? {
            // Kept outside the archive, and no segment to tell by whose
            // claims these are.
            return Ok(None);
        }
        // The bundles read so far. With the open segment following the
        // same segment as then, every one of them is still in it: a
        // bundle goes only when a seal has put its lines into a segment,
        // and then the open segment follows that one. A bundle that is
        // missing means another open segment: a copy of the archive, or
        // a log set back.
        let read = read_bundles(&self.connection)?;
        if open.previous().map(Digest::as_str) == followed.as_deref() {
            let listed: HashSet<&str> = open.bundles().iter().map(Digest::as_str).collect();
            if read.iter().any(|bundle| !listed.contains(bundle.as_str())) {
                return Ok(None);
            }
        }

        // Back from the open segment to the segment the index followed.
        // Every segment passed on the way is new.
        let mut fresh = Vec::new();
        if open.previous().map(Digest::as_str) != followed.as_deref() {
            let mut reached = None;
            for segment in log.walk_back_from(open.previous()) {
                let segment = segment?;
                if known(&self.connection, segment.digest().as_str())?.is_some() {
                    reached = Some(segment.digest().to_string());
                    break;
                }
                fresh.push(segment);
            }
            if reached != followed {
                return Ok(None);
            }
        }
        let places = match followed.as_deref() {
            None => 0,
            Some(digest) => match known(&self.connection, digest)? {
                Some(seq) => seq + 1,
                None => return Ok(None),
            },
        }..;

        let mut folded = Folded::default();
        let mut before = followed;
        for (seq, segment) in places.zip(fresh.iter().rev()) {
            let digest = segment.digest().to_string();
            let claims = log.read(segment.digest())?;
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            // A segment that another process folded since the walk is
            // passed over.
            if known(&transaction, &digest)?.is_none() {
                if open_read(&transaction)?.followed() != before {
                    return Ok(None);
                }
                let had = count_claims(&transaction)?;
                transaction.execute(
                    "INSERT INTO segments (digest, seq) VALUES (?1, ?2)",
                    params![digest, seq],
                )?;
                let id = transaction.last_insert_rowid();
                insert(&transaction, &mut self.ids, id, &claims, 0)?;
                // The bundles read so far went into this segment, or stay
                // in the open one and are read again, which costs nothing
                // but the reading.
                transaction.execute("DELETE FROM bundles", [])?;
                transaction.execute(
                    "INSERT OR REPLACE INTO open_read (id, previous) VALUES (0, ?1)",
                    params![digest],
                )?;
                transaction.commit()?;
                folded.segments += 1;
                folded.claims += count_claims(&self.connection)? - had;
            }
            before = Some(digest);
        }

        // A segment folded above emptied the list of bundles read.
        let read = if fresh.is_empty() {
            read
        } else {
            HashSet::new()
        };
        folded.head += self.follow_open(log, &open, &read)?;
        Ok(Some(folded))
    }

    /// Fold in the bundles of the open segment that are not among `read`,
    /// the ones the index has read, and record which segment the open
    /// segment follows. Returns how many claims went in.
    fn follow_open(&mut self, log: &Log, open: &Open, read: &HashSet<String>) -> Result<usize> {
        // The bundles not read yet, their claims in the order the seal
        // will give them, so that the rows follow the record's order as
        // far as the open segment has one.
        let mut unread = Vec::new();
        let mut claims = Vec::new();
        for bundle in open.bundles() {
            if read.contains(bundle.as_str()) {
                continue;
            }
            // A bundle sealed since the listing has its segment read
            // next time.
            if let Some(read) = log.bundle(open, bundle)? {
                claims.extend(read);
                unread.push(bundle.as_str());
            }
        }
        if !unread.is_empty() {
            sort_as_sealed(&mut claims);
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            if open_read(&transaction)?.followed().as_deref() != open.previous().map(Digest::as_str)
            {
                // Another process folded a seal made after the listing.
                return Ok(0);
            }
            let position = head_rows(&transaction)?;
            insert(&transaction, &mut self.ids, HEAD, &claims, position)?;
            for bundle in &unread {
                transaction.execute(
                    "INSERT OR IGNORE INTO bundles (digest) VALUES (?1)",
                    params![bundle],
                )?;
            }
            transaction.execute(
                "INSERT OR REPLACE INTO open_read (id, previous) VALUES (0, ?1)",
                params![open.previous().map(Digest::as_str)],
            )?;
            transaction.commit()?;
        }
        if open_read(&self.connection)? == OpenRead::Never {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            if open_read(&transaction)? == OpenRead::Never {
                transaction.execute(
                    "INSERT INTO open_read (id, previous) VALUES (0, ?1)",
                    params![open.previous().map(Digest::as_str)],
                )?;
            }
            transaction.commit()?;
        }
        Ok(claims.len())
    }

    /// Empty the index of everything the log put in it. The names stay,
    /// and the ids cached for them stay right.
    fn empty(&mut self) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(
            "DELETE FROM claims; DELETE FROM retractions; DELETE FROM standing;
             DELETE FROM open_read; DELETE FROM bundles;",
        )?;
        transaction.execute("DELETE FROM segments WHERE id != ?1", params![HEAD])?;
        transaction.commit()?;
        Ok(())
    }
}

/// Claims of one segment into the tables, in their order, the first of
/// them at position `first`: every claim a history row, and each one
/// folded into `standing` as [`replay`] describes.
pub(super) fn insert(
    transaction: &rusqlite::Transaction<'_>,
    ids: &mut Ids,
    segment: i64,
    claims: &[Claim],
    first: usize,
) -> Result<()> {
    for (position, claim) in (first..).zip(claims) {
        let position = i64::try_from(position).expect("fewer claims than i64 can count");
        replay(transaction, ids, claim, segment, position)?;
    }
    Ok(())
}

/// The row of `open_read`: the segment the open segment followed when
/// the index last read it.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum OpenRead {
    /// No row: the index has not read an open segment yet.
    Never,
    /// The segment followed; `None` before the first seal.
    Followed(Option<String>),
}

impl OpenRead {
    /// The segment followed; `None` before the first seal and also for an
    /// index that has not read an open segment yet.
    fn followed(self) -> Option<String> {
        match self {
            OpenRead::Never => None,
            OpenRead::Followed(previous) => previous,
        }
    }
}

pub(super) fn open_read(connection: &Connection) -> Result<OpenRead> {
    let read = connection
        .query_row("SELECT previous FROM open_read WHERE id = 0", [], |row| {
            row.get::<_, Option<String>>(0)
        })
        .optional()?;
    Ok(read.map_or(OpenRead::Never, OpenRead::Followed))
}

/// The bundles of the open segment that were read into the index.
///
/// One query instead of one per bundle: outside a transaction each
/// statement takes and releases the file lock, and with the index on an
/// SMB share that is about 3 ms per statement.
pub(super) fn read_bundles(connection: &Connection) -> Result<HashSet<String>> {
    let mut statement = connection.prepare("SELECT digest FROM bundles")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut read = HashSet::new();
    for row in rows {
        read.insert(row?);
    }
    Ok(read)
}

/// How many claims the index holds.
pub(super) fn count_claims(connection: &Connection) -> Result<usize> {
    let count: i64 = connection.query_row("SELECT COUNT(*) FROM claims", [], |row| row.get(0))?;
    Ok(usize::try_from(count).expect("a count is not negative"))
}

/// The place in log order of a segment the index holds, by its digest.
pub(super) fn known(connection: &Connection, digest: &str) -> Result<Option<i64>> {
    let seq = connection
        .query_row(
            "SELECT seq FROM segments WHERE digest = ?1",
            params![digest],
            |row| row.get(0),
        )
        .optional()?;
    Ok(seq)
}

/// How many claims of the open head the index holds.
pub(super) fn head_rows(connection: &Connection) -> Result<usize> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM claims WHERE segment = ?1",
        params![HEAD],
        |row| row.get(0),
    )?;
    Ok(usize::try_from(count).expect("a count is not negative"))
}

/// Whether the index holds any claim at all.
pub(super) fn holds_claims(connection: &Connection) -> Result<bool> {
    let held =
        connection.query_row("SELECT EXISTS (SELECT 1 FROM claims)", [], |row| row.get(0))?;
    Ok(held)
}

/// One claim into the tables: into the history as it is, onto the
/// standing what it asserts or retracts.
///
/// A claim is known by its id, so one that is already in the tables, read
/// from the open segment before it was sealed, only gets its segment and
/// position; the history row and the standing stay. An assertion stands
/// unless a retraction names it, whichever of the two the fold met first:
/// a retraction marks the claims it names as retracted, and an assertion
/// arriving after its retraction is marked on arrival.
pub(super) fn replay(
    transaction: &rusqlite::Transaction<'_>,
    ids: &mut Ids,
    claim: &Claim,
    segment: i64,
    position: i64,
) -> Result<()> {
    let cid = claim
        .id()
        .expect("a claim out of the log carries its id")
        .as_str();
    let subject = ids.subject(transaction, claim.subject().as_str())?;
    let attribute = ids.attribute(transaction, claim.attribute().as_str())?;
    let source = ids.source(transaction, claim.source().as_str())?;
    let run = match claim.run() {
        Some(run) => Some(ids.run(transaction, run.as_str())?),
        None => None,
    };
    let value = claim.value().map(Value::to_string);
    // A retraction retracts the claims it names that have its subject
    // and attribute. One that names a claim of another subject or
    // attribute is a line that contradicts itself, and retracts nothing.
    let retracted: bool = transaction.query_row(
        "SELECT EXISTS (SELECT 1 FROM retractions rt JOIN claims r ON r.id = rt.claim
                        WHERE rt.target = ?1 AND r.subject = ?2 AND r.attribute = ?3)",
        params![cid, subject, attribute],
        |row| row.get(0),
    )?;
    let mut history = transaction.prepare_cached(
        "INSERT INTO claims
             (cid, subject, attribute, value, time, source, run, retract, retracted,
              segment, position)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT (cid) DO UPDATE SET segment = excluded.segment,
                                         position = excluded.position",
    )?;
    let known: Option<i64> = transaction
        .query_row(
            "SELECT id FROM claims WHERE cid = ?1",
            params![cid],
            |row| row.get(0),
        )
        .optional()?;
    history.execute(params![
        cid,
        subject,
        attribute,
        value,
        claim.time().as_str(),
        source,
        run,
        claim.is_retraction(),
        retracted,
        segment,
        position,
    ])?;
    if known.is_some() {
        return Ok(());
    }
    let id = transaction.last_insert_rowid();
    if let Some(value) = value {
        if !retracted {
            // Asserted again, the value stands as before, and the newest
            // assertion answers for it: the later time, and in the same
            // second the larger id. The row id says when this index read
            // a claim, which differs between an index folded on and one
            // built anew, so it does not decide.
            let mut put = transaction.prepare_cached(
                "INSERT INTO standing (subject, attribute, value, time, claim)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (subject, attribute, value)
                 DO UPDATE SET time = excluded.time, claim = excluded.claim
                 WHERE excluded.time > standing.time
                    OR (excluded.time = standing.time
                        AND ?6 > (SELECT cid FROM claims WHERE id = standing.claim))",
            )?;
            put.execute(params![
                subject,
                attribute,
                value,
                claim.time().as_str(),
                id,
                cid
            ])?;
        }
        return Ok(());
    }
    let mut name =
        transaction.prepare_cached("INSERT INTO retractions (target, claim) VALUES (?1, ?2)")?;
    let mut mark = transaction.prepare_cached(
        "UPDATE claims SET retracted = 1
         WHERE cid = ?1 AND subject = ?2 AND attribute = ?3 AND retract = 0 AND retracted = 0
         RETURNING subject, attribute, value",
    )?;
    for target in claim.retracts() {
        name.execute(params![target.as_str(), id])?;
        let taken: Option<(i64, i64, Option<String>)> = mark
            .query_row(params![target.as_str(), subject, attribute], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .optional()?;
        if let Some((subject, attribute, Some(value))) = taken {
            restand(transaction, subject, attribute, &value)?;
        }
    }
    Ok(())
}

/// The standing row of one value after an assertion of it was retracted:
/// the newest assertion of it that still stands answers for it, or the row
/// goes.
pub(super) fn restand(
    transaction: &rusqlite::Transaction<'_>,
    subject: i64,
    attribute: i64,
    value: &str,
) -> Result<()> {
    let newest: Option<(i64, String)> = transaction
        .query_row(
            "SELECT id, time FROM claims
             WHERE subject = ?1 AND attribute = ?2 AND value = ?3
               AND retract = 0 AND retracted = 0
             ORDER BY time DESC, cid DESC LIMIT 1",
            params![subject, attribute, value],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match newest {
        Some((claim, time)) => {
            transaction.execute(
                "INSERT OR REPLACE INTO standing (subject, attribute, value, time, claim)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![subject, attribute, value, time, claim],
            )?;
        }
        None => {
            transaction.execute(
                "DELETE FROM standing WHERE subject = ?1 AND attribute = ?2 AND value = ?3",
                params![subject, attribute, value],
            )?;
        }
    }
    Ok(())
}
