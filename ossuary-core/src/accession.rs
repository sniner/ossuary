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
//! is counted. Bytes new to the store are then read again from the
//! start to detect their kind. What came of it is the
//! [`Admitted`] answer: the subject, whether the bytes were new, how
//! long they were, what they are.
//!
//! The second step, [`sighting_claims`], is about the record: the
//! taker's own facts under its source, the user's tags under theirs, the
//! run all of it arrived in — and, on the bytes' first day only, the
//! facts any content has: its size and its kind as detected. The taker
//! hands the claims to a [`Recorder`](crate::Recorder), which writes them
//! once the bytes are flushed. A walk over
//! unknown files has nothing but the detected kind; a fetcher knows it is
//! holding a message, and its word on the kind is a taker's fact like
//! any other: said on every sighting, whether or not the bytes are new.
//!
//! Bytes the store already holds get the taker's facts and nothing
//! else. Another place they sat, another mailbox they were seen in, is
//! new knowledge; their size and their detected kind describe the
//! content, the log has them from the first day, and saying a
//! deterministic thing twice adds nothing. What a taker knows of the
//! kind is not deterministic in the bytes — it may be the one thing the
//! detection got wrong.

use std::io::{Read, Seek};

use file_format::FileFormat;

use immure::{Algorithm, Store};
use serde_json::json;

use crate::claim::{Attribute, Claim, Run, Source, Subject, Timestamp, Value};
use crate::error::{Error, Result};

/// What [`admit`] answers: the bytes are in, and this is what was seen
/// of them on the way.
///
/// Only [`admit`] makes one: the claims [`sighting_claims`] makes about the
/// bytes come from what the store saw of them, never from a caller's word.
#[derive(Debug)]
pub struct Admitted {
    subject: Subject,
    new: bool,
    size: u64,
    /// `None` for bytes the store already held: their kind is on the
    /// record since their first day.
    kind: Option<String>,
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

    /// What the bytes are, detected from their content; `None` when the
    /// store already held them and nothing was detected.
    #[must_use]
    pub fn kind(&self) -> Option<&str> {
        self.kind.as_deref()
    }

