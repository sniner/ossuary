//! Reading the record the way a fix needs it: whole, and folded down to
//! one attribute's standing set.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use ossuary_core::{Attribute, Claim, Log};

/// The log whole, in the order the fold reads it: every sealed segment
/// in the log's own order, then the open head.
///
/// # Errors
///
/// Whatever reading a segment or the head can answer.
pub fn whole(log: &Log) -> Result<Vec<Claim>> {
    let mut claims = Vec::new();
    for segment in log.segments()? {
        claims.extend(log.read(segment.digest())?);
    }
    claims.extend(log.head()?);
    Ok(claims)
}

/// A subject and a value, the value in its JSON spelling: what one
/// element of a standing set is named by.
pub type Key = (String, String);

/// One attribute's standing set over the claims given, each element
/// with the claim that put it there: an assertion joins under its
/// subject and value, a retraction of a value takes that one out, a
/// retraction of the attribute takes every value of the subject out.
pub fn standing<'a>(
    claims: impl IntoIterator<Item = &'a Claim>,
    attribute: &Attribute,
) -> BTreeMap<Key, &'a Claim> {
    let mut set: BTreeMap<Key, &'a Claim> = BTreeMap::new();
    for claim in claims {
        if claim.attribute() != attribute {
            continue;
        }
        replay(&mut set, claim, || claim);
    }
    set
}

/// The keys alone of one attribute's standing set.
pub fn standing_keys<'a>(
    claims: impl IntoIterator<Item = &'a Claim>,
    attribute: &Attribute,
) -> BTreeSet<Key> {
    let mut set: BTreeSet<Key> = BTreeSet::new();
    for claim in claims {
        if claim.attribute() != attribute {
            continue;
        }
        replay(&mut set, claim, || ());
    }
    set
}

fn replay<T>(set: &mut impl Set<T>, claim: &Claim, kept: impl FnOnce() -> T) {
    let subject = claim.subject().as_str().to_string();
    match (claim.is_retraction(), claim.value()) {
        (false, Some(value)) => set.put((subject, value.to_string()), kept()),
        (true, Some(value)) => set.take(&(subject, value.to_string())),
        (true, None) => set.take_all(&subject),
        (false, None) => {}
    }
}

/// A set or a map, replayed the same way.
trait Set<T> {
    fn put(&mut self, key: Key, kept: T);
    fn take(&mut self, key: &Key);
    fn take_all(&mut self, subject: &str);
}

impl<T> Set<T> for BTreeMap<Key, T> {
    fn put(&mut self, key: Key, kept: T) {
        self.insert(key, kept);
    }
    fn take(&mut self, key: &Key) {
        self.remove(key);
    }
    fn take_all(&mut self, subject: &str) {
        self.retain(|(known, _), _| known != subject);
    }
}

impl Set<()> for BTreeSet<Key> {
    fn put(&mut self, key: Key, (): ()) {
        self.insert(key);
    }
    fn take(&mut self, key: &Key) {
        self.remove(key);
    }
    fn take_all(&mut self, subject: &str) {
        self.retain(|(known, _)| known != subject);
    }
}

#[cfg(test)]
mod tests {
    use ossuary_core::{Run, Source, Subject, Timestamp};
    use serde_json::json;

    use super::*;

    fn subject(first: &str) -> Subject {
        Subject::parse(&format!("{first}{}", "0".repeat(64 - first.len()))).unwrap()
    }

    fn tag(subject: &str, value: Option<&str>, retract: bool) -> Claim {
        let attribute = Attribute::parse("user:tag").unwrap();
        let time = Timestamp::parse("2026-09-01T10:00:00Z").unwrap();
        let source = Source::parse("user").unwrap();
        let run = Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap();
        match (retract, value) {
            (false, Some(value)) => Claim::assert(
                self::subject(subject),
                attribute,
                json!(value),
                time,
                source,
                run,
            )
            .unwrap(),
            (true, Some(value)) => Claim::retract_value(
                self::subject(subject),
                attribute,
                json!(value),
                time,
                source,
                run,
            )
            .unwrap(),
            (true, None) => {
                Claim::retract_attribute(self::subject(subject), attribute, time, source, run)
            }
            (false, None) => unreachable!(),
        }
    }

    #[test]
    fn the_standing_set_applies_both_kinds_of_retraction() {
        let claims = vec![
            tag("a1", Some("x"), false),
            tag("a1", Some("y"), false),
            tag("a1", Some("x"), true),
            tag("b2", Some("x"), false),
            tag("b2", Some("y"), false),
            tag("b2", None, true),
            tag("b2", Some("z"), false),
            tag("c3", Some("x"), false),
        ];
        let attribute = Attribute::parse("user:tag").unwrap();
        let keys: Vec<Key> = standing_keys(&claims, &attribute).into_iter().collect();
        assert_eq!(
            keys,
            vec![
                (subject("a1").as_str().to_string(), "\"y\"".to_string()),
                (subject("b2").as_str().to_string(), "\"z\"".to_string()),
                (subject("c3").as_str().to_string(), "\"x\"".to_string()),
            ],
            "one value taken back, a whole attribute taken back and said again after"
        );
        let with_claims = standing(&claims, &attribute);
        assert_eq!(with_claims.len(), 3);
        assert!(
            standing(&claims, &Attribute::parse("user:note").unwrap()).is_empty(),
            "another attribute's claims are not this set's"
        );
    }
}
