//! The mounted view's answers, blind to the door they leave through:
//! id-based questions answered from the flattened tree, bytes answered
//! from the archive's stores.
//!
//! Everything here is a reader. The view was grown once, when the
//! mount began, and never changes; the doors — NFS on macOS, FUSE on
//! Linux — only spell these answers in their own protocol.

use std::collections::{HashMap, VecDeque};
use std::os::unix::fs::FileExt;
use std::sync::{Arc, Mutex};

use ossuary_core::Archive;

use crate::forest::{Entry, Forest, Kind};

/// How many loaded bytes to keep at hand for entries that cannot be
/// read in place — compressed ones — before the least recently loaded
/// are let go.
const LOADED_AT_MOST: usize = 512 * 1024 * 1024;

/// How many store files to keep open for reading in place before the
/// least recently opened are closed again. A closed one is simply
/// opened anew on its next read, so the number only bounds what the
/// process holds — well under the 1024 descriptors a login shell
/// commonly allows, with room for the doors' own.
const HELD_AT_MOST: usize = 256;

/// Why an answer could not be given. Each door says it in the words its
/// protocol has for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The id names nothing in this view.
    Stale,
    /// The name stands nowhere in this folder.
    NotFound,
    /// Children were asked of a file.
    NotFolder,
    /// Bytes were asked of a folder.
    IsFolder,
    /// The store would not give the bytes.
    Io,
}

/// What a filesystem asks about one entry, in nobody's protocol yet.
#[derive(Debug, Clone, Copy)]
pub struct Attr {
    pub id: u64,
    pub folder: bool,
    pub size: u64,
    /// Seconds since the epoch: the file's recorded change, or the
    /// view's own moment for a folder.
    pub seconds: u32,
}

/// Bytes loaded whole, for entries the store keeps compressed.
#[derive(Default)]
struct Loaded {
    bytes: HashMap<String, Arc<Vec<u8>>>,
    order: VecDeque<String>,
    total: usize,
}

/// Store files held open for reading in place — `None` marks an entry
/// the store keeps compressed, where reading in place would misread
/// the bytes. Bounded: the least recently opened is closed when one
/// too many stands open.
#[derive(Default)]
struct Held {
    files: HashMap<u64, Option<Arc<std::fs::File>>>,
    order: VecDeque<u64>,
}

pub struct Record {
    forest: Forest,
    archive: Archive,
    uid: u32,
    gid: u32,
    /// The moment the view answers for: folders wear it as their time.
    view_time: u32,
    handles: Mutex<Held>,
    loaded: Mutex<Loaded>,
}

impl Record {
    pub fn new(forest: Forest, archive: Archive, uid: u32, gid: u32, view_time: u32) -> Self {
        Record {
            forest,
            archive,
            uid,
            gid,
            view_time,
            handles: Mutex::new(Held::default()),
            loaded: Mutex::new(Loaded::default()),
        }
    }

    /// The id of the entry at a forest index: indices shifted by one,
    /// so the root — entry 0 — wears the number every filesystem
    /// expects of it.
    pub const fn id(index: usize) -> u64 {
        index as u64 + 1
    }

    pub fn uid(&self) -> u32 {
        self.uid
    }

    pub fn gid(&self) -> u32 {
        self.gid
    }

    /// How many entries the view holds, root included.
    pub fn entries(&self) -> usize {
        self.forest.entries.len()
    }

    pub fn entry(&self, id: u64) -> Result<&Entry, Fault> {
        usize::try_from(id)
            .ok()
            .and_then(|id| id.checked_sub(1))
            .and_then(|index| self.forest.entries.get(index))
            .ok_or(Fault::Stale)
    }

    pub fn attr(&self, id: u64) -> Result<Attr, Fault> {
        let entry = self.entry(id)?;
        Ok(match &entry.kind {
            Kind::Folder { .. } => Attr {
                id,
                folder: true,
                size: 0,
                seconds: self.view_time,
            },
            Kind::File { size, modified, .. } => Attr {
                id,
                folder: false,
                size: *size,
                seconds: *modified,
            },
        })
    }

    /// The children of a folder, in name order, each with its forest
    /// index.
    pub fn children(&self, dir: u64) -> Result<&[(String, usize)], Fault> {
        match &self.entry(dir)?.kind {
            Kind::Folder { children } => Ok(children),
            Kind::File { .. } => Err(Fault::NotFolder),
        }
    }

