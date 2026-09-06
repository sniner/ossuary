//! The audit: the archive held against its own record.
//!
//! An archive answers for two things at once — the bytes it holds and the
//! claims it recorded about them — and the audit proves the two still
//! agree. Every entry of both blob stores is read whole and re-hashed: a
//! name must still be true of its bytes. Every sealed segment and the
//! open head must read back claim by claim. And every subject the claims
//! speak of must be held by a store: the claims are the record, and a
//! record that names what nothing holds has found a loss, not a policy —
//! nothing is ever deliberately removed from an archive. The other
//! direction is milder: bytes held that no claim mentions are noted as
//! observations, never findings, because a run interrupted between
//! storing and recording leaves such entries legitimately, and the next
//! arrival of the same bytes records them.
//!
//! The whole pass works from the truth tiers alone: stores, segments,
//! head. The cache is never consulted — an audit is the tool for the day
//! nothing else is trusted, so it leans on nothing the archive could
//! rebuild from what is being audited.

use std::collections::BTreeSet;

use immure::Store;

use crate::claim::{Claim, Subject, Value};
use crate::error::Result;
use crate::log::Log;

/// Attributes whose values name other subjects. A reference in a value
/// is a reference like the claim's own subject, and the audit follows
/// both — a derived file's origin must be held no less than the derived
/// file itself.
const LINKS: [&str; 1] = ["derive:derived-from"];

/// One blob store's fixity: every entry read whole, its bytes re-hashed
/// against the name they are filed under.
#[derive(Debug)]
pub struct StoreAudit {
    /// Entries the walk met.
    pub checked: usize,
    /// Entries whose bytes are no longer what their names say.
    pub damaged: Vec<String>,
    /// Entries nothing could be established about, with what stood in
    /// the way. Unreadable is not damaged: blaming the entry for a
    /// permission would take a healthy name's word away for a reason
    /// that is not the entry's.
    pub unreadable: Vec<(String, String)>,
    /// Every name the store holds, damaged or not — what the presence
    /// check runs against. A damaged entry is still held; it is already
    /// a finding once, and missing on top would count the same wound
    /// twice.
    held: BTreeSet<String>,
}

/// Audit one blob store: walk it whole, verify every entry.
///
/// # Errors
///
/// [`Error::Store`](crate::Error::Store) when the store cannot be walked
/// at all; what a single entry has to answer for lands in the report
/// instead.
pub fn audit_store(store: &Store) -> Result<StoreAudit> {
    let mut report = StoreAudit {
        checked: 0,
        damaged: Vec::new(),
        unreadable: Vec::new(),
        held: BTreeSet::new(),
    };
    for entry in store.entries() {
        let entry = entry?;
        let name = entry.digest().as_str().to_string();
        report.checked += 1;
        report.held.insert(name.clone());
        match store.verify(&entry) {
            Ok(true) => {}
            Ok(false) => report.damaged.push(name),
            Err(error) => report.unreadable.push((name, error.to_string())),
        }
    }
    Ok(report)
}

/// The claim log read back whole: every sealed segment, the open head.
#[derive(Debug)]
pub struct LogAudit {
    /// Sealed segments the walk met.
    pub segments: usize,
    /// Claims read back, the open head's included.
    pub claims: usize,
    /// Segments whose bytes are no longer what their names say. A
    /// damaged segment is not parsed: the finding stands, and lines
    /// from bytes that lie would only decorate it.
    pub damaged: Vec<String>,
    /// Segments nothing could be established about, with what stood in
    /// the way.
    pub unreadable: Vec<(String, String)>,
    /// Segments true to their names that will not read back as
    /// segments, with the first thing wrong.
    pub broken: Vec<(String, String)>,
    /// What stands in the open head's way, when something does.
    pub head_broken: Option<String>,
    /// Every subject the readable claims speak of: their subjects, and
    /// the subjects link values name. Read from the whole history,
    /// retractions included — a retraction withdraws a statement, never
    /// bytes.
    referenced: BTreeSet<String>,
}

/// Audit the claim log: fixity of every sealed segment, every line of
/// every segment that is true to its name, the open head last.
///
/// # Errors
///
/// [`Error::Store`](crate::Error::Store) when the claims store cannot
/// be walked at all; what a single segment or the head has to answer
/// for lands in the report instead.
pub fn audit_log(log: &Log) -> Result<LogAudit> {
    let mut report = LogAudit {
        segments: 0,
        claims: 0,
        damaged: Vec::new(),
        unreadable: Vec::new(),
        broken: Vec::new(),
        head_broken: None,
        referenced: BTreeSet::new(),
    };
    let store = log.store();
    for entry in store.entries() {
        let entry = entry?;
        let name = entry.digest().as_str().to_string();
        report.segments += 1;
        match store.verify(&entry) {
            Ok(true) => {}
            Ok(false) => {
                report.damaged.push(name);
                continue;
            }
            Err(error) => {
                report.unreadable.push((name, error.to_string()));
                continue;
            }
        }
        match log.read(entry.digest()) {
            Ok(claims) => {
                report.claims += claims.len();
                for claim in &claims {
                    reference(claim, &mut report.referenced);
                }
            }
            Err(error) => report.broken.push((name, error.to_string())),
        }
    }
    match log.head() {
        Ok(claims) => {
            report.claims += claims.len();
            for claim in &claims {
                reference(claim, &mut report.referenced);
            }
        }
        Err(error) => report.head_broken = Some(error.to_string()),
    }
    Ok(report)
}

