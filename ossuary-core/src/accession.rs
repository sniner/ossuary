//! Accession: taking content in, in two steps.
//!
//! Every way into the archive ends here. Ingest walks a filesystem and
//! arrives with a path; a mail fetched from a server never was a file
//! and arrives with the mailbox it was seen in. What the takers know
//! differs, what the archive does with the bytes does not.
//!
//! The first step, [`admit`], is about the bytes alone: they stream
//! into the content store under the name their own content gives them —
//! never held whole in memory — and on the way through, their length
//! is counted and their kind sniffed. What came of it is the
//! [`Admitted`] answer: the subject, whether the bytes were new, how
//! long they were, what they looked like.
//!
//! The second step, [`record`], is about the record: the taker's own
//! facts under its source, the user's tags under theirs, the run all of
//! it arrived in — and, on the bytes' first day only, the facts any
//! content has: its size and its kind as the sniff saw it. A walk over
//! unknown files has nothing but the sniff; a fetcher knows it is
//! holding a message, and its word on the kind is a taker's fact like
//! any other: said on every sighting, whether or not the bytes are new.
//!
//! Bytes the store already holds get the taker's facts and nothing
//! else. Another place they sat, another mailbox they were seen in, is
//! new knowledge; their size and their sniffed kind describe the
//! content, the log has them from the first day, and saying a
//! deterministic thing twice adds nothing. What a taker knows of the
//! kind is not deterministic in the bytes — it may be the one thing the
//! sniff got wrong.

use std::io::Read;

use immure::Store;
use serde_json::json;

use crate::claim::{Attribute, Claim, Run, Source, Subject, Timestamp, Value};
use crate::error::Result;
use crate::log::Log;

/// What [`admit`] answers: the bytes are in, and this is what was seen
/// of them on the way.
///
/// Only [`admit`] makes one: what [`record`] writes about the bytes is
/// what the store saw of them, never a caller's word.
#[derive(Debug)]
pub struct Admitted {
    subject: Subject,
    new: bool,
    size: u64,
    kind: String,
}

impl Admitted {
    /// The content's name.
    #[must_use]
    pub fn subject(&self) -> &Subject {
        &self.subject
    }

    /// Whether the bytes were new to the store.
    #[must_use]
    pub fn is_new(&self) -> bool {
        self.new
    }

    /// How many bytes there were.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.size
    }

    /// What the bytes look like: magic bytes first, a UTF-8 look for
    /// plain text second, and `application/octet-stream` when nothing
    /// answers.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// The same bytes for another sighting in the same call. The store
    /// had them by then, so [`record`] does not write their size twice.
    #[must_use]
    pub fn again(&self) -> Admitted {
        Admitted {
            subject: self.subject.clone(),
            new: false,
            size: self.size,
            kind: self.kind.clone(),
        }
    }
}

/// What a taker says about the bytes it brings — one sighting of the
/// content, which [`record`] writes beside the archive's own facts.
#[derive(Debug)]
pub struct Sighting<'a> {
    /// Who is taking in — `ingest`, or a tool's own name.
    pub source: &'a Source,
    /// The call this arrival belongs to: what every claim of it says
    /// in its `run` field.
    pub run: &'a Run,
    /// What the bytes are, in the taker's own words — a fetcher holding
    /// a message knows, and says so on every sighting. `None` takes what
    /// [`admit`] saw, on the bytes' first day only.
    pub mime: Option<&'a str>,
    /// The taker's facts, claimed under its source in this order — a
    /// file's places, a message's mailbox — whether or not the bytes
    /// are new.
    pub facts: &'a [(Attribute, Value)],
    /// The user's own word on the arrival: each a `user:tag` under the
    /// source `user` — the human asserts, the taker is only the pen.
    pub tags: &'a [String],
    /// When the sighting was recorded, if not now: an import that repeats
    /// what another program recorded earlier gives that program's time.
    /// The caller keeps such claims apart from today's in the log (see
    /// `docs/format.md` on segment order). `None` is now.
    pub time: Option<&'a Timestamp>,
}

