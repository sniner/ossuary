//! How answers read on a terminal.

use std::fmt::Write as _;

use ossuary_core::{Attribute, Claim, Episode, Source, Value};

/// One claim as one line: when it was recorded, what it says, who says
/// so in brackets, and in which run, bracketed beside it as `[run:…]` —
/// a claim from before runs were written ends with its source.
///
/// Values read as JSON — a string keeps its quotes, a number stands bare —
/// so what the reader sees is what the log holds, type and all.
pub fn line(claim: &Claim) -> String {
    let time = claim.time().as_str();
    let attribute = claim.attribute().as_str();
    let source = claim.source().as_str();
    let run = claim
        .run()
        .map(|run| format!(" [run:{}]", run.as_str()))
        .unwrap_or_default();
    match (claim.value(), claim.is_retraction()) {
        (Some(value), false) => format!("{time}  {attribute} = {value}  [{source}]{run}"),
        (Some(value), true) => {
            format!("{time}  retracted: {attribute} = {value}  [{source}]{run}")
        }
        (None, _) => format!("{time}  retracted: {attribute}, all values  [{source}]{run}"),
    }
}

/// One run of the record as one line: when it closed, its id, what it
/// wrote, and who spoke in it — the time in the spelling `--as-of`
/// takes, the id the spelling `export`, `extract` and `--as-of` take.
pub fn episode(episode: &Episode) -> String {
    let mut wrote = format!("{} file(s), {} claim(s)", episode.files, episode.claims);
    if episode.retractions > 0 {
        let _ = write!(wrote, ", {} of them retractions", episode.retractions);
    }
    let sources: Vec<&str> = episode.sources.iter().map(Source::as_str).collect();
    format!(
        "{}  {}  {wrote}  [{}]",
        episode.last,
        episode.run.as_str(),
        sources.join(", ")
    )
}

/// One run as one JSON object, ready for `jq`.
pub fn episode_line(episode: &Episode) -> String {
    let sources: Vec<Value> = episode
        .sources
        .iter()
        .map(|source| Value::String(source.as_str().to_string()))
        .collect();
    format!(
        "{{\"run\":{},\"first\":{},\"last\":{},\"files\":{},\"claims\":{},\"retractions\":{},\"sources\":{}}}",
        Value::String(episode.run.as_str().to_string()),
        Value::String(episode.first.clone()),
        Value::String(episode.last.clone()),
        episode.files,
        episode.claims,
        episode.retractions,
        Value::Array(sources)
    )
}

/// One `find` match as one block: the file's short name on a line of
/// its own, then every shown attribute indented beneath it, one
/// `attribute=value` pair per line, spelled the way a query would —
/// the answer speaks the question's own language, so a pair reads
/// back as a term. A string that could be misread — several words, a
/// glob or range character, a quote — wears the double quotes that
/// mean *literal* in a query, and a pasted pair finds exactly this
/// file again. Several standing values repeat the attribute: the set,
/// not a choice. Other types keep their JSON spelling.
pub fn match_block(name: &str, shown: &[(String, Vec<Value>)]) -> String {
    let mut block = String::new();
    block_at(&mut block, name, shown, 0);
    block
}

/// One match with what was won out of it: the file, what it shows, and
/// every derived file the same way, as far as the derivations go.
#[derive(Debug, Default, PartialEq)]
pub struct Found {
    /// The full name, what `--json` and `--id` say.
    pub subject: String,
    /// The short name, what a block is headed by.
    pub name: String,
    /// The shown attributes with their values, in the question's order.
    pub shown: Vec<(String, Vec<Value>)>,
    /// What was derived from this file, each with its own derivations.
    pub derived: Vec<Found>,
}

/// One `find` match with its derivations as one block, drawn the way
/// `tree` draws a disk: the match's short name heads it, each derived
/// file hangs on a branch of its own beneath its origin, the last one
/// closing the branch, and a file's pairs stand under its name ahead
/// of its branches, beside the bar that leads to the first of them. A
/// file with nothing beneath it is a name alone.
pub fn match_tree(found: &Found) -> String {
    let mut lines = vec![found.name.clone()];
    drawn(found, "", &mut lines);
    lines.join("\n")
}

/// What stands beneath one file's name, every line led by `prefix`:
/// its pairs, then each derived file on a branch with its own beneath.
fn drawn(found: &Found, prefix: &str, lines: &mut Vec<String>) {
    // The bar beside the pairs leads down to the first branch; a leaf
    // has none to lead to.
    let bar = if found.derived.is_empty() {
        "    "
    } else {
        "│   "
    };
    for (attribute, values) in &found.shown {
        for value in values {
            lines.push(format!("{prefix}{bar}{}", pair(attribute, value)));
        }
    }
    let mut ahead = found.derived.len();
    for derived in &found.derived {
        ahead -= 1;
        let (tee, below) = if ahead == 0 {
            ("└── ", "    ")
        } else {
            ("├── ", "│   ")
        };
        lines.push(format!("{prefix}{tee}{}", derived.name));
        drawn(derived, &format!("{prefix}{below}"), lines);
    }
}

