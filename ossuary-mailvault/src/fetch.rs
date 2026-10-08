//! Fetching: every configured mailbox, folder by folder, whatever
//! arrived since the last run.
//!
//! Over IMAP a folder is opened read-only and its resume point looked
//! up in the memo: the UIDVALIDITY the server promised last time, and
//! the highest UID fetched under it. If the promise still holds, the
//! server is asked only for what lies above that UID and answers with
//! the new messages alone. If the promise changed, or there is no
//! resume point, the folder is fetched whole: the bytes dedup in the
//! store, and the places are said again.
//!
//! Over MS Graph the resume point is the delta link the server handed
//! out at the end of the last round: the next round starts from it and
//! is handed only what changed. A link the server no longer honours
//! costs one whole round; so does a link that was never handed out.
//! A round only lists ids; the link is saved once the listing is
//! complete, and the ids go on the folder's pending list in the memo.
//! The messages are then fetched one by one, each removed from the
//! list as it is stored. A run that stops early, and a message the
//! server did not return, leave entries on the list, and the next run
//! fetches those first. Graph also lists the categories of each
//! message, and those are recorded as `mailbox:tag`: as listed, and
//! retracted when a message is listed again without them, so the
//! record holds the categories as of the last listing.
//!
//! Either way each message goes in through the archive's two-step
//! accession with its place — account and folder — as the one fact
//! the fetcher has to tell about it. The resume point moves forward
//! only over what has landed: by batches over IMAP, by the pending
//! list over Graph. It lives in `cache/`: losing it costs one whole
//! fetch of the folder, never a claim.
//!
//! One account failing costs that account: the password, the login,
//! a folder that will not open are named in the tally, and the run
//! goes on to the next. The archive or the memo refusing ends the run.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::Result;
use ossuary_core::{
    Archive, Attribute, Claim, Index, Scope, Sighting, Source, Subject, Timestamp, Value, admit,
    record,
};
use serde_json::json;

use crate::config::{self, Account, Reach};
use crate::graph::{Graph, Halt, Miss, Round};
use crate::memo::{ImapResume, Memo, Pending};
use crate::output::{Say, counted};
use crate::place;
use crate::remote::{Mailbox, Remote, Stop};
use crate::tally::Tally;

pub struct Options {
    /// Fetch every message, the resume point notwithstanding.
    pub full: bool,
    /// Count what would be fetched, fetch nothing.
    pub dry_run: bool,
}

/// How many messages land between two commits of the resume point.
const COMMIT_EVERY: usize = 25;

/// How many runs may fail to fetch a pending message before it is
/// given up.
const GIVE_UP_AFTER: u32 = 3;

/// How many messages in a row the server may refuse before the rest
/// of the folder is left for the next run.
const STOP_AFTER: usize = 10;

/// Fetch the accounts into the archive.
///
/// # Errors
///
/// The archive or the memo refusing — everything else is named in the
/// tally and the run goes on.
pub fn run(
    archive: &Archive,
    accounts: &[&Account],
    memo: &Memo,
    options: &Options,
    say: Say,
) -> Result<Tally> {
    // Taking a mark back needs to know what stands, and only a Graph
    // account says marks — an IMAP run leaves the index alone.
    let record = if accounts.iter().any(|account| account.over_graph()) {
        Some(caught_up(archive, say)?)
    } else {
        None
    };
    let fetch = Fetch {
        archive,
        memo,
        options,
        source: Source::parse(crate::SOURCE)?,
        place: Attribute::parse(place::ATTRIBUTE)?,
        tag: Attribute::parse(place::TAG)?,
        record,
        said: RefCell::new(HashMap::new()),
        say,
    };
    let mut tally = Tally::new("fetch");
    for account in accounts {
        if let Err(error) = fetch.account(account, &mut tally)? {
            tally.failed.push(format!("{error:#}"));
        }
    }
    // The pending lists of this run are mostly deleted again by now;
    // without this the file keeps their size.
    memo.compact()?;
    Ok(tally)
}

/// The index in `cache/`, caught up to the log — and, when that was
/// real work, said so.
fn caught_up(archive: &Archive, say: Say) -> Result<Index> {
    let mut index = archive.index()?;
    let folded = index.fold(archive.log())?;
    if folded.segments > 0 {
        say.line(format_args!(
            "index updated: {}",
            counted(folded.segments, "new log segment", "new log segments")
        ));
    }
    Ok(index)
}

/// One fetch in progress: what it works on, and how it speaks.
struct Fetch<'a> {
    archive: &'a Archive,
    memo: &'a Memo,
    options: &'a Options,
    source: Source,
    place: Attribute,
    tag: Attribute,
    /// The record as it stood when the run began, for the marks a
    /// message carried before — `None` when no account says marks.
    record: Option<Index>,
    /// The marks this run said, by subject: the record above does not
    /// see them yet, and a message met twice in one run must not take
    /// back what the run itself just said.
    said: RefCell<HashMap<String, BTreeSet<String>>>,
    say: Say,
}