/// What one claim points at: its subject always, and — for an attribute
/// whose values are references — the subject its value names.
fn reference(claim: &Claim, referenced: &mut BTreeSet<String>) {
    referenced.insert(claim.subject().as_str().to_string());
    if LINKS.contains(&claim.attribute().as_str()) {
        if let Some(Value::String(text)) = claim.value() {
            if let Ok(subject) = Subject::parse(text) {
                referenced.insert(subject.as_str().to_string());
            }
        }
    }
}

/// The whole audit: both stores, the log, and the two held against each
/// other.
#[derive(Debug)]
pub struct Audit {
    /// What was taken in, each entry proved against its name.
    pub content: StoreAudit,
    /// What tools made, proved the same way.
    pub derived: StoreAudit,
    /// The record, read back whole.
    pub log: LogAudit,
    /// Subjects the claims speak of that no store holds — each one a
    /// loss, because absence has no innocent reading.
    pub missing: Vec<String>,
    /// Entries of `content/` no claim speaks of. An observation, not a
    /// finding.
    pub unrecorded_content: Vec<String>,
    /// Entries of `derived/` no claim speaks of. An observation, not a
    /// finding.
    pub unrecorded_derived: Vec<String>,
}

impl Audit {
    /// Hold the pieces against each other: what is spoken of must be
    /// held — by either store, a subject never says where it lies — and
    /// what is held ought to be spoken of.
    #[must_use]
    pub fn assemble(content: StoreAudit, derived: StoreAudit, log: LogAudit) -> Audit {
        let missing: Vec<String> = log
            .referenced
            .iter()
            .filter(|name| !content.held.contains(*name) && !derived.held.contains(*name))
            .cloned()
            .collect();
        let unrecorded = |store: &StoreAudit| -> Vec<String> {
            store
                .held
                .iter()
                .filter(|name| !log.referenced.contains(*name))
                .cloned()
                .collect()
        };
        Audit {
            missing,
            unrecorded_content: unrecorded(&content),
            unrecorded_derived: unrecorded(&derived),
            content,
            derived,
            log,
        }
    }

    /// How many findings stand — what the verdict and the exit code
    /// count. Observations are not among them.
    #[must_use]
    pub fn findings(&self) -> usize {
        let store = |report: &StoreAudit| report.damaged.len() + report.unreadable.len();
        store(&self.content)
            + store(&self.derived)
            + self.log.damaged.len()
            + self.log.unreadable.len()
            + self.log.broken.len()
            + usize::from(self.log.head_broken.is_some())
            + self.missing.len()
    }

    /// Whether the archive is sound: no finding stands. Observations
    /// may — soundness is about loss and damage, not tidiness.
    #[must_use]
    pub fn is_sound(&self) -> bool {
        self.findings() == 0
    }
}

#[cfg(test)]
mod tests {
    use immure::{Algorithm, Digest};
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::Archive;
    use crate::claim::{Attribute, Source, Timestamp};

    fn archive(dir: &TempDir) -> Archive {
        Archive::create(dir.path().join("archive"), Algorithm::Sha256).unwrap()
    }

    /// Bytes into the content store with one claim on the record — the
    /// smallest thing the audit calls whole.
    fn take(archive: &Archive, bytes: &[u8]) -> Subject {
        let (_, entry) = archive.content().add(bytes).unwrap();
        let subject = Subject::parse(entry.digest().as_str()).unwrap();
        record(archive, &subject, "file:size", json!(bytes.len()));
        subject
    }

    fn record(archive: &Archive, subject: &Subject, attribute: &str, value: Value) {
        let claim = Claim::assert(
            subject.clone(),
            Attribute::parse(attribute).unwrap(),
            value,
            Timestamp::parse("2026-09-06T12:00:00Z").unwrap(),
            Source::parse("test").unwrap(),
        )
        .unwrap();
        archive.log().append(&claim).unwrap();
    }

    fn run(archive: &Archive) -> Audit {
        Audit::assemble(
            audit_store(archive.content()).unwrap(),
            audit_store(archive.derived()).unwrap(),
            audit_log(archive.log()).unwrap(),
        )
    }

