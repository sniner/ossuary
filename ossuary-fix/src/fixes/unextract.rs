//! The claims of extractors removed from the log, with the derived
//! files that only they recorded. Afterwards the archive is as it was
//! before `ossuary extract` first ran, and the next run examines every
//! file again.
//!
//! A claim is an extractor's when its source begins with `extractor:`.
//! The lines of all other claims are kept byte for byte, so their ids do
//! not change and a retraction that names one still finds it. A
//! retraction by another source that names only removed claims is
//! removed with them; one that also names a kept claim stays as it is,
//! and the ids of removed claims in it retract nothing.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, Result, bail};
use immure::Digest;
use ossuary_core::{Algorithm, Archive, ClaimId};
use serde_json::Value;

use crate::rewrite::{Rewrite, Segment, Words};

/// The prefix of every extractor's source.
const EXTRACTOR: &str = "extractor:";

/// What the fix removes besides rewriting the segments.
#[derive(Debug, Default)]
pub struct Removed {
    /// The claims to remove, counted by source.
    pub sources: BTreeMap<String, usize>,
    /// Retractions by other sources that name only removed claims.
    pub retractions: usize,
    /// Files in `derived/` that a removed claim was about and no kept
    /// claim names.
    pub derived: Vec<Digest>,
    /// Files in `derived/` that a removed claim was about and a kept
    /// claim names. They stay; the size and type the extractor recorded
    /// are removed with its claims.
    pub kept: usize,
}

impl Removed {
    /// One line per kind of thing to remove. The lines are printed
    /// before anything is written and under --dry-run, so none of them
    /// states a removal as done.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self
            .sources
            .iter()
            .map(|(source, count)| format!("{source}: {count} claim(s) to remove"))
            .collect();
        if self.retractions > 0 {
            lines.push(format!(
                "{} retraction(s) of these claims by other sources to remove",
                self.retractions
            ));
        }
        if !self.derived.is_empty() {
            lines.push(format!(
                "{} file(s) in derived/ that no other claim names to remove",
                self.derived.len()
            ));
        }
        if self.kept > 0 {
            lines.push(format!(
                "{} file(s) in derived/ that other claims name stay",
                self.kept
            ));
        }
        lines
    }

    /// Remove the files from `derived/`. Called before the segments are
    /// rewritten: a run interrupted after this step still has the claims
    /// about the files, so the next run plans the same removal and
    /// finishes it.
    ///
    /// # Errors
    ///
    /// Whatever the store can answer.
    pub fn apply(&self, archive: &Archive) -> Result<()> {
        let derived = archive.derived();
        for digest in &self.derived {
            derived
                .remove(digest)
                .with_context(|| format!("derived/ file {digest}: removing"))?;
        }
        Ok(())
    }
}

/// The rewrite that removes the claims of extractors, and what goes with
/// them. `only` limits it to the sources named: a source with its
/// generation (`extractor:mail/1`) or without (`extractor:mail`, every
/// generation). Empty means every extractor.
///
/// # Errors
///
/// A name in `only` that is not an extractor's source; an archive in the
/// form of ossuary 0.14 or earlier; a claim line that is not JSON; and
/// whatever [`Rewrite::plan`] can answer.
pub fn plan(archive: &Archive, only: &[String]) -> Result<(Rewrite, Removed)> {
    for name in only {
        if !name.starts_with(EXTRACTOR) || name.len() == EXTRACTOR.len() {
            bail!(
                "{name}: not the source of an extractor, which begins with `extractor:`; `ossuary-fix --dry-run unextract` lists the sources in the archive"
            );
        }
    }
    let words = if only.is_empty() {
        Words {
            done: "without the claims of extractors",
            nothing: "the archive has no claims of extractors",
        }
    } else {
        Words {
            done: "without the claims of the named extractors",
            nothing: "the archive has no claims of the named extractors",
        }
    };
    let algorithm = archive.log().store().algorithm();
    let mut removed = Removed::default();
    let mut subjects = BTreeSet::new();
    let mut named = BTreeSet::new();
    let rewrite = Rewrite::plan(archive, words, |segments, head| {
        if segments
            .values()
            .chain(head.iter())
            .any(|segment| segment.listed || segment.mend.is_some())
        {
            bail!(
                "the archive is in the form of ossuary 0.14 or earlier; run `ossuary-fix list` first, nothing was written"
            );
        }
        let going = going(segments, head.as_ref(), only, algorithm, &mut removed)?;
        if removed.sources.is_empty() {
            return Ok(());
        }
        named = strip(segments, head, only, &going, &mut removed);
        subjects = going.subjects;
        relink(segments, head);
        Ok(())
    })?;
    let derived = archive.derived();
    for name in &subjects {
        // A subject that is not the name of a file is in neither store.
        let Ok(digest) = Digest::parse(name) else {
            continue;
        };
        if !derived.contains(&digest)? {
            continue;
        }
        if named.contains(name) {
            removed.kept += 1;
        } else {
            removed.derived.push(digest);
        }
    }
    Ok((rewrite, removed))
}

