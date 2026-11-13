//! Files removed from the archive, with or without a replacement: the
//! claims about them leave the log and their bytes leave `content/`.
//!
//! What is removed with a file:
//!
//! - every line about it, retractions included;
//! - every `prov:origin` that names it;
//! - for a file extracted from it, when every file it was extracted from
//!   is removed and `content/` does not hold it too: the claims of
//!   extractors about it, and the file in `derived/` unless a kept claim
//!   names it. This is the rule of `unextract`, applied to these files
//!   only.
//!
//! A replacement gets the standing claims about the replaced file that
//! do not describe its bytes: everything except its size, its type and
//! the claims of extractors. Each is written again with the time, source
//! and run of the old claim, as `origin` and `packed` restate theirs. The
//! id of a claim is the hash of its line, so a line with another subject
//! is another claim; the old line is removed and the new one is added to
//! the open segment, both in the same rewrite.
//!
//! The steps are ordered so that a run that stops can be finished by the
//! same call. The extracted files are removed before the rewrite: once
//! the claims are gone, nothing in the log tells which files were
//! extracted from the removed ones. The files in `content/` are removed
//! after it, because the caller names them again.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use anyhow::{Context as _, Result, bail};
use immure::Digest;
use ossuary_core::{Algorithm, Archive, Attribute, Claim, ClaimId, Run, Subject};
use serde_json::Value;

use crate::fixes::lines::{EXTRACTOR, names, read, relink, text};
use crate::record;
use crate::rewrite::{Rewrite, Segment, Words};

/// The attribute that links an extracted file to the file it came from.
const ORIGIN: &str = "prov:origin";

/// The attributes that describe a file's bytes, whoever recorded them.
const OF_THE_BYTES: [&str; 2] = ["file:size", "file:mime"];

/// One file to remove, and the file that replaces it, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pair {
    pub old: Digest,
    pub new: Option<Digest>,
}

/// What the fix does besides rewriting the segments.
#[derive(Debug, Default)]
pub struct Taken {
    /// Whether the files are replaced, which only changes the wording.
    replacing: bool,
    /// The claims written again for a replacement, counted by attribute.
    pub moved: BTreeMap<String, usize>,
    /// Standing claims that a replacement already has.
    pub standing: usize,
    /// The lines about the named files.
    pub claims: usize,
    /// The lines about files extracted from them.
    pub extracted: usize,
    /// The named files that `content/` holds.
    pub content: Vec<Digest>,
    /// Files to remove from `derived/`: those extracted from the named
    /// files only, and a named file that `derived/` holds as well.
    pub derived: Vec<Digest>,
    /// Extracted files that stay in `derived/` because a kept claim
    /// names them.
    pub kept: usize,
    /// Named files the archive has neither bytes nor claims of.
    pub absent: usize,
}

