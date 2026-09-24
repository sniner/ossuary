//! The PDF extractor: a document in, its text or its attachments out —
//! two contracts in one program.
//!
//! Speaks the ossuary extractor protocol (`docs/extractors.md`): called
//! with `--identify` it answers two lines, the `text` contract and the
//! `attachments` contract, each with its own source and its own
//! receipts. Called with the contract's name as the first argument and
//! an output directory as the second, it reads one file's bytes from
//! stdin. `text` writes the extracted text as `text.txt` into that
//! directory and announces it on stdout — beside whatever the
//! document's own info dictionary had to say, verbatim under `pdf:`.
//! `attachments` writes every file the document carries embedded — a
//! `ZUGFeRD` or Factur-X invoice's XML, a PDF/A-3 payload, whatever a
//! writer put in — out as a file of its own.
//!
//! The text engine is the system's `pdftotext` (poppler), spoken to
//! over pipes the way ossuary speaks to this program. Its version is
//! deliberately not part of this extractor's source: re-examination
//! follows a raised generation here, not the system's update cadence —
//! `ossuary extract pdf --full` is the lever for the rare poppler leap
//! that warrants a fresh look.
//!
//! A document with no text to give — scanned pages, an empty harvest, a
//! PDF pdftotext cannot open — is an examination like any other, with
//! no file to announce: exit 0. A harvest more than half of which is no
//! text at all — glyph numbers from fonts that do not say what they
//! spell — is discarded the same way. Whenever there is a reason worth
//! a sentence, the sentence goes on the record as a `prov:note` finding
//! and onto stderr, the same words in both places. Only the environment
//! failing (no pdftotext, a broken pipe world) is a failure.
//!
//! Attachments are found where the format keeps them: in the catalog's
//! `EmbeddedFiles` name tree, and on pages as file attachment
//! annotations. Each comes out under the name its file specification
//! spells, said on the record as `file:name` and, as a place inside the
//! document, as an `@`-led `file:path` — the spelling every inner place
//! has, whatever holds it. The kind is what the document declares for
//! the stream, and the honest shrug when it declares none. An
//! attachment that will not come out — a filter this program cannot
//! decode, a stream larger than [`UNPACKED_AT_MOST`] — stays inside,
//! and the reason goes on the record as a `prov:note` finding beside a
//! line on stderr: a document that gave up its attachments incompletely
//! must not read like one that gave them whole. A document lopdf cannot
//! open has no attachments to give.

use std::collections::HashSet;
use std::io::{Read, Write as _};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use lopdf::{Dictionary, Document, Object, ObjectId};
use serde_json::json;

/// The generation of what each contract writes: the number in its
/// source, and so the memory of which files it has seen. Raised by hand
/// when the findings change, when the same bytes would yield more or
/// something different than before, and never for a build, a dependency
/// or a release: a new generation examines every file again, and that
/// is the only reason to have one. Two contracts, two numbers: what
/// `text` learns to see says nothing about `attachments`.
const TEXT_GENERATION: u32 = 1;
const ATTACHMENTS_GENERATION: u32 = 1;

/// The most an attachment may unpack to. A compressed stream's whole
/// point is bytes out of little; beyond this, the attachment stays
/// inside with the reason on the record.
const UNPACKED_AT_MOST: usize = 1 << 30;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--identify"] => identify(),
        ["text", directory] if !directory.starts_with('-') => {
            examine(Contract::Text, Path::new(directory))
        }
        ["attachments", directory] if !directory.starts_with('-') => {
            examine(Contract::Attachments, Path::new(directory))
        }
        _ => {
            eprintln!(
                "ossuary-extract-pdf: expected --identify, or `text DIR` or `attachments DIR` with the file on stdin"
            );
            ExitCode::FAILURE
        }
    }
}

/// The program's two trades.
#[derive(Clone, Copy)]
enum Contract {
    Text,
    Attachments,
}

/// Who this extractor is — answered only when its text engine is
/// actually there: a missing pdftotext fails loudly here, once,
/// instead of quietly on every file.
fn identify() -> ExitCode {
    if !pdftotext_present() {
        eprintln!(
            "no `pdftotext` on PATH; install poppler (Homebrew: poppler, Debian: poppler-utils), then run this again"
        );
        return ExitCode::FAILURE;
    }
    println!(
        "{}",
        json!({
            "ossuary-extractor": 1,
            "contract": "text",
            "source": format!("extractor:pdf-text/{TEXT_GENERATION}"),
            "mimes": ["application/pdf"],
            "derives": true,
        })
    );
    println!(
        "{}",
        json!({
            "ossuary-extractor": 1,
            "contract": "attachments",
            "source": format!("extractor:pdf-attachments/{ATTACHMENTS_GENERATION}"),
            "mimes": ["application/pdf"],
            "derives": true,
        })
    );
    ExitCode::SUCCESS
}

