//! The mounted view's tree: the record's places folded into what a
//! filesystem can show.
//!
//! A filesystem narrows where the record does not, and both narrowings
//! here are this reader's declared policy, never the record's. One
//! file per name: of several standing at the same place, the newest
//! surviving assertion wins. One thing per name: where the record has
//! a name standing as file and folder at once, the folder keeps the
//! name — its children must stay reachable — and the file steps aside
//! under a name carrying the start of its digest.

use std::collections::BTreeMap;

/// What one place answers with: the winning file's digest, when its
/// assertion was made, and where that assertion stood in log order —
/// the tie-breaker for places said twice in one second.
#[derive(Debug, Clone)]
pub struct Sighting {
    pub path: String,
    pub digest: String,
    pub asserted: String,
    pub order: u64,
}

/// One entry of the flattened tree. Ids are indices into
/// [`Forest::entries`]; the root is entry 0.
#[derive(Debug)]
pub struct Entry {
    pub parent: usize,
    pub kind: Kind,
}

#[derive(Debug)]
pub enum Kind {
    /// Children in name order; their indices ascend in that order, so
    /// a listing sorted by name is sorted by id too.
    Folder { children: Vec<(String, usize)> },
    /// A file: the digest naming its bytes, its recorded size, and its
    /// recorded last change as seconds since the epoch.
    File {
        digest: String,
        size: u64,
        modified: u32,
    },
}

/// The whole mounted tree, flattened for a filesystem's id-based
/// questions.
#[derive(Debug)]
pub struct Forest {
    pub entries: Vec<Entry>,
}

enum Node {
    Folder(BTreeMap<String, Node>),
    File { digest: String, fallback: String },
}

/// Grow the tree from standing places, sizes and change times.
///
/// `places` is every standing `file:path` with its assertion moment;
/// `sizes` and `modified` answer per digest with the newest standing
/// `file:size` and `file:modified`. A file whose change time the
/// record does not spell falls back to the moment its place was
/// asserted.
pub fn grown(
    places: &[Sighting],
    sizes: &BTreeMap<String, u64>,
    modified: &BTreeMap<String, u32>,
) -> Forest {
    // The first narrowing: of everything standing at one place, the
    // newest assertion wins, log order breaking ties within a second.
    let mut winners: BTreeMap<&str, &Sighting> = BTreeMap::new();
    for sighting in places {
        match winners.get(sighting.path.as_str()) {
            Some(standing)
                if (standing.asserted.as_str(), standing.order)
                    >= (sighting.asserted.as_str(), sighting.order) => {}
            _ => {
                winners.insert(&sighting.path, sighting);
            }
        }
    }

    let mut root = BTreeMap::new();
    for sighting in winners.values() {
        plant(&mut root, sighting);
    }

    let mut entries = vec![Entry {
        parent: 0,
        kind: Kind::Folder {
            children: Vec::new(),
        },
    }];
    flatten(&root, 0, &mut entries, sizes, modified);
    Forest { entries }
}

/// Put one file into the growing tree, stepping aside where a
/// filesystem cannot say what the record says.
fn plant(root: &mut BTreeMap<String, Node>, sighting: &Sighting) {
    let mut level = root;
    let mut components = sighting
        .path
        .trim_start_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .peekable();
    while let Some(part) = components.next() {
        let last = components.peek().is_none();
        if last {
            // The second narrowing: the folder keeps the name, the
            // file steps aside under one carrying its digest.
            let name = if matches!(level.get(part), Some(Node::Folder(_))) {
                aside(part, &sighting.digest, level)
            } else {
                part.to_string()
            };
            level.insert(
                name,
                Node::File {
                    digest: sighting.digest.clone(),
                    fallback: sighting.asserted.clone(),
                },
            );
            return;
        }
        // A folder is needed here. A file already wearing the name
        // steps aside the same way.
        let stepped = match level.get(part) {
            Some(Node::File { digest, fallback }) => Some((digest.clone(), fallback.clone())),
            _ => None,
        };
        if let Some((digest, fallback)) = stepped {
            let bumped = aside(part, &digest, level);
            level.insert(bumped, Node::File { digest, fallback });
            level.insert(part.to_string(), Node::Folder(BTreeMap::new()));
        }
        level = match level
            .entry(part.to_string())
            .or_insert_with(|| Node::Folder(BTreeMap::new()))
        {
            Node::Folder(children) => children,
            Node::File { .. } => unreachable!("a file at this name was just moved aside"),
        };
    }
}

/// The stepped-aside name: the digest's first characters before the
/// extension, the spelling export uses for its collision bumps —
/// lengthened in the unheard-of case that even that name is taken.
fn aside(name: &str, digest: &str, level: &BTreeMap<String, Node>) -> String {
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, Some(extension)),
        _ => (name, None),
    };
    let mut length = 8;
    loop {
        let mark = &digest[..length.min(digest.len())];
        let bumped = match extension {
            Some(extension) => format!("{stem}-{mark}.{extension}"),
            None => format!("{stem}-{mark}"),
        };
        if !level.contains_key(&bumped) || length >= digest.len() {
            return bumped;
        }
        length += 8;
    }
}

