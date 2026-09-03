//! The walking skeleton, end to end through the public API: claims are
//! appended, sealed, folded — and the one question the architecture promises
//! is answered: "what do I know about this blob?"

use immure::Store;
use ossuary_core::{Attribute, Claim, Folded, Index, Log, Source, Subject, Timestamp, Value};
use serde_json::json;
use tempfile::TempDir;

fn archive(dir: &TempDir) -> (Log, Index) {
    let store = Store::builder(dir.path().join("claims"))
        .suffix(".seg")
        .depth(1)
        .compress(true)
        .create()
        .unwrap();
    let log = Log::new(store, dir.path().join("head.jsonl"));
    let cache = dir.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let index = Index::open(cache.join("index.sqlite")).unwrap();
    (log, index)
}

fn claim(attribute: &str, value: Value, time: &str, source: &str) -> Claim {
    Claim::assert(
        photo(),
        Attribute::parse(attribute).unwrap(),
        value,
        Timestamp::parse(time).unwrap(),
        Source::parse(source).unwrap(),
    )
    .unwrap()
}

fn photo() -> Subject {
    Subject::parse("sha256:9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e")
        .unwrap()
}

#[test]
fn the_skeleton_walks() {
    let dir = TempDir::new().unwrap();
    let (log, mut index) = archive(&dir);

    // Ingest day: the universal facts, then the segment is sealed.
    log.append(&claim(
        "file:path",
        json!("/photos/2019/crete/beach.jpg"),
        "2026-09-01T21:14:03Z",
        "ingest",
    ))
    .unwrap();
    log.append(&claim(
        "file:size",
        json!(4_194_304),
        "2026-09-01T21:14:03Z",
        "ingest",
    ))
    .unwrap();
    log.append(&claim(
        "file:mime",
        json!("image/jpeg"),
        "2026-09-01T21:14:03Z",
        "ingest",
    ))
    .unwrap();
    log.seal().unwrap().unwrap();

    // Weeks later: an extractor speaks, a tag is given, still in the head.
    log.append(&claim(
        "exif:date-time-original",
        json!("2019-07-14T11:02:41"),
        "2026-09-22T08:30:00Z",
        "extractor:exif-rs/0.7",
    ))
    .unwrap();
    log.append(&claim(
        "user:tag",
        json!("holiday"),
        "2026-10-05T19:00:00Z",
        "user",
    ))
    .unwrap();

    // The fold, and the question.
    assert_eq!(
        index.fold(&log).unwrap(),
        Folded {
            segments: 1,
            claims: 3,
            head: 2
        }
    );
    let answer = index.about(&photo()).unwrap();
    assert_eq!(answer.len(), 5, "everything ever said about the blob");
    assert_eq!(
        answer[1].value(),
        Some(&json!(4_194_304)),
        "a number came back as a number"
    );
    assert_eq!(
        answer[4].attribute().as_str(),
        "user:tag",
        "log order: the head's claims last"
    );

    // The index is a cache, and here is the proof: delete it, fold again,
    // ask again — same answer, out of nothing but the log.
    drop(index);
    std::fs::remove_file(dir.path().join("cache").join("index.sqlite")).unwrap();
    let mut rebuilt = Index::open(dir.path().join("cache").join("index.sqlite")).unwrap();
    rebuilt.fold(&log).unwrap();
    assert_eq!(
        rebuilt.about(&photo()).unwrap(),
        answer,
        "truth and query are separate systems, and only one of them was deleted"
    );
}

#[test]
fn the_skeleton_walks_from_disk() {
    let dir = TempDir::new().unwrap();
    let (log, mut index) = archive(&dir);
    let content = Store::builder(dir.path().join("content"))
        .suffix("")
        .depth(2)
        .create()
        .unwrap();

    // A tree on disk, taken in whole.
    let tree = dir.path().join("in");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("notes.txt"), b"hello world").unwrap();
    std::fs::write(tree.join("photo.jpg"), [0xFF, 0xD8, 0xFF, 0xE0, 0x00]).unwrap();
    let run = ossuary_core::ingest(
        &content,
        &log,
        &tree,
        "atlas.example.net",
        &ossuary_core::Excludes::none(),
        None,
    )
    .unwrap();
    assert_eq!(run.stored, 2);

    // Sealed, folded, asked.
    log.seal().unwrap().unwrap();
    index.fold(&log).unwrap();
    let subject = Subject::parse(&format!(
        "sha256:{}",
        immure::Algorithm::Sha256.hash(b"hello world")
    ))
    .unwrap();
    let answer = index.about(&subject).unwrap();
    assert_eq!(answer.len(), 7, "the seven day-one facts");

    // And the subject leads back to the bytes: the archive holds content
    // and everything known about it, and either finds the other.
    let digest = immure::Digest::parse(subject.hex()).unwrap();
    assert_eq!(
        content.read(&digest).unwrap().as_deref(),
        Some(&b"hello world"[..])
    );
}
