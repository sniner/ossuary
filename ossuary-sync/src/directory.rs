//! A directory as the place a copy lies: a local disk, or a share
//! mounted over NFS, SMB, sshfs or rclone.
//!
//! A file is written the way immure writes an entry: into a temporary
//! file under the store's `tmp/`, synced, made read-only, renamed into
//! place, and the directory synced after the rename. The temporary file
//! lies in the store of its destination, so the rename stays within one
//! filesystem, and its name is one that `Store::prune_temp_files`
//! recognises after an interrupted run. A file in the root (`FORMAT`,
//! `head.jsonl`) has its temporary file beside it, as the log does for
//! its head.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, Result, bail};

use crate::remote::{Created, Remote};

/// Serial numbers for temporary names, unique within this process.
static SERIAL: AtomicU64 = AtomicU64::new(0);

/// A directory that holds a copy of an archive, or will.
#[derive(Debug)]
pub struct Directory {
    root: PathBuf,
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
                    "{}: holds files but is not an ossuary archive; name an empty directory or a copy that ossuary sync made",
                    root.display()
                );
            }
        }
        Ok(Directory {
            root: root.to_path_buf(),
        })
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
        let mut temp = Temp::create_in(&self.temp_dir(path))?;
        io::copy(from, &mut temp.file)
            .with_context(|| format!("{}: writing", temp.path.display()))?;
        temp.persist(&dest, true)?;
        Ok(Created::New)
    }

    fn replace(&self, path: &str, bytes: &[u8]) -> Result<()> {
        let dest = self.full(path);
        let dir = dest.parent().unwrap_or(&self.root);
        let mut temp = Temp::create_in(dir)?;
        temp.file
            .write_all(bytes)
            .with_context(|| format!("{}: writing", temp.path.display()))?;
        temp.persist(&dest, false)
    }

    fn make_dir(&self, path: &str) -> Result<()> {
        let full = self.full(path);
        fs::create_dir_all(&full).with_context(|| format!("{}: creating", full.display()))
    }
}

/// A temporary file that removes itself unless it was renamed into
/// place.
struct Temp {
    path: PathBuf,
    file: File,
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
                        file,
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

    /// Sync the file, make it read-only if asked, and rename it to
    /// `dest`, making the directories `dest` needs.
    fn persist(mut self, dest: &Path, read_only: bool) -> Result<()> {
        self.file
            .sync_all()
            .with_context(|| format!("{}: flushing to device", self.path.display()))?;
        if read_only {
            // As immure does for an entry. A share that does not keep the
            // mode refuses it, and that costs the file nothing.
            if let Ok(metadata) = self.file.metadata() {
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
        sync_dir(dir);
        Ok(())
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

/// Flush a directory entry after a rename. Some filesystems refuse to
/// sync a directory; the file itself is synced by then, and only the
/// last rename may be lost in a crash.
fn sync_dir(dir: &Path) {
    if let Ok(handle) = File::open(dir) {
        handle.sync_all().ok();
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

        let created = target.create("FORMAT", &mut &b"second"[..], 6).unwrap();

        assert_eq!(created, Created::Known);
        assert_eq!(target.read("FORMAT").unwrap().unwrap(), b"first");
    }

    #[test]
    fn replacing_writes_over_the_old_file() {
        let dir = TempDir::new().unwrap();
        let target = target(&dir);
        target.replace("head.jsonl", b"old\n").unwrap();

        target.replace("head.jsonl", b"new\n").unwrap();

        assert_eq!(target.read("head.jsonl").unwrap().unwrap(), b"new\n");
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|item| item.unwrap().file_name())
            .collect();
        assert_eq!(names, ["head.jsonl"]);
    }

    #[test]
    fn reading_a_file_that_is_not_there_answers_none() {
        let dir = TempDir::new().unwrap();

        assert!(target(&dir).read("head.jsonl").unwrap().is_none());
    }
}
