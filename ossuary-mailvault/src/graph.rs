//! The MS Graph side: a Microsoft 365 mailbox read over Microsoft's own
//! interface rather than its IMAP. Written along mailvault's
//! `backend/graph.py`, which has fetched real mailboxes for years.
//!
//! The login is an app registration — tenant, client, secret — and one
//! form POST turns it into a bearer token; the mailbox itself is named
//! by its address. Everything after that is plain JSON over HTTPS:
//! the folder tree once, then per folder a **delta round**, a chain of
//! pages that ends on a link meaning "you are caught up here". The
//! next run starts from that link and is handed only what changed —
//! a deleted message included, as an entry marked removed. Messages
//! come as their RFC 822 bytes, one request each.
//!
//! The token goes with every request, so every request is held to the
//! one host mail is asked of: a next-page link comes out of a response,
//! a delta link out of `cache/`, and a link naming another host would
//! hand the token over. Throttling and gateway hiccups are what Graph
//! does under load, and are retried with a growing pause; a token that
//! expired mid-run is fetched anew once.

use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use ureq::Agent;

use crate::config;
use crate::output::Say;

/// Where mail is asked of.
pub const GRAPH: &str = "https://graph.microsoft.com/v1.0";

/// Where a tenant issues tokens.
pub const LOGIN: &str = "https://login.microsoftonline.com";

/// What the token is asked for: every permission the application was
/// granted on Graph.
const SCOPE: &str = "https://graph.microsoft.com/.default";

/// How many ids one page of a delta round carries. Only ids come back,
/// so the round trips are what costs — a folder of 77,000 messages is
/// 155 requests at this size. The service caps it where it will not
/// honour it.
const PAGE: usize = 500;

/// How often a request is asked again before its trouble is named.
const RETRIES: u32 = 5;

/// The pause before a retry doubles from here, up to [`PAUSE_MOST`].
const PAUSE: Duration = Duration::from_secs(2);
const PAUSE_MOST: Duration = Duration::from_secs(60);

/// Throttling and gateway trouble: what Graph says under load, and
/// what goes away by asking again.
const RETRY_STATUS: [u16; 6] = [408, 429, 500, 502, 503, 504];

/// A delta link the server no longer honours, said two ways: `410 Gone`
/// is the documented reset, and an expired token arrives as a 4xx with
/// one of these codes. Both mean the same here, and both are ordinary.
const EXPIRED_STATUS: u16 = 410;
const EXPIRED_CODES: [&str; 4] = [
    "syncstatenotfound",
    "resyncrequired",
    "resyncchangesapplydifferences",
    "resyncchangesuploaddifferences",
];

/// What a 404 for a message carries when the message itself is gone:
/// deleted, or moved to another folder since it was listed.
const GONE_CODE: &str = "erroritemnotfound";

/// The most a message may be — Graph itself stops well below.
const MESSAGE_AT_MOST: u64 = 512 * 1024 * 1024;

/// The most the server may keep quiet at each step before the request
/// is given up and asked again.
const CONNECT_AT_MOST: Duration = Duration::from_secs(60);
const ANSWER_AT_MOST: Duration = Duration::from_secs(300);
const BODY_AT_MOST: Duration = Duration::from_secs(900);

/// One message a round offers: Graph's id to fetch it by, and the
/// marks the mailbox has on it — Outlook's categories, by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offered {
    pub id: String,
    pub tags: Vec<String>,
}

/// A delta round: what the folder offers, and where to carry on.
#[derive(Debug, Default)]
pub struct Round {
    /// The messages — new or changed since the link the round started
    /// from, or every one of the folder. A category put on or taken
    /// off a message is a change too, and brings the message round
    /// again with its marks as they now stand.
    pub offered: Vec<Offered>,
    /// Entries the server marked removed: deleted, or moved out of the
    /// folder. Nothing to fetch, but worth a word.
    pub gone: usize,
    /// The link for next time, when the round ended on one.
    pub link: Option<String>,
}

/// Why a round stopped short.
pub enum Halt {
    /// The server no longer honours the link the round started from.
    Expired,
    /// The server would not go on.
    Failed(anyhow::Error),
}

/// Why a message was not handed over.
#[derive(Debug)]
pub enum Miss {
    /// The server has no message under this id any more: deleted, or
    /// moved to another folder since it was listed.
    Gone,
    /// The server refused this one message. Others may still come.
    Refused(anyhow::Error),
    /// No usable answer from the server: no complete response after
    /// the retries, or a token that could not be renewed. The next
    /// message will not come either.
    Unreachable(anyhow::Error),
}

/// A mailbox reached: logged in, its folders known.
pub struct Graph<'a> {
    agent: Agent,
    name: &'a str,
    account: &'a config::Graph,
    secret: String,
    /// Where tokens come from and where mail is asked of: the real
    /// services, or a test's.
    login: String,
    base: String,
    token: String,
    /// The folders in the server's order, each path with its id.
    folders: Vec<(String, String)>,
    say: Say,
}

