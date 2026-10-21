//! The IMAP side. Folders are opened read-only — a fetcher that changes
//! `\Seen` flags on the server has overstepped its job. Folder names
//! cross this boundary in the server's own encoding ([`utf7`]) and are
//! plain text on every other side of it.
//!
//! What the fetch needs of a mailbox is the [`Mailbox`] trait; the
//! server answers it here, and a test answers it from memory.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use imap::Session;
use imap::types::NameAttribute;
use rustls::pki_types::ServerName;
use rustls::{ClientConnection, RootCertStore, StreamOwned};

use crate::config::Imap;
use crate::utf7;

/// A folder opened: what the server promised about its UIDs.
pub struct Folder {
    pub uidvalidity: u32,
}

/// What became of one UID a fetch asked for.
pub enum Got<'a> {
    /// The message, whole.
    Message(&'a [u8]),
    /// The server would not hand it over: it refused the FETCH, or
    /// answered without the message. A later run may get it.
    Refused(anyhow::Error),
    /// The answer leaves it out: the folder no longer holds it.
    Gone,
}

/// Why a fetch stopped short.
pub enum Stop {
    /// The server would not go on — named in the tally, the run goes on
    /// to the next folder.
    Server(anyhow::Error),
    /// What the caller did with a message went wrong; handed back as it
    /// was, for the caller to judge.
    Caller(anyhow::Error),
}

/// What the fetch asks of a mailbox, once it is logged in.
pub trait Mailbox {
    /// Opens a folder read-only.
    ///
    /// # Errors
    ///
    /// A folder that will not open, or one whose UIDVALIDITY the server
    /// does not state.
    fn examine(&mut self, folder: &str) -> Result<Folder>;

    /// The UIDs above `above`, ascending — every UID of the folder for
    /// `0`. A folder must be examined first.
    ///
    /// # Errors
    ///
    /// A search the server refuses.
    fn uids_above(&mut self, above: u32) -> Result<Vec<u32>>;

    /// The Message-ID header of each of `uids`, as the message spells
    /// it, unfolded; `""` for a message without one. A UID the server
    /// leaves out of its answer is left out here.
    ///
    /// # Errors
    ///
    /// A fetch the server refuses.
    fn message_ids(&mut self, uids: &[u32]) -> Result<Vec<(u32, String)>>;

    /// Whole messages by UID. Every UID asked for is handed to `each`
    /// once, with what became of it, in the order the server answers,
    /// which need not be ascending. `each` says whether to go on, and a
    /// [`ControlFlow::Break`] from it is handed back.
    ///
    /// # Errors
    ///
    /// [`Stop::Server`] when the server will not go on, [`Stop::Caller`]
    /// with whatever `each` answered.
    fn fetch(&mut self, uids: &[u32], each: &mut Each<'_>) -> Result<ControlFlow<()>, Stop>;
}

/// What [`Mailbox::fetch`] hands every UID to.
pub type Each<'e> = dyn FnMut(u32, Got<'_>) -> Result<ControlFlow<()>> + 'e;

/// What one FETCH command came to.
enum Answer {
    /// The server answered, and every UID asked for went to `each`.
    Went(ControlFlow<()>),
    /// The server refused the command as a whole.
    Refused(anyhow::Error),
}

/// A connection, whichever way it was made: TLS to a server, plaintext
/// to a local bridge. Past the login nothing needs to know which.
trait Stream: Read + Write {}
impl<T: Read + Write> Stream for T {}

pub struct Remote {
    session: Session<Box<dyn Stream>>,
}

/// How many messages one FETCH asks for. Messages come whole — a mail
/// is bounded in size, a server sees to that — and stay in memory until
/// stored, so the batch bounds the memory, not the round trips.
const BATCH: usize = 25;

/// How many Message-IDs one FETCH asks for. A line each, so the batch
/// bounds the round trips.
const ID_BATCH: usize = 1000;

impl Remote {
    /// Connect and log in as the account says.
    ///
    /// # Errors
    ///
    /// A server that does not answer, TLS that will not set up, a
    /// greeting that never comes, or a login the server refuses.
    pub fn connect(name: &str, account: &Imap, password: &str) -> Result<Self> {
        let stream: Box<dyn Stream> = if account.tls {
            Box::new(tls(name, account)?)
        } else {
            Box::new(reached(name, account)?)
        };
        let mut client = imap::Client::new(stream);
        client
            .read_greeting()
            .with_context(|| format!("{name}: the server sent no IMAP greeting"))?;
        let session = client
            .login(&account.user, password)
            .map_err(|(error, _)| error)
            .with_context(|| {
                format!(
                    "{name}: {} rejected the login for {}; check the password, or use an \
                     app password if the server requires one",
                    account.host, account.user
                )
            })?;
        Ok(Self { session })
    }

