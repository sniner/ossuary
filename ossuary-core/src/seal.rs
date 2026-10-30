//! The seal: the archive's whole record named by one digest.
//!
//! Every segment names the segment sealed before it, and every claim
//! names its subject by digest, so the digest of the last sealed segment
//! fixes every segment before it and every file their claims refer to. A seal is that digest, made sure to cover everything: the
//! chain is read whole first, and every segment must be reachable from
//! the open head; then the open head is sealed, so that its claims are
//! covered as well. Kept outside the archive, the digest later shows
//! whether the archive still holds everything it held then, unchanged.
//!
//! Nothing is written for a seal that the log does not hold already: the
//! seal is a segment like any other, and every segment's digest names
//! the record up to it.

use std::collections::BTreeSet;
use std::fs;

use immure::{Algorithm, Digest};

use crate::archive::Archive;
use crate::audit::{Fixity, LogAudit, audit_log, reference};
use crate::claim::Timestamp;
use crate::error::{Error, Result};

/// A seal of the archive: the segment whose digest names its record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seal {
    /// The last sealed segment; its digest is the seal.
    pub segment: Digest,
    /// The hash that made the digest, the archive's own.
    pub algorithm: Algorithm,
    /// Whether the open head was sealed for it. False when the head held
    /// no claims and named this segment alone: nothing was recorded
    /// since it was sealed, and the seal is the same as then.
    pub sealed_now: bool,
    /// Sealed segments the seal covers, itself included.
    pub segments: usize,
    /// Claims in those segments.
    pub claims: usize,
}

/// What sealing the archive came to.
#[derive(Debug)]
pub enum Sealing {
    /// The archive's record, named by one segment.
    Sealed(Seal),
    /// The archive holds no claims and no segment: there is nothing to
    /// seal.
    Empty,
    /// The log is not whole, and a seal over it would not cover the
    /// archive, or would cover up a loss. The audit of the log says why:
    /// its findings, or segments the open head does not reach.
    Refused(Box<LogAudit>),
}

/// Seal the archive: read the log whole, refuse if it is not whole, seal
/// the open head when it holds claims, and name the segment that covers
/// the record.
///
/// With `dry_run` nothing is written, and the seal is the one the call
/// would make: the open head's bytes go into the store verbatim when it
/// is sealed, so its digest is known beforehand.
///
/// # Errors
///
/// Everything [`audit_log`] and [`Log::seal`](crate::Log::seal) can
/// answer, and [`Error::Io`] reading the open head for a dry run.
pub fn seal(archive: &Archive, dry_run: bool) -> Result<Sealing> {
    let log = archive.log();
    let report = audit_log(log)?;
    if report.findings() > 0 {
        return Ok(Sealing::Refused(Box::new(report)));
    }
    let head = log.head_contents()?;
    let sealed_now = !head.claims().is_empty();
    let segment = if !sealed_now {
        match head.previous() {
            Some(last) => last.clone(),
            None => return Ok(Sealing::Empty),
        }
    } else if dry_run {
        let bytes = fs::read(log.head_path()).map_err(|source| Error::Io {
            context: format!("{}: reading", log.head_path().display()),
            source,
        })?;
        log.store().digest(&bytes)
    } else {
        match log.seal()? {
            Some(segment) => segment.digest().clone(),
            // The head was read a moment ago and had something to seal,
            // and a log has one writer.
            None => return Ok(Sealing::Empty),
        }
    };
    Ok(Sealing::Sealed(Seal {
        segment,
        algorithm: log.store().algorithm(),
        sealed_now,
        segments: report.segments + usize::from(sealed_now),
        claims: report.claims,
    }))
}

