//! Rewriting sealed segments: what `ossuary maintain` never does, and
//! the one mechanism shared by every fix that has to.
//!
//! A segment is named by its bytes, and every later segment names it in
//! its header, so a change to one segment renames it and every segment
//! after it, up to the open head. The rewriter does the whole of that.
//! It reads every segment and the head, hands each one to the fix to
//! change, and works out the new names in dependency order: a segment
//! is renamed only after every segment it names has been. A segment
//! whose new bytes are its old bytes keeps its name. The result is one
//! plan, applied in three steps: the new entries are added, the head is
//! replaced, and the old entries are removed, those that name others
//! first. An interrupted run leaves old segments that still name old
//! segments and new ones that name new ones, and a second run finishes
//! the job, because the same bytes get the same name.
//!
//! The header is read here as a JSON object, without going through
//! `ossuary_core::Log`, because a fix may have to read a form the
//! current build refuses. It is written back in the current form of the
//! members this build knows (`ossuary-segment`, `previous` as a list,
//! `mend`), with every other member kept as it was.
//!
//! The query index in `cache/` is removed with the old segments: it
//! records which segments it has folded by name, and would fold the
//! renamed ones a second time. Manifests of removed segments are
//! removed as well; the rest of the cache is not touched.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result, anyhow, bail};
use immure::Digest;
use ossuary_core::{Archive, GENERATION};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One segment as a fix sees it: the header taken apart, the claim
/// lines verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Segment {
    /// The segments sealed before this one, by name. The rewriter
    /// replaces a name here with the new name of the segment it names;
    /// a fix does not.
    pub previous: Vec<String>,
    /// The mend's members, when the segment is a mend.
    pub mend: Option<MendHeader>,
    /// Header members this build does not know, kept as they were.
    pub extra: Map<String, Value>,
    /// The claim lines after the header, without their line breaks.
    pub lines: Vec<String>,
}

/// A mend's members. `before` names a segment and is renamed with it;
/// `replaces` names a lost segment and is kept as it was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MendHeader {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The words a fix gives the sentence.
#[derive(Debug, Clone, Copy)]
pub struct Words {
    /// What is done to the segments, after "rewritten": `with previous
    /// as a list`.
    pub done: &'static str,
    /// What to say when no segment changes.
    pub nothing: &'static str,
}

/// A rewrite worked out and not yet written.
#[derive(Debug)]
pub struct Rewrite {
    /// The segments that change, in dependency order: a segment comes
    /// after every segment it names.
    steps: Vec<Step>,
    /// The new text of the open head, when it changes.
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
}

/// The header as it is written: the known members first, in the order
/// `ossuary_core` writes them, then everything else.
#[derive(Serialize)]
struct Header<'a> {
    #[serde(rename = "ossuary-segment")]
    generation: u32,
    #[serde(skip_serializing_if = "no_names")]
    previous: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    mend: Option<&'a MendHeader>,
    #[serde(flatten)]
    extra: &'a Map<String, Value>,
}

fn no_names(names: &&[String]) -> bool {
    names.is_empty()
}

