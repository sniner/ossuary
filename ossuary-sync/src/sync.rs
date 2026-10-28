//! Bringing a copy of the archive up to a seal.
//!
//! The copy's open segment names the seal it holds. What it lacks to
//! hold this archive's seal is worked out from this archive's log alone
//! ([`ossuary_core::delta`]), so the copy is never listed. The copy's
//! open segment is written last, and only when everything before it
//! arrived: an interrupted run leaves it naming the seal it named, and
//! the next run works out the same delta and skips what is there.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use ossuary_core::{Archive, Claim, Contents, Delta, Digest, Store};
use serde_json::Value;

use crate::remote::{Created, Remote};

/// The archive's mark and open segment, by the names `docs/format.md`
/// gives them.
pub const MARK: &str = "FORMAT";
pub const HEAD: &str = "head.jsonl";

/// What the copy holds before the run.
#[derive(Debug, PartialEq, Eq)]
pub struct Held {
    /// Whether the copy has a `FORMAT` mark yet. A first run writes it
    /// before anything else, so a repeated first run finds the copy as
    /// an archive.
    pub marked: bool,
    /// The seal the copy's open segment names, when it has one.
    pub seal: Option<Digest>,
}

/// Read what the copy holds, and refuse a copy this archive cannot
/// bring up to its seal.
///
/// # Errors
///
/// When the copy cannot be read, its mark differs from this archive's,
/// or its open segment holds claims, names more than one segment, or
/// does not read: something other than a sync wrote it.
pub fn held(archive: &Archive, remote: &dyn Remote) -> Result<Held> {
    let ours = fs::read(archive.root().join(MARK))
        .with_context(|| format!("{}: reading {MARK}", archive.root().display()))?;
    let marked = match remote.read(MARK)? {
        None => false,
        Some(theirs) if mark(&theirs) == mark(&ours) && mark(&ours).is_some() => true,
        Some(theirs) => bail!(
            "the target's {MARK} is {}, this archive's is {}; a copy needs the same hash and layout, so name another target",
            first_line(&theirs),
            first_line(&ours)
        ),
    };
    let text = match remote.read(HEAD)? {
        None => return Ok(Held { marked, seal: None }),
        Some(bytes) if bytes.is_empty() => return Ok(Held { marked, seal: None }),
        Some(bytes) => String::from_utf8(bytes).map_err(|_| {
            anyhow::anyhow!("the target's open segment is not text; name another target")
        })?,
    };
    let head = Contents::parse(&text).map_err(|error| {
        anyhow::anyhow!(
            "the target's open segment cannot be read: {}; name another target",
            error.spelled()
        )
    })?;
    match head.previous() {
        [seal] if head.claims().is_empty() => Ok(Held {
            marked,
            seal: Some(seal.clone()),
        }),
        _ => bail!(
            "the target's open segment holds claims or names more than one segment, so something other than ossuary sync wrote to the target; name another target"
        ),
    }
}

/// The mark as JSON, so that spacing does not count.
fn mark(bytes: &[u8]) -> Option<Value> {
    let text = std::str::from_utf8(bytes).ok()?;
    serde_json::from_str(text.lines().next()?).ok()
}

fn first_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Which of the archive's stores a file lies in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Content,
    Derived,
    Claims,
}

impl Kind {
    fn store(self, archive: &Archive) -> &Store {
        match self {
            Kind::Content => archive.content(),
            Kind::Derived => archive.derived(),
            Kind::Claims => archive.log().store(),
        }
    }
}

/// One file as it lies here, and where it goes in the copy.
#[derive(Debug)]
pub struct Piece {
    kind: Kind,
    from: PathBuf,
    /// The path in the copy: the same as here, relative to the root.
    pub to: String,
    pub size: u64,
}

/// A file the copy lacks: every copy of it here, in `content/` and in
/// `derived/`.
#[derive(Debug)]
pub struct Lacked {
    pub digest: Digest,
    pub copies: Vec<Piece>,
}

/// What the copy lacks, located here.
#[derive(Debug)]
pub struct Plan {
    pub files: Vec<Lacked>,
    /// Files the claims refer to that neither store holds.
    pub gone: Vec<Digest>,
    pub segments: Vec<Piece>,
}

