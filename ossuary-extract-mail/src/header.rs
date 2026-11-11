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

/// The values of one recorded header, each a value to record or the
/// reason why a part of the header is not recorded. `name` is one of
/// [`RECORDED`], `raw` the header's value as it is in the message,
/// `parsed` the value as mail-parser read it. A header with nothing in
/// it has no values and no reasons.
pub fn read(name: &str, raw: &[u8], parsed: &HeaderValue<'_>) -> Vec<Result<String, String>> {
    let Some(text) = unfold(raw) else {
        return Vec::new();
    };
    match name {
        "date" => vec![date::rfc3339(&text).ok_or_else(|| format!("{text:?} is not a date"))],
        "message-id" => vec![
            message_id(&text)
                .map(str::to_string)
                .ok_or_else(|| format!("{text:?} contains no message id")),
        ],
        "in-reply-to" | "references" => {
            let ids = message_ids(&text);
            if ids.is_empty() {
                vec![Err(format!("{text:?} contains no message id"))]
            } else {
                ids.into_iter().map(|id| Ok(id.to_string())).collect()
            }
        }
        "subject" => vec![Ok(text)],
        _ => match parsed {
            HeaderValue::Address(address) => address.iter().map(mailbox).collect(),
            _ => vec![Err(format!("{text:?} contains no address"))],
        },
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

/// One address of an address header as `Name <address>`, or `<address>`
/// without a name: the address in lower case, the name decoded and
/// without the quotes of the header. A comment after the address
/// (`a@example.com (Name)`) counts as the name, as it did before display
/// names existed.
fn mailbox(addr: &Addr<'_>) -> Result<String, String> {
    let name = addr.name().map(str::trim).filter(|name| !name.is_empty());
    match addr.address().map(str::trim) {
        Some(address) if is_address(address) => {
            let address = address.to_lowercase();
            Ok(match name {
                Some(name) => format!("{name} <{address}>"),
                None => format!("<{address}>"),
            })
        }
        Some(address) => Err(format!("{:?} is not an address", format!("<{address}>"))),
        None => Err(format!("{:?} has no address", name.unwrap_or_default())),
    }
}

/// Whether the text is an address: a local part and a domain around the
/// last `@`, neither empty, and no whitespace except inside a quoted
/// local part (`"0123 456-789"@fax.example.com`, as fax gateways write
/// a phone number). A local name without a domain counts as well
/// (`root`, `postmaster`), as a local mail system writes it in angle
/// brackets, although RFC 5322 has no address without a domain; it may
/// contain only the characters of an RFC 5322 atom and dots.
fn is_address(text: &str) -> bool {
    let Some((local, domain)) = text.rsplit_once('@') else {
        let atom = |c: char| c.is_alphanumeric() || "!#$%&'*+-/=?^_`{|}~.".contains(c);
        return !text.is_empty() && text.chars().all(atom);
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
        read(&header.name.as_str().to_lowercase(), raw, &header.value)
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
            [Err("\"irgendwann\" is not a date".to_string())]
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
                Err("\"<John Doe>\" is not an address".to_string()),
                Ok("<a@example.org>".to_string()),
            ]
        );
        assert_eq!(
            read_line("To: John Doe"),
            [Err("\"John Doe\" has no address".to_string())]
        );
        assert_eq!(
            values("From: <Root>"),
            ["<root>"],
            "a local name without a domain"
        );
        assert_eq!(
            read_line("To: root"),
            [Err("\"root\" has no address".to_string())],
            "only in angle brackets"
        );
        assert_eq!(
            read_line("To: undisclosed-recipients: <;>"),
            [Err("\"<;>\" is not an address".to_string())]
        );
        assert_eq!(
            read_line("From: <>"),
            [Err("\"<>\" contains no address".to_string())]
        );
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
            [Err("\"<>\" contains no message id".to_string())]
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
                "\"Your message of Fri, 4 Sep 2026\" contains no message id".to_string()
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
