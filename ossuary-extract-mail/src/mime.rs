//! The MIME structure of a message, as ranges of its bytes.
//!
//! mail-parser finds the end of a part while it parses the part, with one
//! state for all levels. It also takes `--boundary` in the middle of a line
//! for a delimiter, and a part that confuses it moves the end of every
//! enclosing part. This module finds the structure itself, from the outside
//! in, and never copies or rewrites a byte. mail-parser only reads header
//! sections.
//!
//! Rules, malformed messages included:
//!
//! - A delimiter is a line that starts with `--` and the boundary, followed
//!   by an optional `--` and nothing but spaces or tabs (RFC 2046, 5.1.1).
//!   The line break before it belongs to it. Anything else is content,
//!   `--boundary` in the middle of a line too.
//! - A part ends at the next delimiter of its own multipart. Nothing inside
//!   the part can move that end, so a malformed part harms only itself.
//! - The first close delimiter ends a multipart; what follows is epilogue.
//!   Without one, the last part runs to the end of the body. A multipart
//!   without a boundary or without an opening delimiter is a single part.
//! - A header section is the run of field lines at the start of an entity.
//!   It ends at an empty line, or at the first line that is not a field,
//!   which then starts the body, as in Python's email package.
//! - An attached message (message/rfc822, message/global) is not opened.
//!   Its body is one range; the next round of `ossuary extract` reads it.
//! - Multiparts nested deeper than `MAX_DEPTH` are not split.
//!
//! The transfer encodings are decoded here as well, as far as the bytes
//! allow; decoding never fails. Besides base64 and quoted-printable this
//! includes uuencode, which older mail programs declared as x-uuencode.
//! mail-parser's decoders drop the last bytes of base64 without padding and
//! an escape cut short at the end of quoted-printable, and give up on a
//! whole part for one stray `==`.

use std::borrow::Cow;
use std::ops::Range;

use mail_parser::{MessageParser, MimeHeaders as _};

/// Real mail nests a few levels deep. The limit keeps a crafted message
/// from exhausting the stack.
const MAX_DEPTH: usize = 64;

/// One MIME entity: the message itself, or one part of a multipart.
#[derive(Debug)]
pub struct Entity {
    /// The header section, every line with its line break. It starts where
    /// the entity starts.
    pub header: Range<usize>,
    /// Everything after the header section and the empty line that ends
    /// it, to the end of the entity. Without an empty line, the body starts
    /// at the first line that is not a field.
    pub body: Range<usize>,
    /// What the header section declares about the body.
    pub fields: Fields,
    /// How the body is structured.
    pub content: Content,
}

/// The header fields this module reads, as mail-parser reads them.
#[derive(Debug, Default)]
pub struct Fields {
    /// `type/subtype` as declared, in lower case.
    pub content_type: Option<String>,
    /// The `boundary` parameter of the Content-Type.
    pub boundary: Option<String>,
    /// The Content-Transfer-Encoding.
    pub encoding: Encoding,
    /// The Content-Disposition `filename`, or else the Content-Type `name`.
    pub name: Option<String>,
    /// The Content-ID, without angle brackets.
    pub content_id: Option<String>,
}

/// A Content-Transfer-Encoding, as far as decoding goes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// 7bit, 8bit, binary, none declared, or one that is not known.
    #[default]
    Identity,
    Base64,
    QuotedPrintable,
    /// x-uuencode, uuencode, x-uue or uue.
    Uuencode,
}

/// How the body of an entity is structured.
#[derive(Debug)]
pub enum Content {
    /// A body that is not split.
    Single,
    /// An attached message, not opened.
    Message,
    /// A multipart body, split at its delimiter lines. Between two parts
    /// lies one delimiter line with the line break before it. Before the
    /// first part lie the preamble and the first delimiter, after the last
    /// the close delimiter and the epilogue, if there is one.
    Multipart(Vec<Entity>),
}

/// The structure of `bytes`, read as one message. Never fails: bytes
/// without structure are a single part.
pub fn parse(bytes: &[u8]) -> Entity {
    entity(bytes, 0..bytes.len(), false, 0)
}

impl Entity {
    /// Whether the entity is an attached message.
    pub fn is_message(&self) -> bool {
        matches!(self.content, Content::Message)
    }

    /// The entities that are not split further, in the order of the bytes:
    /// single parts and attached messages, the entity itself if it is one.
    pub fn leaves(&self) -> Leaves<'_> {
        Leaves { stack: vec![self] }
    }

    /// The body with its transfer encoding undone.
    pub fn decoded<'x>(&self, bytes: &'x [u8]) -> Cow<'x, [u8]> {
        let body = &bytes[self.body.clone()];
        match self.fields.encoding {
            Encoding::Identity => Cow::Borrowed(body),
            Encoding::Base64 => Cow::Owned(base64(body)),
            Encoding::QuotedPrintable => Cow::Owned(quoted_printable(body)),
            Encoding::Uuencode => uudecode(body).map_or(Cow::Borrowed(body), Cow::Owned),
        }
    }
}

/// Iterator over the leaves of an entity, depth first.
pub struct Leaves<'a> {
    stack: Vec<&'a Entity>,
}

