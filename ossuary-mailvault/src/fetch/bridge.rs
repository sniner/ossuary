//! Proton Mail Bridge: a message is known by its Proton id before it is
//! downloaded, and a label folder tags messages without downloading
//! them (see [`crate::proton`]).

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use ossuary_core::{Claim, Scope, Subject, Term, Timestamp};
use serde_json::json;

use super::imap_folder::carry_on;
use super::{Fetch, Place, Standing, State};
use crate::config::Account;
use crate::memo::ImapResume;
use crate::output::counted;
use crate::place;
use crate::proton;
use crate::remote::Mailbox;

/// Under `--full`, the Proton ids that the configured folders of one
/// account listed: the messages still in the mailbox. The full listing
/// makes them at no extra cost; a label is taken back only from one of
/// these.
pub(super) type InMailbox = HashSet<String>;

/// What the Proton ids settled before a folder downloads anything.
#[derive(Default)]
pub(super) struct Identified {
    /// UIDs whose id is on record, with the files their place goes to:
    /// settled without a download. The list may be empty, when every
    /// file with the id has the place already.
    pub(super) known: Vec<(u32, Vec<Subject>)>,
    /// The UIDs in `known`.
    settled: HashSet<u32>,
    /// The id of each UID to download, recorded with it.
    pub(super) ids: HashMap<u32, String>,
}

impl Identified {
    /// Whether the UID needs no download.
    pub(super) fn settles(&self, uid: u32) -> bool {
        self.settled.contains(&uid)
    }
}

