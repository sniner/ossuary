//! The packed extractor: a zip archive in, its inventory or its files
//! out — two contracts in one program.
//!
//! Speaks the ossuary extractor protocol (`docs/extractors.md`), as its
//! first program of two trades: `--identify` answers two lines, the
//! `list` contract and the `unpack` contract, each with its own source
//! and its own receipts. Called with the contract's name as the first
//! argument — and, for `unpack`, the output directory as the second —
//! it reads one file's bytes from stdin. `list` tells every entry the
//! archive holds, one `packed:path` finding each, without unpacking a
//! byte; `unpack` writes every entry out as a file of its own.
//!
//! A place inside another content is spelled with a leading `@` and
//! the entry's path verbatim after it: `@invoices/2026-03.pdf`. The
//! inventory says it on the archive as `packed:path`; the unpacked file
//! says it on itself as `file:path`, beside `prov:origin` naming the
//! archive. The same spelling both ways, so "which archive holds this"
//! and "where did this lie in its archive" are one question asked from
//! either end, and whether the archive was a zip is nobody's concern.
//!
//! Not every zip is an archive. epub and the `OpenDocument` family open
//! with an entry named `mimetype` holding nothing but their own kind,
//! and OOXML carries `[Content_Types].xml` at its root — documents
//! wearing zip as their envelope, which nobody wants shredded into XML
//! innards. Both contracts recognize these by the container's own
//! construction and answer with a sharper `file:mime` instead — said
//! by the bytes, standing beside the sniffed word — so an extractor
//! reading the sharper kind can find them. A jar stays an ordinary
//! archive: it promises nothing about its insides. Bytes that do not
//! read as a zip at all are an examination with nothing found.
//!
//! Unpacked entries lose their inner paths — an announced name is
//! bare — so colliding names yield to a counter, a name longer than a
//! filesystem takes is cut to fit, the true name goes on the record as
//! `file:name` always, since the announced one is a handle the record
//! never learns, and every file's full entry path as its `@`-led
//! `file:path`. A zip declares no kinds, so each announcement carries
//! the same magic-bytes-then-UTF-8 look ingest would take, taken on the
//! way out: an entry streams into its file, never through memory
//! whole, and one that unpacks to more than [`UNPACKED_AT_MOST`] stays
//! inside — a zip bomb is a deterministic reason, not a failure to try
//! again. An entry that will not come out — encrypted, damaged, a
//! spelling with no file name in it, too large — stays inside, and the
//! reason goes on the record as a `prov:note` finding beside a line on
//! stderr: a zip that unpacked incompletely must not read like one that
//! unpacked whole. There is no password to offer, and a receipt beats
//! being offered the same locked door every run. A symlink stays inside
//! silently — its bytes are a name rather than content, and nothing is
//! lost.

use std::collections::HashSet;
use std::io::{Cursor, Read, Seek, Write};
use std::path::Path;
use std::process::ExitCode;

use serde_json::json;
use zip::ZipArchive;

/// The generation of what each contract writes: the number in its
/// source, and so the memory of which files it has seen. Raised by hand
/// when the findings change, when the same bytes would yield more or
/// something different than before, and never for a build, a dependency
/// or a release: a new generation examines every file again, and that
/// is the only reason to have one. Two contracts, two numbers: what
/// `list` learns to see says nothing about `unpack`.
const LIST_GENERATION: u32 = 1;
const UNPACK_GENERATION: u32 = 1;

/// The most an entry may unpack to. A zip bomb's whole point is bytes
/// out of nowhere; beyond this, the entry stays inside with the reason
/// on the record.
const UNPACKED_AT_MOST: u64 = 1 << 30;

