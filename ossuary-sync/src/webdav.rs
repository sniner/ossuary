//! A folder on a `WebDAV` server as the place a copy lies: Nextcloud, a
//! NAS, Apache with `mod_dav`, `rclone serve webdav`.
//!
//! A file is uploaded under a temporary name and moved into place, since
//! a PUT is not atomic on every server. The temporary name is fixed per
//! file, `<store>/tmp/<name>.tmp` or `<name>.tmp` beside a file in the
//! root, as the log names its head's: a run after an interrupted one
//! overwrites what that one left instead of adding to it. `.part` would
//! be the usual suffix, and Nextcloud refuses names that end in it.
//!
//! MOVE with `Overwrite: F` is what keeps `create` from replacing a file.
//! The head is moved with `Overwrite: T`, and RFC 4918 lets a server
//! delete the old head before it moves the new one in. A run interrupted
//! between the two leaves the copy without a head; the next run then
//! holds no seal for it, finds every file there with a HEAD request, and
//! writes the head again.
//!
//! On Nextcloud a file larger than [`PART`] is uploaded in parts, with
//! Nextcloud's chunked upload v2, as Nextcloud's own apps do: a proxy in
//! front of it commonly refuses a request body above some hundred MB,
//! and HTTP has no way to write a file piece by piece (Nextcloud refuses
//! a PUT with `Content-Range`, Apache accepts one). When the proxy
//! refuses a part, the parts go down to [`FLOOR`] for the rest of the
//! run. A folder below `…/remote.php/dav/files/<user>/`, the address
//! Nextcloud shows for `WebDAV`, is what marks the server as Nextcloud
//! and names the user the upload collection belongs to. A server that
//! has the address but takes no upload in parts gets every file whole.
//!
//! The collections a file goes into are made before the upload, each once
//! per run. A PUT that finds no collection has spent its body by then, and
//! servers answer a MOVE into a missing collection differently: 409 as
//! RFC 4918 has it, 403 from rclone.

use std::collections::HashSet;
use std::env;
use std::io::{self, Read};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ureq::http::{Method, Request, Response, StatusCode, Uri};
use ureq::tls::TlsConfig;
use ureq::{Agent, AsSendBody, Body, SendBody};

use crate::output::human_bytes;
use crate::remote::{Created, Remote, parent, temp_path};

#[cfg(test)]
pub(crate) mod fake;

/// Where the login for a server comes from. A URL would show it to every
/// user on the machine through the process list, and in the output.
pub const USERNAME: &str = "OSSUARY_SYNC_USERNAME";
pub const PASSWORD: &str = "OSSUARY_SYNC_PASSWORD";

const CONNECT_AT_MOST: Duration = Duration::from_secs(30);
/// A server may take a while to answer a large PUT: it stores the whole
/// body first, and some check it as well.
const ANSWER_AT_MOST: Duration = Duration::from_secs(300);

/// The size of a part on Nextcloud: the size its own apps send, so a
/// proxy that lets theirs through lets these through.
const PART: u64 = 10 * 1024 * 1024;
/// The part once a proxy refused one of [`PART`]: what nginx lets
/// through when nobody set its limit. A proxy that refuses this ends the
/// run.
const FLOOR: u64 = 1024 * 1024;
/// Nextcloud numbers the parts of one upload from 1 to 10 000.
const PARTS_AT_MOST: u64 = 10_000;

const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/></d:prop></d:propfind>"#;

/// A login for a server, sent as HTTP Basic authentication.
pub struct Login {
    username: String,
    password: String,
}

impl Login {
    #[cfg(test)]
    pub fn new(username: &str, password: &str) -> Login {
        Login {
            username: username.to_string(),
            password: password.to_string(),
        }
    }

    /// The login in `OSSUARY_SYNC_USERNAME` and `OSSUARY_SYNC_PASSWORD`,
    /// or `None` when neither is set.
    ///
    /// # Errors
    ///
    /// When only one of them is set, or one is not UTF-8.
    pub fn from_env() -> Result<Option<Login>> {
        match (variable(USERNAME)?, variable(PASSWORD)?) {
            (Some(username), Some(password)) => Ok(Some(Login { username, password })),
            (None, None) => Ok(None),
            (Some(_), None) => bail!(
                "{USERNAME} is set and {PASSWORD} is not; set both, or neither for a server without login"
            ),
            (None, Some(_)) => bail!(
                "{PASSWORD} is set and {USERNAME} is not; set both, or neither for a server without login"
            ),
        }
    }

    fn header(&self) -> String {
        let pair = format!("{}:{}", self.username, self.password);
        format!("Basic {}", STANDARD.encode(pair))
    }
}

