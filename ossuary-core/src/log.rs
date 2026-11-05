//! The log: where claims accumulate, and how segments come to be.
//!
//! Claims land in the one open segment — `head.jsonl` in the archive root,
//! the only mutable file an archive has. Sealing closes it: the file's bytes
//! go into the claims store verbatim as an ordinary entry, and a fresh head
//! begins. Nothing is reformatted on the way — what was appended is what is
//! sealed, and a sealed segment is never compacted, combined or rewritten:
//! superseded and retracted claims stay where they were written, which for
//! an archive is not a limitation but the point. An append that finds the
//! head at [`SEAL_AT`] or larger seals it first; sealing by hand remains
//! for closing it on demand.
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
//! A seal replaces the head by rename, by way of `head.jsonl.tmp`. On a
//! local filesystem the rename is atomic. macOS on an SMB mount is not:
//! it removes the old head and then renames the new one onto the free
//! name, and a run that stops between the two leaves no head, only the
//! `.tmp`. That `.tmp` is a witness: a seal writes it only after the
//! segment it names is stored, so it says that the head was exactly
//! that segment when the seal began. A missing head whose `.tmp` reads
//! as a segment and names the last sealed one is therefore a seal that
//! stopped before its last step, not a lost head: a writer moves the
//! `.tmp` into place, a reader reads it as the open segment, and nobody
//! has to decide anything. Only a missing head counts. Nothing that
//! replaces the head leaves an empty one next to a `.tmp`, so a `.tmp`
//! beside an empty head vouches for an older head, and the head stays
//! lost.
//!
//! The witness is only worth something if it is never stale, which is
//! why a `.tmp` next to a standing head is cleared: by a replacement
//! that failed while the old head still stood, and by every run that
//! writes, before it writes. Cleared means removed, or emptied where it
//! cannot be removed, since an empty file vouches for nothing. What
//! remains is a seal whose rename failed and whose `.tmp` could be
//! neither removed nor emptied (that is reported), followed by the loss
//! of the head before the next writer. Then the claims since the
//! witness read as never written rather than as lost: the same loss at
//! the end of the chain that only a seal kept outside the archive shows.
//!
//! The head belongs to one writer at a time; concurrency lives in the store,
//! not here. An append is flushed to the device before it returns, and a
//! head that is created or renamed into place also has its directory entry
//! flushed. A claim that an append reported as written survives a crash,
//! so a cache folded from the head never holds a claim the head loses.
//! Everything sealed is durable the way the store promises.
//!
//! Reading is strict: a torn last line stops the reader (and with it the
//! seal) rather than being quietly dropped, because a truth layer does not
//! shrug.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{BufRead as _, BufReader, ErrorKind, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use immure::{Digest, Store};
use serde::{Deserialize, Serialize};

use crate::claim::Claim;
use crate::error::{Error, Result};

/// The format generation this build writes, and the only one it reads.
///
/// Generation 1 is `docs/format.md`. A segment declaring any other is
/// refused as [`Error::SegmentGeneration`] rather than guessed at.
pub const GENERATION: u32 = 1;

/// The open segment is sealed by the first append that finds it at this
/// size or larger.
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

/// The size and modification time of the open segment's file, from one
/// `stat`. A reader that keeps its place in the head compares two stamps
/// to tell whether anything was appended or sealed since it last read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    /// The size in bytes.
    pub size: u64,
    /// The modification time in nanoseconds since the Unix epoch; `None`
    /// where the filesystem keeps none.
    pub modified: Option<i64>,
}

impl Stamp {
    fn of(metadata: &fs::Metadata) -> Stamp {
        let modified =
            metadata
                .modified()
                .ok()
                .and_then(|time| match time.duration_since(UNIX_EPOCH) {
                    Ok(after) => i64::try_from(after.as_nanos()).ok(),
                    Err(before) => i64::try_from(before.duration().as_nanos())
                        .ok()
                        .map(|nanos| -nanos),
                });
        Stamp {
            size: metadata.len(),
            modified,
        }
    }
}

/// The open segment, opened by [`Log::open_head`] with its header read,
/// for reading on from a byte offset.
#[derive(Debug)]
pub struct OpenHead {
    reader: BufReader<fs::File>,
    path: PathBuf,
    previous: Option<Digest>,
    /// The offset of the first claim: the length of the header line.
    claims_at: u64,
    stamp: Stamp,
}

/// Claims read from the open segment, and the offset after them.
#[derive(Debug)]
pub struct Tail {
    /// The claims, in the order appended.
    pub claims: Vec<Claim>,
    /// The offset just after the last line read, where the next read
    /// begins.
    pub end: u64,
}

