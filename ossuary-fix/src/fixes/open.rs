//! An archive of 0.18 or earlier brought into the current form: the open
//! segment moved from `head.jsonl` (0.17 and earlier) or from a directory
//! in the archive root (0.18) into `open/`, and every retraction
//! rewritten to name the claims it retracts by id.
//!
//! Until 0.17 a retraction carried `retract: true` and the value it
//! retracted, or no value for the whole attribute, and meant "what stands
//! at this point of the log". The current form names claims by id, so
//! the fix replays the log in its order and names, for each old
//! retraction, the assertions that stood when it was written. A
//! retraction that found nothing standing is dropped: it said nothing
//! then and cannot be written now. Assertion lines keep their bytes, so
//! their ids are what the current form computes; the segments from the
//! first changed line on get new names.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use immure::{Algorithm, Digest};
use ossuary_core::{Archive, Claim, ClaimId, OLD_HEAD, old_open_dirs};
use serde_json::Value;

use crate::rewrite::{Rewrite, Segment, Words, parse};

/// What the conversion did to the retractions.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Converted {
    /// Retractions rewritten to name claims.
    pub retractions: usize,
    /// Retractions that found nothing standing, left out.
    pub dropped: usize,
}

impl Converted {
    /// The words for what was done, or nothing when no retraction was.
    #[must_use]
    pub fn sentence(&self, dry_run: bool) -> Option<String> {
        if self.retractions == 0 && self.dropped == 0 {
            return None;
        }
        let would = if dry_run { "would be " } else { "" };
        let mut sentence = format!(
            "{} retraction(s) {would}rewritten to name the claims they retract",
            self.retractions
        );
        if self.dropped > 0 {
            write!(
                sentence,
                "; {} retraction(s) without a standing value to retract {would}left out",
                self.dropped
            )
            .expect("writing to a String");
        }
        Some(sentence)
    }
}

/// The archive in the current form, and what the conversion did.
///
/// # Errors
///
/// Whatever [`Rewrite::plan`] can answer; `head.jsonl` or a bundle of
/// 0.18 that does not read; a seal of 0.18 under way or stopped, which
/// a 0.18 run that writes finishes first; an archive in the form of
/// ossuary 0.14 or earlier, which `ossuary-fix list` of 0.17 brings into
/// the chain form first; a sealed segment missing from the chain, which
/// `ossuary-fix chain` of the version that reads the form found closes
/// first; and an old retraction whose line does not read.
pub fn plan(archive: &Archive) -> Result<(Rewrite, Converted)> {
    let old = archive.root().join(OLD_HEAD);
    let head_text = match fs::read_to_string(&old) {
        Ok(text) => Some(text),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => return Err(source).with_context(|| format!("{}: reading", old.display())),
    };
    // A gap in the chain is closed by the chain fix of the version that
    // reads the form found. 0.18 does not open an archive with
    // head.jsonl, and 0.17 finds no head.jsonl in an archive of 0.18 and
    // takes its open segment for lost.
    let (head_text, chain_fix) = match head_text {
        Some(text) => (Some(text), "`ossuary-fix chain` of ossuary 0.17"),
        None => match old_dir_text(archive.root())? {
            Some(text) => (Some(text), "`ossuary-fix chain` of ossuary 0.18"),
            None => (None, "`ossuary-fix chain`"),
        },
    };
    let algorithm = archive.log().store().algorithm();
    let mut converted = Converted::default();
    let rewrite = Rewrite::plan(
        archive,
        Words {
            done: "into the current form",
            nothing: "the archive is in the current form",
        },
        |segments, head| {
            if let Some(text) = &head_text {
                *head = Some(parse(text).with_context(|| format!("{}", old.display()))?);
            }
            convert(segments, head, algorithm, chain_fix, &mut converted)
        },
    )?;
    Ok((rewrite, converted))
}

