//! The values of the recorded headers, in the form they are recorded in.
//!
//! The message stays in the archive, so a value is written in the form
//! it can be searched by: a date in RFC 3339, one claim per address and
//! per Message-ID. The form changes, the content does not; a value that
//! cannot be read in this form is not recorded, and the reason is
//! reported instead. The one change of content is the lower case of an
//! address: the same address in two spellings would otherwise be two
//! values.

use mail_parser::parsers::MessageStream;
use mail_parser::{Addr, HeaderValue};
use ossuary_extract_mail::{message_id, message_ids};

use crate::date;

/// The headers recorded under `mail:`, by their name in lower case. The
/// transport headers (received, return-path, the x- headers) describe
/// the delivery, not the message, and are not recorded.
pub const RECORDED: [&str; 11] = [
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
];

/// The values of one recorded header, each a value to record or, for a
/// part of the header that is not recorded, the reason and the text of
/// that part (`not a date: irgendwann`). `name` is one of [`RECORDED`],
/// `raw` the header's value as it is in the message. A header with
/// nothing in it has no values and no reasons.
pub fn read(name: &str, raw: &[u8]) -> Vec<Result<String, String>> {
    let Some(text) = unfold(raw) else {
        return Vec::new();
    };
    match name {
        "date" => vec![date::rfc3339(&text).ok_or_else(|| unreadable("not a date", &text))],
        "message-id" => vec![
            message_id(&text)
                .map(str::to_string)
                .ok_or_else(|| unreadable("no message id", &text)),
        ],
        "in-reply-to" | "references" => {
            let ids = message_ids(&text);
            if ids.is_empty() {
                vec![Err(unreadable("no message id", &text))]
            } else {
                ids.into_iter().map(|id| Ok(id.to_string())).collect()
            }
        }
        "subject" => vec![Ok(text)],
        _ => entries(raw).into_iter().flat_map(mailboxes).collect(),
    }
}

/// The reason and the text of a part that is not recorded. The text is
/// decoded but otherwise as in the message, with each run of whitespace
/// shown as one space and control characters escaped, so that it can be
/// found in the message. A long text is cut.
fn unreadable(reason: &str, text: &str) -> String {
    const AT_MOST: usize = 200;
    let shown: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .map(|c| {
            if c.is_control() {
                c.escape_default().to_string()
            } else {
                c.to_string()
            }
        })
        .collect();
    match shown.char_indices().nth(AT_MOST) {
        Some((cut, _)) => format!("{reason}: {}…", &shown[..cut]),
        None => format!("{reason}: {shown}"),
    }
}

/// A header's raw value unfolded, the line breaks read as the single
/// space they stand for, and with its RFC 2047 encoded words decoded in
/// the charset each word names. A byte no charset accounts for becomes
/// U+FFFD. `None` for a value that is empty or only whitespace.
pub fn unfold(raw: &[u8]) -> Option<String> {
    let mut data = raw.to_vec();
    if data.last() != Some(&b'\n') {
        data.push(b'\n');
    }
    match MessageStream::new(&data).parse_unstructured() {
        HeaderValue::Text(text) if !text.trim().is_empty() => Some(text.into_owned()),
        _ => None,
    }
}

/// The entries of an address list: the raw value split at the commas
/// that are not inside quotes, a comment, angle brackets or an encoded
/// word. A group (`Team: a@example.com, b@example.com;`) gives its
/// members, without the group's name. Empty entries are left out.
///
/// mail-parser reads a whole list, but gives for each address only the
/// name and address as it read them, not the text of the entry. For an
/// entry that cannot be read, that text is what the report has to show.
fn entries(raw: &[u8]) -> Vec<&[u8]> {
    let mut entries = Vec::new();
    let mut start = 0;
    let mut at = 0;
    let (mut quoted, mut comment, mut angle) = (false, 0usize, false);
    while at < raw.len() {
        let byte = raw[at];
        if quoted {
            match byte {
                b'\\' => at += 1,
                b'"' => quoted = false,
                _ => {}
            }
        } else if comment > 0 {
            match byte {
                b'\\' => at += 1,
                b'(' => comment += 1,
                b')' => comment -= 1,
                _ => {}
            }
        } else if let Some(length) = encoded_word(&raw[at..]) {
            at += length;
            continue;
        } else {
            match byte {
                b'"' => quoted = true,
                b'(' => comment = 1,
                b'<' => angle = true,
                b'>' => angle = false,
                b',' | b';' if !angle => {
                    entries.push(&raw[start..at]);
                    start = at + 1;
                }
                b':' if !angle => start = at + 1,
                _ => {}
            }
        }
        at += 1;
    }
    entries.push(&raw[start.min(raw.len())..]);
    entries.retain(|entry| !entry.iter().all(u8::is_ascii_whitespace));
    entries
}

