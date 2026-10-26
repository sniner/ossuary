//! Mending: a break in the chain closed by a segment that names its two
//! ends, and nothing rewritten for it.
//!
//! The audit says where the chain is broken ([`Break`]); this closes one
//! such break the way `docs/format.md` allows — a mend stored in front of
//! the segment after the break, or in front of the open head when the
//! break stands there. What was lost stays lost, and the record keeps
//! saying so: the segment after the break still names what it named, and
//! the mend says what it stands in for when that was known.
//!
//! Either way the open head names the mend. Nothing sealed can be made to
//! name it, and every segment must be reachable from the head, the
//! segments before the break through the mend.

use immure::Digest;

use crate::audit::{Break, Cause};
use crate::error::Result;
use crate::log::{Log, Mend, Segment};

/// Close one break: store the mend, and make the open head name it.
///
/// For a break in front of the head, the mend takes the place of what
/// the head named; for one in front of a sealed segment, the head names
/// the mend after the segments it names already. `None` when the break
/// cannot be mended (see [`Break::mendable`]): a mend stands in for what
/// is gone, not for what is held and damaged, and only between ends that
/// are surely the break's. Done twice, the same break yields the same
/// mend, so a run interrupted between storing the mend and turning the
/// head to it is finished by running again.
///
/// # Errors
///
/// [`Error::HeadLost`](crate::Error::HeadLost) for a break in front of a
/// sealed segment while the head is lost: the break in front of the head
/// is mended first, and nothing is stored before. Otherwise
/// [`Error::Store`](crate::Error::Store) storing the mend, and
/// everything [`Log::head_follows`] can answer. A digest the audit
/// reported that will not parse is a bug, not an archive state, and is
/// reported as [`Error::SegmentHeader`](crate::Error::SegmentHeader).
pub fn mend(log: &Log, brk: &Break) -> Result<Option<Segment>> {
    if !brk.mendable() {
        return Ok(None);
    }
    let digest =
        |hex: &str| Digest::parse(hex).map_err(|_| crate::Error::SegmentHeader(hex.to_string()));
    let previous = digest(&brk.after)?;
    let before = brk.before.as_deref().map(digest).transpose()?;
    let replaces = match &brk.cause {
        Cause::SegmentLost(name) => Some(digest(name)?),
        Cause::HeadLost | Cause::SegmentUnreadable(_) => None,
    };
    // A head begun here would name this mend alone, and the break in
    // front of the head would no longer show.
    if before.is_some() {
        log.writable()?;
    }
    let named = log.head_contents()?.previous().to_vec();
    let segment = log.mend(&previous, &Mend::new(before.clone(), replaces))?;
    let names = if before.is_some() {
        if named.contains(segment.digest()) {
            return Ok(Some(segment));
        }
        let mut names = named;
        names.push(segment.digest().clone());
        names
    } else {
        // The head's first name is the end the break stands after, lost
        // or never written; a line merged in after it stays named.
        std::iter::once(segment.digest().clone())
            .chain(named.into_iter().skip(1))
            .collect()
    };
    log.head_follows(&names)?;
    Ok(Some(segment))
}

#[cfg(test)]
mod tests {
    use immure::Algorithm;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::Archive;
    use crate::audit::{Audit, audit_log, audit_store};
    use crate::claim::{Attribute, Claim, Source, Subject, Timestamp};

    fn archive(dir: &TempDir) -> Archive {
        Archive::create(dir.path().join("archive"), Algorithm::Sha256).unwrap()
    }

    fn held(archive: &Archive, bytes: &[u8], at: &str) -> Claim {
        let (_, entry) = archive.content().add(bytes).unwrap();
        Claim::assert(
            Subject::parse(entry.digest().as_str()).unwrap(),
            Attribute::parse("file:size").unwrap(),
            json!(bytes.len()),
            Timestamp::parse(at).unwrap(),
            Source::parse("test").unwrap(),
            crate::claim::Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        )
        .unwrap()
    }

    fn take(archive: &Archive, bytes: &[u8], at: &str) {
        archive.log().append(&held(archive, bytes, at)).unwrap();
    }

    /// A first segment of a line of its own, as a merge brings it in.
    fn line(archive: &Archive, bytes: &[u8], at: &str) -> Digest {
        let text = format!(
            "{{\"ossuary-segment\":1}}\n{}\n",
            held(archive, bytes, at).to_line()
        );
        let (_, entry) = archive.log().store().add(text.as_bytes()).unwrap();
        entry.digest().clone()
    }

    fn lose(archive: &Archive, segment: &Digest) {
        let path = archive.log().store().find(segment).unwrap().unwrap();
        std::fs::remove_file(path).unwrap();
    }

