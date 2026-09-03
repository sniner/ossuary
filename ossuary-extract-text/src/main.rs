//! The text extractor: PDFs in, plain text out — as a derived file.
//!
//! Speaks the ossuary extractor protocol (`docs/extractors.md`): called
//! with `--identify` it says who it is, that it reads `application/pdf`,
//! and that it derives files; called with an output directory as its
//! argument it reads one file's bytes from stdin, writes the extracted
//! text as `text.txt` into that directory, and announces it on stdout —
//! beside whatever the document's own info dictionary had to say,
//! verbatim under `pdf:`.
//!
//! The extraction engine is the system's `pdftotext` (poppler), spoken
//! to over pipes the way ossuary speaks to this program. Its version is
//! deliberately not part of this extractor's source: re-examination
//! follows deliberate version bumps here, not the system's update
//! cadence — `ossuary extract text --full` is the lever for the rare
//! poppler leap that warrants a fresh look.
//!
//! A document with no text to give — scanned pages, an empty harvest, a
//! PDF pdftotext cannot open — is an examination like any other, with
//! no file to announce: exit 0. Only the environment failing (no
//! pdftotext, a broken pipe world) is a failure.

use std::io::{Read, Write as _};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use serde_json::json;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        Some("--identify") => identify(),
        Some(directory) if arguments.len() == 1 && !directory.starts_with('-') => {
            examine(Path::new(directory))
        }
        _ => {
            eprintln!(
                "ossuary-extract-text: run with --identify, or with the output directory as the only argument and a file's bytes on stdin"
            );
            ExitCode::FAILURE
        }
    }
}

/// Who this extractor is — answered only when its engine is actually
/// there: a missing pdftotext fails loudly here, once, instead of
/// quietly on every file.
fn identify() -> ExitCode {
    if !pdftotext_present() {
        eprintln!(
            "no `pdftotext` on PATH — this extractor drives poppler; install it (Homebrew: poppler, Debian: poppler-utils), then run this again"
        );
        return ExitCode::FAILURE;
    }
    println!(
        "{}",
        json!({
            "ossuary-extractor": 1,
            "source": format!("extractor:text/{}", env!("CARGO_PKG_VERSION")),
            "mimes": ["application/pdf"],
            "derives": true,
        })
    );
    ExitCode::SUCCESS
}

/// One document: info dictionary onto stdout, text into the directory.
fn examine(directory: &Path) -> ExitCode {
    let mut bytes = Vec::new();
    if let Err(error) = std::io::stdin().lock().read_to_end(&mut bytes) {
        eprintln!("ossuary-extract-text: reading stdin: {error}");
        return ExitCode::FAILURE;
    }
    for (attribute, value) in document_info(&bytes) {
        println!("{}", json!({ "attribute": attribute, "value": value }));
    }
    let text = match pdftotext(bytes) {
        Ok(Harvest::Text(text)) => text,
        Ok(Harvest::Refused) => return ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ossuary-extract-text: {error}");
            return ExitCode::FAILURE;
        }
    };
    if blank(&text) {
        return ExitCode::SUCCESS;
    }
    if let Err(error) = std::fs::write(directory.join("text.txt"), &text) {
        eprintln!("ossuary-extract-text: writing text.txt: {error}");
        return ExitCode::FAILURE;
    }
    println!("{}", json!({ "file": "text.txt", "mime": "text/plain" }));
    ExitCode::SUCCESS
}

/// Whether `pdftotext` answers on PATH at all. Only "not found" counts
/// as missing — any other trouble surfaces later, per file, where the
/// failure list can hold it.
fn pdftotext_present() -> bool {
    !matches!(
        Command::new("pdftotext")
            .arg("-v")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    )
}

/// What one pdftotext run yielded.
enum Harvest {
    /// The extracted text, UTF-8 as asked for.
    Text(Vec<u8>),
    /// The document's own refusal — unreadable or extraction forbidden.
    /// Deterministic, so it counts as examined: retrying will not change
    /// the document.
    Refused,
}

/// The bytes through `pdftotext -q -enc UTF-8 - -`. Quiet on purpose:
/// poppler's syntax warnings are a firehose on real-world PDFs, and the
/// exit code already says everything the record needs.
fn pdftotext(bytes: Vec<u8>) -> std::io::Result<Harvest> {
    let mut child = Command::new("pdftotext")
        .args(["-q", "-enc", "UTF-8", "-", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| {
            std::io::Error::new(error.kind(), format!("running pdftotext: {error}"))
        })?;
    let mut stdin = child.stdin.take().expect("stdin was piped");
    // pdftotext promises nothing about reading everything before it
    // writes, so the handing-over runs beside the reading — a large
    // document must not deadlock on two full pipes. A write that fails
    // because pdftotext bailed early is expected; the exit code judges.
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&bytes);
    });
    let mut text = Vec::new();
    child
        .stdout
        .take()
        .expect("stdout was piped")
        .read_to_end(&mut text)?;
    let status = child.wait()?;
    let _ = writer.join();
    if status.success() {
        Ok(Harvest::Text(text))
    } else if documents_own_fault(status.code()) {
        eprintln!(
            "ossuary-extract-text: pdftotext could not read this document ({status}) — examined, nothing found"
        );
        Ok(Harvest::Refused)
    } else {
        Err(std::io::Error::other(format!(
            "pdftotext failed ({status})"
        )))
    }
}

