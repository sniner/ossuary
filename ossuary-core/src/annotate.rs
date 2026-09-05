//! Annotate: the user's own word onto what already stands.
//!
//! Where ingest records what a walk observed and extractors record what
//! bytes say about themselves, this records what the human says — and
//! the archive takes their word. An annotation is a claim like any
//! other: appended, sourced, timed, never editing anything standing. A
//! second comment stands beside the first, a tag said twice lands on
//! the set element already there.

use serde_json::json;

use crate::claim::{Attribute, Claim, Source, Subject, Timestamp};
use crate::error::Result;
use crate::log::Log;

/// The user's word onto the record: every comment as a `user:comment`
/// claim and every tag as a `user:tag` claim, on every named subject,
/// all under the source `user` — the human asserts, this function is
/// only the pen. One moment for the whole call: everything said
/// together carries the same time.
///
/// Answers how many claims were written. Subjects are taken as given —
/// resolving a spelling to a subject is the caller's business, done
/// *before* anything is written.
///
/// # Errors
///
/// Whatever appending to the log can answer; the claim grammar itself
/// cannot refuse these claims.
pub fn annotate(
    log: &Log,
    subjects: &[Subject],
    comments: &[String],
    tags: &[String],
) -> Result<usize> {
    let word = Source::parse("user")?;
    let time = Timestamp::now();
    let comment = Attribute::parse("user:comment")?;
    let tag = Attribute::parse("user:tag")?;
    let mut written = 0usize;
    for subject in subjects {
        for text in comments {
            log.append(&Claim::assert(
                subject.clone(),
                comment.clone(),
                json!(text),
                time.clone(),
                word.clone(),
            )?)?;
            written += 1;
        }
        for label in tags {
            log.append(&Claim::assert(
                subject.clone(),
                tag.clone(),
                json!(label),
                time.clone(),
                word.clone(),
            )?)?;
            written += 1;
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use immure::Store;
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

    fn subject(fill: char) -> Subject {
        Subject::parse(&format!("sha256:{}", String::from(fill).repeat(64))).unwrap()
    }

    #[test]
    fn every_word_lands_on_every_subject_under_the_users_name() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let subjects = [subject('a'), subject('b')];
        let comments = ["the one from the cellar".to_string()];
        let tags = ["holiday".to_string(), "2026".to_string()];

        let written = annotate(&log, &subjects, &comments, &tags).unwrap();

        assert_eq!(written, 6, "three words on two files");
        let head = log.head().unwrap();
        assert_eq!(head.len(), 6);
        assert!(
            head.iter().all(|claim| claim.source().as_str() == "user"),
            "the human asserts, the pen stays out of the record"
        );
        assert_eq!(
            head.iter()
                .filter(|claim| claim.attribute().as_str() == "user:comment")
                .count(),
            2
        );
        assert_eq!(
            head.iter()
                .filter(|claim| claim.attribute().as_str() == "user:tag"
                    && claim.subject() == &subjects[1])
                .count(),
            2
        );
    }

    #[test]
    fn nothing_to_say_writes_nothing() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);

        assert_eq!(annotate(&log, &[subject('a')], &[], &[]).unwrap(), 0);
        assert_eq!(log.head().unwrap().len(), 0);
    }
}