impl OpenHead {
    /// The last sealed segment as the head names it; `None` for a head
    /// in an archive with nothing sealed.
    #[must_use]
    pub fn previous(&self) -> Option<&Digest> {
        self.previous.as_ref()
    }

    /// The file's stamp, taken when it was opened, before anything was
    /// read: a later append changes it, so the next comparison sees it.
    #[must_use]
    pub fn stamp(&self) -> Stamp {
        self.stamp
    }

    /// The claims on the lines from byte `offset` on; `claims_before` is
    /// the number of claims in front of that offset, for the line number
    /// in an error. An offset inside the header reads from the first
    /// claim.
    ///
    /// Only complete lines are read. The bytes after the last newline
    /// can be an append that is still being written, and the next read
    /// from [`Tail::end`] takes them once the line is complete. Sealing
    /// and the audit read the head whole and refuse such a line.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the file cannot be read, [`Error::NotText`]
    /// for bytes that are not UTF-8, and [`Error::BadLine`] naming the
    /// first line that does not read as a claim.
    pub fn read_from(&mut self, offset: u64, claims_before: usize) -> Result<Tail> {
        let path = &self.path;
        let io = |source| Error::Io {
            context: format!("{}: reading", path.display()),
            source,
        };
        let offset = offset.max(self.claims_at);
        self.reader.seek(SeekFrom::Start(offset)).map_err(io)?;
        let mut bytes = Vec::new();
        self.reader.read_to_end(&mut bytes).map_err(io)?;
        let complete = bytes
            .iter()
            .rposition(|&byte| byte == b'\n')
            .map_or(0, |last| last + 1);
        let text = std::str::from_utf8(&bytes[..complete]).map_err(|_| Error::NotText)?;
        let mut claims = Vec::new();
        for (index, line) in text.lines().enumerate() {
            // The header is line 1, the first claim line 2.
            claims.push(Claim::parse_line(line).map_err(|source| Error::BadLine {
                line: claims_before + index + 2,
                source: Box::new(source),
            })?);
        }
        Ok(Tail {
            claims,
            end: offset + complete as u64,
        })
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

/// What the files of the open segment show; see the module notes on the
/// witness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeadState {
    /// Nothing is sealed and no head has been begun. An empty file in the
    /// head's place counts as no head.
    Absent,
    /// The head is there and holds `size` bytes.
    Open { size: u64 },
    /// The head is missing, and `head.jsonl.tmp` is the fresh head of a
    /// seal that stopped before its last step.
    Interrupted,
    /// The head is missing or empty while sealed segments exist, and
    /// nothing vouches for what it was.
    Lost,
}

/// What [`Log::writable`] had to do before a run may write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum Writable {
    /// The log takes claims as it is.
    Ready,
    /// A seal had stopped before its fresh head was in place, and the
    /// fresh head was moved into place. Nothing was lost.
    SealFinished,
}

