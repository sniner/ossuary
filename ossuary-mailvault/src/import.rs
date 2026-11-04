//! Importing an archive of the Python mailvault.
//!
//! Such an archive stores every message as a file named by the hash of
//! its bytes (SHA-256, or SHA-384 in archives created before mailvault
//! 0.18), and keeps a log of where each message was seen: one file
//! per mailbox, folder and backup run, listing the messages seen there,
//! with the time of the run in its header. Each message goes in through
//! the same two steps as a fetched one (admit, then record), and each
//! place the log names becomes a claim.
//!
//! The Python mailvault records a label of Gmail or Proton Mail as one
//! more folder; a fetch records only the folders `mailvault.toml` lists.
//! So the places are matched against the accounts there, unless the
//! caller passes none. A folder in an account's list becomes a
//! `mailbox:place` under the name the list gives it, the value a fetch
//! of that folder records. A folder not in the list is a label and
//! becomes a `mailbox:tag` when the account is at Gmail, or when it is
//! under `Labels/` on any other IMAP account. The log does not name the
//! server, and asking it would need the server and the login, so every
//! IMAP server is taken to show labels as Proton Mail Bridge does. Every
//! other place becomes a `mailbox:place` as the log names it.
//!
//! A message whose places are all labels keeps its label folders as
//! places, as a fetch places such a message in its label folder; without
//! a place it would no longer be present.
//!
//! The claims carry the time from the log, not the time of the import. A
//! claim's time is when the sighting was recorded, and the Python
//! mailvault recorded it then. The claims of a batch of messages are
//! written sorted by time, into a segment of their own.
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
use ossuary_core::{
    Admitted, Algorithm, Archive, Attribute, Sighting, Source, Timestamp, admit, record,
};
use serde::Deserialize;
use serde_json::json;
use unicode_normalization::UnicodeNormalization as _;

use crate::config::{Account, Config, valid_name};
use crate::memo::Memo;
use crate::output::{Say, counted};
use crate::place;
use crate::proton;
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

/// The hashes the Python mailvault names messages in. Their digests
/// differ in length, so the length of a store id tells which one named
/// it. The `hash` setting in the vault's configuration is not read: the
/// file may live outside the vault, and without the setting the vault
/// is SHA-384.
const HASHES: [Algorithm; 2] = [Algorithm::Sha256, Algorithm::Sha384];

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
/// named in `names`, every one when none is. With `accounts`, the
/// places are matched against them (see the module documentation);
/// without, every place is recorded as the log names it.
///
/// # Errors
///
/// A directory that is not a mailvault archive, a log directory that
/// will not list, a mailbox with no account in `accounts`, the memo
/// refusing, or the archive refusing a message. A single log file or
/// message that will not read is named in the tally, and the run goes
/// on.
pub fn run(
    archive: &Archive,
    vault: &Path,
    names: &[String],
    accounts: Option<&Config>,
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
    let plan = match accounts {
        Some(config) => plan(config, &gathered)?,
        None => Plan::new(),
    };
    let mut labels: BTreeMap<&str, usize> = BTreeMap::new();
    for (logged, (mailbox, _)) in &gathered.folders {
        if let Some(Fact::Tag(_)) = plan.get(logged) {
            *labels.entry(mailbox.as_str()).or_default() += 1;
        }
    }
    for (mailbox, count) in labels {
        say.line(format_args!(
            "{mailbox}: {} not in the account's folders list, imported as labels",
            counted(count, "folder", "folders")
        ));
    }

    let takeover = Takeover {
        archive,
        vault,
        source: Source::parse(crate::SOURCE)?,
        place: Attribute::parse(place::ATTRIBUTE)?,
        tag: Attribute::parse(place::TAG)?,
        id: Attribute::parse(proton::ATTRIBUTE)?,
        plan,
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
    tag: Attribute,
    /// Proton's id of a message the Bridge handed over.
    id: Attribute,
    plan: Plan,
    memo: Option<&'a Memo>,
}

/// What a place of the log is recorded as.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Fact {
    /// A `mailbox:place` with this value.
    Place(String),
    /// A `mailbox:tag`: a label of Gmail or Proton Mail, by its name.
    Tag(String),
}

/// Which folders of an account, of those not in its list, are labels.
#[derive(Debug, Clone, Copy)]
enum Labels {
    /// At Gmail: every one.
    Gmail,
    /// Another IMAP server: those under `Labels/`, as on Proton Mail
    /// Bridge.
    Proton,
    /// Over MS Graph: none.
    None,
}

