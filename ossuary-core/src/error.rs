//! What can go wrong assembling or reading a claim.

use thiserror::Error;

/// This crate's `Result`.
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// The subject is not a full-length digest in hex.
    #[error("not a subject: {0:?}")]
    Subject(String),

    /// The attribute is not `namespace:attribute` in the allowed characters.
    #[error("not an attribute: {0:?}")]
    Attribute(String),

    /// The timestamp is not RFC 3339, UTC, `Z`, whole seconds.
    #[error("not a timestamp: {0:?}")]
    Timestamp(String),

    /// The source is empty, or holds whitespace or control characters.
    #[error("not a source: {0:?}")]
    Source(String),

    /// The run is not a dashed lowercase UUID.
    #[error("not a run id: {0:?}")]
    Run(String),

    /// A term without a colon named no field of a claim.
    #[error(
        "unknown field {0:?}; a term without a colon must name a field: subject, attribute, value, time, source, run or retract"
    )]
    Field(String),

    /// A `retract` term asked for something other than true or false.
    #[error("invalid retract value {0:?}; use retract=true or retract=false")]
    Retract(String),

    /// `null` stood where a value belongs.
    ///
    /// A value may be any JSON type but this one: a claim that asserts
    /// nothing is not a claim, and a retraction says which value it retracts
    /// by carrying it — or none at all, by carrying no `value` key.
    #[error("null is not allowed as a value")]
    NullValue,

    /// The claim carries no value and is not a retraction.
    ///
    /// Only a retraction may leave the value out — that is the form that
    /// retracts the whole attribute. An assertion without a value says
    /// nothing.
    #[error("a claim without a value must be a retraction")]
    ValueRequired,

    /// The line is not JSON, or not the JSON of a claim.
    #[error("not a claim")]
    Line(#[from] serde_json::Error),

    /// The store underneath said no.
    #[error(transparent)]
    Store(#[from] immure::Error),

    /// The index said no — and an index is a cache: when it is broken
    /// rather than merely refusing, deleting it loses nothing that a fold
    /// does not restore.
    #[error(transparent)]
    Index(#[from] rusqlite::Error),

    /// Reading or writing the open segment failed.
    #[error("{context}")]
    Io {
        context: String,
        source: std::io::Error,
    },

    /// The first line of a segment is not a segment header.
    ///
    /// Whatever the file is, it is not a segment — or it is one whose head
    /// was mangled, which for a reader is the same thing: nothing after an
    /// unreadable header can be trusted to be what it looks like.
    #[error("not a segment: first line {0:?}")]
    SegmentHeader(String),

    /// A segment declares a generation this build does not know.
    ///
    /// Written by a newer ossuary, and healthy — the one wrong response is
    /// to guess at it. The header exists so that a reader refuses what it
    /// does not know; a newer build reads it.
    #[error("segment generation {0} is not supported by this version of ossuary")]
    SegmentGeneration(u32),

    /// A digest that should name a segment names nothing in the store.
    #[error("segment {0} not found in claims/")]
    SegmentMissing(String),

    /// A segment is UTF-8 text by definition, and these bytes are not.
    #[error("segment is not UTF-8 text")]
    NotText,

    /// A line of a segment did not read back as a claim.
    ///
    /// The line number is 1-based and counts the header, so it is the number
    /// an editor or `sed -n` would show for the same line.
    #[error("line {line}")]
    BadLine { line: usize, source: Box<Error> },

    /// The directory is not an archive: no `FORMAT` mark stands in it.
    #[error("{}: not an ossuary archive (no FORMAT file)", .0.display())]
    NoArchive(std::path::PathBuf),

    /// An archive already stands where one was to be created.
    #[error("{}: an archive already exists here", .0.display())]
    AlreadyArchive(std::path::PathBuf),

    /// A named ingest root is an archive, or lies inside one. The path
    /// is the archive's root, wherever in it the naming pointed.
    #[error("{}: an ossuary archive; an archive and the paths inside it cannot be ingested", .0.display())]
    IngestsArchive(std::path::PathBuf),

    /// The `FORMAT` mark would not read back.
    #[error("{}: cannot read the FORMAT file; it is damaged or was not written by ossuary", .0.display())]
    BadMark(std::path::PathBuf),

    /// The archive's `config.toml` would not read back.
    ///
    /// Strictness is the point: a key this build does not know may be a
    /// typo or a newer ossuary's knob, and either way applying half a
    /// write policy is worse than applying none.
    #[error("{}: invalid archive configuration; fix the file, or remove it to use the defaults\n{trouble}", .path.display())]
    BadConfig {
        path: std::path::PathBuf,
        trouble: String,
    },

    /// An exclude pattern would not compile into a glob.
    #[error("not a glob pattern: {pattern:?}; {trouble}")]
    Pattern { pattern: String, trouble: String },

    /// The derived copy of a twin read back as other bytes while `weed`
    /// was storing it in the damaged original's place. The original
    /// stays set aside, and what was read stands in `content/` under its
    /// own name, where the next audit notes it as unrecorded.
    #[error(
        "{0}: the copy in derived/ is damaged as well; the original remains set aside, and the bytes read are stored in content/ under their own hash"
    )]
    TwinChanged(String),

    /// The archive is written in a generation this build does not know.
    ///
    /// Written by a newer ossuary, and healthy: a layout this build has
    /// never seen would look familiar in exactly the wrong way, which is
    /// what the mark exists to prevent. A newer build reads it.
    #[error("archive generation {0} is not supported by this version of ossuary")]
    ArchiveGeneration(u32),

    /// A beginning that names several subjects names none of them.
    #[error("{given:?} matches {count} subjects; give a longer prefix")]
    Ambiguous { given: String, count: usize },

    /// A given name, whole or begun, that the record does not know.
    #[error("no subject begins with {0:?}")]
    Unknown(String),

    /// The extract orchestration refused: a name outside the extractor
    /// grammar, an identify answer outside the protocol, an examination
    /// answer that does not add up, a loop that never runs dry. The
    /// sentence names the state and the move; no caller tells the cases
    /// apart, so one variant carries them all.
    #[error("{0}")]
    Extract(String),
}

impl Error {
    /// The whole story in one sentence: the error, then every cause on
    /// its chain, joined the way `anyhow` joins them under `{:#}`.
    /// `Display` names only the error's own step — the cause lives on
    /// [`source`](std::error::Error::source), said once, and a printer
    /// showing a bare error to a reader spells it with this instead.
    #[must_use]
    pub fn spelled(&self) -> String {
        use std::error::Error as _;
        let mut sentence = self.to_string();
        let mut cause = self.source();
        while let Some(error) = cause {
            sentence.push_str(": ");
            sentence.push_str(&error.to_string());
            cause = error.source();
        }
        sentence
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spelled_says_the_cause_once_and_display_not_at_all() {
        let error = Error::Io {
            context: "reading the extractor's output".to_string(),
            source: std::io::Error::other("pipe folded"),
        };
        assert_eq!(error.to_string(), "reading the extractor's output");
        assert_eq!(
            error.spelled(),
            "reading the extractor's output: pipe folded"
        );

        let broken = Error::BadLine {
            line: 2,
            source: Box::new(Error::NullValue),
        };
        assert_eq!(broken.spelled(), "line 2: null is not allowed as a value");
    }
}
