//! The readable text of a message, as the `text` contract writes it.
//!
//! Rules:
//!
//! - Only body parts count: a part with a name and an attached message are
//!   attachments ([`Entity::is_attachment`]) and give no text here.
//! - A body part of type text/plain, or without a declared type, gives its
//!   content. A body part of type text/html gives its content without the
//!   markup. Every other type gives nothing.
//! - In a multipart/alternative, the first text/plain part that is not
//!   blank is the text. Without one, it is the first alternative that gives
//!   any text.
//! - In every other multipart, the texts of the parts follow each other in
//!   the order of the message, each starting on a new line.
//!
//! The text is UTF-8 with LF line breaks. A part is read in the charset it
//! declares, and a byte that cannot be read becomes U+FFFD. Without a
//! charset, or with one that is not known, a part is read as UTF-8 if it
//! is valid UTF-8, and as windows-1252 otherwise.

use mail_parser::decoders::charsets::map::charset_decoder;
use mail_parser::decoders::html::html_to_text;

use crate::mime::{Content, Entity};

/// The text of the message `root` was parsed from, `None` when it has
/// none or only whitespace.
pub fn of(root: &Entity, bytes: &[u8]) -> Option<String> {
    let mut text = String::new();
    append(root, bytes, &mut text);
    (!blank(&text)).then_some(text)
}

/// Adds the text of `entity` to `text`, on a new line.
fn append(entity: &Entity, bytes: &[u8], text: &mut String) {
    match &entity.content {
        Content::Message => {}
        Content::Single => {
            if let Some(part) = single(entity, bytes) {
                push(text, &part);
            }
        }
        Content::Multipart(parts) if is_alternative(entity) => {
            if let Some(plain) = parts
                .iter()
                .filter(|part| is_plain(part))
                .filter_map(|part| single(part, bytes))
                .find(|plain| !blank(plain))
            {
                push(text, &plain);
                return;
            }
            // Each alternative is read at most once here, so alternatives
            // nested in alternatives cost no more than the message is deep.
            for part in parts {
                let mut alternative = String::new();
                append(part, bytes, &mut alternative);
                if !blank(&alternative) {
                    push(text, &alternative);
                    return;
                }
            }
        }
        Content::Multipart(parts) => {
            for part in parts {
                append(part, bytes, text);
            }
        }
    }
}

/// The text of a part that is not split, `None` for an attachment and for
/// a type that is not text.
fn single(entity: &Entity, bytes: &[u8]) -> Option<String> {
    if entity.is_attachment() || !matches!(entity.content, Content::Single) {
        return None;
    }
    let decoded = entity.decoded(bytes);
    let charset = entity.fields.charset.as_deref();
    match entity.fields.content_type.as_deref() {
        None | Some("text/plain") => Some(line_breaks(&utf8(&decoded, charset))),
        Some("text/html") => Some(html_to_text(&utf8(&decoded, charset))),
        Some(_) => None,
    }
}

fn is_alternative(entity: &Entity) -> bool {
    entity.fields.content_type.as_deref() == Some("multipart/alternative")
}

/// Whether the entity is a body part of type text/plain. In an
/// alternative, a part without a declared type is not counted: there the
/// declared types are what the choice goes by.
fn is_plain(entity: &Entity) -> bool {
    matches!(entity.content, Content::Single)
        && !entity.is_attachment()
        && entity.fields.content_type.as_deref() == Some("text/plain")
}

/// `bytes` in `charset` as UTF-8.
fn utf8(bytes: &[u8], charset: Option<&str>) -> String {
    let charset = charset.map(str::trim);
    if let Some(decode) = charset.and_then(|charset| charset_decoder(charset.as_bytes())) {
        return decode(bytes);
    }
    // mail-parser has no decoder for UTF-8 itself.
    let declared_utf8 = charset.is_some_and(|charset| {
        charset.eq_ignore_ascii_case("utf-8") || charset.eq_ignore_ascii_case("utf8")
    });
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) if declared_utf8 => String::from_utf8_lossy(bytes).into_owned(),
        // Mail programs that name no charset for 8-bit text wrote it in
        // the charset of their system, which for western mail was
        // windows-1252 or its subset ISO 8859-1.
        Err(_) => windows_1252(bytes),
    }
}

