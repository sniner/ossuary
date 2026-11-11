//! Fetching: every configured mailbox, folder by folder, whatever
//! arrived since the last run.
//!
//! Over IMAP a folder is opened read-only and its resume point looked
//! up in the memo: the UIDVALIDITY the server promised last time, and
//! the highest UID fetched under it. If the promise still holds, the
//! server is asked only for what lies above that UID and answers with
//! the new messages alone. If the promise changed, or there is no
//! resume point, the folder is fetched whole: the bytes dedup in the
//! store, and the places are said again. A message the server will not
//! hand over goes on the folder's pending list in the memo, and the
//! point may pass it; the next run asks for the pending UIDs first.
//!
//! Proton Mail Bridge hands the same message over as other bytes after
//! it rebuilt its local database, so there the bytes do not dedup.
//! Before a folder of a Proton account downloads anything, the fetch
//! lists the Proton id of every UID it would ask for, and a message
//! whose id is on record is not downloaded (see [`crate::proton`]).
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
//! Gmail hands each message over with its labels, less the label of
//! the folder being read, and those are recorded the same way. Over
//! IMAP a message is listed again only when it is fetched again, so a
//! label changed on a message fetched earlier is not seen until then.
//! Proton Mail Bridge shows each label as a folder under `Labels/`.
//! Those not configured are read after the configured folders, each
//! from its own resume point, and nothing is downloaded from them:
//! every message of a label folder is in `All Mail` too. Each UID above
//! the point is looked up by its Proton id, and the files with the id
//! get the label. A UID whose id is not on record yet, because the
//! message arrived after `All Mail` was read, stays on the folder's
//! pending list and is asked about again by the next run. Under
//! `--full` the configured folders and the label folders are listed
//! whole anyway, and a label is retracted from a message they show
//! still in the mailbox but no longer in the label's folder.
//!
//! Either way each message goes in through the archive's two-step
//! accession with its place — account and folder — as the one fact
//! the fetcher has to tell about it. The resume point moves forward
//! only over what has landed or is on the pending list. It lives in
//! `cache/`: losing it costs one whole fetch of the folder, never a
//! claim.
//!
//! One account failing costs that account: the password, the login,
//! a folder that will not open are named in the tally, and the run
//! goes on to the next. The archive or the memo refusing ends the run.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Result;
use ossuary_core::{
    Archive, Attribute, Claim, ClaimId, Index, Scope, Sighting, Source, Subject, Timestamp, Value,
    admit, record,
};
use serde_json::json;

use crate::config::{self, Account, Reach};
use crate::memo::Memo;
use crate::output::{Say, counted};
use crate::place;
use crate::proton;
use crate::remote::Labels;
use crate::tally::Tally;

mod bridge;
mod comparing;
mod graph_folder;
mod imap_folder;
#[cfg(test)]
mod tests;

pub struct Options {
    /// Fetch every message, the resume point notwithstanding.
    pub full: bool,
    /// List every message, the resume point notwithstanding, and fetch
    /// those the archive does not hold under the same Message-ID.
    pub compare: bool,
    /// Count what would be fetched, fetch nothing.
    pub dry_run: bool,
}

/// Under `--compare`, the messages a round stored, by server id as the
/// memo has it, with their subjects: what decides whether another round
/// is needed.
type Stored = Vec<(String, String)>;

/// One folder of one account: the pair the memo is keyed by, and its
/// name as the record and every line about it have it.
struct Place<'a> {
    account: &'a str,
    folder: &'a str,
    name: String,
}

impl<'a> Place<'a> {
    fn new(account: &'a str, folder: &'a str) -> Self {
        Self {
            account,
            folder,
            name: place::folder(account, folder),
        }
    }
}

/// How many messages land between two commits of the resume point.
const COMMIT_EVERY: usize = 25;

/// How many runs may fail to fetch a pending message before it is
/// given up.
const GIVE_UP_AFTER: u32 = 3;

/// How many messages in a row the server may refuse before the rest
/// of the folder is left for the next run.
const STOP_AFTER: usize = 10;

/// The messages the server refused in a row.
#[derive(Default)]
struct Refusals(usize);

impl Refusals {
    /// One more. Answers whether that makes [`STOP_AFTER`], and the
    /// rest of the folder is to be left for the next run.
    fn one_more(&mut self) -> bool {
        self.0 += 1;
        self.0 >= STOP_AFTER
    }

