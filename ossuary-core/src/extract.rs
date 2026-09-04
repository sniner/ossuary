//! Recording what an extractor found — and that it looked.
//!
//! Extractors are their own programs, spoken to over pipes; the protocol
//! is `docs/extractors.md`. This module is the archive's side of it: what
//! comes back is funnelled through the claim grammar, stamped with the
//! subject, the moment and the extractor's source, and closed with one
//! receipt — [`EXAMINED`], value `true` — so the next run knows this file
//! is done, whatever the findings were. A file whose whole harvest was
//! nothing, or stood on a derived blob, is done all the same.
//!
//! Derived files come through here too: an extractor that wrote files
//! hands them over as [`Derivation`]s, and each becomes content of its
//! own — bytes into the derived store, and onto the record what is known
//! about them: their kind and name in the extractor's words, their
//! origin, and on the bytes' first day their size. The derived store,
//! not the content store: what was taken in and what a tool made of it
//! do not share a rank, and keeping them apart keeps every future
//! cleanup out of the originals by topology, not by care. The one
//! exception runs the other way: bytes `content/` already holds — an
//! attachment that was also saved and taken in as a file — get no copy
//! in `derived/` at all, only their claims; a subject names content
//! wherever it lies, and an original needs no second-class copy.
//!
//! The receipt goes last: a run that dies halfway leaves findings without
//! one, and the file is simply offered again — re-added bytes dedup in
//! the store, re-said values land on the set elements already standing.

use std::fs;
use std::path::PathBuf;

use immure::Store;
use serde_json::json;

use crate::claim::{Attribute, Claim, Source, Subject, Timestamp, Value};
use crate::error::{Error, Result};
use crate::log::Log;

/// The receipt attribute: this blob was examined by the claim's source.
pub const EXAMINED: &str = "prov:examined";

/// One derived file, as the extractor announced it: the bare name and the
/// MIME type in the extractor's own words, where the bytes wait, and
/// whatever else it said about this file rather than the examined one.
#[derive(Debug)]
pub struct Derivation {
    /// The announced name — what `file:name` will say.
    pub name: String,
    /// The announced MIME type — what `file:mime` will say. The one who
    /// wrote the bytes is not guessed at.
    pub mime: String,
    /// Where the bytes wait to be taken in. Read here, never removed:
    /// the directory is the caller's to clean up.
    pub path: PathBuf,
    /// Findings about this file, already through [`Attribute::parse`].
    pub findings: Vec<(Attribute, Value)>,
}

/// What one examination put on the record.
#[derive(Debug)]
pub struct Examined {
    /// Claims appended to the log, the receipt included.
    pub claims: usize,
    /// Derived files whose bytes were new to the derived store.
    pub stored: usize,
    /// Derived files whose bytes the archive already held — in the
    /// content store as something once taken in, or in the derived store
    /// from an earlier harvest. Their origin and name went on the record
    /// all the same — another mail carrying the same attachment is a new
    /// fact about old content.
    pub known: usize,
}

/// One file's examination onto the record: every finding as a claim, each
/// derived file into the derived store with what is known about it, then
/// the receipt.
///
/// A derived file is content with a record like any other, and the log
/// says so: its `file:mime` and `file:name` in the extractor's words, its
/// origin as `derive:derived-from`, and — for bytes new to the store — its
/// `file:size`, the way ingest says it: a fact of the content, once.
/// Findings the extractor made about a derived file stand on it, not on
/// the examined one. All claims of one examination carry one moment and
/// one source; the caller vouches for the findings being what the
/// extractor said.
///
/// `content` is only asked, never written: bytes it already holds were
/// once taken in, and an original needs no second-class copy — the
/// derived file's claims go on the record and nothing lands in
/// `derived`, because a subject names content wherever it lies. Only
/// bytes the archive knows purely as harvest go into `derived`.
///
/// # Errors
///
/// Whatever building or appending a claim can answer — a `null` value
/// among the findings included — and [`Error::Io`] on a derived file that
/// will not read. Nothing is rolled back: claims accrete, and without the
/// receipt the file is examined again.
pub fn record_examination(
    content: &Store,
    derived: &Store,
    log: &Log,
    subject: &Subject,
    findings: &[(Attribute, Value)],
    derivations: &[Derivation],
    source: &Source,
) -> Result<Examined> {
    let time = Timestamp::now();
    let mut examined = Examined {
        claims: 0,
        stored: 0,
        known: 0,
    };
    for (attribute, value) in findings {
        append(log, subject, attribute, value, &time, source)?;
        examined.claims += 1;
    }
    for derivation in derivations {
        let taken = take(content, derived, log, subject, derivation, &time, source)?;
        examined.claims += taken.claims;
        examined.stored += taken.stored;
        examined.known += taken.known;
    }
    let receipt = Claim::assert(
        subject.clone(),
        known_attribute(EXAMINED),
        json!(true),
        time,
        source.clone(),
    )?;
    log.append(&receipt)?;
    examined.claims += 1;
    Ok(examined)
}