fn windows_1252(bytes: &[u8]) -> String {
    match charset_decoder(b"windows-1252") {
        Some(decode) => decode(bytes),
        None => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// `text` with CRLF and a bare CR as LF.
fn line_breaks(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn blank(text: &str) -> bool {
    text.chars().all(char::is_whitespace)
}

/// Appends `part` to `text`, on a new line if `text` does not end with
/// one.
fn push(text: &mut String, part: &str) {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(part);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mime;

    fn text(bytes: &[u8]) -> Option<String> {
        of(&mime::parse(bytes), bytes)
    }

    #[test]
    fn a_message_without_parts_is_its_own_text() {
        assert_eq!(
            text(b"From: a@example.org\r\nTo: b@example.org\r\n\r\nHello.\r\nBye.\r\n").as_deref(),
            Some("Hello.\nBye.\n"),
            "no declared type is text/plain, and line breaks are LF"
        );
        assert_eq!(
            text(b"From: a@example.org\r\nContent-Type: image/png\r\n\r\n\x89PNG"),
            None
        );
        assert_eq!(text(b"From: a@example.org\r\n\r\n \r\n\t\r\n"), None);
        assert_eq!(text(b"From: a@example.org\r\n"), None);
    }

    #[test]
    fn a_part_is_read_in_its_charset_and_transfer_encoding() {
        assert_eq!(
            text(
                b"Content-Type: text/plain; charset=iso-8859-1\r\n\
                Content-Transfer-Encoding: quoted-printable\r\n\
                \r\n\
                Gr=FC=DFe =\r\n\
                aus M=FCnchen\r\n"
            )
            .as_deref(),
            Some("Gr\u{fc}\u{df}e aus M\u{fc}nchen\n")
        );
        assert_eq!(
            text(b"Content-Type: text/plain; charset=windows-1252\r\n\r\n\x80 12").as_deref(),
            Some("\u{20ac} 12")
        );
        assert_eq!(
            text(b"Content-Type: text/plain; charset=UTF-8\r\n\r\nM\xc3\xbcller").as_deref(),
            Some("M\u{fc}ller")
        );
        assert_eq!(
            text(b"Content-Type: text/plain; charset=utf-8\r\n\r\nM\xc3\xbcller, M\xfcller")
                .as_deref(),
            Some("M\u{fc}ller, M\u{fffd}ller"),
            "a byte that is not UTF-8 in a part declared as UTF-8"
        );
    }

    #[test]
    fn multi_byte_charsets_are_read() {
        assert_eq!(
            text(b"Content-Type: text/plain; charset=gb2312\r\n\r\n\xd6\xd0\xce\xc4").as_deref(),
            Some("\u{4e2d}\u{6587}")
        );
        assert_eq!(
            text(b"Content-Type: text/plain; charset=big5\r\n\r\n\xa4\xa4\xa4\xe5").as_deref(),
            Some("\u{4e2d}\u{6587}")
        );
    }

    #[test]
    fn a_part_without_a_known_charset_is_utf8_or_windows_1252() {
        assert_eq!(
            text(b"Content-Type: text/plain\r\n\r\nM\xc3\xbcller").as_deref(),
            Some("M\u{fc}ller")
        );
        assert_eq!(
            text(b"Content-Type: text/plain\r\n\r\nM\xfcller \x84so\x93").as_deref(),
            Some("M\u{fc}ller \u{201e}so\u{201c}")
        );
        assert_eq!(
            text(b"From: a@example.org\r\n\r\nM\xfcller").as_deref(),
            Some("M\u{fc}ller"),
            "no Content-Type at all"
        );
        assert_eq!(
            text(b"Content-Type: text/plain; charset=x-unknown\r\n\r\nM\xc3\xbcller").as_deref(),
            Some("M\u{fc}ller")
        );
        assert_eq!(
            text(b"Content-Type: text/html; charset=x-unknown\r\n\r\n<p>M\xfcller</p>").as_deref(),
            Some("M\u{fc}ller\n")
        );
    }

    #[test]
    fn html_gives_its_text_without_the_markup() {
        let mail = "Content-Type: text/html; charset=utf-8\r\n\
            \r\n\
            <html><head><title>No</title><style>p { color: red }</style></head>\r\n\
            <body><p>Dear B&ouml;b,</p><p>see <a href=\"https://example.org/\">this\r\n\
            page</a>.<br>Alice &amp; Carol</p><script>alert(1)</script></body></html>\r\n";
        assert_eq!(
            text(mail.as_bytes()).as_deref(),
            Some("Dear B\u{f6}b,\nsee this page.\nAlice & Carol\n")
        );
    }

    #[test]
    fn an_alternative_gives_its_plain_part() {
        let mail = b"Content-Type: multipart/alternative; boundary=alt\r\n\
            \r\n\
            --alt\r\n\
            Content-Type: text/html\r\n\
            \r\n\
            <p>rich</p>\r\n\
            --alt\r\n\
            Content-Type: text/plain\r\n\
            \r\n\
            plain\r\n\
            --alt--\r\n";
        assert_eq!(text(mail).as_deref(), Some("plain"));
    }

    #[test]
    fn an_alternative_without_plain_text_gives_its_html() {
        let mail = b"Content-Type: multipart/alternative; boundary=alt\r\n\
            \r\n\
            --alt\r\n\
            Content-Type: text/plain\r\n\
            \r\n\
            \r\n\
            --alt\r\n\
            Content-Type: multipart/related; boundary=rel\r\n\
            \r\n\
            --rel\r\n\
            Content-Type: text/html\r\n\
            \r\n\
            <p>rich</p>\r\n\
            --rel\r\n\
            Content-Type: image/png\r\n\
            Content-ID: <logo@example.org>\r\n\
            \r\n\
            \x89PNG\r\n\
            --rel--\r\n\
            --alt\r\n\
            Content-Type: text/calendar\r\n\
            \r\n\
            BEGIN:VCALENDAR\r\n\
            --alt--\r\n";
        assert_eq!(
            text(mail).as_deref(),
            Some("rich\n"),
            "a blank plain part does not count, and the first alternative with text is enough"
        );
    }

    #[test]
    fn body_parts_follow_each_other_and_attachments_are_left_out() {
        let mail = b"From: a@example.org\r\n\
            Content-Type: multipart/mixed; boundary=cut\r\n\
            \r\n\
            preamble\r\n\
            --cut\r\n\
            Content-Type: text/plain\r\n\
            \r\n\
            first\r\n\
            --cut\r\n\
            Content-Type: text/plain; name=\"notes.txt\"\r\n\
            \r\n\
            attached notes\r\n\
            --cut\r\n\
            Content-Type: message/rfc822\r\n\
            \r\n\
            From: c@example.org\r\nSubject: inner\r\n\r\ninner body\r\n\
            --cut\r\n\
            Content-Type: application/pdf\r\n\
            \r\n\
            %PDF-1.4\r\n\
            --cut\r\n\
            Content-Type: text/html\r\n\
            \r\n\
            <p>second</p>\r\n\
            --cut--\r\n\
            epilogue\r\n";
        assert_eq!(text(mail).as_deref(), Some("first\nsecond\n"));
    }

    #[test]
    fn random_messages_never_fail() {
        for message in mime::random_messages(2_000) {
            text(&message);
        }
    }
}
