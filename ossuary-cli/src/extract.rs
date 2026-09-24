//! `ossuary extract`: the command onto the orchestrator.
//!
//! The orchestration lives in core ([`ossuary_core::examine`]); this
//! side only speaks. It renders the events the run tells — answers onto
//! stdout, the run narrating itself onto stderr, `-q` silencing the
//! narration alone — and turns the settlement into the closing words
//! and the exit code.

use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Result, anyhow};
use ossuary_core::{Archive, Event, Run, Source, Tally};

pub fn extract(
    root: &Path,
    name: Option<&str>,
    subjects: &[String],
    full: bool,
    dry_run: bool,
    temp_dir: Option<&Path>,
    quiet: bool,
) -> Result<ExitCode> {
    // Said no before the archive is even opened: the refusal does not
    // depend on what stands in it.
    if dry_run && subjects.is_empty() {
        return Err(anyhow!(
            "a dry run needs named files; name subjects or a run id"
        ));
    }
    let archive = crate::open(root)?;
    if !quiet {
        eprintln!("archive {}", archive.root().display());
    }
    let subjects = expand(&archive, subjects, quiet)?;
    let mut narrate = |event: Event<'_>| render(&event, quiet, dry_run);
    let settlement = ossuary_core::examine(
        &archive,
        name,
        &subjects,
        full,
        dry_run,
        temp_dir,
        &mut narrate,
    )?;
    if settlement.ran == 0 {
        println!(
            "no extractor ran; none in the [extract] run list of config.toml responded to --identify"
        );
    }
    // A single working round closes silently — that is every ordinary
    // run, and the verdicts have already told it. Only a real cascade
    // is news.
    if settlement.rounds > 1 && !quiet {
        eprintln!(
            "done after {} rounds, {} examinations in total",
            settlement.rounds, settlement.examinations
        );
    }
    // The call's own run id, said once for the whole call: every claim
    // it wrote carries it, so a call that examined nothing has none to
    // name.
    if settlement.examinations > 0 {
        println!(
            "{} file(s) examined in total, {} derived file(s) added; run {}",
            settlement.examinations, settlement.derived, settlement.run
        );
    }
    Ok(if settlement.clean {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The named ids, run ids expanded: a dashed run id becomes every file
/// that run recorded — the same grammar `export` speaks — and file
/// names pass through as given, for core to resolve. Whole before
/// anything runs: an unknown run refuses the call, not its third pass.
fn expand(archive: &Archive, ids: &[String], quiet: bool) -> Result<Vec<String>> {
    if !ids.iter().any(|id| Run::spelled(id)) {
        return Ok(ids.to_vec());
    }
    let mut index = archive.index()?;
    crate::catch_up(&mut index, archive, quiet)?;
    let mut names: Vec<String> = Vec::new();
    for id in ids {
        if Run::spelled(id) {
            let run = Run::parse(id)?;
            let sightings = index.run_sightings(&run)?;
            if sightings.is_empty() {
                return Err(if index.has_run(&run)? {
                    anyhow!("run {id} recorded no files; nothing examined")
                } else {
                    anyhow!(
                        "run {id} not found, nothing examined; `ossuary history` lists the runs"
                    )
                });
            }
            for (subject, _) in sightings {
                let name = subject.as_str().to_string();
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        } else {
            names.push(id.clone());
        }
    }
    Ok(names)
}

/// One event onto the terminal. Idle notes and verdicts are answers and
/// go to stdout; skipped entries and failure lists survive `-q` the way
/// every failure does; the rest is narration.
fn render(event: &Event<'_>, quiet: bool, dry_run: bool) {
    match event {
        Event::Rehearsed {
            subject,
            findings,
            derived,
            ..
        } => {
            // The block an answer speaks: the file's name, what would
            // stand on it, what would arrive beside it.
            let mut block = subject.to_string();
            for (attribute, value) in *findings {
                block.push_str("\n  ");
                block.push_str(&crate::output::pair(attribute.as_str(), value));
            }
            for (name, mime, bytes) in *derived {
                write!(
                    block,
                    "\n  derived {name} ({mime}, {})",
                    crate::output::human_bytes(*bytes)
                )
                .expect("a String takes what is written to it");
            }
            println!("{block}");
        }
        Event::CaughtUp { segments } => {
            if !quiet {
                eprintln!("index updated: {segments} new log segment(s)");
            }
        }
        Event::Skipped { trouble, .. } => eprintln!("skipped: {}", trouble.spelled()),
        Event::Idle { source, full } => {
            if *full {
                println!("no recorded file of a type {source} reads");
            } else {
                println!(
                    "nothing waiting for {source}; every file of a type it reads has been examined"
                );
            }
        }
        Event::Waiting {
            source,
            count,
            round,
        } => {
            if !quiet {
                if *round > 1 {
                    eprintln!("round {round}: {count} file(s) waiting for {source}");
                } else {
                    eprintln!("{count} file(s) waiting for {source}");
                }
            }
        }
        Event::Named { source, count } => {
            if !quiet {
                eprintln!("{count} file(s) given for {source}");
            }
        }
        Event::Remarked {
            subject,
            name,
            remark,
            ..
        } => {
            // The extractor's words, with the file it could not name
            // itself in front: narration, silenced by -q like the rest.
            if !quiet {
                let file = match name {
                    Some(name) => format!("{subject} ({name})"),
                    None => subject.to_string(),
                };
                for line in remark.lines() {
                    eprintln!("  {file}: {line}");
                }
            }
        }
        Event::Verdict {
            source,
            tally,
            already,
            failures,
        } => {
            if dry_run {
                println!("{}", rehearsal_verdict(source, tally, *already));
            } else {
                println!("{}", verdict(source, tally, *already));
            }
            if !failures.is_empty() {
                eprintln!(
                    "{} could not be examined, will be retried on the next run:",
                    failures.len()
                );
                for (subject, error) in *failures {
                    eprintln!("  {subject}: {}", error.spelled());
                }
            }
        }
    }
}

/// The rehearsal's verdict: what was tried, and that nothing stands
/// changed for it.
fn rehearsal_verdict(source: &Source, tally: &Tally, already: usize) -> String {
    let mut verdict = vec![format!(
        "{} file(s) examined by {source} (dry run)",
        tally.examined
    )];
    if tally.nothing > 0 {
        verdict.push(format!("{} found nothing", tally.nothing));
    }
    if already > 0 {
        verdict.push(format!(
            "{already} already examined; --full examines them again"
        ));
    }
    format!("{}; nothing written", verdict.join("; "))
}

/// The pass's verdict, in one line: what happened, and which parts of
/// "nothing" are the calm kind.
fn verdict(source: &Source, tally: &Tally, already: usize) -> String {
    let mut verdict = vec![format!(
        "{} file(s) examined by {source}, {} claim(s) written",
        tally.examined, tally.claims
    )];
    let taken = tally.stored + tally.known;
    if taken > 0 {
        verdict.push(if tally.known > 0 {
            format!(
                "{taken} derived file(s) added, {} of them already in the archive",
                tally.known
            )
        } else {
            format!("{taken} derived file(s) added")
        });
    }
    if tally.nothing > 0 {
        verdict.push(format!("{} found nothing", tally.nothing));
    }
    if already > 0 {
        verdict.push(format!(
            "{already} already examined; --full examines them again"
        ));
    }
    verdict.join("; ")
}
