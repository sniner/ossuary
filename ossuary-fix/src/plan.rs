//! What a fix would append, and how it is appended and told.

use anyhow::Result;
use ossuary_core::{Claim, Log, Writable};

/// A fix's answer: the claims to append, and the words for saying what
/// was found.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    /// The claims the fix appends, in this order. None means nothing to do.
    pub claims: Vec<Claim>,
    /// What the claims are, as the sentence counts them: `origin(s)`.
    pub unit: &'static str,
    /// What is done to them, past participle first, so the sentence can
    /// say both `3 origin(s) copied to prov:origin` and `would be copied
    /// to prov:origin`.
    pub done: &'static str,
    /// What to say when there is nothing to append.
    pub nothing: &'static str,
    /// Findings beside the count, each a short clause: `3 already recorded`.
    pub notes: Vec<String>,
}

impl Plan {
    /// Append the plan's claims to the log, like any writer's: the log
    /// readied first, and what readying it did is the answer.
    ///
    /// # Errors
    ///
    /// Whatever readying the log and appending can answer.
    pub fn apply(&self, log: &Log) -> Result<Writable> {
        let writable = log.writable()?;
        for claim in &self.claims {
            log.append(claim)?;
        }
        Ok(writable)
    }

    /// The one line the fix answers with.
    pub fn sentence(&self, dry_run: bool) -> String {
        let count = self.claims.len();
        let mut sentence = if count == 0 {
            format!("nothing to do: {}", self.nothing)
        } else if dry_run {
            format!("{count} {} would be {}", self.unit, self.done)
        } else {
            format!("{count} {} {}", self.unit, self.done)
        };
        for note in &self.notes {
            sentence.push_str("; ");
            sentence.push_str(note);
        }
        sentence
    }
}

#[cfg(test)]
mod tests {
    use ossuary_core::{Algorithm, Archive, Attribute, Run, Source, Subject, Timestamp};
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    fn claim(time: &str) -> Claim {
        Claim::assert(
            Subject::parse(&"a".repeat(64)).unwrap(),
            Attribute::parse("user:tag").unwrap(),
            json!("x"),
            Timestamp::parse(time).unwrap(),
            Source::parse("user").unwrap(),
            Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        )
        .unwrap()
    }

    fn plan(claims: Vec<Claim>) -> Plan {
        Plan {
            claims,
            unit: "tag(s)",
            done: "copied",
            nothing: "every tag is recorded",
            notes: vec!["1 already recorded".to_string()],
        }
    }

    #[test]
    fn the_claims_join_the_head() {
        let dir = TempDir::new().unwrap();
        let archive = Archive::create(dir.path(), Algorithm::Sha256).unwrap();
        let log = archive.log();
        log.append(&claim("2026-09-20T10:00:00Z")).unwrap();

        assert_eq!(
            plan(vec![claim("2026-09-01T10:00:00Z")])
                .apply(log)
                .unwrap(),
            Writable::Ready
        );

        assert!(log.segments().unwrap().is_empty(), "nothing sealed");
        assert_eq!(
            log.head().unwrap().len(),
            2,
            "the restated claim follows in the log, whatever its time"
        );
    }

    #[test]
    fn the_sentence_counts_and_notes() {
        let one = plan(vec![claim("2026-09-01T10:00:00Z")]);
        assert_eq!(
            one.sentence(true),
            "1 tag(s) would be copied; 1 already recorded"
        );
        assert_eq!(one.sentence(false), "1 tag(s) copied; 1 already recorded");
        assert_eq!(
            plan(Vec::new()).sentence(false),
            "nothing to do: every tag is recorded; 1 already recorded"
        );
    }
}
