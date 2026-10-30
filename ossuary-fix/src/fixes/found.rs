//! What `list` and `chain` find in an archive besides the form of its
//! headers, and how they say it.

/// What a fix found: losses, and segments it has to place.
#[derive(Debug, Default)]
pub struct Found {
    /// Segments named as a predecessor that the store does not hold, and
    /// no mend records as lost, with the segment, or the open segment,
    /// that names each.
    pub missing: Vec<(String, String)>,
    /// Whether the open segment is missing while segments are sealed.
    pub head_lost: bool,
    /// Whether the open segment names no sealed segment while there are
    /// some: it was begun anew after a loss.
    pub head_anew: bool,
    /// Segments that nothing links to the rest, each with where it goes.
    pub placed: Vec<String>,
}

impl Found {
    /// Whether the rewrite gives up claims for good: a segment or the
    /// open segment is missing.
    #[must_use]
    pub fn loses_claims(&self) -> bool {
        !self.missing.is_empty() || self.head_lost
    }

    /// One line per thing found, for the answer.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (named_by, missing) in &self.missing {
            lines.push(format!(
                "segment {missing} is missing ({named_by} names it); the chain is closed over it, and the claims it held are lost"
            ));
        }
        if self.head_lost {
            lines.push(
                "the open segment is missing; a new one is begun after the last sealed segment, and the claims recorded since that seal are lost"
                    .to_string(),
            );
        }
        if self.head_anew {
            lines.push(
                "the open segment names no sealed segment, so it was begun anew after a loss; it now follows the last sealed segment"
                    .to_string(),
            );
        }
        lines.extend(self.placed.iter().cloned());
        lines
    }
}