impl Rewrite {
    /// Read every sealed segment and the open head, let `edit` change
    /// each one, and work out what has to be written.
    ///
    /// Nothing is written. A segment whose bytes do not match its name
    /// is refused before anything is read further: a rewrite would give
    /// the damaged bytes a true name. Segments that name each other in a
    /// circle are refused, as is a header this build cannot read.
    ///
    /// # Errors
    ///
    /// Whatever reading the store and the head can answer, and the
    /// refusals above.
    pub fn plan(
        archive: &Archive,
        words: Words,
        mut edit: impl FnMut(&mut Segment) -> Result<()>,
    ) -> Result<Rewrite> {
        let log = archive.log();
        let store = log.store();
        let mut read: BTreeMap<String, (Digest, String, Segment)> = BTreeMap::new();
        for entry in store.entries() {
            let entry = entry?;
            let digest = entry.digest().clone();
            if !store.verify(&entry)? {
                bail!(
                    "segment {digest} is damaged; restore it from a backup of the archive before running this fix"
                );
            }
            let bytes = store
                .read(&digest)?
                .ok_or_else(|| anyhow!("segment {digest} disappeared while reading"))?;
            let text = String::from_utf8(bytes)
                .map_err(|_| anyhow!("segment {digest} is not UTF-8 text"))?;
            let mut segment = parse(&text).with_context(|| format!("segment {digest}"))?;
            edit(&mut segment)?;
            read.insert(digest.as_str().to_string(), (digest, text, segment));
        }
        let head = match fs::read_to_string(log.head_path()) {
            Ok(text) => {
                let mut segment = parse(&text).context("the open segment")?;
                edit(&mut segment)?;
                Some((text, segment))
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(source).context(format!("{}: reading", log.head_path().display()));
            }
        };

        // New names in dependency order. A name that no entry answers
        // to is kept: it names a lost segment, and the record keeps
        // saying so.
        let mut names: BTreeMap<&str, String> = BTreeMap::new();
        let mut remaining: BTreeSet<&str> = read.keys().map(String::as_str).collect();
        let mut steps = Vec::new();
        let mut unchanged = 0;
        while !remaining.is_empty() {
            let ready: Vec<&str> = remaining
                .iter()
                .copied()
                .filter(|name| {
                    references(&read[*name].2)
                        .all(|named| !read.contains_key(named) || names.contains_key(named))
                })
                .collect();
            if ready.is_empty() {
                // Names are checksums of the bytes that carry them, so
                // no segment can name one that names it back; the loop
                // still has to end if a store holds what no build wrote.
                bail!(
                    "{} segment(s) name each other in a circle; nothing was written",
                    remaining.len()
                );
            }
            for name in ready {
                remaining.remove(name);
                let (old, old_text, segment) = &read[name];
                let text = render(segment, &names);
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
                });
            }
        }
        let head = head.and_then(|(old_text, segment)| {
            let text = render(&segment, &names);
            (text != old_text).then_some(text)
        });
        Ok(Rewrite {
            steps,
            head,
            unchanged,
            words,
        })
    }

    /// Whether anything would be written.
    #[must_use]
    pub fn changes(&self) -> bool {
        !self.steps.is_empty() || self.head.is_some()
    }

    /// Write the plan: new entries, then the head, then the old entries
    /// removed, those that name others first; then the caches that
    /// know segments by name.
    ///
    /// # Errors
    ///
    /// Whatever the store and the file system can answer. The order
    /// of steps is such that a run interrupted anywhere is finished by
    /// planning and applying again.
    pub fn apply(&self, archive: &Archive) -> Result<()> {
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
        forget(archive, &self.steps)?;
        Ok(())
    }

    /// The one line the fix answers with.
    #[must_use]
    pub fn sentence(&self, dry_run: bool) -> String {
        if !self.changes() {
            return format!("nothing to do: {}", self.words.nothing);
        }
        let count = self.steps.len();
        let head = match (count, self.head.is_some()) {
            (0, _) => "the open segment",
            (_, true) => ", and the open segment",
            (_, false) => "",
        };
        let mut sentence = if dry_run {
            if count == 0 {
                format!("{head} would be rewritten {}", self.words.done)
            } else {
                format!(
                    "{count} segment(s){head} would be rewritten {}",
                    self.words.done
                )
            }
        } else if count == 0 {
            format!("{head} rewritten {}", self.words.done)
        } else {
            format!(
                "{count} segment(s){head} rewritten {}; the query index in cache/ was removed and is rebuilt by the next command",
                self.words.done
            )
        };
        if self.unchanged > 0 {
            // Writing to a String does not fail.
            let _ = write!(sentence, "; {} unchanged", self.unchanged);
        }
        sentence
    }
}

/// The names a segment's header refers to that are renamed with their
/// segments: its predecessors, and the segment a mend stands before.
fn references(segment: &Segment) -> impl Iterator<Item = &str> {
    segment.previous.iter().map(String::as_str).chain(
        segment
            .mend
            .iter()
            .filter_map(|mend| mend.before.as_deref()),
    )
}

