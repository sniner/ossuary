//! A directory as the place a copy lies: a local disk, or a share
//! mounted over NFS, SMB, sshfs or rclone.
//!
//! A file is written the way immure writes an entry in a batch: into a
//! temporary file under the store's `tmp/`, and with the others of its
//! batch flushed, made read-only, renamed into place, the directories
//! flushed after the renames. The device writes out its cache once per
//! batch, not once per file. The temporary file lies in the store of its
//! destination, so the rename stays within one filesystem, and its name is
//! one that `Store::prune_temp_files` recognises after an interrupted run.
//! A file in the root (`FORMAT`) has its temporary file beside it.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use anyhow::{Context as _, Result, bail};

use crate::remote::{Created, Remote};

/// Serial numbers for temporary names, unique within this process.
static SERIAL: AtomicU64 = AtomicU64::new(0);

/// How many files a directory puts in place together.
const BATCH: usize = immure::DEFAULT_BATCH.get();

/// A directory that holds a copy of an archive, or will.
#[derive(Debug)]
pub struct Directory {
    root: PathBuf,
    /// Files written and not in place yet, each with where it goes.
    pending: Mutex<Vec<(Temp, PathBuf)>>,
}

impl Directory {
    /// The directory at `root` as the place to copy to.
    ///
    /// The directory must exist. A first sync into a path that does not
    /// exist would otherwise write the whole archive onto the mount point
    /// of a share that is not mounted.
    ///
    /// # Errors
    ///
    /// When `root` is not a directory, or holds files but no `FORMAT`:
    /// a copy written into it would be mixed with what is there.
    pub fn open(root: &Path) -> Result<Directory> {
        match fs::metadata(root) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => bail!("{}: not a directory", root.display()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                bail!(
                    "{}: no such directory; create it for the first sync",
                    root.display()
                )
            }
            Err(error) => {
                return Err(error).with_context(|| format!("{}: reading", root.display()));
            }
        }
        if !root.join("FORMAT").exists() {
            let mut listing =
                fs::read_dir(root).with_context(|| format!("{}: reading", root.display()))?;
            if listing.next().is_some() {
                bail!(
                    "{}: contains files but is not an ossuary archive; name an empty directory or a copy made by ossuary sync",
                    root.display()
                );
            }
        }
        Ok(Directory {
            root: root.to_path_buf(),
            pending: Mutex::default(),
        })
    }

    /// The files not in place yet, whatever a panicking thread left there.
    fn pending(&self) -> MutexGuard<'_, Vec<(Temp, PathBuf)>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Where `path` lies under the root.
    fn full(&self, path: &str) -> PathBuf {
        let mut full = self.root.clone();
        full.extend(path.split('/'));
        full
    }

    /// The directory the temporary file for `path` goes into: the
    /// store's `tmp/` for a path inside a store, the root otherwise.
    fn temp_dir(&self, path: &str) -> PathBuf {
        match path.split_once('/') {
            Some((store, _)) => self.root.join(store).join("tmp"),
            None => self.root.clone(),
        }
    }
}

impl Remote for Directory {
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
        let full = self.full(path);
        match fs::read(&full) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("{}: reading", full.display())),
        }
    }

    fn create(&self, path: &str, from: &mut dyn Read, _size: u64) -> Result<Created> {
        let dest = self.full(path);
        match fs::symlink_metadata(&dest) {
            Ok(_) => return Ok(Created::Known),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("{}: reading", dest.display()));
            }
        }
        if self.pending().iter().any(|(_, held)| held == &dest) {
            return Ok(Created::Known);
        }
        let mut temp = Temp::create_in(&self.temp_dir(path))?;
        if let Some(file) = temp.file.as_mut() {
            io::copy(from, file).with_context(|| format!("{}: writing", temp.path.display()))?;
        }
        // Closed until the batch is flushed: a batch holds more files than
        // a process may keep open.
        temp.file = None;
        let full = {
            let mut pending = self.pending();
            pending.push((temp, dest));
            pending.len() >= BATCH
        };
        if full {
            self.flush()?;
        }
        Ok(Created::New)
    }

    fn flush(&self) -> Result<()> {
        let batch = std::mem::take(&mut *self.pending());
        if batch.is_empty() {
            return Ok(());
        }
        // The bytes are on the device before any of them has its name.
        let mut last = None;
        for (temp, _) in &batch {
            let file = File::open(&temp.path)
                .with_context(|| format!("{}: opening to flush", temp.path.display()))?;
            immure::send_to_device(&file)
                .with_context(|| format!("{}: flushing to device", temp.path.display()))?;
            last = Some(file);
        }
        if let Some(file) = &last {
            immure::flush_device(file).context("flushing the device")?;
        }
        let mut dirs = BTreeSet::new();
        for (temp, dest) in batch {
            dirs.insert(temp.place(&dest, true)?);
        }
        sync_dirs(&dirs);
        Ok(())
    }

    fn make_dir(&self, path: &str) -> Result<()> {
        let full = self.full(path);
        fs::create_dir_all(&full).with_context(|| format!("{}: creating", full.display()))
    }

    fn list(&self, path: &str) -> Result<Vec<String>> {
        let full = if path.is_empty() {
            self.root.clone()
        } else {
            self.full(path)
        };
        let listing =
            fs::read_dir(&full).with_context(|| format!("{}: listing", full.display()))?;
        let mut names = Vec::new();
        for entry in listing {
            let entry = entry.with_context(|| format!("{}: listing", full.display()))?;
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        Ok(names)
    }

    fn remove_tree(&self, path: &str) -> Result<()> {
        let full = self.full(path);
        let removed = match fs::symlink_metadata(&full) {
            Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&full),
            Ok(_) => fs::remove_file(&full),
            Err(error) => Err(error),
        };
        match removed {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| format!("{}: removing", full.display())),
        }
    }
}

