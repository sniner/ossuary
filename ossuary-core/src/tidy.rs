//! Temporary files that interrupted commands left behind.
//!
//! A file is written to `tmp/` in its store and gets its name with its
//! batch. A command that is killed before the batch is written leaves the
//! files of that batch in `tmp/`: nothing reads them and no later run
//! reuses them, so they only take space until they are removed here.
//!
//! A file in `tmp/` can also belong to a command that is running. Nothing
//! in the archive tells the two apart, on a share not even the process id
//! in the name, so the age decides unless the caller knows better.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::archive::Archive;
use crate::error::{Error, Result};

/// The age below which a temporary file may belong to a running command.
pub const TEMP_MIN_AGE: Duration = immure::DEFAULT_TEMP_MIN_AGE;

/// What [`tidy`] found in the `tmp/` directories of an archive.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Tidied {
    /// Temporary files removed, or under `dry_run` the ones a run would
    /// remove.
    pub removed: usize,
    /// Their size in bytes.
    pub bytes: u64,
    /// Temporary files kept because they are younger than the age asked
    /// for.
    pub young: usize,
}

/// Remove the temporary files in `content/tmp`, `derived/tmp` and
/// `claims/tmp` that are older than `min_age`, or all of them when it is
/// `None`. Under `dry_run` nothing is removed and the answer says what
/// would be.
///
/// Only files named the way the stores name their temporary files
/// (`<pid>-<serial>.tmp`) are touched.
///
/// # Errors
///
/// [`Error::Io`] when a `tmp/` directory cannot be listed or a file not
/// removed.
pub fn tidy(archive: &Archive, min_age: Option<Duration>, dry_run: bool) -> Result<Tidied> {
    let cutoff = min_age.and_then(|age| SystemTime::now().checked_sub(age));
    let mut tidied = Tidied::default();
    for store in [archive.content(), archive.derived(), archive.log().store()] {
        tidy_dir(&store.root().join("tmp"), cutoff, dry_run, &mut tidied)?;
    }
    Ok(tidied)
}

fn tidy_dir(
    dir: &Path,
    cutoff: Option<SystemTime>,
    dry_run: bool,
    tidied: &mut Tidied,
) -> Result<()> {
    let io = |what: &str, path: &Path| {
        let context = format!("{}: {what}", path.display());
        move |source| Error::Io { context, source }
    };
    let listing = match fs::read_dir(dir) {
        Ok(listing) => listing,
        Err(source) if source.kind() == ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io("listing", dir)(source)),
    };
    for entry in listing {
        let entry = entry.map_err(io("listing", dir))?;
        if !entry.file_name().to_str().is_some_and(is_temp_name) {
            continue;
        }
        let path = entry.path();
        // Not followed: a link is not a file a store wrote.
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            // Gone since the listing: its writer named it or removed it.
            Err(source) if source.kind() == ErrorKind::NotFound => continue,
            Err(source) => return Err(io("reading", &path)(source)),
        };
        if !metadata.is_file() {
            continue;
        }
        let young = match (cutoff, metadata.modified()) {
            (Some(cutoff), Ok(modified)) => modified > cutoff,
            // A file whose age cannot be read is not known to be old.
            (Some(_), Err(_)) => true,
            (None, _) => false,
        };
        if young {
            tidied.young += 1;
            continue;
        }
        if !dry_run {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(source) if source.kind() == ErrorKind::NotFound => continue,
                Err(source) => return Err(io("removing", &path)(source)),
            }
        }
        tidied.removed += 1;
        tidied.bytes += metadata.len();
    }
    Ok(())
}

/// Whether `name` is one the stores give a temporary file:
/// `<pid>-<serial>.tmp`, both numbers.
fn is_temp_name(name: &str) -> bool {
    let Some((pid, serial)) = name
        .strip_suffix(".tmp")
        .and_then(|stem| stem.split_once('-'))
    else {
        return false;
    };
    let number = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    number(pid) && number(serial)
}

#[cfg(test)]
mod tests {
    use immure::Algorithm;
    use tempfile::TempDir;

    use super::*;

    fn archive(dir: &TempDir) -> Archive {
        Archive::create(dir.path().join("archive"), Algorithm::Sha256).unwrap()
    }

    /// Files as a killed command leaves them: written, never named.
    fn leave(archive: &Archive, count: usize) -> Vec<std::path::PathBuf> {
        let tmp = archive.content().root().join("tmp");
        fs::create_dir_all(&tmp).unwrap();
        (0..count)
            .map(|n| {
                let path = tmp.join(format!("4711-{n}.tmp"));
                fs::write(&path, b"left behind").unwrap();
                path
            })
            .collect()
    }

    #[test]
    fn files_of_a_batch_that_was_never_written_are_removed() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let left = leave(&archive, 3);

        let tidied = tidy(&archive, None, false).unwrap();

        assert_eq!(
            tidied,
            Tidied {
                removed: 3,
                bytes: 33,
                young: 0
            }
        );
        assert!(left.iter().all(|path| !path.exists()));
        assert_eq!(tidy(&archive, None, false).unwrap(), Tidied::default());
    }

    #[test]
    fn a_young_file_may_belong_to_a_running_command_and_is_kept() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let left = leave(&archive, 2);

        let tidied = tidy(&archive, Some(TEMP_MIN_AGE), false).unwrap();

        assert_eq!(tidied.removed, 0);
        assert_eq!(tidied.young, 2);
        assert!(left.iter().all(|path| path.is_file()));
    }

    #[test]
    fn a_dry_run_counts_and_removes_nothing() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let left = leave(&archive, 2);

        let tidied = tidy(&archive, None, true).unwrap();

        assert_eq!(tidied.removed, 2);
        assert!(left.iter().all(|path| path.is_file()));
    }

    #[test]
    fn a_file_the_stores_did_not_name_is_left_alone() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let tmp = archive.derived().root().join("tmp");
        fs::create_dir_all(&tmp).unwrap();
        let foreign = tmp.join("notes.tmp");
        fs::write(&foreign, b"not ours").unwrap();

        assert_eq!(tidy(&archive, None, false).unwrap(), Tidied::default());
        assert!(foreign.is_file());
    }

    #[test]
    fn a_file_waiting_in_a_batch_is_what_a_killed_command_leaves() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir).flush_files();
        archive.content().add(b"in the open batch").unwrap();

        // What tidy counts is exactly what a store writes before a flush.
        let tidied = tidy(&archive, None, true).unwrap();

        assert_eq!(tidied.removed, 1);
        archive.flush().unwrap();
        assert_eq!(tidy(&archive, None, true).unwrap(), Tidied::default());
    }
}