fn variable(name: &str) -> Result<Option<String>> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => bail!("{name} is not UTF-8"),
    }
}

/// Whether the server's TLS certificate is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Certificate {
    /// Against the certificate authorities rustls knows.
    Check,
    /// Not at all, for a server with a self-signed certificate. Whoever
    /// answers in its place gets the login.
    AcceptAny,
}

/// The option that sets [`Certificate::AcceptAny`].
pub const ACCEPT_ANY: &str = "--accept-any-certificate";

/// A folder on a `WebDAV` server that holds a copy of an archive, or will.
pub struct WebDav {
    agent: Agent,
    /// The folder's URL, ending in `/`.
    root: String,
    /// The `Authorization` header, when there is a login.
    login: Option<String>,
    /// Collections known to exist, by path below the root.
    made: Mutex<HashSet<String>>,
    /// Nextcloud's collection for uploads in parts, ending in `/`, when
    /// the folder lies on a Nextcloud.
    uploads: Option<String>,
    /// The size of the parts files go up in on Nextcloud: [`PART`], and
    /// [`FLOOR`] for the rest of the run once the server refused one.
    part: AtomicU64,
    /// The smallest part; a test makes it, and `part`, small.
    floor: u64,
    /// Set once the server took no upload in parts: every file after
    /// that goes whole.
    whole: AtomicBool,
    /// The cookies the server set, by name, sent back with every request.
    /// Nextcloud checks the password on each request that brings no
    /// session, a quarter of a second a time on the test server; with
    /// its cookies it keeps the session of the first.
    cookies: Mutex<Vec<(String, String)>>,
}

/// What a PUT came to.
#[derive(Debug, PartialEq, Eq)]
enum Sent {
    Stored,
    /// The server refuses a body that large: 413.
    TooLarge,
}

impl WebDav {
    /// The folder at `url` as the place to copy to.
    ///
    /// The folder must exist, for the reason [`Directory::open`] gives.
    ///
    /// [`Directory::open`]: crate::directory::Directory::open
    ///
    /// # Errors
    ///
    /// When `url` holds a login, sends a password over plain http to
    /// another machine, or names no `WebDAV` folder; when a certificate is
    /// to be accepted for a URL that is not https; when the server does
    /// not accept the login; and when the folder holds files but no
    /// `FORMAT`.
    pub fn open(url: &str, login: Option<Login>, certificate: Certificate) -> Result<WebDav> {
        let uri: Uri = url
            .parse()
            .map_err(|_| anyhow!("{url}: not a URL; name the folder as https://host/path"))?;
        let authority = uri.authority().map(ureq::http::uri::Authority::as_str);
        if authority.is_some_and(|authority| authority.contains('@')) {
            bail!("{url}: a login in the URL is not used; set {USERNAME} and {PASSWORD} instead");
        }
        if login.is_some() && uri.scheme_str() == Some("http") && !loopback(uri.host()) {
            bail!(
                "{url}: the password would be sent unencrypted over http://; name the https:// URL"
            );
        }
        if certificate == Certificate::AcceptAny && uri.scheme_str() != Some("https") {
            bail!("{url}: {ACCEPT_ANY} is for an https:// URL; leave it out");
        }
        let mut root = url.to_string();
        if !root.ends_with('/') {
            root.push('/');
        }
        let agent = Agent::config_builder()
            .http_status_as_error(false)
            .allow_non_standard_methods(true)
            .max_redirects(0)
            .timeout_connect(Some(CONNECT_AT_MOST))
            .timeout_recv_response(Some(ANSWER_AT_MOST))
            .timeout_recv_body(Some(ANSWER_AT_MOST))
            .tls_config(
                TlsConfig::builder()
                    .disable_verification(certificate == Certificate::AcceptAny)
                    .build(),
            )
            .build()
            .into();
        let place = WebDav {
            agent,
            uploads: uploads(&root),
            root,
            login: login.map(|login| login.header()),
            made: Mutex::new(HashSet::new()),
            part: AtomicU64::new(PART),
            floor: FLOOR,
            whole: AtomicBool::new(false),
            cookies: Mutex::new(Vec::new()),
        };
        place.check()?;
        Ok(place)
    }

    /// Refuse a folder that is not there, and one a copy would be mixed
    /// into.
    fn check(&self) -> Result<()> {
        let listing = self.list("0")?;
        if elements(&listing, "collection") == 0 {
            bail!("{}: not a folder; name a folder", self.root);
        }
        if self.read(crate::sync::MARK)?.is_some() {
            return Ok(());
        }
        // The folder itself is the first response of the listing.
        if elements(&self.list("1")?, "response") > 1 {
            bail!(
                "{}: holds files but is not an ossuary archive; name an empty folder or a copy that ossuary sync made",
                self.root
            );
        }
        Ok(())
    }

