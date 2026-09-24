//! Importing an archive of the Python mailvault.
//!
//! Such an archive stores every message as a file named by the SHA-384
//! of its bytes, and keeps a log of where each message was seen: one file
//! per mailbox, folder and backup run, listing the messages seen there,
//! with the time of the run in its header. Each message goes in through
//! the same two steps as a fetched one (admit, then record), and each
//! place the log names becomes a `mailbox:place` claim, as a fetch
//! records it.
//!
//! The claims carry the time from the log, not the time of the import. A
//! claim's time is when the sighting was recorded, and the Python
//! mailvault recorded it then. Claims with old times must not share a
//! segment with today's, because segments are ordered by the time of
//! their first claim (see `docs/format.md`). So the claims of a batch of
//! messages are written sorted by time, into a segment of their own.
//!
//! An import can take hours and may be interrupted. The memo in `cache/`
//! remembers which places of which message are recorded, so the next run
//! continues instead of reading everything again. It only saves work: a
//! message that gained a place is read and admitted again, and the new
//! place is recorded on the subject the store reports, never on one the
//! memo remembered.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use ossuary_core::{Admitted, Archive, Attribute, Sighting, Source, Timestamp, admit, record};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest as _, Sha384};

use crate::config::valid_name;
use crate::memo::Memo;
use crate::output::{Say, counted};
use crate::place;
use crate::tally::Tally;

/// The mark a mailvault archive carries in its root.
const MARK: &str = "FORMAT";
const MARK_LINE: &str = "mailvault archive format 1";

/// The log versions this build reads. Version 1 carries no chain
/// field; nothing here needs one.
const LOG_VERSIONS: [u32; 2] = [1, 2];

/// How many messages are admitted before their claims are written into a
/// segment of their own and the memo is committed.
const BATCH: usize = 500;

/// The header line of one log file: where its messages were seen, and
/// when: the start of the backup run, as Python's `isoformat()` of a UTC
/// time.
#[derive(Debug, Deserialize)]
struct Header {
    version: u32,
    mailbox: Option<String>,
    folder: Option<String>,
    date: Option<String>,
}

/// One message line of a log file.
#[derive(Debug, Deserialize)]
struct Line {
    store_id: String,
}

pub struct Options {
    /// Read every message anew, the memo notwithstanding.
    pub full: bool,
    /// Count what would be taken over, read nothing.
    pub dry_run: bool,
}

/// Whether `vault` is a mailvault archive — asked before anything is
/// opened or made on its account.
///
/// # Errors
///
/// A directory whose mark does not say so.
pub fn verify(vault: &Path) -> Result<()> {
    let mark = fs::read_to_string(vault.join(MARK)).ok();
    if mark.as_deref().map(str::trim) != Some(MARK_LINE) {
        bail!(
            "{}: not an archive of the Python mailvault; its {MARK} file is missing or not format 1",
            vault.display()
        );
    }
    Ok(())
}

/// Take the vault at `vault` over into the archive — the mailboxes
/// named in `names`, every one when none is.
///
/// # Errors
///
/// A directory that is not a mailvault archive, a log directory that
/// will not list, the memo refusing, or the archive refusing a message.
/// A single log file or message that will not read is named in the
/// tally, and the run goes on.
pub fn run(
    archive: &Archive,
    vault: &Path,
    names: &[String],
    memo: &Memo,
    options: &Options,
    say: Say,
) -> Result<Tally> {
    verify(vault)?;
    let mut tally = Tally::new("import");

    say.line("reading the log files");
    let gathered = gather(&vault.join("meta"), names, &mut tally)?;
    say.line(format_args!(
        "{} in {}, filed in {}",
        counted(gathered.places.len(), "message", "messages"),
        counted(gathered.logs, "log file", "log files"),
        counted(gathered.sightings, "place", "places")
    ));

    let takeover = Takeover {
        archive,
        vault,
        source: Source::parse(crate::SOURCE)?,
        place: Attribute::parse(place::ATTRIBUTE)?,
        memo: (!options.full).then_some(memo),
    };
    let total = gathered.places.len();
    let mut progress = say.progress();
    let mut done = 0;
    let mut batch = Vec::new();
    if let Some(memo) = takeover.memo {
        memo.begin()?;
    }
    for (store_id, places) in &gathered.places {
        done += 1;
        let step = match takeover.take(store_id, places, options.dry_run, &mut tally) {
            Ok(Some(pending)) => {
                batch.push(pending);
                if batch.len() < BATCH {
                    Ok(())
                } else {
                    takeover.settle(&mut batch, &mut tally)
                }
            }
            Ok(None) => Ok(()),
            Err(error) => Err(error),
        };
        if let Err(error) = step {
            // The messages admitted so far still get their claims, and the
            // memo keeps what was recorded.
            let saved = takeover
                .settle(&mut batch, &mut tally)
                .and_then(|()| takeover.memo.map_or(Ok(()), Memo::commit));
            return Err(match saved {
                Ok(()) => error,
                Err(more) => error.context(format!(
                    "and the import progress could not be saved: {more:#}"
                )),
            });
        }
        progress.update(
            done,
            &format!(
                "importing: {done} of {total} message(s), {} stored",
                tally.stored
            ),
        );
    }
    takeover.settle(&mut batch, &mut tally)?;
    progress.finish();
    if let Some(memo) = takeover.memo {
        memo.commit()?;
    }
    Ok(tally)
}

