//! Fetching: every configured mailbox, folder by folder, whatever
//! arrived since the last run.
//!
//! A folder is opened read-only and its resume point looked up in the
//! memo: the UIDVALIDITY the server promised last time, and the highest
//! UID fetched under it. If the promise still holds, the server is
//! asked only for what lies above that UID and answers with the new
//! messages alone. If the promise changed, or there is no resume point,
//! the folder is fetched whole: the bytes dedup in the store, and the
//! places are said again. Each message goes in through the archive's
//! two-step accession with its place — account and folder — as the one
//! fact the fetcher has to tell about it.
//!
//! The resume point moves forward as messages land, in batches, so a
//! run that dies carries on from close to where it was. It lives in
//! `cache/`: losing it costs one whole fetch of the folder, never a
//! claim.
//!
//! One account failing costs that account: the password, the login,
//! a folder that will not open are named in the tally, and the run
//! goes on to the next. The archive or the memo refusing ends the run.

use std::collections::HashSet;

use anyhow::Result;
use ossuary_core::{Archive, Attribute, Sighting, Source, admit, record};
use serde_json::json;

use crate::config::Account;
use crate::memo::{Memo, Resume};
use crate::output::{Say, counted};
use crate::place;
use crate::remote::{Mailbox, Remote, Stop};
use crate::tally::Tally;

pub struct Options {
    /// Fetch every message, the resume point notwithstanding.
    pub full: bool,
    /// Let `password_cmd` run.
    pub allow_exec: bool,
    /// Count what would be fetched, fetch nothing.
    pub dry_run: bool,
}

/// How many messages land between two commits of the resume point.
const COMMIT_EVERY: usize = 25;

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
    let fetch = Fetch {
        archive,
        memo,
        options,
        source: Source::parse(crate::SOURCE)?,
        place: Attribute::parse(place::ATTRIBUTE)?,
        say,
    };
    let mut tally = Tally::new("fetch");
    for account in accounts {
        if let Err(error) = fetch.account(account, &mut tally)? {
            tally.failed.push(format!("{error:#}"));
        }
    }
    Ok(tally)
}

/// One fetch in progress: what it works on, and how it speaks.
struct Fetch<'a> {
    archive: &'a Archive,
    memo: &'a Memo,
    options: &'a Options,
    source: Source,
    place: Attribute,
    say: Say,
}

impl Fetch<'_> {
    /// One account. The outer `Result` is the archive's trouble, which
    /// ends the run; the inner is this account's, which ends only the
    /// account.
    fn account(&self, account: &Account, tally: &mut Tally) -> Result<Result<()>> {
        // Everything that can say no before the first byte is fetched
        // says it here: the password, the login, the folder list.
        let password = match account.password(self.options.allow_exec) {
            Ok(password) => password,
            Err(error) => return Ok(Err(error)),
        };
        let mut remote = match Remote::connect(account, &password) {
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

    /// One folder: from its resume point on, or whole. The server's
    /// trouble ends the folder and is named in the tally; the archive's
    /// or the memo's ends the run.
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
        let resume = self.memo.resume(&account.name, folder)?;
        let (above, how) = carry_on(resume, opened.uidvalidity, self.options.full);
        let uids = match mailbox.uids_above(above) {
            Ok(uids) => uids,
            Err(error) => {
                tally.failed.push(format!("{name}: {error:#}"));
                return Ok(());
            }
        };
        self.say.line(format_args!(
            "{name}: {how} — {} to fetch",
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
            let admitted = admit(self.archive.content(), bytes)?;
            let facts = [(self.place.clone(), json!(name))];
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
            // The message is on the record; the next run may start above
            // the highest UID with nothing missing below it. Committed in
            // batches: a run that dies repeats at most a batch, and the
            // bytes dedup.
            landed.insert(uid);
            while uids.get(frontier).is_some_and(|next| landed.contains(next)) {
                frontier += 1;
            }
            if frontier > 0 {
                self.memo.advance(
                    &account.name,
                    folder,
                    Resume {
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
            self.memo.advance(
                &account.name,
                folder,
                Resume {
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
                Err(more) => error.context(format!("and the memo did not commit: {more:#}")),
            }),
        }
    }
}

/// Where a folder's fetch carries on from, and how to say so: above
/// the last UID when the server's promise still holds, from the start
/// otherwise — or when `full` asks for everything.
fn carry_on(resume: Option<Resume>, uidvalidity: u32, full: bool) -> (u32, String) {
    match resume {
        _ if full => (0, "everything, as --full asks".to_string()),
        Some(point) if point.uidvalidity == uidvalidity => {
            (point.uid, format!("carrying on above UID {}", point.uid))
        }
        Some(_) => (
            0,
            "the server renumbered the folder, fetching it whole".to_string(),
        ),
        None => (0, "first fetch, the whole folder".to_string()),
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

    fn at(uidvalidity: u32, uid: u32) -> Resume {
        Resume { uidvalidity, uid }
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
            account: Account {
                name: "example.org".to_string(),
                host: "imap.example.org".to_string(),
                port: 993,
                tls: true,
                user: "john".to_string(),
                password: None,
                password_cmd: None,
                folders: None,
            },
        }
    }

    fn fetch(bench: &Bench, mailbox: &mut Fake, full: bool, dry_run: bool) -> Tally {
        let options = Options {
            full,
            allow_exec: false,
            dry_run,
        };
        let fetch = Fetch {
            archive: &bench.archive,
            memo: &bench.memo,
            options: &options,
            source: Source::parse(crate::SOURCE).unwrap(),
            place: Attribute::parse(place::ATTRIBUTE).unwrap(),
            say: Say::new(true),
        };
        let mut tally = Tally::new("fetch");
        fetch
            .folder(&bench.account, mailbox, "INBOX", &mut tally)
            .unwrap();
        tally
    }

    fn point(bench: &Bench) -> Option<Resume> {
        bench.memo.resume("example.org", "INBOX").unwrap()
    }

    #[test]
    fn a_first_fetch_takes_the_folder_whole_and_leaves_the_point_at_the_top() {
        let bench = bench();
        let mut inbox = mailbox(7, &[3, 5, 9]);

        let tally = fetch(&bench, &mut inbox, false, false);

        assert_eq!((tally.stored, tally.known), (3, 0));
        assert_eq!(tally.claims, 12, "place, run, size, kind — each message");
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
            tally.claims, 6,
            "place, run and the told kind, each message — the size the log has"
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
        assert!(tally.failed[0].contains("example.org/INBOX"));
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
}
