//! An IMAP account, folder by folder: what to ask the server for, and
//! what becomes of each UID it answers with.

use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;

use anyhow::Result;
use serde_json::json;

use super::bridge::{Identified, InMailbox};
use super::{Fetch, Place, Refusals, State, Stored, opening};
use crate::compare;
use crate::config::{self, Account};
use crate::memo::{ImapResume, Pending};
use crate::output::{Progress, counted};
use crate::remote::{Each, Flavour, Folder, Got, Mailbox, Remote, Stop};

/// One pass over an IMAP folder: what it asks for, and how far the
/// resume point may move.
struct Walk<'a> {
    /// The UIDs above the point, ascending.
    uids: &'a [u32],
    uidvalidity: u32,
    /// The pending list the last run left: failed runs, by UID.
    pending: HashMap<u32, u32>,
    /// UIDs the next run need not ask for above the point: landed,
    /// gone, given up, or on the pending list now.
    passed: HashSet<u32>,
    /// How many of `uids`, from the lowest, are passed.
    frontier: usize,
    /// What the Proton ids settled; empty for any other server.
    identified: Identified,
    /// UIDs handled so far, of `total`.
    done: usize,
    total: usize,
    refusals: Refusals,
    gone: usize,
    /// Under `--compare`, the messages the pass stored.
    stored: Stored,
    progress: Progress,
}

impl Walk<'_> {
    /// Mark a UID above the point as passed. Answers the point to save
    /// when that moved it. The point passes a UID only once everything
    /// below it is passed: the server hands a batch back in its own
    /// order, and a run that dies after the newest must not skip the
    /// rest.
    fn pass(&mut self, uid: u32) -> Option<ImapResume> {
        self.passed.insert(uid);
        let before = self.frontier;
        while self
            .uids
            .get(self.frontier)
            .is_some_and(|next| self.passed.contains(next))
        {
            self.frontier += 1;
        }
        (self.frontier > before).then(|| ImapResume {
            uidvalidity: self.uidvalidity,
            uid: self.uids[self.frontier - 1],
        })
    }
}

/// Where a folder's fetch carries on from, and how to say so: above
/// the last UID when the server's promise still holds, from the start
/// otherwise, or when the option named in `whole` asks for everything.
pub(super) fn carry_on(
    resume: Option<ImapResume>,
    uidvalidity: u32,
    whole: Option<&str>,
) -> (u32, String) {
    match (whole, resume) {
        (Some(option), _) => (0, format!("full fetch ({option})")),
        (None, Some(point)) if point.uidvalidity == uidvalidity => {
            (point.uid, format!("resuming above UID {}", point.uid))
        }
        (None, Some(_)) => (0, "full fetch (UIDVALIDITY changed)".to_string()),
        (None, None) => (0, "first fetch".to_string()),
    }
}