/// What the lines that go by their source take with them.
struct Going {
    /// The ids of the assertions among them.
    claims: BTreeSet<ClaimId>,
    /// The subjects they are about.
    subjects: BTreeSet<String>,
}

/// Read every line that goes by its source, and count it under its
/// source in `removed`.
fn going(
    segments: &BTreeMap<String, Segment>,
    head: Option<&Segment>,
    only: &[String],
    algorithm: Algorithm,
    removed: &mut Removed,
) -> Result<Going> {
    let mut going = Going {
        claims: BTreeSet::new(),
        subjects: BTreeSet::new(),
    };
    let sealed = segments
        .iter()
        .map(|(name, segment)| (format!("segment {name}"), segment));
    let open = head
        .into_iter()
        .map(|segment| ("the open segment".to_string(), segment));
    for (name, segment) in sealed.chain(open) {
        for line in &segment.lines {
            let claim = read(line).with_context(|| name.clone())?;
            if !selected(&claim, only) {
                continue;
            }
            *removed
                .sources
                .entry(text(&claim, "source").to_string())
                .or_default() += 1;
            if claim.get("retracts").is_none() {
                going.claims.insert(ClaimId::of(line, algorithm));
            }
            going.subjects.insert(text(&claim, "subject").to_string());
        }
    }
    Ok(going)
}

/// Take the lines out: those that go by their source, and the
/// retractions by other sources that name only claims that go. A sealed
/// segment left without lines is marked as dropped. Answers the subjects
/// of removed claims that a kept line names.
fn strip(
    segments: &mut BTreeMap<String, Segment>,
    head: &mut Option<Segment>,
    only: &[String],
    going: &Going,
    removed: &mut Removed,
) -> BTreeSet<String> {
    let mut kept: BTreeSet<String> = BTreeSet::new();
    let mut keep = |line: &String| -> bool {
        // Every line was read once in `going`.
        let Ok(claim) = read(line) else {
            return true;
        };
        if selected(&claim, only) {
            return false;
        }
        if let Some(Value::Array(targets)) = claim.get("retracts")
            && targets.iter().all(|target| {
                target
                    .as_str()
                    .and_then(|id| ClaimId::parse(id).ok())
                    .is_some_and(|id| going.claims.contains(&id))
            })
        {
            removed.retractions += 1;
            return false;
        }
        for word in names(&claim) {
            if going.subjects.contains(word) {
                kept.insert(word.to_string());
            }
        }
        true
    };
    for segment in segments.values_mut() {
        let before = segment.lines.len();
        segment.lines.retain(&mut keep);
        segment.dropped = before > 0 && segment.lines.is_empty();
    }
    if let Some(head) = head {
        head.lines.retain(&mut keep);
    }
    kept
}

/// A claim line as a JSON object.
fn read(line: &str) -> Result<Value> {
    let claim: Value =
        serde_json::from_str(line).with_context(|| format!("claim is not JSON: {line}"))?;
    if !claim.is_object() {
        bail!("claim is not a JSON object: {line}");
    }
    Ok(claim)
}

/// A string member of a claim, empty when it is missing.
fn text<'a>(claim: &'a Value, member: &str) -> &'a str {
    claim
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// Whether the claim is one to remove: written by an extractor, and by
/// one of those named when any are.
fn selected(claim: &Value, only: &[String]) -> bool {
    let source = text(claim, "source");
    source.starts_with(EXTRACTOR)
        && (only.is_empty()
            || only.iter().any(|name| {
                source
                    .strip_prefix(name.as_str())
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
            }))
}

