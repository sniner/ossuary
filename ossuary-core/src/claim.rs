//! The claim: one fact, one line.
//!
//! All metadata in an ossuary archive is claims — small, self-describing,
//! append-only facts, one JSON object per line. The shape is fixed by
//! generation 1 of the format (`docs/format.md`): seven fields and no
//! eighth, `subject`, `attribute`, `value`, `time`, `source`, `run` and
//! `retract`. Nothing is ever updated or deleted in place; a correction is
//! a newer claim, a deletion is a retraction, and the log only grows. Every
//! claim this crate writes names its run; a claim written before there was
//! a run to name reads like any other, and has none.
//!
//! Every field with rules of its own is a type of its own, and parsing
//! validates: a [`Subject`], [`Attribute`], [`Timestamp`], [`Source`] or
//! [`Run`] in hand is always well-formed, and a [`Claim`] read back from a
//! line is one this crate could have written.

use std::fmt;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{Error, Result};

/// A claim's value: any JSON value — with `null` refused at every door.
///
/// Re-exported from `serde_json` so that callers build values with the tools
/// they already have, `serde_json::json!` included.
pub use serde_json::Value;

/// The digest lengths a subject may have, in hex characters — those of the
/// algorithms immure speaks: 32, 48 or 64 bytes.
const DIGEST_LENGTHS: [usize; 3] = [64, 96, 128];

/// What a claim is about: a blob, named by its content — the digest as bare
/// lowercase hex, nothing around it.
///
/// Which algorithm made the name is never the line's business: an archive
/// names all content with the one algorithm its `FORMAT` mark declares, for
/// good. The path in the store is this same hex, sharded.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Subject(String);

impl Subject {
    /// Validate a bare digest.
    ///
    /// The hex must be the full length of a digest — a subject names one
    /// blob, not the beginning of one. Uppercase hex is normalised to
    /// lowercase, the way blobs are named on disk.
    ///
    /// # Errors
    ///
    /// [`Error::Subject`] for anything else.
    pub fn parse(s: &str) -> Result<Self> {
        if !DIGEST_LENGTHS.contains(&s.len()) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Subject(s.to_string()));
        }
        Ok(Subject(s.to_ascii_lowercase()))
    }

    /// The digest as lowercase hex — the whole name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What is being said: `namespace:attribute`.
///
/// Lowercase `a-z`, digits and `-`, one colon, both halves non-empty.
/// Validation stops at the grammar on purpose: unknown attributes are legal,
/// because a claim nobody understands yet is a queue entry, not an error.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Attribute(String);

impl Attribute {
    /// Validate `namespace:attribute`.
    ///
    /// # Errors
    ///
    /// [`Error::Attribute`] for anything outside the grammar.
    pub fn parse(s: &str) -> Result<Self> {
        let error = || Error::Attribute(s.to_string());
        let (namespace, name) = s.split_once(':').ok_or_else(error)?;
        let word = |part: &str| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        };
        if !word(namespace) || !word(name) {
            return Err(error());
        }
        Ok(Attribute(s.to_string()))
    }

    /// The whole attribute, colon included.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The half before the colon.
    #[must_use]
    pub fn namespace(&self) -> &str {
        self.0
            .split_once(':')
            .map_or("", |(namespace, _)| namespace)
    }

    /// The half after the colon.
    #[must_use]
    pub fn name(&self) -> &str {
        self.0.split_once(':').map_or("", |(_, name)| name)
    }
}

/// When the claim was recorded: RFC 3339, UTC, `Z`, whole seconds —
/// `2026-09-01T21:14:03Z` and no other shape.
///
/// One fixed form out of the many ISO 8601 allows, so that a timestamp is
/// greppable, unambiguous in fifty years, and comparable as a string: the
/// derived ordering *is* chronological order, which only holds because the
/// shape never varies. Within one second the order of claims is the order of
/// the log, so seconds are all the precision the format needs.
///
/// This is when the claim was *recorded*, not when whatever it describes
/// happened — those dates live in values.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Timestamp(String);