/// One derived file: bytes into the derived store — unless `content`
/// already holds them as an original — and what is known about it into
/// the log. Answers what it added to the tally.
fn take(
    content: &Store,
    derived: &Store,
    log: &Log,
    origin: &Subject,
    derivation: &Derivation,
    time: &Timestamp,
    source: &Source,
) -> Result<Examined> {
    let mut examined = Examined {
        claims: 0,
        stored: 0,
        known: 0,
    };
    let bytes = fs::read(&derivation.path).map_err(|io| Error::Io {
        context: format!("{}: reading the derived file", derivation.name),
        source: io,
    })?;
    let digest = derived.algorithm().hash(&bytes);
    let subject = Subject::parse(&format!("{}:{}", derived.algorithm().name(), digest))?;
    // The digest does not say which store it belongs to — so the writer
    // may choose, and content/ wins: bytes once taken in need no
    // second-class copy, the claims below stand either way. This asks
    // the store itself, truth asking truth; the index has no say here.
    if content.matching(digest.as_str())?.is_empty() {
        let (status, _) = derived.add(&bytes)?;
        // The size describes the content and is said on the bytes' first
        // day, the way ingest says it — and ingest already said it for
        // everything content/ holds. Kind, name and origin belong to
        // this derivation: another extractor, another mail may know the
        // same bytes under other words, and every word stands in the set.
        if status.is_new() {
            let size = known_attribute("file:size");
            append(log, &subject, &size, &json!(bytes.len()), time, source)?;
            examined.claims += 1;
            examined.stored += 1;
        } else {
            examined.known += 1;
        }
    } else {
        examined.known += 1;
    }
    let told = [
        (known_attribute("file:mime"), json!(derivation.mime)),
        (known_attribute("file:name"), json!(derivation.name)),
        (
            known_attribute("derive:derived-from"),
            json!(origin.as_str()),
        ),
    ];
    for (attribute, value) in &told {
        append(log, &subject, attribute, value, time, source)?;
        examined.claims += 1;
    }
    for (attribute, value) in &derivation.findings {
        append(log, &subject, attribute, value, time, source)?;
        examined.claims += 1;
    }
    Ok(examined)
}

/// One claim of the examination, appended.
fn append(
    log: &Log,
    subject: &Subject,
    attribute: &Attribute,
    value: &Value,
    time: &Timestamp,
    source: &Source,
) -> Result<()> {
    let claim = Claim::assert(
        subject.clone(),
        attribute.clone(),
        value.clone(),
        time.clone(),
        source.clone(),
    )?;
    log.append(&claim)
}

/// An attribute this module spells itself.
fn known_attribute(attribute: &'static str) -> Attribute {
    Attribute::parse(attribute).expect("a known attribute")
}

#[cfg(test)]
mod tests {
    use immure::Store;
    use tempfile::TempDir;

    use super::*;
    use crate::index::Index;

    fn archive(dir: &TempDir) -> (Store, Store, Log, Index) {
        let content = Store::builder(dir.path().join("content"))
            .suffix("")
            .depth(2)
            .create()
            .unwrap();
        let derived = Store::builder(dir.path().join("derived"))
            .suffix("")
            .depth(2)
            .create()
            .unwrap();
        let claims = Store::builder(dir.path().join("claims"))
            .suffix(".seg")
            .depth(1)
            .create()
            .unwrap();
        let log = Log::new(claims, dir.path().join("head.jsonl"));
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let index = Index::open(cache.join("index.sqlite")).unwrap();
        (content, derived, log, index)
    }

