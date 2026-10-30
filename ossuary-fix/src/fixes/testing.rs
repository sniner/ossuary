//! Archives in the forms the fixes read, built by hand for their tests.

use std::fs;

use immure::Digest;
use ossuary_core::{
    Algorithm, Archive, Attribute, Claim, Run, Source, Subject, Timestamp, audit_log,
};
use serde_json::json;
use tempfile::TempDir;

pub fn archive(dir: &TempDir) -> Archive {
    Archive::create(dir.path().join("archive"), Algorithm::Sha256).unwrap()
}

/// One claim about a file of its own, recorded at `at`.
pub fn claim(archive: &Archive, at: &str) -> Claim {
    let (_, entry) = archive.content().add(at.as_bytes()).unwrap();
    Claim::assert(
        Subject::parse(entry.digest().as_str()).unwrap(),
        Attribute::parse("file:size").unwrap(),
        json!(at.len()),
        Timestamp::parse(at).unwrap(),
        Source::parse("test").unwrap(),
        Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
    )
    .unwrap()
}

/// A segment stored straight into the claims store, its header given
/// whole, with one claim recorded at `at` unless that is `None`.
pub fn segment(archive: &Archive, header: &str, at: Option<&str>) -> Digest {
    let mut text = format!("{header}\n");
    if let Some(at) = at {
        text.push_str(&claim(archive, at).to_line());
        text.push('\n');
    }
    let (_, entry) = archive.log().store().add(text.as_bytes()).unwrap();
    entry.digest().clone()
}

/// A first segment, in either form: it names nothing.
pub fn first(archive: &Archive, at: &str) -> Digest {
    segment(archive, "{\"ossuary-segment\":1}", Some(at))
}

/// A segment in the form of ossuary 0.14: `previous` a list.
pub fn listed(archive: &Archive, previous: &[&Digest], at: &str) -> Digest {
    segment(archive, &list_header(previous), Some(at))
}

/// A segment in the current form: `previous` one name.
pub fn after(archive: &Archive, previous: &Digest, at: &str) -> Digest {
    segment(
        archive,
        &format!("{{\"ossuary-segment\":1,\"previous\":\"{previous}\"}}"),
        Some(at),
    )
}

pub fn list_header(previous: &[&Digest]) -> String {
    let names: Vec<String> = previous.iter().map(|name| format!("\"{name}\"")).collect();
    format!(
        "{{\"ossuary-segment\":1,\"previous\":[{}]}}",
        names.join(",")
    )
}

/// The open segment, naming `previous` in the form of ossuary 0.14.
pub fn listed_head(archive: &Archive, previous: &[&Digest]) {
    fs::write(
        archive.log().head_path(),
        format!("{}\n", list_header(previous)),
    )
    .unwrap();
}

/// The open segment, naming `previous` in the current form.
pub fn head(archive: &Archive, previous: &Digest) {
    fs::write(
        archive.log().head_path(),
        ossuary_core::head_after(previous),
    )
    .unwrap();
}

/// The time of each segment's claim, in the order of the chain.
pub fn times(archive: &Archive) -> Vec<String> {
    let log = archive.log();
    log.segments()
        .unwrap()
        .iter()
        .map(|segment| log.read(segment.digest()).unwrap()[0].time().to_string())
        .collect()
}

pub fn sound(archive: &Archive) -> bool {
    audit_log(archive.log()).unwrap().findings() == 0
}

/// A sealed segment's file deleted outright.
pub fn lose(archive: &Archive, digest: &Digest) {
    let path = archive.log().store().find(digest).unwrap().unwrap();
    fs::remove_file(path).unwrap();
}
