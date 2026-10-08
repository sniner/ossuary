//! Merging: a second archive taken into this one, whole, as further
//! lines of the record.
//!
//! Both archives name their content by the same hash, so the other
//! archive's files and sealed segments go into this one's stores under
//! the names they have, and what is already here is not copied again.
//! Nothing is rewritten: the other archive's segments keep their bytes
//! and their chain, this archive's head is sealed, and the fresh head
//! names the last segment of both archives. From then on the record has
//! two first segments, and the audit reads the other archive's segments
//! as a line merged into the chain ([`crate::audit`]).
//!
//! The steps are separate functions so that a caller can announce each
//! one: [`check`] before anything is written, [`preview_copy`] and
//! [`copy_store`] for each pair of stores, [`join`] for the chain,
//! [`take_memo`] for the one
//! cache worth carrying over. Every step but `join` is additive and can
//! be repeated; `join` done twice changes nothing the second time.
//!
//! The other archive is read, and its open segment is sealed so that
//! its last claims are in a segment this archive can name. Nothing
//! else in it changes.
//!
//! An archive that names its files by another hash cannot be merged
//! this way: its names mean nothing here. It is taken in by rehashing
//! ([`rehash_store`]): every file is hashed anew on the way in, which
//! gives the table from old names to new, and the other archive's
//! claims are replayed with the new names ([`replay`]), segment for
//! segment, with their time, source and run as they were. The other
//! archive's chain is not carried over, since its segment names mean
//! nothing here either; the replayed segments are sealed in its order.
//! The other archive is only read.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::Path;

use immure::{Digest, Hasher, Status, Store};
use rusqlite::Connection;
use serde_json::Value;

use crate::archive::Archive;
use crate::audit::{LINKS, audit_log};
use crate::claim::Claim;
use crate::error::{Error, Result};
use crate::log::Log;

/// The mail fetcher's configuration and memory, by the names
/// `ossuary-mailvault` gives them. Named here because the merge is the
/// one place that handles another archive's copies of them.
const MAILVAULT_CONFIG: &str = "mailvault.toml";
const MAILVAULT_MEMO: &str = "mailvault.sqlite";

/// What a merge cannot do or should not do without being told to.
///
/// Different hash algorithms are an error, not an obstacle: the other
/// archive's names mean nothing here, and taking it in would be an
/// import, not a merge.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Obstacles {
    /// Findings of the other archive's log audit: damaged, unreadable
    /// or missing segments, a lost head. A merge carries them over.
    pub log_findings: usize,
    /// Account names that both `mailvault.toml` files use for different
    /// mailboxes. `mailbox:place` records the name, so the places of
    /// two mailboxes would read as one.
    pub accounts: Vec<String>,
}

impl Obstacles {
    /// Whether nothing stands against the merge.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.log_findings == 0 && self.accounts.is_empty()
    }
}

/// What a copy did: entries written, entries already here, and the
/// entries that read back as other bytes than their names say.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Copied {
    pub copied: usize,
    pub known: usize,
    /// Names of damaged entries in the other archive, not copied.
    pub damaged: Vec<String>,
}

/// What a copy would do.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Preview {
    /// Entries not yet in this archive's store.
    pub new: usize,
    /// Entries already here.
    pub known: usize,
}

/// What `join` did to the chain.
#[derive(Debug, PartialEq, Eq)]
pub struct Join {
    /// The segment this archive's open segment was sealed as, when it
    /// held claims.
    pub sealed: Option<Digest>,
    /// The segment the other archive's open segment was sealed as,
    /// when it held claims.
    pub sealed_theirs: Option<Digest>,
    /// The segments this archive's open segment names now.
    pub names: Vec<Digest>,
    /// Whether the open segment was changed: false when it already
    /// named the other archive's last segment.
    pub changed: bool,
}