    fn take(content: &Store, log: &Log, dir: &TempDir, name: &str, bytes: &[u8]) -> Subject {
        let file = dir.path().join(name);
        std::fs::write(&file, bytes).unwrap();
        crate::ingest(
            content,
            log,
            &file,
            "atlas.example.net",
            &crate::Excludes::none(),
            None,
        )
        .unwrap();
        Subject::parse(&format!(
            "{}:{}",
            content.algorithm().name(),
            content.algorithm().hash(bytes)
        ))
        .unwrap()
    }

    fn source() -> Source {
        Source::parse("extractor:exif/0.1.0").unwrap()
    }

    /// A derived file waiting in a scratch directory, the way the
    /// orchestrator hands one over.
    fn derivation(dir: &TempDir, name: &str, mime: &str, bytes: &[u8]) -> Derivation {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        Derivation {
            name: name.to_string(),
            mime: mime.to_string(),
            path,
            findings: Vec::new(),
        }
    }

    #[test]
    fn the_worklist_offers_matching_files_until_their_receipt() {
        let dir = TempDir::new().unwrap();
        let (content, derived, log, mut index) = archive(&dir);
        // A JPEG by its magic bytes, and a text file the mimes exclude.
        let jpeg = take(&content, &log, &dir, "a.jpg", &[0xFF, 0xD8, 0xFF, 0xE0]);
        take(&content, &log, &dir, "b.txt", b"plain words");
        let mimes = vec!["image/jpeg".to_string()];

        index.fold(&log).unwrap();
        assert_eq!(
            index.worklist(&mimes, &source()).unwrap(),
            std::slice::from_ref(&jpeg),
            "the jpeg waits, the text was never its business"
        );

        let written =
            record_examination(&content, &derived, &log, &jpeg, &[], &[], &source()).unwrap();
        assert_eq!(written.claims, 1, "nothing found is still one receipt");
        index.fold(&log).unwrap();
        assert_eq!(
            index.worklist(&mimes, &source()).unwrap(),
            [],
            "receipted, and not offered again"
        );
    }

    #[test]
    fn of_kind_ignores_receipts_and_examined_asks_about_one_file() {
        let dir = TempDir::new().unwrap();
        let (content, derived, log, mut index) = archive(&dir);
        let jpeg = take(&content, &log, &dir, "a.jpg", &[0xFF, 0xD8, 0xFF, 0xE0]);
        let mimes = vec!["image/jpeg".to_string()];

        record_examination(&content, &derived, &log, &jpeg, &[], &[], &source()).unwrap();
        index.fold(&log).unwrap();

        assert_eq!(
            index.worklist(&mimes, &source()).unwrap(),
            [],
            "the worklist subtracts the receipt"
        );
        assert_eq!(
            index.of_kind(&mimes).unwrap(),
            std::slice::from_ref(&jpeg),
            "of_kind does not — this is what --full examines anew"
        );
        assert!(index.examined(&jpeg, &source()).unwrap());
        assert!(
            !index
                .examined(&jpeg, &Source::parse("extractor:text/0.1.0").unwrap())
                .unwrap(),
            "a receipt belongs to its source alone"
        );
    }

    #[test]
    fn another_source_is_offered_the_same_file() {
        let dir = TempDir::new().unwrap();
        let (content, derived, log, mut index) = archive(&dir);
        let jpeg = take(&content, &log, &dir, "a.jpg", &[0xFF, 0xD8, 0xFF, 0xE0]);
        let mimes = vec!["image/jpeg".to_string()];

        record_examination(&content, &derived, &log, &jpeg, &[], &[], &source()).unwrap();
        index.fold(&log).unwrap();

        let newer = Source::parse("extractor:exif/3.0").unwrap();
        assert_eq!(
            index.worklist(&mimes, &newer).unwrap(),
            [jpeg],
            "a new version is a new source, and looks at everything again"
        );
    }

