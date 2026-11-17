//! Setting damaged files aside, so that they can be stored again.
//!
//! An archive does not flush the files it stores, so a crash of the
//! system can leave an entry that is empty or cut short. While the entry
//! has its name, the store answers that it holds those bytes, and a run
//! that brings them again stores nothing. [`set_aside`] takes the name
//! away, the way a store does it: the file stays in its shard under a
//! name that says it is damaged, and nothing of it is removed.
//!
//! The claims about the file stay as they are. Its name is the hash of
//! its content, so the same bytes stored again get the same name, and
//! every claim is true of them. Until then the audit reports the file as
//! missing.
//!
//! What brings the bytes again depends on where they came from, and the
//! record says that: a recorded path, a place in a mailbox, or the store
//! of derived files. For a file with recorded paths the ingest memory
//! forgets those paths, or the next run would pass the file by as
//! unchanged.

use std::path::PathBuf;

use immure::{Digest, Store};

use crate::archive::Archive;
use crate::audit::{StoreAudit, audit_store, audit_store_since};
use crate::claim::{Subject, Timestamp, Value};
use crate::error::Result;
use crate::index::Index;

/// One damaged file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aside {
    /// The name it was filed under.
    pub digest: Digest,
    /// Whether it lay in `derived/`, not in `content/`.
    pub derived: bool,
    /// Where it lies now. `None` when nothing was to be written, and
    /// when the file was gone by the time it was to be moved.
    pub moved: Option<PathBuf>,
    /// The paths the record has it at: where ingest met it.
    pub paths: Vec<String>,
    /// Whether the record has it in a mailbox.
    pub mail: bool,
}

/// What [`set_aside`] did.
#[derive(Debug)]
pub struct SetAside {
    /// The walk over `content/`.
    pub content: StoreAudit,
    /// The walk over `derived/`.
    pub derived: StoreAudit,
    /// The damaged files, those of `content/` first.
    pub files: Vec<Aside>,
}

/// Read the files of both stores, all of them or those written at or
/// after `since`, and set every damaged one aside. `index` says where
/// each came from. With `dry_run` nothing is moved and nothing forgotten.
///
/// # Errors
///
/// [`Error::Store`](crate::Error::Store) when a store cannot be walked or
/// a file cannot be moved, and [`Error::Index`](crate::Error::Index) from
/// the index or the ingest memory.
pub fn set_aside(
    archive: &Archive,
    index: &Index,
    since: Option<&Timestamp>,
    dry_run: bool,
) -> Result<SetAside> {
    let walk = |store: &Store| match since {
        Some(since) => audit_store_since(store, since),
        None => audit_store(store, true),
    };
    let content = walk(archive.content())?;
    let derived = walk(archive.derived())?;
    let mut files = Vec::new();
    for (store, report, is_derived) in [
        (archive.content(), &content, false),
        (archive.derived(), &derived, true),
    ] {
        for name in &report.damaged {
            let Some(digest) = report.digest(name) else {
                continue;
            };
            let (paths, mail) = origin(index, name)?;
            let moved = if dry_run {
                None
            } else {
                move_aside(store, digest)?
            };
            files.push(Aside {
                digest: digest.clone(),
                derived: is_derived,
                moved,
                paths,
                mail,
            });
        }
    }
    if !dry_run {
        let paths: Vec<&str> = files
            .iter()
            .flat_map(|file| file.paths.iter().map(String::as_str))
            .collect();
        if !paths.is_empty() {
            archive.ingest_memory()?.forget_paths(&paths)?;
        }
    }
    Ok(SetAside {
        content,
        derived,
        files,
    })
}

/// The paths the record has the file `name` at, and whether it has it in
/// a mailbox.
fn origin(index: &Index, name: &str) -> Result<(Vec<String>, bool)> {
    let mut paths = Vec::new();
    let mut mail = false;
    for (attribute, value) in index.standing(&Subject::parse(name)?)? {
        match (attribute.as_str(), value) {
            ("file:path", Value::String(path)) => paths.push(path),
            ("mailbox:place", _) => mail = true,
            _ => {}
        }
    }
    Ok((paths, mail))
}

fn move_aside(store: &Store, digest: &Digest) -> Result<Option<PathBuf>> {
    let Some(path) = store.find(digest)? else {
        return Ok(None);
    };
    let Some(entry) = store.entry_at(&path) else {
        return Ok(None);
    };
    Ok(Some(store.quarantine(&entry)?.path().to_path_buf()))
}