    /// The server handed a message over, or said it is gone.
    fn ended(&mut self) {
        self.0 = 0;
    }
}

/// Fetch the accounts, read from `accounts_file`, into the archive.
///
/// # Errors
///
/// The archive or the memo refusing — everything else is named in the
/// tally and the run goes on.
pub fn run(
    archive: &Archive,
    accounts_file: &config::Source,
    accounts: &[&Account],
    memo: &Memo,
    options: &Options,
    say: Say,
) -> Result<Tally> {
    let fetch = Fetch {
        archive,
        accounts_file,
        memo,
        options,
        source: Source::parse(crate::SOURCE)?,
        place: Attribute::parse(place::ATTRIBUTE)?,
        tag: Attribute::parse(place::TAG)?,
        id: Attribute::parse(proton::ATTRIBUTE)?,
        say,
    };
    let mut state = State::new();
    for account in accounts {
        if let Err(error) = fetch.account(account, &mut state)? {
            state.tally.failed.push(format!("{error:#}"));
        }
    }
    // The pending lists of this run are mostly deleted again by now;
    // without this the file keeps their size. A file that stays large
    // costs nothing but space, so this does not fail the run.
    if !options.dry_run
        && let Err(error) = memo.compact()
    {
        say.line(format_args!(
            "{}: not compacted ({error:#}); the next run tries again",
            memo.file()
        ));
    }
    Ok(state.tally)
}

/// The line that opens an account: how many folders it fetches, and
/// which ones its `skip` list leaves out.
fn opening(account: &Account, fetched: usize) -> String {
    let count = counted(fetched, "folder", "folders");
    if account.skip.is_empty() {
        format!("{}: {count}", account.name)
    } else {
        format!(
            "{}: {count}, skipped: {}",
            account.name,
            account.skip.join(", ")
        )
    }
}

/// The index in `cache/`, caught up to the log — and, when that was
/// real work, said so.
fn caught_up(archive: &Archive, say: Say) -> Result<Index> {
    let mut index = archive.index()?;
    let folded = index.fold(archive.log())?;
    if folded.rebuilt {
        say.line(format_args!(
            "index rebuilt: {}",
            counted(folded.segments, "log segment", "log segments")
        ));
    } else if folded.segments > 0 {
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
    /// Where the accounts were read from, for a hint that names a
    /// command reading the same file.
    accounts_file: &'a config::Source,
    memo: &'a Memo,
    options: &'a Options,
    source: Source,
    place: Attribute,
    tag: Attribute,
    /// The id a mailbox gives a message, where it gives one that lasts.
    id: Attribute,
    say: Say,
}

/// The marks a mailbox lists for a message: Outlook's categories, or
/// the labels of Gmail.
struct Marks<'a> {
    tags: &'a [String],
    /// How far the list goes.
    listed: &'a Labels,
}

/// What changes while a fetch runs: the count, and what the run knows of
/// the record. Handed from step to step; the [`Fetch`] itself is the
/// same for the whole run.
struct State {
    tally: Tally,
    standing: Standing,
}

impl State {
    fn new() -> Self {
        Self {
            tally: Tally::new("fetch"),
            standing: Standing::default(),
        }
    }
}

/// What stands on the record, as far as the run reads it.
#[derive(Default)]
struct Standing {
    /// The index as it stood when the current folder was reached, with
    /// what the folders before it wrote: `None` until the first folder
    /// that needs it (see [`refresh`](Self::refresh)). No question asked
    /// of it within a folder depends on what that folder writes.
    index: Option<Index>,
    /// The tag assertions this run wrote: by subject, each tag with the
    /// ids its claims got. A retraction later in the run has to name
    /// them as well, since the index only catches up between folders.
    said: HashMap<String, BTreeMap<String, Vec<ClaimId>>>,
}

impl Standing {
    /// Bring the index up to date before a folder that reads it; it then
    /// holds the claims of the folders before this one. The first call
    /// builds it from the log, a later fold reads only the segments sealed
    /// since and what was appended to the open head.
    fn refresh(&mut self, archive: &Archive, say: Say) -> Result<()> {
        match self.index.as_mut() {
            Some(index) => {
                index.fold(archive.log())?;
            }
            None => self.index = Some(caught_up(archive, say)?),
        }
        Ok(())
    }

    /// The index, for a step that cannot do without it.
    ///
    /// # Panics
    ///
    /// When [`refresh`](Self::refresh) has not run. Every folder of a
    /// Proton account, every Graph folder and every folder under
    /// `--compare` runs it before anything else.
    fn index(&self) -> &Index {
        self.index
            .as_ref()
            .expect("the folder refreshed the index when it was opened")
    }

