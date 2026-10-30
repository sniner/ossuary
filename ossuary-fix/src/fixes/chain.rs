//! A broken chain closed: the remedy when a sealed segment or the open
//! segment was lost and no copy of the archive has it.
//!
//! The order of the record is the order of the chain, so nothing that is
//! still linked moves. A segment that names one the store does not hold
//! is made to name the segment before the gap instead; a lost open
//! segment is begun anew after the last sealed one. Where the segment
//! before a gap cannot be told, because the chain is broken in more than
//! one place, the fix stops rather than guess at the order. A piece that
//! nothing links to the rest is put at the end and named.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use ossuary_core::Archive;

use crate::fixes::found::Found;
use crate::rewrite::{Rewrite, Segment, Words, first_time};

/// The chain closed, and what was found on the way.
///
/// # Errors
///
/// Whatever [`Rewrite::plan`] can answer; an archive in the form of
/// ossuary 0.14 or earlier, which `ossuary-fix list` brings into the
/// current form first; two segments that follow the same one; and a
/// chain broken in more than one place.
pub fn plan(archive: &Archive) -> Result<(Rewrite, Found)> {
    let mut found = Found::default();
    let rewrite = Rewrite::plan(
        archive,
        Words {
            done: "to close the chain",
            nothing: "the chain is whole",
        },
        |segments, head| relink(segments, head, &mut found),
    )?;
    Ok((rewrite, found))
}

/// One unbroken run of segments, oldest first.
type Piece = Vec<String>;