/// The open segment of 0.18, which lay in the archive root as
/// `open-<S>/` (`open/` before the first seal), as the text of a segment:
/// the header naming S, then the lines of its bundles in the order a
/// seal gives them. `None` when there is no such directory.
fn old_dir_text(root: &Path) -> Result<Option<String>> {
    let dirs = old_open_dirs(root);
    let [dir] = dirs.as_slice() else {
        if dirs.is_empty() {
            return Ok(None);
        }
        bail!(
            "the archive root has {} open-segment directories of ossuary 0.18 from an unfinished seal; finish it with any command of ossuary 0.18 that writes, then run this fix again; nothing was written",
            dirs.len()
        );
    };
    let name = dir.file_name().unwrap_or_default().to_string_lossy();
    if !(name == "open" || name.starts_with("open-")) {
        bail!(
            "{}: unfinished seal of ossuary 0.18; finish it with any command of ossuary 0.18 that writes, then run this fix again; nothing was written",
            dir.display()
        );
    }
    let previous = match name.strip_prefix("open-") {
        Some(hex) if hex != "first" => Some(
            Digest::parse(hex).with_context(|| format!("{}: names no segment", dir.display()))?,
        ),
        _ => None,
    };
    let mut lines = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("{}: listing", dir.display()))? {
        let entry = entry.with_context(|| format!("{}: listing", dir.display()))?;
        if !entry.file_name().to_string_lossy().ends_with(".jsonl") {
            continue;
        }
        let text = fs::read_to_string(entry.path())
            .with_context(|| format!("{}: reading", entry.path().display()))?;
        for line in text.lines().filter(|line| !line.is_empty()) {
            let claim = Claim::parse_line(line).with_context(|| {
                format!("{}: a line is not a valid claim", entry.path().display())
            })?;
            lines.push((claim.time().as_str().to_string(), line.to_string()));
        }
    }
    lines.sort();
    let mut text = match &previous {
        None => "{\"ossuary-segment\":1}\n".to_string(),
        Some(previous) => format!("{{\"ossuary-segment\":1,\"previous\":\"{previous}\"}}\n"),
    };
    for (_, line) in lines {
        text.push_str(&line);
        text.push('\n');
    }
    Ok(Some(text))
}

/// Every old retraction in `segments` and `head` rewritten, in the order
/// of the log. `chain_fix` is the command that closes a gap in the chain
/// of this form.
fn convert(
    segments: &mut BTreeMap<String, Segment>,
    head: &mut Option<Segment>,
    algorithm: Algorithm,
    chain_fix: &str,
    converted: &mut Converted,
) -> Result<()> {
    if segments
        .values()
        .chain(head.iter())
        .any(|segment| segment.listed || segment.mend.is_some())
    {
        bail!(
            "the archive is in the form of ossuary 0.14 or earlier; run `ossuary-fix list` of ossuary 0.17 first, nothing was written"
        );
    }
    // The chain, oldest first, from what the open segment names.
    let mut order = Vec::new();
    let mut next = head
        .as_ref()
        .and_then(|segment| segment.previous.first().cloned());
    while let Some(name) = next {
        if order.contains(&name) {
            bail!("the chain loops back to segment {name}; nothing was written");
        }
        let Some(segment) = segments.get(&name) else {
            bail!(
                "segment {name} is missing from claims/; close the chain with {chain_fix} first, nothing was written"
            );
        };
        next = segment.previous.first().cloned();
        order.push(name);
    }
    order.reverse();
    let mut standing = Standing::default();
    for name in &order {
        let segment = segments.get_mut(name).expect("named in the chain");
        convert_lines(&mut segment.lines, &mut standing, algorithm, converted)?;
    }
    if let Some(segment) = head {
        convert_lines(&mut segment.lines, &mut standing, algorithm, converted)?;
    }
    Ok(())
}

/// The assertions standing at a point of the replay, by subject,
/// attribute and value, each with its id.
#[derive(Default)]
struct Standing(HashMap<(String, String, String), Vec<ClaimId>>);

impl Standing {
    fn put(&mut self, subject: &str, attribute: &str, value: &Value, id: ClaimId) {
        self.0
            .entry((
                subject.to_string(),
                attribute.to_string(),
                value.to_string(),
            ))
            .or_default()
            .push(id);
    }

