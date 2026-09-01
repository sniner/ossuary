//! Core of the ossuary archive: claims, segments, and the fold.
//!
//! The on-disk format this crate reads and writes is specified in
//! `docs/format.md` at the repository root. The format is the contract and
//! must outlive its software; this crate is one implementation of it.

#![forbid(unsafe_code)]
