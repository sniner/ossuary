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
use imap_proto::NameAttribute;
use rustls::pki_types::ServerName;
use rustls::{ClientConnection, RootCertStore, StreamOwned};

use crate::config::Imap;
use crate::{place, utf7};

/// A folder opened: what the server promised about its UIDs, and how
/// far the labels go that it lists for a message of the folder.
pub struct Folder {
    pub uidvalidity: u32,
    pub labels: Labels,
}

/// How far the labels go that a server lists for a message of one
/// folder. A fetch retracts a tag the list no longer has, so it has to
/// know what the list leaves out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Labels {
    /// All of the message's labels. Also from a server without labels.
    Whole,
    /// All but this one, the label the folder itself stands for.
    Without(String),
    /// All but the label the folder itself stands for, and the folder
    /// list does not tell which one that is.
    WithoutOne,
}

/// What became of one UID a fetch asked for.
pub enum Got<'a> {
    /// The message, whole, and the labels the server lists for it
    /// beside the folder being read: `None` from a server without
    /// labels, and from Gmail when the answer leaves them out.
    Message(&'a [u8], Option<Vec<String>>),
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
    /// Every folder the server offers that can be opened, in the
    /// server's order, as a person reads them. A name the server marks
    /// `\Noselect` — `[Gmail]` itself, a bare branch of the hierarchy —
    /// holds no messages and is left out.
    ///
    /// # Errors
    ///
    /// A list the server refuses, or a name that will not decode.
    fn folders(&mut self) -> Result<Vec<String>>;

    /// Opens a folder read-only.
    ///
    /// # Errors
    ///
    /// A folder that will not open, or one whose UIDVALIDITY the server
    /// does not state.
    fn examine(&mut self, folder: &str) -> Result<Folder>;

    /// Which kind of server it is, as far as labels go: on Gmail
    /// [`fetch`](Self::fetch) hands each message over with its labels.
    fn flavour(&self) -> Flavour;

    /// The UIDs above `above`, ascending — every UID of the folder for
    /// `0`. A folder must be examined first.
    ///
    /// # Errors
    ///
    /// A search the server refuses.
    fn uids_above(&mut self, above: u32) -> Result<Vec<u32>>;

    /// The first header field `name` of each of `uids`, as the message
    /// spells it, unfolded; `""` for a message without one. A UID the
    /// server leaves out of its answer is left out here.
    ///
    /// # Errors
    ///
    /// A fetch the server refuses.
    fn header(&mut self, uids: &[u32], name: &str) -> Result<Vec<(u32, String)>>;

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

/// How a server shows labels, beyond what IMAP itself knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavour {
    /// Gmail, which says so with `X-GM-EXT-1`: each message comes with
    /// its labels as `X-GM-LABELS`, less the label of the folder read.
    Gmail,
    /// Proton Mail Bridge, which names itself in its `ID` answer: each
    /// label is a folder under `Labels/`.
    Proton,
    /// Any other server: no labels.
    Standard,
}

pub struct Remote {
    session: Session<Box<dyn Stream>>,
    flavour: Flavour,
    /// How many messages the folder examined last holds.
    exists: u32,
}

/// The capability Gmail announces its IMAP extensions with.
const GMAIL: &str = "X-GM-EXT-1";

/// How the name in Proton Mail Bridge's `ID` answer begins.
const PROTON: &str = "Proton Mail";

/// How many messages one FETCH asks for. Messages come whole — a mail
/// is bounded in size, a server sees to that — and stay in memory until
/// stored, so the batch bounds the memory, not the round trips.
const BATCH: usize = 25;

/// How many header fields one FETCH asks for. A line each, so the
/// batch bounds the round trips.
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
        let mut session = client
            .login(&account.user, password)
            .map_err(|(error, _)| error)
            .with_context(|| {
                format!(
                    "{name}: {} rejected the login for {}; check the password, or use an \
                     app password if the server requires one",
                    account.host, account.user
                )
            })?;
        let flavour = flavour(&mut session).with_context(|| name.to_string())?;
        Ok(Self {
            session,
            flavour,
            exists: 0,
        })
    }

    pub fn logout(mut self) {
        // A goodbye the server never hears costs nothing — the
        // connection drops either way, and the messages are on disk.
        let _ = self.session.logout();
    }
}

