//! What can go wrong assembling or reading a claim.

use thiserror::Error;

/// This crate's `Result`.
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// The subject is not `<algorithm>:<hex>` with a full-length digest.
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
}
