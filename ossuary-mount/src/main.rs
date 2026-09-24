//! ossuary-mount: the record as a read-only filesystem.
//!
//! A reading room for every place the record knows: the same forest
//! `ossuary ls` and `ossuary tree` answer with, grafted onto a
//! directory so any program can read the archive's files where they
//! once stood. The room is read-only and answers for one moment — the
//! mount's own, or the one `--as-of` names — and Ctrl-C gives the
//! directory back.
//!
//! The view is grown here and answered by [`record`]; the door it is
//! served through is the platform's own. On macOS an NFS server
//! answers on 127.0.0.1 and the operating system's own client mounts
//! it ([`nfs`]); on Linux the view is a FUSE filesystem mounted through
//! `fusermount3` ([`fuse`]). Neither installs anything kernel-side or
//! asks for root. Where the record says more than a filesystem can —
//! several files at one name, a name that is file and folder at once —
//! the view narrows by declared policy; the narrowing lives in
//! [`forest`].

use std::collections::BTreeMap;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result, anyhow};
use clap::Parser;
use ossuary_core::{Archive, Error, Index, Standing, Timestamp, Value};

mod forest;
mod record;

#[cfg(target_os = "linux")]
mod fuse;
#[cfg(target_os = "macos")]
mod nfs;

#[cfg(target_os = "linux")]
use fuse as door;
#[cfg(target_os = "macos")]
use nfs as door;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("ossuary-mount supports only macOS (NFS) and Linux (FUSE)");

use forest::{Forest, Kind, Sighting};
use record::Record;

#[derive(Parser)]
#[command(
    name = "ossuary-mount",
    version,
    about = "Mount an ossuary archive as a read-only filesystem, each file at its recorded path"
)]
struct Cli {
    /// The archive to work in
    #[arg(long, value_name = "DIR", env = "OSSUARY_ARCHIVE", default_value = ".")]
    archive: PathBuf,

    /// The directory to mount on; created if missing, and then removed
    /// again on unmount
    #[arg(value_name = "DIR")]
    mountpoint: PathBuf,

    /// Show the archive as it was at TIME (UTC): 2026-01-01 (end of that
    /// day), 2026-01-01T08:00:00 (trailing Z optional), or a run id (after
    /// that run's last claim)
    #[arg(long, value_name = "TIME")]
    as_of: Option<String>,

    /// Print only errors
    #[arg(short, long)]
    quiet: bool,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("ossuary-mount: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let Cli {
        archive,
        mountpoint,
        as_of,
        quiet,
    } = cli;
    let archive = open(&archive)?;
    let index = caught_up(&archive, quiet)?;
    let (index, moment, stood) = match as_of.as_deref() {
        None => (index, Timestamp::now(), None),
        Some(given) => {
            let (view, closed) = at(&index, given)?;
            let stood = if ossuary_core::Run::spelled(given) {
                format!(", as of run {given} ({})", closed.as_str())
            } else {
                format!(", as of {}", closed.as_str())
            };
            (view, closed, Some(stood))
        }
    };
    let view = grown_view(&index)?;
    let view_time = forest::clamped(forest::epoch(moment.as_str())).unwrap_or(0);

    let made = !mountpoint.exists();
    std::fs::create_dir_all(&mountpoint)
        .with_context(|| format!("{}: creating the mountpoint", mountpoint.display()))?;
    let owner = std::fs::metadata(&mountpoint)
        .with_context(|| format!("{}: reading the mountpoint", mountpoint.display()))?;
    let (uid, gid) = {
        use std::os::unix::fs::MetadataExt as _;
        (owner.uid(), owner.gid())
    };

    let (files, folders) = counted(&view);
    let room = Room {
        place: mountpoint.display().to_string(),
        stood: stood.as_deref(),
        files,
        folders,
        quiet,
    };
    let record = Record::new(view, archive, uid, gid, view_time);
    let served = door::serve(record, &mountpoint, &room);

    // A directory made for the mount is taken back with it; one that
    // stood before stays. Removal only works on an empty, unmounted
    // directory, so a room still occupied simply remains — a leftover,
    // never a failure.
    if made {
        let _ = std::fs::remove_dir(&mountpoint);
    }
    served
}

/// What the run says about the room it holds — the same words at
/// every door.
pub struct Room<'a> {
    place: String,
    /// The moment the view answers for, as a clause of the opening
    /// line; `None` for the mount's own.
    stood: Option<&'a str>,
    files: usize,
    folders: usize,
    quiet: bool,
}

