//! What the index answers, apart from `find`. Nothing here reads the
//! log.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use rusqlite::params;

use super::{Episode, Index, PLACES, Placement, Scope, Standing};
use crate::claim::{Attribute, Claim, ClaimId, Run, Source, Subject, Timestamp, Value};
use crate::error::{Error, Result};
use crate::vocabulary::{
    FILE_MIME, FILE_NAME, FILE_PATH, MAILBOX_PLACE, PROV_EXAMINED, PROV_ORIGIN, known_attribute,
};

/// The columns a claim is read back from, spoken of `c` the claim, `su`,
/// `a`, `so` and `r` its names: what [`claim`] builds a [`Claim`] from.
/// The retracted ids come as a JSON array, empty for an assertion.
pub(super) const CLAIM_COLUMNS: &str = "su.digest, a.name, c.value, c.time, so.name, r.name, c.cid,
     (SELECT json_group_array(rt.target) FROM retractions rt WHERE rt.claim = c.id)";

/// The joins that give [`CLAIM_COLUMNS`] their names.
pub(super) const CLAIM_JOINS: &str = "JOIN subjects su ON su.id = c.subject
     JOIN attributes a ON a.id = c.attribute
     JOIN sources so ON so.id = c.source
     LEFT JOIN runs r ON r.id = c.run";

/// A claim's fields as [`CLAIM_COLUMNS`] read them.
pub(super) type ClaimRow = (
    String,
    String,
    Option<String>,
    String,
    String,
    Option<String>,
    String,
    String,
);

/// Read [`CLAIM_COLUMNS`] from a row, starting at column 0.
pub(super) fn claim_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ClaimRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
    ))
}

/// The recursive table of placed subjects, for a query to open with:
/// every subject a place stands on, and every subject derived from one
/// of those.
///
/// A `file:path` led by `@` is a place inside another content — an
/// entry's path in its archive — and places nothing by itself: the
/// entry is present while its archive is, which the step below
/// answers, and a walk that finds the archive gone takes back the
/// archive's place, never the entry's. Only a place of the first kind
/// opens the recursion.
///
/// The step walks from a placed subject to what was derived from it:
/// its digest, spelled as the JSON string a standing value is, looked
/// up under `prov:origin` through `standing_lookup`. Joining the
/// other way round, on the value unquoted, has no index to use and reads
/// every derivation once per placed subject.
pub(super) fn placed() -> String {
    format!(
        "WITH RECURSIVE placed(subject) AS (
        SELECT subject FROM standing
         WHERE (attribute = (SELECT id FROM attributes WHERE name = '{FILE_PATH}')
                AND value NOT LIKE '\"@%')
            OR attribute = (SELECT id FROM attributes WHERE name = '{MAILBOX_PLACE}')
        UNION
        SELECT st.subject FROM placed
          JOIN subjects su ON su.id = placed.subject
          JOIN standing st ON st.attribute = (SELECT id FROM attributes WHERE name = '{PROV_ORIGIN}')
                          AND st.value = json_quote(su.digest)
    ) "
    )
}

