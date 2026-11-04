//! The command line as a script sees it: exit codes, and what happens
//! when the other end of a pipe is gone.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

fn ossuary(archive: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ossuary"));
    command
        .env_remove("OSSUARY_ARCHIVE")
        .arg("--archive")
        .arg(archive);
    command
}

/// The write end of a pipe whose read end is already closed, so that
/// the first write fails with a broken pipe.
fn closed_pipe() -> Stdio {
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    writer.into()
}

fn run(command: &mut Command) -> Output {
    command.stdin(Stdio::null()).output().unwrap()
}

/// A directory with one file to ingest, and where the archive goes.
fn files_and_archive() -> (TempDir, PathBuf, PathBuf) {
    let dir = TempDir::new().unwrap();
    let files = dir.path().join("files");
    std::fs::create_dir(&files).unwrap();
    std::fs::write(files.join("note.txt"), b"a note").unwrap();
    let archive = dir.path().join("archive");
    (dir, files, archive)
}

/// An archive holding one file.
fn archive_with_a_file() -> (TempDir, PathBuf) {
    let (dir, files, archive) = files_and_archive();
    assert!(run(ossuary(&archive).arg("init")).status.success());
    assert!(
        run(ossuary(&archive).arg("ingest").arg(&files))
            .status
            .success()
    );
    (dir, archive)
}

#[test]
fn a_closed_stdout_stops_the_output_and_the_command_succeeds() {
    let (_dir, _, archive) = files_and_archive();
    let output = ossuary(&archive)
        .arg("init")
        .stdin(Stdio::null())
        .stdout(closed_pipe())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{:?}: {stderr}", output.status);
    assert!(archive.join("FORMAT").is_file(), "the archive was created");
}

#[test]
fn a_closed_stderr_does_not_stop_the_command() {
    let (_dir, files, archive) = files_and_archive();
    assert!(run(ossuary(&archive).arg("init")).status.success());
    let output = ossuary(&archive)
        .arg("ingest")
        .arg(&files)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(closed_pipe())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{:?}: {stdout}", output.status);
    assert!(stdout.contains("1 file(s) stored"), "{stdout}");
}

#[test]
fn about_a_name_that_matches_no_file_exits_1() {
    let (_dir, archive) = archive_with_a_file();
    let output = run(ossuary(&archive).args(["about", "ffffffff"]));
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no subject begins with \"ffffffff\""),
        "{stderr}"
    );
}

#[test]
fn about_a_recorded_file_exits_0() {
    let (_dir, archive) = archive_with_a_file();
    let found = run(ossuary(&archive).args(["find", "--id", "file:name=note.txt"]));
    let name = String::from_utf8(found.stdout).unwrap();
    let output = run(ossuary(&archive).arg("about").arg(name.trim()));
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("file:name = \"note.txt\""));
}

/// An archive holding two text files, and a directory holding the test
/// extractor `ossuary-extract-fake`. It reads text/plain, refuses a file
/// containing "refuse", and otherwise reports one finding and writes one
/// derived file.
#[cfg(unix)]
fn archive_and_fake_extractor() -> (TempDir, PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = TempDir::new().unwrap();
    let files = dir.path().join("files");
    std::fs::create_dir(&files).unwrap();
    std::fs::write(files.join("a.txt"), b"accept this one").unwrap();
    std::fs::write(files.join("b.txt"), b"refuse this one").unwrap();
    let archive = dir.path().join("archive");
    assert!(run(ossuary(&archive).arg("init")).status.success());
    assert!(
        run(ossuary(&archive).arg("ingest").arg(&files))
            .status
            .success()
    );

    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let fake = bin.join("ossuary-extract-fake");
    std::fs::write(
        &fake,
        r#"#!/bin/sh
if [ "$1" = "--identify" ]; then
    echo '{"ossuary-extractor":1,"source":"extractor:fake/1","mimes":["text/plain"],"derives":true}'
    exit 0
fi
input=$(cat)
case "$input" in
    *refuse*) echo "ossuary-extract-fake: refused" >&2; exit 1 ;;
esac
echo derived > "$1/out.bin"
echo '{"attribute":"fake:seen","value":true}'
echo '{"file":"out.bin","mime":"application/octet-stream"}'
"#,
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    (dir, archive, bin)
}

/// `ossuary extract fake` with the test extractor first on PATH.
#[cfg(unix)]
fn extract_fake(archive: &Path, bin: &Path) -> Output {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![bin.to_path_buf()];
    paths.extend(std::env::split_paths(&path));
    run(ossuary(archive)
        .args(["extract", "fake"])
        .env("PATH", std::env::join_paths(paths).unwrap()))
}

#[test]
#[cfg(unix)]
fn a_refused_file_is_reported_at_once_and_the_run_goes_on() {
    let (_dir, archive, bin) = archive_and_fake_extractor();

    let output = extract_fake(&archive, &bin);

    assert_eq!(output.status.code(), Some(1), "a failed file fails the run");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("could not examine")
            && stderr.contains("(b.txt): `ossuary-extract-fake` failed"),
        "{stderr}"
    );
    assert!(
        stderr.contains("1 file(s) could not be examined, each reported above"),
        "{stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("1 file(s) examined by extractor:fake/1"),
        "the other file was examined: {stdout}"
    );
}

#[test]
#[cfg(unix)]
fn an_archive_that_cannot_store_a_derived_file_stops_the_run_and_records_nothing() {
    use std::os::unix::fs::PermissionsExt as _;

    let (_dir, archive, bin) = archive_and_fake_extractor();
    let head = std::fs::read(archive.join("head.jsonl")).unwrap();
    let derived = archive.join("derived");
    std::fs::set_permissions(&derived, std::fs::Permissions::from_mode(0o555)).unwrap();

    let output = extract_fake(&archive, &bin);
    std::fs::set_permissions(&derived, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("stopped at") && stderr.contains("(a.txt)"),
        "{stderr}"
    );
    assert!(
        stderr.contains("the next run starts again with this one"),
        "{stderr}"
    );
    assert_eq!(
        std::fs::read(archive.join("head.jsonl")).unwrap(),
        head,
        "no claim of the stopped examination went on the record"
    );
}
