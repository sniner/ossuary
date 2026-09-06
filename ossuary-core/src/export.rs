//! Export: what the record says about where files stood, turned into
//! places under one destination.
//!
//! The archive keeps custody; an export is a copy out. *Which* files go
//! is the caller's question — a run's sightings, one subject's newest
//! place — and this module owes the laying out: relative targets that
//! keep what the recorded paths shared, without the folders above their
//! common ground. Two files that lay side by side land side by side;
//! the way from `/` down to them comes along only where it told them
//! apart.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use crate::claim::Subject;

/// Where a file stood, as far as the record tells: a full path some
/// sighting recorded, or a bare name — a derived file never sat
/// anywhere.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Placement {
    /// A recorded `file:path` — absolute, in its host's own spelling.
    Path(String),
    /// A recorded `file:name` — all a derived file has.
    Name(String),
}

/// One file of an export: which content, and where it lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    /// The content, by its name in the archive.
    pub subject: Subject,
    /// Where it lands, relative to the destination directory.
    pub target: PathBuf,
}

/// Lay an export out: every pair a file, every target relative to one
/// destination, sorted by target.
///
/// Pathed placements keep their relations: the folder components all
/// of them share are trimmed away, the rest stays. When the paths
/// share nothing — several roots were recorded, or several hosts —
/// they split at the root into groups, and each group keeps the last
/// component of *its* shared ground as a folder of its own, so the
/// groups stay apart under the destination. A single group needs no
/// such folder, which is also what makes one exported file land as its
/// bare name. Named placements have no relations to keep and land
/// flat.
///
/// A pair said twice is laid out once; the same subject under two
/// placements is honestly two files. Targets are built from the paths'
/// named components alone — `..` and `.` fall away — so a target
/// cannot climb out of the destination. Two pairs may still want the
/// same target; settling that is the writer's business, who sees what
/// already stands.
#[must_use]
pub fn lay_out(pairs: &[(Subject, Placement)]) -> Vec<Placed> {
    let mut seen: HashSet<(&Subject, &Placement)> = HashSet::new();
    let mut pathed: Vec<Pathed> = Vec::new();
    let mut named: Vec<(Subject, String)> = Vec::new();
    for (subject, placement) in pairs {
        if !seen.insert((subject, placement)) {
            continue;
        }
        match placement {
            Placement::Path(path) => {
                let parts = parts(path);
                if !parts.is_empty() {
                    pathed.push((subject.clone(), parts));
                }
            }
            Placement::Name(name) => {
                if let Some(bare) = parts(name).pop() {
                    named.push((subject.clone(), bare));
                }
            }
        }
    }

    let mut placed: Vec<Placed> = Vec::new();
    let clusters = cluster(pathed);
    let several = clusters.len() > 1;
    for members in &clusters {
        let shared = shared_prefix(members);
        for (subject, parts) in members {
            let mut target = PathBuf::new();
            if several && shared > 0 {
                target.push(&parts[shared - 1]);
            }
            for part in &parts[shared..] {
                target.push(part);
            }
            placed.push(Placed {
                subject: subject.clone(),
                target,
            });
        }
    }
    for (subject, name) in named {
        placed.push(Placed {
            subject,
            target: PathBuf::from(name),
        });
    }
    placed.sort_by(|a, b| (&a.target, &a.subject).cmp(&(&b.target, &b.subject)));
    placed
}

/// One file on its way out: the content, and its recorded path in
/// pieces — the last one its name.
type Pathed = (Subject, Vec<String>);

/// The pathed placements in groups whose members share ground. One
/// group while anything is shared; where nothing is — the recorded
/// paths diverge at the root itself — one group per first component,
/// in the order first met.
fn cluster(pathed: Vec<Pathed>) -> Vec<Vec<Pathed>> {
    if pathed.is_empty() {
        return Vec::new();
    }
    if pathed.len() == 1 || shared_prefix(&pathed) > 0 {
        return vec![pathed];
    }
    let mut groups: Vec<(Option<String>, Vec<Pathed>)> = Vec::new();
    for member in pathed {
        // The first folder component; a file with none — recorded at
        // the root itself — groups with its kind.
        let key = (member.1.len() > 1).then(|| member.1[0].clone());
        match groups.iter_mut().find(|(known, _)| *known == key) {
            Some((_, members)) => members.push(member),
            None => groups.push((key, vec![member])),
        }
    }
    groups.into_iter().map(|(_, members)| members).collect()
}