impl Room<'_> {
    /// A line of narration, kept back under `--quiet`.
    pub fn tell(&self, line: impl Display) {
        if !self.quiet {
            eprintln!("{line}");
        }
    }

    /// The room is open: what stands in it, and how to leave.
    pub fn opened(&self) {
        let Room {
            place,
            stood,
            files,
            folders,
            ..
        } = self;
        let stood = stood.unwrap_or("");
        self.tell(format_args!(
            "mounted read-only at {place}: {files} file(s) in {folders} folder(s){stood}; press Ctrl-C to unmount"
        ));
        if *files == 0 {
            self.tell("no files to show; add files with `ossuary ingest`");
        }
    }

    /// The room is given back.
    pub fn given_back(&self) {
        self.tell(format_args!("{} unmounted", self.place));
    }
}

/// The archive, or the way to one — `ossuary`'s own wording.
fn open(root: &Path) -> Result<Archive> {
    Archive::open(root).map_err(|error| match error {
        Error::NoArchive(path) => anyhow!(
            "{}: not an ossuary archive; run in an archive, name one with --archive, or create one with `ossuary init`",
            path.display()
        ),
        other => other.into(),
    })
}

/// The archive's index, caught up with its log.
fn caught_up(archive: &Archive, quiet: bool) -> Result<Index> {
    let mut index = archive.index()?;
    let folded = index.fold(archive.log())?;
    if folded.segments > 0 && !quiet {
        eprintln!("index updated: {} new log segment(s)", folded.segments);
    }
    Ok(index)
}

/// The record as it stood when `--as-of` names, and the moment that
/// is — [`Index::at`], with `ossuary`'s own words for what it refuses.
fn at(index: &Index, given: &str) -> Result<(Index, Timestamp)> {
    match index.at(given) {
        Ok(Some(view)) => Ok(view),
        Ok(None) => Err(anyhow!("no run {given}; `ossuary history` lists the runs")),
        Err(Error::Timestamp(_)) => Err(anyhow!(
            "{given:?} is not a valid time; use RFC 3339 (2026-01-01T12:00:00Z), a date (2026-01-01) or a run id"
        )),
        Err(error) => Err(error.into()),
    }
}

/// The view, grown from the record `index` holds.
fn grown_view(index: &Index) -> Result<Forest> {
    let mut places = Vec::new();
    for standing in index.standing_as_of("file:path", None)? {
        let Value::String(path) = standing.value else {
            continue;
        };
        // A place inside another content is no place in the tree: the
        // entry's archive is what stands here. Showing an archive as a
        // folder of its entries would be a view of its own.
        if path.starts_with('@') {
            continue;
        }
        places.push(Sighting {
            path,
            digest: standing.subject.as_str().to_string(),
            asserted: standing.asserted,
            order: standing.order,
        });
    }

    let mut sizes = BTreeMap::new();
    for (subject, value) in newest(index.standing_as_of("file:size", None)?) {
        if let Some(size) = value.as_u64() {
            sizes.insert(subject, size);
        }
    }
    let mut modified = BTreeMap::new();
    for (subject, value) in newest(index.standing_as_of("file:modified", None)?) {
        if let Value::String(moment) = value {
            if let Some(seconds) = forest::clamped(forest::epoch(&moment)) {
                modified.insert(subject, seconds);
            }
        }
    }

    Ok(forest::grown(&places, &sizes, &modified))
}

/// The newest standing value per subject — the mounted view's
/// newest-wins, decided by assertion moment, log order breaking ties.
fn newest(standing: Vec<Standing>) -> BTreeMap<String, Value> {
    let mut best: BTreeMap<String, (String, u64, Value)> = BTreeMap::new();
    for one in standing {
        let key = one.subject.as_str().to_string();
        match best.get(&key) {
            Some((asserted, order, _))
                if (asserted.as_str(), *order) >= (one.asserted.as_str(), one.order) => {}
            _ => {
                best.insert(key, (one.asserted, one.order, one.value));
            }
        }
    }
    best.into_iter()
        .map(|(subject, (_, _, value))| (subject, value))
        .collect()
}

/// The view's own count of what it shows.
fn counted(view: &Forest) -> (usize, usize) {
    let files = view
        .entries
        .iter()
        .filter(|entry| matches!(entry.kind, Kind::File { .. }))
        .count();
    let folders = view.entries.len() - files - 1;
    (files, folders)
}
