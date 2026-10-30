//! The archive under another hash: every file hashed anew into a new
//! archive, and every claim written there with the new names.
//!
//! Every name in an archive is a hash, so a different algorithm renames
//! everything: the files, and with them the subjects of the claims and
//! the values that name other files. The archive is only read. The new
//! one is created in a directory of its own, with the configuration of
//! the old one, and its claims are written in the order of the old log,
//! with their time, source and run as they were.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::Path;

use anyhow::{Context as _, Result, anyhow, bail};
use immure::{Hasher, Status, Store};
use ossuary_core::{Algorithm, Archive, Claim, LINKS};
use serde_json::Value;

/// Files of the archive's own that travel with it: its configuration,
/// and the mail fetcher's memory of where each folder's fetch stands.
const CONFIGURATION: [&str; 2] = ["config.toml", "mailvault.toml"];
const MEMO: &str = "cache/mailvault.sqlite";

/// What a rehash did, or would do.
#[derive(Debug, Default)]
pub struct Rehashed {
    /// Files stored in the new `content/` and `derived/`.
    pub content: usize,
    pub derived: usize,
    /// Files that read back as other bytes than their names say, by
    /// store and name. Not stored, and the claims about them left out.
    pub damaged: Vec<String>,
    /// Claims written with the new names.
    pub claims: usize,
    /// Claims left out because the file they are about, or the file
    /// their value names, has no new name.
    pub dropped: usize,
}

/// Rehash `archive` into a new archive at `target` that uses
/// `algorithm`. With `dry_run` nothing is written, and the counts are
/// what the archive holds. Without `force`, damaged files and claims
/// that cannot be given the new names stop the run.
///
/// # Errors
///
/// The same algorithm twice, a target that is not a new or empty
/// directory, a log that does not read whole, and whatever the stores
/// can answer.
pub fn rehash(
    archive: &Archive,
    target: &Path,
    algorithm: Algorithm,
    force: bool,
    dry_run: bool,
) -> Result<Rehashed> {
    let own = archive.content().algorithm();
    if own == algorithm {
        bail!("the archive uses {own} already; name another algorithm");
    }
    if target.exists()
        && (!target.is_dir()
            || fs::read_dir(target)
                .with_context(|| format!("{}: reading", target.display()))?
                .next()
                .is_some())
    {
        bail!(
            "{}: exists and is not an empty directory; name a new one",
            target.display()
        );
    }
    let log = archive.log();
    let mut claims = Vec::new();
    for segment in log.segments()? {
        claims.extend(log.read(segment.digest())?);
    }
    claims.extend(log.head()?);
    if dry_run {
        return Ok(Rehashed {
            content: entries(archive.content())?,
            derived: entries(archive.derived())?,
            claims: claims.len(),
            ..Rehashed::default()
        });
    }

    let rehashed = Archive::create(target, algorithm)?;
    for name in CONFIGURATION {
        let from = archive.root().join(name);
        if from.is_file() {
            fs::copy(&from, rehashed.root().join(name))
                .with_context(|| format!("{}: copying", from.display()))?;
        }
    }
    let mut done = Rehashed::default();
    let mut names = BTreeMap::new();
    for (place, from, to, count) in [
        (
            "content/",
            archive.content(),
            rehashed.content(),
            &mut done.content,
        ),
        (
            "derived/",
            archive.derived(),
            rehashed.derived(),
            &mut done.derived,
        ),
    ] {
        let (stored, damaged) = rehash_store(from, to, &mut names)?;
        *count = stored;
        done.damaged
            .extend(damaged.into_iter().map(|name| format!("{place}{name}")));
    }
    if !done.damaged.is_empty() && !force {
        bail!(
            "{} damaged file(s) in the archive, the first {}; restore them from a copy of the archive, remove {} and run again, or pass --force to leave them and the claims about them out",
            done.damaged.len(),
            done.damaged[0],
            target.display()
        );
    }
    let renamed: Vec<Option<Claim>> = claims
        .iter()
        .map(|claim| rename(claim, &names))
        .collect::<Result<_>>()?;
    let dropped = renamed.iter().filter(|claim| claim.is_none()).count();
    if dropped > 0 && !force {
        bail!(
            "{dropped} claim(s) are about files the archive does not hold, and cannot be given the new names; restore the files from a copy of the archive, remove {} and run again, or pass --force to leave those claims out",
            target.display()
        );
    }
    for claim in renamed.into_iter().flatten() {
        rehashed.log().append(&claim)?;
        done.claims += 1;
    }
    rehashed.log().seal()?;
    done.dropped = dropped;
    let memo = archive.root().join(MEMO);
    if memo.is_file() {
        let to = rehashed.root().join(MEMO);
        if let Some(cache) = to.parent() {
            fs::create_dir_all(cache).with_context(|| format!("{}: creating", cache.display()))?;
        }
        fs::copy(&memo, &to).with_context(|| format!("{}: copying", memo.display()))?;
    }
    Ok(done)
}

fn entries(store: &Store) -> Result<usize> {
    let mut count = 0;
    for entry in store.entries() {
        entry?;
        count += 1;
    }
    Ok(count)
}