/// The length of the RFC 2047 encoded word at the start of `bytes`, if
/// one starts there and can be decoded.
fn encoded_word(bytes: &[u8]) -> Option<usize> {
    if !bytes.starts_with(b"=?") {
        return None;
    }
    let mut stream = MessageStream::new(&bytes[1..]);
    stream.decode_rfc2047().map(|_| 1 + stream.offset())
}

/// The text with every encoded word decoded where it stands, and nothing
/// else changed. Whitespace between two encoded words is dropped, as RFC
/// 2047 (6.2) says. mail-parser's unstructured reading would also put
/// spaces around an encoded word in angle brackets.
fn decoded(raw: &[u8]) -> String {
    let mut text = String::new();
    let mut after_word = false;
    let mut at = 0;
    let mut plain_start = 0;
    while at < raw.len() {
        if raw[at..].starts_with(b"=?") {
            let mut stream = MessageStream::new(&raw[at + 1..]);
            if let Some(word) = stream.decode_rfc2047() {
                let plain = &raw[plain_start..at];
                if !(after_word && plain.iter().all(u8::is_ascii_whitespace)) {
                    text.push_str(&String::from_utf8_lossy(plain));
                }
                text.push_str(&word);
                at += 1 + stream.offset();
                plain_start = at;
                after_word = true;
                continue;
            }
        }
        at += 1;
    }
    text.push_str(&String::from_utf8_lossy(&raw[plain_start..]));
    text
}

/// The text outside the encoded words.
fn outside_words(raw: &[u8]) -> Vec<u8> {
    let mut outside = Vec::new();
    let mut at = 0;
    while at < raw.len() {
        if let Some(length) = encoded_word(&raw[at..]) {
            at += length;
        } else {
            outside.push(raw[at]);
            at += 1;
        }
    }
    outside
}

/// The addresses of one entry of an address list, or the reason it gives
/// none.
fn mailboxes(entry: &[u8]) -> Vec<Result<String, String>> {
    let mut data = entry.to_vec();
    data.push(b'\n');
    let parsed = MessageStream::new(&data).parse_address();
    let addrs: Vec<&Addr<'_>> = match &parsed {
        HeaderValue::Address(address) => address.iter().collect(),
        _ => Vec::new(),
    };
    let unread = || unreadable(reason(entry), &decoded(entry));
    if addrs.is_empty() {
        return vec![Err(unread())];
    }
    addrs
        .into_iter()
        .map(|addr| mailbox(addr).ok_or_else(unread))
        .collect()
}

/// Why an entry of an address list gives no address. An encoded word
/// (RFC 2047) may only stand for text such as a name, never for an
/// address. Decoded, an address that was encoded looks valid, so the
/// reason says where the encoded word was: inside the angle brackets
/// (`<=?iso-8859-1?Q?a=40example.com?=>`), or as the whole entry with no
/// angle brackets outside it, which makes all of it a name.
fn reason(entry: &[u8]) -> &'static str {
    let outside = outside_words(entry);
    if !outside.contains(&b'@') && decoded(entry).contains('@') {
        return if outside.contains(&b'<') {
            "the address in angle brackets is MIME-encoded, which RFC 2047 does not allow"
        } else {
            "the address is part of a MIME-encoded name, so there is no address"
        };
    }
    let angle = outside.iter().position(|&byte| byte == b'<').map(|open| {
        let rest = &outside[open + 1..];
        &rest[..rest
            .iter()
            .position(|&byte| byte == b'>')
            .unwrap_or(rest.len())]
    });
    match angle {
        Some(inner) if inner.iter().all(u8::is_ascii_whitespace) => "the address is empty",
        Some(_) => "not an address",
        None if outside.contains(&b'@') => "not an address",
        None => "no address",
    }
}

/// One address as `Name <address>`, or `<address>` without a name: the
/// address in lower case, the name decoded and without the quotes of
/// the header. A comment after the address (`a@example.com (Name)`)
/// counts as the name, as it did before display names existed. `None`
/// when mail-parser found no address, or one that [`is_address`] does
/// not accept.
fn mailbox(addr: &Addr<'_>) -> Option<String> {
    let name = addr.name().map(str::trim).filter(|name| !name.is_empty());
    let address = addr.address().map(str::trim).filter(|a| is_address(a))?;
    let address = address.to_lowercase();
    Some(match name {
        Some(name) => format!("{name} <{address}>"),
        None => format!("<{address}>"),
    })
}