impl Timestamp {
    /// Validate `YYYY-MM-DDThh:mm:ssZ`.
    ///
    /// The calendar is checked, leap days included. Seconds run to 60: RFC
    /// 3339 admits the leap second, and a reader must not choke on a log that
    /// recorded one — this crate never writes one.
    ///
    /// # Errors
    ///
    /// [`Error::Timestamp`] for any other shape, and for a date or time that
    /// does not exist.
    pub fn parse(s: &str) -> Result<Self> {
        let error = || Error::Timestamp(s.to_string());
        let bytes = s.as_bytes();
        let shape = bytes.len() == 20
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes[10] == b'T'
            && bytes[13] == b':'
            && bytes[16] == b':'
            && bytes[19] == b'Z';
        if !shape {
            return Err(error());
        }
        let field = |start: usize, len: usize| -> Option<u32> {
            let part = &s[start..start + len];
            part.bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| part.parse().ok())
                .flatten()
        };
        let (year, month, day) = (field(0, 4), field(5, 2), field(8, 2));
        let (hour, minute, second) = (field(11, 2), field(14, 2), field(17, 2));
        let (Some(year), Some(month), Some(day)) = (year, month, day) else {
            return Err(error());
        };
        let (Some(hour), Some(minute), Some(second)) = (hour, minute, second) else {
            return Err(error());
        };
        let date = (1..=12).contains(&month) && (1..=days_in(year, month)).contains(&day);
        if !date || hour > 23 || minute > 59 || second > 60 {
            return Err(error());
        }
        Ok(Timestamp(s.to_string()))
    }

    /// The moment a friendlier spelling names, read as the end of what
    /// it names: a date alone is that day's last second, a full moment
    /// is itself, with or without the trailing `Z`. "As of the first"
    /// means the first has happened; a range up to a day includes the
    /// day.
    ///
    /// # Errors
    ///
    /// [`Error::Timestamp`] for any other shape, and for a date or time
    /// that does not exist.
    pub fn closing(given: &str) -> Result<Self> {
        Self::friendly(given, "T23:59:59")
    }

    /// The moment a friendlier spelling names, read as the beginning of
    /// what it names: a date alone is that day's first second, a full
    /// moment is itself, with or without the trailing `Z`. A range from
    /// a day begins with the day.
    ///
    /// # Errors
    ///
    /// [`Error::Timestamp`] for any other shape, and for a date or time
    /// that does not exist.
    pub fn opening(given: &str) -> Result<Self> {
        Self::friendly(given, "T00:00:00")
    }

    /// Whether `given` is a date alone — the shape a friendlier spelling
    /// widens to a whole day.
    #[must_use]
    pub fn is_date(given: &str) -> bool {
        let bytes = given.as_bytes();
        bytes.len() == 10
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes
                .iter()
                .enumerate()
                .all(|(at, byte)| matches!(at, 4 | 7) || byte.is_ascii_digit())
    }

    /// The friendlier spellings, with a date alone widened by `edge`.
    /// The error names what was given, not what it was widened to.
    fn friendly(given: &str, edge: &str) -> Result<Self> {
        let mut spelled = given.trim().to_string();
        if Self::is_date(&spelled) {
            spelled.push_str(edge);
        }
        if spelled.len() == 19 && !spelled.ends_with('Z') {
            spelled.push('Z');
        }
        Self::parse(&spelled).map_err(|_| Error::Timestamp(given.to_string()))
    }

    /// The timestamp of a moment in Unix time — seconds since the epoch,
    /// negative for the years before it.
    ///
    /// For clocks the filesystem hands over, an mtime above all. The result
    /// goes back through [`parse`](Timestamp::parse) on its way out, so
    /// parsing stays the only door in.
    ///
    /// # Errors
    ///
    /// [`Error::Timestamp`] when the year falls outside 0000–9999: the
    /// format has four digits, and an mtime from a broken clock should be
    /// refused, not folded into the shape.
    pub fn from_unix(seconds: i64) -> Result<Self> {
        let days = seconds.div_euclid(86_400);
        let rest = seconds.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        if !(0..=9999).contains(&year) {
            return Err(Error::Timestamp(format!("unix time {seconds}")));
        }
        let (hour, minute, second) = (rest / 3600, rest % 3600 / 60, rest % 60);
        Timestamp::parse(&format!(
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
        ))
    }

    /// The moment an RFC 3339 time from another program names, in this
    /// format's shape: the offset applied, fractional seconds dropped.
    /// `2024-03-01T02:00:03.512345+01:00` becomes `2024-03-01T01:00:03Z`.
    ///
    /// A time without an offset is refused: its time zone is unknown, so
    /// it names no moment.
    ///
    /// # Errors
    ///
    /// [`Error::Timestamp`] for any other shape, and for a date or time
    /// that does not exist.
    pub fn from_rfc3339(given: &str) -> Result<Self> {
        let error = || Error::Timestamp(given.to_string());
        let (Some(base), Some(rest)) = (given.get(..19), given.get(19..)) else {
            return Err(error());
        };
        let whole = Self::parse(&format!("{base}Z")).map_err(|_| error())?;
        let rest = match rest.strip_prefix('.') {
            Some(fraction) => {
                let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
                if digits == 0 {
                    return Err(error());
                }
                &fraction[digits..]
            }
            None => rest,
        };
        let offset = match rest.as_bytes() {
            b"Z" | b"z" => 0,
            [sign @ (b'+' | b'-'), h1, h2, b':', m1, m2]
                if [h1, h2, m1, m2].iter().all(|b| b.is_ascii_digit()) =>
            {
                let hours = i64::from((h1 - b'0') * 10 + (h2 - b'0'));
                let minutes = i64::from((m1 - b'0') * 10 + (m2 - b'0'));
                if hours > 23 || minutes > 59 {
                    return Err(error());
                }
                let seconds = hours * 3600 + minutes * 60;
                if *sign == b'-' { -seconds } else { seconds }
            }
            _ => return Err(error()),
        };
        if offset == 0 {
            return Ok(whole);
        }
        Self::from_unix(whole.unix() - offset).map_err(|_| error())
    }

    /// Seconds since the Unix epoch, negative before it.
    fn unix(&self) -> i64 {
        let field = |start: usize, len: usize| -> u32 {
            self.0[start..start + len]
                .parse()
                .expect("the shape was checked at parsing")
        };
        let days = days_from_civil(i64::from(field(0, 4)), field(5, 2), field(8, 2));
        days * 86_400
            + i64::from(field(11, 2)) * 3600
            + i64::from(field(14, 2)) * 60
            + i64::from(field(17, 2))
    }

    /// This very second, UTC.
    ///
    /// A system clock standing before 1970 is read as the epoch — a broken
    /// clock should not keep facts out of the log.
    ///
    /// # Panics
    ///
    /// In the year 10000, when the four digits run out — a problem this
    /// crate is content to leave to its successors.
    #[must_use]
    pub fn now() -> Self {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let seconds = i64::try_from(elapsed.as_secs()).unwrap_or(0);
        Timestamp::from_unix(seconds).expect("the year is still four digits")
    }

    /// The timestamp, exactly as it stands in the line.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The civil date a day count since 1970-01-01 falls on — Howard Hinnant's
