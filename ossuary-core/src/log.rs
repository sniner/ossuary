//! The log: where claims accumulate, and how segments come to be.
//!
//! Claims land in the one open segment — `head.jsonl` in the archive root,
//! the only mutable file an archive has. Sealing closes it: the file's bytes
//! go into the claims store verbatim as an ordinary entry, and a fresh head
//! begins. Nothing is reformatted on the way — what was appended is what is
//! sealed, and a sealed segment is never compacted, combined or rewritten:
//! superseded and retracted claims stay where they were written, which for
//! an archive is not a limitation but the point. The head seals itself
//! once it grows to [`SEAL_AT`]; sealing by hand remains for closing it
//! on demand.
//!
//! Segments hang together. The fresh head that a seal begins names, in
//! its header, the segment just sealed, so every segment but the first
//! names the one before it, and the open head names the last. The
//! segments form one chain, and the chain is the order of the record:
//! the claims of the first segment come first, those of the open head
//! last, whatever their timestamps say. A sealed segment that goes
//! missing leaves a name nothing answers to, in the header of its
//! successor or of the head. The walk back from the head stops there,
//! and the readers here refuse rather than read around it; repairing
//! the chain rewrites segments, which only `ossuary-fix` does. What the
//! chain cannot show is a loss at its very end that the head was
//! rewritten to hide (the head is the one mutable file); a seal kept
//! outside the archive shows it (see `seal`).
//!
//! The head belongs to one writer at a time; concurrency lives in the store,
//! not here. An append does not fsync — the open segment's tail is the one
//! thing a crash may cost, and an ingest can say it again — while everything
//! sealed is durable the way the store promises. Reading is strict: a torn
//! last line stops the reader (and with it the seal) rather than being
//! quietly dropped, because a truth layer does not shrug.

use std::fs::{self, OpenOptions};
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};

use immure::{Digest, Store};
use serde::{Deserialize, Serialize};

use crate::claim::Claim;
use crate::error::{Error, Result};

/// The format generation this build writes, and the only one it reads.
///
/// Generation 1 is `docs/format.md`. A segment declaring any other is
/// refused as [`Error::SegmentGeneration`] rather than guessed at.
pub const GENERATION: u32 = 1;

/// The open segment seals itself once an append grows it to this size.
///
/// A mebibyte of head is a few thousand claims — the granularity the
/// concept asks of a segment — and seals into a store entry of one or two
/// hundred compressed kilobytes: a comfortable unit to read whole, to
/// replicate, and for a bloom filter to answer for without going blunt.
/// When to seal is the software's business (`docs/format.md` says so in
/// words), so this is a constant, not a knob; [`seal`](Log::seal) stays
/// alongside as the on-demand grip, and a head below the threshold simply
/// waits.
const SEAL_AT: u64 = 1024 * 1024;

/// The first line of every segment, open or sealed: a segment names its own
/// format before anything else, so that a stray file found alone in fifty
/// years still says what it is — and, from the second segment on, the
/// segment sealed before it.
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

