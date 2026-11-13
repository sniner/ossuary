//! Adding the messages in a directory of files, one message per file.
//!
//! The files come from a backup or an export of a mailbox: a server that
//! no longer runs, a Maildir copied out of an archive file. Each message
//! is stored as a fetched one is (admit, then record), and its place is
//! the account name the caller gives and, as the folder, the directory
//! the file is in below the directory given. The account needs no entry
//! in `mailvault.toml`: nothing is fetched from it.
//!
//! The folder is written in Unicode NFC, the form a fetch records:
//! macOS reads names from a share or from HFS+ in NFD, so `ü` would
//! otherwise be `u` and a combining diaeresis, and the place would not
//! match the one a fetch of the folder records.
//!
//! A file is added only if the mail extractor would read it as a
//! message, so a README or an index file of the mail server in the
//! directory is not stored as one. Files whose names begin with a dot
//! are not looked at: the metadata files a file manager leaves on a
//! share.
//!
//! Nothing is remembered between runs. A second run over the same
//! directory stores nothing new and records each place again, as a fetch
//! with `--full` does.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use ossuary_core::{Archive, Attribute, Sighting, Source, Timestamp, admit, record};
use ossuary_extract_mail::looks_like_mail;
use serde_json::json;
use unicode_normalization::UnicodeNormalization as _;

use crate::output::{Say, counted};
use crate::place;
use crate::proton;
use crate::tally::Tally;

/// How much of a file a dry run reads to tell whether it is a message.
/// A header section is rarely longer; a longer one is cut off inside a
/// line, which the test reads as the end of the headers.
const HEAD: u64 = 64 * 1024;

pub struct Options<'a> {
    /// The account name of every place recorded.
    pub account: &'a str,
    /// The time of the claims, when it is not the time of the run.
    pub seen: Option<Timestamp>,
    /// Count what would be added, write nothing.
    pub dry_run: bool,
}

/// One file below the directory: its path, and the place of its message.
struct Found {
    path: PathBuf,
    place: String,
}

