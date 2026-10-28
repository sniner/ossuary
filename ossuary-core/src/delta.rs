//! What a copy of the archive lacks, worked out from the log alone.
//!
//! A copy whose open segment names segment `behind` holds every segment
//! the walk from `behind` reaches and every file their claims refer to:
//! that is what a seal covers. What it lacks to hold a later point
//! `ahead` as well follows from this archive's log without looking at
//! the copy: the segments the walk from `ahead` reaches before it meets
//! one the walk from `behind` reaches, and the files their claims refer
//! to that no claim behind `behind` refers to. The second subtraction is
//! needed because `extract` and `annotate` write new claims about old
//! files.
//!
//! The walk from `ahead` stops at every segment behind `behind`, not at
//! `behind` alone: a line merged in can hold segments older than
//! `behind` that the walk from `behind` does not reach.

use std::collections::BTreeSet;

use immure::Digest;

use crate::audit::reference;
use crate::claim::Claim;
use crate::error::{Error, Result};
use crate::log::{Log, Mend};

/// What a copy that holds the record up to one point lacks to hold it
/// up to another.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Delta {
    /// The segments the copy lacks, in the order the walk met them.
    pub segments: Vec<Digest>,
    /// The files the copy lacks, by name. Whether `content/` or
    /// `derived/` holds them is not recorded in the log.
    pub files: BTreeSet<Digest>,
    /// Whether the walk from `ahead` reaches `behind`. When it does not,
    /// the copy holds a point this record does not lead to, and the
    /// delta means nothing for it.
    pub follows: bool,
}

/// What a copy that holds everything behind `behind` lacks to hold
/// everything behind `ahead` and the claims in `open`.
///
/// `behind` is `None` for a copy that holds nothing yet. `ahead` is
/// usually one segment, a seal; a dry run that has not sealed the open
/// segment passes the segments the open segment names, and its claims
/// as `open`.
///
/// A segment that a mend names as replaced was lost before and is not
/// lacked. A segment behind `behind` that this archive no longer holds
/// is passed over: the copy holds it, and only the files its claims
/// refer to cannot be subtracted, so they are listed again.
///
/// # Errors
///
/// [`Error::SegmentMissing`] for a segment ahead of `behind` that the
/// claims store does not hold and no mend replaces, and everything
/// [`Log::contents`] can answer for a segment that does not read.
pub fn delta(
    log: &Log,
    behind: Option<&Digest>,
    ahead: &[Digest],
    open: &[Claim],
) -> Result<Delta> {
    let held = match behind {
        Some(segment) => walk(log, std::slice::from_ref(segment), &BTreeSet::new())?,
        None => Walk::default(),
    };
    let lacked = walk(log, ahead, &held.reached)?;
    let replaced: BTreeSet<&Digest> = held.replaced.union(&lacked.replaced).collect();
    if let Some(lost) = lacked.missing.iter().find(|name| !replaced.contains(name)) {
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
        follows: behind.is_none_or(|segment| lacked.met.contains(segment)),
    })
}

/// One walk over `previous`.
#[derive(Default)]
struct Walk {
    /// Segments reached that the store holds, in the order met.
    segments: Vec<Digest>,
    /// Every name reached, held or not.
    reached: BTreeSet<Digest>,
    /// Names reached that the store does not hold.
    missing: BTreeSet<Digest>,
    /// Names that mends reached by the walk replace.
    replaced: BTreeSet<Digest>,
    /// Files the claims of the segments reached refer to.
    refs: BTreeSet<String>,
    /// Names of `stop` the walk met.
    met: BTreeSet<Digest>,
}