/// Copy every entry of `from` into `to` under the name `to`'s hash
/// gives it, and record in `names` which new name each old name got.
/// An entry that reads back as other bytes than its old name says is
/// not kept and is answered as damaged; it gets no new name.
fn rehash_store(
    from: &Store,
    to: &Store,
    names: &mut BTreeMap<String, String>,
) -> Result<(usize, Vec<String>)> {
    let mut stored = 0;
    let mut damaged = Vec::new();
    for entry in from.entries() {
        let entry = entry?;
        let digest = entry.digest();
        let Some(reader) = from.reader(digest)? else {
            continue;
        };
        // One pass: the bytes are hashed with the old algorithm on the
        // way to the new store, which hashes them with its own.
        let mut tee = Tee {
            inner: reader,
            hasher: from.hasher(),
        };
        let (status, entry) = to.add_reader(&mut tee)?;
        if &tee.hasher.finish() != digest {
            if status == Status::New {
                to.remove(entry.digest())?;
            }
            damaged.push(digest.as_str().to_string());
            continue;
        }
        stored += 1;
        names.insert(
            digest.as_str().to_string(),
            entry.digest().as_str().to_string(),
        );
    }
    Ok((stored, damaged))
}

/// A reader that hashes what passes through it.
struct Tee<R> {
    inner: R,
    hasher: Hasher,
}

impl<R: Read> Read for Tee<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.hasher.update(&buf[..read]);
        Ok(read)
    }
}

/// A claim with its subject, and its value where the attribute links
/// to a file, under the new names; `None` when either has no new name.
/// Rewritten as JSON so that everything else, a claim without a run
/// included, stays exactly as it was.
fn rename(claim: &Claim, names: &BTreeMap<String, String>) -> Result<Option<Claim>> {
    let mut line: Value = serde_json::from_str(&claim.to_line())?;
    let object = line
        .as_object_mut()
        .ok_or_else(|| anyhow!("a claim line is not a JSON object"))?;
    let Some(subject) = names.get(claim.subject().as_str()) else {
        return Ok(None);
    };
    object.insert("subject".to_string(), Value::String(subject.clone()));
    if LINKS.contains(&claim.attribute().as_str())
        && let Some(Value::String(linked)) = claim.value()
    {
        let Some(linked) = names.get(linked) else {
            return Ok(None);
        };
        object.insert("value".to_string(), Value::String(linked.clone()));
    }
    Ok(Some(Claim::parse_line(&line.to_string())?))
}

#[cfg(test)]
mod tests {
    use ossuary_core::{Attribute, Run, Source, Subject, Timestamp};
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    fn say(archive: &Archive, subject: &str, attribute: &str, value: Value) {
        let claim = Claim::assert(
            Subject::parse(subject).unwrap(),
            Attribute::parse(attribute).unwrap(),
            value,
            Timestamp::parse("2026-09-01T00:00:00Z").unwrap(),
            Source::parse("test").unwrap(),
            Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        )
        .unwrap();
        archive.log().append(&claim).unwrap();
    }

    /// An archive with a file, a file derived from it, and a seal
    /// between their claims.
    fn archive(dir: &TempDir) -> Archive {
        let archive = Archive::create(dir.path().join("old"), Algorithm::Sha256).unwrap();
        let (_, mail) = archive.content().add(b"a mail").unwrap();
        let (_, attachment) = archive.derived().add(b"its attachment").unwrap();
        say(&archive, mail.digest().as_str(), "file:size", json!(6));
        archive.log().seal().unwrap();
        say(
            &archive,
            attachment.digest().as_str(),
            "prov:origin",
            json!(mail.digest().as_str()),
        );
        fs::write(archive.root().join("config.toml"), "# kept\n").unwrap();
        archive
    }

    #[test]
    fn files_and_claims_move_under_the_new_names() {
        let dir = TempDir::new().unwrap();
        let old = archive(&dir);
        let target = dir.path().join("new");

        let done = rehash(&old, &target, Algorithm::Blake3, false, false).unwrap();

        assert_eq!((done.content, done.derived, done.claims), (1, 1, 2));
        let new = Archive::open(&target).unwrap();
        assert_eq!(new.content().algorithm(), Algorithm::Blake3);
        let mail = new.content().digest(b"a mail");
        let attachment = new.derived().digest(b"its attachment");
        assert!(new.content().contains(&mail).unwrap());
        let mut claims = Vec::new();
        for segment in new.log().segments().unwrap() {
            claims.extend(new.log().read(segment.digest()).unwrap());
        }
        claims.extend(new.log().head().unwrap());
        assert_eq!(claims.len(), 2);
        assert_eq!(claims[0].subject().as_str(), mail.as_str());
        assert_eq!(claims[1].subject().as_str(), attachment.as_str());
        assert_eq!(
            claims[1].value(),
            Some(&json!(mail.as_str())),
            "the origin names the file under its new name"
        );
        assert_eq!(
            fs::read_to_string(target.join("config.toml")).unwrap(),
            "# kept\n"
        );
        assert_eq!(
            old.content().algorithm(),
            Algorithm::Sha256,
            "the archive is only read"
        );
    }

    #[test]
    fn a_dry_run_counts_and_writes_nothing() {
        let dir = TempDir::new().unwrap();
        let old = archive(&dir);
        let target = dir.path().join("new");

        let done = rehash(&old, &target, Algorithm::Blake3, false, true).unwrap();

        assert_eq!((done.content, done.derived, done.claims), (1, 1, 2));
        assert!(!target.exists());
    }

    #[test]
    fn the_same_algorithm_and_a_full_target_are_refused() {
        let dir = TempDir::new().unwrap();
        let old = archive(&dir);
        let target = dir.path().join("new");

        let same = rehash(&old, &target, Algorithm::Sha256, false, false).unwrap_err();
        assert!(same.to_string().contains("uses sha256 already"), "{same}");

        fs::create_dir(&target).unwrap();
        fs::write(target.join("stray"), b"").unwrap();
        let full = rehash(&old, &target, Algorithm::Blake3, false, false).unwrap_err();
        assert!(
            full.to_string().contains("not an empty directory"),
            "{full}"
        );
    }
}
