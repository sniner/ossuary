//! Core of the ossuary archive: claims, segments, and the fold.
//!
//! The on-disk format this crate reads and writes is specified in
//! `docs/format.md` at the repository root. The format is the contract and
//! must outlive its software; this crate is one implementation of it.
//!
//! ```
//! use ossuary_core::Claim;
//!
//! let line = r#"{"subject":"9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","attribute":"user:tag","value":"holiday","time":"2026-10-05T19:00:00Z","source":"user"}"#;
//! let claim = Claim::parse_line(line)?;
//!
//! assert_eq!(claim.attribute().as_str(), "user:tag");
//! assert_eq!(claim.to_line(), line, "reading and writing agree");
//! # Ok::<(), ossuary_core::Error>(())
//! ```

// `assert!(x.is_empty())` reads as the statement it makes; the form
// clippy suggests, `assert_eq!(x, [] as [T; 0])`, says the same and
// names the type for nothing.
#![cfg_attr(test, allow(clippy::assert_is_empty))]
#![forbid(unsafe_code)]

mod accession;
mod annotate;
mod archive;
mod aside;
mod audit;
mod claim;
mod config;
mod delta;
mod describe;
mod error;
mod examine;
mod export;
mod extract;
mod extractor;
mod index;
mod ingest;
mod log;
mod memory;
mod part;
mod recorder;
mod retract;
mod seal;
mod tidy;
pub mod vocabulary;
mod weed;

pub use accession::{Admitted, Sighting, admit, sighting_claims};
pub use annotate::annotate;
pub use archive::{Archive, is_archive};
pub use aside::{Aside, SetAside, set_aside};
pub use audit::{
    Audit, Ends, Fixity, HEAD, LINKS, LogAudit, StoreAudit, Twin, Undescribed, audit_log,
    audit_store, audit_store_since,
};
pub use claim::{
    Attribute, Claim, ClaimId, Run, Source, Subject, Timestamp, Value, Written, standing_ids_in,
};
pub use config::{Config, Excludes};
pub use delta::{Delta, delta};
pub use describe::{Described, Undescribing, describe, undescribed};
pub use error::{Error, Result};
pub use examine::{Event, Observer, Settlement, Tally, examine};
pub use export::{Placed, lay_out};
pub use immure::{Algorithm, Digest, Flush, Store};
pub use index::{Episode, Field, Folded, Index, Placement, Scope, Standing, Term};
pub use ingest::{Ingested, Previewed, Sweep, ingest, preview};
pub use log::{
    Contents, GENERATION, Log, OLD_HEAD, Open, SEAL_AT, SEAL_AT_BYTES, Segment, WalkBack, Writable,
    old_open_dirs,
};
pub use memory::IngestMemory;
pub use part::Part;
pub use recorder::{Batch, Recorder};
pub use retract::retract;
pub use seal::{
    Continued, Cover, Files, Seal, SealCheck, Sealing, Since, cover, files, parse_id, seal, since,
};
pub use tidy::{TEMP_MIN_AGE, Tidied, tidy};
pub use weed::{Weeded, twins, weed};
