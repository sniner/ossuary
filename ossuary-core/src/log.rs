//! The log: where claims accumulate, and how segments come to be.
//!
//! Claims land in the one open segment — `head.jsonl` in the archive root,
//! the only mutable file an archive has. Sealing closes it: the file's bytes
//! go into the claims store verbatim as an ordinary entry, and a fresh head
//! begins. Nothing is reformatted on the way — what was appended is what is
//! sealed — and a sealed segment is never compacted, merged or rewritten:
//! superseded and retracted claims stay where they were written, which for
//! an archive is not a limitation but the point. The head seals itself
//! once it grows to [`SEAL_AT`]; sealing by hand remains for closing it
//! on demand.
//!
//! The head belongs to one writer at a time; concurrency lives in the store,
//! not here. An append does not fsync — the open segment's tail is the one
//! thing a crash may cost, and an ingest can say it again — while everything
//! sealed is durable the way the store promises. Reading is strict: a torn
//! last line stops the reader (and with it the seal) rather than being
//! quietly dropped, because a truth layer does not shrug.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;

use immure::{Digest, Store};
use serde::{Deserialize, Serialize};

use crate::claim::{Claim, Timestamp};
use crate::error::{Error, Result};
use crate::manifest::{Manifest, Manifests};

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
/// years still says what it is.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    #[serde(rename = "ossuary-segment")]
    generation: u32,
}

/// The generation alone, read leniently — see [`parse_segment`] for why the
/// strict read comes second.
#[derive(Debug, Deserialize)]
struct Generation {
    #[serde(rename = "ossuary-segment")]
    generation: u32,
}

/// The header as it stands in a file, newline included.
fn header_line() -> String {
    let header = serde_json::to_string(&Header {
        generation: GENERATION,
    });
    // A struct of one number serialises; see `Claim::to_line` for the
    // reasoning behind not pretending otherwise.
    let mut line = header.expect("a header serialises");
    line.push('\n');
    line
}

/// A sealed segment: its name in the store, and where it stands in the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    digest: Digest,
    first: Option<Timestamp>,
}

impl Segment {
    /// The entry the segment lies under in the claims store.
    #[must_use]
    pub fn digest(&self) -> &Digest {
        &self.digest
    }

    /// When the segment's first claim was recorded — its place in the log.
    ///
    /// `None` for a segment of no claims at all, which this crate never
    /// seals but a reader must not choke on.
    #[must_use]
    pub fn first_claim_at(&self) -> Option<&Timestamp> {
        self.first.as_ref()
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
    manifests: Option<Manifests>,
}

impl Log {
    /// The log kept in this claims store, with its open segment at `head`.
    #[must_use]
    pub fn new(store: Store, head: impl Into<PathBuf>) -> Self {
        Log {
            store,
            head: head.into(),
            manifests: None,
        }
    }

    /// The same log, keeping a manifest drawer at `dir`.
    ///
    /// With a drawer, sealing files each fresh segment's manifest in
    /// passing and [`segments`](Log::segments) answers from manifests
    /// instead of opening every segment. Without one — a bare `Log` — the
    /// walk reads segments whole, as a cacheless reader must.
    #[must_use]
    pub fn with_manifests(mut self, dir: impl Into<PathBuf>) -> Self {
        self.manifests = Some(Manifests::new(dir));
        self
    }