/// `civil_from_days`, integer arithmetic all the way down.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (
        year,
        u32::try_from(month).expect("a month is 1..=12"),
        u32::try_from(day).expect("a day is 1..=31"),
    )
}

/// The day count since 1970-01-01 of a civil date: the inverse of
/// [`civil_from_days`], from the same source.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let month = i64::from(month);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// How many days `month` has in `year`.
fn days_in(year: u32, month: u32) -> u32 {
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Who says so: `ingest`, `user`, or `kind:name/version` for tooling —
/// `extractor:exif-rs/0.7`.
///
/// Deliberately a flat string with a convention rather than a structure: a
/// fold supersedes by prefix — "everything from `extractor:exif-rs/` older
/// than 2.0" — and a new kind of source is a new convention, not a schema
/// change. Validation stops at what would break the line: empty says
/// nothing, and whitespace and control characters have no place in it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Source(String);

impl Source {
    /// Validate a source.
    ///
    /// # Errors
    ///
    /// [`Error::Source`] when it is empty or holds whitespace or control
    /// characters.
    pub fn parse(s: &str) -> Result<Self> {
        if s.is_empty() || s.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(Error::Source(s.to_string()));
        }
        Ok(Source(s.to_string()))
    }

    /// The source string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The call a claim was written in: one id per invocation of whatever
/// writes — an ingest, an extract, a fetch, an annotation, a retraction —
/// stamped on every claim of that call, its rounds included.
///
/// Where [`Source`] says who was speaking and [`Timestamp`] when, the run
/// says in which breath: "arrived together", "taken back in the same
/// sweep" are exact because of it, and a moment in the log can be named
/// by the call that closed it. A UUID in its dashed form, so that a run's
/// name is never mistaken for a subject's.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Run(String);

impl Run {
    /// A fresh id for one call.
    #[must_use]
    #[allow(
        clippy::new_without_default,
        reason = "a fresh id is the opposite of a default value"
    )]
    pub fn new() -> Self {
        Run(Uuid::new_v4().to_string())
    }

    /// Validate a run id: the dashed UUID, lowercase, whole.
    ///
    /// # Errors
    ///
    /// [`Error::Run`] for any other shape.
    pub fn parse(s: &str) -> Result<Self> {
        if !Run::spelled(s) {
            return Err(Error::Run(s.to_string()));
        }
        Ok(Run(s.to_string()))
    }

    /// Whether `s` has the shape of a run id — the grammar alone, for a
    /// caller telling a run from a subject before either is looked up.
    #[must_use]
    pub fn spelled(s: &str) -> bool {
        let bytes = s.as_bytes();
        bytes.len() == 36
            && bytes
                .iter()
                .enumerate()
                .all(|(position, byte)| match position {
                    8 | 13 | 18 | 23 => *byte == b'-',
                    _ => byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase(),
                })
    }

    /// The id as the log spells it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What one call put on the record: how many claims, and under which