    /// The multistatus of a PROPFIND on the root, to `depth`.
    fn list(&self, depth: &str) -> Result<String> {
        let mut response = self.call(
            "PROPFIND",
            "",
            &[("depth", depth), ("content-type", "application/xml")],
            PROPFIND_BODY.as_bytes(),
        )?;
        match response.status().as_u16() {
            207 => response
                .body_mut()
                .read_to_string()
                .with_context(|| format!("{}: reading the listing", self.root)),
            404 => bail!(
                "{}: no such folder; create it for the first sync",
                self.root
            ),
            405 | 501 => bail!(
                "{}: the server offers no WebDAV there; name the WebDAV URL of a folder",
                self.root
            ),
            _ => Err(self.refused("reading", "", &response)),
        }
    }

    fn made(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        // The set only ever grows, so a panic while it was held leaves it
        // usable.
        self.made.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.root)
    }

    /// Send one request for `path` below the root.
    fn call(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: impl AsSendBody,
    ) -> Result<Response<Body>> {
        self.send(method, &self.url(path), headers, body)
    }

    /// Send one request for `url`.
    fn send(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: impl AsSendBody,
    ) -> Result<Response<Body>> {
        let method = Method::from_bytes(method.as_bytes())
            .expect("every method named here is an HTTP token");
        let mut request = Request::builder().method(method).uri(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        if let Some(login) = &self.login {
            request = request.header("authorization", login);
        }
        let cookie = self.cookie();
        if !cookie.is_empty() {
            request = request.header("cookie", cookie);
        }
        let request = request
            .body(body)
            .with_context(|| format!("{url}: not a URL"))?;
        let response = self
            .agent
            .run(request)
            .map_err(|error| failed(url, &error))?;
        self.keep_cookies(&response);
        Ok(response)
    }

    /// The `Cookie` header for the cookies the server set.
    fn cookie(&self) -> String {
        let cookies = self.cookies.lock().unwrap_or_else(PoisonError::into_inner);
        let pairs: Vec<_> = cookies
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        pairs.join("; ")
    }

    /// Keep the cookies `response` sets. There is one server, so the
    /// attributes that say where a cookie goes do not matter; one that
    /// is set empty or to `deleted` is dropped.
    fn keep_cookies(&self, response: &Response<Body>) {
        let mut cookies = self.cookies.lock().unwrap_or_else(PoisonError::into_inner);
        for header in response.headers().get_all("set-cookie") {
            let Ok(header) = header.to_str() else {
                continue;
            };
            let pair = header.split(';').next().unwrap_or_default();
            let Some((name, value)) = pair.split_once('=') else {
                continue;
            };
            let (name, value) = (name.trim(), value.trim());
            cookies.retain(|(kept, _)| kept != name);
            if !value.is_empty() && value != "deleted" {
                cookies.push((name.to_string(), value.to_string()));
            }
        }
    }

    /// The error for an answer the caller does not expect.
    fn refused(&self, act: &str, path: &str, response: &Response<Body>) -> anyhow::Error {
        let url = self.url(path);
        let status = response.status();
        match status.as_u16() {
            401 if self.login.is_some() => anyhow!(
                "{url}: the server does not accept the login in {USERNAME} and {PASSWORD}; check both"
            ),
            401 => anyhow!("{url}: the server asks for a login; set {USERNAME} and {PASSWORD}"),
            403 => anyhow!(
                "{url}: the server forbids {act}; give the account read and write access to the target folder"
            ),
            507 => anyhow!("{url}: the server has no space left; free space there and sync again"),
            300..=399 => match response.headers().get("location") {
                Some(location) => anyhow!(
                    "{url}: the server redirects to {}; name that URL as the target",
                    location.to_str().unwrap_or("another URL")
                ),
                None => anyhow!("{url}: {act} failed, the server answers {}", show(status)),
            },
            _ => anyhow!("{url}: {act} failed, the server answers {}", show(status)),
        }
    }

    /// Make the collection at `path` and the ones above it, unless they
    /// are known to exist.
    fn make(&self, path: &str) -> Result<()> {
        if path.is_empty() || self.made().contains(path) {
            return Ok(());
        }
        self.make(parent(path))?;
        let response = done(self.call("MKCOL", &format!("{path}/"), &[], ())?);
        match response.status().as_u16() {
            // 405: there is one already. rclone answers 201 then too.
            201 | 405 => {
                self.made().insert(path.to_string());
                Ok(())
            }
            _ => Err(self.refused("making the folder", path, &response)),
        }
    }

    /// Upload to `path`, replacing what is there, on the way to `dest`,
    /// the path an error names.
    fn put(&self, path: &str, dest: &str, body: impl AsSendBody, size: u64) -> Result<()> {
        match self.try_put(&self.url(path), &[], body, size, dest)? {
            Sent::Stored => Ok(()),
            Sent::TooLarge if self.root.contains("/remote.php/webdav/") => bail!(
                "{}: the server refuses a file of {}; name the folder by its address below /remote.php/dav/files/, where files go up in parts, or raise the server's upload limit",
                self.url(dest),
                human_bytes(size)
            ),
            Sent::TooLarge => bail!(
                "{}: the server refuses a file of {}; raise its upload limit and sync again",
                self.url(dest),
                human_bytes(size)
            ),
        }
    }

    /// One PUT of `size` bytes to `url`, on the way to `dest`.
    fn try_put(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: impl AsSendBody,
        size: u64,
        dest: &str,
    ) -> Result<Sent> {
        let length = size.to_string();
        let mut all = vec![("content-length", length.as_str())];
        all.extend_from_slice(headers);
        let response = done(self.send("PUT", url, &all, body)?);
        match response.status().as_u16() {
            200 | 201 | 204 => Ok(Sent::Stored),
            413 => Ok(Sent::TooLarge),
            _ => Err(self.refused("writing", dest, &response)),
        }
    }

    /// Upload to `path` on a Nextcloud, on the way to `dest`. A file no
    /// larger than a part is read into memory and sent whole, so that it
    /// can go again in parts when the server refuses it whole.
    fn put_to_nextcloud(
        &self,
        uploads: &str,
        path: &str,
        dest: &str,
        from: &mut dyn Read,
        size: u64,
    ) -> Result<()> {
        if size > self.part() {
            return self.put_in_parts(uploads, path, dest, from, size);
        }
        let bytes = read_exactly(from, size, dest)?;
        if self.try_put(&self.url(path), &[], &bytes[..], size, dest)? == Sent::Stored {
            return Ok(());
        }
        if !self.lower() {
            bail!(
                "{}: the server refuses a file of {}; raise its upload limit and sync again",
                self.url(dest),
                human_bytes(size)
            );
        }
        self.put_in_parts(uploads, path, dest, &mut &bytes[..], size)
    }

    /// Upload to `path` in parts, on the way to `dest`: a collection
    /// for the upload in `uploads`, one PUT per part, and a MOVE of its
    /// `.file`, on which Nextcloud puts the parts together at `path`.
    /// A part is read into memory before it is sent, so that it can go
    /// again in smaller parts when the server refuses it.
    fn put_in_parts(
        &self,
        uploads: &str,
        path: &str,
        dest: &str,
        from: &mut dyn Read,
        size: u64,
    ) -> Result<()> {
        // Named after the file, so a run after an interrupted one finds
        // the parts that one left, and removes them first.
        let name = dest.rsplit('/').next().unwrap_or(dest);
        let upload = format!("{uploads}ossuary-{name}/");
        let destination = self.url(path);
        self.send("DELETE", &upload, &[], ()).map(done).ok();
        let headers = [("destination", destination.as_str())];
        let response = done(self.send("MKCOL", &upload, &headers, ())?);
        if response.status() != StatusCode::CREATED {
            // A server that only looks like Nextcloud, or one that takes
            // no uploads in parts: this file and every one after it go
            // whole.
            self.whole.store(true, Ordering::Relaxed);
            return self.put(path, dest, SendBody::from_reader(from), size);
        }
        let mut sent = 0;
        let mut number = 1;
        while sent < size {
            let length = self.part_for(size).min(size - sent);
            let buffer = read_exactly(&mut *from, length, dest)?;
            let mut rest = &buffer[..];
            while !rest.is_empty() {
                let take = usize::try_from(self.part_for(size))
                    .map_or(rest.len(), |part| part.min(rest.len()));
                let (piece, after) = rest.split_at(take);
                // Padded, so that the parts sort by name as by number.
                let url = format!("{upload}{number:05}");
                match self.try_put(&url, &headers, piece, piece.len() as u64, dest)? {
                    Sent::Stored => {
                        number += 1;
                        rest = after;
                    }
                    Sent::TooLarge if self.lower() => {}
                    Sent::TooLarge => bail!(
                        "{}: the server refuses a part of {} of a file uploaded in parts; raise its upload limit and sync again",
                        self.url(dest),
                        human_bytes(piece.len() as u64)
                    ),
                }
            }
            sent += length;
        }
        let headers = [
            ("destination", destination.as_str()),
            ("oc-total-length", &size.to_string()),
            ("overwrite", "T"),
        ];
        let response = done(self.send("MOVE", &format!("{upload}.file"), &headers, ())?);
        match response.status().as_u16() {
            201 | 204 => Ok(()),
            _ => Err(self.refused("putting the parts together", dest, &response)),
        }
    }

    fn part(&self) -> u64 {
        self.part.load(Ordering::Relaxed)
    }

    /// The part for a file of `size`: no more than Nextcloud numbers.
    fn part_for(&self, size: u64) -> u64 {
        self.part().max(size.div_ceil(PARTS_AT_MOST))
    }

    /// Go down to the smallest part: `false` when the part is that small
    /// already.
    fn lower(&self) -> bool {
        self.part.fetch_min(self.floor, Ordering::Relaxed) > self.floor
    }

    /// Move `from` to `to`: `false` when `to` exists and `overwrite` is
    /// not set.
    fn move_to(&self, from: &str, to: &str, overwrite: bool) -> Result<bool> {
        let destination = self.url(to);
        let headers = [
            ("destination", destination.as_str()),
            ("overwrite", if overwrite { "T" } else { "F" }),
        ];
        let response = done(self.call("MOVE", from, &headers, ())?);
        match response.status().as_u16() {
            201 | 204 => Ok(true),
            412 => Ok(false),
            _ => Err(self.refused("moving a file into place", to, &response)),
        }
    }

    /// Remove `path`. Only the temporary files are ever removed, and one
    /// that stays is overwritten by the next upload of the same file.
    fn remove(&self, path: &str) {
        self.call("DELETE", path, &[], ()).map(done).ok();
    }
}

