//! Describe: record the size and the type of files that have neither.
//!
//! Size and detected type are recorded when bytes are new to a store. A
//! file that one run stored and a later run recorded has claims and
//! neither of the two; the audit lists such files (see `audit`). The
//! bytes are in the store, so both can be recorded from them at any time:
//! each file is read once, its length counted and its type detected the
//! way an ingest detects it.
//!
//! The claims carry the source `maintain`. No run that took the file in
//! recorded them, and the source says so.

use serde_json::json;

use crate::accession::{Observed, known_attribute};
use crate::archive::Archive;
use crate::audit::{Audit, LogAudit, Undescribed, audit_log, audit_store};
use crate::claim::{Claim, Run, Source, Subject, Timestamp};
use crate::error::{Error, Result};

/// The source of the claims [`describe`] writes.
const SOURCE: &str = "maintain";

/// What looking for files without size or type came to.
#[derive(Debug)]
pub enum Undescribing {
    /// The files, none when every recorded file has both.
    Found(Vec<Undescribed>),
    /// The log has findings. A claim that cannot be read may be the one
    /// that gives a file its size, so nothing is concluded from the rest.
    /// The audit of the log says what is wrong.
    Refused(Box<LogAudit>),
}

/// What [`describe`] recorded.
#[derive(Debug)]
pub struct Described {
    /// Files that got a size, a type or both.
    pub files: usize,
    /// Claims written.
    pub claims: usize,
    /// The run they were written in.
    pub run: Run,
    /// Files that neither store held any more when they were to be read.
    pub gone: Vec<String>,
}

/// Every file that is held and recorded and has no `file:size` or no
/// `file:mime` in the history of the log.
///
/// Reads every sealed segment and the open segment and lists both
/// stores; no file is read.
///
/// # Errors
///
/// Everything [`audit_log`] and [`audit_store`] can answer.
pub fn undescribed(archive: &Archive) -> Result<Undescribing> {
    let log = audit_log(archive.log())?;
    if log.findings() > 0 {
        return Ok(Undescribing::Refused(Box::new(log)));
    }
    let audit = Audit::assemble(
        audit_store(archive.content(), false)?,
        audit_store(archive.derived(), false)?,
        log,
    );
    Ok(Undescribing::Found(audit.undescribed))
}

