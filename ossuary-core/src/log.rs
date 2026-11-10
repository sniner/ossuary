//! The log: where claims accumulate, and how segments come to be.
//!
//! Claims land in the open segment: a directory under `open/` in the
//! archive root that holds one file per append, a bundle of claim lines
//! named by its own digest. A bundle is written once and never opened for writing again,
//! like everything else in the archive. Sealing closes the open segment:
//! the bundles' lines go into one segment file in the claims store, sorted
//! by time and then by the line itself, and the directory becomes the next
//! open segment. A sealed segment is never compacted, combined or rewritten:
//! superseded and retracted claims stay where they were written, which for
//! an archive is not a limitation but the point. An append that finds
//! [`SEAL_AT`] bundles or more, or that has itself appended
//! [`SEAL_AT_BYTES`] since the last seal it saw, seals first; sealing by
//! hand remains for closing the segment on demand.
//!
//! Segments hang together. The directory's name is the segment sealed
//! last, `open/<S>`, and the segment sealed from it names S in its header
//! as `previous`: every segment but the first names the one before it, and
//! the open segment names the last. The segments form one chain, and the
//! chain is the order of the record between segments. Within a segment the
//! order carries no meaning: a retraction names the claims it retracts by
//! id, so it means the same wherever it stands. A sealed segment that goes
//! missing leaves a name nothing answers to, in the header of its
//! successor or of the open segment. The walk back from the open segment
//! stops there, and the readers here refuse rather than read around it;
//! repairing the chain rewrites segments, which only `ossuary-fix` does.
//!
//! Several writers, several machines, no lock. The open segment is one
//! directory for the life of the archive; it is never created again, only
//! renamed, and every rename is atomic and won by one process:
//!
//! 1. `open/<S>` becomes `open/<S>.sealing`. A writer whose directory was
//!    renamed away finds its root gone (the bundle store never creates
//!    it) and looks for the open segment again. The set is not frozen by
//!    this: a writer whose rename into `open/<S>/` had already resolved
//!    the directory lands its bundle in `<S>.sealing` after the listing.
//!    Such a late bundle is simply not part of this seal.
//! 2. Whoever finds `<S>.sealing` lists it, reads every bundle it listed,
//!    builds the segment and writes its bytes into the directory under a
//!    name of its own. Nothing is deleted yet.
//! 3. `<S>.sealing` becomes `<S>.sealed-<S'>`, S' being the digest of the
//!    segment the renaming process built. Only one rename wins, so one
//!    segment is decided; a finisher that built another one loses the
//!    rename and stops.
//! 4. The winner stores its segment file. Whoever finds a `.sealed-`
//!    directory whose segment is not in the store stores the segment file
//!    with that digest from the directory; only after the store holds S'
//!    is anything deleted.
//! 5. The bundles whose lines are all in S' are deleted, every segment
//!    file too, and what is in `tmp/`; the directory is renamed to
//!    `open/<S'>`. A late bundle stays and goes into the next seal.
//!
//! Before the first seal the directory is `open/first`. `open/` itself is
//! made once and never renamed; it holds the one directory of the open
//! segment, whatever its name, and nothing else.
//!
//! `tmp/` stays, so that the directory is never empty: macOS over SMB
//! replaces an existing empty directory instead of failing, and two
//! processes renaming to the same name at once would delete the winner's
//! directory. A writer that finds no open segment waits a moment for the
//! seal under way to produce one before it finishes the seal itself: the
//! sealer is usually alive and a few steps ahead, and a helper storing
//! alongside it is a race the store has to defend against. A reader waits
//! as long and then reads the directory as it stands, whatever its name;
//! it changes nothing, so a seal that stopped in an archive nobody can
//! write to does not keep the archive from being read. A listing that
//! shows none of the names at all is repeated before the open segment
//! counts as lost: `readdir` is not atomic, and a rename can fall into it.
//! A missing open segment is never begun anew here; `ossuary-fix chain`
//! does that, alone.
//!
//! What the chain cannot show is a loss at its very end: an open segment
//! and the segments after some point removed, the rest intact. A seal kept
//! outside the archive shows it (see `seal`).
//!
//! An append is flushed to the device before it returns, so a claim that
//! an append reported as written survives a crash. Everything sealed is
//! durable the way the store promises. Reading is strict: a line that does
//! not read as a claim stops the reader rather than being quietly dropped.

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead as _, ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use immure::{Algorithm, Digest, Store};
use serde::{Deserialize, Serialize};

use crate::claim::{Claim, ClaimId};
use crate::error::{Error, Result};

/// The format generation this build writes, and the only one it reads.
///
/// Generation 1 is `docs/format.md`. A segment declaring any other is
/// refused as [`Error::SegmentGeneration`] rather than guessed at.
pub const GENERATION: u32 = 1;

/// The open segment is sealed by the first append that finds this many
/// bundles in it or more.
///
/// A bundle is one append: a file's claims from an ingest, a message's
/// from a fetch, an extractor's findings. A thousand of them is about the
/// size of segment the concept asks for, and a listing of a thousand
/// names is what every append pays to know. When to seal is the
/// software's business (`docs/format.md` says so in words), so this is a
/// constant, not a knob; [`seal`](Log::seal) stays alongside as the
/// on-demand grip.
pub const SEAL_AT: usize = 1000;

/// The open segment is sealed by an append once the appends of this
/// process since the last seal it saw add up to this many bytes.
///
/// A count of bundles is blind to their size: a caller that passes
/// thousands of claims to one append makes one large bundle, and a
/// thousand of those would be an open segment every reader folds in
/// whole. The size of the directory is not asked for, which would be a
/// `stat` per bundle; each writer knows what it wrote itself, so each
/// bounds its own share, and the open segment stays below writers times
/// this plus the count above.
pub const SEAL_AT_BYTES: u64 = 1 << 20;

/// How long a writer or reader without an open segment waits for the seal
/// under way to produce one, before a writer finishes the seal itself and
/// a reader gives up.
const HELP_AFTER: Duration = Duration::from_secs(2);

/// How often a listing that shows no open segment is repeated, and how
/// long apart, before the open segment counts as lost.
const LISTING_TRIES: usize = 3;
const LISTING_DELAY: Duration = Duration::from_millis(100);

/// How often a directory rename refused with EACCES or EBUSY is tried
/// again, and how long apart. gvfs refuses while it holds handles inside,
/// which it does for a while after a listing.
const RENAME_TRIES: usize = 20;
const RENAME_DELAY: Duration = Duration::from_millis(100);

/// The directory in the archive root that holds the open segment's
/// directory.
const OPEN_DIR: &str = "open";
/// The open segment before the first seal, which has no segment to be
/// named after.
const FIRST: &str = "first";
const SEALING_SUFFIX: &str = ".sealing";
const SEALED_INFIX: &str = ".sealed-";
const BUNDLE_SUFFIX: &str = ".jsonl";
const SEGMENT_FILE: &str = "segment-";
const TMP: &str = "tmp";

/// The open segment of the form before 0.18, in the archive root.
pub const OLD_HEAD: &str = "head.jsonl";

/// The directories of the form of 0.18 in the archive root, where the
/// open segment lay as `open-<S>/` (`open/` before the first seal, with
/// `tmp/` in it), `sealing-<S>/` or `sealed-<S>-<S'>/`. Nothing here
/// reads them; `ossuary-fix open` moves them. Empty when the root cannot
/// be listed.
#[must_use]
pub fn old_open_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(listing) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    for entry in listing.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let old = (name == OPEN_DIR && entry.path().join(TMP).is_dir())
            || name.starts_with("open-")
            || name == "sealing"
            || name.starts_with("sealing-")
            || name.starts_with("sealed-");
        if old && entry.path().is_dir() {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    dirs
}

/// The first line of every sealed segment: a segment names its own format
/// before anything else, so that a stray file found alone in fifty years
/// still says what it is — and, from the second segment on, the segment
/// sealed before it.
///
/// Members this build does not know are skipped, not refused: generation
/// 1 may gain header members that add to what is known without changing
/// how the claims after the header are read. `docs/format.md` says so, and
/// a member that a reader *must* understand is what a new generation is
/// for.
#[derive(Debug, Serialize, Deserialize)]
struct Header {
    #[serde(rename = "ossuary-segment")]
    generation: u32,
    /// The digest of the segment sealed before this one, as the claims
    /// store files it: bare hex. Absent from a first segment. Read as
    /// any JSON value, so that the list ossuary 0.14 and earlier wrote
    /// is refused as what it is, not as a broken header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous: Option<serde_json::Value>,
}

/// The generation alone, read leniently; see [`parse_header`] for why the
/// strict read comes second.
#[derive(Debug, Deserialize)]
struct Generation {
    #[serde(rename = "ossuary-segment")]
    generation: u32,
}

/// The header as it stands in a file, newline included: the generation,
/// and the segment sealed before this one when there is one.
fn header_line(previous: Option<&Digest>) -> String {
    let header = serde_json::to_string(&Header {
        generation: GENERATION,
        previous: previous.map(|digest| serde_json::Value::from(digest.as_str())),
    });
    // A struct of one number serialises; see `Claim::to_line` for the
    // reasoning behind not pretending otherwise.
    let mut line = header.expect("a header serialises");
    line.push('\n');
    line
}

/// A sealed segment, by its name in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    digest: Digest,
}

