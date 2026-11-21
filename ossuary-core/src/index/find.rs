//! `find`: terms over attributes, parts of values and fields of the
//! claim, built into one SQL statement.

use std::fmt::Write as _;

use rusqlite::params;

use super::query::{placed_hits, read_by};
use super::{Field, Index, Scope, Term};
use crate::claim::{Attribute, Subject, Timestamp, Value};
use crate::error::{Error, Result};
use crate::part::Part;

impl Index {
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
    /// A part term reads its pattern the same way against one part of a
    /// value, in the part's JSON spelling; a value without that part does
    /// not match. All part terms on one attribute must hold on the same
    /// value: `geo:position.latitude=48.1..48.2` and
    /// `geo:position.longitude=11.5..11.6` together find a position inside
    /// both ranges, never the latitude of one position and the longitude
    /// of another.
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
    /// ending in `:` it names a whole namespace, and with a path a part
    /// that none of the attribute's values has. With no terms at all,
    /// `missing` is asked of every subject the log speaks about.
    ///
    /// Under [`Scope::Present`] and [`Scope::Held`] only standing claims
    /// are read: assertions that no retraction names. A retracted claim
    /// finds nothing, however long it stays in the log. A value recorded
    /// by several claims matches a field term through any of them:
    /// `run=X file:name=abc` finds the file when run X recorded the name
    /// and that claim is not retracted, whether or not another run
    /// recorded the name as well. Under [`Scope::Record`] every claim
    /// ever written is read, retractions included, which is the one way
    /// to `retract=true`.
    ///
    /// # Errors
    ///
    /// [`Error::Index`] from `SQLite`; an entry of `missing` that fits
    /// neither the attribute grammar nor `namespace:` is refused with the
    /// grammar's own error; [`Error::Retract`] for a `retract` pattern
    /// that is neither true nor false.
    pub fn find(&self, terms: &[Term], missing: &[String], scope: Scope) -> Result<Vec<Subject>> {
        if terms.is_empty() && missing.is_empty() {
            return Ok(Vec::new());
        }
        let (sql, params) = find_statement(terms, missing, scope)?;
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
    /// included. Under the standing scopes the subject's standing
    /// claims; under [`Scope::Record`] every claim about it. A string field answers strings, `value` the
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
        let mut statement = self
            .connection
            .prepare(&field_values_statement(field, scope))?;
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
}

/// The statement of a [`field_values`](Index::field_values): one column,
/// the distinct values of `field`, with the subject's digest as its one
/// parameter.
pub(super) fn field_values_statement(field: Field, scope: Scope) -> String {
    let named = |name: &str, table: &str, alias: &str, column: &str| {
        (
            format!("{alias}.{name}"),
            format!(" JOIN {table} {alias} ON {alias}.id = c.{column}"),
        )
    };
    let (expression, join) = match field {
        Field::Subject => named("digest", "subjects", "su", "subject"),
        Field::Attribute => named("name", "attributes", "a", "attribute"),
        Field::Source => named("name", "sources", "so", "source"),
        Field::Run => named("name", "runs", "r", "run"),
        Field::Value => ("c.value".to_string(), String::new()),
        Field::Time => ("c.time".to_string(), String::new()),
        Field::Retract => ("c.retract".to_string(), String::new()),
    };
    format!(
        "SELECT DISTINCT {expression} FROM claims c INDEXED BY claims_subject{join}
         WHERE c.subject = (SELECT id FROM subjects WHERE digest = ?1)
           AND {expression} IS NOT NULL
           AND {}
         ORDER BY 1",
        read_by(scope, "c")
    )
}

/// The statement of a [`find`](Index::find) with its parameters: one
/// column, the digests of the matching subjects in order.
pub(super) fn find_statement(
    terms: &[Term],
    missing: &[String],
    scope: Scope,
) -> Result<(String, Vec<rusqlite::types::Value>)> {
    use rusqlite::types::Value as Sql;
    let attributes: Vec<(&Attribute, &str)> = terms
        .iter()
        .filter_map(|term| match term {
            Term::Attribute(attribute, pattern) => Some((attribute, pattern.as_str())),
            Term::Part(..) | Term::Field(..) => None,
        })
        .collect();
    let parts = parts_by_attribute(terms);
    let fields: Vec<(Field, &str)> = terms
        .iter()
        .filter_map(|term| match term {
            Term::Field(field, pattern) => Some((*field, pattern.as_str())),
            Term::Attribute(..) | Term::Part(..) => None,
        })
        .collect();
    // Every term is a condition on one claim `c`, among the claims the
    // scope reads.
    let read = read_by(scope, "c");
    // The terms meet as sets of subject ids in `hits`; the names come
    // last.
    let mut sql = String::from("WITH RECURSIVE hits(subject) AS (");
    let mut params: Vec<Sql> = Vec::new();
    if attributes.is_empty() && parts.is_empty() {
        let _ = write!(sql, "SELECT c.subject FROM claims c WHERE {read}");
        field_clauses(&mut sql, &mut params, &fields, None)?;
    }
    let mut arms = 0;
    for (attribute, pattern) in &attributes {
        if arms > 0 {
            sql.push_str(" INTERSECT ");
        }
        arms += 1;
        let _ = write!(
            sql,
            "SELECT c.subject FROM claims c
             WHERE {read} AND c.attribute = (SELECT id FROM attributes WHERE name = ?)"
        );
        params.push(Sql::Text(attribute.as_str().to_string()));
        value_clause(&mut sql, &mut params, "c.value", pattern);
        field_clauses(&mut sql, &mut params, &fields, Some(attribute))?;
    }
    for (attribute, asked) in &parts {
        if arms > 0 {
            sql.push_str(" INTERSECT ");
        }
        arms += 1;
        part_arm(&mut sql, &mut params, &read, attribute, asked);
        field_clauses(&mut sql, &mut params, &fields, Some(attribute))?;
    }
    lacking(&mut sql, &mut params, missing, &read)?;
    sql.push(')');
    if scope == Scope::Present {
        sql.push_str(", ");
        sql.push_str(&placed_hits());
        sql.push_str("SELECT digest FROM subjects WHERE id IN (SELECT subject FROM placed_hits)");
    } else {
        sql.push_str(" SELECT digest FROM subjects WHERE id IN (SELECT subject FROM hits)");
    }
    sql.push_str(" ORDER BY digest");
    Ok((sql, params))
}

/// The part terms among `terms`, gathered by attribute in the order
/// first named: the part terms of one attribute are asked of one value.
pub(super) fn parts_by_attribute(terms: &[Term]) -> Vec<(&Attribute, Vec<(&Part, &str)>)> {
    let mut parts: Vec<(&Attribute, Vec<(&Part, &str)>)> = Vec::new();
    for term in terms {
        if let Term::Part(part, pattern) = term {
            let asked = (part, pattern.as_str());
            match parts
                .iter_mut()
                .find(|(attribute, _)| *attribute == part.attribute())
            {
                Some((_, gathered)) => gathered.push(asked),
                None => parts.push((part.attribute(), vec![asked])),
            }
        }
    }
    parts
}

/// One arm of a `find`, appended to `sql`: the subjects with a value of
/// `attribute` that matches every part term in `asked`, among the
/// claims `c` for which `read` holds. The caller appends the field
/// conditions.
pub(super) fn part_arm(
    sql: &mut String,
    params: &mut Vec<rusqlite::types::Value>,
    read: &str,
    attribute: &Attribute,
    asked: &[(&Part, &str)],
) {
    use rusqlite::types::Value as Sql;
    // The paths are bound once, as the columns of a one-row table:
    // `value_clause` writes its column several times and binds only its
    // own parameters.
    let columns: Vec<String> = (0..asked.len()).map(|i| format!("? AS p{i}")).collect();
    let _ = write!(
        sql,
        "SELECT c.subject FROM claims c CROSS JOIN (SELECT {}) AS part
         WHERE {read} AND c.attribute = (SELECT id FROM attributes WHERE name = ?)",
        columns.join(", ")
    );
    for (part, _) in asked {
        params.push(Sql::Text(part.json_path()));
    }
    params.push(Sql::Text(attribute.as_str().to_string()));
    for (i, (_, pattern)) in asked.iter().enumerate() {
        value_clause(sql, params, &format!("(c.value -> part.p{i})"), pattern);
    }
}

/// The pattern of an attribute term, appended to `sql` as a condition
/// on `column`, a value in the log's JSON spelling: literal in quotes,
/// glob, range, or exact.
pub(super) fn value_clause(
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
pub(super) fn text_clause(
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
/// `column`: in claim time's friendlier spellings, the ones `--as-of`
/// takes. A date alone is the whole day, a range from a date opens with
/// the day, one up to a date closes with it, and a moment without its
/// `Z` is the moment. A quoted literal and a glob read as written.
pub(super) fn time_clause(
    sql: &mut String,
    params: &mut Vec<rusqlite::types::Value>,
    column: &str,
    pattern: &str,
) -> Result<()> {
    use rusqlite::types::Value as Sql;
    let quoted = pattern.starts_with('"') && pattern.ends_with('"') && pattern.len() >= 2;
    if quoted || pattern.contains('*') || pattern.contains('?') {
        text_clause(sql, params, column, pattern);
    } else if let Some((low, high)) = pattern.split_once("..") {
        if !low.is_empty() {
            let _ = write!(sql, " AND {column} >= ?");
            params.push(Sql::Text(Timestamp::opening(low)?.as_str().to_string()));
        }
        if !high.is_empty() {
            let _ = write!(sql, " AND {column} <= ?");
            params.push(Sql::Text(Timestamp::closing(high)?.as_str().to_string()));
        }
    } else if Timestamp::is_date(pattern) {
        let _ = write!(sql, " AND {column} BETWEEN ? AND ?");
        params.push(Sql::Text(Timestamp::opening(pattern)?.as_str().to_string()));
        params.push(Sql::Text(Timestamp::closing(pattern)?.as_str().to_string()));
    } else {
        let _ = write!(sql, " AND {column} = ?");
        params.push(Sql::Text(Timestamp::closing(pattern)?.as_str().to_string()));
    }
    Ok(())
}

/// How a term on `run`, `source` or `time` is written into one arm of a
/// `find`. The three are in no index together with a value, so read
/// carelessly they cost one table row per candidate claim: 64 s for a
/// run of 15 000 claims beside a value that 280 000 claims have, with
/// the cache on a network share.
#[derive(Debug, Clone, Copy)]
enum Check<'a> {
    /// A plain condition. It may choose the index the arm is read by.
    Leading,
    /// A condition on a row that is read anyway. It chooses no index.
    Row,
    /// A set of claim ids read from the field's own index, here those of
    /// one attribute. The arm compares the ids while it reads its own
    /// index, before it reads a table row.
    Ids(Option<&'a Attribute>),
}

/// The field terms of one arm, appended to `sql`. `attribute` is the
/// attribute of the arm's attribute or part term.
///
/// An arm reads one index and checks the other terms against it. With a
/// `subject` term that is the few claims of the subject, and every other
/// term is checked on their rows. With an attribute it is `claims_value`,
/// which has no run, source or time, so those are checked as sets of
/// ids. Without either, the first term on run, time or source is the
/// index, and the others are sets of ids.
fn field_clauses(
    sql: &mut String,
    params: &mut Vec<rusqlite::types::Value>,
    fields: &[(Field, &str)],
    attribute: Option<&Attribute>,
) -> Result<()> {
    let has = |wanted: Field| fields.iter().any(|(field, _)| *field == wanted);
    let mut leading = [Field::Run, Field::Time, Field::Source]
        .into_iter()
        .find(|field| has(*field));
    for (field, pattern) in fields {
        let check = if has(Field::Subject) {
            Check::Row
        } else if attribute.is_some() || has(Field::Attribute) {
            Check::Ids(attribute)
        } else if leading == Some(*field) {
            leading = None;
            Check::Leading
        } else {
            Check::Ids(None)
        };
        field_clause(sql, params, *field, pattern, check)?;
    }
    Ok(())
}

/// One field term as a condition on the claim `c`, appended to `sql`.
/// `check` is how a term on `run`, `source` or `time` is written; the
/// other fields are plain conditions.
fn field_clause(
    sql: &mut String,
    params: &mut Vec<rusqlite::types::Value>,
    field: Field,
    pattern: &str,
    check: Check<'_>,
) -> Result<()> {
    use rusqlite::types::Value as Sql;
    let named = |sql: &mut String, params: &mut Vec<Sql>, column: &str, table: &str, name: &str| {
        let _ = write!(sql, " AND {column} IN (SELECT id FROM {table} WHERE 1");
        text_clause(sql, params, name, pattern);
        sql.push(')');
    };
    // A set of ids is read from the index led by the field. The index
    // is named, because with the attribute in the condition SQLite
    // would read `claims_value` and a table row per claim.
    let ids = |sql: &mut String, index: Option<&str>| {
        sql.push_str(" AND +c.id IN (SELECT x.id FROM claims x");
        if let Some(index) = index {
            let _ = write!(sql, " INDEXED BY {index}");
        }
        sql.push_str(" WHERE 1");
    };
    let of_attribute = |sql: &mut String, params: &mut Vec<Sql>, attribute: Option<&Attribute>| {
        if let Some(attribute) = attribute {
            sql.push_str(" AND x.attribute = (SELECT id FROM attributes WHERE name = ?)");
            params.push(Sql::Text(attribute.as_str().to_string()));
        }
        sql.push(')');
    };
    match field {
        Field::Subject => named(sql, params, "c.subject", "subjects", "digest"),
        Field::Attribute => named(sql, params, "c.attribute", "attributes", "name"),
        Field::Run | Field::Source => {
            let (column, table, index) = match field {
                Field::Run => ("run", "runs", "claims_run"),
                _ => ("source", "sources", "claims_source"),
            };
            match check {
                Check::Leading => named(sql, params, &format!("c.{column}"), table, "name"),
                Check::Row => named(sql, params, &format!("+c.{column}"), table, "name"),
                Check::Ids(attribute) => {
                    ids(sql, Some(index));
                    named(sql, params, &format!("x.{column}"), table, "name");
                    of_attribute(sql, params, attribute);
                }
            }
        }
        Field::Time => match check {
            Check::Leading => time_clause(sql, params, "c.time", pattern)?,
            Check::Row => time_clause(sql, params, "+c.time", pattern)?,
            Check::Ids(_) => {
                // `claims_time` has no attribute, and the time is the
                // only condition, so the index needs no naming.
                ids(sql, None);
                time_clause(sql, params, "x.time", pattern)?;
                sql.push(')');
            }
        },
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
/// in `:`, a whole namespace, among the claims `c` for which `read`
/// holds.
pub(super) fn lacking(
    sql: &mut String,
    params: &mut Vec<rusqlite::types::Value>,
    missing: &[String],
    read: &str,
) -> Result<()> {
    use rusqlite::types::Value as Sql;
    let text = |s: &str| Sql::Text(s.to_string());
    for absent in missing {
        if !absent.ends_with(':') && absent.contains('.') {
            // A part is missing when no value of the attribute has it.
            let part = Part::parse(absent)?;
            let _ = write!(
                sql,
                " EXCEPT SELECT c.subject FROM claims c CROSS JOIN (SELECT ? AS path) AS part
                  WHERE {read} AND c.attribute = (SELECT id FROM attributes WHERE name = ?)
                    AND (c.value -> part.path) IS NOT NULL"
            );
            params.push(text(&part.json_path()));
            params.push(text(part.attribute().as_str()));
            continue;
        }
        let _ = write!(
            sql,
            " EXCEPT SELECT c.subject FROM claims c
              WHERE {read} AND c.attribute IN (SELECT id FROM attributes WHERE "
        );
        if let Some(namespace) = absent.strip_suffix(':') {
            // The grammar is defined only in Attribute::parse, so a
            // namespace is checked with a placeholder name. The error
            // names the entry as given, which has no placeholder.
            Attribute::parse(&format!("{namespace}:a"))
                .map_err(|_| Error::Attribute(absent.clone()))?;
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