/// Whether the two archives can be merged, and what stands against it.
/// With `rehash`, the hashes must differ; without, they must be the
/// same.
///
/// # Errors
///
/// [`Error::MergeSelf`] for the same archive twice,
/// [`Error::MergeAlgorithm`] and [`Error::MergeSameHash`] for the
/// wrong way for the hashes, and whatever auditing the other archive's
/// log can answer.
pub fn check(ours: &Archive, theirs: &Archive, rehash: bool) -> Result<Obstacles> {
    if same_archive(ours.root(), theirs.root()) {
        return Err(Error::MergeSelf(ours.root().to_path_buf()));
    }
    let (here, there) = (ours.content().algorithm(), theirs.content().algorithm());
    if here != there && !rehash {
        return Err(Error::MergeAlgorithm {
            ours: here.to_string(),
            theirs: there.to_string(),
        });
    }
    if here == there && rehash {
        return Err(Error::MergeSameHash(here.to_string()));
    }
    let log = audit_log(theirs.log())?;
    Ok(Obstacles {
        log_findings: log.findings(),
        accounts: clashing_accounts(ours.root(), theirs.root())?,
    })
}

fn same_archive(ours: &Path, theirs: &Path) -> bool {
    match (fs::canonicalize(ours), fs::canonicalize(theirs)) {
        (Ok(ours), Ok(theirs)) => ours == theirs,
        _ => ours == theirs,
    }
}

/// Account names both `mailvault.toml` files use for different
/// mailboxes. An account is the same mailbox when every key but the
/// secrets and the folder list is the same.
fn clashing_accounts(ours: &Path, theirs: &Path) -> Result<Vec<String>> {
    let (Some(ours), Some(theirs)) = (accounts(ours)?, accounts(theirs)?) else {
        return Ok(Vec::new());
    };
    Ok(ours
        .iter()
        .filter(|(name, mailbox)| theirs.get(*name).is_some_and(|other| other != *mailbox))
        .map(|(name, _)| name.clone())
        .collect())
}

/// The accounts of an archive's `mailvault.toml`, by name, each
/// reduced to what identifies its mailbox. `None` without the file. A
/// file that is not TOML, or whose accounts are not tables with names,
/// is not this crate's to judge: it counts as no accounts.
fn accounts(root: &Path) -> Result<Option<BTreeMap<String, toml::Table>>> {
    const SECRETS: [&str; 5] = [
        "password",
        "password_cmd",
        "client_secret",
        "client_secret_cmd",
        "folders",
    ];
    let path = root.join(MAILVAULT_CONFIG);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(Error::Io {
                context: format!("{}: reading", path.display()),
                source,
            });
        }
    };
    let Ok(file) = text.parse::<toml::Table>() else {
        return Ok(Some(BTreeMap::new()));
    };
    let mut accounts = BTreeMap::new();
    for account in file
        .get("account")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(table) = account.as_table() else {
            continue;
        };
        let Some(name) = table.get("name").and_then(toml::Value::as_str) else {
            continue;
        };
        let mut mailbox = table.clone();
        for key in SECRETS {
            mailbox.remove(key);
        }
        accounts.insert(name.to_string(), mailbox);
    }
    Ok(Some(accounts))
}

/// What copying `from` into `to` would do.
///
/// # Errors
///
/// [`Error::Store`] walking either store.
pub fn preview_copy(from: &Store, to: &Store) -> Result<Preview> {
    let mut preview = Preview::default();
    for entry in from.entries() {
        let entry = entry?;
        if to.contains(entry.digest())? {
            preview.known += 1;
        } else {
            preview.new += 1;
        }
    }
    Ok(preview)
}

/// Copy every entry of `from` that `to` does not hold. An entry is
/// hashed on the way; one whose bytes are not what its name says is
/// not kept and is reported as damaged.
///
/// # Errors
///
/// [`Error::Store`] from either store.
pub fn copy_store(from: &Store, to: &Store) -> Result<Copied> {
    let mut copied = Copied::default();
    for entry in from.entries() {
        let entry = entry?;
        let digest = entry.digest();
        if to.contains(digest)? {
            copied.known += 1;
            continue;
        }
        let Some(reader) = from.reader(digest)? else {
            // Gone between the walk and the read: not ours to copy.
            continue;
        };
        let (status, stored) = to.add_reader(reader)?;
        if stored.digest() == digest {
            copied.copied += 1;
            continue;
        }
        // The bytes read are not the entry's. What was just stored is
        // named truly but belongs to nothing here; it goes, unless the
        // store held those bytes already.
        if status == Status::New {
            to.remove(stored.digest())?;
        }
        copied.damaged.push(digest.as_str().to_string());
    }
    Ok(copied)
}

