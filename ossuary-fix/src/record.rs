//! Reading the record the way a fix needs it: whole, and folded down to
//! one attribute's standing set.

use std::collections::{BTreeMap, BTreeSet, HashSet};

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
/// with the newest claim that put it there: an assertion joins under its
/// subject and value unless a retraction of that subject and attribute,
/// anywhere in the claims, names it.
/// The claims must carry their ids, as those read from the log do; one
/// without an id cannot be named and always stands.
pub fn standing<'a>(
    claims: impl IntoIterator<Item = &'a Claim> + Clone,
    attribute: &Attribute,
) -> BTreeMap<Key, &'a Claim> {
    // Each retracted id with the subject of its retraction: a
    // retraction retracts only claims of its own subject and attribute.
    let retracted: HashSet<(&str, &str)> = claims
        .clone()
        .into_iter()
        .filter(|claim| claim.attribute() == attribute)
        .flat_map(|claim| {
            claim
                .retracts()
                .iter()
                .map(|id| (claim.subject().as_str(), id.as_str()))
        })
        .collect();
    let mut set: BTreeMap<Key, &'a Claim> = BTreeMap::new();
    for claim in claims {
        if claim.attribute() != attribute {
            continue;
        }
        let Some(value) = claim.value() else {
            continue;
        };
        if claim
            .id()
            .is_some_and(|id| retracted.contains(&(claim.subject().as_str(), id.as_str())))
        {
            continue;
        }
        set.insert(
            (claim.subject().as_str().to_string(), value.to_string()),
            claim,
        );
    }
    set
}

/// The keys alone of one attribute's standing set.
pub fn standing_keys<'a>(
    claims: impl IntoIterator<Item = &'a Claim> + Clone,
    attribute: &Attribute,
) -> BTreeSet<Key> {
    standing(claims, attribute).into_keys().collect()
}

#[cfg(test)]
mod tests {
    use ossuary_core::{Run, Source, Subject, Timestamp};
    use serde_json::json;

    use super::*;

    fn subject(first: &str) -> Subject {
        Subject::parse(&format!("{first}{}", "0".repeat(64 - first.len()))).unwrap()
    }

    /// An assertion of `value` on `subject`, with its id as the log would
    /// give it; `retract` names the claims to retract instead.
    fn tag(subject: &str, value: Option<&str>, retract: &[&Claim]) -> Claim {
        let attribute = Attribute::parse("user:tag").unwrap();
        let time = Timestamp::parse("2026-09-01T10:00:00Z").unwrap();
        let source = Source::parse("user").unwrap();
        let run = Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap();
        let claim = match value {
            Some(value) => Claim::assert(
                self::subject(subject),
                attribute,
                json!(value),
                time,
                source,
                run,
            )
            .unwrap(),
            None => Claim::retract(
                self::subject(subject),
                attribute,
                retract
                    .iter()
                    .map(|claim| claim.id().unwrap().clone())
                    .collect(),
                time,
                source,
                run,
            )
            .unwrap(),
        };
        Claim::read_line(&claim.to_line(), ossuary_core::Algorithm::Sha256).unwrap()
    }

    #[test]
    fn a_retraction_of_another_subject_retracts_nothing() {
        let a1x = tag("a1", Some("x"), &[]);
        let claims = vec![a1x.clone(), tag("b2", None, &[&a1x])];
        let attribute = Attribute::parse("user:tag").unwrap();

        let set = standing(&claims, &attribute);

        assert_eq!(set.len(), 1, "the claim of a1 stands");
    }

    #[test]
    fn the_standing_set_applies_retractions_by_id() {
        let a1x = tag("a1", Some("x"), &[]);
        let a1y = tag("a1", Some("y"), &[]);
        let b2x = tag("b2", Some("x"), &[]);
        let b2y = tag("b2", Some("y"), &[]);
        let claims = vec![
            // A value taken back, and the subject's other value standing.
            tag("a1", None, &[&a1x]),
            a1x.clone(),
            a1y.clone(),
            // Everything the subject had taken back, and a value said after.
            b2x.clone(),
            b2y.clone(),
            tag("b2", None, &[&b2x, &b2y]),
            tag("b2", Some("z"), &[]),
            tag("c3", Some("x"), &[]),
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
            "retractions apply wherever they stand, before their target too"
        );
        let with_claims = standing(&claims, &attribute);
        assert_eq!(with_claims.len(), 3);
        assert!(
            standing(&claims, &Attribute::parse("user:note").unwrap()).is_empty(),
            "another attribute's claims are not this set's"
        );
    }
}