    /// Note the tag assertions among the claims a message was recorded
    /// with.
    fn wrote(&mut self, tag: &Attribute, claims: &[Claim]) {
        for claim in claims {
            if claim.attribute() != tag {
                continue;
            }
            let Some((Value::String(value), id)) = claim.value().zip(claim.id()) else {
                continue;
            };
            self.said
                .entry(claim.subject().as_str().to_string())
                .or_default()
                .entry(value.clone())
                .or_default()
                .push(id.clone());
        }
    }

    /// The tags standing on `subject` that are not among `current`, each
    /// with the ids of the claims that put it there: the index's claims,
    /// and the ones this run wrote since the index last caught up.
    /// Without an index, only the latter.
    fn gone(
        &self,
        subject: &Subject,
        tag: &Attribute,
        current: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, Vec<ClaimId>>> {
        let mut gone: BTreeMap<String, Vec<ClaimId>> = BTreeMap::new();
        if let Some(index) = &self.index {
            for value in index.values(subject, tag, Scope::Held)? {
                let Value::String(name) = &value else {
                    continue;
                };
                if !current.contains(name) {
                    gone.entry(name.clone())
                        .or_default()
                        .extend(index.standing_ids(subject, tag, Some(&value))?);
                }
            }
        }
        if let Some(said) = self.said.get(subject.as_str()) {
            for (name, ids) in said {
                if !current.contains(name) {
                    gone.entry(name.clone())
                        .or_default()
                        .extend(ids.iter().cloned());
                }
            }
        }
        for ids in gone.values_mut() {
            ids.sort();
            ids.dedup();
        }
        Ok(gone)
    }

    /// The ids of the claims that put the tag `name` on `subject`, from
    /// the index and from this run.
    fn ids_of(&self, subject: &Subject, tag: &Attribute, name: &str) -> Result<Vec<ClaimId>> {
        let mut ids = self
            .index()
            .standing_ids(subject, tag, Some(&json!(name)))?;
        if let Some(said) = self
            .said
            .get(subject.as_str())
            .and_then(|tags| tags.get(name))
        {
            ids.extend(said.iter().cloned());
        }
        ids.sort();
        ids.dedup();
        Ok(ids)
    }
}

impl Fetch<'_> {
    /// One account. The outer `Result` is the archive's trouble, which
    /// ends the run; the inner is this account's, which ends only the
    /// account.
    fn account(&self, account: &Account, state: &mut State) -> Result<Result<()>> {
        // Everything that can say no before the first byte is fetched
        // says it here: the commanded keys, the login, the folder list.
        match account.reach() {
            Ok(Reach::Imap(imap)) => self.imap_account(account, &imap, state),
            Ok(Reach::Graph(graph)) => self.graph_account(account, &graph, state),
            Err(error) => Ok(Err(error)),
        }
    }

    /// A message is in: its bytes admitted, its place on the record —
    /// and its marks, where the mailbox says marks: those it carries
    /// said, those it carried and no longer does taken back. The id the
    /// mailbox gives it is recorded too, where it gives one that lasts.
    ///
    /// # Errors
    ///
    /// The archive refusing.
    fn land(
        &self,
        place: &Place<'_>,
        bytes: &[u8],
        marks: Option<&Marks<'_>>,
        id: Option<&str>,
        state: &mut State,
    ) -> Result<Subject> {
        let admitted = admit(self.archive.content(), bytes)?;
        let mut facts = vec![(self.place.clone(), json!(place.name))];
        for tag in marks.map(|marks| marks.tags).unwrap_or_default() {
            facts.push((self.tag.clone(), json!(tag)));
        }
        if let Some(id) = id {
            facts.push((self.id.clone(), json!(id)));
        }
        let written = record(
            self.archive.log(),
            &admitted,
            &Sighting {
                source: &self.source,
                run: &state.tally.run,
                mime: Some(crate::MESSAGE),
                facts: &facts,
                tags: &[],
                time: None,
            },
        )?;
        state.tally.claims += written.len();
        state.standing.wrote(&self.tag, &written);
        if admitted.is_new() {
            state.tally.stored += 1;
        } else {
            state.tally.known += 1;
        }
        if let Some(marks) = marks {
            self.take_back(admitted.subject(), marks, &state.standing, &mut state.tally)?;
        }
        Ok(admitted.subject().clone())
    }