/// The name at `depth` steps of two spaces, each pair one step deeper.
fn block_at(block: &mut String, name: &str, shown: &[(String, Vec<Value>)], depth: usize) {
    let indent = "  ".repeat(depth);
    block.push_str(&indent);
    block.push_str(name);
    for (attribute, values) in shown {
        for value in values {
            block.push('\n');
            block.push_str(&indent);
            block.push_str("  ");
            block.push_str(&pair(attribute, value));
        }
    }
}

/// One `find` match with its derivations as one JSON object: the match
/// as [`json_line`] spells it, then `derived`, a list of the derived
/// files as objects of the same shape, nested as far as the
/// derivations go. The list is there even when empty, so a reader can
/// count on the key.
pub fn json_tree(found: &Found) -> String {
    let mut line = json_line(&found.subject, &found.shown);
    line.pop();
    line.push_str(",\"derived\":[");
    for (position, derived) in found.derived.iter().enumerate() {
        if position > 0 {
            line.push(',');
        }
        line.push_str(&json_tree(derived));
    }
    line.push_str("]}");
    line
}

/// One `find` match as one JSON object: the full subject, then each
/// shown attribute with every standing value as a list. One object per
/// line, ready for `jq`.
pub fn json_line(subject: &str, shown: &[(String, Vec<Value>)]) -> String {
    let mut line = format!("{{\"subject\":{}", Value::String(subject.to_string()));
    for (attribute, values) in shown {
        line.push(',');
        line.push_str(&Value::String(attribute.clone()).to_string());
        line.push(':');
        line.push_str(&Value::Array(values.clone()).to_string());
    }
    line.push('}');
    line
}

/// One `attributes` answer as one JSON object: the attribute, and the
/// number of files it stands on. One object per line, ready for `jq`.
pub fn attribute_line(attribute: &Attribute, files: u64) -> String {
    format!(
        "{{\"attribute\":{},\"files\":{files}}}",
        Value::String(attribute.as_str().to_string())
    )
}

/// What `standing` answers without a heading: every pair on a line of
/// its own, the same query spelling a `find` block indents — the name
/// is absent because the asker typed it themselves.
pub fn pairs(shown: &[(String, Vec<Value>)]) -> String {
    let mut lines = Vec::new();
    for (attribute, values) in shown {
        for value in values {
            lines.push(pair(attribute, value));
        }
    }
    lines.join("\n")
}

/// A byte count the way a human sizes one: binary steps, one decimal
/// under ten — 3.7 GiB reads at a glance where 3971934208 does not.
pub fn human_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "display only — a tenth of a unit is the finest this ever says"
    )]
    let mut size = bytes as f64;
    let mut unit = "B";
    for step in ["KiB", "MiB", "GiB", "TiB", "PiB"] {
        size /= 1024.0;
        unit = step;
        if size < 1024.0 {
            break;
        }
    }
    if size < 10.0 {
        format!("{size:.1} {unit}")
    } else {
        format!("{size:.0} {unit}")
    }
}

/// One shown attribute or field and its value, spelled as a query term.
pub(crate) fn pair(name: &str, value: &Value) -> String {
    match value {
        Value::String(text) if plain(text) => format!("{name}={text}"),
        Value::String(text) => format!("{name}=\"{text}\""),
        other => format!("{name}={other}"),
    }
}