    fn run(archive: &Archive) -> Audit {
        Audit::assemble(
            audit_store(archive.content()).unwrap(),
            audit_store(archive.derived()).unwrap(),
            audit_log(archive.log()).unwrap(),
        )
    }

    /// The head lost and begun anew naming nothing, which `append` refuses
    /// to do: the audit still has to read an archive in that state.
    fn begin_head_anew(archive: &Archive) {
        std::fs::write(
            archive.root().join("head.jsonl"),
            "{\"ossuary-segment\":1}\n",
        )
        .unwrap();
    }

    #[test]
    fn a_head_sealed_anew_is_mended_in_front_of_its_segment() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let first = archive.log().seal().unwrap().unwrap();
        begin_head_anew(&archive);
        take(&archive, b"anew", "2026-09-02T00:00:00Z");
        let anew = archive.log().seal().unwrap().unwrap();
        let before = run(&archive);
        assert_eq!(before.log.breaks.len(), 1);

        let mended = mend(archive.log(), &before.log.breaks[0])
            .unwrap()
            .expect("mendable");

        let after = run(&archive);
        assert!(after.is_sound());
        assert_eq!(after.log.chains.len(), 1);
        assert_eq!(after.log.mended.len(), 1);
        assert_eq!(after.log.mended[0].mend, mended.digest().as_str());
        assert_eq!(
            after.log.mended[0].before.as_deref(),
            Some(anew.digest().as_str())
        );
        assert_eq!(
            archive.log().head_contents().unwrap().previous(),
            [anew.digest().clone(), mended.digest().clone()],
            "the head names the mend after the segment it follows"
        );
        assert_eq!(
            archive.log().contents(anew.digest()).unwrap().previous(),
            &[] as &[Digest],
            "the segment after the break is not touched"
        );
        assert_eq!(
            archive.log().contents(mended.digest()).unwrap().previous(),
            std::slice::from_ref(first.digest())
        );
    }

    #[test]
    fn a_break_in_front_of_the_head_turns_the_head_to_the_mend() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let first = archive.log().seal().unwrap().unwrap();
        begin_head_anew(&archive);
        take(&archive, b"anew", "2026-09-02T00:00:00Z");
        let before = run(&archive);
        assert_eq!(before.log.breaks[0].before, None);

        let mended = mend(archive.log(), &before.log.breaks[0])
            .unwrap()
            .expect("mendable");

        let head = archive.log().head_contents().unwrap();
        assert_eq!(head.previous(), std::slice::from_ref(mended.digest()));
        assert_eq!(head.claims().len(), 1, "with its claim kept");
        let contents = archive.log().contents(mended.digest()).unwrap();
        assert_eq!(contents.previous(), std::slice::from_ref(first.digest()));
        assert_eq!(contents.mend().unwrap().before(), None);
        assert!(run(&archive).is_sound());
    }

    #[test]
    fn a_lost_head_is_begun_anew_by_its_mend() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let first = archive.log().seal().unwrap().unwrap();
        std::fs::remove_file(archive.root().join("head.jsonl")).unwrap();
        let before = run(&archive);
        assert_eq!(before.log.breaks.len(), 1);
        assert_eq!(before.log.breaks[0].before, None);

        let mended = mend(archive.log(), &before.log.breaks[0])
            .unwrap()
            .expect("mendable");

        assert!(!archive.log().head_lost().unwrap());
        assert_eq!(
            archive.log().head_contents().unwrap().previous(),
            std::slice::from_ref(mended.digest())
        );
        assert_eq!(
            archive.log().contents(mended.digest()).unwrap().previous(),
            std::slice::from_ref(first.digest())
        );
        take(&archive, b"after the mend", "2026-09-02T00:00:00Z");
        assert!(run(&archive).is_sound());
    }

    #[test]
    fn a_lost_segment_is_named_by_the_mend_that_stands_in_for_it() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        archive.log().seal().unwrap().unwrap();
        take(&archive, b"second", "2026-09-02T00:00:00Z");
        let second = archive.log().seal().unwrap().unwrap();
        take(&archive, b"third", "2026-09-03T00:00:00Z");
        archive.log().seal().unwrap().unwrap();
        let path = archive
            .log()
            .store()
            .find(second.digest())
            .unwrap()
            .unwrap();
        std::fs::remove_file(path).unwrap();
        let before = run(&archive);
        assert_eq!(before.log.breaks.len(), 1);

        let mended = mend(archive.log(), &before.log.breaks[0])
            .unwrap()
            .expect("mendable");

        let contents = archive.log().contents(mended.digest()).unwrap();
        assert_eq!(
            contents.mend().unwrap().replaces(),
            Some(second.digest()),
            "what was lost is on the record by name"
        );
        let after = run(&archive);
        assert!(after.is_sound());
        assert!(after.log.predecessor_missing.is_empty());
    }

    #[test]
    fn a_mend_in_front_of_a_sealed_segment_is_named_once() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        archive.log().seal().unwrap().unwrap();
        take(&archive, b"second", "2026-09-02T00:00:00Z");
        let second = archive.log().seal().unwrap().unwrap();
        take(&archive, b"third", "2026-09-03T00:00:00Z");
        let third = archive.log().seal().unwrap().unwrap();
        lose(&archive, second.digest());
        let brk = run(&archive).log.breaks.remove(0);

        let once = mend(archive.log(), &brk).unwrap().unwrap();
        let twice = mend(archive.log(), &brk).unwrap().unwrap();

        assert_eq!(once, twice);
        assert_eq!(
            archive.log().head_contents().unwrap().previous(),
            [third.digest().clone(), once.digest().clone()]
        );
    }

    #[test]
    fn a_mend_in_front_of_a_sealed_segment_waits_for_a_lost_head() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        archive.log().seal().unwrap().unwrap();
        take(&archive, b"second", "2026-09-02T00:00:00Z");
        let second = archive.log().seal().unwrap().unwrap();
        take(&archive, b"third", "2026-09-03T00:00:00Z");
        archive.log().seal().unwrap().unwrap();
        lose(&archive, second.digest());
        std::fs::remove_file(archive.root().join("head.jsonl")).unwrap();
        let breaks = run(&archive).log.breaks;
        assert_eq!(breaks.len(), 2);
        assert!(breaks[0].before.is_some());

        assert!(matches!(
            mend(archive.log(), &breaks[0]),
            Err(crate::Error::HeadLost(_))
        ));
        assert_eq!(run(&archive).log.segments, 2, "nothing was stored");

        mend(archive.log(), &breaks[1]).unwrap().unwrap();
        let named = mend(archive.log(), &breaks[0]).unwrap().unwrap();
        assert_eq!(
            archive.log().head_contents().unwrap().previous()[1],
            *named.digest()
        );
        assert!(run(&archive).is_sound());
    }

    #[test]
    fn a_line_merged_in_stays_named_when_the_head_is_mended() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        archive.log().seal().unwrap().unwrap();
        let merged = line(&archive, b"merged", "2026-08-01T00:00:00Z");
        take(&archive, b"second", "2026-09-02T00:00:00Z");
        let second = archive.log().seal().unwrap().unwrap();
        archive
            .log()
            .head_follows(&[second.digest().clone(), merged.clone()])
            .unwrap();
        lose(&archive, second.digest());
        let brk = run(&archive).log.breaks.remove(0);
        assert_eq!(brk.before, None);

        let mended = mend(archive.log(), &brk).unwrap().unwrap();

        assert_eq!(
            archive.log().head_contents().unwrap().previous(),
            [mended.digest().clone(), merged]
        );
        assert!(run(&archive).is_sound());
    }

    #[test]
    fn the_same_break_mended_twice_is_one_mend() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        archive.log().seal().unwrap().unwrap();
        begin_head_anew(&archive);
        take(&archive, b"anew", "2026-09-02T00:00:00Z");
        let brk = run(&archive).log.breaks.remove(0);

        let once = mend(archive.log(), &brk).unwrap().unwrap();
        let twice = mend(archive.log(), &brk).unwrap().unwrap();

        assert_eq!(once, twice);
        assert_eq!(run(&archive).log.mended.len(), 1);
    }

    #[test]
    fn a_break_behind_damage_is_left_alone() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        archive.log().seal().unwrap().unwrap();
        take(&archive, b"second", "2026-09-02T00:00:00Z");
        let second = archive.log().seal().unwrap().unwrap();
        take(&archive, b"third", "2026-09-03T00:00:00Z");
        archive.log().seal().unwrap().unwrap();
        let path = archive
            .log()
            .store()
            .find(second.digest())
            .unwrap()
            .unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        #[allow(
            clippy::permissions_set_readonly_false,
            reason = "the test plays the corruption"
        )]
        permissions.set_readonly(false);
        std::fs::set_permissions(&path, permissions).unwrap();
        std::fs::write(&path, b"garbage").unwrap();
        let brk = run(&archive).log.breaks.remove(0);

        assert_eq!(mend(archive.log(), &brk).unwrap(), None);
        assert_eq!(run(&archive).log.segments, 3, "nothing was stored");
    }
}
