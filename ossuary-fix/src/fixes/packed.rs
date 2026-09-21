//! `zip:entry` and `zip:path` said again as inner places.
//!
//! Until 0.7.0 the packed extractor spoke in a namespace named after
//! the one format it read: the inventory stood on the archive as
//! `zip:entry`, an unpacked entry's path inside it stood on the entry
//! as `zip:path`. A place inside another content is now spelled with
//! a leading `@` and stands as `packed:path` on the archive and as
//! `file:path` on the entry — the same value both ways, whatever the
//! archive's format. The fix restates each standing old value under
//! the new word, `@` in front, with the old claim's moment, source and
//! run, in a segment of its own, so the record reads as of any day as
//! if the new words had always been used.

use anyhow::{Result, anyhow};
use ossuary_core::{Attribute, Claim, Log, Run, Value};

use crate::plan::Plan;
use crate::record;

/// The old words and the new, on the archive and on the entry.
const ON_THE_ARCHIVE: (&str, &str) = ("zip:entry", "packed:path");
const ON_THE_ENTRY: (&str, &str) = ("zip:path", "file:path");

/// Every inner place standing under an old word and not, `@`-led,
/// under the new one, as a plan to say it again.
///
/// # Errors
///
/// Whatever reading the log can answer.
pub fn plan(log: &Log) -> Result<Plan> {
    let claims = record::whole(log)?;
    let mut plan = Plan {
        apart: true,
        unit: "inner place(s)",
        done: "said again with a leading @, as packed:path on the archive and file:path on the entry",
        nothing: "every zip:entry and zip:path on the record already stands in the new words",
        ..Plan::default()
    };
    let mut standing = 0;
    let mut without_run = 0;
    // One fresh run for every claim from before runs were written: the
    // old claim has none to carry over, and a claim needs one.
    let fresh = Run::new();
    for (old, new) in [ON_THE_ARCHIVE, ON_THE_ENTRY] {
        let old = Attribute::parse(old)?;
        let new = Attribute::parse(new)?;
        let olds = record::standing(&claims, &old);
        let news = record::standing_keys(&claims, &new);
        for ((subject, _), claim) in olds {
            let Some(Value::String(spelled)) = claim.value() else {
                return Err(anyhow!(
                    "a standing {old} on {subject} that is no string; the record is not what this fix expects"
                ));
            };
            let value = Value::String(inner(spelled));
            if news.contains(&(subject, value.to_string())) {
                standing += 1;
                continue;
            }
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
    }
    if without_run > 0 {
        plan.notes.push(format!(
            "{without_run} of them from before runs were written, under one fresh run"
        ));
    }
    if standing > 0 {
        plan.notes.push(format!("{standing} already stood"));
    }
    Ok(plan)
}

/// The inner-place spelling of an entry's path: a leading `@`, the
/// path verbatim after it.
fn inner(entry: &str) -> String {
    format!("@{entry}")
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

    fn claim(subject: &str, attribute: &str, value: &str, time: &str, retract: bool) -> Claim {
        let make = if retract {
            Claim::retract_value
        } else {
            Claim::assert
        };
        make(
            self::subject(subject),
            Attribute::parse(attribute).unwrap(),
            json!(value),
            Timestamp::parse(time).unwrap(),
            Source::parse("extractor:packed-list/1").unwrap(),
            Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn every_standing_old_inner_place_is_said_again_with_an_at() {
        let dir = TempDir::new().unwrap();
        let archive = Archive::create(dir.path(), Algorithm::Sha256).unwrap();
        let log = archive.log();
        // On the archive: three entries, one of them taken back, one
        // already standing in the new word too. On an entry: one path.
        for (subject, attribute, value, time, retract) in [
            (
                "a1",
                "zip:entry",
                "dir/a.txt",
                "2026-09-01T10:00:00Z",
                false,
            ),
            ("a1", "zip:entry", "gone.txt", "2026-09-01T10:00:00Z", false),
            ("a1", "zip:entry", "gone.txt", "2026-09-02T10:00:00Z", true),
            ("a1", "zip:entry", "kept.txt", "2026-09-01T10:00:00Z", false),
            (
                "a1",
                "packed:path",
                "@kept.txt",
                "2026-09-01T10:00:00Z",
                false,
            ),
            ("b2", "zip:path", "dir/a.txt", "2026-09-01T10:00:01Z", false),
        ] {
            log.append(&claim(subject, attribute, value, time, retract))
                .unwrap();
        }
        log.seal().unwrap();

        let planned = plan(log).unwrap();
        assert_eq!(
            planned
                .claims
                .iter()
                .map(|claim| (
                    claim.subject().as_str()[..2].to_string(),
                    claim.attribute().as_str().to_string(),
                    claim.value().cloned().unwrap(),
                    claim.time().as_str().to_string(),
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    "a1".to_string(),
                    "packed:path".to_string(),
                    json!("@dir/a.txt"),
                    "2026-09-01T10:00:00Z".to_string()
                ),
                (
                    "b2".to_string(),
                    "file:path".to_string(),
                    json!("@dir/a.txt"),
                    "2026-09-01T10:00:01Z".to_string()
                ),
            ],
            "the two waiting places, @-led, with their old moments"
        );
        assert_eq!(
            planned.claims[0].source().as_str(),
            "extractor:packed-list/1"
        );
        assert!(planned.apart);
        assert_eq!(planned.notes, vec!["1 already stood".to_string()]);

        planned.apply(log).unwrap();
        let mut index = Index::open(dir.path().join("cache").join("fix-test.sqlite")).unwrap();
        index.fold(log).unwrap();
        assert_eq!(
            index
                .values(
                    &subject("a1"),
                    &Attribute::parse("packed:path").unwrap(),
                    Scope::Held
                )
                .unwrap(),
            vec![json!("@dir/a.txt"), json!("@kept.txt")],
            "an entry taken back under the old word is not resurrected"
        );
        assert_eq!(
            index
                .values(
                    &subject("b2"),
                    &Attribute::parse("file:path").unwrap(),
                    Scope::Held
                )
                .unwrap(),
            vec![json!("@dir/a.txt")]
        );

        let again = plan(log).unwrap();
        assert!(
            again.claims.is_empty(),
            "a second run finds everything standing"
        );
        assert_eq!(
            again.sentence(false),
            "nothing to say: every zip:entry and zip:path on the record already stands in the new words; 3 already stood"
        );
    }
}