impl Taken {
    /// One line per kind of thing the fix does. The lines are printed
    /// before anything is written and under --dry-run, so none of them
    /// states a change as done.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let (other, files) = if self.replacing {
            ("other ", "the replaced file(s)")
        } else {
            ("", "the file(s)")
        };
        let mut lines: Vec<String> = self
            .moved
            .iter()
            .map(|(attribute, count)| {
                format!("{attribute}: {count} claim(s) to move to the replacement(s)")
            })
            .collect();
        if self.standing > 0 {
            lines.push(format!(
                "{} claim(s) not moved, the replacement has the value already",
                self.standing
            ));
        }
        // The lines about the named files that are only removed.
        let moved: usize = self.moved.values().sum();
        let rest = self.claims.saturating_sub(moved + self.standing);
        if rest > 0 {
            lines.push(format!("{rest} {other}claim(s) about {files} to remove"));
        }
        if self.extracted > 0 {
            lines.push(format!(
                "{} claim(s) about files extracted from them to remove",
                self.extracted
            ));
        }
        if !self.content.is_empty() {
            lines.push(format!(
                "{} file(s) to remove from content/",
                self.content.len()
            ));
        }
        if !self.derived.is_empty() {
            lines.push(format!(
                "{} file(s) to remove from derived/",
                self.derived.len()
            ));
        }
        if self.kept > 0 {
            lines.push(format!(
                "{} file(s) extracted from them stay in derived/, other claims name them",
                self.kept
            ));
        }
        if self.absent > 0 {
            lines.push(format!(
                "{} of the named file(s) not in the archive, nothing to remove for them",
                self.absent
            ));
        }
        lines
    }

    /// Sort the files into those to remove from each store, those that
    /// stay, and those the archive does not have: `extracted` are the
    /// files extracted from the named ones only, `named` those of them
    /// that a kept claim names, `found` the named files with claims.
    fn files_of(
        &mut self,
        archive: &Archive,
        pairs: &[Pair],
        extracted: &BTreeSet<String>,
        named: &BTreeSet<String>,
        found: &BTreeSet<String>,
    ) -> Result<()> {
        let content = archive.content();
        let derived = archive.derived();
        for name in extracted {
            // A subject that is not the name of a file is in neither store.
            let Ok(digest) = Digest::parse(name) else {
                continue;
            };
            if !derived.contains(&digest)? {
                continue;
            }
            if named.contains(name) {
                self.kept += 1;
            } else {
                self.derived.push(digest);
            }
        }
        for pair in pairs {
            let in_content = content.contains(&pair.old)?;
            let in_derived = derived.contains(&pair.old)?;
            if in_content {
                self.content.push(pair.old.clone());
            }
            if in_derived {
                self.derived.push(pair.old.clone());
            }
            if !in_content && !in_derived && !found.contains(pair.old.as_str()) {
                self.absent += 1;
            }
        }
        Ok(())
    }

    /// How many files the fix removes from the two stores.
    #[must_use]
    pub fn files(&self) -> usize {
        self.content.len() + self.derived.len()
    }

    /// Remove the files from `derived/`. Called before the segments are
    /// rewritten: a run interrupted after this step still has the claims
    /// about the files, so the next run plans the same removal and
    /// finishes it.
    ///
    /// # Errors
    ///
    /// Whatever the store can answer.
    pub fn remove_derived(&self, archive: &Archive) -> Result<()> {
        let derived = archive.derived();
        for digest in &self.derived {
            derived
                .remove(digest)
                .with_context(|| format!("derived/ file {digest}: removing"))?;
        }
        Ok(())
    }

    /// Remove the named files from `content/`. Called after the segments
    /// are rewritten: a run interrupted before this step leaves files
    /// without claims, and the same call removes them.
    ///
    /// # Errors
    ///
    /// Whatever the store can answer.
    pub fn remove_content(&self, archive: &Archive) -> Result<()> {
        let content = archive.content();
        for digest in &self.content {
            content
                .remove(digest)
                .with_context(|| format!("content/ file {digest}: removing"))?;
        }
        Ok(())
    }
}

/// The names in a list file: one file to remove per line, or with
/// `replacing` the file to replace and its replacement, separated by
/// spaces or a tab. Empty lines are skipped.
///
/// # Errors
///
/// A line with another number of words.
pub fn listed(list: &str, replacing: bool) -> Result<Vec<(String, Option<String>)>> {
    let mut given = Vec::new();
    for (index, line) in list.lines().enumerate() {
        let words: Vec<&str> = line.split_whitespace().collect();
        match (words.as_slice(), replacing) {
            ([], _) => {}
            ([name], false) => given.push(((*name).to_string(), None)),
            ([old, new], true) => given.push(((*old).to_string(), Some((*new).to_string()))),
            (words, false) => bail!(
                "line {}: {} names, expected the name of one file",
                index + 1,
                words.len()
            ),
            (words, true) => bail!(
                "line {}: {} name(s), expected the file to replace and its replacement",
                index + 1,
                words.len()
            ),
        }
    }
    Ok(given)
}

/// The names given, checked against each other and against the stores.
///
/// # Errors
///
/// A name that is not a full name in this archive; a file named as its
/// own replacement, with two replacements, or as a replacement and as a
/// file to remove; a replacement that `content/` does not hold; and a
/// file to remove that only `derived/` holds.
pub fn pairs(archive: &Archive, given: &[(String, Option<String>)]) -> Result<Vec<Pair>> {
    let length = archive.content().algorithm().hash(b"").len();
    let name = |given: &str| -> Result<Digest> {
        match Digest::parse(given) {
            Ok(digest) if digest.len() == length => Ok(digest),
            _ => bail!(
                "{given}: not the name of a file in this archive, which is {length} hex digits; `ossuary find --id` prints the names"
            ),
        }
    };
    let mut replacements: BTreeMap<Digest, Option<Digest>> = BTreeMap::new();
    let mut pairs = Vec::new();
    for (old, new) in given {
        let old = name(old)?;
        let new = new.as_deref().map(name).transpose()?;
        if new.as_ref() == Some(&old) {
            bail!("{old}: named as its own replacement");
        }
        match replacements.get(&old) {
            Some(known) if *known == new => continue,
            Some(_) => bail!("{old}: named with two replacements"),
            None => {}
        }
        replacements.insert(old.clone(), new.clone());
        pairs.push(Pair { old, new });
    }
    let content = archive.content();
    let derived = archive.derived();
    for pair in &pairs {
        if let Some(new) = &pair.new {
            if replacements.contains_key(new) {
                bail!("{new}: named as a replacement and as a file to replace");
            }
            if !content.contains(new)? {
                bail!(
                    "{new}: the replacement is not in content/; add it to the archive first, with `ossuary ingest` or `ossuary mailvault add`"
                );
            }
        }
        if !content.contains(&pair.old)? && derived.contains(&pair.old)? {
            bail!(
                "{}: an extracted file in derived/; name the file it was extracted from",
                pair.old
            );
        }
    }
    Ok(pairs)
}