/// Stream `bytes` into `content` and answer what was seen of them. The
/// bytes never stand whole in memory: the store hashes as it writes,
/// and the length and the sniff are observed in passing.
///
/// # Errors
///
/// [`Error::Store`](crate::Error::Store) when the store will not take
/// the bytes — a reader that fails reads as that.
pub fn admit(content: &Store, bytes: impl Read) -> Result<Admitted> {
    let mut observed = Observed::over(bytes);
    let (status, entry) = content.add_reader(&mut observed)?;
    Ok(Admitted {
        subject: Subject::parse(entry.digest().as_str())?,
        new: status.is_new(),
        size: observed.length,
        kind: observed.mime(),
    })
}

/// Put an admission on the record: the taker's facts, the tags, the
/// kind in the taker's words where it has them — and for bytes new to
/// the store their size, and the sniffed kind where the taker had no
/// word. Every claim of one call carries one moment, the sighting's
/// own or now, and one run.
/// Answers how many claims went in.
///
/// # Errors
///
/// Whatever building or appending a claim can answer.
pub fn record(log: &Log, admitted: &Admitted, said: &Sighting<'_>) -> Result<usize> {
    let time = said.time.cloned().unwrap_or_else(Timestamp::now);
    let word = Source::parse("user")?;
    let subject = &admitted.subject;

    let mut claims = Vec::new();
    for (attribute, value) in said.facts {
        claims.push(claim(
            subject,
            attribute.clone(),
            value.clone(),
            &time,
            said.source,
            said.run,
        )?);
    }
    for tag in said.tags {
        claims.push(claim(
            subject,
            known_attribute("user:tag"),
            json!(tag),
            &time,
            &word,
            said.run,
        )?);
    }
    // Size and sniffed kind describe the content, not the sighting: the
    // log has them from the blob's first day, and they never say
    // anything new. The taker's word on the kind is the taker's, and
    // goes with every sighting.
    if admitted.new {
        claims.push(claim(
            subject,
            known_attribute("file:size"),
            json!(admitted.size),
            &time,
            said.source,
            said.run,
        )?);
    }
    let kind = match said.mime {
        Some(told) => Some(told),
        None if admitted.new => Some(admitted.kind.as_str()),
        None => None,
    };
    if let Some(kind) = kind {
        claims.push(claim(
            subject,
            known_attribute("file:mime"),
            json!(kind),
            &time,
            said.source,
            said.run,
        )?);
    }
    for claim in &claims {
        log.append(claim)?;
    }
    Ok(claims.len())
}

/// One claim of the record.
fn claim(
    subject: &Subject,
    attribute: Attribute,
    value: Value,
    time: &Timestamp,
    source: &Source,
    run: &Run,
) -> Result<Claim> {
    Claim::assert(
        subject.clone(),
        attribute,
        value,
        time.clone(),
        source.clone(),
        run.clone(),
    )
}

/// An attribute the crate spells itself.
pub(crate) fn known_attribute(attribute: &'static str) -> Attribute {
    Attribute::parse(attribute).expect("a known attribute")
}

/// How much of a file's head the magic-byte sniff sees — the same 8 KiB
/// infer's own file reading takes, comfortably past every offset its
/// matchers look at.
pub(crate) const SNIFF: usize = 8192;

/// A reader that watches bytes on their way into the store: the head for
/// the sniff, a running UTF-8 check for the text fallback, the length.
/// Everything [`mime`](Observed::mime) will need, observed in passing —
/// which is what lets a file stream in without standing whole in memory.
struct Observed<R> {
    inner: R,
    head: Vec<u8>,
    text: Utf8Watch,
    length: u64,
}

impl<R: Read> Observed<R> {
    fn over(inner: R) -> Observed<R> {
        Observed {
            inner,
            head: Vec::with_capacity(SNIFF),
            text: Utf8Watch::new(),
            length: 0,
        }
    }

