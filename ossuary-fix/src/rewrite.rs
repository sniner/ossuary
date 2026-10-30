//! Rewriting sealed segments: what `ossuary` never does, and the one
//! mechanism shared by every fix that has to.
//!
//! A segment is named by its bytes, and every later segment names it in
//! its header, so a change to one segment renames it and every segment
//! after it, up to the open head. The rewriter does the whole of that.
//! It reads every segment and the head, hands them all to the fix to
//! change, and works out the new names in dependency order: a segment
//! is renamed only after the segment it names has been. A segment whose
//! new bytes are its old bytes keeps its name. A fix may drop segments,
//! and may begin an open head that was lost. The result is one plan,
//! applied in four steps: the query index is removed, the new entries
//! are added, the head is replaced, and the old entries are removed, those
//! that name others first. An interrupted run leaves old segments that
//! still name old segments and new ones that name new ones, and a second
//! run finishes the job, because the same bytes get the same name.
//!
//! The header is read here as a JSON object, without going through
//! `ossuary_core::Log`, because a fix has to read the forms the current
//! build refuses: `previous` as a list, and the `mend` member of ossuary
//! 0.14 and earlier. It is written back in the current form, `previous`
//! as one name, with every member this build does not know kept as it
//! was.
//!
//! The query index in `cache/` is removed before anything else: it
//! records which segments it has folded by name, and would fold the
//! renamed ones a second time. The rest of the cache is not touched.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result, anyhow, bail};
use immure::Digest;
use ossuary_core::{Archive, GENERATION};
use serde::Serialize;
use serde_json::{Map, Value};

/// One segment as a fix sees it: the header taken apart, the claim
/// lines verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Segment {
    /// The segments named as sealed before this one: one name in the
    /// current form, several where ossuary 0.14 or earlier merged lines
    /// or named a mend. The rewriter writes one name at most, and
    /// replaces it with the new name of the segment it names.
    pub previous: Vec<String>,
    /// Whether the header names its predecessors as a list, the form of
    /// ossuary 0.14 and earlier.
    pub listed: bool,
    /// The `mend` member of ossuary 0.14 and earlier, when the header
    /// carries one. It is never written back.
    pub mend: Option<Value>,
    /// Whether the segment is to go: removed from the store, and named
    /// by no segment that stays.
    pub dropped: bool,
    /// Header members this build does not know, kept as they were.
    pub extra: Map<String, Value>,
    /// The claim lines after the header, without their line breaks.
    pub lines: Vec<String>,
}

/// The words a fix gives the sentence.
#[derive(Debug, Clone, Copy)]
pub struct Words {
    /// What is done to the segments, after "rewritten": `into one
    /// chain`.
    pub done: &'static str,
    /// What to say when no segment changes.
    pub nothing: &'static str,
}

/// A rewrite worked out and not yet written.
#[derive(Debug)]
pub struct Rewrite {
    /// The segments that change, in dependency order: a segment comes
    /// after the segment it names.
    steps: Vec<Step>,
    /// The segments that go, by name.
    dropped: Vec<Digest>,
    /// The new text of the open head, when it changes or is begun anew.
    head: Option<String>,
    /// Sealed segments that do not change.
    unchanged: usize,
    words: Words,
}

#[derive(Debug)]
struct Step {
    old: Digest,
    new: Digest,
    text: String,
    /// The `time` of the segment's first claim, as written.
    first: Option<String>,
}

/// The header as it is written: the known members first, in the order
/// `ossuary_core` writes them, then everything else.
#[derive(Serialize)]
struct Header<'a> {
    #[serde(rename = "ossuary-segment")]
    generation: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous: Option<&'a str>,
    #[serde(flatten)]
    extra: &'a Map<String, Value>,
}

