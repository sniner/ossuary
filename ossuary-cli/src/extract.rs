//! `ossuary extract`: the command onto the orchestrator.
//!
//! The orchestration lives in core ([`ossuary_core::examine`]); this
//! side only speaks. It renders the events the run tells — answers onto
//! stdout, the run narrating itself onto stderr, `-q` silencing the
//! narration alone — and turns the settlement into the closing words
//! and the exit code.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Result, anyhow};
use ossuary_core::{Archive, Event, Source, Tally};

pub fn extract(
    root: &Path,
    name: Option<&str>,
    subjects: &[String],
    full: bool,
    temp_dir: Option<&Path>,
    quiet: bool,
) -> Result<ExitCode> {
    let archive = crate::open(root)?;
    if !quiet {
        eprintln!("archive {}", archive.root().display());
    }
    let subjects = expand(&archive, subjects, quiet)?;
    let mut narrate = |event: Event<'_>| render(&event, quiet);
    let settlement =
        ossuary_core::examine(&archive, name, &subjects, full, temp_dir, &mut narrate)?;
    if settlement.ran == 0 {
        println!("nothing ran — no extractor in the archive's list answered --identify");
    }
    // A single working round closes silently — that is every ordinary
    // run, and the verdicts have already told it. Only a real cascade
    // is news.
    if settlement.rounds > 1 && !quiet {
        eprintln!(
            "settled after {} rounds: {} examinations in all",
            settlement.rounds, settlement.examinations
        );
    }
    // The call's own run id, said once for the whole call — but only
    // when something carries it: a call that derived nothing put no
    // prov:run on the record, and naming one would point at nothing.
    if settlement.derived > 0 {
        println!(
            "{} derived file(s) in all, taken in as run {}",
            settlement.derived, settlement.run
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
    if !ids.iter().any(|id| crate::export::run_id(id)) {
        return Ok(ids.to_vec());
    }
    let mut index = archive.index()?;
    crate::catch_up(&mut index, archive, quiet)?;
    let mut names: Vec<String> = Vec::new();
    for id in ids {
        if crate::export::run_id(id) {
            let sightings = index.run_sightings(id)?;
            if sightings.is_empty() {
                return Err(anyhow!(
                    "no run {id} on the record — `ossuary about FILE prov:run` names the runs a file arrived in; nothing was examined"
                ));
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
fn render(event: &Event<'_>, quiet: bool) {
    match event {
        Event::CaughtUp { segments } => {
            if !quiet {
                eprintln!("catching the index up: {segments} sealed segment(s) it had not seen");
            }
        }
        Event::Skipped { trouble, .. } => eprintln!("skipped: {}", trouble.spelled()),
        Event::Idle { source, full } => {
            if *full {
                println!("nothing of a kind {source} reads is on the record");
            } else {
                println!(
                    "nothing waiting for {source} — no file of a kind it reads stands unexamined"
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
                eprintln!("{count} file(s) named for {source}");
            }
        }
        Event::Verdict {
            source,
            tally,
            already,
            failures,
        } => {
            println!("{}", verdict(source, tally, *already));
            if !failures.is_empty() {
                eprintln!(
                    "{} could not be examined — offered again next run:",
                    failures.len()
                );
                for (subject, error) in *failures {
                    eprintln!("  {subject}: {}", error.spelled());
                }
            }
        }
    }
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
                "{taken} derived file(s) taken in, {} of them bytes the archive already held",
                tally.known
            )
        } else {
            format!("{taken} derived file(s) taken in")
        });
    }
    if tally.nothing > 0 {
        verdict.push(format!("{} had nothing to tell", tally.nothing));
    }
    if already > 0 {
        verdict.push(format!(
            "{already} already examined — --full examines them anew"
        ));
    }
    verdict.join("; ")
}
