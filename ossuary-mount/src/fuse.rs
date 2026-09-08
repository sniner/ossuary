//! The Linux door: the view as a FUSE filesystem, mounted through
//! `fusermount3` — no root, nothing beyond what every distribution
//! ships in its fuse3 package.
//!
//! The answers come from [`Record`]; this module only spells them in
//! FUSE and holds the mount for the run. The mount is declared
//! read-only, so the kernel itself answers every write with read-only
//! filesystem before it could reach here. And since the view never
//! changes for the life of the mount, whatever the kernel is told it
//! may keep.

use std::ffi::OsStr;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow};
use fuser::{
    Config, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo,
    MountOption, OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, ReplyOpen,
    ReplyStatfs, Request, Session,
};

use crate::Room;
use crate::record::{Attr, Fault, Record};

// The kernel's root inode is the forest's entry 0, as every door
// agrees.
const _: () = assert!(Record::id(0) == INodeNo::ROOT.0);

/// How long the kernel may keep an answer. The view never changes for
/// the life of the mount, so any length is honest; an hour keeps the
/// kernel's arithmetic comfortable.
const KEPT: Duration = Duration::from_secs(60 * 60);

/// How long to wait, after letting go, for a reader still holding a
/// file — then the door closes on it.
const GRACE: Duration = Duration::from_secs(2);

struct Door {
    record: Record,
}

/// The fault in the kernel's words.
fn spelled(fault: Fault) -> Errno {
    match fault {
        Fault::Stale | Fault::NotFound => Errno::ENOENT,
        Fault::NotFolder => Errno::ENOTDIR,
        Fault::IsFolder => Errno::EISDIR,
        Fault::Io => Errno::EIO,
    }
}

impl Door {
    fn attributes(&self, attr: Attr) -> FileAttr {
        let (kind, perm) = if attr.folder {
            (FileType::Directory, 0o555)
        } else {
            (FileType::RegularFile, 0o444)
        };
        let time = UNIX_EPOCH + Duration::from_secs(u64::from(attr.seconds));
        FileAttr {
            ino: INodeNo(attr.id),
            size: attr.size,
            blocks: attr.size.div_ceil(512),
            atime: time,
            mtime: time,
            ctime: time,
            crtime: time,
            kind,
            perm,
            nlink: 1,
            uid: self.record.uid(),
            gid: self.record.gid(),
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }
}

impl Filesystem for Door {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let Some(name) = name.to_str() else {
            reply.error(Errno::ENOENT);
            return;
        };
        match self
            .record
            .lookup(parent.0, name)
            .and_then(|id| self.record.attr(id))
        {
            Ok(attr) => reply.entry(&KEPT, &self.attributes(attr), Generation(0)),
            Err(fault) => reply.error(spelled(fault)),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.record.attr(ino.0) {
            Ok(attr) => reply.attr(&KEPT, &self.attributes(attr)),
            Err(fault) => reply.error(spelled(fault)),
        }
    }

    fn open(&self, _req: &Request, _ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        // The bytes behind a name never change while mounted: what the
        // kernel read once it may keep across opens.
        reply.opened(FileHandle(0), FopenFlags::FOPEN_KEEP_CACHE);
    }

    #[allow(clippy::too_many_arguments, reason = "the trait's own signature")]
    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyData,
    ) {
        match self.record.read(ino.0, offset, size) {
            Ok((bytes, _)) => reply.data(&bytes),
            Err(fault) => reply.error(spelled(fault)),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let (children, parent) = match (self.record.children(ino.0), self.record.entry(ino.0)) {
            (Ok(children), Ok(entry)) => (children, Record::id(entry.parent)),
            (Err(fault), _) | (_, Err(fault)) => {
                reply.error(spelled(fault));
                return;
            }
        };
        // The listing the kernel pages through: the two dots first,
        // then the children in name order. Each entry carries the
        // offset the next page starts at, so a page ends where the
        // last one left off.
        let dots = [(".", ino.0, true), ("..", parent, true)];
        let named = children.iter().map(|(name, index)| {
            let id = Record::id(*index);
            let folder = self.record.attr(id).is_ok_and(|attr| attr.folder);
            (name.as_str(), id, folder)
        });
        let listing = dots.iter().copied().chain(named);
        let skipped = usize::try_from(offset).unwrap_or(usize::MAX);
        for (position, (name, id, folder)) in listing.enumerate().skip(skipped) {
            let kind = if folder {
                FileType::Directory
            } else {
                FileType::RegularFile
            };
            if reply.add(INodeNo(id), position as u64 + 1, kind, name) {
                break;
            }
        }
        reply.ok();
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        reply.statfs(0, 0, 0, self.record.entries() as u64, 0, 4096, 255, 4096);
    }
}

/// Mount, wait, give back.
pub fn serve(record: Record, mountpoint: &Path, room: &Room<'_>) -> Result<ExitCode> {
    let place = mountpoint.display().to_string();
    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::RO,
        MountOption::NoSuid,
        MountOption::NoDev,
        MountOption::NoAtime,
        MountOption::FSName("ossuary".to_string()),
        MountOption::Subtype("ossuary".to_string()),
    ];
    let mut session = Session::new(Door { record }, mountpoint, &config).with_context(|| {
        format!(
            "{place}: mount refused — the mountpoint must be a directory of your own with nothing mounted there, and the fuse3 package (fusermount3) must be installed"
        )
    })?;
    let mut unmounter = session.unmount_callable();

    // The requests are answered on their own thread; the run waits
    // here for a signal, or for the door to close from the other side.
    let (ended, mut ending) = tokio::sync::oneshot::channel();
    let worker = std::thread::Builder::new()
        .name("ossuary-mount".to_string())
        .spawn(move || {
            let outcome = session.run();
            // Nobody waiting means the room was given back from this
            // side, and the outcome was decided here already.
            let _ = ended.send(outcome);
        })
        .context("starting the filesystem thread")?;

    room.opened();

    let runtime = tokio::runtime::Runtime::new().context("starting the runtime")?;
    let let_go = runtime.block_on(async {
        let mut terminated =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .context("listening for signals")?;
        Ok::<_, anyhow::Error>(tokio::select! {
            _ = tokio::signal::ctrl_c() => None,
            _ = terminated.recv() => None,
            outcome = &mut ending => Some(outcome),
        })
    })?;

    match let_go {
        // Someone unmounted it themselves; the room is already given
        // back, and the thread has said how it went.
        Some(Ok(Ok(()))) => {
            room.given_back();
            Ok(ExitCode::SUCCESS)
        }
        Some(Ok(Err(error))) => Err(anyhow!(
            "the FUSE door closed on its own: {error} — unmount with `fusermount3 -u {place}`, then mount anew"
        )),
        Some(Err(_)) => Err(anyhow!(
            "the filesystem thread ended without a word — unmount with `fusermount3 -u {place}`, then mount anew"
        )),
        // A signal: give the room back ourselves.
        None => {
            unmounter.unmount().with_context(|| {
                format!("{place} is still mounted — close what reads it and run `fusermount3 -u {place}`")
            })?;
            // Letting go detaches the room at once; a reader still
            // holding a file keeps the thread a moment longer. Wait
            // a little for it, then close the door on it.
            let asked = Instant::now();
            while !worker.is_finished() && asked.elapsed() < GRACE {
                std::thread::sleep(Duration::from_millis(50));
            }
            if worker.is_finished() {
                room.given_back();
            } else {
                room.tell(format_args!(
                    "{place} given back — a reader still held a file, and sees it go"
                ));
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}