/// A temporary file that removes itself unless it was renamed into
/// place.
#[derive(Debug)]
struct Temp {
    path: PathBuf,
    /// Open while it is written.
    file: Option<File>,
    persisted: bool,
}

impl Temp {
    /// A new temporary file in `dir`, made if it does not exist, named
    /// `<pid>-<serial>.tmp` as immure names its own.
    fn create_in(dir: &Path) -> Result<Temp> {
        fs::create_dir_all(dir).with_context(|| format!("{}: creating", dir.display()))?;
        let pid = std::process::id();
        loop {
            let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!("{pid}-{serial}.tmp"));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return Ok(Temp {
                        path,
                        file: Some(file),
                        persisted: false,
                    });
                }
                // Left by an earlier run of the same pid: the next name.
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("{}: creating", path.display()));
                }
            }
        }
    }

    /// Make the file read-only if asked, and rename it to `dest`, making
    /// the directories `dest` needs. Answers the directory it went into,
    /// for the caller to flush. The file is flushed by then.
    fn place(mut self, dest: &Path, read_only: bool) -> Result<PathBuf> {
        if read_only {
            // As immure does for an entry. A share that does not keep the
            // mode refuses it, and that costs the file nothing.
            if let Ok(metadata) = fs::metadata(&self.path) {
                let mut permissions = metadata.permissions();
                permissions.set_readonly(true);
                fs::set_permissions(&self.path, permissions).ok();
            }
        }
        let dir = dest
            .parent()
            .context("a path in the archive has a parent")?;
        fs::create_dir_all(dir).with_context(|| format!("{}: creating", dir.display()))?;
        fs::rename(&self.path, dest).with_context(|| format!("{}: writing", dest.display()))?;
        self.persisted = true;
        Ok(dir.to_path_buf())
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        if !self.persisted {
            // Best effort: a file that cannot be removed stays in `tmp/`,
            // where `Store::prune_temp_files` finds it.
            fs::remove_file(&self.path).ok();
        }
    }
}

/// Flush the directory entries after the renames of a batch: each handed
/// to the device, the device flushed once. Some filesystems refuse to
/// sync a directory; the files themselves are synced by then, and only
/// the last renames may be lost in a crash.
fn sync_dirs(dirs: &BTreeSet<PathBuf>) {
    let mut last = None;
    for dir in dirs {
        if let Ok(handle) = File::open(dir)
            && immure::send_to_device(&handle).is_ok()
        {
            last = Some(handle);
        }
    }
    if let Some(handle) = last {
        immure::flush_device(&handle).ok();
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn target(dir: &TempDir) -> Directory {
        Directory::open(dir.path()).unwrap()
    }

    #[test]
    fn a_missing_directory_is_refused() {
        let dir = TempDir::new().unwrap();

        let error = Directory::open(&dir.path().join("absent")).unwrap_err();

        assert!(error.to_string().contains("no such directory"));
    }

    #[test]
    fn a_directory_with_other_files_is_refused() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("notes.txt"), "mine").unwrap();

        let error = Directory::open(dir.path()).unwrap_err();

        assert!(error.to_string().contains("not an ossuary archive"));
    }

    #[test]
    fn a_created_file_lands_read_only_and_leaves_no_temporary_file() {
        let dir = TempDir::new().unwrap();
        let target = target(&dir);

        let created = target
            .create("content/ab/cd/abcd", &mut &b"bytes"[..], 5)
            .unwrap();

        assert_eq!(created, Created::New);
        let path = dir.path().join("content/ab/cd/abcd");
        assert!(!path.exists(), "in place with the batch");
        target.flush().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"bytes");
        assert!(fs::metadata(&path).unwrap().permissions().readonly());
        let leftovers: Vec<_> = fs::read_dir(dir.path().join("content/tmp"))
            .unwrap()
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn a_file_that_exists_is_not_written_again() {
        let dir = TempDir::new().unwrap();
        let target = target(&dir);
        target.create("FORMAT", &mut &b"first"[..], 5).unwrap();
        assert_eq!(
            target.create("FORMAT", &mut &b"again"[..], 5).unwrap(),
            Created::Known,
            "held in the batch"
        );
        target.flush().unwrap();

        let created = target.create("FORMAT", &mut &b"second"[..], 6).unwrap();

        assert_eq!(created, Created::Known);
        assert_eq!(target.read("FORMAT").unwrap().unwrap(), b"first");
    }

    #[test]
    fn reading_a_file_that_is_not_there_answers_none() {
        let dir = TempDir::new().unwrap();

        assert!(target(&dir).read("FORMAT").unwrap().is_none());
    }
}