impl Labels {
    fn of(account: &Account) -> Result<Self> {
        Ok(if account.over_graph() {
            Self::None
        } else if account.on_gmail()? {
            Self::Gmail
        } else {
            Self::Proton
        })
    }

    /// The tag a folder not in the list is recorded as, if it is a label.
    fn label(self, folder: &str) -> Option<String> {
        match self {
            Self::Gmail => Some(place::gmail_label(folder)),
            Self::Proton => place::proton_label(folder).map(str::to_string),
            Self::None => None,
        }
    }
}

/// The places of the log that are not recorded as `mailbox:place` under
/// the name the log gives them, keyed by that name.
type Plan = BTreeMap<String, Fact>;

/// Match the folders of the log against the accounts.
///
/// # Errors
///
/// A mailbox of the log with no account, or a `host_cmd` failing.
fn plan(config: &Config, gathered: &Gathered) -> Result<Plan> {
    let missing: Vec<&str> = gathered
        .mailboxes
        .iter()
        .filter(|mailbox| !config.accounts.iter().any(|a| &a.name == *mailbox))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        bail!(
            "{}: no account for {} from the Python mailvault archive; add an account for each, \
             or import these by name with --no-auto-labels and the other mailboxes by name \
             without it",
            config.path.display(),
            missing.join(", ")
        );
    }
    // Per account of the log: which folders are labels, and its folders
    // list by compared name. No list means every folder is fetched, a
    // label folder included, so nothing is a label.
    let mut lists = BTreeMap::new();
    for account in &config.accounts {
        if !gathered.mailboxes.contains(&account.name) {
            continue;
        }
        let Some(folders) = &account.folders else {
            continue;
        };
        let folders: BTreeMap<String, &str> = folders
            .iter()
            .map(|folder| (compared(folder), folder.as_str()))
            .collect();
        lists.insert(account.name.as_str(), (Labels::of(account)?, folders));
    }
    let mut plan = Plan::new();
    for (logged, (mailbox, folder)) in &gathered.folders {
        let Some((labels, folders)) = lists.get(mailbox.as_str()) else {
            continue;
        };
        if let Some(listed) = folders.get(&compared(folder)) {
            let named = place::folder(mailbox, listed);
            if named != *logged {
                plan.insert(logged.clone(), Fact::Place(named));
            }
        } else if let Some(label) = labels.label(folder) {
            plan.insert(logged.clone(), Fact::Tag(label));
        }
    }
    Ok(plan)
}

/// A folder's name as compared with another: in NFC, and `INBOX` in any
/// case, as IMAP defines it. The server, the Python mailvault and the
/// person writing `mailvault.toml` need not agree on the Unicode form
/// of a name: `ö` is one code point in NFC, `o` and a combining
/// diaeresis in NFD.
fn compared(folder: &str) -> String {
    if folder.eq_ignore_ascii_case("INBOX") {
        return "INBOX".to_string();
    }
    folder.nfc().collect()
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
    /// Whether one of all its places, recorded or not, is not a label.
    /// If none is, its label folders are its places as well.
    elsewhere: bool,
    /// The `mailbox:id` its bytes carry, from Proton Mail Bridge.
    id: Option<String>,
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
            .map(|memo| memo.imported(store_id))
            .transpose()?
            .flatten();
        let (known, recorded) = match said {
            Some(recorded) => (true, recorded),
            None => (false, BTreeSet::new()),
        };
        let elsewhere = places
            .keys()
            .any(|place| !matches!(self.plan.get(place), Some(Fact::Tag(_))));
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
            elsewhere,
            id: proton::id_of(&bytes),
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
            let first = !seen[*index];
            let admitted = if first {
                &pending.admitted
            } else {
                again = pending.admitted.again();
                &again
            };
            seen[*index] = true;
            // Two names of the log can match one folder of the list.
            let facts: BTreeSet<Fact> = places
                .iter()
                .flat_map(|place| {
                    let fact = self
                        .plan
                        .get(*place)
                        .cloned()
                        .unwrap_or_else(|| Fact::Place((*place).clone()));
                    let kept = matches!(fact, Fact::Tag(_)) && !pending.elsewhere;
                    std::iter::once(fact).chain(kept.then(|| Fact::Place((*place).clone())))
                })
                .collect();
            let mut facts: Vec<(Attribute, serde_json::Value)> = facts
                .into_iter()
                .map(|fact| match fact {
                    Fact::Place(place) => (self.place.clone(), json!(place)),
                    Fact::Tag(label) => (self.tag.clone(), json!(label)),
                })
                .collect();
            // The id holds for the message, not for one sighting.
            if first && let Some(id) = &pending.id {
                facts.push((self.id.clone(), json!(id)));
            }
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
                memo.mark_imported(&pending.store_id, &places)?;
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
    /// Every mailbox the log files read name.
    mailboxes: BTreeSet<String>,
    /// Every place that names a mailbox and a folder, with the two.
    folders: BTreeMap<String, (String, String)>,
    logs: usize,
    sightings: usize,
}