/// One document: stdin to its end, then whatever the contract trades in.
fn examine(contract: Contract, directory: &Path) -> ExitCode {
    let mut bytes = Vec::new();
    if let Err(error) = std::io::stdin().lock().read_to_end(&mut bytes) {
        eprintln!("ossuary-extract-pdf: reading stdin: {error}");
        return ExitCode::FAILURE;
    }
    match contract {
        Contract::Text => text(&bytes, directory),
        Contract::Attachments => match attachments(&bytes, directory, UNPACKED_AT_MOST) {
            Ok(lines) => {
                for line in lines {
                    println!("{line}");
                }
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("ossuary-extract-pdf: {error}");
                ExitCode::FAILURE
            }
        },
    }
}

/// The `text` contract: info dictionary onto stdout, text into the
/// directory.
fn text(bytes: &[u8], directory: &Path) -> ExitCode {
    for (attribute, value) in document_info(bytes) {
        println!("{}", json!({ "attribute": attribute, "value": value }));
    }
    let text = match pdftotext(bytes.to_vec()) {
        Ok(Harvest::Text(text)) => text,
        Ok(Harvest::Refused(sentence)) => {
            note(&sentence);
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("ossuary-extract-pdf: {error}");
            return ExitCode::FAILURE;
        }
    };
    if blank(&text) {
        return ExitCode::SUCCESS;
    }
    if let Some(sentence) = junk_verdict(&String::from_utf8_lossy(&text)) {
        note(&sentence);
        return ExitCode::SUCCESS;
    }
    if let Err(error) = std::fs::write(directory.join("text.txt"), &text) {
        eprintln!("ossuary-extract-pdf: writing text.txt: {error}");
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
    /// The document's own refusal — unreadable or extraction forbidden —
    /// with the sentence that says which. Deterministic, so it counts as
    /// examined: retrying will not change the document.
    Refused(String),
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
        Ok(Harvest::Refused(if status.code() == Some(3) {
            "the document does not permit text extraction".to_string()
        } else {
            format!("pdftotext could not read this document ({status})")
        }))
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

/// A reason worth a sentence is said twice from one wording: onto the
/// record as a `prov:note` finding, and onto stderr for whoever watches
/// the run.
fn note(sentence: &str) {
    println!("{}", json!({ "attribute": "prov:note", "value": sentence }));
    eprintln!("ossuary-extract-pdf: {sentence}");
}

/// Whether the harvest is mostly not text at all — and the sentence
/// that says so when it is. Among the non-whitespace characters, more
/// than half unwritable is a harvest of noise: fonts without a
/// `ToUnicode` map give pdftotext glyph numbers, not characters, and
/// printed as bytes those are exactly the ranges [`unwritable`] names.
/// Where the line errs, it errs toward keeping — a bad `text.txt` can
/// be derived again, a silently discarded good one cannot.
fn junk_verdict(text: &str) -> Option<String> {
    let mut junk = 0usize;
    let mut ink = 0usize;
    for character in text.chars().filter(|character| !character.is_whitespace()) {
        ink += 1;
        if unwritable(character) {
            junk += 1;
        }
    }
    (junk * 2 > ink).then(|| {
        let percent = (junk * 100 + ink / 2) / ink;
        format!("{percent}% of the extracted characters are invalid; text discarded")
    })
}

/// A character written text cannot contain: controls (tab and the line
/// and page breaks are whitespace and never get here), the replacement
/// character — poppler's "no idea what this glyph spells", and a broken
/// UTF-8 sequence reads as one too — the Private Use Areas, and the
/// noncharacters. Plain codepoint ranges, deliberately no Unicode
/// tables: the measure must not drift with a dependency.
fn unwritable(character: char) -> bool {
    let code = u32::from(character);
    character.is_control()
        || character == '\u{FFFD}'
        || matches!(code, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x10_0000..=0x10_FFFD)
        || matches!(code, 0xFDD0..=0xFDEF)
        || code & 0xFFFE == 0xFFFE
}

/// The `attachments` contract: every embedded file out as a file of
/// its own, announced with the kind the document declares, its spelled
/// name on the record as `file:name` and as an `@`-led `file:path`.
/// What will not come out stays inside and says why — see
/// [`stays_inside`] — and costs no other attachment its examination.
/// Only failing to write a file is a failure.
fn attachments(
    bytes: &[u8],
    directory: &Path,
    at_most: usize,
) -> std::io::Result<Vec<serde_json::Value>> {
    let Ok(document) = Document::load_mem(bytes) else {
        return Ok(Vec::new());
    };
    let mut lines = Vec::new();
    let mut taken = HashSet::new();
    for specification in file_specifications(&document) {
        let Some(spelled) = specification.name(&document) else {
            lines.push(stays_inside(
                &format!("{:?}", specification.stream),
                "no file name",
            ));
            continue;
        };
        let Some(name) = basename(&spelled) else {
            lines.push(stays_inside(&format!("{spelled:?}"), "no file name"));
            continue;
        };
        let wearable = fits(&name);
        if wearable.is_empty() {
            lines.push(stays_inside(&format!("{spelled:?}"), "no file name"));
            continue;
        }
        let Ok(object) = document.get_object(specification.stream) else {
            lines.push(stays_inside(&spelled, "embedded file missing"));
            continue;
        };
        let Ok(stream) = object.as_stream() else {
            lines.push(stays_inside(&spelled, "embedded file damaged"));
            continue;
        };
        // The bound is checked before decoding as well as after: a
        // stream already too large as stored need not be inflated to
        // find out.
        if stream.content.len() > at_most {
            lines.push(stays_inside(
                &spelled,
                &format!("larger than {}", human_bytes(at_most)),
            ));
            continue;
        }
        // A stream without a filter is its bytes as they stand; lopdf
        // asks for a filter before it inflates anything.
        let content = if stream.dict.has(b"Filter") {
            match stream.decompressed_content() {
                Ok(content) => content,
                Err(error) => {
                    lines.push(stays_inside(&spelled, &error.to_string()));
                    continue;
                }
            }
        } else {
            stream.content.clone()
        };
        if content.len() > at_most {
            lines.push(stays_inside(
                &spelled,
                &format!("larger than {}", human_bytes(at_most)),
            ));
            continue;
        }
        let announced = uniquify(wearable, &mut taken);
        std::fs::write(directory.join(&announced), &content).map_err(|error| {
            std::io::Error::new(error.kind(), format!("writing {announced}: {error}"))
        })?;
        lines.push(json!({ "file": &announced, "mime": declared_kind(&stream.dict) }));
        // The name the document spelled goes on the record, whether or
        // not the announcement could wear it: the announced name is a
        // handle in the directory, and the record never learns it.
        lines.push(json!({ "file": &announced, "attribute": "file:name", "value": name }));
        lines.push(
            json!({ "file": &announced, "attribute": "file:path", "value": inner(&spelled) }),
        );
        for (attribute, value) in specification.facts(&document, &stream.dict) {
            lines.push(json!({ "file": &announced, "attribute": attribute, "value": value }));
        }
    }
    Ok(lines)
}

/// One embedded file as the document points at it: the file
/// specification dictionary, and the stream its `EF` entry names.
struct FileSpecification<'a> {
    dictionary: &'a Dictionary,
    stream: ObjectId,
}

impl FileSpecification<'_> {
    /// The name the specification spells: `UF`, the Unicode one, before
    /// `F`, the byte one — decoded the way any PDF text string is, and
    /// nothing else done to it.
    fn name(&self, document: &Document) -> Option<String> {
        [b"UF".as_slice(), b"F"].iter().find_map(|key| {
            let spelled = self.dictionary.get(key).ok()?;
            let spelled = decode_text(dereference(document, spelled)?.as_str().ok()?);
            (!spelled.is_empty()).then_some(spelled)
        })
    }

    /// What the document says about the attachment beyond its name and
    /// kind, verbatim under `pdf:` the way the info dictionary is: the
    /// specification's `Desc` and `AFRelationship`, and from the
    /// stream's `Params` its `CreationDate` and `ModDate`, the dates as
    /// the document spells them. `Size` and `CheckSum` are facts of the
    /// bytes, and the archive says those itself.
    fn facts(&self, document: &Document, stream: &Dictionary) -> Vec<(String, String)> {
        let params = stream
            .get(b"Params")
            .ok()
            .and_then(|params| dereference(document, params))
            .and_then(|params| params.as_dict().ok());
        let mut facts = Vec::new();
        for (dictionary, key) in [
            (Some(self.dictionary), b"Desc".as_slice()),
            (Some(self.dictionary), b"AFRelationship"),
            (params, b"CreationDate"),
            (params, b"ModDate"),
        ] {
            let Some(object) = dictionary
                .and_then(|dictionary| dictionary.get(key).ok())
                .and_then(|object| dereference(document, object))
            else {
                continue;
            };
            let value = match object {
                Object::String(bytes, _) => decode_text(bytes),
                Object::Name(name) => String::from_utf8_lossy(name).into_owned(),
                _ => continue,
            };
            if value.is_empty() {
                continue;
            }
            facts.push((
                format!("pdf:{}", kebab(&String::from_utf8_lossy(key))),
                value,
            ));
        }
        facts
    }
}

