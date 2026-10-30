//! What a copy of the archive lacks, worked out from the log alone.
//!
//! A copy whose open segment names segment `behind` holds every segment
//! from `behind` back to the first and every file their claims refer to:
//! that is what a seal covers. What it lacks to hold a later point
//! `ahead` as well follows from this archive's log without looking at
//! the copy: the segments from `ahead` back to `behind`, and the files
//! their claims refer to that no claim behind `behind` refers to. The
//! second subtraction is needed because `extract` and `annotate` write
//! new claims about old files.

use std::collections::BTreeSet;

use immure::Digest;

use crate::audit::reference;
use crate::claim::Claim;
use crate::error::{Error, Result};
use crate::log::Log;

/// What a copy that holds the record up to one point lacks to hold it
/// up to another.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Delta {
    /// The segments the copy lacks, from `ahead` back.
    pub segments: Vec<Digest>,
    /// The files the copy lacks, by name. Whether `content/` or
    /// `derived/` holds them is not recorded in the log.
    pub files: BTreeSet<Digest>,
    /// Whether the walk back from `ahead` reaches `behind`. When it does
    /// not, the copy holds a point this record does not lead to, and the
    /// delta means nothing for it.
    pub follows: bool,
}

/// What a copy that holds everything behind `behind` lacks to hold
/// everything behind `ahead` and the claims in `open`.
///
/// `behind` is `None` for a copy that holds nothing yet. `ahead` is
/// usually a seal; a dry run that has not sealed the open segment passes
/// the segment the open segment names, and its claims as `open`.
///
/// A segment behind `behind` that this archive no longer holds ends the
/// walk from `behind` there: the copy holds it, and only the files the
/// claims before it refer to cannot be subtracted, so they are listed
/// again.
///
/// # Errors
///
/// [`Error::SegmentMissing`] for a segment ahead of `behind` that the
/// claims store does not hold, and everything [`Log::contents`] can
/// answer for a segment that does not read.
pub fn delta(
    log: &Log,
    behind: Option<&Digest>,
    ahead: Option<&Digest>,
    open: &[Claim],
) -> Result<Delta> {
    let held = match behind {
        Some(segment) => walk(log, Some(segment), None)?,
        None => Walk::default(),
    };
    let lacked = walk(log, ahead, behind)?;
    if let Some(lost) = &lacked.missing {
        return Err(Error::SegmentMissing(lost.to_string()));
    }
    let mut refs = lacked.refs;
    for claim in open {
        reference(claim, &mut refs);
    }
    // Every name here was parsed as a subject, so it parses as a digest.
    let files = refs
        .difference(&held.refs)
        .filter_map(|name| Digest::parse(name).ok())
        .collect();
    Ok(Delta {
        segments: lacked.segments,
        files,
        follows: behind.is_none() || lacked.met,
    })
}

/// One walk back along the chain.
#[derive(Default)]
struct Walk {
    /// Segments read, in the order met.
    segments: Vec<Digest>,
    /// The name the walk stopped at because the store does not hold it.
    missing: Option<Digest>,
    /// Files the claims of the segments read refer to.
    refs: BTreeSet<String>,
    /// Whether the walk met `stop`.
    met: bool,
}

/// Walk back from `from` to the first segment, or to `stop`, which is
/// not read.
fn walk(log: &Log, from: Option<&Digest>, stop: Option<&Digest>) -> Result<Walk> {
    let mut walk = Walk::default();
    let mut next = from.cloned();
    while let Some(digest) = next.take() {
        if Some(&digest) == stop {
            walk.met = true;
            break;
        }
        let contents = match log.contents(&digest) {
            Ok(contents) => contents,
            Err(Error::SegmentMissing(_)) => {
                walk.missing = Some(digest);
                break;
            }
            Err(error) => return Err(error),
        };
        for claim in contents.claims() {
            reference(claim, &mut walk.refs);
        }
        next = contents.previous().cloned();
        walk.segments.push(digest);
    }
    Ok(walk)
}

#[cfg(test)]
mod tests {
    use immure::Algorithm;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::archive::Archive;
    use crate::claim::{Attribute, Run, Source, Subject, Timestamp};
    use crate::log::head_after;

    fn archive(dir: &TempDir) -> Archive {
        Archive::create(dir.path().join("archive"), Algorithm::Sha256).unwrap()
    }

    fn claim(subject: &Digest, attribute: &str, value: serde_json::Value, at: &str) -> Claim {
        Claim::assert(
            Subject::parse(subject.as_str()).unwrap(),
            Attribute::parse(attribute).unwrap(),
            value,
            Timestamp::parse(at).unwrap(),
            Source::parse("test").unwrap(),
            Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        )
        .unwrap()
    }