/// The segment a seal id names: the whole hash in hex, as `ossuary
/// seal` prints it and every other command shows a segment. The
/// algorithm is the archive's own.
///
/// # Errors
///
/// [`Error::SealId`] for anything but a whole hash in hex.
pub fn parse_id(archive: &Archive, given: &str) -> Result<Digest> {
    let algorithm = archive.log().store().algorithm();
    let refuse = |trouble: String| Error::SealId {
        given: given.to_string(),
        trouble,
    };
    if given.is_empty() || !given.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(refuse("not a hash in hex".to_string()));
    }
    let whole = algorithm.hash(b"").as_str().len();
    if given.len() != whole {
        return Err(refuse(format!(
            "{} hex characters where a whole {} hash has {whole}; the beginning of an id proves nothing, as a forged seal can be made to start the same way",
            given.len(),
            algorithm.name()
        )));
    }
    Digest::parse(given).map_err(|_| refuse("not a hash in hex".to_string()))
}

/// The sealed segments a seal covers, read back: every segment from the
/// seal back to the first, each one checked against its hash, and every
/// file their claims refer to. The walk stops at the first segment that
/// is gone, damaged or unreadable, so each of those lists holds one name
/// at most.
#[derive(Debug)]
pub struct Cover {
    /// The seal's segment.
    pub segment: Digest,
    /// Whether the claims store holds it. Without it, nothing else could
    /// be checked: it was removed, or the id belongs to another archive.
    pub held: bool,
    /// Sealed segments read back.
    pub segments: usize,
    /// Claims in them.
    pub claims: usize,
    /// The latest time among those claims: the seal covers the record
    /// up to it.
    pub last: Option<Timestamp>,
    /// Segments the walk names that the store does not hold.
    pub gone: Vec<String>,
    /// Segments whose bytes are no longer what their names say.
    pub damaged: Vec<String>,
    /// Segments that could not be read, or not read back as segments,
    /// with what stood in the way.
    pub unreadable: Vec<(String, String)>,
    /// Every segment the walk reached, held or not.
    reached: BTreeSet<Digest>,
    /// Every file the claims refer to, by name.
    referenced: BTreeSet<String>,
}

impl Cover {
    /// How many files the sealed claims refer to.
    #[must_use]
    pub fn files(&self) -> usize {
        self.referenced.len()
    }
}

/// Walk the segments a seal covers, from its segment back to the first.
///
/// # Errors
///
/// [`Error::Store`] when the claims store cannot be looked into at all;
/// what a single segment has to answer for lands in the [`Cover`].
pub fn cover(archive: &Archive, segment: &Digest) -> Result<Cover> {
    let log = archive.log();
    let store = log.store();
    let mut cover = Cover {
        segment: segment.clone(),
        held: store.contains(segment)?,
        segments: 0,
        claims: 0,
        last: None,
        gone: Vec::new(),
        damaged: Vec::new(),
        unreadable: Vec::new(),
        reached: BTreeSet::new(),
        referenced: BTreeSet::new(),
    };
    if !cover.held {
        return Ok(cover);
    }
    let mut next = Some(segment.clone());
    while let Some(digest) = next.take() {
        cover.reached.insert(digest.clone());
        let name = digest.as_str().to_string();
        let entry = store.find(&digest)?.and_then(|path| store.entry_at(&path));
        let Some(entry) = entry else {
            cover.gone.push(name);
            break;
        };
        match store.verify(&entry) {
            Ok(true) => {}
            Ok(false) => {
                cover.damaged.push(name);
                break;
            }
            Err(error) => {
                cover.unreadable.push((name, error.to_string()));
                break;
            }
        }
        let contents = match log.contents(&digest) {
            Ok(contents) => contents,
            Err(error) => {
                cover.unreadable.push((name, error.spelled()));
                break;
            }
        };
        cover.segments += 1;
        cover.claims += contents.claims().len();
        for claim in contents.claims() {
            reference(claim, &mut cover.referenced);
            if cover.last.as_ref().is_none_or(|last| claim.time() > last) {
                cover.last = Some(claim.time().clone());
            }
        }
        next = contents.previous().cloned();
    }
    cover.damaged.sort();
    cover.unreadable.sort();
    Ok(cover)
}