    /// Append one claim to the open segment.
    ///
    /// A head that is not there yet is begun, header first. Header and
    /// claim go out in one write, and nothing is fsynced — see the module
    /// notes on what a crash may cost and what it may not.
    ///
    /// An append that grows the head to [`SEAL_AT`] seals it in the same
    /// breath, so no writer needs a sealing policy of its own. The claim
    /// is on the record before the seal begins — trouble sealing is still
    /// reported, but the head stands with everything in it, and a later
    /// append crosses the threshold again.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the file cannot be opened or written, and
    /// everything [`seal`](Log::seal) can answer when this append crossed
    /// the threshold.
    pub fn append(&self, claim: &Claim) -> Result<()> {
        let io = |source| Error::Io {
            context: format!("{}: appending", self.head.display()),
            source,
        };
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.head)
            .map_err(io)?;
        let fresh = file.metadata().map_err(io)?.len() == 0;
        let mut lines = String::new();
        if fresh {
            lines.push_str(&header_line());
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
    /// [`Error::Io`] when the file cannot be read, and everything
    /// [`read`](Log::read) can answer for a segment that is broken.
    pub fn head(&self) -> Result<Vec<Claim>> {
        match fs::read_to_string(&self.head) {
            Ok(text) => parse_segment(&text),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(source) => Err(Error::Io {
                context: format!("{}: reading", self.head.display()),
                source,
            }),
        }
    }

    /// Seal the open segment: its bytes go into the store verbatim, and a
    /// fresh head takes its place.
    ///
    /// `None` when there is nothing to seal — no head, or a head of no
    /// claims. The whole head is validated first, so nothing broken is ever
    /// immured; and because the store is content-addressed, a run that was
    /// interrupted between storing and starting the fresh head simply seals
    /// the same bytes again as a no-op. With a manifest drawer, the fresh
    /// segment's manifest is filed in passing — best effort, and never a
    /// reason for a seal that succeeded to say otherwise.
    ///
    /// # Errors
    ///
    /// Everything [`head`](Log::head) can answer, [`Error::Store`] from the
    /// store, and [`Error::Io`] replacing the head file.
    pub fn seal(&self) -> Result<Option<Segment>> {
        let text = match fs::read_to_string(&self.head) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(Error::Io {
                    context: format!("{}: reading", self.head.display()),
                    source,
                });
            }
        };
        let claims = parse_segment(&text)?;
        if claims.is_empty() {
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
        fs::write(&tmp, header_line()).map_err(io("beginning the fresh head"))?;
        fs::rename(&tmp, &self.head).map_err(io("replacing the head"))?;

        if let Some(manifests) = &self.manifests {
            // Filed in passing, best effort: the seal has already
            // succeeded, and cache trouble never outranks the truth
            // operation it decorates — a manifest that could not be
            // written is merely absent, and segments() rebuilds it.
            let _ = manifests.store(&Manifest::of(entry.digest(), &claims));
        }

        Ok(Some(Segment {
            digest: entry.digest().clone(),
            first: claims.first().map(|claim| claim.time().clone()),
        }))
    }

    /// Every sealed segment, in log order: by the time of their first
    /// claims, ties broken by digest.
    ///
    /// The order is needed only to break same-second ties *across* segments
    /// — within one, the order of lines rules. With a manifest drawer the
    /// manifests answer, and a segment whose manifest is missing or will
    /// not read is read whole once and its manifest filed in passing —
    /// self-healing, so deleting the drawer costs one such walk. A bare
    /// `Log` reads every segment whole, which at least validates the
    /// entire log on the way.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] from the walk, and everything [`read`](Log::read)
    /// can answer for a segment that is broken.
    pub fn segments(&self) -> Result<Vec<Segment>> {
        let mut segments = Vec::new();
        for entry in self.store.entries() {
            let entry = entry?;
            let drawer = self.manifests.as_ref();
            let manifest = if let Some(manifest) = drawer.and_then(|m| m.load(entry.digest())) {
                manifest
            } else {
                let claims = self.read(entry.digest())?;
                let manifest = Manifest::of(entry.digest(), &claims);
                if let Some(manifests) = drawer {
                    // Best effort, as at sealing time: an unwritable
                    // drawer merely keeps the walk expensive.
                    let _ = manifests.store(&manifest);
                }
                manifest
            };
            segments.push(Segment {
                digest: entry.digest().clone(),
                first: manifest.first_claim_at().cloned(),
            });
        }
        segments.sort_by(|a, b| a.first.cmp(&b.first).then_with(|| a.digest.cmp(&b.digest)));
        Ok(segments)
    }

    /// The claims store itself — the audit's door to walking every
    /// sealed segment as the store holds it, manifests left out of it.
    pub(crate) fn store(&self) -> &Store {
        &self.store
    }

    /// Read one sealed segment back: its claims, in the order recorded.
    ///
    /// # Errors
    ///
    /// [`Error::SegmentMissing`] when the digest names nothing,
    /// [`Error::NotText`] and [`Error::SegmentHeader`] when what it names is
    /// not a segment, [`Error::SegmentGeneration`] for one from a newer
    /// build, and [`Error::BadLine`] naming the first line that would not
    /// read back.
    pub fn read(&self, digest: &Digest) -> Result<Vec<Claim>> {
        let bytes = self
            .store
            .read(digest)?
            .ok_or_else(|| Error::SegmentMissing(digest.to_string()))?;
        let text = String::from_utf8(bytes).map_err(|_| Error::NotText)?;
        parse_segment(&text)
    }
}

/// Header first, claims after, strict throughout.
fn parse_segment(text: &str) -> Result<Vec<Claim>> {
    let mut lines = text.lines().enumerate();
    let first = lines.next().map(|(_, line)| line).unwrap_or_default();
    // The generation is read leniently before the header is read strictly:
    // a future generation may add members to its header, and it must be
    // refused as what it is — newer — not reported as broken.
    let generation: Generation =
        serde_json::from_str(first).map_err(|_| Error::SegmentHeader(first.to_string()))?;
    if generation.generation != GENERATION {
        return Err(Error::SegmentGeneration(generation.generation));
    }
    let _: Header =
        serde_json::from_str(first).map_err(|_| Error::SegmentHeader(first.to_string()))?;
    let mut claims = Vec::new();
    for (index, line) in lines {
        let claim = Claim::parse_line(line).map_err(|source| Error::BadLine {
            line: index + 1,
            source: Box::new(source),
        })?;
        claims.push(claim);
    }
    Ok(claims)
}

#[cfg(test)]
mod tests {
    use immure::Store;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::claim::{Attribute, Source, Subject};

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
    }