/// Whether the text is an address: a local part and a domain around the
/// last `@`, neither empty, and no whitespace except inside a quoted
/// local part (`"0123 456-789"@fax.example.com`, as fax gateways write
/// a phone number). A local name without a domain counts as well
/// (`root`, `MAILER-DAEMON`), as a local mail system writes it in angle
/// brackets, although RFC 5322 has no address without a domain. Such a
/// name may contain only letters, digits, `.`, `-`, `_` and `+`, the
/// characters of user names, so that a broken encoded word
/// (`=?iso-8859-1?Q?…`) is not taken for one.
fn is_address(text: &str) -> bool {
    let Some((local, domain)) = text.rsplit_once('@') else {
        let named = |c: char| c.is_alphanumeric() || ".-_+".contains(c);
        return !text.is_empty() && text.chars().all(named);
    };
    let spaced = |part: &str| part.contains(char::is_whitespace);
    let quoted = local.len() >= 2 && local.starts_with('"') && local.ends_with('"');
    !local.is_empty() && !domain.is_empty() && !spaced(domain) && (quoted || !spaced(local))
}

#[cfg(test)]
mod tests {
    use mail_parser::MessageParser;

    use super::*;

    /// What [`read`] makes of one header line, as mail-parser would hand
    /// it over from a message.
    fn read_line(line: &str) -> Vec<Result<String, String>> {
        let message = format!("{line}\r\n\r\n");
        let parsed = MessageParser::default()
            .parse_headers(message.as_bytes())
            .unwrap();
        let header = &parsed.headers()[0];
        let raw = &message.as_bytes()[header.offset_start as usize..header.offset_end as usize];
        read(&header.name.as_str().to_lowercase(), raw)
    }

    fn nothing() -> Vec<Result<String, String>> {
        Vec::new()
    }

    fn values(line: &str) -> Vec<String> {
        read_line(line).into_iter().map(Result::unwrap).collect()
    }

    #[test]
    fn unfold_joins_folded_lines_and_decodes_encoded_words() {
        assert_eq!(
            unfold(b" alice <a@example.com>,\r\n bob <b@example.com>\r\n"),
            Some("alice <a@example.com>, bob <b@example.com>".to_string())
        );
        assert_eq!(
            unfold(b" =?ISO-8859-1?Q?Gr=FC=DFe?= aus dem Test\r\n"),
            Some("Gr\u{fc}\u{df}e aus dem Test".to_string())
        );
        assert_eq!(unfold(b"   \r\n"), None);
    }

    #[test]
    fn the_date_is_recorded_in_rfc_3339() {
        assert_eq!(
            values("Date: Fri, 4 Sep 2026 12:34:56 +0200"),
            ["2026-09-04T10:34:56Z"]
        );
        assert_eq!(
            read_line("Date: irgendwann"),
            [Err("not a date: irgendwann".to_string())]
        );
        assert_eq!(
            read_line("Date: "),
            nothing(),
            "an empty header is no error"
        );
    }

    #[test]
    fn each_address_is_one_value_with_its_name() {
        assert_eq!(
            values(
                "To: \"Doe, John\" <J.Doe@Example.COM>, =?ISO-8859-1?Q?J=FCrgen?= <j@example.net>"
            ),
            [
                "Doe, John <j.doe@example.com>",
                "J\u{fc}rgen <j@example.net>"
            ]
        );
        assert_eq!(values("From: erika@example.org"), ["<erika@example.org>"]);
        assert_eq!(
            values("From: erika@example.org (Erika Muster)"),
            ["Erika Muster <erika@example.org>"]
        );
        assert_eq!(
            values("Cc: \"\" <a@example.org>"),
            ["<a@example.org>"],
            "an empty name is no name"
        );
        assert_eq!(
            values("From: Fax <\"0123 456-789\"@Fax.example.com>"),
            ["Fax <\"0123 456-789\"@fax.example.com>"],
            "a quoted local part may contain a space"
        );
    }

    #[test]
    fn a_group_gives_its_members_without_the_group_name() {
        assert_eq!(
            values("To: Team: a@example.com, B <b@example.com>;, c@example.com"),
            ["<a@example.com>", "B <b@example.com>", "<c@example.com>"]
        );
        assert_eq!(read_line("To: undisclosed-recipients:;"), nothing());
    }

    #[test]
    fn an_entry_without_an_address_is_reported() {
        assert_eq!(
            read_line("To: <John Doe>, a@example.org"),
            [
                Err("not an address: <John Doe>".to_string()),
                Ok("<a@example.org>".to_string()),
            ]
        );
        assert_eq!(
            read_line("To: John Doe"),
            [Err("no address: John Doe".to_string())]
        );
        assert_eq!(
            values("From: <Root>"),
            ["<root>"],
            "a local name without a domain"
        );
        assert_eq!(
            read_line("To: root"),
            [Err("no address: root".to_string())],
            "only in angle brackets"
        );
        assert_eq!(
            read_line("To: undisclosed-recipients: <;>"),
            [Err("not an address: <;>".to_string())]
        );
        assert_eq!(
            read_line("From: <>"),
            [Err("the address is empty: <>".to_string())]
        );
    }

