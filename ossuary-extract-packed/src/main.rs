//! The packed extractor: a zip archive in, its inventory or its files
//! out — two contracts in one program.
//!
//! Speaks the ossuary extractor protocol (`docs/extractors.md`), as its
//! first program of two trades: `--identify` answers two lines, the
//! `list` contract and the `unpack` contract, each with its own source
//! and its own receipts. Called with the contract's name as the first
//! argument — and, for `unpack`, the output directory as the second —
//! it reads one file's bytes from stdin. `list` tells every entry the
//! archive holds, one `zip:entry` finding each, without unpacking a
//! byte; `unpack` writes every entry out as a file of its own.
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
//! bare — so colliding names yield to a counter, the true name goes on
//! the record as `file:name`, and every file's full entry path as
//! `zip:path`. A zip declares no kinds, so each announcement carries
//! the same magic-bytes-then-UTF-8 look ingest would take. An entry
//! that will not come out — encrypted, damaged, or a symlink, whose
//! bytes are a name rather than content — stays inside with a line on
//! stderr; there is no password to offer, and a receipt beats being
//! offered the same locked door every run.

use std::collections::HashSet;
use std::io::{Cursor, Read, Seek};
use std::path::Path;
use std::process::ExitCode;

use serde_json::json;
use zip::ZipArchive;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--identify"] => {
            let version = env!("CARGO_PKG_VERSION");
            println!(
                "{}",
                json!({
                    "ossuary-extractor": 1,
                    "contract": "list",
                    "source": format!("extractor:packed-list/{version}"),
                    "mimes": ["application/zip"],
                })
            );
            println!(
                "{}",
                json!({
                    "ossuary-extractor": 1,
                    "contract": "unpack",
                    "source": format!("extractor:packed-unpack/{version}"),
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
                "ossuary-extract-packed: run with --identify, with `list`, or with `unpack DIR` — a file's bytes on stdin either way"
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

/// The inventory: one finding per file entry, the entry's name as the
/// zip spells it. Directories are structure, not content, and stay
/// untold. Sorted so the answer reads the same however the zip was
/// written — the record holds a set either way.
fn list<R: Read + Seek>(archive: &ZipArchive<R>) -> Vec<serde_json::Value> {
    let mut entries: Vec<&str> = archive
        .file_names()
        .filter(|name| !name.ends_with('/'))
        .collect();
    entries.sort_unstable();
    entries
        .into_iter()
        .map(|entry| json!({ "attribute": "zip:entry", "value": entry }))
        .collect()
}

/// Every entry out as a file of its own: flattened to its bare name,
/// yielded to a counter on collision, announced with the kind its own
/// bytes answer to. What will not come out stays inside with a word on
/// stderr and costs no other entry its examination.
fn unpack<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    directory: &Path,
) -> std::io::Result<Vec<serde_json::Value>> {
    let mut lines = Vec::new();
    let mut taken = HashSet::new();
    for index in 0..archive.len() {
        let mut file = match archive.by_index(index) {
            Ok(file) => file,
            Err(error) => {
                eprintln!("ossuary-extract-packed: entry {index}: {error} — stays inside");
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
        let mut bytes = Vec::new();
        if let Err(error) = file.read_to_end(&mut bytes) {
            eprintln!("ossuary-extract-packed: {spelled}: {error} — stays inside");
            continue;
        }
        let Some(name) = basename(&spelled) else {
            eprintln!("ossuary-extract-packed: {spelled:?}: no file name in it — stays inside");
            continue;
        };
        let announced = uniquify(name.clone(), &mut taken);
        std::fs::write(directory.join(&announced), &bytes).map_err(|error| {
            std::io::Error::new(error.kind(), format!("writing {announced}: {error}"))
        })?;
        lines.push(json!({ "file": &announced, "mime": sniff(&bytes) }));
        if name != announced {
            // The announcement had to yield to a name already taken; the
            // name the zip spelled goes on the record beside it.
            lines.push(json!({ "file": &announced, "attribute": "file:name", "value": name }));
        }
        lines.push(json!({ "file": &announced, "attribute": "zip:path", "value": spelled }));
    }
    Ok(lines)
}

/// What the bytes say they are — a zip declares no kinds, so this is
/// the look ingest would take: magic bytes first, a UTF-8 look for
/// plain text second, and the honest shrug when nothing answers.
fn sniff(bytes: &[u8]) -> String {
    infer::get(bytes)
        .map(|kind| kind.mime_type().to_string())
        .or_else(|| {
            (!bytes.is_empty() && std::str::from_utf8(bytes).is_ok())
                .then(|| "text/plain".to_string())
        })
        .unwrap_or_else(|| "application/octet-stream".to_string())
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
    use std::io::Write as _;

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
                json!({ "attribute": "zip:entry", "value": "dir/inner.bin" }),
                json!({ "attribute": "zip:entry", "value": "readme.txt" }),
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
        expect(json!({ "file": "a.txt", "attribute": "zip:path", "value": "a.txt" }));
        expect(json!({ "file": "a-2.txt", "mime": "text/plain" }));
        expect(json!({ "file": "a-2.txt", "attribute": "file:name", "value": "a.txt" }));
        expect(json!({ "file": "a-2.txt", "attribute": "zip:path", "value": "dir/a.txt" }));
        expect(json!({ "file": "carried.zip", "mime": "application/zip" }));
        expect(json!({
            "file": "carried.zip",
            "attribute": "zip:path",
            "value": "dir/deeper/carried.zip",
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
}
