//! The mail extractor: three contracts in one program.
//!
//! It implements the ossuary extractor protocol (`docs/extractors.md`).
//! `--identify` prints one line for each contract:
//!
//! - `headers` records the message's headers as `mail:` attributes, in a
//!   form that can be searched (see `header.rs`).
//! - `attachments` writes every attachment and every nested message into
//!   the output directory, each with the type the message declares for it,
//!   its name as `file:name` and its place in the message as an `@`-led
//!   `file:path`.
//! - `text` writes the readable text of the message into the output
//!   directory as `text.txt` (see `text.rs`).
//!
//! Called with a contract's name, and for `attachments` and `text` an
//! output directory, it reads one file from stdin and prints the findings
//! of that contract.
//!
//! Only `headers` reads text/plain. Ingest cannot tell a mail from other
//! text, so every text file is examined once by `headers`. For a message
//! it records `file:mime = message/rfc822` in addition to the detected
//! type, and with that type the file is offered to the other two
//! contracts. Bytes that are no message are an examination with nothing
//! found: exit 0 and no output, under every contract. An mbox (a `From `
//! separator line followed by a message) is not opened; `headers` records
//! its type as `application/mbox`.
//!
//! A header value that cannot be read in the searchable form is not
//! recorded and is reported on stderr, one line for each. Only failing
//! to read stdin or to write a file is a failure.

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

use mail_parser::MessageParser;
use serde_json::json;

mod date;
mod header;
mod mime;
mod text;

/// The generation of what each contract writes: the number in its
/// source, which decides whether a file has been examined. Raised by hand
/// when the findings change, when the same bytes would yield more or
/// something different than before, and never for a build, a dependency
/// or a release: a new generation examines every file again. Each
/// contract has its own number, so a change to one does not examine the
/// files again for the others.
const HEADERS_GENERATION: u32 = 1;
const ATTACHMENTS_GENERATION: u32 = 1;
const TEXT_GENERATION: u32 = 1;