impl Segment {
    /// The entry the segment lies under in the claims store.
    #[must_use]
    pub fn digest(&self) -> &Digest {
        &self.digest
    }
}

/// A segment read back whole: the segment it follows, and its claims.
#[derive(Debug, Clone, PartialEq)]
pub struct Contents {
    previous: Option<Digest>,
    claims: Vec<Claim>,
}

impl Contents {
    /// Read a sealed segment from its text as a file holds it, header
    /// first, the claims' ids under `algorithm`.
    ///
    /// # Errors
    ///
    /// [`Error::SegmentHeader`], [`Error::PreviousList`] and
    /// [`Error::SegmentGeneration`] for a header that does not read or
    /// is in another form, and [`Error::BadLine`] naming the first claim
    /// that does not read.
    pub fn parse(text: &str, algorithm: Algorithm) -> Result<Contents> {
        parse_segment(text, algorithm)
    }

    /// The segment sealed before this one, as its header names it;
    /// `None` for a first segment.
    #[must_use]
    pub fn previous(&self) -> Option<&Digest> {
        self.previous.as_ref()
    }

    /// The claims, in the order recorded.
    #[must_use]
    pub fn claims(&self) -> &[Claim] {
        &self.claims
    }

    /// The claims alone, the header's word left behind.
    #[must_use]
    pub fn into_claims(self) -> Vec<Claim> {
        self.claims
    }
}

/// The open segment as a listing: which segment it follows, and the
/// bundles it holds at this moment. What the index reads from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Open {
    dir: PathBuf,
    previous: Option<Digest>,
    bundles: Vec<Digest>,
}

impl Open {
    /// The last sealed segment, as the directory's name carries it;
    /// `None` before the first seal.
    #[must_use]
    pub fn previous(&self) -> Option<&Digest> {
        self.previous.as_ref()
    }

    /// The bundles the listing showed, by digest, in digest order.
    #[must_use]
    pub fn bundles(&self) -> &[Digest] {
        &self.bundles
    }
}

/// The walk back along the chain, from the segment the open segment names
/// to the first segment: one header read per step. See
/// [`Log::walk_back`].
#[derive(Debug)]
pub struct WalkBack<'a> {
    log: &'a Log,
    /// The next segment to read, and who named it, for the error when
    /// the store does not hold it.
    next: Option<(String, Digest)>,
}

impl Iterator for WalkBack<'_> {
    type Item = Result<Segment>;

    fn next(&mut self) -> Option<Result<Segment>> {
        let (named_by, digest) = self.next.take()?;
        match self.log.previous(&digest) {
            Ok(previous) => {
                self.next = previous.map(|previous| (format!("segment {digest}"), previous));
                Some(Ok(Segment { digest }))
            }
            Err(Error::SegmentMissing(_)) => Some(Err(Error::ChainBroken {
                segment: named_by,
                missing: digest.to_string(),
            })),
            Err(error) => Some(Err(error)),
        }
    }
}

/// What [`Log::writable`] had to do before a run may write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum Writable {
    /// The log takes claims as it is.
    Ready,
    /// A seal had stopped before the open segment was back in place, and
    /// it was finished. Nothing was lost.
    SealFinished,
}

impl Writable {
    /// What a run says about readying the log, or `None` when there was
    /// nothing to do.
    #[must_use]
    pub fn note(self) -> Option<&'static str> {
        match self {
            Writable::Ready => None,
            Writable::SealFinished => Some(
                "a seal had stopped before the open segment was back; finished it, nothing was lost",
            ),
        }
    }
}

/// What the archive root shows of the open segment.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Found {
    /// The open segment, under this directory.
    Open(PathBuf),
    /// No open segment, but a seal under way or stopped: the directory
    /// is `<S>.sealing` or `<S>.sealed-<S'>`.
    Pending {
        sealing: Vec<PathBuf>,
        sealed: Vec<PathBuf>,
    },
    /// None of the names.
    Nothing,
}

/// The claim log: one open segment in front, sealed segments behind it.
///
/// A `Log` is a description — a store handle and the archive root — and
/// describing is not creating: [`init`](Log::init) makes the open
/// segment, once, for a new archive.
#[derive(Debug)]
pub struct Log {
    store: Store,
    root: PathBuf,
    written: Mutex<Written>,
}

/// What this process appended to one open segment directory: the bytes
/// behind [`SEAL_AT_BYTES`]. A different directory, after a seal, starts
/// the count anew.
#[derive(Debug, Default)]
struct Written {
    dir: PathBuf,
    bytes: u64,
}

impl Log {
    /// The log kept in this claims store, with its open segment in the
    /// archive root `root`.
    #[must_use]
    pub fn new(store: Store, root: impl Into<PathBuf>) -> Self {
        Log {
            store,
            root: root.into(),
            written: Mutex::default(),
        }
    }

    /// The archive root the open segment lies in.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `open/`, the directory that holds the open segment's directory.
    fn open_root(&self) -> PathBuf {
        self.root.join(OPEN_DIR)
    }

    /// The claims store itself: for the audit and for repair tools that
    /// read every sealed segment as the store holds it.
    #[must_use]
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Begin the open segment of a new archive: `open/first/`, with its
    /// `tmp/`. The one place that creates it; from here on it is only
    /// renamed.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the directory cannot be made, which includes
    /// one already standing.
    pub fn init(&self) -> Result<()> {
        self.begin(None)
    }