    /// A fact about messages the archive holds, recorded without a
    /// download: one claim on each of `subjects`.
    fn assert_on(
        &self,
        subjects: &[Subject],
        attribute: &Attribute,
        value: &serde_json::Value,
        tally: &mut Tally,
    ) -> Result<()> {
        let time = Timestamp::now();
        let mut claims = Vec::new();
        for subject in subjects {
            claims.push(Claim::assert(
                subject.clone(),
                attribute.clone(),
                value.clone(),
                time.clone(),
                self.source.clone(),
                tally.run.clone(),
            )?);
        }
        self.archive.log().append_all(&claims)?;
        tally.claims += claims.len();
        Ok(())
    }

    /// Under `--compare`, note which subject the message fetched under
    /// the server's `id` turned out to be: in `stored` for the next
    /// round of this run, and in the memo for later runs.
    fn note(
        &self,
        place: &Place<'_>,
        id: &str,
        subject: &Subject,
        stored: &mut Stored,
    ) -> Result<()> {
        if self.options.compare {
            self.memo
                .add_compared(place.account, place.folder, id, subject.as_str())?;
            stored.push((id.to_string(), subject.as_str().to_string()));
        }
        Ok(())
    }

    /// The marks that stood on the message and are not among those
    /// listed any more are taken back, one retraction each, under this
    /// fetcher's source, in this run. What stood is what the record
    /// held when the folder was reached.
    ///
    /// A list that leaves out the folder's own label does not show that
    /// label gone: it stays. Where the label left out is not known,
    /// nothing is taken back, since any one of the tags may be it.
    fn take_back(
        &self,
        subject: &Subject,
        marks: &Marks<'_>,
        standing: &Standing,
        tally: &mut Tally,
    ) -> Result<()> {
        let mut current: BTreeSet<String> = marks.tags.iter().cloned().collect();
        match marks.listed {
            Labels::Whole => {}
            Labels::Without(own) => {
                current.insert(own.clone());
            }
            Labels::WithoutOne => return Ok(()),
        }
        let time = Timestamp::now();
        let mut claims = Vec::new();
        // A retraction names the claims that put the tag there.
        for ids in standing.gone(subject, &self.tag, &current)?.into_values() {
            claims.push(Claim::retract(
                subject.clone(),
                self.tag.clone(),
                ids,
                time.clone(),
                self.source.clone(),
                tally.run.clone(),
            )?);
        }
        self.archive.log().append_all(&claims)?;
        tally.claims += claims.len();
        tally.taken += claims.len();
        Ok(())
    }

    /// A message on the folder's pending list that the server refused
    /// again, after `attempts` runs that failed before this one: one
    /// more failed run on the list, or, after [`GIVE_UP_AFTER`], given
    /// up and taken off it. Named in the tally either way. Answers
    /// whether it stays on the list.
    fn refused(
        &self,
        place: &Place<'_>,
        id: &str,
        attempts: u32,
        error: &anyhow::Error,
        tally: &mut Tally,
    ) -> Result<bool> {
        let name = &place.name;
        if attempts + 1 >= GIVE_UP_AFTER {
            self.memo.remove_pending(place.account, place.folder, id)?;
            tally.failed.push(format!(
                "{name}: {error:#}; given up after {GIVE_UP_AFTER} runs, --full fetches it again"
            ));
            tally.given_up += 1;
            return Ok(false);
        }
        self.memo.fail_pending(place.account, place.folder, id)?;
        tally.failed.push(format!("{name}: {error:#}"));
        Ok(true)
    }

    /// The server refused [`STOP_AFTER`] messages in a row: name that in
    /// the tally, with the `rest` of the folder the fetch leaves.
    fn stopped(place: &Place<'_>, rest: usize, tally: &mut Tally) {
        tally.failed.push(format!(
            "{}: {STOP_AFTER} messages in a row refused; {} left for the next run",
            place.name,
            counted(rest, "message", "messages")
        ));
    }

    /// Commit the memo after every [`COMMIT_EVERY`]th message and open
    /// the next batch: a run that dies repeats at most a batch, and the
    /// bytes dedup.
    fn commit_every(&self, done: usize) -> Result<()> {
        if done.is_multiple_of(COMMIT_EVERY) {
            self.memo.commit_for(self.archive)?;
            self.memo.begin()?;
        }
        Ok(())
    }
}