    /// What the bytes say they are: magic bytes first, a UTF-8 look for
    /// plain text second, and the honest shrug when nothing answers. The
    /// sniff reads the head, the UTF-8 look judged the whole stream.
    fn mime(&self) -> String {
        infer::get(&self.head)
            .map(|kind| kind.mime_type().to_string())
            .or_else(|| (self.length > 0 && self.text.holds()).then(|| "text/plain".to_string()))
            .unwrap_or_else(|| "application/octet-stream".to_string())
    }
}

impl<R: Read> Read for Observed<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let count = self.inner.read(buf)?;
        let passed = &buf[..count];
        if self.head.len() < SNIFF {
            let room = SNIFF - self.head.len();
            self.head
                .extend_from_slice(&passed[..room.min(passed.len())]);
        }
        self.text.feed(passed);
        self.length += count as u64;
        Ok(count)
    }
}

/// Whether everything fed through so far could be one valid UTF-8 text —
/// the whole-stream judgement, kept across chunk borders: a multi-byte
/// character split between two reads must not read as junk.
pub(crate) struct Utf8Watch {
    sound: bool,
    /// The bytes at a chunk's end that began a character whose end had
    /// not arrived yet — at most three, a character being four at the
    /// longest.
    pending: [u8; 4],
    pended: usize,
}

impl Utf8Watch {
    pub(crate) fn new() -> Utf8Watch {
        Utf8Watch {
            sound: true,
            pending: [0; 4],
            pended: 0,
        }
    }

    pub(crate) fn feed(&mut self, chunk: &[u8]) {
        if !self.sound {
            return;
        }
        let mut rest = chunk;
        // First finish the character the last chunk began: one byte at a
        // time until it is whole or proven junk — four bytes always
        // decide, so this ends within three steps and the buffer holds.
        while self.pended > 0 && !rest.is_empty() {
            self.pending[self.pended] = rest[0];
            self.pended += 1;
            rest = &rest[1..];
            match std::str::from_utf8(&self.pending[..self.pended]) {
                Ok(_) => self.pended = 0,
                Err(error) if error.error_len().is_some() => {
                    self.sound = false;
                    return;
                }
                Err(_) => {}
            }
        }
        match std::str::from_utf8(rest) {
            Ok(_) => {}
            Err(error) if error.error_len().is_some() => self.sound = false,
            Err(error) => {
                let tail = &rest[error.valid_up_to()..];
                self.pending[..tail.len()].copy_from_slice(tail);
                self.pended = tail.len();
            }
        }
    }

    /// Whether the whole stream read as text. A character still waiting
    /// for its end when the stream ends is junk, not text.
    pub(crate) fn holds(&self) -> bool {
        self.sound && self.pended == 0
    }
}

#[cfg(test)]
mod tests {
    use immure::Algorithm;
    use tempfile::TempDir;

    use super::*;

    static RUN: std::sync::LazyLock<Run> =
        std::sync::LazyLock::new(|| Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap());

    fn archive(dir: &TempDir) -> (Store, Log) {
        let content = Store::builder(dir.path().join("content"))
            .suffix("")
            .depth(2)
            .create()
            .unwrap();
        let claims = Store::builder(dir.path().join("claims"))
            .suffix(".seg")
            .depth(1)
            .create()
            .unwrap();
        (content, Log::new(claims, dir.path().join("head.jsonl")))
    }

    fn source() -> Source {
        Source::parse("mailvault").unwrap()
    }

    #[test]
    fn admit_answers_what_it_saw_on_the_way() {
        let dir = TempDir::new().unwrap();
        let (content, _) = archive(&dir);

        let first = admit(&content, &b"From: a@example.org\r\n\r\nhello"[..]).unwrap();
        assert_eq!(
            first.subject.as_str(),
            Algorithm::Sha256
                .hash(b"From: a@example.org\r\n\r\nhello")
                .as_str()
        );
        assert!(first.new);
        assert_eq!(first.size, 28);
        assert_eq!(first.kind, "text/plain");

        let again = admit(&content, &b"From: a@example.org\r\n\r\nhello"[..]).unwrap();
        assert!(!again.new, "the store answers for what it holds");
        assert_eq!(again.subject, first.subject);
    }

