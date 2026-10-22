//! The mail extractor: an internet message in, its own voice out — and
//! what it carries handed over as files of their own.
//!
//! Speaks the ossuary extractor protocol (`docs/extractors.md`): called
//! with `--identify` it says who it is, that it reads `message/rfc822`,
//! `message/global` and `text/plain`, and that it derives files; called with an output
//! directory as its argument it reads one file's bytes from stdin and
//! answers with findings on stdout — the message's own words about
//! itself, verbatim under `mail:` — while the attachments and nested
//! messages it unpacks wait in the directory, each announced with the
//! kind the mail itself declared, the name the mail spelled on the
//! record as `file:name` and, as a place inside the mail, as an
//! `@`-led `file:path` — the spelling every inner place has, whatever
//! holds it.
//!
//! text/plain is read deliberately: ingest's sniff cannot tell a mail
//! from any other text, so every text file passes through here once, and
//! bytes that are no message are an examination with nothing found —
//! exit 0, no output, and the receipt keeps them from being offered
//! again. Bytes that are a message also get a sharper `file:mime` said
//! onto the record, `message/rfc822` beside the sniffed word — the set
//! holds both. An mbox — the `From ` separator line with a message
//! behind it — is a mailbox, not a message: no headers spoken, nothing
//! unpacked, another format's business — but the recognized kind goes
//! on the record the same way, `application/mbox`, so a future mailbox
//! reader finds its work waiting.
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
use mail_parser::{HeaderValue, MessageParser};
use serde_json::json;

mod mime;

/// The generation of what this extractor writes: the number in its
/// source, and so the memory of which files it has seen. Raised by hand
/// when the findings change, when the same bytes would yield more or
/// something different than before, and never for a build, a dependency
/// or a release: a new generation examines every file again, and that
/// is the only reason to have one.
const GENERATION: u32 = 1;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        Some("--identify") => {
            println!("{}", identity());
            ExitCode::SUCCESS
        }
        Some(directory) if arguments.len() == 1 && !directory.starts_with('-') => {
            examine(Path::new(directory))
        }
        _ => {
            eprintln!(
                "ossuary-extract-mail: expected --identify, or an output directory with the file on stdin"
            );
            ExitCode::FAILURE
        }
    }
}

