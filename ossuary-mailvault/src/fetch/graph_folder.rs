//! A Microsoft 365 account over MS Graph, folder by folder: the delta
//! round, then the messages on the folder's pending list.

use std::collections::HashSet;

use anyhow::{Result, anyhow};

use super::{Fetch, Marks, Place, Refusals, State, Stored, opening};
use crate::config::{self, Account};
use crate::graph::{Graph, Halt, Miss, Round};
use crate::memo::Pending;
use crate::output::counted;
use crate::remote::Labels;

/// What a pass over a folder's pending list came to.
#[derive(Default)]
pub(super) struct Landed {
    /// Still on the list after the pass.
    pub(super) left: usize,
    /// Taken off because the server no longer has them.
    pub(super) gone: usize,
    /// Under `--compare`, the messages the pass stored.
    pub(super) stored: Stored,
}

/// How many messages a Graph fetch would download: those pending from
/// the last run and those the round lists. A message listed again while
/// still pending is one fetch.
fn would_fetch(left: &[Pending<Vec<String>>], round: &Round) -> usize {
    let pending: HashSet<&str> = left.iter().map(|message| message.id.as_str()).collect();
    let new = round
        .offered
        .iter()
        .filter(|message| !pending.contains(message.id.as_str()))
        .count();
    left.len() + new
}