impl Fetch<'_> {
    /// One account. The outer `Result` is the archive's trouble, which
    /// ends the run; the inner is this account's, which ends only the
    /// account.
    fn account(&self, account: &Account, tally: &mut Tally) -> Result<Result<()>> {
        // Everything that can say no before the first byte is fetched
        // says it here: the commanded keys, the login, the folder list.
        match account.reach() {
            Ok(Reach::Imap(imap)) => self.imap_account(account, &imap, tally),
            Ok(Reach::Graph(graph)) => self.graph_account(account, &graph, tally),
            Err(error) => Ok(Err(error)),
        }
    }

    /// A message is in: its bytes admitted, its place on the record —
    /// and its marks, where the mailbox says marks: those it carries
    /// said, those it carried and no longer does taken back.
    ///
    /// # Errors
    ///
    /// The archive refusing.
    fn land(
        &self,
        name: &str,
        bytes: &[u8],
        tags: Option<&[String]>,
        tally: &mut Tally,
    ) -> Result<()> {
        let admitted = admit(self.archive.content(), bytes)?;
        let mut facts = vec![(self.place.clone(), json!(name))];
        for tag in tags.unwrap_or_default() {
            facts.push((self.tag.clone(), json!(tag)));
        }
        tally.claims += record(
            self.archive.log(),
            &admitted,
            &Sighting {
                source: &self.source,
                run: &tally.run,
                mime: Some(crate::MESSAGE),
                facts: &facts,
                tags: &[],
                time: None,
            },
        )?;
        if admitted.is_new() {
            tally.stored += 1;
        } else {
            tally.known += 1;
        }
        if let Some(tags) = tags {
            self.take_back(admitted.subject(), tags, tally)?;
        }
        Ok(())
    }

    /// The marks that stood on the message and are not among `tags`
    /// any more are taken back, one retraction each — under this
    /// fetcher's source, in this run. What stood is what the record
    /// held when the run began, and what this run has said since.
    fn take_back(&self, subject: &Subject, tags: &[String], tally: &mut Tally) -> Result<()> {
        let current: BTreeSet<String> = tags.iter().cloned().collect();
        let mut stood = BTreeSet::new();
        if let Some(record) = &self.record {
            for value in record.values(subject, &self.tag, Scope::Held)? {
                if let Value::String(tag) = value {
                    stood.insert(tag);
                }
            }
        }
        let mut said = self.said.borrow_mut();
        if let Some(earlier) = said.get(subject.as_str()) {
            stood.extend(earlier.iter().cloned());
        }
        let time = Timestamp::now();
        for gone in stood.difference(&current) {
            let claim = Claim::retract_value(
                subject.clone(),
                self.tag.clone(),
                json!(gone),
                time.clone(),
                self.source.clone(),
                tally.run.clone(),
            )?;
            self.archive.log().append(&claim)?;
            tally.claims += 1;
            tally.taken += 1;
        }
        said.insert(subject.as_str().to_string(), current);
        Ok(())
    }

    fn imap_account(
        &self,
        account: &Account,
        imap: &config::Imap,
        tally: &mut Tally,
    ) -> Result<Result<()>> {
        let password = match imap.password(&account.name) {
            Ok(password) => password,
            Err(error) => return Ok(Err(error)),
        };
        let mut remote = match Remote::connect(&account.name, imap, password) {
            Ok(remote) => remote,
            Err(error) => return Ok(Err(error)),
        };
        let folders = match &account.folders {
            Some(folders) => folders.clone(),
            None => match remote.folders() {
                Ok(folders) => folders,
                Err(error) => return Ok(Err(error.context(account.name.clone()))),
            },
        };
        self.say.line(format_args!(
            "{}: {}",
            account.name,
            counted(folders.len(), "folder", "folders")
        ));
        for folder in &folders {
            self.folder(account, &mut remote, folder, tally)?;
        }
        remote.logout();
        Ok(Ok(()))
    }

    /// One IMAP folder: from its resume point on, or whole. The
    /// server's trouble ends the folder and is named in the tally; the
    /// archive's or the memo's ends the run.
    fn folder(
        &self,
        account: &Account,
        mailbox: &mut impl Mailbox,
        folder: &str,
        tally: &mut Tally,
    ) -> Result<()> {
        let name = place::folder(&account.name, folder);
        let opened = match mailbox.examine(folder) {
            Ok(opened) => opened,
            Err(error) => {
                tally.failed.push(format!("{}: {error:#}", account.name));
                return Ok(());
            }
        };
        let resume = self.memo.imap_resume(&account.name, folder)?;
        let (above, how) = carry_on(resume, opened.uidvalidity, self.options.full);
        let uids = match mailbox.uids_above(above) {
            Ok(uids) => uids,
            Err(error) => {
                tally.failed.push(format!("{name}: {error:#}"));
                return Ok(());
            }
        };
        self.say.line(format_args!(
            "{name}: {how}, {} to fetch",
            counted(uids.len(), "message", "messages")
        ));
        if self.options.dry_run {
            tally.would += uids.len();
            return Ok(());
        }

        let mut progress = self.say.progress();
        let mut done = 0;
        // The point may only pass a UID once everything below it has
        // landed: the server hands a batch back in its own order, and a
        // run that dies after the newest must not skip the rest.
        let mut landed = HashSet::new();
        let mut frontier = 0;
        self.memo.begin()?;
        let outcome = mailbox.fetch(&uids, &mut |uid, bytes| {
            self.land(&name, bytes, None, tally)?;
            // The message is on the record; the next run may start above
            // the highest UID with nothing missing below it. Committed in
            // batches: a run that dies repeats at most a batch, and the
            // bytes dedup.
            landed.insert(uid);
            while uids.get(frontier).is_some_and(|next| landed.contains(next)) {
                frontier += 1;
            }
            if frontier > 0 {
                self.memo.advance_imap(
                    &account.name,
                    folder,
                    ImapResume {
                        uidvalidity: opened.uidvalidity,
                        uid: uids[frontier - 1],
                    },
                )?;
            }
            done += 1;
            if done % COMMIT_EVERY == 0 {
                self.memo.commit()?;
                self.memo.begin()?;
            }
            progress.update(done, &format!("{name}: {done} of {} fetched", uids.len()));
            Ok(())
        });
        progress.finish();
        if outcome.is_ok() {
            // A folder with nothing above the point still has one: the
            // promise it was seen under, and where the next run asks
            // from — an empty folder is not a folder never fetched.
            self.memo.advance_imap(
                &account.name,
                folder,
                ImapResume {
                    uidvalidity: opened.uidvalidity,
                    uid: above,
                },
            )?;
        }
        // What landed before the trouble stays remembered either way.
        let closed = self.memo.commit();
        match outcome {
            Ok(()) => closed,
            Err(Stop::Server(error)) => {
                tally.failed.push(format!("{name}: {error:#}"));
                closed
            }
            Err(Stop::Caller(error)) => Err(match closed {
                Ok(()) => error,
                Err(more) => {
                    error.context(format!("and the resume point could not be saved: {more:#}"))
                }
            }),
        }
    }

    fn graph_account(
        &self,
        account: &Account,
        graph: &config::Graph,
        tally: &mut Tally,
    ) -> Result<Result<()>> {
        // The token and the folder tree say no before the first
        // message is asked for.
        let secret = match graph.secret(&account.name) {
            Ok(secret) => secret.to_string(),
            Err(error) => return Ok(Err(error)),
        };
        let mut client = match Graph::connect(&account.name, graph, secret) {
            Ok(client) => client,
            Err(error) => return Ok(Err(error)),
        };
        let folders = match &account.folders {
            Some(folders) => folders.clone(),
            None => client.folders(),
        };
        self.say.line(format_args!(
            "{}: {}",
            account.name,
            counted(folders.len(), "folder", "folders")
        ));
        for folder in &folders {
            self.graph_folder(account, &mut client, folder, tally)?;
        }
        Ok(Ok(()))
    }

    /// The listing is complete: the link and the ids are saved
    /// together, before the first message is fetched. A round from
    /// scratch lists the folder whole and replaces an earlier list.
    fn graph_listed(
        &self,
        account: &Account,
        folder: &str,
        name: &str,
        resumed: bool,
        round: Round,
    ) -> Result<()> {
        self.memo.begin()?;
        if !resumed {
            self.memo.clear_pending(&account.name, folder)?;
        }
        self.memo.add_pending(
            &account.name,
            folder,
            round
                .offered
                .iter()
                .map(|message| (message.id.as_str(), &message.tags)),
        )?;
        match round.link {
            // The link is the server's own "caught up here", so an
            // unchanged folder records it too. The exception is a first
            // round that listed nothing: an empty folder and a mailbox
            // not answering properly yet look alike from here, and the
            // link would claim coverage of mail nobody listed. That
            // costs one more round next time, on an empty folder.
            Some(link) if resumed || !round.offered.is_empty() => {
                self.memo.advance_graph(&account.name, folder, &link)?;
            }
            Some(_) => self.say.line(format_args!(
                "{name}: no messages listed; no resume point saved"
            )),
            None => self.say.line(format_args!(
                "{name}: the server returned no resume point; the next run fetches all messages again"
            )),
        }
        self.memo.commit()
    }

    /// Every message on the folder's pending list, one request each,
    /// with the marks the listing reported. A message stored, and one
    /// the server no longer has, comes off the list. One the server
    /// refuses is named in the tally and stays on the list, until it
    /// has been refused in [`GIVE_UP_AFTER`] runs. The rest of the list
    /// is left for the next run when the server refuses [`STOP_AFTER`]
    /// messages in a row, or gives no usable answer at all. Returns
    /// how many are left on the list.
    fn graph_land(
        &self,
        account: &Account,
        folder: &str,
        name: &str,
        client: &mut Graph<'_>,
        tally: &mut Tally,
    ) -> Result<usize> {
        let pending: Vec<Pending<Vec<String>>> = self.memo.pending(&account.name, folder)?;
        let mut progress = self.say.progress();
        let mut left = 0;
        let mut gone = 0;
        let mut refused_in_a_row = 0;
        self.memo.begin()?;
        for (done, message) in pending.iter().enumerate() {
            match client.message(&message.id) {
                Ok(bytes) => {
                    self.land(name, &bytes, Some(&message.detail), tally)?;
                    self.memo
                        .remove_pending(&account.name, folder, &message.id)?;
                    refused_in_a_row = 0;
                }
                Err(Miss::Gone) => {
                    gone += 1;
                    self.memo
                        .remove_pending(&account.name, folder, &message.id)?;
                    refused_in_a_row = 0;
                }
                Err(Miss::Refused(error)) => {
                    refused_in_a_row += 1;
                    if message.attempts + 1 >= GIVE_UP_AFTER {
                        self.memo
                            .remove_pending(&account.name, folder, &message.id)?;
                        tally.failed.push(format!(
                            "{name}: {error:#}; given up after {GIVE_UP_AFTER} runs, --full lists it again"
                        ));
                    } else {
                        self.memo.fail_pending(&account.name, folder, &message.id)?;
                        tally.failed.push(format!("{name}: {error:#}"));
                        left += 1;
                    }
                    if refused_in_a_row >= STOP_AFTER {
                        let rest = pending.len() - done - 1;
                        left += rest;
                        tally.failed.push(format!(
                            "{name}: {STOP_AFTER} messages in a row refused; {} left for the next run",
                            counted(rest, "message", "messages")
                        ));
                        break;
                    }
                }
                Err(Miss::Unreachable(error)) => {
                    let rest = pending.len() - done;
                    left += rest;
                    tally.failed.push(format!(
                        "{name}: {error:#}; {} left for the next run",
                        counted(rest, "message", "messages")
                    ));
                    break;
                }
            }
            // Committed in batches: a run that dies repeats at most a
            // batch, and the bytes dedup.
            if (done + 1) % COMMIT_EVERY == 0 {
                self.memo.commit()?;
                self.memo.begin()?;
            }
            progress.update(
                done + 1,
                &format!("{name}: {} of {} fetched", done + 1, pending.len()),
            );
        }
        progress.finish();
        self.memo.commit()?;
        if gone > 0 {
            self.say.line(format_args!(
                "{name}: {} no longer in the folder, skipped",
                counted(gone, "message", "messages")
            ));
        }
        Ok(left)
    }

    /// One Graph folder: a delta round from its link on, or whole. The
    /// link and the ids the round listed are saved before any message
    /// is fetched, then everything pending for the folder is fetched,
    /// what an earlier run left included.
    fn graph_folder(
        &self,
        account: &Account,
        client: &mut Graph<'_>,
        folder: &str,
        tally: &mut Tally,
    ) -> Result<()> {
        let name = place::folder(&account.name, folder);
        let Some(id) = client.resolve(folder).map(str::to_string) else {
            tally.failed.push(format!(
                "{name}: no such folder; the mailbox has {}",
                client.folders().join(", ")
            ));
            return Ok(());
        };
        let point = if self.options.full {
            None
        } else {
            self.memo.graph_resume(&account.name, folder)?
        };
        // A link out of cache/ is held to the host mail is asked of
        // before it is followed; one naming another host is worth
        // exactly as much as none.
        let point = point.filter(|point| {
            let owned = client.owns(&point.link);
            if !owned {
                self.say.line(format_args!(
                    "{name}: the saved resume point points to another host; ignoring it and fetching all messages"
                ));
            }
            owned
        });
        let mut from = point.as_ref().map(|point| point.link.as_str());
        let mut how = if self.options.full {
            "full fetch (--full)"
        } else if from.is_some() {
            "resuming from the last run"
        } else {
            "first fetch"
        };
        let round = match client.round(&id, from) {
            Ok(round) => round,
            Err(Halt::Expired) => {
                self.say.line(format_args!(
                    "{name}: the resume point has expired (age {}); fetching all messages",
                    point
                        .as_ref()
                        .map_or_else(|| "unknown".to_string(), crate::memo::GraphResume::age)
                ));
                from = None;
                how = "full fetch";
                match client.round(&id, None) {
                    Ok(round) => round,
                    Err(Halt::Expired | Halt::Failed(_)) => {
                        tally.failed.push(format!(
                            "{name}: the server did not list the folder's messages"
                        ));
                        return Ok(());
                    }
                }
            }
            Err(Halt::Failed(error)) => {
                tally.failed.push(format!("{name}: {error:#}"));
                return Ok(());
            }
        };
        // What an earlier run left on the list is still to fetch when
        // the round carries on from its link; a round from scratch
        // lists it again.
        let left = if from.is_some() {
            self.memo
                .pending::<Vec<String>>(&account.name, folder)?
                .len()
        } else {
            0
        };
        if left > 0 {
            self.say.line(format_args!(
                "{name}: {} pending from the last run",
                counted(left, "message", "messages")
            ));
        }
        let gone = if round.gone > 0 {
            format!(", {} removed from the folder", round.gone)
        } else {
            String::new()
        };
        self.say.line(format_args!(
            "{name}: {how}, {} to fetch{gone}",
            counted(round.offered.len(), "message", "messages")
        ));
        if self.options.dry_run {
            tally.would += left + round.offered.len();
            return Ok(());
        }

        self.graph_listed(account, folder, &name, from.is_some(), round)?;
        let left = self.graph_land(account, folder, &name, client, tally)?;
        if left > 0 {
            self.say.line(format_args!(
                "{name}: {} not fetched; the next run tries them again",
                counted(left, "message", "messages")
            ));
        }
        Ok(())
    }
}