/// pdftotext's exit codes, sorted by whose fault they are: 1 (cannot
/// open the PDF) and 3 (the document forbids extraction) are the
/// document's own, deterministic answer. Everything else — including
/// death by signal — is the environment's trouble, and worth a retry.
fn documents_own_fault(code: Option<i32>) -> bool {
    matches!(code, Some(1 | 3))
}

/// Nothing but whitespace is nothing: no `text.txt` for it. Page breaks
/// are whitespace too — a form feed per empty page is still an empty
/// harvest.
fn blank(text: &[u8]) -> bool {
    text.iter().all(u8::is_ascii_whitespace)
}

/// The document information dictionary, verbatim under `pdf:`: the keys
/// kebab-cased (`CreationDate` → `pdf:creation-date`), the values as the
/// document spells them — a date stays `D:20190714110241+02'00'`. A key
/// that does not fit the attribute grammar is skipped rather than
/// guessed at; a document lopdf cannot open simply has no info to give,
/// and the text extraction is not asked for its opinion.
fn document_info(bytes: &[u8]) -> Vec<(String, serde_json::Value)> {
    let Ok(document) = lopdf::Document::load_mem(bytes) else {
        return Vec::new();
    };
    let Ok(info) = document.trailer.get(b"Info") else {
        return Vec::new();
    };
    let info = match info {
        lopdf::Object::Reference(id) => match document.get_object(*id) {
            Ok(object) => object,
            Err(_) => return Vec::new(),
        },
        other => other,
    };
    let Ok(dictionary) = info.as_dict() else {
        return Vec::new();
    };
    let mut findings = Vec::new();
    for (key, object) in dictionary {
        let Ok(key) = std::str::from_utf8(key) else {
            continue;
        };
        let name = kebab(key);
        if !attribute_worthy(&name) {
            continue;
        }
        let value = match object {
            lopdf::Object::String(bytes, _) => decode_text(bytes),
            lopdf::Object::Name(name) => String::from_utf8_lossy(name).into_owned(),
            _ => continue,
        };
        if value.is_empty() {
            continue;
        }
        findings.push((format!("pdf:{name}"), json!(value)));
    }
    findings
}

/// A PDF text string, decoded: UTF-16BE behind its BOM, UTF-8 behind
/// the BOM PDF 2.0 allows, `PDFDocEncoding` otherwise. Decoding is
/// conversion, not tidying — nothing is trimmed or normalized.
fn decode_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .chunks(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair.get(1).copied().unwrap_or_default()]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    bytes.iter().map(|&byte| pdf_doc_char(byte)).collect()
}

/// `PDFDocEncoding` where it leaves Latin-1 (PDF 32000-1, Annex D.3): the
/// accents at 0x18–0x1F, the punctuation block at 0x80–0x9F, the euro
/// at 0xA0. Every other byte reads as the Latin-1 it is.
fn pdf_doc_char(byte: u8) -> char {
    match byte {
        0x18 => '˘',
        0x19 => 'ˇ',
        0x1A => 'ˆ',
        0x1B => '˙',
        0x1C => '˝',
        0x1D => '˛',
        0x1E => '˚',
        0x1F => '˜',
        0x80 => '•',
        0x81 => '†',
        0x82 => '‡',
        0x83 => '…',
        0x84 => '—',
        0x85 => '–',
        0x86 => 'ƒ',
        0x87 => '⁄',
        0x88 => '‹',
        0x89 => '›',
        0x8A => '−',
        0x8B => '‰',
        0x8C => '„',
        0x8D => '“',
        0x8E => '”',
        0x8F => '‘',
        0x90 => '’',
        0x91 => '‚',
        0x92 => '™',
        0x93 => 'ﬁ',
        0x94 => 'ﬂ',
        0x95 => 'Ł',
        0x96 => 'Œ',
        0x97 => 'Š',
        0x98 => 'Ÿ',
        0x99 => 'Ž',
        0x9A => 'ı',
        0x9B => 'ł',
        0x9C => 'œ',
        0x9D => 'š',
        0x9E => 'ž',
        0x9F => '\u{FFFD}',
        0xA0 => '€',
        other => other as char,
    }
}