/// One page of any listing.
#[derive(Deserialize)]
struct Page<T> {
    #[serde(default = "Vec::new")]
    value: Vec<T>,
    #[serde(rename = "@odata.nextLink")]
    next: Option<String>,
    #[serde(rename = "@odata.deltaLink")]
    delta: Option<String>,
}

#[derive(Deserialize)]
struct FolderItem {
    id: String,
    #[serde(rename = "displayName", default)]
    display_name: String,
    #[serde(rename = "childFolderCount", default)]
    child_folder_count: u64,
}

#[derive(Deserialize)]
struct DeltaItem {
    id: String,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(rename = "@removed")]
    removed: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Token {
    #[serde(rename = "access_token")]
    access: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Deserialize)]
struct Failure {
    error: Option<FailureBody>,
}

#[derive(Deserialize)]
struct FailureBody {
    code: Option<String>,
    message: Option<String>,
}

/// What the server answered, read whole.
struct Answer {
    status: u16,
    retry_after: Option<String>,
    body: Vec<u8>,
}

impl Answer {
    fn read(response: ureq::http::Response<ureq::Body>) -> Result<Self> {
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = response
            .into_body()
            .into_with_config()
            .limit(MESSAGE_AT_MOST)
            .read_to_vec()
            .context("reading the response failed")?;
        Ok(Self {
            status,
            retry_after,
            body,
        })
    }

    fn json<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.body).context("the response is not the expected JSON")
    }

    /// The trouble in the server's words: the status, and the code and
    /// message Graph puts in the body when it has one.
    fn trouble(&self) -> String {
        let said = self
            .json::<Failure>()
            .ok()
            .and_then(|failure| failure.error)
            .map(|error| {
                let code = error.code.unwrap_or_default();
                let message = error.message.unwrap_or_default();
                format!(" {code}: {message}").trim_end().to_string()
            })
            .unwrap_or_default();
        format!("HTTP {}{said}", self.status)
    }

    /// The error code Graph puts in the body, lower-cased, when there
    /// is one.
    fn code(&self) -> Option<String> {
        self.json::<Failure>()
            .ok()
            .and_then(|failure| failure.error)
            .and_then(|error| error.code)
            .map(|code| code.to_ascii_lowercase())
    }

    /// Whether the delta link this answers to is no longer honoured.
    fn expired(&self) -> bool {
        if self.status == EXPIRED_STATUS {
            return true;
        }
        if !(400..500).contains(&self.status) {
            return false;
        }
        self.code()
            .is_some_and(|code| EXPIRED_CODES.contains(&code.as_str()))
    }

    /// Whether the message this answers for is no longer on the server.
    /// A 404 alone is not enough: the server answers 404 for a mailbox
    /// it cannot reach or an id it cannot parse as well, and a message
    /// taken off the pending list on such an answer would not be listed
    /// again.
    fn gone(&self) -> bool {
        self.status == 404 && self.code().as_deref() == Some(GONE_CODE)
    }
}

impl<'a> Graph<'a> {
    /// Reach the mailbox: a token from the tenant, the folder tree from
    /// the server.
    ///
    /// # Errors
    ///
    /// A tenant that issues no token, or a server that will not list
    /// the folders — the permission not granted, most often.
    pub fn connect(
        name: &'a str,
        account: &'a config::Graph,
        secret: String,
        say: Say,
    ) -> Result<Self> {
        Self::reach(name, account, secret, LOGIN, GRAPH, say)
    }