impl Plan {
    /// Bytes of the files to be copied, as they lie here. Segments are
    /// left out: a dry run cannot size the open segment before it is
    /// sealed, and the files are what takes the time.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        let copies = self.files.iter().flat_map(|file| &file.copies);
        copies.map(|piece| piece.size).sum()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.gone.is_empty() && self.segments.is_empty()
    }
}

/// Work out what a copy that holds `held` lacks to hold the record up to
/// `ahead` and the claims in `open`, and find each file here.
///
/// # Errors
///
/// When `held` is a point this record does not lead to, and when the log
/// or a store cannot be read.
pub fn plan(
    archive: &Archive,
    held: Option<&Digest>,
    ahead: &[Digest],
    open: &[Claim],
) -> Result<Plan> {
    let Delta {
        segments,
        files,
        follows,
    } = ossuary_core::delta(archive.log(), held, ahead, open)?;
    if !follows {
        // `follows` is false only with a seal held.
        let seal = held.map(Digest::as_str).unwrap_or_default();
        bail!(
            "the target holds the seal {seal}, which this archive's record does not lead to: the target is a copy of another archive, or something other than ossuary sync wrote to it; name another target"
        );
    }
    let mut plan = Plan {
        files: Vec::new(),
        gone: Vec::new(),
        segments: Vec::new(),
    };
    for digest in files {
        let mut copies = Vec::new();
        for kind in [Kind::Content, Kind::Derived] {
            if let Some(from) = kind.store(archive).find(&digest)? {
                copies.push(piece(archive, kind, from)?);
            }
        }
        if copies.is_empty() {
            plan.gone.push(digest);
        } else {
            plan.files.push(Lacked { digest, copies });
        }
    }
    for digest in segments {
        // `delta` lists only segments the store holds.
        let from = Kind::Claims
            .store(archive)
            .find(&digest)?
            .with_context(|| format!("segment {digest} is not in claims/"))?;
        plan.segments.push(piece(archive, Kind::Claims, from)?);
    }
    Ok(plan)
}

fn piece(archive: &Archive, kind: Kind, from: PathBuf) -> Result<Piece> {
    let size = fs::metadata(&from)
        .with_context(|| format!("{}: reading", from.display()))?
        .len();
    let to = relative(archive.root(), &from)?;
    Ok(Piece {
        kind,
        from,
        to,
        size,
    })
}

/// `path` relative to `root`, separated by `/`.
fn relative(root: &Path, path: &Path) -> Result<String> {
    let inside = path
        .strip_prefix(root)
        .with_context(|| format!("{}: not inside the archive", path.display()))?;
    let parts = inside
        .iter()
        .map(|part| part.to_str())
        .collect::<Option<Vec<_>>>()
        .with_context(|| format!("{}: not a name the archive gives", path.display()))?;
    Ok(parts.join("/"))
}

/// What copying the files came to.
#[derive(Debug, Default)]
pub struct Copied {
    /// Files written to the copy.
    pub new: usize,
    /// Files the copy held already, from an earlier run that did not
    /// finish.
    pub known: usize,
    /// Bytes written.
    pub bytes: u64,
    /// Files with no copy here that matches its name.
    pub damaged: Vec<String>,
    /// Files with no copy here that could be read, with the reason.
    pub unreadable: Vec<(String, String)>,
}

/// Give the copy what makes it an archive, before any file: the mark
/// first, so that a repeated first run finds an archive there, then the
/// store directories, which a reader of the copy opens even when they
/// are empty. Done on every run, since a first run may have stopped
/// between the two.
///
/// # Errors
///
/// When the mark cannot be read here, or the copy cannot be written.
pub fn prepare(archive: &Archive, remote: &dyn Remote, marked: bool) -> Result<()> {
    if !marked {
        let path = archive.root().join(MARK);
        let mut file = File::open(&path).with_context(|| format!("{}: reading", path.display()))?;
        let size = file
            .metadata()
            .with_context(|| format!("{}: reading", path.display()))?
            .len();
        remote.create(MARK, &mut file, size)?;
    }
    for store in [archive.content(), archive.derived(), archive.log().store()] {
        remote.make_dir(&relative(archive.root(), store.root())?)?;
    }
    Ok(())
}