/// The files a seal covers, held against the archive.
#[derive(Debug, Default)]
pub struct Files {
    /// Whether every file was read whole and held against its name, or
    /// only looked for.
    pub read: bool,
    /// Files that neither `content/` nor `derived/` holds.
    pub gone: Vec<String>,
    /// Files whose bytes are no longer what their names say, in every
    /// store that holds them.
    pub damaged: Vec<String>,
    /// Files that could not be read, with what stood in the way.
    pub unreadable: Vec<(String, String)>,
}

/// Look for every file the sealed claims refer to, in `content/` first
/// and then in `derived/`, and with `read`, read each one whole and hold
/// it against its name. A file with a sound copy in either store is
/// held unchanged.
///
/// # Errors
///
/// [`Error::Store`] when a store cannot be looked into at all.
pub fn files(archive: &Archive, cover: &Cover, read: bool) -> Result<Files> {
    let mut files = Files {
        read,
        ..Files::default()
    };
    for name in &cover.referenced {
        // Every name here was parsed as a subject, so it parses as a
        // digest.
        let Ok(digest) = Digest::parse(name) else {
            continue;
        };
        let mut copies = Vec::new();
        for store in [archive.content(), archive.derived()] {
            let Some(entry) = store.find(&digest)?.and_then(|path| store.entry_at(&path)) else {
                continue;
            };
            let fixity = if read {
                Fixity::of(store, &entry)
            } else {
                Fixity::Sound
            };
            let sound = fixity == Fixity::Sound;
            copies.push(fixity);
            if sound {
                break;
            }
        }
        if copies.is_empty() {
            files.gone.push(name.clone());
        } else if !copies.contains(&Fixity::Sound) {
            match copies
                .into_iter()
                .find(|fixity| matches!(fixity, Fixity::Unreadable(_)))
            {
                Some(Fixity::Unreadable(error)) => files.unreadable.push((name.clone(), error)),
                _ => files.damaged.push(name.clone()),
            }
        }
    }
    Ok(files)
}

/// What the archive recorded after the seal, and whether its record
/// still leads to it.
#[derive(Debug)]
pub struct Since {
    /// Whether the walk back from the open head reaches the seal.
    pub continued: Continued,
    /// Sealed segments the walk passes before it reaches the seal.
    pub segments: usize,
    /// Claims in them and in the open head.
    pub claims: usize,
}

/// Whether the record goes on from a seal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Continued {
    /// The walk back from the open head reaches the seal.
    Yes,
    /// The open head is missing, while sealed segments are there.
    HeadLost,
    /// The open head cannot be read, with what stood in the way.
    HeadBroken(String),
    /// A segment recorded since the seal is missing or will not read:
    /// its name. The walk back from the open head ends there, and
    /// whether it would reach the seal is not known.
    Blocked(String),
    /// The walk back from the open head does not reach the seal: the
    /// record was cut back to an earlier point, or begun anew. Only a
    /// head rewritten by hand leaves this.
    No,
}

/// Walk back from the open head to the seal, through the segments
/// recorded since. A segment the seal covers other than the seal's own
/// lies behind it, and a walk that meets one has passed the seal by.
///
/// # Errors
///
/// [`Error::Store`] when the claims store cannot be looked into.
pub fn since(archive: &Archive, cover: &Cover) -> Result<Since> {
    let log = archive.log();
    let mut since = Since {
        continued: Continued::No,
        segments: 0,
        claims: 0,
    };
    if log.head_lost()? {
        since.continued = Continued::HeadLost;
        return Ok(since);
    }
    let head = match log.head_contents() {
        Ok(head) => head,
        Err(error) => {
            since.continued = Continued::HeadBroken(error.spelled());
            return Ok(since);
        }
    };
    since.claims = head.claims().len();
    let mut next = head.previous().cloned();
    while let Some(digest) = next.take() {
        if digest == cover.segment {
            since.continued = Continued::Yes;
            break;
        }
        if cover.reached.contains(&digest) {
            break;
        }
        let Ok(contents) = log.contents(&digest) else {
            since.continued = Continued::Blocked(digest.as_str().to_string());
            break;
        };
        since.segments += 1;
        since.claims += contents.claims().len();
        next = contents.previous().cloned();
    }
    Ok(since)
}