/// One import in progress: where from, where to, and what is
/// remembered (nothing, under `--full`).
struct Takeover<'a> {
    archive: &'a Archive,
    vault: &'a Path,
    source: Source,
    place: Attribute,
    memo: Option<&'a Memo>,
}

/// The places one message was seen in, each with the earliest time the
/// log has for it; `None` where no log file for that place had a usable
/// date.
type Places = BTreeMap<String, Option<Timestamp>>;

/// A message admitted and waiting for its claims, which are written for
/// a whole batch at once.
struct Pending {
    store_id: String,
    admitted: Admitted,
    /// The places not yet recorded, with their times from the log.
    places: Vec<(String, Option<Timestamp>)>,
}

impl Takeover<'_> {
    /// Read and admit one message whose places are not all recorded yet.
    /// `None` when there is nothing to do, in a dry run, and when the
    /// message could not be read (which the tally names).
    fn take(
        &self,
        store_id: &str,
        places: &Places,
        dry_run: bool,
        tally: &mut Tally,
    ) -> Result<Option<Pending>> {
        // A place the memo lists is not recorded again, and a message
        // with all its places recorded is not even read.
        let said = self
            .memo
            .map(|memo| memo.said(store_id))
            .transpose()?
            .flatten();
        let (known, recorded) = match said {
            Some(recorded) => (true, recorded),
            None => (false, BTreeSet::new()),
        };
        let places: Vec<(String, Option<Timestamp>)> = places
            .iter()
            .filter(|(place, _)| !recorded.contains(*place))
            .map(|(place, time)| (place.clone(), time.clone()))
            .collect();
        if known && places.is_empty() {
            tally.left += 1;
            return Ok(None);
        }
        if dry_run {
            tally.would += 1;
            return Ok(None);
        }
        let bytes = match read_message(self.vault, store_id) {
            Ok(bytes) => bytes,
            Err(error) => {
                tally.failed.push(format!("{error:#}"));
                return Ok(None);
            }
        };
        let admitted = admit(self.archive.content(), &bytes[..])?;
        if admitted.is_new() {
            tally.stored += 1;
        } else {
            tally.known += 1;
        }
        Ok(Some(Pending {
            store_id: store_id.to_string(),
            admitted,
            places,
        }))
    }

    /// Write the claims of a batch into a segment of their own, sorted by
    /// time, then remember what was recorded and commit the memo. One
    /// sighting per message and time: the first of a message also records
    /// the size of new bytes.
    fn settle(&self, batch: &mut Vec<Pending>, tally: &mut Tally) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let log = self.archive.log();
        // Whatever the head holds from today goes into a segment first.
        log.seal()?;
        let now = Timestamp::now();
        let mut sightings: Vec<(Timestamp, usize, Vec<&String>)> = Vec::new();
        for (index, pending) in batch.iter().enumerate() {
            let mut by_time: BTreeMap<Timestamp, Vec<&String>> = BTreeMap::new();
            for (place, time) in &pending.places {
                by_time
                    .entry(time.clone().unwrap_or_else(|| now.clone()))
                    .or_default()
                    .push(place);
            }
            if by_time.is_empty() {
                // No place at all: the message still gets its size and kind.
                by_time.insert(now.clone(), Vec::new());
            }
            sightings.extend(
                by_time
                    .into_iter()
                    .map(|(time, places)| (time, index, places)),
            );
        }
        sightings.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        let mut seen = vec![false; batch.len()];
        for (time, index, places) in &sightings {
            let pending = &batch[*index];
            let again;
            let admitted = if seen[*index] {
                again = pending.admitted.again();
                &again
            } else {
                &pending.admitted
            };
            seen[*index] = true;
            let facts: Vec<(Attribute, serde_json::Value)> = places
                .iter()
                .map(|place| (self.place.clone(), json!(place)))
                .collect();
            tally.claims += record(
                log,
                admitted,
                &Sighting {
                    source: &self.source,
                    run: &tally.run,
                    mime: Some(crate::MESSAGE),
                    facts: &facts,
                    tags: &[],
                    time: Some(time),
                },
            )?;
        }
        log.seal()?;
        if let Some(memo) = self.memo {
            for pending in batch.iter() {
                let places: Vec<&String> = pending.places.iter().map(|(place, _)| place).collect();
                memo.say(&pending.store_id, &places)?;
            }
            memo.commit()?;
            memo.begin()?;
        }
        batch.clear();
        Ok(())
    }
}