impl Rewrite {
    /// Read every sealed segment and the open head, let `edit` change
    /// them, and work out what has to be written.
    ///
    /// `edit` gets the sealed segments by name and the open head, `None`
    /// when it is missing or empty; it may set the head to begin one.
    /// Nothing is written. A segment whose bytes do not match its name
    /// is refused before anything is read further: a rewrite would give
    /// the damaged bytes a true name. A header this build cannot read is
    /// refused as well.
    ///
    /// # Errors
    ///
    /// Whatever reading the store and the head can answer, the refusals
    /// above, whatever `edit` answers, and an edit that leaves a segment
    /// naming several segments, or one that is dropped.
    pub fn plan(
        archive: &Archive,
        words: Words,
        edit: impl FnOnce(&mut BTreeMap<String, Segment>, &mut Option<Segment>) -> Result<()>,
    ) -> Result<Rewrite> {
        let log = archive.log();
        let store = log.store();
        let mut texts: BTreeMap<String, (Digest, String)> = BTreeMap::new();
        let mut segments: BTreeMap<String, Segment> = BTreeMap::new();
        for entry in store.entries() {
            let entry = entry?;
            let digest = entry.digest().clone();
            if !store.verify(&entry)? {
                bail!(
                    "segment {digest} is damaged; restore it from a copy of the archive before running this fix"
                );
            }
            let bytes = store
                .read(&digest)?
                .ok_or_else(|| anyhow!("segment {digest} disappeared while reading"))?;
            let text = String::from_utf8(bytes)
                .map_err(|_| anyhow!("segment {digest} is not UTF-8 text"))?;
            let segment = parse(&text).with_context(|| format!("segment {digest}"))?;
            let name = digest.as_str().to_string();
            segments.insert(name.clone(), segment);
            texts.insert(name, (digest, text));
        }
        let old_head = match fs::read_to_string(log.head_path()) {
            Ok(text) if text.is_empty() => None,
            Ok(text) => Some(text),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(source).context(format!("{}: reading", log.head_path().display()));
            }
        };
        let mut head = old_head
            .as_deref()
            .map(|text| parse(text).context("the open segment"))
            .transpose()?;
        edit(&mut segments, &mut head)?;