/// Record what `files` lack: each file is read from `content/`, or from
/// `derived/` when `content/` does not hold it, and its size and detected
/// type are appended in one write, under one run and one time.
///
/// # Errors
///
/// [`Error::Io`] for a file that cannot be read to its end, and
/// everything readying the log and appending to it can answer. A failure
/// writes no claim.
pub fn describe(archive: &Archive, files: &[Undescribed]) -> Result<Described> {
    let log = archive.log();
    // The CLI asks first and says when a seal was finished; here it only
    // has to be done.
    let _ = log.writable()?;
    let source = Source::parse(SOURCE)?;
    let time = Timestamp::now();
    let run = Run::new();
    let mut claims = Vec::new();
    let mut described = 0;
    let mut gone = Vec::new();
    for file in files {
        let reader = match archive.content().reader(&file.digest)? {
            Some(reader) => Some(reader),
            None => archive.derived().reader(&file.digest)?,
        };
        let Some(reader) = reader else {
            gone.push(file.digest.as_str().to_string());
            continue;
        };
        let mut observed = Observed::over(reader);
        std::io::copy(&mut observed, &mut std::io::sink()).map_err(|source| Error::Io {
            context: format!("{}: reading the stored file", file.digest),
            source,
        })?;
        let subject = Subject::parse(file.digest.as_str())?;
        let say = |attribute: &'static str, value| {
            Claim::assert(
                subject.clone(),
                known_attribute(attribute),
                value,
                time.clone(),
                source.clone(),
                run.clone(),
            )
        };
        if file.lacks_size {
            claims.push(say("file:size", json!(observed.length))?);
        }
        if file.lacks_mime {
            claims.push(say("file:mime", json!(observed.mime()))?);
        }
        described += 1;
    }
    log.append_all(&claims)?;
    Ok(Described {
        files: described,
        claims: claims.len(),
        run,
        gone,
    })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::accession::{Sighting, admit, record};
    use crate::claim::{Attribute, Value};
    use crate::index::Scope;
    use immure::Algorithm;

    fn archive(dir: &TempDir) -> Archive {
        Archive::create(dir.path().join("archive"), Algorithm::Sha256).unwrap()
    }

    /// One arrival of `bytes` at `path`, as ingest records it.
    fn arrive(archive: &Archive, bytes: &[u8], path: &str) -> Subject {
        let admitted = admit(archive.content(), bytes).unwrap();
        let facts = [(Attribute::parse("file:path").unwrap(), json!(path))];
        record(
            archive.log(),
            &admitted,
            &Sighting {
                source: &Source::parse("ingest").unwrap(),
                run: &Run::new(),
                mime: None,
                facts: &facts,
                tags: &[],
                time: None,
            },
        )
        .unwrap();
        admitted.subject().clone()
    }

    fn found(archive: &Archive) -> Vec<Undescribed> {
        match undescribed(archive).unwrap() {
            Undescribing::Found(files) => files,
            Undescribing::Refused(log) => panic!("refused: {log:?}"),
        }
    }

    fn standing(archive: &Archive, subject: &Subject, attribute: &str) -> Vec<Value> {
        let mut index = archive.index().unwrap();
        index.fold(archive.log()).unwrap();
        index
            .values(subject, &Attribute::parse(attribute).unwrap(), Scope::Held)
            .unwrap()
    }

    #[test]
    fn a_file_stored_by_one_run_and_recorded_by_the_next_gets_size_and_type() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        arrive(&archive, b"whole from the start", "/home/john/whole.txt");
        // The run that stored these bytes stopped before it recorded
        // them; the next one finds them in the store.
        admit(archive.content(), &b"stored, then met again"[..]).unwrap();
        let late = arrive(&archive, b"stored, then met again", "/home/john/late.txt");
        assert!(standing(&archive, &late, "file:size").is_empty());

        let files = found(&archive);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].digest.as_str(), late.as_str());
        let described = describe(&archive, &files).unwrap();

        assert_eq!((described.files, described.claims), (1, 2));
        assert!(described.gone.is_empty());
        assert_eq!(standing(&archive, &late, "file:size"), [json!(22)]);
        assert_eq!(
            standing(&archive, &late, "file:mime"),
            [json!("text/plain")]
        );
        assert!(found(&archive).is_empty(), "nothing is left to describe");
    }

    #[test]
    fn only_what_is_missing_is_recorded() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        let (_, entry) = archive.derived().add(b"%PDF-1.4 derived").unwrap();
        let subject = Subject::parse(entry.digest().as_str()).unwrap();
        archive
            .log()
            .append(
                &Claim::assert(
                    subject.clone(),
                    Attribute::parse("file:mime").unwrap(),
                    json!("text/x-announced"),
                    Timestamp::now(),
                    Source::parse("extractor:mail/1").unwrap(),
                    Run::new(),
                )
                .unwrap(),
            )
            .unwrap();

        let described = describe(&archive, &found(&archive)).unwrap();

        assert_eq!(described.claims, 1, "the size alone");
        assert_eq!(standing(&archive, &subject, "file:size"), [json!(16)]);
        assert_eq!(
            standing(&archive, &subject, "file:mime"),
            [json!("text/x-announced")],
            "the type the extractor announced is not detected over"
        );
    }

    #[test]
    fn a_log_with_findings_is_not_concluded_from() {
        let dir = TempDir::new().unwrap();
        let archive = archive(&dir);
        arrive(&archive, b"one", "/home/john/one.txt");
        let segment = archive.log().seal().unwrap().unwrap();
        let path = archive
            .log()
            .store()
            .find(segment.digest())
            .unwrap()
            .unwrap();
        std::fs::remove_file(path).unwrap();

        assert!(matches!(
            undescribed(&archive).unwrap(),
            Undescribing::Refused(_)
        ));
    }
}