impl Remote for WebDav {
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
        let mut response = self.call("GET", path, &[], ())?;
        match response.status().as_u16() {
            200 => response
                .body_mut()
                .read_to_vec()
                .map(Some)
                .with_context(|| format!("{}: reading", self.url(path))),
            404 => Ok(None),
            _ => Err(self.refused("reading", path, &response)),
        }
    }

    fn create(&self, path: &str, from: &mut dyn Read, size: u64) -> Result<Created> {
        let response = done(self.call("HEAD", path, &[], ())?);
        match response.status().as_u16() {
            200 => return Ok(Created::Known),
            404 => {}
            _ => return Err(self.refused("reading", path, &response)),
        }
        let temp = temp_path(path);
        self.make(parent(&temp))?;
        self.make(parent(path))?;
        match &self.uploads {
            Some(uploads) if !self.whole.load(Ordering::Relaxed) => {
                self.put_to_nextcloud(uploads, &temp, path, from, size)?;
            }
            _ => self.put(&temp, path, SendBody::from_reader(from), size)?,
        }
        if self.move_to(&temp, path, false)? {
            Ok(Created::New)
        } else {
            self.remove(&temp);
            Ok(Created::Known)
        }
    }

    fn replace(&self, path: &str, bytes: &[u8]) -> Result<()> {
        let temp = temp_path(path);
        self.make(parent(&temp))?;
        self.put(&temp, path, bytes, bytes.len() as u64)?;
        self.move_to(&temp, path, true)?;
        Ok(())
    }

    fn make_dir(&self, path: &str) -> Result<()> {
        self.make(path)
    }
}

