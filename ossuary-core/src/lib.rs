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

mod archive;
mod claim;
mod config;
mod error;
mod extract;
mod index;
mod ingest;
mod log;
mod manifest;

pub use archive::Archive;
pub use claim::{Attribute, Claim, Source, Subject, Timestamp, Value};
pub use config::{Config, Excludes};
pub use error::{Error, Result};
pub use extract::{Derivation, EXAMINED, Examined, record_examination};
pub use immure::Algorithm;
pub use index::{Folded, Index};
pub use ingest::{IngestMemory, Ingested, ingest};
pub use log::{GENERATION, Log, Segment};
pub use manifest::{Manifest, Manifests};
