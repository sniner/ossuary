//! What one run came to.

use crate::output::counted;

/// The count of a run — a fetch or a takeover, the verdict reads the
/// same.
#[derive(Debug)]
pub struct Tally {
    /// What the run does to a message, for the dry run's verdict:
    /// "fetch" or "import".
    act: &'static str,
    /// The run's id, as every claim of it carries in its `run` field.
    pub run: ossuary_core::Run,
    /// Messages whose bytes were new to the archive.
    pub stored: usize,
    /// Messages the archive already held — their place went on the
    /// record all the same.
    pub known: usize,
    /// Messages a takeover found already in with every place: not
    /// read, nothing recorded.
    pub left: usize,
    /// Claims appended to the log, retractions included.
    pub claims: usize,
    /// Marks taken back: a `mailbox:tag` that stood and no longer does.
    pub taken: usize,
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
            run: ossuary_core::Run::new(),
            stored: 0,
            known: 0,
            left: 0,
            claims: 0,
            taken: 0,
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
                format!(", {} already on record", self.left)
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
            "{} stored",
            counted(self.stored, "message", "messages")
        )];
        if self.known > 0 {
            parts.push(format!("{} already stored", self.known));
        }
        if self.left > 0 {
            parts.push(format!("{} already on record", self.left));
        }
        if self.taken > 0 {
            parts.push(format!("{} taken back", counted(self.taken, "tag", "tags")));
        }
        let record = if self.claims > 0 {
            format!("{} claim(s) written, run {}", self.claims, self.run)
        } else {
            "0 claims written".to_string()
        };
        format!("{}; {record}", parts.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tally() -> Tally {
        let mut tally = Tally::new("fetch");
        tally.run = ossuary_core::Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap();
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
            "would fetch 1 message, 2 already on record; nothing written"
        );
    }

    #[test]
    fn a_run_names_each_clean_outcome_and_its_run() {
        let mut tally = tally();
        assert_eq!(tally.verdict(false), "0 messages stored; 0 claims written");
        tally.stored = 2;
        tally.known = 1;
        tally.left = 4;
        tally.claims = 11;
        assert_eq!(
            tally.verdict(false),
            "2 messages stored, 1 already stored, 4 already on record; 11 claim(s) written, run 315e360b-020e-48be-8f2d-f2002a2ea9b4"
        );
        tally.taken = 1;
        assert!(
            tally
                .verdict(false)
                .contains("4 already on record, 1 tag taken back; 11 claim(s)"),
            "{}",
            tally.verdict(false)
        );
    }
}
