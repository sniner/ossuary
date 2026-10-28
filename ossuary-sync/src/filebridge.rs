//! A folder in a location on a filebridge server as the place a copy
//! lies.
//!
//! filebridged writes straight into the file it is sent, so a reader
//! could see half of one. A file is written under the temporary name
//! [`temp_path`] gives and renamed into place. The rename replaces a file
//! of that name when the location allows replacing, which the head needs
//! anyway; every other file in a copy is named by its content or fixed by
//! the format, so a file replaced that way is replaced with the same
//! bytes.
//!
//! With a token, filebridge signs every request and encrypts the file and
//! its path (ChaCha20-Poly1305), over plain http too. Without one,
//! everything goes in the clear.
//!
//! The client is async. A place keeps a runtime of one thread for it. An
//! upload streams the file through an in-memory pipe: this thread reads
//! the file into it, while a second thread drives the request that sends
//! from it, so a file is never held in memory whole.

use std::io::{self, Read};
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use filebridge::{Error, FileBridgeClient, FileBridgeLocation};
use tokio::io::{AsyncWriteExt as _, DuplexStream};
use tokio::runtime::Runtime;
use ureq::http::Uri;

use crate::output::human_bytes;
use crate::remote::{Created, Remote, temp_path};
use crate::sync::MARK;

/// Where the token for a location comes from.
pub const TOKEN: &str = "OSSUARY_SYNC_TOKEN";

/// How long one request may take. A request is one whole file, and
/// filebridge offers no limit on silence instead, so it is long.
const REQUEST_AT_MOST: Duration = Duration::from_secs(6 * 60 * 60);

/// The size of the pipe an upload streams through.
const PIPE: usize = 256 * 1024;

/// A folder in a filebridge location that holds a copy of an archive, or
/// will.
pub struct Filebridge {
    runtime: Runtime,
    client: FileBridgeClient,
    /// The target as it was named, for messages.
    target: String,
    location: String,
    token: Option<String>,
    /// The folder in the location, `""` or ending in `/`.
    root: String,
}