    /// [`connect`](Self::connect) against services standing elsewhere.
    ///
    /// # Errors
    ///
    /// As `connect`.
    pub fn reach(
        name: &'a str,
        account: &'a config::Graph,
        secret: String,
        login: &str,
        base: &str,
        say: Say,
    ) -> Result<Self> {
        let agent: Agent = Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(CONNECT_AT_MOST))
            .timeout_recv_response(Some(ANSWER_AT_MOST))
            .timeout_recv_body(Some(BODY_AT_MOST))
            .build()
            .into();
        let mut graph = Self {
            agent,
            name,
            account,
            secret,
            login: login.trim_end_matches('/').to_string(),
            base: base.trim_end_matches('/').to_string(),
            token: String::new(),
            folders: Vec::new(),
            say,
        };
        graph.token = graph.fetch_token()?;
        let root = format!(
            "{}/users/{}/mailFolders?$select=id,displayName,childFolderCount&$top=100",
            graph.base, account.user
        );
        graph.walk(root, "")?;
        Ok(graph)
    }

    /// A token for Graph, or why the tenant refused one.
    fn fetch_token(&self) -> Result<String> {
        let url = format!(
            "{}/{}/oauth2/v2.0/token",
            self.login, self.account.tenant_id
        );
        let response = self
            .agent
            .post(&url)
            .send_form([
                ("client_id", self.account.client_id.as_str()),
                ("client_secret", self.secret.as_str()),
                ("scope", SCOPE),
                ("grant_type", "client_credentials"),
            ])
            .with_context(|| format!("{}: no response from {}", self.name, host_of(&url)))?;
        let answer = Answer::read(response)?;
        let token: Token = answer.json().with_context(|| {
            format!(
                "{}: {} returned HTTP {}",
                self.name,
                host_of(&url),
                answer.status
            )
        })?;
        if let Some(token) = token.access {
            return Ok(token);
        }
        let reason = token
            .error_description
            .or(token.error)
            .unwrap_or_else(|| format!("HTTP {}", answer.status));
        bail!(
            "{}: token request failed: {reason}; check the client secret, the tenant id and the admin consent",
            self.name
        );
    }

    /// Every folder of the mailbox, as paths, in the server's order —
    /// a folder inside another reads `Inbox/Projects`.
    #[must_use]
    pub fn folders(&self) -> Vec<String> {
        self.folders.iter().map(|(path, _)| path.clone()).collect()
    }

    /// The folder's id: by its path as spelled, or as spelled in
    /// another case.
    #[must_use]
    pub fn resolve(&self, folder: &str) -> Option<&str> {
        self.folders
            .iter()
            .find(|(path, _)| path == folder)
            .or_else(|| {
                self.folders
                    .iter()
                    .find(|(path, _)| path.eq_ignore_ascii_case(folder))
            })
            .map(|(_, id)| id.as_str())
    }

    /// Whether a URL may be asked with this mailbox's token: the same
    /// scheme and host mail is asked of, nothing else.
    #[must_use]
    pub fn owns(&self, url: &str) -> bool {
        origin_of(url).is_some_and(|origin| {
            origin_of(&self.base).is_some_and(|own| origin.eq_ignore_ascii_case(own))
        })
    }

    /// The folders under `url`, and theirs, into the list.
    fn walk(&mut self, url: String, prefix: &str) -> Result<()> {
        let mut url = url;
        loop {
            let answer = self.get(&url, None)?;
            if answer.status != 200 {
                bail!("{}", self.refused("listing the folders", &answer));
            }
            let page: Page<FolderItem> = answer.json()?;
            for folder in page.value {
                let path = if prefix.is_empty() {
                    folder.display_name
                } else {
                    format!("{prefix}/{}", folder.display_name)
                };
                if folder.child_folder_count > 0 {
                    let below = format!(
                        "{}/users/{}/mailFolders/{}/childFolders?$select=id,displayName,childFolderCount&$top=100",
                        self.base, self.account.user, folder.id
                    );
                    self.walk(below, &path)?;
                }
                self.folders.push((path, folder.id));
            }
            match page.next {
                Some(next) => url = next,
                None => return Ok(()),
            }
        }
    }

    /// One delta round over the folder: from `from` on, or whole.
    ///
    /// The first request carries the query, and every one after it is
    /// a link the server handed back, followed verbatim: the server
    /// folds the query into its links. Entries marked removed are
    /// counted and dropped — a deleted message, or one moved out of
    /// the folder, is nothing to fetch. A read/unread flip arrives as
    /// a change too and is kept: fetching a message again costs a
    /// download the store then dedups, where dropping one would cost
    /// the message.
    ///
    /// # Errors
    ///
    /// [`Halt::Expired`] when the server no longer honours `from`,
    /// [`Halt::Failed`] when it will not go on.
    pub fn round(&mut self, folder_id: &str, from: Option<&str>) -> Result<Round, Halt> {
        let mut url = match from {
            Some(link) => link.to_string(),
            None => format!(
                "{}/users/{}/mailFolders/{folder_id}/messages/delta?$select=id,categories",
                self.base, self.account.user
            ),
        };
        let prefer = format!("odata.maxpagesize={PAGE}");
        let mut round = Round::default();
        loop {
            let answer = self.get(&url, Some(&prefer)).map_err(Halt::Failed)?;
            if from.is_some() && answer.expired() {
                return Err(Halt::Expired);
            }
            if answer.status != 200 {
                return Err(Halt::Failed(anyhow!(
                    "{}",
                    self.refused("listing the messages", &answer)
                )));
            }
            let page: Page<DeltaItem> = answer.json().map_err(Halt::Failed)?;
            for item in page.value {
                if item.removed.is_some() {
                    round.gone += 1;
                } else {
                    round.offered.push(Offered {
                        id: item.id,
                        tags: item.categories,
                    });
                }
            }
            let Some(next) = page.next else {
                round.link = page.delta.filter(|link| !link.is_empty());
                return Ok(round);
            };
            url = next;
        }
    }

    /// The message's bytes, as sent.
    ///
    /// # Errors
    ///
    /// [`Miss::Gone`] when the server says the message is gone,
    /// [`Miss::Refused`] when it will not hand it over, a 404 for any
    /// other reason included, [`Miss::Unreachable`] when it gave no
    /// usable answer at all.
    pub fn message(&mut self, id: &str) -> Result<Vec<u8>, Miss> {
        let url = format!(
            "{}/users/{}/messages/{id}/$value",
            self.base, self.account.user
        );
        let answer = self
            .get(&url, None)
            .with_context(|| format!("message {}", shortened(id)))
            .map_err(Miss::Unreachable)?;
        if answer.status == 200 {
            Ok(answer.body)
        } else if answer.gone() {
            Err(Miss::Gone)
        } else {
            Err(Miss::Refused(anyhow!(
                "message {}: {}",
                shortened(id),
                answer.trouble()
            )))
        }
    }

    /// The trouble, named — with the way forward when the status says
    /// what it is.
    fn refused(&self, what: &str, answer: &Answer) -> String {
        match answer.status {
            403 => format!(
                "{}: {what} failed ({}); the application needs Mail.Read with admin consent, and an application access policy may block {}",
                self.name,
                answer.trouble(),
                self.account.user
            ),
            404 => format!(
                "{}: {what} failed ({}); no mailbox for {} in this tenant, or it has no licence",
                self.name,
                answer.trouble(),
                self.account.user
            ),
            _ => format!("{}: {what} failed ({})", self.name, answer.trouble()),
        }
    }

    /// GET with the token, asked again through throttling and gateway
    /// trouble, the token fetched anew once when it has expired.
    fn get(&mut self, url: &str, prefer: Option<&str>) -> Result<Answer> {
        if !self.owns(url) {
            bail!(
                "refusing to send the token to {}; only {} is allowed",
                host_of(url),
                host_of(&self.base)
            );
        }
        let mut attempt = 0;
        let mut refreshed = false;
        loop {
            let mut request = self
                .agent
                .get(url)
                .header("Authorization", format!("Bearer {}", self.token))
                .header("Accept", "application/json");
            if let Some(prefer) = prefer {
                request = request.header("Prefer", prefer);
            }
            // A body cut short or stalled is a failure of the same kind
            // as no response at all: the request is sent again whole.
            let read = request
                .call()
                .map_err(anyhow::Error::from)
                .and_then(Answer::read);
            let answer = match read {
                Ok(answer) => answer,
                Err(error) if too_large(&error) => {
                    return Err(error.context(format!(
                        "the response from {} is larger than {} MiB and was not read",
                        host_of(url),
                        MESSAGE_AT_MOST / (1024 * 1024)
                    )));
                }
                Err(error) if attempt < RETRIES => {
                    let pause = backoff(attempt);
                    self.say.line(format_args!(
                        "{}: {}: {error:#}; retrying in {}s, attempt {} of {}",
                        self.name,
                        host_of(url),
                        pause.as_secs(),
                        attempt + 2,
                        RETRIES + 1
                    ));
                    sleep(pause);
                    attempt += 1;
                    continue;
                }
                Err(error) => {
                    return Err(error.context(format!(
                        "no complete response from {} after {} attempts",
                        host_of(url),
                        attempt + 1
                    )));
                }
            };
            if answer.status == 401 && !refreshed {
                refreshed = true;
                self.token = self.fetch_token()?;
                continue;
            }
            if RETRY_STATUS.contains(&answer.status) && attempt < RETRIES {
                let pause = pause_for(&answer, attempt);
                self.say.line(format_args!(
                    "{}: {}: HTTP {}; retrying in {}s, attempt {} of {}",
                    self.name,
                    host_of(url),
                    answer.status,
                    pause.as_secs(),
                    attempt + 2,
                    RETRIES + 1
                ));
                sleep(pause);
                attempt += 1;
                continue;
            }
            return Ok(answer);
        }
    }
}