    #[test]
    fn sealing_stores_the_bytes_verbatim_and_begins_a_fresh_head() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("holiday", "2026-09-01T21:14:03Z"))
            .unwrap();
        let before = fs::read_to_string(dir.path().join("head.jsonl")).unwrap();

        let segment = log.seal().unwrap().expect("one claim seals");

        assert_eq!(
            segment.first_claim_at().unwrap().as_str(),
            "2026-09-01T21:14:03Z"
        );
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
        assert_eq!(log.seal().unwrap(), None, "an empty head does not seal");
    }

    #[test]
    fn sealing_the_same_claims_twice_is_the_same_segment() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);

        log.append(&claim("holiday", "2026-09-01T21:14:03Z"))
            .unwrap();
        let first = log.seal().unwrap().unwrap();
        log.append(&claim("holiday", "2026-09-01T21:14:03Z"))
            .unwrap();
        let again = log.seal().unwrap().unwrap();

        assert_eq!(
            first.digest(),
            again.digest(),
            "identical bytes, identical name — the store dedups, the log inherits it"
        );
    }

    #[test]
    fn segments_stand_in_the_order_of_their_first_claims() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);

        // Sealed out of order: the later claims first.
        log.append(&claim("later", "2026-09-02T00:00:00Z")).unwrap();
        let later = log.seal().unwrap().unwrap();
        log.append(&claim("earlier", "2026-09-01T00:00:00Z"))
            .unwrap();
        let earlier = log.seal().unwrap().unwrap();

        let segments = log.segments().unwrap();
        assert_eq!(
            segments.iter().map(Segment::digest).collect::<Vec<_>>(),
            [earlier.digest(), later.digest()],
            "log order is the claims' order, not the sealing order"
        );
    }

    fn manifested_log_in(dir: &TempDir) -> Log {
        Log::new(store_in(dir), dir.path().join("head.jsonl"))
            .with_manifests(dir.path().join("manifests"))
    }

    fn manifest_path(dir: &TempDir, segment: &Segment) -> std::path::PathBuf {
        dir.path()
            .join("manifests")
            .join(format!("{}.json", segment.digest()))
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
    fn sealing_files_a_manifest_in_passing() {
        let dir = TempDir::new().unwrap();
        let log = manifested_log_in(&dir);
        log.append(&claim("holiday", "2026-09-01T21:14:03Z"))
            .unwrap();

        let segment = log.seal().unwrap().unwrap();

        let filed = fs::read_to_string(manifest_path(&dir, &segment)).unwrap();
        assert!(
            filed.starts_with("{\"ossuary-manifest\":1,"),
            "the drawer holds the fresh segment's manifest"
        );
    }

    #[test]
    fn segments_answer_from_the_drawer_without_opening_segments() {
        let dir = TempDir::new().unwrap();
        let log = manifested_log_in(&dir);
        log.append(&claim("holiday", "2026-09-01T21:14:03Z"))
            .unwrap();
        let segment = log.seal().unwrap().unwrap();

        // A manifest planted over the filed one, answering with a different
        // first claim: segments() repeating it proves the segment stayed
        // shut — cheekily, but a cache that is asked is a cache that works.
        let planted = Manifest::of(
            segment.digest(),
            &[claim("planted", "2001-01-01T00:00:00Z")],
        );
        Manifests::new(dir.path().join("manifests"))
            .store(&planted)
            .unwrap();

        let segments = log.segments().unwrap();
        assert_eq!(
            segments[0].first_claim_at().unwrap().as_str(),
            "2001-01-01T00:00:00Z"
        );
    }

    #[test]
    fn a_missing_manifest_is_rebuilt_in_passing() {
        let dir = TempDir::new().unwrap();
        let log = manifested_log_in(&dir);
        log.append(&claim("holiday", "2026-09-01T21:14:03Z"))
            .unwrap();
        let segment = log.seal().unwrap().unwrap();
        fs::remove_file(manifest_path(&dir, &segment)).unwrap();

        let segments = log.segments().unwrap();

        assert_eq!(
            segments[0].first_claim_at().unwrap().as_str(),
            "2026-09-01T21:14:03Z",
            "the walk fell back to the segment itself"
        );
        assert!(
            manifest_path(&dir, &segment).exists(),
            "and refilled the drawer on the way — deleting it costs one walk"
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
