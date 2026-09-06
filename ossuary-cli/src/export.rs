//! `ossuary export`: files back out of the archive, laid down as they
//! arrived.
//!
//! The laying out — which file lands where — is core's
//! ([`ossuary_core::lay_out`]); this module turns what the caller named
//! into subjects and placements, and owns the writing: never over what
//! already stands, the same content's second landing a copy of its
//! first.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result, anyhow};
use ossuary_core::{Archive, Attribute, Index, Placed, Placement, Subject};

use crate::{catch_up, open, resolve, say, shorten};

pub(crate) fn export(
    root: &Path,
    destination: &Path,
    ids: &[String],
    dry_run: bool,
    quiet: bool,
) -> Result<ExitCode> {
    if forgotten_destination(destination) {
        return Err(anyhow!(
            "{}: reads like an id, and no such directory stands — the destination comes first: `ossuary export DIR ID…`",
            destination.display()
        ));
    }
    let archive = open(root)?;
    let mut index = archive.index()?;
    catch_up(&mut index, &archive, quiet)?;
    let pairs = gather(&index, ids)?;
    let plan = disambiguate(ossuary_core::lay_out(&pairs), quiet);

    if dry_run {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        for placed in &plan {
            // The name first: it holds one width, so the ragged paths
            // share a calm left edge — the order every answer speaks.
            let line = format!(
                "{}  {}",
                shorten(&index, &placed.subject)?,
                placed.target.display()
            );
            if !say(&mut out, &line)? {
                break;
            }
        }
        if !quiet {
            eprintln!(
                "would export {} file(s) into {} — nothing written",
                plan.len(),
                destination.display()
            );
        }
        return Ok(ExitCode::SUCCESS);
    }

    fs::create_dir_all(destination)
        .with_context(|| format!("{}: creating the destination", destination.display()))?;
    if !quiet {
        eprintln!("archive {}", archive.root().display());
        eprintln!(
            "exporting {} file(s) into {}",
            plan.len(),
            destination.display()
        );
    }
    let delivery = deliver(&archive, &plan, destination);

    let mut verdict = vec![format!(
        "{} file(s) exported into {}",
        delivery.exported,
        destination.display()
    )];
    if delivery.standing > 0 {
        verdict.push(format!(
            "{} already standing there, same bytes",
            delivery.standing
        ));
    }
    println!("{}", verdict.join(", "));
    if delivery.failed.is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("{} could not be exported:", delivery.failed.len());
        for (target, why) in &delivery.failed {
            eprintln!("  {}: {why}", target.display());
        }
        Ok(ExitCode::FAILURE)
    }
}

/// What the caller named, resolved into pairs of file and place — whole,
/// before anything is written: a mistake in the third of five ids must
/// not leave a half-written export.
fn gather(index: &Index, ids: &[String]) -> Result<Vec<(Subject, Placement)>> {
    let file_path = Attribute::parse("file:path")?;
    let file_name = Attribute::parse("file:name")?;
    let mut pairs: Vec<(Subject, Placement)> = Vec::new();
    for id in ids {
        if run_id(id) {
            let sightings = index.run_sightings(id)?;
            if sightings.is_empty() {
                return Err(anyhow!(
                    "no run {id} on the record — `ossuary about FILE prov:run` names the runs a file arrived in; nothing was exported"
                ));
            }
            pairs.extend(sightings);
        } else if id.contains('=') {
            // An attribute=value pair is find's rendered answer, not a
            // name — the likeliest way one lands here is a pipe that
            // forgot --id.
            return Err(anyhow!(
                "{id:?} is not a file's name — it reads like find's rendered answer; `ossuary find --id …` hands over bare names, ready to pipe; nothing was exported"
            ));
        } else {
            let Some(subject) = resolve(index, id)? else {
                return Err(anyhow!(
                    "nothing on the record begins with {id:?} — nothing was exported"
                ));
            };
            let standing = places(index, &subject, &file_path, &file_name)?;
            if standing.is_empty() {
                return Err(anyhow!(
                    "no path and no name stands on the record for {subject} — `ossuary get` still hands the bytes out, to a name of yours; nothing was exported"
                ));
            }
            pairs.extend(standing.into_iter().map(|place| (subject.clone(), place)));
        }
    }
    Ok(pairs)
}