    #[test]
    fn findings_go_in_with_the_receipt_and_one_moment() {
        let dir = TempDir::new().unwrap();
        let (content, derived, log, mut index) = archive(&dir);
        let jpeg = take(&content, &log, &dir, "a.jpg", &[0xFF, 0xD8, 0xFF, 0xE0]);

        let findings = vec![(
            Attribute::parse("exif:date-time-original").unwrap(),
            serde_json::json!("2019:07:14 11:02:41"),
        )];
        let written =
            record_examination(&content, &derived, &log, &jpeg, &findings, &[], &source()).unwrap();
        assert_eq!(written.claims, 2);

        index.fold(&log).unwrap();
        let about = index.about(&jpeg).unwrap();
        let ours: Vec<_> = about
            .iter()
            .filter(|claim| claim.source().as_str() == "extractor:exif/0.1.0")
            .collect();
        assert_eq!(ours.len(), 2);
        assert_eq!(
            ours[0].time(),
            ours[1].time(),
            "one examination, one moment"
        );
        assert_eq!(
            ours[1].attribute().as_str(),
            EXAMINED,
            "the receipt is last"
        );
    }

    #[test]
    fn a_derived_file_becomes_content_with_its_own_record() {
        let dir = TempDir::new().unwrap();
        let (content, derived, log, mut index) = archive(&dir);
        let mail = take(&content, &log, &dir, "letter", b"From: a@example.com");
        let scratch = TempDir::new().unwrap();
        let mut attachment = derivation(&scratch, "report.pdf", "application/pdf", b"%PDF-1.7");
        attachment.findings.push((
            Attribute::parse("mail:content-id").unwrap(),
            serde_json::json!("<part2@example.com>"),
        ));

        let written = record_examination(
            &content,
            &derived,
            &log,
            &mail,
            &[],
            &[attachment],
            &source(),
        )
        .unwrap();
        assert_eq!(written.stored, 1, "the bytes were new to the store");
        assert_eq!(written.known, 0);
        assert_eq!(
            written.claims, 6,
            "size, kind, name, origin and the finding on the derived file, then the receipt"
        );

        let pdf = Subject::parse(&format!(
            "{}:{}",
            derived.algorithm().name(),
            derived.algorithm().hash(b"%PDF-1.7")
        ))
        .unwrap();
        assert_eq!(
            derived.matching(pdf.hex()).unwrap().len(),
            1,
            "the bytes stand in the derived store"
        );
        assert!(
            content.matching(pdf.hex()).unwrap().is_empty(),
            "and never beside the originals — that is the topology's promise"
        );
        index.fold(&log).unwrap();
        let value = |attribute: &str| {
            index
                .values(&pdf, &Attribute::parse(attribute).unwrap())
                .unwrap()
        };
        assert_eq!(value("file:size"), [serde_json::json!(8)]);
        assert_eq!(value("file:mime"), [serde_json::json!("application/pdf")]);
        assert_eq!(value("file:name"), [serde_json::json!("report.pdf")]);
        assert_eq!(
            value("derive:derived-from"),
            [serde_json::json!(mail.as_str())],
            "the derived file points at its origin"
        );
        assert_eq!(
            value("mail:content-id"),
            [serde_json::json!("<part2@example.com>")],
            "a finding named for the derived file stands on it"
        );
        assert_eq!(
            index
                .values(&mail, &Attribute::parse(EXAMINED).unwrap())
                .unwrap(),
            [serde_json::json!(true)],
            "the receipt stands on the examined file"
        );
        assert!(
            index
                .values(&pdf, &Attribute::parse(EXAMINED).unwrap())
                .unwrap()
                .is_empty(),
            "the derived file was not examined, only made"
        );
    }