impl<'a> Iterator for Leaves<'a> {
    type Item = &'a Entity;

    fn next(&mut self) -> Option<&'a Entity> {
        while let Some(entity) = self.stack.pop() {
            match &entity.content {
                Content::Multipart(parts) => self.stack.extend(parts.iter().rev()),
                Content::Single | Content::Message => return Some(entity),
            }
        }
        None
    }
}

impl Fields {
    fn read(header: &[u8]) -> Fields {
        let header = unfolded(header);
        let Some(message) = MessageParser::default().parse_headers(&header) else {
            return Fields::default();
        };
        let content_type = message.content_type();
        Fields {
            content_type: content_type.and_then(|declared| {
                let subtype = declared.subtype()?;
                Some(format!("{}/{subtype}", declared.ctype()).to_ascii_lowercase())
            }),
            boundary: content_type
                .and_then(|declared| declared.attribute("boundary"))
                .map(str::to_string),
            encoding: message
                .content_transfer_encoding()
                .map_or(Encoding::Identity, Encoding::named),
            name: message.attachment_name().map(str::to_string),
            content_id: message.content_id().map(str::to_string),
        }
    }
}

/// The header section with its folds removed (RFC 5322, 2.2.3). mail-parser
/// cannot read a parameter whose value starts on the next line, as Exchange
/// writes a long file name: `name=`, line break, tab, `"..."`.
fn unfolded(header: &[u8]) -> Vec<u8> {
    let mut unfolded = Vec::with_capacity(header.len());
    let mut lines = header.split_inclusive(|&byte| byte == b'\n').peekable();
    while let Some(line) = lines.next() {
        let folded = lines
            .peek()
            .is_some_and(|next| matches!(next.first(), Some(b' ' | b'\t')));
        unfolded.extend_from_slice(if folded { without_break(line) } else { line });
    }
    unfolded
}

impl Encoding {
    fn named(name: &str) -> Encoding {
        match name.trim().to_ascii_lowercase().as_str() {
            "base64" => Encoding::Base64,
            "quoted-printable" => Encoding::QuotedPrintable,
            "x-uuencode" | "uuencode" | "x-uue" | "uue" => Encoding::Uuencode,
            _ => Encoding::Identity,
        }
    }
}

/// The entity in `range`. In a multipart/digest, a part that declares no
/// type is a message (RFC 2046, 5.1.5).
fn entity(bytes: &[u8], range: Range<usize>, in_digest: bool, depth: usize) -> Entity {
    let (header, body_start) = header_section(bytes, range.clone());
    let body = body_start..range.end;
    let fields = Fields::read(&bytes[header.clone()]);
    let content = match fields.content_type.as_deref() {
        Some("message/rfc822" | "message/global") => Content::Message,
        None if in_digest => Content::Message,
        Some(declared) if declared.starts_with("multipart/") && depth < MAX_DEPTH => fields
            .boundary
            .as_deref()
            .filter(|boundary| !boundary.is_empty())
            .and_then(|boundary| {
                let digest = declared == "multipart/digest";
                multipart(bytes, body.clone(), boundary.as_bytes(), digest, depth + 1)
            })
            .map_or(Content::Single, Content::Multipart),
        _ => Content::Single,
    };
    Entity {
        header,
        body,
        fields,
        content,
    }
}

/// The header section at the start of `range`, and where the body starts.
fn header_section(bytes: &[u8], range: Range<usize>) -> (Range<usize>, usize) {
    let mut end = range.start;
    let mut in_field = false;
    for line in lines(bytes, range.clone()) {
        let text = without_break(&bytes[line.clone()]);
        if text.is_empty() {
            return (range.start..line.start, line.end);
        }
        let folded = in_field && matches!(text[0], b' ' | b'\t');
        if !folded && !is_field(text) {
            break;
        }
        in_field = true;
        end = line.end;
    }
    (range.start..end, end)
}

/// Whether a line starts a header field: a name of printable characters,
/// then a colon. Whitespace before the colon is the obsolete syntax of
/// RFC 5322 (4.5) and still accepted.
fn is_field(line: &[u8]) -> bool {
    let Some(colon) = line.iter().position(|&byte| byte == b':') else {
        return false;
    };
    let name = line[..colon].trim_ascii_end();
    !name.is_empty() && name.iter().all(|byte| (33..=126).contains(byte))
}

/// `body` split at the delimiter lines of `boundary`, or `None` when no
/// line opens a part.
fn multipart(
    bytes: &[u8],
    body: Range<usize>,
    boundary: &[u8],
    digest: bool,
    depth: usize,
) -> Option<Vec<Entity>> {
    // Every delimiter from the line break before it to the end of its line,
    // and whether it closes, up to the first that closes.
    let mut delimiters: Vec<(Range<usize>, bool)> = Vec::new();
    // A line break already taken by the previous delimiter line cannot
    // belong to the next one as well.
    let mut floor = body.start;
    for line in lines(bytes, body.clone()) {
        let Some(closes) = delimiter(&bytes[line.clone()], boundary) else {
            continue;
        };
        delimiters.push((
            line_break_before(bytes, floor, line.start)..line.end,
            closes,
        ));
        floor = line.end;
        if closes {
            break;
        }
    }
    if delimiters.first().is_none_or(|(_, closes)| *closes) {
        return None;
    }
    let parts = delimiters
        .iter()
        .enumerate()
        .take_while(|(_, (_, closes))| !closes)
        .map(|(index, (delimiter, _))| {
            let end = delimiters
                .get(index + 1)
                .map_or(body.end, |(next, _)| next.start);
            entity(bytes, delimiter.end..end, digest, depth)
        })
        .collect();
    Some(parts)
}

