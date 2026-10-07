//! `ossuary merge`: another archive taken into this one, in steps the
//! run announces, additive until the last. By copying when both
//! archives use the same hash; by rehashing and replaying when they do
//! not.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Result, anyhow};
use ossuary_core::{Archive, Copied, Digest, Error, Join, Store};

use crate::open;

/// How the merge is to run.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    /// Hash the other archive's files anew and replay its claims.
    pub rehash: bool,
    /// Report only, write nothing.
    pub dry_run: bool,
    /// Go on past obstacles, damage and claims that cannot be rewritten.
    pub force: bool,
}

pub(crate) fn merge(root: &Path, other: &Path, options: Options, quiet: bool) -> Result<ExitCode> {
    let Options {
        rehash,
        dry_run,
        force,
    } = options;
    let ours = open(root)?;
    let theirs = Archive::open(other).map_err(|error| match error {
        Error::NoArchive(path) => anyhow!("{}: not an ossuary archive", path.display()),
        other => other.into(),
    })?;
    if !quiet {
        eprintln!("archive {}", ours.root().display());
        eprintln!("merging {}", theirs.root().display());
        eprintln!("step 1 of 5: reading the other archive's chain of segments");
    }
    let obstacles = ossuary_core::check(&ours, &theirs, rehash)?;
    for name in &obstacles.accounts {
        println!(
            "account {name} is configured for different mailboxes in the two mailvault.toml files; the mailbox:place values of both would read as one mailbox; rename the account in one archive"
        );
    }
    if obstacles.log_findings > 0 {
        println!(
            "{} finding(s) in the other archive's chain of segments; `ossuary --archive {} audit` lists them, and a merge carries them over",
            obstacles.log_findings,
            theirs.root().display()
        );
    }
    if !obstacles.is_empty() && !force {
        println!("not merged; nothing was written. Pass --force to merge anyway");
        return Ok(ExitCode::FAILURE);
    }
    match (dry_run, rehash) {
        (true, false) => preview(&ours, &theirs),
        (true, true) => preview_rehash(&ours, &theirs),
        (false, false) => by_copy(&ours, &theirs, force, quiet),
        (false, true) => by_rehash(&ours, &theirs, force, quiet),
    }
}

/// The merge of two archives with the same hash: their files and
/// segments copied as they are, the chains joined.
fn by_copy(ours: &Archive, theirs: &Archive, force: bool, quiet: bool) -> Result<ExitCode> {
    if !quiet {
        eprintln!("step 2 of 5: sealing the other archive's open segment");
    }
    let sealed_theirs = theirs.log().seal()?;
    if !quiet {
        eprintln!("step 3 of 5: copying content/ and derived/");
    }
    let content = ossuary_core::copy_store(theirs.content(), ours.content())?;
    let derived = ossuary_core::copy_store(theirs.derived(), ours.derived())?;
    if !quiet {
        eprintln!("step 4 of 5: copying the sealed segments");
    }
    let segments = ossuary_core::copy_store(theirs.log().store(), ours.log().store())?;
    let damaged = damaged(&[
        ("content/", &content),
        ("derived/", &derived),
        ("claims/", &segments),
    ]);
    if damaged > 0 && !force {
        println!(
            "not merged: {damaged} damaged file(s) in the other archive; {} copied so far stay here. Restore the damaged files from a backup of the other archive and run the merge again, or pass --force to merge without them",
            content.copied + derived.copied + segments.copied
        );
        return Ok(ExitCode::FAILURE);
    }
    if !quiet {
        eprintln!("step 5 of 5: joining the chains of segments");
    }
    let joined = ossuary_core::join(ours, theirs)?;
    let memo = ossuary_core::take_memo(ours, theirs)?;
    let mut clauses = vec![format!(
        "merged: {} file(s) copied to content/{}, {} file(s) copied to derived/{}, {} segment(s) copied{}",
        content.copied,
        known(&content),
        derived.copied,
        known(&derived),
        segments.copied,
        known(&segments),
    )];
    if let Some(segment) = sealed_theirs {
        clauses.push(format!(
            "the other archive's open segment was sealed as {}",
            segment.digest()
        ));
    }
    clauses.extend(chain_clauses(&joined));
    clauses.extend(memo_clause(memo));
    if damaged > 0 {
        clauses.push(format!(
            "{damaged} damaged file(s) left out; the claims about them stand without the file"
        ));
    }
    println!("{}", clauses.join("; "));
    Ok(exit(damaged == 0))
}

