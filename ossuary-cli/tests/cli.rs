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