    /// The ids standing for the value, or for every value of the
    /// attribute; they stand no more.
    fn take(&mut self, subject: &str, attribute: &str, value: Option<&Value>) -> Vec<ClaimId> {
        let mut taken = Vec::new();
        if let Some(value) = value {
            let key = (
                subject.to_string(),
                attribute.to_string(),
                value.to_string(),
            );
            if let Some(ids) = self.0.remove(&key) {
                taken.extend(ids);
            }
        } else {
            let keys: Vec<_> = self
                .0
                .keys()
                .filter(|(s, a, _)| s == subject && a == attribute)
                .cloned()
                .collect();
            for key in keys {
                if let Some(ids) = self.0.remove(&key) {
                    taken.extend(ids);
                }
            }
        }
        taken.sort();
        taken.dedup();
        taken
    }
}

/// The lines of one segment replayed: assertions remembered, old
/// retractions rewritten or dropped. A line in the current form is
/// replayed as it is.
fn convert_lines(
    lines: &mut Vec<String>,
    standing: &mut Standing,
    algorithm: Algorithm,
    converted: &mut Converted,
) -> Result<()> {
    let mut kept = Vec::with_capacity(lines.len());
    for line in lines.drain(..) {
        let Value::Object(fields) =
            serde_json::from_str::<Value>(&line).with_context(|| format!("not a claim: {line}"))?
        else {
            bail!("not a claim: {line}");
        };
        let subject = string_field(&fields, "subject", &line)?;
        let attribute = string_field(&fields, "attribute", &line)?;
        if fields.get("retract") != Some(&Value::Bool(true)) {
            if let Some(targets) = fields.get("retracts").and_then(Value::as_array) {
                // Already in the current form.
                for target in targets {
                    let Some(target) = target.as_str().and_then(|t| ClaimId::parse(t).ok()) else {
                        bail!("a retraction names something that is not a claim id: {line}");
                    };
                    for ids in standing.0.values_mut() {
                        ids.retain(|id| id != &target);
                    }
                }
            } else if let Some(value) = fields.get("value") {
                standing.put(subject, attribute, value, ClaimId::of(&line, algorithm));
            }
            kept.push(line);
            continue;
        }
        let ids = standing.take(subject, attribute, fields.get("value"));
        if ids.is_empty() {
            converted.dropped += 1;
            continue;
        }
        let time = string_field(&fields, "time", &line)?;
        let source = string_field(&fields, "source", &line)?;
        let run = fields.get("run").and_then(Value::as_str);
        let mut rewritten = format!(
            "{{\"subject\":{},\"attribute\":{},\"retracts\":{},\"time\":{},\"source\":{}",
            serde_json::to_string(subject)?,
            serde_json::to_string(attribute)?,
            serde_json::to_string(&ids)?,
            serde_json::to_string(time)?,
            serde_json::to_string(source)?,
        );
        if let Some(run) = run {
            write!(rewritten, ",\"run\":{}", serde_json::to_string(run)?)
                .expect("writing to a String");
        }
        rewritten.push('}');
        // The bytes must be what the current form writes, so that the
        // line's id is what a reader computes.
        let read_back = Claim::parse_line(&rewritten).with_context(|| {
            format!("the rewritten retraction is not a valid claim: {rewritten}")
        })?;
        if read_back.to_line() != rewritten {
            bail!("internal error: the rewritten retraction is not in canonical form: {rewritten}");
        }
        converted.retractions += 1;
        kept.push(rewritten);
    }
    *lines = kept;
    Ok(())
}

fn string_field<'a>(
    fields: &'a serde_json::Map<String, Value>,
    name: &str,
    line: &str,
) -> Result<&'a str> {
    fields
        .get(name)
        .and_then(Value::as_str)
        .with_context(|| format!("a claim without a {name}: {line}"))
}

#[cfg(test)]
mod tests {
    use ossuary_core::{Attribute, Scope, Subject};
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::fixes::testing::{archive, lose, sound};

    const SUBJECT: &str = "9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e9f2ac41e";

    fn assertion(value: &str, time: &str) -> String {
        format!(
            r#"{{"subject":"{SUBJECT}","attribute":"user:tag","value":"{value}","time":"{time}","source":"user","run":"315e360b-020e-48be-8f2d-f2002a2ea9b4"}}"#
        )
    }

