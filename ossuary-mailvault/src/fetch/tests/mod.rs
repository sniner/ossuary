//! The fetch against a mailbox answering from memory, and what the
//! tests of each kind of folder share.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;

use anyhow::{Result, anyhow};
use ossuary_core::{Algorithm, Archive, Attribute, Claim, Scope, Source, Subject, Timestamp};
use serde_json::json;
use tempfile::TempDir;

use super::bridge::InMailbox;
use super::*;
use crate::compare;
use crate::config::{self, Account, Reach};
use crate::graph::Graph;
use crate::memo::{ImapResume, Memo, Pending};
use crate::output::Say;
use crate::place;
use crate::proton;
use crate::remote::{Each, Flavour, Folder, Got, Mailbox, Stop};
use crate::tally::Tally;

mod bridge;
mod comparing;
mod graph_folder;
mod imap_folder;

fn at(uidvalidity: u32, uid: u32) -> ImapResume {
    ImapResume { uidvalidity, uid }
}

/// A mailbox answering from memory: one folder, its messages by UID,
/// handed back newest first — the order a server may choose.
struct Fake {
    uidvalidity: u32,
    messages: BTreeMap<u32, Vec<u8>>,
    /// Stop after this many messages, the way a dropped connection
    /// would.
    give_up_after: Option<usize>,
    /// Messages the folder holds and the server will not hand over.
    refused: BTreeSet<u32>,
    /// Message-IDs the server lists in place of the one in the
    /// message.
    listed_as: BTreeMap<u32, String>,
    flavour: Flavour,
    /// The labels of each message, handed over on Gmail.
    labels: BTreeMap<u32, Vec<String>>,
    /// The UIDs whole messages were asked for, in order.
    fetched: Vec<u32>,
    /// The UIDs a header field was asked for, in order.
    headed: Vec<u32>,
    /// The folders the server lists. Each holds the messages above.
    folders: Vec<String>,
    /// Folders that will not open.
    unopenable: BTreeSet<String>,
    /// The folders opened, in order.
    examined: Vec<String>,
}

impl Mailbox for Fake {
    fn folders(&mut self) -> Result<Vec<String>> {
        Ok(self.folders.clone())
    }

    fn examine(&mut self, folder: &str) -> Result<Folder> {
        self.examined.push(folder.to_string());
        if self.unopenable.contains(folder) {
            return Err(anyhow!("{folder} could not be opened"));
        }
        Ok(Folder {
            uidvalidity: self.uidvalidity,
        })
    }

    fn flavour(&self) -> Flavour {
        self.flavour
    }

    fn uids_above(&mut self, above: u32) -> Result<Vec<u32>> {
        Ok(self
            .messages
            .keys()
            .copied()
            .filter(|&uid| uid > above)
            .collect())
    }

    fn header(&mut self, uids: &[u32], name: &str) -> Result<Vec<(u32, String)>> {
        self.headed.extend_from_slice(uids);
        Ok(uids
            .iter()
            .filter_map(|uid| {
                let bytes = self.messages.get(uid)?;
                let listed = self
                    .listed_as
                    .get(uid)
                    .filter(|_| name.eq_ignore_ascii_case(compare::HEADER));
                let value = listed
                    .cloned()
                    .unwrap_or_else(|| crate::remote::header_value(bytes, name));
                Some((*uid, value))
            })
            .collect())
    }

    fn fetch(&mut self, uids: &[u32], each: &mut Each<'_>) -> Result<ControlFlow<()>, Stop> {
        self.fetched.extend_from_slice(uids);
        for (handed, uid) in uids.iter().rev().enumerate() {
            if self.give_up_after == Some(handed) {
                return Err(Stop::Server(anyhow!("connection dropped")));
            }
            let got = match self.messages.get(uid) {
                _ if self.refused.contains(uid) => Got::Refused(anyhow!("UID {uid}: NO")),
                Some(bytes) => Got::Message(
                    bytes,
                    (self.flavour == Flavour::Gmail)
                        .then(|| self.labels.get(uid).cloned().unwrap_or_default()),
                ),
                None => Got::Gone,
            };
            if each(*uid, got).map_err(Stop::Caller)?.is_break() {
                return Ok(ControlFlow::Break(()));
            }
        }
        Ok(ControlFlow::Continue(()))
    }
}

fn mailbox(uidvalidity: u32, uids: &[u32]) -> Fake {
    Fake {
        uidvalidity,
        messages: uids
            .iter()
            .map(|&uid| {
                (
                    uid,
                    format!("Subject: {uid}\r\n\r\nbody {uid}").into_bytes(),
                )
            })
            .collect(),
        give_up_after: None,
        refused: BTreeSet::new(),
        listed_as: BTreeMap::new(),
        flavour: Flavour::Standard,
        labels: BTreeMap::new(),
        fetched: Vec::new(),
        headed: Vec::new(),
        folders: Vec::new(),
        unopenable: BTreeSet::new(),
        examined: Vec::new(),
    }
}

