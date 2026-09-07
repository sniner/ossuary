//! The NFS face of the mounted view: id-based questions answered from
//! the flattened tree, bytes answered from the archive's stores.
//!
//! Everything here is a reader. The view was grown once, when the
//! mount began, and never changes; every writing call answers
//! read-only filesystem, which is the operating system's own way of
//! saying what the narration already said.

use std::collections::{HashMap, VecDeque};
use std::os::unix::fs::FileExt;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nfsserve::nfs::{
    fattr3, fileid3, filename3, ftype3, nfspath3, nfsstat3, nfstime3, sattr3, specdata3,
};
use nfsserve::vfs::{DirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities};
use ossuary_core::Archive;

use crate::forest::{Entry, Forest, Kind};

/// How many loaded bytes to keep at hand for entries that cannot be
/// read in place — compressed ones — before the least recently loaded
/// are let go.
const LOADED_AT_MOST: usize = 512 * 1024 * 1024;

/// Bytes loaded whole, for entries the store keeps compressed.
#[derive(Default)]
struct Loaded {
    bytes: HashMap<String, Arc<Vec<u8>>>,
    order: VecDeque<String>,
    total: usize,
}

pub struct RecordFs {
    forest: Forest,
    archive: Archive,
    uid: u32,
    gid: u32,
    /// The moment the view answers for: folders wear it as their time.
    view_time: u32,
    /// Plain store entries opened once, read in place ever after —
    /// `None` marks one the store keeps compressed, where reading in
    /// place would misread the bytes.
    handles: Mutex<HashMap<fileid3, Option<std::fs::File>>>,
    loaded: Mutex<Loaded>,
}

impl RecordFs {
    pub fn new(forest: Forest, archive: Archive, uid: u32, gid: u32, view_time: u32) -> Self {
        RecordFs {
            forest,
            archive,
            uid,
            gid,
            view_time,
            handles: Mutex::new(HashMap::new()),
            loaded: Mutex::new(Loaded::default()),
        }
    }

    fn entry(&self, id: fileid3) -> Result<&Entry, nfsstat3> {
        usize::try_from(id)
            .ok()
            .and_then(|id| id.checked_sub(1))
            .and_then(|index| self.forest.entries.get(index))
            .ok_or(nfsstat3::NFS3ERR_STALE)
    }

    fn attributes(&self, id: fileid3, entry: &Entry) -> fattr3 {
        let (ftype, mode, size, seconds) = match &entry.kind {
            Kind::Folder { .. } => (ftype3::NF3DIR, 0o555, 0, self.view_time),
            Kind::File { size, modified, .. } => (ftype3::NF3REG, 0o444, *size, *modified),
        };
        let time = nfstime3 {
            seconds,
            nseconds: 0,
        };
        fattr3 {
            ftype,
            mode,
            nlink: 1,
            uid: self.uid,
            gid: self.gid,
            size,
            used: size,
            rdev: specdata3::default(),
            fsid: 1,
            fileid: id,
            atime: time,
            mtime: time,
            ctime: time,
        }
    }

    /// The bytes of one file, `offset` and `count` honoured, and
    /// whether that reached the end.
    fn bytes_of(
        &self,
        id: fileid3,
        digest: &str,
        size: u64,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), nfsstat3> {
        if offset >= size {
            return Ok((Vec::new(), true));
        }
        let wanted = usize::try_from(u64::from(count).min(size - offset))
            .map_err(|_| nfsstat3::NFS3ERR_IO)?;

        // Read in place when the store holds the bytes as they are —
        // the usual case, and the one that keeps a big file cheap.
        if let Some(data) = self.read_in_place(id, digest, offset, wanted)? {
            let done = offset + data.len() as u64 >= size;
            return Ok((data, done));
        }

        // A compressed entry is loaded whole and kept at hand a while.
        let bytes = self.loaded(digest)?;
        let from = usize::try_from(offset).map_err(|_| nfsstat3::NFS3ERR_IO)?;
        let until = bytes.len().min(from.saturating_add(wanted));
        let data = bytes.get(from..until).unwrap_or_default().to_vec();
        let done = until >= bytes.len();
        Ok((data, done))
    }

