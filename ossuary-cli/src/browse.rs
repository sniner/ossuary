//! `ossuary ls` and `ossuary tree`: looking around the record's places.
//!
//! The record answers, not a disk: every place a file was ever seen at
//! and still stands — [`Index::under`] — folded into one level (`ls`)
//! or a whole drawn subtree (`tree`). One name may honestly hold
//! several files, and a name may stand as file and folder at once;
//! both show as they stand.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Result, anyhow};
use ossuary_core::{Index, Subject, Value};

use crate::{catch_up, open, say, shorten};

pub(crate) fn ls(root: &Path, place: Option<&str>, json: bool, quiet: bool) -> Result<ExitCode> {
    let place = named(place)?;
    let archive = open(root)?;
    let mut index = archive.index()?;
    catch_up(&mut index, &archive, quiet)?;
    let pairs = index.under(&place)?;
    if pairs.is_empty() {
        if !quiet {
            eprintln!("{}", nothing(&place, "at"));
        }
        return Ok(ExitCode::SUCCESS);
    }
    // The human answer leads with short names; a script gets them
    // spelled in full, the contract `find --json` already keeps.
    let pairs = if json {
        pairs
            .into_iter()
            .map(|(path, subject)| (path, subject.as_str().to_string()))
            .collect()
    } else {
        shortened(&index, pairs)?
    };
    let node = grown(&place, &pairs);
    let lines = if json {
        objects(&place, &node)
    } else {
        listed(&place, &node)
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in &lines {
        if !say(&mut out, line)? {
            return Ok(ExitCode::SUCCESS);
        }
    }
    if !quiet {
        let (folders, files) = level_counts(&node);
        eprintln!("{} at {place}", counted(folders, files));
    }
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn tree(root: &Path, place: Option<&str>, quiet: bool) -> Result<ExitCode> {
    let place = named(place)?;
    let archive = open(root)?;
    let mut index = archive.index()?;
    catch_up(&mut index, &archive, quiet)?;
    let pairs = shortened(&index, index.under(&place)?)?;
    if pairs.is_empty() {
        if !quiet {
            eprintln!("{}", nothing(&place, "under"));
        }
        return Ok(ExitCode::SUCCESS);
    }
    let node = grown(&place, &pairs);
    let mut lines = vec![if node.ids.is_empty() {
        place.clone()
    } else {
        format!("{place}  {}", bracketed(&node.ids))
    }];
    drawn(&node, "", &mut lines);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in &lines {
        if !say(&mut out, line)? {
            return Ok(ExitCode::SUCCESS);
        }
    }
    if !quiet {
        eprintln!("{} under {place}", counted(folders_in(&node), pairs.len()));
    }
    Ok(ExitCode::SUCCESS)
}

/// The place as the record spells one: absolute, no trailing slash,
/// the root as `/`. Left out, the root — where every place begins.
fn named(given: Option<&str>) -> Result<String> {
    let given = given.unwrap_or("/");
    if !given.starts_with('/') {
        return Err(anyhow!(
            "{given:?} names no place — the record spells places whole, from /, the way ingest saw them"
        ));
    }
    let bare = given.trim_end_matches('/');
    Ok(if bare.is_empty() {
        "/".to_string()
    } else {
        bare.to_string()
    })
}

/// The calm word for an empty answer: nothing stands at the place —
/// or the record holds no places at all, which only the root can say.
fn nothing(place: &str, how: &str) -> String {
    if place == "/" {
        "the record knows no places yet — they arrive when `ossuary ingest` takes files in"
            .to_string()
    } else {
        format!(
            "nothing on the record {how} {place} — `ossuary ls /` shows where the record begins"
        )
    }
}

/// The pairs with their subjects shortened for showing, each subject
/// asked once however many places it stands at.
fn shortened(index: &Index, pairs: Vec<(String, Subject)>) -> Result<Vec<(String, String)>> {
    let mut known: BTreeMap<String, String> = BTreeMap::new();
    let mut shown = Vec::with_capacity(pairs.len());
    for (path, subject) in pairs {
        let id = if let Some(id) = known.get(subject.as_str()) {
            id.clone()
        } else {
            let id = shorten(index, &subject)?;
            known.insert(subject.as_str().to_string(), id.clone());
            id
        };
        shown.push((path, id));
    }
    Ok(shown)
}

/// One name in the tree: the files standing at it, the names below it.
#[derive(Default)]
struct Node {
    ids: Vec<String>,
    children: BTreeMap<String, Node>,
}

/// The pairs grown into a tree beneath `place`: each path one walk
/// down, its file's short name landing on the node its last component
/// names — on the root itself when the place is the file. Every path
/// names `place` or lies below it, the way [`Index::under`] answers.
fn grown(place: &str, pairs: &[(String, String)]) -> Node {
    let mut root = Node::default();
    for (path, id) in pairs {
        let rest = path
            .strip_prefix(place)
            .unwrap_or("")
            .trim_start_matches('/');
        let mut node = &mut root;
        if !rest.is_empty() {
            for part in rest.split('/') {
                node = node.children.entry(part.to_string()).or_default();
            }
        }
        node.ids.push(id.clone());
    }
    root
}

/// One entry at one level: a name standing as a file with what stands
/// there, or a name opening a folder.
enum Item<'a> {
    File(&'a str, &'a [String]),
    Folder(&'a str, &'a Node),
}

/// One level's entries, in name order: a name standing as a file, a
/// name opening a folder — a name that is both answers twice, the
/// file first.
fn flattened(node: &Node) -> Vec<Item<'_>> {
    let mut items = Vec::new();
    for (name, child) in &node.children {
        if !child.ids.is_empty() {
            items.push(Item::File(name, &child.ids));
        }
        if !child.children.is_empty() {
            items.push(Item::Folder(name, child));
        }
    }
    items
}