/// How a delivery went: what landed, what already stood, what failed.
struct Delivery {
    exported: usize,
    standing: usize,
    failed: Vec<(PathBuf, String)>,
}

/// The plan onto the disk, one file at a time: nothing standing is ever
/// overwritten, and one file's trouble costs only itself.
fn deliver(archive: &Archive, plan: &[Placed], destination: &Path) -> Delivery {
    let algorithm = archive.content().algorithm();
    let mut delivery = Delivery {
        exported: 0,
        standing: 0,
        failed: Vec::new(),
    };
    // Where each subject's bytes already lie below the destination: the
    // same content's second landing is a copy of its first — a clone
    // where the filesystem has them — not another read of the store.
    let mut landed: HashMap<Subject, PathBuf> = HashMap::new();
    for placed in plan {
        let mut fail = |why: String| delivery.failed.push((placed.target.clone(), why));
        let dest = destination.join(&placed.target);
        if let Some(parent) = dest.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                fail(format!("creating {}: {error}", parent.display()));
                continue;
            }
        }
        match fs::symlink_metadata(&dest) {
            Ok(_) => {
                // Something already stands here. The same bytes count
                // as done; anything else is left untouched and named.
                match fs::read(&dest) {
                    Ok(bytes) if algorithm.hash(&bytes).as_str() == placed.subject.as_str() => {
                        delivery.standing += 1;
                        landed.entry(placed.subject.clone()).or_insert(dest);
                    }
                    Ok(_) => {
                        fail("different content already stands here — left untouched".to_string());
                    }
                    Err(error) => fail(format!("reading what stands here: {error}")),
                }
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                fail(error.to_string());
                continue;
            }
        }
        if let Some(first) = landed.get(&placed.subject) {
            match fs::copy(first, &dest) {
                Ok(_) => delivery.exported += 1,
                Err(error) => fail(format!("copying its first landing: {error}")),
            }
            continue;
        }
        match write_out(archive, &placed.subject, &dest) {
            Ok(true) => {
                delivery.exported += 1;
                landed.insert(placed.subject.clone(), dest);
            }
            Ok(false) => fail("on the record, but no store holds the bytes".to_string()),
            Err(error) => {
                fail(format!("{error:#}"));
                // A write that died halfway leaves no half file behind;
                // if even removing fails, the named failure above
                // already points at the place to look.
                let _ = fs::remove_file(&dest);
            }
        }
    }
    delivery
}

/// Every place still standing on one subject's record: all its
/// standing `file:path` values — or, where none stands, all its
/// standing `file:name`s, the fallback a run's sightings make too.
/// Deliberately *all* of them, not a chosen one: which of several
/// standing values matters is the reader's policy, and an export that
/// silently preferred the newest place would leave a hole in the tree
/// the caller actually asked about. A retracted place no longer
/// stands, and no longer lands.
fn places(
    index: &Index,
    subject: &Subject,
    file_path: &Attribute,
    file_name: &Attribute,
) -> Result<Vec<Placement>> {
    let spelled = |values: Vec<ossuary_core::Value>| -> Vec<String> {
        values
            .iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect()
    };
    let paths = spelled(index.values(subject, file_path)?);
    if !paths.is_empty() {
        return Ok(paths.into_iter().map(Placement::Path).collect());
    }
    let names = spelled(index.values(subject, file_name)?);
    Ok(names.into_iter().map(Placement::Name).collect())
}

