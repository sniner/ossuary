//! ossuary-sync: the archive copied to a second place, and kept up to
//! date there.
//!
//! A run seals the archive and brings the copy up to that seal. The
//! copy's open segment names the seal it holds, and what it lacks is
//! worked out from this archive's record alone, so a later run copies
//! only what was recorded since and never lists the copy. The copy is an
//! archive of its own, and `ossuary seal --check` checks it against the
//! id a run prints.
//!
//! An outside verb like `ossuary-mount`: found on the PATH, handed the
//! archive in `OSSUARY_ARCHIVE`, linking the core. The place a copy lies
//! is a [`Remote`]: a directory, or a folder on a `WebDAV` server.

#![cfg_attr(test, allow(clippy::assert_is_empty))]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Result, anyhow, bail};
use clap::Parser;
use ossuary_core::{Archive, Digest, Error, Seal, Sealing};

mod directory;
mod filebridge;
mod output;
mod remote;
mod sync;
mod webdav;

use directory::Directory;
use filebridge::Filebridge;
use output::{Say, human_bytes, listing};
use remote::Remote;
use sync::{Copied, Held, Plan};
use webdav::{Certificate, Login, WebDav};

#[derive(Parser)]
#[command(
    name = "ossuary-sync",
    version,
    about = "Copy an ossuary archive to another directory, a filebridge location or a WebDAV folder, or bring the copy up to date",
    long_about = "Copy an ossuary archive to another directory, a filebridge location or a WebDAV folder, or bring the copy up to date

sync seals the archive and copies to TARGET every file and sealed segment the
copy does not hold yet; a later sync copies only what was recorded since. The
copy is an archive of its own, and the seal id that sync prints checks it:
`ossuary --archive DIR seal --check ID`, with DIR the copy's directory, on the
server for a filebridge or WebDAV folder. cache/, config.toml and
mailvault.toml are not copied, and nothing in the copy is deleted.

TARGET is a directory, a folder in a filebridge location named as
filebridge+http://HOST:PORT/LOCATION/FOLDER, or the http:// or https:// URL of a
WebDAV folder. Set OSSUARY_SYNC_TOKEN for a filebridge location with a token,
and OSSUARY_SYNC_USERNAME and OSSUARY_SYNC_PASSWORD for a WebDAV server that
asks for a login.

If a file is missing or damaged in the archive, the copy keeps the seal it had,
and sync exits 1.",
    max_term_width = 100
)]
#[allow(
    clippy::doc_markdown,
    reason = "help texts; backticks would show in --help"
)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the flags of the command line, as they were set"
)]
struct Cli {
    /// The ossuary archive to copy
    #[arg(long, value_name = "DIR", env = "OSSUARY_ARCHIVE", default_value = ".")]
    archive: PathBuf,

    /// Print only results and errors
    #[arg(short, long)]
    quiet: bool,

    /// List every file where the output would otherwise show only a
    /// count
    #[arg(short, long)]
    verbose: bool,

    /// The directory, filebridge folder or WebDAV folder the copy is in.
    /// It must exist, and be empty for the first sync
    #[arg(value_name = "TARGET")]
    target: PathBuf,

    /// Show what would be copied, write nothing
    #[arg(long)]
    dry_run: bool,

    /// Accept the server's certificate without checking it, for a WebDAV
    /// https:// target with a self-signed certificate
    #[arg(long)]
    accept_any_certificate: bool,
}

