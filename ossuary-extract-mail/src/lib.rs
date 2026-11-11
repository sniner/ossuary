//! The mail extractor for ossuary is the program `ossuary-extract-mail`.
//! This library holds only what other programs of the workspace read by
//! the same rule as the extractor.

/// A Message-ID or Content-ID without its angle brackets, which RFC 5322
/// (3.6.4) does not count as part of the id: the first id
/// [`message_ids`] finds, or, without `<`, the whole value. Whitespace
/// at both ends is removed. `None` when nothing is left.
///
/// The value is read leniently because mail programs write broken ones:
/// without brackets, or with the closing bracket cut off.
#[must_use]
pub fn message_id(text: &str) -> Option<&str> {
    if text.contains('<') {
        message_ids(text).into_iter().next()
    } else {
        Some(text.trim()).filter(|id| !id.is_empty())
    }
}

/// Every id in angle brackets, without the brackets and the whitespace at
/// both ends. An id runs from a `<` to the next `>`; a missing `>` ends
/// it at the next `<` or at the end of the value. Text outside the
/// brackets (comments, a phrase like "your message of") is skipped.
/// Without any `<`, a value of one word is one id: some programs write
/// `In-Reply-To` and `References` without brackets.
#[must_use]
pub fn message_ids(text: &str) -> Vec<&str> {
    if !text.contains('<') {
        let word = text.trim();
        return if word.is_empty() || word.contains(char::is_whitespace) {
            Vec::new()
        } else {
            vec![word]
        };
    }
    let mut ids = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        rest = &rest[open + 1..];
        let end = rest.find(['>', '<']).unwrap_or(rest.len());
        let id = rest[..end].trim();
        if !id.is_empty() {
            ids.push(id);
        }
        rest = rest[end..].strip_prefix('>').unwrap_or(&rest[end..]);
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_id_is_read_without_its_brackets() {
        assert_eq!(message_id("<Abc@Example.org>"), Some("Abc@Example.org"));
        assert_eq!(
            message_id("  < abc@example.org >  "),
            Some("abc@example.org")
        );
        assert_eq!(message_id("<abc@example.org"), Some("abc@example.org"));
        assert_eq!(message_id("abc@example.org"), Some("abc@example.org"));
        assert_eq!(
            message_id("<a@example.org> <b@example.org>"),
            Some("a@example.org")
        );
        assert_eq!(message_id("<>"), None);
        assert_eq!(message_id("  "), None);
    }

    #[test]
    fn reading_a_message_id_again_changes_nothing() {
        for value in [
            "<abc@example.org>",
            "<a<b>",
            "abc>",
            "<abc@\r\n example.org> (x)",
        ] {
            let id = message_id(value).unwrap();
            assert_eq!(message_id(id), Some(id), "{value:?}");
        }
    }
}