/// A segment's text with the names it refers to replaced by their new
/// ones: the header line, then the claim lines, each ended by a line
/// break.
fn render(segment: &Segment, names: &BTreeMap<&str, String>) -> String {
    let rename = |name: &String| {
        names
            .get(name.as_str())
            .cloned()
            .unwrap_or_else(|| name.clone())
    };
    let previous: Vec<String> = segment.previous.iter().map(rename).collect();
    let mend = segment.mend.as_ref().map(|mend| MendHeader {
        before: mend.before.as_ref().map(rename),
        replaces: mend.replaces.clone(),
        extra: mend.extra.clone(),
    });
    let header = Header {
        generation: GENERATION,
        previous: &previous,
        mend: mend.as_ref(),
        extra: &segment.extra,
    };
    // A header of known members and a map of JSON values serialises.
    let mut text = serde_json::to_string(&header).expect("a header serialises");
    text.push('\n');
    for line in &segment.lines {
        text.push_str(line);
        text.push('\n');
    }
    text
}

/// A segment's text taken apart: the header's known members, the
/// members this build does not know, and the claim lines as they are.
/// `previous` is read as a list or, from a build before the list, as a
/// string; nothing else about the header is relaxed.
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
        Some(other) => bail!("previous is neither a list nor a string: {other}"),
    };
    let mend = header
        .remove("mend")
        .map(serde_json::from_value::<MendHeader>)
        .transpose()
        .context("the mend member")?;
    Ok(Segment {
        previous,
        mend,
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

/// Drop what the cache knows of segments by name: the query index
/// whole, and the manifests of the segments removed.
fn forget(archive: &Archive, steps: &[Step]) -> Result<()> {
    if steps.is_empty() {
        return Ok(());
    }
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
    // A manifest of a removed segment is never read again; one that
    // will not go is left.
    for step in steps {
        let _ = fs::remove_file(cache.join("manifests").join(format!("{}.json", step.old)));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ossuary_core::{
        Algorithm, Attribute, Claim, Run, Source, Subject, Timestamp, audit_log, audit_store,
    };
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

    /// A segment in the form of a build before the list: `previous` a
    /// string. Stored straight into the claims store.
    fn old_segment(archive: &Archive, previous: Option<&Digest>, claims: &[Claim]) -> Digest {
        let mut text = match previous {
            Some(previous) => format!("{{\"ossuary-segment\":1,\"previous\":\"{previous}\"}}\n"),
            None => "{\"ossuary-segment\":1}\n".to_string(),
        };
        for claim in claims {
            text.push_str(&claim.to_line());
            text.push('\n');
        }
        let (_, entry) = archive.log().store().add(text.as_bytes()).unwrap();
        entry.digest().clone()
    }

    fn old_head(archive: &Archive, previous: &Digest, claims: &[Claim]) {
        let mut text = format!("{{\"ossuary-segment\":1,\"previous\":\"{previous}\"}}\n");
        for claim in claims {
            text.push_str(&claim.to_line());
            text.push('\n');
        }
        fs::write(archive.log().head_path(), text).unwrap();
    }

    const WORDS: Words = Words {
        done: "with previous as a list",
        nothing: "every segment is in the current form",
    };

    fn sound(archive: &Archive) -> bool {
        ossuary_core::Audit::assemble(
            audit_store(archive.content()).unwrap(),
            audit_store(archive.derived()).unwrap(),
            audit_log(archive.log()).unwrap(),
        )
        .is_sound()
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

        let plan = Rewrite::plan(&archive, WORDS, |_| Ok(())).unwrap();

        assert!(!plan.changes());
        assert_eq!(plan.unchanged, 2);
        assert_eq!(
            plan.sentence(false),
            "nothing to do: every segment is in the current form"
        );
    }

    #[test]
    fn the_old_form_is_rewritten_along_the_whole_chain() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let first = old_segment(
            &archive,
            None,
            &[claim(&archive, b"first", "2026-09-01T00:00:00Z")],
        );
        let second = old_segment(
            &archive,
            Some(&first),
            &[claim(&archive, b"second", "2026-09-02T00:00:00Z")],
        );
        let third = old_segment(
            &archive,
            Some(&second),
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

        let plan = Rewrite::plan(&archive, WORDS, |_| Ok(())).unwrap();

        assert_eq!(
            plan.steps.iter().map(|step| &step.old).collect::<Vec<_>>(),
            vec![&second, &third],
            "the first segment names nothing and keeps its form; the others follow in order"
        );
        assert!(plan.head.is_some());
        assert_eq!(plan.unchanged, 1);
        assert_eq!(
            plan.sentence(true),
            "2 segment(s), and the open segment would be rewritten with previous as a list; 1 unchanged"
        );

        plan.apply(&archive).unwrap();

        let log = archive.log();
        assert_eq!(log.head().unwrap().len(), 1, "the head's claim is kept");
        let segments = log.segments().unwrap();
        assert_eq!(segments.len(), 3, "the old entries are gone");
        assert_eq!(
            log.contents(segments[1].digest()).unwrap().previous(),
            std::slice::from_ref(&first),
            "the first segment kept its name"
        );
        assert_eq!(
            log.contents(segments[2].digest()).unwrap().previous(),
            std::slice::from_ref(segments[1].digest()),
            "the third names the renamed second"
        );
        assert_eq!(
            log.head_contents().unwrap().previous(),
            std::slice::from_ref(segments[2].digest())
        );
        assert!(sound(&archive));
        assert!(!cache.join("index.sqlite").exists(), "the index is dropped");
        assert_eq!(
            plan.sentence(false),
            "2 segment(s), and the open segment rewritten with previous as a list; the query index in cache/ was removed and is rebuilt by the next command; 1 unchanged"
        );

        let again = Rewrite::plan(&archive, WORDS, |_| Ok(())).unwrap();
        assert!(!again.changes(), "a second run finds the current form");
    }

    #[test]
    fn a_mend_follows_the_segment_it_stands_before() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        // A head lost between the first and the second segment, mended;
        // the second names the first in the old form and is renamed, so
        // the mend's `before` must follow it.
        let first = old_segment(
            &archive,
            None,
            &[claim(&archive, b"first", "2026-09-01T00:00:00Z")],
        );
        let second = old_segment(
            &archive,
            Some(&first),
            &[claim(&archive, b"second", "2026-09-02T00:00:00Z")],
        );
        let third = old_segment(
            &archive,
            None,
            &[claim(
                &archive,
                b"third, after a lost head",
                "2026-09-03T00:00:00Z",
            )],
        );
        let lost = "ab".repeat(32);
        let mend_text = format!(
            "{{\"ossuary-segment\":1,\"previous\":\"{second}\",\"mend\":{{\"before\":\"{third}\",\"replaces\":\"{lost}\"}}}}\n"
        );
        let (_, mend) = archive.log().store().add(mend_text.as_bytes()).unwrap();
        old_head(&archive, &third, &[]);

        let plan = Rewrite::plan(&archive, WORDS, |_| Ok(())).unwrap();
        assert_eq!(plan.steps.len(), 2, "the second and the mend");
        plan.apply(&archive).unwrap();

        let log = audit_log(archive.log()).unwrap();
        assert_eq!(log.mended.len(), 1, "the mend still closes the break");
        assert_eq!(log.mended[0].before.as_deref(), Some(third.as_str()));
        assert_eq!(log.mended[0].replaces.as_deref(), Some(lost.as_str()));
        assert_ne!(log.mended[0].mend, mend.digest().as_str());
        assert!(log.breaks.is_empty());
        assert!(sound(&archive));
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
        log.head_follows(std::slice::from_ref(first.digest()))
            .unwrap();
        log.append(&claim(&archive, b"open", "2026-09-02T00:00:00Z"))
            .unwrap();

        let plan = Rewrite::plan(&archive, WORDS, |segment| {
            for line in &mut segment.lines {
                *line = line.replace("\"source\":\"test\"", "\"source\":\"edited\"");
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
        assert_eq!(
            log.read(renamed).unwrap()[0].source().as_str(),
            "edited",
            "the claim line was changed"
        );
        assert_eq!(
            log.head().unwrap()[0].source().as_str(),
            "edited",
            "the head was edited too"
        );
        assert!(sound(&archive));
    }

    #[test]
    fn a_damaged_segment_is_refused_before_anything_is_planned() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let first = old_segment(
            &archive,
            None,
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

        let error = Rewrite::plan(&archive, WORDS, |_| Ok(())).unwrap_err();

        assert!(error.to_string().contains("damaged"), "{error}");
    }
}