/// The rewrite that removes the files of `pairs` from the log, and what
/// is done besides. With `same`, a file and its replacement must have
/// the same standing values of that attribute.
///
/// # Errors
///
/// A pair that fails `same`; a replacement while the open segment is
/// missing; an archive in the form of ossuary 0.14 or earlier; a claim
/// line that is not JSON; and whatever reading the log, the stores and
/// [`Rewrite::plan`] can answer.
pub fn plan(
    archive: &Archive,
    pairs: &[Pair],
    same: Option<&Attribute>,
) -> Result<(Rewrite, Taken)> {
    let replacing = pairs.iter().any(|pair| pair.new.is_some());
    let mut taken = Taken {
        replacing,
        ..Taken::default()
    };
    let restated = if replacing {
        let claims = record::whole(archive.log())?;
        let standing = standing(&claims, pairs);
        if let Some(attribute) = same {
            compare(pairs, &standing, attribute)?;
        }
        restate(pairs, &standing, &mut taken)?
    } else {
        Vec::new()
    };

    let gone: BTreeSet<String> = pairs
        .iter()
        .map(|pair| pair.old.as_str().to_string())
        .collect();
    let algorithm = archive.log().store().algorithm();
    let content = archive.content();
    let mut extracted = BTreeSet::new();
    let mut named = BTreeSet::new();
    let mut found = BTreeSet::new();
    let rewrite = Rewrite::plan(
        archive,
        Words {
            done: if replacing {
                "with the replacements in place of the replaced files"
            } else {
                "without the claims about the removed files"
            },
            nothing: "the log has no claims about these files",
        },
        |segments, head| {
            if segments
                .values()
                .chain(head.iter())
                .any(|segment| segment.listed || segment.mend.is_some())
            {
                bail!(
                    "the archive is in the form of ossuary 0.14 or earlier; run `ossuary-fix list` first, nothing was written"
                );
            }
            let origins = origins(segments, head.as_ref())?;
            extracted = only_from(&origins, &gone, |name| {
                // A subject that is not the name of a file is in neither
                // store.
                let Ok(digest) = Digest::parse(name) else {
                    return Ok(false);
                };
                Ok(content.contains(&digest)?)
            })?;
            let leaving = Leaving {
                gone: &gone,
                extracted: &extracted,
            };
            let ids = going(
                segments,
                head.as_ref(),
                &leaving,
                algorithm,
                &mut taken,
                &mut found,
            )?;
            named = strip(segments, head, &leaving, &ids, &mut taken);
            if !restated.is_empty() {
                let Some(head) = head else {
                    bail!(
                        "the open segment is missing; run `ossuary-fix chain` first, nothing was written"
                    );
                };
                head.lines.extend(restated.iter().map(Claim::to_line));
            }
            relink(segments, head);
            Ok(())
        },
    )?;

    taken.files_of(archive, pairs, &extracted, &named, &found)?;
    Ok((rewrite, taken))
}

/// The standing assertions about each file of `pairs`, in the order of
/// the log.
fn standing<'a>(claims: &'a [Claim], pairs: &[Pair]) -> BTreeMap<&'a str, Vec<&'a Claim>> {
    let files: BTreeSet<&str> = pairs
        .iter()
        .flat_map(|pair| std::iter::once(&pair.old).chain(&pair.new))
        .map(Digest::as_str)
        .collect();
    let about = |claim: &&Claim| files.contains(claim.subject().as_str());
    // A retraction retracts only claims of its own subject and attribute.
    let retracted: HashSet<(&str, &str, &str)> = claims
        .iter()
        .filter(about)
        .flat_map(|claim| {
            claim.retracts().iter().map(|id| {
                (
                    claim.subject().as_str(),
                    claim.attribute().as_str(),
                    id.as_str(),
                )
            })
        })
        .collect();
    let mut standing: BTreeMap<&str, Vec<&Claim>> = BTreeMap::new();
    for claim in claims.iter().filter(about) {
        if claim.value().is_none() {
            continue;
        }
        let subject = claim.subject().as_str();
        if claim.id().is_some_and(|id| {
            retracted.contains(&(subject, claim.attribute().as_str(), id.as_str()))
        }) {
            continue;
        }
        standing.entry(subject).or_default().push(claim);
    }
    standing
}