    #[test]
    fn a_mime_encoded_address_is_reported_decoded() {
        assert_eq!(
            read_line(
                "Cc: =?iso-8859-1?Q?M=FCller=40example=2Ecom?=\r\n\
                 \x20   <=?iso-8859-1?Q?M=FCller=40example=2Ecom?=>, b@example.com"
            ),
            [
                Err(concat!(
                    "the address in angle brackets is MIME-encoded, which RFC 2047 does not allow: ",
                    "M\u{fc}ller@example.com <M\u{fc}ller@example.com>"
                )
                .to_string()),
                Ok("<b@example.com>".to_string()),
            ]
        );
        assert_eq!(
            read_line(
                "From: =?iso-8859-1?Q?=22Soci=E9t=E9_Exemple=22_=3Cinfo=40example=2Ecom=3E?="
            ),
            [Err(concat!(
                "the address is part of a MIME-encoded name, so there is no address: ",
                "\"Soci\u{e9}t\u{e9} Exemple\" <info@example.com>"
            )
            .to_string())],
            "the whole entry as one encoded word"
        );
        assert_eq!(
            read_line("To: =?bogus?X?garbage?= <=?iso-8859-1?Q?broken"),
            [Err(
                "not an address: =?bogus?X?garbage?= <=?iso-8859-1?Q?broken".to_string()
            )],
            "what cannot be decoded is shown as written"
        );
    }

    #[test]
    fn entries_split_only_at_commas_that_separate_them() {
        let split = |raw: &str| -> Vec<String> {
            entries(raw.as_bytes())
                .into_iter()
                .map(|entry| String::from_utf8_lossy(entry).trim().to_string())
                .collect()
        };
        assert_eq!(
            split(" \"Doe, John\" <a@x>, (one, two) b@y, <c@x,d@y>, =?utf-8?Q?A,_B?= <e@z>"),
            [
                "\"Doe, John\" <a@x>",
                "(one, two) b@y",
                "<c@x,d@y>",
                "=?utf-8?Q?A,_B?= <e@z>"
            ]
        );
        assert_eq!(split(" Team: a@x, b@y;, c@z,"), ["a@x", "b@y", "c@z"]);
        assert_eq!(split(" undisclosed-recipients:;"), Vec::<String>::new());
    }

    #[test]
    fn a_long_text_is_cut_in_the_report() {
        let reported = unreadable("not a date", &"x".repeat(500));
        assert_eq!(reported, format!("not a date: {}\u{2026}", "x".repeat(200)));
        assert_eq!(unreadable("not a date", "a\u{7}b"), "not a date: a\\u{7}b");
    }

    #[test]
    fn a_message_id_loses_its_brackets_and_nothing_else() {
        assert_eq!(
            values("Message-ID: <Abc.1@Example.org>"),
            ["Abc.1@Example.org"]
        );
        assert_eq!(
            values("Message-ID:  < abc@example.org >  "),
            ["abc@example.org"]
        );
        assert_eq!(values("Message-ID: abc@example.org"), ["abc@example.org"]);
        assert_eq!(
            values("Message-ID: <no closing bracket@example.org"),
            ["no closing bracket@example.org"]
        );
        assert_eq!(
            values("Message-ID: <a@example.org> <b@example.org>"),
            ["a@example.org"]
        );
        assert_eq!(
            read_line("Message-ID: <>"),
            [Err("no message id: <>".to_string())]
        );
    }

    #[test]
    fn references_give_one_value_per_id() {
        assert_eq!(
            values("References: <a@x> <b@y>,<c@z>\r\n <d@w"),
            ["a@x", "b@y", "c@z", "d@w"]
        );
        assert_eq!(
            values("In-Reply-To: <a@x> (Your message of Fri, 4 Sep 2026)"),
            ["a@x"]
        );
        assert_eq!(values("In-Reply-To: <a@x <b@y>"), ["a@x", "b@y"]);
        assert_eq!(
            values("In-Reply-To: 1a2b-3c@mail.example.com"),
            ["1a2b-3c@mail.example.com"],
            "one word without brackets is one id"
        );
        assert_eq!(values("References: 1700000000000"), ["1700000000000"]);
        assert_eq!(
            read_line("In-Reply-To: Your message of Fri, 4 Sep 2026"),
            [Err(
                "no message id: Your message of Fri, 4 Sep 2026".to_string()
            )]
        );
    }

    #[test]
    fn the_subject_is_recorded_as_written() {
        assert_eq!(
            values("Subject: Re: =?UTF-8?Q?=C3=9Cber?= den Test"),
            ["Re: \u{dc}ber den Test"]
        );
    }
}