/// The token in `OSSUARY_SYNC_TOKEN`, or `None` when it is not set.
///
/// # Errors
///
/// When it is not UTF-8.
pub fn token_from_env() -> Result<Option<String>> {
    match std::env::var(TOKEN) {
        Ok(token) => Ok(Some(token)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => bail!("{TOKEN} is not UTF-8"),
    }
}

/// The parts of a target `filebridge+http://HOST:PORT/LOCATION/FOLDER`:
/// the server's URL, the location, and the folder in it.
fn split(target: &str) -> Result<(String, String, String)> {
    let url = target.strip_prefix("filebridge+").unwrap_or(target);
    let uri: Uri = url.parse().map_err(|_| shape(target))?;
    let (Some(scheme), Some(authority)) = (uri.scheme_str(), uri.authority()) else {
        return Err(shape(target));
    };
    if scheme != "http" && scheme != "https" {
        return Err(shape(target));
    }
    if authority.as_str().contains('@') {
        bail!("{target}: a token in the URL is not used; set {TOKEN} instead");
    }
    let mut parts = uri.path().split('/').filter(|part| !part.is_empty());
    let location = parts.next().ok_or_else(|| shape(target))?.to_string();
    let mut root = parts.collect::<Vec<_>>().join("/");
    if !root.is_empty() {
        root.push('/');
    }
    Ok((format!("{scheme}://{authority}"), location, root))
}

fn shape(target: &str) -> anyhow::Error {
    anyhow!("{target}: name a filebridge target as filebridge+http://HOST:PORT/LOCATION/FOLDER")
}

impl Filebridge {
    /// The folder `target` names as the place to copy to.
    ///
    /// The folder must exist, for the reason [`Directory::open`] gives.
    ///
    /// [`Directory::open`]: crate::directory::Directory::open
    ///
    /// # Errors
    ///
    /// When `target` is not a filebridge target; when the location
    /// refuses the token or does not allow what a copy needs; and when
    /// the folder is missing, or holds files but no `FORMAT`.
    pub fn open(target: &str, token: Option<String>) -> Result<Filebridge> {
        let (server, location, root) = split(target)?;
        let client = FileBridgeClient::with_timeout(&server, REQUEST_AT_MOST)
            .map_err(|error| anyhow!("{target}: {error}"))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("starting the runtime for the filebridge client")?;
        let place = Filebridge {
            runtime,
            client,
            target: target.to_string(),
            location,
            token,
            root,
        };
        place.check()?;
        Ok(place)
    }

    fn handle(&self) -> FileBridgeLocation<'_> {
        self.client.location(&self.location, self.token.clone())
    }

    /// `path` in the location.
    fn full(&self, path: &str) -> String {
        format!("{}{path}", self.root)
    }

    /// Refuse a location that does not allow what a copy needs, a folder
    /// that is not there, and one a copy would be mixed into.
    fn check(&self) -> Result<()> {
        let location = self.handle();
        let allowed = self
            .runtime
            .block_on(location.permissions())
            .map_err(|error| self.failed("reading the location's permissions", "", &error))?;
        let needed = [
            ("allow_read", allowed.read),
            ("allow_create", allowed.create),
            ("allow_replace", allowed.replace),
            ("allow_delete", allowed.delete),
            ("allow_inspect", allowed.inspect),
            ("allow_recurse", allowed.recurse),
            ("allow_mkdir", allowed.mkdir),
        ];
        let missing: Vec<_> = needed
            .iter()
            .filter(|(_, granted)| !granted)
            .map(|(name, _)| *name)
            .collect();
        if !missing.is_empty() {
            bail!(
                "{}: the location does not allow what a copy needs; set {} to true for it in the configuration of filebridged",
                self.target,
                missing.join(", ")
            );
        }
        let folder = self.root.trim_end_matches('/');
        let listing = self
            .runtime
            .block_on(location.list((!folder.is_empty()).then_some(folder)));
        let entries = match listing {
            Ok(entries) => entries,
            Err(error) if status(&error) == Some(404) => bail!(
                "{}: no such folder; create it for the first sync",
                self.target
            ),
            Err(error) => return Err(self.failed("reading the folder", "", &error)),
        };
        if !entries.is_empty() && !entries.iter().any(|entry| entry.name == MARK) {
            bail!(
                "{}: holds files but is not an ossuary archive; name an empty folder or a copy that ossuary sync made",
                self.target
            );
        }
        Ok(())
    }

    /// The error for a request that failed while doing `act` for `path`.
    fn failed(&self, act: &str, path: &str, error: &Error) -> anyhow::Error {
        let at = if path.is_empty() {
            self.target.clone()
        } else {
            format!("{} {path}", self.target)
        };
        match (status(error), error) {
            (Some(401), _) if self.token.is_some() => {
                anyhow!("{at}: the location does not accept the token in {TOKEN}; check it")
            }
            (Some(401), _) => anyhow!("{at}: the location asks for a token; set {TOKEN}"),
            // A signed request to a location without a token: filebridged
            // 0.4.1 refuses it, an older one answers it unsigned, without
            // the request's nonce.
            (Some(400), Error::Api(_, reason)) if reason.contains("has no token") => {
                anyhow!("{at}: the location has no token; unset {TOKEN} for it")
            }
            (_, Error::NonceMismatch) => {
                anyhow!("{at}: the location has no token; unset {TOKEN} for it")
            }
            (Some(403), Error::Api(_, reason)) => anyhow!(
                "{at}: the server forbids {act} ({}); check the location's configuration",
                reason.trim()
            ),
            _ => anyhow!("{at}: {act} failed: {error}"),
        }
    }

    /// Send `from`, `size` bytes, to `path` in the location, on the way
    /// to `dest`, the path an error names.
    fn upload(&self, path: &str, dest: &str, from: &mut dyn Read, size: u64) -> Result<()> {
        let (mut writer, reader) = tokio::io::duplex(PIPE);
        let location = self.handle();
        let (sent, pumped) = thread::scope(|scope| {
            let sending =
                scope.spawn(|| self.runtime.block_on(location.write_stream(path, reader)));
            let pumped = self.pump(from, &mut writer);
            // The end of the pipe ends the upload, also after a read
            // error: what arrived then stays under the temporary name.
            drop(writer);
            (sending.join(), pumped)
        });
        let sent = sent.map_err(|_| anyhow!("{}: the upload stopped", self.target))?;
        sent.map_err(|error| {
            if status(&error) == Some(413) {
                anyhow!(
                    "{} {dest}: the server refuses a file of {}; raise its upload limit and sync again",
                    self.target,
                    human_bytes(size)
                )
            } else {
                self.failed("writing", dest, &error)
            }
        })?;
        let pumped = pumped.with_context(|| format!("{path}: reading the file here"))?;
        if pumped != size {
            bail!("{path}: the file here changed while it was read; sync again");
        }
        Ok(())
    }

    /// Read `from` into the pipe until it ends, and count the bytes.
    fn pump(&self, from: &mut dyn Read, writer: &mut DuplexStream) -> io::Result<u64> {
        let handle = self.runtime.handle();
        let mut buffer = vec![0; PIPE];
        let mut total = 0;
        loop {
            let read = from.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            handle.block_on(writer.write_all(&buffer[..read]))?;
            total += read as u64;
        }
        handle.block_on(writer.shutdown())?;
        Ok(total)
    }

    /// Rename `from` to `to` in the location.
    fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.runtime
            .block_on(self.handle().rename(from, to))
            .map_err(|error| self.failed("moving a file into place", to, &error))
    }
}