/// Add every message file below `dir` to the archive.
///
/// # Errors
///
/// A directory that cannot be listed or holds no files, and the archive
/// refusing a message. A file that cannot be read or is not a message is
/// named in the tally, and the run goes on.
pub fn run(archive: &Archive, dir: &Path, options: &Options<'_>, say: Say) -> Result<Tally> {
    let mut tally = Tally::new("add");
    let mut files = Vec::new();
    walk(dir, dir, options.account, &mut files)?;
    if files.is_empty() {
        bail!("{}: no files in this directory", dir.display());
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    say.line(format_args!(
        "{} in {}",
        counted(files.len(), "file", "files"),
        dir.display()
    ));

    let log = archive.log();
    let source = Source::parse(crate::SOURCE)?;
    let place = Attribute::parse(place::ATTRIBUTE)?;
    let id = Attribute::parse(proton::ATTRIBUTE)?;
    // Claims with an earlier time are kept in segments of their own, as
    // `Sighting::time` asks.
    let apart = options.seen.is_some() && !options.dry_run;
    if apart {
        log.seal()?;
    }
    let total = files.len();
    let mut progress = say.progress();
    for (index, found) in files.iter().enumerate() {
        let bytes = match read(&found.path, options.dry_run) {
            Ok(bytes) if looks_like_mail(&bytes) => bytes,
            Ok(_) => {
                tally.failed.push(format!(
                    "{}: not a message, not added",
                    found.path.display()
                ));
                continue;
            }
            Err(error) => {
                tally.failed.push(format!("{error:#}"));
                continue;
            }
        };
        if options.dry_run {
            tally.would += 1;
        } else {
            let admitted = admit(archive.content(), &bytes[..])?;
            if admitted.is_new() {
                tally.stored += 1;
            } else {
                tally.known += 1;
            }
            let mut facts = vec![(place.clone(), json!(found.place))];
            if let Some(proton) = proton::id_of(&bytes) {
                facts.push((id.clone(), json!(proton)));
            }
            tally.claims += record(
                log,
                &admitted,
                &Sighting {
                    source: &source,
                    run: &tally.run,
                    mime: Some(crate::MESSAGE),
                    facts: &facts,
                    tags: &[],
                    time: options.seen.as_ref(),
                },
            )?
            .len();
        }
        let done = index + 1;
        progress.update(
            done,
            &format!("adding: {done} of {total} file(s), {} stored", tally.stored),
        );
    }
    progress.finish();
    if apart {
        log.seal()?;
    }
    Ok(tally)
}

/// Every regular file below `dir`, with the place its directory gives
/// it. Links are not followed.
fn walk(root: &Path, dir: &Path, account: &str, files: &mut Vec<Found>) -> Result<()> {
    let entries = fs::read_dir(dir).with_context(|| format!("{}: listing", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("{}: listing", dir.display()))?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .with_context(|| format!("{}: reading its type", path.display()))?;
        if kind.is_dir() {
            walk(root, &path, account, files)?;
        } else if kind.is_file() && !entry.file_name().to_string_lossy().starts_with('.') {
            files.push(Found {
                place: place_of(root, dir, account),
                path,
            });
        }
    }
    Ok(())
}

/// The place of the messages in `dir`: the account, and as the folder
/// the path of `dir` below `root` in NFC, with `/` between its parts
/// whatever the system writes there. The account alone for `root`
/// itself.
fn place_of(root: &Path, dir: &Path, account: &str) -> String {
    let below = dir.strip_prefix(root).unwrap_or(dir);
    let parts: Vec<String> = below
        .components()
        .map(|part| part.as_os_str().to_string_lossy().nfc().collect())
        .collect();
    if parts.is_empty() {
        place::account(account)
    } else {
        place::folder(account, &parts.join("/"))
    }
}

/// The bytes of a file: all of them, or only the beginning when nothing
/// is stored.
fn read(path: &Path, head_only: bool) -> Result<Vec<u8>> {
    let context = || format!("{}: reading", path.display());
    if !head_only {
        return fs::read(path).with_context(context);
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .with_context(context)?
        .take(HEAD)
        .read_to_end(&mut bytes)
        .with_context(context)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use ossuary_core::{Algorithm, Index, Scope, Subject, Value};
    use tempfile::TempDir;

    use super::*;

    const MESSAGE: &[u8] = b"From: a@example.org\nTo: b@example.org\nSubject: Hello\n\nHi.\n";
    const OTHER: &[u8] = b"From: a@example.org\nTo: c@example.org\nSubject: Again\n\nHi.\n";

    struct Bench {
        _dir: TempDir,
        archive: Archive,
        files: PathBuf,
    }

    fn bench() -> Bench {
        let dir = TempDir::new().unwrap();
        let archive = Archive::create(dir.path().join("archive"), Algorithm::Sha256).unwrap();
        let files = dir.path().join("files");
        fs::create_dir(&files).unwrap();
        Bench {
            _dir: dir,
            archive,
            files,
        }
    }

    fn write(bench: &Bench, path: &str, bytes: &[u8]) {
        let path = bench.files.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn add(bench: &Bench, seen: Option<&str>, dry_run: bool) -> Tally {
        run(
            &bench.archive,
            &bench.files,
            &Options {
                account: "backup",
                seen: seen.map(|time| Timestamp::parse(time).unwrap()),
                dry_run,
            },
            Say::new(true),
        )
        .unwrap()
    }

    fn subject(bench: &Bench, bytes: &[u8]) -> Subject {
        let digest = bench.archive.content().algorithm().hash(bytes);
        Subject::parse(digest.as_str()).unwrap()
    }

    fn standing(bench: &Bench, bytes: &[u8], attribute: &str) -> Vec<Value> {
        let mut index = Index::open(bench.files.join("../index.sqlite")).unwrap();
        index.fold(bench.archive.log()).unwrap();
        index
            .values(
                &subject(bench, bytes),
                &Attribute::parse(attribute).unwrap(),
                Scope::Held,
            )
            .unwrap()
    }

    #[test]
    fn a_message_gets_the_account_and_its_directory_as_its_place() {
        let bench = bench();
        write(&bench, "Archive/2010/1.eml", MESSAGE);
        write(&bench, "2.eml", OTHER);

        let tally = add(&bench, None, false);

        assert_eq!((tally.stored, tally.known), (2, 0));
        assert!(tally.failed.is_empty());
        assert_eq!(
            standing(&bench, MESSAGE, "mailbox:place"),
            vec![json!("backup:Archive/2010")]
        );
        assert_eq!(
            standing(&bench, OTHER, "mailbox:place"),
            vec![json!("backup")],
            "a file in the directory itself has no folder"
        );
        assert_eq!(
            standing(&bench, MESSAGE, "file:mime"),
            vec![json!("message/rfc822")]
        );
        assert_eq!(
            standing(&bench, MESSAGE, "file:size"),
            vec![json!(MESSAGE.len())]
        );
    }

    #[test]
    fn a_folder_name_is_recorded_in_nfc() {
        let decomposed = "Entwu\u{308}rfe";
        assert_eq!(
            place_of(
                Path::new("/backup"),
                &Path::new("/backup").join(decomposed),
                "backup"
            ),
            "backup:Entw\u{fc}rfe"
        );
    }

    #[test]
    fn a_message_in_two_directories_is_stored_once_with_both_places() {
        let bench = bench();
        write(&bench, "INBOX/1.eml", MESSAGE);
        write(&bench, "Sent/1.eml", MESSAGE);

        let tally = add(&bench, None, false);

        assert_eq!((tally.stored, tally.known), (1, 1));
        assert_eq!(
            standing(&bench, MESSAGE, "mailbox:place"),
            vec![json!("backup:INBOX"), json!("backup:Sent")]
        );
    }

    #[test]
    fn a_file_that_is_no_message_is_named_and_not_added() {
        let bench = bench();
        write(&bench, "INBOX/1.eml", MESSAGE);
        write(&bench, "README.md", b"# Mail\n\nSubject: nothing\n");
        write(&bench, "INBOX/mbox", b"From a@example.org Thu Sep  4 12:00:00 2026\nFrom: a@example.org\nTo: b@example.org\n\nHi.\n");
        write(&bench, "INBOX/.DS_Store", b"\0\0\0\x01Bud1");

        let tally = add(&bench, None, false);

        assert_eq!(tally.stored, 1);
        assert_eq!(tally.failed.len(), 2, "{:?}", tally.failed);
        assert!(
            tally
                .failed
                .iter()
                .all(|line| line.ends_with("not a message, not added")),
            "{:?}",
            tally.failed
        );
        assert_eq!(bench.archive.content().entries().count(), 1);
    }

    #[test]
    fn a_dry_run_counts_and_writes_nothing() {
        let bench = bench();
        write(&bench, "INBOX/1.eml", MESSAGE);
        write(&bench, "README.md", b"# Mail\n");

        let tally = add(&bench, None, true);

        assert_eq!(tally.would, 1);
        assert_eq!(tally.failed.len(), 1);
        assert_eq!(bench.archive.content().entries().count(), 0);
        assert!(bench.archive.log().head().unwrap().is_empty());
    }

    #[test]
    fn a_given_time_is_the_time_of_every_claim() {
        let bench = bench();
        write(&bench, "INBOX/1.eml", MESSAGE);

        add(&bench, Some("2019-10-05T00:00:00Z"), false);

        let log = bench.archive.log();
        let segments = log.segments().unwrap();
        assert_eq!(segments.len(), 1, "the claims are sealed apart");
        let claims = log.read(segments[0].digest()).unwrap();
        assert!(!claims.is_empty());
        assert!(
            claims
                .iter()
                .all(|claim| claim.time().as_str() == "2019-10-05T00:00:00Z")
        );
        assert!(log.head().unwrap().is_empty());
    }

    #[test]
    fn a_second_run_stores_nothing_new() {
        let bench = bench();
        write(&bench, "INBOX/1.eml", MESSAGE);
        add(&bench, None, false);

        let tally = add(&bench, None, false);

        assert_eq!((tally.stored, tally.known), (0, 1));
        assert_eq!(
            standing(&bench, MESSAGE, "mailbox:place"),
            vec![json!("backup:INBOX")]
        );
    }

    #[test]
    fn a_directory_without_files_is_refused() {
        let bench = bench();
        let error = run(
            &bench.archive,
            &bench.files,
            &Options {
                account: "backup",
                seen: None,
                dry_run: false,
            },
            Say::new(true),
        )
        .unwrap_err();
        assert!(error.to_string().contains("no files"), "{error}");
    }
}
