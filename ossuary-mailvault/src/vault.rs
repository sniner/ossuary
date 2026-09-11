//! Taking over a mailvault archive — the Python one — whole.
//!
//! A mailvault archive holds every message as a file named by the hash
//! of its bytes, and beside them an append-only log of where each was
//! seen: one file per mailbox and folder, listing the messages observed
//! there, with the date the file was sealed. That is everything the
//! record needs. The messages go in through the archive's two-step
//! accession like a fetch's would, and each place the log names becomes
//! a `mailbox:place` claim — the same fact a fetch records, so the seam
//! does not show afterwards. The date the vault saw a place goes with
//! it as `mailbox:seen`: a fetch's sighting is its claim's own time, a
//! takeover's lies years before the claim.
//!
//! A takeover is long — hundreds of thousands of files, often over a
//! network share — and may be interrupted. The memo in `cache/`
//! remembers which places of which message are on the record, so the
//! next run carries on rather than reading everything again. A cache
//! like the ingest walk's: it informs the effort, never the truth. A
//! message that gained a place is read again and admitted again — the
//! store says the bytes are held, and the new place goes on the record
//! that way, never on a subject the memo remembered.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use ossuary_core::{Archive, Attribute, Sighting, Source, admit, record};
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

/// How many messages land between two commits of the memo.
const COMMIT_EVERY: usize = 500;

/// The header line of one log file: where its messages were seen, and
/// when the file was sealed.
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
            "{}: not a mailvault archive — its {MARK} mark would say so",
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
    let mut tally = Tally::new("take over");

    say.line("reading the vault's log: where every message was seen");
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
        seen: Attribute::parse(place::SEEN)?,
        memo: (!options.full).then_some(memo),
    };
    let total = gathered.places.len();
    let mut progress = say.progress();
    let mut done = 0;
    if let Some(memo) = takeover.memo {
        memo.begin()?;
    }
    for (store_id, places) in &gathered.places {
        done += 1;
        takeover.take(store_id, places, options.dry_run, &mut tally)?;
        if let Some(memo) = takeover.memo {
            if done % COMMIT_EVERY == 0 {
                memo.commit()?;
                memo.begin()?;
            }
        }
        progress.update(
            done,
            &format!(
                "taking over: {done} of {total} message(s), {} new to the archive",
                tally.stored
            ),
        );
    }
    progress.finish();
    if let Some(memo) = takeover.memo {
        memo.commit()?;
    }
    Ok(tally)
}

/// One takeover in progress: where from, where to, and what is
/// remembered — nothing, under `--full`.
struct Takeover<'a> {
    archive: &'a Archive,
    vault: &'a Path,
    source: Source,
    place: Attribute,
    seen: Attribute,
    memo: Option<&'a Memo>,
}

/// The places one message was seen in, each with the earliest date the
/// log has for it — `None` where the log file carried no date.
type Places = BTreeMap<String, Option<String>>;