/// Whether the read stopped because the body passed [`MESSAGE_AT_MOST`].
/// Asking again would get the same body; the other read failures are
/// worth a retry.
fn too_large(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<ureq::Error>(),
        Some(ureq::Error::BodyExceedsLimit(_))
    )
}

/// The pause before retry number `attempt`: doubling, capped.
fn backoff(attempt: u32) -> Duration {
    PAUSE
        .checked_mul(1 << attempt.min(16))
        .map_or(PAUSE_MOST, |pause| pause.min(PAUSE_MOST))
}

/// The pause the server asked for in `Retry-After` when it named a
/// number of seconds, the backoff otherwise.
fn pause_for(answer: &Answer, attempt: u32) -> Duration {
    answer
        .retry_after
        .as_deref()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|seconds| seconds.is_finite())
        .map_or_else(
            || backoff(attempt),
            |seconds| Duration::from_secs_f64(seconds.max(0.0)).min(PAUSE_MOST),
        )
}

/// `scheme://host[:port]` of a URL, or nothing for what is no URL.
fn origin_of(url: &str) -> Option<&str> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme.is_empty() || rest.is_empty() {
        return None;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = &rest[..end];
    if host.is_empty() {
        return None;
    }
    Some(&url[..scheme.len() + 3 + end])
}

/// The host a URL names, for a line that must not repeat the URL — a
/// link carries a continuation token in its query.
fn host_of(url: &str) -> &str {
    origin_of(url)
        .and_then(|origin| origin.split_once("://"))
        .map_or("(invalid URL)", |(_, host)| host)
}

/// A Graph id is long and says nothing to a reader; its head is
/// enough to find it again.
fn shortened(id: &str) -> String {
    let head: String = id.chars().take(20).collect();
    if head.len() < id.len() {
        format!("{head}…")
    } else {
        head
    }
}