/// The error for a request that got no answer. A certificate rustls
/// does not accept surfaces as invalid data on the connection, which is
/// all ureq passes on; nothing else in a TLS handshake fails that way.
fn failed(url: &str, error: &ureq::Error) -> anyhow::Error {
    if let ureq::Error::Io(io) = error
        && url.starts_with("https://")
        && io.kind() == io::ErrorKind::InvalidData
    {
        return anyhow!(
            "{url}: the server's certificate is not trusted ({io}); for a server with a self-signed certificate, pass {ACCEPT_ANY}"
        );
    }
    anyhow!("{url}: {error}")
}

/// The answer with its body read and dropped, so that its connection can
/// carry the next request: one left unread is closed, and over https each
/// new connection costs a handshake.
fn done(mut response: Response<Body>) -> Response<Body> {
    io::copy(&mut response.body_mut().as_reader(), &mut io::sink()).ok();
    response
}

/// Exactly `size` bytes from `from`, on the way to `dest`.
fn read_exactly(from: &mut dyn Read, size: u64, dest: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    from.take(size)
        .read_to_end(&mut bytes)
        .with_context(|| format!("{dest}: reading the file here"))?;
    if bytes.len() as u64 != size {
        bail!("{dest}: the file here changed while it was read; sync again");
    }
    Ok(bytes)
}

/// Nextcloud's collection for uploads in parts, for a folder below
/// `…/remote.php/dav/files/<user>/`: the user's below
/// `…/remote.php/dav/uploads/`.
fn uploads(root: &str) -> Option<String> {
    let (base, rest) = root.split_once("/remote.php/dav/files/")?;
    let (user, _) = rest.split_once('/')?;
    Some(format!("{base}/remote.php/dav/uploads/{user}/"))
}