/// Refuse the pairs whose two files do not have the same standing values
/// of `attribute`, or have none.
fn compare(
    pairs: &[Pair],
    standing: &BTreeMap<&str, Vec<&Claim>>,
    attribute: &Attribute,
) -> Result<()> {
    let values = |file: &Digest| -> BTreeSet<String> {
        standing
            .get(file.as_str())
            .into_iter()
            .flatten()
            .filter(|claim| claim.attribute() == attribute)
            .filter_map(|claim| claim.value())
            .map(Value::to_string)
            .collect()
    };
    let mut refused = 0;
    let mut first = None;
    for pair in pairs {
        let Some(new) = &pair.new else {
            continue;
        };
        // A file without claims has nothing to move: an earlier run
        // replaced it already.
        if !standing.contains_key(pair.old.as_str()) {
            continue;
        }
        let (old_values, new_values) = (values(&pair.old), values(new));
        let why = if old_values.is_empty() {
            format!("{} has none on record", pair.old)
        } else if new_values.is_empty() {
            format!("{new} has none on record")
        } else if old_values != new_values {
            "the values differ".to_string()
        } else {
            continue;
        };
        refused += 1;
        first.get_or_insert_with(|| format!("{} and {new}: {why}", pair.old));
    }
    if let Some(first) = first {
        bail!(
            "{refused} pair(s) do not have the same {attribute}, the first is {first}; nothing was written"
        );
    }
    Ok(())
}

/// The claims to write for the replacements: each standing claim about a
/// replaced file that does not describe its bytes, unless the
/// replacement already has that value.
fn restate(
    pairs: &[Pair],
    standing: &BTreeMap<&str, Vec<&Claim>>,
    taken: &mut Taken,
) -> Result<Vec<Claim>> {
    // What each replacement has, as (file, attribute, value).
    let mut has: BTreeSet<(String, String, String)> = BTreeSet::new();
    for new in pairs.iter().filter_map(|pair| pair.new.as_ref()) {
        for claim in standing.get(new.as_str()).into_iter().flatten() {
            if let Some(value) = claim.value() {
                has.insert((
                    new.as_str().to_string(),
                    claim.attribute().as_str().to_string(),
                    value.to_string(),
                ));
            }
        }
    }
    // One new run for every claim from before runs were recorded: the
    // old claim has none to copy, and a claim needs one.
    let fresh = Run::new();
    let mut restated = Vec::new();
    for pair in pairs {
        let Some(new) = &pair.new else {
            continue;
        };
        let subject = Subject::parse(new.as_str())?;
        for claim in standing.get(pair.old.as_str()).into_iter().flatten() {
            let attribute = claim.attribute().as_str();
            let Some(value) = claim.value() else {
                continue;
            };
            if OF_THE_BYTES.contains(&attribute) || claim.source().as_str().starts_with(EXTRACTOR) {
                continue;
            }
            let key = (
                new.as_str().to_string(),
                attribute.to_string(),
                value.to_string(),
            );
            if !has.insert(key) {
                taken.standing += 1;
                continue;
            }
            *taken.moved.entry(attribute.to_string()).or_default() += 1;
            restated.push(Claim::assert(
                subject.clone(),
                claim.attribute().clone(),
                value.clone(),
                claim.time().clone(),
                claim.source().clone(),
                claim.run().cloned().unwrap_or_else(|| fresh.clone()),
            )?);
        }
    }
    Ok(restated)
}

/// Every line of every segment and the open one, read as JSON, with the
/// name of the segment for an error.
fn every_line<'a>(
    segments: &'a BTreeMap<String, Segment>,
    head: Option<&'a Segment>,
) -> impl Iterator<Item = Result<(&'a String, Value)>> {
    let sealed = segments
        .iter()
        .map(|(name, segment)| (format!("segment {name}"), segment));
    let open = head
        .into_iter()
        .map(|segment| ("the open segment".to_string(), segment));
    sealed.chain(open).flat_map(|(name, segment)| {
        segment.lines.iter().map(move |line| {
            let claim = read(line).with_context(|| name.clone())?;
            Ok((line, claim))
        })
    })
}

/// For each extracted file, the files its `prov:origin` claims name.
fn origins(
    segments: &BTreeMap<String, Segment>,
    head: Option<&Segment>,
) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let mut origins: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for line in every_line(segments, head) {
        let (_, claim) = line?;
        if text(&claim, "attribute") != ORIGIN {
            continue;
        }
        if let Some(origin) = claim.get("value").and_then(Value::as_str) {
            origins
                .entry(text(&claim, "subject").to_string())
                .or_default()
                .insert(origin.to_string());
        }
    }
    Ok(origins)
}

/// The files extracted from the removed ones and from nothing else, at
/// any depth, that `content/` does not hold as well.
fn only_from(
    origins: &BTreeMap<String, BTreeSet<String>>,
    gone: &BTreeSet<String>,
    in_content: impl Fn(&str) -> Result<bool>,
) -> Result<BTreeSet<String>> {
    let mut extracted: BTreeSet<String> = BTreeSet::new();
    loop {
        let before = extracted.len();
        for (file, from) in origins {
            if gone.contains(file) || extracted.contains(file) {
                continue;
            }
            if from
                .iter()
                .all(|origin| gone.contains(origin) || extracted.contains(origin))
                && !in_content(file)?
            {
                extracted.insert(file.clone());
            }
        }
        if extracted.len() == before {
            return Ok(extracted);
        }
    }
}