/// Every file specification the document carries, in the order the
/// format keeps them: the catalog's `EmbeddedFiles` name tree first,
/// then each page's file attachment annotations. A stream reached
/// twice — the same file in the tree and on a page — comes out once.
fn file_specifications(document: &Document) -> Vec<FileSpecification<'_>> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    let Ok(catalog) = document.catalog() else {
        return found;
    };
    if let Some(names) = catalog
        .get(b"Names")
        .ok()
        .and_then(|names| dereference(document, names))
        .and_then(|names| names.as_dict().ok())
        && let Some(tree) = names
            .get(b"EmbeddedFiles")
            .ok()
            .and_then(|tree| dereference(document, tree))
            .and_then(|tree| tree.as_dict().ok())
    {
        let mut visited = HashSet::new();
        name_tree(document, tree, &mut visited, &mut |specification| {
            take(specification, document, &mut seen, &mut found);
        });
    }
    for page in document.get_pages().into_values() {
        let annotations = document
            .get_object(page)
            .ok()
            .and_then(|page| page.as_dict().ok())
            .and_then(|page| page.get(b"Annots").ok())
            .and_then(|annotations| dereference(document, annotations))
            .and_then(|annotations| annotations.as_array().ok());
        let Some(annotations) = annotations else {
            continue;
        };
        for annotation in annotations {
            let Some(annotation) =
                dereference(document, annotation).and_then(|annotation| annotation.as_dict().ok())
            else {
                continue;
            };
            if annotation
                .get(b"Subtype")
                .ok()
                .and_then(|subtype| subtype.as_name().ok())
                != Some(b"FileAttachment")
            {
                continue;
            }
            if let Ok(specification) = annotation.get(b"FS") {
                take(specification, document, &mut seen, &mut found);
            }
        }
    }
    found
}

