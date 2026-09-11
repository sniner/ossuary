//! What one run came to.

use crate::output::counted;

/// The count of a run — a fetch or a takeover, the verdict reads the
/// same.
#[derive(Debug)]
pub struct Tally {
    /// What the run does to a message, for the dry run's verdict:
    /// "fetch" or "take over".
    act: &'static str,
    /// The run's id, as every `prov:run` claim of it says.
    pub run: String,
    /// Messages whose bytes were new to the archive.
    pub stored: usize,
    /// Messages the archive already held — their place went on the
    /// record all the same.
    pub known: usize,
    /// Messages a takeover found already in with every place: not
    /// read, nothing recorded.
    pub left: usize,
    /// Claims appended to the log.
    pub claims: usize,
    /// Under `--dry-run`: messages the run would have fetched.
    pub would: usize,
    /// What went wrong, named — an account that would not answer, a
    /// folder that would not open, a message whose bytes are damaged.
    /// The rest of the run went on.
    pub failed: Vec<String>,
}

impl Tally {
    pub fn new(act: &'static str) -> Self {
        Self {
            act,
            run: ossuary_core::run_id(),
            stored: 0,
            known: 0,
            left: 0,
            claims: 0,
            would: 0,
            failed: Vec::new(),
        }
    }

    /// The verdict, one line: which clean outcome it was, and what went
    /// on the record.
    #[must_use]
    pub fn verdict(&self, dry_run: bool) -> String {
        if dry_run {
            let left = if self.left > 0 {
                format!(", {} on the record before and left in peace", self.left)
            } else {
                String::new()
            };
            return format!(
                "would {} {}{left}; nothing written",
                self.act,
                counted(self.would, "message", "messages")
            );
        }
        let mut parts = vec![format!(
            "{} new to the archive",
            counted(self.stored, "message", "messages")
        )];
        if self.known > 0 {
            parts.push(format!(
                "{} already held — every place they sat is on the record",
                self.known
            ));
        }
        if self.left > 0 {
            parts.push(format!(
                "{} on the record before and left in peace",
                self.left
            ));
        }
        let record = if self.claims > 0 {
            format!("{} claim(s) written as run {}", self.claims, self.run)
        } else {
            "nothing new to record".to_string()
        };
        format!("{}; {record}", parts.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tally() -> Tally {
        let mut tally = Tally::new("fetch");
        tally.run = "run-0001".to_string();
        tally
    }

    #[test]
    fn a_dry_run_says_what_it_would_do_and_that_nothing_was_written() {
        let mut tally = tally();
        tally.would = 3;
        assert_eq!(
            tally.verdict(true),
            "would fetch 3 messages; nothing written"
        );
        tally.would = 1;
        tally.left = 2;
        assert_eq!(
            tally.verdict(true),
            "would fetch 1 message, 2 on the record before and left in peace; nothing written"
        );
    }

    #[test]
    fn a_run_names_each_clean_outcome_and_its_run() {
        let mut tally = tally();
        assert_eq!(
            tally.verdict(false),
            "0 messages new to the archive; nothing new to record"
        );
        tally.stored = 2;
        tally.known = 1;
        tally.left = 4;
        tally.claims = 11;
        assert_eq!(
            tally.verdict(false),
            "2 messages new to the archive, 1 already held — every place they sat is on the \
             record, 4 on the record before and left in peace; 11 claim(s) written as run run-0001"
        );
    }
}