impl Fetch<'_> {
    /// Proton Mail Bridge shows each label as a folder under `Labels/`.
    /// Those not configured as folders are read after the configured
    /// ones, for the labels of their messages. Without a folders list
    /// every folder is fetched, and a label folder is a place like any
    /// other.
    pub(super) fn proton_labels(
        &self,
        account: &Account,
        mailbox: &mut impl Mailbox,
        configured: &[String],
        in_mailbox: &InMailbox,
        state: &mut State,
    ) -> Result<()> {
        let listed = match mailbox.folders() {
            Ok(listed) => listed,
            Err(error) => {
                state
                    .tally
                    .failed
                    .push(format!("{}: {error:#}", account.name));
                return Ok(());
            }
        };
        let labels: Vec<(&str, &str)> = listed
            .iter()
            .filter(|folder| !configured.contains(folder))
            .filter_map(|folder| Some((folder.as_str(), place::proton_label(folder)?)))
            .collect();
        if labels.is_empty() {
            return Ok(());
        }
        let their = if labels.len() == 1 { "its" } else { "their" };
        self.say.line(format_args!(
            "{}: reading {} to tag {their} messages",
            account.name,
            counted(labels.len(), "label folder", "label folders")
        ));
        for (folder, label) in labels {
            let place = Place::new(&account.name, folder);
            self.label_folder(&place, label, mailbox, in_mailbox, state)?;
        }
        Ok(())
    }

    /// The Proton id of each UID a folder would download, `uids` and the
    /// pending ones, before anything is downloaded: the messages whose id
    /// is on record, with the files their place goes to, and the id of
    /// each of the others. The outer `Result` is the server's trouble,
    /// which ends the folder; the inner the archive's, which ends the
    /// run.
    pub(super) fn identify(
        &self,
        place: &Place<'_>,
        mailbox: &mut impl Mailbox,
        uids: &[u32],
        attempts: &HashMap<u32, u32>,
        in_mailbox: &mut InMailbox,
        standing: &Standing,
    ) -> Result<Result<Identified>> {
        let mut asked: Vec<u32> = uids.iter().chain(attempts.keys()).copied().collect();
        asked.sort_unstable();
        asked.dedup();
        let listed = mailbox.header(&asked, proton::HEADER)?;
        if self.options.full {
            in_mailbox.extend(listed.iter().filter_map(|(_, value)| proton::id(value)));
        }
        Ok(self.identified(place, &asked, &listed, standing))
    }

    /// The archive's side of [`identify`](Self::identify), once the
    /// server has listed the ids.
    fn identified(
        &self,
        place: &Place<'_>,
        asked: &[u32],
        listed: &[(u32, String)],
        standing: &Standing,
    ) -> Result<Identified> {
        let record = self.proton_record(standing);
        let sorted = proton::sort(asked, listed, |id| record.holders(id))?;
        let here = json!(place.name);
        let mut known = Vec::with_capacity(sorted.known.len());
        for (uid, holders) in sorted.known {
            known.push((uid, record.receivers(&holders, &self.place, &here)?));
        }
        let ids = sorted
            .fetch
            .into_iter()
            .filter_map(|(uid, id)| Some((uid, id?)))
            .collect();
        let settled = known.iter().map(|(uid, _)| *uid).collect();
        Ok(Identified {
            known,
            settled,
            ids,
        })
    }

    /// The record's side of the questions a folder of Proton Mail Bridge
    /// asks: which files carry an id, and which of them a fact goes to.
    fn proton_record<'s>(&'s self, standing: &'s Standing) -> proton::Record<'s> {
        proton::Record {
            index: standing.index(),
            id: &self.id,
            place: &self.place,
        }
    }

    /// Under `--full`: the files that carry `label` but whose Proton id
    /// the label folder no longer lists (`in_folder`), while a configured
    /// folder of the account still does (`in_mailbox`). The label was
    /// removed from them. A message no longer in the mailbox keeps its
    /// labels, as from Gmail, and a file without a Proton id is not this
    /// folder's to judge.
    fn label_removed(
        &self,
        label: &str,
        in_folder: &HashSet<String>,
        in_mailbox: &InMailbox,
        standing: &Standing,
    ) -> Result<Vec<Subject>> {
        let index = standing.index();
        // Quoted: the label is meant literally.
        let terms = [
            Term::Attribute(self.tag.clone(), format!("\"{label}\"")),
            Term::Attribute(self.id.clone(), "proton:*".to_string()),
        ];
        let mut gone = Vec::new();
        for subject in index.find(&terms, &[], Scope::Held)? {
            let ids: Vec<String> = index
                .values(&subject, &self.id, Scope::Held)?
                .into_iter()
                .filter_map(|value| value.as_str().map(ToString::to_string))
                .collect();
            let present = ids.iter().any(|id| in_mailbox.contains(id));
            let labelled = ids.iter().any(|id| in_folder.contains(id));
            if present && !labelled {
                gone.push(subject);
            }
        }
        Ok(gone)
    }

    /// A label folder of Proton Mail Bridge. Its messages are in `All
    /// Mail` as well, so nothing is downloaded or stored here. The UIDs
    /// above the folder's resume point, and those left open by earlier
    /// runs, are looked up by their Proton id, and the files with the id
    /// get the label. A UID whose id is not on record yet stays open, and
    /// the next run asks about it again, after `All Mail` has brought it;
    /// unlike a pending message it is never given up. The point passes
    /// every UID asked about, since each is then tagged or open.
    pub(super) fn label_folder(
        &self,
        place: &Place<'_>,
        label: &str,
        mailbox: &mut impl Mailbox,
        in_mailbox: &InMailbox,
        state: &mut State,
    ) -> Result<()> {
        let name = &place.name;
        let Some(opened) = self.open(place, mailbox, state)? else {
            return Ok(());
        };
        let uidvalidity = opened.uidvalidity;
        let resume = self.memo.imap_resume(place.account, place.folder)?;
        let resumed =
            !self.options.full && resume.is_some_and(|point| point.uidvalidity == uidvalidity);
        let (above, how) = carry_on(resume, uidvalidity, self.options.full.then_some("--full"));
        let uids = match mailbox.uids_above(above) {
            Ok(uids) => uids,
            Err(error) => {
                state.tally.failed.push(format!("{name}: {error:#}"));
                return Ok(());
            }
        };
        let open: Vec<u32> = if resumed {
            self.memo
                .pending::<()>(place.account, place.folder)?
                .iter()
                .filter_map(|entry| entry.id.parse().ok())
                .collect()
        } else {
            Vec::new()
        };
        let mut asked: Vec<u32> = uids.iter().chain(&open).copied().collect();
        asked.sort_unstable();
        asked.dedup();
        let listed = if asked.is_empty() {
            Vec::new()
        } else {
            match mailbox.header(&asked, proton::HEADER) {
                Ok(listed) => listed,
                Err(error) => {
                    state.tally.failed.push(format!("{name}: {error:#}"));
                    return Ok(());
                }
            }
        };
        let sorted = self.sort_labelled(&asked, &listed, &state.standing)?;
        // A UID the server no longer lists, or one without an id, can
        // never be found; only an id not on record yet stays open.
        let still_open: Vec<u32> = sorted
            .fetch
            .iter()
            .filter_map(|(uid, id)| id.as_ref().map(|_| *uid))
            .collect();
        if asked.is_empty() {
            self.say
                .line(format_args!("{name}: {how}, nothing to look up"));
        } else if still_open.is_empty() {
            self.say.line(format_args!(
                "{name}: {how}, {} to look up, all found in the archive",
                counted(asked.len(), "message", "messages")
            ));
        } else {
            self.say.line(format_args!(
                "{name}: {how}, {} to look up, {} not in the archive yet; the next run looks for them again",
                counted(asked.len(), "message", "messages"),
                still_open.len()
            ));
        }
        // Under --full the folder is listed whole, and with the listing of
        // the configured folders that shows which labels were removed.
        let in_folder: Option<HashSet<String>> = self.options.full.then(|| {
            listed
                .iter()
                .filter_map(|(_, value)| proton::id(value))
                .collect()
        });
        if self.options.dry_run {
            if let Some(in_folder) = &in_folder {
                let gone = self.label_removed(label, in_folder, in_mailbox, &state.standing)?;
                if !gone.is_empty() {
                    self.say.line(format_args!(
                        "{name}: the label would be retracted from {} no longer in the folder",
                        counted(gone.len(), "message", "messages")
                    ));
                }
            }
            return Ok(());
        }
        self.memo.begin()?;
        if !resumed {
            self.memo.clear_pending(place.account, place.folder)?;
        }
        self.tag_labelled(&sorted.known, label, state)?;
        if let Some(in_folder) = &in_folder {
            self.retract_label(name, label, in_folder, in_mailbox, state)?;
        }
        let top = uids.iter().copied().max().unwrap_or(above).max(above);
        self.settle_label_folder(place, &open, &still_open, top, uidvalidity)
    }

    /// `asked` sorted by what the record holds of each UID's Proton id.
    fn sort_labelled(
        &self,
        asked: &[u32],
        listed: &[(u32, String)],
        standing: &Standing,
    ) -> Result<proton::Sorted> {
        let record = self.proton_record(standing);
        proton::sort(asked, listed, |id| record.holders(id))
    }

    /// Tag the files of the UIDs found in the record with `label`, those
    /// that do not have it yet.
    fn tag_labelled(
        &self,
        known: &[(u32, Vec<Subject>)],
        label: &str,
        state: &mut State,
    ) -> Result<()> {
        let value = json!(label);
        let record = self.proton_record(&state.standing);
        // One append for the whole folder: each append lists the open
        // segment first, and over SMB that listing takes up to a second
        // once the previous append has changed the directory.
        let mut receivers = Vec::new();
        for (_, holders) in known {
            receivers.extend(record.receivers(holders, &self.tag, &value)?);
        }
        receivers.sort();
        receivers.dedup();
        self.assert_on(&receivers, &self.tag, &value, &mut state.tally)
    }

    /// Under `--full`: retract the label from the files that the label
    /// folder no longer lists (see [`label_removed`](Self::label_removed)).
    fn retract_label(
        &self,
        name: &str,
        label: &str,
        in_folder: &HashSet<String>,
        in_mailbox: &InMailbox,
        state: &mut State,
    ) -> Result<()> {
        let gone = self.label_removed(label, in_folder, in_mailbox, &state.standing)?;
        let time = Timestamp::now();
        let mut claims = Vec::new();
        for subject in &gone {
            let ids = state.standing.ids_of(subject, &self.tag, label)?;
            if ids.is_empty() {
                continue;
            }
            claims.push(Claim::retract(
                subject.clone(),
                self.tag.clone(),
                ids,
                time.clone(),
                self.source.clone(),
                state.tally.run.clone(),
            )?);
        }
        self.archive.log().append_all(&claims)?;
        state.tally.claims += claims.len();
        state.tally.taken += claims.len();
        if !gone.is_empty() {
            self.say.line(format_args!(
                "{name}: the label was retracted from {} no longer in the folder",
                counted(gone.len(), "message", "messages")
            ));
        }
        Ok(())
    }

    /// The end of a label folder's run: the open UIDs that were found
    /// leave the pending list, the ones still open are added to it, the
    /// resume point moves to `top`, and the batch is committed.
    fn settle_label_folder(
        &self,
        place: &Place<'_>,
        open: &[u32],
        still_open: &[u32],
        top: u32,
        uidvalidity: u32,
    ) -> Result<()> {
        for uid in open {
            if !still_open.contains(uid) {
                self.memo
                    .remove_pending(place.account, place.folder, &uid.to_string())?;
            }
        }
        let ids: Vec<String> = still_open.iter().map(ToString::to_string).collect();
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
        self.memo.commit_for(self.archive)
    }
}