#[cfg(test)]
mod tests {
    use immure::Algorithm;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::claim::{Attribute, Claim, Run, Source};
    use crate::config::Excludes;
    use crate::ingest::{IngestMemory, Sweep, ingest};

    fn archive(dir: &TempDir) -> Archive {
        Archive::create(dir.path().join("archive"), Algorithm::Sha256).unwrap()
    }

    fn index(archive: &Archive) -> Index {
        let mut index = archive.index().unwrap();
        index.fold(archive.log()).unwrap();
        index
    }

    /// Store `bytes` and record one claim about them.
    fn take(archive: &Archive, store: &Store, bytes: &[u8], attribute: &str) -> Digest {
        let (_, entry) = store.add(bytes).unwrap();
        let claim = Claim::assert(
            Subject::parse(entry.digest().as_str()).unwrap(),
            Attribute::parse(attribute).unwrap(),
            json!("somewhere"),
            Timestamp::now(),
            Source::parse("test").unwrap(),
            Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        )
        .unwrap();
        archive.log().append(&claim).unwrap();
        entry.digest().clone()
    }

    fn sweeping<'a>(memory: &'a IngestMemory, excludes: &'a Excludes) -> Sweep<'a> {
        Sweep {
            host: "testhost",
            tags: &[],
            excludes,
            memory: Some(memory),
            record: None,
            emptied: false,
        }
    }

    fn cut_short(store: &Store, digest: &Digest) {
        let path = store.find(digest).unwrap().unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        #[allow(
            clippy::permissions_set_readonly_false,
            reason = "the test plays the corruption"
        )]
        permissions.set_readonly(false);
        std::fs::set_permissions(&path, permissions).unwrap();
        std::fs::write(&path, b"").unwrap();
    }

    #[test]
    fn a_damaged_file_is_set_aside_and_a_sound_one_is_kept() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let sound = take(&archive, archive.content(), b"sound", "file:path");
        let mail = take(&archive, archive.content(), b"a message", "mailbox:place");
        let derived = take(&archive, archive.derived(), b"extracted", "prov:origin");
        cut_short(archive.content(), &mail);
        cut_short(archive.derived(), &derived);

        let done = set_aside(&archive, &index(&archive), None, false).unwrap();

        assert_eq!(done.files.len(), 2);
        let message = &done.files[0];
        assert_eq!(message.digest, mail);
        assert!(message.mail && !message.derived && message.paths.is_empty());
        assert!(
            message.moved.as_ref().unwrap().is_file(),
            "kept, not removed"
        );
        assert!(done.files[1].derived);
        assert!(archive.content().find(&mail).unwrap().is_none());
        assert!(archive.derived().find(&derived).unwrap().is_none());
        assert!(archive.content().find(&sound).unwrap().is_some());

        let (status, _) = archive.content().add(b"a message").unwrap();
        assert!(status.is_new(), "the same bytes are stored again");
    }

    #[test]
    fn a_dry_run_moves_nothing() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let file = take(&archive, archive.content(), b"a file", "file:path");
        cut_short(archive.content(), &file);

        let done = set_aside(&archive, &index(&archive), None, true).unwrap();

        assert_eq!(done.files.len(), 1);
        assert_eq!(done.files[0].moved, None);
        assert_eq!(done.files[0].paths, ["somewhere"]);
        assert!(archive.content().find(&file).unwrap().is_some());
    }

    #[test]
    fn the_next_ingest_stores_a_file_that_was_set_aside_again() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let source = dir.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("note.txt"), b"a note worth keeping").unwrap();
        let excludes = Excludes::default();
        let memory = archive.ingest_memory().unwrap();
        let sweep = sweeping(&memory, &excludes);
        let first = ingest(archive.content(), archive.log(), [&source], &sweep).unwrap();
        assert_eq!(first.stored, 1);
        let again = ingest(archive.content(), archive.log(), [&source], &sweep).unwrap();
        assert_eq!(again.unchanged, 1, "the memory passes the file by");
        let digest = archive
            .content()
            .entries()
            .next()
            .unwrap()
            .unwrap()
            .digest()
            .clone();
        cut_short(archive.content(), &digest);

        let done = set_aside(&archive, &index(&archive), None, false).unwrap();
        assert_eq!(done.files[0].paths.len(), 1);
        let memory = archive.ingest_memory().unwrap();
        let sweep = sweeping(&memory, &excludes);
        let repaired = ingest(archive.content(), archive.log(), [&source], &sweep).unwrap();

        assert_eq!(repaired.stored, 1);
        assert_eq!(
            archive.content().read(&digest).unwrap().unwrap(),
            b"a note worth keeping"
        );
    }
}