/// The level's lines the way `ls` speaks them: one line per file, its
/// name in the archive first — the spelling `export --dry-run` uses —
/// and folders behind the blank the names align on. The place itself
/// answering as a file comes ahead of what lies below.
fn listed(place: &str, node: &Node) -> Vec<String> {
    let width = node
        .ids
        .iter()
        .chain(node.children.values().flat_map(|child| child.ids.iter()))
        .map(String::len)
        .max()
        .unwrap_or(0);
    let gap = if width == 0 {
        String::new()
    } else {
        " ".repeat(width + 2)
    };
    let mut lines = Vec::new();
    for id in &node.ids {
        lines.push(format!("{id:<width$}  {}", tail(place)));
    }
    for item in flattened(node) {
        match item {
            Item::File(name, ids) => {
                for id in ids {
                    lines.push(format!("{id:<width$}  {name}"));
                }
            }
            Item::Folder(name, _) => lines.push(format!("{gap}{name}/")),
        }
    }
    lines
}

/// The level as JSON lines, one object per entry: a file's name with
/// everything standing there spelled in full, a folder as itself —
/// the set as a list, the way `find --json` speaks.
fn objects(place: &str, node: &Node) -> Vec<String> {
    let mut lines = Vec::new();
    if !node.ids.is_empty() {
        lines.push(file_object(tail(place), &node.ids));
    }
    for item in flattened(node) {
        match item {
            Item::File(name, ids) => lines.push(file_object(name, ids)),
            Item::Folder(name, _) => {
                lines.push(format!(
                    "{{\"folder\":{}}}",
                    Value::String(name.to_string())
                ));
            }
        }
    }
    lines
}

/// One file entry as its JSON object.
fn file_object(name: &str, subjects: &[String]) -> String {
    let spelled: Vec<Value> = subjects
        .iter()
        .map(|subject| Value::String(subject.clone()))
        .collect();
    format!(
        "{{\"file\":{},\"subjects\":{}}}",
        Value::String(name.to_string()),
        Value::Array(spelled)
    )
}

/// What one level holds: its folders, and its files — the place
/// itself counted when it answers as one.
fn level_counts(node: &Node) -> (usize, usize) {
    let mut folders = 0;
    let mut files = node.ids.len();
    for item in flattened(node) {
        match item {
            Item::File(_, ids) => files += ids.len(),
            Item::Folder(..) => folders += 1,
        }
    }
    (folders, files)
}

/// One node's entries drawn beneath `prefix`, the way `tree` draws a
/// disk: every entry on its own line, the last one closing its branch.
fn drawn(node: &Node, prefix: &str, lines: &mut Vec<String>) {
    let items = flattened(node);
    let mut ahead = items.len();
    for item in items {
        ahead -= 1;
        let (tee, bar) = if ahead == 0 {
            ("└── ", "    ")
        } else {
            ("├── ", "│   ")
        };
        match item {
            Item::File(name, ids) => {
                lines.push(format!("{prefix}{tee}{name}  {}", bracketed(ids)));
            }
            Item::Folder(name, child) => {
                lines.push(format!("{prefix}{tee}{name}/"));
                drawn(child, &format!("{prefix}{bar}"), lines);
            }
        }
    }
}

