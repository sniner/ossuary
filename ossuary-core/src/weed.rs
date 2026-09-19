//! Weeding: the derived copy of what `content/` holds too, let go.
//!
//! A file won as a derived file — an attachment unpacked from a mail —
//! and later taken in as an original stands in both stores under the
//! same name, because the name is the bytes' own. The log speaks of the
//! subject and never of a store, and `content/` answers first wherever
//! both hold a name, so the copy in `derived/` answers no question any
//! more. It is the one thing an archive holds that it can let go of
//! without loss: no claim points at it, and the audit's presence check
//! is satisfied by either store.
//!
//! Letting go is done on proof, never on the name alone. Both copies are
//! verified first, and what happens follows from how they fared: both
//! sound, the derived copy goes; the derived copy damaged and the
//! original sound, it goes as well, being worth nothing; the original
//! damaged and the derived copy sound, nothing goes on its own — that is
//! the one case where `derived/` can make an original good again, and it
//! is done only when asked, by setting the damaged original aside the
//! way its store does and storing the sound bytes under the name. Both
//! damaged, or either unreadable, everything stays as it is: those are
//! the audit's findings, and a copy of the archive is the way to them.

use std::path::PathBuf;

use immure::{Entry, Store};

use crate::archive::Archive;
use crate::audit::{Fixity, Twin};
use crate::error::{Error, Result};

/// What weeding one twin did.
#[derive(Debug, PartialEq, Eq)]
pub enum Weeded {
    /// The copy in `derived/` is gone; the original answers for the
    /// bytes.
    Released,
    /// The damaged original was set aside under this path, the sound
    /// bytes from `derived/` stored in its place, and the copy in
    /// `derived/` is gone.
    Repaired {
        /// Where the damaged original lies now.
        aside: PathBuf,
    },
    /// Nothing was touched: the twin is not one to let go of, or not
    /// without being asked — see [`Twin::releasable`] and
    /// [`Twin::repairable`].
    Standing,
}

impl Twin {
    /// Whether the copy in `derived/` can go: the original is sound, so
    /// the archive loses nothing, and the derived copy could be read, so
    /// nothing unexplained is being deleted through.
    #[must_use]
    pub fn releasable(&self) -> bool {
        self.content == Fixity::Sound && !matches!(self.derived, Fixity::Unreadable(_))
    }
}

/// Every content held by both stores, each copy verified.
///
/// Walks `derived/`, the smaller store, and asks `content/` for each
/// name; only what both hold is read and re-hashed, on both sides. The
/// answer is a list, not the audit's: nothing here consults what an
/// earlier audit found, because what is about to be let go of has to be
/// proved now.
///
/// # Errors
///
/// [`Error::Store`](crate::Error::Store) when a store cannot be walked or
/// asked; what one entry has to answer for lands in its [`Fixity`].
pub fn twins(archive: &Archive) -> Result<Vec<Twin>> {
    let content = archive.content();
    let derived = archive.derived();
    let mut twins = Vec::new();
    for entry in derived.entries() {
        let entry = entry?;
        let Some(path) = content.find(entry.digest())? else {
            continue;
        };
        let original = content.entry_at(&path).map_or_else(
            || Fixity::Unreadable(format!("{}: not an entry's name", path.display())),
            |original| fixity(content, &original),
        );
        twins.push(Twin {
            digest: entry.digest().clone(),
            content: original,
            derived: fixity(derived, &entry),
        });
    }
    Ok(twins)
}

/// Weed one twin: let the derived copy go where that loses nothing, put
/// the sound bytes back under the original's name where `repair` asks
/// for it, and leave everything else standing.
///
/// # Errors
///
/// [`Error::Store`](crate::Error::Store) when removing, setting aside or
/// storing fails, and [`Error::TwinChanged`] when the derived copy read
/// back as other bytes while it was being stored in the original's
/// place — the original stays set aside, and what was read stands in
/// `content/` under its own name, as the audit's next observation.
pub fn weed(archive: &Archive, twin: &Twin, repair: bool) -> Result<Weeded> {
    let content = archive.content();
    let derived = archive.derived();
    if twin.releasable() {
        derived.remove(&twin.digest)?;
        return Ok(Weeded::Released);
    }
    if !(repair && twin.repairable()) {
        return Ok(Weeded::Standing);
    }
    // The reader is opened before the original is set aside, so a derived
    // copy that went away in the meantime leaves the twin standing
    // instead of an original set aside for nothing.
    let Some(sound) = derived.reader(&twin.digest)? else {
        return Ok(Weeded::Standing);
    };
    let Some(path) = content.find(&twin.digest)? else {
        return Ok(Weeded::Standing);
    };
    let Some(original) = content.entry_at(&path) else {
        return Ok(Weeded::Standing);
    };
    let aside = content.quarantine(&original)?;
    let (_, stored) = content.add_reader(sound)?;
    if stored.digest() != &twin.digest {
        return Err(Error::TwinChanged(twin.digest.as_str().to_string()));
    }
    derived.remove(&twin.digest)?;
    Ok(Weeded::Repaired {
        aside: aside.path().to_path_buf(),
    })
}