    /// Read a plain entry in place, opening it on first use — `None`
    /// when the store keeps these bytes compressed.
    fn read_in_place(
        &self,
        id: fileid3,
        digest: &str,
        offset: u64,
        wanted: usize,
    ) -> Result<Option<Vec<u8>>, nfsstat3> {
        let mut handles = self.handles.lock().expect("no poisoned lock");
        let opened = match handles.entry(id) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(slot) => slot.insert(self.openable(digest)?),
        };
        let Some(file) = opened else {
            return Ok(None);
        };
        let mut data = vec![0; wanted];
        let mut filled = 0;
        while filled < wanted {
            let got = file
                .read_at(&mut data[filled..], offset + filled as u64)
                .map_err(|_| nfsstat3::NFS3ERR_IO)?;
            if got == 0 {
                break;
            }
            filled += got;
        }
        data.truncate(filled);
        Ok(Some(data))
    }

    /// The entry's file, when its bytes lie in the store as they are.
    fn openable(&self, digest: &str) -> Result<Option<std::fs::File>, nfsstat3> {
        let parsed = ossuary_core::Digest::parse(digest).map_err(|_| nfsstat3::NFS3ERR_IO)?;
        let stores = [self.archive.content(), self.archive.derived()];
        for store in stores {
            let Ok(Some(path)) = store.find(&parsed) else {
                continue;
            };
            let Some(entry) = store.entry_at(&path) else {
                continue;
            };
            if entry.is_compressed() || entry.is_encrypted() {
                return Ok(None);
            }
            return std::fs::File::open(&path)
                .map(Some)
                .map_err(|_| nfsstat3::NFS3ERR_IO);
        }
        Err(nfsstat3::NFS3ERR_IO)
    }

    fn loaded(&self, digest: &str) -> Result<Arc<Vec<u8>>, nfsstat3> {
        if let Some(bytes) = self
            .loaded
            .lock()
            .expect("no poisoned lock")
            .bytes
            .get(digest)
        {
            return Ok(bytes.clone());
        }
        let parsed = ossuary_core::Digest::parse(digest).map_err(|_| nfsstat3::NFS3ERR_IO)?;
        let bytes = match self.archive.content().read(&parsed) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => match self.archive.derived().read(&parsed) {
                Ok(Some(bytes)) => bytes,
                _ => return Err(nfsstat3::NFS3ERR_IO),
            },
            Err(_) => return Err(nfsstat3::NFS3ERR_IO),
        };
        let bytes = Arc::new(bytes);
        let mut loaded = self.loaded.lock().expect("no poisoned lock");
        if bytes.len() <= LOADED_AT_MOST {
            loaded.total += bytes.len();
            loaded.order.push_back(digest.to_string());
            loaded.bytes.insert(digest.to_string(), bytes.clone());
            while loaded.total > LOADED_AT_MOST {
                let Some(oldest) = loaded.order.pop_front() else {
                    break;
                };
                if let Some(gone) = loaded.bytes.remove(&oldest) {
                    loaded.total -= gone.len();
                }
            }
        }
        Ok(bytes)
    }
}

#[async_trait]
impl NFSFileSystem for RecordFs {
    fn capabilities(&self) -> VFSCapabilities {
        VFSCapabilities::ReadOnly
    }

    fn root_dir(&self) -> fileid3 {
        1
    }

    async fn lookup(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        let entry = self.entry(dirid)?;
        let Kind::Folder { children } = &entry.kind else {
            return Err(nfsstat3::NFS3ERR_NOTDIR);
        };
        let name = std::str::from_utf8(&filename.0).map_err(|_| nfsstat3::NFS3ERR_NOENT)?;
        match name {
            "." => Ok(dirid),
            ".." => Ok(entry.parent as fileid3 + 1),
            _ => children
                .binary_search_by(|(child, _)| child.as_str().cmp(name))
                .map(|found| children[found].1 as fileid3 + 1)
                .map_err(|_| nfsstat3::NFS3ERR_NOENT),
        }
    }

    async fn getattr(&self, id: fileid3) -> Result<fattr3, nfsstat3> {
        Ok(self.attributes(id, self.entry(id)?))
    }

    async fn setattr(&self, _id: fileid3, _setattr: sattr3) -> Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn read(
        &self,
        id: fileid3,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), nfsstat3> {
        let entry = self.entry(id)?;
        let Kind::File { digest, size, .. } = &entry.kind else {
            return Err(nfsstat3::NFS3ERR_ISDIR);
        };
        self.bytes_of(id, digest, *size, offset, count)
    }

    async fn write(&self, _id: fileid3, _offset: u64, _data: &[u8]) -> Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn create(
        &self,
        _dirid: fileid3,
        _filename: &filename3,
        _attr: sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn create_exclusive(
        &self,
        _dirid: fileid3,
        _filename: &filename3,
    ) -> Result<fileid3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn mkdir(
        &self,
        _dirid: fileid3,
        _dirname: &filename3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn remove(&self, _dirid: fileid3, _filename: &filename3) -> Result<(), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn rename(
        &self,
        _from_dirid: fileid3,
        _from_filename: &filename3,
        _to_dirid: fileid3,
        _to_filename: &filename3,
    ) -> Result<(), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn readdir(
        &self,
        dirid: fileid3,
        start_after: fileid3,
        max_entries: usize,
    ) -> Result<ReadDirResult, nfsstat3> {
        let entry = self.entry(dirid)?;
        let Kind::Folder { children } = &entry.kind else {
            return Err(nfsstat3::NFS3ERR_NOTDIR);
        };
        let mut result = ReadDirResult {
            entries: Vec::new(),
            end: true,
        };
        for (name, index) in children {
            let id = *index as fileid3 + 1;
            if id <= start_after {
                continue;
            }
            if result.entries.len() >= max_entries {
                result.end = false;
                break;
            }
            result.entries.push(DirEntry {
                fileid: id,
                name: filename3::from(name.as_bytes()),
                attr: self.attributes(id, &self.forest.entries[*index]),
            });
        }
        Ok(result)
    }

    async fn symlink(
        &self,
        _dirid: fileid3,
        _linkname: &filename3,
        _symlink: &nfspath3,
        _attr: &sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn readlink(&self, _id: fileid3) -> Result<nfspath3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_INVAL)
    }
}