    /// The same bytes for another sighting in the same call. The store
    /// had them by then, so [`sighting_claims`] does not claim their size twice.
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
/// content, which [`sighting_claims`] records beside the archive's own facts.
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
/// and the length is counted in passing. Bytes new to the store are
/// read a second time to detect their kind, which needs to seek; for
/// bytes the store already held, the kind is not needed and not
/// detected.
///
/// # Errors
///
/// [`Error::Store`](crate::Error::Store) when the store will not take
/// the bytes (a reader that fails reads as that), and [`Error::Io`]
/// when the second read fails.
pub fn admit(content: &Store, mut bytes: impl Read + Seek) -> Result<Admitted> {
    let mut counted = Counted::over(&mut bytes);
    let (status, entry) = content.add_reader(&mut counted)?;
    let size = counted.length;
    let kind = if status.is_new() {
        Some(detect(&mut bytes).map_err(|source| Error::Io {
            context: "detecting the file type".to_string(),
            source,
        })?)
    } else {
        None
    };
    Ok(Admitted {
        subject: Subject::parse(entry.digest().as_str())?,
        new: status.is_new(),
        size,
        kind,
    })
}

/// What the bytes are, from their content: a format the `file-format`
/// crate recognizes, `text/plain` for text, `application/x-empty` for
/// no bytes at all, and [`UNKNOWN_KIND`] when nothing answers. Reads
/// from the start whatever the position, and seeks: an Office document
/// or a zip is told apart by its directory at the end.
///
/// # Errors
///
/// Whatever reading or seeking `bytes` answers.
pub(crate) fn detect(mut bytes: impl Read + Seek) -> std::io::Result<String> {
    bytes.rewind()?;
    Ok(FileFormat::from_reader(bytes)?.media_type().to_string())
}

/// What [`detect`] answers for bytes it does not recognize.
pub(crate) const UNKNOWN_KIND: &str = "application/octet-stream";

/// The claims of an admission: the taker's facts, the tags, the kind in
/// the taker's words where it has them, and for bytes new to the store
/// their size, and the sniffed kind where the taker had no word. Every
/// claim of one call carries one moment, the sighting's own or now, and
/// one run. Each comes with the id it gets in the log under `algorithm`,
/// so that a retraction in the same run can name one.
///
/// Nothing is written here: the caller hands the claims to a
/// [`Recorder`](crate::Recorder), which writes them once the stored
/// bytes are flushed.
///
/// # Errors
///
/// Whatever building a claim can answer.
pub fn sighting_claims(
    admitted: &Admitted,
    said: &Sighting<'_>,
    algorithm: Algorithm,
) -> Result<Vec<Claim>> {
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
    // Size and detected kind describe the content, not the sighting: the
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
        None if admitted.new => admitted.kind.as_deref(),
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
    Ok(claims
        .into_iter()
        .map(|claim| claim.identified(algorithm))
        .collect())
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

/// A reader that counts the bytes on their way into the store.
pub(crate) struct Counted<R> {
    inner: R,
    pub(crate) length: u64,
}

impl<R: Read> Counted<R> {
    pub(crate) fn over(inner: R) -> Counted<R> {
        Counted { inner, length: 0 }
    }
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let count = self.inner.read(buf)?;
        self.length += count as u64;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use tempfile::TempDir;

    use super::*;
    use crate::log::Log;

    /// The claims of an admission, written at once.
    fn record(log: &Log, admitted: &Admitted, said: &Sighting<'_>) -> Result<Vec<Claim>> {
        let claims = sighting_claims(admitted, said, log.store().algorithm())?;
        log.append_all(&claims)?;
        Ok(claims)
    }

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
        (content, crate::log::test_log(claims, dir.path()))
    }

    fn source() -> Source {
        Source::parse("mailvault").unwrap()
    }

    #[test]
    fn admit_answers_what_it_saw_on_the_way() {
        let dir = TempDir::new().unwrap();
        let (content, _) = archive(&dir);

        let first = admit(
            &content,
            Cursor::new(&b"From: a@example.org\r\n\r\nhello"[..]),
        )
        .unwrap();
        assert_eq!(
            first.subject.as_str(),
            Algorithm::Sha256
                .hash(b"From: a@example.org\r\n\r\nhello")
                .as_str()
        );
        assert!(first.new);
        assert_eq!(first.size, 28);
        assert_eq!(first.kind(), Some("text/plain"));

        let again = admit(
            &content,
            Cursor::new(&b"From: a@example.org\r\n\r\nhello"[..]),
        )
        .unwrap();
        assert!(!again.new, "the store answers for what it holds");
        assert_eq!(again.subject, first.subject);
    }

    #[test]
    fn the_kind_is_detected_from_the_content() {
        let dir = TempDir::new().unwrap();
        let (content, _) = archive(&dir);
        let kinds: Vec<String> = [
            &b"plain words"[..],
            &b"BMW, Mercedes and Audi"[..],
            &b"-----BEGIN PGP SIGNATURE-----\n\niQEz\n-----END PGP SIGNATURE-----\n"[..],
            &[0xFF, 0xD8, 0xFF, 0xE0][..],
            &[0x00, 0xFF, 0x00][..],
            &b""[..],
        ]
        .into_iter()
        .map(|bytes| admit(&content, Cursor::new(bytes)).unwrap().kind.unwrap())
        .collect();
        assert_eq!(
            kinds,
            [
                "text/plain",
                "text/plain",
                "application/pgp-signature",
                "image/jpeg",
                UNKNOWN_KIND,
                "application/x-empty"
            ],
            "a text starting with BM is no bitmap"
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
        let admitted = admit(
            &content,
            Cursor::new(&b"From: a@example.org\r\n\r\nhello"[..]),
        )
        .unwrap();
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
            (first.len(), second.len()),
            (3, 2),
            "the size once, on the first sighting"
        );
        // The open segment reads in the order a seal gives it: by time,
        // then by the line.
        let head = log.head().unwrap();
        let written: Vec<(&str, &str)> = head
            .iter()
            .map(|claim| (claim.attribute().as_str(), claim.time().as_str()))
            .collect();
        assert_eq!(
            written,
            vec![
                ("file:mime", "2019-05-01T08:00:00Z"),
                ("file:size", "2019-05-01T08:00:00Z"),
                ("mailbox:place", "2019-05-01T08:00:00Z"),
                ("file:mime", "2023-01-01T00:00:00Z"),
                ("mailbox:place", "2023-01-01T00:00:00Z"),
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
        let admitted = admit(
            &content,
            Cursor::new(&b"From: a@example.org\r\n\r\nhello"[..]),
        )
        .unwrap();

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

        assert_eq!(written.len(), 3);
        let head = log.head().unwrap();
        let spelled: Vec<(&str, Value)> = head
            .iter()
            .map(|claim| (claim.attribute().as_str(), claim.value().unwrap().clone()))
            .collect();
        assert_eq!(
            spelled,
            vec![
                ("file:mime", json!("message/rfc822")),
                ("file:size", json!(28)),
                ("mailbox:place", json!("example.org:INBOX")),
            ],
            "the taker's facts and what any content has, the kind in the taker's words; one second, so the order is the lines'"
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

        let admitted = admit(&content, Cursor::new(&b"hello"[..])).unwrap();
        record(&log, &admitted, &said(&first)).unwrap();
        let again = admit(&content, Cursor::new(&b"hello"[..])).unwrap();
        let written = record(&log, &again, &said(&second)).unwrap();

        assert_eq!(
            written.len(),
            1,
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
        let first = admit(&content, Cursor::new(&b"hello"[..])).unwrap();
        record(&log, &first, &told).unwrap();
        let again = admit(&content, Cursor::new(&b"hello"[..])).unwrap();
        let written = record(&log, &again, &told).unwrap();

        assert_eq!(
            written.len(),
            1,
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
        let admitted = admit(&content, Cursor::new(&[0xFF, 0xD8, 0xFF, 0xE0][..])).unwrap();
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
        let admitted = admit(&content, Cursor::new(&b"hello"[..])).unwrap();

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
}