    /// Damage an entry in place — past the read-only mode the store put
    /// on it, which tampering does not politely honour.
    fn tamper(path: &std::path::Path, bytes: &[u8]) {
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        #[allow(
            clippy::permissions_set_readonly_false,
            reason = "the test plays the corruption"
        )]
        permissions.set_readonly(false);
        std::fs::set_permissions(path, permissions).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn a_sound_archive_has_nothing_to_report() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"kept for good");
        archive.log().seal().unwrap();
        take(&archive, b"and this one too");

        let audit = run(&archive);

        assert!(audit.is_sound());
        assert_eq!(audit.findings(), 0);
        assert_eq!(audit.content.checked, 2);
        assert_eq!(audit.log.segments, 1);
        assert_eq!(audit.log.claims, 2, "one sealed, one still in the head");
        assert!(audit.missing.is_empty());
        assert!(audit.unrecorded_content.is_empty());
        assert!(audit.unrecorded_derived.is_empty());
    }

    #[test]
    fn bytes_no_longer_true_to_their_name_are_damaged() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let subject = take(&archive, b"original bytes");
        let digest = Digest::parse(subject.as_str()).unwrap();
        let path = archive.content().find(&digest).unwrap().unwrap();
        tamper(&path, b"tampered");

        let audit = run(&archive);

        assert_eq!(audit.content.damaged, vec![subject.as_str().to_string()]);
        assert!(!audit.is_sound());
        assert_eq!(
            audit.findings(),
            1,
            "damaged is still held — one finding, not damaged and missing both"
        );
        assert!(audit.missing.is_empty());
    }

    #[test]
    fn what_the_claims_speak_of_must_be_held() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let held = take(&archive, b"held");
        let ghost = Subject::parse(&"ab".repeat(32)).unwrap();
        record(&archive, &ghost, "user:tag", json!("gone"));
        let linked = Subject::parse(&"cd".repeat(32)).unwrap();
        record(
            &archive,
            &held,
            "derive:derived-from",
            json!(linked.as_str()),
        );
        // A string value outside the link attributes is words, not a name.
        record(&archive, &held, "user:comment", json!(&"ef".repeat(32)));

        let audit = run(&archive);

        assert_eq!(
            audit.missing,
            vec![ghost.as_str().to_string(), linked.as_str().to_string()],
            "a claim's subject and a link's value are both spoken of"
        );
        assert_eq!(audit.findings(), 2);
    }

    #[test]
    fn held_bytes_no_claim_speaks_of_are_noted_not_found() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"recorded");
        let (_, stray) = archive.content().add(b"stray").unwrap();

        let audit = run(&archive);

        assert!(audit.is_sound(), "an observation is not a finding");
        assert_eq!(
            audit.unrecorded_content,
            vec![stray.digest().as_str().to_string()]
        );
    }

    #[test]
    fn a_subject_held_by_either_store_is_held() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let (_, entry) = archive.derived().add(b"a derived file").unwrap();
        let subject = Subject::parse(entry.digest().as_str()).unwrap();
        record(&archive, &subject, "file:mime", json!("text/plain"));

        let audit = run(&archive);

        assert!(
            audit.missing.is_empty(),
            "a subject never says where it lies"
        );
        assert!(audit.is_sound());
        assert!(audit.unrecorded_derived.is_empty());
    }

    #[test]
    fn a_segment_that_will_not_read_back_is_named() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let (_, entry) = archive.log().store().add(b"not a header\n").unwrap();
        std::fs::write(
            archive.root().join("head.jsonl"),
            "{\"ossuary-segment\":1}\nnot a claim\n",
        )
        .unwrap();

        let audit = run(&archive);

        assert_eq!(audit.log.broken.len(), 1);
        assert_eq!(audit.log.broken[0].0, entry.digest().as_str());
        assert!(audit.log.head_broken.is_some());
        assert_eq!(audit.findings(), 2);
    }

    #[test]
    fn a_damaged_segment_is_one_finding_not_two() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"sealed away");
        let segment = archive.log().seal().unwrap().unwrap();
        let path = archive
            .log()
            .store()
            .find(segment.digest())
            .unwrap()
            .unwrap();
        tamper(&path, b"garbage");

        let audit = run(&archive);

        assert_eq!(
            audit.log.damaged,
            vec![segment.digest().as_str().to_string()]
        );
        assert!(
            audit.log.broken.is_empty(),
            "a damaged segment is not parsed"
        );
        assert_eq!(audit.log.claims, 0, "its claims are lost with it");
        assert_eq!(
            audit.findings(),
            1,
            "the blob it spoke of turns unrecorded, an observation, not a second finding"
        );
    }
}
