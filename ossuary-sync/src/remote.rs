//! The place an archive is copied to, reduced to what a sync needs:
//! read a file, create a file that must not exist yet, replace the one
//! file that changes, make a directory.
//!
//! The trait states guarantees, not the way to keep them. A temporary
//! file and a rename is how a directory makes a file appear complete;
//! a server without rename keeps the same guarantee its own way, so the
//! rename is not part of the trait.
//!
//! Paths are relative to the archive root and separated by `/`, as the
//! files lie in the archive: `FORMAT`, `head.jsonl`,
//! `content/80/0a/800a…`, `claims/1b/1bbb….seg.zst`. Making the
//! directories a path needs is the backend's business.

use std::io::Read;

use anyhow::Result;

/// What [`Remote::create`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Created {
    /// The file was written.
    New,
    /// A file of that name was there already and was left as it is.
    /// Every name but the head's is the hash of its content or fixed by
    /// the format, so the file holds the same bytes.
    Known,
}

/// A place an archive can be copied to.
pub trait Remote: Send + Sync {
    /// The file at `path`, or `None` when there is none.
    ///
    /// # Errors
    ///
    /// When the place cannot be read.
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>>;

    /// Write a file that must not exist yet, from `from`, which yields
    /// `size` bytes. A reader sees the file complete or not at all, and
    /// once this returns `New` the file is stored as durably as the place
    /// can store it. `from` is read as it is sent, never whole: a file
    /// can be larger than memory.
    ///
    /// # Errors
    ///
    /// When the file cannot be written. Nothing is left under `path`.
    fn create(&self, path: &str, from: &mut dyn Read, size: u64) -> Result<Created>;

    /// Replace the file at `path` with `bytes`, or write it if there is
    /// none. A reader sees the old file or the new one, never a mix; a
    /// place may let it see no file in between. Only the open segment is
    /// ever replaced.
    ///
    /// # Errors
    ///
    /// When the file cannot be written. The old file is left as it was.
    fn replace(&self, path: &str, bytes: &[u8]) -> Result<()>;

    /// Make the directory at `path`, and the ones above it, if there is
    /// none. A copy needs every store directory an archive has, an empty
    /// one too: a reader that opens the copy as an archive reads each
    /// store.
    ///
    /// # Errors
    ///
    /// When the directory cannot be made.
    fn make_dir(&self, path: &str) -> Result<()>;
}

/// The temporary file for `path`, for a place that writes a file under
/// another name and renames it into place: in the store's `tmp/` for a
/// path inside a store, beside it in the root otherwise. The name is
/// fixed per file, so a run after an interrupted one overwrites what that
/// one left instead of adding to it.
pub fn temp_path(path: &str) -> String {
    match path.split_once('/') {
        Some((store, rest)) => {
            let name = rest.rsplit('/').next().unwrap_or(rest);
            format!("{store}/tmp/{name}.tmp")
        }
        None => format!("{path}.tmp"),
    }
}

/// The collection `path` lies in, `""` for the root.
pub fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_files_lie_in_the_store_or_beside_the_file() {
        assert_eq!(
            temp_path("content/80/0a/800a.zst"),
            "content/tmp/800a.zst.tmp"
        );
        assert_eq!(temp_path("head.jsonl"), "head.jsonl.tmp");
        assert_eq!(parent("content/tmp/800a.tmp"), "content/tmp");
        assert_eq!(parent("FORMAT"), "");
    }
}