/// How many elements of the local name `name` an XML text holds, under
/// whatever prefix the server binds `DAV:` to. A listing is read for two
/// counts only, and a parser would be a dependency for those.
fn elements(xml: &str, name: &str) -> usize {
    xml.split('<')
        .skip(1)
        .filter(|tag| {
            let tag = tag
                .split(|c: char| c.is_whitespace() || c == '>' || c == '/')
                .next()
                .unwrap_or_default();
            tag.rsplit(':').next() == Some(name)
        })
        .count()
}

/// Whether `host` is this machine, where plain http stays on the machine.
fn loopback(host: Option<&str>) -> bool {
    let Some(host) = host else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn show(status: StatusCode) -> String {
    match status.canonical_reason() {
        Some(reason) => format!("{} {reason}", status.as_u16()),
        None => status.as_u16().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::fake::Server;
    use super::*;

    fn served() -> (TempDir, Server) {
        let dir = TempDir::new().unwrap();
        let server = Server::start(dir.path(), None);
        (dir, server)
    }

    #[test]
    fn a_created_file_arrives_whole_and_leaves_no_temporary_file() {
        let (dir, server) = served();
        let place = WebDav::open(&server.url, None, Certificate::Check).unwrap();

        let created = place
            .create("content/ab/cd/abcd", &mut &b"bytes"[..], 5)
            .unwrap();

        assert_eq!(created, Created::New);
        assert_eq!(
            fs::read(dir.path().join("content/ab/cd/abcd")).unwrap(),
            b"bytes"
        );
        let leftovers: Vec<_> = fs::read_dir(dir.path().join("content/tmp"))
            .unwrap()
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn a_file_that_exists_is_not_sent_again() {
        let (dir, server) = served();
        let place = WebDav::open(&server.url, None, Certificate::Check).unwrap();
        place.create("FORMAT", &mut &b"first"[..], 5).unwrap();
        let before = server.requests().len();

        let created = place.create("FORMAT", &mut &b"second"[..], 6).unwrap();

        assert_eq!(created, Created::Known);
        assert_eq!(fs::read(dir.path().join("FORMAT")).unwrap(), b"first");
        assert_eq!(server.requests()[before..], ["HEAD /dav/FORMAT"]);
    }

    #[test]
    fn a_file_that_arrives_after_the_upload_is_kept() {
        let (dir, server) = served();
        let place = WebDav::open(&server.url, None, Certificate::Check).unwrap();
        fs::create_dir_all(dir.path().join("content/tmp")).unwrap();
        fs::create_dir_all(dir.path().join("content/ab/cd")).unwrap();
        fs::write(dir.path().join("content/tmp/abcd.tmp"), b"second").unwrap();
        fs::write(dir.path().join("content/ab/cd/abcd"), b"first").unwrap();

        let moved = place
            .move_to("content/tmp/abcd.tmp", "content/ab/cd/abcd", false)
            .unwrap();

        assert!(!moved);
        let kept = fs::read(dir.path().join("content/ab/cd/abcd")).unwrap();
        assert_eq!(kept, b"first");
    }

    #[test]
    fn replacing_writes_over_the_old_file() {
        let (dir, server) = served();
        let place = WebDav::open(&server.url, None, Certificate::Check).unwrap();
        place.replace("head.jsonl", b"old\n").unwrap();

        place.replace("head.jsonl", b"new\n").unwrap();

        assert_eq!(place.read("head.jsonl").unwrap().unwrap(), b"new\n");
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|item| item.unwrap().file_name())
            .collect();
        assert_eq!(names, ["head.jsonl"]);
    }

    #[test]
    fn reading_a_file_that_is_not_there_answers_none() {
        let (_dir, server) = served();
        let place = WebDav::open(&server.url, None, Certificate::Check).unwrap();

        assert!(place.read("head.jsonl").unwrap().is_none());
    }

    #[test]
    fn a_collection_is_made_once() {
        let (dir, server) = served();
        let place = WebDav::open(&server.url, None, Certificate::Check).unwrap();

        place.make_dir("content").unwrap();
        place.make_dir("content").unwrap();

        assert!(dir.path().join("content").is_dir());
        let made = server.requests();
        let made: Vec<_> = made
            .iter()
            .filter(|line| line.starts_with("MKCOL"))
            .collect();
        assert_eq!(made, ["MKCOL /dav/content/"]);
    }

    #[test]
    fn a_file_above_the_servers_limit_is_named_with_its_size() {
        let dir = TempDir::new().unwrap();
        let server = Server::limited(dir.path(), 4);
        let place = WebDav::open(&server.url, None, Certificate::Check).unwrap();

        let error = place
            .create("content/ab/cd/abcd", &mut &b"bytes"[..], 5)
            .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("content/ab/cd/abcd: the server refuses a file of 5 B"));
    }

    #[test]
    fn a_large_file_goes_to_nextcloud_in_parts() {
        let dir = TempDir::new().unwrap();
        let server = Server::nextcloud(dir.path(), 4);
        let place = small_parts(&server, 4, 4);

        let created = place
            .create("content/ab/cd/abcd", &mut &b"0123456789"[..], 10)
            .unwrap();

        assert_eq!(created, Created::New);
        let stored = fs::read(dir.path().join("content/ab/cd/abcd")).unwrap();
        assert_eq!(stored, b"0123456789");
        let parts = server.requests();
        let parts: Vec<_> = parts
            .iter()
            .filter(|line| line.starts_with("PUT /remote.php/dav/uploads/"))
            .collect();
        assert_eq!(parts.len(), 3);
    }

    #[test]
    fn a_small_file_goes_to_nextcloud_whole() {
        let dir = TempDir::new().unwrap();
        let server = Server::nextcloud(dir.path(), 4);
        let place = small_parts(&server, 4, 4);

        place.create("FORMAT", &mut &b"tiny"[..], 4).unwrap();

        assert_eq!(fs::read(dir.path().join("FORMAT")).unwrap(), b"tiny");
        let uploads = server.requests();
        assert!(!uploads.iter().any(|line| line.contains("/uploads/")));
    }

    /// A place on `server` whose parts start at `part` bytes and go down
    /// to `floor`.
    fn small_parts(server: &Server, part: u64, floor: u64) -> WebDav {
        let mut place = WebDav::open(&server.url, None, Certificate::Check).unwrap();
        place.part = AtomicU64::new(part);
        place.floor = floor;
        place
    }

    #[test]
    fn parts_get_smaller_when_the_server_refuses_them() {
        let dir = TempDir::new().unwrap();
        let server = Server::nextcloud(dir.path(), 4);
        let place = small_parts(&server, 8, 4);

        // Smaller than a part, so refused whole first.
        place
            .create("content/ef/01/ef01", &mut &b"abcdef"[..], 6)
            .unwrap();
        place
            .create("content/ab/cd/abcd", &mut &b"0123456789"[..], 10)
            .unwrap();

        let stored = |path| fs::read(dir.path().join(path)).unwrap();
        assert_eq!(stored("content/ab/cd/abcd"), b"0123456789");
        assert_eq!(stored("content/ef/01/ef01"), b"abcdef");
        assert_eq!(place.part(), 4, "the smaller part is kept");
    }

    #[test]
    fn a_part_refused_at_the_smallest_size_ends_the_run() {
        let dir = TempDir::new().unwrap();
        let server = Server::nextcloud(dir.path(), 2);
        let place = small_parts(&server, 8, 4);

        let error = place
            .create("content/ab/cd/abcd", &mut &b"0123456789"[..], 10)
            .unwrap_err();

        assert!(error.to_string().contains("refuses a part of 4 B"));
    }

    #[test]
    fn a_server_that_takes_no_parts_gets_whole_files() {
        let dir = TempDir::new().unwrap();
        let server = Server::files_without_parts(dir.path());
        let place = small_parts(&server, 4, 4);

        place
            .create("content/ab/cd/abcd", &mut &b"0123456789"[..], 10)
            .unwrap();
        place
            .create("content/ef/01/ef01", &mut &b"abcdef"[..], 6)
            .unwrap();

        assert_eq!(
            fs::read(dir.path().join("content/ab/cd/abcd")).unwrap(),
            b"0123456789"
        );
        let tries = server.requests();
        let tries = tries
            .iter()
            .filter(|line| line.starts_with("MKCOL /remote.php/dav/uploads/"));
        assert_eq!(tries.count(), 1, "tried once, then whole");
    }

    #[test]
    fn cookies_the_server_sets_go_back_to_it() {
        let (_dir, server) = served();
        let place = WebDav::open(&server.url, None, Certificate::Check).unwrap();
        let set = Response::builder()
            .header("set-cookie", "oc_session=abc; path=/; HttpOnly")
            .header("set-cookie", "nc_sameSiteCookielax=true; path=/")
            .body(Body::builder().data(""))
            .unwrap();
        let gone = Response::builder()
            .header(
                "set-cookie",
                "oc_session=deleted; expires=Thu, 01 Jan 1970 00:00:00 GMT",
            )
            .body(Body::builder().data(""))
            .unwrap();

        place.keep_cookies(&set);
        assert_eq!(place.cookie(), "oc_session=abc; nc_sameSiteCookielax=true");
        place.keep_cookies(&gone);
        assert_eq!(place.cookie(), "nc_sameSiteCookielax=true");
    }

    #[test]
    fn nextcloud_uploads_lie_beside_the_users_files() {
        assert_eq!(
            uploads("https://cloud.example.com/remote.php/dav/files/backup/archive/").as_deref(),
            Some("https://cloud.example.com/remote.php/dav/uploads/backup/")
        );
        assert_eq!(
            uploads("https://example.com/nextcloud/remote.php/dav/files/backup/").as_deref(),
            Some("https://example.com/nextcloud/remote.php/dav/uploads/backup/")
        );
        assert_eq!(
            uploads("https://cloud.example.com/remote.php/webdav/archive/"),
            None
        );
        assert_eq!(uploads("https://nas.example.com/archive/"), None);
    }

    #[test]
    fn a_missing_folder_is_refused() {
        let (_dir, server) = served();

        let error = WebDav::open(&format!("{}absent", server.url), None, Certificate::Check)
            .err()
            .unwrap();

        assert!(error.to_string().contains("no such folder"));
    }

    #[test]
    fn a_folder_with_other_files_is_refused() {
        let (dir, server) = served();
        fs::write(dir.path().join("notes.txt"), "mine").unwrap();

        let error = WebDav::open(&server.url, None, Certificate::Check)
            .err()
            .unwrap();

        assert!(error.to_string().contains("not an ossuary archive"));
    }

    #[test]
    fn a_folder_with_a_mark_is_a_copy() {
        let (dir, server) = served();
        fs::write(dir.path().join("FORMAT"), "{}").unwrap();
        fs::create_dir(dir.path().join("content")).unwrap();

        assert!(WebDav::open(&server.url, None, Certificate::Check).is_ok());
    }

    #[test]
    fn a_file_is_not_a_folder() {
        let (dir, server) = served();
        fs::write(dir.path().join("archive"), "a file").unwrap();

        let error = WebDav::open(&format!("{}archive", server.url), None, Certificate::Check)
            .err()
            .unwrap();

        assert!(error.to_string().contains("not a folder"));
    }

    #[test]
    fn a_server_that_asks_for_a_login_names_the_variables() {
        let dir = TempDir::new().unwrap();
        let server = Server::start(dir.path(), Some(("backup", "secret")));

        let error = WebDav::open(&server.url, None, Certificate::Check)
            .err()
            .unwrap();

        assert!(error.to_string().contains(USERNAME));
    }

    #[test]
    fn a_wrong_login_is_refused_and_the_right_one_works() {
        let dir = TempDir::new().unwrap();
        let server = Server::start(dir.path(), Some(("backup", "secret")));

        let wrong = WebDav::open(
            &server.url,
            Some(Login::new("backup", "guess")),
            Certificate::Check,
        );
        let right = WebDav::open(
            &server.url,
            Some(Login::new("backup", "secret")),
            Certificate::Check,
        );

        assert!(wrong.err().unwrap().to_string().contains("does not accept"));
        assert!(right.is_ok());
    }

    #[test]
    fn a_login_in_the_url_is_refused() {
        let error = WebDav::open(
            "https://backup:secret@dav.example.com/archive",
            None,
            Certificate::Check,
        )
        .err()
        .unwrap();

        assert!(error.to_string().contains(USERNAME));
    }

    #[test]
    fn a_password_is_not_sent_over_plain_http_to_another_machine() {
        let login = Some(Login::new("backup", "secret"));

        let error = WebDav::open("http://dav.example.com/archive", login, Certificate::Check)
            .err()
            .unwrap();

        assert!(error.to_string().contains("https://"));
    }

    #[test]
    fn accepting_any_certificate_needs_https() {
        let (_dir, server) = served();

        let error = WebDav::open(&server.url, None, Certificate::AcceptAny)
            .err()
            .unwrap();

        assert!(error.to_string().contains("is for an https:// URL"));
    }

    #[test]
    fn elements_are_counted_under_any_prefix() {
        let xml = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:resourcetype><d:collection/></d:resourcetype></d:response><response xmlns="DAV:"/></d:multistatus>"#;

        assert_eq!(elements(xml, "response"), 2);
        assert_eq!(elements(xml, "collection"), 1);
        assert_eq!(elements(xml, "multistatus"), 1);
    }

    #[test]
    fn loopback_hosts_are_this_machine() {
        assert!(loopback(Some("localhost")));
        assert!(loopback(Some("127.0.0.1")));
        assert!(loopback(Some("[::1]")));
        assert!(!loopback(Some("dav.example.com")));
        assert!(!loopback(None));
    }
}