/// Copy every entry of `from` into `to` under the name `to`'s hash
/// gives it, and record in `names` which new name each old name got.
/// An entry that reads back as other bytes than its old name says is
/// not kept and is reported as damaged; it gets no new name.
///
/// # Errors
///
/// [`Error::Store`] from either store.
pub fn rehash_store(
    from: &Store,
    to: &Store,
    names: &mut BTreeMap<String, String>,
) -> Result<Copied> {
    let mut copied = Copied::default();
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
        let (status, stored) = to.add_reader(&mut tee)?;
        if &tee.hasher.finish() != digest {
            if status == Status::New {
                to.remove(stored.digest())?;
            }
            copied.damaged.push(digest.as_str().to_string());
            continue;
        }
        match status {
            Status::New => copied.copied += 1,
            Status::Exists => copied.known += 1,
        }
        names.insert(
            digest.as_str().to_string(),
            stored.digest().as_str().to_string(),
        );
    }
    Ok(copied)
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

/// What a replay did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Replayed {
    /// Segments sealed here: one per segment of the other archive
    /// that held claims, and one for its open segment when that did.
    pub segments: usize,
    /// Claims written with their new names.
    pub claims: usize,
    /// Claims left out because the file they are about, or the file
    /// their value names, has no new name.
    pub dropped: usize,
}

/// Replay the other archive's log here with the new names: every
/// sealed segment in log order, then the open segment, each sealed as
/// a segment of its own, so that the replayed claims sort by their
/// own time among the segments already here. This archive's open
/// segment is sealed first for the same reason. Time, source, run and
/// retraction of every claim stay as they were.
///
/// A claim about a file that has no new name in `names` cannot be
/// rewritten. Without `force` the replay stops before writing anything
/// when there is one; with `force` such claims are left out.
///
/// # Errors
///
/// [`Error::MergeMissing`] as above, and whatever reading the other
/// log and writing this one can answer.
pub fn replay(
    ours: &Archive,
    theirs: &Archive,
    names: &BTreeMap<String, String>,
    force: bool,
) -> Result<Replayed> {
    let log = theirs.log();
    let mut batches: Vec<Vec<Claim>> = Vec::new();
    for segment in log.segments()? {
        let claims = log.read(segment.digest())?;
        if !claims.is_empty() {
            batches.push(claims);
        }
    }
    let head = log.head()?;
    if !head.is_empty() {
        batches.push(head);
    }
    let missing = batches
        .iter()
        .flatten()
        .filter(|claim| rename(claim, names).is_none())
        .count();
    if missing > 0 && !force {
        return Err(Error::MergeMissing(missing));
    }
    let mut replayed = Replayed::default();
    ours.log().seal()?;
    for batch in batches {
        let mut written = 0;
        for claim in &batch {
            let Some(line) = rename(claim, names) else {
                replayed.dropped += 1;
                continue;
            };
            ours.log().append(&Claim::parse_line(&line)?)?;
            written += 1;
        }
        if written > 0 {
            seal_apart(ours.log())?;
            replayed.segments += 1;
            replayed.claims += written;
        }
    }
    Ok(replayed)
}

/// A claim's line with its subject, and its value where the attribute
/// links to a subject, under the new names. `None` when either has no
/// new name. Rewritten as JSON so that everything else, a claim
/// without a run included, stays exactly as it was.
fn rename(claim: &Claim, names: &BTreeMap<String, String>) -> Option<String> {
    let mut line: Value = serde_json::from_str(&claim.to_line()).ok()?;
    let object = line.as_object_mut()?;
    let subject = names.get(claim.subject().as_str())?;
    object.insert("subject".to_string(), Value::String(subject.clone()));
    if LINKS.contains(&claim.attribute().as_str()) {
        if let Some(Value::String(linked)) = claim.value() {
            let linked = names.get(linked)?;
            object.insert("value".to_string(), Value::String(linked.clone()));
        }
    }
    Some(line.to_string())
}

/// Seal the open segment; a seal that finds it empty is no error
/// here, the segment was sealed by size on the way.
fn seal_apart(log: &Log) -> Result<()> {
    log.seal().map(|_| ())
}