fn gather(meta: &Path, names: &[String], tally: &mut Tally) -> Result<Gathered> {
    let mut gathered = Gathered {
        places: BTreeMap::new(),
        mailboxes: BTreeSet::new(),
        folders: BTreeMap::new(),
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
        (Some(mailbox), Some(folder)) => {
            let place = place::folder(mailbox, folder);
            gathered
                .folders
                .entry(place.clone())
                .or_insert_with(|| (mailbox.clone(), folder.clone()));
            Some(place)
        }
        (Some(mailbox), None) => Some(place::account(mailbox)),
        (None, Some(folder)) => Some(place::orphan(folder)),
        (None, None) => None,
    };
    if let Some(mailbox) = &header.mailbox {
        gathered.mailboxes.insert(mailbox.clone());
    }
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
                if let Some(time) = &time
                    && seen.as_ref().is_none_or(|earlier| time < earlier)
                {
                    *seen = Some(time.clone());
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
    let Some(hash) = HASHES
        .into_iter()
        .find(|hash| hash.hex_len() == store_id.len())
    else {
        bail!(
            "{store_id}: listed in the log, but neither a SHA-256 nor a SHA-384 hash; \
             if a newer version of the Python mailvault wrote it, update ossuary-mailvault"
        );
    };
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
    if hash.hash(&bytes).as_str() != store_id {
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
    const THREE: &[u8] = b"From: c@example.org\r\nSubject: three\r\n\r\nthird";

    /// A small vault named in `hash`: two messages, one seen in two
    /// places, one compressed; the newer log file dated, the older not.
    fn vault(dir: &TempDir, hash: Algorithm) -> PathBuf {
        let root = dir.path().join("vault");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("FORMAT"), "mailvault archive format 1\n").unwrap();
        let id_one = store(&root, hash, ONE, false);
        let id_two = store(&root, hash, TWO, true);
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

    fn store(root: &Path, hash: Algorithm, bytes: &[u8], compressed: bool) -> String {
        let id = hash.hash(bytes).as_str().to_string();
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
        let name = id(text.as_bytes());
        let dir = root.join("meta").join(&name[..2]);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{name}.jsonl")), text).unwrap();
    }

    /// The store id in the vault of [`bench`].
    fn id(bytes: &[u8]) -> String {
        Algorithm::Sha384.hash(bytes).as_str().to_string()
    }

    struct Bench {
        _dir: TempDir,
        vault: PathBuf,
        archive: Archive,
        memo: Memo,
    }

    /// A vault named in SHA-384, as every vault before mailvault 0.18.
    fn bench() -> Bench {
        bench_in(Algorithm::Sha384)
    }

    fn bench_in(hash: Algorithm) -> Bench {
        let dir = TempDir::new().unwrap();
        let vault = vault(&dir, hash);
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
            None,
            &bench.memo,
            &Options { full, dry_run },
            Say::new(true),
        )
        .unwrap()
    }

    /// An import matched against the accounts in `toml`.
    fn take_matched(bench: &Bench, toml: &str) -> Result<Tally> {
        let path = bench.vault.with_file_name("mailvault.toml");
        fs::write(&path, toml).unwrap();
        let config = crate::config::Source::Named(path).load().unwrap();
        run(
            &bench.archive,
            &bench.vault,
            &[],
            Some(&config),
            &bench.memo,
            &Options {
                full: false,
                dry_run: false,
            },
            Say::new(true),
        )
    }

    /// `example.org` fetches its INBOX, `gmail.com` its All Mail and one
    /// label folder, named in NFC.
    const ACCOUNTS: &str = "[[account]]\nname = \"example.org\"\nhost = \"imap.example.org\"\n\
         user = \"john\"\nfolders = [\"INBOX\"]\n\
         [[account]]\nname = \"gmail.com\"\nhost = \"imap.gmail.com\"\nuser = \"john\"\n\
         folders = [\"[Gmail]/All Mail\", \"Pers\u{f6}nlich\"]\n";

    /// The vault of [`bench`] with a Gmail mailbox: message one in All
    /// Mail, under the labels `\Inbox` and `Persönlich` (in NFD), and in
    /// `inbox` of example.org.
    fn gmail_bench() -> Bench {
        let bench = bench();
        for folder in ["[Gmail]/All Mail", "\\\\Inbox", "Perso\u{308}nlich"] {
            log(
                &bench.vault,
                &format!(
                    r#"{{"version": 2, "mailbox": "gmail.com", "folder": "{folder}", "date": "2024-04-01T00:00:00+00:00", "prev": null}}"#
                ),
                &[&id(ONE)],
            );
        }
        log(
            &bench.vault,
            r#"{"version": 2, "mailbox": "example.org", "folder": "inbox", "date": "2024-04-01T00:00:00+00:00", "prev": null}"#,
            &[&id(ONE)],
        );
        bench
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
    fn a_message_from_proton_mail_bridge_gets_its_proton_id() {
        let bench = bench();
        let built: &[u8] =
            b"X-Pm-Internal-Id: abc==\r\nFrom: d@example.org\r\nSubject: four\r\n\r\nfourth";
        let four = store(&bench.vault, Algorithm::Sha384, built, false);
        for date in ["2024-03-06T10:00:00+00:00", "2024-03-07T10:00:00+00:00"] {
            log(
                &bench.vault,
                &format!(
                    r#"{{"version": 2, "mailbox": "example.org", "folder": "Archive {date}", "date": "{date}", "prev": null}}"#
                ),
                &[&four],
            );
        }
        let tally = take(&bench, &[], false, false);
        assert!(tally.failed.is_empty(), "{:?}", tally.failed);
        assert_eq!(
            standing(&bench, built, "mailbox:id"),
            vec![json!("proton:abc==")]
        );
        assert!(
            standing(&bench, ONE, "mailbox:id").is_empty(),
            "no header, no id"
        );
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
            bench.memo.imported(&id(TWO)).unwrap().unwrap().len(),
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
    fn a_sha256_vault_is_imported() {
        let bench = bench_in(Algorithm::Sha256);
        let tally = take(&bench, &[], false, false);
        assert_eq!((tally.stored, tally.known), (2, 0));
        assert!(tally.failed.is_empty(), "{:?}", tally.failed);
        assert_eq!(
            standing(&bench, ONE, "mailbox:place"),
            vec![json!("example.org:INBOX"), json!("example.org:\\Sent")]
        );
    }

    #[test]
    fn a_store_id_in_no_known_hash_fails_alone() {
        let bench = bench();
        // As long as a SHA-1 digest.
        let store_id = "ab".repeat(20);
        log(
            &bench.vault,
            r#"{"version": 2, "mailbox": "example.org", "folder": "INBOX", "date": null, "prev": null}"#,
            &[&store_id],
        );
        let tally = take(&bench, &[], false, false);
        assert_eq!(tally.stored, 2);
        assert_eq!(tally.failed.len(), 1);
        assert!(tally.failed[0].contains("neither a SHA-256 nor a SHA-384 hash"));
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
                None,
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

    #[test]
    fn gmail_labels_become_tags_and_folders_take_the_name_of_the_list() {
        let bench = gmail_bench();
        let tally = take_matched(&bench, ACCOUNTS).unwrap();
        assert!(tally.failed.is_empty(), "{:?}", tally.failed);

        assert_eq!(
            standing(&bench, ONE, "mailbox:place"),
            vec![
                json!("example.org:INBOX"),
                json!("example.org:\\Sent"),
                json!("gmail.com:Pers\u{f6}nlich"),
                json!("gmail.com:[Gmail]/All Mail"),
            ],
            "inbox is INBOX, NFD is the list's NFC, and \\Sent stays: example.org is not at Gmail"
        );
        assert_eq!(
            standing(&bench, ONE, "mailbox:tag"),
            vec![json!("/Inbox")],
            "the label not in the list, Gmail's own with a slash"
        );
        assert_eq!(
            standing_as_of(&bench, ONE, "mailbox:tag", Some("2024-04-01T00:00:00Z")),
            vec![json!("/Inbox")],
            "at the time of the log"
        );
        assert!(
            standing_as_of(&bench, ONE, "mailbox:tag", Some("2024-03-31T23:59:59Z")).is_empty()
        );
        assert!(standing(&bench, TWO, "mailbox:tag").is_empty());
    }

    #[test]
    fn a_gmail_message_under_labels_only_keeps_them_as_places() {
        let bench = gmail_bench();
        store(&bench.vault, Algorithm::Sha384, THREE, false);
        log(
            &bench.vault,
            r#"{"version": 2, "mailbox": "gmail.com", "folder": "Sales", "date": "2024-04-01T00:00:00+00:00", "prev": null}"#,
            &[&id(THREE)],
        );
        take_matched(&bench, ACCOUNTS).unwrap();

        assert_eq!(
            standing(&bench, THREE, "mailbox:place"),
            vec![json!("gmail.com:Sales")],
            "no other place, so the label folder stays one"
        );
        assert_eq!(standing(&bench, THREE, "mailbox:tag"), vec![json!("Sales")]);
        assert!(
            !standing(&bench, ONE, "mailbox:place").contains(&json!("gmail.com:\\Inbox")),
            "message one is in All Mail"
        );
    }

    /// `proton.me` fetches All Mail, behind a host that does not name it.
    const PROTON: &str = "[[account]]\nname = \"proton.me\"\nhost = \"127.0.0.1\"\n\
         user = \"john\"\nfolders = [\"All Mail\"]\n";

    fn proton_log(bench: &Bench, folder: &str, ids: &[&str]) {
        log(
            &bench.vault,
            &format!(
                r#"{{"version": 2, "mailbox": "proton.me", "folder": "{folder}", "date": "2024-04-01T00:00:00+00:00", "prev": null}}"#
            ),
            ids,
        );
    }

    #[test]
    fn a_proton_label_folder_becomes_a_tag_and_a_place_only_without_another() {
        let bench = bench();
        let three = store(&bench.vault, Algorithm::Sha384, THREE, false);
        proton_log(&bench, "All Mail", &[&id(ONE)]);
        proton_log(&bench, "Folders/Work", &[&id(ONE)]);
        proton_log(&bench, "Labels/Sales", &[&id(ONE), &three]);
        proton_log(&bench, "Labels/Alias", &[&three]);
        let accounts = format!("{ACCOUNTS}{PROTON}");
        take_matched(&bench, &accounts).unwrap();

        assert_eq!(
            standing(&bench, ONE, "mailbox:place"),
            vec![
                json!("example.org:INBOX"),
                json!("example.org:\\Sent"),
                json!("proton.me:All Mail"),
                json!("proton.me:Folders/Work"),
            ],
            "a folder not under Labels/ is no label"
        );
        assert_eq!(standing(&bench, ONE, "mailbox:tag"), vec![json!("Sales")]);
        assert_eq!(
            standing(&bench, THREE, "mailbox:place"),
            vec![
                json!("proton.me:Labels/Alias"),
                json!("proton.me:Labels/Sales")
            ],
            "no other place, so the label folders stay places"
        );
        assert_eq!(
            standing(&bench, THREE, "mailbox:tag"),
            vec![json!("Alias"), json!("Sales")]
        );

        // A label added later: message one has its place from the first
        // import, which the memo skips.
        proton_log(&bench, "Labels/Gmail", &[&id(ONE)]);
        take_matched(&bench, &accounts).unwrap();
        assert!(
            !standing(&bench, ONE, "mailbox:place").contains(&json!("proton.me:Labels/Gmail")),
            "All Mail places it"
        );
        assert_eq!(
            standing(&bench, ONE, "mailbox:tag"),
            vec![json!("Gmail"), json!("Sales")]
        );
    }

    #[test]
    fn a_gmail_account_without_a_folders_list_keeps_every_place() {
        let bench = gmail_bench();
        let accounts = ACCOUNTS.replace(
            "folders = [\"[Gmail]/All Mail\", \"Pers\u{f6}nlich\"]\n",
            "",
        );
        take_matched(&bench, &accounts).unwrap();
        assert!(standing(&bench, ONE, "mailbox:tag").is_empty());
        assert!(
            standing(&bench, ONE, "mailbox:place").contains(&json!("gmail.com:\\Inbox")),
            "every folder is fetched, a label folder included"
        );
    }

    #[test]
    fn a_mailbox_without_an_account_is_named_before_anything_is_written() {
        let bench = gmail_bench();
        let only_gmail = &ACCOUNTS[ACCOUNTS.find("[[account]]\nname = \"gmail.com\"").unwrap()..];
        let refused = take_matched(&bench, only_gmail).err().unwrap();
        let message = format!("{refused:#}");
        assert!(message.contains("no account for example.org"), "{message}");
        assert!(message.contains("--no-auto-labels"), "{message}");
        assert!(bench.archive.log().head().unwrap().is_empty());
        assert!(standing(&bench, ONE, "mailbox:place").is_empty());
    }
}