    /// Every folder the server offers that can be opened, in the
    /// server's order, as a person reads them. A name the server marks
    /// `\Noselect` — `[Gmail]` itself, a bare branch of the hierarchy —
    /// holds no messages and is left out.
    ///
    /// # Errors
    ///
    /// A list the server refuses, or a name that will not decode.
    pub fn folders(&mut self) -> Result<Vec<String>> {
        let names = self
            .session
            .list(Some(""), Some("*"))
            .context("listing the folders failed")?;
        names
            .iter()
            .filter(|name| !name.attributes().contains(&NameAttribute::NoSelect))
            .map(|name| utf7::decode(name.name()))
            .collect::<Result<Vec<_>>>()
            .context("the server listed a folder name that cannot be decoded")
    }

    pub fn logout(mut self) {
        // A goodbye the server never hears costs nothing — the
        // connection drops either way, and the messages are on disk.
        let _ = self.session.logout();
    }
}

/// The value of the first Message-ID field in a header section,
/// unfolded; `""` when there is none.
pub(crate) fn message_id(header: &[u8]) -> String {
    let text = String::from_utf8_lossy(header);
    let mut lines = text.split('\n').map(|line| line.trim_end_matches('\r'));
    let Some(first) = lines.by_ref().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("message-id")
            .then_some(value)
    }) else {
        return String::new();
    };
    let mut value = first.trim().to_string();
    for line in lines.take_while(|line| line.starts_with([' ', '\t'])) {
        value.push(' ');
        value.push_str(line.trim());
    }
    value.trim().to_string()
}

/// TLS to the account's server, the certificate held against the
/// public roots and the host's name.
fn tls(name: &str, account: &Imap) -> Result<StreamOwned<ClientConnection, TcpStream>> {
    let roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server = ServerName::try_from(account.host.clone())
        .with_context(|| format!("{name}: {:?} is not a valid host name", account.host))?;
    let connection = ClientConnection::new(Arc::new(config), server)
        .with_context(|| format!("{name}: TLS setup failed"))?;
    Ok(StreamOwned::new(connection, reached(name, account)?))
}

/// The most a server may keep quiet, once connected, before the run
/// gives it up: a load balancer that accepts and says nothing, a
/// network gone without a word, would otherwise hold the run for good.
const SILENT_AT_MOST: Duration = Duration::from_secs(300);

/// The account's server, connected, and bound to answer: a read or a
/// write that gets nothing within [`SILENT_AT_MOST`] fails, and the
/// folder is named in the tally instead of the run standing still.
fn reached(name: &str, account: &Imap) -> Result<TcpStream> {
    let tcp = TcpStream::connect((account.host.as_str(), account.port)).with_context(|| {
        format!(
            "{name}: cannot connect to {}:{}",
            account.host, account.port
        )
    })?;
    tcp.set_read_timeout(Some(SILENT_AT_MOST))
        .and_then(|()| tcp.set_write_timeout(Some(SILENT_AT_MOST)))
        .with_context(|| format!("{name}: setting the connection timeout failed"))?;
    Ok(tcp)
}

impl Mailbox for Remote {
    fn examine(&mut self, folder: &str) -> Result<Folder> {
        let wire = utf7::encode(folder);
        let mailbox = self
            .session
            .examine(&wire)
            .with_context(|| format!("{folder} could not be opened"))?;
        let Some(uidvalidity) = mailbox.uid_validity else {
            bail!("{folder}: the server sent no UIDVALIDITY");
        };
        Ok(Folder { uidvalidity })
    }

    fn uids_above(&mut self, above: u32) -> Result<Vec<u32>> {
        // "UID n:*" always includes the newest message even when its UID
        // is below n, so the filter is not decoration.
        let query = format!("UID {}:*", above.saturating_add(1));
        let found = self
            .session
            .uid_search(&query)
            .context("the UID search failed")?;
        let mut uids: Vec<u32> = found.into_iter().filter(|&uid| uid > above).collect();
        uids.sort_unstable();
        Ok(uids)
    }

    // The header field rather than ENVELOPE: the envelope brings every
    // address along, and the parser fails a whole batch on one it cannot
    // read.
    fn message_ids(&mut self, uids: &[u32]) -> Result<Vec<(u32, String)>> {
        let mut found = Vec::with_capacity(uids.len());
        for batch in uids.chunks(ID_BATCH) {
            let set = batch
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let fetches = self
                .session
                .uid_fetch(&set, "(UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])")
                .context("fetching the Message-IDs failed")?;
            let mut asked: HashSet<u32> = batch.iter().copied().collect();
            for fetch in &*fetches {
                // `remove`: a UID answered twice is counted once.
                let Some(uid) = fetch.uid.filter(|uid| asked.remove(uid)) else {
                    continue;
                };
                found.push((uid, message_id(fetch.header().unwrap_or_default())));
            }
        }
        Ok(found)
    }