/// Where a folder's fetch carries on from, and how to say so: above
/// the last UID when the server's promise still holds, from the start
/// otherwise — or when `full` asks for everything.
fn carry_on(resume: Option<ImapResume>, uidvalidity: u32, full: bool) -> (u32, String) {
    match resume {
        _ if full => (0, "full fetch (--full)".to_string()),
        Some(point) if point.uidvalidity == uidvalidity => {
            (point.uid, format!("resuming above UID {}", point.uid))
        }
        Some(_) => (0, "full fetch (UIDVALIDITY changed)".to_string()),
        None => (0, "first fetch".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use anyhow::anyhow;
    use ossuary_core::Algorithm;
    use tempfile::TempDir;

    use super::*;
    use crate::remote::Folder;

    fn at(uidvalidity: u32, uid: u32) -> ImapResume {
        ImapResume { uidvalidity, uid }
    }

    #[test]
    fn where_to_carry_on_from() {
        assert_eq!(carry_on(None, 7, false).0, 0, "first fetch: whole");
        assert_eq!(
            carry_on(Some(at(7, 40)), 7, false).0,
            40,
            "the promise holds: above the point"
        );
        assert_eq!(
            carry_on(Some(at(7, 40)), 8, false).0,
            0,
            "renumbered: whole"
        );
        assert_eq!(
            carry_on(Some(at(7, 40)), 7, true).0,
            0,
            "--full: whole, whatever the point"
        );
        assert!(carry_on(Some(at(7, 40)), 7, false).1.contains("40"));
    }

    /// A mailbox answering from memory: one folder, its messages by UID,
    /// handed back newest first — the order a server may choose.
    struct Fake {
        uidvalidity: u32,
        messages: BTreeMap<u32, Vec<u8>>,
        /// Stop after this many messages, the way a dropped connection
        /// would.
        give_up_after: Option<usize>,
    }

    impl Mailbox for Fake {
        fn examine(&mut self, _folder: &str) -> Result<Folder> {
            Ok(Folder {
                uidvalidity: self.uidvalidity,
            })
        }

        fn uids_above(&mut self, above: u32) -> Result<Vec<u32>> {
            Ok(self
                .messages
                .keys()
                .copied()
                .filter(|&uid| uid > above)
                .collect())
        }

        fn fetch(
            &mut self,
            uids: &[u32],
            each: &mut dyn FnMut(u32, &[u8]) -> Result<()>,
        ) -> Result<(), Stop> {
            for (handed, uid) in uids.iter().rev().enumerate() {
                if self.give_up_after == Some(handed) {
                    return Err(Stop::Server(anyhow!("connection dropped")));
                }
                each(*uid, &self.messages[uid]).map_err(Stop::Caller)?;
            }
            Ok(())
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

    /// A fetch with the record caught up to the log, as `run` makes one.
    fn fetcher<'a>(bench: &'a Bench, options: &'a Options) -> Fetch<'a> {
        Fetch {
            archive: &bench.archive,
            memo: &bench.memo,
            options,
            source: Source::parse(crate::SOURCE).unwrap(),
            place: Attribute::parse(place::ATTRIBUTE).unwrap(),
            tag: Attribute::parse(place::TAG).unwrap(),
            record: Some(caught_up(&bench.archive, Say::new(true)).unwrap()),
            said: RefCell::new(HashMap::new()),
            say: Say::new(true),
        }
    }

    fn options(full: bool, dry_run: bool) -> Options {
        Options { full, dry_run }
    }

    fn fetch(bench: &Bench, mailbox: &mut Fake, full: bool, dry_run: bool) -> Tally {
        let options = options(full, dry_run);
        let fetch = fetcher(bench, &options);
        let mut tally = Tally::new("fetch");
        fetch
            .folder(&bench.account, mailbox, "INBOX", &mut tally)
            .unwrap();
        tally
    }

    fn point(bench: &Bench) -> Option<ImapResume> {
        bench.memo.imap_resume("example.org", "INBOX").unwrap()
    }

    #[test]
    fn a_first_fetch_takes_the_folder_whole_and_leaves_the_point_at_the_top() {
        let bench = bench();
        let mut inbox = mailbox(7, &[3, 5, 9]);

        let tally = fetch(&bench, &mut inbox, false, false);

        assert_eq!((tally.stored, tally.known), (3, 0));
        assert_eq!(tally.claims, 9, "place, size, kind — each message");
        assert!(tally.failed.is_empty());
        assert_eq!(
            point(&bench),
            Some(at(7, 9)),
            "handed back newest first, the point still ends at the highest UID"
        );

        let again = fetch(&bench, &mut inbox, false, false);
        assert_eq!(
            (again.stored, again.known, again.claims),
            (0, 0, 0),
            "nothing above the point, nothing fetched, nothing said"
        );
        assert_eq!(point(&bench), Some(at(7, 9)));
    }

    #[test]
    fn an_empty_folder_gets_a_point_too() {
        let bench = bench();
        let mut inbox = mailbox(7, &[]);
        fetch(&bench, &mut inbox, false, false);
        assert_eq!(
            point(&bench),
            Some(at(7, 0)),
            "seen under this promise, with nothing in it — not a folder never fetched"
        );
    }

    #[test]
    fn a_renumbered_folder_is_fetched_whole() {
        let bench = bench();
        fetch(&bench, &mut mailbox(7, &[3, 5, 9]), false, false);

        let mut renumbered = mailbox(8, &[1, 2]);
        let tally = fetch(&bench, &mut renumbered, false, false);

        assert_eq!(tally.stored, 2, "new numbers, new messages here");
        assert_eq!(point(&bench), Some(at(8, 2)), "the point starts over");
    }

    #[test]
    fn full_fetches_everything_again_and_says_the_place_again() {
        let bench = bench();
        let mut inbox = mailbox(7, &[3, 5]);
        fetch(&bench, &mut inbox, false, false);

        let tally = fetch(&bench, &mut inbox, true, false);

        assert_eq!((tally.stored, tally.known), (0, 2), "the bytes are held");
        assert_eq!(
            tally.claims, 4,
            "place and the told kind, each message — the size the log has"
        );
        assert_eq!(point(&bench), Some(at(7, 5)));
    }

    #[test]
    fn a_dry_run_counts_and_writes_nothing() {
        let bench = bench();
        let tally = fetch(&bench, &mut mailbox(7, &[3, 5]), false, true);
        assert_eq!((tally.would, tally.claims), (2, 0));
        assert!(bench.archive.log().head().unwrap().is_empty());
        assert_eq!(point(&bench), None, "a rehearsal leaves no point");
    }

    #[test]
    fn a_server_giving_up_mid_folder_is_named_and_what_landed_stays() {
        let bench = bench();
        let mut inbox = mailbox(7, &[3, 5, 9]);
        inbox.give_up_after = Some(1);

        let tally = fetch(&bench, &mut inbox, false, false);

        assert_eq!(tally.stored, 1);
        assert_eq!(tally.failed.len(), 1);
        assert!(tally.failed[0].contains("example.org:INBOX"));
        assert_eq!(
            point(&bench),
            None,
            "the one that landed was the newest; below it two are missing, so the point stays"
        );

        inbox.give_up_after = None;
        let rest = fetch(&bench, &mut inbox, false, false);
        assert_eq!(
            (rest.stored, rest.known),
            (2, 1),
            "the rest lands, the one already held dedups"
        );
        assert_eq!(point(&bench), Some(at(7, 9)));
    }

    mod graph {
        //! The Graph side of the fetch, against a stub on loopback.

        use serde_json::json;

        use super::*;
        use crate::graph::stub::{Reply, Stub};

        const USER: &str = "john@example.com";

        fn m365() -> Account {
            toml::from_str(&format!(
                "name = \"m365\"\nbackend = \"msgraph\"\ntenant_id = \"tenant\"\n\
                 client_id = \"client\"\nclient_secret = \"s3cret\"\nuser = \"{USER}\"\n"
            ))
            .unwrap()
        }

        fn graph(account: &Account) -> config::Graph {
            match account.reach().unwrap() {
                Reach::Graph(graph) => graph,
                Reach::Imap(_) => unreachable!(),
            }
        }

        /// The bytes the stub hands out for a message id, and their
        /// subject.
        fn message(id: &str) -> (Vec<u8>, Subject) {
            let bytes = format!("Subject: {id}\r\n\r\nbody {id}").into_bytes();
            let subject = Subject::parse(Algorithm::Sha256.hash(&bytes).as_str()).unwrap();
            (bytes, subject)
        }

        /// A tenant issuing tokens and a mailbox of one folder,
        /// `Inbox` as `F1`, with these messages in it.
        fn stub(ids: &[&str]) -> Stub {
            let stub = Stub::start();
            stub.script(
                "POST /tenant/oauth2/v2.0/token",
                vec![Reply::json(200, &json!({ "access_token": "tok" }))],
            );
            stub.script(
                &format!(
                    "GET /v1.0/users/{USER}/mailFolders?$select=id,displayName,childFolderCount&$top=100"
                ),
                vec![Reply::json(
                    200,
                    &json!({ "value": [{ "id": "F1", "displayName": "Inbox", "childFolderCount": 0 }] }),
                )],
            );
            for id in ids {
                stub.script(
                    &format!("GET /v1.0/users/{USER}/messages/{id}/$value"),
                    vec![Reply::bytes(200, &message(id).0)],
                );
            }
            stub
        }

        /// A message as a round lists it, with these marks on it.
        fn marked(id: &str, tags: &[&str]) -> serde_json::Value {
            json!({ "id": id, "categories": tags })
        }

        /// A round over the folder, from scratch when `from` is `None`
        /// and from that link otherwise, offers `items` and ends on
        /// `link`.
        fn offer(stub: &Stub, from: Option<&str>, items: &[serde_json::Value], link: &str) {
            let key = match from {
                Some(from) => format!("GET /v1.0/delta/{from}"),
                None => format!(
                    "GET /v1.0/users/{USER}/mailFolders/F1/messages/delta?$select=id,categories"
                ),
            };
            stub.script(
                &key,
                vec![Reply::json(
                    200,
                    &json!({ "value": items, "@odata.deltaLink": format!("{}/v1.0/delta/{link}", stub.url()) }),
                )],
            );
        }

        /// The first round, from scratch, offers `ids` and ends on
        /// `link`.
        fn first_round(stub: &Stub, ids: &[&str], link: &str) {
            let items: Vec<_> = ids.iter().map(|id| json!({ "id": id })).collect();
            offer(stub, None, &items, link);
        }

        /// A round from `link` on offers `ids` and ends on `next`.
        fn next_round(stub: &Stub, link: &str, ids: &[&str], next: &str) {
            let items: Vec<_> = ids.iter().map(|id| json!({ "id": id })).collect();
            offer(stub, Some(link), &items, next);
        }

        fn fetch(
            bench: &Bench,
            stub: &Stub,
            account: &Account,
            full: bool,
            dry_run: bool,
        ) -> Tally {
            let options = options(full, dry_run);
            let fetch = fetcher(bench, &options);
            let graph = graph(account);
            let mut client = Graph::reach(
                &account.name,
                &graph,
                "s3cret".to_string(),
                stub.url(),
                &format!("{}/v1.0", stub.url()),
            )
            .unwrap();
            let mut tally = Tally::new("fetch");
            fetch
                .graph_folder(account, &mut client, "Inbox", &mut tally)
                .unwrap();
            tally
        }

        fn link(bench: &Bench) -> Option<String> {
            bench
                .memo
                .graph_resume("m365", "Inbox")
                .unwrap()
                .map(|point| point.link)
        }

        /// The marks standing on a message, as the record has them.
        fn standing_tags(bench: &Bench, subject: &Subject) -> Vec<String> {
            let index = caught_up(&bench.archive, Say::new(true)).unwrap();
            index
                .values(subject, &Attribute::parse(place::TAG).unwrap(), Scope::Held)
                .unwrap()
                .into_iter()
                .map(|value| value.as_str().unwrap().to_string())
                .collect()
        }

        #[test]
        fn a_first_round_takes_the_folder_whole_and_the_next_carries_on_from_its_link() {
            let bench = bench();
            let account = m365();
            let stub = stub(&["M1", "M2", "M3"]);
            first_round(&stub, &["M1", "M2"], "one");
            next_round(&stub, "one", &["M3"], "two");
            next_round(&stub, "two", &[], "three");

            let tally = fetch(&bench, &stub, &account, false, false);
            assert_eq!((tally.stored, tally.known, tally.claims), (2, 0, 6));
            assert!(tally.failed.is_empty(), "{:?}", tally.failed);
            assert!(link(&bench).unwrap().ends_with("/delta/one"));

            let again = fetch(&bench, &stub, &account, false, false);
            assert_eq!((again.stored, again.claims), (1, 3), "only what changed");
            assert!(link(&bench).unwrap().ends_with("/delta/two"));

            let quiet = fetch(&bench, &stub, &account, false, false);
            assert_eq!((quiet.stored, quiet.claims), (0, 0));
            assert!(
                link(&bench).unwrap().ends_with("/delta/three"),
                "an unchanged folder still records the server's word"
            );
        }

        #[test]
        fn the_marks_are_said_as_seen_and_taken_back_when_gone() {
            let bench = bench();
            let account = m365();
            let stub = stub(&["M1", "M2"]);
            let (_, m1) = message("M1");
            let (_, m2) = message("M2");
            offer(
                &stub,
                None,
                &[marked("M1", &["Red", "Later"]), marked("M2", &[])],
                "one",
            );

            let tally = fetch(&bench, &stub, &account, false, false);

            assert_eq!(
                (tally.stored, tally.claims, tally.taken),
                (2, 8, 0),
                "place, size, kind each — and two marks on the first"
            );
            assert_eq!(standing_tags(&bench, &m1), ["Later", "Red"]);
            assert!(standing_tags(&bench, &m2).is_empty());

            // Red comes off M1 and goes onto M2: both come round again.
            offer(
                &stub,
                Some("one"),
                &[marked("M1", &["Later"]), marked("M2", &["Red"])],
                "two",
            );
            let again = fetch(&bench, &stub, &account, false, false);

            assert_eq!((again.stored, again.known), (0, 2), "the bytes are held");
            assert_eq!(again.taken, 1, "Red is taken back from M1");
            assert_eq!(
                again.claims, 7,
                "M1: place, kind, Later, and Red taken back; M2: place, kind, Red"
            );
            assert_eq!(standing_tags(&bench, &m1), ["Later"]);
            assert_eq!(standing_tags(&bench, &m2), ["Red"]);

            // Said again unchanged, by a round from scratch: nothing is
            // taken back.
            offer(
                &stub,
                None,
                &[marked("M1", &["Later"]), marked("M2", &["Red"])],
                "three",
            );
            let full = fetch(&bench, &stub, &account, true, false);
            assert_eq!(full.taken, 0);
            assert_eq!(standing_tags(&bench, &m1), ["Later"]);
        }

        #[test]
        fn a_message_met_twice_in_one_run_keeps_what_the_run_said() {
            // The same bytes under two ids — a message in two folders —
            // and the record does not see the run's own claims yet.
            let bench = bench();
            let account = m365();
            let stub = stub(&["M1"]);
            let (bytes, m1) = message("M1");
            stub.script(
                &format!("GET /v1.0/users/{USER}/messages/M1-again/$value"),
                vec![Reply::bytes(200, &bytes)],
            );
            offer(
                &stub,
                None,
                &[marked("M1", &["Red"]), marked("M1-again", &["Red"])],
                "one",
            );

            let tally = fetch(&bench, &stub, &account, false, false);

            assert_eq!(
                tally.taken, 0,
                "the second sighting says Red again, takes nothing back"
            );
            assert_eq!(standing_tags(&bench, &m1), ["Red"]);

            offer(
                &stub,
                Some("one"),
                &[marked("M1", &["Red"]), marked("M1-again", &[])],
                "two",
            );
            let again = fetch(&bench, &stub, &account, false, false);
            assert_eq!(
                again.taken, 1,
                "the second sighting in a run sees what the first just said, and takes it back"
            );
            assert!(standing_tags(&bench, &m1).is_empty());
        }

        #[test]
        fn a_first_round_that_offers_nothing_starts_no_point() {
            let bench = bench();
            let account = m365();
            let stub = stub(&[]);
            first_round(&stub, &[], "one");

            let tally = fetch(&bench, &stub, &account, false, false);

            assert_eq!(tally.stored, 0);
            assert_eq!(
                link(&bench),
                None,
                "an empty folder and a mailbox not answering yet look alike"
            );
        }

        #[test]
        fn a_link_the_server_no_longer_honours_costs_one_whole_round() {
            let bench = bench();
            let account = m365();
            let stub = stub(&["M1", "M2"]);
            first_round(&stub, &["M1"], "one");
            let tally = fetch(&bench, &stub, &account, false, false);
            assert_eq!(tally.stored, 1);

            stub.script(
                "GET /v1.0/delta/one",
                vec![Reply::json(
                    410,
                    &json!({ "error": { "code": "syncStateNotFound" } }),
                )],
            );
            first_round(&stub, &["M1", "M2"], "fresh");

            let again = fetch(&bench, &stub, &account, false, false);

            assert_eq!(
                (again.stored, again.known),
                (1, 1),
                "the folder whole: the held one dedups, the new one lands"
            );
            assert!(again.failed.is_empty(), "{:?}", again.failed);
            assert!(link(&bench).unwrap().ends_with("/delta/fresh"));
        }

        /// The list pending for `Inbox`, in listing order.
        fn pending_list(bench: &Bench) -> Vec<Pending<Vec<String>>> {
            bench.memo.pending("m365", "Inbox").unwrap()
        }

        /// The ids pending for `Inbox`, in listing order.
        fn pending(bench: &Bench) -> Vec<String> {
            pending_list(bench)
                .into_iter()
                .map(|message| message.id)
                .collect()
        }

        /// A pending entry as the memo hands it back.
        fn entry(id: &str, tags: &[&str], attempts: u32) -> Pending<Vec<String>> {
            Pending {
                id: id.to_string(),
                detail: tags.iter().map(ToString::to_string).collect(),
                attempts,
            }
        }

        /// The server refuses `id` with a status that is not retried.
        fn refuse(stub: &Stub, id: &str) {
            stub.script(
                &format!("GET /v1.0/users/{USER}/messages/{id}/$value"),
                vec![Reply::json(
                    403,
                    &json!({ "error": { "code": "ErrorAccessDenied", "message": "denied" } }),
                )],
            );
        }

        #[test]
        fn a_message_the_server_keeps_stays_pending_under_the_saved_link() {
            let bench = bench();
            let account = m365();
            let stub = stub(&["M1", "M3"]);
            offer(
                &stub,
                None,
                &[marked("M1", &[]), marked("M2", &["Red"]), marked("M3", &[])],
                "one",
            );
            refuse(&stub, "M2");

            let tally = fetch(&bench, &stub, &account, false, false);

            assert_eq!(tally.stored, 2, "the two handed over are in");
            assert_eq!(tally.failed.len(), 1);
            assert!(
                tally.failed[0].starts_with("m365:Inbox: message M2: HTTP 403"),
                "{:?}",
                tally.failed
            );
            assert!(
                link(&bench).unwrap().ends_with("/delta/one"),
                "the listing was complete, so the link is saved"
            );
            assert_eq!(pending(&bench), ["M2"]);

            // Next run: the server hands M2 over now, the round from
            // the link offers nothing new.
            let (_, m2) = message("M2");
            stub.script(
                &format!("GET /v1.0/users/{USER}/messages/M2/$value"),
                vec![Reply::bytes(200, &message("M2").0)],
            );
            next_round(&stub, "one", &[], "two");
            let again = fetch(&bench, &stub, &account, false, false);

            assert_eq!((again.stored, again.known), (1, 0), "only M2 is fetched");
            assert!(again.failed.is_empty(), "{:?}", again.failed);
            assert_eq!(
                standing_tags(&bench, &m2),
                ["Red"],
                "with the marks the listing reported"
            );
            assert!(pending(&bench).is_empty());
            assert!(link(&bench).unwrap().ends_with("/delta/two"));
            let asked = stub
                .seen()
                .iter()
                .filter(|seen| seen.target.ends_with("/$value"))
                .count();
            assert_eq!(asked, 4, "M1, M2, M3, then M2 again; nothing else");
        }

        #[test]
        fn a_message_gone_since_the_listing_is_skipped_not_kept_pending() {
            let bench = bench();
            let account = m365();
            let stub = stub(&["M1"]);
            first_round(&stub, &["M1", "M2"], "one");
            stub.script(
                &format!("GET /v1.0/users/{USER}/messages/M2/$value"),
                vec![Reply::json(
                    404,
                    &json!({ "error": { "code": "ErrorItemNotFound", "message": "gone" } }),
                )],
            );

            let tally = fetch(&bench, &stub, &account, false, false);

            assert_eq!(tally.stored, 1);
            assert!(tally.failed.is_empty(), "{:?}", tally.failed);
            assert!(pending(&bench).is_empty());
            assert!(link(&bench).unwrap().ends_with("/delta/one"));
        }

        #[test]
        fn a_404_without_the_gone_code_keeps_the_message_pending() {
            let bench = bench();
            let account = m365();
            let stub = stub(&["M1"]);
            first_round(&stub, &["M1", "M2"], "one");
            stub.script(
                &format!("GET /v1.0/users/{USER}/messages/M2/$value"),
                vec![Reply::json(
                    404,
                    &json!({ "error": { "code": "ResourceNotFound", "message": "Resource could not be discovered." } }),
                )],
            );

            let tally = fetch(&bench, &stub, &account, false, false);

            assert_eq!(tally.stored, 1);
            assert_eq!(tally.failed.len(), 1);
            assert!(
                tally.failed[0].starts_with("m365:Inbox: message M2: HTTP 404 ResourceNotFound"),
                "{:?}",
                tally.failed
            );
            assert_eq!(
                pending(&bench),
                ["M2"],
                "not gone, so not taken off the list"
            );
            assert!(link(&bench).unwrap().ends_with("/delta/one"));
        }

        #[test]
        fn a_round_that_lists_a_pending_message_again_renews_its_marks() {
            let bench = bench();
            let account = m365();
            let stub = stub(&["M1"]);
            offer(
                &stub,
                None,
                &[marked("M1", &[]), marked("M2", &["Red"])],
                "one",
            );
            refuse(&stub, "M2");
            fetch(&bench, &stub, &account, false, false);
            assert_eq!(pending(&bench), ["M2"]);

            offer(&stub, Some("one"), &[marked("M2", &["Blue"])], "two");
            fetch(&bench, &stub, &account, false, false);

            assert_eq!(
                pending_list(&bench),
                [entry("M2", &["Blue"], 2)],
                "listed once, with the newer marks, and the refusals of both runs counted"
            );
        }

        #[test]
        fn a_message_refused_in_three_runs_is_given_up() {
            let bench = bench();
            let account = m365();
            let stub = stub(&["M1"]);
            first_round(&stub, &["M1", "M2"], "one");
            refuse(&stub, "M2");
            next_round(&stub, "one", &[], "one");

            fetch(&bench, &stub, &account, false, false);
            assert_eq!(pending_list(&bench), [entry("M2", &[], 1)]);
            let second = fetch(&bench, &stub, &account, false, false);
            assert_eq!(pending_list(&bench), [entry("M2", &[], 2)]);
            assert_eq!(
                second.failed,
                ["m365:Inbox: message M2: HTTP 403 ErrorAccessDenied: denied"]
            );

            let third = fetch(&bench, &stub, &account, false, false);

            assert!(pending(&bench).is_empty(), "given up: off the list");
            assert_eq!(
                third.failed,
                [
                    "m365:Inbox: message M2: HTTP 403 ErrorAccessDenied: denied; given up after 3 runs, --full lists it again"
                ]
            );

            // --full lists it again, and it gets a fresh count.
            first_round(&stub, &["M1", "M2"], "fresh");
            let full = fetch(&bench, &stub, &account, true, false);
            assert_eq!(full.failed.len(), 1);
            assert_eq!(pending_list(&bench), [entry("M2", &[], 1)]);
        }

        #[test]
        fn ten_refusals_in_a_row_leave_the_rest_of_the_folder_for_the_next_run() {
            let bench = bench();
            let account = m365();
            let ids: Vec<String> = (1..=12).map(|n| format!("M{n}")).collect();
            let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
            let stub = stub(&[]);
            first_round(&stub, &ids, "one");
            for id in &ids {
                refuse(&stub, id);
            }

            let tally = fetch(&bench, &stub, &account, false, false);

            assert_eq!(
                tally.failed.len(),
                11,
                "ten refusals and the stop: {:?}",
                tally.failed
            );
            assert_eq!(
                tally.failed[10],
                "m365:Inbox: 10 messages in a row refused; 2 messages left for the next run"
            );
            let list = pending_list(&bench);
            assert_eq!(list.len(), 12, "all still pending");
            assert_eq!(
                list.iter().filter(|message| message.attempts == 1).count(),
                10,
                "the ten refused count a run, the two not asked do not"
            );
            let asked = stub
                .seen()
                .iter()
                .filter(|seen| seen.target.ends_with("/$value"))
                .count();
            assert_eq!(asked, 10, "M11 and M12 are not asked for");
        }

        #[test]
        fn a_server_that_stops_answering_leaves_the_rest_pending_uncounted() {
            let bench = bench();
            let account = m365();
            let stub = stub(&["M1", "M3"]);
            first_round(&stub, &["M1", "M2", "M3"], "one");
            // M2 answers 401, and the tenant will not renew the token:
            // nothing after M2 can be asked for.
            stub.script(
                &format!("GET /v1.0/users/{USER}/messages/M2/$value"),
                vec![Reply::bytes(401, b"")],
            );
            stub.script(
                "POST /tenant/oauth2/v2.0/token",
                vec![
                    Reply::json(200, &json!({ "access_token": "tok" })),
                    Reply::json(401, &json!({ "error": "invalid_client" })),
                ],
            );

            let tally = fetch(&bench, &stub, &account, false, false);

            assert_eq!(tally.stored, 1, "M1 is in");
            assert_eq!(tally.failed.len(), 1, "{:?}", tally.failed);
            assert!(
                tally.failed[0].ends_with("; 2 messages left for the next run"),
                "{:?}",
                tally.failed
            );
            assert_eq!(
                pending_list(&bench),
                [entry("M2", &[], 0), entry("M3", &[], 0)],
                "neither counts a failed run: the server was gone, not the message"
            );
            assert!(link(&bench).unwrap().ends_with("/delta/one"));
            assert!(
                stub.seen().iter().all(|seen| !seen.target.contains("/M3/")),
                "M3 is not asked for"
            );
        }

        #[test]
        fn a_round_from_scratch_replaces_the_pending_list() {
            let bench = bench();
            let account = m365();
            let stub = stub(&["M1"]);
            first_round(&stub, &["M1", "M2"], "one");
            refuse(&stub, "M2");
            fetch(&bench, &stub, &account, false, false);
            assert_eq!(pending(&bench), ["M2"]);

            // M2 is gone from the mailbox; --full lists the folder
            // whole and M2 is not asked for again.
            first_round(&stub, &["M1"], "fresh");
            let full = fetch(&bench, &stub, &account, true, false);

            assert_eq!((full.stored, full.known), (0, 1));
            assert!(full.failed.is_empty(), "{:?}", full.failed);
            assert!(pending(&bench).is_empty());
            let asked = stub
                .seen()
                .iter()
                .filter(|seen| seen.target.ends_with("/M2/$value"))
                .count();
            assert_eq!(asked, 1, "only in the first run");
        }

        #[test]
        fn a_dry_run_counts_what_is_pending_too() {
            let bench = bench();
            let account = m365();
            let stub = stub(&["M1"]);
            first_round(&stub, &["M1", "M2"], "one");
            refuse(&stub, "M2");
            fetch(&bench, &stub, &account, false, false);
            next_round(&stub, "one", &["M3"], "two");

            let rehearsed = fetch(&bench, &stub, &account, false, true);

            assert_eq!((rehearsed.would, rehearsed.claims), (2, 0), "M2 and M3");
            assert_eq!(pending(&bench), ["M2"], "a rehearsal moves nothing");
            assert!(link(&bench).unwrap().ends_with("/delta/one"));
        }

        #[test]
        fn full_rounds_the_folder_whole_and_a_dry_run_counts() {
            let bench = bench();
            let account = m365();
            let stub = stub(&["M1", "M2"]);
            first_round(&stub, &["M1", "M2"], "one");
            next_round(&stub, "one", &[], "two");
            fetch(&bench, &stub, &account, false, false);

            let rehearsed = fetch(&bench, &stub, &account, true, true);
            assert_eq!((rehearsed.would, rehearsed.claims), (2, 0));
            assert!(
                link(&bench).unwrap().ends_with("/delta/one"),
                "a rehearsal moves nothing"
            );

            let full = fetch(&bench, &stub, &account, true, false);
            assert_eq!(
                (full.stored, full.known),
                (0, 2),
                "the bytes are held, the place is said again"
            );
            assert!(
                link(&bench).unwrap().ends_with("/delta/one"),
                "the round's own link"
            );
        }

        #[test]
        fn a_folder_the_mailbox_does_not_have_is_named_with_what_it_has() {
            let bench = bench();
            let account = m365();
            let stub = stub(&[]);
            let options = options(false, false);
            let fetch = fetcher(&bench, &options);
            let graph = graph(&account);
            let mut client = Graph::reach(
                &account.name,
                &graph,
                "s3cret".to_string(),
                stub.url(),
                &format!("{}/v1.0", stub.url()),
            )
            .unwrap();
            let mut tally = Tally::new("fetch");
            fetch
                .graph_folder(&account, &mut client, "Drafts", &mut tally)
                .unwrap();
            assert_eq!(
                tally.failed,
                ["m365:Drafts: no such folder; the mailbox has Inbox"]
            );
        }

        #[test]
        fn a_point_naming_another_host_is_worth_as_much_as_none() {
            let bench = bench();
            let account = m365();
            let stub = stub(&["M1"]);
            first_round(&stub, &["M1"], "one");
            bench
                .memo
                .advance_graph("m365", "Inbox", "https://evil.example.com/delta")
                .unwrap();

            let tally = fetch(&bench, &stub, &account, false, false);

            assert_eq!(
                tally.stored, 1,
                "the folder whole, the foreign link never followed"
            );
            assert!(
                stub.seen().iter().all(|seen| !seen.target.contains("evil")),
                "nothing went anywhere else"
            );
            assert!(link(&bench).unwrap().ends_with("/delta/one"));
        }
    }
}
