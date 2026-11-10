//! `fetch --compare`: list the folder whole and fetch only what the
//! archive does not hold under the same Message-ID, and what no earlier
//! `--compare` fetched and found to be archived.
//!
//! The messages to fetch replace the folder's pending list, and the
//! resume point is set to the top of the listing, both before the first
//! message is fetched: a run that stops early leaves exactly those on
//! the list for the next one. The list is then fetched as by a resumed
//! run. If a fetched message turns out to be archived under another
//! Message-ID, the messages matched to that Message-ID are compared
//! again (see [`Comparison::landed`]).

use std::collections::{HashMap, HashSet};

use anyhow::Result;

use super::bridge::Identified;
use super::{Fetch, Place, Standing, State, Stored};
use crate::compare::{self, Archived, Comparison, Split};
use crate::graph::{Graph, Halt, Offered, Round};
use crate::memo::ImapResume;
use crate::output::counted;
use crate::remote::Mailbox;

impl Fetch<'_> {
    /// The archive's side of the folder and what earlier `--compare`
    /// runs fetched from it, and a word when some archived messages
    /// have no Message-ID on record.
    fn comparison(&self, place: &Place<'_>, standing: &Standing) -> Result<Comparison> {
        let archived = Archived::read(standing.index(), &self.place, &place.name)?;
        let known = self.memo.compared(place.account, place.folder)?;
        let comparison = Comparison::new(archived, known);
        let without = comparison.without_message_id();
        if without > 0 {
            let has = if without == 1 { "has" } else { "have" };
            let fetched = if self.options.dry_run {
                "would be fetched"
            } else {
                "are fetched"
            };
            self.say.line(format_args!(
                "{}: {without} of {} archived messages {has} no Message-ID on record, and their copies on the server {fetched} again; run `ossuary extract mail` first if it has not run since the last fetch",
                place.name,
                comparison.archived()
            ));
        }
        Ok(comparison)
    }

    /// Remove these messages from what earlier `--compare` runs fetched
    /// from the folder.
    fn forget<'a>(&self, place: &Place<'_>, ids: impl IntoIterator<Item = &'a str>) -> Result<()> {
        self.memo.begin()?;
        self.memo
            .remove_compared(place.account, place.folder, ids)?;
        self.memo.commit_for(self.archive)
    }

    /// After the last round: what the Message-ID settled needs no row
    /// in the memo.
    fn forget_settled<T>(
        &self,
        place: &Place<'_>,
        comparison: &Comparison,
        kept: &compare::Keyed<T>,
        id: impl Fn(&T) -> String,
    ) -> Result<()> {
        let settled: Vec<String> = kept
            .iter()
            .map(|(item, _)| id(item))
            .filter(|id| comparison.remembers(id))
            .collect();
        self.forget(place, settled.iter().map(String::as_str))
    }

    /// The line that opens a compared folder.
    fn compared(&self, place: &Place<'_>, wanted: usize, on_server: usize) {
        self.say.line(format_args!(
            "{}: compared with the archive, {wanted} of {} to fetch",
            place.name,
            counted(on_server, "message", "messages")
        ));
    }

    /// The line that opens another round.
    fn reopened(&self, place: &Place<'_>, more: usize) {
        self.say.line(format_args!(
            "{}: {} more to fetch: an archived message is listed on the server under another Message-ID",
            place.name,
            counted(more, "message", "messages")
        ));
    }

    /// Hand the comparison what the last round stored, by the server's
    /// Message-ID for each id. Answers whether anything was reopened.
    fn settle(comparison: &mut Comparison, keys: &HashMap<String, String>, stored: Stored) -> bool {
        let mut reopened = false;
        for (id, subject) in stored {
            if let Some(key) = keys.get(&id) {
                reopened |= comparison.landed(key, &subject);
            }
        }
        reopened
    }

    pub(super) fn imap_compare(
        &self,
        place: &Place<'_>,
        mailbox: &mut impl Mailbox,
        uidvalidity: u32,
        state: &mut State,
    ) -> Result<()> {
        let name = &place.name;
        let mut comparison = self.comparison(place, &state.standing)?;
        let listed = match mailbox
            .uids_above(0)
            .and_then(|uids| mailbox.header(&uids, compare::HEADER))
        {
            Ok(listed) => listed,
            Err(error) => {
                state.tally.failed.push(format!("{name}: {error:#}"));
                return Ok(());
            }
        };
        let top = listed.iter().map(|(uid, _)| *uid).max().unwrap_or(0);
        let listed: Vec<(u32, String)> = listed
            .into_iter()
            .map(|(uid, value)| (uid, compare::key(&value)))
            .collect();
        let id = |uid: &u32| compare::imap_id(uidvalidity, *uid);
        let keys: HashMap<String, String> = listed
            .iter()
            .map(|(uid, key)| (id(uid), key.clone()))
            .collect();
        let on_server = listed.len();
        let Split {
            mut wanted,
            mut kept,
            recalled,
        } = comparison.split(listed, id);
        state.tally.recalled += recalled;
        self.compared(place, wanted.len(), on_server);
        if self.options.dry_run {
            state.tally.would += wanted.len();
            state.tally.matched += kept.len();
            return Ok(());
        }
        self.forget(place, comparison.stale(|id| keys.contains_key(id)))?;
        let mut first = true;
        loop {
            let ids: Vec<String> = wanted.iter().map(|(uid, _)| uid.to_string()).collect();
            self.memo.begin()?;
            if first {
                self.memo.clear_pending(place.account, place.folder)?;
            }
            self.memo.add_pending(
                place.account,
                place.folder,
                ids.iter().map(|id| (id.as_str(), ())),
            )?;
            self.memo.advance_imap(
                place.account,
                place.folder,
                ImapResume {
                    uidvalidity,
                    uid: top,
                },
            )?;
            self.memo.commit_for(self.archive)?;
            if wanted.is_empty() {
                break;
            }
            // Only this round's: a UID refused in an earlier round waits
            // for the next run, as it would after any other fetch.
            let attempts: HashMap<u32, u32> = wanted.iter().map(|(uid, _)| (*uid, 0)).collect();
            let stored = self.imap_fetch(
                place,
                mailbox,
                &[],
                uidvalidity,
                true,
                top,
                attempts,
                Identified::default(),
                state,
            )?;
            if !Self::settle(&mut comparison, &keys, stored) {
                break;
            }
            let more = comparison.split(kept, id);
            kept = more.kept;
            state.tally.recalled += more.recalled;
            if more.wanted.is_empty() {
                break;
            }
            self.reopened(place, more.wanted.len());
            wanted = more.wanted;
            first = false;
        }
        state.tally.matched += kept.len();
        self.forget_settled(place, &comparison, &kept, id)
    }

    pub(super) fn graph_compare(
        &self,
        place: &Place<'_>,
        client: &mut Graph<'_>,
        folder_id: &str,
        state: &mut State,
    ) -> Result<()> {
        let name = &place.name;
        let mut comparison = self.comparison(place, &state.standing)?;
        let Round {
            offered,
            gone,
            link,
        } = match client.listing(folder_id) {
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
        };
        let listed: Vec<(Offered, String)> = offered
            .into_iter()
            .map(|message| {
                let key = compare::key(message.message_id.as_deref().unwrap_or_default());
                (message, key)
            })
            .collect();
        let keys: HashMap<String, String> = listed
            .iter()
            .map(|(message, key)| (message.id.clone(), key.clone()))
            .collect();
        let on_server = listed.len();
        let id = |message: &Offered| message.id.clone();
        let Split {
            wanted,
            mut kept,
            recalled,
        } = comparison.split(listed, id);
        state.tally.recalled += recalled;
        self.compared(place, wanted.len(), on_server);
        if self.options.dry_run {
            state.tally.would += wanted.len();
            state.tally.matched += kept.len();
            return Ok(());
        }
        self.forget(place, comparison.stale(|id| keys.contains_key(id)))?;
        let mut only: HashSet<String> = wanted
            .iter()
            .map(|(message, _)| message.id.clone())
            .collect();
        let round = Round {
            offered: wanted.into_iter().map(|(message, _)| message).collect(),
            gone,
            link,
        };
        self.graph_listed(place, false, on_server > 0, round)?;
        while !only.is_empty() {
            let landed = self.graph_land(place, client, Some(&only), state)?;
            if landed.left > 0 {
                self.say.line(format_args!(
                    "{name}: {} not fetched; the next run tries them again",
                    counted(landed.left, "message", "messages")
                ));
            }
            if !Self::settle(&mut comparison, &keys, landed.stored) {
                break;
            }
            let more = comparison.split(kept, id);
            kept = more.kept;
            state.tally.recalled += more.recalled;
            if more.wanted.is_empty() {
                break;
            }
            self.reopened(place, more.wanted.len());
            self.memo.begin()?;
            self.memo.add_pending(
                place.account,
                place.folder,
                more.wanted
                    .iter()
                    .map(|(message, _)| (message.id.as_str(), &message.tags)),
            )?;
            self.memo.commit_for(self.archive)?;
            only = more
                .wanted
                .into_iter()
                .map(|(message, _)| message.id)
                .collect();
        }
        state.tally.matched += kept.len();
        self.forget_settled(place, &comparison, &kept, id)
    }
}