/// How much of an entry's beginning the kind is read from: what
/// `infer` needs, and then some.
const HEAD: usize = 8 * 1024;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--identify"] => {
            println!(
                "{}",
                json!({
                    "ossuary-extractor": 1,
                    "contract": "list",
                    "source": format!("extractor:packed-list/{LIST_GENERATION}"),
                    "mimes": ["application/zip"],
                })
            );
            println!(
                "{}",
                json!({
                    "ossuary-extractor": 1,
                    "contract": "unpack",
                    "source": format!("extractor:packed-unpack/{UNPACK_GENERATION}"),
                    "mimes": ["application/zip"],
                    "derives": true,
                })
            );
            ExitCode::SUCCESS
        }
        ["list"] => examine(Contract::List, None),
        ["unpack", directory] if !directory.starts_with('-') => {
            examine(Contract::Unpack, Some(Path::new(directory)))
        }
        _ => {
            eprintln!(
                "ossuary-extract-packed: run with --identify, with `list`, or with `unpack DIR`; a file's bytes on stdin either way"
            );
            ExitCode::FAILURE
        }
    }
}

/// The program's two trades.
#[derive(Clone, Copy)]
enum Contract {
    List,
    Unpack,
}

/// One file: stdin to its end, then the harvest — for `unpack`, the
/// entries into the directory on the way — findings onto stdout.
fn examine(contract: Contract, directory: Option<&Path>) -> ExitCode {
    let mut bytes = Vec::new();
    if let Err(error) = std::io::stdin().lock().read_to_end(&mut bytes) {
        eprintln!("ossuary-extract-packed: reading stdin: {error}");
        return ExitCode::FAILURE;
    }
    match harvest(&bytes, contract, directory) {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("ossuary-extract-packed: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The whole answer for one file: nothing when the bytes are no
/// readable zip, the sharper kind alone when they are a known
/// container, and otherwise whatever the contract trades in. Only
/// failing to write an entry's file is a failure.
fn harvest(
    bytes: &[u8],
    contract: Contract,
    directory: Option<&Path>,
) -> std::io::Result<Vec<serde_json::Value>> {
    let Ok(mut archive) = ZipArchive::new(Cursor::new(bytes)) else {
        return Ok(Vec::new());
    };
    match nature(&mut archive) {
        Nature::Container(Some(mime)) => {
            return Ok(vec![json!({ "attribute": "file:mime", "value": mime })]);
        }
        // Recognized as a container, but of no kind this program can
        // name: left shut, with nothing to say about it.
        Nature::Container(None) => return Ok(Vec::new()),
        Nature::Archive => {}
    }
    match contract {
        Contract::List => Ok(list(&archive)),
        Contract::Unpack => unpack(
            &mut archive,
            directory.expect("unpack is called with a directory"),
            UNPACKED_AT_MOST,
        ),
    }
}

/// What these bytes turn out to be: a zip that is honestly an archive,
/// or a known container format wearing zip as its envelope — with the
/// sharper mime when the container names one.
enum Nature {
    Archive,
    Container(Option<String>),
}

/// The container check, by each format's own construction. epub and
/// `OpenDocument` mandate a first entry `mimetype` holding nothing but
/// the container's kind — the declaration is read verbatim, whatever
/// kind it names, because the construction itself is the signal. OOXML
/// is known by `[Content_Types].xml` at the root, and which Office
/// kind it is by the directory the payload lives in.
fn nature<R: Read + Seek>(archive: &mut ZipArchive<R>) -> Nature {
    if !archive.is_empty()
        && let Ok(first) = archive.by_index(0)
        && first.name() == "mimetype"
        && first.size() <= 100
    {
        let mut declared = String::new();
        if first.take(101).read_to_string(&mut declared).is_ok() {
            let declared = declared.trim();
            if mimey(declared) {
                return Nature::Container(Some(declared.to_string()));
            }
        }
    }
    if archive
        .file_names()
        .any(|name| name == "[Content_Types].xml")
    {
        let office = |directory: &str, kind: &str| {
            archive
                .file_names()
                .any(|name| name.starts_with(directory))
                .then(|| format!("application/vnd.openxmlformats-officedocument.{kind}"))
        };
        let flavor = office("word/", "wordprocessingml.document")
            .or_else(|| office("xl/", "spreadsheetml.sheet"))
            .or_else(|| office("ppt/", "presentationml.presentation"));
        return Nature::Container(flavor);
    }
    Nature::Archive
}

/// Whether a declared kind reads as a mime type: two halves around one
/// slash, each in the alphabet mime names use. The guard against a
/// stray file named `mimetype` holding prose.
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

/// The inventory: one finding per file entry, the entry's path as the
/// zip spells it, as an inner place. Directories are structure, not
/// content, and stay untold. Sorted so the answer reads the same
/// however the zip was written — the record holds a set either way.
fn list<R: Read + Seek>(archive: &ZipArchive<R>) -> Vec<serde_json::Value> {
    let mut entries: Vec<&str> = archive
        .file_names()
        .filter(|name| !name.ends_with('/'))
        .collect();
    entries.sort_unstable();
    entries
        .into_iter()
        .map(|entry| json!({ "attribute": "packed:path", "value": inner(entry) }))
        .collect()
}

/// A place inside another content, spelled: a leading `@`, then the
/// entry's path verbatim — an entry that itself begins with `@` reads
/// `@@…`. The one spelling both the inventory and the unpacked file
/// use.
fn inner(entry: &str) -> String {
    format!("@{entry}")
}

/// Every entry out as a file of its own: flattened to its bare name,
/// yielded to a counter on collision, announced with the kind its own
/// bytes answer to. What will not come out stays inside and says why —
/// see [`stays_inside`] — and costs no other entry its examination.
fn unpack<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    directory: &Path,
    at_most: u64,
) -> std::io::Result<Vec<serde_json::Value>> {
    let mut lines = Vec::new();
    let mut taken = HashSet::new();
    for index in 0..archive.len() {
        // Name and lock are read raw up front: a failed open below still
        // pins the archive, so the refusal could not go asking for them
        // then.
        let (spelled_raw, locked) = archive.by_index_raw(index).map_or_else(
            |_| (format!("{index}"), false),
            |raw| (raw.name().to_string(), raw.encrypted()),
        );
        let mut file = match archive.by_index(index) {
            Ok(file) => file,
            Err(error) => {
                // The library's word for why, except for the locked door,
                // where the entry's own flag beats guessing from error
                // texts — and where the honest reason is this program's
                // to say.
                let reason = if locked {
                    "encrypted, and there is no password to offer".to_string()
                } else {
                    error.to_string()
                };
                lines.push(stays_inside(&spelled_raw, &reason));
                continue;
            }
        };
        if file.is_dir() {
            continue;
        }
        if file
            .unix_mode()
            .is_some_and(|mode| mode & 0o17_0000 == 0o12_0000)
        {
            // A symlink's bytes are the name of its target, not content.
            continue;
        }
        let spelled = file.name().to_string();
        let Some(name) = basename(&spelled) else {
            lines.push(stays_inside(&format!("{spelled:?}"), "no file name in it"));
            continue;
        };
        // A name too long for the filesystem must not fail the whole zip
        // every run; the file waits under a name that fits, and the
        // spelled name goes on the record.
        let wearable = fits(&name);
        if wearable.is_empty() {
            lines.push(stays_inside(&format!("{spelled:?}"), "no file name in it"));
            continue;
        }
        let announced = uniquify(wearable, &mut taken);
        let target = directory.join(&announced);
        let opened = std::fs::File::create(&target).map_err(|error| {
            std::io::Error::new(error.kind(), format!("writing {announced}: {error}"))
        })?;
        // The entry streams into its file, the look at its kind taken on
        // the way; one byte past the bound tells too large from exactly
        // as large.
        let mut look = Look::new(opened);
        let copied = std::io::copy(&mut file.by_ref().take(at_most + 1), &mut look);
        match copied {
            Ok(_) if look.written > at_most => {
                let _ = std::fs::remove_file(&target);
                lines.push(stays_inside(
                    &spelled,
                    &format!("larger than {}", human_bytes(at_most)),
                ));
                continue;
            }
            Ok(_) => {}
            Err(error) => {
                let _ = std::fs::remove_file(&target);
                if look.failed {
                    // The directory would not take it: this run's
                    // trouble, not the entry's.
                    return Err(std::io::Error::new(
                        error.kind(),
                        format!("writing {announced}: {error}"),
                    ));
                }
                lines.push(stays_inside(&spelled, &error.to_string()));
                continue;
            }
        }
        lines.push(json!({ "file": &announced, "mime": look.kind() }));
        // The name the zip spelled goes on the record, whether or not
        // the announcement could wear it: the announced name is a
        // handle in the directory, and the record never learns it.
        lines.push(json!({ "file": &announced, "attribute": "file:name", "value": name }));
        lines.push(
            json!({ "file": &announced, "attribute": "file:path", "value": inner(&spelled) }),
        );
    }
    Ok(lines)
}

/// The writer an entry streams through on its way into its file: the
/// beginning kept for the magic bytes, the whole watched for being
/// UTF-8, so the kind is known once the copy is done without the bytes
/// ever being held together.
struct Look<W: Write> {
    inner: W,
    head: Vec<u8>,
    /// Still UTF-8 as far as written.
    text: bool,
    /// The tail of the last chunk that was an incomplete sequence — a
    /// character cut by the chunk boundary, judged with the next chunk.
    carry: Vec<u8>,
    written: u64,
    /// Whether it was the file, not the entry, that failed.
    failed: bool,
}

impl<W: Write> Look<W> {
    fn new(inner: W) -> Self {
        Look {
            inner,
            head: Vec::with_capacity(HEAD),
            text: true,
            carry: Vec::new(),
            written: 0,
            failed: false,
        }
    }

    /// What the bytes say they are — a zip declares no kinds, so this
    /// is the look ingest would take: magic bytes first, a UTF-8 look
    /// for plain text second, and the honest shrug when nothing
    /// answers.
    fn kind(&self) -> String {
        infer::get(&self.head)
            .map(|kind| kind.mime_type().to_string())
            .or_else(|| {
                (self.written > 0 && self.text && self.carry.is_empty())
                    .then(|| "text/plain".to_string())
            })
            .unwrap_or_else(|| "application/octet-stream".to_string())
    }

    /// Whether the bytes so far, this chunk included, are still UTF-8 —
    /// an incomplete sequence at the chunk's end carried over, not
    /// judged yet.
    fn still_text(&mut self, chunk: &[u8]) -> bool {
        let mut bytes = std::mem::take(&mut self.carry);
        bytes.extend_from_slice(chunk);
        let Err(error) = std::str::from_utf8(&bytes) else {
            return true;
        };
        if error.error_len().is_some() {
            return false;
        }
        self.carry = bytes[error.valid_up_to()..].to_vec();
        true
    }
}

impl<W: Write> Write for Look<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let taken = self.inner.write(buf).inspect_err(|_| self.failed = true)?;
        let chunk = &buf[..taken];
        let room = HEAD - self.head.len();
        self.head.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if self.text {
            self.text = self.still_text(chunk);
        }
        self.written += taken as u64;
        Ok(taken)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush().inspect_err(|_| self.failed = true)
    }
}

