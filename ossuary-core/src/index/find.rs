//! `find`: terms over attributes, parts of values and fields of the
//! claim, built into one SQL statement.

use std::fmt::Write as _;

use rusqlite::params;

use super::query::{placed, rows_of};
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
            sql.push_str(&placed());
        }
        sql.push_str("SELECT digest FROM subjects WHERE id IN (");
        let mut params: Vec<Sql> = Vec::new();
        if attributes.is_empty() && parts.is_empty() {
            let _ = write!(sql, "SELECT c.subject FROM {rows} WHERE 1");
            for (field, pattern) in &fields {
                field_clause(&mut sql, &mut params, *field, pattern)?;
            }
        }
        let mut arms = 0;
        for (attribute, pattern) in &attributes {
            if arms > 0 {
                sql.push_str(" INTERSECT ");
            }
            arms += 1;
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
        for (attribute, asked) in &parts {
            if arms > 0 {
                sql.push_str(" INTERSECT ");
            }
            arms += 1;
            part_arm(&mut sql, &mut params, (rows, row), attribute, asked);
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
/// `attribute` that matches every part term in `asked`. `rows` is the
/// row source of [`Index::find`] and `row` the alias of the row in it;
/// the caller appends the field conditions.
pub(super) fn part_arm(
    sql: &mut String,
    params: &mut Vec<rusqlite::types::Value>,
    (rows, row): (&str, &str),
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
        "SELECT c.subject FROM {rows} CROSS JOIN (SELECT {}) AS part
         WHERE {row}.attribute = (SELECT id FROM attributes WHERE name = ?)",
        columns.join(", ")
    );
    for (part, _) in asked {
        params.push(Sql::Text(part.json_path()));
    }
    params.push(Sql::Text(attribute.as_str().to_string()));
    for (i, (_, pattern)) in asked.iter().enumerate() {
        value_clause(sql, params, &format!("({row}.value -> part.p{i})"), pattern);
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
/// `c.time`: in claim time's friendlier spellings, the ones `--as-of`
/// takes. A date alone is the whole day, a range from a date opens with
/// the day, one up to a date closes with it, and a moment without its
/// `Z` is the moment. A quoted literal and a glob read as written.
pub(super) fn time_clause(
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
pub(super) fn field_clause(
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
pub(super) fn lacking(
    sql: &mut String,
    params: &mut Vec<rusqlite::types::Value>,
    missing: &[String],
    rows: &str,
) -> Result<()> {
    use rusqlite::types::Value as Sql;
    let text = |s: &str| Sql::Text(s.to_string());
    for absent in missing {
        if !absent.ends_with(':') && absent.contains('.') {
            // A part is missing when no value of the attribute has it.
            let part = Part::parse(absent)?;
            let _ = write!(
                sql,
                " EXCEPT SELECT subject FROM {rows} CROSS JOIN (SELECT ? AS path) AS part
                  WHERE attribute = (SELECT id FROM attributes WHERE name = ?)
                    AND (value -> part.path) IS NOT NULL"
            );
            params.push(text(&part.json_path()));
            params.push(text(part.attribute().as_str()));
            continue;
        }
        let _ = write!(
            sql,
            " EXCEPT SELECT subject FROM {rows} WHERE attribute IN (SELECT id FROM attributes WHERE "
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