        // New names in dependency order. A name that no entry answers
        // to is kept as it is.
        let mut names: BTreeMap<&str, String> = BTreeMap::new();
        let mut remaining: BTreeSet<&str> = segments
            .iter()
            .filter(|(_, segment)| !segment.dropped)
            .map(|(name, _)| name.as_str())
            .collect();
        let mut steps = Vec::new();
        let mut unchanged = 0;
        while !remaining.is_empty() {
            let ready: Vec<&str> = remaining
                .iter()
                .copied()
                .filter(|name| {
                    segments[*name].previous.iter().all(|named| {
                        !segments.contains_key(named) || names.contains_key(named.as_str())
                    })
                })
                .collect();
            if ready.is_empty() {
                // Names are checksums of the bytes that carry them, so
                // no segment can name one that names it back; a segment
                // that names one the edit dropped waits for ever.
                bail!(
                    "{} segment(s) name a segment that is dropped, or each other in a circle; nothing was written",
                    remaining.len()
                );
            }
            for name in ready {
                remaining.remove(name);
                let (old, old_text) = &texts[name];
                let segment = &segments[name];
                let text = render(segment, &names).with_context(|| format!("segment {name}"))?;
                if text == *old_text {
                    names.insert(name, name.to_string());
                    unchanged += 1;
                    continue;
                }
                let new = store.digest(text.as_bytes());
                names.insert(name, new.as_str().to_string());
                steps.push(Step {
                    old: old.clone(),
                    new,
                    text,
                    first: first_time(segment),
                });
            }
        }
        let head = match &head {
            Some(segment) => {
                let text = render(segment, &names).context("the open segment")?;
                (old_head.as_deref() != Some(text.as_str())).then_some(text)
            }
            None => None,
        };
        let dropped = segments
            .iter()
            .filter(|(_, segment)| segment.dropped)
            .map(|(name, _)| texts[name].0.clone())
            .collect();
        Ok(Rewrite {
            steps,
            dropped,
            head,
            unchanged,
            words,
        })
    }

    /// Whether anything would be written.
    #[must_use]
    pub fn changes(&self) -> bool {
        !self.steps.is_empty() || !self.dropped.is_empty() || self.head.is_some()
    }

    /// Write the plan: the caches that know segments by name removed,
    /// new entries added, then the head, then the old entries removed,
    /// those that name others first.
    ///
    /// # Errors
    ///
    /// Whatever the store and the file system can answer. The order
    /// of steps is such that a run interrupted anywhere is finished by
    /// planning and applying again.
    pub fn apply(&self, archive: &Archive) -> Result<()> {
        if !self.changes() {
            return Ok(());
        }
        // First: deleting a cache is always allowed, and an index left
        // standing over renamed segments would fold them a second time.
        forget(archive)?;
        let log = archive.log();
        let store = log.store();
        for step in &self.steps {
            let (_, entry) = store.add(step.text.as_bytes())?;
            if entry.digest() != &step.new {
                bail!(
                    "internal error: segment {} was stored as {}; the old segments were not removed",
                    step.new,
                    entry.digest()
                );
            }
        }
        if let Some(text) = &self.head {
            replace_head(log.head_path(), text)?;
        }
        for step in self.steps.iter().rev() {
            store.remove(&step.old)?;
        }
        for digest in &self.dropped {
            store.remove(digest)?;
        }
        Ok(())
    }

    /// What the rewrite does to seals given out for the archive, when
    /// it renames or removes sealed segments: a seal is a segment's name,
    /// and every one renamed stops checking. The segments renamed are
    /// the first one changed and every one after it, so the line names
    /// the time of the earliest first claim among them.
    #[must_use]
    pub fn seals(&self, dry_run: bool) -> Option<String> {
        if self.steps.is_empty() && self.dropped.is_empty() {
            return None;
        }
        let since = self
            .steps
            .iter()
            .filter_map(|step| step.first.as_deref())
            .min();
        let (which, checks) = match since {
            Some(time) if self.unchanged > 0 => {
                (format!("seal ids given out since {time}"), "check")
            }
            _ => ("every seal id given out so far".to_string(), "checks"),
        };
        if dry_run {
            Some(format!("{which} would no longer check"))
        } else {
            Some(format!(
                "{which} no longer {checks}; run `ossuary seal` and keep the new id in their place"
            ))
        }
    }

    /// The one line the fix answers with.
    #[must_use]
    pub fn sentence(&self, dry_run: bool) -> String {
        if !self.changes() {
            return format!("nothing to do: {}", self.words.nothing);
        }
        let count = self.steps.len();
        let head = match (count, self.head.is_some()) {
            (0, true) => "the open segment",
            (_, true) => ", and the open segment",
            (_, false) => "",
        };
        let mut sentence = match (dry_run, count, head.is_empty()) {
            (true, 0, true) => "no segment would be rewritten".to_string(),
            (false, 0, true) => "no segment rewritten".to_string(),
            (true, 0, false) => format!("{head} would be rewritten {}", self.words.done),
            (false, 0, false) => format!("{head} rewritten {}", self.words.done),
            (true, _, _) => format!(
                "{count} segment(s){head} would be rewritten {}",
                self.words.done
            ),
            (false, _, _) => format!(
                "{count} segment(s){head} rewritten {}; the query index in cache/ was removed and is rebuilt by the next command",
                self.words.done
            ),
        };
        // Writing to a String does not fail.
        if !self.dropped.is_empty() {
            let verb = if dry_run {
                "would be removed"
            } else {
                "removed"
            };
            let _ = write!(
                sentence,
                "; {} segment(s) without claims {verb}",
                self.dropped.len()
            );
        }
        if self.unchanged > 0 {
            let _ = write!(sentence, "; {} unchanged", self.unchanged);
        }
        sentence
    }
}

/// The `time` of a segment's first claim, read from the line as it
/// stands; `None` for a segment without claims, or a line that does not
/// say.
pub fn first_time(segment: &Segment) -> Option<String> {
    let line: Value = serde_json::from_str(segment.lines.first()?).ok()?;
    line.get("time")?.as_str().map(str::to_string)
}

/// A segment's text with the name it refers to replaced by its new one:
/// the header line, then the claim lines, each ended by a line break.
fn render(segment: &Segment, names: &BTreeMap<&str, String>) -> Result<String> {
    let previous = match segment.previous.as_slice() {
        [] => None,
        [name] => Some(
            names
                .get(name.as_str())
                .map_or(name.as_str(), String::as_str),
        ),
        several => bail!(
            "names {} segments as sealed before it; the fix leaves one at most",
            several.len()
        ),
    };
    let header = Header {
        generation: GENERATION,
        previous,
        extra: &segment.extra,
    };
    // A header of known members and a map of JSON values serialises.
    let mut text = serde_json::to_string(&header).expect("a header serialises");
    text.push('\n');
    for line in &segment.lines {
        text.push_str(line);
        text.push('\n');
    }
    Ok(text)
}