/// Copy every file the plan lists, each copy checked against its name
/// first: `seal` reads segments, not files, and a damaged file would
/// otherwise reach the copy under a name its bytes do not have. A file
/// counts as copied when at least one of its copies here is sound.
///
/// # Errors
///
/// When the copy cannot be written, or a file here cannot be opened
/// after it was checked.
pub fn copy_files(
    archive: &Archive,
    remote: &dyn Remote,
    files: &[Lacked],
    mut done: impl FnMut(usize),
) -> Result<Copied> {
    let mut copied = Copied::default();
    for (index, file) in files.iter().enumerate() {
        let mut sound = false;
        let mut trouble = None;
        for copy in &file.copies {
            match check(archive, copy) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(reason) => {
                    trouble = Some(reason);
                    continue;
                }
            }
            sound = true;
            match send(remote, copy)? {
                Created::New => {
                    copied.new += 1;
                    copied.bytes += copy.size;
                }
                Created::Known => copied.known += 1,
            }
        }
        if !sound {
            let name = file.digest.as_str().to_string();
            match trouble {
                Some(reason) => copied.unreadable.push((name, reason)),
                None => copied.damaged.push(name),
            }
        }
        done(index + 1);
    }
    Ok(copied)
}

/// Copy the segments the plan lists, each one checked against its name.
///
/// # Errors
///
/// When a segment is damaged or cannot be read: `seal` checked every
/// segment a moment ago, so either is a fault of this run, not a
/// finding. And when the copy cannot be written.
pub fn copy_segments(archive: &Archive, remote: &dyn Remote, segments: &[Piece]) -> Result<()> {
    for segment in segments {
        match check(archive, segment) {
            Ok(true) => {}
            Ok(false) => bail!("{}: damaged", segment.from.display()),
            Err(reason) => bail!("{}: {reason}", segment.from.display()),
        }
        send(remote, segment)?;
    }
    Ok(())
}

/// Write the copy's open segment, naming `seal`. This is what makes the
/// copy hold the seal, so it comes after everything the seal covers.
///
/// # Errors
///
/// When the copy cannot be written.
pub fn advance(remote: &dyn Remote, seal: &Digest) -> Result<()> {
    remote.replace(HEAD, ossuary_core::head_after(seal).as_bytes())
}

/// Whether one file here matches its name: `Ok(false)` when it does not,
/// the reason when it cannot be read.
fn check(archive: &Archive, piece: &Piece) -> std::result::Result<bool, String> {
    let store = piece.kind.store(archive);
    let entry = store
        .entry_at(&piece.from)
        .ok_or_else(|| "not a file the archive gives that name".to_string())?;
    store.verify(&entry).map_err(|error| error.to_string())
}