/// The files a claim names: its subject, and its value when that is a
/// string or a list of strings, which is how a link to a file is
/// recorded.
fn names(claim: &Value) -> impl Iterator<Item = &str> {
    let subject = claim.get("subject").and_then(Value::as_str);
    let values: Vec<&str> = match claim.get("value") {
        Some(Value::String(value)) => vec![value.as_str()],
        Some(Value::Array(values)) => values.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    subject.into_iter().chain(values)
}

/// Each kept segment, and the open one, made to name the nearest kept
/// segment before it.
fn relink(segments: &mut BTreeMap<String, Segment>, head: &mut Option<Segment>) {
    let before = |segments: &BTreeMap<String, Segment>, segment: &Segment| -> Vec<String> {
        let mut previous = segment.previous.first().cloned();
        while let Some(name) = &previous {
            match segments.get(name) {
                Some(named) if named.dropped => previous = named.previous.first().cloned(),
                _ => break,
            }
        }
        previous.into_iter().collect()
    };
    let relinked: Vec<(String, Vec<String>)> = segments
        .iter()
        .filter(|(_, segment)| !segment.dropped)
        .map(|(name, segment)| (name.clone(), before(segments, segment)))
        .collect();
    if let Some(head) = head {
        head.previous = before(segments, head);
    }
    for (name, previous) in relinked {
        if let Some(segment) = segments.get_mut(&name) {
            segment.previous = previous;
        }
    }
}

#[cfg(test)]
mod tests {
    use ossuary_core::{Attribute, Claim, Run, Source, Subject, Timestamp};
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::fixes::testing::{archive, sound};

    const RUN: &str = "315e360b-020e-48be-8f2d-f2002a2ea9b4";
    const TIME: &str = "2026-11-11T10:00:00Z";

    /// A file in `content/`, or in `derived/`.
    fn file(archive: &Archive, bytes: &str, derived: bool) -> Subject {
        let store = if derived {
            archive.derived()
        } else {
            archive.content()
        };
        let (_, entry) = store.add(bytes.as_bytes()).unwrap();
        Subject::parse(entry.digest().as_str()).unwrap()
    }

    fn assert(subject: &Subject, attribute: &str, value: Value, source: &str) -> Claim {
        Claim::assert(
            subject.clone(),
            Attribute::parse(attribute).unwrap(),
            value,
            Timestamp::parse(TIME).unwrap(),
            Source::parse(source).unwrap(),
            Run::parse(RUN).unwrap(),
        )
        .unwrap()
    }

    fn retract(archive: &Archive, claims: &[&Claim], source: &str) -> Claim {
        let algorithm = archive.log().store().algorithm();
        Claim::retract(
            claims[0].subject().clone(),
            claims[0].attribute().clone(),
            claims
                .iter()
                .map(|claim| ClaimId::of(&claim.to_line(), algorithm))
                .collect(),
            Timestamp::parse(TIME).unwrap(),
            Source::parse(source).unwrap(),
            Run::parse(RUN).unwrap(),
        )
        .unwrap()
    }

    /// Every claim line of the archive, sorted: a seal orders the lines
    /// of a segment itself, so the order of an append is not kept.
    fn lines(archive: &Archive) -> Vec<String> {
        let log = archive.log();
        let mut lines = Vec::new();
        for segment in log.segments().unwrap() {
            lines.extend(log.read(segment.digest()).unwrap());
        }
        lines.extend(log.head().unwrap());
        let mut lines: Vec<String> = lines.iter().map(Claim::to_line).collect();
        lines.sort();
        lines
    }

    fn sorted<const N: usize>(lines: [String; N]) -> Vec<String> {
        let mut lines = Vec::from(lines);
        lines.sort();
        lines
    }

    fn fix(archive: &Archive, only: &[&str]) -> (Rewrite, Removed) {
        let only: Vec<String> = only.iter().map(ToString::to_string).collect();
        let (rewrite, removed) = plan(archive, &only).unwrap();
        removed.apply(archive).unwrap();
        rewrite.apply(archive).unwrap();
        (rewrite, removed)
    }

    #[test]
    fn extractor_claims_go_from_sealed_segments_and_the_open_one() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let log = archive.log();
        let mail = file(&archive, "a mail", false);
        let size = assert(&mail, "file:size", json!(6), "ingest");
        let date = assert(
            &mail,
            "mail:date",
            json!("Thu, 27 Jun 2019"),
            "extractor:mail/1",
        );
        let tag = assert(&mail, "user:tag", json!("kept"), "user");
        let receipt = assert(
            &mail,
            "prov:examined",
            json!("extractor:mail/1"),
            "extractor:mail/1",
        );
        log.append_all(&[size.clone(), date]).unwrap();
        log.seal().unwrap();
        log.append_all(&[tag.clone(), receipt]).unwrap();

        let (rewrite, removed) = fix(&archive, &[]);

        assert_eq!(lines(&archive), sorted([size.to_line(), tag.to_line()]));
        assert_eq!(removed.sources["extractor:mail/1"], 2);
        assert!(rewrite.changes());
        assert!(sound(&archive));
        let (again, nothing) = plan(&archive, &[]).unwrap();
        assert!(!again.changes(), "a second run writes nothing");
        assert!(nothing.sources.is_empty());
    }

    #[test]
    fn a_segment_left_without_claims_is_removed_and_the_chain_closed() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let log = archive.log();
        let mail = file(&archive, "a mail", false);
        let size = assert(&mail, "file:size", json!(6), "ingest");
        let tag = assert(&mail, "user:tag", json!("kept"), "user");
        log.append(&size).unwrap();
        log.seal().unwrap();
        log.append(&assert(
            &mail,
            "mail:subject",
            json!("Hello"),
            "extractor:mail/1",
        ))
        .unwrap();
        log.seal().unwrap();
        log.append(&tag).unwrap();
        log.seal().unwrap();

        fix(&archive, &[]);

        assert_eq!(log.segments().unwrap().len(), 2);
        assert_eq!(lines(&archive), sorted([size.to_line(), tag.to_line()]));
        assert!(sound(&archive));
    }

    #[test]
    fn a_retraction_goes_only_when_all_it_names_is_gone() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let log = archive.log();
        let mail = file(&archive, "a mail", false);
        let detected = assert(&mail, "file:mime", json!("text/plain"), "ingest");
        let same = assert(&mail, "file:mime", json!("text/plain"), "extractor:mail/1");
        let subject = assert(&mail, "mail:subject", json!("Hello"), "extractor:mail/1");
        let both = retract(&archive, &[&detected, &same], "user");
        let only_extracted = retract(&archive, &[&subject], "user");
        log.append_all(&[
            detected.clone(),
            same,
            subject,
            both.clone(),
            only_extracted,
        ])
        .unwrap();
        log.seal().unwrap();

        let (_, removed) = fix(&archive, &[]);

        assert_eq!(
            lines(&archive),
            sorted([detected.to_line(), both.to_line()])
        );
        assert_eq!(removed.retractions, 1);
        assert!(sound(&archive));
    }

    #[test]
    fn a_derived_file_goes_unless_another_claim_names_it() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let log = archive.log();
        let mail = file(&archive, "a mail", false);
        let plain = file(&archive, "a body", true);
        let tagged = file(&archive, "an attachment", true);
        let linked = file(&archive, "another attachment", true);
        let origin = |part: &Subject| {
            assert(
                part,
                "prov:origin",
                json!(mail.as_str()),
                "extractor:mail/1",
            )
        };
        log.append_all(&[
            assert(&mail, "file:size", json!(6), "ingest"),
            origin(&plain),
            origin(&tagged),
            origin(&linked),
            assert(&tagged, "user:tag", json!("kept"), "user"),
            assert(&mail, "user:see", json!(linked.as_str()), "user"),
        ])
        .unwrap();
        log.seal().unwrap();

        let (_, removed) = fix(&archive, &[]);

        let held = |subject: &Subject| {
            archive
                .derived()
                .contains(&Digest::parse(subject.as_str()).unwrap())
                .unwrap()
        };
        assert_eq!(removed.derived.len(), 1);
        assert_eq!(removed.kept, 2);
        assert!(!held(&plain));
        assert!(held(&tagged), "a claim of the user is about it");
        assert!(held(&linked), "a claim of the user names it as its value");
        assert!(
            archive
                .content()
                .contains(&Digest::parse(mail.as_str()).unwrap())
                .unwrap(),
            "content/ is not touched"
        );
    }

    #[test]
    fn named_sources_limit_the_fix() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let log = archive.log();
        let mail = file(&archive, "a mail", false);
        let old = assert(&mail, "mail:subject", json!("Hello"), "extractor:mail/1");
        let new = assert(&mail, "mail:subject", json!("Hello"), "extractor:mail/2");
        let other = assert(&mail, "mail:x", json!("y"), "extractor:mailbox/1");
        let pdf = assert(&mail, "pdf:title", json!("T"), "extractor:pdf-text/1");
        log.append_all(&[old.clone(), new, other.clone(), pdf.clone()])
            .unwrap();

        fix(&archive, &["extractor:mail/2"]);
        assert_eq!(
            lines(&archive),
            sorted([old.to_line(), other.to_line(), pdf.to_line()])
        );

        fix(&archive, &["extractor:mail"]);
        assert_eq!(
            lines(&archive),
            sorted([other.to_line(), pdf.to_line()]),
            "a name without generation is every generation, and no other extractor"
        );
    }

    #[test]
    fn a_name_that_is_no_extractor_is_refused() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        for name in ["ingest", "user", "extractor:", "mail"] {
            let error = plan(&archive, &[name.to_string()]).unwrap_err();
            assert!(
                error.to_string().contains("not the source of an extractor"),
                "{name}: {error}"
            );
        }
    }
}
