//! `derive:derived-from` said again as `prov:origin`.
//!
//! Until 0.6.3 a derived file's origin was recorded under the old word.
//! The present is asked in the new one, so a derived file whose origin
//! stands only under the old word is held but not placed. The fix
//! restates each such origin with the old claim's moment, source and
//! run, in a segment of its own, so the record reads as of any day as
//! if the new word had always been used.

use anyhow::{Result, anyhow};
use ossuary_core::{Attribute, Claim, Log, Run};

use crate::plan::Plan;
use crate::record;

/// The old word, and the new one.
const OLD: &str = "derive:derived-from";
const NEW: &str = "prov:origin";

/// Every origin standing under the old word and not under the new one,
/// as a plan to say it again under the new one.
///
/// # Errors
///
/// Whatever reading the log can answer.
pub fn plan(log: &Log) -> Result<Plan> {
    let old = Attribute::parse(OLD)?;
    let new = Attribute::parse(NEW)?;
    let claims = record::whole(log)?;
    let olds = record::standing(&claims, &old);
    let news = record::standing_keys(&claims, &new);
    let mut plan = Plan {
        apart: true,
        unit: "origin(s)",
        done: "copied to prov:origin",
        nothing: "every origin is already recorded as prov:origin",
        ..Plan::default()
    };
    let mut standing = 0;
    let mut without_run = 0;
    // One fresh run for every claim from before runs were written: the
    // old claim has none to carry over, and a claim needs one.
    let fresh = Run::new();
    for (key, claim) in olds {
        if news.contains(&key) {
            standing += 1;
            continue;
        }
        let value = claim.value().cloned().ok_or_else(|| {
            anyhow!("internal error: a standing origin without a value; nothing was written")
        })?;
        let run = claim.run().cloned().unwrap_or_else(|| {
            without_run += 1;
            fresh.clone()
        });
        plan.claims.push(Claim::assert(
            claim.subject().clone(),
            new.clone(),
            value,
            claim.time().clone(),
            claim.source().clone(),
            run,
        )?);
    }
    if without_run > 0 {
        plan.notes.push(format!(
            "{without_run} of them have no run id and are given a single new run id"
        ));
    }
    if standing > 0 {
        plan.notes.push(format!("{standing} already recorded"));
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use ossuary_core::{Algorithm, Archive, Index, Scope, Source, Subject, Timestamp};
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    fn subject(first: &str) -> Subject {
        Subject::parse(&format!("{first}{}", "0".repeat(64 - first.len()))).unwrap()
    }

    fn claim(subject: &str, attribute: &str, origin: &str, time: &str, retract: bool) -> Claim {
        let make = if retract {
            Claim::retract_value
        } else {
            Claim::assert
        };
        make(
            self::subject(subject),
            Attribute::parse(attribute).unwrap(),
            json!(self::subject(origin).as_str()),
            Timestamp::parse(time).unwrap(),
            Source::parse("extractor:mail/1").unwrap(),
            Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn every_standing_old_origin_is_said_again_under_the_new_word() {
        let dir = TempDir::new().unwrap();
        let archive = Archive::create(dir.path(), Algorithm::Sha256).unwrap();
        let log = archive.log();
        // Four origins under the old word: one taken back, one already
        // standing under the new word too, two waiting.
        log.append(&claim("a1", OLD, "0e", "2026-09-01T10:00:00Z", false))
            .unwrap();
        log.append(&claim("b2", OLD, "0e", "2026-09-01T10:00:01Z", false))
            .unwrap();
        log.append(&claim("c3", OLD, "0e", "2026-09-01T10:00:02Z", false))
            .unwrap();
        log.append(&claim("c3", OLD, "0e", "2026-09-02T10:00:00Z", true))
            .unwrap();
        log.append(&claim("d4", OLD, "0f", "2026-09-03T10:00:00Z", false))
            .unwrap();
        log.append(&claim("d4", NEW, "0f", "2026-09-03T10:00:00Z", false))
            .unwrap();
        log.seal().unwrap();
        log.append(&claim("e5", NEW, "0f", "2026-09-20T10:00:00Z", false))
            .unwrap();

        let planned = plan(log).unwrap();
        assert_eq!(
            planned
                .claims
                .iter()
                .map(|claim| (
                    claim.subject().as_str()[..2].to_string(),
                    claim.attribute().as_str().to_string(),
                    claim.time().as_str().to_string(),
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    "a1".to_string(),
                    NEW.to_string(),
                    "2026-09-01T10:00:00Z".to_string()
                ),
                (
                    "b2".to_string(),
                    NEW.to_string(),
                    "2026-09-01T10:00:01Z".to_string()
                ),
            ],
            "the two waiting origins, with their old moments"
        );
        assert_eq!(planned.claims[0].source().as_str(), "extractor:mail/1");
        assert_eq!(
            planned.claims[0].run().unwrap().as_str(),
            "315e360b-020e-48be-8f2d-f2002a2ea9b4"
        );
        assert!(planned.apart);
        assert_eq!(planned.notes, vec!["1 already recorded".to_string()]);
        assert_eq!(
            planned.sentence(false),
            "2 origin(s) copied to prov:origin, in a separate segment; 1 already recorded"
        );

        planned.apply(log).unwrap();
        let mut index = Index::open(dir.path().join("cache").join("fix-test.sqlite")).unwrap();
        index.fold(log).unwrap();
        let new = Attribute::parse(NEW).unwrap();
        assert_eq!(
            index.values(&subject("a1"), &new, Scope::Held).unwrap(),
            vec![json!(subject("0e").as_str())]
        );
        assert!(
            index
                .values(&subject("c3"), &new, Scope::Held)
                .unwrap()
                .is_empty(),
            "an origin taken back under the old word is not resurrected"
        );
        assert_eq!(
            index.derived(&subject("0e"), Scope::Held).unwrap(),
            vec![subject("a1"), subject("b2")]
        );

        let again = plan(log).unwrap();
        assert!(
            again.claims.is_empty(),
            "a second run finds everything standing"
        );
        assert_eq!(
            again.sentence(false),
            "nothing to do: every origin is already recorded as prov:origin; 3 already recorded"
        );
    }
}