/// A byte count as a person reads it — the bound, said once.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KiB", "MiB", "GiB", "TiB"];
    let mut unit = 0;
    let mut count = bytes;
    while count >= 1024 && count % 1024 == 0 && unit < UNITS.len() - 1 {
        count /= 1024;
        unit += 1;
    }
    format!("{count} {}", UNITS[unit])
}

/// An entry the examination could not bring out, said twice from one
/// wording: onto the record as a `prov:note` finding — a zip that
/// unpacked incompletely must not read like one that unpacked whole —
/// and onto stderr for whoever watches the run.
fn stays_inside(spelled: &str, reason: &str) -> serde_json::Value {
    let sentence = format!("entry {spelled} stayed inside: {reason}");
    eprintln!("ossuary-extract-packed: {sentence}");
    json!({ "attribute": "prov:note", "value": sentence })
}

/// The bare file name inside an entry's path: the last element past
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

#[cfg(test)]
mod tests {
    use tempfile::TempDir;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    use super::*;

    /// A zip built entry by entry, in the order given.
    fn packed(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            if name.ends_with('/') {
                writer
                    .add_directory(*name, SimpleFileOptions::default())
                    .unwrap();
            } else {
                let stored =
                    SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
                writer.start_file(*name, stored).unwrap();
                writer.write_all(bytes).unwrap();
            }
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn bytes_that_are_no_zip_are_an_empty_answer_not_an_error() {
        let dir = TempDir::new().unwrap();
        for bytes in [&b"plain words"[..], &b""[..], &b"PK\x03\x04truncated"[..]] {
            assert_eq!(
                harvest(bytes, Contract::List, None).unwrap(),
                Vec::<serde_json::Value>::new()
            );
            assert_eq!(
                harvest(bytes, Contract::Unpack, Some(dir.path())).unwrap(),
                Vec::<serde_json::Value>::new()
            );
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_container_names_itself_and_stays_shut() {
        let dir = TempDir::new().unwrap();
        let epub = packed(&[
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", b"<container/>"),
        ]);
        let odt = packed(&[
            ("mimetype", b"application/vnd.oasis.opendocument.text"),
            ("content.xml", b"<document/>"),
        ]);
        for (bytes, mime) in [
            (&epub, "application/epub+zip"),
            (&odt, "application/vnd.oasis.opendocument.text"),
        ] {
            let refined = vec![json!({ "attribute": "file:mime", "value": mime })];
            assert_eq!(harvest(bytes, Contract::List, None).unwrap(), refined);
            assert_eq!(
                harvest(bytes, Contract::Unpack, Some(dir.path())).unwrap(),
                refined
            );
        }
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "a document is not shredded into its innards"
        );
    }

    #[test]
    fn ooxml_is_known_by_its_type_manifest() {
        let docx = packed(&[
            ("[Content_Types].xml", b"<Types/>"),
            ("_rels/.rels", b"<Relationships/>"),
            ("word/document.xml", b"<document/>"),
        ]);
        assert_eq!(
            harvest(&docx, Contract::List, None).unwrap(),
            vec![json!({
                "attribute": "file:mime",
                "value": "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            })]
        );
        let xlsx = packed(&[
            ("[Content_Types].xml", b"<Types/>"),
            ("xl/workbook.xml", b"<workbook/>"),
        ]);
        assert_eq!(
            harvest(&xlsx, Contract::List, None).unwrap(),
            vec![json!({
                "attribute": "file:mime",
                "value": "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            })]
        );
        let unknown = packed(&[
            ("[Content_Types].xml", b"<Types/>"),
            ("other/x.xml", b"<x/>"),
        ]);
        assert_eq!(
            harvest(&unknown, Contract::List, None).unwrap(),
            Vec::<serde_json::Value>::new(),
            "recognized as a container, of no kind this program can name"
        );
    }

    #[test]
    fn only_the_first_entry_spoken_as_a_mime_makes_a_declaration() {
        let elsewhere = packed(&[("a.txt", b"hello"), ("mimetype", b"application/epub+zip")]);
        assert_eq!(
            harvest(&elsewhere, Contract::List, None).unwrap().len(),
            2,
            "a mimetype entry not first is an ordinary entry"
        );
        let prose = packed(&[("mimetype", b"just some words"), ("a.txt", b"hello")]);
        assert_eq!(
            harvest(&prose, Contract::List, None).unwrap().len(),
            2,
            "a first entry holding prose declares nothing"
        );
        assert!(mimey("application/epub+zip"));
        assert!(mimey("application/vnd.oasis.opendocument.text"));
        assert!(!mimey("just some words"));
        assert!(!mimey("application/"));
        assert!(!mimey("TEXT/PLAIN"));
    }

    #[test]
    fn list_tells_the_entries_and_leaves_the_directories_untold() {
        let bytes = packed(&[
            ("readme.txt", b"hello"),
            ("dir/", b""),
            ("dir/inner.bin", b"\x00\x01"),
        ]);
        assert_eq!(
            harvest(&bytes, Contract::List, None).unwrap(),
            vec![
                json!({ "attribute": "packed:path", "value": "@dir/inner.bin" }),
                json!({ "attribute": "packed:path", "value": "@readme.txt" }),
            ]
        );
    }

    #[test]
    fn unpack_flattens_yields_and_remembers_the_spelled_path() {
        let dir = TempDir::new().unwrap();
        let inner_zip = packed(&[("inside.txt", b"nested")]);
        let bytes = packed(&[
            ("a.txt", b"first"),
            ("dir/a.txt", b"second"),
            ("dir/deeper/carried.zip", &inner_zip),
        ]);

        let lines = harvest(&bytes, Contract::Unpack, Some(dir.path())).unwrap();

        let expect = |line: serde_json::Value| {
            assert!(lines.contains(&line), "missing {line}; got {lines:#?}");
        };
        expect(json!({ "file": "a.txt", "mime": "text/plain" }));
        expect(json!({ "file": "a.txt", "attribute": "file:name", "value": "a.txt" }));
        expect(json!({ "file": "a.txt", "attribute": "file:path", "value": "@a.txt" }));
        expect(json!({ "file": "a-2.txt", "mime": "text/plain" }));
        expect(json!({ "file": "a-2.txt", "attribute": "file:name", "value": "a.txt" }));
        expect(json!({ "file": "a-2.txt", "attribute": "file:path", "value": "@dir/a.txt" }));
        expect(json!({ "file": "carried.zip", "mime": "application/zip" }));
        expect(json!({ "file": "carried.zip", "attribute": "file:name", "value": "carried.zip" }));
        expect(json!({
            "file": "carried.zip",
            "attribute": "file:path",
            "value": "@dir/deeper/carried.zip",
        }));
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"first");
        assert_eq!(
            std::fs::read(dir.path().join("a-2.txt")).unwrap(),
            b"second"
        );
        assert_eq!(
            std::fs::read(dir.path().join("carried.zip")).unwrap(),
            inner_zip,
            "a nested zip comes out as itself, ready for the next round"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
    }

    #[test]
    fn an_encrypted_entry_stays_inside_and_the_reason_goes_on_the_record() {
        let dir = TempDir::new().unwrap();
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        writer.start_file("open.txt", stored).unwrap();
        writer.write_all(b"readable").unwrap();
        writer
            .start_file(
                "secret.txt",
                SimpleFileOptions::default().with_aes_encryption(zip::AesMode::Aes256, "hidden"),
            )
            .unwrap();
        writer.write_all(b"locked away").unwrap();
        let bytes = writer.finish().unwrap().into_inner();

        let lines = harvest(&bytes, Contract::Unpack, Some(dir.path())).unwrap();

        assert!(
            lines.contains(&json!({
                "attribute": "prov:note",
                "value": "entry secret.txt stayed inside: encrypted, and there is no password to offer",
            })),
            "got {lines:#?}"
        );
        assert!(
            lines.contains(&json!({ "file": "open.txt", "mime": "text/plain" })),
            "the locked door costs no other entry its examination; got {lines:#?}"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

        let inventory = harvest(&bytes, Contract::List, None).unwrap();
        assert!(
            inventory
                .iter()
                .all(|line| line["attribute"] != "prov:note"),
            "the inventory reads without unpacking — nothing stayed inside; got {inventory:#?}"
        );
        assert_eq!(inventory.len(), 2, "both entries are listed all the same");
    }

    #[test]
    fn a_damaged_entry_stays_inside_and_the_reason_goes_on_the_record() {
        let dir = TempDir::new().unwrap();
        let mut bytes = packed(&[("sound.txt", b"kept whole"), ("torn.txt", b"damaged goods")]);
        let at = bytes
            .windows(13)
            .position(|window| window == b"damaged goods")
            .unwrap();
        bytes[at] ^= 0xFF;

        let lines = harvest(&bytes, Contract::Unpack, Some(dir.path())).unwrap();

        assert!(
            lines.iter().any(|line| {
                line["attribute"] == "prov:note"
                    && line["value"]
                        .as_str()
                        .is_some_and(|value| value.starts_with("entry torn.txt stayed inside: "))
            }),
            "got {lines:#?}"
        );
        assert!(
            lines.contains(&json!({ "file": "sound.txt", "mime": "text/plain" })),
            "got {lines:#?}"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_symlink_stays_inside() {
        let dir = TempDir::new().unwrap();
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                "real.txt",
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(b"content").unwrap();
        writer
            .add_symlink("link.txt", "real.txt", SimpleFileOptions::default())
            .unwrap();
        let bytes = writer.finish().unwrap().into_inner();

        let lines = harvest(&bytes, Contract::Unpack, Some(dir.path())).unwrap();

        assert!(
            lines
                .iter()
                .all(|line| line.get("file") != Some(&json!("link.txt"))),
            "got {lines:#?}"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn names_shed_their_paths_and_yield_to_the_taken() {
        assert_eq!(basename("invoice.pdf"), Some("invoice.pdf".to_string()));
        assert_eq!(basename("deep/path/q3.pdf"), Some("q3.pdf".to_string()));
        assert_eq!(basename("evil/.."), None);
        assert_eq!(basename("dir/"), None);

        let mut taken = HashSet::new();
        assert_eq!(uniquify("a.txt".to_string(), &mut taken), "a.txt");
        assert_eq!(uniquify("A.txt".to_string(), &mut taken), "A-2.txt");
        assert_eq!(uniquify("a.txt".to_string(), &mut taken), "a-3.txt");
    }

    #[test]
    fn an_entry_streams_out_with_its_kind_read_on_the_way() {
        let dir = TempDir::new().unwrap();
        // A character cut by the copy's chunk boundary is still text;
        // a byte that is no UTF-8 far past the head is not.
        let straddling = format!("{}ä{}", "a".repeat(HEAD - 1), "b".repeat(100));
        let mut spoiled = "c".repeat(HEAD + 800).into_bytes();
        spoiled.push(0xff);
        let bytes = packed(&[
            ("straddling.txt", straddling.as_bytes()),
            ("spoiled.bin", &spoiled),
            ("empty", b""),
        ]);

        let lines = harvest(&bytes, Contract::Unpack, Some(dir.path())).unwrap();

        assert!(lines.contains(&json!({ "file": "straddling.txt", "mime": "text/plain" })));
        assert!(
            lines.contains(&json!({ "file": "spoiled.bin", "mime": "application/octet-stream" }))
        );
        assert!(lines.contains(&json!({ "file": "empty", "mime": "application/octet-stream" })));
        assert_eq!(
            std::fs::read(dir.path().join("straddling.txt")).unwrap(),
            straddling.as_bytes()
        );
        assert_eq!(
            std::fs::read(dir.path().join("spoiled.bin")).unwrap(),
            spoiled
        );
    }

    #[test]
    fn an_entry_larger_than_the_bound_stays_inside_with_the_reason_on_the_record() {
        let dir = TempDir::new().unwrap();
        let bytes = packed(&[("fits.txt", &[b'x'; 16]), ("bomb.txt", &[b'x'; 17])]);
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();

        let lines = unpack(&mut archive, dir.path(), 16).unwrap();

        assert!(lines.contains(&json!({ "file": "fits.txt", "mime": "text/plain" })));
        assert!(
            lines.contains(&json!({
                "attribute": "prov:note",
                "value": "entry bomb.txt stayed inside: larger than 16 bytes",
            })),
            "got {lines:#?}"
        );
        assert!(!lines.iter().any(|line| line["file"] == "bomb.txt"));
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "nothing half-written waits in the directory"
        );
        assert_eq!(human_bytes(UNPACKED_AT_MOST), "1 GiB");
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

        let dir = TempDir::new().unwrap();
        let spelled = format!("deep/{}", "x".repeat(300));
        let bytes = packed(&[(spelled.as_str(), b"content")]);

        let lines = harvest(&bytes, Contract::Unpack, Some(dir.path())).unwrap();

        let announced = "x".repeat(NAME_AT_MOST);
        assert!(
            lines.contains(&json!({ "file": &announced, "mime": "text/plain" })),
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
            "value": format!("@{spelled}"),
        })));
        assert!(dir.path().join(&announced).is_file());
    }
}