    #[test]
    fn the_kind_is_sniffed_three_ways() {
        let dir = TempDir::new().unwrap();
        let (content, _) = archive(&dir);
        let kinds: Vec<String> = [
            &b"plain words"[..],
            &[0xFF, 0xD8, 0xFF, 0xE0][..],
            &[0x00, 0xFF, 0x00][..],
        ]
        .into_iter()
        .map(|bytes| admit(&content, bytes).unwrap().kind)
        .collect();
        assert_eq!(
            kinds,
            ["text/plain", "image/jpeg", "application/octet-stream"]
        );
    }

    #[test]
    fn a_sighting_with_a_time_of_its_own_is_recorded_at_that_time() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let source = source();
        let place = |folder: &str| {
            vec![(
                Attribute::parse("mailbox:place").unwrap(),
                json!(format!("example.org:{folder}")),
            )]
        };
        let (early, late) = (
            Timestamp::parse("2019-05-01T08:00:00Z").unwrap(),
            Timestamp::parse("2023-01-01T00:00:00Z").unwrap(),
        );
        let admitted = admit(&content, &b"From: a@example.org\r\n\r\nhello"[..]).unwrap();
        let (inbox, archive_folder) = (place("INBOX"), place("Archive"));
        let sighting = |facts, time| Sighting {
            source: &source,
            run: &RUN,
            mime: Some("message/rfc822"),
            facts,
            tags: &[],
            time: Some(time),
        };

        let first = record(&log, &admitted, &sighting(&inbox, &early)).unwrap();
        let second = record(&log, &admitted.again(), &sighting(&archive_folder, &late)).unwrap();