/// Whether `line` is a delimiter line of `boundary`: `Some(true)` for the
/// close delimiter, `Some(false)` for one that opens a part.
fn delimiter(line: &[u8], boundary: &[u8]) -> Option<bool> {
    let rest = without_break(line)
        .strip_prefix(b"--")?
        .strip_prefix(boundary)?;
    let (closes, rest) = match rest.strip_prefix(b"--") {
        Some(rest) => (true, rest),
        None => (false, rest),
    };
    rest.iter()
        .all(|&byte| byte == b' ' || byte == b'\t')
        .then_some(closes)
}

/// Where the line break before the line at `at` starts, CRLF or LF. At
/// `floor` there is none to take.
fn line_break_before(bytes: &[u8], floor: usize, at: usize) -> usize {
    if at == floor {
        at
    } else if at - floor >= 2 && &bytes[at - 2..at] == b"\r\n" {
        at - 2
    } else {
        at - 1
    }
}

/// The lines in `range`, each with its line break. The last one may have
/// none.
fn lines(bytes: &[u8], range: Range<usize>) -> impl Iterator<Item = Range<usize>> + '_ {
    let end = range.end;
    let mut start = range.start;
    std::iter::from_fn(move || {
        if start >= end {
            return None;
        }
        let line_end = bytes[start..end]
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(end, |at| start + at + 1);
        let line = start..line_end;
        start = line_end;
        Some(line)
    })
}

/// A line without its line break, LF or CRLF.
fn without_break(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// base64, read leniently: characters outside the alphabet are skipped,
/// `=` ends a group and decoding goes on after it, and a group cut short
/// yields the bytes it holds. A single leftover character holds none.
fn base64(encoded: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(encoded.len() / 4 * 3);
    let mut group = 0u32;
    let mut count = 0u32;
    for &byte in encoded {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                flush(&mut decoded, group, count);
                (group, count) = (0, 0);
                continue;
            }
            _ => continue,
        };
        group = group << 6 | u32::from(value);
        count += 1;
        if count == 4 {
            flush(&mut decoded, group, count);
            (group, count) = (0, 0);
        }
    }
    flush(&mut decoded, group, count);
    decoded
}

/// Appends the bytes in a group of `count` base64 characters: three for a
/// full group, one or two for a group cut short.
fn flush(decoded: &mut Vec<u8>, group: u32, count: u32) {
    if count < 2 {
        return;
    }
    let aligned = group << (6 * (4 - count));
    let held = (count - 1) as usize;
    decoded.extend_from_slice(&aligned.to_be_bytes()[1..=held]);
}

/// quoted-printable (RFC 2045, 6.7), with the robust handling the RFC
/// suggests: an `=` that starts neither an escape of two hex digits nor a
/// soft line break is kept, with what follows it. Spaces and tabs at the
/// end of a line are transport padding and dropped (rule 3). Line breaks
/// stay as they are.
fn quoted_printable(encoded: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(encoded.len());
    for line in encoded.split_inclusive(|&byte| byte == b'\n') {
        let text = without_break(line);
        let line_break = &line[text.len()..];
        let end = text
            .iter()
            .rposition(|&byte| byte != b' ' && byte != b'\t')
            .map_or(0, |last| last + 1);
        let (text, soft) = match text[..end].strip_suffix(b"=") {
            Some(text) => (text, true),
            None => (&text[..end], false),
        };
        let mut at = 0;
        while at < text.len() {
            if text[at] == b'='
                && let Some(byte) = text.get(at + 1..at + 3).and_then(escaped)
            {
                decoded.push(byte);
                at += 3;
            } else {
                decoded.push(text[at]);
                at += 1;
            }
        }
        if !soft {
            decoded.extend_from_slice(line_break);
        }
    }
    decoded
}

/// The byte two hex digits stand for, upper or lower case.
fn escaped(digits: &[u8]) -> Option<u8> {
    let digit = |byte: u8| char::from(byte).to_digit(16);
    let value = digit(digits[0])? << 4 | digit(digits[1])?;
    u8::try_from(value).ok()
}

/// uuencode: the data starts after the `begin` line and ends at the `end`
/// line or the end of the body. Encoders wrote zeros as spaces, and
/// transport strips spaces at the end of a line, so a line shorter than its
/// length character says is filled with zeros. Empty lines are skipped.
/// `None` when there is no `begin` line.
fn uudecode(encoded: &[u8]) -> Option<Vec<u8>> {
    let mut lines = encoded
        .split_inclusive(|&byte| byte == b'\n')
        .map(without_break);
    lines.by_ref().find(|line| is_begin(line))?;
    let mut decoded = Vec::with_capacity(encoded.len() / 4 * 3);
    for line in lines {
        if line.trim_ascii() == b"end" {
            break;
        }
        let Some((&length, data)) = line.split_first() else {
            continue;
        };
        let length = usize::from(sextet(length));
        let start = decoded.len();
        for group in 0..length.div_ceil(3) {
            let value = (0..4).fold(0u32, |value, index| {
                let byte = data.get(group * 4 + index).copied().unwrap_or(b' ');
                value << 6 | u32::from(sextet(byte))
            });
            decoded.extend_from_slice(&value.to_be_bytes()[1..]);
        }
        decoded.truncate(start + length);
    }
    Some(decoded)
}