struct Bench {
    _dir: TempDir,
    archive: Archive,
    memo: Memo,
    account: Account,
}

fn bench() -> Bench {
    let dir = TempDir::new().unwrap();
    let archive = Archive::create(dir.path().join("archive"), Algorithm::Sha256).unwrap();
    let memo = Memo::open(&archive.root().join("cache").join(crate::memo::FILE_NAME)).unwrap();
    Bench {
        _dir: dir,
        archive,
        memo,
        account: toml::from_str(
            "name = \"example.org\"\nhost = \"imap.example.org\"\nuser = \"john\"\n",
        )
        .unwrap(),
    }
}

fn fetcher<'a>(bench: &'a Bench, options: &'a Options) -> Fetch<'a> {
    Fetch {
        archive: &bench.archive,
        memo: &bench.memo,
        options,
        source: Source::parse(crate::SOURCE).unwrap(),
        place: Attribute::parse(place::ATTRIBUTE).unwrap(),
        tag: Attribute::parse(place::TAG).unwrap(),
        id: Attribute::parse(proton::ATTRIBUTE).unwrap(),
        say: Say::new(true),
    }
}

/// The state of a fetch whose index is caught up to the log already.
fn caught(bench: &Bench) -> State {
    let mut state = State::new();
    state
        .standing
        .refresh(&bench.archive, Say::new(true))
        .unwrap();
    state
}

/// One folder read on its own, as the only folder of its account.
fn read(fetch: &Fetch<'_>, bench: &Bench, mailbox: &mut Fake, folder: &str, state: &mut State) {
    let place = Place::new(&bench.account.name, folder);
    fetch
        .read_folder(&place, mailbox, &mut InMailbox::new(), state)
        .unwrap();
}

fn options(full: bool, dry_run: bool) -> Options {
    Options {
        full,
        compare: false,
        dry_run,
    }
}

fn fetch(bench: &Bench, mailbox: &mut Fake, full: bool, dry_run: bool) -> Tally {
    let options = options(full, dry_run);
    let fetch = fetcher(bench, &options);
    let mut state = caught(bench);
    read(&fetch, bench, mailbox, "INBOX", &mut state);
    state.tally
}

fn point(bench: &Bench) -> Option<ImapResume> {
    bench.memo.imap_resume("example.org", "INBOX").unwrap()
}

/// A message with this Message-ID.
fn mail(n: u32, message_id: &str) -> Vec<u8> {
    format!("Message-ID: {message_id}\r\nSubject: {n}\r\n\r\nbody {n}").into_bytes()
}

/// A folder holding these messages, by UID.
fn holding(uidvalidity: u32, messages: &[(u32, &[u8])]) -> Fake {
    let mut fake = mailbox(uidvalidity, &[]);
    fake.messages = messages
        .iter()
        .map(|(uid, bytes)| (*uid, bytes.to_vec()))
        .collect();
    fake
}

/// The Message-ID on the record, as `ossuary extract mail` writes it.
fn extracted(bench: &Bench, bytes: &[u8], message_id: &str) {
    let claim = Claim::assert(
        Subject::parse(Algorithm::Sha256.hash(bytes).as_str()).unwrap(),
        Attribute::parse(compare::MESSAGE_ID).unwrap(),
        json!(message_id),
        Timestamp::now(),
        Source::parse("extractor:mail/1").unwrap(),
        ossuary_core::Run::new(),
    )
    .unwrap();
    bench.archive.log().append(&claim).unwrap();
}

fn compare(bench: &Bench, mailbox: &mut Fake, dry_run: bool) -> Tally {
    let options = Options {
        full: false,
        compare: true,
        dry_run,
    };
    let fetch = fetcher(bench, &options);
    let mut state = caught(bench);
    read(&fetch, bench, mailbox, "INBOX", &mut state);
    state.tally
}

/// The server ids the memo keeps for `--compare`, sorted.
fn compared(bench: &Bench) -> Vec<String> {
    let mut ids: Vec<String> = bench
        .memo
        .compared("example.org", "INBOX")
        .unwrap()
        .into_keys()
        .collect();
    ids.sort();
    ids
}

/// The folder's pending list: UID and failed runs.
fn imap_pending(bench: &Bench) -> Vec<(String, u32)> {
    bench
        .memo
        .pending::<()>("example.org", "INBOX")
        .unwrap()
        .into_iter()
        .map(|message| (message.id, message.attempts))
        .collect()
}

/// The values standing on the message `bytes` under `attribute`.
fn held(bench: &Bench, bytes: &[u8], attribute: &str) -> Vec<String> {
    let index = caught_up(&bench.archive, Say::new(true)).unwrap();
    let subject = Subject::parse(Algorithm::Sha256.hash(bytes).as_str()).unwrap();
    let mut values: Vec<String> = index
        .values(&subject, &Attribute::parse(attribute).unwrap(), Scope::Held)
        .unwrap()
        .into_iter()
        .map(|value| value.as_str().unwrap().to_string())
        .collect();
    values.sort();
    values
}

