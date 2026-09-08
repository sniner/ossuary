//! The macOS door: an NFS server answering on 127.0.0.1, mounted by the
//! operating system's own client — nothing kernel-side is installed.
//!
//! The answers come from [`Record`]; this module only spells them in
//! NFS and holds the mount for the run. Every writing call answers
//! read-only filesystem, which is the operating system's own way of
//! saying what the narration already said.

use std::path::Path;
use std::process::{Command, ExitCode};

use anyhow::{Context as _, Result, anyhow};
use async_trait::async_trait;
use nfsserve::nfs::{
    fattr3, fileid3, filename3, ftype3, nfspath3, nfsstat3, nfstime3, sattr3, specdata3,
};
use nfsserve::tcp::{NFSTcp as _, NFSTcpListener};
use nfsserve::vfs::{DirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities};

use crate::Room;
use crate::record::{Attr, Fault, Record};

struct Door {
    record: Record,
}

/// The fault in NFS's words.
fn spelled(fault: Fault) -> nfsstat3 {
    match fault {
        Fault::Stale => nfsstat3::NFS3ERR_STALE,
        Fault::NotFound => nfsstat3::NFS3ERR_NOENT,
        Fault::NotFolder => nfsstat3::NFS3ERR_NOTDIR,
        Fault::IsFolder => nfsstat3::NFS3ERR_ISDIR,
        Fault::Io => nfsstat3::NFS3ERR_IO,
    }
}

impl Door {
    fn attributes(&self, attr: Attr) -> fattr3 {
        let (ftype, mode) = if attr.folder {
            (ftype3::NF3DIR, 0o555)
        } else {
            (ftype3::NF3REG, 0o444)
        };
        let time = nfstime3 {
            seconds: attr.seconds,
            nseconds: 0,
        };
        fattr3 {
            ftype,
            mode,
            nlink: 1,
            uid: self.record.uid(),
            gid: self.record.gid(),
            size: attr.size,
            used: attr.size,
            rdev: specdata3::default(),
            fsid: 1,
            fileid: attr.id,
            atime: time,
            mtime: time,
            ctime: time,
        }
    }
}

#[async_trait]
impl NFSFileSystem for Door {
    fn capabilities(&self) -> VFSCapabilities {
        VFSCapabilities::ReadOnly
    }

    fn root_dir(&self) -> fileid3 {
        Record::id(0)
    }

    async fn lookup(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        let name = std::str::from_utf8(&filename.0).map_err(|_| nfsstat3::NFS3ERR_NOENT)?;
        self.record.lookup(dirid, name).map_err(spelled)
    }

    async fn getattr(&self, id: fileid3) -> Result<fattr3, nfsstat3> {
        self.record
            .attr(id)
            .map(|attr| self.attributes(attr))
            .map_err(spelled)
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
        self.record.read(id, offset, count).map_err(spelled)
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
        let children = self.record.children(dirid).map_err(spelled)?;
        let mut result = ReadDirResult {
            entries: Vec::new(),
            end: true,
        };
        for (name, index) in children {
            let id = Record::id(*index);
            if id <= start_after {
                continue;
            }
            if result.entries.len() >= max_entries {
                result.end = false;
                break;
            }
            let attr = self.record.attr(id).map_err(spelled)?;
            result.entries.push(DirEntry {
                fileid: id,
                name: filename3::from(name.as_bytes()),
                attr: self.attributes(attr),
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

/// Serve, mount, wait, give back.
pub fn serve(record: Record, mountpoint: &Path, room: &Room<'_>) -> Result<ExitCode> {
    tokio::runtime::Runtime::new()
        .context("starting the runtime")?
        .block_on(held(Door { record }, mountpoint, room))
}

async fn held(door: Door, mountpoint: &Path, room: &Room<'_>) -> Result<ExitCode> {
    let mut listener = NFSTcpListener::bind("127.0.0.1:0", door)
        .await
        .context("opening the NFS door on 127.0.0.1")?;
    let port = listener.get_listen_port();
    let (mounted, mut mount_events) = tokio::sync::mpsc::channel(8);
    listener.set_mount_listener(mounted);
    let mut server = tokio::spawn(async move { listener.handle_forever().await });

    let place = mountpoint.display().to_string();
    let options = format!(
        "nolocks,vers=3,tcp,soft,timeo=10,retrans=3,rsize=131072,actimeo=120,rdonly,port={port},mountport={port}"
    );
    let outcome = Command::new("mount")
        .args(["-t", "nfs", "-o", &options, "127.0.0.1:/", &place])
        .status()
        .context("running mount")?;
    if !outcome.success() {
        server.abort();
        return Err(anyhow!(
            "{place}: mount refused — the mountpoint must be a directory of your own, and nothing may already be mounted there"
        ));
    }

    room.opened();

    let mut terminated = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("listening for signals")?;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = terminated.recv() => break,
            event = mount_events.recv() => {
                // `false` is the client letting go: someone ran umount
                // themselves, and the room is already given back.
                if event == Some(false) {
                    room.given_back();
                    server.abort();
                    return Ok(ExitCode::SUCCESS);
                }
            }
            _ = &mut server => {
                return Err(anyhow!(
                    "the NFS door closed on its own — unmount with `umount {place}`, then mount anew"
                ));
            }
        }
    }

    let given_back = give_back(&place, room);
    server.abort();
    given_back
}

/// Unmount, forcing politely when something still reads.
fn give_back(place: &str, room: &Room<'_>) -> Result<ExitCode> {
    if Command::new("umount")
        .arg(place)
        .status()
        .context("running umount")?
        .success()
    {
        room.given_back();
        return Ok(ExitCode::SUCCESS);
    }
    room.tell(format_args!(
        "{place} still in use — asking diskutil to force it"
    ));
    if Command::new("diskutil")
        .args(["unmount", "force", place])
        .status()
        .context("running diskutil")?
        .success()
    {
        room.given_back();
        return Ok(ExitCode::SUCCESS);
    }
    Err(anyhow!(
        "{place} is still mounted — close what reads it and run `umount {place}`"
    ))
}