/// The files whose lines are removed.
struct Leaving<'a> {
    /// The files named by the caller.
    gone: &'a BTreeSet<String>,
    /// The files extracted from them only.
    extracted: &'a BTreeSet<String>,
}

/// Why a line is removed.
enum Why {
    /// It is about a named file.
    Named,
    /// It links a file to a removed one, or is an extractor's claim about
    /// a file extracted from the named ones only.
    Extracted,
}

impl Leaving<'_> {
    fn why(&self, claim: &Value) -> Option<Why> {
        let subject = text(claim, "subject");
        if self.gone.contains(subject) {
            return Some(Why::Named);
        }
        let links_removed = text(claim, "attribute") == ORIGIN
            && claim
                .get("value")
                .and_then(Value::as_str)
                .is_some_and(|origin| {
                    self.gone.contains(origin) || self.extracted.contains(origin)
                });
        let of_extractor =
            self.extracted.contains(subject) && text(claim, "source").starts_with(EXTRACTOR);
        (links_removed || of_extractor).then_some(Why::Extracted)
    }
}

/// Count the lines that are removed, note which named files have lines,
/// and answer the ids of the removed assertions about other files: a
/// retraction that names only those is removed with them.
fn going(
    segments: &BTreeMap<String, Segment>,
    head: Option<&Segment>,
    leaving: &Leaving<'_>,
    algorithm: Algorithm,
    taken: &mut Taken,
    found: &mut BTreeSet<String>,
) -> Result<BTreeSet<ClaimId>> {
    let mut ids = BTreeSet::new();
    for line in every_line(segments, head) {
        let (line, claim) = line?;
        match leaving.why(&claim) {
            Some(Why::Named) => {
                taken.claims += 1;
                found.insert(text(&claim, "subject").to_string());
            }
            Some(Why::Extracted) => {
                taken.extracted += 1;
                if claim.get("retracts").is_none() {
                    ids.insert(ClaimId::of(line, algorithm));
                }
            }
            None => {}
        }
    }
    Ok(ids)
}

