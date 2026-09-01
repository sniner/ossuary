//! The claim: one fact, one line.
//!
//! All metadata in an ossuary archive is claims — small, self-describing,
//! append-only facts, one JSON object per line. The shape is fixed by
//! generation 1 of the format (`docs/format.md`): six fields and no seventh,
//! `subject`, `attribute`, `value`, `time`, `source` and `retract`. Nothing
//! is ever updated or deleted in place; a correction is a newer claim, a
//! deletion is a retraction, and the log only grows.
//!
//! Every field with rules of its own is a type of its own, and parsing
//! validates: a [`Subject`], [`Attribute`], [`Timestamp`] or [`Source`] in
//! hand is always well-formed, and a [`Claim`] read back from a line is one
//! this crate could have written.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A claim's value: any JSON value — with `null` refused at every door.
///
/// Re-exported from `serde_json` so that callers build values with the tools
/// they already have, `serde_json::json!` included.
pub use serde_json::Value;

/// The hash algorithms a subject may name, with the length of their digests
/// in hex characters. The list mirrors immure's; the prefix in the subject is
/// what lets digests of several algorithms stand in one log.
const ALGORITHMS: [(&str, usize); 4] = [
    ("sha256", 64),
    ("sha384", 96),
    ("sha512", 128),
    ("blake3", 64),
];

/// What a claim is about: `<algorithm>:<hex>`, a blob named by its content
/// with the algorithm that made the name spelled out in front.
///
/// The prefix is what survives a hash migration — subjects of several
/// algorithms stand in one log without a rule outside the line — and it
/// reserves the namespace: prefixes that are not hash algorithms are held
/// back for subjects that are not blobs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Subject(String);

impl Subject {
    /// Validate `<algorithm>:<hex>`.
    ///
    /// The digest must be full length for its algorithm — a subject names one
    /// blob, not the beginning of one. Uppercase hex is normalised to
    /// lowercase, the way blobs are named on disk.
    ///
    /// # Errors
    ///
    /// [`Error::Subject`] for anything else.
    pub fn parse(s: &str) -> Result<Self> {
        let error = || Error::Subject(s.to_string());
        let (algorithm, hex) = s.split_once(':').ok_or_else(error)?;
        let (_, len) = ALGORITHMS
            .iter()
            .find(|(name, _)| *name == algorithm)
            .ok_or_else(error)?;
        if hex.len() != *len || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(error());
        }
        Ok(Subject(format!("{algorithm}:{}", hex.to_ascii_lowercase())))
    }

    /// The whole subject, prefix included.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The algorithm prefix, without the colon.
    #[must_use]
    pub fn algorithm(&self) -> &str {
        self.0
            .split_once(':')
            .map_or("", |(algorithm, _)| algorithm)
    }

    /// The digest as lowercase hex, without the prefix.
    #[must_use]
    pub fn hex(&self) -> &str {
        self.0.split_once(':').map_or("", |(_, hex)| hex)
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

    /// The timestamp, exactly as it stands in the line.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
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

/// One fact: an attribute of a subject has a value — who says so, and when.
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
    ) -> Self {
        Claim {
            subject,
            attribute,
            value: None,
            time,
            source,
            retract: true,
        }
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
/// that would add a seventh field is a new generation.
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn subject() -> Subject {
        Subject::parse("sha256:9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e")
            .unwrap()
    }

    fn time() -> Timestamp {
        Timestamp::parse("2026-09-01T21:14:03Z").unwrap()
    }

    #[test]
    fn an_assertion_serialises_in_the_order_of_the_format_document() {
        let claim = Claim::assert(
            subject(),
            Attribute::parse("file:size").unwrap(),
            json!(4_194_304),
            time(),
            Source::parse("ingest").unwrap(),
        )
        .unwrap();

        assert_eq!(
            claim.to_line(),
            r#"{"subject":"sha256:9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"file:size","value":4194304,"time":"2026-09-01T21:14:03Z","source":"ingest"}"#,
            "a number is a number, and an assertion carries no retract key"
        );
    }

    #[test]
    fn the_example_lines_of_the_format_document_round_trip() {
        // The examples from docs/format.md, with the digest at full length.
        let lines = [
            r#"{"subject":"sha256:9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"prov:ingest-path","value":"/photos/2019/crete/beach.jpg","time":"2026-09-01T21:14:03Z","source":"ingest"}"#,
            r#"{"subject":"sha256:9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"file:size","value":4194304,"time":"2026-09-01T21:14:03Z","source":"ingest"}"#,
            r#"{"subject":"sha256:9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"exif:date-time-original","value":"2019-07-14T11:02:41","time":"2026-09-22T08:30:00Z","source":"extractor:exif-rs/0.7"}"#,
            r#"{"subject":"sha256:9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"user:tag","value":"holiday","time":"2026-10-05T19:00:00Z","source":"user"}"#,
            r#"{"subject":"sha256:9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"user:tag","value":"holiday","time":"2030-04-01T10:00:00Z","source":"user","retract":true}"#,
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
            ),
            Err(Error::NullValue)
        ));

        let line = r#"{"subject":"sha256:9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"user:tag","value":null,"time":"2026-09-01T21:14:03Z","source":"user"}"#;
        assert!(matches!(Claim::parse_line(line), Err(Error::NullValue)));

        // `"value": null` on a retraction is not the same line as no value
        // key: it must be refused, not read as a whole-attribute retraction.
        let line = r#"{"subject":"sha256:9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"user:tag","value":null,"time":"2026-09-01T21:14:03Z","source":"user","retract":true}"#;
        assert!(matches!(Claim::parse_line(line), Err(Error::NullValue)));
    }

    #[test]
    fn an_assertion_without_a_value_says_nothing() {
        let line = r#"{"subject":"sha256:9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"user:tag","time":"2026-09-01T21:14:03Z","source":"user"}"#;
        assert!(matches!(Claim::parse_line(line), Err(Error::ValueRequired)));
    }

    #[test]
    fn the_field_set_is_closed() {
        let line = r#"{"subject":"sha256:9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"user:tag","value":"holiday","time":"2026-09-01T21:14:03Z","source":"user","confidence":0.9}"#;
        assert!(
            matches!(Claim::parse_line(line), Err(Error::Line(_))),
            "a seventh field is a new generation, not an extension point"
        );
    }

    #[test]
    fn subjects_carry_their_algorithm_and_normalise_their_hex() {
        let subject = Subject::parse(
            "sha256:9F2AC41E9F2AC41E9F2AC41E9F2AC41E9F2AC41E9F2AC41E9F2AC41E9F2AC41E",
        )
        .unwrap();
        assert_eq!(subject.algorithm(), "sha256");
        assert_eq!(
            subject.hex(),
            "9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e"
        );

        for wrong in [
            "9f2ac41e",                                                                  // no prefix
            "md5:9f2ac41e",    // not an algorithm of the family
            "sha256:9f2ac41e", // not full length
            "sha256:9z2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e", // not hex
            "blake3:9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e00", // too long
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
}