/// The six bits a uuencode character stands for; space and backtick are
/// both zero.
fn sextet(byte: u8) -> u8 {
    byte.wrapping_sub(b' ') & 0x3f
}

/// Whether a line is a `begin` line: `begin`, an octal mode, a name.
fn is_begin(line: &[u8]) -> bool {
    let Some(rest) = line.strip_prefix(b"begin ") else {
        return false;
    };
    let mode = rest.split(|&byte| byte == b' ').next().unwrap_or_default();
    !mode.is_empty() && mode.iter().all(|byte| (b'0'..=b'7').contains(byte))
}

/// Random messages, line by line: delimiter lines of two boundaries,
/// MIME fields, empty lines, and lines of pieces that matter to the
/// splitter or the decoders. Line breaks are CRLF, LF, a bare CR or
/// missing.
#[cfg(test)]
pub(crate) fn random_messages(count: usize) -> Vec<Vec<u8>> {
    const LINES: &[&[u8]] = &[
        b"--b",
        b"--b",
        b"--b--",
        b"--b \t",
        b"--bX",
        b"--c",
        b"--c--",
        b"",
        b"",
        b"",
        b"Content-Type: multipart/mixed; boundary=b",
        b"Content-Type: multipart/mixed; boundary=c",
        b"Content-Type: multipart/digest; boundary=c",
        b"Content-Type: multipart/mixed",
        b"Content-Type: message/rfc822",
        b"Content-Type: text/plain",
        b"Content-Transfer-Encoding: base64",
        b"Content-Transfer-Encoding: quoted-printable",
        b"Content-Disposition: attachment; filename=a",
        b" folded",
        b"--b\r\nContent-Type: multipart/mixed; boundary=c\r\n\r\n--c",
        b"--b\r\nContent-Type: message/rfc822\r\n",
        b"--c\r\nContent-Type: multipart/digest; boundary=b\r\n\r\n--b",
    ];
    const PIECES: &[&[u8]] = &[
        b"x", b" ", b"=", b"==", b"=4", b"QUJD", b":", b"--b", b"--", b"\xff", b"\x00", b"\r",
    ];
    const BREAKS: &[&[u8]] = &[b"\r\n", b"\r\n", b"\n", b"\n", b"\r", b""];
    // xorshift64: reproducible without a dependency.
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move |below: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        usize::try_from(state % below as u64).unwrap()
    };
    (0..count)
        .map(|_| {
            let mut message = Vec::new();
            if next(2) == 0 {
                message.extend_from_slice(b"Content-Type: multipart/mixed; boundary=b\r\n\r\n");
            }
            for _ in 0..next(40) {
                if next(3) == 0 {
                    for _ in 0..next(6) {
                        message.extend_from_slice(PIECES[next(PIECES.len())]);
                    }
                } else {
                    message.extend_from_slice(LINES[next(LINES.len())]);
                }
                message.extend_from_slice(BREAKS[next(BREAKS.len())]);
            }
            message
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses `bytes`, checks that every byte is where the rules put it, and
    /// that every leaf decodes.
    fn lossless(bytes: &[u8]) -> Entity {
        let root = parse(bytes);
        check(bytes, &root, 0..bytes.len());
        for leaf in root.leaves() {
            let _ = leaf.decoded(bytes);
        }
        root
    }

    /// The entity covers `range`: header, at most one empty line, body. In a
    /// multipart body, the parts follow each other with nothing between
    /// them but one delimiter line each.
    fn check(bytes: &[u8], entity: &Entity, range: Range<usize>) {
        let shown = || String::from_utf8_lossy(bytes).into_owned();
        assert_eq!(entity.header.start, range.start, "{}", shown());
        assert_eq!(entity.body.end, range.end, "{}", shown());
        let separator = &bytes[entity.header.end..entity.body.start];
        assert!(
            matches!(separator, b"" | b"\n" | b"\r\n" | b"\r"),
            "{separator:?} between header and body: {}",
            shown()
        );
        let Content::Multipart(parts) = &entity.content else {
            return;
        };
        let boundary = entity.fields.boundary.as_deref().unwrap().as_bytes();
        let mut at = entity.body.start;
        for (index, part) in parts.iter().enumerate() {
            let gap = &bytes[at..part.header.start];
            let line = if index == 0 {
                // The preamble may come first; the delimiter is the last line.
                let without = gap.strip_suffix(b"\n").unwrap_or(gap);
                &gap[without
                    .iter()
                    .rposition(|&byte| byte == b'\n')
                    .map_or(0, |at| at + 1)..]
            } else {
                let line = before_break(gap);
                assert!(
                    !without_break(line).contains(&b'\n'),
                    "more than a delimiter between parts: {gap:?}"
                );
                line
            };
            assert_eq!(
                delimiter(line, boundary),
                Some(false),
                "{gap:?}: {}",
                shown()
            );
            check(bytes, part, part.header.start..part.body.end);
            at = part.body.end;
        }
        let rest = &bytes[at..entity.body.end];
        if !rest.is_empty() {
            let after = before_break(rest);
            let line = &after[..after
                .iter()
                .position(|&byte| byte == b'\n')
                .map_or(after.len(), |at| at + 1)];
            assert_eq!(
                delimiter(line, boundary),
                Some(true),
                "{rest:?}: {}",
                shown()
            );
        }
    }

    /// The bytes after the line break they start with, if any.
    fn before_break(bytes: &[u8]) -> &[u8] {
        bytes
            .strip_prefix(b"\r\n")
            .or_else(|| bytes.strip_prefix(b"\n"))
            .unwrap_or(bytes)
    }

    fn parts(entity: &Entity) -> &[Entity] {
        match &entity.content {
            Content::Multipart(parts) => parts,
            other => panic!("not split: {other:?}"),
        }
    }

    fn at(bytes: &[u8], range: Range<usize>) -> &[u8] {
        &bytes[range]
    }

    #[test]
    fn a_delimiter_is_a_whole_line() {
        let bytes = b"Content-Type: multipart/mixed; boundary=\"b\"\r\n\
            \r\n\
            preamble\r\n\
            --b\r\n\
            Content-Type: text/plain\r\n\
            \r\n\
            see --b here\r\n\
            --b \t\r\n\
            \r\n\
            second\r\n\
            --bX\r\n\
            --b--\r\n\
            epilogue\r\n";
        let root = lossless(bytes);
        let parts = parts(&root);

        assert_eq!(parts.len(), 2);
        assert_eq!(
            at(bytes, root.body.start..parts[0].header.start),
            b"preamble\r\n--b\r\n"
        );
        assert_eq!(at(bytes, parts[0].body.clone()), b"see --b here");
        assert_eq!(
            at(bytes, parts[0].body.end..parts[1].header.start),
            b"\r\n--b \t\r\n",
            "spaces and tabs after the boundary are padding"
        );
        assert_eq!(
            at(bytes, parts[1].body.clone()),
            b"second\r\n--bX",
            "a longer boundary is content"
        );
        assert_eq!(
            at(bytes, parts[1].body.end..root.body.end),
            b"\r\n--b--\r\nepilogue\r\n"
        );
    }

    #[test]
    fn a_broken_part_does_not_move_the_end_of_its_neighbours() {
        let bytes = b"Content-Type: multipart/mixed; boundary=o\r\n\
            \r\n\
            --o\r\n\
            Content-Type: multipart/mixed; boundary=i\r\n\
            \r\n\
            --i\r\n\
            \r\n\
            inner, never closed\r\n\
            --o\r\n\
            Content-Type: application/pdf\r\n\
            Content-Disposition: attachment; filename=after.pdf\r\n\
            \r\n\
            %PDF\r\n\
            --o--\r\n";
        let root = lossless(bytes);
        let outer = parts(&root);

        assert_eq!(outer.len(), 2);
        let inner = parts(&outer[0]);
        assert_eq!(
            at(bytes, inner[0].body.clone()),
            b"inner, never closed",
            "the last inner part runs to the end of its outer part"
        );
        assert_eq!(inner[0].body.end, outer[0].body.end);
        assert_eq!(outer[1].fields.name.as_deref(), Some("after.pdf"));
        assert_eq!(at(bytes, outer[1].body.clone()), b"%PDF");
    }

    #[test]
    fn an_attached_message_is_one_range() {
        let message = b"From: a@example.org\r\n\
            Content-Type: multipart/mixed; boundary=m\r\n\
            \r\n\
            --m\r\n\
            \r\n\
            mentions --o in passing, never closes";
        let bytes = [
            &b"Content-Type: multipart/mixed; boundary=o\r\n\r\n--o\r\n\
                Content-Type: message/rfc822\r\n\r\n"[..],
            message,
            b"\r\n--o--\r\n",
        ]
        .concat();
        let root = lossless(&bytes);
        let outer = parts(&root);

        assert_eq!(outer.len(), 1);
        assert!(outer[0].is_message());
        assert_eq!(at(&bytes, outer[0].body.clone()), message);
        assert_eq!(outer[0].leaves().count(), 1, "not opened");
    }

    #[test]
    fn a_multipart_that_cannot_be_split_is_a_single_part() {
        for bytes in [
            &b"Content-Type: multipart/mixed\r\n\r\n--b\r\n\r\nno boundary declared\r\n"[..],
            b"Content-Type: multipart/mixed; boundary=b\r\n\r\nno delimiter at all\r\n",
            b"Content-Type: multipart/mixed; boundary=b\r\n\r\ntext --b\r\n",
            b"Content-Type: multipart/mixed; boundary=b\r\n\r\n--b--\r\nclosed before opened\r\n",
            b"Content-Type: multipart/mixed; boundary=\"\"\r\n\r\n--\r\n\r\nempty boundary\r\n",
        ] {
            let root = lossless(bytes);
            assert!(
                matches!(root.content, Content::Single),
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn without_a_close_delimiter_the_last_part_runs_to_the_end() {
        let bytes = b"Content-Type: multipart/mixed; boundary=b\n\n--b\n\nfirst\n--b\n\nlast\n";
        let root = lossless(bytes);
        let parts = parts(&root);

        assert_eq!(parts.len(), 2);
        assert_eq!(at(bytes, parts[1].body.clone()), b"last\n");
        assert_eq!(parts[1].body.end, bytes.len());
    }

    #[test]
    fn only_the_first_close_delimiter_counts() {
        let bytes = b"Content-Type: multipart/mixed; boundary=b\n\n\
            --b\n\nfirst\n--b--\n--b\n\nafter the end\n--b--\n";
        let root = lossless(bytes);
        let parts = parts(&root);

        assert_eq!(parts.len(), 1);
        assert_eq!(
            at(bytes, parts[0].body.end..root.body.end),
            b"\n--b--\n--b\n\nafter the end\n--b--\n"
        );
    }

    #[test]
    fn the_header_section_is_the_run_of_field_lines() {
        let bytes = b"From: a@example.org\r\nSubject: no body";
        let root = lossless(bytes);
        assert_eq!(root.header, 0..bytes.len(), "everything is header");
        assert!(root.body.is_empty());

        let bytes = b"Content-Type: text/plain\r\nHello, no empty line.\r\n\r\nMore.\r\n";
        let root = lossless(bytes);
        assert_eq!(
            at(bytes, root.header.clone()),
            b"Content-Type: text/plain\r\n"
        );
        assert_eq!(
            at(bytes, root.body.clone()),
            b"Hello, no empty line.\r\n\r\nMore.\r\n",
            "the first line that is no field starts the body"
        );

        let bytes = b"\r\nonly a body\r\n";
        let root = lossless(bytes);
        assert!(root.header.is_empty());
        assert_eq!(at(bytes, root.body.clone()), b"only a body\r\n");

        let bytes = b"Subject : obsolete\r\n folded\r\nX-A: b\r\n\r\nbody";
        let root = lossless(bytes);
        assert_eq!(at(bytes, root.body.clone()), b"body");

        let bytes = b" leading fold\r\nFrom: a@example.org\r\n\r\nbody";
        let root = lossless(bytes);
        assert!(
            root.header.is_empty(),
            "a fold before any field is no header"
        );
    }

    #[test]
    fn in_a_digest_a_part_without_a_type_is_a_message() {
        let bytes = b"Content-Type: multipart/digest; boundary=d\r\n\r\n\
            --d\r\n\
            \r\n\
            From: a@example.org\r\n\r\nfirst\r\n\
            --d\r\n\
            Content-Type: text/plain\r\n\
            \r\n\
            not a message\r\n\
            --d--\r\n";
        let root = lossless(bytes);
        let parts = parts(&root);

        assert!(parts[0].is_message());
        assert_eq!(
            at(bytes, parts[0].body.clone()),
            b"From: a@example.org\r\n\r\nfirst"
        );
        assert!(!parts[1].is_message());
    }

    #[test]
    fn message_global_is_a_message() {
        let bytes = b"Content-Type: message/global\r\n\r\nFrom: a@example.org\r\n\r\nbody\r\n";
        assert!(lossless(bytes).is_message());
    }

    #[test]
    fn with_one_boundary_on_two_levels_the_outer_level_splits() {
        let bytes = b"Content-Type: multipart/mixed; boundary=b\r\n\r\n\
            --b\r\n\
            Content-Type: multipart/alternative; boundary=b\r\n\
            \r\n\
            --b\r\n\
            \r\n\
            inner\r\n\
            --b--\r\n";
        let root = lossless(bytes);
        let parts = parts(&root);

        assert_eq!(parts.len(), 2, "every --b line belongs to the outer level");
        assert!(matches!(parts[0].content, Content::Single));
    }

    #[test]
    fn empty_parts_and_mixed_line_breaks() {
        let bytes = b"Content-Type: multipart/mixed; boundary=b\n\n--b\r\n--b\n\r\nbody\n--b--";
        let root = lossless(bytes);
        let parts = parts(&root);

        assert_eq!(parts.len(), 2);
        assert!(parts[0].header.is_empty() && parts[0].body.is_empty());
        assert_eq!(
            at(bytes, parts[0].body.end..parts[1].header.start),
            b"--b\n",
            "the line break before it belongs to the previous delimiter"
        );
        assert_eq!(at(bytes, parts[1].header.end..parts[1].body.start), b"\r\n");
        assert_eq!(at(bytes, parts[1].body.clone()), b"body");
        assert_eq!(at(bytes, parts[1].body.end..root.body.end), b"\n--b--");
    }

    #[test]
    fn deep_nesting_stops_at_the_limit() {
        let depth = 3 * MAX_DEPTH;
        let mut bytes = Vec::new();
        for level in 0..depth {
            bytes.extend_from_slice(
                format!("Content-Type: multipart/mixed; boundary=b{level}\r\n\r\n--b{level}\r\n")
                    .as_bytes(),
            );
        }
        bytes.extend_from_slice(b"\r\ninnermost\r\n");
        let root = lossless(&bytes);

        let mut levels = 0;
        let mut entity = &root;
        while let Content::Multipart(parts) = &entity.content {
            levels += 1;
            entity = &parts[0];
        }
        assert_eq!(levels, MAX_DEPTH);
    }

    #[test]
    fn random_messages_keep_every_byte_in_place() {
        let (mut split, mut nested) = (0, 0);
        for bytes in random_messages(5_000) {
            if let Content::Multipart(parts) = lossless(&bytes).content {
                split += usize::from(parts.len() > 1);
                nested +=
                    usize::from(parts.iter().any(|part| {
                        matches!(part.content, Content::Multipart(_) | Content::Message)
                    }));
            }
        }
        assert!(split > 500, "only {split} split into several parts");
        assert!(
            nested > 100,
            "only {nested} with a multipart or message inside"
        );
    }

    #[test]
    fn decoding_follows_the_declared_encoding() {
        let decoded = |header: &[u8], body: &[u8]| {
            let bytes = [header, b"\r\n", body].concat();
            parse(&bytes).decoded(&bytes).into_owned()
        };
        assert_eq!(
            decoded(
                b"Content-Transfer-Encoding: base64\r\n",
                b"PHA+5DwvcD4=\r\n"
            ),
            b"<p>\xe4</p>"
        );
        assert_eq!(
            decoded(
                b"Content-Transfer-Encoding: Quoted-Printable \r\n",
                b"Gr=FC=DFe =\r\naus M=FCnchen   "
            ),
            b"Gr\xfc\xdfe aus M\xfcnchen"
        );
        assert_eq!(
            decoded(b"Content-Transfer-Encoding: 8bit\r\n", b"G\xfcnter\r\n"),
            b"G\xfcnter\r\n"
        );
        assert_eq!(
            decoded(b"Content-Transfer-Encoding: x-unknown\r\n", b"=E4 QUJD\r\n"),
            b"=E4 QUJD\r\n"
        );
    }

    #[test]
    fn base64_is_read_leniently() {
        assert_eq!(base64(b"QUJDREVG"), b"ABCDEF");
        assert_eq!(
            base64(b"QU\r\nJD RE\tVG\r\n"),
            b"ABCDEF",
            "whitespace between"
        );
        assert_eq!(base64(b"QUJDRA=="), b"ABCD");
        assert_eq!(base64(b"QUJDREU="), b"ABCDE");
        assert_eq!(base64(b"QUJDRA"), b"ABCD", "padding missing");
        assert_eq!(base64(b"QUJDREU"), b"ABCDE", "padding missing");
        assert_eq!(base64(b"QUJDR"), b"ABC", "one character holds no byte");
        assert_eq!(
            base64(b"QUJD!RA==\r\n"),
            b"ABCD",
            "characters outside the alphabet"
        );
        assert_eq!(
            base64(b"QUJDRA==QUJD"),
            b"ABCDABC",
            "padding ends a group, the next block follows"
        );
        assert_eq!(base64(b"QUJD=QUJD"), b"ABCABC");
        assert_eq!(base64(b"===="), b"");
        assert_eq!(base64(b""), b"");
    }

    #[test]
    fn quoted_printable_is_read_leniently() {
        assert_eq!(
            quoted_printable(b"a=3Db =E4 =e4"),
            b"a=b \xe4 \xe4",
            "either case"
        );
        assert_eq!(
            quoted_printable(b"soft=\r\nbreak=\nand LF"),
            b"softbreakand LF"
        );
        assert_eq!(
            quoted_printable(b"soft= \t\r\nbreak"),
            b"softbreak",
            "padding after a soft line break"
        );
        assert_eq!(
            quoted_printable(b"x  \r\ny\t\nlast  "),
            b"x\r\ny\nlast",
            "padding at the end of every line"
        );
        assert_eq!(
            quoted_printable(b"=E4="),
            b"\xe4",
            "a soft break at the end"
        );
        assert_eq!(quoted_printable(b"=XY a=4g"), b"=XY a=4g", "no escape");
        assert_eq!(
            quoted_printable(b"cut =4"),
            b"cut =4",
            "an escape cut short"
        );
        assert_eq!(quoted_printable(b"a= b"), b"a= b", "= inside a line");
        assert_eq!(quoted_printable(b"a==b"), b"a==b");
        assert_eq!(
            quoted_printable(b"====\r\nnext"),
            b"===next",
            "the last = of a line is a soft break"
        );
        assert_eq!(
            quoted_printable(b"lone\rcr\r\n"),
            b"lone\rcr\r\n",
            "line breaks stay as they are"
        );
        assert_eq!(quoted_printable(b""), b"");
    }

    #[test]
    fn uuencode_is_read_from_begin_to_end() {
        let data = [
            &b"\x00\x00\x00ABC\xff\xfe"[..],
            &(0u8..40).collect::<Vec<_>>(),
        ]
        .concat();
        let spaces = b"M    04)#__X  0(#! 4&!P@)\"@L,#0X/$!$2$Q05%A<8&1H;'!T>'R A(B,D\n#)28G\n";
        let backticks = b"M````04)#__X``0(#!`4&!P@)\"@L,#0X/$!$2$Q05%A<8&1H;'!T>'R`A(B,D\n#)28G\n";
        for (lines, line_break) in [(&spaces[..], &b"\r\n"[..]), (backticks, b"\n")] {
            let encoded = [
                &b"begin 644 data.bin"[..],
                line_break,
                &lines
                    .split(|&byte| byte == b'\n')
                    .collect::<Vec<_>>()
                    .join(line_break),
                b"`",
                line_break,
                b"end",
                line_break,
            ]
            .concat();
            assert_eq!(uudecode(&encoded).unwrap(), data);
        }
        assert_eq!(
            uudecode(b"begin 644 x\n$\nend\n").unwrap(),
            b"\0\0\0\0",
            "the spaces of a line stripped in transport are zeros"
        );
        assert_eq!(
            uudecode(b"some text\nbegin here we go\nbegin 600 x\n\n#0V%T\n\nend \nmore text\n")
                .unwrap(),
            b"Cat",
            "before begin, empty lines, after end"
        );
        assert_eq!(
            uudecode(b"begin 644 x\r\n#0V%T\r\n").unwrap(),
            b"Cat",
            "no end"
        );
        assert_eq!(uudecode(b"#0V%T\nend\n"), None, "no begin");
    }

    #[test]
    fn uuencode_goes_by_several_names() {
        for name in ["x-uuencode", "X-UUENCODE", "uuencode", "x-uue", "uue"] {
            let bytes =
                format!("Content-Transfer-Encoding: {name}\r\n\r\nbegin 644 x\r\n#0V%T\r\nend\r\n");
            assert_eq!(
                parse(bytes.as_bytes()).decoded(bytes.as_bytes()).as_ref(),
                b"Cat",
                "{name}"
            );
        }
        let bytes = b"Content-Transfer-Encoding: x-uuencode\r\n\r\nno begin line\r\n";
        assert_eq!(
            parse(bytes).decoded(bytes).as_ref(),
            b"no begin line\r\n",
            "without data the body stays as it is"
        );
    }

    // The tests below pin what this module takes from mail-parser. One that
    // fails after an update of mail-parser means an assumption no longer
    // holds, not that the test needs new values.

    #[test]
    fn mail_parser_reads_the_fields() {
        let bytes = b"Content-Type: Text/CSV; charset=windows-1252;\r\n name=\"list.csv\"\r\n\
            Content-ID: <part@example.org>\r\n\
            Content-Transfer-Encoding:\r\n BASE64\r\n\
            \r\n\
            Content-Type: image/png\r\n";
        let root = lossless(bytes);

        assert_eq!(root.fields.content_type.as_deref(), Some("text/csv"));
        assert_eq!(root.fields.name.as_deref(), Some("list.csv"));
        assert_eq!(root.fields.content_id.as_deref(), Some("part@example.org"));
        assert_eq!(root.fields.encoding, Encoding::Base64);
        assert_eq!(
            parse(b"Content-Type: multipart/mixed;\r\n boundary=\"=_a b\"\r\n\r\n")
                .fields
                .boundary
                .as_deref(),
            Some("=_a b")
        );
    }

    #[test]
    fn a_parameter_may_start_on_the_next_line() {
        let bytes = b"Content-Type: multipart/mixed; boundary=\r\n\t\"cut\"\r\n\
            \r\n\
            --cut\r\n\
            Content-Type: image/jpeg; name=\r\n\t\"type.jpg\"\r\n\
            \r\n\
            --cut\r\n\
            Content-Disposition: inline; filename\r\n =\"disposition.jpg\"\r\n\
            \r\n\
            --cut--\r\n";
        let root = lossless(bytes);
        let parts = parts(&root);

        assert_eq!(root.fields.boundary.as_deref(), Some("cut"));
        assert_eq!(parts[0].fields.name.as_deref(), Some("type.jpg"));
        assert_eq!(parts[1].fields.name.as_deref(), Some("disposition.jpg"));
    }

    fn name(header: &[u8]) -> Option<String> {
        let bytes = [header, b"\r\nbody"].concat();
        parse(&bytes).fields.name
    }

    #[test]
    fn mail_parser_reads_file_names() {
        assert_eq!(
            name(
                b"Content-Disposition: attachment;\r\n \
                  filename*0*=utf-8''Gr%C3%BC%C3%9Fe;\r\n filename*1*=%20aus.txt\r\n"
            )
            .as_deref(),
            Some("Gr\u{fc}\u{df}e aus.txt"),
            "RFC 2231: continuations, charset, percent encoding"
        );
        assert_eq!(
            name(b"Content-Type: application/pdf; name=\"=?UTF-8?Q?Gr=C3=BC=C3=9Fe.pdf?=\"\r\n")
                .as_deref(),
            Some("Gr\u{fc}\u{df}e.pdf"),
            "an encoded word in quotes, as Outlook writes it"
        );
        assert_eq!(
            name(
                b"Content-Type: application/pdf; name=\"type.pdf\"\r\n\
                  Content-Disposition: attachment; filename=\"disposition.pdf\"\r\n"
            )
            .as_deref(),
            Some("disposition.pdf"),
            "filename before name"
        );
        assert_eq!(name(b"Content-Type: text/plain\r\n"), None);
    }
}
