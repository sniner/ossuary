//! Retract: taking a statement back.
//!
//! A retraction is a claim like any other: appended, sourced, timed,
//! editing nothing. It names the claims it retracts by id; those no longer
//! stand, the record keeps the story, and a later assertion may put the
//! value back.

use crate::claim::{Attribute, Claim, ClaimId, Run, Source, Subject, Timestamp, Written};
use crate::error::Result;
use crate::log::Log;

/// Take statements back: one retraction claim per taking, all under the
/// source `user` — the human takes back, this function is only the pen.
/// One moment for the whole call.
///
/// A taking is a subject, an attribute and the ids of the claims to
/// retract, as [`Index::standing_ids`](crate::Index::standing_ids) answers
/// them: every assertion of one value, or every assertion of the
/// attribute. Looking them up is the caller's business, done *before*
/// anything is written. A taking with no ids writes nothing.
///
/// Answers what was written, the call's run included.
///
/// # Errors
///
/// Whatever readying the log and appending to it can answer.
pub fn retract(log: &Log, takings: &[(Subject, Attribute, Vec<ClaimId>)]) -> Result<Written> {
    // The CLI asks first and says when a seal was finished; here it only
    // has to be done.
    let _ = log.writable()?;
    let word = Source::parse("user")?;
    let time = Timestamp::now();
    let run = Run::new();
    let mut claims = Vec::new();
    for (subject, attribute, ids) in takings {
        if ids.is_empty() {
            continue;
        }
        claims.push(Claim::retract(
            subject.clone(),
            attribute.clone(),
            ids.clone(),
            time.clone(),
            word.clone(),
            run.clone(),
        )?);
    }
    log.append_all(&claims)?;
    Ok(Written {
        claims: claims.len(),
        run,
    })
}

#[cfg(test)]
mod tests {
    use immure::Store;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    fn log_in(dir: &TempDir) -> Log {
        let store = Store::builder(dir.path().join("claims"))
            .suffix(".seg")
            .depth(1)
            .create()
            .unwrap();
        Log::new(store, dir.path().join("head.jsonl"))
    }

    fn subject() -> Subject {
        Subject::parse("9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e").unwrap()
    }

    #[test]
    fn a_retraction_takes_the_standing_and_leaves_the_story() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let tag = Attribute::parse("user:tag").unwrap();
        let comment = Attribute::parse("user:comment").unwrap();
        crate::annotate(
            &log,
            &[subject()],
            &["first".to_string(), "second".to_string()],
            &["beach".to_string()],
        )
        .unwrap();

        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let mut index = crate::Index::open(cache.join("index.sqlite")).unwrap();
        index.fold(&log).unwrap();
        let beach = index
            .standing_ids(&subject(), &tag, Some(&json!("beach")))
            .unwrap();
        let comments = index.standing_ids(&subject(), &comment, None).unwrap();
        assert_eq!((beach.len(), comments.len()), (1, 2));

        let written = retract(
            &log,
            &[
                (subject(), tag.clone(), beach),
                (subject(), comment.clone(), comments),
                (subject(), tag.clone(), Vec::new()),
            ],
        )
        .unwrap();
        assert_eq!(
            written.claims, 2,
            "one claim per taking, an empty taking writes none"
        );
        index.fold(&log).unwrap();
        assert_eq!(
            index.values(&subject(), &tag, crate::Scope::Held).unwrap(),
            Vec::<crate::Value>::new(),
            "the tag no longer stands"
        );
        assert_eq!(
            index
                .values(&subject(), &comment, crate::Scope::Held)
                .unwrap(),
            Vec::<crate::Value>::new(),
            "All emptied both comments with one claim"
        );
        assert_eq!(
            index.about(&subject()).unwrap().len(),
            5,
            "three words and two retractions — the story keeps them all"
        );
    }
}