impl Writable {
    /// What a run says about readying the log, or `None` when there was
    /// nothing to do. The head is named as it stands in the archive,
    /// which the run has named already.
    #[must_use]
    pub fn note(self) -> Option<&'static str> {
        match self {
            Writable::Ready => None,
            Writable::SealFinished => Some(
                "head.jsonl: the last seal stopped before the new open segment was in place; finished it, nothing was lost",
            ),
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

    /// Append one claim to the open segment, as [`append_all`](Log::append_all)
    /// does for several.
    ///
    /// # Errors
    ///
    /// As [`append_all`](Log::append_all).
    pub fn append(&self, claim: &Claim) -> Result<()> {
        self.append_all(std::slice::from_ref(claim))
    }

    /// Append claims to the open segment, in one write.
    ///
    /// A head that is not there yet is begun, header first, naming no
    /// predecessor, but only in an archive with nothing sealed: every
    /// seal leaves a head behind, so a head missing over sealed segments
    /// was lost (see [`head_lost`](Log::head_lost)), unless a seal
    /// stopped before its last step, and then its fresh head is moved
    /// into place first. Header and claims go out in one write, flushed
    /// to the device before this returns. Each call costs one flush, so a
    /// caller with many claims passes them together.
    ///
    /// A head at [`SEAL_AT`] or larger is sealed before anything is
    /// written, so no writer needs a sealing policy of its own. The order
    /// is what makes an error mean what it says: a seal that fails leaves
    /// the claims unwritten, and the caller can treat them as not
    /// recorded. The head may grow past the threshold by one append.
    ///
    /// # Errors
    ///
    /// [`Error::HeadLost`] when the head was lost, [`Error::Io`] when the
    /// file cannot be opened, written or flushed, and everything
    /// [`seal`](Log::seal) can answer when the head was due to be sealed.
    pub fn append_all(&self, claims: &[Claim]) -> Result<()> {
        if claims.is_empty() {
            return Ok(());
        }
        let io = |source| Error::Io {
            context: format!("{}: appending", self.head.display()),
            source,
        };
        let fresh = match self.head_state()? {
            HeadState::Absent => true,
            HeadState::Open { size } => {
                if size >= SEAL_AT {
                    self.seal()?;
                }
                false
            }
            HeadState::Interrupted => {
                self.finish_seal()?;
                false
            }
            HeadState::Lost => return Err(Error::HeadLost(self.head.clone())),
        };
        let mut lines = String::new();
        if fresh {
            lines.push_str(&header_line(None));
        }
        for claim in claims {
            lines.push_str(&claim.to_line());
            lines.push('\n');
        }
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.head)
            .map_err(io)?;
        file.write_all(lines.as_bytes()).map_err(io)?;
        immure::sync_file(&file).map_err(io)?;
        if fresh {
            sync_parent(&self.head);
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
    /// names nothing and holds nothing. While a seal is interrupted, the
    /// open segment is its fresh head (see [`head_text`](Log::head_text)).
    ///
    /// # Errors
    ///
    /// Everything [`head_text`](Log::head_text) can answer, and everything
    /// [`contents`](Log::contents) can answer for a segment that is broken.
    pub fn head_contents(&self) -> Result<Contents> {
        match self.head_text()? {
            Some(text) => parse_segment(&text),
            None => Ok(Contents {
                previous: None,
                claims: Vec::new(),
            }),
        }
    }

    /// The open segment's text, from the file the log reads it from:
    /// the head, or, while a seal is interrupted, the fresh head it left
    /// as `head.jsonl.tmp` (see the module notes). `None` when there is
    /// no head or an empty file in its place.
    ///
    /// Nothing outside the log reads the head's file itself; this is the
    /// way to its bytes.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when a file cannot be read, and everything the
    /// claims store can answer while looking for the last sealed segment.
    pub fn head_text(&self) -> Result<Option<String>> {
        match fs::read_to_string(&self.head) {
            Ok(text) if text.is_empty() => Ok(None),
            Ok(text) => Ok(Some(text)),
            Err(source) if source.kind() == ErrorKind::NotFound => self.witness(),
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
        Ok(self.head_state()? == HeadState::Lost)
    }

    /// Whether a seal stopped before its fresh head was in place: the
    /// head is missing, and `head.jsonl.tmp` vouches for it (see the
    /// module notes). Nothing is lost then; the next run that writes
    /// moves the fresh head into place.
    ///
    /// # Errors
    ///
    /// Everything [`head_lost`](Log::head_lost) can answer.
    pub fn seal_interrupted(&self) -> Result<bool> {
        Ok(self.head_state()? == HeadState::Interrupted)
    }

    /// Ready the log for a run that writes, or refuse the run while the
    /// head is lost.
    ///
    /// [`append`](Log::append) refuses each claim on its own; a run that
    /// stores bytes or asks an extractor or a server before recording
    /// what it got asks this first, so that it stops before any of that
    /// work. Readying finishes a seal that stopped before its last step
    /// and clears a `head.jsonl.tmp` left next to a standing head (see
    /// the module notes); the answer says which, so that the run can
    /// tell.
    ///
    /// # Errors
    ///
    /// [`Error::HeadLost`] when the head was lost, [`Error::LeftOver`]
    /// when a `head.jsonl.tmp` next to the head can be neither removed
    /// nor emptied, [`Error::Io`] when the fresh head of an interrupted
    /// seal cannot be moved into place, and everything
    /// [`head_lost`](Log::head_lost) can answer.
    pub fn writable(&self) -> Result<Writable> {
        match self.head_state()? {
            HeadState::Lost => Err(Error::HeadLost(self.head.clone())),
            HeadState::Interrupted => {
                self.finish_seal()?;
                Ok(Writable::SealFinished)
            }
            HeadState::Open { .. } => {
                self.clear_left_over()?;
                Ok(Writable::Ready)
            }
            HeadState::Absent => Ok(Writable::Ready),
        }
    }

    /// What the files of the open segment show. Telling an interrupted
    /// seal from a lost head reads the header of every sealed segment,
    /// but only when the head is missing.
    fn head_state(&self) -> Result<HeadState> {
        match fs::metadata(&self.head) {
            Ok(metadata) if metadata.len() > 0 => {
                return Ok(HeadState::Open {
                    size: metadata.len(),
                });
            }
            // An empty head has no witness, whatever lies next to it.
            Ok(_) => {
                return Ok(if self.holds_segments()? {
                    HeadState::Lost
                } else {
                    HeadState::Absent
                });
            }
            Err(source) if source.kind() == ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Error::Io {
                    context: format!("{}: reading", self.head.display()),
                    source,
                });
            }
        }
        if !self.holds_segments()? {
            return Ok(HeadState::Absent);
        }
        Ok(if self.witness()?.is_some() {
            HeadState::Interrupted
        } else {
            HeadState::Lost
        })
    }

    /// Where a replacement writes the new head before renaming it over
    /// the old one: `head.jsonl.tmp`, next to the head.
    fn tmp_path(&self) -> PathBuf {
        let mut name = self.head.file_name().unwrap_or_default().to_os_string();
        name.push(".tmp");
        self.head.with_file_name(name)
    }

    /// The text of `head.jsonl.tmp` when it vouches for the open
    /// segment: it reads as a segment, and the segment it names as its
    /// predecessor is the last sealed one. Asked only while the head is
    /// missing. A `.tmp` that does not read, or names anything else,
    /// vouches for nothing, and the head counts as lost.
    fn witness(&self) -> Result<Option<String>> {
        let tmp = self.tmp_path();
        let text = match fs::read_to_string(&tmp) {
            Ok(text) => text,
            Err(source)
                if matches!(source.kind(), ErrorKind::NotFound | ErrorKind::InvalidData) =>
            {
                return Ok(None);
            }
            Err(source) => {
                return Err(Error::Io {
                    context: format!("{}: reading", tmp.display()),
                    source,
                });
            }
        };
        let Ok(contents) = parse_segment(&text) else {
            return Ok(None);
        };
        let Some(named) = contents.previous else {
            return Ok(None);
        };
        Ok((self.last_sealed()? == Some(named)).then_some(text))
    }

    /// The last sealed segment: the one that no other segment names as
    /// its predecessor. `None` when there is none, or more than one, and
    /// the end of the chain cannot be told.
    fn last_sealed(&self) -> Result<Option<Digest>> {
        let mut held = BTreeSet::new();
        let mut named = BTreeSet::new();
        for entry in self.store.entries() {
            let digest = entry?.digest().clone();
            if let Some(previous) = self.previous(&digest)? {
                named.insert(previous);
            }
            held.insert(digest);
        }
        let mut ends = held.difference(&named);
        Ok(match (ends.next(), ends.next()) {
            (Some(end), None) => Some(end.clone()),
            _ => None,
        })
    }

    /// Move the fresh head of an interrupted seal into place. The head's
    /// name is free, so nothing is replaced on the way.
    fn finish_seal(&self) -> Result<()> {
        fs::rename(self.tmp_path(), &self.head).map_err(|source| Error::Io {
            context: format!("{}: finishing the interrupted seal", self.head.display()),
            source,
        })?;
        sync_parent(&self.head);
        Ok(())
    }

    /// Clear a `head.jsonl.tmp` that lies next to a standing head. It is
    /// what is left of a seal that failed or stopped before the old head
    /// went, and if it stayed, it would vouch for this head were the head
    /// to go missing after it had grown. The log has one writer at a
    /// time, so it is never the fresh head of a seal still running. An
    /// empty one vouches for nothing and is left.
    fn clear_left_over(&self) -> Result<()> {
        let tmp = self.tmp_path();
        match fs::metadata(&tmp) {
            Ok(metadata) if metadata.len() == 0 => Ok(()),
            Ok(_) => discard(&tmp).map_err(|cleanup| Error::LeftOver {
                path: tmp,
                cleanup,
                after: None,
            }),
            Err(source) if source.kind() == ErrorKind::NotFound => Ok(()),
            Err(source) => Err(Error::Io {
                context: format!("{}: reading", tmp.display()),
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
    /// head simply seals the same bytes again as a no-op. A seal that
    /// stopped after that, with the old head gone and the fresh one not
    /// yet in place, is finished, and leaves nothing to seal.
    ///
    /// # Errors
    ///
    /// Everything [`head`](Log::head) can answer, [`Error::Store`] from the
    /// store, and everything [`replace_head`](Log::replace_head) can answer.
    pub fn seal(&self) -> Result<Option<Segment>> {
        let text = match fs::read_to_string(&self.head) {
            Ok(text) if text.is_empty() => return Ok(None),
            Ok(text) => text,
            Err(source) if source.kind() == ErrorKind::NotFound => {
                if self.head_state()? == HeadState::Interrupted {
                    self.finish_seal()?;
                }
                return Ok(None);
            }
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
        self.replace_head(&head_after(entry.digest()))?;
        Ok(Some(Segment {
            digest: entry.digest().clone(),
        }))
    }

    /// Replace the open segment with `text`: written to
    /// `head.jsonl.tmp` next to it and renamed over it, so the head is
    /// never half a file. A crash before the rename leaves the old head
    /// standing; a crash inside a rename that is not atomic leaves the
    /// `.tmp` as the only head, and the next writer finishes it (see the
    /// module notes). The new head is flushed before the rename, so that
    /// a power failure right after it cannot leave an empty head, and the
    /// directory is flushed after it. A lost rename would leave the old
    /// head in place although its claims are sealed already, and the next
    /// seal would store them again in a second segment after the same
    /// predecessor.
    ///
    /// When writing or renaming fails while the old head still stands,
    /// the `.tmp` is cleared, so that it cannot vouch for a head that
    /// grows on. When the old head is gone, the `.tmp` is the head and
    /// stays.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the new head cannot be written or renamed into
    /// place, and [`Error::LeftOver`] when, after that, the `.tmp` can be
    /// neither removed nor emptied.
    pub fn replace_head(&self, text: &str) -> Result<()> {
        let tmp = self.tmp_path();
        let failed = |context: &str, source| Error::Io {
            context: format!("{}: {context}", self.head.display()),
            source,
        };
        let replaced = write_flushed(&tmp, text)
            .map_err(|source| failed("writing the new head", source))
            .and_then(|()| {
                fs::rename(&tmp, &self.head).map_err(|source| failed("replacing the head", source))
            });
        let Err(error) = replaced else {
            sync_parent(&self.head);
            return Ok(());
        };
        if matches!(self.head.try_exists(), Ok(true))
            && let Err(cleanup) = discard(&tmp)
        {
            return Err(Error::LeftOver {
                path: tmp,
                cleanup,
                after: Some(Box::new(error)),
            });
        }
        Err(error)
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
        Ok(self.walk_back_from(head.previous()))
    }

    /// Walk back along the chain from `segment`, the one the open head
    /// names: for a reader that has read the head's header itself, as
    /// [`OpenHead::previous`] gives it. `None` walks nothing. A step
    /// answers as in [`walk_back`](Log::walk_back).
    #[must_use]
    pub fn walk_back_from(&self, segment: Option<&Digest>) -> WalkBack<'_> {
        WalkBack {
            log: self,
            next: segment.map(|digest| ("the open segment".to_string(), digest.clone())),
        }
    }

    /// The stamp of the head's file, from one `stat`: `None` when there
    /// is no file with bytes in it. Nothing else is looked at, so this is
    /// cheap enough to ask before every read; whether a missing head was
    /// lost or a seal was interrupted is for [`open_head`](Log::open_head)
    /// to find out.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the file cannot be looked at.
    pub fn head_stamp(&self) -> Result<Option<Stamp>> {
        match fs::metadata(&self.head) {
            Ok(metadata) if metadata.len() > 0 => Ok(Some(Stamp::of(&metadata))),
            Ok(_) => Ok(None),
            Err(source) if source.kind() == ErrorKind::NotFound => Ok(None),
            Err(source) => Err(Error::Io {
                context: format!("{}: reading", self.head.display()),
                source,
            }),
        }
    }

    /// Open the open segment and read its header, for a reader that
    /// keeps its place in the head, as the index does: it reads the
    /// claims from where it stopped last time with
    /// [`OpenHead::read_from`]. `None` when there is no head yet. While a
    /// seal is interrupted, the open segment is its fresh head (see the
    /// module notes).
    ///
    /// # Errors
    ///
    /// [`Error::HeadLost`] when the head was lost, [`Error::Io`] when the
    /// file cannot be opened or read, [`Error::NotText`],
    /// [`Error::SegmentHeader`], [`Error::PreviousList`] and
    /// [`Error::SegmentGeneration`] for a header that does not read, and
    /// [`Error::Store`] while looking for the last sealed segment.
    pub fn open_head(&self) -> Result<Option<OpenHead>> {
        let path = match self.head_state()? {
            HeadState::Absent => return Ok(None),
            HeadState::Lost => return Err(Error::HeadLost(self.head.clone())),
            HeadState::Open { .. } => self.head.clone(),
            HeadState::Interrupted => self.tmp_path(),
        };
        let io = |source| Error::Io {
            context: format!("{}: reading", path.display()),
            source,
        };
        let file = fs::File::open(&path).map_err(io)?;
        let stamp = Stamp::of(&file.metadata().map_err(io)?);
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        reader.read_until(b'\n', &mut line).map_err(io)?;
        let header = String::from_utf8(line).map_err(|_| Error::NotText)?;
        let previous = parse_header(header.lines().next().unwrap_or_default())?;
        let claims_at = header.len() as u64;
        Ok(Some(OpenHead {
            reader,
            path,
            previous,
            claims_at,
            stamp,
        }))
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

/// Write `text` to `path` and flush it to the device. `immure::sync_file`
/// is the flush the stores use, which also works on SMB mounts on macOS.
fn write_flushed(path: &Path, text: &str) -> std::io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(text.as_bytes())?;
    immure::sync_file(&file)
}

/// Flush the directory entry of `path` after the file was created or
/// renamed into place. Best effort, as in the store: some filesystems do
/// not let a directory be opened or flushed, and then only this one name
/// may be lost in a crash.
fn sync_parent(path: &Path) {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    if let Ok(handle) = fs::File::open(dir) {
        immure::sync_file(&handle).ok();
    }
}

/// Clear a `head.jsonl.tmp` that must not vouch for anything: remove it,
/// or, where it cannot be removed, empty it, since an empty file does not
/// read as a segment. The removal can fail where the truncation does not:
/// Samba 4.22 up to 4.23.11 and 4.24.6 will not delete a file that has a
/// stream while the share's top directory holds an entry of the same name
/// (Samba bug 16144).
fn discard(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != ErrorKind::NotFound => OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)
            .map(drop),
        _ => Ok(()),
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
    fn an_append_that_finds_the_head_full_seals_it_first() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        // A claim of three hundred thousand bytes: three of them leave the
        // head below the mebibyte, the fourth crosses it.
        let heavy = claim(&"x".repeat(300_000), "2026-09-05T12:00:00Z");

        for _ in 0..4 {
            log.append(&heavy).unwrap();
        }
        assert_eq!(
            log.segments().unwrap().len(),
            0,
            "the append that crossed the threshold only wrote"
        );

        log.append(&heavy).unwrap();

        let segments = log.segments().unwrap();
        assert_eq!(segments.len(), 1, "the fifth append sealed first");
        assert_eq!(
            log.read(segments[0].digest()).unwrap().len(),
            4,
            "with the four claims before it inside"
        );
        assert_eq!(
            log.head().unwrap(),
            vec![heavy],
            "and the fifth claim in the fresh head"
        );
    }

    #[test]
    #[cfg(unix)]
    fn an_append_whose_seal_fails_writes_nothing() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let heavy = claim(&"x".repeat(300_000), "2026-09-05T12:00:00Z");
        for _ in 0..4 {
            log.append(&heavy).unwrap();
        }
        let before = fs::read(dir.path().join("head.jsonl")).unwrap();

        // A claims store that cannot take the segment.
        let claims = dir.path().join("claims");
        let saved = fs::metadata(&claims).unwrap().permissions();
        fs::set_permissions(&claims, fs::Permissions::from_mode(0o555)).unwrap();
        let appended = log.append(&claim("one more", "2026-09-05T12:00:01Z"));
        fs::set_permissions(&claims, saved).unwrap();

        assert!(appended.is_err(), "the failed seal is the append's error");
        assert_eq!(
            fs::read(dir.path().join("head.jsonl")).unwrap(),
            before,
            "and the claim was not written"
        );
        assert_eq!(log.segments().unwrap().len(), 0);
    }

    #[test]
    fn claims_appended_together_go_out_in_order() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        let claims = [
            claim("first", "2026-09-05T12:00:00Z"),
            claim("second", "2026-09-05T12:00:00Z"),
        ];

        log.append_all(&claims).unwrap();
        log.append_all(&[]).unwrap();

        assert_eq!(log.head().unwrap(), claims);
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

    /// A log one segment in, and its head as a seal on macOS over SMB
    /// leaves it when the run stops between removing the old head and
    /// renaming the new one: no head, the fresh head as `head.jsonl.tmp`.
    fn interrupted(dir: &TempDir) -> (Log, Segment) {
        let log = log_in(dir);
        log.append(&claim("sealed", "2026-09-01T00:00:00Z"))
            .unwrap();
        let segment = log.seal().unwrap().unwrap();
        let head = dir.path().join("head.jsonl");
        fs::rename(&head, dir.path().join("head.jsonl.tmp")).unwrap();
        (log, segment)
    }

    #[test]
    fn a_seal_stopped_after_the_old_head_went_is_not_a_lost_head() {
        let dir = TempDir::new().unwrap();
        let (log, segment) = interrupted(&dir);

        assert!(!log.head_lost().unwrap(), "the .tmp vouches for the head");
        assert!(log.seal_interrupted().unwrap());
        assert_eq!(
            log.head_contents().unwrap().previous(),
            Some(segment.digest())
        );
        assert_eq!(log.segments().unwrap(), vec![segment]);
        assert!(
            !dir.path().join("head.jsonl").exists(),
            "and reading wrote nothing"
        );
    }

    #[test]
    fn an_opened_head_is_the_fresh_head_of_an_interrupted_seal() {
        let dir = TempDir::new().unwrap();
        let (log, segment) = interrupted(&dir);

        assert_eq!(log.head_stamp().unwrap(), None, "no head file to stat");
        let mut head = log.open_head().unwrap().unwrap();

        assert_eq!(head.previous(), Some(segment.digest()));
        assert!(head.read_from(0, 0).unwrap().claims.is_empty());
        assert!(!dir.path().join("head.jsonl").exists(), "nothing written");
    }

    #[test]
    fn a_read_from_an_offset_names_the_line_in_the_file() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("first", "2026-09-01T00:00:00Z")).unwrap();
        let mut head = log.open_head().unwrap().unwrap();
        let after_first = head.read_from(0, 0).unwrap();
        assert_eq!(after_first.claims.len(), 1);
        let mut file = OpenOptions::new()
            .append(true)
            .open(log.head_path())
            .unwrap();
        file.write_all(b"not a claim\n").unwrap();

        let error = head.read_from(after_first.end, 1).unwrap_err();

        assert!(
            matches!(error, Error::BadLine { line: 3, .. }),
            "header, first claim, then the third line: {error:?}"
        );
    }

    #[test]
    fn the_next_append_finishes_an_interrupted_seal() {
        let dir = TempDir::new().unwrap();
        let (log, segment) = interrupted(&dir);

        log.append(&claim("after", "2026-09-02T00:00:00Z")).unwrap();

        assert!(!dir.path().join("head.jsonl.tmp").exists());
        let head = log.head_contents().unwrap();
        assert_eq!(head.previous(), Some(segment.digest()));
        assert_eq!(head.claims().len(), 1, "the claim, after the fresh header");
    }

    #[test]
    fn readying_the_log_finishes_an_interrupted_seal_and_says_so() {
        let dir = TempDir::new().unwrap();
        let (log, _) = interrupted(&dir);
        let head = dir.path().join("head.jsonl");

        assert_eq!(log.writable().unwrap(), Writable::SealFinished);
        assert!(head.exists());
        assert!(
            Writable::SealFinished
                .note()
                .unwrap()
                .ends_with("finished it, nothing was lost")
        );
        assert_eq!(log.writable().unwrap(), Writable::Ready, "once is enough");
        assert_eq!(Writable::Ready.note(), None);
    }

    #[test]
    fn sealing_finishes_an_interrupted_seal_and_finds_nothing_to_seal() {
        let dir = TempDir::new().unwrap();
        let (log, _) = interrupted(&dir);

        assert_eq!(log.seal().unwrap(), None);
        assert!(dir.path().join("head.jsonl").exists());
        assert!(!log.seal_interrupted().unwrap());
    }

    #[test]
    fn an_empty_head_beside_a_witness_stays_lost() {
        let dir = TempDir::new().unwrap();
        let (log, _) = interrupted(&dir);
        fs::write(dir.path().join("head.jsonl"), "").unwrap();

        assert!(log.head_lost().unwrap());
        assert!(!log.seal_interrupted().unwrap());
        assert!(matches!(log.writable(), Err(Error::HeadLost(_))));
    }

    #[test]
    fn a_witness_for_an_older_segment_vouches_for_nothing() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("first", "2026-09-01T00:00:00Z")).unwrap();
        let first = log.seal().unwrap().unwrap();
        log.append(&claim("second", "2026-09-02T00:00:00Z"))
            .unwrap();
        log.seal().unwrap().unwrap();
        fs::write(
            dir.path().join("head.jsonl.tmp"),
            head_after(first.digest()),
        )
        .unwrap();
        fs::remove_file(dir.path().join("head.jsonl")).unwrap();