/// The merge of two archives with different hashes: their files
/// hashed anew on the way in, their claims replayed with the new names.
fn by_rehash(ours: &Archive, theirs: &Archive, force: bool, quiet: bool) -> Result<ExitCode> {
    if !quiet {
        eprintln!("step 2 of 5: hashing the files in content/ anew and storing them");
    }
    let mut names = BTreeMap::new();
    let content = ossuary_core::rehash_store(theirs.content(), ours.content(), &mut names)?;
    if !quiet {
        eprintln!("step 3 of 5: the same for derived/");
    }
    let derived = ossuary_core::rehash_store(theirs.derived(), ours.derived(), &mut names)?;
    let damaged = damaged(&[("content/", &content), ("derived/", &derived)]);
    if damaged > 0 && !force {
        println!(
            "not merged: {damaged} damaged file(s) in the other archive; {} stored so far stay here. Restore the damaged files from a backup of the other archive and run the merge again, or pass --force to merge without them",
            content.copied + derived.copied
        );
        return Ok(ExitCode::FAILURE);
    }
    if !quiet {
        eprintln!("step 4 of 5: replaying the other archive's claims with the new names");
    }
    let replayed = ossuary_core::replay(ours, theirs, &names, force)?;
    if !quiet {
        eprintln!("step 5 of 5: taking over the mail fetcher's resume points");
    }
    let memo = ossuary_core::take_memo(ours, theirs)?;
    let mut clauses = vec![
        format!(
            "merged by rehashing: {} file(s) stored in content/{}, {} file(s) stored in derived/{}",
            content.copied,
            known(&content),
            derived.copied,
            known(&derived),
        ),
        format!(
            "{} segment(s) replayed with {} claim(s)",
            replayed.segments, replayed.claims
        ),
    ];
    clauses.extend(memo_clause(memo));
    if replayed.dropped > 0 {
        clauses.push(format!(
            "{} claim(s) left out; the files they are about are not in the other archive",
            replayed.dropped
        ));
    }
    if damaged > 0 {
        clauses.push(format!(
            "{damaged} damaged file(s) left out, and the claims about them with them"
        ));
    }
    println!("{}", clauses.join("; "));
    Ok(exit(damaged == 0 && replayed.dropped == 0))
}

/// Name every damaged entry, and count them.
fn damaged(stores: &[(&str, &Copied)]) -> usize {
    let mut count = 0;
    for (store, copied) in stores {
        for name in &copied.damaged {
            println!("{store}{name} in the other archive is damaged and was not copied");
            count += 1;
        }
    }
    count
}

/// What the chain step did, for the verdict.
fn chain_clauses(joined: &Join) -> Vec<String> {
    let mut clauses = Vec::new();
    if let Some(segment) = &joined.sealed {
        clauses.push(format!(
            "this archive's open segment was sealed as {segment}"
        ));
    }
    if joined.changed {
        let names: Vec<&str> = joined.names.iter().map(Digest::as_str).collect();
        clauses.push(format!(
            "the open segment now follows {}",
            names.join(" and ")
        ));
    } else {
        clauses
            .push("the open segment already followed the other archive's last segment".to_string());
    }
    clauses
}

fn memo_clause(memo: Option<usize>) -> Option<String> {
    memo.map(|rows| format!("{rows} mailbox resume point(s) taken over"))
}

/// What was here already, when that is not nothing.
fn known(copied: &Copied) -> String {
    if copied.known > 0 {
        format!(" ({} already here)", copied.known)
    } else {
        String::new()
    }
}

fn exit(clean: bool) -> ExitCode {
    if clean {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// What the merge would copy, and nothing written.
fn preview(ours: &Archive, theirs: &Archive) -> Result<ExitCode> {
    let content = ossuary_core::preview_copy(theirs.content(), ours.content())?;
    let derived = ossuary_core::preview_copy(theirs.derived(), ours.derived())?;
    let segments = ossuary_core::preview_copy(theirs.log().store(), ours.log().store())?;
    let open = theirs.log().head()?.len();
    let sealing = if open > 0 {
        format!(", and seal the {open} claim(s) in the other archive's open segment as one more")
    } else {
        String::new()
    };
    println!(
        "would copy {} file(s) to content/ ({} already here), {} file(s) to derived/ ({} already here) and {} sealed segment(s) ({} already here){sealing}; nothing written",
        content.new, content.known, derived.new, derived.known, segments.new, segments.known
    );
    Ok(ExitCode::SUCCESS)
}

/// What a rehash would do, and nothing written. Which files are here
/// already is not known before they are hashed, so none are counted
/// as such.
fn preview_rehash(ours: &Archive, theirs: &Archive) -> Result<ExitCode> {
    let content = entries(theirs.content())?;
    let derived = entries(theirs.derived())?;
    let log = theirs.log();
    let mut segments = 0;
    let mut claims = 0;
    for segment in log.segments()? {
        let read = log.read(segment.digest())?.len();
        if read > 0 {
            segments += 1;
            claims += read;
        }
    }
    let open = log.head()?.len();
    if open > 0 {
        segments += 1;
        claims += open;
    }
    println!(
        "would hash {content} file(s) in content/ and {derived} file(s) in derived/ anew with {}, store them here, and replay {segments} segment(s) with {claims} claim(s) under the new names; nothing written",
        ours.content().algorithm()
    );
    Ok(ExitCode::SUCCESS)
}

fn entries(store: &Store) -> Result<usize> {
    let mut count = 0;
    for entry in store.entries() {
        entry?;
        count += 1;
    }
    Ok(count)
}
