//! The audit: the archive held against its own record.
//!
//! An archive answers for two things at once — the bytes it holds and the
//! claims it recorded about them — and the audit proves the two still
//! agree. Every entry of both blob stores is read whole and re-hashed: a
//! name must still be true of its bytes. Every sealed segment and the
//! open head must read back claim by claim, and the chain they form must
//! hold: every segment a header names as the one sealed before it must
//! be held, because a sealed segment that is gone is a loss the record
//! itself points at. And every subject the claims speak of must be held
//! by a store: the claims are the record, and a record that names what
//! nothing holds has found a loss, not a policy — nothing is ever
//! deliberately removed from an archive. The other direction is milder:
//! bytes held that no claim mentions are noted as observations, never
//! findings, because a run interrupted between storing and recording
//! leaves such entries legitimately, and the next arrival of the same
//! bytes records them. Not so a segment the walk back from the open head
//! does not reach: it is not part of the record's chain, whatever it
//! holds, and that is a finding.
//!
//! Bytes held by both stores are an observation of their own: a file
//! that was won as a derived file first and taken in as an original
//! later stands in `derived/` and `content/` alike, under the same name,
//! and the log never says which store answers for it. Nothing is wrong
//! with that; the audit notes each such twin with how both copies fared
//! (a [`Twin`]), because the derived copy is the one thing an archive
//! holds that it can let go of without loss, and `weed` does that.
//!
//! The chain is walked back from the open head, segment by segment,
//! until a first segment. It must reach every segment the store holds.
//! Where it stops early, at a name the store does not hold or at a
//! segment that will not read, the order of what lies beyond is not
//! known, and the audit says where the walk stopped instead of guessing
//! at it. Repairing the chain rewrites segments, which only
//! `ossuary-fix` does.
//!
//! The whole pass works from the truth tiers alone: stores, segments,
//! head. The cache is never consulted — an audit is the tool for the day
//! nothing else is trusted, so it leans on nothing the archive could
//! rebuild from what is being audited.

use std::collections::{BTreeMap, BTreeSet};

use immure::{Digest, Store};

use crate::claim::{Claim, Subject, Value};
use crate::error::Result;
use crate::log::Log;

/// Attributes whose values name other subjects. A reference in a value
/// is a reference like the claim's own subject, and the audit follows
/// both — a derived file's origin must be held no less than the derived
/// file itself.
pub const LINKS: [&str; 1] = ["prov:origin"];

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
    /// twice. Keyed by the name as the claims spell it, with the digest
    /// it was walked under beside it.
    held: BTreeMap<String, Digest>,
}

impl StoreAudit {
    /// What the walk established about each entry that is not sound —
    /// every name not in the map verified.
    fn troubles(&self) -> BTreeMap<&str, Fixity> {
        let mut troubles = BTreeMap::new();
        for name in &self.damaged {
            troubles.insert(name.as_str(), Fixity::Damaged);
        }
        for (name, error) in &self.unreadable {
            troubles.insert(name.as_str(), Fixity::Unreadable(error.clone()));
        }
        troubles
    }
}

/// What one look at one entry established: the bytes are what the name
/// says, they are not, or nothing could be established at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fixity {
    /// Read whole and re-hashed: the name is true of its bytes.
    Sound,
    /// Read whole and re-hashed: the name is not.
    Damaged,
    /// Not read, with what stood in the way. Not damaged: the entry
    /// answers for its bytes, not for a permission.
    Unreadable(String),
}

impl Fixity {
    /// One entry read whole and re-hashed.
    pub(crate) fn of(store: &Store, entry: &immure::Entry) -> Fixity {
        match store.verify(entry) {
            Ok(true) => Fixity::Sound,
            Ok(false) => Fixity::Damaged,
            Err(error) => Fixity::Unreadable(error.to_string()),
        }
    }
}