impl Fetch<'_> {
    pub(super) fn imap_account(
        &self,
        account: &Account,
        imap: &config::Imap,
        state: &mut State,
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
            None => match remote
                .folders()
                .and_then(|offered| account.unskipped(offered))
            {
                Ok(folders) => folders,
                Err(error) => return Ok(Err(error.context(account.name.clone()))),
            },
        };
        self.say
            .line(format_args!("{}", opening(account, folders.len())));
        self.imap_folders(account, &mut remote, &folders, state)?;
        remote.logout();
        Ok(Ok(()))
    }

    /// The folders of an IMAP account, then, from Proton Mail Bridge,
    /// the label folders not among them.
    pub(super) fn imap_folders(
        &self,
        account: &Account,
        mailbox: &mut impl Mailbox,
        folders: &[String],
        state: &mut State,
    ) -> Result<()> {
        let failed = state.tally.failed.len();
        let mut in_mailbox = InMailbox::new();
        for folder in folders {
            let place = Place::new(&account.name, folder);
            self.read_folder(&place, mailbox, &mut in_mailbox, state)?;
        }
        if mailbox.flavour() == Flavour::Proton
            && let Some(configured) = &account.folders
        {
            // A label folder's messages are found through the configured
            // folders. Read after one of them failed, it would count the
            // messages that folder did not bring as missing; the next run
            // asks about them in any case.
            if state.tally.failed.len() > failed {
                self.say.line(format_args!(
                    "{}: the label folders are left for the next run, because a folder of the account could not be read",
                    account.name
                ));
            } else {
                self.proton_labels(account, mailbox, configured, &in_mailbox, state)?;
            }
        }
        Ok(())
    }

    /// Open an IMAP folder, and bring the index up to date where the
    /// folder reads it: Gmail's marks are taken back against it, a
    /// folder of Proton Mail Bridge asks it for ids and places, and a
    /// comparison reads the archive's side from it. `None` when the
    /// folder would not open, which the tally then names.
    pub(super) fn open(
        &self,
        place: &Place<'_>,
        mailbox: &mut impl Mailbox,
        state: &mut State,
    ) -> Result<Option<Folder>> {
        let opened = match mailbox.examine(place.folder) {
            Ok(opened) => opened,
            Err(error) => {
                state
                    .tally
                    .failed
                    .push(format!("{}: {error:#}", place.account));
                return Ok(None);
            }
        };
        if mailbox.flavour() != Flavour::Standard || self.options.compare {
            state.standing.refresh(self.archive, self.say)?;
        }
        Ok(Some(opened))
    }

    /// One IMAP folder: from its resume point on, or whole, and what
    /// the last run left on the pending list first. The server's
    /// trouble ends the folder and is named in the tally; the archive's
    /// or the memo's ends the run. Under `--full` the Proton ids the
    /// folder lists are added to `in_mailbox`.
    pub(super) fn read_folder(
        &self,
        place: &Place<'_>,
        mailbox: &mut impl Mailbox,
        in_mailbox: &mut InMailbox,
        state: &mut State,
    ) -> Result<()> {
        let name = &place.name;
        let Some(opened) = self.open(place, mailbox, state)? else {
            return Ok(());
        };
        // From Proton Mail Bridge the archive's side is the Proton id,
        // and a comparison by Message-ID is not needed: --compare lists
        // the folder whole like --full, and neither downloads a message
        // whose id is on record.
        let proton = mailbox.flavour() == Flavour::Proton;
        if self.options.compare && !proton {
            return self.imap_compare(place, mailbox, opened.uidvalidity, state);
        }
        let whole = if self.options.full {
            Some("--full")
        } else if self.options.compare {
            Some("--compare")
        } else {
            None
        };
        let resume = self.memo.imap_resume(place.account, place.folder)?;
        // The pending list holds UIDs of the promise the point was saved
        // under, and is valid as long as that promise is. A fetch of the
        // whole folder keeps it too: the point stays where it is, and
        // when the fetch stops short, the list is all that names the
        // UIDs below the point that are still missing.
        let pending_valid = resume.is_some_and(|point| point.uidvalidity == opened.uidvalidity);
        let (above, how) = carry_on(resume, opened.uidvalidity, whole);
        let uids = match mailbox.uids_above(above) {
            Ok(uids) => uids,
            Err(error) => {
                state.tally.failed.push(format!("{name}: {error:#}"));
                return Ok(());
            }
        };
        let pending: Vec<Pending<()>> = if pending_valid {
            self.memo.pending(place.account, place.folder)?
        } else {
            Vec::new()
        };
        let mut attempts: HashMap<u32, u32> = pending
            .iter()
            .filter_map(|message| Some((message.id.parse().ok()?, message.attempts)))
            .collect();
        let identified = if proton {
            match self.identify(
                place,
                mailbox,
                &uids,
                &attempts,
                in_mailbox,
                &state.standing,
            ) {
                Ok(identified) => identified?,
                Err(error) => {
                    state.tally.failed.push(format!("{name}: {error:#}"));
                    return Ok(());
                }
            }
        } else {
            Identified::default()
        };
        attempts.retain(|uid, _| !identified.settles(*uid));
        // A run stopped short leaves pending UIDs above the point; they
        // are asked for once, as pending.
        let fresh = uids
            .iter()
            .filter(|uid| !attempts.contains_key(uid) && !identified.settles(**uid))
            .count();
        if !attempts.is_empty() {
            self.say.line(format_args!(
                "{name}: {} pending from the last run",
                counted(attempts.len(), "message", "messages")
            ));
        }
        if identified.known.is_empty() {
            self.say.line(format_args!(
                "{name}: {how}, {} to fetch",
                counted(fresh, "message", "messages")
            ));
        } else {
            self.say.line(format_args!(
                "{name}: {how}, {} to fetch, {} found in the archive by their Proton id",
                counted(fresh, "message", "messages"),
                identified.known.len()
            ));
        }
        if self.options.dry_run {
            state.tally.would += fresh + attempts.len();
            state.tally.identified += identified.known.len();
            return Ok(());
        }
        self.imap_fetch(
            place,
            mailbox,
            &uids,
            opened.uidvalidity,
            pending_valid,
            above,
            attempts,
            identified,
            state,
        )?;
        Ok(())
    }

    /// The fetch of a folder once it is decided what to ask for: `uids`
    /// above the point `above`, and the pending UIDs in `attempts` with
    /// the runs that failed them. Without `pending_valid` the folder's
    /// pending list is of another UIDVALIDITY, or of none, and is emptied
    /// first. Answers, under `--compare`, the messages it stored.
    #[allow(
        clippy::too_many_arguments,
        reason = "the parts of one decision, made by two callers"
    )]
    pub(super) fn imap_fetch(
        &self,
        place: &Place<'_>,
        mailbox: &mut impl Mailbox,
        uids: &[u32],
        uidvalidity: u32,
        pending_valid: bool,
        above: u32,
        attempts: HashMap<u32, u32>,
        identified: Identified,
        state: &mut State,
    ) -> Result<Stored> {
        let name = &place.name;
        let fresh = uids
            .iter()
            .filter(|uid| !attempts.contains_key(uid) && !identified.settles(**uid))
            .count();
        let mut walk = Walk {
            uids,
            uidvalidity,
            total: fresh + attempts.len(),
            pending: attempts,
            passed: HashSet::new(),
            frontier: 0,
            identified,
            done: 0,
            refusals: Refusals::default(),
            gone: 0,
            stored: Stored::new(),
            progress: self.say.progress(),
        };
        self.imap_walk(place, mailbox, pending_valid, above, &mut walk, state)?;
        if walk.gone > 0 {
            self.say.line(format_args!(
                "{name}: {} no longer in the folder, skipped",
                counted(walk.gone, "message", "messages")
            ));
        }
        let left = self.memo.pending::<()>(place.account, place.folder)?.len();
        if left > 0 {
            self.say.line(format_args!(
                "{name}: {} not fetched; the next run tries them again",
                counted(left, "message", "messages")
            ));
        }
        Ok(walk.stored)
    }

    /// The fetches of [`read_folder`](Self::read_folder): the pending UIDs, then
    /// the others above the point, committed every [`COMMIT_EVERY`](super::COMMIT_EVERY)
    /// messages.
    fn imap_walk(
        &self,
        place: &Place<'_>,
        mailbox: &mut impl Mailbox,
        pending_valid: bool,
        above: u32,
        walk: &mut Walk<'_>,
        state: &mut State,
    ) -> Result<()> {
        let name = &place.name;
        let uidvalidity = walk.uidvalidity;
        let mut retry: Vec<u32> = walk.pending.keys().copied().collect();
        retry.sort_unstable();
        let fresh: Vec<u32> = walk
            .uids
            .iter()
            .copied()
            .filter(|uid| !walk.pending.contains_key(uid) && !walk.identified.settles(*uid))
            .collect();
        self.memo.begin()?;
        if !pending_valid {
            self.memo.clear_pending(place.account, place.folder)?;
        }
        // A message whose Proton id is on record gets its place without a
        // download, and the point passes it as if it had landed.
        let known = std::mem::take(&mut walk.identified.known);
        for (settled, (uid, receivers)) in known.iter().enumerate() {
            self.assert_on(receivers, &self.place, &json!(place.name), &mut state.tally)?;
            self.memo
                .remove_pending(place.account, place.folder, &uid.to_string())?;
            if let Some(point) = walk.pass(*uid) {
                self.memo.advance_imap(place.account, place.folder, point)?;
            }
            state.tally.identified += 1;
            self.commit_every(settled + 1)?;
        }
        let each: &mut Each<'_> = &mut |uid, got| self.imap_got(place, uid, got, walk, state);
        let mut outcome = mailbox.fetch(&retry, each);
        if matches!(outcome, Ok(ControlFlow::Continue(()))) {
            outcome = mailbox.fetch(&fresh, each);
        }
        walk.progress.finish();
        if outcome.is_ok() {
            // A folder with nothing above the point still has one: the
            // promise it was seen under, and where the next run asks
            // from. An empty folder is not a folder never fetched.
            self.memo.advance_imap(
                place.account,
                place.folder,
                ImapResume {
                    uidvalidity,
                    uid: above,
                },
            )?;
        }
        // What landed before the trouble stays remembered either way.
        let closed = self.memo.commit_for(self.archive);
        match outcome {
            Ok(_) => closed,
            Err(Stop::Server(error)) => {
                state.tally.failed.push(format!("{name}: {error:#}"));
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

    /// What became of one UID goes on the record: a message lands, one
    /// the server refused goes on the pending list or stays there, one
    /// gone comes off it. Says whether the fetch goes on: not after
    /// [`STOP_AFTER`](super::STOP_AFTER) refusals in a row.
    fn imap_got(
        &self,
        place: &Place<'_>,
        uid: u32,
        got: Got<'_>,
        walk: &mut Walk<'_>,
        state: &mut State,
    ) -> Result<ControlFlow<()>> {
        let name = &place.name;
        let id = uid.to_string();
        // Some for a UID the last run left on the pending list.
        let attempts = walk.pending.get(&uid).copied();
        let mut stop = false;
        match got {
            Got::Message(bytes, labels) => {
                let proton_id = walk.identified.ids.get(&uid).cloned();
                let subject =
                    self.land(place, bytes, labels.as_deref(), proton_id.as_deref(), state)?;
                let compared = compare::imap_id(walk.uidvalidity, uid);
                self.note(place, &compared, &subject, &mut walk.stored)?;
                walk.refusals.ended();
                if attempts.is_some() {
                    self.memo.remove_pending(place.account, place.folder, &id)?;
                }
            }
            Got::Gone => {
                walk.gone += 1;
                walk.refusals.ended();
                if attempts.is_some() {
                    self.memo.remove_pending(place.account, place.folder, &id)?;
                }
            }
            Got::Refused(error) => {
                // A UID above the point is not on the list yet.
                if attempts.is_none() {
                    self.memo
                        .add_pending(place.account, place.folder, [(id.as_str(), ())])?;
                }
                let attempts = attempts.unwrap_or(0);
                self.refused(place, &id, attempts, &error, &mut state.tally)?;
                stop = walk.refusals.one_more();
            }
        }
        if let Some(point) = walk.pass(uid) {
            self.memo.advance_imap(place.account, place.folder, point)?;
        }
        walk.done += 1;
        self.commit_every(walk.done)?;
        walk.progress.update(
            walk.done,
            &format!("{name}: {} of {} fetched", walk.done, walk.total),
        );
        if stop {
            Self::stopped(place, walk.total - walk.done, &mut state.tally);
            return Ok(ControlFlow::Break(()));
        }
        Ok(ControlFlow::Continue(()))
    }
}