/// Which kind of server the session talks to. Gmail says so in its
/// capabilities; Proton Mail Bridge only in its `ID` answer, which is
/// asked for only where the server lists `ID`.
fn flavour(session: &mut Session<Box<dyn Stream>>) -> Result<Flavour> {
    let capabilities = session
        .capabilities()
        .context("the server did not list its capabilities")?;
    if capabilities.has_str(GMAIL) {
        return Ok(Flavour::Gmail);
    }
    if !capabilities.has_str("ID") {
        return Ok(Flavour::Standard);
    }
    let answer = match session.run_command_and_read_response("ID NIL") {
        Ok(answer) => answer,
        // Listed and then refused: nothing to learn from this server.
        Err(imap::Error::No(_) | imap::Error::Bad(_)) => return Ok(Flavour::Standard),
        Err(error) => return Err(error).context("the server did not answer ID"),
    };
    Ok(match server_name(&answer) {
        Some(name) if name.starts_with(PROTON) => Flavour::Proton,
        _ => Flavour::Standard,
    })
}

/// The name a server gives in its `ID` answer, if it gives one.
fn server_name(answer: &[u8]) -> Option<String> {
    let mut rest = answer;
    while !rest.is_empty() {
        let (left, response) = imap_proto::Response::from_bytes(rest).ok()?;
        if let imap_proto::Response::Id(Some(fields)) = response {
            return fields.get("name").map(ToString::to_string);
        }
        rest = left;
    }
    None
}

/// The label Gmail gives a message in the inbox.
const GMAIL_INBOX: &str = "\\Inbox";

