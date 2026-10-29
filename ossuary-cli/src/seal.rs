//! `ossuary seal`: the archive's record under one id, and the check of
//! an id against the archive.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use ossuary_core::{Continued, Seal, SealCheck, Sealing};

use crate::output::{errln, outln, say};
use crate::{audit, open};

#[allow(
    clippy::fn_params_excessive_bools,
    reason = "two switches of the verb and two of the run, each one flag the user set"
)]
pub(crate) fn seal(
    root: &Path,
    json: bool,
    dry_run: bool,
    verbose: bool,
    quiet: bool,
) -> Result<ExitCode> {
    let archive = open(root)?;
    if !quiet {
        errln!("archive {}", archive.root().display());
        errln!("checking every sealed segment against its hash");
    }
    match ossuary_core::seal(&archive, dry_run)? {
        Sealing::Empty => {
            errln!("the archive holds no claims yet; there is nothing to seal");
            Ok(ExitCode::FAILURE)
        }
        Sealing::Refused(log) => {
            let stderr = std::io::stderr();
            let mut err = stderr.lock();
            audit::claims_report(&mut err, &log, verbose)?;
            if log.findings() == 0 {
                let heading = format!(
                    "claims: {} sealed segment(s) that the open segment does not lead to",
                    log.unreached.len()
                );
                audit::listing(&mut err, &heading, &log.unreached, verbose)?;
            }
            errln!(
                "no seal made: a seal now would not cover what is listed above; seal again once that is resolved"
            );
            Ok(ExitCode::FAILURE)
        }
        Sealing::Sealed(seal) if dry_run => {
            outln!("{}", rehearsal(&seal))?;
            Ok(ExitCode::SUCCESS)
        }
        Sealing::Sealed(seal) => {
            if !quiet {
                if seal.sealed_now {
                    errln!("open segment sealed");
                } else {
                    errln!("nothing recorded since the last seal; the id is the same as then");
                }
                errln!("{}", covers(&seal));
                errln!(
                    "the id is the {} hash of the last sealed segment; keep it outside the archive, `ossuary seal --check ID` checks it",
                    seal.algorithm.name()
                );
            }
            if json {
                let line = serde_json::json!({
                    "algorithm": seal.algorithm.name(),
                    "segment": seal.segment.as_str(),
                });
                outln!("{line}")?;
            } else {
                outln!("{}", seal.segment)?;
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// What a seal covers, in a line.
fn covers(seal: &Seal) -> String {
    format!(
        "the seal covers {} sealed segment(s) with {} claim(s) and every file they refer to",
        seal.segments, seal.claims
    )
}

/// The answer of a dry run: whether the open segment would be sealed,
/// and what the seal would cover.
fn rehearsal(seal: &Seal) -> String {
    if seal.sealed_now {
        format!(
            "would seal the open segment; {}; --dry-run, nothing written",
            covers(seal).replacen("covers", "would cover", 1)
        )
    } else {
        format!(
            "nothing recorded since the last seal, {}; {}; --dry-run, nothing written",
            seal.segment,
            covers(seal)
        )
    }
}

/// `seal --check`: the seal held against the archive, step by step, and
/// the verdict last.
pub(crate) fn check(
    root: &Path,
    given: &str,
    fast: bool,
    verbose: bool,
    quiet: bool,
) -> Result<ExitCode> {
    let archive = open(root)?;
    let segment = ossuary_core::parse_id(&archive, given)?;
    let id = segment.as_str();
    if !quiet {
        errln!("archive {}", archive.root().display());
        errln!("step 1 of 3: the sealed segments, each one checked against its hash");
    }
    let cover = ossuary_core::cover(&archive, &segment)?;
    if !cover.held {
        outln!(
            "this archive holds no segment {id}, so the seal cannot be checked; it was removed, or the id belongs to another archive"
        )?;
        return Ok(ExitCode::FAILURE);
    }
    if !quiet {
        if fast {
            errln!(
                "step 2 of 3: the {} file(s) the sealed claims refer to, each one looked for; --fast, their contents are not read",
                cover.files()
            );
        } else {
            errln!(
                "step 2 of 3: the {} file(s) the sealed claims refer to, each one read in full",
                cover.files()
            );
        }
    }
    let files = ossuary_core::files(&archive, &cover, !fast)?;
    if !quiet {
        errln!("step 3 of 3: the record since the seal");
    }
    let since = ossuary_core::since(&archive, &cover)?;
    let checked = SealCheck {
        cover,
        files,
        since,
    };

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if render(&mut out, &checked, verbose)? {
        say(&mut out, &verdict(id, &checked))?;
    }
    if checked.holds() {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

/// Every finding of a check, and what was recorded since; false when the
/// reader closed the pipe.
fn render(out: &mut impl Write, checked: &SealCheck, verbose: bool) -> Result<bool> {
    let SealCheck {
        cover,
        files,
        since,
    } = checked;
    if since.continued == Continued::Yes
        && (since.segments > 0 || since.claims > 0)
        && !say(
            out,
            &format!(
                "recorded since: {} claim(s) in {} sealed segment(s) and the open segment; the seal does not cover them",
                since.claims, since.segments
            ),
        )?
    {
        return Ok(false);
    }
    let lists = [
        (
            &cover.lost_before,
            "sealed segment(s) already missing when the seal was made; a mend in the record stands in for each",
        ),
        (&cover.gone, "sealed segment(s) gone"),
        (&cover.damaged, "sealed segment(s) damaged"),
        (&files.gone, "file(s) gone"),
        (&files.damaged, "file(s) damaged"),
    ];
    for (names, what) in lists {
        if !names.is_empty()
            && !audit::listing(out, &format!("{} {what}", names.len()), names, verbose)?
        {
            return Ok(false);
        }
    }
    for (kind, unreadable) in [
        ("sealed segment", &cover.unreadable),
        ("file", &files.unreadable),
    ] {
        for (name, error) in unreadable {
            if !say(out, &format!("{kind} {name} cannot be read: {error}"))? {
                return Ok(false);
            }
        }
    }
    let line = match &since.continued {
        Continued::Yes => return Ok(true),
        Continued::No => "the open segment no longer leads to the seal: the record was cut back to an earlier point, or begun anew, after the seal was made".to_string(),
        Continued::HeadLost => "the open segment is missing, so the record no longer leads to the seal; `ossuary maintain mend` begins a new one".to_string(),
        Continued::HeadBroken(error) => format!("the open segment cannot be read: {error}; the record no longer leads to the seal"),
    };
    say(out, &line)
}

/// The last line of a check: whether the seal holds, as far as it was
/// checked, or how much is wrong.
fn verdict(id: &str, checked: &SealCheck) -> String {
    let SealCheck { cover, files, .. } = checked;
    if !checked.holds() {
        return format!(
            "the seal {id} does not hold: {} finding(s) above; restore what is named from a backup of the archive, then check again",
            checked.findings()
        );
    }
    let record = match &cover.last {
        Some(last) => format!("the record up to {}", last.as_str()),
        None => "the record".to_string(),
    };
    let size = format!(
        "{} sealed segment(s), {} claim(s)",
        cover.segments, cover.claims
    );
    if files.read {
        format!(
            "the seal {id} holds: {record} ({size}) and the {} file(s) it refers to are present and unchanged",
            cover.files()
        )
    } else {
        format!(
            "the seal {id} holds as far as --fast checks: {record} ({size}) is present and unchanged, and the {} file(s) it refers to are present; their contents were not read",
            cover.files()
        )
    }
}