    /// Begin an open segment after `previous`, for `ossuary-fix` when it
    /// has rewritten the record: `open/<S>/` with its `tmp/`, made with
    /// `mkdir`, so that a second one fails. `open/` itself is made when
    /// it is missing.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the directory cannot be made, or already stands.
    pub fn begin(&self, previous: Option<&Digest>) -> Result<()> {
        let parent = self.open_root();
        match fs::create_dir(&parent) {
            Ok(()) => sync_dir(&self.root),
            Err(source) if source.kind() == ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(Error::Io {
                    context: format!("{}: making", parent.display()),
                    source,
                });
            }
        }
        let dir = parent.join(open_name(previous));
        let io = |context: &str| {
            let context = format!("{}: {context}", dir.display());
            move |source| Error::Io { context, source }
        };
        fs::create_dir(&dir).map_err(io("beginning the open segment"))?;
        fs::create_dir(dir.join(TMP)).map_err(io("making tmp/"))?;
        sync_dir(&parent);
        Ok(())
    }

    /// Remove every directory of the open segment, whatever state it is
    /// in: for `ossuary-fix` before it begins one anew. Bundles in them
    /// go with them; `open/` itself stays.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when `open/` cannot be listed or a directory not
    /// removed.
    pub fn discard_open(&self) -> Result<()> {
        for dir in self.list()? {
            fs::remove_dir_all(&dir).map_err(|source| Error::Io {
                context: format!("{}: removing", dir.display()),
                source,
            })?;
        }
        Ok(())
    }

    /// Append one claim to the open segment, as [`append_all`](Log::append_all)
    /// does for several.
    ///
    /// # Errors
    ///
    /// As [`append_all`](Log::append_all).
    pub fn append(&self, claim: &Claim) -> Result<()> {
        self.append_all(std::slice::from_ref(claim))
    }

    /// Append claims to the open segment, as one bundle: a file named by
    /// the digest of its lines, flushed to the device before this
    /// returns. Each call costs one file, so a caller with many claims
    /// passes them together.
    ///
    /// An open segment at [`SEAL_AT`] bundles or more, or one this
    /// process has appended [`SEAL_AT_BYTES`] to, is sealed before
    /// anything is written, so no writer needs a sealing policy of its
    /// own. The order is what makes an error mean what it says: a seal
    /// that fails leaves the claims unwritten, and the caller can treat
    /// them as not recorded.
    ///
    /// # Errors
    ///
    /// [`Error::HeadLost`] when there is no open segment, [`Error::Io`]
    /// when the file cannot be written or flushed, and everything
    /// [`seal`](Log::seal) can answer when the open segment was due to
    /// be sealed.
    pub fn append_all(&self, claims: &[Claim]) -> Result<()> {
        if claims.is_empty() {
            return Ok(());
        }
        let mut text = String::new();
        for claim in claims {
            text.push_str(&claim.to_line());
            text.push('\n');
        }
        loop {
            let dir = self.open_dir_to_write()?;
            let bundles = self.bundle_store(&dir);
            let count = match bundles.entries().count_entries() {
                Ok(count) => count,
                Err(immure::Error::RootMissing(_)) => continue,
                Err(error) => return Err(error.into()),
            };
            if count >= SEAL_AT || self.written_to(&dir) >= SEAL_AT_BYTES {
                self.seal()?;
                self.forget_written();
                continue;
            }
            match bundles.add(text.as_bytes()) {
                Ok(_) => {
                    self.note_written(&dir, text.len() as u64);
                    return Ok(());
                }
                // Sealed since the count; the next round writes to the
                // new open segment.
                Err(immure::Error::RootMissing(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// What this process has appended to `dir`; nothing, when its last
    /// append went elsewhere.
    fn written_to(&self, dir: &Path) -> u64 {
        let written = self.written.lock().unwrap_or_else(PoisonError::into_inner);
        if written.dir == dir { written.bytes } else { 0 }
    }

    fn note_written(&self, dir: &Path, bytes: u64) {
        let mut written = self.written.lock().unwrap_or_else(PoisonError::into_inner);
        if written.dir != dir {
            written.dir = dir.to_path_buf();
            written.bytes = 0;
        }
        written.bytes += bytes;
    }

    /// After a seal this process asked for, whatever came of it: the
    /// count starts anew, so a seal that was not needed is not asked for
    /// again at once.
    fn forget_written(&self) {
        let mut written = self.written.lock().unwrap_or_else(PoisonError::into_inner);
        *written = Written::default();
    }

    /// The claims of the open segment, in the order a seal would give
    /// them: by time, then by the line itself. An archive with no open
    /// segment at all and nothing sealed holds none — that is an empty
    /// log, not an error.
    ///
    /// # Errors
    ///
    /// Everything [`head_contents`](Log::head_contents) can answer.
    pub fn head(&self) -> Result<Vec<Claim>> {
        self.head_contents().map(Contents::into_claims)
    }

    /// The open segment whole: the last sealed segment as its name
    /// carries it, and the claims of its bundles.
    ///
    /// # Errors
    ///
    /// Everything [`open`](Log::open) and [`bundle`](Log::bundle) can
    /// answer.
    pub fn head_contents(&self) -> Result<Contents> {
        let Some(open) = self.open()? else {
            return Ok(Contents {
                previous: None,
                claims: Vec::new(),
            });
        };
        let mut claims = Vec::new();
        for digest in &open.bundles {
            if let Some(bundle) = self.bundle(&open, digest)? {
                claims.extend(bundle);
            }
        }
        sort_as_sealed(&mut claims);
        Ok(Contents {
            previous: open.previous,
            claims,
        })
    }

    /// The open segment as a listing, for a reader that keeps track of
    /// which bundles it has read: the index. `None` when the archive has
    /// no open segment and nothing sealed, the empty log.
    ///
    /// While a seal is under way, the open segment is gone for a moment;
    /// this waits for it. A seal that stays stopped is read as it stands,
    /// the way `docs/format.md` describes it for a reader, so an archive
    /// that nobody can write to (a snapshot, a read-only copy) still
    /// reads:
    ///
    /// - `<S>.sealing`: nothing is decided. The open segment follows S,
    ///   and every bundle in the directory is unsealed.
    /// - `<S>.sealed-<S'>` with S' in the claims store: the open segment
    ///   follows S', and the bundles whose lines are not all in S' are
    ///   unsealed.
    /// - `<S>.sealed-<S'>` with S' only as a segment file in the
    ///   directory: the seal can still be finished, and until then the
    ///   record is S and every bundle.
    /// - `<S>.sealed-<S'>` with S' nowhere: the record names a segment
    ///   that is lost. The open segment follows S', and the walk back
    ///   from it reports the loss.
    ///
    /// # Errors
    ///
    /// [`Error::HeadLost`] when there is no open segment over sealed
    /// segments, [`Error::TwoOpen`] when two open segments stand or more
    /// than one directory of a seal, [`Error::Io`] when the root cannot
    /// be listed, and everything reading a decided segment can answer.
    pub fn open(&self) -> Result<Option<Open>> {
        let started = Instant::now();
        loop {
            match self.find()? {
                Found::Open(dir) => {
                    let previous = digest_in_name(&dir);
                    let bundles = match self.bundle_store(&dir).entries().digests() {
                        Ok(bundles) => bundles,
                        Err(immure::Error::RootMissing(_)) => continue,
                        Err(error) => return Err(error.into()),
                    };
                    return Ok(Some(Open {
                        dir,
                        previous,
                        bundles,
                    }));
                }
                Found::Pending { sealing, sealed } => {
                    if started.elapsed() < HELP_AFTER {
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                    // `None`: renamed on since the listing.
                    if let Some(open) = self.open_pending(&sealing, &sealed)? {
                        return Ok(Some(open));
                    }
                }
                Found::Nothing => {
                    if self.find_again()? != Found::Nothing {
                        continue;
                    }
                    if self.holds_segments()? {
                        return Err(Error::HeadLost(self.root.clone()));
                    }
                    return Ok(None);
                }
            }
        }
    }

    /// The open segment of a seal that stays stopped, for a reader; the
    /// four cases are in [`open`](Log::open). `None` when the directory
    /// was renamed since it was listed.
    fn open_pending(&self, sealing: &[PathBuf], sealed: &[PathBuf]) -> Result<Option<Open>> {
        let (dir, decided) = match (sealing, sealed) {
            ([dir], []) => (dir, None),
            ([], [dir]) => (dir, digest_in_name(dir)),
            _ => return Err(Error::TwoOpen(self.root.clone())),
        };
        let store = self.bundle_store(dir);
        let listed = match store.entries().digests() {
            Ok(listed) => listed,
            Err(immure::Error::RootMissing(_)) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let before = predecessor_in_name(dir);
        let (previous, bundles) = match decided {
            None => (before, listed),
            Some(segment) if self.store.contains(&segment)? => {
                let (_, unsealed) = self.sealed_among(&store, listed, &segment)?;
                (Some(segment), unsealed)
            }
            Some(segment) if self.segment_file(dir, &segment).is_some() => (before, listed),
            Some(segment) => (Some(segment), listed),
        };
        Ok(Some(Open {
            dir: dir.clone(),
            previous,
            bundles,
        }))
    }

    /// The bundles `names` of a directory split by the decided segment:
    /// those whose lines are all in it, and the rest. A bundle that will
    /// not read is among the rest.
    fn sealed_among(
        &self,
        bundles: &Store,
        names: Vec<Digest>,
        segment: &Digest,
    ) -> Result<(Vec<Digest>, Vec<Digest>)> {
        let held: BTreeSet<ClaimId> = self
            .contents(segment)?
            .into_claims()
            .into_iter()
            .filter_map(|claim| claim.id().cloned())
            .collect();
        let mut sealed = Vec::new();
        let mut rest = Vec::new();
        for name in names {
            let Ok(Some(bytes)) = bundles.read(&name) else {
                rest.push(name);
                continue;
            };
            let text = String::from_utf8(bytes).map_err(|_| Error::NotText)?;
            let claims = parse_bundle(&text, self.store.algorithm())?;
            if claims
                .iter()
                .all(|claim| claim.id().is_some_and(|id| held.contains(id)))
            {
                sealed.push(name);
            } else {
                rest.push(name);
            }
        }
        Ok((sealed, rest))
    }

    /// One bundle of the open segment, its claims with their ids. `None`
    /// when the bundle is gone: sealed since the listing, and to be read
    /// from its segment.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the file cannot be read, [`Error::NotText`] for
    /// bytes that are not UTF-8, and [`Error::BadLine`] naming the first
    /// line that does not read as a claim.
    pub fn bundle(&self, open: &Open, digest: &Digest) -> Result<Option<Vec<Claim>>> {
        let text = match self.bundle_store(&open.dir).read(digest) {
            Ok(Some(bytes)) => String::from_utf8(bytes).map_err(|_| Error::NotText)?,
            Ok(None) | Err(immure::Error::RootMissing(_)) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        parse_bundle(&text, self.store.algorithm()).map(Some)
    }

    /// Whether the open segment was lost: none of its names stands in
    /// the root while the claims store holds sealed segments.
    ///
    /// Every seal leaves the open segment behind, so an archive in order
    /// is never in this state. One begun anew here would name no
    /// predecessor and start a second record that the end of the first
    /// does not reach. `ossuary-fix chain` begins it after the last
    /// sealed segment instead.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the root cannot be listed, and [`Error::Store`]
    /// when the claims store cannot be walked.
    pub fn head_lost(&self) -> Result<bool> {
        if self.find()? != Found::Nothing || self.find_again()? != Found::Nothing {
            return Ok(false);
        }
        self.holds_segments()
    }

    /// Whether a seal is under way or stopped: the open segment's
    /// directory stands as `<S>.sealing` or `<S>.sealed-<S'>`. A run that
    /// writes finishes it.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when `open/` cannot be listed.
    pub fn seal_pending(&self) -> Result<bool> {
        Ok(matches!(self.find()?, Found::Pending { .. }))
    }

    /// Whether a seal was decided and its segment is lost: the directory
    /// is `<S>.sealed-<S'>`, and S' is neither in the claims store nor a
    /// segment file in the directory. Such a seal cannot be finished;
    /// [`finish_pending`](Log::finish_pending) answers
    /// [`Error::SealUnfinished`] for it.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when `open/` cannot be listed, and [`Error::Store`]
    /// from the claims store.
    pub fn decided_segment_lost(&self) -> Result<bool> {
        let Found::Pending { sealed, .. } = self.find()? else {
            return Ok(false);
        };
        for dir in &sealed {
            if let Some(digest) = digest_in_name(dir)
                && !self.store.contains(&digest)?
                && self.segment_file(dir, &digest).is_none()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Ready the log for a run that writes, or refuse the run while the
    /// open segment is lost.
    ///
    /// [`append`](Log::append) refuses each claim on its own; a run that
    /// stores bytes or asks an extractor or a server before recording
    /// what it got asks this first, so that it stops before any of that
    /// work. Readying finishes a seal that stopped; the answer says so,
    /// so that the run can tell.
    ///
    /// # Errors
    ///
    /// [`Error::HeadLost`] when the open segment is missing, and
    /// everything finishing a seal can answer.
    pub fn writable(&self) -> Result<Writable> {
        match self.find()? {
            Found::Open(_) => Ok(Writable::Ready),
            Found::Pending { .. } => {
                self.finish_pending()?;
                Ok(Writable::SealFinished)
            }
            Found::Nothing => {
                if self.find_again()? != Found::Nothing {
                    return self.writable();
                }
                Err(Error::HeadLost(self.root.clone()))
            }
        }
    }

    /// Seal the open segment: its bundles' lines go into the store as
    /// one segment, and the directory becomes the open segment after it.
    ///
    /// `None` when there is nothing to seal: no bundle in the open
    /// segment, or another process sealing it at this moment. A seal
    /// that another process left unfinished is finished first.
    ///
    /// # Errors
    ///
    /// [`Error::HeadLost`] when there is no open segment, [`Error::Store`]
    /// from the store, [`Error::Io`] from the files, and everything
    /// reading a bundle can answer.
    pub fn seal(&self) -> Result<Option<Segment>> {
        self.finish_pending()?;
        let dir = match self.find()? {
            Found::Open(dir) => dir,
            Found::Pending { .. } => return Ok(None),
            Found::Nothing => {
                if self.holds_segments()? {
                    return Err(Error::HeadLost(self.root.clone()));
                }
                return Ok(None);
            }
        };
        match self.bundle_store(&dir).entries().count_entries() {
            Ok(0) | Err(immure::Error::RootMissing(_)) => return Ok(None),
            Ok(_) => {}
            Err(error) => return Err(error.into()),
        }
        let previous = digest_in_name(&dir);
        let sealing = self.open_root().join(sealing_name(previous.as_ref()));
        if !rename_dir(&dir, &sealing)? {
            return Ok(None);
        }
        self.finish_sealing(&sealing)
    }

    /// The digest the open segment would be sealed under right now: the
    /// segment built from its bundles as [`seal`](Log::seal) builds it,
    /// hashed and not stored. `None` when there is nothing to seal. A
    /// bundle appended after this changes the answer.
    ///
    /// # Errors
    ///
    /// Everything [`open`](Log::open) and [`bundle`](Log::bundle) can
    /// answer.
    pub fn seal_preview(&self) -> Result<Option<Digest>> {
        let Some(open) = self.open()? else {
            return Ok(None);
        };
        let mut lines = Vec::new();
        for digest in &open.bundles {
            if let Some(bundle) = self.bundle(&open, digest)? {
                for claim in bundle {
                    lines.push((claim.time().as_str().to_string(), claim.line().into_owned()));
                }
            }
        }
        if lines.is_empty() {
            return Ok(None);
        }
        let text = segment_text(open.previous.as_ref(), lines);
        Ok(Some(self.store.digest(text.as_bytes())))
    }

    /// Finish every seal that is under way or stopped. Answers the
    /// segments the finished seals ended up with.
    ///
    /// # Errors
    ///
    /// As [`seal`](Log::seal).
    pub fn finish_pending(&self) -> Result<Vec<Segment>> {
        let Found::Pending { sealing, sealed } = self.find()? else {
            return Ok(Vec::new());
        };
        let mut done = Vec::new();
        for dir in sealing {
            if let Some(segment) = self.finish_sealing(&dir)? {
                done.push(segment);
            }
        }
        for dir in sealed {
            if let Some(digest) = digest_in_name(&dir) {
                self.finish_sealed(&dir, None)?;
                done.push(Segment { digest });
            }
        }
        Ok(done)
    }

    /// Steps 2 to 5 for a `<S>.sealing` directory; `None` when another
    /// process got there first or the directory is empty.
    fn finish_sealing(&self, sealing: &Path) -> Result<Option<Segment>> {
        let previous = digest_in_name(sealing);
        let bundles = self.bundle_store(sealing);
        let names = match bundles.entries().digests() {
            Ok(names) => names,
            Err(immure::Error::RootMissing(_)) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut lines = Vec::new();
        for name in &names {
            let bytes = match bundles.read(name) {
                Ok(Some(bytes)) => bytes,
                // Renamed to `sealed-` by another finisher, or emptied.
                Ok(None) | Err(immure::Error::RootMissing(_)) => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            let text = String::from_utf8(bytes).map_err(|_| Error::NotText)?;
            // The lines as they stand in the bundle, not as this build
            // would write them: a claim's id is the digest of its line.
            for claim in parse_bundle(&text, self.store.algorithm())? {
                lines.push((claim.time().as_str().to_string(), claim.line().into_owned()));
            }
        }
        if names.is_empty() {
            // A threshold seen in a stale listing: nothing to seal, and
            // the directory is the open segment again.
            rename_dir(
                sealing,
                &self.open_root().join(open_name(previous.as_ref())),
            )?;
            return Ok(None);
        }
        let text = segment_text(previous.as_ref(), lines);
        let digest = self.store.digest(text.as_bytes());
        if !write_segment_file(sealing, &text)? {
            return Ok(None);
        }
        let sealed = self
            .open_root()
            .join(sealed_name(previous.as_ref(), &digest));
        if !rename_dir(sealing, &sealed)? {
            // Another finisher won and stores its segment; the segment
            // file written here goes with step 5.
            return Ok(None);
        }
        self.store.add(text.as_bytes())?;
        let listed: BTreeSet<Digest> = names.into_iter().collect();
        self.finish_sealed(&sealed, Some(&listed))?;
        Ok(Some(Segment { digest }))
    }

    /// Steps 4 and 5 for a `<S>.sealed-<S'>` directory. `listed` is the
    /// set of bundles the segment was built from, when this process built
    /// it; a helper works it out from the segment.
    fn finish_sealed(&self, sealed: &Path, listed: Option<&BTreeSet<Digest>>) -> Result<()> {
        let Some(digest) = digest_in_name(sealed) else {
            return Ok(());
        };
        if !self.store.contains(&digest)? && !self.store_segment_file(sealed, &digest)? {
            return Ok(());
        }
        let bundles = self.bundle_store(sealed);
        let names = match bundles.entries().digests() {
            Ok(names) => names,
            Err(immure::Error::RootMissing(_)) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let sealed_bundles: Vec<Digest> = match listed {
            Some(listed) => names
                .into_iter()
                .filter(|name| listed.contains(name))
                .collect(),
            None => self.sealed_among(&bundles, names, &digest)?.0,
        };
        for name in &sealed_bundles {
            remove_quietly(&bundles.path_of(name));
        }
        if let Ok(entries) = fs::read_dir(sealed) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(SEGMENT_FILE) {
                    remove_quietly(&entry.path());
                } else if name == TMP {
                    // Emptied, never removed: see the module notes.
                    if let Ok(leftovers) = fs::read_dir(entry.path()) {
                        for leftover in leftovers.flatten() {
                            remove_quietly(&leftover.path());
                        }
                    }
                }
            }
        }
        rename_dir(sealed, &self.open_root().join(open_name(Some(&digest))))?;
        Ok(())
    }

    /// The winner of the rename to `.sealed-` stopped before storing: store
    /// the segment file with the decided digest from the directory. False
    /// when the directory is gone; an error when it holds no such file.
    fn store_segment_file(&self, sealed: &Path, digest: &Digest) -> Result<bool> {
        if fs::read_dir(sealed).is_err() {
            return Ok(false);
        }
        let Some(bytes) = self.segment_file(sealed, digest) else {
            return Err(Error::SealUnfinished(sealed.to_path_buf()));
        };
        self.store.add(&bytes)?;
        Ok(true)
    }

    /// The bytes of the segment file in `dir` that has `digest`, when
    /// there is one: what a seal decided and has not stored yet.
    fn segment_file(&self, dir: &Path, digest: &Digest) -> Option<Vec<u8>> {
        for entry in fs::read_dir(dir).ok()?.flatten() {
            if !entry
                .file_name()
                .to_string_lossy()
                .starts_with(SEGMENT_FILE)
            {
                continue;
            }
            let Ok(bytes) = fs::read(entry.path()) else {
                continue;
            };
            if self.store.digest(&bytes) == *digest {
                return Some(bytes);
            }
        }
        None
    }

    /// Every sealed segment in log order: along the chain, from the
    /// first segment to the one the open segment names.
    ///
    /// The walk reads each segment's header and nothing more. See
    /// [`walk_back`](Log::walk_back) for what ends it with an error.
    ///
    /// # Errors
    ///
    /// Everything [`walk_back`](Log::walk_back) and its steps can answer.
    pub fn segments(&self) -> Result<Vec<Segment>> {
        let mut segments = self.walk_back()?.collect::<Result<Vec<_>>>()?;
        segments.reverse();
        Ok(segments)
    }

    /// Walk back along the chain from the open segment, one segment at a
    /// time: for a reader that can stop at a segment it already knows,
    /// as the index does.
    ///
    /// A lost open segment refuses the walk before it begins, and a step
    /// to a predecessor the store does not hold is an error: the order of
    /// whatever lies beyond a break is not known, so nothing reads around
    /// one.
    ///
    /// # Errors
    ///
    /// [`Error::HeadLost`] when the open segment was lost, and everything
    /// [`open`](Log::open) can answer. A step answers
    /// [`Error::ChainBroken`] for a predecessor the store does not hold,
    /// and everything [`previous`](Log::previous) can answer otherwise.
    pub fn walk_back(&self) -> Result<WalkBack<'_>> {
        let previous = self.open()?.and_then(|open| open.previous);
        Ok(self.walk_back_from(previous.as_ref()))
    }

    /// Walk back along the chain from `segment`, the one the open
    /// segment names: for a reader that has the listing in hand, as
    /// [`Open::previous`] gives it. `None` walks nothing. A step answers
    /// as in [`walk_back`](Log::walk_back).
    #[must_use]
    pub fn walk_back_from(&self, segment: Option<&Digest>) -> WalkBack<'_> {
        WalkBack {
            log: self,
            next: segment.map(|digest| ("the open segment".to_string(), digest.clone())),
        }
    }

    /// The segment a sealed segment names as the one sealed before it,
    /// read from its header alone.
    ///
    /// # Errors
    ///
    /// [`Error::SegmentMissing`] when the digest names nothing,
    /// [`Error::NotText`], [`Error::SegmentHeader`],
    /// [`Error::PreviousList`] and [`Error::SegmentGeneration`] for a
    /// header that does not read, and [`Error::Store`] from the store.
    pub fn previous(&self, digest: &Digest) -> Result<Option<Digest>> {
        let mut reader = self
            .store
            .reader(digest)?
            .ok_or_else(|| Error::SegmentMissing(digest.to_string()))?;
        let mut line = Vec::new();
        reader.read_until(b'\n', &mut line).map_err(|source| {
            immure::Error::from_io(source, format!("segment {digest}: reading the header"))
        })?;
        let text = String::from_utf8(line).map_err(|_| Error::NotText)?;
        parse_header(text.lines().next().unwrap_or_default())
    }

    /// Read one sealed segment back: its claims, in the order recorded.
    ///
    /// # Errors
    ///
    /// Everything [`contents`](Log::contents) can answer.
    pub fn read(&self, digest: &Digest) -> Result<Vec<Claim>> {
        self.contents(digest).map(Contents::into_claims)
    }

    /// Read one sealed segment back whole: the segment it names as sealed
    /// before it, and its claims in the order recorded.
    ///
    /// # Errors
    ///
    /// [`Error::SegmentMissing`] when the digest names nothing,
    /// [`Error::NotText`] and [`Error::SegmentHeader`] when what it names is
    /// not a segment, [`Error::SegmentGeneration`] for one from a newer
    /// build, and [`Error::BadLine`] naming the first line that would not
    /// read back.
    pub fn contents(&self, digest: &Digest) -> Result<Contents> {
        let bytes = self
            .store
            .read(digest)?
            .ok_or_else(|| Error::SegmentMissing(digest.to_string()))?;
        let text = String::from_utf8(bytes).map_err(|_| Error::NotText)?;
        parse_segment(&text, self.store.algorithm())
    }

    /// The bundle store in `dir`: entries named by digest, `.jsonl`, never
    /// compressed, and never making the directory.
    fn bundle_store(&self, dir: &Path) -> Store {
        Store::builder(dir)
            .suffix(BUNDLE_SUFFIX)
            .depth(0)
            .algorithm(self.store.algorithm())
            .create_root(false)
            .build()
            .expect("a bundle store of depth 0 builds")
    }

    /// Every directory of the open segment in `open/`, whatever its name;
    /// none when `open/` is missing.
    fn list(&self) -> Result<Vec<PathBuf>> {
        let parent = self.open_root();
        let listing = match fs::read_dir(&parent) {
            Ok(listing) => listing,
            Err(source) if source.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(Error::Io {
                    context: format!("{}: listing", parent.display()),
                    source,
                });
            }
        };
        let mut dirs = Vec::new();
        for entry in listing {
            let entry = entry.map_err(|source| Error::Io {
                context: format!("{}: listing", parent.display()),
                source,
            })?;
            if State::of(&entry.file_name().to_string_lossy()).is_some() {
                dirs.push(entry.path());
            }
        }
        dirs.sort();
        Ok(dirs)
    }

    /// What `open/` shows of the open segment, from one listing.
    fn find(&self) -> Result<Found> {
        let mut opens = Vec::new();
        let mut sealing = Vec::new();
        let mut sealed = Vec::new();
        for dir in self.list()? {
            let name = dir.file_name().unwrap_or_default().to_string_lossy();
            match State::of(&name) {
                Some(State::Open) => opens.push(dir),
                Some(State::Sealed) => sealed.push(dir),
                Some(State::Sealing) => sealing.push(dir),
                None => {}
            }
        }
        if opens.len() > 1 {
            return Err(Error::TwoOpen(self.root.clone()));
        }
        if let Some(dir) = opens.pop() {
            return Ok(Found::Open(dir));
        }
        if sealing.is_empty() && sealed.is_empty() {
            return Ok(Found::Nothing);
        }
        Ok(Found::Pending { sealing, sealed })
    }

    /// The listing repeated, for an `open/` that showed none of the names:
    /// `readdir` is not atomic, and a rename can fall into it.
    fn find_again(&self) -> Result<Found> {
        for _ in 1..LISTING_TRIES {
            std::thread::sleep(LISTING_DELAY);
            let found = self.find()?;
            if found != Found::Nothing {
                return Ok(found);
            }
        }
        Ok(Found::Nothing)
    }

    /// The open segment's directory for a writer: waits for a seal under
    /// way, finishes one that stays, and refuses when there is none.
    fn open_dir_to_write(&self) -> Result<PathBuf> {
        let mut waiting_since: Option<Instant> = None;
        loop {
            match self.find()? {
                Found::Open(dir) => return Ok(dir),
                Found::Pending { .. } => {
                    let since = *waiting_since.get_or_insert_with(Instant::now);
                    if since.elapsed() < HELP_AFTER {
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                    self.finish_pending()?;
                    waiting_since = None;
                }
                Found::Nothing => {
                    if self.find_again()? != Found::Nothing {
                        continue;
                    }
                    return Err(Error::HeadLost(self.root.clone()));
                }
            }
        }
    }

    /// Whether the claims store holds any sealed segment.
    fn holds_segments(&self) -> Result<bool> {
        Ok(self.store.entries().next().transpose()?.is_some())
    }
}

/// What the entries of a bundle store are asked for.
trait Digests {
    fn digests(self) -> immure::Result<Vec<Digest>>;
    fn count_entries(self) -> immure::Result<usize>;
}

impl Digests for immure::Entries<'_> {
    fn digests(self) -> immure::Result<Vec<Digest>> {
        let mut digests = Vec::new();
        for entry in self {
            digests.push(entry?.digest().clone());
        }
        digests.sort();
        Ok(digests)
    }

    fn count_entries(self) -> immure::Result<usize> {
        let mut count = 0;
        for entry in self {
            entry?;
            count += 1;
        }
        Ok(count)
    }
}

/// Where the bundle store keeps an entry, by digest: for removing it.
trait PathOf {
    fn path_of(&self, digest: &Digest) -> PathBuf;
}

impl PathOf for Store {
    fn path_of(&self, digest: &Digest) -> PathBuf {
        self.root()
            .join(format!("{}{}", digest.as_str(), self.suffix()))
    }
}

/// The state a directory name in `open/` says: `<S>` or `first` open,
/// `<S>.sealing`, `<S>.sealed-<S'>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Open,
    Sealing,
    Sealed,
}

impl State {
    /// `None` for a name that is none of the three, which `open/` holds
    /// nothing of.
    fn of(name: &str) -> Option<State> {
        if let Some(stem) = name.strip_suffix(SEALING_SUFFIX) {
            return is_open_name(stem).then_some(State::Sealing);
        }
        if let Some((stem, decided)) = name.split_once(SEALED_INFIX) {
            return (is_open_name(stem) && Digest::parse(decided).is_ok()).then_some(State::Sealed);
        }
        is_open_name(name).then_some(State::Open)
    }
}

fn is_open_name(name: &str) -> bool {
    name == FIRST || Digest::parse(name).is_ok()
}

fn open_name(previous: Option<&Digest>) -> String {
    match previous {
        None => FIRST.to_string(),
        Some(digest) => digest.to_string(),
    }
}

fn sealing_name(previous: Option<&Digest>) -> String {
    format!("{}{SEALING_SUFFIX}", open_name(previous))
}

fn sealed_name(previous: Option<&Digest>, digest: &Digest) -> String {
    format!("{}{SEALED_INFIX}{digest}", open_name(previous))
}

/// The segment a directory's name carries: the predecessor of an open or
/// sealing directory, the decided segment of a sealed one. `None` for the
/// first open segment, and for a name that carries no digest.
fn digest_in_name(dir: &Path) -> Option<Digest> {
    let name = dir.file_name()?.to_str()?;
    let carried = match name.split_once(SEALED_INFIX) {
        Some((_, decided)) => decided,
        None => name.strip_suffix(SEALING_SUFFIX).unwrap_or(name),
    };
    Digest::parse(carried).ok()
}

/// The segment the directory of an open segment follows, in any of its
/// three names: S for `<S>`, `<S>.sealing` and `<S>.sealed-<S'>`. `None`
/// for `first`.
fn predecessor_in_name(dir: &Path) -> Option<Digest> {
    let name = dir.file_name()?.to_str()?;
    let stem = match name.split_once(SEALED_INFIX) {
        Some((stem, _)) => stem,
        None => name.strip_suffix(SEALING_SUFFIX).unwrap_or(name),
    };
    Digest::parse(stem).ok()
}

/// Rename a directory; `false` when it is gone or the name is taken.
///
/// Over gvfs and on Windows servers the rename is refused with EACCES
/// while another process holds a handle inside; that is tried again a few
/// times and then given up for this call. Nothing is decided by a rename
/// that did not happen.
fn rename_dir(from: &Path, to: &Path) -> Result<bool> {
    for attempt in 1..=RENAME_TRIES {
        match fs::rename(from, to) {
            Ok(()) => {
                sync_dir(to.parent().unwrap_or(Path::new(".")));
                return Ok(true);
            }
            Err(source) if source.kind() == ErrorKind::NotFound => return Ok(false),
            Err(source)
                if matches!(
                    source.kind(),
                    ErrorKind::AlreadyExists | ErrorKind::DirectoryNotEmpty
                ) =>
            {
                return Ok(false);
            }
            Err(source)
                if matches!(
                    source.kind(),
                    ErrorKind::PermissionDenied | ErrorKind::ResourceBusy
                ) && attempt < RENAME_TRIES =>
            {
                std::thread::sleep(RENAME_DELAY);
            }
            Err(source) => {
                return Err(Error::Io {
                    context: format!("{}: renaming to {}", from.display(), to.display()),
                    source,
                });
            }
        }
    }
    Ok(false)
}

/// Claims in the order a seal gives them: by time, then by the line.
pub fn sort_as_sealed(claims: &mut [Claim]) {
    claims
        .sort_by_cached_key(|claim| (claim.time().as_str().to_string(), claim.line().into_owned()));
}

/// The segment's bytes from its lines: the header, then the lines sorted
/// by time and then by the line itself, a total order so that the same set
/// of bundles always gives the same digest.
fn segment_text(previous: Option<&Digest>, mut lines: Vec<(String, String)>) -> String {
    lines.sort();
    let mut text = header_line(previous);
    for (_, line) in lines {
        text.push_str(&line);
        text.push('\n');
    }
    text
}

/// Write the segment's bytes into `dir` under a name of this call's own,
/// flushed; `false` when the directory is gone.
///
/// The name is random. A seal that stopped after this step leaves its file
/// behind until step 5, and a name built from the process id would be
/// taken for the next process with that id: the same host after a restart,
/// or another container, where the id is the same small number every time.
fn write_segment_file(dir: &Path, text: &str) -> Result<bool> {
    let path = dir.join(format!("{SEGMENT_FILE}{}.seg", uuid::Uuid::new_v4()));
    match fs::File::create_new(&path) {
        Ok(mut file) => {
            let io = |source| Error::Io {
                context: format!("{}: writing the segment", path.display()),
                source,
            };
            file.write_all(text.as_bytes()).map_err(io)?;
            immure::sync_file(&file).map_err(io)?;
            Ok(true)
        }
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(false),
        Err(source) => Err(Error::Io {
            context: format!("{}: writing the segment", path.display()),
            source,
        }),
    }
}

/// Remove a file that may already be gone, or be gone with its directory.
/// A removal that fails for another reason leaves a sealed bundle behind,
/// which the next seal would seal again; that shows up as a duplicate
/// line, which the index folds into one claim by its id.
fn remove_quietly(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Flush the directory entry of `path`. Best effort, as in the store: some
/// filesystems do not let a directory be opened or flushed.
fn sync_dir(dir: &Path) {
    if let Ok(handle) = fs::File::open(dir) {
        immure::sync_file(&handle).ok();
    }
}

/// The header line read back: the segment it names as sealed before it.
///
/// The generation is read on its own before the header is read whole: a
/// future generation may spell its members differently, and it must be
/// refused as what it is, newer, not reported as broken. A member this
/// build does not know is passed over.
fn parse_header(first: &str) -> Result<Option<Digest>> {
    let broken = || Error::SegmentHeader(first.to_string());
    let generation: Generation = serde_json::from_str(first).map_err(|_| broken())?;
    if generation.generation != GENERATION {
        return Err(Error::SegmentGeneration(generation.generation));
    }
    let header: Header = serde_json::from_str(first).map_err(|_| broken())?;
    // A predecessor is named by digest, and a header that names one in
    // any other spelling is a broken header, not a segment without one.
    match header.previous {
        None => Ok(None),
        Some(serde_json::Value::String(hex)) => Digest::parse(&hex).map(Some).map_err(|_| broken()),
        Some(serde_json::Value::Array(_)) => Err(Error::PreviousList),
        Some(_) => Err(broken()),
    }
}

/// Header first, claims after, strict throughout. Every claim gets its id
/// under `algorithm`, the archive's.
fn parse_segment(text: &str, algorithm: Algorithm) -> Result<Contents> {
    let mut lines = text.lines().enumerate();
    let first = lines.next().map(|(_, line)| line).unwrap_or_default();
    let previous = parse_header(first)?;
    let mut claims = Vec::new();
    for (index, line) in lines {
        let claim = Claim::read_line(line, algorithm).map_err(|source| Error::BadLine {
            line: index + 1,
            source: Box::new(source),
        })?;
        claims.push(claim);
    }
    Ok(Contents { previous, claims })
}

/// A bundle's lines, every one a claim with its id under `algorithm`.
fn parse_bundle(text: &str, algorithm: Algorithm) -> Result<Vec<Claim>> {
    let mut claims = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let claim = Claim::read_line(line, algorithm).map_err(|source| Error::BadLine {
            line: index + 1,
            source: Box::new(source),
        })?;
        claims.push(claim);
    }
    Ok(claims)
}

/// A log with its open segment begun, for the tests of the other modules.
#[cfg(test)]
pub(crate) fn test_log(store: Store, root: &Path) -> Log {
    let log = Log::new(store, root);
    log.init().unwrap();
    log
}

/// Where a seal stopped, for [`stop_seal`].
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub(crate) enum StoppedAfter {
    /// Step 1: the directory is `<S>.sealing`.
    Renaming,
    /// Step 3: the directory is `<S>.sealed-<S'>`, and S' is not in the
    /// store. With `segment_file` false the file of step 2 is missing
    /// as well, and S' is nowhere.
    Deciding { segment_file: bool },
    /// Step 4: S' is in the store, and no bundle was removed yet.
    Storing,
}

/// Bring the open segment into the state a seal that stopped there leaves,
/// for the tests of this and the other modules. Answers S', the segment
/// the seal decides on.
#[cfg(test)]
pub(crate) fn stop_seal(log: &Log, after: StoppedAfter) -> Digest {
    let open = log.open().unwrap().unwrap();
    let mut lines = Vec::new();
    for bundle in open.bundles() {
        for claim in log.bundle(&open, bundle).unwrap().unwrap() {
            lines.push((claim.time().as_str().to_string(), claim.line().into_owned()));
        }
    }
    let previous = open.previous().cloned();
    let text = segment_text(previous.as_ref(), lines);
    let digest = log.store.digest(text.as_bytes());
    let sealing = log.open_root().join(sealing_name(previous.as_ref()));
    fs::rename(&open.dir, &sealing).unwrap();
    let segment_file = match after {
        StoppedAfter::Renaming => return digest,
        StoppedAfter::Deciding { segment_file } => segment_file,
        StoppedAfter::Storing => true,
    };
    if segment_file {
        write_segment_file(&sealing, &text).unwrap();
    }
    let sealed = log
        .open_root()
        .join(sealed_name(previous.as_ref(), &digest));
    fs::rename(&sealing, sealed).unwrap();
    if matches!(after, StoppedAfter::Storing) {
        log.store.add(text.as_bytes()).unwrap();
    }
    digest
}

#[cfg(test)]
mod tests {
    use immure::Store;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::claim::{Attribute, Source, Subject, Timestamp};

    fn store_in(dir: &TempDir) -> Store {
        Store::builder(dir.path().join("claims"))
            .suffix(".seg")
            .depth(1)
            .compress(true)
            .create()
            .unwrap()
    }

    fn log_in(dir: &TempDir) -> Log {
        let log = Log::new(store_in(dir), dir.path());
        log.init().unwrap();
        log
    }

    fn claim(tag: &str, time: &str) -> Claim {
        Claim::assert(
            Subject::parse("9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e")
                .unwrap(),
            Attribute::parse("user:tag").unwrap(),
            json!(tag),
            Timestamp::parse(time).unwrap(),
            Source::parse("user").unwrap(),
            crate::claim::Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        )
        .unwrap()
    }

    /// What `open/` holds, by name.
    fn names(dir: &TempDir) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir.path().join("open"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// `open/<name>`.
    fn under_open(dir: &TempDir, name: &str) -> PathBuf {
        dir.path().join("open").join(name)
    }

    fn lines_of(claims: &[Claim]) -> Vec<(String, String)> {
        claims
            .iter()
            .map(|claim| (claim.time().as_str().to_string(), claim.to_line()))
            .collect()
    }

    #[test]
    fn the_open_segment_is_a_directory_of_bundles() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        assert_eq!(names(&dir), ["first"]);
        let mut root: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        root.sort();
        assert_eq!(
            root,
            ["claims", "open"],
            "the root holds open/, nothing more"
        );
        assert!(log.head().unwrap().is_empty());

        log.append_all(&[
            claim("first", "2026-09-05T12:00:00Z"),
            claim("second", "2026-09-05T12:00:00Z"),
        ])
        .unwrap();
        log.append(&claim("third", "2026-09-05T12:00:01Z")).unwrap();

        let open = log.open().unwrap().unwrap();
        assert_eq!(open.previous(), None);
        assert_eq!(open.bundles().len(), 2, "one file per append");
        let claims = log.head().unwrap();
        assert_eq!(claims.len(), 3);
        assert!(
            claims.iter().all(|claim| claim.id().is_some()),
            "read with ids"
        );
        let files: Vec<String> = fs::read_dir(under_open(&dir, "first"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(files.len(), 3, "two bundles and tmp/");
        assert_eq!(
            files
                .iter()
                .filter(|name| Path::new(name)
                    .extension()
                    .is_some_and(|ext| ext == "jsonl"))
                .count(),
            2
        );
    }

    #[test]
    fn a_log_without_an_open_segment_and_nothing_sealed_is_empty_rather_than_broken() {
        let dir = TempDir::new().unwrap();
        let log = Log::new(store_in(&dir), dir.path());
        assert!(log.head().unwrap().is_empty());
        assert!(log.open().unwrap().is_none());
        assert!(!log.head_lost().unwrap());
        assert!(matches!(
            log.append(&claim("x", "2026-09-05T12:00:00Z")),
            Err(Error::HeadLost(_))
        ));
    }

    #[test]
    fn sealing_sorts_the_lines_by_time_and_the_directory_becomes_the_next_open_segment() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("late", "2026-09-05T12:00:09Z")).unwrap();
        log.append(&claim("early", "2026-09-05T12:00:01Z")).unwrap();

        let segment = log.seal().unwrap().unwrap();

        let contents = log.contents(segment.digest()).unwrap();
        assert_eq!(contents.previous(), None);
        assert_eq!(
            contents
                .claims()
                .iter()
                .map(|c| c.value().unwrap().as_str().unwrap())
                .collect::<Vec<_>>(),
            ["early", "late"]
        );
        assert_eq!(names(&dir), [segment.digest().to_string()]);
        let open = log.open().unwrap().unwrap();
        assert_eq!(open.previous(), Some(segment.digest()));
        assert!(open.bundles().is_empty());
        assert!(log.head().unwrap().is_empty());
        assert_eq!(log.seal().unwrap(), None, "nothing to seal");
    }

    #[test]
    fn every_segment_names_the_one_sealed_before_it() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("one", "2026-09-05T12:00:00Z")).unwrap();
        let first = log.seal().unwrap().unwrap();
        log.append(&claim("two", "2026-09-05T12:00:01Z")).unwrap();
        let second = log.seal().unwrap().unwrap();

        assert_eq!(log.previous(first.digest()).unwrap(), None);
        assert_eq!(
            log.previous(second.digest()).unwrap().as_ref(),
            Some(first.digest())
        );
        assert_eq!(log.segments().unwrap(), [first.clone(), second.clone()]);
        assert_eq!(
            log.walk_back()
                .unwrap()
                .map(Result::unwrap)
                .collect::<Vec<_>>(),
            [second, first]
        );
    }

    #[test]
    fn an_append_that_finds_the_open_segment_full_seals_it_first() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        for n in 0..SEAL_AT {
            log.append(&claim(&format!("tag{n}"), "2026-09-05T12:00:00Z"))
                .unwrap();
        }
        assert_eq!(
            log.segments().unwrap().len(),
            0,
            "the threshold is not crossed yet"
        );

        log.append(&claim("one more", "2026-09-05T12:00:01Z"))
            .unwrap();

        let segments = log.segments().unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(log.read(segments[0].digest()).unwrap().len(), SEAL_AT);
        let open = log.open().unwrap().unwrap();
        assert_eq!(
            open.bundles().len(),
            1,
            "the one more, in the fresh open segment"
        );
    }

    #[test]
    fn an_append_that_has_itself_written_a_mebibyte_seals_first() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        // One bundle of about half the bytes, twice: the count stays far
        // below SEAL_AT, the bytes of this process do not.
        let half = |prefix: &str| -> Vec<Claim> {
            (0..2000)
                .map(|n| claim(&format!("{prefix}{n:0>160}"), "2026-09-05T12:00:00Z"))
                .collect()
        };
        log.append_all(&half("a")).unwrap();
        log.append_all(&half("b")).unwrap();
        assert_eq!(log.segments().unwrap().len(), 0, "written, not sealed");
        assert!(log.written_to(&log.open().unwrap().unwrap().dir) >= SEAL_AT_BYTES);

        log.append(&claim("one more", "2026-09-05T12:00:01Z"))
            .unwrap();

        let segments = log.segments().unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(log.read(segments[0].digest()).unwrap().len(), 4000);
        assert_eq!(log.open().unwrap().unwrap().bundles().len(), 1);

        // Another Log on the same archive knows nothing of those bytes.
        let other = Log::new(store_in(&dir), dir.path());
        assert_eq!(other.written_to(&other.open().unwrap().unwrap().dir), 0);
    }

    #[test]
    fn a_late_bundle_survives_the_seal_and_goes_into_the_next() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let first = claim("first", "2026-09-05T12:00:00Z");
        log.append(&first).unwrap();
        // The seal has listed and renamed; a writer whose path had
        // resolved before lands its bundle in the sealing directory.
        let sealing = under_open(&dir, "first.sealing");
        fs::rename(under_open(&dir, "first"), &sealing).unwrap();
        let late_line = format!("{}\n", claim("late", "2026-09-05T12:00:01Z").to_line());
        log.bundle_store(&sealing)
            .add(late_line.as_bytes())
            .unwrap();
        // What the finisher listed: the first bundle only.
        let listed = log
            .bundle_store(&sealing)
            .digest(format!("{}\n", first.to_line()).as_bytes());
        let text = segment_text(None, lines_of(&[first]));
        let digest = log.store.digest(text.as_bytes());
        write_segment_file(&sealing, &text).unwrap();
        let sealed = under_open(&dir, &format!("first.sealed-{digest}"));
        fs::rename(&sealing, &sealed).unwrap();
        log.store.add(text.as_bytes()).unwrap();
        log.finish_sealed(&sealed, Some(&BTreeSet::from([listed])))
            .unwrap();

        let open = log.open().unwrap().unwrap();
        assert_eq!(open.previous(), Some(&digest));
        assert_eq!(open.bundles().len(), 1, "the late bundle stays");
        let second = log.seal().unwrap().unwrap();
        let claims = log.read(second.digest()).unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].value(), Some(&json!("late")));
    }

    #[test]
    fn a_seal_that_stopped_is_finished_by_the_next_writer_and_said_so() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("first", "2026-09-05T12:00:00Z")).unwrap();
        // Stopped right after step 1.
        fs::rename(under_open(&dir, "first"), under_open(&dir, "first.sealing")).unwrap();
        assert!(log.seal_pending().unwrap());
        assert!(!log.head_lost().unwrap());

        assert_eq!(log.writable().unwrap(), Writable::SealFinished);

        let segments = log.segments().unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(log.read(segments[0].digest()).unwrap().len(), 1);
        assert!(log.head().unwrap().is_empty());
        assert_eq!(log.writable().unwrap(), Writable::Ready);
    }

    #[test]
    fn a_seal_that_stopped_after_the_decision_stores_the_segment_file() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("first", "2026-09-05T12:00:00Z")).unwrap();
        log.append(&claim("second", "2026-09-05T12:00:00Z"))
            .unwrap();
        let text = segment_text(None, lines_of(&log.head().unwrap()));
        let digest = log.store.digest(text.as_bytes());
        fs::rename(under_open(&dir, "first"), under_open(&dir, "first.sealing")).unwrap();
        write_segment_file(&under_open(&dir, "first.sealing"), &text).unwrap();
        // Stopped right after step 3: decided, not stored.
        fs::rename(
            under_open(&dir, "first.sealing"),
            under_open(&dir, &format!("first.sealed-{digest}")),
        )
        .unwrap();
        assert!(!log.store.contains(&digest).unwrap());

        let finished = log.finish_pending().unwrap();

        assert_eq!(
            finished,
            [Segment {
                digest: digest.clone()
            }]
        );
        assert!(log.store.contains(&digest).unwrap());
        assert_eq!(log.read(&digest).unwrap().len(), 2);
        assert_eq!(names(&dir), [digest.to_string()]);
        assert!(
            log.head().unwrap().is_empty(),
            "both bundles were in the segment and went"
        );
    }

    /// One claim as another program may write it: the same JSON with a
    /// space after every colon and comma.
    fn spaced(tag: &str, time: &str) -> String {
        claim(tag, time)
            .to_line()
            .replace("\":", "\": ")
            .replace(",\"", ", \"")
    }

    #[test]
    fn a_seal_copies_each_line_as_it_stands_so_the_id_stays() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let line = spaced("foreign", "2026-09-05T12:00:00Z");
        assert_ne!(line, claim("foreign", "2026-09-05T12:00:00Z").to_line());
        log.bundle_store(&under_open(&dir, "first"))
            .add(format!("{line}\n").as_bytes())
            .unwrap();
        let before = log.head().unwrap();
        let id = before[0].id().unwrap().clone();

        let segment = log.seal().unwrap().unwrap();

        let sealed = log.read(segment.digest()).unwrap();
        assert_eq!(sealed.len(), 1);
        assert_eq!(
            sealed[0].id(),
            Some(&id),
            "a retraction written before the seal still names it"
        );
        let bytes = log.store.read(segment.digest()).unwrap().unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text.lines().nth(1), Some(line.as_str()));
    }

    #[test]
    fn a_helper_finds_a_bundle_of_such_lines_in_the_segment() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let line = spaced("foreign", "2026-09-05T12:00:00Z");
        log.bundle_store(&under_open(&dir, "first"))
            .add(format!("{line}\n").as_bytes())
            .unwrap();
        let text = segment_text(None, vec![("2026-09-05T12:00:00Z".to_string(), line)]);
        let digest = log.store.digest(text.as_bytes());
        let sealing = under_open(&dir, "first.sealing");
        fs::rename(under_open(&dir, "first"), &sealing).unwrap();
        write_segment_file(&sealing, &text).unwrap();
        // Stopped right after step 3; the helper has no list of bundles
        // and goes by the claims' ids.
        fs::rename(
            &sealing,
            under_open(&dir, &format!("first.sealed-{digest}")),
        )
        .unwrap();

        log.finish_pending().unwrap();

        assert!(log.head().unwrap().is_empty(), "the bundle went");
        assert_eq!(log.read(&digest).unwrap().len(), 1);
    }

    #[test]
    fn a_segment_file_left_by_a_stopped_seal_is_not_in_the_way() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("first", "2026-09-05T12:00:00Z")).unwrap();
        let sealing = under_open(&dir, "first.sealing");
        fs::rename(under_open(&dir, "first"), &sealing).unwrap();
        // Stopped after step 2, twice: each try left its file.
        assert!(write_segment_file(&sealing, "left behind\n").unwrap());
        assert!(write_segment_file(&sealing, "left behind\n").unwrap());

        assert_eq!(log.writable().unwrap(), Writable::SealFinished);

        let segments = log.segments().unwrap();
        assert_eq!(segments.len(), 1);
        let left: Vec<String> = fs::read_dir(under_open(&dir, segments[0].digest().as_str()))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["tmp"], "the segment files went with step 5");
    }

    #[test]
    fn a_reader_takes_the_bundles_of_a_stopped_seal_as_unsealed() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("first", "2026-09-05T12:00:00Z")).unwrap();
        stop_seal(&log, StoppedAfter::Renaming);

        let contents = log.head_contents().unwrap();

        assert_eq!(contents.previous(), None);
        assert_eq!(contents.claims().len(), 1);
        assert_eq!(names(&dir), ["first.sealing"], "reading changes nothing");
    }

    #[test]
    fn a_decided_seal_reads_as_before_it_while_its_segment_is_only_a_file() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("first", "2026-09-05T12:00:00Z")).unwrap();
        let decided = stop_seal(&log, StoppedAfter::Deciding { segment_file: true });

        let open = log.open().unwrap().unwrap();

        assert_eq!(open.previous(), None, "the store does not hold it yet");
        assert_eq!(open.bundles().len(), 1);
        assert_eq!(names(&dir), [format!("first.sealed-{decided}")]);
    }

    #[test]
    fn a_decided_seal_in_the_store_is_followed_and_only_late_bundles_are_unsealed() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("first", "2026-09-05T12:00:00Z")).unwrap();
        let decided = stop_seal(&log, StoppedAfter::Storing);
        let late = format!("{}\n", claim("late", "2026-09-05T12:00:01Z").to_line());
        log.bundle_store(&under_open(&dir, &format!("first.sealed-{decided}")))
            .add(late.as_bytes())
            .unwrap();

        let contents = log.head_contents().unwrap();

        assert_eq!(contents.previous(), Some(&decided));
        assert_eq!(contents.claims().len(), 1);
        assert_eq!(contents.claims()[0].value(), Some(&json!("late")));
    }

    #[test]
    fn a_decided_seal_whose_segment_is_nowhere_reads_as_a_lost_segment() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("first", "2026-09-05T12:00:00Z")).unwrap();
        let decided = stop_seal(
            &log,
            StoppedAfter::Deciding {
                segment_file: false,
            },
        );

        let open = log.open().unwrap().unwrap();

        assert_eq!(open.previous(), Some(&decided));
        let step = log.walk_back_from(open.previous()).next().unwrap();
        assert!(matches!(
            step,
            Err(Error::ChainBroken { missing, .. }) if missing == decided.to_string()
        ));
    }

    #[test]
    fn a_decided_seal_without_its_segment_file_is_an_error_not_a_guess() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("first", "2026-09-05T12:00:00Z")).unwrap();
        let digest = log.store.digest(b"not what was decided");
        fs::rename(
            under_open(&dir, "first"),
            under_open(&dir, &format!("first.sealed-{digest}")),
        )
        .unwrap();

        assert!(matches!(
            log.finish_pending(),
            Err(Error::SealUnfinished(_))
        ));
        assert!(matches!(
            log.append(&claim("x", "2026-09-05T12:00:00Z")),
            Err(Error::SealUnfinished(_))
        ));
    }

    #[test]
    fn an_open_segment_lost_over_sealed_segments_is_not_begun_anew() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("one", "2026-09-05T12:00:00Z")).unwrap();
        let segment = log.seal().unwrap().unwrap();
        fs::remove_dir_all(under_open(&dir, segment.digest().as_ref())).unwrap();

        assert!(log.head_lost().unwrap());
        assert!(matches!(
            log.append(&claim("x", "2026-09-05T12:00:00Z")),
            Err(Error::HeadLost(_))
        ));
        assert!(matches!(log.writable(), Err(Error::HeadLost(_))));
        assert!(matches!(log.open(), Err(Error::HeadLost(_))));
        assert!(matches!(log.walk_back(), Err(Error::HeadLost(_))));
        assert_eq!(names(&dir), Vec::<String>::new(), "nothing was begun");

        log.begin(Some(segment.digest())).unwrap();
        assert!(!log.head_lost().unwrap());
        assert_eq!(
            log.open().unwrap().unwrap().previous(),
            Some(segment.digest())
        );
    }

    #[test]
    fn two_open_segments_are_refused() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let other = log.store.digest(b"other");
        fs::create_dir(under_open(&dir, other.as_ref())).unwrap();
        assert!(matches!(log.open(), Err(Error::TwoOpen(_))));
        assert!(matches!(
            log.append(&claim("x", "2026-09-05T12:00:00Z")),
            Err(Error::TwoOpen(_))
        ));
    }

    #[test]
    fn the_walk_back_stops_at_a_lost_segment_and_names_who_named_it() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("one", "2026-09-05T12:00:00Z")).unwrap();
        let first = log.seal().unwrap().unwrap();
        log.append(&claim("two", "2026-09-05T12:00:01Z")).unwrap();
        let second = log.seal().unwrap().unwrap();
        log.store.remove(first.digest()).unwrap();

        let steps: Vec<Result<Segment>> = log.walk_back().unwrap().collect();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].as_ref().unwrap(), &second);
        assert!(matches!(
            &steps[1],
            Err(Error::ChainBroken { segment, missing })
                if segment == &format!("segment {}", second.digest()) && missing == first.digest().as_str()
        ));
        assert!(log.segments().is_err());
    }

    #[test]
    fn a_header_member_this_build_does_not_know_is_passed_over() {
        let line = r#"{"ossuary-segment":1,"previous":"9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e","note":"from a later build"}"#;
        assert_eq!(
            parse_header(line).unwrap().unwrap().as_str(),
            "9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e"
        );
    }

    #[test]
    fn a_predecessor_list_is_refused_for_what_it_is() {
        let line = r#"{"ossuary-segment":1,"previous":["9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e"]}"#;
        assert!(matches!(parse_header(line), Err(Error::PreviousList)));
        assert!(matches!(
            parse_header(r#"{"ossuary-segment":1,"previous":7}"#),
            Err(Error::SegmentHeader(_))
        ));
        assert!(matches!(
            parse_header(r#"{"ossuary-segment":2}"#),
            Err(Error::SegmentGeneration(2))
        ));
    }

    #[test]
    fn the_names_carry_the_digests() {
        let digest =
            Digest::parse("9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e")
                .unwrap();
        let other =
            Digest::parse("1f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e")
                .unwrap();
        assert_eq!(digest_in_name(Path::new("/a/open/first")), None);
        assert_eq!(digest_in_name(Path::new("/a/open/first.sealing")), None);
        assert_eq!(
            digest_in_name(Path::new(&format!("/a/open/{digest}"))),
            Some(digest.clone())
        );
        assert_eq!(
            digest_in_name(Path::new(&format!("/a/open/{digest}.sealing"))),
            Some(digest.clone())
        );
        assert_eq!(State::of("first"), Some(State::Open));
        assert_eq!(State::of(digest.as_ref()), Some(State::Open));
        assert_eq!(State::of("first.sealing"), Some(State::Sealing));
        assert_eq!(
            State::of(&sealed_name(Some(&digest), &other)),
            Some(State::Sealed)
        );
        assert_eq!(State::of("tmp"), None);
        assert_eq!(State::of("open-abc"), None);
        assert_eq!(
            digest_in_name(Path::new(&sealed_name(Some(&digest), &other))),
            Some(other.clone())
        );
        assert_eq!(
            digest_in_name(Path::new(&sealed_name(None, &other))),
            Some(other)
        );
    }
}