/// Seal both open segments and make this archive's open segment name
/// the last segment of both archives.
///
/// The other archive's open segment is sealed first, so that its last
/// claims are in a segment with a name; that segment must have been
/// copied here already ([`copy_store`] on the claims stores, after the
/// seal). An open segment of this archive that already names the other
/// archive's last segment is left as it is.
///
/// # Errors
///
/// [`Error::MergeNothing`] when the other archive has no sealed
/// segment, [`Error::SegmentMissing`] when its last segment is not in
/// this archive's claims store, and whatever sealing and rewriting the
/// head can answer.
pub fn join(ours: &Archive, theirs: &Archive) -> Result<Join> {
    let sealed_theirs = theirs.log().seal()?.map(|segment| segment.digest().clone());
    let Some(last_theirs) = theirs.log().head_contents()?.previous().first().cloned() else {
        return Err(Error::MergeNothing(theirs.root().to_path_buf()));
    };
    if !ours.log().store().contains(&last_theirs)? {
        return Err(Error::SegmentMissing(last_theirs.to_string()));
    }
    let sealed = ours.log().seal()?.map(|segment| segment.digest().clone());
    let mut names = ours.log().head_contents()?.previous().to_vec();
    if names.contains(&last_theirs) {
        return Ok(Join {
            sealed,
            sealed_theirs,
            names,
            changed: false,
        });
    }
    // An open segment without claims is not sealed, so after a merge
    // without new claims here it still names the other archive's last
    // segment of that time. The new last segment follows it, and a
    // name the walk back from the new one reaches is not needed twice.
    let behind = behind(theirs, &last_theirs)?;
    names.retain(|name| !behind.contains(name));
    names.push(last_theirs);
    ours.log().head_follows(&names)?;
    Ok(Join {
        sealed,
        sealed_theirs,
        names,
        changed: true,
    })
}

/// Every segment the walk back from `from` reaches in the other
/// archive's log, `from` itself left out. A name that is not held
/// ends that branch of the walk.
fn behind(theirs: &Archive, from: &Digest) -> Result<BTreeSet<Digest>> {
    let log = theirs.log();
    let mut seen = BTreeSet::new();
    let mut pending = vec![from.clone()];
    while let Some(digest) = pending.pop() {
        let previous = match log.contents(&digest) {
            Ok(contents) => contents.previous().to_vec(),
            Err(Error::SegmentMissing(_)) => continue,
            Err(error) => return Err(error),
        };
        for name in previous {
            if seen.insert(name.clone()) {
                pending.push(name);
            }
        }
    }
    Ok(seen)
}