/// A name tree walked to its leaves: `Kids` down, `Names` as pairs of
/// key and value, the value handed on. A node met twice is a cycle,
/// not a second copy, and is walked once.
fn name_tree<'a>(
    document: &'a Document,
    node: &'a Dictionary,
    visited: &mut HashSet<ObjectId>,
    leaf: &mut impl FnMut(&'a Object),
) {
    if let Some(kids) = node
        .get(b"Kids")
        .ok()
        .and_then(|kids| dereference(document, kids))
        .and_then(|kids| kids.as_array().ok())
    {
        for kid in kids {
            if let Object::Reference(id) = kid
                && !visited.insert(*id)
            {
                continue;
            }
            if let Some(kid) = dereference(document, kid).and_then(|kid| kid.as_dict().ok()) {
                name_tree(document, kid, visited, leaf);
            }
        }
    }
    if let Some(pairs) = node
        .get(b"Names")
        .ok()
        .and_then(|names| dereference(document, names))
        .and_then(|names| names.as_array().ok())
    {
        for pair in pairs.chunks_exact(2) {
            leaf(&pair[1]);
        }
    }
}

/// A file specification resolved to the stream behind it and kept,
/// unless that stream was kept already. The stream is under `EF`, as
/// `UF` or `F` — a specification with neither embeds nothing and is
/// passed over: a reference to a file elsewhere is not an attachment.
fn take<'a>(
    specification: &'a Object,
    document: &'a Document,
    seen: &mut HashSet<ObjectId>,
    found: &mut Vec<FileSpecification<'a>>,
) {
    let Some(dictionary) =
        dereference(document, specification).and_then(|object| object.as_dict().ok())
    else {
        return;
    };
    let Some(embedded) = dictionary
        .get(b"EF")
        .ok()
        .and_then(|embedded| dereference(document, embedded))
        .and_then(|embedded| embedded.as_dict().ok())
    else {
        return;
    };
    let stream = [b"UF".as_slice(), b"F"]
        .iter()
        .find_map(|key| match embedded.get(key) {
            Ok(Object::Reference(id)) => Some(*id),
            _ => None,
        });
    if let Some(stream) = stream
        && seen.insert(stream)
    {
        found.push(FileSpecification { dictionary, stream });
    }
}