/// The ids the way `tree` wears them beside a name: each in brackets
/// of its own — two files that stood at one name are two.
fn bracketed(ids: &[String]) -> String {
    ids.iter()
        .map(|id| format!("[{id}]"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// How many folders stand below this node, itself not counted.
fn folders_in(node: &Node) -> usize {
    node.children
        .values()
        .map(|child| usize::from(!child.children.is_empty()) + folders_in(child))
        .sum()
}

/// The last component of a place: what an entry would call it.
fn tail(place: &str) -> &str {
    place.rsplit_once('/').map_or(place, |(_, name)| name)
}

/// The counts as a verdict speaks them, zero parts left unsaid.
fn counted(folders: usize, files: usize) -> String {
    let mut parts = Vec::new();
    if folders > 0 {
        parts.push(format!("{folders} folder(s)"));
    }
    if files > 0 {
        parts.push(format!("{files} file(s)"));
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(path: &str, id: &str) -> (String, String) {
        (path.to_string(), id.to_string())
    }

    #[test]
    fn a_place_is_absolute_and_carries_no_trailing_slash() {
        assert_eq!(named(None).unwrap(), "/");
        assert_eq!(named(Some("/")).unwrap(), "/");
        assert_eq!(named(Some("/home/john/")).unwrap(), "/home/john");
        assert!(
            named(Some("photos")).is_err(),
            "a relative place is refused"
        );
    }

    #[test]
    fn a_level_leads_with_the_archives_names() {
        let pairs = [
            pair("/a/b", "id3"),
            pair("/a/b/c.txt", "id1"),
            pair("/a/d.txt", "id2"),
        ];
        let node = grown("/a", &pairs);
        assert_eq!(
            listed("/a", &node),
            ["id3  b", "     b/", "id2  d.txt"],
            "a name standing as file and folder answers twice, the file first, \
             and every entry's name stands in one column"
        );
        assert_eq!(level_counts(&node), (1, 2));
    }

    #[test]
    fn a_level_of_folders_alone_wears_no_blank() {
        let node = grown("/", &[pair("/home/john/a.txt", "id1")]);
        assert_eq!(listed("/", &node), ["home/"]);
    }

    #[test]
    fn the_place_itself_may_be_the_file() {
        let node = grown("/a/b.txt", &[pair("/a/b.txt", "id1")]);
        assert_eq!(node.ids, ["id1"], "the file lands on the root node");
        assert!(node.children.is_empty());
        assert_eq!(listed("/a/b.txt", &node), ["id1  b.txt"]);
    }

    #[test]
    fn a_tree_is_drawn_with_its_branches_closed() {
        let pairs = [
            pair("/home/john/docs/letter.pdf", "id1"),
            pair("/home/john/photos/2019/beach.jpg", "id2"),
            pair("/home/john/photos/pixel.jpg", "id3"),
        ];
        let node = grown("/home/john", &pairs);
        let mut lines = Vec::new();
        drawn(&node, "", &mut lines);
        assert_eq!(
            lines,
            [
                "├── docs/",
                "│   └── letter.pdf  [id1]",
                "└── photos/",
                "    ├── 2019/",
                "    │   └── beach.jpg  [id2]",
                "    └── pixel.jpg  [id3]",
            ],
            "the last entry closes its branch, and a closed branch draws no bar"
        );
        assert_eq!(folders_in(&node), 3);
    }

    #[test]
    fn two_files_at_one_name_are_two() {
        let node = grown("/a", &[pair("/a/b.txt", "id1"), pair("/a/b.txt", "id2")]);
        let mut lines = Vec::new();
        drawn(&node, "", &mut lines);
        assert_eq!(
            lines,
            ["└── b.txt  [id1] [id2]"],
            "different bytes stood at the name over time, each in brackets of its own"
        );
        assert_eq!(
            listed("/a", &node),
            ["id1  b.txt", "id2  b.txt"],
            "and ls answers once per file, the way export --dry-run would"
        );
    }

    #[test]
    fn a_json_entry_is_a_file_or_a_folder() {
        let pairs = [
            pair("/a/b.txt", "full1"),
            pair("/a/b.txt", "full2"),
            pair("/a/c/d.txt", "full3"),
        ];
        let node = grown("/a", &pairs);
        assert_eq!(
            objects("/a", &node),
            [
                "{\"file\":\"b.txt\",\"subjects\":[\"full1\",\"full2\"]}",
                "{\"folder\":\"c\"}",
            ],
            "the set as a list, a folder as itself"
        );
    }

    #[test]
    fn counts_leave_zero_parts_unsaid() {
        assert_eq!(counted(2, 12), "2 folder(s), 12 file(s)");
        assert_eq!(counted(0, 3), "3 file(s)");
        assert_eq!(counted(1, 0), "1 folder(s)");
    }
}