impl Fetch<'_> {
    pub(super) fn graph_account(
        &self,
        account: &Account,
        graph: &config::Graph,
        state: &mut State,
    ) -> Result<Result<()>> {
        // The token and the folder tree say no before the first
        // message is asked for.
        let secret = match graph.secret(&account.name) {
            Ok(secret) => secret.to_string(),
            Err(error) => return Ok(Err(error)),
        };
        let mut client = match Graph::connect(&account.name, graph, secret, self.say) {
            Ok(client) => client,
            Err(error) => return Ok(Err(error)),
        };
        let folders = match &account.folders {
            Some(folders) => folders.clone(),
            None => match account.unskipped(client.folders(), self.accounts_file) {
                Ok(folders) => folders,
                Err(error) => return Ok(Err(error.context(account.name.clone()))),
            },
        };
        self.say
            .line(format_args!("{}", opening(account, folders.len())));
        for folder in &folders {
            self.graph_folder(account, &mut client, folder, state)?;
        }
        Ok(Ok(()))
    }

    /// The listing is complete: the link and the ids are saved
    /// together, before the first message is fetched. A round from
    /// scratch lists the folder whole and replaces an earlier list.
    /// `listed_any` says whether the round listed any message, which
    /// under `--compare` need not be one of those to fetch.
    pub(super) fn graph_listed(
        &self,
        place: &Place<'_>,
        resumed: bool,
        listed_any: bool,
        round: Round,
    ) -> Result<()> {
        let name = &place.name;
        self.memo.begin()?;
        if !resumed {
            self.memo.clear_pending(place.account, place.folder)?;
        }
        self.memo.add_pending(
            place.account,
            place.folder,
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
            Some(link) if resumed || listed_any => {
                self.memo.advance_graph(place.account, place.folder, &link)?;
            }
            Some(_) => self.say.line(format_args!(
                "{name}: no messages listed; no resume point saved"
            )),
            None if resumed => self.say.line(format_args!(
                "{name}: the server returned no resume point; the next run lists the same changes again"
            )),
            None => self.say.line(format_args!(
                "{name}: the server returned no resume point; the next run fetches all messages again"
            )),
        }
        self.memo.commit_for(self.archive)
    }

    /// Every message on the folder's pending list, one request each,
    /// with the marks the listing reported. A message stored, and one
    /// the server no longer has, comes off the list. One the server
    /// refuses is named in the tally and stays on the list, until it
    /// has been refused in [`GIVE_UP_AFTER`](super::GIVE_UP_AFTER) runs. The rest of the list
    /// is left for the next run when the server refuses [`STOP_AFTER`](super::STOP_AFTER)
    /// messages in a row, or gives no usable answer at all. Answers
    /// what the pass came to. With `only`, just those ids of the list
    /// are fetched.
    pub(super) fn graph_land(
        &self,
        place: &Place<'_>,
        client: &mut Graph<'_>,
        only: Option<&HashSet<String>>,
        state: &mut State,
    ) -> Result<Landed> {
        let mut pending: Vec<Pending<Vec<String>>> =
            self.memo.pending(place.account, place.folder)?;
        if let Some(only) = only {
            pending.retain(|message| only.contains(&message.id));
        }
        self.memo.begin()?;
        let outcome = self.graph_each(place, &pending, client, state);
        // What came off the list before the trouble stays off it
        // either way.
        let closed = self.memo.commit_for(self.archive);
        match outcome {
            Ok(landed) => {
                closed?;
                if landed.gone > 0 {
                    self.say.line(format_args!(
                        "{}: {} no longer in the folder, skipped",
                        place.name,
                        counted(landed.gone, "message", "messages")
                    ));
                }
                Ok(landed)
            }
            Err(error) => Err(match closed {
                Ok(()) => error,
                Err(more) => {
                    anyhow!("{error:#}; the pending list could not be saved either: {more:#}")
                }
            }),
        }
    }

    /// The loop of [`graph_land`](Self::graph_land), inside the
    /// batch of memo writes it opened.
    fn graph_each(
        &self,
        place: &Place<'_>,
        pending: &[Pending<Vec<String>>],
        client: &mut Graph<'_>,
        state: &mut State,
    ) -> Result<Landed> {
        let name = &place.name;
        let mut progress = self.say.progress();
        let mut landed = Landed::default();
        let mut refusals = Refusals::default();
        for (done, message) in pending.iter().enumerate() {
            match client.message(&message.id) {
                Ok(bytes) => {
                    // Graph lists every category of a message.
                    let marks = Marks {
                        tags: &message.detail,
                        listed: &Labels::Whole,
                    };
                    let subject = self.land(place, &bytes, Some(&marks), None, state)?;
                    self.note(place, &message.id, &subject, &mut landed.stored)?;
                    self.memo
                        .remove_pending(place.account, place.folder, &message.id)?;
                    refusals.ended();
                }
                Err(Miss::Gone) => {
                    landed.gone += 1;
                    self.memo
                        .remove_pending(place.account, place.folder, &message.id)?;
                    refusals.ended();
                }
                Err(Miss::Refused(error)) => {
                    let tally = &mut state.tally;
                    if self.refused(place, &message.id, message.attempts, &error, tally)? {
                        landed.left += 1;
                    }
                    if refusals.one_more() {
                        let rest = pending.len() - done - 1;
                        landed.left += rest;
                        Self::stopped(place, rest, tally);
                        break;
                    }
                }
                Err(Miss::Unreachable(error)) => {
                    let rest = pending.len() - done;
                    landed.left += rest;
                    state.tally.failed.push(format!(
                        "{name}: {error:#}; {} left for the next run",
                        counted(rest, "message", "messages")
                    ));
                    break;
                }
            }
            self.commit_every(done + 1)?;
            progress.update(
                done + 1,
                &format!("{name}: {} of {} fetched", done + 1, pending.len()),
            );
        }
        progress.finish();
        Ok(landed)
    }

    /// One Graph folder: a delta round from its link on, or whole. The
    /// link and the ids the round listed are saved before any message
    /// is fetched, then everything pending for the folder is fetched,
    /// what an earlier run left included.
    pub(super) fn graph_folder(
        &self,
        account: &Account,
        client: &mut Graph<'_>,
        folder: &str,
        state: &mut State,
    ) -> Result<()> {
        let place = Place::new(&account.name, folder);
        let name = &place.name;
        // Categories are taken back against the record.
        state.standing.refresh(self.archive, self.say)?;
        let Some(id) = client.resolve(folder).map(str::to_string) else {
            state.tally.failed.push(format!(
                "{name}: no such folder; the mailbox has {}",
                client.folders().join(", ")
            ));
            return Ok(());
        };
        if self.options.compare {
            return self.graph_compare(&place, client, &id, state);
        }
        let point = if self.options.full {
            None
        } else {
            self.memo.graph_resume(place.account, place.folder)?
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
                    Err(Halt::Failed(error)) => {
                        state.tally.failed.push(format!("{name}: {error:#}"));
                        return Ok(());
                    }
                    // A round from scratch has no link to expire.
                    Err(Halt::Expired) => {
                        state.tally.failed.push(format!(
                            "{name}: the server did not list the folder's messages"
                        ));
                        return Ok(());
                    }
                }
            }
            Err(Halt::Failed(error)) => {
                state.tally.failed.push(format!("{name}: {error:#}"));
                return Ok(());
            }
        };
        // What an earlier run left on the list is still to fetch when
        // the round carries on from its link; a round from scratch
        // lists it again.
        let left: Vec<Pending<Vec<String>>> = if from.is_some() {
            self.memo.pending(place.account, place.folder)?
        } else {
            Vec::new()
        };
        if !left.is_empty() {
            self.say.line(format_args!(
                "{name}: {} pending from the last run",
                counted(left.len(), "message", "messages")
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
            state.tally.would += would_fetch(&left, &round);
            return Ok(());
        }

        let listed_any = !round.offered.is_empty();
        self.graph_listed(&place, from.is_some(), listed_any, round)?;
        let left = self.graph_land(&place, client, None, state)?.left;
        if left > 0 {
            self.say.line(format_args!(
                "{name}: {} not fetched; the next run tries them again",
                counted(left, "message", "messages")
            ));
        }
        Ok(())
    }
}