impl Takeover<'_> {
    /// One message with its places: read and taken in with the places
    /// the record lacks, or left in peace when it lacks none.
    fn take(
        &self,
        store_id: &str,
        places: &Places,
        dry_run: bool,
        tally: &mut Tally,
    ) -> Result<()> {
        // What the memo says is on the record already: a place said
        // before is not said again, a message said with every place is
        // not even read.
        let said = self
            .memo
            .map(|memo| memo.said(store_id))
            .transpose()?
            .flatten();
        let (known, recorded) = match said {
            Some(recorded) => (true, recorded),
            None => (false, BTreeSet::new()),
        };
        let pending: Vec<(&String, &Option<String>)> = places
            .iter()
            .filter(|(place, _)| !recorded.contains(*place))
            .collect();
        if known && pending.is_empty() {
            tally.left += 1;
            return Ok(());
        }
        if dry_run {
            tally.would += 1;
            return Ok(());
        }
        let bytes = match read_message(self.vault, store_id) {
            Ok(bytes) => bytes,
            Err(error) => {
                tally.failed.push(format!("{error:#}"));
                return Ok(());
            }
        };
        let mut facts: Vec<(Attribute, serde_json::Value)> = pending
            .iter()
            .map(|(place, _)| (self.place.clone(), json!(place)))
            .collect();
        let dates: BTreeSet<&String> = pending
            .iter()
            .filter_map(|(_, date)| date.as_ref())
            .collect();
        facts.extend(
            dates
                .into_iter()
                .map(|date| (self.seen.clone(), json!(date))),
        );
        let admitted = admit(self.archive.content(), &bytes[..])?;
        tally.claims += record(
            self.archive.log(),
            &admitted,
            &Sighting {
                source: &self.source,
                run: &tally.run,
                mime: Some(crate::MESSAGE),
                facts: &facts,
                tags: &[],
            },
        )?;
        if admitted.is_new() {
            tally.stored += 1;
        } else {
            tally.known += 1;
        }
        if let Some(memo) = self.memo {
            let places: Vec<&String> = pending.iter().map(|(place, _)| *place).collect();
            memo.say(store_id, &places)?;
        }
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
    let listing = |dir: &Path| format!("{}: listing the log", dir.display());
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
        bail!("empty — not a log file");
    };
    let header: Header = serde_json::from_str(head).context("its header is not one")?;
    if !LOG_VERSIONS.contains(&header.version) {
        bail!(
            "written by a newer mailvault (log version {}) — upgrade this program to read it",
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
        // The first slash of a place ends the mailbox's name; a name
        // holding one would make every place of this file unreadable.
        if !valid_name(mailbox) {
            bail!(
                "its mailbox {mailbox:?} cannot name a place — letters, digits, '.', '_' \
                 and '-' only; rename it in the vault's log or leave it out"
            );
        }
    }
    gathered.logs += 1;
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
                places.insert(place.clone(), header.date.clone());
                gathered.sightings += 1;
            }
            // The same place in two files: the earlier date is when it
            // was first seen there.
            Some(seen) => {
                if let Some(date) = &header.date {
                    if seen.as_ref().is_none_or(|earlier| date < earlier) {
                        *seen = Some(date.clone());
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
        format!("{store_id}: the vault names this message and holds no file for it")
    })?;
    let raw = fs::read(&path).with_context(|| format!("{}: could not be read", path.display()))?;
    let bytes = if compressed {
        zstd::decode_all(&raw[..])
            .with_context(|| format!("{}: would not decompress", path.display()))?
    } else {
        raw
    };
    let digest = format!("{:x}", Sha384::digest(&bytes));
    if digest != store_id {
        bail!(
            "{}: damaged — the bytes do not answer to their name",
            path.display()
        );
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
        let mut index = bench.archive.index().unwrap();
        index.fold(bench.archive.log()).unwrap();
        let subject = Subject::parse(Algorithm::Sha256.hash(bytes).as_str()).unwrap();
        index
            .values(&subject, &Attribute::parse(attribute).unwrap())
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
            vec![json!("example.org/INBOX"), json!("example.org/\\Sent")],
            "both places, account and folder — the vault's own words"
        );
        assert_eq!(
            standing(&bench, ONE, "mailbox:seen"),
            vec![json!("2024-03-05T10:00:00+00:00")],
            "the dated file's date, verbatim; the undated file says nothing"
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
        assert_eq!(
            tally.claims, 4,
            "the place, its date, the run, the told kind"
        );
        assert!(tally.failed.is_empty());
        assert_eq!(
            standing(&bench, TWO, "mailbox:place"),
            vec![json!("example.org/Archive"), json!("example.org/INBOX")]
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
            standing(&bench, ONE, "mailbox:seen"),
            vec![json!("2023-06-01T08:00:00+00:00")],
            "seen in INBOX twice, first in 2023"
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
