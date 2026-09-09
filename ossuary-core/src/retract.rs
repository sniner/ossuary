//! Retract: taking a statement back.
//!
//! A retraction is a claim like any other: appended, sourced, timed,
//! editing nothing. What it names no longer stands; the record keeps
//! the story, and a later assertion may honestly put the value back —
//! a retraction is an event, not a curse.

use crate::claim::{Attribute, Claim, Source, Subject, Timestamp, Value};
use crate::error::Result;
use crate::log::Log;

/// What one retraction takes back of an attribute.
#[derive(Debug, Clone)]
pub enum Taking {
    /// One value, named exactly.
    Value(Value),
    /// Every value the attribute holds.
    All,
}

/// Take statements back: one retraction claim per taking, all under the
/// source `user` — the human takes back, this function is only the pen.
/// One moment for the whole call.
///
/// Answers how many retractions were written. Takings are taken as
/// given — resolving spellings to subjects, and checking that what is
/// named actually stands, is the caller's business, done *before*
/// anything is written.
///
/// # Errors
///
/// Whatever appending to the log can answer, and [`crate::Error::NullValue`]
/// for a `null` taking, which the claim grammar refuses.
pub fn retract(log: &Log, takings: &[(Subject, Attribute, Taking)]) -> Result<usize> {
    let word = Source::parse("user")?;
    let time = Timestamp::now();
    let mut written = 0usize;
    for (subject, attribute, taking) in takings {
        let claim = match taking {
            Taking::Value(value) => Claim::retract_value(
                subject.clone(),
                attribute.clone(),
                value.clone(),
                time.clone(),
                word.clone(),
            )?,
            Taking::All => Claim::retract_attribute(
                subject.clone(),
                attribute.clone(),
                time.clone(),
                word.clone(),
            ),
        };
        log.append(&claim)?;
        written += 1;
    }
    Ok(written)
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

        let written = retract(
            &log,
            &[
                (subject(), tag.clone(), Taking::Value(json!("beach"))),
                (subject(), comment.clone(), Taking::All),
            ],
        )
        .unwrap();
        assert_eq!(written, 2, "one claim per taking, All is one claim");

        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let mut index = crate::Index::open(cache.join("index.sqlite")).unwrap();
        index.fold(&log).unwrap();
        assert_eq!(
            index.values(&subject(), &tag).unwrap(),
            Vec::<Value>::new(),
            "the tag no longer stands"
        );
        assert_eq!(
            index.values(&subject(), &comment).unwrap(),
            Vec::<Value>::new(),
            "All emptied both comments with one claim"
        );
        assert_eq!(
            index.about(&subject()).unwrap().len(),
            5,
            "three words and two retractions — the story keeps them all"
        );
    }
}