/// The object behind a reference, or the object itself; a reference
/// to nothing is nothing.
fn dereference<'a>(document: &'a Document, object: &'a Object) -> Option<&'a Object> {
    match object {
        Object::Reference(id) => document.get_object(*id).ok(),
        other => Some(other),
    }
}

/// The kind the document declares for an embedded stream — its
/// `Subtype`, a mime type as a PDF name — and the honest shrug when it
/// declares none, or something that is no mime type.
fn declared_kind(stream: &Dictionary) -> String {
    stream
        .get(b"Subtype")
        .ok()
        .and_then(|subtype| subtype.as_name().ok())
        .and_then(|name| std::str::from_utf8(name).ok())
        .filter(|declared| mimey(declared))
        .map_or_else(|| "application/octet-stream".to_string(), str::to_string)
}

/// Whether a declared kind reads as a mime type: two halves around one
/// slash, each in the alphabet mime names use.
fn mimey(declared: &str) -> bool {
    let fits = |half: &str| {
        !half.is_empty()
            && half.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'+' | b'-')
            })
    };
    matches!(declared.split_once('/'), Some((kind, subtype)) if fits(kind) && fits(subtype))
}

/// A place inside another content, spelled: a leading `@`, then the
/// name verbatim — the one spelling every inner place has.
fn inner(spelled: &str) -> String {
    format!("@{spelled}")
}

/// An attachment the examination could not bring out, said twice from
/// one wording: onto the record as a `prov:note` finding — a document
/// that gave up its attachments incompletely must not read like one
/// that gave them whole — and onto stderr for whoever watches the run.
fn stays_inside(spelled: &str, reason: &str) -> serde_json::Value {
    let sentence = format!("attachment {spelled} not extracted: {reason}");
    eprintln!("ossuary-extract-pdf: {sentence}");
    json!({ "attribute": "prov:note", "value": sentence })
}

/// A byte count as a person reads it — the bound, said once.
fn human_bytes(bytes: usize) -> String {
    const UNITS: [&str; 5] = ["bytes", "KiB", "MiB", "GiB", "TiB"];
    let mut unit = 0;
    let mut count = bytes;
    while count >= 1024 && count % 1024 == 0 && unit < UNITS.len() - 1 {
        count /= 1024;
        unit += 1;
    }
    format!("{count} {}", UNITS[unit])
}