/// Whether a string can stand bare in a pair without being misread as a
/// glob, a range, a quote, or several words.
fn plain(text: &str) -> bool {
    !text.is_empty()
        && !text.contains("..")
        && !text
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '*' | '?' | '"'))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn attribute(name: &str) -> String {
        Attribute::parse(name).unwrap().as_str().to_string()
    }

    #[test]
    fn an_episode_reads_as_one_line_in_the_spellings_the_verbs_take() {
        let episode = Episode {
            run: ossuary_core::Run::parse("315e360b-020e-48be-8f2d-f2002a2ea9b4").unwrap(),
            first: "2026-09-18T21:26:35Z".to_string(),
            last: "2026-09-18T21:27:05Z".to_string(),
            sources: vec![
                Source::parse("ingest").unwrap(),
                Source::parse("user").unwrap(),
            ],
            files: 29,
            claims: 210,
            retractions: 7,
        };
        assert_eq!(
            self::episode(&episode),
            "2026-09-18T21:27:05Z  315e360b-020e-48be-8f2d-f2002a2ea9b4  29 file(s), 210 claim(s), 7 of them retractions  [ingest, user]",
            "the closing moment, the id, what was written, who spoke"
        );
        assert_eq!(
            episode_line(&episode),
            r#"{"run":"315e360b-020e-48be-8f2d-f2002a2ea9b4","first":"2026-09-18T21:26:35Z","last":"2026-09-18T21:27:05Z","files":29,"claims":210,"retractions":7,"sources":["ingest","user"]}"#
        );
    }

    #[test]
    fn a_match_block_speaks_the_query_language() {
        let shown = vec![
            (attribute("file:mime"), vec![json!("application/pdf")]),
            (
                attribute("file:name"),
                vec![json!("Rechnung 07.pdf"), json!("scan.pdf")],
            ),
            (attribute("file:size"), vec![json!(54597)]),
        ];
        assert_eq!(
            match_block("1f95c2ab", &shown),
            "1f95c2ab\n  file:mime=application/pdf\n  file:name=\"Rechnung 07.pdf\"\n  file:name=scan.pdf\n  file:size=54597",
            "the name a line of its own, each pair beneath it: bare when plain, literal quotes on a space, numbers as spelled"
        );
        assert_eq!(
            match_block("1f95c2ab", &[]),
            "1f95c2ab",
            "nothing shown is the name alone"
        );
        assert_eq!(
            pair("user:tag", &json!("a..b")),
            "user:tag=\"a..b\"",
            "a value that reads as a range is quoted back to literal"
        );
        assert_eq!(pair("user:tag", &json!("v*")), "user:tag=\"v*\"");
    }

    #[test]
    fn a_match_tree_hangs_each_derivation_on_a_branch_below_its_origin() {
        let found = Found {
            subject: "e9ed6104aa".to_string(),
            name: "e9ed6104".to_string(),
            shown: vec![(attribute("file:name"), vec![json!("quarterly.eml")])],
            derived: vec![
                Found {
                    subject: "b5743276aa".to_string(),
                    name: "b5743276".to_string(),
                    shown: vec![
                        (attribute("file:mime"), vec![json!("application/pdf")]),
                        (attribute("file:name"), vec![json!("report.pdf")]),
                    ],
                    derived: vec![Found {
                        subject: "3f0c91aaaa".to_string(),
                        name: "3f0c91aa".to_string(),
                        shown: vec![(attribute("file:mime"), vec![json!("text/plain")])],
                        derived: Vec::new(),
                    }],
                },
                Found {
                    subject: "17a2c0ffaa".to_string(),
                    name: "17a2c0ff".to_string(),
                    shown: Vec::new(),
                    derived: Vec::new(),
                },
            ],
        };
        assert_eq!(
            match_tree(&found),
            [
                "e9ed6104",
                "│   file:name=quarterly.eml",
                "├── b5743276",
                "│   │   file:mime=application/pdf",
                "│   │   file:name=report.pdf",
                "│   └── 3f0c91aa",
                "│           file:mime=text/plain",
                "└── 17a2c0ff",
            ]
            .join("\n"),
            "the pairs under their name beside the bar to the first branch, a leaf's without one"
        );
        assert_eq!(
            json_tree(&found),
            "{\"subject\":\"e9ed6104aa\",\"file:name\":[\"quarterly.eml\"],\"derived\":[{\"subject\":\"b5743276aa\",\"file:mime\":[\"application/pdf\"],\"file:name\":[\"report.pdf\"],\"derived\":[{\"subject\":\"3f0c91aaaa\",\"file:mime\":[\"text/plain\"],\"derived\":[]}]},{\"subject\":\"17a2c0ffaa\",\"derived\":[]}]}",
            "the same tree nested under `derived`, the list there even when empty"
        );
        let alone = Found {
            subject: "9f2a".to_string(),
            name: "9f2a".to_string(),
            shown: Vec::new(),
            derived: Vec::new(),
        };
        assert_eq!(
            match_tree(&alone),
            "9f2a",
            "nothing derived is the match alone"
        );
        assert_eq!(json_tree(&alone), "{\"subject\":\"9f2a\",\"derived\":[]}");
    }

    #[test]
    fn pairs_speak_the_query_language_without_a_heading() {
        let shown = vec![
            (attribute("file:mime"), vec![json!("application/pdf")]),
            (
                attribute("file:name"),
                vec![json!("Rechnung 07.pdf"), json!("scan.pdf")],
            ),
        ];
        assert_eq!(
            pairs(&shown),
            "file:mime=application/pdf\nfile:name=\"Rechnung 07.pdf\"\nfile:name=scan.pdf",
            "one pair per line, no name and no indent — the asker typed the subject themselves"
        );
    }

    #[test]
    fn bytes_read_the_way_a_human_sizes_them() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(4096), "4.0 KiB");
        assert_eq!(human_bytes(54_597), "53 KiB");
        assert_eq!(human_bytes(3_971_934_208), "3.7 GiB");
    }

    #[test]
    fn a_json_line_carries_the_whole_sets() {
        let shown = vec![(attribute("file:name"), vec![json!("a.pdf"), json!("b.pdf")])];
        assert_eq!(
            json_line("9f2a", &shown),
            "{\"subject\":\"9f2a\",\"file:name\":[\"a.pdf\",\"b.pdf\"]}"
        );
        assert_eq!(json_line("9f2a", &[]), "{\"subject\":\"9f2a\"}");
    }
}