#[cfg(test)]
pub mod stub {
    //! A Graph and a tenant answering from a script on loopback, so the
    //! client is tried whole — requests as sent, answers as read.

    use std::collections::{HashMap, VecDeque};
    use std::fmt::Write as _;
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;

    /// What the stub answers to one request.
    #[derive(Clone)]
    pub struct Reply {
        pub status: u16,
        pub headers: Vec<(String, String)>,
        pub body: Vec<u8>,
        /// Announce the body whole, send half of it and close: the
        /// client's read of the body fails.
        pub cut: bool,
    }

    impl Reply {
        pub fn json(status: u16, body: &serde_json::Value) -> Self {
            Self {
                status,
                headers: vec![("Content-Type".to_string(), "application/json".to_string())],
                body: body.to_string().into_bytes(),
                cut: false,
            }
        }

        pub fn bytes(status: u16, body: &[u8]) -> Self {
            Self {
                status,
                headers: Vec::new(),
                body: body.to_vec(),
                cut: false,
            }
        }

        pub fn header(mut self, name: &str, value: &str) -> Self {
            self.headers.push((name.to_string(), value.to_string()));
            self
        }

        pub fn cut(mut self) -> Self {
            self.cut = true;
            self
        }
    }

    /// One request as the stub saw it.
    #[derive(Debug, Clone)]
    pub struct Seen {
        pub method: String,
        pub target: String,
        pub headers: Vec<(String, String)>,
        pub body: String,
    }