/// run — so a verdict can name the run every one of them carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// Claims appended to the log.
    pub claims: usize,
    /// The run they were written in.
    pub run: Run,
}

/// One fact: an attribute of a subject has a value — who says so, when,
/// and in which call.
///
/// Three shapes and no fourth, enforced at construction and again at
/// parsing:
///
/// - an **assertion** carries a value and no `retract`
/// - a **retraction of one value** carries the value and `retract: true`
/// - a **retraction of the whole attribute** carries `retract: true` and no
///   value at all
///
/// A retraction is a claim like any other — stamped, sourced, and never
/// removing from the log what it retracts.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Claim {
    subject: Subject,
    attribute: Attribute,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<Value>,
    time: Timestamp,
    source: Source,
    #[serde(skip_serializing_if = "Option::is_none")]
    run: Option<Run>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    retract: bool,
}

impl Claim {
    /// Assert a fact: `attribute` of `subject` has `value`.
    ///
    /// # Errors
    ///
    /// [`Error::NullValue`] — `null` is not a value.
    pub fn assert(
        subject: Subject,
        attribute: Attribute,
        value: Value,
        time: Timestamp,
        source: Source,
        run: Run,
    ) -> Result<Self> {
        if value.is_null() {
            return Err(Error::NullValue);
        }
        Ok(Claim {
            subject,
            attribute,
            value: Some(value),
            time,
            source,
            run: Some(run),
            retract: false,
        })
    }

    /// Retract exactly this value of the attribute.
    ///
    /// The value travels in the retraction because nothing else could say
    /// which of a multi-valued attribute's values is meant — which of three
    /// tags, say.
    ///
    /// # Errors
    ///
    /// [`Error::NullValue`] — `null` is not a value here either.
    pub fn retract_value(
        subject: Subject,
        attribute: Attribute,
        value: Value,
        time: Timestamp,
        source: Source,
        run: Run,
    ) -> Result<Self> {
        if value.is_null() {
            return Err(Error::NullValue);
        }
        Ok(Claim {
            subject,
            attribute,
            value: Some(value),
            time,
            source,
            run: Some(run),
            retract: true,
        })
    }

    /// Retract every value the attribute has for this subject.
    #[must_use]
    pub fn retract_attribute(
        subject: Subject,
        attribute: Attribute,
        time: Timestamp,
        source: Source,
        run: Run,
    ) -> Self {
        Claim {
            subject,
            attribute,
            value: None,
            time,
            source,
            run: Some(run),
            retract: true,
        }
    }

    /// A claim as the record holds it, run and all — for reading rows
    /// back from an index, where a claim from before runs has none.
    /// The three shapes are enforced as at every other door.
    ///
    /// # Errors
    ///
    /// [`Error::NullValue`] and [`Error::ValueRequired`] when the shape
    /// is not one of the three.
    pub(crate) fn recorded(
        subject: Subject,
        attribute: Attribute,
        value: Option<Value>,
        time: Timestamp,
        source: Source,
        run: Option<Run>,
        retract: bool,
    ) -> Result<Self> {
        Claim::try_from(RawClaim {
            subject,
            attribute,
            value,
            time,
            source,
            run,
            retract,
        })
    }