/// How many leading folder components every member shares. The last
/// component of each path is its file name and never counts.
fn shared_prefix(members: &[Pathed]) -> usize {
    let mut dirs = members.iter().map(|(_, parts)| &parts[..parts.len() - 1]);
    let Some(first) = dirs.next() else { return 0 };
    let mut shared = first.len();
    for other in dirs {
        let mut common = 0;
        while common < shared && common < other.len() && other[common] == first[common] {
            common += 1;
        }
        shared = common;
    }
    shared
}

/// A path's own pieces: the named components alone. Roots, `..` and
/// `.` fall away, so whatever is built from these stays below the
/// destination.
fn parts(path: &str) -> Vec<String> {
    Path::new(path)
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subject(fill: &str) -> Subject {
        Subject::parse(&fill.repeat(32)).unwrap()
    }

    fn path(subject: &Subject, path: &str) -> (Subject, Placement) {
        (subject.clone(), Placement::Path(path.to_string()))
    }

    fn targets(placed: &[Placed]) -> Vec<String> {
        placed
            .iter()
            .map(|placed| placed.target.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_single_file_lands_as_its_bare_name() {
        let placed = lay_out(&[path(&subject("1f"), "/home/s/docs/report.pdf")]);
        assert_eq!(targets(&placed), ["report.pdf"]);
    }

    #[test]
    fn one_shared_ground_is_trimmed_whole() {
        let placed = lay_out(&[
            path(&subject("1f"), "/home/s/photos/a.jpg"),
            path(&subject("2e"), "/home/s/photos/2024/b.jpg"),
        ]);
        assert_eq!(
            targets(&placed),
            ["2024/b.jpg", "a.jpg"],
            "what the paths shared is gone, what told them apart stays"
        );
    }

    #[test]
    fn shared_ground_reaches_only_as_far_as_it_is_shared() {
        let placed = lay_out(&[
            path(&subject("1f"), "/home/s/photos/a.jpg"),
            path(&subject("2e"), "/home/s/mail/b.eml"),
        ]);
        assert_eq!(
            targets(&placed),
            ["mail/b.eml", "photos/a.jpg"],
            "one group — /home/s is shared — and the difference is kept"
        );
    }

    #[test]
    fn unrelated_places_become_sibling_folders() {
        let placed = lay_out(&[
            path(&subject("1f"), "/home/s/photos/a.jpg"),
            path(&subject("2e"), "/home/s/photos/b.jpg"),
            path(&subject("3d"), "/mnt/nas/mail/c.eml"),
        ]);
        assert_eq!(
            targets(&placed),
            ["mail/c.eml", "photos/a.jpg", "photos/b.jpg"],
            "each group keeps the last folder it shared, and they stay apart"
        );
    }

    #[test]
    fn a_name_lands_flat_and_a_pair_said_twice_lands_once() {
        let pdf = subject("4c");
        let placed = lay_out(&[
            (pdf.clone(), Placement::Name("invoice.pdf".to_string())),
            (pdf.clone(), Placement::Name("invoice.pdf".to_string())),
        ]);
        assert_eq!(targets(&placed), ["invoice.pdf"]);
        assert_eq!(placed[0].subject, pdf);
    }

    #[test]
    fn the_same_content_at_two_places_is_two_files() {
        let copy = subject("5b");
        let placed = lay_out(&[
            path(&copy, "/home/s/docs/a.txt"),
            path(&copy, "/home/s/docs/backup/a.txt"),
        ]);
        assert_eq!(
            targets(&placed),
            ["a.txt", "backup/a.txt"],
            "the run's reality had two, so the export has two"
        );
    }

    #[test]
    fn a_target_cannot_climb_out_of_the_destination() {
        let placed = lay_out(&[(subject("6a"), Placement::Name("../../etc/evil".to_string()))]);
        assert_eq!(
            targets(&placed),
            ["evil"],
            "only named components survive into a target"
        );
    }
}
