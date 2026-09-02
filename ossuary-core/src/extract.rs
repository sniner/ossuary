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
//! The receipt goes last: a run that dies halfway leaves findings without
//! one, and the file is simply offered again — re-said values land on the
//! set elements already standing.

use serde_json::json;

use crate::claim::{Attribute, Claim, Source, Subject, Timestamp, Value};
use crate::error::Result;
use crate::log::Log;

/// The receipt attribute: this blob was examined by the claim's source.
pub const EXAMINED: &str = "prov:examined";

/// One file's examination onto the record: every finding as a claim, then
/// the receipt. Returns how many claims went in — the findings plus one.
///
/// All claims of one examination carry one moment; the caller vouches for
/// `findings` being what the extractor said, already through
/// [`Attribute::parse`] — the values are checked here, the way every
/// claim is.
///
/// # Errors
///
/// Whatever building or appending a claim can answer — a `null` value
/// among the findings included. Nothing is rolled back: claims accrete,
/// and without the receipt the file is examined again.
pub fn record_examination(
    log: &Log,
    subject: &Subject,
    findings: &[(Attribute, Value)],
    source: &Source,
) -> Result<usize> {
    let time = Timestamp::now();
    for (attribute, value) in findings {
        let claim = Claim::assert(
            subject.clone(),
            attribute.clone(),
            value.clone(),
            time.clone(),
            source.clone(),
        )?;
        log.append(&claim)?;
    }
    let receipt = Claim::assert(
        subject.clone(),
        Attribute::parse(EXAMINED)?,
        json!(true),
        time,
        source.clone(),
    )?;
    log.append(&receipt)?;
    Ok(findings.len() + 1)
}

#[cfg(test)]
mod tests {
    use immure::Store;
    use tempfile::TempDir;

    use super::*;
    use crate::index::Index;

    fn archive(dir: &TempDir) -> (Store, Log, Index) {
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
        let log = Log::new(claims, dir.path().join("head.jsonl"));
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let index = Index::open(cache.join("index.sqlite")).unwrap();
        (content, log, index)
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

    #[test]
    fn the_worklist_offers_matching_files_until_their_receipt() {
        let dir = TempDir::new().unwrap();
        let (content, log, mut index) = archive(&dir);
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

        let written = record_examination(&log, &jpeg, &[], &source()).unwrap();
        assert_eq!(written, 1, "nothing found is still one receipt");
        index.fold(&log).unwrap();
        assert_eq!(
            index.worklist(&mimes, &source()).unwrap(),
            [],
            "receipted, and not offered again"
        );
    }

    #[test]
    fn another_source_is_offered_the_same_file() {
        let dir = TempDir::new().unwrap();
        let (content, log, mut index) = archive(&dir);
        let jpeg = take(&content, &log, &dir, "a.jpg", &[0xFF, 0xD8, 0xFF, 0xE0]);
        let mimes = vec!["image/jpeg".to_string()];

        record_examination(&log, &jpeg, &[], &source()).unwrap();
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
        let (content, log, mut index) = archive(&dir);
        let jpeg = take(&content, &log, &dir, "a.jpg", &[0xFF, 0xD8, 0xFF, 0xE0]);

        let findings = vec![(
            Attribute::parse("exif:date-time-original").unwrap(),
            serde_json::json!("2019:07:14 11:02:41"),
        )];
        let written = record_examination(&log, &jpeg, &findings, &source()).unwrap();
        assert_eq!(written, 2);

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
}