/// The open segment that sealing leaves behind: a header that names
/// `segment`, and no claims. These are the bytes of `head.jsonl` right
/// after a seal.
#[must_use]
pub fn head_after(segment: &Digest) -> String {
    header_line(Some(segment))
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
    /// Read a segment from its text as a file holds it, header first:
    /// for an open segment that is not this log's own, such as the head
    /// of a copy of the archive.
    ///
    /// # Errors
    ///
    /// [`Error::SegmentHeader`], [`Error::PreviousList`] and
    /// [`Error::SegmentGeneration`] for a header that does not read or
    /// is in another form, and [`Error::BadLine`] naming the first claim
    /// that does not read.
    pub fn parse(text: &str) -> Result<Contents> {
        parse_segment(text)
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

/// The walk back along the chain, from the segment the open head names
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

/// The claim log: one open segment in front, sealed segments behind it.
///
/// A `Log` is a description — a store handle and a path — and describing is
/// not creating: the head file appears at the first
/// [`append`](Log::append), the way the store makes what it needs when it
/// writes.
#[derive(Debug)]
pub struct Log {
    store: Store,
    head: PathBuf,
}

impl Log {
    /// The log kept in this claims store, with its open segment at `head`.
    #[must_use]
    pub fn new(store: Store, head: impl Into<PathBuf>) -> Self {
        Log {
            store,
            head: head.into(),
        }
    }

    /// Append one claim to the open segment.
    ///
    /// A head that is not there yet is begun, header first, naming no
    /// predecessor, but only in an archive with nothing sealed: every
    /// seal leaves a head behind, so a head missing over sealed segments
    /// was lost (see [`head_lost`](Log::head_lost)). Header and claim go
    /// out in one write, and nothing is fsynced — see the module notes on
    /// what a crash may cost and what it may not.
    ///
    /// An append that grows the head to [`SEAL_AT`] seals it in the same
    /// breath, so no writer needs a sealing policy of its own. The claim
    /// is on the record before the seal begins — trouble sealing is still
    /// reported, but the head stands with everything in it, and a later
    /// append crosses the threshold again.
    ///
    /// # Errors
    ///
    /// [`Error::HeadLost`] when the head was lost, [`Error::Io`] when the
    /// file cannot be opened or written, and everything
    /// [`seal`](Log::seal) can answer when this append crossed the
    /// threshold.
    pub fn append(&self, claim: &Claim) -> Result<()> {
        let io = |source| Error::Io {
            context: format!("{}: appending", self.head.display()),
            source,
        };
        let fresh = !self.head_begun()?;
        if fresh && self.holds_segments()? {
            return Err(Error::HeadLost(self.head.clone()));
        }
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.head)
            .map_err(io)?;
        let mut lines = String::new();
        if fresh {
            lines.push_str(&header_line(None));
        }
        lines.push_str(&claim.to_line());
        lines.push('\n');
        file.write_all(lines.as_bytes()).map_err(io)?;
        if file.metadata().map_err(io)?.len() >= SEAL_AT {
            self.seal()?;
        }
        Ok(())
    }

    /// The claims of the open segment, in the order they were appended.
    ///
    /// A head that does not exist yet holds none — that is an empty log,
    /// not an error.
    ///
    /// # Errors
    ///
    /// Everything [`head_contents`](Log::head_contents) can answer.
    pub fn head(&self) -> Result<Vec<Claim>> {
        self.head_contents().map(Contents::into_claims)
    }

    /// The open segment whole: the last sealed segment as the head names
    /// it, and the claims appended since.
    ///
    /// A head that does not exist yet, or an empty file in its place,
    /// names nothing and holds nothing.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the file cannot be read, and everything
    /// [`contents`](Log::contents) can answer for a segment that is broken.
    pub fn head_contents(&self) -> Result<Contents> {
        let nothing = Contents {
            previous: None,
            claims: Vec::new(),
        };
        match fs::read_to_string(&self.head) {
            Ok(text) if text.is_empty() => Ok(nothing),
            Ok(text) => parse_segment(&text),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(nothing),
            Err(source) => Err(Error::Io {
                context: format!("{}: reading", self.head.display()),
                source,
            }),
        }
    }

    /// Whether the head was lost: it is missing or empty while the
    /// claims store holds sealed segments.
    ///
    /// Every seal leaves a head behind that names the segment just
    /// sealed, so an archive in order is never in this state. A head
    /// begun anew here would name no predecessor and start a second
    /// record that the end of the first does not reach. `ossuary-fix
    /// chain` begins it after the last sealed segment instead.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the head cannot be looked at, and
    /// [`Error::Store`] when the claims store cannot be walked.
    pub fn head_lost(&self) -> Result<bool> {
        Ok(!self.head_begun()? && self.holds_segments()?)
    }

    /// Refuse a run that would write claims while the head is lost.
    ///
    /// [`append`](Log::append) refuses each claim on its own; a run that
    /// stores bytes or asks an extractor or a server before recording
    /// what it got asks this first, so that it stops before any of that
    /// work.
    ///
    /// # Errors
    ///
    /// [`Error::HeadLost`] when the head was lost, and everything
    /// [`head_lost`](Log::head_lost) can answer.
    pub fn writable(&self) -> Result<()> {
        if self.head_lost()? {
            return Err(Error::HeadLost(self.head.clone()));
        }
        Ok(())
    }

    /// Whether the head file exists and is not empty. An empty file
    /// counts as no head: a head is written header first, in one write,
    /// so an empty one holds nothing.
    fn head_begun(&self) -> Result<bool> {
        match fs::metadata(&self.head) {
            Ok(metadata) => Ok(metadata.len() > 0),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(Error::Io {
                context: format!("{}: reading", self.head.display()),
                source,
            }),
        }
    }

    /// Whether the claims store holds any sealed segment.
    fn holds_segments(&self) -> Result<bool> {
        Ok(self.store.entries().next().transpose()?.is_some())
    }

    /// Seal the open segment: its bytes go into the store verbatim, and a
    /// fresh head takes its place, naming the segment just sealed.
    ///
    /// `None` when there is nothing to seal: no head, an empty file, or a
    /// head of no claims. The whole head is validated first, so nothing
    /// broken is ever immured; and because the store is content-addressed,
    /// a run that was interrupted between storing and starting the fresh
    /// head simply seals the same bytes again as a no-op.
    ///
    /// # Errors
    ///
    /// Everything [`head`](Log::head) can answer, [`Error::Store`] from the
    /// store, and [`Error::Io`] replacing the head file.
    pub fn seal(&self) -> Result<Option<Segment>> {
        let text = match fs::read_to_string(&self.head) {
            Ok(text) if text.is_empty() => return Ok(None),
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(Error::Io {
                    context: format!("{}: reading", self.head.display()),
                    source,
                });
            }
        };
        if parse_segment(&text)?.claims.is_empty() {
            return Ok(None);
        }
        let (_, entry) = self.store.add(text.as_bytes())?;

        // The fresh head goes in over the old one by rename, so the head is
        // never half a file: a crash before the rename leaves the old head
        // standing, and sealing it again is the no-op above.
        let io = |context: &str| {
            let context = format!("{}: {context}", self.head.display());
            move |source| Error::Io { context, source }
        };
        let mut name = self.head.file_name().unwrap_or_default().to_os_string();
        name.push(".tmp");
        let tmp = self.head.with_file_name(name);
        fs::write(&tmp, head_after(entry.digest())).map_err(io("writing the new head"))?;
        fs::rename(&tmp, &self.head).map_err(io("replacing the head"))?;

        Ok(Some(Segment {
            digest: entry.digest().clone(),
        }))
    }

    /// Every sealed segment in log order: along the chain, from the
    /// first segment to the one the open head names.
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

    /// Walk back along the chain from the open head, one segment at a
    /// time: for a reader that can stop at a segment it already knows,
    /// as the index does.
    ///
    /// A lost head refuses the walk before it begins, and a step to a
    /// predecessor the store does not hold is an error: the order of
    /// whatever lies beyond a break is not known, so nothing reads
    /// around one.
    ///
    /// # Errors
    ///
    /// [`Error::HeadLost`] when the head was lost, and everything
    /// [`head_contents`](Log::head_contents) can answer. A step answers
    /// [`Error::ChainBroken`] for a predecessor the store does not hold,
    /// and everything [`previous`](Log::previous) can answer otherwise.
    pub fn walk_back(&self) -> Result<WalkBack<'_>> {
        if self.head_lost()? {
            return Err(Error::HeadLost(self.head.clone()));
        }
        let head = self.head_contents()?;
        Ok(WalkBack {
            log: self,
            next: head
                .previous
                .map(|digest| ("the open segment".to_string(), digest)),
        })
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

    /// The claims store itself: for the audit and for repair tools that
    /// read every sealed segment as the store holds it.
    #[must_use]
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The open segment's file, whether or not it exists yet.
    #[must_use]
    pub fn head_path(&self) -> &Path {
        &self.head
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
        parse_segment(&text)
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

/// Header first, claims after, strict throughout.
fn parse_segment(text: &str) -> Result<Contents> {
    let mut lines = text.lines().enumerate();
    let first = lines.next().map(|(_, line)| line).unwrap_or_default();
    let previous = parse_header(first)?;
    let mut claims = Vec::new();
    for (index, line) in lines {
        let claim = Claim::parse_line(line).map_err(|source| Error::BadLine {
            line: index + 1,
            source: Box::new(source),
        })?;
        claims.push(claim);
    }
    Ok(Contents { previous, claims })
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
        Log::new(store_in(dir), dir.path().join("head.jsonl"))
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

    #[test]
    fn the_head_begins_with_the_header_and_grows_by_lines() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);

        log.append(&claim("holiday", "2026-09-01T21:14:03Z"))
            .unwrap();
        log.append(&claim("beach", "2026-09-01T21:14:04Z")).unwrap();

        let text = fs::read_to_string(dir.path().join("head.jsonl")).unwrap();
        assert!(
            text.starts_with("{\"ossuary-segment\":1}\n"),
            "the segment names its own format before anything else"
        );
        assert_eq!(text.lines().count(), 3);
        assert_eq!(log.head().unwrap().len(), 2);
    }

    #[test]
    fn a_log_without_a_head_is_empty_rather_than_broken() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);

        assert_eq!(log.head().unwrap(), Vec::new());
        assert_eq!(log.seal().unwrap(), None, "and there is nothing to seal");
        assert!(!log.head_lost().unwrap(), "an archive with nothing sealed");
    }

    #[test]
    fn a_head_lost_over_sealed_segments_is_not_begun_anew() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("sealed", "2026-09-01T00:00:00Z"))
            .unwrap();
        log.seal().unwrap().unwrap();
        assert!(!log.head_lost().unwrap(), "a seal leaves a head behind");
        let head = dir.path().join("head.jsonl");
        fs::remove_file(&head).unwrap();

        assert!(log.head_lost().unwrap());
        assert!(matches!(
            log.append(&claim("anew", "2026-09-02T00:00:00Z")),
            Err(Error::HeadLost(path)) if path == head
        ));
        assert!(!head.exists(), "nothing was begun");
    }

    #[test]
    fn an_empty_head_over_sealed_segments_was_lost_as_well() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("sealed", "2026-09-01T00:00:00Z"))
            .unwrap();
        log.seal().unwrap().unwrap();
        fs::write(dir.path().join("head.jsonl"), "").unwrap();

        assert!(log.head_lost().unwrap());
        assert!(log.head_contents().unwrap().previous().is_none());
        assert_eq!(log.seal().unwrap(), None);
        assert!(matches!(
            log.append(&claim("anew", "2026-09-02T00:00:00Z")),
            Err(Error::HeadLost(_))
        ));
    }

    #[test]
    fn sealing_stores_the_bytes_verbatim_and_begins_a_fresh_head() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("holiday", "2026-09-01T21:14:03Z"))
            .unwrap();
        let before = fs::read_to_string(dir.path().join("head.jsonl")).unwrap();

        let segment = log.seal().unwrap().expect("one claim seals");

        let claims = log.read(segment.digest()).unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(
            log.store.read(segment.digest()).unwrap().unwrap(),
            before.into_bytes(),
            "what was appended is what was sealed, byte for byte"
        );
        assert_eq!(
            log.head().unwrap(),
            Vec::new(),
            "the fresh head holds the header and nothing else"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("head.jsonl")).unwrap(),
            format!(
                "{{\"ossuary-segment\":1,\"previous\":\"{}\"}}\n",
                segment.digest()
            ),
            "and the header names the segment just sealed"
        );
        assert_eq!(log.seal().unwrap(), None, "an empty head does not seal");
    }

    #[test]
    fn a_seal_interrupted_before_the_fresh_head_seals_the_same_bytes_again() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("holiday", "2026-09-01T21:14:03Z"))
            .unwrap();
        let head = dir.path().join("head.jsonl");
        let before = fs::read_to_string(&head).unwrap();

        let first = log.seal().unwrap().unwrap();
        // The crash: stored, but the old head still stands.
        fs::write(&head, before).unwrap();
        let again = log.seal().unwrap().unwrap();

        assert_eq!(
            first.digest(),
            again.digest(),
            "identical bytes, identical name — the store dedups, the log inherits it"
        );
    }

    #[test]
    fn every_segment_names_the_one_sealed_before_it() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);

        log.append(&claim("first", "2026-09-01T00:00:00Z")).unwrap();
        let first = log.seal().unwrap().unwrap();
        log.append(&claim("second", "2026-09-02T00:00:00Z"))
            .unwrap();
        let second = log.seal().unwrap().unwrap();
        log.append(&claim("open", "2026-09-03T00:00:00Z")).unwrap();

        assert_eq!(
            log.contents(first.digest()).unwrap().previous(),
            None,
            "the first segment of an archive follows nothing"
        );
        assert_eq!(
            log.contents(second.digest()).unwrap().previous(),
            Some(first.digest())
        );
        assert_eq!(
            log.previous(second.digest()).unwrap().as_ref(),
            Some(first.digest()),
            "the header alone says the same"
        );
        let head = log.head_contents().unwrap();
        assert_eq!(head.previous(), Some(second.digest()));
        assert_eq!(head.claims().len(), 1);
        assert_ne!(
            first.digest(),
            second.digest(),
            "the same claim text after a seal is a new segment: its header names a different predecessor"
        );
    }

    #[test]
    fn a_header_member_this_build_does_not_know_is_passed_over() {
        let dir = TempDir::new().unwrap();
        let store = store_in(&dir);
        let mut text = String::from("{\"ossuary-segment\":1,\"signature\":\"ed25519:…\"}\n");
        text.push_str(&claim("holiday", "2026-09-01T21:14:03Z").to_line());
        text.push('\n');
        let (_, entry) = store.add(text.as_bytes()).unwrap();
        let log = Log::new(store, dir.path().join("head.jsonl"));

        let contents = log.contents(entry.digest()).unwrap();

        assert_eq!(contents.claims().len(), 1);
        assert!(contents.previous().is_none());
    }

    #[test]
    fn a_predecessor_that_is_not_a_digest_is_a_broken_header() {
        let dir = TempDir::new().unwrap();
        let store = store_in(&dir);
        let (_, entry) = store
            .add(b"{\"ossuary-segment\":1,\"previous\":\"the one before\"}\n")
            .unwrap();
        let log = Log::new(store, dir.path().join("head.jsonl"));

        assert!(matches!(
            log.contents(entry.digest()),
            Err(Error::SegmentHeader(_))
        ));
    }

    #[test]
    fn a_predecessor_list_is_refused_for_what_it_is() {
        // The form ossuary 0.14 and earlier wrote; ossuary-fix rewrites it.
        let dir = TempDir::new().unwrap();
        let store = store_in(&dir);
        let line = format!(
            "{{\"ossuary-segment\":1,\"previous\":[\"{}\"]}}\n",
            "ab".repeat(32)
        );
        let (_, entry) = store.add(line.as_bytes()).unwrap();
        let log = Log::new(store, dir.path().join("head.jsonl"));

        assert!(matches!(
            log.contents(entry.digest()),
            Err(Error::PreviousList)
        ));
        assert!(matches!(
            log.previous(entry.digest()),
            Err(Error::PreviousList)
        ));
    }

    #[test]
    fn segments_stand_in_chain_order_whatever_their_claims_say() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);

        // The later timestamp sealed first, as a clock set back leaves it.
        log.append(&claim("later", "2026-09-02T00:00:00Z")).unwrap();
        let later = log.seal().unwrap().unwrap();
        log.append(&claim("earlier", "2026-09-01T00:00:00Z"))
            .unwrap();
        let earlier = log.seal().unwrap().unwrap();

        let segments = log.segments().unwrap();
        assert_eq!(
            segments.iter().map(Segment::digest).collect::<Vec<_>>(),
            [later.digest(), earlier.digest()],
            "log order is the order of the chain"
        );
    }

    fn lose(log: &Log, segment: &Segment) {
        let path = log.store().find(segment.digest()).unwrap().unwrap();
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn the_walk_back_stops_at_a_lost_segment_and_names_who_named_it() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("first", "2026-09-01T00:00:00Z")).unwrap();
        log.seal().unwrap().unwrap();
        log.append(&claim("second", "2026-09-02T00:00:00Z"))
            .unwrap();
        let second = log.seal().unwrap().unwrap();
        log.append(&claim("third", "2026-09-03T00:00:00Z")).unwrap();
        let third = log.seal().unwrap().unwrap();
        lose(&log, &second);

        let mut walk = log.walk_back().unwrap();
        assert_eq!(walk.next().unwrap().unwrap(), third);
        match walk.next() {
            Some(Err(Error::ChainBroken { segment, missing })) => {
                assert_eq!(segment, format!("segment {}", third.digest()));
                assert_eq!(missing, second.digest().as_str());
            }
            other => panic!("expected the break, got {other:?}"),
        }
        assert!(walk.next().is_none(), "and nothing beyond it");
        assert!(matches!(log.segments(), Err(Error::ChainBroken { .. })));
    }

    #[test]
    fn a_lost_head_refuses_the_walk() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("sealed", "2026-09-01T00:00:00Z"))
            .unwrap();
        log.seal().unwrap().unwrap();
        fs::remove_file(dir.path().join("head.jsonl")).unwrap();

        assert!(matches!(log.walk_back(), Err(Error::HeadLost(_))));
    }

    #[test]
    fn an_append_that_fills_the_head_seals_it_by_itself() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        // A claim of three hundred thousand bytes: three of them leave the
        // head below the mebibyte, the fourth crosses it.
        let heavy = claim(&"x".repeat(300_000), "2026-09-05T12:00:00Z");

        for _ in 0..3 {
            log.append(&heavy).unwrap();
        }
        assert_eq!(
            log.segments().unwrap().len(),
            0,
            "below the threshold the head waits"
        );

        log.append(&heavy).unwrap();

        let segments = log.segments().unwrap();
        assert_eq!(segments.len(), 1, "the fourth append sealed in passing");
        assert_eq!(
            log.read(segments[0].digest()).unwrap().len(),
            4,
            "with everything appended so far inside"
        );
        assert_eq!(
            log.head().unwrap(),
            Vec::new(),
            "and a fresh head stands ready"
        );
    }

    #[test]
    fn a_digest_that_names_nothing_says_so() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let absent = Digest::parse(&"ab".repeat(32)).unwrap();

        assert!(matches!(
            log.read(&absent),
            Err(Error::SegmentMissing(digest)) if digest == absent.to_string()
        ));
    }

    #[test]
    fn a_segment_from_the_future_is_refused_for_what_it_is() {
        let dir = TempDir::new().unwrap();
        let store = store_in(&dir);
        let (_, entry) = store.add(b"{\"ossuary-segment\":2}\n").unwrap();
        let log = Log::new(store, dir.path().join("head.jsonl"));

        assert!(matches!(
            log.read(entry.digest()),
            Err(Error::SegmentGeneration(2))
        ));
    }

    #[test]
    fn what_is_not_a_segment_is_named_for_what_it_is_missing() {
        let dir = TempDir::new().unwrap();
        let store = store_in(&dir);
        let log = Log::new(store, dir.path().join("head.jsonl"));

        let (_, headless) = log.store.add(b"not a header\n").unwrap();
        assert!(matches!(
            log.read(headless.digest()),
            Err(Error::SegmentHeader(line)) if line == "not a header"
        ));

        let (_, torn) = log
            .store
            .add(b"{\"ossuary-segment\":1}\n{\"subject\":\"broken\n")
            .unwrap();
        assert!(matches!(
            log.read(torn.digest()),
            Err(Error::BadLine { line: 2, .. })
        ));
    }
}