impl Remote for Filebridge {
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
        let full = self.full(path);
        match self.runtime.block_on(self.handle().read(&full)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if status(&error) == Some(404) => Ok(None),
            Err(error) => Err(self.failed("reading", &full, &error)),
        }
    }

    fn create(&self, path: &str, from: &mut dyn Read, size: u64) -> Result<Created> {
        let full = self.full(path);
        match self.runtime.block_on(self.handle().info(&full)) {
            Ok(_) => return Ok(Created::Known),
            Err(error) if status(&error) == Some(404) => {}
            Err(error) => return Err(self.failed("reading", &full, &error)),
        }
        let temp = self.full(&temp_path(path));
        self.upload(&temp, &full, from, size)?;
        self.rename(&temp, &full)?;
        Ok(Created::New)
    }

    fn replace(&self, path: &str, bytes: &[u8]) -> Result<()> {
        let full = self.full(path);
        let temp = self.full(&temp_path(path));
        self.runtime
            .block_on(self.handle().write(&temp, bytes, None))
            .map_err(|error| self.failed("writing", &temp, &error))?;
        self.rename(&temp, &full)
    }

    /// filebridge makes a directory only on the way to a file, so a file
    /// is written into the store's `tmp/` and removed again. The `tmp/`
    /// stays, where a store does not look.
    fn make_dir(&self, path: &str) -> Result<()> {
        let marker = self.full(&format!("{path}/tmp/ossuary-sync.tmp"));
        let location = self.handle();
        self.runtime
            .block_on(location.write(&marker, b"", None))
            .map_err(|error| self.failed("making the folder", path, &error))?;
        self.runtime
            .block_on(location.delete(&marker))
            .map_err(|error| self.failed("making the folder", path, &error))
    }
}

/// The HTTP status a request failed with, when the server answered.
fn status(error: &Error) -> Option<u16> {
    match error {
        Error::Api(status, _) => Some(status.as_u16()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_names_the_server_the_location_and_the_folder() {
        let (server, location, root) =
            split("filebridge+http://backup.example.com:8000/archive/ossuary/copy").unwrap();

        assert_eq!(server, "http://backup.example.com:8000");
        assert_eq!(location, "archive");
        assert_eq!(root, "ossuary/copy/");
    }

    #[test]
    fn a_location_alone_is_its_root() {
        let (server, location, root) =
            split("filebridge+https://backup.example.com/archive/").unwrap();

        assert_eq!(server, "https://backup.example.com");
        assert_eq!(location, "archive");
        assert_eq!(root, "");
    }

    #[test]
    fn a_target_without_a_location_is_refused() {
        let error = split("filebridge+http://backup.example.com:8000/").unwrap_err();

        assert!(error.to_string().contains("LOCATION/FOLDER"));
    }

    #[test]
    fn a_token_in_the_url_is_refused() {
        let error = split("filebridge+http://secret@backup.example.com:8000/archive").unwrap_err();

        assert!(error.to_string().contains(TOKEN));
    }
}