/// One content held by both stores: won as a derived file, and taken in
/// as an original as well, in whichever order. The log speaks of the
/// subject, never of a store, so the two copies are one file to it, and
/// `content/` answers for it first. Which is why the derived copy can go
/// — once both are known to be sound, or the one in `content/` is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Twin {
    /// The name both copies are filed under.
    pub digest: Digest,
    /// How the copy in `content/` fared.
    pub content: Fixity,
    /// How the copy in `derived/` fared.
    pub derived: Fixity,
}

impl Twin {
    /// Whether the one in `content/` is the damaged copy and the one in
    /// `derived/` the sound one: the case where `weed` with repair puts
    /// the sound bytes back under the original's name.
    #[must_use]
    pub fn repairable(&self) -> bool {
        self.content == Fixity::Damaged && self.derived == Fixity::Sound
    }
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
        held: BTreeMap::new(),
    };
    for entry in store.entries() {
        let entry = entry?;
        let name = entry.digest().as_str().to_string();
        report.checked += 1;
        report.held.insert(name.clone(), entry.digest().clone());
        match store.verify(&entry) {
            Ok(true) => {}
            Ok(false) => report.damaged.push(name),
            Err(error) => report.unreadable.push((name, error.to_string())),
        }
    }
    Ok(report)
}

/// The claim log read back whole: every sealed segment, the open head,
/// and the chain they form.
#[derive(Debug)]
pub struct LogAudit {
    /// Sealed segments the store holds.
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
    /// Whether the open head was lost: it is missing or empty while
    /// sealed segments are there, and the claims recorded since the
    /// last seal went with it.
    pub head_lost: bool,
    /// The segments the walk back from the open head reached, in log
    /// order: the first segment first.
    pub chain: Vec<String>,
    /// Where the walk ended.
    pub ends: Ends,
    /// Readable segments that the walk does not reach. Filled only when
    /// the walk ended at a first segment; behind a break, what the walk
    /// did not reach is the break's to answer for.
    pub unreached: Vec<String>,
    /// Every subject the readable claims speak of: their subjects, and
    /// the subjects link values name. Read from the whole history,
    /// retractions included — a retraction withdraws a statement, never
    /// bytes.
    referenced: BTreeSet<String>,
}

/// Where the walk back from the open head ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ends {
    /// At a first segment, or at once in an archive with nothing
    /// sealed: the chain is whole.
    First,
    /// Before it began: the open head was lost or does not read, which
    /// is a finding of its own.
    NoHead,
    /// At a name the store does not hold: the segment that names it, or
    /// [`HEAD`] for the open head, and the name. A sealed segment lost,
    /// the finding the chain exists for.
    Missing { segment: String, missing: String },
    /// At a segment the store holds that is damaged, unreadable or does
    /// not read as a segment, which is a finding of its own.
    Blocked(String),
}

impl LogAudit {
    /// How many findings the log alone stands for: damage, segments
    /// that will not read, a lost head, a lost segment, segments outside
    /// the chain.
    #[must_use]
    pub fn findings(&self) -> usize {
        self.damaged.len()
            + self.unreadable.len()
            + self.broken.len()
            + usize::from(self.head_broken.is_some())
            + usize::from(self.head_lost)
            + usize::from(matches!(self.ends, Ends::Missing { .. }))
            + self.unreached.len()
    }
}

/// The name a walk gives the open head where a segment would stand.
pub const HEAD: &str = "head";