    fn old_retraction(value: Option<&str>, time: &str) -> String {
        match value {
            Some(value) => format!(
                r#"{{"subject":"{SUBJECT}","attribute":"user:tag","value":"{value}","time":"{time}","source":"user","run":"315e360b-020e-48be-8f2d-f2002a2ea9b4","retract":true}}"#
            ),
            None => format!(
                r#"{{"subject":"{SUBJECT}","attribute":"user:tag","time":"{time}","source":"user","run":"315e360b-020e-48be-8f2d-f2002a2ea9b4","retract":true}}"#
            ),
        }
    }

    /// An archive in the form of 0.18: the open segment moved from
    /// `open/<name>` into the root as 0.18 named it.
    fn as_of_0_18(archive: &Archive) {
        let root = archive.root();
        let dirs: Vec<_> = fs::read_dir(root.join("open"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(dirs.len(), 1);
        let name = dirs[0].file_name().unwrap().to_string_lossy().into_owned();
        let old = if name == "first" {
            "open".to_string()
        } else {
            format!("open-{name}")
        };
        fs::rename(&dirs[0], root.join("open.moving")).unwrap();
        fs::remove_dir(root.join("open")).unwrap();
        fs::rename(root.join("open.moving"), root.join(old)).unwrap();
    }

    /// An archive in the form before 0.18: sealed segments as given, the
    /// open segment as `head.jsonl` with `lines`.
    fn old_archive(dir: &TempDir, segments: &[&[String]], head: &[String]) -> Archive {
        let archive = archive(dir);
        archive.log().discard_open().unwrap();
        fs::remove_dir(archive.root().join("open")).unwrap();
        let mut previous: Option<String> = None;
        for lines in segments {
            let mut text = match &previous {
                None => "{\"ossuary-segment\":1}\n".to_string(),
                Some(name) => format!("{{\"ossuary-segment\":1,\"previous\":\"{name}\"}}\n"),
            };
            for line in *lines {
                text.push_str(line);
                text.push('\n');
            }
            let (_, entry) = archive.log().store().add(text.as_bytes()).unwrap();
            previous = Some(entry.digest().to_string());
        }
        let mut text = match &previous {
            None => "{\"ossuary-segment\":1}\n".to_string(),
            Some(name) => format!("{{\"ossuary-segment\":1,\"previous\":\"{name}\"}}\n"),
        };
        for line in head {
            text.push_str(line);
            text.push('\n');
        }
        fs::write(archive.root().join(OLD_HEAD), text).unwrap();
        archive
    }

    fn standing_tags(archive: &Archive) -> Vec<Value> {
        let mut index = archive.index().unwrap();
        index.fold(archive.log()).unwrap();
        index
            .values(
                &Subject::parse(SUBJECT).unwrap(),
                &Attribute::parse("user:tag").unwrap(),
                Scope::Held,
            )
            .unwrap()
    }

    #[test]
    fn an_open_segment_of_0_18_moves_into_open_before_the_first_seal() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let log = archive.log();
        log.append(&Claim::parse_line(&assertion("holiday", "2026-09-01T10:00:01Z")).unwrap())
            .unwrap();
        log.append(&Claim::parse_line(&assertion("beach", "2026-09-01T10:00:00Z")).unwrap())
            .unwrap();
        as_of_0_18(&archive);
        assert!(archive.root().join("open").join("tmp").is_dir());
        assert!(
            Archive::open(archive.root()).is_err(),
            "0.18's layout is refused"
        );

        let (rewrite, converted) = plan(&archive).unwrap();
        assert_eq!(converted, Converted::default());
        assert!(rewrite.changes());
        assert!(rewrite.sentence(true).contains("the open segment"));
        rewrite.apply(&archive).unwrap();

        assert!(old_open_dirs(archive.root()).is_empty());
        let archive = Archive::open(archive.root()).unwrap();
        assert!(sound(&archive));
        assert!(
            archive
                .root()
                .join("open")
                .join("first")
                .join("tmp")
                .is_dir()
        );
        let claims = archive.log().head().unwrap();
        assert_eq!(claims.len(), 2);
        assert_eq!(claims[0].value().unwrap(), "beach", "sorted by time");
        assert_eq!(archive.log().open().unwrap().unwrap().bundles().len(), 1);
    }

    #[test]
    fn an_open_segment_of_0_18_moves_into_open_after_a_seal() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let log = archive.log();
        log.append(&Claim::parse_line(&assertion("holiday", "2026-09-01T10:00:00Z")).unwrap())
            .unwrap();
        let sealed = log.seal().unwrap().unwrap();
        log.append(&Claim::parse_line(&assertion("beach", "2026-09-02T10:00:00Z")).unwrap())
            .unwrap();
        as_of_0_18(&archive);
        assert!(
            archive
                .root()
                .join(format!("open-{}", sealed.digest()))
                .is_dir()
        );

        let (rewrite, _) = plan(&archive).unwrap();
        rewrite.apply(&archive).unwrap();

        assert!(old_open_dirs(archive.root()).is_empty());
        let archive = Archive::open(archive.root()).unwrap();
        assert!(sound(&archive));
        let open = archive.log().open().unwrap().unwrap();
        assert_eq!(open.previous(), Some(sealed.digest()));
        assert_eq!(archive.log().head().unwrap().len(), 1);
        assert_eq!(archive.log().segments().unwrap().len(), 1);
    }