/// Two different files may want one target: two groups sharing a folder
/// name, two derived files spelled alike. The later one lands beside
/// the first, `-2` before the extension — the spelling extractors
/// already use for a colliding name.
fn disambiguate(mut plan: Vec<Placed>, quiet: bool) -> Vec<Placed> {
    let mut taken: HashSet<PathBuf> = HashSet::new();
    for placed in &mut plan {
        if taken.insert(placed.target.clone()) {
            continue;
        }
        let mut count = 2;
        loop {
            let bumped = bump(&placed.target, count);
            if taken.insert(bumped.clone()) {
                if !quiet {
                    eprintln!(
                        "{}: name already taken in this export — landing as {}",
                        placed.target.display(),
                        bumped.display()
                    );
                }
                placed.target = bumped;
                break;
            }
            count += 1;
        }
    }
    plan
}

/// `report.pdf` counted up: `report-2.pdf`.
fn bump(target: &Path, count: usize) -> PathBuf {
    let stem = target.file_stem().unwrap_or_default().to_string_lossy();
    let name = match target.extension() {
        Some(extension) => format!("{stem}-{count}.{}", extension.to_string_lossy()),
        None => format!("{stem}-{count}"),
    };
    target.with_file_name(name)
}

/// One subject's bytes to `dest`, from whichever store holds them —
/// content first, the way `get` reads. `Ok(false)` when no store does,
/// and no file is begun.
fn write_out(archive: &Archive, subject: &Subject, dest: &Path) -> Result<bool> {
    for store in [archive.content(), archive.derived()] {
        let Some(digest) = store.matching(subject.as_str())?.into_iter().next() else {
            continue;
        };
        let Some(mut reader) = store.reader(&digest)? else {
            continue;
        };
        let mut file = fs::File::create(dest).context("writing")?;
        std::io::copy(&mut reader, &mut file).context("writing")?;
        return Ok(true);
    }
    Ok(false)
}

/// Whether the destination reads like an id while no such directory
/// stands: a run id, or a whole digest. The likeliest way to spell one
/// here is a forgotten destination, and the first ID must not quietly
/// become a folder named like a digest. A beginning of a digest stays a
/// legal folder name — nothing is overwritten either way.
fn forgotten_destination(destination: &Path) -> bool {
    if destination.exists() {
        return false;
    }
    let Some(name) = destination.to_str() else {
        return false;
    };
    run_id(name)
        || (matches!(name.len(), 64 | 96 | 128)
            && name.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// A run id as the verdicts spell one: the dashed UUID, whole. Anything
/// else a caller names is a file.
fn run_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 36
        && bytes
            .iter()
            .enumerate()
            .all(|(position, byte)| match position {
                8 | 13 | 18 | 23 => *byte == b'-',
                _ => byte.is_ascii_hexdigit(),
            })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_id_is_the_dashed_uuid_whole() {
        assert!(run_id("71ffc940-4b1e-417b-87a3-3c7847461e0b"));
        assert!(!run_id("71ffc940"), "a beginning is a file name");
        assert!(
            !run_id("71ffc9404b1e417b87a33c7847461e0b"),
            "undashed hex is a file name"
        );
        assert!(!run_id("71ffc940-4b1e-417b-87a3-3c7847461e0g"));
    }

    #[test]
    fn a_colliding_name_counts_up_before_its_extension() {
        assert_eq!(
            bump(Path::new("mail/report.pdf"), 2),
            Path::new("mail/report-2.pdf")
        );
        assert_eq!(bump(Path::new("README"), 3), Path::new("README-3"));
    }

    #[test]
    fn an_id_where_the_destination_belongs_is_caught() {
        let digest = "9f".repeat(32);
        assert!(forgotten_destination(Path::new(&digest)));
        assert!(forgotten_destination(Path::new(
            "71ffc940-4b1e-417b-87a3-3c7847461e0b"
        )));
        assert!(
            !forgotten_destination(Path::new("9f2ac41e")),
            "a short beginning stays a legal folder name"
        );
        assert!(!forgotten_destination(Path::new("restore")));
        assert!(
            !forgotten_destination(Path::new(".")),
            "a standing directory is a destination, whatever it is called"
        );
    }
}
