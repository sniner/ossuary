//! An archive of ossuary 0.14 or earlier brought into the current form:
//! the format fix that runs once per archive.
//!
//! Until 0.14 a segment header named the segments sealed before it as a
//! list, so that a segment could follow several: the last segments of two
//! archives after a merge, or a mend that stood in for a lost segment.
//! Such an archive was read in the order of its segments' first claims,
//! then their digests, not along the chain. This fix keeps that reading:
//! it links the segments with claims in that order, each naming the one
//! before it by one name, and drops the segments without claims, the
//! mends and the segments that joined merged lines. The open segment
//! names the last.
//!
//! A segment that a header names and the store does not hold was lost.
//! A mend that names it in `replaces` recorded that already; any other
//! loss, and a lost open segment, the caller accepts or refuses.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use ossuary_core::Archive;

use crate::fixes::found::Found;
use crate::rewrite::{Rewrite, Segment, Words, first_time};

/// The archive in the current form, and what was found on the way.
///
/// # Errors
///
/// Whatever [`Rewrite::plan`] can answer.
pub fn plan(archive: &Archive) -> Result<(Rewrite, Found)> {
    let mut found = Found::default();
    let rewrite = Rewrite::plan(
        archive,
        Words {
            done: "into one chain in the order ossuary 0.14 read them, each naming its predecessor by one name",
            nothing: "every segment names its predecessor by one name",
        },
        |segments, head| {
            relink(segments, head, &mut found);
            Ok(())
        },
    )?;
    Ok((rewrite, found))
}

