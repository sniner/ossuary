//! The mail extractor: an internet message in, its own voice out — and
//! what it carries handed over as files of their own.
//!
//! Speaks the ossuary extractor protocol (`docs/extractors.md`): called
//! with `--identify` it says who it is, that it reads `message/rfc822`
//! and `text/plain`, and that it derives files; called with an output
//! directory as its argument it reads one file's bytes from stdin and
//! answers with findings on stdout — the message's own words about
//! itself, verbatim under `mail:` — while the attachments and nested
//! messages it unpacks wait in the directory, each announced with the
//! kind the mail itself declared.
//!
//! text/plain is read deliberately: ingest's sniff cannot tell a mail
//! from any other text, so every text file passes through here once, and
//! bytes that are no message are an examination with nothing found —
//! exit 0, no output, and the receipt keeps them from being offered
//! again. Bytes that are a message also get a sharper `file:mime` said
//! onto the record, `message/rfc822` beside the sniffed word — the set
//! holds both. An mbox — bytes opening with the `From ` separator
//! line — is a mailbox, not a message: nothing found, another format's
//! business.
//!
//! Header values are unfolded and their RFC 2047 encoded words decoded —
//! conversion, not tidying, the way the text extractor reads a PDF's
//! UTF-16 strings — and otherwise stand as the mail spells them: the
//! date keeps its own calendar, addresses their display names, commas
//! and angle brackets. A raw byte no charset accounts for reads as
//! U+FFFD rather than silencing its whole header. Only failing to read
//! stdin or to write a carried file is a failure.

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

use mail_parser::parsers::MessageStream;
use mail_parser::{HeaderValue, MessageParser, MessagePart, MimeHeaders as _};
use serde_json::json;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        Some("--identify") => {
            println!(
                "{}",
                json!({
                    "ossuary-extractor": 1,
                    "source": format!("extractor:mail/{}", env!("CARGO_PKG_VERSION")),
                    "mimes": ["message/rfc822", "text/plain"],
                    "derives": true,
                })
            );
            ExitCode::SUCCESS
        }
        Some(directory) if arguments.len() == 1 && !directory.starts_with('-') => {
            examine(Path::new(directory))
        }
        _ => {
            eprintln!(
                "ossuary-extract-mail: run with --identify, or with the output directory as the only argument and a file's bytes on stdin"
            );
            ExitCode::FAILURE
        }
    }
}