/// Every message the vault's log names, with every place it was seen
/// in — read whole before anything is taken in, so a log file that will
/// not read costs only itself.
struct Gathered {
    places: BTreeMap<String, Places>,
    logs: usize,
    sightings: usize,
}

fn gather(meta: &Path, names: &[String], tally: &mut Tally) -> Result<Gathered> {
    let mut gathered = Gathered {
        places: BTreeMap::new(),
        logs: 0,
        sightings: 0,
    };
    let listing = |dir: &Path| format!("{}: listing the log files", dir.display());
    let mut files = Vec::new();
    for shard in fs::read_dir(meta).with_context(|| listing(meta))? {
        let shard = shard.with_context(|| listing(meta))?.path();
        if !shard.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&shard).with_context(|| listing(&shard))? {
            let path = entry.with_context(|| listing(&shard))?.path();
            if path.extension().is_some_and(|ext| ext == "jsonl") {
                files.push(path);
            }
        }
    }
    files.sort();
    for path in files {
        match read_log(&path, names, &mut gathered) {
            Ok(()) => {}
            Err(error) => tally.failed.push(format!("{}: {error:#}", path.display())),
        }
    }
    Ok(gathered)
}

/// One log file into the gathering. `Ok` with nothing added is a file
/// of a mailbox not asked for.
fn read_log(path: &Path, names: &[String], gathered: &mut Gathered) -> Result<()> {
    let text = fs::read_to_string(path).context("could not be read")?;
    let mut lines = text.lines();
    let Some(head) = lines.next() else {
        bail!("empty, not a log file");
    };
    let header: Header = serde_json::from_str(head).context("invalid header line")?;
    if !LOG_VERSIONS.contains(&header.version) {
        bail!(
            "written by a newer version of the Python mailvault (log version {}); update ossuary-mailvault to read it",
            header.version
        );
    }
    if !names.is_empty()
        && !header
            .mailbox
            .as_ref()
            .is_some_and(|mailbox| names.contains(mailbox))
    {
        return Ok(());
    }
    if let Some(mailbox) = &header.mailbox {
        // The first colon of a place ends the mailbox's name; a name
        // holding one would make every place of this file unreadable.
        if !valid_name(mailbox) {
            bail!(
                "invalid mailbox name {mailbox:?} (letters, digits, '.', '_' and '-' only); \
                 rename it in the log file, or import only the other mailboxes by naming them"
            );
        }
    }
    gathered.logs += 1;
    // A date that names no moment leaves the place undated; its claims
    // then get the time of the import.
    let time = header
        .date
        .as_deref()
        .and_then(|date| Timestamp::from_rfc3339(date).ok());
    let place = match (&header.mailbox, &header.folder) {
        (Some(mailbox), Some(folder)) => Some(place::folder(mailbox, folder)),
        (Some(mailbox), None) => Some(place::account(mailbox)),
        (None, Some(folder)) => Some(place::orphan(folder)),
        (None, None) => None,
    };
    for line in lines {
        // A torn last line — two writers, a power cut — names nothing
        // reliably and is passed by, the way the vault itself reads it.
        let Ok(line) = serde_json::from_str::<Line>(line) else {
            continue;
        };
        if !line.store_id.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let places = gathered.places.entry(line.store_id).or_default();
        let Some(place) = &place else {
            continue;
        };
        match places.get_mut(place) {
            None => {
                places.insert(place.clone(), time.clone());
                gathered.sightings += 1;
            }
            // The same place in two files: the earlier time is when it
            // was first seen there.
            Some(seen) => {
                if let Some(time) = &time {
                    if seen.as_ref().is_none_or(|earlier| time < earlier) {
                        *seen = Some(time.clone());
                    }
                }
            }
        }
    }
    Ok(())
}