/// The types of a message. An attached message has one of them, so the
/// files the `attachments` contract writes are examined in turn.
const MESSAGE_TYPES: [&str; 2] = ["message/rfc822", "message/global"];

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    match arguments.as_slice() {
        ["--identify"] => {
            for line in identities() {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        ["headers"] => examine(|bytes| Ok(headers(bytes))),
        ["attachments", directory] if !directory.starts_with('-') => {
            examine(|bytes| attachments(bytes, Path::new(directory)).map(Harvest::of))
        }
        ["text", directory] if !directory.starts_with('-') => {
            examine(|bytes| text(bytes, Path::new(directory)).map(Harvest::of))
        }
        _ => {
            eprintln!(
                "ossuary-extract-mail: expected --identify, or one of `headers`, `attachments DIR`, `text DIR` with the file on stdin"
            );
            ExitCode::FAILURE
        }
    }
}

/// The answer to `--identify`, one line for each contract. `headers`
/// comes first: `ossuary extract` runs the contracts in this order, and
/// the other two read a text file only after `headers` has recorded that
/// it is a message.
fn identities() -> [serde_json::Value; 3] {
    [
        json!({
            "ossuary-extractor": 1,
            "contract": "headers",
            "source": format!("extractor:mail-headers/{HEADERS_GENERATION}"),
            "mimes": ["message/rfc822", "message/global", "text/plain"],
        }),
        json!({
            "ossuary-extractor": 1,
            "contract": "attachments",
            "source": format!("extractor:mail-attachments/{ATTACHMENTS_GENERATION}"),
            "mimes": MESSAGE_TYPES,
            "derives": true,
        }),
        json!({
            "ossuary-extractor": 1,
            "contract": "text",
            "source": format!("extractor:mail-text/{TEXT_GENERATION}"),
            "mimes": MESSAGE_TYPES,
            "derives": true,
        }),
    ]
}

/// One file under one contract: stdin to its end, then the lines of
/// `contract` onto stdout and its remarks onto stderr.
fn examine(contract: impl FnOnce(&[u8]) -> std::io::Result<Harvest>) -> ExitCode {
    let mut bytes = Vec::new();
    if let Err(error) = std::io::stdin().lock().read_to_end(&mut bytes) {
        eprintln!("ossuary-extract-mail: reading stdin: {error}");
        return ExitCode::FAILURE;
    }
    match contract(&bytes) {
        Ok(harvest) => {
            for line in harvest.lines {
                println!("{line}");
            }
            for remark in harvest.remarks {
                eprintln!("ossuary-extract-mail: {remark}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("ossuary-extract-mail: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The result for one file: the lines for stdout, and the remarks for
/// stderr about values that could not be read.
#[derive(Debug, Default)]
struct Harvest {
    lines: Vec<serde_json::Value>,
    remarks: Vec<String>,
}

impl Harvest {
    /// A result without remarks.
    fn of(lines: Vec<serde_json::Value>) -> Harvest {
        Harvest {
            lines,
            remarks: Vec::new(),
        }
    }
}

/// The `headers` contract: nothing when the bytes are no message,
/// otherwise `file:mime` and the message's own headers. For an mbox, its
/// `file:mime` alone.
fn headers(bytes: &[u8]) -> Harvest {
    if looks_like_mbox(bytes) {
        return Harvest::of(vec![
            json!({ "attribute": "file:mime", "value": "application/mbox" }),
        ]);
    }
    if !looks_like_mail(bytes) {
        return Harvest::default();
    }
    let root = mime::parse(bytes);
    let mut lines = Vec::new();
    let mut remarks = Vec::new();
    // For a file that ingest detected as text/plain this is the type the
    // other two contracts read.
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
        for value in header::read(name, raw) {
            match value {
                Ok(value) => {
                    lines.push(json!({ "attribute": format!("mail:{name}"), "value": value }));
                }
                Err(reason) => remarks.push(format!("mail:{name} not recorded, {reason}")),
            }
        }
    }
    Harvest { lines, remarks }
}

/// The `attachments` contract: every attachment and every nested message
/// of a message written into `directory`, and the lines that announce
/// them. Nothing when the bytes are no message.
fn attachments(bytes: &[u8], directory: &Path) -> std::io::Result<Vec<serde_json::Value>> {
    let mut lines = Vec::new();
    if !looks_like_mail(bytes) {
        return Ok(lines);
    }
    let root = mime::parse(bytes);
    let mut taken = HashSet::new();
    for part in root.leaves().filter(|part| part.is_attachment()) {
        let spelled = part.fields.name.as_deref();
        let wanted = spelled.and_then(|spelled| basename(spelled).map(|name| (name, spelled)));
        // A nested message without a name still becomes a file, so it
        // needs a name to be written under.
        let fallback = if part.is_message() {
            "message.eml"
        } else {
            "attachment"
        };
        // A name too long for the filesystem, or one nothing is left of,
        // must not fail the whole mail on every run. The file is written
        // under a name that fits, and the name from the mail is recorded.
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
            // The announced name is only a handle in the directory and is
            // never recorded. Recorded are the name from the mail, and as
            // the place the full name with `@` in front.
            lines.push(json!({ "file": &announced, "attribute": "file:name", "value": name }));
            lines.push(
                json!({ "file": &announced, "attribute": "file:path", "value": inner(spelled) }),
            );
        }
        if let Some(id) = &part.fields.content_id {
            lines.push(json!({
                "file": &announced,
                "attribute": "mail:content-id",
                "value": id,
            }));
        }
    }
    Ok(lines)
}

/// The name of the file the `text` contract writes.
const TEXT_FILE: &str = "text.txt";

/// The `text` contract: the text of a message written into `directory`,
/// and the line that announces it. Nothing when the bytes are no message
/// or the message has no text.
fn text(bytes: &[u8], directory: &Path) -> std::io::Result<Vec<serde_json::Value>> {
    if !looks_like_mail(bytes) {
        return Ok(Vec::new());
    }
    let Some(text) = text::of(&mime::parse(bytes), bytes) else {
        return Ok(Vec::new());
    };
    std::fs::write(directory.join(TEXT_FILE), text).map_err(|error| {
        std::io::Error::new(error.kind(), format!("writing {TEXT_FILE}: {error}"))
    })?;
    Ok(vec![json!({ "file": TEXT_FILE, "mime": "text/plain" })])
}

/// The attribute name for a header that is recorded, `None` for one that
/// is not.
fn spoken(name: &str) -> Option<&'static str> {
    header::RECORDED
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

    const NOTHING: Vec<serde_json::Value> = Vec::new();

    #[test]
    fn only_headers_reads_text_and_runs_first() {
        let identities = identities();
        let contracts: Vec<_> = identities.iter().map(|line| &line["contract"]).collect();
        assert_eq!(contracts, ["headers", "attachments", "text"]);
        let reads_text: Vec<bool> = identities
            .iter()
            .map(|line| {
                line["mimes"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("text/plain"))
            })
            .collect();
        assert_eq!(reads_text, [true, false, false]);
        assert_eq!(identities[0].get("derives"), None);
        assert_eq!(identities[1]["derives"], true);
        assert_eq!(identities[2]["derives"], true);
    }

    #[test]
    fn the_text_is_written_as_one_file() {
        let dir = TempDir::new().unwrap();
        let mail = concat!(
            "From: alice@example.com\r\n",
            "To: bob@example.org\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=\"cut\"\r\n",
            "\r\n",
            "--cut\r\n",
            "Content-Type: text/plain; charset=iso-8859-1\r\n",
            "Content-Transfer-Encoding: quoted-printable\r\n",
            "\r\n",
            "Gr=FC=DFe,\r\n",
            "see attached.\r\n",
            "--cut\r\n",
            "Content-Type: text/plain; name=\"notes.txt\"\r\n",
            "\r\n",
            "attached notes\r\n",
            "--cut--\r\n",
        );

        let lines = text(mail.as_bytes(), dir.path()).unwrap();

        assert_eq!(lines, [json!({ "file": "text.txt", "mime": "text/plain" })]);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("text.txt")).unwrap(),
            "Gr\u{fc}\u{df}e,\nsee attached."
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_message_without_text_gets_no_file() {
        let dir = TempDir::new().unwrap();
        let mail = concat!(
            "From: alice@example.com\r\n",
            "To: bob@example.org\r\n",
            "Content-Type: multipart/mixed; boundary=\"cut\"\r\n",
            "\r\n",
            "--cut\r\n",
            "Content-Type: application/pdf; name=\"invoice.pdf\"\r\n",
            "\r\n",
            "%PDF-1.4\r\n",
            "--cut--\r\n",
        );

        assert_eq!(text(mail.as_bytes(), dir.path()).unwrap(), NOTHING);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

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

        let lines = attachments(mail.as_bytes(), dir.path()).unwrap();

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

        assert_eq!(
            headers(mbox.as_bytes()).lines,
            vec![json!({ "attribute": "file:mime", "value": "application/mbox" })],
            "the type alone, no headers"
        );
        assert_eq!(attachments(mbox.as_bytes(), dir.path()).unwrap(), NOTHING);
        assert_eq!(text(mbox.as_bytes(), dir.path()).unwrap(), NOTHING);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn bytes_without_a_message_are_an_empty_answer_not_an_error() {
        let dir = TempDir::new().unwrap();
        for bytes in [&b"plain words"[..], b""] {
            assert_eq!(headers(bytes).lines, NOTHING);
            assert_eq!(attachments(bytes, dir.path()).unwrap(), NOTHING);
            assert_eq!(text(bytes, dir.path()).unwrap(), NOTHING);
        }
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "and nothing lands in the directory"
        );
    }

    #[test]
    fn an_encoded_word_in_a_multi_byte_charset_is_decoded() {
        let mail = concat!(
            "From: =?ISO-2022-JP?B?GyRCRnxLXBsoQg==?= <taro@example.jp>\r\n",
            "To: bob@example.org\r\n",
            "Subject: =?ISO-2022-JP?B?GyRCRnxLXBsoQg==?=\r\n",
            "\r\n",
            "body\r\n",
        );

        let lines = headers(mail.as_bytes()).lines;

        assert!(
            lines.contains(&json!({ "attribute": "mail:subject", "value": "\u{65e5}\u{672c}" })),
            "got {lines:#?}"
        );
        assert!(
            lines.contains(&json!({
                "attribute": "mail:from",
                "value": "\u{65e5}\u{672c} <taro@example.jp>",
            })),
            "got {lines:#?}"
        );
    }

    #[test]
    fn the_headers_are_recorded_under_mail_in_searchable_form() {
        let dir = TempDir::new().unwrap();
        let mail = concat!(
            "Received: from mx.example.com by mail.example.org\r\n",
            "From: Alice Adams <Alice@Example.com>\r\n",
            "To: bob <bob@example.org>,\r\n",
            " carol <carol@example.org>\r\n",
            "Cc: John Doe\r\n",
            "Subject: =?ISO-8859-1?Q?Gr=FC=DFe?=\r\n",
            "Date: Fri, 4 Sep 2026 12:34:56 +0200\r\n",
            "Message-ID: <one@example.com>\r\n",
            "References: <zero@example.com> <half@example.com>\r\n",
            "\r\n",
            "Hello.\r\n",
        );

        let harvest = headers(mail.as_bytes());
        let lines = harvest.lines;

        let expect = |line: serde_json::Value| {
            assert!(lines.contains(&line), "missing {line}; got {lines:#?}");
        };
        expect(json!({ "attribute": "file:mime", "value": "message/rfc822" }));
        expect(json!({ "attribute": "mail:from", "value": "Alice Adams <alice@example.com>" }));
        expect(json!({ "attribute": "mail:to", "value": "bob <bob@example.org>" }));
        expect(json!({ "attribute": "mail:to", "value": "carol <carol@example.org>" }));
        expect(json!({ "attribute": "mail:subject", "value": "Gr\u{fc}\u{df}e" }));
        expect(json!({ "attribute": "mail:date", "value": "2026-09-04T10:34:56Z" }));
        expect(json!({ "attribute": "mail:message-id", "value": "one@example.com" }));
        expect(json!({ "attribute": "mail:references", "value": "zero@example.com" }));
        expect(json!({ "attribute": "mail:references", "value": "half@example.com" }));
        assert!(
            !lines.iter().any(|line| line["attribute"] == "mail:cc"),
            "a name without an address is not recorded; got {lines:#?}"
        );
        assert_eq!(
            harvest.remarks,
            ["mail:cc not recorded, no address: John Doe"]
        );
        assert!(
            !lines
                .iter()
                .any(|line| line["attribute"] == "mail:received"),
            "the transport's trail stays untold; got {lines:#?}"
        );
        assert_eq!(
            attachments(mail.as_bytes(), dir.path()).unwrap(),
            NOTHING,
            "a body without a name is no attachment"
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

        let lines = attachments(mail.as_bytes(), dir.path()).unwrap();

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
                "value": "part2@example.com",
            })),
            "the content-id is recorded on the part, without angle brackets; got {lines:#?}"
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

        attachments(mail, dir.path()).unwrap();

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

        attachments(&mail, dir.path()).unwrap();

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

        let lines = attachments(mail.as_bytes(), dir.path()).unwrap();

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
            headers(carried.as_bytes()).lines.len() > 1,
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

        let lines = attachments(mail.as_bytes(), dir.path()).unwrap();

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

        let lines = attachments(mail.as_bytes(), dir.path()).unwrap();

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

    /// The files `attachments` writes for `bytes`, by name.
    fn extracted(bytes: &[u8]) -> std::collections::BTreeMap<String, Vec<u8>> {
        let dir = TempDir::new().unwrap();
        attachments(bytes, dir.path()).unwrap();
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
    fn random_messages_never_fail_a_contract() {
        for message in mime::random_messages(2_000) {
            let bytes = [
                &b"From: a@example.org\r\nTo: b@example.org\r\n"[..],
                &message,
            ]
            .concat();
            let dir = TempDir::new().unwrap();
            headers(&bytes);
            attachments(&bytes, dir.path()).unwrap();
            text(&bytes, dir.path()).unwrap();
        }
    }

    #[test]
    fn every_message_written_is_a_type_every_contract_reads() {
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
        let lines = attachments(mail, dir.path()).unwrap();

        let written: Vec<_> = lines.iter().filter_map(|line| line.get("mime")).collect();
        assert_eq!(written, ["message/rfc822", "message/global"]);
        for identity in identities() {
            let read = identity["mimes"].as_array().unwrap();
            for mime in &written {
                assert!(
                    read.contains(mime),
                    "{mime} is written but not read by {}",
                    identity["contract"]
                );
            }
        }
        assert_eq!(
            std::fs::read(dir.path().join("message-2.eml")).unwrap(),
            "From: d@example.org\r\nSubject: \u{fc}ber\r\n\r\nbody".as_bytes()
        );
    }
}
