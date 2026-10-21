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
    /// Messages `fetch --compare` found in the archive by their
    /// Message-ID: not fetched.
    pub matched: usize,
    /// Messages `fetch --compare` did not fetch because an earlier
    /// `--compare` fetched them and the archive holds them.
    pub recalled: usize,
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
            matched: 0,
            recalled: 0,
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
            let mut parts = vec![counted(self.would, "message", "messages")];
            if self.left > 0 {
                parts.push(format!("{} already imported", self.left));
            }
            parts.extend(self.found());
            return format!("would {} {}; nothing written", self.act, parts.join(", "));
        }
        let mut parts = vec![format!(
            "{} stored",
            counted(self.stored, "message", "messages")
        )];
        if self.known > 0 {
            parts.push(format!("{} already stored", self.known));
        }
        if self.left > 0 {
            parts.push(format!("{} already imported", self.left));
        }
        parts.extend(self.found());
        if self.taken > 0 {
            parts.push(format!("{} retracted", counted(self.taken, "tag", "tags")));
        }
        let record = if self.claims > 0 {
            format!("{} claim(s) written, run {}", self.claims, self.run)
        } else {
            "0 claims written".to_string()
        };
        format!("{}; {record}", parts.join(", "))
    }

    /// The messages `fetch --compare` did not fetch, and why.
    fn found(&self) -> Option<String> {
        match (self.matched, self.recalled) {
            (0, 0) => None,
            (matched, 0) => Some(format!("{matched} found in the archive by Message-ID")),
            (0, recalled) => Some(format!(
                "{recalled} found in the archive by an earlier --compare"
            )),
            (matched, recalled) => Some(format!(
                "{matched} found in the archive by Message-ID, {recalled} by an earlier --compare"
            )),
        }
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
            "would fetch 1 message, 2 already imported; nothing written"
        );
        tally.left = 0;
        tally.matched = 7;
        assert_eq!(
            tally.verdict(true),
            "would fetch 1 message, 7 found in the archive by Message-ID; nothing written"
        );
        tally.recalled = 2;
        assert_eq!(
            tally.verdict(true),
            "would fetch 1 message, 7 found in the archive by Message-ID, 2 by an earlier --compare; nothing written"
        );
        tally.matched = 0;
        assert_eq!(
            tally.verdict(true),
            "would fetch 1 message, 2 found in the archive by an earlier --compare; nothing written"
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
            "2 messages stored, 1 already stored, 4 already imported; 11 claim(s) written, run 315e360b-020e-48be-8f2d-f2002a2ea9b4"
        );
        tally.taken = 1;
        assert!(
            tally
                .verdict(false)
                .contains("4 already imported, 1 tag retracted; 11 claim(s)"),
            "{}",
            tally.verdict(false)
        );
    }
}
