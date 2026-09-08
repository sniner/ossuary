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
use ossuary_core::{Archive, Error, Standing, Timestamp, Value};

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
compile_error!("ossuary-mount has a door for macOS (NFS) and one for Linux (FUSE) only");

use forest::{Forest, Kind, Sighting};
use record::Record;

#[derive(Parser)]
#[command(
    name = "ossuary-mount",
    version,
    about = "The record as a read-only filesystem: every place it knows, browsable in a file manager and readable by any program"
)]
struct Cli {
    /// The archive to mount; standing in it is enough
    #[arg(long, value_name = "DIR", env = "OSSUARY_ARCHIVE", default_value = ".")]
    archive: PathBuf,

    /// Where the view appears; created when missing, and a directory
    /// this command created goes with the mount when it ends. The
    /// command stays in the foreground — Ctrl-C gives the directory
    /// back
    #[arg(value_name = "DIR")]
    mountpoint: PathBuf,

    /// Show the record as it stood at this moment, UTC — what was
    /// known then, including what was retracted since. 2026-01-01 or
    /// 2026-01-01T08:00:00, a trailing Z welcome
    #[arg(long, value_name = "TIME")]
    as_of: Option<String>,

    /// Answers and errors only — the run keeps its narration to itself
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
    let cutoff = as_of.as_deref().map(closing).transpose()?;
    let archive = open(&archive)?;
    let view = grown_view(&archive, cutoff.as_deref(), quiet)?;

    let moment = cutoff
        .clone()
        .unwrap_or_else(|| Timestamp::now().as_str().to_string());
    let view_time = forest::clamped(forest::epoch(&moment)).unwrap_or(0);

    let made = !mountpoint.exists();
    std::fs::create_dir_all(&mountpoint)
        .with_context(|| format!("{}: making the mountpoint", mountpoint.display()))?;
    let owner = std::fs::metadata(&mountpoint)
        .with_context(|| format!("{}: reading the mountpoint", mountpoint.display()))?;
    let (uid, gid) = {
        use std::os::unix::fs::MetadataExt as _;
        (owner.uid(), owner.gid())
    };

    let (files, folders) = counted(&view);
    let room = Room {
        place: mountpoint.display().to_string(),
        cutoff: cutoff.as_deref(),
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
    cutoff: Option<&'a str>,
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
            cutoff,
            files,
            folders,
            ..
        } = self;
        let stood = match cutoff {
            Some(moment) => format!(", as it stood at {moment}"),
            None => String::new(),
        };
        self.tell(format_args!(
            "the record stands at {place} — read-only, {files} file(s) in {folders} folder(s){stood}; Ctrl-C gives it back"
        ));
        if *files == 0 {
            self.tell("no places on the record yet — `ossuary ingest` fills the view");
        }
    }

    /// The room is given back.
    pub fn given_back(&self) {
        self.tell(format_args!("{} given back", self.place));
    }
}

/// The archive, or the way to one — `ossuary`'s own wording.
fn open(root: &Path) -> Result<Archive> {
    Archive::open(root).map_err(|error| match error {
        Error::NoArchive(path) => anyhow!(
            "{}: not an ossuary archive — stand in one, name it with --archive, or begin one with `ossuary init`",
            path.display()
        ),
        other => other.into(),
    })
}

/// The view, grown from the record as it stood at `cutoff`.
fn grown_view(archive: &Archive, cutoff: Option<&str>, quiet: bool) -> Result<Forest> {
    let mut index = archive.index()?;
    let folded = index.fold(archive.log())?;
    if folded.segments > 0 && !quiet {
        eprintln!(
            "catching the index up: {} sealed segment(s) it had not seen",
            folded.segments
        );
    }

    let mut places = Vec::new();
    for standing in index.standing_as_of("file:path", cutoff)? {
        let Value::String(path) = standing.value else {
            continue;
        };
        places.push(Sighting {
            path,
            digest: standing.subject.as_str().to_string(),
            asserted: standing.asserted,
            order: standing.order,
        });
    }

    let mut sizes = BTreeMap::new();
    for (subject, value) in newest(index.standing_as_of("file:size", cutoff)?) {
        if let Some(size) = value.as_u64() {
            sizes.insert(subject, size);
        }
    }
    let mut modified = BTreeMap::new();
    for (subject, value) in newest(index.standing_as_of("file:modified", cutoff)?) {
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

/// The cutoff in claim time's own spelling, from friendlier forms.
fn closing(given: &str) -> Result<String> {
    let mut spelled = given.trim().to_string();
    if spelled.len() == 10 {
        spelled.push_str("T00:00:00");
    }
    if !spelled.ends_with('Z') {
        spelled.push('Z');
    }
    match Timestamp::parse(&spelled) {
        Ok(moment) => Ok(moment.as_str().to_string()),
        Err(_) => Err(anyhow!(
            "{given:?} names no moment — the record reads UTC: 2026-01-01 or 2026-01-01T08:00:00, a trailing Z welcome"
        )),
    }
}