/// A segment's text taken apart: the header's known members, the
/// members this build does not know, and the claim lines as they are.
/// `previous` is read as a string or as the list of ossuary 0.14 and
/// earlier; nothing else about the header is relaxed.
fn parse(text: &str) -> Result<Segment> {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or_default();
    let Value::Object(mut header) = serde_json::from_str::<Value>(first)
        .with_context(|| format!("header is not JSON: {first}"))?
    else {
        bail!("header is not a JSON object: {first}");
    };
    match header.remove("ossuary-segment") {
        Some(Value::Number(generation)) if generation.as_u64() == Some(u64::from(GENERATION)) => {}
        Some(Value::Number(generation)) => {
            bail!("segment generation {generation} is not one this build can rewrite")
        }
        _ => bail!("header does not name a segment generation: {first}"),
    }
    let listed = matches!(header.get("previous"), Some(Value::Array(_)));
    let previous = match header.remove("previous") {
        None => Vec::new(),
        Some(Value::String(name)) => vec![name],
        Some(Value::Array(names)) => names
            .into_iter()
            .map(|name| match name {
                Value::String(name) => Ok(name),
                other => Err(anyhow!("previous names a non-string: {other}")),
            })
            .collect::<Result<Vec<_>>>()?,
        Some(other) => bail!("previous is neither a string nor a list: {other}"),
    };
    let mend = header.remove("mend");
    Ok(Segment {
        previous,
        listed,
        mend,
        dropped: false,
        extra: header,
        lines: lines.map(str::to_string).collect(),
    })
}

/// The head replaced by rename, so it is never half a file.
fn replace_head(head: &Path, text: &str) -> Result<()> {
    let mut name = head.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let tmp = head.with_file_name(name);
    fs::write(&tmp, text).with_context(|| format!("{}: writing", tmp.display()))?;
    fs::rename(&tmp, head).with_context(|| format!("{}: replacing", head.display()))
}