        assert!(log.head_lost().unwrap());
    }

    #[test]
    fn a_witness_that_does_not_read_vouches_for_nothing() {
        let dir = TempDir::new().unwrap();
        let (log, _) = interrupted(&dir);
        let tmp = dir.path().join("head.jsonl.tmp");

        for text in [
            "",
            "{\"ossuary-segment\":1,\"previous\":",
            "not a segment\n",
        ] {
            fs::write(&tmp, text).unwrap();
            assert!(log.head_lost().unwrap(), "{text:?}");
        }
    }

    #[test]
    fn a_chain_with_two_ends_leaves_the_last_segment_untold() {
        let dir = TempDir::new().unwrap();
        let (log, _) = interrupted(&dir);
        // A second first segment that nothing follows: the end of the
        // chain is no longer one segment.
        let stray = format!(
            "{}{}\n",
            header_line(None),
            claim("stray", "2026-09-03T00:00:00Z").to_line()
        );
        log.store.add(stray.as_bytes()).unwrap();

        assert!(log.head_lost().unwrap());
    }

    #[test]
    fn readying_the_log_clears_a_left_over_beside_a_standing_head() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("sealed", "2026-09-01T00:00:00Z"))
            .unwrap();
        let segment = log.seal().unwrap().unwrap();
        let tmp = dir.path().join("head.jsonl.tmp");
        fs::write(&tmp, head_after(segment.digest())).unwrap();

        assert_eq!(log.writable().unwrap(), Writable::Ready);
        assert!(!tmp.exists(), "it would vouch for a head that grows on");
    }

    #[test]
    fn a_rename_refused_while_the_old_head_stands_clears_the_new_one() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        // A head the rename cannot replace: a directory that is not empty.
        let head = dir.path().join("head.jsonl");
        fs::create_dir(&head).unwrap();
        fs::write(head.join("inside"), "").unwrap();

        let replaced = log.replace_head("{\"ossuary-segment\":1}\n");

        assert!(matches!(replaced, Err(Error::Io { .. })), "{replaced:?}");
        assert!(!dir.path().join("head.jsonl.tmp").exists());
    }

    /// Run `with` while `path` has `mode`, and give it its mode back
    /// before anything is asserted, so that the directory can go.
    #[cfg(unix)]
    fn with_mode<T>(path: &Path, mode: u32, with: impl FnOnce() -> T) -> T {
        use std::os::unix::fs::PermissionsExt as _;

        let saved = fs::metadata(path).unwrap().permissions();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        let result = with();
        fs::set_permissions(path, saved).unwrap();
        result
    }

    #[test]
    #[cfg(unix)]
    fn a_rename_refused_after_the_old_head_went_keeps_the_new_one() {
        let dir = TempDir::new().unwrap();
        let (log, segment) = interrupted(&dir);
        let tmp = dir.path().join("head.jsonl.tmp");

        // A directory that takes no rename; the .tmp itself stays writable.
        let replaced = with_mode(dir.path(), 0o555, || {
            log.replace_head(&head_after(segment.digest()))
        });

        assert!(replaced.is_err());
        assert_eq!(
            fs::read_to_string(&tmp).unwrap(),
            head_after(segment.digest()),
            "the .tmp is the only head and stays"
        );
        assert!(log.seal_interrupted().unwrap());
    }

    #[test]
    #[cfg(unix)]
    fn a_left_over_that_cannot_be_removed_is_emptied() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("sealed", "2026-09-01T00:00:00Z"))
            .unwrap();
        let segment = log.seal().unwrap().unwrap();
        let tmp = dir.path().join("head.jsonl.tmp");
        fs::write(&tmp, head_after(segment.digest())).unwrap();

        let ready = with_mode(dir.path(), 0o555, || log.writable());

        assert_eq!(ready.unwrap(), Writable::Ready);
        assert_eq!(
            fs::read(&tmp).unwrap(),
            b"",
            "an empty file vouches for nothing"
        );
        assert_eq!(log.writable().unwrap(), Writable::Ready, "and is left");
    }

    #[test]
    #[cfg(unix)]
    fn a_left_over_that_can_be_neither_removed_nor_emptied_stops_the_run() {
        let dir = TempDir::new().unwrap();
        let log = log_in(&dir);
        log.append(&claim("sealed", "2026-09-01T00:00:00Z"))
            .unwrap();
        let segment = log.seal().unwrap().unwrap();
        let tmp = dir.path().join("head.jsonl.tmp");
        fs::write(&tmp, head_after(segment.digest())).unwrap();

        let ready = with_mode(&tmp, 0o444, || {
            with_mode(dir.path(), 0o555, || log.writable())
        });

        let Err(error) = ready else {
            panic!("readied over a left-over it could not clear");
        };
        assert!(matches!(error, Error::LeftOver { after: None, .. }));
        assert!(
            error
                .spelled()
                .ends_with("remove it by hand before the next run"),
            "{}",
            error.spelled()
        );
    }
}