/// Walk back from `from` over every name in every `previous`, without
/// going past a name in `stop`.
fn walk(log: &Log, from: &[Digest], stop: &BTreeSet<Digest>) -> Result<Walk> {
    let mut walk = Walk::default();
    let mut pending = from.to_vec();
    while let Some(digest) = pending.pop() {
        if stop.contains(&digest) {
            walk.met.insert(digest);
            continue;
        }
        if !walk.reached.insert(digest.clone()) {
            continue;
        }
        let contents = match log.contents(&digest) {
            Ok(contents) => contents,
            Err(Error::SegmentMissing(_)) => {
                walk.missing.insert(digest);
                continue;
            }
            Err(error) => return Err(error),
        };
        for claim in contents.claims() {
            reference(claim, &mut walk.refs);
        }
        if let Some(replaces) = contents.mend().and_then(Mend::replaces) {
            walk.replaced.insert(replaces.clone());
        }
        pending.extend(contents.previous().iter().cloned());
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

        let delta = delta(archive.log(), None, std::slice::from_ref(&two), &[]).unwrap();

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

        let delta = delta(archive.log(), Some(&one), std::slice::from_ref(&two), &[]).unwrap();

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

        let delta = delta(archive.log(), Some(&one), std::slice::from_ref(&one), &[]).unwrap();

        assert_eq!(
            delta,
            Delta {
                follows: true,
                ..Delta::default()
            }
        );
    }

    #[test]
    fn a_merged_line_older_than_the_copy_is_lacked() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-05T00:00:00Z");
        let one = seal(&archive);
        // A second line whose only segment is older than the copy's seal.
        let (_, entry) = archive.content().add(b"theirs").unwrap();
        let theirs = entry.digest().clone();
        let mut text = r#"{"ossuary-segment":1}"#.to_string();
        text.push('\n');
        text.push_str(&claim(&theirs, "file:size", json!(6), "2026-09-01T00:00:00Z").to_line());
        text.push('\n');
        let (_, segment) = archive.log().store().add(text.as_bytes()).unwrap();
        let line = segment.digest().clone();
        archive
            .log()
            .head_follows(&[one.clone(), line.clone()])
            .unwrap();
        let two = seal(&archive);

        let delta = delta(archive.log(), Some(&one), std::slice::from_ref(&two), &[]).unwrap();

        assert_eq!(delta.segments, [two, line]);
        assert_eq!(delta.files, files(&[&theirs]));
        assert!(delta.follows);
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

        let delta = delta(
            archive.log(),
            Some(&elsewhere),
            std::slice::from_ref(&one),
            &[],
        )
        .unwrap();

        assert!(!delta.follows);
    }

    #[test]
    fn a_lost_segment_that_a_mend_replaces_is_not_lacked() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let one = seal(&archive);
        take(&archive, b"second", "2026-09-02T00:00:00Z");
        let two = seal(&archive);
        take(&archive, b"third", "2026-09-03T00:00:00Z");
        let three = seal(&archive);
        let path = archive.log().store().find(&two).unwrap().unwrap();
        std::fs::remove_file(path).unwrap();
        let mend = archive
            .log()
            .mend(&one, &Mend::new(Some(three.clone()), Some(two.clone())))
            .unwrap();
        archive
            .log()
            .head_follows(&[three.clone(), mend.digest().clone()])
            .unwrap();
        let four = seal(&archive);

        let delta = delta(archive.log(), None, std::slice::from_ref(&four), &[]).unwrap();

        assert!(!delta.segments.contains(&two));
        assert!(delta.segments.contains(mend.digest()));
        assert_eq!(delta.segments.len(), 4, "four, three, the mend and one");
    }

    #[test]
    fn a_lost_segment_without_a_mend_is_an_error() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let one = seal(&archive);
        take(&archive, b"second", "2026-09-02T00:00:00Z");
        let two = seal(&archive);
        let path = archive.log().store().find(&one).unwrap().unwrap();
        std::fs::remove_file(path).unwrap();

        let error = delta(archive.log(), None, std::slice::from_ref(&two), &[]).unwrap_err();

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
        take(&archive, b"third", "2026-09-03T00:00:00Z");
        let three = seal(&archive);
        // The copy holds the record up to `three`; here `two` is lost
        // after that and mended.
        let path = archive.log().store().find(&two).unwrap().unwrap();
        std::fs::remove_file(path).unwrap();
        let mend = archive
            .log()
            .mend(&one, &Mend::new(Some(three.clone()), Some(two.clone())))
            .unwrap();
        archive
            .log()
            .head_follows(&[three.clone(), mend.digest().clone()])
            .unwrap();
        let four = seal(&archive);

        let delta = delta(
            archive.log(),
            Some(&three),
            std::slice::from_ref(&four),
            &[],
        )
        .unwrap();

        assert_eq!(
            delta.segments,
            [four, mend.digest().clone(), one],
            "the walk from the copy's seal cannot get past the lost segment, so `one` is listed again"
        );
        assert_eq!(delta.files, files(&[&first]));
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
        assert_eq!(contents.previous(), std::slice::from_ref(&one));
        assert!(contents.claims().is_empty());
    }
}