    #[test]
    fn known_bytes_derived_again_get_no_second_size() {
        let dir = TempDir::new().unwrap();
        let (content, derived, log, mut index) = archive(&dir);
        let first = take(&content, &log, &dir, "one", b"first letter");
        let second = take(&content, &log, &dir, "two", b"second letter");
        let scratch = TempDir::new().unwrap();

        // The same attachment falls out of two different mails, under two
        // different names.
        let written = record_examination(
            &content,
            &derived,
            &log,
            &first,
            &[],
            &[derivation(
                &scratch,
                "invoice.pdf",
                "application/pdf",
                b"%PDF",
            )],
            &source(),
        )
        .unwrap();
        assert_eq!(written.stored, 1);
        let written = record_examination(
            &content,
            &derived,
            &log,
            &second,
            &[],
            &[derivation(
                &scratch,
                "Rechnung.pdf",
                "application/pdf",
                b"%PDF",
            )],
            &source(),
        )
        .unwrap();
        assert_eq!(written.stored, 0);
        assert_eq!(written.known, 1, "the bytes were already held");
        assert_eq!(
            written.claims, 4,
            "kind, name and origin again — the size is a fact of the content, said once"
        );

        let pdf = Subject::parse(&format!(
            "{}:{}",
            derived.algorithm().name(),
            derived.algorithm().hash(b"%PDF")
        ))
        .unwrap();
        index.fold(&log).unwrap();
        let names = index
            .values(&pdf, &Attribute::parse("file:name").unwrap())
            .unwrap();
        assert_eq!(names.len(), 2, "both names stand in the set");
        let origins = index
            .values(&pdf, &Attribute::parse("derive:derived-from").unwrap())
            .unwrap();
        assert_eq!(origins.len(), 2, "and both origins");
        let sizes = index
            .about(&pdf)
            .unwrap()
            .iter()
            .filter(|claim| claim.attribute().as_str() == "file:size")
            .count();
        assert_eq!(sizes, 1, "one size claim in the whole history");
    }

    #[test]
    fn bytes_already_taken_in_get_no_second_class_copy() {
        let dir = TempDir::new().unwrap();
        let (content, derived, log, mut index) = archive(&dir);
        // The everyday mail workflow: the attachment was saved to disk,
        // both were ingested together, and now extraction unpacks it.
        let mail = take(&content, &log, &dir, "letter", b"From: c@example.com");
        let saved = take(&content, &log, &dir, "invoice.pdf", b"%PDF-saved");
        let scratch = TempDir::new().unwrap();

        let written = record_examination(
            &content,
            &derived,
            &log,
            &mail,
            &[],
            &[derivation(
                &scratch,
                "invoice.pdf",
                "application/pdf",
                b"%PDF-saved",
            )],
            &source(),
        )
        .unwrap();

        assert_eq!(written.stored, 0);
        assert_eq!(
            written.known, 1,
            "the archive held these bytes — as an original"
        );
        assert!(
            derived.matching(saved.hex()).unwrap().is_empty(),
            "an original needs no second-class copy"
        );
        index.fold(&log).unwrap();
        assert_eq!(
            index
                .values(&saved, &Attribute::parse("derive:derived-from").unwrap())
                .unwrap(),
            [serde_json::json!(mail.as_str())],
            "the record grew all the same — a subject names content wherever it lies"
        );
        let sizes = index
            .about(&saved)
            .unwrap()
            .iter()
            .filter(|claim| claim.attribute().as_str() == "file:size")
            .count();
        assert_eq!(sizes, 1, "the size stands from ingest, said once");
    }

    #[test]
    fn a_derived_file_that_will_not_read_is_an_error_before_the_receipt() {
        let dir = TempDir::new().unwrap();
        let (content, derived, log, mut index) = archive(&dir);
        let mail = take(&content, &log, &dir, "letter", b"From: b@example.com");
        let gone = Derivation {
            name: "missing.pdf".to_string(),
            mime: "application/pdf".to_string(),
            path: dir.path().join("nowhere").join("missing.pdf"),
            findings: Vec::new(),
        };

        let result = record_examination(&content, &derived, &log, &mail, &[], &[gone], &source());
        assert!(result.is_err());

        index.fold(&log).unwrap();
        assert!(
            index
                .values(&mail, &Attribute::parse(EXAMINED).unwrap())
                .unwrap()
                .is_empty(),
            "no receipt — the file will be offered again"
        );
    }
}