/// Drop what the cache knows of segments by name: the query index.
fn forget(archive: &Archive) -> Result<()> {
    let cache = archive.root().join("cache");
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let path = cache.join(format!("index.sqlite{suffix}"));
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(source).with_context(|| format!("{}: removing", path.display()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ossuary_core::{Algorithm, Attribute, Claim, Run, Source, Subject, Timestamp, audit_log};
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    fn archive(dir: &TempDir) -> Archive {
        Archive::create(dir.path().join("archive"), Algorithm::Sha256).unwrap()
    }

    fn claim(archive: &Archive, bytes: &[u8], at: &str) -> Claim {
        let (_, entry) = archive.content().add(bytes).unwrap();
        Claim::assert(
            Subject::parse(entry.digest().as_str()).unwrap(),
            Attribute::parse("file:size").unwrap(),
            json!(bytes.len()),
            Timestamp::parse(at).unwrap(),
            Source::parse("test").unwrap(),
            Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
        )
        .unwrap()
    }

    /// A segment in the form of ossuary 0.14 and earlier: `previous` a
    /// list. Stored straight into the claims store.
    fn old_segment(archive: &Archive, previous: &[&Digest], claims: &[Claim]) -> Digest {
        let mut text = if previous.is_empty() {
            "{\"ossuary-segment\":1}\n".to_string()
        } else {
            let names: Vec<String> = previous.iter().map(|name| format!("\"{name}\"")).collect();
            format!(
                "{{\"ossuary-segment\":1,\"previous\":[{}]}}\n",
                names.join(",")
            )
        };
        for claim in claims {
            text.push_str(&claim.to_line());
            text.push('\n');
        }
        let (_, entry) = archive.log().store().add(text.as_bytes()).unwrap();
        entry.digest().clone()
    }

    fn old_head(archive: &Archive, previous: &Digest, claims: &[Claim]) {
        let mut text = format!("{{\"ossuary-segment\":1,\"previous\":[\"{previous}\"]}}\n");
        for claim in claims {
            text.push_str(&claim.to_line());
            text.push('\n');
        }
        fs::write(archive.log().head_path(), text).unwrap();
    }

    const WORDS: Words = Words {
        done: "in the current form",
        nothing: "every segment is in the current form",
    };

    fn sound(archive: &Archive) -> bool {
        audit_log(archive.log()).unwrap().findings() == 0
    }

    #[test]
    fn a_log_in_the_current_form_has_nothing_to_rewrite() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let log = archive.log();
        log.append(&claim(&archive, b"first", "2026-09-01T00:00:00Z"))
            .unwrap();
        log.seal().unwrap().unwrap();
        log.append(&claim(&archive, b"second", "2026-09-02T00:00:00Z"))
            .unwrap();
        log.seal().unwrap().unwrap();
        log.append(&claim(&archive, b"open", "2026-09-03T00:00:00Z"))
            .unwrap();

        let plan = Rewrite::plan(&archive, WORDS, |_, _| Ok(())).unwrap();

        assert!(!plan.changes());
        assert_eq!(plan.unchanged, 2);
        assert_eq!(
            plan.sentence(false),
            "nothing to do: every segment is in the current form"
        );
        assert_eq!(plan.seals(false), None);
    }

    #[test]
    fn the_old_form_is_rewritten_along_the_whole_chain() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let first = old_segment(
            &archive,
            &[],
            &[claim(&archive, b"first", "2026-09-01T00:00:00Z")],
        );
        let second = old_segment(
            &archive,
            &[&first],
            &[claim(&archive, b"second", "2026-09-02T00:00:00Z")],
        );
        let third = old_segment(
            &archive,
            &[&second],
            &[claim(&archive, b"third", "2026-09-03T00:00:00Z")],
        );
        old_head(
            &archive,
            &third,
            &[claim(&archive, b"open", "2026-09-04T00:00:00Z")],
        );
        assert!(
            archive.log().head().is_err(),
            "the current build refuses the old form"
        );
        let cache = archive.root().join("cache");
        fs::write(cache.join("index.sqlite"), b"stale").unwrap();

        let plan = Rewrite::plan(&archive, WORDS, |_, _| Ok(())).unwrap();

        assert_eq!(
            plan.steps.iter().map(|step| &step.old).collect::<Vec<_>>(),
            vec![&second, &third],
            "the first segment names nothing and keeps its bytes; the others follow in order"
        );
        assert!(plan.head.is_some());
        assert_eq!(plan.unchanged, 1);
        assert_eq!(
            plan.sentence(true),
            "2 segment(s), and the open segment would be rewritten in the current form; 1 unchanged"
        );
        assert_eq!(
            plan.seals(true).as_deref(),
            Some("seal ids given out since 2026-09-02T00:00:00Z would no longer check")
        );

        plan.apply(&archive).unwrap();

        let log = archive.log();
        assert_eq!(log.head().unwrap().len(), 1, "the head's claim is kept");
        let segments = log.segments().unwrap();
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0].digest(), &first, "the first kept its name");
        assert_eq!(
            log.contents(segments[2].digest()).unwrap().previous(),
            Some(segments[1].digest()),
            "the third names the renamed second"
        );
        assert!(sound(&archive), "and the old entries are gone");
        assert!(!cache.join("index.sqlite").exists(), "the index is dropped");

        let again = Rewrite::plan(&archive, WORDS, |_, _| Ok(())).unwrap();
        assert!(!again.changes(), "a second run finds the current form");
    }

    #[test]
    fn a_dropped_segment_is_removed_and_its_successor_relinked() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let log = archive.log();
        log.append(&claim(&archive, b"first", "2026-09-01T00:00:00Z"))
            .unwrap();
        let first = log.seal().unwrap().unwrap().digest().clone();
        log.append(&claim(&archive, b"second", "2026-09-02T00:00:00Z"))
            .unwrap();
        let second = log.seal().unwrap().unwrap().digest().clone();
        log.append(&claim(&archive, b"third", "2026-09-03T00:00:00Z"))
            .unwrap();
        let third = log.seal().unwrap().unwrap().digest().clone();

        let plan = Rewrite::plan(&archive, WORDS, |segments, _| {
            segments.get_mut(second.as_str()).unwrap().dropped = true;
            segments.get_mut(third.as_str()).unwrap().previous = vec![first.to_string()];
            Ok(())
        })
        .unwrap();
        assert_eq!(
            plan.sentence(true),
            "1 segment(s), and the open segment would be rewritten in the current form; 1 segment(s) without claims would be removed; 1 unchanged"
        );
        plan.apply(&archive).unwrap();

        let segments = log.segments().unwrap();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].digest(), &first);
        assert!(!log.store().contains(&second).unwrap());
        assert!(sound(&archive));
    }

    #[test]
    fn an_edit_may_begin_a_lost_head() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let log = archive.log();
        log.append(&claim(&archive, b"first", "2026-09-01T00:00:00Z"))
            .unwrap();
        let first = log.seal().unwrap().unwrap().digest().clone();
        fs::remove_file(log.head_path()).unwrap();

        let plan = Rewrite::plan(&archive, WORDS, |_, head| {
            assert!(head.is_none(), "a missing head comes as none");
            *head = Some(Segment {
                previous: vec![first.to_string()],
                ..Segment::default()
            });
            Ok(())
        })
        .unwrap();
        plan.apply(&archive).unwrap();

        assert_eq!(log.head_contents().unwrap().previous(), Some(&first));
        assert!(sound(&archive));
    }

    #[test]
    fn several_predecessors_left_by_an_edit_are_refused() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let first = old_segment(
            &archive,
            &[],
            &[claim(&archive, b"first", "2026-09-01T00:00:00Z")],
        );
        let other = old_segment(
            &archive,
            &[],
            &[claim(&archive, b"other", "2026-09-01T00:00:00Z")],
        );
        old_segment(
            &archive,
            &[&first, &other],
            &[claim(&archive, b"joined", "2026-09-02T00:00:00Z")],
        );

        let error = Rewrite::plan(&archive, WORDS, |_, _| Ok(())).unwrap_err();

        assert!(
            format!("{error:#}").contains("names 2 segments"),
            "{error:#}"
        );
    }

    #[test]
    fn an_edit_renames_the_segment_and_keeps_unknown_header_members() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let log = archive.log();
        let text = format!(
            "{{\"ossuary-segment\":1,\"signature\":\"ed25519:…\"}}\n{}\n",
            claim(&archive, b"first", "2026-09-01T00:00:00Z").to_line()
        );
        let (_, first) = log.store().add(text.as_bytes()).unwrap();
        fs::write(log.head_path(), ossuary_core::head_after(first.digest())).unwrap();
        log.append(&claim(&archive, b"open", "2026-09-02T00:00:00Z"))
            .unwrap();

        let plan = Rewrite::plan(&archive, WORDS, |segments, head| {
            for segment in segments.values_mut().chain(head.as_mut()) {
                for line in &mut segment.lines {
                    *line = line.replace("\"source\":\"test\"", "\"source\":\"edited\"");
                }
            }
            Ok(())
        })
        .unwrap();
        plan.apply(&archive).unwrap();

        let segments = log.segments().unwrap();
        assert_eq!(segments.len(), 1);
        let renamed = segments[0].digest();
        assert_ne!(renamed, first.digest());
        let stored = String::from_utf8(log.store().read(renamed).unwrap().unwrap()).unwrap();
        assert!(
            stored.starts_with("{\"ossuary-segment\":1,\"signature\":\"ed25519:…\"}\n"),
            "the member this build does not know stays: {stored}"
        );
        assert_eq!(log.read(renamed).unwrap()[0].source().as_str(), "edited");
        assert_eq!(log.head().unwrap()[0].source().as_str(), "edited");
        assert!(sound(&archive));
    }

    #[test]
    fn a_damaged_segment_is_refused_before_anything_is_planned() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let first = old_segment(
            &archive,
            &[],
            &[claim(&archive, b"first", "2026-09-01T00:00:00Z")],
        );
        let path = archive.log().store().find(&first).unwrap().unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        #[allow(
            clippy::permissions_set_readonly_false,
            reason = "the test plays the corruption"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(&path, permissions).unwrap();
        fs::write(&path, b"{\"ossuary-segment\":1}\n").unwrap();

        let error = Rewrite::plan(&archive, WORDS, |_, _| Ok(())).unwrap_err();

        assert!(error.to_string().contains("damaged"), "{error}");
    }
}