/// Audit the claim log: fixity of every sealed segment, every line of
/// every segment that is true to its name, the open head last, and the
/// chain the headers form held against what the store holds.
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
        head_lost: false,
        chain: Vec::new(),
        ends: Ends::First,
        unreached: Vec::new(),
        referenced: BTreeSet::new(),
    };
    // Every name the store holds, damaged or not: a walk that meets a
    // held name it cannot read is blocked there, not told the segment
    // is gone.
    let mut held = BTreeSet::new();
    // The readable segments, by name, with the segment each names as
    // sealed before it.
    let mut previous: BTreeMap<String, Option<String>> = BTreeMap::new();
    let hex = |digest: &Digest| digest.as_str().to_string();
    let store = log.store();
    for entry in store.entries() {
        let entry = entry?;
        let name = entry.digest().as_str().to_string();
        report.segments += 1;
        held.insert(name.clone());
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
        match log.contents(entry.digest()) {
            Ok(contents) => {
                report.claims += contents.claims().len();
                for claim in contents.claims() {
                    reference(claim, &mut report.referenced);
                }
                previous.insert(name, contents.previous().map(hex));
            }
            Err(error) => report.broken.push((name, error.spelled())),
        }
    }
    report.head_lost = !held.is_empty() && log.head_lost()?;
    let open = match log.head_contents() {
        Ok(contents) => {
            report.claims += contents.claims().len();
            for claim in contents.claims() {
                reference(claim, &mut report.referenced);
            }
            contents.previous().map(hex)
        }
        Err(error) => {
            report.head_broken = Some(error.spelled());
            None
        }
    };
    if report.head_lost || report.head_broken.is_some() {
        report.ends = Ends::NoHead;
    } else {
        walk(&mut report, &held, &previous, open);
    }
    Ok(report)
}

/// Walk back from the open head, which names `next`, to a first segment,
/// and note where the walk ended and what it did not reach.
fn walk(
    report: &mut LogAudit,
    held: &BTreeSet<String>,
    previous: &BTreeMap<String, Option<String>>,
    mut next: Option<String>,
) {
    let mut named_by = HEAD.to_string();
    let mut reached = BTreeSet::new();
    while let Some(name) = next.take() {
        let Some(before) = previous.get(&name) else {
            report.ends = if held.contains(&name) {
                Ends::Blocked(name)
            } else {
                Ends::Missing {
                    segment: named_by,
                    missing: name,
                }
            };
            break;
        };
        reached.insert(name.clone());
        report.chain.push(name.clone());
        next.clone_from(before);
        named_by = name;
    }
    report.chain.reverse();
    if report.ends == Ends::First {
        report.unreached = previous
            .keys()
            .filter(|name| !reached.contains(*name))
            .cloned()
            .collect();
    }
}