fn main() -> ExitCode {
    match run(&Cli::parse()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("ossuary-sync: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<ExitCode> {
    let say = Say::new(cli.quiet);
    let archive = open(&cli.archive)?;
    let certificate = if cli.accept_any_certificate {
        Certificate::AcceptAny
    } else {
        Certificate::Check
    };
    let remote = place(&cli.target, certificate)?;
    let remote = remote.as_ref();
    say.line(format_args!("archive {}", archive.root().display()));
    say.line(format_args!("target {}", cli.target.display()));
    if cli.accept_any_certificate {
        say.line("the server's certificate is not checked");
    }

    say.line("step 1 of 4: sealing the archive, after checking every sealed segment");
    let seal = sealed(&archive, cli.dry_run)?;
    say.line(if !seal.sealed_now {
        "nothing recorded since the last seal"
    } else if cli.dry_run {
        "would seal the open segment"
    } else {
        "open segment sealed"
    });

    say.line("step 2 of 4: working out what the target lacks");
    let held = sync::held(&archive, remote)?;
    say.line(match &held.seal {
        None => "the target holds no seal yet".to_string(),
        Some(segment) if *segment == seal.segment => "the target holds this seal".to_string(),
        Some(segment) => format!("the target holds the seal {segment}"),
    });
    // A dry run has not sealed the open segment, so its claims are
    // counted on their own and the segment it would become is one more.
    let unsealed = cli.dry_run && seal.sealed_now;
    let plan = if unsealed {
        let open = archive.log().head_contents()?;
        sync::plan(&archive, held.seal.as_ref(), open.previous(), open.claims())?
    } else {
        sync::plan(
            &archive,
            held.seal.as_ref(),
            std::slice::from_ref(&seal.segment),
            &[],
        )?
    };
    let segments = plan.segments.len() + usize::from(unsealed);

    if plan.is_empty() && !unsealed {
        if !cli.dry_run {
            sync::prepare(&archive, remote, held.marked)?;
        }
        say.line("nothing to copy");
        println!("{}", seal.segment);
        return Ok(ExitCode::SUCCESS);
    }
    say.line(format_args!(
        "the target lacks {} file(s), {}, and {segments} segment(s)",
        plan.files.len() + plan.gone.len(),
        human_bytes(plan.bytes())
    ));

    if cli.dry_run {
        return Ok(rehearsal(&plan, segments, &held, &seal, cli.verbose));
    }

    sync::prepare(&archive, remote, held.marked)?;
    let copied = copy_files(&archive, remote, &plan, say)?;
    let findings = plan.gone.len() + copied.damaged.len() + copied.unreadable.len();
    if findings > 0 {
        report(&plan, &copied, cli.verbose);
        let kept = if copied.new > 0 {
            format!("; the {} file(s) copied stay there", copied.new)
        } else {
            String::new()
        };
        println!(
            "the target was not brought up to the seal {}: {findings} file(s) above; {}{kept}; `ossuary audit` lists what is missing or damaged here, sync again once it is restored",
            seal.segment,
            holds(&held)
        );
        return Ok(ExitCode::FAILURE);
    }

    say.line(format_args!(
        "step 4 of 4: copying {} segment(s), then the open segment",
        plan.segments.len()
    ));
    sync::copy_segments(&archive, remote, &plan.segments)?;
    sync::advance(remote, &seal.segment)?;
    say.line(copied_line(&copied));
    let id = &seal.segment;
    say.line(if cli.target.to_string_lossy().contains("://") {
        format!(
            "the target holds the seal {id}; in the folder on the server, or with it mounted, `ossuary --archive DIR seal --check {id}` checks it"
        )
    } else {
        format!(
            "the target holds the seal {id}; `ossuary --archive {} seal --check {id}` checks it",
            cli.target.display()
        )
    });
    println!("{}", seal.segment);
    Ok(ExitCode::SUCCESS)
}

/// The place `target` names: a `WebDAV` folder for an http or https URL, a
/// directory otherwise.
fn place(target: &Path, certificate: Certificate) -> Result<Box<dyn Remote>> {
    let name = target.to_string_lossy();
    if name.starts_with("filebridge+") {
        if certificate == Certificate::AcceptAny {
            bail!(
                "{name}: {} is for a WebDAV https:// target; leave it out",
                webdav::ACCEPT_ANY
            );
        }
        let place = Filebridge::open(&name, filebridge::token_from_env()?)?;
        return Ok(Box::new(place));
    }
    if webdav_url(target) {
        let url = target.to_string_lossy();
        let place = WebDav::open(&url, Login::from_env()?, certificate)?;
        return Ok(Box::new(place));
    }
    if certificate == Certificate::AcceptAny {
        bail!(
            "{}: {} is for an https:// target; leave it out",
            target.display(),
            webdav::ACCEPT_ANY
        );
    }
    if target.to_string_lossy().contains("://") {
        bail!(
            "{}: a target is a directory, a filebridge+http:// or filebridge+https:// folder, or the http:// or https:// URL of a WebDAV folder; mount any other share and name the directory",
            target.display()
        );
    }
    Ok(Box::new(Directory::open(target)?))
}

fn webdav_url(target: &Path) -> bool {
    let target = target.to_string_lossy();
    target.starts_with("https://") || target.starts_with("http://")
}

/// The archive sealed, or why there is nothing to sync.
fn sealed(archive: &Archive, dry_run: bool) -> Result<Seal> {
    match ossuary_core::seal(archive, dry_run)? {
        Sealing::Sealed(seal) => Ok(seal),
        Sealing::Empty => bail!("the archive holds no claims yet; there is nothing to sync"),
        Sealing::Refused(log) => bail!(
            "the archive cannot be sealed, {} finding(s) in its record; nothing was copied; `ossuary seal` lists them and the way out, sync again once they are resolved",
            log.findings() + log.unreached.len()
        ),
    }
}

/// Step 3: every file the target lacks, with a progress line.
fn copy_files(archive: &Archive, remote: &dyn Remote, plan: &Plan, say: Say) -> Result<Copied> {
    let total = plan.files.len();
    if total == 0 {
        say.line("step 3 of 4: no file to copy");
        return Ok(Copied::default());
    }
    say.line(format_args!("step 3 of 4: copying {total} file(s)"));
    let mut progress = say.progress();
    let copied = sync::copy_files(archive, remote, &plan.files, |done| {
        progress.update(done, &format!("copying files: {done} of {total}"));
    })?;
    progress.finish();
    Ok(copied)
}

/// The answer of a dry run.
fn rehearsal(plan: &Plan, segments: usize, held: &Held, seal: &Seal, verbose: bool) -> ExitCode {
    let copy = format!(
        "would copy {} file(s), {}, and {segments} segment(s), then the open segment",
        plan.files.len(),
        human_bytes(plan.bytes())
    );
    if plan.gone.is_empty() {
        println!("{copy}; --dry-run, nothing written");
        return ExitCode::SUCCESS;
    }
    let gone = names(&plan.gone);
    listing(
        &format!("{} file(s) missing here", gone.len()),
        &gone,
        verbose,
    );
    println!(
        "{copy} but would not bring the target up to the seal {}, because of the file(s) above; {}; --dry-run, nothing written",
        seal.segment,
        holds(held)
    );
    ExitCode::FAILURE
}

/// Every file that kept the target from the seal.
fn report(plan: &Plan, copied: &Copied, verbose: bool) {
    let gone = names(&plan.gone);
    if !gone.is_empty() {
        listing(
            &format!("{} file(s) missing here", gone.len()),
            &gone,
            verbose,
        );
    }
    if !copied.damaged.is_empty() {
        listing(
            &format!("{} file(s) damaged here", copied.damaged.len()),
            &copied.damaged,
            verbose,
        );
    }
    for (name, error) in &copied.unreadable {
        println!("file {name} cannot be read here: {error}");
    }
}

/// What the target holds while it was not brought up to the seal.
fn holds(held: &Held) -> String {
    match &held.seal {
        Some(segment) => format!("it still holds the seal {segment}"),
        None => "it holds no seal yet".to_string(),
    }
}

fn copied_line(copied: &Copied) -> String {
    let line = format!(
        "copied {} file(s), {}",
        copied.new,
        human_bytes(copied.bytes)
    );
    if copied.known == 0 {
        line
    } else {
        format!(
            "{line}; {} file(s) were at the target already",
            copied.known
        )
    }
}

fn names(digests: &[Digest]) -> Vec<String> {
    digests
        .iter()
        .map(|digest| digest.as_str().to_string())
        .collect()
}

/// The archive, or the way to one.
fn open(root: &Path) -> Result<Archive> {
    Archive::open(root).map_err(|error| match error {
        Error::NoArchive(path) => anyhow!(
            "{}: not an ossuary archive; run in an archive, name one with --archive, or create one with `ossuary init`",
            path.display()
        ),
        other => other.into(),
    })
}