    impl Seen {
        pub fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        }
    }

    type Script = HashMap<String, VecDeque<Reply>>;

    pub struct Stub {
        url: String,
        script: Arc<Mutex<Script>>,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl Stub {
        pub fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let script: Arc<Mutex<Script>> = Arc::default();
            let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
            let (script_, seen_) = (Arc::clone(&script), Arc::clone(&seen));
            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let (script, seen) = (Arc::clone(&script_), Arc::clone(&seen_));
                    thread::spawn(move || serve(stream, &script, &seen));
                }
            });
            Self { url, script, seen }
        }

        /// `http://127.0.0.1:PORT`
        pub fn url(&self) -> &str {
            &self.url
        }

        /// Answer `METHOD /target` with these replies in turn; the last
        /// one stands for every request after.
        pub fn script(&self, key: &str, replies: Vec<Reply>) {
            self.script
                .lock()
                .unwrap()
                .insert(key.to_string(), replies.into());
        }

        pub fn seen(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }
    }

    fn serve(stream: TcpStream, script: &Mutex<Script>, seen: &Mutex<Vec<Seen>>) {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut words = line.split_whitespace();
        let method = words.next().unwrap_or_default().to_string();
        let target = words.next().unwrap_or_default().to_string();
        let mut headers = Vec::new();
        let mut length = 0;
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                break;
            }
            if let Some((name, value)) = trimmed.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap();
                }
                headers.push((name.trim().to_string(), value.trim().to_string()));
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let body = String::from_utf8_lossy(&body).into_owned();
        seen.lock().unwrap().push(Seen {
            method: method.clone(),
            target: target.clone(),
            headers,
            body,
        });
        let key = format!("{method} {target}");
        let reply = {
            let mut script = script.lock().unwrap();
            match script.get_mut(&key) {
                Some(replies) if replies.len() > 1 => replies.pop_front(),
                Some(replies) => replies.front().cloned(),
                None => None,
            }
        }
        .unwrap_or_else(|| Reply::bytes(404, format!("no script for {key}").as_bytes()));
        let mut out = format!(
            "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
            reply.status,
            reply.body.len()
        );
        for (name, value) in &reply.headers {
            let _ = write!(out, "{name}: {value}\r\n");
        }
        out.push_str("\r\n");
        let mut stream = reader.into_inner();
        stream.write_all(out.as_bytes()).unwrap();
        let sent = if reply.cut {
            reply.body.len() / 2
        } else {
            reply.body.len()
        };
        stream.write_all(&reply.body[..sent]).unwrap();
        let _ = stream.flush();
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::stub::{Reply, Stub};
    use super::*;

    const USER: &str = "john@example.com";

    fn account() -> config::Graph {
        config::Graph {
            tenant_id: "tenant".to_string(),
            client_id: "client".to_string(),
            client_secret: Some("s3cret".to_string()),
            user: USER.to_string(),
        }
    }

    const TOKEN: &str = "POST /tenant/oauth2/v2.0/token";

    fn token(access: &str) -> Reply {
        Reply::json(200, &json!({ "access_token": access, "expires_in": 3599 }))
    }

    fn folders_key() -> String {
        format!(
            "GET /v1.0/users/{USER}/mailFolders?$select=id,displayName,childFolderCount&$top=100"
        )
    }

    fn folder(id: &str, name: &str, children: u64) -> serde_json::Value {
        json!({ "id": id, "displayName": name, "childFolderCount": children })
    }

    /// A stub with a tenant that issues `tok` and a mailbox of one
    /// folder, `Inbox` as `F1`.
    fn plain() -> Stub {
        let stub = Stub::start();
        stub.script(TOKEN, vec![token("tok")]);
        stub.script(
            &folders_key(),
            vec![Reply::json(
                200,
                &json!({ "value": [folder("F1", "Inbox", 0)] }),
            )],
        );
        stub
    }

    fn reach<'a>(stub: &Stub, name: &'a str, account: &'a config::Graph) -> Result<Graph<'a>> {
        Graph::reach(
            name,
            account,
            "s3cret".to_string(),
            stub.url(),
            &format!("{}/v1.0", stub.url()),
            Say::new(true),
        )
    }

    #[test]
    fn the_login_is_one_form_post_and_the_folders_come_as_a_tree() {
        let stub = Stub::start();
        stub.script(TOKEN, vec![token("tok")]);
        let more = format!("{}/v1.0/folders/page2", stub.url());
        stub.script(
            &folders_key(),
            vec![Reply::json(
                200,
                &json!({ "value": [folder("F1", "Inbox", 1)], "@odata.nextLink": more }),
            )],
        );
        stub.script(
            "GET /v1.0/folders/page2",
            vec![Reply::json(
                200,
                &json!({ "value": [folder("F2", "Sent Items", 0)] }),
            )],
        );
        stub.script(
            &format!(
                "GET /v1.0/users/{USER}/mailFolders/F1/childFolders?$select=id,displayName,childFolderCount&$top=100"
            ),
            vec![Reply::json(
                200,
                &json!({ "value": [folder("F3", "Projects", 0)] }),
            )],
        );
        let account = account();

        let graph = reach(&stub, "m365", &account).unwrap();

        assert_eq!(graph.folders(), ["Inbox/Projects", "Inbox", "Sent Items"]);
        assert_eq!(graph.resolve("Inbox"), Some("F1"));
        assert_eq!(
            graph.resolve("inbox/PROJECTS"),
            Some("F3"),
            "another case still finds it"
        );
        assert_eq!(graph.resolve("Drafts"), None);

        let seen = stub.seen();
        let login = &seen[0];
        assert_eq!(
            (login.method.as_str(), login.target.as_str()),
            ("POST", "/tenant/oauth2/v2.0/token")
        );
        assert!(
            login.body.contains("grant_type=client_credentials"),
            "{}",
            login.body
        );
        assert!(
            login.body.contains("client_secret=s3cret"),
            "{}",
            login.body
        );
        assert!(
            login
                .body
                .contains("scope=https%3A%2F%2Fgraph.microsoft.com%2F.default"),
            "{}",
            login.body
        );
        assert_eq!(seen[1].header("authorization"), Some("Bearer tok"));
    }

    #[test]
    fn a_tenant_that_issues_no_token_says_why() {
        let stub = Stub::start();
        stub.script(
            TOKEN,
            vec![Reply::json(
                401,
                &json!({ "error": "invalid_client", "error_description": "AADSTS7000215: Invalid client secret provided." }),
            )],
        );
        let account = account();
        let refused = reach(&stub, "m365", &account).err().unwrap();
        let text = format!("{refused:#}");
        assert!(text.contains("m365: token request failed"), "{text}");
        assert!(text.contains("AADSTS7000215"), "{text}");
    }

    #[test]
    fn a_folder_list_refused_names_the_permission() {
        let stub = Stub::start();
        stub.script(TOKEN, vec![token("tok")]);
        stub.script(
            &folders_key(),
            vec![Reply::json(
                403,
                &json!({ "error": { "code": "ErrorAccessDenied", "message": "Access is denied." } }),
            )],
        );
        let account = account();
        let refused = reach(&stub, "m365", &account).err().unwrap();
        let text = format!("{refused:#}");
        assert!(
            text.contains("HTTP 403 ErrorAccessDenied: Access is denied."),
            "{text}"
        );
        assert!(text.contains("Mail.Read"), "{text}");
    }

    #[test]
    fn an_expired_token_is_fetched_anew_once_and_throttling_is_waited_out() {
        let stub = plain();
        stub.script(TOKEN, vec![token("first"), token("second")]);
        let key = format!("GET /v1.0/users/{USER}/messages/M1/$value");
        stub.script(
            &key,
            vec![
                Reply::bytes(401, b""),
                Reply::bytes(429, b"").header("Retry-After", "0"),
                Reply::bytes(200, b"Subject: hi\r\n\r\nbody"),
            ],
        );
        let account = account();
        let mut graph = reach(&stub, "m365", &account).unwrap();

        let bytes = graph.message("M1").unwrap();

        assert_eq!(bytes, b"Subject: hi\r\n\r\nbody");
        let asked: Vec<_> = stub
            .seen()
            .into_iter()
            .filter(|seen| seen.target.ends_with("/$value"))
            .map(|seen| seen.header("authorization").unwrap().to_string())
            .collect();
        assert_eq!(asked, ["Bearer first", "Bearer second", "Bearer second"]);
    }

    #[test]
    fn a_body_cut_short_is_asked_for_again() {
        let stub = plain();
        let key = format!("GET /v1.0/users/{USER}/messages/M1/$value");
        stub.script(
            &key,
            vec![
                Reply::bytes(200, b"Subject: hi\r\n\r\nbody").cut(),
                Reply::bytes(200, b"Subject: hi\r\n\r\nbody"),
            ],
        );
        let account = account();
        let mut graph = reach(&stub, "m365", &account).unwrap();

        let bytes = graph.message("M1").unwrap();

        assert_eq!(bytes, b"Subject: hi\r\n\r\nbody");
        let asked = stub
            .seen()
            .iter()
            .filter(|seen| seen.target.ends_with("/$value"))
            .count();
        assert_eq!(asked, 2);
    }

    #[test]
    fn a_message_refused_is_named_with_the_servers_words() {
        let stub = plain();
        let key = format!("GET /v1.0/users/{USER}/messages/M1/$value");
        stub.script(
            &key,
            vec![Reply::json(
                403,
                &json!({ "error": { "code": "ErrorAccessDenied", "message": "Access is denied." } }),
            )],
        );
        let account = account();
        let mut graph = reach(&stub, "m365", &account).unwrap();
        assert_eq!(
            refused(graph.message("M1")),
            "message M1: HTTP 403 ErrorAccessDenied: Access is denied."
        );
    }

    /// The words of a [`Miss::Refused`]; anything else fails the test.
    fn refused(missed: Result<Vec<u8>, Miss>) -> String {
        match missed {
            Err(Miss::Refused(error)) => error.to_string(),
            other => panic!("not refused: {other:?}"),
        }
    }

    #[test]
    fn a_message_the_server_no_longer_has_is_gone() {
        let stub = plain();
        let key = format!("GET /v1.0/users/{USER}/messages/M1/$value");
        stub.script(
            &key,
            vec![Reply::json(
                404,
                &json!({ "error": { "code": "ErrorItemNotFound", "message": "The specified object was not found in the store." } }),
            )],
        );
        let account = account();
        let mut graph = reach(&stub, "m365", &account).unwrap();
        assert!(matches!(graph.message("M1"), Err(Miss::Gone)));
    }

    #[test]
    fn a_token_that_cannot_be_renewed_makes_the_server_unreachable() {
        let stub = plain();
        stub.script(
            TOKEN,
            vec![
                token("tok"),
                Reply::json(
                    401,
                    &json!({ "error": "invalid_client", "error_description": "AADSTS7000222: The provided client secret keys are expired." }),
                ),
            ],
        );
        let key = format!("GET /v1.0/users/{USER}/messages/M1/$value");
        stub.script(&key, vec![Reply::bytes(401, b"")]);
        let account = account();
        let mut graph = reach(&stub, "m365", &account).unwrap();
        let Err(Miss::Unreachable(error)) = graph.message("M1") else {
            panic!("a token the tenant will not renew is not this message's fault");
        };
        assert!(format!("{error:#}").contains("AADSTS7000222"), "{error:#}");
    }

    #[test]
    fn a_404_for_any_other_reason_is_refused_not_gone() {
        let stub = plain();
        let key = format!("GET /v1.0/users/{USER}/messages/M1/$value");
        stub.script(
            &key,
            vec![
                Reply::json(
                    404,
                    &json!({ "error": { "code": "MailboxNotEnabledForRESTAPI", "message": "The mailbox is either inactive, soft-deleted, or is hosted on-premise." } }),
                ),
                Reply::bytes(404, b""),
            ],
        );
        let account = account();
        let mut graph = reach(&stub, "m365", &account).unwrap();
        assert_eq!(
            refused(graph.message("M1")),
            "message M1: HTTP 404 MailboxNotEnabledForRESTAPI: The mailbox is either inactive, soft-deleted, or is hosted on-premise."
        );
        assert_eq!(
            refused(graph.message("M1")),
            "message M1: HTTP 404",
            "no code is not gone either"
        );
    }

    #[test]
    fn a_round_follows_the_pages_drops_the_removed_and_ends_on_the_link() {
        let stub = plain();
        let page2 = format!("{}/v1.0/delta/page2", stub.url());
        let done = format!("{}/v1.0/delta/done", stub.url());
        stub.script(
            &format!(
                "GET /v1.0/users/{USER}/mailFolders/F1/messages/delta?$select=id,categories"
            ),
            vec![Reply::json(
                200,
                &json!({ "value": [{ "id": "M1", "categories": ["Red", "Later"] }, { "id": "M2", "@removed": { "reason": "deleted" } }], "@odata.nextLink": page2 }),
            )],
        );
        stub.script(
            "GET /v1.0/delta/page2",
            vec![Reply::json(
                200,
                &json!({ "value": [{ "id": "M3" }], "@odata.deltaLink": done }),
            )],
        );
        let account = account();
        let mut graph = reach(&stub, "m365", &account).unwrap();

        let round = graph.round("F1", None).ok().unwrap();

        assert_eq!(
            round.offered,
            [
                Offered {
                    id: "M1".to_string(),
                    tags: vec!["Red".to_string(), "Later".to_string()],
                },
                Offered {
                    id: "M3".to_string(),
                    tags: Vec::new(),
                },
            ],
            "the marks come with the id; none listed is none"
        );
        assert_eq!(round.gone, 1);
        assert_eq!(round.link.as_deref(), Some(done.as_str()));
        let prefer: Vec<_> = stub
            .seen()
            .into_iter()
            .filter(|seen| seen.target.contains("delta"))
            .map(|seen| seen.header("prefer").map(str::to_string))
            .collect();
        assert_eq!(
            prefer,
            vec![Some("odata.maxpagesize=500".to_string()); 2],
            "every request of the chain asks for the page size"
        );
    }

    #[test]
    fn a_link_the_server_no_longer_honours_is_said_so_either_way() {
        let stub = plain();
        stub.script(
            "GET /v1.0/delta/gone",
            vec![Reply::json(
                410,
                &json!({ "error": { "code": "syncStateNotFound" } }),
            )],
        );
        stub.script(
            "GET /v1.0/delta/stale",
            vec![Reply::json(
                400,
                &json!({ "error": { "code": "SyncStateNotFound", "message": "…" } }),
            )],
        );
        stub.script(
            "GET /v1.0/delta/refused",
            vec![Reply::json(
                403,
                &json!({ "error": { "code": "ErrorAccessDenied" } }),
            )],
        );
        let account = account();
        let mut graph = reach(&stub, "m365", &account).unwrap();
        let link = |path: &str| format!("{}/v1.0/delta/{path}", stub.url());

        assert!(matches!(
            graph.round("F1", Some(&link("gone"))),
            Err(Halt::Expired)
        ));
        assert!(matches!(
            graph.round("F1", Some(&link("stale"))),
            Err(Halt::Expired)
        ));
        match graph.round("F1", Some(&link("refused"))) {
            Err(Halt::Failed(error)) => {
                assert!(error.to_string().contains("HTTP 403"), "{error:#}");
            }
            _ => panic!("a 403 is about the permission, not the link"),
        }
    }

    #[test]
    fn the_token_goes_to_the_one_host_mail_is_asked_of() {
        let stub = plain();
        let account = account();
        let mut graph = reach(&stub, "m365", &account).unwrap();

        assert!(graph.owns(&format!("{}/v1.0/anything?x=1", stub.url())));
        assert!(!graph.owns("https://evil.example.com/v1.0/delta"));
        assert!(!graph.owns("not a url"));

        let before = stub.seen().len();
        let Err(Halt::Failed(refused)) = graph.round("F1", Some("https://evil.example.com/delta"))
        else {
            panic!("a foreign link is refused");
        };
        assert!(
            refused
                .to_string()
                .contains("refusing to send the token to evil.example.com"),
            "{refused:#}"
        );
        assert_eq!(stub.seen().len(), before, "and nothing was asked anywhere");
    }

    #[test]
    fn the_pause_before_a_retry_grows_and_the_server_may_name_it() {
        assert_eq!(backoff(0), Duration::from_secs(2));
        assert_eq!(backoff(1), Duration::from_secs(4));
        assert_eq!(backoff(4), Duration::from_secs(32));
        assert_eq!(backoff(5), Duration::from_secs(60), "capped");
        assert_eq!(backoff(40), Duration::from_secs(60), "however far it goes");

        let answer = |retry_after: Option<&str>| Answer {
            status: 429,
            retry_after: retry_after.map(str::to_string),
            body: Vec::new(),
        };
        assert_eq!(pause_for(&answer(Some("7")), 0), Duration::from_secs(7));
        assert_eq!(pause_for(&answer(Some("900")), 0), Duration::from_secs(60));
        assert_eq!(
            pause_for(&answer(Some("Wed, 21 Oct 2026 07:28:00 GMT")), 1),
            Duration::from_secs(4),
            "a date falls back to the backoff"
        );
        assert_eq!(pause_for(&answer(None), 2), Duration::from_secs(8));
    }

    #[test]
    fn a_url_names_its_origin_and_its_host() {
        assert_eq!(
            origin_of("https://graph.microsoft.com/v1.0/users?x=1"),
            Some("https://graph.microsoft.com")
        );
        assert_eq!(
            origin_of("http://127.0.0.1:8080"),
            Some("http://127.0.0.1:8080")
        );
        assert_eq!(origin_of("graph.microsoft.com/v1.0"), None);
        assert_eq!(origin_of("https:///v1.0"), None);
        assert_eq!(
            host_of("https://Graph.Microsoft.com/v1.0"),
            "Graph.Microsoft.com"
        );
        assert_eq!(host_of("nonsense"), "(invalid URL)");
        assert_eq!(shortened("AAMkAGI2TG93AAA="), "AAMkAGI2TG93AAA=");
        assert_eq!(shortened(&"A".repeat(30)), format!("{}…", "A".repeat(20)));
    }
}