/// The answer to `--identify`. The types include every type a message
/// extracted here can have, so `ossuary extract` examines it in turn.
fn identity() -> serde_json::Value {
    json!({
        "ossuary-extractor": 1,
        "source": format!("extractor:mail/{GENERATION}"),
        "mimes": ["message/rfc822", "message/global", "text/plain"],
        "derives": true,
    })
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
    if looks_like_mbox(bytes) {
        // A mailbox is not a message and stays shut — but the bytes have
        // told their kind, and the sharper word goes on the record.
        return Ok(vec![
            json!({ "attribute": "file:mime", "value": "application/mbox" }),
        ]);
    }
    if !looks_like_mail(bytes) {
        return Ok(Vec::new());
    }
    let root = mime::parse(bytes);
    let mut lines = Vec::new();
    // The bytes answered as a message from their first byte — a sharper
    // word than the sniff's text/plain, said onto the same set.
    lines.push(json!({ "attribute": "file:mime", "value": "message/rfc822" }));
    // mail-parser reads the header section mime.rs found, so the recorded
    // headers and the split agree on where it ends. The section starts at
    // byte 0: the offsets mail-parser reports are offsets into `bytes`.
    let headers = MessageParser::default().parse_headers(&bytes[root.header.clone()]);
    for header in headers.iter().flat_map(mail_parser::Message::headers) {
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
    for part in root.leaves() {
        let spelled = part.fields.name.as_deref();
        if spelled.is_none() && !part.is_message() {
            // A body part without a name is the mail speaking, not the
            // mail carrying: it stays inside.
            continue;
        }
        let wanted = spelled.and_then(|spelled| basename(spelled).map(|name| (name, spelled)));
        let fallback = if part.is_message() {
            // A forwarded mail is carried content even when nobody named
            // it — this extractor's own word for it, so it has a name to
            // stand under at all.
            "message.eml"
        } else {
            "attachment"
        };
        // A name too long for the filesystem, or one nothing is left of,
        // must not fail the whole mail every run; the file waits under
        // a name that fits, and the spelled name goes on the record.
        let wearable = wanted
            .as_ref()
            .map(|(name, _)| fits(name))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| fallback.to_string());
        let announced = uniquify(wearable, &mut taken);
        std::fs::write(directory.join(&announced), part.decoded(bytes)).map_err(|error| {
            std::io::Error::new(error.kind(), format!("writing {announced}: {error}"))
        })?;
        lines.push(json!({ "file": &announced, "mime": kind(part) }));
        if let Some((name, spelled)) = wanted {
            // The name the mail spelled goes on the record, whether or
            // not the announcement could wear it: the announced name is
            // a handle in the directory, and the record never learns it.
            // The name is the file's; the place is the spelled name
            // whole with `@` in front, the spelling every inner place has.
            lines.push(json!({ "file": &announced, "attribute": "file:name", "value": name }));
            lines.push(
                json!({ "file": &announced, "attribute": "file:path", "value": inner(spelled) }),
            );
        }
        if let Some(id) = &part.fields.content_id {
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

/// Whether these bytes open as an mbox: the `From ` separator line
/// first, and behind it a message by the same yardstick the mail gate
/// uses. `From ` alone is any prose's opening word — a message behind
/// it is a mailbox.
fn looks_like_mbox(bytes: &[u8]) -> bool {
    if !bytes.starts_with(b"From ") {
        return false;
    }
    bytes
        .iter()
        .position(|&byte| byte == b'\n')
        .is_some_and(|end| looks_like_mail(&bytes[end + 1..]))
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

/// The type of an extracted file: the content type the part declares. A
/// part that declares none is application/octet-stream, or message/rfc822
/// in a digest.
fn kind(part: &mime::Entity) -> String {
    match &part.fields.content_type {
        Some(declared) => declared.clone(),
        None if part.is_message() => "message/rfc822".to_string(),
        None => "application/octet-stream".to_string(),
    }
}

/// A place inside another content, spelled: a leading `@`, then the
/// name verbatim — the one spelling every inner place has.
fn inner(spelled: &str) -> String {
    format!("@{spelled}")
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

/// The most bytes an announced name may have: what a filesystem takes
/// in one name, with room left for the counter `uniquify` slips in.
const NAME_AT_MOST: usize = 240;

/// The name as a file can wear it: control characters dropped, the
/// whole cut to what a filesystem takes in one name — at a character
/// boundary, the extension kept when it is one. The spelled name goes
/// on the record whole; this is only the name the file waits under.
/// Empty when nothing wearable is left.
fn fits(spelled: &str) -> String {
    let name: String = spelled.chars().filter(|c| !c.is_control()).collect();
    if name.len() <= NAME_AT_MOST {
        return name;
    }
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if dot > 0 && name.len() - dot <= 16 => name.split_at(dot),
        _ => (name.as_str(), ""),
    };
    let mut cut = NAME_AT_MOST - extension.len();
    while !stem.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{extension}", &stem[..cut])
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
    fn a_name_the_filesystem_would_refuse_is_cut_to_fit_and_spelled_whole_on_the_record() {
        let long = format!("{}.pdf", "ä".repeat(200));
        let fitting = fits(&long);
        assert!(fitting.len() <= NAME_AT_MOST);
        assert_eq!(
            &fitting[fitting.len() - 4..],
            ".pdf",
            "the extension is kept"
        );
        assert!(fitting.starts_with("ääää"), "cut at a character boundary");
        assert_eq!(fits("a\u{0}b\tc.txt"), "abc.txt", "control characters drop");
        assert_eq!(fits(""), "");

        let dir = TempDir::new().unwrap();
        let mail = format!(
            concat!(
                "From: alice@example.com\r\n",
                "To: bob@example.org\r\n",
                "MIME-Version: 1.0\r\n",
                "Content-Type: multipart/mixed; boundary=\"cut\"\r\n",
                "\r\n",
                "--cut\r\n",
                "Content-Type: application/pdf\r\n",
                "Content-Disposition: attachment; filename=\"{long}\"\r\n",
                "\r\n",
                "%PDF-1.4\r\n",
                "--cut--\r\n",
            ),
            long = "x".repeat(300)
        );

        let lines = harvest(mail.as_bytes(), dir.path()).unwrap();

        let announced = "x".repeat(NAME_AT_MOST);
        assert!(
            lines.contains(&json!({ "file": &announced, "mime": "application/pdf" })),
            "the file waits under a name that fits; got {lines:#?}"
        );
        assert!(lines.contains(&json!({
            "file": &announced,
            "attribute": "file:name",
            "value": "x".repeat(300),
        })));
        assert!(lines.contains(&json!({
            "file": &announced,
            "attribute": "file:path",
            "value": format!("@{}", "x".repeat(300)),
        })));
        assert!(dir.path().join(&announced).is_file());
    }

    #[test]
    fn an_mbox_is_told_by_the_message_behind_its_separator() {
        assert!(looks_like_mbox(
            b"From alice@example.com Thu Sep  4 12:34:56 2026\nFrom: alice\nTo: bob\n\nbody\n"
        ));
        assert!(
            !looks_like_mbox(b"From here on, everything changed.\nIt was a quiet morning.\n"),
            "prose opening with the word From declares nothing"
        );
        assert!(!looks_like_mbox(b"From "));
        assert!(
            !looks_like_mbox(b"From: alice\nTo: bob\n\nbody\n"),
            "a message is no mailbox"
        );
    }

    #[test]
    fn a_recognized_mbox_gains_its_kind_and_stays_shut() {
        let dir = TempDir::new().unwrap();
        let mbox = concat!(
            "From alice@example.com Thu Sep  4 12:34:56 2026\n",
            "From: alice@example.com\n",
            "To: bob@example.org\n",
            "Subject: first of many\n",
            "\n",
            "body\n",
        );

        let lines = harvest(mbox.as_bytes(), dir.path()).unwrap();

        assert_eq!(
            lines,
            vec![json!({ "attribute": "file:mime", "value": "application/mbox" })],
            "the kind alone — no headers spoken, nothing unpacked"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
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
        assert!(lines.contains(&json!({
            "file": "invoice.pdf",
            "attribute": "file:name",
            "value": "invoice.pdf",
        })));
        assert!(
            lines.contains(&json!({
                "file": "invoice.pdf",
                "attribute": "file:path",
                "value": "@invoice.pdf",
            })),
            "the attachment's place inside the mail, spelled the way every inner place is; got {lines:#?}"
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
    fn text_attachments_keep_their_charset_and_bytes() {
        let dir = TempDir::new().unwrap();
        let mail = b"From: alice@example.com\r\n\
            To: bob@example.org\r\n\
            MIME-Version: 1.0\r\n\
            Content-Type: multipart/mixed; boundary=\"cut\"\r\n\
            \r\n\
            --cut\r\n\
            Content-Type: text/plain\r\n\
            \r\n\
            Body.\r\n\
            --cut\r\n\
            Content-Type: text/csv; charset=windows-1252\r\n\
            Content-Disposition: attachment; filename=\"list.csv\"\r\n\
            Content-Transfer-Encoding: 8bit\r\n\
            \r\n\
            M\xfcller;\x80 12\r\n\
            --cut\r\n\
            Content-Type: text/plain\r\n\
            Content-Disposition: attachment; filename=\"raw.txt\"\r\n\
            Content-Transfer-Encoding: 8bit\r\n\
            \r\n\
            G\xfcnter\r\n\
            --cut\r\n\
            Content-Type: text/plain; charset=iso-8859-1\r\n\
            Content-Disposition: attachment; filename=\"qp.txt\"\r\n\
            Content-Transfer-Encoding: quoted-printable\r\n\
            \r\n\
            Gr=FC=DFe =\r\n\
            aus M=FCnchen\r\n\
            last line   \r\n\
            --cut\r\n\
            Content-Type: text/html; charset=iso-8859-1\r\n\
            Content-Disposition: attachment; filename=\"page.html\"\r\n\
            Content-Transfer-Encoding: base64\r\n\
            \r\n\
            PHA+5DwvcD4NCg==\r\n\
            --cut--\r\n";

        harvest(mail, dir.path()).unwrap();

        let read = |name: &str| std::fs::read(dir.path().join(name)).unwrap();
        assert_eq!(
            read("list.csv"),
            b"M\xfcller;\x80 12",
            "windows-1252, not UTF-8"
        );
        assert_eq!(read("raw.txt"), b"G\xfcnter", "no charset, no U+FFFD");
        assert_eq!(
            read("qp.txt"),
            b"Gr\xfc\xdfe aus M\xfcnchen\r\nlast line",
            "quoted-printable decoded, trailing whitespace of the last line dropped"
        );
        assert_eq!(read("page.html"), b"<p>\xe4</p>\r\n");
    }

    #[test]
    fn a_nested_message_is_written_byte_for_byte() {
        let dir = TempDir::new().unwrap();
        // Python's email package rewrites these headers when it writes a
        // parsed message back: the 8-bit Subject, the Message-ID without its
        // closing bracket, the folded ARC-Seal.
        let inner = b"From: carol@example.org\r\n\
            To: alice@example.com\r\n\
            Subject: Gr\xfc\xdfe aus M\xfcnchen\r\n\
            Message-ID: <no closing bracket@example.org\r\n\
            ARC-Seal: i=1; a=rsa-sha256; d=example.org; cv=none;\r\n b=abc+def/ghi==\r\n\
            Date: Wed, 3 Sep 2026 09:00:00 +0200\r\n\
            \r\n\
            The original.  \r\n";
        let mail = [
            &b"From: journal@example.com\r\n\
                To: archive@example.org\r\n\
                MIME-Version: 1.0\r\n\
                Content-Type: multipart/mixed; boundary=\"cut\"\r\n\
                \r\n\
                --cut\r\n\
                Content-Type: text/plain\r\n\
                \r\n\
                Report.\r\n\
                --cut\r\n\
                Content-Type: message/rfc822\r\n\
                \r\n"[..],
            inner,
            b"\r\n--cut--\r\n",
        ]
        .concat();

        harvest(&mail, dir.path()).unwrap();

        assert_eq!(
            std::fs::read(dir.path().join("message.eml")).unwrap(),
            inner,
            "everything between the part's header and the line break before the delimiter"
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
        assert!(
            !lines.iter().any(|line| {
                line["file"] == "message.eml"
                    && (line["attribute"] == "file:name" || line["attribute"] == "file:path")
            }),
            "a name nobody spelled is this extractor's handle, not the mail's word; got {lines:#?}"
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
        assert!(lines.contains(&json!({
            "file": "image.png",
            "attribute": "file:name",
            "value": "image.png",
        })));
        assert!(lines.contains(&json!({ "file": "image-2.png", "mime": "image/png" })));
        assert!(
            lines.contains(&json!({
                "file": "image-2.png",
                "attribute": "file:name",
                "value": "image.png",
            })),
            "the name the mail spelled stands beside the one that had to yield; got {lines:#?}"
        );
        for announced in ["image.png", "image-2.png"] {
            assert!(
                lines.contains(&json!({
                    "file": announced,
                    "attribute": "file:path",
                    "value": "@image.png",
                })),
                "both had the same place in the mail; got {lines:#?}"
            );
        }
    }

    #[test]
    fn a_spelled_path_is_the_place_and_its_last_element_the_name() {
        let dir = TempDir::new().unwrap();
        let mail = concat!(
            "From: alice@example.com\r\n",
            "To: bob@example.org\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=\"cut\"\r\n",
            "\r\n",
            "--cut\r\n",
            "Content-Type: application/pdf\r\n",
            "Content-Disposition: attachment; filename=\"Belege/Rechnung.pdf\"\r\n",
            "\r\n",
            "%PDF-1.4\r\n",
            "--cut\r\n",
            "Content-Type: application/pdf\r\n",
            "Content-Disposition: attachment; filename=\"reports/\"\r\n",
            "\r\n",
            "%PDF-1.4\r\n",
            "--cut--\r\n",
        );

        let lines = harvest(mail.as_bytes(), dir.path()).unwrap();

        assert!(lines.contains(&json!({
            "file": "Rechnung.pdf",
            "attribute": "file:name",
            "value": "Rechnung.pdf",
        })));
        assert!(
            lines.contains(&json!({
                "file": "Rechnung.pdf",
                "attribute": "file:path",
                "value": "@Belege/Rechnung.pdf",
            })),
            "the place keeps the spelling whole; got {lines:#?}"
        );
        assert!(
            lines.contains(&json!({ "file": "attachment", "mime": "application/pdf" }))
                && !lines
                    .iter()
                    .any(|line| line["file"] == "attachment" && line["attribute"] == "file:path"),
            "a spelling with no name in it names no place either; got {lines:#?}"
        );
    }

    /// The files `harvest` writes for `bytes`, by name.
    fn extracted(bytes: &[u8]) -> std::collections::BTreeMap<String, Vec<u8>> {
        let dir = TempDir::new().unwrap();
        harvest(bytes, dir.path()).unwrap();
        std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let name = entry.file_name().into_string().unwrap();
                (name, std::fs::read(entry.path()).unwrap())
            })
            .collect()
    }

    #[test]
    fn a_boundary_in_the_middle_of_a_line_is_content_on_every_level() {
        let u = b"first\nsee --b-b here, and --a-b, and --env-b\nlast";
        let b = [
            &b"From: b@example.org\n\
                Subject: B\n\
                Content-Type: multipart/mixed; boundary=\"b-b\"\n\
                \n\
                --b-b\n\
                Content-Type: text/plain\n\
                \n\
                body of B\n\
                --b-b\n\
                Content-Type: text/plain; name=\"u.txt\"\n\
                \n"[..],
            u,
            b"\n--b-b--\n",
        ]
        .concat();
        let a = [
            &b"From: a@example.org\n\
                Subject: A\n\
                Content-Type: multipart/mixed; boundary=\"a-b\"\n\
                \n\
                --a-b\n\
                Content-Type: text/plain\n\
                \n\
                body of A\n\
                --a-b\n\
                Content-Type: message/rfc822; name=\"b.eml\"\n\
                \n"[..],
            &b,
            b"\n--a-b--\n",
        ]
        .concat();
        let envelope = [
            &b"From: journal@example.org\n\
                To: archive@example.org\n\
                Content-Type: multipart/mixed; boundary=\"env-b\"\n\
                \n\
                --env-b\n\
                Content-Type: text/plain\n\
                \n\
                report\n\
                --env-b\n\
                Content-Type: message/rfc822\n\
                \n"[..],
            &a,
            b"\n--env-b--\n",
        ]
        .concat();

        assert_eq!(extracted(&envelope)["message.eml"], a);
        assert_eq!(extracted(&a)["b.eml"], b);
        assert_eq!(extracted(&b)["u.txt"], u);
    }

    #[test]
    fn a_malformed_attached_message_keeps_to_its_part() {
        let unclosed = b"From: a@example.org\n\
            Content-Type: multipart/mixed; boundary=\"inner\"\n\
            \n\
            --inner\n\
            Content-Type: text/plain\n\
            \n\
            never closed";
        let mail = [
            &b"From: journal@example.org\n\
                To: archive@example.org\n\
                Content-Type: multipart/mixed; boundary=\"outer\"\n\
                \n\
                --outer\n\
                Content-Type: message/rfc822\n\
                \n"[..],
            unclosed,
            b"\n--outer\n\
                Content-Type: application/pdf\n\
                Content-Disposition: attachment; filename=\"after.pdf\"\n\
                \n\
                %PDF-1.4\n\
                --outer\n\
                Content-Type: message/rfc822\n\
                \n\
                From: c@example.org\n\
                Subject: headers only\n\
                --outer\n\
                Content-Type: message/rfc822; name=\"empty.eml\"\n\
                \n\
                \n\
                --outer--\n",
        ]
        .concat();

        let files = extracted(&mail);

        assert_eq!(files["message.eml"], unclosed);
        assert_eq!(files["after.pdf"], b"%PDF-1.4");
        assert_eq!(
            files["message-2.eml"],
            b"From: c@example.org\nSubject: headers only"
        );
        assert_eq!(files["empty.eml"], b"");
        assert_eq!(files.len(), 4);
    }

    #[test]
    fn random_messages_never_fail_the_harvest() {
        for message in mime::random_messages(2_000) {
            let bytes = [
                &b"From: a@example.org\r\nTo: b@example.org\r\n"[..],
                &message,
            ]
            .concat();
            let dir = TempDir::new().unwrap();
            harvest(&bytes, dir.path()).unwrap();
        }
    }

    #[test]
    fn every_message_written_is_a_type_this_extractor_reads() {
        let mail = b"From: a@example.org\r\n\
            To: b@example.org\r\n\
            Content-Type: multipart/mixed; boundary=cut\r\n\
            \r\n\
            --cut\r\n\
            Content-Type: message/rfc822\r\n\
            \r\n\
            From: c@example.org\r\nSubject: one\r\n\r\nbody\r\n\
            --cut\r\n\
            Content-Type: message/global\r\n\
            Content-Transfer-Encoding: quoted-printable\r\n\
            \r\n\
            From: d@example.org\r\nSubject: =C3=BCber\r\n\r\nbody\r\n\
            --cut--\r\n";
        let dir = TempDir::new().unwrap();
        let lines = harvest(mail, dir.path()).unwrap();
        let read = identity()["mimes"].as_array().unwrap().clone();

        let written: Vec<_> = lines.iter().filter_map(|line| line.get("mime")).collect();
        assert_eq!(written, ["message/rfc822", "message/global"]);
        for mime in written {
            assert!(read.contains(mime), "{mime} is written but not read");
        }
        assert_eq!(
            std::fs::read(dir.path().join("message-2.eml")).unwrap(),
            "From: d@example.org\r\nSubject: \u{fc}ber\r\n\r\nbody".as_bytes()
        );
    }
}
