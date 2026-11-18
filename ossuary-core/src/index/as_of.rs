//! The record as it stood at an earlier moment, replayed into an index
//! of its own in memory.

use rusqlite::{OptionalExtension as _, params};

use super::Index;
use super::fold::replay;
use super::query::{CLAIM_COLUMNS, CLAIM_JOINS, claim, claim_row};
use super::schema::HEAD;
use crate::claim::{Run, Timestamp};
use crate::error::Result;

/// One row of `segments`: id, digest, place in the chain.
pub(super) type SegmentRow = (i64, String, i64);

impl Index {
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
    /// [`Error::Index`](crate::Error::Index) from `SQLite`; the row-to-claim errors cannot
    /// happen for rows a fold wrote, but are propagated rather than
    /// sworn away.
    pub fn as_of(&self, cutoff: &str) -> Result<Index> {
        use rusqlite::types::Value as Sql;
        self.replay_until("c.time <= ?1", &[Sql::Text(cutoff.to_string())])
    }

    /// The archive's knowledge as of the end of one run: every claim in
    /// the segments before the run's last claim, every claim before its
    /// last second in that segment, and the run's own claims from that
    /// second. `None` when no claim carries the run.
    ///
    /// Within a segment claims are ordered by time and then by their
    /// line, so another run's claims from the run's last second stand
    /// in no order to it; the last second is taken as the run's own.
    ///
    /// # Errors
    ///
    /// [`Error::Index`](crate::Error::Index) from `SQLite`, as for [`as_of`](Index::as_of).
    pub fn as_of_run(&self, run: &Run) -> Result<Option<Index>> {
        use rusqlite::types::Value as Sql;
        // The record's order: the chain between segments, time within
        // one.
        let last = self
            .connection
            .query_row(
                "SELECT s.seq, c.time, c.run
                 FROM claims c JOIN segments s ON s.id = c.segment
                 WHERE c.run = (SELECT id FROM runs WHERE name = ?1)
                 ORDER BY s.seq DESC, c.time DESC
                 LIMIT 1",
                params![run.as_str()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((seq, time, run_id)) = last else {
            return Ok(None);
        };
        let replayed = self.replay_until(
            "s.seq < ?1 OR (s.seq = ?1 AND (c.time < ?2 OR (c.time = ?2 AND c.run = ?3)))",
            &[Sql::Integer(seq), Sql::Text(time), Sql::Integer(run_id)],
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
    /// [`Error::Timestamp`](crate::Error::Timestamp) when `given` is neither a run id nor a
    /// moment; [`Error::Index`](crate::Error::Index) from `SQLite`.
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
        // The segments' places in the chain ride along: they are the log
        // order, and the replayed record must read like the original.
        let segments: std::result::Result<Vec<SegmentRow>, _> = self
            .connection
            .prepare("SELECT id, digest, seq FROM segments WHERE id != ?1")?
            .query_map(params![HEAD], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect();
        let mut statement = self.connection.prepare(&format!(
            "SELECT {CLAIM_COLUMNS}, c.segment, c.position
             FROM claims c
             {CLAIM_JOINS}
             JOIN segments s ON s.id = c.segment
             WHERE {until}
             ORDER BY s.seq, c.position"
        ))?;
        let rows = statement.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok((
                claim_row(row)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
            ))
        })?;
        let Index {
            connection, ids, ..
        } = &mut replayed;
        let transaction = connection.transaction()?;
        for (id, digest, seq) in segments? {
            transaction.execute(
                "INSERT INTO segments (id, digest, seq) VALUES (?1, ?2, ?3)",
                params![id, digest, seq],
            )?;
        }
        for row in rows {
            let (fields, segment, position) = row?;
            replay(&transaction, ids, &claim(fields)?, segment, position)?;
        }
        transaction.commit()?;
        Ok(replayed)
    }
}