/// A message's bytes, read from the vault and held against their name.
/// Read whole: a mail is bounded in size, and the name can only be
/// checked once all of it is seen.
fn read_message(vault: &Path, store_id: &str) -> Result<Vec<u8>> {
    let (path, compressed) = message_path(vault, store_id).with_context(|| {
        format!("{store_id}: listed in the log, but the message file is missing")
    })?;
    let raw = fs::read(&path).with_context(|| format!("{}: could not be read", path.display()))?;
    let bytes = if compressed {
        zstd::decode_all(&raw[..])
            .with_context(|| format!("{}: decompression failed", path.display()))?
    } else {
        raw
    };
    let digest = format!("{:x}", Sha384::digest(&bytes));
    if digest != store_id {
        bail!("{}: damaged", path.display());
    }
    Ok(bytes)
}

/// Where the vault keeps a message: `mail/ab/cd/<id>.eml`, or the same
/// with `.zst` when it was stored compressed.
fn message_path(vault: &Path, store_id: &str) -> Option<(PathBuf, bool)> {
    if store_id.len() < 4 {
        return None;
    }
    let dir = vault
        .join("mail")
        .join(&store_id[..2])
        .join(&store_id[2..4]);
    let plain = dir.join(format!("{store_id}.eml"));
    if plain.is_file() {
        return Some((plain, false));
    }
    let packed = dir.join(format!("{store_id}.eml.zst"));
    if packed.is_file() {
        return Some((packed, true));
    }
    None
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use ossuary_core::{Algorithm, Subject};
    use tempfile::TempDir;

    use super::*;

    const ONE: &[u8] = b"From: a@example.org\r\nSubject: one\r\n\r\nfirst";
    const TWO: &[u8] = b"From: b@example.org\r\nSubject: two\r\n\r\nsecond";

    /// A small vault: two messages, one seen in two places, one
    /// compressed; the newer log file dated, the older not.
    fn vault(dir: &TempDir) -> PathBuf {
        let root = dir.path().join("vault");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("FORMAT"), "mailvault archive format 1\n").unwrap();
        let id_one = store(&root, ONE, false);
        let id_two = store(&root, TWO, true);
        log(
            &root,
            r#"{"version": 2, "mailbox": "example.org", "folder": "INBOX", "date": "2024-03-05T10:00:00+00:00", "prev": null}"#,
            &[&id_one, &id_two],
        );
        log(
            &root,
            r#"{"version": 1, "mailbox": "example.org", "folder": "\\Sent"}"#,
            &[&id_one],
        );
        root
    }

    fn store(root: &Path, bytes: &[u8], compressed: bool) -> String {
        let id = format!("{:x}", Sha384::digest(bytes));
        let dir = root.join("mail").join(&id[..2]).join(&id[2..4]);
        fs::create_dir_all(&dir).unwrap();
        if compressed {
            fs::write(
                dir.join(format!("{id}.eml.zst")),
                zstd::encode_all(bytes, 3).unwrap(),
            )
            .unwrap();
        } else {
            fs::write(dir.join(format!("{id}.eml")), bytes).unwrap();
        }
        id
    }

    fn log(root: &Path, header: &str, ids: &[&str]) {
        let mut text = format!("{header}\n");
        for id in ids {
            writeln!(text, "{{\"store_id\": \"{id}\"}}").unwrap();
        }
        let name = format!("{:x}", Sha384::digest(text.as_bytes()));
        let dir = root.join("meta").join(&name[..2]);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{name}.jsonl")), text).unwrap();
    }

    fn id(bytes: &[u8]) -> String {
        format!("{:x}", Sha384::digest(bytes))
    }

    struct Bench {
        _dir: TempDir,
        vault: PathBuf,
        archive: Archive,
        memo: Memo,
    }

    fn bench() -> Bench {
        let dir = TempDir::new().unwrap();
        let vault = vault(&dir);
        let archive = Archive::create(dir.path().join("archive"), Algorithm::Sha256).unwrap();
        let memo = Memo::open(&archive.root().join("cache").join(crate::memo::FILE_NAME)).unwrap();
        Bench {
            _dir: dir,
            vault,
            archive,
            memo,
        }
    }

    fn take(bench: &Bench, names: &[String], full: bool, dry_run: bool) -> Tally {
        run(
            &bench.archive,
            &bench.vault,
            names,
            &bench.memo,
            &Options { full, dry_run },
            Say::new(true),
        )
        .unwrap()
    }

    fn standing(bench: &Bench, bytes: &[u8], attribute: &str) -> Vec<serde_json::Value> {
        standing_as_of(bench, bytes, attribute, None)
    }

    /// The standing values, today or as the record stood at `cutoff`.
    fn standing_as_of(
        bench: &Bench,
        bytes: &[u8],
        attribute: &str,
        cutoff: Option<&str>,
    ) -> Vec<serde_json::Value> {
        let mut index = bench.archive.index().unwrap();
        index.fold(bench.archive.log()).unwrap();
        let index = match cutoff {
            Some(cutoff) => index.as_of(cutoff).unwrap(),
            None => index,
        };
        let subject = Subject::parse(Algorithm::Sha256.hash(bytes).as_str()).unwrap();
        index
            .values(
                &subject,
                &Attribute::parse(attribute).unwrap(),
                ossuary_core::Scope::Held,
            )
            .unwrap()
    }

    #[test]
    fn a_vault_is_taken_over_with_every_place_and_remembered() {
        let bench = bench();

        let first = take(&bench, &[], false, false);
        assert_eq!((first.stored, first.known, first.left), (2, 0, 0));
        assert!(first.failed.is_empty(), "{:?}", first.failed);

        assert_eq!(
            standing(&bench, ONE, "mailbox:place"),
            vec![json!("example.org:INBOX"), json!("example.org:\\Sent")],
            "both places, account and folder — the vault's own words"
        );
        assert_eq!(
            standing_as_of(&bench, ONE, "mailbox:place", Some("2024-03-05T10:00:00Z")),
            vec![json!("example.org:INBOX")],
            "recorded at the log's time; the undated place at the import's"
        );
        assert!(
            standing_as_of(&bench, ONE, "mailbox:place", Some("2024-03-05T09:59:59Z")).is_empty(),
            "nothing before the first sighting"
        );
        assert!(
            bench.archive.log().head().unwrap().is_empty(),
            "the claims with old times are sealed into a segment of their own"
        );
        assert_eq!(
            standing(&bench, ONE, "file:mime"),
            vec![json!("message/rfc822")],
            "told, not sniffed"
        );

        let again = take(&bench, &[], false, false);
        assert_eq!(
            (again.stored, again.known, again.left, again.claims),
            (0, 0, 2, 0),
            "the memo leaves what it knows in peace"
        );

        let full = take(&bench, &[], true, false);
        assert_eq!(
            (full.stored, full.known),
            (0, 2),
            "--full reads everything anew; the bytes are not stored twice"
        );
    }

    #[test]
    fn a_dry_run_counts_and_writes_nothing() {
        let bench = bench();
        let tally = take(&bench, &[], false, true);
        assert_eq!((tally.would, tally.claims), (2, 0));
        assert!(bench.archive.log().head().unwrap().is_empty());
    }

    #[test]
    fn a_new_place_of_a_known_message_is_said_after_reading_it_again() {
        let bench = bench();
        take(&bench, &[], false, false);
        log(
            &bench.vault,
            r#"{"version": 2, "mailbox": "example.org", "folder": "Archive", "date": "2025-01-01T00:00:00+00:00", "prev": null}"#,
            &[&id(TWO)],
        );

        let tally = take(&bench, &[], false, false);

        assert_eq!(
            (tally.stored, tally.known, tally.left),
            (0, 1, 1),
            "read again, held already: the store answers, not the memo"
        );
        assert_eq!(tally.claims, 2, "the place and the told kind");
        assert!(tally.failed.is_empty());
        assert_eq!(
            standing(&bench, TWO, "mailbox:place"),
            vec![json!("example.org:Archive"), json!("example.org:INBOX")]
        );
    }

    #[test]
    fn a_message_whose_bytes_are_gone_is_named_not_remembered() {
        let bench = bench();
        take(&bench, &[], false, false);
        log(
            &bench.vault,
            r#"{"version": 2, "mailbox": "example.org", "folder": "Archive", "date": null, "prev": null}"#,
            &[&id(TWO)],
        );
        fs::remove_dir_all(bench.vault.join("mail")).unwrap();

        let tally = take(&bench, &[], false, false);

        assert_eq!(
            (tally.stored, tally.known, tally.left, tally.claims),
            (0, 0, 1, 0)
        );
        assert_eq!(
            tally.failed.len(),
            1,
            "no bytes, no claim — the memo alone never answers"
        );
        assert_eq!(
            bench.memo.said(&id(TWO)).unwrap().unwrap().len(),
            1,
            "the place that was not said is not remembered as said"
        );
    }

    #[test]
    fn the_earliest_date_of_a_place_is_the_one_kept() {
        let bench = bench();
        log(
            &bench.vault,
            r#"{"version": 2, "mailbox": "example.org", "folder": "INBOX", "date": "2023-06-01T08:00:00+00:00", "prev": null}"#,
            &[&id(ONE)],
        );
        take(&bench, &[], false, false);
        assert_eq!(
            standing_as_of(&bench, ONE, "mailbox:place", Some("2023-06-01T08:00:00Z")),
            vec![json!("example.org:INBOX")],
            "seen in INBOX twice, first in 2023"
        );
        assert_eq!(
            standing_as_of(&bench, ONE, "file:size", Some("2023-06-01T08:00:00Z")),
            vec![json!(ONE.len())],
            "the size goes with the first sighting"
        );
    }

    #[test]
    fn a_damaged_message_is_named_and_the_run_goes_on() {
        let bench = bench();
        let id = id(ONE);
        let path = bench
            .vault
            .join("mail")
            .join(&id[..2])
            .join(&id[2..4])
            .join(format!("{id}.eml"));
        fs::write(&path, b"not what it was").unwrap();

        let tally = take(&bench, &[], false, false);
        assert_eq!(tally.stored, 1, "the other message went in");
        assert_eq!(tally.failed.len(), 1);
        assert!(tally.failed[0].contains("damaged"));
    }

    #[test]
    fn only_the_named_mailboxes_are_taken() {
        let bench = bench();
        let tally = take(&bench, &["other.org".to_string()], false, false);
        assert_eq!(tally.stored, 0);
    }

    #[test]
    fn a_mailbox_that_cannot_name_a_place_costs_its_log_file() {
        let bench = bench();
        log(
            &bench.vault,
            r#"{"version": 2, "mailbox": "a/b", "folder": "INBOX", "date": null, "prev": null}"#,
            &[&id(ONE)],
        );
        let tally = take(&bench, &[], false, false);
        assert_eq!(tally.stored, 2, "the other files are read as before");
        assert_eq!(tally.failed.len(), 1);
        assert!(tally.failed[0].contains("a/b"));
    }

    #[test]
    fn a_directory_that_is_no_vault_is_refused() {
        let bench = bench();
        assert!(verify(bench.archive.root()).is_err());
        assert!(
            run(
                &bench.archive,
                bench.archive.root(),
                &[],
                &bench.memo,
                &Options {
                    full: false,
                    dry_run: false
                },
                Say::new(true)
            )
            .is_err()
        );
    }
}