    /// A batch the server refuses is asked for again one UID at a time.
    /// One message the server cannot hand over fails the whole FETCH,
    /// and would otherwise take the rest of its batch with it on every
    /// run.
    fn fetch(&mut self, uids: &[u32], each: &mut Each<'_>) -> Result<ControlFlow<()>, Stop> {
        for batch in uids.chunks(BATCH) {
            let flow = match self.ask(batch, each)? {
                Answer::Went(flow) => flow,
                Answer::Refused(error) if batch.len() == 1 => {
                    let error = error.context(format!("UID {}", batch[0]));
                    each(batch[0], Got::Refused(error)).map_err(Stop::Caller)?
                }
                Answer::Refused(_) => self.one_by_one(batch, each)?,
            };
            if flow.is_break() {
                return Ok(flow);
            }
        }
        Ok(ControlFlow::Continue(()))
    }
}

impl Remote {
    /// One FETCH for `batch`. PEEK keeps it from marking anything read
    /// even where EXAMINE would not. A NO or BAD refuses this command
    /// only; any other error means the connection is of no further use.
    fn ask(&mut self, batch: &[u32], each: &mut Each<'_>) -> Result<Answer, Stop> {
        let set = batch
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let fetches = match self.session.uid_fetch(&set, "(UID BODY.PEEK[])") {
            Ok(fetches) => fetches,
            Err(error @ (imap::Error::No(_) | imap::Error::Bad(_))) => {
                return Ok(Answer::Refused(
                    anyhow::Error::new(error).context("the server refused the fetch"),
                ));
            }
            Err(error) => {
                return Err(Stop::Server(
                    anyhow::Error::new(error).context("fetching the messages failed"),
                ));
            }
        };
        let mut answered = HashSet::new();
        for fetch in &*fetches {
            // A server may add FETCH responses nobody asked for, such
            // as flag changes of other messages.
            let Some(uid) = fetch.uid.filter(|uid| batch.contains(uid)) else {
                continue;
            };
            if !answered.insert(uid) {
                continue;
            }
            let got = match fetch.body() {
                Some(body) => Got::Message(body),
                None => Got::Refused(anyhow!("UID {uid}: the server sent no message body")),
            };
            if each(uid, got).map_err(Stop::Caller)?.is_break() {
                return Ok(Answer::Went(ControlFlow::Break(())));
            }
        }
        let missing: Vec<u32> = batch
            .iter()
            .copied()
            .filter(|uid| !answered.contains(uid))
            .collect();
        let held = self.still_held(&missing)?;
        for uid in missing {
            let got = if held.contains(&uid) {
                Got::Refused(anyhow!(
                    "UID {uid}: the server sent nothing for a message the folder holds"
                ))
            } else {
                Got::Gone
            };
            if each(uid, got).map_err(Stop::Caller)?.is_break() {
                return Ok(Answer::Went(ControlFlow::Break(())));
            }
        }
        Ok(Answer::Went(ControlFlow::Continue(())))
    }

    /// Which of `uids` the folder still holds. A UID left out of a FETCH
    /// answer is taken for gone only when the folder no longer holds it:
    /// gone means it comes off the pending list for good. When the
    /// server refuses the search, every one of them counts as held.
    fn still_held(&mut self, uids: &[u32]) -> Result<HashSet<u32>, Stop> {
        if uids.is_empty() {
            return Ok(HashSet::new());
        }
        let set = uids
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        match self.session.uid_search(format!("UID {set}")) {
            Ok(found) => Ok(found),
            Err(imap::Error::No(_) | imap::Error::Bad(_)) => Ok(uids.iter().copied().collect()),
            Err(error) => Err(Stop::Server(
                anyhow::Error::new(error).context("the UID search failed"),
            )),
        }
    }

    /// The UIDs of a refused batch, one FETCH each.
    fn one_by_one(&mut self, batch: &[u32], each: &mut Each<'_>) -> Result<ControlFlow<()>, Stop> {
        for &uid in batch {
            let flow = match self.ask(&[uid], each)? {
                Answer::Went(flow) => flow,
                Answer::Refused(error) => {
                    let error = error.context(format!("UID {uid}"));
                    each(uid, Got::Refused(error)).map_err(Stop::Caller)?
                }
            };
            if flow.is_break() {
                return Ok(flow);
            }
        }
        Ok(ControlFlow::Continue(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_id_is_read_from_its_header_field() {
        assert_eq!(
            message_id(b"Message-ID: <a@example.org>\r\n\r\n"),
            "<a@example.org>"
        );
        assert_eq!(
            message_id(b"message-id:\r\n <a@example.org>\r\n\r\n"),
            "<a@example.org>",
            "folded"
        );
        assert_eq!(
            message_id(b"Message-Id: <a@example.org>\r\nMessage-Id: <b@example.org>\r\n\r\n"),
            "<a@example.org>",
            "the first of two"
        );
        assert_eq!(message_id(b"\r\n"), "", "none");
    }
}
