//! How answers read on a terminal.

use ossuary_core::{Attribute, Claim, Value};

/// One claim as one line: when it was recorded, what it says, who says so.
///
/// Values read as JSON — a string keeps its quotes, a number stands bare —
/// so what the reader sees is what the log holds, type and all.
pub fn line(claim: &Claim) -> String {
    let time = claim.time().as_str();
    let attribute = claim.attribute().as_str();
    let source = claim.source().as_str();
    match (claim.value(), claim.is_retraction()) {
        (Some(value), false) => format!("{time}  {attribute} = {value}  [{source}]"),
        (Some(value), true) => format!("{time}  retracted: {attribute} = {value}  [{source}]"),
        (None, _) => format!("{time}  retracted: {attribute}, every value  [{source}]"),
    }
}

/// One `find` match as one line: the file's short name, then every shown
/// attribute as the `attribute=value` pair a query would spell — the
/// answer speaks the question's own language, so a line reads back as
/// terms. A string that could be misread — several words, a glob or
/// range character, a quote — wears the double quotes that mean
/// *literal* in a query, and a pasted pair finds exactly this file
/// again. Several standing values repeat the attribute: the set, not a
/// choice. Other types keep their JSON spelling.
pub fn match_line(name: &str, shown: &[(Attribute, Vec<Value>)]) -> String {
    let mut line = name.to_string();
    for (attribute, values) in shown {
        for value in values {
            line.push_str("  ");
            line.push_str(&pair(attribute, value));
        }
    }
    line
}

/// One `find` match as one JSON object: the full subject, then each
/// shown attribute with every standing value as a list. One object per
/// line, ready for `jq`.
pub fn json_line(subject: &str, shown: &[(Attribute, Vec<Value>)]) -> String {
    let mut line = format!("{{\"subject\":{}", Value::String(subject.to_string()));
    for (attribute, values) in shown {
        line.push(',');
        line.push_str(&Value::String(attribute.as_str().to_string()).to_string());
        line.push(':');
        line.push_str(&Value::Array(values.clone()).to_string());
    }
    line.push('}');
    line
}

/// One shown attribute and value, spelled as a query term.
fn pair(attribute: &Attribute, value: &Value) -> String {
    match value {
        Value::String(text) if plain(text) => format!("{}={text}", attribute.as_str()),
        Value::String(text) => format!("{}=\"{text}\"", attribute.as_str()),
        other => format!("{}={other}", attribute.as_str()),
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

    fn attribute(name: &str) -> Attribute {
        Attribute::parse(name).unwrap()
    }

    #[test]
    fn a_match_line_speaks_the_query_language() {
        let shown = vec![
            (attribute("file:mime"), vec![json!("application/pdf")]),
            (
                attribute("file:name"),
                vec![json!("Rechnung 07.pdf"), json!("scan.pdf")],
            ),
            (attribute("file:size"), vec![json!(54597)]),
        ];
        assert_eq!(
            match_line("1f95c2ab", &shown),
            "1f95c2ab  file:mime=application/pdf  file:name=\"Rechnung 07.pdf\"  file:name=scan.pdf  file:size=54597",
            "a plain value stands bare, one with a space wears literal quotes, a number keeps its spelling"
        );
        assert_eq!(
            match_line("1f95c2ab", &[]),
            "1f95c2ab",
            "nothing shown is the name alone"
        );
        assert_eq!(
            pair(&attribute("user:tag"), &json!("a..b")),
            "user:tag=\"a..b\"",
            "a value that reads as a range is quoted back to literal"
        );
        assert_eq!(
            pair(&attribute("user:tag"), &json!("v*")),
            "user:tag=\"v*\""
        );
    }

    #[test]
    fn a_json_line_carries_the_whole_sets() {
        let shown = vec![(attribute("file:name"), vec![json!("a.pdf"), json!("b.pdf")])];
        assert_eq!(
            json_line("sha256:9f2a", &shown),
            "{\"subject\":\"sha256:9f2a\",\"file:name\":[\"a.pdf\",\"b.pdf\"]}"
        );
        assert_eq!(
            json_line("sha256:9f2a", &[]),
            "{\"subject\":\"sha256:9f2a\"}"
        );
    }
}
