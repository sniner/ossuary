//! Folder names on the wire: IMAP's modified UTF-7 (RFC 3501, 5.1.3).
//!
//! A server lists its folders in this encoding and expects it back in
//! every command that names one. Printable ASCII stands for itself,
//! except `&`, which becomes `&-`; every run of other characters
//! becomes `&`, the run as UTF-16BE in a base64 whose 64th character
//! is `,` instead of `/` and which carries no padding, then `-`.
//!
//! ```text
//! Entwürfe  <->  Entw&APw-rfe
//! R&D       <->  R&-D
//! INBOX     <->  INBOX
//! ```
//!
//! The record never sees the wire form: a name is decoded when the
//! server lists it and encoded when a folder is opened, so what stands
//! in `mailbox:place` is the name a person reads.

use anyhow::{Result, bail};

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";

/// A folder name as the server wants it.
#[must_use]
pub fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut run: Vec<u16> = Vec::new();
    let flush = |run: &mut Vec<u16>, out: &mut String| {
        if run.is_empty() {
            return;
        }
        let bytes: Vec<u8> = run.iter().flat_map(|unit| unit.to_be_bytes()).collect();
        out.push('&');
        out.push_str(&base64(&bytes));
        out.push('-');
        run.clear();
    };
    for c in name.chars() {
        match c {
            '&' => {
                flush(&mut run, &mut out);
                out.push_str("&-");
            }
            ' '..='~' => {
                flush(&mut run, &mut out);
                out.push(c);
            }
            other => {
                let mut units = [0u16; 2];
                run.extend_from_slice(other.encode_utf16(&mut units));
            }
        }
    }
    flush(&mut run, &mut out);
    out
}

/// A folder name as a person reads it, from what the server listed.
///
/// # Errors
///
/// A `&` never closed by `-`, a character outside the encoding's
/// base64, or an encoded run that is not UTF-16.
pub fn decode(wire: &str) -> Result<String> {
    let mut out = String::with_capacity(wire.len());
    let mut rest = wire;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let Some(end) = after.find('-') else {
            bail!("{wire:?}: a '&' that is never closed by '-'");
        };
        let run = &after[..end];
        if run.is_empty() {
            out.push('&');
        } else {
            let bytes = unbase64(run)
                .ok_or_else(|| anyhow::anyhow!("{wire:?}: {run:?} is not the encoding's base64"))?;
            if bytes.len() % 2 != 0 {
                bail!("{wire:?}: {run:?} does not decode to whole UTF-16 units");
            }
            let units: Vec<u16> = bytes
                .chunks(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                .collect();
            let text = String::from_utf16(&units)
                .map_err(|_| anyhow::anyhow!("{wire:?}: {run:?} is not UTF-16"))?;
            out.push_str(&text);
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The encoding's base64: `,` for the 64th character, no padding.
fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut triple = [0u8; 3];
        triple[..chunk.len()].copy_from_slice(chunk);
        let bits = u32::from(triple[0]) << 16 | u32::from(triple[1]) << 8 | u32::from(triple[2]);
        let symbols = chunk.len() + 1;
        for i in 0..symbols {
            let index = (bits >> (18 - 6 * i)) & 0x3F;
            out.push(char::from(ALPHABET[index as usize]));
        }
    }
    out
}

/// `None` for a character outside the alphabet, or a length no
/// unpadded base64 can have.
fn unbase64(text: &str) -> Option<Vec<u8>> {
    if text.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    for chunk in text.as_bytes().chunks(4) {
        let mut bits: u32 = 0;
        for (i, &symbol) in chunk.iter().enumerate() {
            let value = ALPHABET.iter().position(|&a| a == symbol)?;
            // The alphabet has 64 entries; a position always fits.
            bits |= u32::from(u8::try_from(value).ok()?) << (18 - 6 * i);
        }
        let bytes = bits.to_be_bytes();
        out.extend_from_slice(&bytes[1..chunk.len()]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_stands_for_itself_and_the_ampersand_is_escaped() {
        assert_eq!(encode("INBOX"), "INBOX");
        assert_eq!(encode("[Google Mail]/Gesendet"), "[Google Mail]/Gesendet");
        assert_eq!(encode("R&D"), "R&-D");
        assert_eq!(decode("R&-D").unwrap(), "R&D");
    }

    #[test]
    fn other_characters_become_a_base64_run() {
        assert_eq!(encode("Entwürfe"), "Entw&APw-rfe");
        assert_eq!(decode("Entw&APw-rfe").unwrap(), "Entwürfe");
        // RFC 3501's own example.
        assert_eq!(
            encode("~peter/mail/台北/日本語"),
            "~peter/mail/&U,BTFw-/&ZeVnLIqe-"
        );
        assert_eq!(
            decode("~peter/mail/&U,BTFw-/&ZeVnLIqe-").unwrap(),
            "~peter/mail/台北/日本語"
        );
    }

    #[test]
    fn a_character_beyond_the_basic_plane_takes_two_units() {
        assert_eq!(encode("📁"), "&2D3cwQ-");
        assert_eq!(decode("&2D3cwQ-").unwrap(), "📁");
    }

    #[test]
    fn everything_round_trips() {
        for name in [
            "",
            "a",
            "&",
            "&&",
            "ä&ö",
            "Gelöscht/Papierkorb",
            "日本語 & more",
        ] {
            assert_eq!(decode(&encode(name)).unwrap(), name, "{name:?}");
        }
    }

    #[test]
    fn what_no_server_should_send_is_refused_by_name() {
        assert!(decode("Entw&APw").is_err(), "never closed");
        assert!(decode("&A*w-").is_err(), "outside the alphabet");
        assert!(decode("&AP-").is_err(), "not whole units");
        assert!(
            decode("&2D0-").is_err(),
            "an unpaired surrogate is not UTF-16"
        );
    }
}