/// One file: stdin to its end, then the harvest — carried files into the
/// directory, findings onto stdout.
fn examine(directory: &Path) -> ExitCode {
    let mut bytes = Vec::new();
    if let Err(error) = std::io::stdin().lock().read_to_end(&mut bytes) {
        eprintln!("ossuary-extract-mail: reading stdin: {error}");
        return ExitCode::FAILURE;
    }
    match harvest(&bytes, directory) {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("ossuary-extract-mail: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The whole answer for one file: nothing when the bytes are no message,
/// otherwise the sharper kind, the message's own headers, and one
/// announcement per carried file — written into the directory on the
/// way. The protocol reads the answer whole, so the order of lines is
/// convenience, not contract.
fn harvest(bytes: &[u8], directory: &Path) -> std::io::Result<Vec<serde_json::Value>> {
    if !looks_like_mail(bytes) {
        return Ok(Vec::new());
    }
    let Some(message) = MessageParser::default().parse(bytes) else {
        return Ok(Vec::new());
    };
    let mut lines = Vec::new();
    // The bytes answered as a message from their first byte — a sharper
    // word than the sniff's text/plain, said onto the same set.
    lines.push(json!({ "attribute": "file:mime", "value": "message/rfc822" }));
    for header in message.headers() {
        let Some(name) = spoken(header.name.as_str()) else {
            continue;
        };
        let raw = bytes
            .get(header.offset_start as usize..header.offset_end as usize)
            .unwrap_or_default();
        if let Some(value) = unfold(raw) {
            lines.push(json!({ "attribute": format!("mail:{name}"), "value": value }));
        }
    }
    let mut taken = HashSet::new();
    for part in &message.parts {
        if part.is_multipart() {
            continue;
        }
        let spelled = part.attachment_name();
        if spelled.is_none() && !part.is_message() {
            // A body part without a name is the mail speaking, not the
            // mail carrying: it stays inside.
            continue;
        }
        let wanted = spelled.and_then(basename);
        let fallback = if part.is_message() {
            // A forwarded mail is carried content even when nobody named
            // it — this extractor's own word for it, so it has a name to
            // stand under at all.
            "message.eml"
        } else {
            "attachment"
        };
        let announced = uniquify(
            wanted.clone().unwrap_or_else(|| fallback.to_string()),
            &mut taken,
        );
        std::fs::write(directory.join(&announced), part.contents()).map_err(|error| {
            std::io::Error::new(error.kind(), format!("writing {announced}: {error}"))
        })?;
        lines.push(json!({ "file": &announced, "mime": kind(part) }));
        if let Some(name) = wanted.filter(|name| *name != announced) {
            // The announcement had to yield to a name already taken; the
            // name the mail spelled goes on the record beside it.
            lines.push(json!({ "file": &announced, "attribute": "file:name", "value": name }));
        }
        if let Some(id) = part.content_id() {
            lines.push(json!({
                "file": &announced,
                "attribute": "mail:content-id",
                "value": format!("<{id}>"),
            }));
        }
    }
    Ok(lines)
}

/// The headers this extractor repeats: the message's own voice — who
/// speaks, to whom, about what, when, and the message-id family that
/// threads it. The transport's trail — received, return-path, the x-
/// families — describes the journey, not the message, and stays untold.
/// Answers the attribute spelling for a header that is spoken.
fn spoken(name: &str) -> Option<&'static str> {
    [
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
    ]
    .into_iter()
    .find(|known| known.eq_ignore_ascii_case(name))
}

/// Header names that only a mail carries — the gate's yardstick. The
/// spoken headers plus the transport's own: a bounce with nothing but
/// received and return-path is still a mail.
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

/// Whether these bytes read as an internet message from their first
/// byte: a header section of well-formed fields — names in RFC 5322's
/// own alphabet, folds only after a field — holding at least two
/// distinct names only mail uses. One is not enough: any prose may
/// mention `Date:` at the start of a line; two of them, from byte zero,
/// is a mail. Bytes opening with mbox's `From ` line are a mailbox
/// holding messages, not a message.
fn looks_like_mail(bytes: &[u8]) -> bool {
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

/// One header's raw value in the claim's spelling: unfolded — the
/// transport's line breaks read as the single space they mean — and its
/// RFC 2047 encoded words decoded, in whatever charset each word names.
/// Everything else stands as the mail spells it. A header with nothing
/// left to say is no finding.
fn unfold(raw: &[u8]) -> Option<String> {
    let mut data = raw.to_vec();
    if data.last() != Some(&b'\n') {
        data.push(b'\n');
    }
    match MessageStream::new(&data).parse_unstructured() {
        HeaderValue::Text(text) if !text.trim().is_empty() => Some(text.into_owned()),
        _ => None,
    }
}

/// A carried file's kind, in the mail's own words: the part's declared
/// content type, lowercased to MIME's canonical case. A part that
/// declares none gets the honest shrug — except a nested message, whose
/// nature needs no declaring.
fn kind(part: &MessagePart) -> String {
    if let Some(content_type) = part.content_type()
        && let Some(subtype) = &content_type.c_subtype
    {
        return format!(
            "{}/{}",
            content_type.c_type.to_ascii_lowercase(),
            subtype.to_ascii_lowercase()
        );
    }
    if part.is_message() {
        "message/rfc822".to_string()
    } else {
        "application/octet-stream".to_string()
    }
}

/// The bare file name inside a spelled attachment name: the last element
/// past either separator, because an announced name names a file, never
/// a place. A spelling with no name left in it answers nothing.
fn basename(spelled: &str) -> Option<String> {
    let name = spelled
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .trim();
    (!name.is_empty() && name != "." && name != "..").then(|| name.to_string())
}

/// The wanted name, or the nearest free one: a counter slips in before
/// the extension until nothing collides — case-insensitively, because
/// the directory the files wait in may not tell Report from report.
fn uniquify(wanted: String, taken: &mut HashSet<String>) -> String {
    if taken.insert(wanted.to_lowercase()) {
        return wanted;
    }
    let (stem, extension) = match wanted.rfind('.') {
        Some(dot) if dot > 0 => wanted.split_at(dot),
        _ => (wanted.as_str(), ""),
    };
    let mut count = 2usize;
    loop {
        let attempt = format!("{stem}-{count}{extension}");
        if taken.insert(attempt.to_lowercase()) {
            return attempt;
        }
        count += 1;
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn a_mail_is_told_from_prose_by_its_header_section() {
        assert!(looks_like_mail(
            b"From: a@example.com\r\nDate: Thu, 4 Sep 2026 12:34:56 +0200\r\n\r\nbody"
        ));
        assert!(
            looks_like_mail(b"Received: by mx.example.com\nReturn-Path: <a@example.com>\n"),
            "a bounce's transport headers are mail enough, bare linefeeds and a missing body too"
        );
        assert!(!looks_like_mail(b"# README\n\nDate: whenever you like"));
        assert!(!looks_like_mail(b""));
        assert!(
            !looks_like_mail(b"Date: Thu, 4 Sep 2026 12:34:56 +0200\nContent-Length: 3\n\nabc"),
            "one mailly name is any protocol's header block"
        );
        assert!(
            !looks_like_mail(
                b"From alice@example.com Thu Sep  4 12:34:56 2026\nFrom: alice\nTo: bob\n\n"
            ),
            "an mbox is a mailbox, not a message"
        );
        assert!(
            !looks_like_mail(b" folded\nFrom: a@example.com\nTo: b@example.com\n\n"),
            "a fold before any field is no header section"
        );
    }

    #[test]
    fn only_the_messages_own_voice_is_spoken() {
        assert_eq!(spoken("Subject"), Some("subject"));
        assert_eq!(spoken("MESSAGE-ID"), Some("message-id"));
        assert_eq!(spoken("Received"), None);
        assert_eq!(spoken("X-Spam-Status"), None);
        assert_eq!(spoken("DKIM-Signature"), None);
    }

    #[test]
    fn header_values_unfold_and_decode_but_stay_spelled() {
        assert_eq!(
            unfold(b" Thu, 4 Sep 2026 12:34:56 +0200\r\n"),
            Some("Thu, 4 Sep 2026 12:34:56 +0200".to_string()),
            "the date keeps its own calendar"
        );
        assert_eq!(
            unfold(b" alice <a@example.com>,\r\n bob <b@example.com>\r\n"),
            Some("alice <a@example.com>, bob <b@example.com>".to_string()),
            "a fold reads as the single space it means"
        );
        assert_eq!(
            unfold(b" =?ISO-8859-1?Q?Gr=FC=DFe?= aus dem Test\r\n"),
            Some("Gr\u{fc}\u{df}e aus dem Test".to_string()),
            "an encoded word is spelling, not content"
        );
        assert_eq!(
            unfold(b"   \r\n"),
            None,
            "nothing left to say is no finding"
        );
    }

    #[test]
    fn names_shed_their_paths_and_yield_to_the_taken() {
        assert_eq!(basename("invoice.pdf"), Some("invoice.pdf".to_string()));
        assert_eq!(basename("reports/q3.pdf"), Some("q3.pdf".to_string()));
        assert_eq!(basename("C:\\docs\\q3.pdf"), Some("q3.pdf".to_string()));
        assert_eq!(basename(".."), None);
        assert_eq!(basename("reports/"), None);

        let mut taken = HashSet::new();
        assert_eq!(uniquify("image.png".to_string(), &mut taken), "image.png");
        assert_eq!(
            uniquify("Image.png".to_string(), &mut taken),
            "Image-2.png",
            "the directory may not tell Image from image"
        );
        assert_eq!(uniquify("image.png".to_string(), &mut taken), "image-3.png");
        assert_eq!(uniquify("noext".to_string(), &mut taken), "noext");
        assert_eq!(uniquify("noext".to_string(), &mut taken), "noext-2");
    }

    #[test]
    fn bytes_without_a_message_are_an_empty_answer_not_an_error() {
        let dir = TempDir::new().unwrap();
        assert_eq!(
            harvest(b"plain words", dir.path()).unwrap(),
            Vec::<serde_json::Value>::new()
        );
        assert_eq!(
            harvest(b"", dir.path()).unwrap(),
            Vec::<serde_json::Value>::new()
        );
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "and nothing lands in the directory"
        );
    }

    #[test]
    fn the_message_speaks_verbatim_under_mail() {
        let dir = TempDir::new().unwrap();
        let mail = concat!(
            "Received: from mx.example.com by mail.example.org\r\n",
            "From: Alice Adams <alice@example.com>\r\n",
            "To: bob <bob@example.org>,\r\n",
            " carol <carol@example.org>\r\n",
            "Subject: =?ISO-8859-1?Q?Gr=FC=DFe?=\r\n",
            "Date: Thu, 4 Sep 2026 12:34:56 +0200\r\n",
            "Message-ID: <one@example.com>\r\n",
            "\r\n",
            "Hello.\r\n",
        );

        let lines = harvest(mail.as_bytes(), dir.path()).unwrap();

        let expect = |line: serde_json::Value| {
            assert!(lines.contains(&line), "missing {line}; got {lines:#?}");
        };
        expect(json!({ "attribute": "file:mime", "value": "message/rfc822" }));
        expect(json!({ "attribute": "mail:from", "value": "Alice Adams <alice@example.com>" }));
        expect(json!({
            "attribute": "mail:to",
            "value": "bob <bob@example.org>, carol <carol@example.org>",
        }));
        expect(json!({ "attribute": "mail:subject", "value": "Gr\u{fc}\u{df}e" }));
        expect(json!({ "attribute": "mail:date", "value": "Thu, 4 Sep 2026 12:34:56 +0200" }));
        expect(json!({ "attribute": "mail:message-id", "value": "<one@example.com>" }));
        assert!(
            !lines
                .iter()
                .any(|line| line["attribute"] == "mail:received"),
            "the transport's trail stays untold; got {lines:#?}"
        );
        assert!(
            !lines.iter().any(|line| line.get("file").is_some()),
            "a bare body is the mail speaking, not carrying; got {lines:#?}"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn carried_files_come_out_as_themselves() {
        let dir = TempDir::new().unwrap();
        let mail = concat!(
            "From: alice@example.com\r\n",
            "To: bob@example.org\r\n",
            "Subject: the report\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=\"cut\"\r\n",
            "\r\n",
            "--cut\r\n",
            "Content-Type: text/plain\r\n",
            "\r\n",
            "See attached.\r\n",
            "--cut\r\n",
            "Content-Type: application/pdf; name=\"invoice.pdf\"\r\n",
            "Content-Disposition: attachment; filename=\"invoice.pdf\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "Content-ID: <part2@example.com>\r\n",
            "\r\n",
            "JVBERi0xLjQgdGVzdA==\r\n",
            "--cut--\r\n",
        );

        let lines = harvest(mail.as_bytes(), dir.path()).unwrap();

        assert!(
            lines.contains(&json!({ "file": "invoice.pdf", "mime": "application/pdf" })),
            "got {lines:#?}"
        );
        assert!(
            lines.contains(&json!({
                "file": "invoice.pdf",
                "attribute": "mail:content-id",
                "value": "<part2@example.com>",
            })),
            "the part's identity stands on the part; got {lines:#?}"
        );
        assert_eq!(
            std::fs::read(dir.path().join("invoice.pdf")).unwrap(),
            b"%PDF-1.4 test",
            "the attachment's true bytes, transfer encoding undone"
        );
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "the unnamed body stays inside the mail"
        );
    }

    #[test]
    fn a_forwarded_mail_is_carried_content_even_unnamed() {
        let dir = TempDir::new().unwrap();
        let mail = concat!(
            "From: alice@example.com\r\n",
            "To: bob@example.org\r\n",
            "Subject: Fwd: hello\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=\"cut\"\r\n",
            "\r\n",
            "--cut\r\n",
            "Content-Type: text/plain\r\n",
            "\r\n",
            "Look at this.\r\n",
            "--cut\r\n",
            "Content-Type: message/rfc822\r\n",
            "\r\n",
            "From: carol@example.org\r\n",
            "To: alice@example.com\r\n",
            "Subject: hello\r\n",
            "Date: Wed, 3 Sep 2026 09:00:00 +0200\r\n",
            "\r\n",
            "The original.\r\n",
            "--cut--\r\n",
        );

        let lines = harvest(mail.as_bytes(), dir.path()).unwrap();

        assert!(
            lines.contains(&json!({ "file": "message.eml", "mime": "message/rfc822" })),
            "got {lines:#?}"
        );
        let carried = std::fs::read_to_string(dir.path().join("message.eml")).unwrap();
        assert!(
            carried.starts_with("From: carol@example.org"),
            "the nested message's own bytes, headers and all; got {carried:?}"
        );
        assert!(
            harvest(carried.as_bytes(), dir.path()).unwrap().len() > 1,
            "and what came out reads as a mail in its own right"
        );
    }

    #[test]
    fn colliding_names_yield_and_the_spelled_name_goes_on_the_record() {
        let dir = TempDir::new().unwrap();
        let mail = concat!(
            "From: alice@example.com\r\n",
            "To: bob@example.org\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=\"cut\"\r\n",
            "\r\n",
            "--cut\r\n",
            "Content-Type: image/png; name=\"image.png\"\r\n",
            "Content-Disposition: attachment; filename=\"image.png\"\r\n",
            "\r\n",
            "first\r\n",
            "--cut\r\n",
            "Content-Type: image/png; name=\"image.png\"\r\n",
            "Content-Disposition: attachment; filename=\"image.png\"\r\n",
            "\r\n",
            "second\r\n",
            "--cut--\r\n",
        );

        let lines = harvest(mail.as_bytes(), dir.path()).unwrap();

        assert!(lines.contains(&json!({ "file": "image.png", "mime": "image/png" })));
        assert!(lines.contains(&json!({ "file": "image-2.png", "mime": "image/png" })));
        assert!(
            lines.contains(&json!({
                "file": "image-2.png",
                "attribute": "file:name",
                "value": "image.png",
            })),
            "the name the mail spelled stands beside the one that had to yield; got {lines:#?}"
        );
    }
}