/// What one claim points at: its subject always, and — for an attribute
/// whose values are references — the subject its value names.
pub(crate) fn reference(claim: &Claim, referenced: &mut BTreeSet<String>) {
    referenced.insert(claim.subject().as_str().to_string());
    if LINKS.contains(&claim.attribute().as_str())
        && let Some(Value::String(text)) = claim.value()
        && let Ok(subject) = Subject::parse(text)
    {
        referenced.insert(subject.as_str().to_string());
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
    /// Contents held by both stores, each with how its two copies fared.
    /// An observation, not a finding: what is wrong with either copy
    /// already stands as one on its store.
    pub twins: Vec<Twin>,
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
            .filter(|name| !content.held.contains_key(*name) && !derived.held.contains_key(*name))
            .cloned()
            .collect();
        let unrecorded = |store: &StoreAudit| -> Vec<String> {
            store
                .held
                .keys()
                .filter(|name| !log.referenced.contains(*name))
                .cloned()
                .collect()
        };
        let content_troubles = content.troubles();
        let derived_troubles = derived.troubles();
        let fixity = |troubles: &BTreeMap<&str, Fixity>, name: &str| {
            troubles.get(name).cloned().unwrap_or(Fixity::Sound)
        };
        let twins = content
            .held
            .iter()
            .filter(|(name, _)| derived.held.contains_key(*name))
            .map(|(name, digest)| Twin {
                digest: digest.clone(),
                content: fixity(&content_troubles, name),
                derived: fixity(&derived_troubles, name),
            })
            .collect();
        Audit {
            missing,
            unrecorded_content: unrecorded(&content),
            unrecorded_derived: unrecorded(&derived),
            twins,
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
        store(&self.content) + store(&self.derived) + self.log.findings() + self.missing.len()
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
        record_at(archive, subject, attribute, value, "2026-09-06T12:00:00Z");
    }

    fn record_at(archive: &Archive, subject: &Subject, attribute: &str, value: Value, at: &str) {
        let claim = Claim::assert(
            subject.clone(),
            Attribute::parse(attribute).unwrap(),
            value,
            Timestamp::parse(at).unwrap(),
            Source::parse("test").unwrap(),
            crate::claim::Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
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
        let first = archive.log().seal().unwrap().unwrap();
        take(&archive, b"and this one too");
        let second = archive.log().seal().unwrap().unwrap();
        take(&archive, b"still in the head");

        let audit = run(&archive);

        assert!(audit.is_sound());
        assert_eq!(audit.findings(), 0);
        assert_eq!(audit.content.checked, 3);
        assert_eq!(audit.log.segments, 2);
        assert_eq!(audit.log.claims, 3, "two sealed, one still in the head");
        assert!(audit.missing.is_empty());
        assert!(audit.unrecorded_content.is_empty());
        assert!(audit.unrecorded_derived.is_empty());
        assert_eq!(
            audit.log.chain,
            vec![name(&first), name(&second)],
            "the chain in log order, first segment first"
        );
        assert_eq!(audit.log.ends, Ends::First);
        assert!(audit.log.unreached.is_empty());
    }

    #[test]
    fn an_archive_with_nothing_sealed_is_whole() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"in the head");

        let audit = run(&archive);

        assert!(audit.is_sound());
        assert!(audit.log.chain.is_empty());
        assert_eq!(audit.log.ends, Ends::First);
    }

    /// Delete a sealed segment's file outright — the loss the chain
    /// exists to show.
    fn lose(archive: &Archive, digest: &Digest) {
        let path = archive.log().store().find(digest).unwrap().unwrap();
        std::fs::remove_file(path).unwrap();
    }

    fn name(segment: &crate::Segment) -> String {
        segment.digest().as_str().to_string()
    }

    #[test]
    fn a_lost_segment_is_found_by_the_one_sealed_after_it() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"first");
        archive.log().seal().unwrap().unwrap();
        take(&archive, b"second");
        let second = archive.log().seal().unwrap().unwrap();
        take(&archive, b"third");
        let third = archive.log().seal().unwrap().unwrap();
        lose(&archive, second.digest());

        let audit = run(&archive);

        assert_eq!(
            audit.log.ends,
            Ends::Missing {
                segment: name(&third),
                missing: name(&second),
            }
        );
        assert_eq!(audit.log.chain, vec![name(&third)]);
        assert!(
            audit.log.unreached.is_empty(),
            "the first segment lies behind the break, the break answers for it"
        );
        assert_eq!(audit.findings(), 1);
        assert!(!audit.is_sound());
        assert_eq!(
            audit.unrecorded_content.len(),
            1,
            "the blob the lost segment spoke of turns unrecorded — an observation, the loss is counted once"
        );
    }

    #[test]
    fn a_lost_last_segment_is_found_by_the_open_head() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"sealed and lost");
        let segment = archive.log().seal().unwrap().unwrap();
        lose(&archive, segment.digest());

        let audit = run(&archive);

        assert_eq!(
            audit.log.ends,
            Ends::Missing {
                segment: HEAD.to_string(),
                missing: name(&segment),
            }
        );
        assert_eq!(audit.log.segments, 0);
        assert_eq!(audit.findings(), 1);
    }

    #[test]
    fn a_damaged_predecessor_blocks_the_walk_and_counts_once() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"damaged later");
        let segment = archive.log().seal().unwrap().unwrap();
        let path = archive
            .log()
            .store()
            .find(segment.digest())
            .unwrap()
            .unwrap();
        tamper(&path, b"garbage");

        let audit = run(&archive);

        assert_eq!(audit.log.damaged.len(), 1);
        assert_eq!(
            audit.log.ends,
            Ends::Blocked(name(&segment)),
            "damaged is still held, the walk stops at it rather than calling it gone"
        );
        assert_eq!(audit.findings(), 1);
    }

    #[test]
    fn a_lost_head_is_a_finding_and_the_walk_does_not_begin() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"sealed");
        archive.log().seal().unwrap().unwrap();
        std::fs::remove_file(archive.root().join("head.jsonl")).unwrap();

        let audit = run(&archive);

        assert!(audit.log.head_lost);
        assert_eq!(audit.log.ends, Ends::NoHead);
        assert!(audit.log.chain.is_empty());
        assert_eq!(audit.findings(), 1);
    }

    #[test]
    fn a_head_begun_anew_leaves_the_segments_before_it_unreached() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"before the loss");
        let first = archive.log().seal().unwrap().unwrap();
        // The head lost and begun anew naming nothing, which `append`
        // refuses to do: the audit still has to read an archive in that
        // state.
        std::fs::write(
            archive.root().join("head.jsonl"),
            "{\"ossuary-segment\":1}\n",
        )
        .unwrap();
        take(&archive, b"after the loss");
        let anew = archive.log().seal().unwrap().unwrap();

        let audit = run(&archive);

        assert_eq!(audit.log.chain, vec![name(&anew)]);
        assert_eq!(audit.log.ends, Ends::First);
        assert_eq!(audit.log.unreached, vec![name(&first)]);
        assert_eq!(audit.findings(), 1);
    }

    #[test]
    fn a_segment_outside_the_chain_is_unreached() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"in the chain");
        archive.log().seal().unwrap().unwrap();
        let (_, stray) = archive
            .log()
            .store()
            .add(b"{\"ossuary-segment\":1}\n")
            .unwrap();

        let audit = run(&archive);

        assert_eq!(
            audit.log.unreached,
            vec![stray.digest().as_str().to_string()]
        );
        assert_eq!(audit.findings(), 1);
    }

    #[test]
    fn a_header_with_previous_as_a_list_is_broken() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let (_, entry) = archive
            .log()
            .store()
            .add(b"{\"ossuary-segment\":1}\n")
            .unwrap();
        std::fs::write(
            archive.root().join("head.jsonl"),
            format!(
                "{{\"ossuary-segment\":1,\"previous\":[\"{}\"]}}\n",
                entry.digest()
            ),
        )
        .unwrap();

        let audit = run(&archive);

        assert!(
            audit
                .log
                .head_broken
                .as_deref()
                .is_some_and(|reason| reason.contains("ossuary-fix list")),
            "the reason names the way out: {:?}",
            audit.log.head_broken
        );
        assert_eq!(audit.log.ends, Ends::NoHead);
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
        record(&archive, &held, "prov:origin", json!(linked.as_str()));
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
        assert!(audit.twins.is_empty());
    }

    #[test]
    fn a_content_held_by_both_stores_is_a_twin_and_not_a_finding() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let subject = take(&archive, b"an attachment, later taken in");
        archive
            .derived()
            .add(b"an attachment, later taken in")
            .unwrap();

        let audit = run(&archive);

        assert!(audit.is_sound());
        assert_eq!(
            audit.twins,
            vec![Twin {
                digest: Digest::parse(subject.as_str()).unwrap(),
                content: Fixity::Sound,
                derived: Fixity::Sound,
            }]
        );
        assert!(!audit.twins[0].repairable());
    }

    #[test]
    fn a_twin_carries_how_each_copy_fared() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        take(&archive, b"sound where it was won");
        let (_, derived) = archive.derived().add(b"sound where it was won").unwrap();
        let content = archive
            .content()
            .find(derived.digest())
            .unwrap()
            .expect("taken in");
        tamper(&content, b"sound where it was won?");

        let audit = run(&archive);

        assert_eq!(audit.findings(), 1, "the damage is the content store's");
        assert_eq!(audit.twins.len(), 1);
        assert_eq!(audit.twins[0].content, Fixity::Damaged);
        assert_eq!(audit.twins[0].derived, Fixity::Sound);
        assert!(audit.twins[0].repairable());
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