    /// Store `bytes` and record their size: one file, one claim.
    fn take(archive: &Archive, bytes: &[u8], at: &str) -> Digest {
        let (_, entry) = archive.content().add(bytes).unwrap();
        let digest = entry.digest().clone();
        let size = claim(&digest, "file:size", json!(bytes.len()), at);
        archive.log().append(&size).unwrap();
        digest
    }

    fn seal(archive: &Archive) -> Digest {
        archive.log().seal().unwrap().unwrap().digest().clone()
    }

    fn files(names: &[&Digest]) -> BTreeSet<Digest> {
        names.iter().map(|&name| name.clone()).collect()
    }

    #[test]
    fn a_copy_that_holds_nothing_lacks_everything() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let first = take(&archive, b"first", "2026-09-01T00:00:00Z");
        let one = seal(&archive);
        let second = take(&archive, b"second", "2026-09-02T00:00:00Z");
        let two = seal(&archive);

        let delta = delta(archive.log(), None, Some(&two), &[]).unwrap();

        assert_eq!(delta.segments, [two, one]);
        assert_eq!(delta.files, files(&[&first, &second]));
        assert!(delta.follows);
    }

    #[test]
    fn a_copy_at_an_earlier_seal_lacks_only_what_came_after() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let first = take(&archive, b"first", "2026-09-01T00:00:00Z");
        let one = seal(&archive);
        let second = take(&archive, b"second", "2026-09-02T00:00:00Z");
        let tag = claim(&first, "user:tag", json!("holiday"), "2026-09-02T00:00:01Z");
        archive.log().append(&tag).unwrap();
        let two = seal(&archive);

        let delta = delta(archive.log(), Some(&one), Some(&two), &[]).unwrap();

        assert_eq!(delta.segments, [two]);
        assert_eq!(
            delta.files,
            files(&[&second]),
            "the tag is about a file the copy holds already"
        );
        assert!(delta.follows);
    }

    #[test]
    fn a_copy_at_the_seal_itself_lacks_nothing() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let one = seal(&archive);

        let delta = delta(archive.log(), Some(&one), Some(&one), &[]).unwrap();

        assert_eq!(
            delta,
            Delta {
                follows: true,
                ..Delta::default()
            }
        );
    }

    #[test]
    fn the_claims_of_an_unsealed_head_count_as_well() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let one = seal(&archive);
        let second = take(&archive, b"second", "2026-09-02T00:00:00Z");
        let head = archive.log().head_contents().unwrap();

        let delta = delta(archive.log(), Some(&one), head.previous(), head.claims()).unwrap();

        assert!(delta.segments.is_empty());
        assert_eq!(delta.files, files(&[&second]));
        assert!(delta.follows);
    }

    #[test]
    fn a_point_the_record_does_not_lead_to_is_not_followed() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let one = seal(&archive);
        let elsewhere = archive.log().store().digest(b"another archive's segment");

        let delta = delta(archive.log(), Some(&elsewhere), Some(&one), &[]).unwrap();

        assert!(!delta.follows);
    }

    #[test]
    fn a_lost_segment_ahead_of_the_copy_is_an_error() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let one = seal(&archive);
        take(&archive, b"second", "2026-09-02T00:00:00Z");
        let two = seal(&archive);
        let path = archive.log().store().find(&one).unwrap().unwrap();
        std::fs::remove_file(path).unwrap();

        let error = delta(archive.log(), None, Some(&two), &[]).unwrap_err();

        assert!(matches!(error, Error::SegmentMissing(name) if name == one.as_str()));
    }

    #[test]
    fn a_segment_the_copy_holds_and_this_archive_lost_is_passed_over() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let first = take(&archive, b"first", "2026-09-01T00:00:00Z");
        let one = seal(&archive);
        take(&archive, b"second", "2026-09-02T00:00:00Z");
        let two = seal(&archive);
        // The copy holds the record up to `two`; here `one` is lost
        // after that, and a later claim speaks of its file again.
        let path = archive.log().store().find(&one).unwrap().unwrap();
        std::fs::remove_file(path).unwrap();
        let tag = claim(&first, "user:tag", json!("holiday"), "2026-09-03T00:00:00Z");
        archive.log().append(&tag).unwrap();
        let three = seal(&archive);

        let delta = delta(archive.log(), Some(&two), Some(&three), &[]).unwrap();

        assert_eq!(delta.segments, [three]);
        assert_eq!(
            delta.files,
            files(&[&first]),
            "the walk from the copy's seal cannot get past the lost segment, so its file is listed again"
        );
        assert!(delta.follows);
    }

    #[test]
    fn the_head_after_a_seal_is_what_sealing_leaves() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let one = seal(&archive);

        let head = std::fs::read_to_string(archive.log().head_path()).unwrap();

        assert_eq!(head, head_after(&one));
        let contents = crate::log::Contents::parse(&head).unwrap();
        assert_eq!(contents.previous(), Some(&one));
        assert!(contents.claims().is_empty());
    }
}