        assert_eq!(
            (first, second),
            (3, 2),
            "the size once, on the first sighting"
        );
        let head = log.head().unwrap();
        let written: Vec<(&str, &str)> = head
            .iter()
            .map(|claim| (claim.attribute().as_str(), claim.time().as_str()))
            .collect();
        assert_eq!(
            written,
            vec![
                ("mailbox:place", "2019-05-01T08:00:00Z"),
                ("file:size", "2019-05-01T08:00:00Z"),
                ("file:mime", "2019-05-01T08:00:00Z"),
                ("mailbox:place", "2023-01-01T00:00:00Z"),
                ("file:mime", "2023-01-01T00:00:00Z"),
            ]
        );
    }

    #[test]
    fn record_writes_the_takers_facts_and_the_day_one_facts() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let source = source();
        let facts = vec![(
            Attribute::parse("mailbox:place").unwrap(),
            json!("example.org:INBOX"),
        )];
        let admitted = admit(&content, &b"From: a@example.org\r\n\r\nhello"[..]).unwrap();

        let written = record(
            &log,
            &admitted,
            &Sighting {
                source: &source,
                run: &RUN,
                mime: Some("message/rfc822"),
                facts: &facts,
                tags: &[],
                time: None,
            },
        )
        .unwrap();

        assert_eq!(written, 3);
        let head = log.head().unwrap();
        let spelled: Vec<(&str, Value)> = head
            .iter()
            .map(|claim| (claim.attribute().as_str(), claim.value().unwrap().clone()))
            .collect();
        assert_eq!(
            spelled,
            vec![
                ("mailbox:place", json!("example.org:INBOX")),
                ("file:size", json!(28)),
                ("file:mime", json!("message/rfc822")),
            ],
            "the taker's facts first, then what any content has — the kind in the taker's words"
        );
        assert!(
            head.iter().all(|claim| claim.run() == Some(&RUN)),
            "every claim of the sighting carries the call's run"
        );
        assert!(
            head.iter()
                .all(|claim| claim.source().as_str() == "mailvault"),
            "every claim under the taker's source"
        );
    }

    #[test]
    fn known_bytes_get_the_sighting_only() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let source = source();
        let said = |facts| Sighting {
            source: &source,
            run: &RUN,
            mime: None,
            facts,
            tags: &[],
            time: None,
        };
        let first = vec![(Attribute::parse("mailbox:place").unwrap(), json!("a:INBOX"))];
        let second = vec![(Attribute::parse("mailbox:place").unwrap(), json!("b:INBOX"))];

        let admitted = admit(&content, &b"hello"[..]).unwrap();
        record(&log, &admitted, &said(&first)).unwrap();
        let again = admit(&content, &b"hello"[..]).unwrap();
        let written = record(&log, &again, &said(&second)).unwrap();

        assert_eq!(
            written, 1,
            "the place alone — size and kind were said on day one"
        );
        let head = log.head().unwrap();
        assert_eq!(head.len(), 4);
        assert_eq!(head[3].attribute().as_str(), "mailbox:place");
        assert_eq!(head[3].value(), Some(&json!("b:INBOX")));
    }

    #[test]
    fn a_told_kind_is_said_on_every_sighting() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let source = source();
        let told = Sighting {
            source: &source,
            run: &RUN,
            mime: Some("message/rfc822"),
            facts: &[],
            tags: &[],
            time: None,
        };
        let first = admit(&content, &b"hello"[..]).unwrap();
        record(&log, &first, &told).unwrap();
        let again = admit(&content, &b"hello"[..]).unwrap();
        let written = record(&log, &again, &told).unwrap();

        assert_eq!(
            written, 1,
            "the kind alone — the size the log has from day one"
        );
        let kinds = log
            .head()
            .unwrap()
            .iter()
            .filter(|claim| claim.attribute().as_str() == "file:mime")
            .count();
        assert_eq!(
            kinds, 2,
            "what the taker knows is not answered by the store"
        );
    }

    #[test]
    fn the_sniffed_kind_stands_when_the_taker_says_nothing() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let source = source();
        let admitted = admit(&content, &[0xFF, 0xD8, 0xFF, 0xE0][..]).unwrap();
        record(
            &log,
            &admitted,
            &Sighting {
                source: &source,
                run: &RUN,
                mime: None,
                facts: &[],
                tags: &[],
                time: None,
            },
        )
        .unwrap();
        let kind = log
            .head()
            .unwrap()
            .into_iter()
            .find(|claim| claim.attribute().as_str() == "file:mime")
            .unwrap();
        assert_eq!(kind.value(), Some(&json!("image/jpeg")));
    }

    #[test]
    fn tags_ride_along_under_the_users_own_source() {
        let dir = TempDir::new().unwrap();
        let (content, log) = archive(&dir);
        let source = Source::parse("ingest").unwrap();
        let tags = vec!["holiday".to_string()];
        let admitted = admit(&content, &b"hello"[..]).unwrap();

        record(
            &log,
            &admitted,
            &Sighting {
                source: &source,
                run: &RUN,
                mime: None,
                facts: &[],
                tags: &tags,
                time: None,
            },
        )
        .unwrap();

        let head = log.head().unwrap();
        let tag = head
            .iter()
            .find(|claim| claim.attribute().as_str() == "user:tag")
            .unwrap();
        assert_eq!(tag.value(), Some(&json!("holiday")));
        assert_eq!(
            tag.source().as_str(),
            "user",
            "the human asserts, the taker is the pen"
        );
    }

    #[test]
    fn the_utf8_watch_reads_across_chunk_borders() {
        let mut split = Utf8Watch::new();
        for byte in "grüße, öl".as_bytes() {
            // Byte by byte: every multi-byte character is torn apart.
            split.feed(std::slice::from_ref(byte));
        }
        assert!(split.holds());

        let mut torn = Utf8Watch::new();
        torn.feed(&[0xC3]);
        assert!(
            !torn.holds(),
            "a character begun and never finished is junk"
        );
        torn.feed(&[0xA4]);
        assert!(torn.holds(), "finished, it is the text it always was");

        let mut junk = Utf8Watch::new();
        junk.feed(&[b'a', 0xFF, b'b']);
        assert!(!junk.holds());
        junk.feed(b"all text from here on");
        assert!(!junk.holds(), "junk once is junk for good");
    }
}