    /// Read one line of the log back.
    ///
    /// A line that parses is one this crate could have written: every field
    /// validated, the field set closed, the three shapes enforced.
    ///
    /// # Errors
    ///
    /// [`Error::Line`] for JSON that is broken, carries an unknown member or
    /// fails a field's validation; [`Error::NullValue`] and
    /// [`Error::ValueRequired`] when the fields are sound but the shape is
    /// not.
    pub fn parse_line(line: &str) -> Result<Self> {
        let raw: RawClaim = serde_json::from_str(line)?;
        Claim::try_from(raw)
    }

    /// The claim as its line: one JSON object, fields in the order of the
    /// format document, no trailing newline — appending that is the log's
    /// business.
    ///
    /// # Panics
    ///
    /// It does not, in practice: serialising a claim has no failure mode —
    /// every key is a struct field's name, and a [`Value`] holds no
    /// non-finite numbers. The panic stands in for the arm that cannot be
    /// reached, rather than a quiet empty line standing in for a claim.
    #[must_use]
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("a claim serialises")
    }

    /// What the claim is about.
    #[must_use]
    pub fn subject(&self) -> &Subject {
        &self.subject
    }

    /// What is being said.
    #[must_use]
    pub fn attribute(&self) -> &Attribute {
        &self.attribute
    }

    /// The value — `None` only on a whole-attribute retraction.
    #[must_use]
    pub fn value(&self) -> Option<&Value> {
        self.value.as_ref()
    }

    /// When the claim was recorded.
    #[must_use]
    pub fn time(&self) -> &Timestamp {
        &self.time
    }

    /// Who says so.
    #[must_use]
    pub fn source(&self) -> &Source {
        &self.source
    }

    /// In which call it was said — `None` on a claim from before runs
    /// were written down, which reads like any other and has none.
    #[must_use]
    pub fn run(&self) -> Option<&Run> {
        self.run.as_ref()
    }

    /// Whether this claim retracts rather than asserts.
    #[must_use]
    pub fn is_retraction(&self) -> bool {
        self.retract
    }
}

/// The shape on the wire, before the rules.
///
/// `deny_unknown_fields` is the closed field set of generation 1 in code: an
/// unknown member is a format violation, not an extension point — anything
/// that would add an eighth field is a new generation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClaim {
    subject: Subject,
    attribute: Attribute,
    #[serde(default, deserialize_with = "value_present")]
    value: Option<Value>,
    time: Timestamp,
    source: Source,
    #[serde(default)]
    run: Option<Run>,
    #[serde(default)]
    retract: bool,
}

/// A `value` key that is present arrives here — even `null`, which `Option`
/// on its own would fold into absence and thereby read `"value": null` as a
/// whole-attribute retraction. It must arrive, so that it can be refused.
fn value_present<'de, D>(deserializer: D) -> std::result::Result<Option<Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Value::deserialize(deserializer).map(Some)
}

impl TryFrom<RawClaim> for Claim {
    type Error = Error;

    fn try_from(raw: RawClaim) -> Result<Self> {
        match (&raw.value, raw.retract) {
            (Some(Value::Null), _) => Err(Error::NullValue),
            (None, false) => Err(Error::ValueRequired),
            _ => Ok(Claim {
                subject: raw.subject,
                attribute: raw.attribute,
                value: raw.value,
                time: raw.time,
                source: raw.source,
                run: raw.run,
                retract: raw.retract,
            }),
        }
    }
}

/// `Display`, `FromStr` and a validating `Deserialize` for the string
/// newtypes: parsing is the only door in, whichever way a value arrives.
macro_rules! string_newtype {
    ($type:ident) => {
        impl fmt::Display for $type {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $type {
            type Err = Error;

            fn from_str(s: &str) -> Result<Self> {
                $type::parse(s)
            }
        }

        impl<'de> Deserialize<'de> for $type {
            fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let raw = String::deserialize(deserializer)?;
                $type::parse(&raw).map_err(serde::de::Error::custom)
            }
        }
    };
}

