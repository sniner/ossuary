//! The extractor as `ossuary extract` runs it: the document on stdin,
//! lopdf in the worker process.

use std::io::Write as _;
use std::process::{Command, Output, Stdio};

use flate2::Compression;
use flate2::write::ZlibEncoder;
use lopdf::{Document, Object, Stream, dictionary};
use serde_json::{Value, json};
use tempfile::TempDir;

fn run(contract: &str, document: &[u8]) -> (Output, TempDir) {
    let directory = TempDir::new().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_ossuary-extract-pdf"))
        .arg(contract)
        .arg(directory.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The extractor reads all of stdin before it writes anything.
    child.stdin.take().unwrap().write_all(document).unwrap();
    (child.wait_with_output().unwrap(), directory)
}

fn lines(output: &Output) -> Vec<Value> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn notes(output: &Output) -> Vec<String> {
    lines(output)
        .into_iter()
        .filter(|line| line["attribute"] == "prov:note")
        .map(|line| line["value"].as_str().unwrap().to_string())
        .collect()
}

/// A PDF whose cross-reference stream inflates to `mib` MiB of zeros.
/// lopdf inflates it while it opens the document.
fn bomb(mib: usize) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
    let zeros = vec![0u8; 1 << 20];
    for _ in 0..mib {
        encoder.write_all(&zeros).unwrap();
    }
    let packed = encoder.finish().unwrap();
    let mut pdf = b"%PDF-1.5\n".to_vec();
    let start = pdf.len();
    pdf.extend_from_slice(
        format!(
            "1 0 obj\n<< /Type /XRef /Size 2 /W [1 1 1] /Filter /FlateDecode /Length {} >>\nstream\n",
            packed.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(&packed);
    pdf.extend_from_slice(format!("\nendstream\nendobj\nstartxref\n{start}\n%%EOF\n").as_bytes());
    pdf
}

/// A one-page PDF with `content` embedded as `note.txt`.
fn with_attachment(content: &[u8]) -> Vec<u8> {
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
    let mut stream = Stream::new(
        dictionary! {
            "Type" => "EmbeddedFile",
            "Subtype" => Object::Name(b"text/plain".to_vec()),
        },
        content.to_vec(),
    );
    stream.compress().unwrap();
    let stream = document.add_object(stream);
    let specification = document.add_object(dictionary! {
        "Type" => "Filespec",
        "F" => Object::string_literal("note.txt"),
        "EF" => dictionary! { "F" => Object::Reference(stream) },
    });
    let tree = document.add_object(dictionary! {
        "Names" => vec![Object::string_literal("note.txt"), Object::Reference(specification)],
    });
    let names = document.add_object(dictionary! { "EmbeddedFiles" => Object::Reference(tree) });
    let catalog = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => Object::Reference(pages),
        "Names" => Object::Reference(names),
    });
    document.trailer.set("Root", Object::Reference(catalog));
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}

#[test]
fn an_attachment_comes_out_through_the_worker() {
    let content = b"an embedded note, long enough for the writer to deflate it\n".repeat(4);
    let (output, directory) = run("attachments", &with_attachment(&content));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(directory.path().join("note.txt")).unwrap(),
        content
    );
    assert!(lines(&output).contains(&json!({ "file": "note.txt", "mime": "text/plain" })));
}

#[test]
fn a_document_that_needs_more_memory_than_the_limit_gets_a_note() {
    let (output, directory) = run("attachments", &bomb(128));
    assert!(
        output.status.success(),
        "the examination counts; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(lines(&output).len(), 1, "nothing but the note");
    let note = &notes(&output)[0];
    assert!(
        note.starts_with("attachments not extracted: the document needs more than ")
            && note.ends_with(" MiB of memory"),
        "{note}"
    );
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn the_text_contract_notes_the_limit_and_goes_on() {
    if Command::new("pdftotext").arg("-v").output().is_err() {
        eprintln!("skipped: no pdftotext on PATH");
        return;
    }
    let (output, _directory) = run("text", &bomb(128));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let notes = notes(&output);
    assert!(
        notes[0].starts_with("document information not read: the document needs more than "),
        "{notes:?}"
    );
}
