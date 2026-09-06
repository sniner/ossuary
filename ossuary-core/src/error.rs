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

    /// `null` stood where a value belongs.
    ///
    /// A value may be any JSON type but this one: a claim that asserts
    /// nothing is not a claim, and a retraction says which value it retracts
    /// by carrying it — or none at all, by carrying no `value` key.
    #[error("null is not a value")]
    NullValue,

    /// The claim carries no value and is not a retraction.
    ///
    /// Only a retraction may leave the value out — that is the form that
    /// retracts the whole attribute. An assertion without a value says
    /// nothing.
    #[error("a claim without a value must be a retraction")]
    ValueRequired,

    /// The line is not JSON, or not the JSON of a claim.
    #[error("not a claim: {0}")]
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
    #[error("{context}: {source}")]
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
    #[error("segment generation {0} is not one this build understands")]
    SegmentGeneration(u32),

    /// A digest that should name a segment names nothing in the store.
    #[error("no segment under {0}")]
    SegmentMissing(String),

    /// A segment is UTF-8 text by definition, and these bytes are not.
    #[error("a segment is UTF-8 text, and this is not")]
    NotText,

    /// A line of a segment did not read back as a claim.
    ///
    /// The line number is 1-based and counts the header, so it is the number
    /// an editor or `sed -n` would show for the same line.
    #[error("line {line}: {source}")]
    BadLine { line: usize, source: Box<Error> },

    /// The directory is not an archive: no `FORMAT` mark stands in it.
    #[error("{}: not an ossuary archive — no FORMAT mark", .0.display())]
    NoArchive(std::path::PathBuf),

    /// An archive already stands where one was to be created.
    #[error("{}: an archive already stands here", .0.display())]
    AlreadyArchive(std::path::PathBuf),

    /// The `FORMAT` mark would not read back.
    #[error("{}: the FORMAT mark did not read back — damaged, or not ossuary's", .0.display())]
    BadMark(std::path::PathBuf),

    /// The archive's `config.toml` would not read back.
    ///
    /// Strictness is the point: a key this build does not know may be a
    /// typo or a newer ossuary's knob, and either way applying half a
    /// write policy is worse than applying none.
    #[error("{}: not readable as the archive's settings — fix the file, or remove it to run on the defaults\n{trouble}", .path.display())]
    BadConfig {
        path: std::path::PathBuf,
        trouble: String,
    },

    /// An exclude pattern would not compile into a glob.
    #[error("not a glob pattern: {pattern:?} — {trouble}")]
    Pattern { pattern: String, trouble: String },

    /// The archive is written in a generation this build does not know.
    ///
    /// Written by a newer ossuary, and healthy: a layout this build has
    /// never seen would look familiar in exactly the wrong way, which is
    /// what the mark exists to prevent. A newer build reads it.
    #[error("archive generation {0} is not one this build understands")]
    ArchiveGeneration(u32),
}
