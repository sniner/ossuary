//! Claim lines read as JSON, for the fixes that take lines out of the
//! log. Such a fix keeps every other line byte for byte, so the ids of
//! the kept claims do not change and a retraction that names one still
//! finds it.

use std::collections::BTreeMap;

use anyhow::{Context as _, Result, bail};
use serde_json::Value;

use crate::rewrite::Segment;

/// The prefix of every extractor's source.
pub const EXTRACTOR: &str = "extractor:";

/// A claim line as a JSON object.
pub fn read(line: &str) -> Result<Value> {
    let claim: Value =
        serde_json::from_str(line).with_context(|| format!("claim is not JSON: {line}"))?;
    if !claim.is_object() {
        bail!("claim is not a JSON object: {line}");
    }
    Ok(claim)
}

/// A string member of a claim, empty when it is missing.
pub fn text<'a>(claim: &'a Value, member: &str) -> &'a str {
    claim
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// The files a claim names: its subject, and its value when that is a
/// string or a list of strings, which is how a link to a file is
/// recorded.
pub fn names(claim: &Value) -> impl Iterator<Item = &str> {
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
pub fn relink(segments: &mut BTreeMap<String, Segment>, head: &mut Option<Segment>) {
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