/// Remove the lines. A sealed segment left without lines is marked as
/// dropped. Answers the extracted files that a kept line names.
fn strip(
    segments: &mut BTreeMap<String, Segment>,
    head: &mut Option<Segment>,
    leaving: &Leaving<'_>,
    ids: &BTreeSet<ClaimId>,
    taken: &mut Taken,
) -> BTreeSet<String> {
    let mut named: BTreeSet<String> = BTreeSet::new();
    let mut keep = |line: &String| -> bool {
        // Every line was read once in `going`.
        let Ok(claim) = read(line) else {
            return true;
        };
        if leaving.why(&claim).is_some() {
            return false;
        }
        if let Some(Value::Array(targets)) = claim.get("retracts")
            && targets.iter().all(|target| {
                target
                    .as_str()
                    .and_then(|id| ClaimId::parse(id).ok())
                    .is_some_and(|id| ids.contains(&id))
            })
        {
            taken.extracted += 1;
            return false;
        }
        for word in names(&claim) {
            if leaving.extracted.contains(word) {
                named.insert(word.to_string());
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
    named
}

#[cfg(test)]
mod tests {
    use ossuary_core::{Source, Timestamp};
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

    fn retract(archive: &Archive, claim: &Claim, source: &str) -> Claim {
        let algorithm = archive.log().store().algorithm();
        Claim::retract(
            claim.subject().clone(),
            claim.attribute().clone(),
            vec![ClaimId::of(&claim.to_line(), algorithm)],
            Timestamp::parse("2026-11-12T10:00:00Z").unwrap(),
            Source::parse(source).unwrap(),
            Run::parse(RUN).unwrap(),
        )
        .unwrap()
    }

    fn origin(part: &Subject, from: &Subject) -> Claim {
        assert(
            part,
            "prov:origin",
            json!(from.as_str()),
            "extractor:mail-attachments/1",
        )
    }

    /// Every claim line of the archive, sorted: a seal orders the lines
    /// of a segment itself, so the order of an append is not kept.
    fn lines(archive: &Archive) -> Vec<String> {
        let mut lines: Vec<String> = record::whole(archive.log())
            .unwrap()
            .iter()
            .map(Claim::to_line)
            .collect();
        lines.sort();
        lines
    }

    fn sorted(claims: &[&Claim]) -> Vec<String> {
        let mut lines: Vec<String> = claims.iter().map(|claim| claim.to_line()).collect();
        lines.sort();
        lines
    }

    fn pair(old: &Subject, new: Option<&Subject>) -> (String, Option<String>) {
        (
            old.as_str().to_string(),
            new.map(|new| new.as_str().to_string()),
        )
    }

    /// Plan and write the fix, in the order the command does.
    fn fix(
        archive: &Archive,
        given: &[(String, Option<String>)],
        same: Option<&str>,
    ) -> (Rewrite, Taken) {
        let same = same.map(|name| Attribute::parse(name).unwrap());
        let pairs = pairs(archive, given).unwrap();
        let (rewrite, taken) = plan(archive, &pairs, same.as_ref()).unwrap();
        taken.remove_derived(archive).unwrap();
        rewrite.apply(archive).unwrap();
        taken.remove_content(archive).unwrap();
        (rewrite, taken)
    }

    fn holds(store: &ossuary_core::Store, subject: &Subject) -> bool {
        store
            .contains(&Digest::parse(subject.as_str()).unwrap())
            .unwrap()
    }

    #[test]
    fn a_removed_file_leaves_no_claim_and_no_bytes() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let log = archive.log();
        let mail = file(&archive, "a mail", false);
        let other = file(&archive, "another mail", false);
        let tag = assert(&mail, "user:tag", json!("odd"), "user");
        let kept = assert(&other, "file:size", json!(12), "mailvault");
        log.append_all(&[
            assert(&mail, "file:size", json!(6), "mailvault"),
            assert(
                &mail,
                "mailbox:place",
                json!("example.org:INBOX"),
                "mailvault",
            ),
            kept.clone(),
            tag.clone(),
        ])
        .unwrap();
        log.seal().unwrap();
        log.append_all(&[
            retract(&archive, &tag, "user"),
            assert(
                &mail,
                "mail:subject",
                json!("Hello"),
                "extractor:mail-headers/1",
            ),
        ])
        .unwrap();

        let (rewrite, taken) = fix(&archive, &[pair(&mail, None)], None);

        assert_eq!(lines(&archive), sorted(&[&kept]));
        assert_eq!(taken.claims, 5);
        assert_eq!(taken.content.len(), 1);
        assert!(rewrite.changes());
        assert!(!holds(archive.content(), &mail));
        assert!(holds(archive.content(), &other));
        assert!(sound(&archive));

        let pairs = pairs(&archive, &[pair(&mail, None)]).unwrap();
        let (again, nothing) = plan(&archive, &pairs, None).unwrap();
        assert!(!again.changes(), "a second run writes nothing");
        assert_eq!(nothing.absent, 1);
        assert_eq!(nothing.files(), 0);
    }

    #[test]
    fn an_extracted_file_goes_unless_something_else_holds_it() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let log = archive.log();
        let mail = file(&archive, "a mail", false);
        let stays = file(&archive, "a mail that stays", false);
        let only = file(&archive, "extracted from the mail only", true);
        let nested = file(&archive, "extracted from that file", true);
        let shared = file(&archive, "extracted from both mails", true);
        let tagged = file(&archive, "extracted and tagged", true);
        let both_stores = file(&archive, "extracted and ingested", true);
        file(&archive, "extracted and ingested", false);
        let size =
            |part: &Subject| assert(part, "file:size", json!(1), "extractor:mail-attachments/1");
        let shared_origin = origin(&shared, &stays);
        let tag = assert(&tagged, "user:tag", json!("kept"), "user");
        let ingested = assert(&both_stores, "file:path", json!("/a"), "ingest");
        let kept = [
            assert(&stays, "file:size", json!(17), "mailvault"),
            shared_origin,
            size(&shared),
            tag,
            ingested,
            size(&both_stores),
        ];
        log.append_all(&[
            assert(&mail, "file:size", json!(6), "mailvault"),
            origin(&only, &mail),
            size(&only),
            origin(&nested, &only),
            size(&nested),
            origin(&shared, &mail),
            origin(&tagged, &mail),
            size(&tagged),
            origin(&both_stores, &mail),
        ])
        .unwrap();
        log.append_all(&kept).unwrap();
        log.seal().unwrap();

        let (_, taken) = fix(&archive, &[pair(&mail, None)], None);

        assert_eq!(lines(&archive), sorted(&kept.iter().collect::<Vec<_>>()));
        assert_eq!(taken.claims, 1);
        assert_eq!(taken.extracted, 8);
        assert_eq!(taken.kept, 1, "the tagged file");
        let derived = archive.derived();
        assert!(!holds(derived, &only));
        assert!(!holds(derived, &nested), "extracted from a removed file");
        assert!(holds(derived, &shared), "the other mail still has it");
        assert!(holds(derived, &tagged), "a claim of the user is about it");
        assert!(holds(derived, &both_stores), "content/ holds it as well");
        assert!(sound(&archive));
    }

    #[test]
    fn a_replacement_gets_the_claims_that_are_not_about_the_bytes() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let log = archive.log();
        let broken = file(&archive, "headers only", false);
        let whole = file(&archive, "headers and body", false);
        let inbox = assert(
            &broken,
            "mailbox:place",
            json!("example.org:INBOX"),
            "mailvault",
        );
        let all = assert(
            &broken,
            "mailbox:place",
            json!("example.org:All"),
            "mailvault",
        );
        let tag = assert(&broken, "user:tag", json!("tax"), "user");
        let dropped = assert(&broken, "user:tag", json!("todo"), "user");
        log.append_all(&[
            assert(&broken, "file:size", json!(12), "mailvault"),
            assert(&broken, "file:mime", json!("message/rfc822"), "mailvault"),
            inbox,
            all,
            tag,
            dropped.clone(),
            assert(
                &broken,
                "mail:subject",
                json!("Hi"),
                "extractor:mail-headers/1",
            ),
        ])
        .unwrap();
        log.seal().unwrap();
        let own = [
            assert(&whole, "file:size", json!(16), "mailvault"),
            assert(&whole, "mailbox:place", json!("backup:INBOX"), "mailvault"),
            assert(
                &whole,
                "mailbox:place",
                json!("example.org:All"),
                "mailvault",
            ),
        ];
        log.append_all(&own).unwrap();
        log.append(&retract(&archive, &dropped, "user")).unwrap();

        let (_, taken) = fix(&archive, &[pair(&broken, Some(&whole))], None);

        let mut expected: Vec<&Claim> = own.iter().collect();
        let inbox = assert(
            &whole,
            "mailbox:place",
            json!("example.org:INBOX"),
            "mailvault",
        );
        let tag = assert(&whole, "user:tag", json!("tax"), "user");
        expected.extend([&inbox, &tag]);
        assert_eq!(
            lines(&archive),
            sorted(&expected),
            "with the time, source and run of the old claims"
        );
        assert_eq!(
            taken.moved,
            BTreeMap::from([
                ("mailbox:place".to_string(), 1),
                ("user:tag".to_string(), 1)
            ])
        );
        assert_eq!(taken.standing, 1, "the place the replacement had");
        assert_eq!(taken.claims, 8);
        assert!(!holds(archive.content(), &broken));
        assert!(holds(archive.content(), &whole));
        assert!(sound(&archive));

        let pairs = pairs(&archive, &[pair(&broken, Some(&whole))]).unwrap();
        let (again, nothing) = plan(&archive, &pairs, None).unwrap();
        assert!(!again.changes(), "a second run writes nothing");
        assert!(nothing.moved.is_empty());
    }

    #[test]
    fn two_files_with_one_replacement_give_it_each_value_once() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let log = archive.log();
        let first = file(&archive, "headers only", false);
        let second = file(&archive, "headers only, other line breaks", false);
        let whole = file(&archive, "headers and body", false);
        log.append_all(&[
            assert(
                &first,
                "mailbox:place",
                json!("example.org:INBOX"),
                "mailvault",
            ),
            assert(
                &second,
                "mailbox:place",
                json!("example.org:INBOX"),
                "mailvault",
            ),
            assert(
                &second,
                "mailbox:place",
                json!("example.net:INBOX"),
                "mailvault",
            ),
        ])
        .unwrap();

        let (_, taken) = fix(
            &archive,
            &[pair(&first, Some(&whole)), pair(&second, Some(&whole))],
            None,
        );

        assert_eq!(
            lines(&archive),
            sorted(&[
                &assert(
                    &whole,
                    "mailbox:place",
                    json!("example.org:INBOX"),
                    "mailvault"
                ),
                &assert(
                    &whole,
                    "mailbox:place",
                    json!("example.net:INBOX"),
                    "mailvault"
                ),
            ])
        );
        assert_eq!(taken.standing, 1);
        assert_eq!(taken.content.len(), 2);
    }

    #[test]
    fn same_refuses_a_pair_with_other_values_or_none() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let log = archive.log();
        let broken = file(&archive, "headers only", false);
        let whole = file(&archive, "headers and body", false);
        let other = file(&archive, "another mail", false);
        let id = |mail: &Subject, id: &str| {
            assert(
                mail,
                "mail:message-id",
                json!(id),
                "extractor:mail-headers/1",
            )
        };
        log.append_all(&[
            id(&broken, "1@example.org"),
            id(&whole, "1@example.org"),
            id(&other, "2@example.org"),
        ])
        .unwrap();
        let before = lines(&archive);
        let attribute = Attribute::parse("mail:message-id").unwrap();
        let refused = |old: &Subject, new: &Subject, same: &Attribute| {
            let pairs = pairs(&archive, &[pair(old, Some(new))]).unwrap();
            plan(&archive, &pairs, Some(same)).unwrap_err().to_string()
        };

        let differ = refused(&broken, &other, &attribute);
        assert!(
            differ.contains("1 pair(s) do not have the same mail:message-id")
                && differ.contains("the values differ"),
            "{differ}"
        );
        let none = refused(&broken, &whole, &Attribute::parse("mail:date").unwrap());
        assert!(none.contains("has none on record"), "{none}");
        assert_eq!(lines(&archive), before);

        fix(
            &archive,
            &[pair(&broken, Some(&whole))],
            Some("mail:message-id"),
        );
        assert!(!holds(archive.content(), &broken));

        let pairs = pairs(&archive, &[pair(&broken, Some(&whole))]).unwrap();
        let (again, _) = plan(&archive, &pairs, Some(&attribute)).unwrap();
        assert!(
            !again.changes(),
            "a second run does not compare a file that is gone"
        );
    }

    #[test]
    fn the_lines_count_what_moves_apart_from_what_is_only_removed() {
        let replaced = Taken {
            replacing: true,
            moved: BTreeMap::from([("mailbox:place".to_string(), 2)]),
            standing: 1,
            claims: 10,
            content: vec![Digest::parse(&"a".repeat(64)).unwrap()],
            ..Taken::default()
        };
        assert_eq!(
            replaced.lines(),
            vec![
                "mailbox:place: 2 claim(s) to move to the replacement(s)",
                "1 claim(s) not moved, the replacement has the value already",
                "7 other claim(s) about the replaced file(s) to remove",
                "1 file(s) to remove from content/",
            ]
        );
        let removed = Taken {
            claims: 4,
            extracted: 3,
            kept: 1,
            absent: 2,
            ..Taken::default()
        };
        assert_eq!(
            removed.lines(),
            vec![
                "4 claim(s) about the file(s) to remove",
                "3 claim(s) about files extracted from them to remove",
                "1 file(s) extracted from them stay in derived/, other claims name them",
                "2 of the named file(s) not in the archive, nothing to remove for them",
            ]
        );
    }

    #[test]
    fn the_names_are_checked_before_the_log_is_read() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let a = file(&archive, "a", false);
        let b = file(&archive, "b", false);
        let c = file(&archive, "c", false);
        let extracted = file(&archive, "extracted", true);
        let nowhere = Subject::parse(&"0".repeat(64)).unwrap();
        let refused =
            |given: &[(String, Option<String>)]| pairs(&archive, given).unwrap_err().to_string();

        assert!(refused(&[pair(&a, Some(&a))]).contains("its own replacement"));
        assert!(refused(&[pair(&a, Some(&b)), pair(&a, Some(&c))]).contains("two replacements"));
        assert!(
            refused(&[pair(&a, Some(&b)), pair(&b, Some(&c))])
                .contains("as a replacement and as a file to replace")
        );
        assert!(refused(&[pair(&a, Some(&nowhere))]).contains("not in content/"));
        assert!(refused(&[pair(&extracted, None)]).contains("an extracted file"));
        assert!(
            refused(&[("abc".to_string(), None)]).contains("not the name of a file"),
            "a prefix is not a name"
        );
        assert_eq!(
            pairs(&archive, &[pair(&a, Some(&b)), pair(&a, Some(&b))])
                .unwrap()
                .len(),
            1,
            "a pair named twice is one pair"
        );
    }

    #[test]
    fn a_list_names_one_file_or_one_pair_per_line() {
        assert_eq!(
            listed("aa\n\n  bb  \n", false).unwrap(),
            vec![("aa".to_string(), None), ("bb".to_string(), None)]
        );
        assert_eq!(
            listed("aa\tbb\ncc dd\n", true).unwrap(),
            vec![
                ("aa".to_string(), Some("bb".to_string())),
                ("cc".to_string(), Some("dd".to_string()))
            ]
        );
        let error = listed("aa bb\n", false).unwrap_err().to_string();
        assert!(error.contains("line 1"), "{error}");
        let error = listed("aa bb\ncc\n", true).unwrap_err().to_string();
        assert!(error.contains("line 2"), "{error}");
    }

    #[test]
    fn a_run_stopped_after_the_rewrite_is_finished_by_the_same_call() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let mail = file(&archive, "a mail", false);
        archive
            .log()
            .append(&assert(&mail, "file:size", json!(6), "mailvault"))
            .unwrap();
        let given = [pair(&mail, None)];
        let pairs = pairs(&archive, &given).unwrap();
        let (rewrite, _) = plan(&archive, &pairs, None).unwrap();
        rewrite.apply(&archive).unwrap();

        let (again, taken) = fix(&archive, &given, None);

        assert!(!again.changes());
        assert_eq!(taken.content.len(), 1);
        assert_eq!(taken.absent, 0);
        assert!(!holds(archive.content(), &mail));
    }
}