/// A seal checked whole: its segments, its files, and the record since.
#[derive(Debug)]
pub struct SealCheck {
    pub cover: Cover,
    pub files: Files,
    pub since: Since,
}

impl SealCheck {
    /// How many things are not as the seal recorded them.
    #[must_use]
    pub fn findings(&self) -> usize {
        usize::from(!self.cover.held)
            + self.cover.gone.len()
            + self.cover.damaged.len()
            + self.cover.unreadable.len()
            + self.files.gone.len()
            + self.files.damaged.len()
            + self.files.unreadable.len()
            + usize::from(self.since.continued != Continued::Yes)
    }

    /// Whether the archive still holds everything the seal covers, as
    /// far as it was checked: unchanged when the files were read, present
    /// when they were only looked for.
    #[must_use]
    pub fn holds(&self) -> bool {
        self.findings() == 0
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::claim::{Attribute, Claim, Run, Source, Subject, Timestamp};

    fn archive(dir: &TempDir) -> Archive {
        Archive::create(dir.path().join("archive"), Algorithm::Sha256).unwrap()
    }

    fn take(archive: &Archive, bytes: &[u8], at: &str) -> Digest {
        let (_, entry) = archive.content().add(bytes).unwrap();
        let claim = Claim::assert(
            Subject::parse(entry.digest().as_str()).unwrap(),
            Attribute::parse("file:size").unwrap(),
            json!(bytes.len()),
            Timestamp::parse(at).unwrap(),
            Source::parse("test").unwrap(),
            Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        )
        .unwrap();
        archive.log().append(&claim).unwrap();
        entry.digest().clone()
    }

    fn check(archive: &Archive, segment: &Digest, read: bool) -> SealCheck {
        let cover = cover(archive, segment).unwrap();
        SealCheck {
            files: files(archive, &cover, read).unwrap(),
            since: since(archive, &cover).unwrap(),
            cover,
        }
    }

    fn writable(path: &std::path::Path) {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        #[allow(
            clippy::permissions_set_readonly_false,
            reason = "the test plays the corruption"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn sealed(sealing: Sealing) -> Seal {
        match sealing {
            Sealing::Sealed(seal) => seal,
            other => panic!("not sealed: {other:?}"),
        }
    }

    #[test]
    fn an_empty_archive_has_nothing_to_seal() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);

        assert!(matches!(seal(&archive, false).unwrap(), Sealing::Empty));
    }

    #[test]
    fn the_open_head_is_sealed_and_its_segment_is_the_seal() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        archive.log().seal().unwrap().unwrap();
        take(&archive, b"second", "2026-09-02T00:00:00Z");

        let seal = sealed(seal(&archive, false).unwrap());

