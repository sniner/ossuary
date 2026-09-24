//! The IMAP side. Folders are opened read-only — a fetcher that changes
//! `\Seen` flags on the server has overstepped its job. Folder names
//! cross this boundary in the server's own encoding ([`utf7`]) and are
//! plain text on every other side of it.
//!
//! What the fetch needs of a mailbox is the [`Mailbox`] trait; the
//! server answers it here, and a test answers it from memory.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
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

    /// Whole messages by UID, handed to `each` one at a time, in the
    /// order the server sends them — which need not be ascending.
    ///
    /// # Errors
    ///
    /// [`Stop::Server`] when the server will not go on, [`Stop::Caller`]
    /// with whatever `each` answered.
    fn fetch(
        &mut self,
        uids: &[u32],
        each: &mut dyn FnMut(u32, &[u8]) -> Result<()>,
    ) -> Result<(), Stop>;
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

    /// PEEK keeps the fetch from marking anything read even where
    /// EXAMINE would not.
    fn fetch(
        &mut self,
        uids: &[u32],
        each: &mut dyn FnMut(u32, &[u8]) -> Result<()>,
    ) -> Result<(), Stop> {
        for batch in uids.chunks(BATCH) {
            let set = batch
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let fetches = self
                .session
                .uid_fetch(&set, "(UID BODY.PEEK[])")
                .context("fetching the messages failed")
                .map_err(Stop::Server)?;
            for fetch in &*fetches {
                let (Some(uid), Some(body)) = (fetch.uid, fetch.body()) else {
                    continue;
                };
                each(uid, body).map_err(Stop::Caller)?;
            }
        }
        Ok(())
    }
}