#[allow(
    clippy::too_many_lines,
    reason = "the walk back from the head is one sequence of cases; split, its order would be harder to follow"
)]
fn relink(
    segments: &mut BTreeMap<String, Segment>,
    head: &mut Option<Segment>,
    found: &mut Found,
) -> Result<()> {
    if segments
        .values()
        .chain(head.iter())
        .any(|segment| segment.listed || segment.mend.is_some())
    {
        bail!(
            "the archive is in the form of ossuary 0.14 or earlier; run `ossuary-fix list` first, nothing was written"
        );
    }
    let previous = |name: &String| segments[name].previous.first().cloned();

    // The pieces: a piece begins at a segment that names nothing, or names
    // a segment the store does not hold, and runs on through every
    // segment that names the one before.
    let mut after: BTreeMap<String, String> = BTreeMap::new();
    for name in segments.keys() {
        if let Some(before) = previous(name).filter(|before| segments.contains_key(before))
            && let Some(other) = after.insert(before.clone(), name.clone())
        {
            bail!(
                "segments {other} and {name} both follow segment {before}; this fix cannot tell which comes first, nothing was written"
            );
        }
    }
    let mut pieces: Vec<Piece> = segments
        .keys()
        .filter(|name| previous(name).is_none_or(|before| !segments.contains_key(&before)))
        .map(|start| {
            let mut piece = vec![start.clone()];
            while let Some(next) = piece.last().and_then(|last| after.get(last)) {
                piece.push(next.clone());
            }
            piece
        })
        .collect();
    for piece in &pieces {
        if let Some(missing) = previous(&piece[0]) {
            found
                .missing
                .push((format!("segment {}", piece[0]), missing));
        }
    }
    let take = |pieces: &mut Vec<Piece>, index: usize| pieces.remove(index);

    // Back from the head: the piece it continues, then for each gap the
    // one piece that can stand before it.
    let mut line: Vec<Piece> = Vec::new();
    match head
        .as_ref()
        .map(|segment| segment.previous.first().cloned())
    {
        Some(Some(last)) if segments.contains_key(&last) => {
            let Some(index) = pieces.iter().position(|piece| piece.contains(&last)) else {
                unreachable!("every held segment is in a piece");
            };
            if pieces[index].last() != Some(&last) {
                bail!(
                    "the open segment names segment {last}, which another segment follows; the record was cut back by hand, nothing was written"
                );
            }
            line.push(take(&mut pieces, index));
        }
        Some(Some(missing)) => {
            found
                .missing
                .push(("the open segment".to_string(), missing));
        }
        Some(None) => found.head_anew = !pieces.is_empty(),
        None => found.head_lost = !pieces.is_empty(),
    }
    let gap_before = |piece: &Piece| previous(&piece[0]).is_some();
    if line.is_empty() && !pieces.is_empty() {
        // No piece is continued by the head: the last one is the piece
        // that begins at a gap, or the only one.
        let candidates: Vec<usize> = if pieces.iter().any(gap_before) {
            (0..pieces.len())
                .filter(|&index| gap_before(&pieces[index]))
                .collect()
        } else {
            (0..pieces.len()).collect()
        };
        let [index] = candidates[..] else {
            bail!(
                "the chain is broken in more than one place, and which piece came last cannot be told; copy the missing segments back from a copy of the archive, nothing was written"
            );
        };
        line.push(take(&mut pieces, index));
    }
    // Before a gap stands the one piece that is left; with none left,
    // the gap was at the start of the record.
    while let Some(start) = line
        .last()
        .filter(|piece| gap_before(piece))
        .map(|piece| piece[0].clone())
    {
        match pieces.len() {
            0 => break,
            1 => line.push(take(&mut pieces, 0)),
            _ => bail!(
                "the chain is broken in more than one place, and which piece stood before the gap at segment {start} cannot be told; copy the missing segments back from a copy of the archive, nothing was written"
            ),
        }
    }
    line.reverse();

    // Pieces that nothing links to the rest have no place in the order of
    // the log: they go at the end, oldest first, and are named.
    pieces.sort_by_key(|piece| (first_time(&segments[&piece[0]]), piece[0].clone()));
    for piece in &pieces {
        let behind = line.iter().flatten().last().map_or_else(
            || "at the start".to_string(),
            |last| format!("behind segment {last}"),
        );
        found.placed.push(format!(
            "segment {} and the {} after it are linked to nothing else; they are put at the end, {behind}",
            piece[0],
            piece.len() - 1
        ));
        line.push(piece.clone());
    }

    let order: Vec<String> = line.into_iter().flatten().collect();
    let mut predecessor: Option<String> = None;
    for name in &order {
        let segment = segments.get_mut(name).expect("a placed segment is held");
        segment.previous = predecessor.iter().cloned().collect();
        predecessor = Some(name.clone());
    }
    let last: Vec<String> = predecessor.into_iter().collect();
    match head {
        Some(segment) => segment.previous = last,
        None if !last.is_empty() => {
            *head = Some(Segment {
                previous: last,
                ..Segment::default()
            });
        }
        None => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::fixes::testing::{
        after, archive, first, head, listed, listed_head, lose, sound, times,
    };

    #[test]
    fn a_gap_is_closed_and_the_part_before_it_keeps_its_names() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let one = first(&archive, "2026-09-01T00:00:00Z");
        let two = after(&archive, &one, "2026-09-02T00:00:00Z");
        let three = after(&archive, &two, "2026-09-03T00:00:00Z");
        head(&archive, &three);
        lose(&archive, &two);

        let (rewrite, found) = plan(&archive).unwrap();
        assert!(found.loses_claims());
        assert_eq!(
            found.missing,
            [(format!("segment {three}"), two.to_string())]
        );
        rewrite.apply(&archive).unwrap();

        assert_eq!(
            times(&archive),
            ["2026-09-01T00:00:00Z", "2026-09-03T00:00:00Z"]
        );
        assert_eq!(
            archive.log().segments().unwrap()[0].digest(),
            &one,
            "seals up to the gap still check"
        );
        assert!(sound(&archive));
    }

    #[test]
    fn nothing_linked_moves_whatever_the_times_say() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let one = first(&archive, "2026-09-05T00:00:00Z");
        let two = after(&archive, &one, "2026-09-06T00:00:00Z");
        let three = after(&archive, &two, "2026-09-01T00:00:00Z");
        head(&archive, &three);
        lose(&archive, &two);

        let (rewrite, _) = plan(&archive).unwrap();
        rewrite.apply(&archive).unwrap();

        assert_eq!(
            times(&archive),
            ["2026-09-05T00:00:00Z", "2026-09-01T00:00:00Z"],
            "the piece the head continues stays last"
        );
    }

    #[test]
    fn a_lost_head_is_begun_after_the_last_segment() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let log = archive.log();
        let one = first(&archive, "2026-09-01T00:00:00Z");
        let last = after(&archive, &one, "2026-09-02T00:00:00Z");
        assert!(!log.head_path().exists());

        let (rewrite, found) = plan(&archive).unwrap();
        assert!(found.head_lost);
        rewrite.apply(&archive).unwrap();

        assert_eq!(log.head_contents().unwrap().previous(), Some(&last));
        assert!(sound(&archive));
    }

    #[test]
    fn a_chain_broken_twice_is_left_alone() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let one = first(&archive, "2026-09-01T00:00:00Z");
        let two = after(&archive, &one, "2026-09-02T00:00:00Z");
        let three = after(&archive, &two, "2026-09-03T00:00:00Z");
        let four = after(&archive, &three, "2026-09-04T00:00:00Z");
        let five = after(&archive, &four, "2026-09-05T00:00:00Z");
        head(&archive, &five);
        lose(&archive, &two);
        lose(&archive, &four);

        let error = plan(&archive).unwrap_err();

        assert!(error.to_string().contains("more than one place"), "{error}");
    }

    #[test]
    fn a_piece_linked_to_nothing_goes_at_the_end_and_is_named() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let one = first(&archive, "2026-09-02T00:00:00Z");
        head(&archive, &one);
        let stray = first(&archive, "2026-09-01T00:00:00Z");

        let (rewrite, found) = plan(&archive).unwrap();
        assert!(!found.loses_claims(), "nothing is lost by placing it");
        assert_eq!(found.placed.len(), 1);
        assert!(
            found.placed[0].contains(stray.as_str()),
            "{:?}",
            found.placed
        );
        rewrite.apply(&archive).unwrap();

        assert_eq!(
            times(&archive),
            ["2026-09-02T00:00:00Z", "2026-09-01T00:00:00Z"]
        );
        assert!(sound(&archive));
    }

    #[test]
    fn the_form_of_0_14_is_left_to_list() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let one = first(&archive, "2026-09-01T00:00:00Z");
        let two = listed(&archive, &[&one], "2026-09-02T00:00:00Z");
        listed_head(&archive, &[&two]);

        let error = plan(&archive).unwrap_err();

        assert!(error.to_string().contains("ossuary-fix list"), "{error}");
    }

    #[test]
    fn a_whole_chain_needs_nothing() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let log = archive.log();
        log.append(&crate::fixes::testing::claim(
            &archive,
            "2026-09-01T00:00:00Z",
        ))
        .unwrap();
        log.seal().unwrap().unwrap();
        assert!(fs::metadata(log.head_path()).is_ok());

        let (rewrite, found) = plan(&archive).unwrap();

        assert!(!rewrite.changes());
        assert!(found.lines().is_empty());
        assert_eq!(rewrite.sentence(false), "nothing to do: the chain is whole");
    }
}