/// One entry read whole and re-hashed, the way the audit does it.
fn fixity(store: &Store, entry: &Entry) -> Fixity {
    match store.verify(entry) {
        Ok(true) => Fixity::Sound,
        Ok(false) => Fixity::Damaged,
        Err(error) => Fixity::Unreadable(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use immure::{Algorithm, Digest};
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::audit::{Audit, audit_log, audit_store};
    use crate::claim::{Attribute, Claim, Source, Subject, Timestamp};

    fn archive(dir: &TempDir) -> Archive {
        Archive::create(dir.path().join("archive"), Algorithm::Sha256).unwrap()
    }

    /// Bytes in both stores, with one claim on the record — a twin the
    /// way the archive gets one.
    fn twin(archive: &Archive, bytes: &[u8]) -> Digest {
        let (_, entry) = archive.content().add(bytes).unwrap();
        archive.derived().add(bytes).unwrap();
        let claim = Claim::assert(
            Subject::parse(entry.digest().as_str()).unwrap(),
            Attribute::parse("file:size").unwrap(),
            json!(bytes.len()),
            Timestamp::parse("2026-09-18T12:00:00Z").unwrap(),
            Source::parse("test").unwrap(),
            crate::claim::Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        )
        .unwrap();
        archive.log().append(&claim).unwrap();
        entry.digest().clone()
    }

    fn audit(archive: &Archive) -> Audit {
        Audit::assemble(
            audit_store(archive.content()).unwrap(),
            audit_store(archive.derived()).unwrap(),
            audit_log(archive.log()).unwrap(),
        )
    }

    fn tamper(store: &Store, digest: &Digest, bytes: &[u8]) {
        let path = store.find(digest).unwrap().expect("held");
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        #[allow(
            clippy::permissions_set_readonly_false,
            reason = "the test plays the corruption"
        )]
        permissions.set_readonly(false);
        std::fs::set_permissions(&path, permissions).unwrap();
        std::fs::write(&path, bytes).unwrap();
    }

    #[test]
    fn a_file_held_by_one_store_is_no_twin() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        archive.content().add(b"an original").unwrap();
        archive.derived().add(b"a derived file").unwrap();

        assert!(twins(&archive).unwrap().is_empty());
    }

    #[test]
    fn a_sound_twin_is_released_and_the_archive_stays_sound() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let digest = twin(&archive, b"won first, taken in later");

        let found = twins(&archive).unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].releasable());
        assert_eq!(weed(&archive, &found[0], false).unwrap(), Weeded::Released);

        assert!(!archive.derived().contains(&digest).unwrap());
        assert!(archive.content().contains(&digest).unwrap());
        let after = audit(&archive);
        assert!(after.is_sound(), "nothing the claims speak of is missing");
        assert!(after.twins.is_empty());
        assert!(twins(&archive).unwrap().is_empty());
    }

    #[test]
    fn a_damaged_derived_copy_of_a_sound_original_is_released() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let digest = twin(&archive, b"the original is what counts");
        tamper(archive.derived(), &digest, b"the original is what counts?");

        let found = twins(&archive).unwrap();
        assert_eq!(found[0].derived, Fixity::Damaged);
        assert!(found[0].releasable());
        assert_eq!(weed(&archive, &found[0], false).unwrap(), Weeded::Released);

        assert!(!archive.derived().contains(&digest).unwrap());
        assert!(audit(&archive).is_sound());
    }

    #[test]
    fn a_damaged_original_stands_until_repair_is_asked_for() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let digest = twin(&archive, b"the original went bad");
        tamper(archive.content(), &digest, b"the original went bad!");

        let found = twins(&archive).unwrap();
        assert_eq!(found[0].content, Fixity::Damaged);
        assert_eq!(found[0].derived, Fixity::Sound);
        assert!(!found[0].releasable());
        assert!(found[0].repairable());
        assert_eq!(weed(&archive, &found[0], false).unwrap(), Weeded::Standing);
        assert!(archive.derived().contains(&digest).unwrap());

        let repaired = weed(&archive, &found[0], true).unwrap();
        let Weeded::Repaired { aside } = repaired else {
            panic!("repaired, got {repaired:?}");
        };
        assert!(aside.is_file(), "the damaged original is kept, set aside");
        assert!(!archive.derived().contains(&digest).unwrap());
        let entry = archive
            .content()
            .entry_at(&archive.content().find(&digest).unwrap().unwrap())
            .unwrap();
        assert!(archive.content().verify(&entry).unwrap());
        assert_eq!(
            archive.content().read(&digest).unwrap().unwrap(),
            b"the original went bad"
        );
        assert!(audit(&archive).is_sound());
    }

    #[test]
    fn a_twin_damaged_in_both_stores_stands() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let digest = twin(&archive, b"gone bad twice");
        tamper(archive.content(), &digest, b"gone bad twice!");
        tamper(archive.derived(), &digest, b"gone bad twice?");

        let found = twins(&archive).unwrap();
        assert!(!found[0].releasable());
        assert!(!found[0].repairable());
        assert_eq!(weed(&archive, &found[0], true).unwrap(), Weeded::Standing);
        assert!(archive.derived().contains(&digest).unwrap());
        assert!(archive.content().contains(&digest).unwrap());
    }
}