fn relink(segments: &mut BTreeMap<String, Segment>, head: &mut Option<Segment>, found: &mut Found) {
    // An archive in the current form is left as it is: its order is the
    // order of its chain, and sorting it by time would change what it
    // says.
    let old = |segment: &Segment| segment.listed || segment.mend.is_some();
    if !segments.values().chain(head.iter()).any(old) {
        return;
    }
    let replaced: BTreeSet<String> = segments
        .values()
        .filter_map(|segment| segment.mend.as_ref()?.get("replaces")?.as_str())
        .map(str::to_string)
        .collect();
    let lost = |named: &String| !segments.contains_key(named) && !replaced.contains(named);
    for (name, segment) in segments.iter() {
        for named in segment.previous.iter().filter(|named| lost(named)) {
            found
                .missing
                .push((format!("segment {name}"), named.clone()));
        }
    }
    for named in head
        .iter()
        .flat_map(|segment| &segment.previous)
        .filter(|named| lost(named))
    {
        found
            .missing
            .push(("the open segment".to_string(), named.clone()));
    }
    found.head_lost = head.is_none() && !segments.is_empty();

    let mut order: Vec<String> = segments
        .iter()
        .filter(|(_, segment)| !segment.lines.is_empty())
        .map(|(name, _)| name.clone())
        .collect();
    order.sort_by_key(|name| (first_time(&segments[name]), name.clone()));
    let kept: BTreeSet<&String> = order.iter().collect();
    let mut predecessor: Option<String> = None;
    for name in &order {
        let segment = segments.get_mut(name).expect("a kept segment is held");
        segment.previous = predecessor.iter().cloned().collect();
        predecessor = Some(name.clone());
    }
    for (name, segment) in segments.iter_mut() {
        segment.dropped = !kept.contains(name);
    }
    let previous: Vec<String> = predecessor.into_iter().collect();
    match head {
        Some(segment) => segment.previous = previous,
        None if !previous.is_empty() => {
            *head = Some(Segment {
                previous,
                ..Segment::default()
            });
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use immure::Digest;
    use tempfile::TempDir;

    use super::*;
    use crate::fixes::testing::{
        after, archive, first, head, listed, listed_head, segment, sound, times,
    };

    #[test]
    fn one_line_in_the_list_form_keeps_its_order() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let one = first(&archive, "2026-09-01T00:00:00Z");
        let two = listed(&archive, &[&one], "2026-09-02T00:00:00Z");
        let three = listed(&archive, &[&two], "2026-09-03T00:00:00Z");
        listed_head(&archive, &[&three]);

        let (rewrite, found) = plan(&archive).unwrap();
        assert!(!found.loses_claims());
        rewrite.apply(&archive).unwrap();

        assert_eq!(
            times(&archive),
            [
                "2026-09-01T00:00:00Z",
                "2026-09-02T00:00:00Z",
                "2026-09-03T00:00:00Z"
            ]
        );
        assert!(sound(&archive));
        let (again, _) = plan(&archive).unwrap();
        assert!(!again.changes(), "a second run finds the current form");
    }

    #[test]
    fn merged_lines_interleave_as_ossuary_0_14_read_them() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let ours = first(&archive, "2026-09-05T00:00:00Z");
        let theirs = first(&archive, "2026-09-06T00:00:00Z");
        let ours_too = listed(&archive, &[&ours], "2026-09-07T00:00:00Z");
        // The segment a merge sealed to name the last of both lines.
        let joint = segment(
            &archive,
            &crate::fixes::testing::list_header(&[&ours_too, &theirs]),
            None,
        );
        listed_head(&archive, &[&joint]);

        let (rewrite, found) = plan(&archive).unwrap();
        assert!(!found.loses_claims());
        rewrite.apply(&archive).unwrap();

        assert_eq!(
            times(&archive),
            [
                "2026-09-05T00:00:00Z",
                "2026-09-06T00:00:00Z",
                "2026-09-07T00:00:00Z"
            ],
            "by the time of each segment's first claim, as 0.14 read the record"
        );
        assert!(
            !archive.log().store().contains(&joint).unwrap(),
            "the joint is gone"
        );
        assert!(sound(&archive));
    }

    #[test]
    fn a_loss_a_mend_records_is_known_and_the_mend_dropped() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let one = first(&archive, "2026-09-01T00:00:00Z");
        let lost = Digest::parse(&"ab".repeat(32)).unwrap();
        let three = listed(&archive, &[&lost], "2026-09-03T00:00:00Z");
        let mend = segment(
            &archive,
            &format!(
                "{{\"ossuary-segment\":1,\"previous\":[\"{one}\"],\"mend\":{{\"before\":\"{three}\",\"replaces\":\"{lost}\"}}}}"
            ),
            None,
        );
        listed_head(&archive, &[&three, &mend]);

        let (rewrite, found) = plan(&archive).unwrap();
        assert!(
            !found.loses_claims(),
            "the mend recorded the loss: {found:?}"
        );
        rewrite.apply(&archive).unwrap();

        assert_eq!(
            times(&archive),
            ["2026-09-01T00:00:00Z", "2026-09-03T00:00:00Z"]
        );
        assert!(!archive.log().store().contains(&mend).unwrap());
        assert!(sound(&archive));
    }

    #[test]
    fn a_loss_no_mend_records_is_found() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let ours = first(&archive, "2026-09-05T00:00:00Z");
        let lost = Digest::parse(&"ab".repeat(32)).unwrap();
        let joined = listed(&archive, &[&ours, &lost], "2026-09-06T00:00:00Z");
        listed_head(&archive, &[&joined]);

        let (_, found) = plan(&archive).unwrap();

        assert!(found.loses_claims());
        assert_eq!(
            found.missing,
            [(format!("segment {joined}"), lost.to_string())]
        );
    }

    #[test]
    fn an_archive_in_the_current_form_is_not_sorted() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        // A clock set back: the later segment has the earlier time.
        let one = first(&archive, "2026-09-05T00:00:00Z");
        let two = after(&archive, &one, "2026-09-01T00:00:00Z");
        head(&archive, &two);

        let (rewrite, _) = plan(&archive).unwrap();

        assert!(!rewrite.changes(), "the order of the chain is its meaning");
        assert_eq!(
            rewrite.sentence(false),
            "nothing to do: every segment names its predecessor by one name"
        );
    }
}