/// A Gmail folder holding these messages, with these labels.
fn labeled(uidvalidity: u32, messages: &[(u32, &[u8], &[&str])]) -> Fake {
    let mut fake = mailbox(uidvalidity, &[]);
    fake.messages = messages
        .iter()
        .map(|(uid, bytes, _)| (*uid, bytes.to_vec()))
        .collect();
    fake.flavour = Flavour::Gmail;
    fake.labels = messages
        .iter()
        .map(|(uid, _, labels)| (*uid, labels.iter().map(ToString::to_string).collect()))
        .collect();
    fake
}

/// A fetch of INBOX as `run` makes one for an IMAP account: the
/// record is read only once the folder turns out to have labels.
fn fetch_labeled(bench: &Bench, mailbox: &mut Fake, full: bool) -> Tally {
    let options = options(full, false);
    let fetch = fetcher(bench, &options);
    let mut state = State::new();
    read(&fetch, bench, mailbox, "INBOX", &mut state);
    state.tally
}

/// A Proton Mail Bridge folder holding these messages.
fn proton(uidvalidity: u32, messages: &[(u32, &[u8])]) -> Fake {
    let mut fake = holding(uidvalidity, messages);
    fake.flavour = Flavour::Proton;
    fake
}

/// A message as Proton Mail Bridge builds it: Proton's id, and the
/// id of the Bridge's local database, which a rebuild changes.
fn built(n: u32, local: &str) -> Vec<u8> {
    format!(
        "X-Pm-Gluon-Id: {local}\r\nX-Pm-Internal-Id: id{n}==\r\n\
             Message-ID: <{n}@example.org>\r\nSubject: {n}\r\n\r\nbody {n}"
    )
    .into_bytes()
}

/// One fetch, folder after folder, as `run` makes it for an IMAP
/// account: the record is read once a folder needs it.
fn run_folders(bench: &Bench, folders: &mut [(&str, &mut Fake)], options: &Options) -> Tally {
    let fetch = fetcher(bench, options);
    let mut state = State::new();
    let mut in_mailbox = InMailbox::new();
    for (folder, mailbox) in folders.iter_mut() {
        let place = Place::new(&bench.account.name, folder);
        fetch
            .read_folder(&place, *mailbox, &mut in_mailbox, &mut state)
            .unwrap();
    }
    assert!(state.tally.failed.is_empty(), "{:?}", state.tally.failed);
    state.tally
}

/// A claim on the record about a message, as another run left it.
fn claimed(bench: &Bench, bytes: &[u8], attribute: &str, value: &str, retracted: bool) {
    let subject = Subject::parse(Algorithm::Sha256.hash(bytes).as_str()).unwrap();
    let attribute = Attribute::parse(attribute).unwrap();
    let source = Source::parse(crate::SOURCE).unwrap();
    let run = ossuary_core::Run::new();
    let claim = if retracted {
        let mut index = bench.archive.index().unwrap();
        index.fold(bench.archive.log()).unwrap();
        let ids = index
            .standing_ids(&subject, &attribute, Some(&json!(value)))
            .unwrap();
        Claim::retract(subject, attribute, ids, Timestamp::now(), source, run)
    } else {
        Claim::assert(
            subject,
            attribute,
            json!(value),
            Timestamp::now(),
            source,
            run,
        )
    }
    .unwrap();
    bench.archive.log().append(&claim).unwrap();
}

/// One run over All Mail and then the label folder `Labels/Sales`.
fn with_label(bench: &Bench, all: &mut Fake, sales: &mut Fake, options: &Options) -> Tally {
    let fetch = fetcher(bench, options);
    let mut state = State::new();
    let mut in_mailbox = InMailbox::new();
    fetch
        .read_folder(
            &Place::new("example.org", "All Mail"),
            all,
            &mut in_mailbox,
            &mut state,
        )
        .unwrap();
    fetch
        .label_folder(
            &Place::new("example.org", "Labels/Sales"),
            "Sales",
            sales,
            &in_mailbox,
            &mut state,
        )
        .unwrap();
    assert!(state.tally.failed.is_empty(), "{:?}", state.tally.failed);
    state.tally
}

/// The UIDs of `Labels/Sales` left open, and its resume point.
fn sales(bench: &Bench) -> (Vec<u32>, Option<ImapResume>) {
    let mut open: Vec<u32> = bench
        .memo
        .pending::<()>("example.org", "Labels/Sales")
        .unwrap()
        .iter()
        .map(|entry| entry.id.parse().unwrap())
        .collect();
    open.sort_unstable();
    let point = bench
        .memo
        .imap_resume("example.org", "Labels/Sales")
        .unwrap();
    (open, point)
}