/// Take over the mail fetcher's memory from the other archive: where
/// each folder's fetch carries on. Without it the next fetch reads the
/// other archive's mailboxes whole, which costs time and nothing else.
///
/// Rows this archive's memory already has, by account and folder, stay
/// as they are. Returns the rows taken over, or `None` when the other
/// archive has no memory.
///
/// # Errors
///
/// [`Error::Io`] copying the file, [`Error::Index`] from `SQLite`.
pub fn take_memo(ours: &Archive, theirs: &Archive) -> Result<Option<usize>> {
    let from = theirs.root().join("cache").join(MAILVAULT_MEMO);
    if !from.is_file() {
        return Ok(None);
    }
    let cache = ours.root().join("cache");
    let to = cache.join(MAILVAULT_MEMO);
    if !to.exists() {
        let io = |context: String| move |source| Error::Io { context, source };
        fs::create_dir_all(&cache).map_err(io(format!("{}: creating", cache.display())))?;
        fs::copy(&from, &to).map_err(io(format!("{}: copying", from.display())))?;
        let connection = Connection::open(&to)?;
        let rows = tables(&connection, "main")?
            .iter()
            .map(|table| {
                connection.query_row(&format!("SELECT COUNT(*) FROM main.{table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
            })
            .sum::<std::result::Result<i64, _>>()?;
        let rows = usize::try_from(rows).unwrap_or(usize::MAX);
        return Ok(Some(rows));
    }
    let connection = Connection::open(&to)?;
    connection.execute(
        "ATTACH DATABASE ?1 AS theirs",
        [from.to_string_lossy().into_owned()],
    )?;
    let ours_tables = tables(&connection, "main")?;
    let mut rows = 0;
    connection.execute_batch("BEGIN")?;
    for table in tables(&connection, "theirs")? {
        if !ours_tables.contains(&table) {
            continue;
        }
        // Same program, same schema: the columns line up. A row for an
        // account and folder this archive knows is kept as it is here.
        rows += connection.execute(
            &format!("INSERT OR IGNORE INTO main.{table} SELECT * FROM theirs.{table}"),
            [],
        )?;
    }
    connection.execute_batch("COMMIT")?;
    Ok(Some(rows))
}

/// The tables of one attached database, by name.
fn tables(connection: &Connection, schema: &str) -> Result<Vec<String>> {
    let mut statement = connection.prepare(&format!(
        "SELECT name FROM {schema}.sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'"
    ))?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(names)
}

#[cfg(test)]
mod tests {
    use immure::Algorithm;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::audit::{Audit, Joined, audit_store};
    use crate::claim::{Attribute, Claim, Run, Source, Subject, Timestamp};
    use crate::index::{Index, Scope};

    fn archive(dir: &TempDir, name: &str) -> Archive {
        Archive::create(dir.path().join(name), Algorithm::Sha256).unwrap()
    }

    /// Bytes into the content store with one claim on the record.
    fn take(archive: &Archive, bytes: &[u8], at: &str) -> Subject {
        let (_, entry) = archive.content().add(bytes).unwrap();
        let subject = Subject::parse(entry.digest().as_str()).unwrap();
        let claim = Claim::assert(
            subject.clone(),
            Attribute::parse("file:size").unwrap(),
            json!(bytes.len()),
            Timestamp::parse(at).unwrap(),
            Source::parse("test").unwrap(),
            Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        )
        .unwrap();
        archive.log().append(&claim).unwrap();
        subject
    }

    fn audit(archive: &Archive) -> Audit {
        Audit::assemble(
            audit_store(archive.content()).unwrap(),
            audit_store(archive.derived()).unwrap(),
            audit_log(archive.log()).unwrap(),
        )
    }

    /// The whole merge, the way the command runs it.
    fn merge(ours: &Archive, theirs: &Archive) -> (Copied, Copied, Join) {
        theirs.log().seal().unwrap();
        let content = copy_store(theirs.content(), ours.content()).unwrap();
        let derived = copy_store(theirs.derived(), ours.derived()).unwrap();
        copy_store(theirs.log().store(), ours.log().store()).unwrap();
        let joined = join(ours, theirs).unwrap();
        (content, derived, joined)
    }

    #[test]
    fn the_other_archive_becomes_a_line_of_the_record() {
        let dir = TempDir::new().unwrap();
        let ours = archive(&dir, "a");
        let theirs = archive(&dir, "b");
        let shared = take(&ours, b"in both", "2026-09-05T00:00:00Z");
        ours.log().seal().unwrap();
        take(&ours, b"ours alone", "2026-09-06T00:00:00Z");
        take(&theirs, b"in both", "2026-09-01T00:00:00Z");
        theirs.log().seal().unwrap();
        let mail = take(&theirs, b"a mail", "2026-09-02T00:00:00Z");
        theirs.derived().add(b"its text").unwrap();

        assert_eq!(check(&ours, &theirs, false).unwrap(), Obstacles::default());
        assert_eq!(
            preview_copy(theirs.content(), ours.content()).unwrap(),
            Preview { new: 1, known: 1 }
        );
        let (content, derived, joined) = merge(&ours, &theirs);

        assert_eq!(content.copied, 1);
        assert_eq!(content.known, 1, "the shared file is not copied again");
        assert_eq!(derived.copied, 1);
        assert!(joined.changed);
        assert!(joined.sealed.is_some(), "our open claim was sealed");
        assert!(
            joined.sealed_theirs.is_none(),
            "theirs was sealed before the copy, as the command does it"
        );
        assert_eq!(joined.names.len(), 2);
        let report = audit(&ours);
        assert!(report.is_sound(), "{report:?}");
        assert_eq!(report.log.chains.len(), 2);
        assert_eq!(report.log.chains[0].joined, Some(Joined::Head));
        assert!(report.log.chains[1].open_head);
        assert!(report.log.breaks.is_empty());

        let mut index = Index::open(":memory:").unwrap();
        index.fold(ours.log()).unwrap();
        let size = Attribute::parse("file:size").unwrap();
        assert_eq!(
            index.values(&mail, &size, Scope::Held).unwrap(),
            vec![json!(6)],
            "their claims answer here"
        );
        assert_eq!(
            index.values(&shared, &size, Scope::Held).unwrap(),
            vec![json!(7)]
        );

        let again = join(&ours, &theirs).unwrap();
        assert!(!again.changed, "merged again, nothing changes");
        assert_eq!(again.names, joined.names);
        assert!(audit(&ours).is_sound());
    }

    #[test]
    fn a_line_merged_again_later_continues_where_it_was() {
        let dir = TempDir::new().unwrap();
        let ours = archive(&dir, "a");
        let theirs = archive(&dir, "b");
        take(&ours, b"ours", "2026-09-05T00:00:00Z");
        take(&theirs, b"theirs, first", "2026-09-01T00:00:00Z");
        let (_, _, first) = merge(&ours, &theirs);
        take(&ours, b"ours, later", "2026-09-07T00:00:00Z");
        take(&theirs, b"theirs, later", "2026-09-08T00:00:00Z");

        let (content, _, second) = merge(&ours, &theirs);

        assert_eq!(content.copied, 1);
        assert_eq!(content.known, 1);
        assert!(second.changed);
        assert_ne!(second.names, first.names);
        let report = audit(&ours);
        assert!(report.is_sound(), "{report:?}");
        assert_eq!(
            report.log.chains.len(),
            2,
            "their line runs on through the segment merged first"
        );
        assert_eq!(report.log.chains[0].segments.len(), 2);
        assert!(report.log.breaks.is_empty());
    }

    #[test]
    fn a_line_merged_again_without_claims_between_is_named_once() {
        let dir = TempDir::new().unwrap();
        let ours = archive(&dir, "a");
        let theirs = archive(&dir, "b");
        take(&ours, b"ours", "2026-09-05T00:00:00Z");
        take(&theirs, b"theirs, first", "2026-09-01T00:00:00Z");
        let (_, _, first) = merge(&ours, &theirs);
        take(&theirs, b"theirs, later", "2026-09-08T00:00:00Z");

        let (_, _, second) = merge(&ours, &theirs);

        assert!(second.sealed.is_none(), "nothing here to seal between");
        assert_eq!(second.names.len(), 2, "{:?}", second.names);
        assert_eq!(second.names[0], first.names[0], "our line's last segment");
        assert_ne!(second.names[1], first.names[1], "their new last segment");
        let report = audit(&ours);
        assert!(report.is_sound(), "{report:?}");
        assert_eq!(report.log.chains.len(), 2);
    }

    #[test]
    fn an_archive_with_another_hash_is_refused() {
        let dir = TempDir::new().unwrap();
        let ours = archive(&dir, "a");
        let theirs = Archive::create(dir.path().join("b"), Algorithm::Blake3).unwrap();

        assert!(matches!(
            check(&ours, &theirs, false),
            Err(Error::MergeAlgorithm { .. })
        ));
        assert!(matches!(
            check(&ours, &ours, false),
            Err(Error::MergeSelf(_))
        ));
        assert!(
            check(&ours, &theirs, true).unwrap().is_empty(),
            "with a rehash, different hashes are the point"
        );
        let same = archive(&dir, "c");
        assert!(matches!(
            check(&ours, &same, true),
            Err(Error::MergeSameHash(_))
        ));
    }

    #[test]
    fn an_archive_with_nothing_sealed_cannot_be_joined() {
        let dir = TempDir::new().unwrap();
        let ours = archive(&dir, "a");
        let theirs = archive(&dir, "b");

        assert!(matches!(join(&ours, &theirs), Err(Error::MergeNothing(_))));
    }

    #[test]
    fn a_broken_chain_and_a_clashing_account_are_obstacles() {
        let dir = TempDir::new().unwrap();
        let ours = archive(&dir, "a");
        let theirs = archive(&dir, "b");
        take(&theirs, b"before the loss", "2026-09-01T00:00:00Z");
        theirs.log().seal().unwrap();
        fs::remove_file(theirs.root().join("head.jsonl")).unwrap();
        take(&theirs, b"after the loss", "2026-09-02T00:00:00Z");
        fs::write(
            ours.root().join(MAILVAULT_CONFIG),
            "[[account]]\nname = \"work\"\nhost = \"imap.example.org\"\nuser = \"a\"\npassword = \"x\"\n\
             [[account]]\nname = \"home\"\nhost = \"imap.example.net\"\nuser = \"a\"\n",
        )
        .unwrap();
        fs::write(
            theirs.root().join(MAILVAULT_CONFIG),
            "[[account]]\nname = \"work\"\nhost = \"imap.example.com\"\nuser = \"a\"\n\
             [[account]]\nname = \"home\"\nhost = \"imap.example.net\"\nuser = \"a\"\npassword = \"y\"\nfolders = [\"INBOX\"]\n",
        )
        .unwrap();

        let obstacles = check(&ours, &theirs, false).unwrap();

        assert_eq!(obstacles.log_findings, 1, "the lost head");
        assert_eq!(
            obstacles.accounts,
            vec!["work".to_string()],
            "home differs only in secrets and folders"
        );
        assert!(!obstacles.is_empty());
    }

    #[test]
    fn a_damaged_file_in_the_other_archive_is_not_copied() {
        let dir = TempDir::new().unwrap();
        let ours = archive(&dir, "a");
        let theirs = archive(&dir, "b");
        let subject = take(&theirs, b"damaged later", "2026-09-01T00:00:00Z");
        let digest = Digest::parse(subject.as_str()).unwrap();
        let path = theirs.content().find(&digest).unwrap().unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        #[allow(
            clippy::permissions_set_readonly_false,
            reason = "the test plays the corruption"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(&path, permissions).unwrap();
        fs::write(&path, b"other bytes").unwrap();

        let copied = copy_store(theirs.content(), ours.content()).unwrap();

        assert_eq!(copied.damaged, vec![subject.as_str().to_string()]);
        assert_eq!(copied.copied, 0);
        assert!(
            !ours.content().contains(&digest).unwrap(),
            "nothing under the name"
        );
        assert_eq!(
            audit_store(ours.content()).unwrap().checked,
            0,
            "and the bytes read are not kept under their own name either"
        );
    }

    /// A claim with a link value: the derived file's origin.
    fn link(archive: &Archive, derived: &Subject, origin: &Subject, at: &str) {
        let claim = Claim::assert(
            derived.clone(),
            Attribute::parse("prov:origin").unwrap(),
            json!(origin.as_str()),
            Timestamp::parse(at).unwrap(),
            Source::parse("extractor:test/1").unwrap(),
            Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        )
        .unwrap();
        archive.log().append(&claim).unwrap();
    }

    #[test]
    fn an_archive_with_another_hash_is_rehashed_and_its_claims_replayed() {
        let dir = TempDir::new().unwrap();
        let ours = archive(&dir, "a");
        let theirs = Archive::create(dir.path().join("b"), Algorithm::Blake3).unwrap();
        take(&ours, b"ours", "2026-09-05T00:00:00Z");
        let mail = take(&theirs, b"a mail", "2026-09-01T00:00:00Z");
        theirs.log().seal().unwrap();
        let (_, text) = theirs.derived().add(b"its text").unwrap();
        let text = Subject::parse(text.digest().as_str()).unwrap();
        link(&theirs, &text, &mail, "2026-09-02T00:00:00Z");
        // A claim from before runs were recorded, written as a line.
        let old_line = format!(
            "{{\"subject\":\"{mail}\",\"attribute\":\"user:tag\",\"value\":\"old\",\"time\":\"2026-09-03T00:00:00Z\",\"source\":\"user\"}}"
        );
        theirs
            .log()
            .append(&Claim::parse_line(&old_line).unwrap())
            .unwrap();

        let mut names = BTreeMap::new();
        let content = rehash_store(theirs.content(), ours.content(), &mut names).unwrap();
        let derived = rehash_store(theirs.derived(), ours.derived(), &mut names).unwrap();
        let replayed = replay(&ours, &theirs, &names, false).unwrap();

        assert_eq!(content.copied, 1);
        assert_eq!(derived.copied, 1);
        assert_eq!(names.len(), 2);
        assert_eq!(
            replayed,
            Replayed {
                segments: 2,
                claims: 3,
                dropped: 0
            },
            "their sealed segment and their open segment, each as one here"
        );
        let report = audit(&ours);
        assert!(report.is_sound(), "{report:?}");
        assert_eq!(
            report.log.chains.len(),
            1,
            "no second line: the chain is ours"
        );
        let mut index = Index::open(":memory:").unwrap();
        index.fold(ours.log()).unwrap();
        let new_mail = Subject::parse(&names[mail.as_str()]).unwrap();
        let new_text = Subject::parse(&names[text.as_str()]).unwrap();
        assert_eq!(
            index
                .values(
                    &new_mail,
                    &Attribute::parse("file:size").unwrap(),
                    Scope::Held
                )
                .unwrap(),
            vec![json!(6)],
            "their claim answers under the new name"
        );
        assert_eq!(
            index
                .values(
                    &new_text,
                    &Attribute::parse("prov:origin").unwrap(),
                    Scope::Held
                )
                .unwrap(),
            vec![json!(new_mail.as_str())],
            "the link value was renamed too"
        );
        let replayed_lines: Vec<Claim> = ours
            .log()
            .segments()
            .unwrap()
            .iter()
            .flat_map(|segment| ours.log().read(segment.digest()).unwrap())
            .collect();
        let tag = replayed_lines
            .iter()
            .find(|claim| claim.attribute().as_str() == "user:tag")
            .unwrap();
        assert!(tag.run().is_none(), "a claim without a run stays without");
        assert_eq!(tag.time().as_str(), "2026-09-03T00:00:00Z");
    }

    #[test]
    fn a_claim_about_a_file_the_other_archive_lacks_stops_the_replay() {
        let dir = TempDir::new().unwrap();
        let ours = archive(&dir, "a");
        let theirs = Archive::create(dir.path().join("b"), Algorithm::Blake3).unwrap();
        let subject = take(&theirs, b"gone later", "2026-09-01T00:00:00Z");
        take(&theirs, b"kept", "2026-09-02T00:00:00Z");
        let digest = Digest::parse(subject.as_str()).unwrap();
        theirs.content().remove(&digest).unwrap();
        let mut names = BTreeMap::new();
        rehash_store(theirs.content(), ours.content(), &mut names).unwrap();

        assert!(matches!(
            replay(&ours, &theirs, &names, false),
            Err(Error::MergeMissing(1))
        ));
        assert!(ours.log().segments().unwrap().is_empty(), "nothing written");
        let replayed = replay(&ours, &theirs, &names, true).unwrap();
        assert_eq!(
            replayed,
            Replayed {
                segments: 1,
                claims: 1,
                dropped: 1
            }
        );
        assert!(audit(&ours).is_sound());
    }

    fn memo_with(path: &Path, rows: &[(&str, &str, u32)]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE resume (account TEXT NOT NULL, folder TEXT NOT NULL, state TEXT NOT NULL, PRIMARY KEY (account, folder));
                 CREATE TABLE pending (account TEXT NOT NULL, folder TEXT NOT NULL, id TEXT NOT NULL, detail TEXT NOT NULL, PRIMARY KEY (account, folder, id));",
            )
            .unwrap();
        for (account, folder, uid) in rows {
            connection
                .execute(
                    "INSERT INTO resume VALUES (?1, ?2, ?3)",
                    rusqlite::params![
                        account,
                        folder,
                        format!(r#"{{"uidvalidity":1,"uid":{uid}}}"#)
                    ],
                )
                .unwrap();
        }
    }

    fn resume(path: &Path) -> Vec<(String, String, u32)> {
        let connection = Connection::open(path).unwrap();
        let mut statement = connection
            .prepare(
                "SELECT account, folder, json_extract(state, '$.uid') FROM resume ORDER BY account, folder",
            )
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    }

    #[test]
    fn the_fetchers_memory_is_taken_over_where_ours_has_none() {
        let dir = TempDir::new().unwrap();
        let ours = archive(&dir, "a");
        let theirs = archive(&dir, "b");
        assert_eq!(take_memo(&ours, &theirs).unwrap(), None, "no memory there");
        let from = theirs.root().join("cache").join(MAILVAULT_MEMO);
        let to = ours.root().join("cache").join(MAILVAULT_MEMO);
        memo_with(&from, &[("work", "INBOX", 40), ("work", "Sent", 7)]);

        assert_eq!(take_memo(&ours, &theirs).unwrap(), Some(2), "copied whole");
        assert_eq!(resume(&to).len(), 2);

        fs::remove_file(&to).unwrap();
        memo_with(&to, &[("work", "INBOX", 100), ("home", "INBOX", 3)]);
        assert_eq!(
            take_memo(&ours, &theirs).unwrap(),
            Some(1),
            "one folder we did not know"
        );
        assert_eq!(
            resume(&to),
            vec![
                ("home".to_string(), "INBOX".to_string(), 3),
                ("work".to_string(), "INBOX".to_string(), 100),
                ("work".to_string(), "Sent".to_string(), 7),
            ],
            "our own resume point stays"
        );
    }
}