/// How far Gmail's labels go for a message of a folder with these
/// attributes in the folder list, other than `INBOX`.
///
/// All Mail (`\All`) is no label, so nothing is left out. A folder
/// without a special use is a label of the user's, under the folder's
/// name. A folder with any other special use, such as Sent or Starred,
/// stands for one of Gmail's own labels, and the folder list names the
/// use (`\Flagged`), not the label (`\Starred`).
fn labels_of(attributes: &[NameAttribute<'_>], folder: &str) -> Labels {
    if attributes.contains(&NameAttribute::All) {
        return Labels::Whole;
    }
    let plain = attributes.iter().all(|attribute| match attribute {
        NameAttribute::NoInferiors
        | NameAttribute::NoSelect
        | NameAttribute::Marked
        | NameAttribute::Unmarked => true,
        NameAttribute::Extension(name) => {
            name.eq_ignore_ascii_case("\\HasChildren")
                || name.eq_ignore_ascii_case("\\HasNoChildren")
        }
        _ => false,
    });
    if plain {
        Labels::Without(folder.to_string())
    } else {
        Labels::WithoutOne
    }
}

/// A label as the tag has it (see [`place::gmail_label`]). Gmail names
/// labels in the same encoding as folders; a name that does not decode
/// is kept as the server sent it, since the message matters more than
/// the spelling of one of its labels, and the server sends the same
/// spelling again.
fn label(wire: &str) -> String {
    let text = unescaped(wire);
    place::gmail_label(&utf7::decode(&text).unwrap_or(text))
}

/// A quoted string with its escapes undone. imap-proto hands a quoted
/// label over as it stood between the quotes, so Gmail's `"\\Inbox"`
/// arrives as `\\Inbox`. A label sent as an atom, `\Inbox`, holds no
/// escape: a backslash before anything but `\` or `"` stays.
fn unescaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(&next) = chars.peek()
            && matches!(next, '\\' | '"')
        {
            out.push(next);
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

/// The value of the first field `name` in a header section, unfolded;
/// `""` when there is none. The section ends at the first empty line,
/// so a whole message may be handed in: a field in a part's own header
/// is not the message's.
pub(crate) fn header_value(header: &[u8], name: &str) -> String {
    let text = String::from_utf8_lossy(header_section(header));
    let mut lines = text
        .split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .take_while(|line| !line.is_empty());
    let Some(first) = lines.by_ref().find_map(|line| {
        let (field, value) = line.split_once(':')?;
        field.trim().eq_ignore_ascii_case(name).then_some(value)
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

/// The bytes up to the empty line that ends the header section, or all
/// of them when there is none. Only this part is decoded.
fn header_section(message: &[u8]) -> &[u8] {
    let ends = [&b"\r\n\r\n"[..], &b"\n\n"[..]];
    let end = ends
        .iter()
        .filter_map(|end| {
            message
                .windows(end.len())
                .position(|window| window == *end)
                .map(|at| at + end.len())
        })
        .min();
    &message[..end.unwrap_or(message.len())]
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
    fn folders(&mut self) -> Result<Vec<String>> {
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

    fn examine(&mut self, folder: &str) -> Result<Folder> {
        let wire = utf7::encode(folder);
        let mailbox = self
            .session
            .examine(&wire)
            .with_context(|| format!("{folder} could not be opened"))?;
        let Some(uidvalidity) = mailbox.uid_validity else {
            bail!("{folder}: the server sent no UIDVALIDITY");
        };
        self.exists = mailbox.exists;
        let labels = if self.flavour == Flavour::Gmail {
            self.listed_labels(folder, &wire)?
        } else {
            Labels::Whole
        };
        Ok(Folder {
            uidvalidity,
            labels,
        })
    }

    fn flavour(&self) -> Flavour {
        self.flavour
    }

    fn uids_above(&mut self, above: u32) -> Result<Vec<u32>> {
        // Proton Mail Bridge answers "UID n:*" on an empty folder with
        // NO, where other servers find nothing.
        if self.exists == 0 {
            return Ok(Vec::new());
        }
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
    fn header(&mut self, uids: &[u32], name: &str) -> Result<Vec<(u32, String)>> {
        let mut found = Vec::with_capacity(uids.len());
        for batch in uids.chunks(ID_BATCH) {
            let set = batch
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let fetches = self
                .session
                .uid_fetch(&set, format!("(UID BODY.PEEK[HEADER.FIELDS ({name})])"))
                .with_context(|| format!("fetching the {name} fields failed"))?;
            let mut asked: HashSet<u32> = batch.iter().copied().collect();
            for fetch in fetches.iter() {
                // `remove`: a UID answered twice is counted once.
                let Some(uid) = fetch.uid.filter(|uid| asked.remove(uid)) else {
                    continue;
                };
                found.push((uid, header_value(fetch.header().unwrap_or_default(), name)));
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
    /// How far Gmail's labels go for a message of `folder`: Gmail lists
    /// them without the label of the folder being read.
    fn listed_labels(&mut self, folder: &str, wire: &str) -> Result<Labels> {
        if folder.eq_ignore_ascii_case("INBOX") {
            return Ok(Labels::Without(place::gmail_label(GMAIL_INBOX)));
        }
        // `list` sends the pattern as it is given.
        let quoted = format!("\"{}\"", wire.replace('\\', "\\\\").replace('"', "\\\""));
        let names = match self.session.list(Some(""), Some(&quoted)) {
            Ok(names) => names,
            Err(imap::Error::No(_) | imap::Error::Bad(_)) => return Ok(Labels::WithoutOne),
            Err(error) => {
                return Err(error).with_context(|| format!("{folder}: the server did not list it"));
            }
        };
        Ok(names
            .iter()
            .find(|name| name.name() == wire)
            .map_or(Labels::WithoutOne, |name| {
                labels_of(name.attributes(), folder)
            }))
    }

    /// One FETCH for `batch`. PEEK keeps it from marking anything read
    /// even where EXAMINE would not. A NO or BAD refuses this command
    /// only; any other error means the connection is of no further use.
    /// From Gmail the labels come in the same FETCH: asked of any other
    /// server, the unknown item would fail the whole command.
    fn ask(&mut self, batch: &[u32], each: &mut Each<'_>) -> Result<Answer, Stop> {
        let set = batch
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let items = if self.flavour == Flavour::Gmail {
            "(UID BODY.PEEK[] X-GM-LABELS)"
        } else {
            "(UID BODY.PEEK[])"
        };
        let fetches = match self.session.uid_fetch(&set, items) {
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
        for fetch in fetches.iter() {
            // A server may add FETCH responses nobody asked for, such
            // as flag changes of other messages.
            let Some(uid) = fetch.uid.filter(|uid| batch.contains(uid)) else {
                continue;
            };
            if !answered.insert(uid) {
                continue;
            }
            let got = match fetch.body() {
                Some(body) => Got::Message(
                    body,
                    if self.flavour == Flavour::Gmail {
                        fetch
                            .gmail_labels()
                            .map(|labels| labels.map(label).collect())
                    } else {
                        None
                    },
                ),
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
pub mod stub {
    //! An IMAP server answering from a script on loopback, so that
    //! [`Remote`](super::Remote) is tried whole: commands as sent,
    //! answers as read.

    use std::collections::{HashMap, VecDeque};
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;

    /// What the stub answers to one command.
    #[derive(Clone)]
    pub struct Reply {
        /// The untagged responses, each with its line end.
        untagged: Vec<u8>,
        /// How the command ends: `OK`, `NO` or `BAD`.
        status: &'static str,
        /// Close the connection instead of answering.
        hang_up: bool,
    }

    impl Reply {
        /// These untagged responses, then `OK`.
        pub fn ok(untagged: &[u8]) -> Self {
            Self {
                untagged: untagged.to_vec(),
                status: "OK",
                hang_up: false,
            }
        }

        /// `NO`: the server refuses the command.
        pub fn no() -> Self {
            Self {
                untagged: Vec::new(),
                status: "NO",
                hang_up: false,
            }
        }

        /// No answer: the connection is closed.
        pub fn hang_up() -> Self {
            Self {
                untagged: Vec::new(),
                status: "OK",
                hang_up: true,
            }
        }
    }

    type Script = HashMap<String, VecDeque<Reply>>;

    pub struct Stub {
        port: u16,
        script: Arc<Mutex<Script>>,
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl Stub {
        pub fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let script: Arc<Mutex<Script>> = Arc::default();
            let seen: Arc<Mutex<Vec<String>>> = Arc::default();
            let (script_, seen_) = (Arc::clone(&script), Arc::clone(&seen));
            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let (script, seen) = (Arc::clone(&script_), Arc::clone(&seen_));
                    thread::spawn(move || serve(stream, &script, &seen));
                }
            });
            Self { port, script, seen }
        }

        pub fn port(&self) -> u16 {
            self.port
        }

        /// Answer `command`, as the client sends it without its tag,
        /// with these replies in turn; the last one stands for every
        /// time after.
        pub fn script(&self, command: &str, replies: Vec<Reply>) {
            self.script
                .lock()
                .unwrap()
                .insert(command.to_string(), replies.into());
        }

        /// The commands received so far, without their tags.
        pub fn seen(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }
    }

    /// One message as a FETCH response hands it over.
    pub fn message(sequence: u32, uid: u32, body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "* {sequence} FETCH (UID {uid} BODY[] {{{}}}\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out.extend_from_slice(b")\r\n");
        out
    }

    /// What a command gets that the test did not script: a login is
    /// accepted, the capabilities are those of a plain server, and
    /// anything else is an error of the test.
    fn unscripted(command: &str) -> Reply {
        if command.starts_with("LOGIN ") {
            Reply::ok(b"")
        } else if command == "CAPABILITY" {
            Reply::ok(b"* CAPABILITY IMAP4rev1\r\n")
        } else if command == "LOGOUT" {
            Reply::ok(b"* BYE\r\n")
        } else {
            Reply {
                status: "BAD",
                ..Reply::ok(b"")
            }
        }
    }

    fn serve(mut stream: TcpStream, script: &Mutex<Script>, seen: &Mutex<Vec<String>>) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        stream.write_all(b"* OK ready\r\n").unwrap();
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let Some((tag, command)) = line.trim_end().split_once(' ') else {
                return;
            };
            seen.lock().unwrap().push(command.to_string());
            let reply = {
                let mut script = script.lock().unwrap();
                match script.get_mut(command) {
                    Some(replies) if replies.len() > 1 => replies.pop_front(),
                    Some(replies) => replies.front().cloned(),
                    None => None,
                }
            }
            .unwrap_or_else(|| unscripted(command));
            if reply.hang_up {
                return;
            }
            let mut out = reply.untagged;
            out.extend_from_slice(format!("{tag} {} done\r\n", reply.status).as_bytes());
            if stream.write_all(&out).is_err() || command == "LOGOUT" {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_field_is_read_from_the_header_section() {
        let message_id = |header: &[u8]| header_value(header, "Message-ID");
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
        assert_eq!(
            message_id(b"Subject: x\r\n\r\n--b\r\nMessage-ID: <part@example.org>\r\n"),
            "",
            "a field below the header section is not the message's"
        );
        assert_eq!(
            header_value(
                b"X-Pm-Internal-Id: abc==\nSubject: x\n\nbody",
                "x-pm-internal-id"
            ),
            "abc==",
            "any field, bare line ends"
        );
    }

    #[test]
    fn the_server_names_itself_in_its_id_answer() {
        let proton = b"* ID (\"name\" \"Proton Mail Bridge\" \"version\" \"03.27.01\" \"vendor\" \"TODO\")\r\n";
        assert_eq!(server_name(proton).as_deref(), Some("Proton Mail Bridge"));
        assert_eq!(server_name(b"* ID NIL\r\n"), None, "no fields");
        assert_eq!(server_name(b"* ID (\"vendor\" \"x\")\r\n"), None, "no name");
        assert_eq!(server_name(b"garbage\r\n"), None);
    }

    #[test]
    fn the_folder_list_tells_how_far_gmails_labels_go() {
        let children = || NameAttribute::Extension("\\HasNoChildren".into());
        assert_eq!(
            labels_of(&[NameAttribute::All, children()], "[Gmail]/All Mail"),
            Labels::Whole
        );
        assert_eq!(
            labels_of(&[children()], "Work/Invoices"),
            Labels::Without("Work/Invoices".to_string())
        );
        assert_eq!(labels_of(&[], "Work"), Labels::Without("Work".to_string()));
        assert_eq!(
            labels_of(&[children(), NameAttribute::Sent], "[Gmail]/Sent Mail"),
            Labels::WithoutOne
        );
        assert_eq!(
            labels_of(
                &[NameAttribute::Extension("\\Important".into())],
                "[Gmail]/Important"
            ),
            Labels::WithoutOne
        );
    }

    #[test]
    fn a_label_is_decoded_like_a_folder_name() {
        assert_eq!(label("Pers&APY-nlich"), "Pers\u{f6}nlich");
        assert_eq!(label("\\Inbox"), "/Inbox", "Gmail's own, with a slash");
        assert_eq!(label("Q&A"), "Q&A", "kept as sent when it does not decode");
        assert_eq!(
            label("\\\\Important"),
            "/Important",
            "quoted: \"\\\\Important\""
        );
        assert_eq!(label("say \\\"hi\\\""), "say \"hi\"");
    }

    use stub::{Reply, Stub, message};

    fn account(stub: &Stub) -> Imap {
        Imap {
            host: "127.0.0.1".to_string(),
            port: stub.port(),
            tls: false,
            user: "john".to_string(),
            password: None,
        }
    }

    fn connected(stub: &Stub) -> Remote {
        Remote::connect("example.org", &account(stub), "s3cret").unwrap()
    }

    /// A stub whose capabilities are these, besides IMAP itself.
    fn serving(capabilities: &str) -> Stub {
        let stub = Stub::start();
        stub.script(
            "CAPABILITY",
            vec![Reply::ok(
                format!("* CAPABILITY IMAP4rev1 {capabilities}\r\n").as_bytes(),
            )],
        );
        stub
    }

    /// The folder opens with this many messages under UIDVALIDITY 7.
    fn holding(stub: &Stub, folder: &str, exists: u32) {
        stub.script(
            &format!("EXAMINE \"{folder}\""),
            vec![Reply::ok(
                format!("* {exists} EXISTS\r\n* OK [UIDVALIDITY 7] ok\r\n").as_bytes(),
            )],
        );
    }

    /// A connection with INBOX open, three messages in it.
    fn in_inbox(stub: &Stub) -> Remote {
        holding(stub, "INBOX", 3);
        let mut remote = connected(stub);
        remote.examine("INBOX").unwrap();
        remote
    }

    /// What a fetch of `uids` handed over, in order, and how it ended.
    fn fetched(remote: &mut Remote, uids: &[u32]) -> (Vec<(u32, String)>, String) {
        let mut got = Vec::new();
        let outcome = remote.fetch(uids, &mut |uid, what| {
            let what = match what {
                Got::Message(bytes, labels) => format!(
                    "message {} {labels:?}",
                    String::from_utf8_lossy(bytes).trim_end()
                ),
                Got::Refused(error) => format!("refused: {error:#}"),
                Got::Gone => "gone".to_string(),
            };
            got.push((uid, what));
            Ok(ControlFlow::Continue(()))
        });
        let ended = match outcome {
            Ok(_) => "done".to_string(),
            Err(Stop::Server(error)) => format!("server: {error:#}"),
            Err(Stop::Caller(error)) => format!("caller: {error:#}"),
        };
        (got, ended)
    }

    const WHOLE: &str = "(UID BODY.PEEK[])";

    #[test]
    fn a_login_is_followed_by_the_question_what_the_server_is() {
        let stub = Stub::start();
        let remote = connected(&stub);
        assert_eq!(remote.flavour(), Flavour::Standard);
        assert_eq!(stub.seen(), ["LOGIN \"john\" \"s3cret\"", "CAPABILITY"]);
    }

    #[test]
    fn a_login_the_server_refuses_is_named_with_the_way_out() {
        let stub = Stub::start();
        stub.script("LOGIN \"john\" \"s3cret\"", vec![Reply::no()]);
        let refused = Remote::connect("example.org", &account(&stub), "s3cret")
            .err()
            .unwrap();
        assert!(
            format!("{refused:#}").contains("rejected the login for john"),
            "{refused:#}"
        );
    }

    #[test]
    fn gmail_is_known_by_a_capability_and_proton_mail_bridge_by_its_id() {
        assert_eq!(
            connected(&serving("X-GM-EXT-1 ID")).flavour(),
            Flavour::Gmail
        );

        let bridge = serving("ID");
        bridge.script(
            "ID NIL",
            vec![Reply::ok(
                b"* ID (\"name\" \"Proton Mail Bridge\" \"version\" \"3.21.2\")\r\n",
            )],
        );
        assert_eq!(connected(&bridge).flavour(), Flavour::Proton);

        let other = serving("ID");
        other.script(
            "ID NIL",
            vec![Reply::ok(b"* ID (\"name\" \"Dovecot\")\r\n")],
        );
        assert_eq!(connected(&other).flavour(), Flavour::Standard);

        let silent = serving("ID");
        silent.script("ID NIL", vec![Reply::no()]);
        assert_eq!(connected(&silent).flavour(), Flavour::Standard);
    }

    #[test]
    fn folders_are_listed_decoded_and_without_those_that_hold_no_messages() {
        let stub = Stub::start();
        stub.script(
            "LIST \"\" *",
            vec![Reply::ok(
                b"* LIST (\\HasNoChildren) \"/\" \"INBOX\"\r\n\
                  * LIST (\\HasChildren \\Noselect) \"/\" \"[Gmail]\"\r\n\
                  * LIST (\\HasNoChildren) \"/\" \"Entw&APw-rfe\"\r\n",
            )],
        );
        let mut remote = connected(&stub);
        assert_eq!(remote.folders().unwrap(), ["INBOX", "Entw\u{fc}rfe"]);
    }

    #[test]
    fn a_folder_is_opened_read_only_under_its_name_on_the_wire() {
        let stub = Stub::start();
        holding(&stub, "Entw&APw-rfe", 3);
        let mut remote = connected(&stub);

        let folder = remote.examine("Entw\u{fc}rfe").unwrap();

        assert_eq!(folder.uidvalidity, 7);
        assert_eq!(folder.labels, Labels::Whole);
        assert_eq!(stub.seen().last().unwrap(), "EXAMINE \"Entw&APw-rfe\"");
    }

    #[test]
    fn a_folder_without_a_uidvalidity_is_not_opened() {
        let stub = Stub::start();
        stub.script("EXAMINE \"INBOX\"", vec![Reply::ok(b"* 3 EXISTS\r\n")]);
        let refused = connected(&stub).examine("INBOX").err().unwrap();
        assert!(
            format!("{refused:#}").contains("no UIDVALIDITY"),
            "{refused:#}"
        );
    }

    #[test]
    fn a_gmail_folder_says_how_far_its_labels_go() {
        let stub = serving("X-GM-EXT-1");
        let lists = |folder: &str, attributes: &str| {
            holding(&stub, folder, 3);
            stub.script(
                &format!("LIST \"\" \"{folder}\""),
                vec![Reply::ok(
                    format!("* LIST ({attributes}) \"/\" \"{folder}\"\r\n").as_bytes(),
                )],
            );
        };
        holding(&stub, "INBOX", 3);
        lists("[Gmail]/All Mail", "\\All \\HasNoChildren");
        lists("Work", "\\HasNoChildren");
        lists("[Gmail]/Sent Mail", "\\HasNoChildren \\Sent");
        holding(&stub, "Refused", 3);
        stub.script("LIST \"\" \"Refused\"", vec![Reply::no()]);
        let mut remote = connected(&stub);
        let mut labels = |folder: &str| remote.examine(folder).unwrap().labels;

        assert_eq!(labels("INBOX"), Labels::Without("/Inbox".to_string()));
        assert_eq!(labels("[Gmail]/All Mail"), Labels::Whole);
        assert_eq!(labels("Work"), Labels::Without("Work".to_string()));
        assert_eq!(labels("[Gmail]/Sent Mail"), Labels::WithoutOne);
        assert_eq!(labels("Refused"), Labels::WithoutOne);
        assert!(
            !stub.seen().contains(&"LIST \"\" \"INBOX\"".to_string()),
            "INBOX needs no look at the folder list"
        );
    }

    #[test]
    fn the_uids_above_a_point_are_asked_for_and_those_below_it_dropped() {
        let stub = Stub::start();
        // The range `5:*` always has the newest message in it.
        stub.script("UID SEARCH UID 5:*", vec![Reply::ok(b"* SEARCH 9 4 7\r\n")]);
        let mut remote = in_inbox(&stub);
        assert_eq!(remote.uids_above(4).unwrap(), [7, 9]);
    }

    #[test]
    fn an_empty_folder_is_not_searched() {
        let stub = Stub::start();
        holding(&stub, "INBOX", 0);
        let mut remote = connected(&stub);
        remote.examine("INBOX").unwrap();

        assert!(remote.uids_above(0).unwrap().is_empty());
        assert!(
            !stub
                .seen()
                .iter()
                .any(|line| line.starts_with("UID SEARCH"))
        );
    }

    #[test]
    fn a_batch_hands_every_message_over_and_passes_by_what_nobody_asked_for() {
        let stub = Stub::start();
        let mut answer = message(1, 3, b"three");
        answer.extend_from_slice(b"* 7 FETCH (UID 99 FLAGS (\\Seen))\r\n");
        answer.extend(message(2, 5, b"five"));
        stub.script(&format!("UID FETCH 3,5 {WHOLE}"), vec![Reply::ok(&answer)]);
        let mut remote = in_inbox(&stub);

        let (got, ended) = fetched(&mut remote, &[3, 5]);

        assert_eq!(
            got,
            [
                (3, "message three None".to_string()),
                (5, "message five None".to_string())
            ]
        );
        assert_eq!(ended, "done");
    }

    #[test]
    fn a_refused_batch_is_asked_for_again_one_message_at_a_time() {
        let stub = Stub::start();
        stub.script(&format!("UID FETCH 3,5,9 {WHOLE}"), vec![Reply::no()]);
        stub.script(
            &format!("UID FETCH 3 {WHOLE}"),
            vec![Reply::ok(&message(1, 3, b"three"))],
        );
        stub.script(&format!("UID FETCH 5 {WHOLE}"), vec![Reply::no()]);
        stub.script(
            &format!("UID FETCH 9 {WHOLE}"),
            vec![Reply::ok(&message(3, 9, b"nine"))],
        );
        let mut remote = in_inbox(&stub);

        let (got, ended) = fetched(&mut remote, &[3, 5, 9]);

        assert_eq!(got[0], (3, "message three None".to_string()));
        assert!(
            got[1]
                .1
                .starts_with("refused: UID 5: the server refused the fetch"),
            "{:?}",
            got[1]
        );
        assert_eq!(got[2], (9, "message nine None".to_string()));
        assert_eq!(ended, "done");
    }

    #[test]
    fn a_uid_the_answer_leaves_out_is_gone_only_when_the_folder_no_longer_holds_it() {
        let stub = Stub::start();
        stub.script(
            &format!("UID FETCH 3,5,9 {WHOLE}"),
            vec![Reply::ok(&message(1, 3, b"three"))],
        );
        stub.script("UID SEARCH UID 5,9", vec![Reply::ok(b"* SEARCH 9\r\n")]);
        let mut remote = in_inbox(&stub);

        let (got, _) = fetched(&mut remote, &[3, 5, 9]);

        assert_eq!(got[1], (5, "gone".to_string()));
        assert_eq!(
            got[2],
            (
                9,
                "refused: UID 9: the server sent nothing for a message the folder holds"
                    .to_string()
            )
        );
    }

    #[test]
    fn a_uid_left_out_counts_as_held_when_the_server_refuses_to_say() {
        let stub = Stub::start();
        stub.script(&format!("UID FETCH 5 {WHOLE}"), vec![Reply::ok(b"")]);
        stub.script("UID SEARCH UID 5", vec![Reply::no()]);
        let mut remote = in_inbox(&stub);

        let (got, _) = fetched(&mut remote, &[5]);

        assert!(got[0].1.starts_with("refused:"), "{:?}", got[0]);
    }

    #[test]
    fn an_answer_without_the_message_is_a_refusal() {
        let stub = Stub::start();
        stub.script(
            &format!("UID FETCH 3 {WHOLE}"),
            vec![Reply::ok(b"* 1 FETCH (UID 3 FLAGS (\\Seen))\r\n")],
        );
        let mut remote = in_inbox(&stub);

        let (got, ended) = fetched(&mut remote, &[3]);

        assert_eq!(
            got,
            [(
                3,
                "refused: UID 3: the server sent no message body".to_string()
            )]
        );
        assert_eq!(ended, "done");
    }

    #[test]
    fn a_connection_that_drops_ends_the_fetch_as_the_servers_trouble() {
        let stub = Stub::start();
        stub.script(&format!("UID FETCH 3,5 {WHOLE}"), vec![Reply::hang_up()]);
        let mut remote = in_inbox(&stub);

        let (got, ended) = fetched(&mut remote, &[3, 5]);

        assert!(got.is_empty());
        assert!(
            ended.starts_with("server: fetching the messages failed"),
            "{ended}"
        );
    }

    #[test]
    fn gmail_hands_a_message_over_with_its_labels() {
        let stub = serving("X-GM-EXT-1");
        stub.script(
            "UID FETCH 3 (UID BODY.PEEK[] X-GM-LABELS)",
            vec![Reply::ok(
                b"* 1 FETCH (UID 3 X-GM-LABELS (\"\\\\Inbox\" \"Pers&APY-nlich\" Work) \
                  BODY[] {5}\r\nthree)\r\n",
            )],
        );
        let mut remote = in_inbox(&stub);

        let (got, _) = fetched(&mut remote, &[3]);

        assert_eq!(
            got,
            [(
                3,
                "message three Some([\"/Inbox\", \"Pers\u{f6}nlich\", \"Work\"])".to_string()
            )]
        );
    }

    #[test]
    fn a_header_field_is_listed_by_uid_and_a_uid_left_out_is_left_out() {
        let stub = Stub::start();
        stub.script(
            "UID FETCH 3,5,9 (UID BODY.PEEK[HEADER.FIELDS (Message-ID)])",
            vec![Reply::ok(
                b"* 1 FETCH (UID 3 BODY[HEADER.FIELDS (Message-ID)] {31}\r\n\
                  Message-ID: <a@example.org>\r\n\r\n)\r\n\
                  * 2 FETCH (UID 5 BODY[HEADER.FIELDS (Message-ID)] {2}\r\n\r\n)\r\n",
            )],
        );
        let mut remote = in_inbox(&stub);

        let listed = remote.header(&[3, 5, 9], "Message-ID").unwrap();

        assert_eq!(
            listed,
            [(3, "<a@example.org>".to_string()), (5, String::new())]
        );
    }
}
