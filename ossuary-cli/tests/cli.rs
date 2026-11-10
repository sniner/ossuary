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
    let head = open_bundles(&archive);
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
        open_bundles(&archive),
        head,
        "no claim of the stopped examination went on the record"
    );
}

/// The directories of the open segment in the archive's `open/`, by name.
fn open_dirs(archive: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(archive.join("open"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// The bundles of the open segment, by name.
fn open_bundles(archive: &Path) -> Vec<String> {
    let dirs = open_dirs(archive);
    assert_eq!(dirs.len(), 1, "one open segment: {dirs:?}");
    let mut names: Vec<String> = std::fs::read_dir(&dirs[0])
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| {
            Path::new(name)
                .extension()
                .is_some_and(|ext| ext == "jsonl")
        })
        .collect();
    names.sort();
    names
}

/// An archive whose last seal stopped right after it began: the open
/// segment renamed to `.sealing`, nothing else done.
fn archive_with_an_interrupted_seal() -> (TempDir, PathBuf) {
    let (dir, archive) = archive_with_a_file();
    let before = open_dirs(&archive);
    assert_eq!(before.len(), 1);
    let name = before[0]
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    std::fs::rename(
        &before[0],
        archive.join("open").join(format!("{name}.sealing")),
    )
    .unwrap();
    (dir, archive)
}

#[test]
fn an_interrupted_seal_is_sound_and_named_by_the_audit() {
    let (_dir, archive) = archive_with_an_interrupted_seal();

    let output = run(ossuary(&archive).arg("audit"));

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout
            .contains("a seal stopped before the open segment was back in place; nothing is lost"),
        "{stdout}"
    );
    assert!(stdout.contains("sound"), "{stdout}");
    let dirs = open_dirs(&archive);
    assert!(
        dirs.len() == 1
            && dirs[0]
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(".sealing"),
        "the audit wrote nothing: {dirs:?}"
    );
}

#[test]
fn the_next_run_that_writes_finishes_an_interrupted_seal_and_says_so() {
    let (dir, archive) = archive_with_an_interrupted_seal();
    let more = dir.path().join("more");
    std::fs::create_dir(&more).unwrap();
    std::fs::write(more.join("other.txt"), b"another note").unwrap();

    let output = run(ossuary(&archive).arg("-q").arg("ingest").arg(&more));

    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains(
            "a seal had stopped before the open segment was back; finished it, nothing was lost"
        ),
        "said even under -q: {stderr}"
    );
    let dirs = open_dirs(&archive);
    assert!(
        dirs.len() == 1 && dirs[0].file_name().unwrap().to_string_lossy().len() == 64,
        "the seal was finished, the directory is named after the segment: {dirs:?}"
    );
}

#[test]
fn a_run_inside_the_archive_names_it_by_its_absolute_path() {
    let (_dir, archive) = archive_with_a_file();

    let output = run(Command::new(env!("CARGO_BIN_EXE_ossuary"))
        .env_remove("OSSUARY_ARCHIVE")
        .current_dir(&archive)
        .arg("audit"));

    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    let expected = format!(
        "archive {}",
        std::fs::canonicalize(&archive).unwrap().display()
    );
    assert_eq!(stderr.lines().next(), Some(expected.as_str()), "{stderr}");
}

#[test]
fn audit_fast_misses_a_damaged_file_and_says_it_did_not_check() {
    let (_dir, archive) = archive_with_a_file();
    let name = name_of_the_note(&archive);
    let found = run(ossuary(&archive).args(["get", "--path", &name]));
    let path = PathBuf::from(String::from_utf8(found.stdout).unwrap().trim_end());
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    #[allow(
        clippy::permissions_set_readonly_false,
        reason = "the test plays the corruption"
    )]
    permissions.set_readonly(false);
    std::fs::set_permissions(&path, permissions).unwrap();
    std::fs::write(&path, b"a changed note").unwrap();

    let fast = run(ossuary(&archive).args(["audit", "--fast"]));

    assert!(fast.status.success(), "{fast:?}");
    let stdout = String::from_utf8(fast.stdout).unwrap();
    assert!(
        stdout.contains("content/: 1 file(s), not checked against their hashes (--fast)"),
        "{stdout}"
    );
    let verdict = stdout.lines().last().unwrap();
    assert!(
        verdict.starts_with("sound as far as --fast checks:")
            && verdict.contains("not checked against their hashes"),
        "{verdict}"
    );
    let stderr = String::from_utf8(fast.stderr).unwrap();
    assert!(
        stderr.contains("--fast, their contents are not read"),
        "{stderr}"
    );

    let json = run(ossuary(&archive).args(["audit", "--fast", "--json"]));

    assert!(json.status.success(), "{json:?}");
    assert_eq!(
        String::from_utf8(json.stdout).unwrap(),
        "{\"mode\":\"fast\"}\n",
        "a sound archive answers the mode line alone"
    );

    let full = run(ossuary(&archive).args(["audit", "--json"]));

    assert_eq!(full.status.code(), Some(1), "{full:?}");
    let stdout = String::from_utf8(full.stdout).unwrap();
    let mut lines = stdout.lines();
    assert_eq!(lines.next(), Some("{\"mode\":\"full\"}"), "{stdout}");
    assert!(
        lines.any(|line| line.contains("\"finding\":\"damaged\"")),
        "{stdout}"
    );
}

/// The full name of note.txt in an archive made by `archive_with_a_file`.
fn name_of_the_note(archive: &Path) -> String {
    let found = run(ossuary(archive).args(["find", "--id", "file:name=note.txt"]));
    String::from_utf8(found.stdout).unwrap().trim().to_string()
}

#[test]
fn get_path_prints_the_stored_file_and_nothing_else() {
    let (_dir, archive) = archive_with_a_file();
    let name = name_of_the_note(&archive);

    let output = run(ossuary(&archive).args(["get", "--path", &name[..8]]));

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let path = PathBuf::from(stdout.trim_end());
    assert!(path.is_absolute(), "{stdout}");
    assert!(
        path.starts_with(std::fs::canonicalize(&archive).unwrap().join("content")),
        "{stdout}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"a note");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
}

#[test]
fn get_path_prints_a_compressed_file_as_it_is_stored() {
    let (_dir, files, archive) = files_and_archive();
    assert!(run(ossuary(&archive).arg("init")).status.success());
    std::fs::write(archive.join("config.toml"), "[store]\ncompress = true\n").unwrap();
    assert!(
        run(ossuary(&archive).arg("ingest").arg(&files))
            .status
            .success()
    );
    let name = name_of_the_note(&archive);

    let output = run(ossuary(&archive).args(["get", "--path", &name]));

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.trim_end().ends_with(&format!("{name}.zst")),
        "{stdout}"
    );
}

#[test]
fn get_refuses_path_and_output_together() {
    let (dir, archive) = archive_with_a_file();
    let name = name_of_the_note(&archive);
    let target = dir.path().join("out");

    let output = run(ossuary(&archive)
        .args(["get", "--path", "--output"])
        .arg(&target)
        .arg(&name));

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(!target.exists());
}