string_newtype!(Subject);
string_newtype!(Attribute);
string_newtype!(Timestamp);
string_newtype!(Source);
string_newtype!(Run);

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn subject() -> Subject {
        Subject::parse("9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e").unwrap()
    }

    fn time() -> Timestamp {
        Timestamp::parse("2026-09-01T21:14:03Z").unwrap()
    }

    fn run() -> Run {
        Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap()
    }

    #[test]
    fn an_assertion_serialises_in_the_order_of_the_format_document() {
        let claim = Claim::assert(
            subject(),
            Attribute::parse("file:size").unwrap(),
            json!(4_194_304),
            time(),
            Source::parse("ingest").unwrap(),
            run(),
        )
        .unwrap();

        assert_eq!(
            claim.to_line(),
            r#"{"subject":"9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"file:size","value":4194304,"time":"2026-09-01T21:14:03Z","source":"ingest","run":"315e360b-020e-48be-8f2d-f2002a2ea9b4"}"#,
            "a number is a number, and an assertion carries no retract key"
        );
    }

    #[test]
    fn the_example_lines_of_the_format_document_round_trip() {
        // The examples from docs/format.md, with the digest at full length.
        let lines = [
            r#"{"subject":"9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"file:path","value":"/photos/2019/crete/beach.jpg","time":"2026-09-01T21:14:03Z","source":"ingest","run":"315e360b-020e-48be-8f2d-f2002a2ea9b4"}"#,
            r#"{"subject":"9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"file:size","value":4194304,"time":"2026-09-01T21:14:03Z","source":"ingest","run":"315e360b-020e-48be-8f2d-f2002a2ea9b4"}"#,
            r#"{"subject":"9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"exif:date-time-original","value":"2019-07-14T11:02:41","time":"2026-09-22T08:30:00Z","source":"extractor:exif-rs/0.7","run":"315e360b-020e-48be-8f2d-f2002a2ea9b4"}"#,
            r#"{"subject":"9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"user:tag","value":"holiday","time":"2026-10-05T19:00:00Z","source":"user","run":"315e360b-020e-48be-8f2d-f2002a2ea9b4"}"#,
            r#"{"subject":"9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"user:tag","value":"holiday","time":"2030-04-01T10:00:00Z","source":"user","run":"315e360b-020e-48be-8f2d-f2002a2ea9b4","retract":true}"#,
        ];
        for line in lines {
            let claim = Claim::parse_line(line).unwrap();
            assert_eq!(claim.to_line(), line, "reading and writing agree");
        }
    }

    #[test]
    fn a_value_retraction_carries_the_value_and_the_mark() {
        let claim = Claim::retract_value(
            subject(),
            Attribute::parse("user:tag").unwrap(),
            json!("holiday"),
            time(),
            Source::parse("user").unwrap(),
            run(),
        )
        .unwrap();

        assert!(claim.is_retraction());
        assert_eq!(claim.value(), Some(&json!("holiday")));
        assert!(claim.to_line().ends_with(r#""retract":true}"#));
    }

    #[test]
    fn an_attribute_retraction_carries_no_value_at_all() {
        let claim = Claim::retract_attribute(
            subject(),
            Attribute::parse("user:note").unwrap(),
            time(),
            Source::parse("user").unwrap(),
            run(),
        );

        assert!(claim.is_retraction());
        assert_eq!(claim.value(), None);
        assert!(
            !claim.to_line().contains(r#""value""#),
            "no value key, not a value of null"
        );
        assert_eq!(Claim::parse_line(&claim.to_line()).unwrap(), claim);
    }

    #[test]
    fn null_is_not_a_value_at_any_door() {
        assert!(matches!(
            Claim::assert(
                subject(),
                Attribute::parse("user:tag").unwrap(),
                Value::Null,
                time(),
                Source::parse("user").unwrap(),
                run(),
            ),
            Err(Error::NullValue)
        ));

        let line = r#"{"subject":"9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"user:tag","value":null,"time":"2026-09-01T21:14:03Z","source":"user","run":"315e360b-020e-48be-8f2d-f2002a2ea9b4"}"#;
        assert!(matches!(Claim::parse_line(line), Err(Error::NullValue)));

        // `"value": null` on a retraction is not the same line as no value
        // key: it must be refused, not read as a whole-attribute retraction.
        let line = r#"{"subject":"9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"user:tag","value":null,"time":"2026-09-01T21:14:03Z","source":"user","run":"315e360b-020e-48be-8f2d-f2002a2ea9b4","retract":true}"#;
        assert!(matches!(Claim::parse_line(line), Err(Error::NullValue)));
    }

    #[test]
    fn an_assertion_without_a_value_says_nothing() {
        let line = r#"{"subject":"9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"user:tag","time":"2026-09-01T21:14:03Z","source":"user","run":"315e360b-020e-48be-8f2d-f2002a2ea9b4"}"#;
        assert!(matches!(Claim::parse_line(line), Err(Error::ValueRequired)));
    }

    #[test]
    fn the_field_set_is_closed() {
        let line = r#"{"subject":"9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"user:tag","value":"holiday","time":"2026-09-01T21:14:03Z","source":"user","run":"315e360b-020e-48be-8f2d-f2002a2ea9b4","confidence":0.9}"#;
        assert!(
            matches!(Claim::parse_line(line), Err(Error::Line(_))),
            "an eighth field is a new generation, not an extension point"
        );
    }

    #[test]
    fn a_claim_from_before_runs_reads_like_any_other_and_has_none() {
        let line = r#"{"subject":"9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"user:tag","value":"holiday","time":"2026-09-01T21:14:03Z","source":"user"}"#;
        let claim = Claim::parse_line(line).unwrap();
        assert_eq!(claim.run(), None);
        assert_eq!(claim.to_line(), line, "and writes back as it was");
    }

    #[test]
    fn a_run_is_the_dashed_lowercase_uuid_whole() {
        assert!(Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").is_ok());
        assert!(
            Run::spelled(Run::new().as_str()),
            "a fresh id passes its own grammar"
        );
        for wrong in [
            "315e360b020e48be8f2df2002a2ea9b4",
            "315E360B-020E-48BE-8F2D-F2002A2EA9B4",
            "315e360b-020e-48be-8f2d-f2002a2ea9b",
            "",
        ] {
            assert!(
                matches!(Run::parse(wrong), Err(Error::Run(_))),
                "{wrong:?} is not a run id"
            );
        }
    }

    #[test]
    fn subjects_are_bare_full_length_digests_in_lowercase() {
        let subject =
            Subject::parse("9F2AC41E9F2AC41E9F2AC41E9F2AC41E9F2AC41E9F2AC41E9F2AC41E9F2AC41E")
                .unwrap();
        assert_eq!(
            subject.as_str(),
            "9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e"
        );
        assert!(
            Subject::parse(&"9f".repeat(48)).is_ok() && Subject::parse(&"9f".repeat(64)).is_ok(),
            "the longer digests immure speaks are names too"
        );

        for wrong in [
            "9f2ac41e",                                                           // not full length
            "9z2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e",   // not hex
            "9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e00", // not a digest's length
            "sha256:9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e", // an algorithm's name is no part of a subject
        ] {
            assert!(
                matches!(Subject::parse(wrong), Err(Error::Subject(_))),
                "{wrong}"
            );
        }
    }

    #[test]
    fn attributes_are_one_colon_of_lowercase_words() {
        let attribute = Attribute::parse("exif:date-time-original").unwrap();
        assert_eq!(attribute.namespace(), "exif");
        assert_eq!(attribute.name(), "date-time-original");

        for wrong in [
            "tag",            // no namespace
            "user:",          // empty name
            ":tag",           // empty namespace
            "user:tag:extra", // a second colon
            "User:tag",       // uppercase
            "user:tag name",  // whitespace
        ] {
            assert!(
                matches!(Attribute::parse(wrong), Err(Error::Attribute(_))),
                "{wrong}"
            );
        }
    }

    #[test]
    fn timestamps_are_one_shape_and_a_real_calendar() {
        assert!(Timestamp::parse("2026-09-01T21:14:03Z").is_ok());
        assert!(
            Timestamp::parse("2024-02-29T00:00:00Z").is_ok(),
            "a leap day"
        );
        assert!(
            Timestamp::parse("2026-06-30T23:59:60Z").is_ok(),
            "a leap second"
        );

        for wrong in [
            "2026-09-01T21:14:03",       // no Z
            "2026-09-01T21:14:03+02:00", // an offset is not Z
            "2026-09-01T21:14:03.5Z",    // fractional seconds
            "2026-09-01 21:14:03Z",      // a space is not T
            "2026-13-01T00:00:00Z",      // no thirteenth month
            "2026-02-29T00:00:00Z",      // not a leap year
            "1900-02-29T00:00:00Z",      // a century is not, either
            "2026-09-31T00:00:00Z",      // September ends a day earlier
            "2026-09-01T24:00:00Z",      // the day ends at 23:59
        ] {
            assert!(
                matches!(Timestamp::parse(wrong), Err(Error::Timestamp(_))),
                "{wrong}"
            );
        }

        assert!(
            Timestamp::parse("2000-02-29T00:00:00Z").is_ok(),
            "four centuries are"
        );
    }

    #[test]
    fn timestamps_order_as_strings_because_the_shape_never_varies() {
        let earlier = Timestamp::parse("2026-09-01T21:14:03Z").unwrap();
        let later = Timestamp::parse("2026-09-01T21:14:04Z").unwrap();
        assert!(earlier < later);
    }

    #[test]
    fn a_friendlier_spelling_closes_at_the_end_and_opens_at_the_start() {
        assert_eq!(
            Timestamp::closing("2026-09-01").unwrap().as_str(),
            "2026-09-01T23:59:59Z"
        );
        assert_eq!(
            Timestamp::opening("2026-09-01").unwrap().as_str(),
            "2026-09-01T00:00:00Z"
        );
        for whole in [
            "2026-09-01T08:00:00Z",
            "2026-09-01T08:00:00",
            " 2026-09-01T08:00:00 ",
        ] {
            assert_eq!(
                Timestamp::closing(whole).unwrap().as_str(),
                "2026-09-01T08:00:00Z",
                "{whole:?} names one moment, closing or opening"
            );
            assert_eq!(
                Timestamp::opening(whole).unwrap().as_str(),
                "2026-09-01T08:00:00Z"
            );
        }
        assert!(Timestamp::is_date("2026-09-01"));
        assert!(!Timestamp::is_date("2026-09-01T"));
        assert!(!Timestamp::is_date("2026-09-0a"));

        for wrong in ["2026-02-30", "2026-09", "yesterday", "2026-09-01T25:00:00"] {
            assert!(
                matches!(Timestamp::closing(wrong), Err(Error::Timestamp(given)) if given == wrong),
                "{wrong:?} names no moment, and the error names what was given"
            );
        }
    }

    #[test]
    fn unix_time_becomes_the_one_shape() {
        let stamp = |seconds| Timestamp::from_unix(seconds).unwrap();
        assert_eq!(stamp(0).as_str(), "1970-01-01T00:00:00Z");
        assert_eq!(stamp(-1).as_str(), "1969-12-31T23:59:59Z");
        assert_eq!(stamp(951_782_400).as_str(), "2000-02-29T00:00:00Z");
        assert_eq!(stamp(1_788_297_243).as_str(), "2026-09-01T21:14:03Z");
        assert!(
            matches!(Timestamp::from_unix(i64::MAX / 4), Err(Error::Timestamp(_))),
            "the year has four digits"
        );
        assert!(
            Timestamp::now() >= stamp(1_788_220_800),
            "the clock stands after 2026-09-01T00:00:00Z, when this test was written"
        );
    }

    #[test]
    fn sources_are_flat_and_never_empty() {
        assert!(Source::parse("ingest").is_ok());
        assert!(Source::parse("extractor:exif-rs/0.7").is_ok());
        for wrong in ["", "two words", "line\nbreak"] {
            assert!(
                matches!(Source::parse(wrong), Err(Error::Source(_))),
                "{wrong:?}"
            );
        }
    }

    #[test]
    fn another_programs_rfc3339_time_becomes_a_timestamp() {
        let read = |given| Timestamp::from_rfc3339(given).map(|t| t.as_str().to_string());
        assert_eq!(
            read("2024-03-01T02:00:03.512345+00:00").unwrap(),
            "2024-03-01T02:00:03Z",
            "Python's isoformat of a UTC time; the fraction is dropped"
        );
        assert_eq!(
            read("2024-03-01T02:00:03Z").unwrap(),
            "2024-03-01T02:00:03Z"
        );
        assert_eq!(
            read("2024-03-01T01:30:00-01:00").unwrap(),
            "2024-03-01T02:30:00Z"
        );
        assert_eq!(
            read("2024-03-01T00:30:00+01:00").unwrap(),
            "2024-02-29T23:30:00Z",
            "the offset crosses a leap day"
        );
        assert_eq!(
            read("1969-12-31T23:59:59+00:00").unwrap(),
            "1969-12-31T23:59:59Z"
        );
        for refused in [
            "2024-03-01T02:00:03",
            "2024-03-01T02:00:03.+00:00",
            "2024-03-01T02:00:03+0000",
            "2024-02-30T02:00:03+00:00",
            "2024-03-01T02:00:03+24:00",
            "yesterday",
        ] {
            assert!(read(refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn days_from_civil_undoes_civil_from_days() {
        for days in [-800_000, -1, 0, 1, 11_016, 19_783, 2_932_896] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(
                days_from_civil(year, month, day),
                days,
                "{year}-{month}-{day}"
            );
        }
    }
}