/// Whether a kebabbed key fits the claim grammar's attribute name:
/// lowercase `a-z`, `0-9` and `-`, no dash at either edge. A key that
/// does not is skipped, the way exif skips tags it cannot name.
fn attribute_worthy(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// `CreationDate` → `creation-date`, `ModDate` → `mod-date`: a word
/// starts at an uppercase letter after a lowercase one, and at the last
/// uppercase letter of a run when lowercase follows it.
fn kebab(name: &str) -> String {
    let characters: Vec<char> = name.chars().collect();
    let mut result = String::with_capacity(name.len() + 4);
    for (position, &character) in characters.iter().enumerate() {
        if character.is_ascii_uppercase() && position > 0 {
            let after_lower = characters[position - 1].is_ascii_lowercase()
                || characters[position - 1].is_ascii_digit();
            let before_lower = characters
                .get(position + 1)
                .is_some_and(char::is_ascii_lowercase);
            if after_lower || before_lower {
                result.push('-');
            }
        }
        result.push(character.to_ascii_lowercase());
    }
    result
}

#[cfg(test)]
mod tests {
    use lopdf::{Document, Object, StringFormat, dictionary};

    use super::*;

    #[test]
    fn keys_become_attribute_names() {
        assert_eq!(kebab("Title"), "title");
        assert_eq!(kebab("CreationDate"), "creation-date");
        assert_eq!(kebab("ModDate"), "mod-date");
        assert_eq!(kebab("GTSPDFXVersion"), "gtspdfx-version");
    }

    #[test]
    fn only_grammar_fitting_names_are_worthy() {
        assert!(attribute_worthy("creation-date"));
        assert!(!attribute_worthy(""));
        assert!(!attribute_worthy("custom key"));
        assert!(!attribute_worthy("gts_pdfx"));
        assert!(!attribute_worthy("-edge"));
    }

    #[test]
    fn pdf_strings_decode_without_tidying() {
        assert_eq!(
            decode_text(b"D:20190714110241+02'00'"),
            "D:20190714110241+02'00'"
        );
        assert_eq!(
            decode_text(&[b'a', 0x85, b'b', 0xA0]),
            "a\u{2013}b\u{20AC}",
            "PDFDocEncoding's dash and euro, not Latin-1's controls"
        );
        assert_eq!(
            decode_text(&[0xFE, 0xFF, 0x00, 0x4D, 0x00, 0xFC]),
            "M\u{FC}",
            "UTF-16BE behind its BOM"
        );
    }

    #[test]
    fn a_blank_harvest_is_blank_page_breaks_included() {
        assert!(blank(b""));
        assert!(blank(b" \n\x0c \n"));
        assert!(!blank(b" a "));
        assert!(!blank("ü".as_bytes()));
    }

    #[test]
    fn pdftotexts_own_faults_are_sorted_from_the_environments() {
        assert!(documents_own_fault(Some(1)), "cannot open the document");
        assert!(documents_own_fault(Some(3)), "extraction forbidden");
        assert!(!documents_own_fault(Some(99)));
        assert!(!documents_own_fault(None), "death by signal is no verdict");
    }

    /// A document with an info dictionary, the way a producer writes one.
    fn sample() -> Vec<u8> {
        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => Object::Array(Vec::new()),
                "Count" => 0,
            }),
        );
        let catalog = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        });
        document.trailer.set("Root", Object::Reference(catalog));
        let info = document.add_object(dictionary! {
            "Title" => Object::string_literal("Annual Report"),
            "CreationDate" => Object::string_literal("D:20190714110241+02'00'"),
            "Author" => Object::String(vec![0xFE, 0xFF, 0x00, 0x4D, 0x00, 0xFC], StringFormat::Literal),
            "Trapped" => Object::Name(b"True".to_vec()),
        });
        document.trailer.set("Info", Object::Reference(info));
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn the_info_dictionary_comes_out_verbatim() {
        let findings = document_info(&sample());
        assert!(
            findings.contains(&(
                "pdf:creation-date".to_string(),
                serde_json::json!("D:20190714110241+02'00'")
            )),
            "the document's own date spelling, apostrophes and all; got {findings:?}"
        );
        assert!(findings.contains(&("pdf:title".to_string(), serde_json::json!("Annual Report"))));
        assert!(
            findings.contains(&("pdf:author".to_string(), serde_json::json!("M\u{FC}"))),
            "UTF-16 decoded, not dumped; got {findings:?}"
        );
        assert!(
            findings.contains(&("pdf:trapped".to_string(), serde_json::json!("True"))),
            "a name value reads as its name; got {findings:?}"
        );
    }

    #[test]
    fn bytes_without_a_pdf_are_an_empty_answer_not_an_error() {
        assert_eq!(document_info(b"plain words"), Vec::new());
        assert_eq!(document_info(&[]), Vec::new());
    }
}
