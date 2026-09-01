//! Core of the ossuary archive: claims, segments, and the fold.
//!
//! The on-disk format this crate reads and writes is specified in
//! `docs/format.md` at the repository root. The format is the contract and
//! must outlive its software; this crate is one implementation of it.
//!
//! ```
//! use ossuary_core::Claim;
//!
//! let line = r#"{"subject":"sha256:9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"user:tag","value":"holiday","time":"2026-10-05T19:00:00Z","source":"user"}"#;
//! let claim = Claim::parse_line(line)?;
//!
//! assert_eq!(claim.attribute().as_str(), "user:tag");
//! assert_eq!(claim.to_line(), line, "reading and writing agree");
//! # Ok::<(), ossuary_core::Error>(())
//! ```

#![forbid(unsafe_code)]

mod claim;
mod error;

pub use claim::{Attribute, Claim, Source, Subject, Timestamp, Value};
pub use error::{Error, Result};
