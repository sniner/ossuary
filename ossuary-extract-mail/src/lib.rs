//! The mail extractor for ossuary is the program `ossuary-extract-mail`.
//! This library holds only what other programs of the workspace read by
//! the same rule as the extractor.

use std::collections::HashSet;

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

/// Header names that only a message has: the headers the extractor
/// records, and those the transport adds. A bounce with nothing but
/// `Received` and `Return-Path` is still a message.
const MAILLY: [&str; 14] = [
    "from",
    "sender",
    "reply-to",
    "to",
    "cc",
    "bcc",
    "subject",
    "date",
    "message-id",
    "in-reply-to",
    "references",
    "received",
    "return-path",
    "delivered-to",
];

/// Whether the bytes are an internet message from their first byte: a
/// header section of well-formed fields (names in the alphabet of RFC
/// 5322, a folded line only after a field) with at least two different
/// names that only a message has. One is not enough, because any text
/// can have `Date:` at the start of a line. Bytes that begin with the
/// `From ` line of an mbox are a mailbox with messages in it, not a
/// message.
#[must_use]
pub fn looks_like_mail(bytes: &[u8]) -> bool {
    if bytes.starts_with(b"From ") {
        return false;
    }
    let mut known: HashSet<&'static str> = HashSet::new();
    let mut fields = 0usize;
    for line in bytes.split(|&byte| byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            break;
        }
        if line[0] == b' ' || line[0] == b'\t' {
            if fields == 0 {
                return false;
            }
            continue;
        }
        let Some(colon) = line.iter().position(|&byte| byte == b':') else {
            return false;
        };
        let name = &line[..colon];
        if name.is_empty() || !name.iter().all(|byte| (33..=126).contains(byte)) {
            return false;
        }
        if let Ok(name) = std::str::from_utf8(name)
            && let Some(field) = MAILLY
                .iter()
                .find(|mailly| mailly.eq_ignore_ascii_case(name))
        {
            known.insert(field);
        }
        fields += 1;
    }
    known.len() >= 2
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