impl Index {
    /// Everything the log says about one subject, in log order: along
    /// the chain, the head last.
    ///
    /// Retractions come back as the claims they are — interpreting them is
    /// the caller's policy, not the index's.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; the row-to-claim errors cannot happen
    /// for rows a fold wrote, but are propagated rather than sworn away.
    pub fn about(&self, subject: &Subject) -> Result<Vec<Claim>> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {CLAIM_COLUMNS}
             FROM claims c
             {CLAIM_JOINS}
             JOIN segments s ON s.id = c.segment
             WHERE su.digest = ?1
             ORDER BY s.seq, c.position"
        ))?;
        let rows = statement.query_map(params![subject.as_str()], claim_row)?;
        let mut claims = Vec::new();
        for row in rows {
            claims.push(claim(row?)?);
        }
        Ok(claims)
    }

    /// The ids of the assertions that stand for `attribute` on `subject`:
    /// those of `value` when one is given, every one otherwise. What a
    /// retraction names, looked up before it is written. Empty when
    /// nothing stands.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`.
    pub fn standing_ids(
        &self,
        subject: &Subject,
        attribute: &Attribute,
        value: Option<&Value>,
    ) -> Result<Vec<ClaimId>> {
        let mut statement = self.connection.prepare(
            "SELECT c.cid FROM claims c
             WHERE c.subject = (SELECT id FROM subjects WHERE digest = ?1)
               AND c.attribute = (SELECT id FROM attributes WHERE name = ?2)
               AND c.retract = 0 AND c.retracted = 0
               AND (?3 IS NULL OR c.value = ?3)
             ORDER BY c.id",
        )?;
        let rows = statement.query_map(
            params![
                subject.as_str(),
                attribute.as_str(),
                value.map(Value::to_string)
            ],
            |row| row.get::<_, String>(0),
        )?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(ClaimId::parse(&row?)?);
        }
        Ok(ids)
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

    /// What was won out of one subject: every subject whose
    /// `prov:origin` names it, one step down, in digest order.
    /// The [`Scope`] reads as in [`values`](Index::values): the standing
    /// origins, or every origin ever said. Each answer is one step; the
    /// whole tree is walked by asking again for each answer.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; the row-to-subject errors cannot
    /// happen for rows a fold wrote, but are propagated rather than sworn
    /// away.
    pub fn derived(&self, subject: &Subject, scope: Scope) -> Result<Vec<Subject>> {
        let mut statement = self.connection.prepare(&format!(
            // The origin's digest, spelled as the JSON string a value is,
            // finds its derivations through the lookup index; the walk
            // to grandchildren is the caller's, one step at a time.
            "SELECT DISTINCT su.digest
             FROM {} st JOIN subjects su ON su.id = st.subject
             WHERE st.attribute = (SELECT id FROM attributes WHERE name = '{PROV_ORIGIN}')
               AND st.value = json_quote(?1)
             ORDER BY su.digest",
            rows_of(scope)
        ))?;
        let rows = statement.query_map(params![subject.as_str()], |row| row.get::<_, String>(0))?;
        let mut subjects = Vec::new();
        for row in rows {
            subjects.push(Subject::parse(&row?)?);
        }
        Ok(subjects)
    }

    /// What one subject was won out of: every subject its `prov:origin`
    /// names, one step up, in digest order. The [`Scope`] reads as in
    /// [`values`](Index::values): the standing origins, or every origin
    /// ever said. Each answer is one step; the whole line of descent is
    /// walked by asking again for each answer. A value that does not
    /// spell a name is no step and is passed over.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`.
    pub fn origins(&self, subject: &Subject, scope: Scope) -> Result<Vec<Subject>> {
        let mut origins: Vec<Subject> = self
            .values(subject, &known_attribute(PROV_ORIGIN), scope)?
            .iter()
            .filter_map(Value::as_str)
            .filter_map(|digest| Subject::parse(digest).ok())
            .collect();
        origins.sort();
        origins.dedup();
        Ok(origins)
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
            sql.push_str(&placed());
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

    /// Every attribute a claim was made under, sorted, as of the last
    /// [`fold`](Index::fold), the open head included. An attribute whose
    /// values were all retracted is among them.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; the row-to-attribute error cannot
    /// happen for rows a fold wrote, but is propagated.
    pub fn attributes(&self) -> Result<Vec<Attribute>> {
        let mut statement = self
            .connection
            .prepare("SELECT name FROM attributes ORDER BY name")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut attributes = Vec::new();
        for row in rows {
            attributes.push(Attribute::parse(&row?)?);
        }
        Ok(attributes)
    }

    /// The [`attributes`](Index::attributes), each with the number of
    /// files that have a standing value of it: 0 for an attribute whose
    /// values were all retracted. Counting reads every standing claim,
    /// so it takes as long as the record is large.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; the row-to-attribute errors cannot
    /// happen for rows a fold wrote, but are propagated.
    pub fn attribute_counts(&self) -> Result<Vec<(Attribute, u64)>> {
        let mut statement = self.connection.prepare(
            "SELECT a.name, COUNT(DISTINCT st.subject)
             FROM attributes a LEFT JOIN standing st ON st.attribute = a.id
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
        let mut statement = self.connection.prepare(&format!(
            "SELECT st.value, su.digest
             FROM standing st JOIN subjects su ON su.id = st.subject
             WHERE st.attribute = (SELECT id FROM attributes WHERE name = '{FILE_PATH}')"
        ))?;
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
        match cutoff {
            None => self.standing_now(attribute),
            Some(cutoff) => self.standing_replayed(attribute, cutoff),
        }
    }

    /// [`standing_as_of`](Index::standing_as_of) without a cutoff: the
    /// standing set as it is.
    fn standing_now(&self, attribute: &str) -> Result<Vec<Standing>> {
        let mut statement = self.connection.prepare(
            "SELECT su.digest, st.value, st.time, c.cid
             FROM standing st
             JOIN subjects su ON su.id = st.subject
             JOIN claims c ON c.id = st.claim
             WHERE st.attribute = (SELECT id FROM attributes WHERE name = ?1)
             ORDER BY su.digest, st.value",
        )?;
        let rows = statement.query_map(params![attribute], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut standing = Vec::new();
        for row in rows {
            let (subject, value, asserted, claim) = row?;
            standing.push(Standing {
                subject: Subject::parse(&subject)?,
                value: serde_json::from_str(&value)?,
                asserted,
                claim: ClaimId::parse(&claim)?,
            });
        }
        Ok(standing)
    }

    /// [`standing_as_of`](Index::standing_as_of) with a cutoff: the
    /// claims up to it, replayed in log order.
    fn standing_replayed(&self, attribute: &str, cutoff: &str) -> Result<Vec<Standing>> {
        use std::collections::BTreeMap;

        // Subject, value, claim id, and the time of that claim.
        type Held = BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>;

        let mut statement = self.connection.prepare(
            "SELECT su.digest, c.value, c.time, c.cid,
                    (SELECT json_group_array(rt.target) FROM retractions rt WHERE rt.claim = c.id)
             FROM claims c
             JOIN subjects su ON su.id = c.subject
             JOIN segments s ON s.id = c.segment
             WHERE c.attribute = (SELECT id FROM attributes WHERE name = ?1)
               AND c.time <= ?2
             ORDER BY s.seq, c.position",
        )?;
        let rows = statement.query_map(params![attribute, cutoff], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;

        // Every assertion by subject and value, each under its id, so that
        // a retraction can take one of several assertions of a value away
        // and the others keep it standing. Retractions apply whichever
        // side of their target they stand on.
        let mut held: Held = BTreeMap::new();
        let mut where_is: HashMap<String, (String, String)> = HashMap::new();
        // Retracted ids, each with the subject of the retraction: it
        // retracts only a claim of that subject. The attribute is the
        // one asked for on both sides.
        let mut retracted: HashSet<(String, String)> = HashSet::new();
        for row in rows {
            let (subject, value, time, cid, targets) = row?;
            if let Some(value) = value {
                if retracted.contains(&(cid.clone(), subject.clone())) {
                    continue;
                }
                where_is.insert(cid.clone(), (subject.clone(), value.clone()));
                held.entry(subject)
                    .or_default()
                    .entry(value)
                    .or_default()
                    .insert(cid, time);
            } else {
                let targets: Vec<String> = serde_json::from_str(&targets)?;
                for target in targets {
                    if where_is
                        .get(&target)
                        .is_some_and(|(owner, _)| owner != &subject)
                    {
                        continue;
                    }
                    if let Some((owner, value)) = where_is.remove(&target)
                        && let Some(values) = held.get_mut(&owner)
                        && let Some(claims) = values.get_mut(&value)
                    {
                        claims.remove(&target);
                    }
                    retracted.insert((target, subject.clone()));
                }
            }
        }
        // The newest assertion of each value, by time and then by id.
        let folded: BTreeMap<String, BTreeMap<String, (String, String)>> = held
            .into_iter()
            .map(|(subject, values)| {
                let values = values
                    .into_iter()
                    .filter_map(|(value, claims)| {
                        claims
                            .into_iter()
                            .map(|(cid, time)| (time, cid))
                            .max()
                            .map(|newest| (value, newest))
                    })
                    .collect();
                (subject, values)
            })
            .collect();

        let mut standing = Vec::new();
        for (subject, values) in folded {
            if values.is_empty() {
                continue;
            }
            let subject = Subject::parse(&subject)?;
            for (value, (asserted, claim)) in values {
                standing.push(Standing {
                    subject: subject.clone(),
                    value: serde_json::from_str(&value)?,
                    asserted,
                    claim: ClaimId::parse(&claim)?,
                });
            }
        }
        Ok(standing)
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
            // Runs in record order of their first claim: the chain
            // between segments, time within one. The lowest segment and
            // the lowest position taken apart would put a run that went
            // on past a seal before one that began earlier.
            "WITH first AS (
                 SELECT run, seq, time, position FROM (
                     SELECT c.run, s.seq, c.time, c.position,
                            ROW_NUMBER() OVER (
                                PARTITION BY c.run ORDER BY s.seq, c.time, c.position
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
             ORDER BY f.seq, f.time, f.position",
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
    /// among `mimes`, and no standing [`prov:examined`](crate::vocabulary::PROV_EXAMINED)
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
                  WHERE attribute = (SELECT id FROM attributes WHERE name = '{FILE_MIME}')
                    AND value IN ({holes})
                 EXCEPT
                 SELECT subject FROM standing
                  WHERE attribute = (SELECT id FROM attributes WHERE name = '{PROV_EXAMINED}')
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
                  WHERE attribute = (SELECT id FROM attributes WHERE name = '{FILE_MIME}')
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
    /// [`prov:examined`](crate::vocabulary::PROV_EXAMINED) receipt naming exactly this
    /// source as its value. What the worklist subtracts wholesale, asked
    /// about one file — for a run that was handed names instead of a kind.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`.
    pub fn examined(&self, subject: &Subject, source: &Source) -> Result<bool> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT 1 FROM standing
              WHERE subject = (SELECT id FROM subjects WHERE digest = ?1)
                AND attribute = (SELECT id FROM attributes WHERE name = '{PROV_EXAMINED}')
                AND value = ?2
              LIMIT 1"
        ))?;
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
    /// `file:path`, a fetched message with its `mailbox:place`. A
    /// sighting with neither (a derived file never sat anywhere) answers
    /// with its `file:name`, every name the run spelled. Of these, a
    /// subject answers with the first kind it has, in that order. Empty
    /// when no claim carries the run.
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
        let mut statement = self.connection.prepare(&format!(
            "SELECT DISTINCT su.digest,
                    CASE a.name WHEN '{FILE_PATH}' THEN 0 WHEN '{MAILBOX_PLACE}' THEN 1 ELSE 2 END,
                    json_extract(c.value, '$')
             FROM claims c
             JOIN subjects su ON su.id = c.subject
             JOIN attributes a ON a.id = c.attribute
             WHERE c.run = (SELECT id FROM runs WHERE name = ?1)
               AND c.retract = 0
               AND a.name IN ('{FILE_PATH}', '{MAILBOX_PLACE}', '{FILE_NAME}')
               AND json_type(c.value) = 'text'
             ORDER BY su.digest, 2, c.value"
        ))?;
        let rows = statement.query_map(params![run.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u8>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut sightings: Vec<(Subject, Placement)> = Vec::new();
        let mut current: Option<(Subject, u8)> = None;
        for row in rows {
            let (digest, rank, spelled) = row?;
            // The rows of a subject come best kind first, so its first
            // row sets the kind it answers with.
            let subject = match &current {
                Some((subject, best)) if subject.as_str() == digest => {
                    if rank != *best {
                        continue;
                    }
                    subject.clone()
                }
                _ => {
                    let subject = Subject::parse(&digest)?;
                    current = Some((subject.clone(), rank));
                    subject
                }
            };
            sightings.push((subject, Placement::ranked(rank.into(), spelled)));
        }
        Ok(sightings)
    }

    /// Where one file stands now: its standing `file:path` values; without
    /// any, its standing `mailbox:place` values; without those, its
    /// standing `file:name` values. The same choice
    /// [`run_sightings`](Index::run_sightings) makes for a run, here from
    /// the standing set: a place that was retracted is not among them.
    /// All values of the kind come back, sorted; which of several places
    /// matters is the caller's to decide. Empty for a file with none of
    /// the three.
    ///
    /// # Errors
    ///
    /// As [`values`](Index::values).
    pub fn placements(&self, subject: &Subject) -> Result<Vec<Placement>> {
        for (rank, name) in PLACES.into_iter().enumerate() {
            let attribute = Attribute::parse(name)?;
            let spelled: Vec<Placement> = self
                .values(subject, &attribute, Scope::Held)?
                .into_iter()
                .filter_map(|value| match value {
                    Value::String(text) => Some(Placement::ranked(rank, text)),
                    _ => None,
                })
                .collect();
            if !spelled.is_empty() {
                return Ok(spelled);
            }
        }
        Ok(Vec::new())
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

/// A row back into the claim it was — through the validating constructors,
/// so the index cannot smuggle in what the log could not have held.
pub(super) fn claim(
    (subject, attribute, value, time, source, run, cid, retracts): ClaimRow,
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
    let targets: Vec<String> = serde_json::from_str(&retracts)?;
    let retracts = targets
        .iter()
        .map(|target| ClaimId::parse(target))
        .collect::<Result<Vec<_>>>()?;
    let id = ClaimId::parse(&cid)?;
    Ok(Claim::recorded(subject, attribute, value, time, source, run, retracts)?.with_id(id))
}

/// The rows a [`Scope`] reads: the standing set, or the claims.
pub(super) fn rows_of(scope: Scope) -> &'static str {
    match scope {
        Scope::Present | Scope::Held => "standing",
        Scope::Record => "claims",
    }
}

/// A count out of `SQLite` as the unsigned number it is.
pub(super) fn counted(count: i64) -> Result<u64> {
    u64::try_from(count)
        .map_err(|_| Error::Index(rusqlite::Error::IntegralValueOutOfRange(0, count)))
}