/// The file as it lies here, compressed or not, under the same path.
fn send(remote: &dyn Remote, piece: &Piece) -> Result<Created> {
    let mut file =
        File::open(&piece.from).with_context(|| format!("{}: reading", piece.from.display()))?;
    remote.create(&piece.to, &mut file, piece.size)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use ossuary_core::{Algorithm, Attribute, Run, Source, Subject, Timestamp};
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::directory::Directory;

    fn archive(dir: &TempDir) -> Archive {
        Archive::create(dir.path().join("archive"), Algorithm::Sha256).unwrap()
    }

    fn target(dir: &TempDir) -> (PathBuf, Directory) {
        let path = dir.path().join("copy");
        fs::create_dir(&path).unwrap();
        let target = Directory::open(&path).unwrap();
        (path, target)
    }

    fn take(archive: &Archive, bytes: &[u8], at: &str) -> Digest {
        let (_, entry) = archive.content().add(bytes).unwrap();
        let claim = Claim::assert(
            Subject::parse(entry.digest().as_str()).unwrap(),
            Attribute::parse("file:size").unwrap(),
            json!(bytes.len()),
            Timestamp::parse(at).unwrap(),
            Source::parse("test").unwrap(),
            Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        )
        .unwrap();
        archive.log().append(&claim).unwrap();
        entry.digest().clone()
    }

    /// One whole run, as `main` puts it together.
    fn sync(archive: &Archive, remote: &dyn Remote) -> Result<(Plan, Copied)> {
        let seal = seal_or_last(archive);
        let held = held(archive, remote)?;
        prepare(archive, remote, held.marked)?;
        let plan = plan(
            archive,
            held.seal.as_ref(),
            std::slice::from_ref(&seal),
            &[],
        )?;
        let copied = copy_files(archive, remote, &plan.files, |_| {})?;
        if plan.gone.is_empty() && copied.damaged.is_empty() && copied.unreadable.is_empty() {
            copy_segments(archive, remote, &plan.segments)?;
            advance(remote, &seal)?;
        }
        Ok((plan, copied))
    }

    fn seal_or_last(archive: &Archive) -> Digest {
        match archive.log().seal().unwrap() {
            Some(segment) => segment.digest().clone(),
            None => archive.log().head_contents().unwrap().previous()[0].clone(),
        }
    }

    #[test]
    fn a_first_run_makes_a_copy_that_holds_the_seal() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        take(&archive, b"second", "2026-09-02T00:00:00Z");
        let (path, target) = target(&dir);

        let (plan, copied) = sync(&archive, &target).unwrap();

        assert_eq!(plan.files.len(), 2);
        assert_eq!(copied.new, 2);
        let copy = Archive::open(&path).unwrap();
        let seal = archive.log().head_contents().unwrap().previous()[0].clone();
        let cover = ossuary_core::cover(&copy, &seal).unwrap();
        let files = ossuary_core::files(&copy, &cover, true).unwrap();
        let since = ossuary_core::since(&copy, &cover).unwrap();
        assert!(cover.held && cover.gone.is_empty() && cover.damaged.is_empty());
        assert!(files.gone.is_empty() && files.damaged.is_empty());
        assert_eq!(since.continued, ossuary_core::Continued::Yes);
        assert!(copy.root().join("cache").read_dir().is_err(), "no cache/");
        let derived = ossuary_core::audit_store(copy.derived()).unwrap();
        assert_eq!(derived.checked, 0, "an empty derived/ is there to be read");
    }

    #[test]
    fn a_later_run_sends_only_what_came_after() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let (_, target) = target(&dir);
        sync(&archive, &target).unwrap();
        let second = take(&archive, b"second", "2026-09-02T00:00:00Z");

        let (plan, copied) = sync(&archive, &target).unwrap();

        let sent: Vec<_> = plan.files.iter().map(|file| &file.digest).collect();
        assert_eq!(sent, [&second]);
        assert_eq!(plan.segments.len(), 1);
        assert_eq!((copied.new, copied.known), (1, 0));
    }

    #[test]
    fn a_run_with_nothing_new_has_nothing_to_send() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let (_, target) = target(&dir);
        sync(&archive, &target).unwrap();

        let (plan, _) = sync(&archive, &target).unwrap();

        assert!(plan.is_empty());
    }

    #[test]
    fn a_missing_file_leaves_the_copy_at_the_seal_it_held() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let (path, target) = target(&dir);
        sync(&archive, &target).unwrap();
        let before = fs::read(path.join(HEAD)).unwrap();
        let second = take(&archive, b"second", "2026-09-02T00:00:00Z");
        take(&archive, b"third", "2026-09-02T00:00:01Z");
        let lost = archive.content().find(&second).unwrap().unwrap();
        fs::remove_file(lost).unwrap();

        let (plan, copied) = sync(&archive, &target).unwrap();

        assert_eq!(plan.gone, [second]);
        assert_eq!(copied.new, 1, "the other file is copied");
        assert_eq!(fs::read(path.join(HEAD)).unwrap(), before);
        let copy = Archive::open(&path).unwrap();
        assert_eq!(
            copy.log().store().entries().count(),
            1,
            "no segment the copy's head does not reach"
        );
    }

    /// A place that fails after a number of files, as a dropped
    /// connection does.
    struct Failing<'a> {
        inner: &'a Directory,
        left: Mutex<usize>,
    }

    impl Remote for Failing<'_> {
        fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
            self.inner.read(path)
        }

        fn create(&self, path: &str, from: &mut dyn std::io::Read, size: u64) -> Result<Created> {
            let mut left = self.left.lock().unwrap();
            if *left == 0 {
                bail!("connection lost");
            }
            *left -= 1;
            self.inner.create(path, from, size)
        }

        fn replace(&self, path: &str, bytes: &[u8]) -> Result<()> {
            self.inner.replace(path, bytes)
        }

        fn make_dir(&self, path: &str) -> Result<()> {
            self.inner.make_dir(path)
        }
    }

    #[test]
    fn an_interrupted_run_is_completed_by_the_next() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        for (index, bytes) in [&b"one"[..], b"two", b"three"].iter().enumerate() {
            take(
                &archive,
                bytes,
                &format!("2026-09-0{}T00:00:00Z", index + 1),
            );
        }
        let (path, target) = target(&dir);
        let failing = Failing {
            inner: &target,
            left: Mutex::new(3),
        };

        assert!(sync(&archive, &failing).is_err());
        assert!(!path.join(HEAD).exists(), "the copy holds no seal yet");

        let (_, copied) = sync(&archive, &target).unwrap();

        assert_eq!((copied.new, copied.known), (1, 2));
        assert!(path.join(HEAD).exists());
    }

    #[test]
    fn a_copy_of_another_archive_is_refused() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let (_, target) = target(&dir);
        let other_dir = TempDir::new().unwrap();
        let other = Archive::create(other_dir.path().join("other"), Algorithm::Sha256).unwrap();
        take(&other, b"theirs", "2026-09-01T00:00:00Z");
        sync(&other, &target).unwrap();

        let error = sync(&archive, &target).unwrap_err();

        assert!(error.to_string().contains("does not lead to"));
    }

    #[test]
    fn a_copy_with_another_mark_is_refused() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let (path, target) = target(&dir);
        fs::write(
            path.join(MARK),
            r#"{"ossuary-archive": 1, "algorithm": "blake3", "content-depth": 2, "derived-depth": 2, "claims-depth": 1}"#,
        )
        .unwrap();

        let error = sync(&archive, &target).unwrap_err();

        assert!(error.to_string().contains("same hash and layout"));
    }

    #[test]
    fn a_copy_someone_wrote_claims_into_is_refused() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let (path, target) = target(&dir);
        sync(&archive, &target).unwrap();
        let copy = Archive::open(&path).unwrap();
        take(&copy, b"written into the copy", "2026-09-02T00:00:00Z");

        let error = held(&archive, &target).unwrap_err();

        assert!(error.to_string().contains("holds claims"));
    }

    #[test]
    fn a_file_in_both_stores_is_sent_from_both() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let first = take(&archive, b"first", "2026-09-01T00:00:00Z");
        archive.derived().add(b"first").unwrap();
        let (path, target) = target(&dir);

        let (_, copied) = sync(&archive, &target).unwrap();

        assert_eq!(copied.new, 2);
        let copy = Archive::open(&path).unwrap();
        assert!(copy.content().contains(&first).unwrap());
        assert!(copy.derived().contains(&first).unwrap());
    }

    #[test]
    fn a_copy_on_a_webdav_server_holds_the_seal() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first", "2026-09-01T00:00:00Z");
        let (path, _) = target(&dir);
        let server = crate::webdav::fake::Server::start(&path, None);
        let target =
            crate::webdav::WebDav::open(&server.url, None, crate::webdav::Certificate::Check)
                .unwrap();
        sync(&archive, &target).unwrap();
        take(&archive, b"second", "2026-09-02T00:00:00Z");

        let (plan, copied) = sync(&archive, &target).unwrap();

        assert_eq!((plan.files.len(), copied.new), (1, 1));
        let copy = Archive::open(&path).unwrap();
        let seal = archive.log().head_contents().unwrap().previous()[0].clone();
        let cover = ossuary_core::cover(&copy, &seal).unwrap();
        let files = ossuary_core::files(&copy, &cover, true).unwrap();
        assert!(cover.held && cover.gone.is_empty() && cover.damaged.is_empty());
        assert!(files.gone.is_empty() && files.damaged.is_empty());
        for store in ["content", "derived", "claims"] {
            let temp = path.join(store).join("tmp");
            let left = temp.read_dir().map_or(0, Iterator::count);
            assert_eq!(left, 0, "nothing left in {store}/tmp");
        }
    }

    #[test]
    fn a_damaged_file_is_not_sent() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let first = take(&archive, b"first", "2026-09-01T00:00:00Z");
        let path = archive.content().find(&first).unwrap().unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        #[allow(
            clippy::permissions_set_readonly_false,
            reason = "the test plays the corruption"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(&path, permissions).unwrap();
        fs::write(&path, b"fir5t").unwrap();
        let (copy, target) = target(&dir);

        let (_, copied) = sync(&archive, &target).unwrap();

        assert_eq!(copied.damaged, [first.as_str().to_string()]);
        assert_eq!(copied.new, 0);
        assert!(!copy.join(HEAD).exists());
    }
}