        assert!(seal.sealed_now);
        assert_eq!(seal.algorithm, Algorithm::Sha256);
        assert_eq!(seal.segments, 2);
        assert_eq!(seal.claims, 2);
        assert_eq!(
            archive.log().head_contents().unwrap().previous(),
            Some(&seal.segment),
            "the head now follows the seal"
        );
    }

    #[test]
    fn nothing_recorded_since_gives_the_same_seal_again() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let first = sealed(seal(&archive, false).unwrap());

        let again = sealed(seal(&archive, false).unwrap());

        assert_eq!(again.segment, first.segment);
        assert!(!again.sealed_now);
        assert_eq!(again.segments, 1);
    }

    #[test]
    fn a_dry_run_names_the_seal_and_writes_nothing() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");

        let rehearsed = sealed(seal(&archive, true).unwrap());

        assert!(rehearsed.sealed_now);
        assert_eq!(archive.log().head().unwrap().len(), 1, "nothing sealed");
        let made = sealed(seal(&archive, false).unwrap());
        assert_eq!(made.segment, rehearsed.segment);
    }

    #[test]
    fn a_lost_head_refuses_the_seal() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        archive.log().seal().unwrap().unwrap();
        fs::remove_file(archive.log().head_path()).unwrap();

        let Sealing::Refused(report) = seal(&archive, false).unwrap() else {
            panic!("sealed over a lost head");
        };
        assert!(report.head_lost);
        assert!(!archive.log().head_path().exists(), "nothing was written");
    }

    #[test]
    fn a_lost_segment_refuses_the_seal() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let first = archive.log().seal().unwrap().unwrap();
        take(&archive, b"second", "2026-09-02T00:00:00Z");
        archive.log().seal().unwrap().unwrap();
        let path = archive.log().store().find(first.digest()).unwrap().unwrap();
        fs::remove_file(path).unwrap();

        assert!(matches!(
            seal(&archive, false).unwrap(),
            Sealing::Refused(_)
        ));
    }

    #[test]
    fn an_id_is_read_as_the_whole_hash() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let hex = "ab".repeat(32);

        assert_eq!(parse_id(&archive, &hex).unwrap().as_str(), hex);
        assert_eq!(
            parse_id(&archive, &hex.to_uppercase()).unwrap().as_str(),
            hex
        );
    }

    #[test]
    fn an_id_that_is_not_whole_is_refused() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let hex = "ab".repeat(32);

        for given in [
            &hex[..12],
            "",
            "not hex",
            &format!("sha256:{hex}"),
            &format!("{hex}ab"),
        ] {
            assert!(
                matches!(parse_id(&archive, given), Err(Error::SealId { .. })),
                "{given}"
            );
        }
    }

    #[test]
    fn a_seal_holds_while_the_archive_is_unchanged() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        archive.log().seal().unwrap().unwrap();
        take(&archive, b"second", "2026-09-02T00:00:00Z");
        let seal = sealed(seal(&archive, false).unwrap());

        let checked = check(&archive, &seal.segment, true);

        assert!(checked.holds(), "{checked:?}");
        assert_eq!(checked.cover.segments, 2);
        assert_eq!(checked.cover.claims, 2);
        assert_eq!(checked.cover.files(), 2);
        assert_eq!(
            checked.cover.last,
            Some(Timestamp::parse("2026-09-02T00:00:00Z").unwrap())
        );
        assert_eq!(checked.since.continued, Continued::Yes);
        assert_eq!(checked.since.segments, 0);
    }

    #[test]
    fn what_is_recorded_after_a_seal_is_counted_and_not_covered() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let seal = sealed(seal(&archive, false).unwrap());
        take(&archive, b"second", "2026-09-02T00:00:00Z");
        archive.log().seal().unwrap().unwrap();
        take(&archive, b"third", "2026-09-03T00:00:00Z");

        let checked = check(&archive, &seal.segment, true);

        assert!(checked.holds());
        assert_eq!(checked.cover.files(), 1);
        assert_eq!(checked.since.segments, 1);
        assert_eq!(checked.since.claims, 2);
    }

    #[test]
    fn a_file_gone_or_damaged_breaks_the_seal() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let gone = take(&archive, b"gone", "2026-09-01T00:00:00Z");
        let damaged = take(&archive, b"damaged", "2026-09-01T00:00:00Z");
        let seal = sealed(seal(&archive, false).unwrap());
        let path = archive.content().find(&gone).unwrap().unwrap();
        fs::remove_file(path).unwrap();
        let path = archive.content().find(&damaged).unwrap().unwrap();
        writable(&path);
        fs::write(&path, b"changed").unwrap();

        let checked = check(&archive, &seal.segment, true);

        assert_eq!(checked.files.gone, vec![gone.as_str().to_string()]);
        assert_eq!(checked.files.damaged, vec![damaged.as_str().to_string()]);
        assert_eq!(checked.findings(), 2);

        let quick = check(&archive, &seal.segment, false);
        assert_eq!(
            quick.files.gone.len(),
            1,
            "a missing file shows without reading"
        );
        assert!(quick.files.damaged.is_empty(), "a changed one does not");
        assert!(!quick.files.read);
    }

    #[test]
    fn a_damaged_copy_with_a_sound_twin_is_held() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let twin = take(&archive, b"twin", "2026-09-01T00:00:00Z");
        archive.derived().add(b"twin").unwrap();
        let seal = sealed(seal(&archive, false).unwrap());
        let path = archive.content().find(&twin).unwrap().unwrap();
        writable(&path);
        fs::write(&path, b"changed").unwrap();

        assert!(check(&archive, &seal.segment, true).holds());
    }

    #[test]
    fn a_segment_gone_or_damaged_breaks_the_seal() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let first = archive.log().seal().unwrap().unwrap();
        take(&archive, b"second", "2026-09-02T00:00:00Z");
        let second = archive.log().seal().unwrap().unwrap();
        take(&archive, b"third", "2026-09-03T00:00:00Z");
        let seal = sealed(seal(&archive, false).unwrap());
        let store = archive.log().store();
        fs::remove_file(store.find(first.digest()).unwrap().unwrap()).unwrap();
        let path = store.find(second.digest()).unwrap().unwrap();
        writable(&path);
        fs::write(&path, b"changed").unwrap();

        let checked = check(&archive, &seal.segment, false);

        assert_eq!(
            checked.cover.damaged,
            vec![second.digest().as_str().to_string()]
        );
        assert!(
            checked.cover.gone.is_empty(),
            "behind a damaged segment the walk cannot go on"
        );
        assert!(!checked.holds());
    }

    #[test]
    fn a_record_cut_back_behind_the_seal_is_a_finding() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let first = archive.log().seal().unwrap().unwrap();
        take(&archive, b"second", "2026-09-02T00:00:00Z");
        let seal = sealed(seal(&archive, false).unwrap());
        // The head rewritten by hand to name an earlier segment.
        fs::write(
            archive.log().head_path(),
            crate::log::head_after(first.digest()),
        )
        .unwrap();

        let checked = check(&archive, &seal.segment, true);

        assert_eq!(checked.since.continued, Continued::No);
        assert_eq!(checked.findings(), 1, "the seal itself is intact");
    }

    #[test]
    fn a_segment_lost_since_the_seal_is_named_as_such() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let seal = sealed(seal(&archive, false).unwrap());
        take(&archive, b"second", "2026-09-02T00:00:00Z");
        let since = archive.log().seal().unwrap().unwrap();
        take(&archive, b"third", "2026-09-03T00:00:00Z");
        archive.log().seal().unwrap().unwrap();
        let path = archive.log().store().find(since.digest()).unwrap().unwrap();
        fs::remove_file(path).unwrap();

        let checked = check(&archive, &seal.segment, true);

        assert_eq!(
            checked.since.continued,
            Continued::Blocked(since.digest().as_str().to_string()),
            "a lost segment, not a record cut back"
        );
        assert!(!checked.holds());
    }

    #[test]
    fn a_lost_head_is_a_finding_for_the_seal() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let seal = sealed(seal(&archive, false).unwrap());
        fs::remove_file(archive.log().head_path()).unwrap();

        let checked = check(&archive, &seal.segment, true);

        assert_eq!(checked.since.continued, Continued::HeadLost);
        assert!(!checked.holds());
    }

    #[test]
    fn a_seal_the_archive_does_not_hold_cannot_be_checked() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        sealed(seal(&archive, false).unwrap());
        let elsewhere = Digest::parse(&"ab".repeat(32)).unwrap();

        let cover = cover(&archive, &elsewhere).unwrap();

        assert!(!cover.held);
        assert_eq!(cover.segments, 0);
    }
}