    /// The id of the entry called `name` in a folder — `.` and `..`
    /// answered too.
    pub fn lookup(&self, dir: u64, name: &str) -> Result<u64, Fault> {
        let entry = self.entry(dir)?;
        let Kind::Folder { children } = &entry.kind else {
            return Err(Fault::NotFolder);
        };
        match name {
            "." => Ok(dir),
            ".." => Ok(Self::id(entry.parent)),
            _ => children
                .binary_search_by(|(child, _)| child.as_str().cmp(name))
                .map(|found| Self::id(children[found].1))
                .map_err(|_| Fault::NotFound),
        }
    }

    /// The bytes of one file, `offset` and `count` honoured, and
    /// whether that reached the end.
    pub fn read(&self, id: u64, offset: u64, count: u32) -> Result<(Vec<u8>, bool), Fault> {
        let entry = self.entry(id)?;
        let Kind::File { digest, size, .. } = &entry.kind else {
            return Err(Fault::IsFolder);
        };
        let size = *size;
        if offset >= size {
            return Ok((Vec::new(), true));
        }
        let wanted = usize::try_from(u64::from(count).min(size - offset)).map_err(|_| Fault::Io)?;

        // Read in place when the store holds the bytes as they are —
        // the usual case, and the one that keeps a big file cheap.
        if let Some(data) = self.read_in_place(id, digest, offset, wanted)? {
            let done = offset + data.len() as u64 >= size;
            return Ok((data, done));
        }

        // A compressed entry is loaded whole and kept at hand a while.
        let bytes = self.loaded(digest)?;
        let from = usize::try_from(offset).map_err(|_| Fault::Io)?;
        let until = bytes.len().min(from.saturating_add(wanted));
        let data = bytes.get(from..until).unwrap_or_default().to_vec();
        let done = until >= bytes.len();
        Ok((data, done))
    }

    /// Read a plain entry in place, opening it when it is not held —
    /// `None` when the store keeps these bytes compressed.
    fn read_in_place(
        &self,
        id: u64,
        digest: &str,
        offset: u64,
        wanted: usize,
    ) -> Result<Option<Vec<u8>>, Fault> {
        // The file is cloned out from under the lock: the bytes are
        // read without holding up every other reader, and a file
        // closed meanwhile stays open for this read alone.
        let Some(file) = self.held(id, digest)? else {
            return Ok(None);
        };
        let mut data = vec![0; wanted];
        let mut filled = 0;
        while filled < wanted {
            let got = file
                .read_at(&mut data[filled..], offset + filled as u64)
                .map_err(|_| Fault::Io)?;
            if got == 0 {
                break;
            }
            filled += got;
        }
        data.truncate(filled);
        Ok(Some(data))
    }

    /// The entry's file as held open, opened now when it is not — and
    /// the least recently opened closed to make room for it.
    fn held(&self, id: u64, digest: &str) -> Result<Option<Arc<std::fs::File>>, Fault> {
        let mut held = self.handles.lock().expect("no poisoned lock");
        if let Some(file) = held.files.get(&id) {
            return Ok(file.clone());
        }
        let file = self.openable(digest)?.map(Arc::new);
        held.files.insert(id, file.clone());
        held.order.push_back(id);
        while held.files.len() > HELD_AT_MOST {
            let Some(oldest) = held.order.pop_front() else {
                break;
            };
            held.files.remove(&oldest);
        }
        Ok(file)
    }

    /// The entry's file, when its bytes lie in the store as they are.
    fn openable(&self, digest: &str) -> Result<Option<std::fs::File>, Fault> {
        let parsed = ossuary_core::Digest::parse(digest).map_err(|_| Fault::Io)?;
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
            return std::fs::File::open(&path).map(Some).map_err(|_| Fault::Io);
        }
        Err(Fault::Io)
    }

    fn loaded(&self, digest: &str) -> Result<Arc<Vec<u8>>, Fault> {
        if let Some(bytes) = self
            .loaded
            .lock()
            .expect("no poisoned lock")
            .bytes
            .get(digest)
        {
            return Ok(bytes.clone());
        }
        let parsed = ossuary_core::Digest::parse(digest).map_err(|_| Fault::Io)?;
        let bytes = match self.archive.content().read(&parsed) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => match self.archive.derived().read(&parsed) {
                Ok(Some(bytes)) => bytes,
                _ => return Err(Fault::Io),
            },
            Err(_) => return Err(Fault::Io),
        };
        let bytes = Arc::new(bytes);
        let mut loaded = self.loaded.lock().expect("no poisoned lock");
        // Another reader may have loaded the same bytes meanwhile: theirs
        // stand, and this load is let go rather than counted twice.
        if let Some(theirs) = loaded.bytes.get(digest) {
            return Ok(theirs.clone());
        }
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