/// The bare file name inside a spelled name: the last element past
/// either separator, because an announced name names a file, never a
/// place. A spelling with no name left in it answers nothing.
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
    use lopdf::{Document, Object, Stream, StringFormat, dictionary};
    use tempfile::TempDir;

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
    fn prose_passes_whatever_the_script() {
        for prose in [
            "Der Umbau beginnt im Frühjahr, sobald der Boden trocken ist.",
            "Η θάλασσα ήταν ήρεμη και ο ουρανός καθαρός όλο το πρωί.",
            "Архив хранит всё, что ему доверили, без изменений.",
            "書類は箱の中に整理されて保管されている。",
            "الأرشيف يحفظ كل ما أودع فيه دون تغيير.",
        ] {
            assert_eq!(junk_verdict(prose), None, "{prose}");
        }
    }

    /// Letters are no signal: phone lists, timetables and blood-pressure
    /// logs are flawless extractions with hardly a letter in them. The
    /// letter-share idea was measured against a real corpus and
    /// rejected — this test stands guard against its return.
    #[test]
    fn a_table_of_digits_and_punctuation_passes() {
        let table = "07:15  4711 / 22  120/80\n08:00  0231 555-77  118/79\n";
        assert_eq!(junk_verdict(table), None);
    }

    #[test]
    fn glyph_numbers_are_refused_and_the_sentence_carries_the_share() {
        let mut noise = String::new();
        for _ in 0..30 {
            for code in 1u8..=8 {
                noise.push(code as char);
                noise.push(' ');
            }
        }
        let sentence = junk_verdict(&noise).expect("nothing here is writable");
        assert!(sentence.contains("100%"), "{sentence}");
    }

    #[test]
    fn private_use_text_is_refused() {
        let pua: String = "\u{E000}\u{E742} \u{F0001} \u{10FFFD}".into();
        assert!(junk_verdict(&pua).is_some());
    }

    #[test]
    fn a_stray_replacement_character_does_not_condemn_prose() {
        let prose = "The one sign \u{FFFD} was all the font could not say.";
        assert_eq!(junk_verdict(prose), None);
    }

    #[test]
    fn broken_utf8_reads_as_unwritable() {
        let mut bytes = b"ab".to_vec();
        bytes.extend(std::iter::repeat_n(0xFF, 10));
        assert!(junk_verdict(&String::from_utf8_lossy(&bytes)).is_some());
    }

    #[test]
    fn whitespace_stays_blanks_business() {
        assert_eq!(junk_verdict(""), None, "an empty measure refuses nothing");
        assert_eq!(junk_verdict(" \n\x0c \n"), None);
    }

    #[test]
    fn one_control_character_in_a_page_of_prose_passes() {
        let prose = format!("{}\x02 and the rest reads fine.", "words ".repeat(50));
        assert_eq!(junk_verdict(&prose), None);
    }

    #[test]
    fn exactly_half_junk_is_kept() {
        assert_eq!(
            junk_verdict("a\u{E000}"),
            None,
            "the line is MORE than half"
        );
    }

    #[test]
    fn the_unwritable_ranges_and_no_others() {
        for bad in [
            '\x01',
            '\u{9F}',
            '\u{FFFD}',
            '\u{E000}',
            '\u{F8FF}',
            '\u{F0000}',
            '\u{10FFFD}',
            '\u{FDD0}',
            '\u{FFFE}',
            '\u{5FFFF}',
        ] {
            assert!(unwritable(bad), "U+{:04X}", u32::from(bad));
        }
        for fine in ['a', '0', '€', '種', 'ب', 'ß', '—', '·'] {
            assert!(!unwritable(fine), "{fine}");
        }
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

    /// A document under construction: one page, and every way the
    /// format has of pointing at an embedded file.
    struct Builder {
        document: Document,
        page: ObjectId,
        names: Vec<Object>,
        annotations: Vec<Object>,
    }

    impl Builder {
        fn new() -> Self {
            let mut document = Document::with_version("1.7");
            let pages = document.new_object_id();
            let page = document.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages),
            });
            document.objects.insert(
                pages,
                Object::Dictionary(dictionary! {
                    "Type" => "Pages",
                    "Kids" => vec![Object::Reference(page)],
                    "Count" => 1,
                }),
            );
            Builder {
                document,
                page,
                names: Vec::new(),
                annotations: Vec::new(),
            }
        }

        /// An embedded stream, declared as the kind given — or as
        /// nothing — and deflated when the bytes shrink for it, the way
        /// writers write them; a short one stays as it is.
        fn stream(&mut self, content: &[u8], kind: Option<&str>) -> ObjectId {
            let mut dict = dictionary! { "Type" => "EmbeddedFile" };
            if let Some(kind) = kind {
                dict.set("Subtype", Object::Name(kind.as_bytes().to_vec()));
            }
            dict.set(
                "Params",
                dictionary! {
                    "Size" => Object::Integer(i64::try_from(content.len()).unwrap()),
                    "ModDate" => Object::string_literal("D:20260725120000Z"),
                },
            );
            let mut stream = Stream::new(dict, content.to_vec());
            stream.compress().unwrap();
            assert_eq!(
                stream.dict.has(b"Filter"),
                content.len() > 64,
                "short content stays plain, long content deflates"
            );
            self.document.add_object(stream)
        }

        /// A file specification pointing at a stream, `UF` and `F` as
        /// given.
        fn specification(
            &mut self,
            unicode: Option<&str>,
            byte: Option<&str>,
            stream: Option<ObjectId>,
        ) -> ObjectId {
            let mut dict = dictionary! { "Type" => "Filespec" };
            if let Some(unicode) = unicode {
                let mut utf16 = vec![0xFE, 0xFF];
                for unit in unicode.encode_utf16() {
                    utf16.extend_from_slice(&unit.to_be_bytes());
                }
                dict.set("UF", Object::String(utf16, StringFormat::Literal));
            }
            if let Some(byte) = byte {
                dict.set("F", Object::string_literal(byte));
            }
            if let Some(stream) = stream {
                dict.set("EF", dictionary! { "F" => Object::Reference(stream) });
                dict.set("Desc", Object::string_literal("what it is"));
                dict.set("AFRelationship", Object::Name(b"Alternative".to_vec()));
            }
            self.document.add_object(dict)
        }

        fn in_tree(&mut self, key: &str, specification: ObjectId) {
            self.names.push(Object::string_literal(key));
            self.names.push(Object::Reference(specification));
        }

        fn on_page(&mut self, specification: ObjectId) {
            let annotation = self.document.add_object(dictionary! {
                "Type" => "Annot",
                "Subtype" => "FileAttachment",
                "FS" => Object::Reference(specification),
            });
            self.annotations.push(Object::Reference(annotation));
        }

        fn finish(mut self, kids_cycle: bool) -> Vec<u8> {
            let leaf = self
                .document
                .add_object(dictionary! { "Names" => self.names });
            let tree = if kids_cycle {
                let root = self.document.new_object_id();
                self.document.objects.insert(
                    root,
                    Object::Dictionary(dictionary! {
                        "Kids" => vec![Object::Reference(leaf), Object::Reference(root)],
                    }),
                );
                root
            } else {
                leaf
            };
            let names = self.document.add_object(dictionary! {
                "EmbeddedFiles" => Object::Reference(tree),
            });
            let page = self
                .document
                .get_object_mut(self.page)
                .unwrap()
                .as_dict_mut()
                .unwrap();
            let pages = page.get(b"Parent").unwrap().clone();
            page.set("Annots", self.annotations);
            let catalog = self.document.add_object(dictionary! {
                "Type" => "Catalog",
                "Pages" => pages,
                "Names" => Object::Reference(names),
            });
            self.document
                .trailer
                .set("Root", Object::Reference(catalog));
            let mut bytes = Vec::new();
            self.document.save_to(&mut bytes).unwrap();
            bytes
        }
    }

    fn harvest(bytes: &[u8], at_most: usize) -> (Vec<serde_json::Value>, TempDir) {
        let directory = TempDir::new().unwrap();
        let lines = attachments(bytes, directory.path(), at_most).unwrap();
        (lines, directory)
    }

    #[test]
    fn an_embedded_invoice_comes_out_with_its_name_place_kind_and_facts() {
        let mut builder = Builder::new();
        let xml = b"<?xml version=\"1.0\"?><rsm:CrossIndustryInvoice/>";
        let stream = builder.stream(xml, Some("text/xml"));
        let specification =
            builder.specification(Some("factur-x.xml"), Some("factur-x.xml"), Some(stream));
        builder.in_tree("factur-x.xml", specification);
        let (lines, directory) = harvest(&builder.finish(false), UNPACKED_AT_MOST);
        assert_eq!(
            std::fs::read(directory.path().join("factur-x.xml")).unwrap(),
            xml,
            "the stream inflated, byte for byte"
        );
        for expected in [
            json!({ "file": "factur-x.xml", "mime": "text/xml" }),
            json!({ "file": "factur-x.xml", "attribute": "file:name", "value": "factur-x.xml" }),
            json!({ "file": "factur-x.xml", "attribute": "file:path", "value": "@factur-x.xml" }),
            json!({ "file": "factur-x.xml", "attribute": "pdf:desc", "value": "what it is" }),
            json!({ "file": "factur-x.xml", "attribute": "pdf:af-relationship", "value": "Alternative" }),
            json!({ "file": "factur-x.xml", "attribute": "pdf:mod-date", "value": "D:20260725120000Z" }),
        ] {
            assert!(
                lines.contains(&expected),
                "missing {expected}; got {lines:#?}"
            );
        }
        assert!(
            !lines.iter().any(|line| line["attribute"] == "pdf:size"),
            "the size is the archive's to say"
        );
        assert!(!lines.iter().any(|line| line["attribute"] == "prov:note"));
    }

    #[test]
    fn the_unicode_name_wins_and_a_path_in_it_becomes_the_place() {
        let mut builder = Builder::new();
        let stream = builder.stream(b"payload", Some("application/pdf"));
        let specification = builder.specification(
            Some("Belege\\Rechnung Müller.pdf"),
            Some("Belege\\Rechnung M?ller.pdf"),
            Some(stream),
        );
        builder.in_tree("a", specification);
        let (lines, directory) = harvest(&builder.finish(false), UNPACKED_AT_MOST);
        assert!(directory.path().join("Rechnung Müller.pdf").is_file());
        assert!(lines.contains(
            &json!({ "file": "Rechnung Müller.pdf", "attribute": "file:name", "value": "Rechnung Müller.pdf" })
        ));
        assert!(lines.contains(
            &json!({ "file": "Rechnung Müller.pdf", "attribute": "file:path", "value": "@Belege\\Rechnung Müller.pdf" })
        ));
    }

    #[test]
    fn a_page_annotation_is_found_and_a_stream_reached_twice_comes_out_once() {
        let mut builder = Builder::new();
        let shared = builder.stream(b"one", Some("text/plain"));
        let other = builder.stream(b"two", None);
        let in_tree = builder.specification(Some("one.txt"), None, Some(shared));
        let on_page_too = builder.specification(None, Some("one.txt"), Some(shared));
        let on_page = builder.specification(None, Some("two.bin"), Some(other));
        builder.in_tree("one.txt", in_tree);
        builder.on_page(on_page_too);
        builder.on_page(on_page);
        let (lines, _directory) = harvest(&builder.finish(false), UNPACKED_AT_MOST);
        let announced: Vec<&serde_json::Value> = lines
            .iter()
            .filter(|line| line.get("mime").is_some())
            .collect();
        assert_eq!(announced.len(), 2, "{lines:#?}");
        assert!(lines.contains(&json!({ "file": "one.txt", "mime": "text/plain" })));
        assert!(
            lines.contains(&json!({ "file": "two.bin", "mime": "application/octet-stream" })),
            "no declared kind is the honest shrug; got {lines:#?}"
        );
    }

    #[test]
    fn a_specification_embedding_nothing_is_passed_over() {
        let mut builder = Builder::new();
        let specification = builder.specification(Some("elsewhere.pdf"), None, None);
        builder.in_tree("elsewhere.pdf", specification);
        let (lines, _directory) = harvest(&builder.finish(false), UNPACKED_AT_MOST);
        assert_eq!(lines, Vec::<serde_json::Value>::new());
    }

    #[test]
    fn a_cycle_in_the_tree_is_walked_once() {
        let mut builder = Builder::new();
        let stream = builder.stream(b"leaf", Some("text/plain"));
        let specification = builder.specification(Some("leaf.txt"), None, Some(stream));
        builder.in_tree("leaf.txt", specification);
        let (lines, _directory) = harvest(&builder.finish(true), UNPACKED_AT_MOST);
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.get("mime").is_some())
                .count(),
            1
        );
    }

    #[test]
    fn too_large_stays_inside_and_says_so() {
        let mut builder = Builder::new();
        let stream = builder.stream(&[0u8; 4096], Some("application/octet-stream"));
        let specification = builder.specification(Some("zeros.bin"), None, Some(stream));
        builder.in_tree("zeros.bin", specification);
        let (lines, directory) = harvest(&builder.finish(false), 1024);
        assert!(!directory.path().join("zeros.bin").exists());
        assert_eq!(
            lines,
            vec![json!({
                "attribute": "prov:note",
                "value": "attachment zeros.bin not extracted: larger than 1 KiB"
            })]
        );
    }

    #[test]
    fn colliding_names_yield_to_a_counter() {
        let mut builder = Builder::new();
        let first = builder.stream(b"a", Some("text/plain"));
        let second = builder.stream(b"b", Some("text/plain"));
        let one = builder.specification(Some("dir/Note.txt"), None, Some(first));
        let two = builder.specification(Some("other/note.txt"), None, Some(second));
        builder.in_tree("one", one);
        builder.in_tree("two", two);
        let (lines, directory) = harvest(&builder.finish(false), UNPACKED_AT_MOST);
        assert!(directory.path().join("Note.txt").is_file());
        assert!(directory.path().join("note-2.txt").is_file());
        assert!(lines.contains(
            &json!({ "file": "note-2.txt", "attribute": "file:path", "value": "@other/note.txt" })
        ));
    }

    #[test]
    fn a_document_without_attachments_or_without_a_pdf_answers_nothing() {
        let (lines, _directory) = harvest(&sample(), UNPACKED_AT_MOST);
        assert_eq!(lines, Vec::<serde_json::Value>::new());
        let (lines, _directory) = harvest(b"plain words", UNPACKED_AT_MOST);
        assert_eq!(lines, Vec::<serde_json::Value>::new());
    }

    #[test]
    fn a_declared_kind_must_read_as_one() {
        assert_eq!(
            declared_kind(&dictionary! { "Subtype" => "text/xml" }),
            "text/xml"
        );
        assert_eq!(
            declared_kind(&dictionary! { "Subtype" => "XML" }),
            "application/octet-stream"
        );
        assert_eq!(declared_kind(&dictionary! {}), "application/octet-stream");
    }
}