    #[test]
    fn a_seal_of_0_18_under_way_is_refused() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        archive
            .log()
            .append(&Claim::parse_line(&assertion("holiday", "2026-09-01T10:00:00Z")).unwrap())
            .unwrap();
        as_of_0_18(&archive);
        fs::rename(archive.root().join("open"), archive.root().join("sealing")).unwrap();

        let error = plan(&archive).unwrap_err();

        assert!(error.to_string().contains("0.18"), "{error}");
        assert!(
            archive.root().join("sealing").is_dir(),
            "nothing was written"
        );
    }

    #[test]
    fn a_gap_in_an_archive_of_0_17_needs_the_chain_fix_of_0_17() {
        let dir = TempDir::new().unwrap();
        let first = assertion("holiday", "2026-09-01T10:00:00Z");
        let archive = old_archive(
            &dir,
            &[
                std::slice::from_ref(&first),
                &[assertion("beach", "2026-09-02T10:00:00Z")],
            ],
            &[assertion("sea", "2026-09-03T10:00:00Z")],
        );
        let lost = archive
            .log()
            .store()
            .digest(format!("{{\"ossuary-segment\":1}}\n{first}\n").as_bytes());
        lose(&archive, &lost);

        let error = plan(&archive).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("`ossuary-fix chain` of ossuary 0.17"),
            "{error}"
        );
    }

    #[test]
    fn a_gap_in_an_archive_of_0_18_needs_the_chain_fix_of_0_18() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let log = archive.log();
        log.append(&Claim::parse_line(&assertion("holiday", "2026-09-01T10:00:00Z")).unwrap())
            .unwrap();
        let lost = log.seal().unwrap().unwrap();
        log.append(&Claim::parse_line(&assertion("beach", "2026-09-02T10:00:00Z")).unwrap())
            .unwrap();
        log.seal().unwrap().unwrap();
        log.append(&Claim::parse_line(&assertion("sea", "2026-09-03T10:00:00Z")).unwrap())
            .unwrap();
        as_of_0_18(&archive);
        lose(&archive, lost.digest());

        let error = plan(&archive).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("`ossuary-fix chain` of ossuary 0.18"),
            "{error}"
        );
    }

    #[test]
    fn a_gap_in_the_current_form_needs_the_current_chain_fix() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let log = archive.log();
        log.append(&Claim::parse_line(&assertion("holiday", "2026-09-01T10:00:00Z")).unwrap())
            .unwrap();
        let lost = log.seal().unwrap().unwrap();
        log.append(&Claim::parse_line(&assertion("beach", "2026-09-02T10:00:00Z")).unwrap())
            .unwrap();
        log.seal().unwrap().unwrap();
        lose(&archive, lost.digest());

        let error = plan(&archive).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("close the chain with `ossuary-fix chain` first"),
            "{error}"
        );
    }

    #[test]
    fn the_open_segment_moves_into_its_directory_and_retractions_name_their_claims() {
        let dir = TempDir::new().unwrap();
        let archive = old_archive(
            &dir,
            &[],
            &[
                assertion("holiday", "2026-09-01T10:00:00Z"),
                assertion("beach", "2026-09-01T10:00:01Z"),
                old_retraction(Some("holiday"), "2026-09-02T10:00:00Z"),
            ],
        );
        assert!(
            Archive::open(archive.root()).is_err(),
            "the current build refuses the old form"
        );

        let (rewrite, converted) = plan(&archive).unwrap();
        assert_eq!(
            converted,
            Converted {
                retractions: 1,
                dropped: 0
            }
        );
        assert!(rewrite.changes());
        rewrite.apply(&archive).unwrap();

        assert!(!archive.root().join(OLD_HEAD).exists());
        let archive = Archive::open(archive.root()).unwrap();
        assert!(sound(&archive));
        let claims = archive.log().head().unwrap();
        assert_eq!(claims.len(), 3);
        let retraction = claims.iter().find(|claim| claim.is_retraction()).unwrap();
        let holiday = claims
            .iter()
            .find(|claim| claim.value() == Some(&json!("holiday")))
            .unwrap();
        assert_eq!(retraction.retracts(), &[holiday.id().unwrap().clone()]);
        assert_eq!(standing_tags(&archive), [json!("beach")]);
        let (again, converted) = plan(&archive).unwrap();
        assert!(!again.changes(), "a second run finds the current form");
        assert_eq!(converted, Converted::default());
    }

    #[test]
    fn a_retraction_in_a_later_segment_names_the_claims_of_an_earlier_one() {
        let dir = TempDir::new().unwrap();
        let archive = old_archive(
            &dir,
            &[
                &[
                    assertion("holiday", "2026-09-01T10:00:00Z"),
                    assertion("holiday", "2026-09-01T10:00:00Z"),
                ],
                &[old_retraction(None, "2026-09-02T10:00:00Z")],
                &[assertion("holiday", "2026-09-03T10:00:00Z")],
            ],
            &[],
        );
        let before: Vec<_> = archive
            .log()
            .store()
            .entries()
            .map(|e| e.unwrap().digest().clone())
            .collect();

        let (rewrite, converted) = plan(&archive).unwrap();
        assert_eq!(converted.retractions, 1);
        rewrite.apply(&archive).unwrap();

        let archive = Archive::open(archive.root()).unwrap();
        assert!(sound(&archive));
        let segments = archive.log().segments().unwrap();
        assert_eq!(segments.len(), 3);
        assert_eq!(
            segments[0].digest(),
            &before
                .iter()
                .find(|d| d == &segments[0].digest())
                .cloned()
                .unwrap(),
            "the first segment keeps its name"
        );
        let retraction = &archive.log().read(segments[1].digest()).unwrap()[0];
        assert_eq!(
            retraction.retracts().len(),
            1,
            "the two identical lines are one claim, named once"
        );
        assert_eq!(
            standing_tags(&archive),
            [json!("holiday")],
            "the assertion after the retraction stands"
        );
    }

    #[test]
    fn a_retraction_of_nothing_standing_is_left_out() {
        let dir = TempDir::new().unwrap();
        let archive = old_archive(
            &dir,
            &[],
            &[
                old_retraction(Some("holiday"), "2026-09-02T10:00:00Z"),
                assertion("holiday", "2026-09-03T10:00:00Z"),
            ],
        );

        let (rewrite, converted) = plan(&archive).unwrap();
        assert_eq!(
            converted,
            Converted {
                retractions: 0,
                dropped: 1
            }
        );
        rewrite.apply(&archive).unwrap();

        let archive = Archive::open(archive.root()).unwrap();
        assert_eq!(archive.log().head().unwrap().len(), 1);
        assert_eq!(standing_tags(&archive), [json!("holiday")]);
    }

    #[test]
    fn an_archive_in_the_current_form_has_nothing_to_do() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        archive
            .log()
            .append(&Claim::parse_line(&assertion("holiday", "2026-09-01T10:00:00Z")).unwrap())
            .unwrap();

        let (rewrite, converted) = plan(&archive).unwrap();

        assert!(!rewrite.changes());
        assert_eq!(converted, Converted::default());
        assert_eq!(
            rewrite.sentence(false),
            "nothing to do: the archive is in the current form"
        );
    }
}