/// Flatten depth-first in name order, so within any folder the
/// children's ids ascend the way their names sort.
fn flatten(
    level: &BTreeMap<String, Node>,
    parent: usize,
    entries: &mut Vec<Entry>,
    sizes: &BTreeMap<String, u64>,
    modified: &BTreeMap<String, u32>,
) {
    for (name, node) in level {
        let index = entries.len();
        match node {
            Node::File { digest, fallback } => {
                entries.push(Entry {
                    parent,
                    kind: Kind::File {
                        digest: digest.clone(),
                        size: sizes.get(digest).copied().unwrap_or(0),
                        modified: modified
                            .get(digest)
                            .copied()
                            .or_else(|| clamped(epoch(fallback)))
                            .unwrap_or(0),
                    },
                });
            }
            Node::Folder(children) => {
                entries.push(Entry {
                    parent,
                    kind: Kind::Folder {
                        children: Vec::new(),
                    },
                });
                flatten(children, index, entries, sizes, modified);
            }
        }
        if let Kind::Folder { children } = &mut entries[parent].kind {
            children.push((name.clone(), index));
        }
    }
}

/// Seconds since the epoch from the record's RFC 3339 spelling —
/// fractional seconds read past, a missing `Z` forgiven, anything
/// else answering `None` for the caller's fallback.
pub fn epoch(text: &str) -> Option<i64> {
    let text = text.strip_suffix('Z').unwrap_or(text);
    let (date, clock) = text.split_once('T')?;
    let clock = clock.split_once('.').map_or(clock, |(whole, _)| whole);
    let mut date = date.splitn(3, '-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: i64 = date.next()?.parse().ok()?;
    let day: i64 = date.next()?.parse().ok()?;
    let mut clock = clock.splitn(3, ':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    // Howard Hinnant's days-from-civil, the textbook calendar walk.
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = if shifted >= 0 { shifted } else { shifted - 399 } / 400;
    let of_era = shifted - era * 400;
    let of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let days = era * 146_097 + of_era * 365 + of_era / 4 - of_era / 100 + of_year - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// The epoch seconds a filesystem's timestamp field can carry.
pub fn clamped(seconds: Option<i64>) -> Option<u32> {
    seconds.and_then(|seconds| u32::try_from(seconds).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sighting(path: &str, digest: &str, asserted: &str, order: u64) -> Sighting {
        Sighting {
            path: path.to_string(),
            digest: digest.to_string(),
            asserted: asserted.to_string(),
            order,
        }
    }

    fn names(forest: &Forest, folder: usize) -> Vec<String> {
        match &forest.entries[folder].kind {
            Kind::Folder { children } => children.iter().map(|(name, _)| name.clone()).collect(),
            Kind::File { .. } => panic!("asked a file for children"),
        }
    }

    #[test]
    fn the_newest_assertion_wins_a_contested_name() {
        let forest = grown(
            &[
                sighting("/a/pixel.jpg", "1111", "2026-01-01T00:00:00Z", 0),
                sighting("/a/pixel.jpg", "2222", "2026-02-01T00:00:00Z", 1),
            ],
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        let a = forest.entries.len() - 2;
        assert_eq!(names(&forest, a), ["pixel.jpg"]);
        let Kind::File { digest, .. } = &forest.entries[a + 1].kind else {
            panic!("pixel.jpg is a file");
        };
        assert_eq!(digest, "2222");
    }

    #[test]
    fn within_one_second_the_later_claim_wins() {
        let forest = grown(
            &[
                sighting("/a", "1111", "2026-01-01T00:00:00Z", 7),
                sighting("/a", "2222", "2026-01-01T00:00:00Z", 3),
            ],
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        let Kind::File { digest, .. } = &forest.entries[1].kind else {
            panic!("/a is a file");
        };
        assert_eq!(digest, "1111", "log order breaks the tie");
    }

    #[test]
    fn a_folder_keeps_the_name_and_the_file_steps_aside() {
        let forest = grown(
            &[
                sighting("/a/b", "1234567812345678", "2026-01-01T00:00:00Z", 0),
                sighting("/a/b/c.txt", "9999", "2026-01-01T00:00:01Z", 1),
            ],
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        let a = 1;
        assert_eq!(
            names(&forest, a),
            ["b", "b-12345678"],
            "the folder wears the name; the file carries its digest"
        );
    }

    #[test]
    fn the_stepped_aside_name_keeps_its_extension() {
        let mut level = BTreeMap::new();
        level.insert("b.txt".to_string(), Node::Folder(BTreeMap::new()));
        assert_eq!(aside("b.txt", "abcdef0123456789", &level), "b-abcdef01.txt");
        assert_eq!(
            aside(".hidden", "abcdef0123456789", &level),
            ".hidden-abcdef01"
        );
    }

    #[test]
    fn sizes_and_moments_come_from_the_record() {
        let mut sizes = BTreeMap::new();
        sizes.insert("1111".to_string(), 491);
        let mut modified = BTreeMap::new();
        modified.insert("1111".to_string(), 1_700_000_000);
        let forest = grown(
            &[sighting("/a", "1111", "2026-01-01T00:00:00Z", 0)],
            &sizes,
            &modified,
        );
        let Kind::File { size, modified, .. } = &forest.entries[1].kind else {
            panic!("/a is a file");
        };
        assert_eq!((*size, *modified), (491, 1_700_000_000));
    }

    #[test]
    fn epoch_reads_the_records_spelling() {
        assert_eq!(epoch("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch("2026-09-06T15:23:40Z"), Some(1_788_708_220));
        assert_eq!(
            epoch("2026-09-06T15:23:40.89362092Z"),
            Some(1_788_708_220),
            "fractional seconds are read past"
        );
        assert_eq!(epoch("2026-09-06T15:23:40"), Some(1_788_708_220));
        assert_eq!(epoch("not a moment"), None);
    }
}
