//! The extractor protocol, as this side speaks it.
//!
//! `docs/extractors.md` is the protocol. An extractor is a program that
//! answers `--identify` with one line of JSON per contract, and for one
//! file on its stdin answers with findings as lines of JSON on stdout.
//! This module starts the program, hands the bytes over and reads both
//! answers strictly. It knows nothing of the archive: which files are
//! examined, and what is recorded about them, is decided in `examine`
//! and `extract`.

use std::collections::HashSet;
use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::claim::{Attribute, Value};
use crate::error::{Error, Result};
use std::path::PathBuf;

/// One line of the answer to `--identify` — one contract. Unknown keys
/// are tolerated on purpose: the protocol number is the gate, and it is
/// checked before anything else is believed.
#[derive(Deserialize)]
pub(crate) struct Identity {
    #[serde(rename = "ossuary-extractor")]
    pub(crate) protocol: u32,
    /// The contract's name, when the program carries several — passed
    /// back as the first argument so the program knows which of its
    /// trades is meant. A single-line answer may leave it out.
    #[serde(default)]
    pub(crate) contract: Option<String>,
    pub(crate) source: String,
    pub(crate) mimes: Vec<String>,
    #[serde(default)]
    pub(crate) derives: bool,
}

/// One answer line, in any of its three shapes — a finding about the
/// examined file, the announcement of a derived file, a finding about
/// one. Strict: protocol 1 says these fields, and a misspelled key must
/// not pass as an empty answer.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Line {
    file: Option<String>,
    mime: Option<String>,
    attribute: Option<String>,
    value: Option<Value>,
}

/// The grammar an extractor's name and a contract's name share: what
/// fits after `ossuary-extract-` fits behind the colon.
pub(crate) fn well_formed(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Who is this extractor, and what does it read? One line per
/// contract: a single line may leave the contract unnamed — the
/// program of one trade, spoken to as it always was — while several
/// lines must each name theirs, no name twice.
pub(crate) fn identify(program: &str) -> Result<Vec<Identity>> {
    let output = Command::new(program)
        .arg("--identify")
        .output()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::Extract(format!(
                    "`{program}` not found on PATH; install it, then run again"
                ))
            } else {
                Error::Io {
                    context: format!("running `{program} --identify`"),
                    source: error,
                }
            }
        })?;
    if !output.status.success() {
        // The extractor's stderr is its own account of what is wrong —
        // "no pdftotext on PATH" beats "it failed".
        let complaint = String::from_utf8_lossy(&output.stderr);
        let complaint = complaint.trim();
        if !complaint.is_empty() {
            return Err(Error::Extract(format!(
                "`{program} --identify` failed: {complaint}"
            )));
        }
        return Err(Error::Extract(format!(
            "`{program} --identify` failed; nothing was examined"
        )));
    }
    read_identities(program, &String::from_utf8_lossy(&output.stdout))
}

/// The identify answer read whole and held to its rules — apart from
/// running the program, the whole of `identify`.
fn read_identities(program: &str, answer: &str) -> Result<Vec<Identity>> {
    let mut identities = Vec::new();
    for line in answer.lines().filter(|line| !line.trim().is_empty()) {
        let identity: Identity = serde_json::from_str(line.trim()).map_err(|error| {
            Error::Extract(format!(
                "`{program} --identify` output does not follow the extractor protocol: {error}"
            ))
        })?;
        if identity.protocol != 1 {
            return Err(Error::Extract(format!(
                "`{program}` uses extractor protocol {}, this version supports 1; upgrade ossuary",
                identity.protocol
            )));
        }
        if let Some(contract) = &identity.contract
            && !well_formed(contract)
        {
            return Err(Error::Extract(format!(
                "`{program}` offers a contract named {contract:?}; a contract name may contain only lowercase letters, digits and dashes"
            )));
        }
        identities.push(identity);
    }
    if identities.is_empty() {
        return Err(Error::Extract(format!(
            "`{program} --identify` printed nothing; an extractor must print at least one identify line"
        )));
    }
    if identities.len() > 1 {
        let mut seen = HashSet::new();
        for identity in &identities {
            let Some(contract) = &identity.contract else {
                return Err(Error::Extract(format!(
                    "`{program}` offers {} contracts and one of them has no name; with several contracts, each must be named",
                    identities.len()
                )));
            };
            if !seen.insert(contract.as_str()) {
                return Err(Error::Extract(format!(
                    "`{program}` offers the contract {contract:?} twice"
                )));
            }
        }
    }
    Ok(identities)
}

/// Why one file's examination did not happen, by what the run does next.
#[derive(Debug)]
pub(crate) enum Failure {
    /// Trouble with this file or with the extractor's answer about it:
    /// reported, and the run goes on with the next file.
    Skip(Error),
    /// Anything else, writing to the archive above all: the run stops,
    /// because the next file would meet the same trouble.
    Stop(Error),
}

impl From<Error> for Failure {
    /// An error nobody classified ends the run: carrying on past trouble
    /// that was not understood is what must never happen silently.
    fn from(error: Error) -> Self {
        Failure::Stop(error)
    }
}

impl From<immure::Error> for Failure {
    fn from(error: immure::Error) -> Self {
        Failure::Stop(error.into())
    }
}

/// One file through an extractor: `program` started for `contract`, with
/// `scratch` as the directory for derived files when the contract
/// derives, the bytes of `reader` on its stdin, and its answer sorted
/// into a [`Harvest`]. Answers the harvest and what the extractor said
/// on stderr. A contract that was announced by name is named back as the
/// first argument, the directory follows.
pub(crate) fn exchange(
    program: &str,
    contract: Option<&str>,
    scratch: Option<&Path>,
    reader: &mut impl Read,
) -> std::result::Result<(Harvest, String), Failure> {
    let mut command = Command::new(program);
    if let Some(contract) = contract {
        command.arg(contract);
    }
    if let Some(scratch) = scratch {
        command.arg(scratch);
    }
    let (answer, remarks) = converse(&mut command, program, reader)?;
    let harvest = harvest(program, &answer, scratch).map_err(Failure::Skip)?;
    Ok((harvest, remarks))
}

/// One exchange with an extractor: the bytes in, its answer and its
/// remarks out. stdout is the answer; stderr is the extractor's to
/// narrate or complain on, and comes back with the program's own name
/// stripped from the front of each line, since the caller will say
/// which file was meant and the program's name would only stand in
/// the way. A non-zero exit is a refusal, and what stderr said is the
/// reason. Exit 0 before the whole file was read is an error as well:
/// the protocol has the extractor read to the end before it answers.
/// What goes wrong between the two ends of the pipes is this file's
/// trouble; a program that cannot be started or waited for stops the run.
fn converse(
    command: &mut Command,
    program: &str,
    reader: &mut impl Read,
) -> std::result::Result<(String, String), Failure> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| Error::Io {
            context: format!("running `{program}`"),
            source: error,
        })?;
    let mut stdin = child.stdin.take().expect("stdin was piped");
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let mut stderr = child.stderr.take().expect("stderr was piped");
    // stdout and stderr are read while the file is still being written:
    // an extractor that writes before it has read to the end, against
    // the protocol, would otherwise block on a full pipe while this side
    // blocks on its stdin.
    let (handed, answer, remarks) = std::thread::scope(|scope| {
        let answer = scope.spawn(move || drain(&mut stdout));
        let remarks = scope.spawn(move || drain(&mut stderr));
        let handed = hand_over(reader, &mut stdin);
        if matches!(handed, Err(Shortfall::Read(_) | Shortfall::Write(_))) {
            // The extractor must not take the bytes so far for the whole
            // file. A failed kill changes nothing: the examination fails
            // either way, and the wait below reaps the process.
            let _ = child.kill();
        }
        // Closing stdin is the extractor's end-of-file.
        drop(stdin);
        (
            handed,
            answer.join().expect("reading a pipe does not panic"),
            remarks.join().expect("reading a pipe does not panic"),
        )
    });
    let status = child.wait().map_err(|error| Error::Io {
        context: format!("waiting for `{program}` to exit"),
        source: error,
    })?;
    let closed = match handed {
        Ok(()) => false,
        Err(Shortfall::Closed) => true,
        Err(Shortfall::Read(error)) => {
            return Err(Failure::Skip(Error::Io {
                context: "reading the file from the archive".to_string(),
                source: error,
            }));
        }
        Err(Shortfall::Write(error)) => {
            return Err(Failure::Skip(Error::Io {
                context: "writing the file to the extractor".to_string(),
                source: error,
            }));
        }
    };
    let read_error = |error| {
        Failure::Skip(Error::Io {
            context: "reading the extractor's output".to_string(),
            source: error,
        })
    };
    let answer = answer.map_err(read_error)?;
    let remarks = unprefixed(
        program,
        &String::from_utf8_lossy(&remarks.map_err(read_error)?),
    );
    if !status.success() {
        // The reason on one line, as a failure list wants it.
        let complaint = remarks.lines().collect::<Vec<_>>().join("; ");
        return Err(Failure::Skip(Error::Extract(if complaint.is_empty() {
            format!("`{program}` failed ({status})")
        } else {
            format!("`{program}` failed ({status}): {complaint}")
        })));
    }
    if closed {
        // Exit 0 says the file was examined, but part of it was never
        // read.
        return Err(Failure::Skip(Error::Extract(format!(
            "`{program}` exited before it had read the whole file"
        ))));
    }
    Ok((String::from_utf8_lossy(&answer).into_owned(), remarks))
}

/// Why the file did not reach the extractor whole.
enum Shortfall {
    /// The extractor closed its stdin before the end.
    Closed,
    /// The file could not be read from the archive.
    Read(std::io::Error),
    /// Writing into the pipe failed for another reason.
    Write(std::io::Error),
}

/// Stream the file into the extractor's stdin, so that it is never
/// held whole in memory. Unlike `io::copy`, this keeps a failed read
/// from the store apart from a failed write into the pipe.
fn hand_over(reader: &mut impl Read, stdin: &mut impl Write) -> std::result::Result<(), Shortfall> {
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => read,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(Shortfall::Read(error)),
        };
        match stdin.write_all(&buffer[..read]) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::BrokenPipe => {
                return Err(Shortfall::Closed);
            }
            Err(error) => return Err(Shortfall::Write(error)),
        }
    }
}

/// Everything a pipe gives until the other end closes it.
fn drain(pipe: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// The lines of an extractor's stderr without `program: ` in front,
/// where a line began with it; blank lines dropped.
fn unprefixed(program: &str, stderr: &str) -> String {
    let prefix = format!("{program}: ");
    stderr
        .lines()
        .map(|line| line.strip_prefix(prefix.as_str()).unwrap_or(line))
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// One derived file, as the extractor announced it: the bare name and the
/// MIME type in the extractor's own words, where the bytes wait, and
/// whatever else it said about this file rather than the examined one.
#[derive(Debug)]
pub(crate) struct Derivation {
    /// The announced name: the file's handle in the scratch directory,
    /// for the lines that speak about it and for the tally. The record
    /// never learns it. A name the content was known by is a finding
    /// the extractor says as `file:name`, when there is one.
    pub(crate) name: String,
    /// The announced MIME type, recorded as `file:mime`. For bytes new
    /// to the archive, the type detected from the content is recorded
    /// beside it when the two differ.
    pub(crate) mime: String,
    /// Where the bytes wait to be taken in. Read here, never removed:
    /// the directory is the caller's to clean up.
    pub(crate) path: PathBuf,
    /// Findings about this file, already through [`Attribute::parse`].
    pub(crate) findings: Vec<(Attribute, Value)>,
}

/// An answer sorted: what stands on the examined file, and what became
/// files of its own.
pub(crate) struct Harvest {
    pub(crate) findings: Vec<(Attribute, Value)>,
    pub(crate) derived: Vec<Derivation>,
}

/// The whole answer sorted into findings and derived files — or refused
/// whole. The stream is read to its end before anything is judged, so
/// the order of its lines never matters: announcing a file after
/// speaking about it is legal.
fn harvest(program: &str, answer: &str, scratch: Option<&Path>) -> Result<Harvest> {
    let mut findings = Vec::new();
    let mut derived: Vec<Derivation> = Vec::new();
    let mut announced: HashSet<String> = HashSet::new();
    let mut spoken: Vec<(String, Attribute, Value)> = Vec::new();
    for line in answer.lines().filter(|line| !line.trim().is_empty()) {
        let parsed: Line = serde_json::from_str(line).map_err(|error| {
            Error::Extract(format!(
                "`{program}` printed a line that does not follow the extractor protocol: {line:?}: {error}"
            ))
        })?;
        match parsed {
            Line {
                file: Some(name),
                mime: Some(mime),
                attribute: None,
                value: None,
            } => {
                let Some(scratch) = scratch else {
                    return Err(Error::Extract(format!(
                        "`{program}` announced the derived file {name:?}, but its identify line does not declare `derives`"
                    )));
                };
                if !bare(&name) {
                    return Err(Error::Extract(format!(
                        "`{program}` announced {name:?}; a derived file name must not contain a path"
                    )));
                }
                if !announced.insert(name.clone()) {
                    return Err(Error::Extract(format!(
                        "`{program}` announced {name:?} twice"
                    )));
                }
                let path = scratch.join(&name);
                if !path.is_file() {
                    return Err(Error::Extract(format!(
                        "`{program}` announced {name:?} but did not write it"
                    )));
                }
                derived.push(Derivation {
                    name,
                    mime,
                    path,
                    findings: Vec::new(),
                });
            }
            Line {
                file: None,
                mime: None,
                attribute: Some(attribute),
                value: Some(value),
            } => findings.push((Attribute::parse(&attribute)?, value)),
            Line {
                file: Some(name),
                mime: None,
                attribute: Some(attribute),
                value: Some(value),
            } => spoken.push((name, Attribute::parse(&attribute)?, value)),
            _ => {
                return Err(Error::Extract(format!(
                    "`{program}` printed a line that does not follow the extractor protocol: {line:?}"
                )));
            }
        }
    }
    for (name, attribute, value) in spoken {
        let Some(derivation) = derived
            .iter_mut()
            .find(|derivation| derivation.name == name)
        else {
            return Err(Error::Extract(format!(
                "`{program}` reported attributes for {name:?}, a file it did not announce"
            )));
        };
        derivation.findings.push((attribute, value));
    }
    Ok(Harvest { findings, derived })
}

/// A derived file's announced name: the key the stream and the directory
/// share. Bare means it names a file, not a place — nothing empty, no
/// dot-directories, no separators of either persuasion.
fn bare(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remarks_lose_the_program_prefix_and_blank_lines() {
        assert_eq!(
            unprefixed(
                "ossuary-extract-pdf",
                "ossuary-extract-pdf: mostly not text, discarded\n\nsomething else\n"
            ),
            "mostly not text, discarded\nsomething else"
        );
        assert_eq!(unprefixed("x", ""), "");
    }

    #[test]
    fn a_conversation_keeps_answer_and_remarks_apart() {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "cat > /dev/null; echo '{\"attribute\":\"a:b\",\"value\":1}'; echo 'sh: a remark' >&2",
        ]);
        let mut bytes: &[u8] = b"the file";
        let (answer, remarks) = converse(&mut command, "sh", &mut bytes).unwrap();
        assert_eq!(answer.trim(), "{\"attribute\":\"a:b\",\"value\":1}");
        assert_eq!(remarks, "a remark");
    }

    /// The error of a failure the run goes on after; a failure that stops
    /// the run fails the test.
    fn skipped(failure: Failure) -> Error {
        match failure {
            Failure::Skip(error) => error,
            Failure::Stop(error) => panic!("the run stopped: {}", error.spelled()),
        }
    }

    #[test]
    fn a_program_that_cannot_be_started_stops_the_run() {
        let mut command = Command::new("/nonexistent/ossuary-extract-gone");
        let mut bytes: &[u8] = b"the file";
        let failure = converse(&mut command, "ossuary-extract-gone", &mut bytes).unwrap_err();
        assert!(
            matches!(&failure, Failure::Stop(error) if error.spelled().starts_with("running `ossuary-extract-gone`")),
            "{failure:?}"
        );
    }

    #[test]
    fn a_null_value_is_outside_the_protocol() {
        let error = harvest("x", "{\"attribute\":\"a:b\",\"value\":null}", None)
            .err()
            .unwrap();
        assert!(
            error
                .spelled()
                .contains("does not follow the extractor protocol"),
            "{}",
            error.spelled()
        );
    }

    #[test]
    fn a_refusal_carries_what_stderr_said() {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "cat > /dev/null; echo 'sh: cannot read this' >&2; exit 3",
        ]);
        let mut bytes: &[u8] = b"the file";
        let error = skipped(converse(&mut command, "sh", &mut bytes).unwrap_err());
        let spelled = error.to_string();
        assert!(
            spelled.contains("`sh` failed") && spelled.contains("cannot read this"),
            "{spelled}"
        );
    }

    /// Larger than a pipe's buffer, so that an extractor which stops
    /// reading leaves the rest unwritten.
    fn a_large_file() -> Vec<u8> {
        vec![b'x'; 1024 * 1024]
    }

    #[test]
    fn an_extractor_that_stops_reading_gives_its_own_reason() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "echo 'sh: no such contract' >&2; exit 2"]);
        let error =
            skipped(converse(&mut command, "sh", &mut a_large_file().as_slice()).unwrap_err());
        let spelled = error.spelled();
        assert!(
            spelled.contains("`sh` failed") && spelled.contains("no such contract"),
            "{spelled}"
        );
    }

    #[test]
    fn exit_0_before_the_end_of_the_file_is_an_error() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 0"]);
        let error =
            skipped(converse(&mut command, "sh", &mut a_large_file().as_slice()).unwrap_err());
        assert_eq!(
            error.spelled(),
            "`sh` exited before it had read the whole file"
        );
    }

    #[test]
    fn an_extractor_that_writes_before_it_reads_does_not_block() {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "head -c 300000 /dev/zero | tr '\\0' x >&2; cat > /dev/null",
        ]);
        let (answer, remarks) =
            converse(&mut command, "sh", &mut a_large_file().as_slice()).unwrap();
        assert_eq!(answer, "");
        assert_eq!(remarks.len(), 300_000);
    }

    #[test]
    fn a_file_the_archive_cannot_read_fails_as_a_read() {
        /// Gives 100 000 bytes, then fails.
        struct Damaged(usize);
        impl Read for Damaged {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.0 >= 100_000 {
                    return Err(std::io::Error::other("frame damaged"));
                }
                let given = buffer.len().min(100_000 - self.0);
                buffer[..given].fill(b'x');
                self.0 += given;
                Ok(given)
            }
        }
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "cat > /dev/null; echo '{\"attribute\":\"a:b\",\"value\":1}'",
        ]);
        let error = skipped(converse(&mut command, "sh", &mut Damaged(0)).unwrap_err());
        assert_eq!(
            error.spelled(),
            "reading the file from the archive: frame damaged"
        );
    }

    #[test]
    fn a_bare_name_names_a_file_and_nothing_else() {
        assert!(bare("report.pdf"));
        assert!(bare("no extension"));
        assert!(!bare(""));
        assert!(!bare("."));
        assert!(!bare(".."));
        assert!(!bare("sub/report.pdf"));
        assert!(!bare("..\\report.pdf"));
    }

    #[test]
    fn the_three_line_shapes_sort_themselves() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("report.pdf"), b"%PDF").unwrap();
        let answer = concat!(
            "{\"attribute\":\"mail:subject\",\"value\":\"Re: the plan\"}\n",
            "{\"file\":\"report.pdf\",\"attribute\":\"mail:content-id\",\"value\":\"<p2@example.com>\"}\n",
            "{\"file\":\"report.pdf\",\"mime\":\"application/pdf\"}\n",
        );

        let Harvest { findings, derived } = harvest("x", answer, Some(dir.path())).unwrap();

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].0.as_str(), "mail:subject");
        assert_eq!(derived.len(), 1);
        assert_eq!(derived[0].name, "report.pdf");
        assert_eq!(derived[0].mime, "application/pdf");
        assert_eq!(
            derived[0].findings.len(),
            1,
            "spoken about before it was announced, and sorted out all the same"
        );
    }

    #[test]
    fn an_answer_that_does_not_add_up_is_refused_whole() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("real.txt"), b"present").unwrap();
        let refused = [
            // A shape the protocol does not know.
            "{\"file\":\"real.txt\",\"mime\":\"text/plain\",\"value\":1}",
            // Announced, but never written.
            "{\"file\":\"ghost.txt\",\"mime\":\"text/plain\"}",
            // Spoken about, but never announced.
            "{\"file\":\"real.txt\",\"attribute\":\"a:b\",\"value\":1}",
            // A path where a bare name belongs.
            "{\"file\":\"sub/real.txt\",\"mime\":\"text/plain\"}",
            // Announced twice.
            "{\"file\":\"real.txt\",\"mime\":\"text/plain\"}\n{\"file\":\"real.txt\",\"mime\":\"text/plain\"}",
        ];
        for answer in refused {
            assert!(
                harvest("x", answer, Some(dir.path())).is_err(),
                "{answer} should have been refused"
            );
        }
    }

    #[test]
    fn without_a_directory_an_announcement_is_a_breach() {
        assert!(harvest("x", "{\"file\":\"a.txt\",\"mime\":\"text/plain\"}", None).is_err());
    }

    #[test]
    fn one_identify_line_may_stay_nameless_but_several_may_not() {
        let single =
            r#"{"ossuary-extractor":1,"source":"extractor:exif/0.1.0","mimes":["image/jpeg"]}"#;
        let read = read_identities("x", single).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].contract, None);

        let pair = concat!(
            r#"{"ossuary-extractor":1,"contract":"list","source":"extractor:packed-list/0.1.0","mimes":["application/zip"]}"#,
            "\n",
            r#"{"ossuary-extractor":1,"contract":"unpack","source":"extractor:packed-unpack/0.1.0","mimes":["application/zip"],"derives":true}"#,
            "\n",
        );
        let read = read_identities("x", pair).unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].contract.as_deref(), Some("list"));
        assert!(read[1].derives);

        let refused = [
            // Nothing at all.
            "",
            // Two lines, one nameless.
            concat!(
                r#"{"ossuary-extractor":1,"contract":"list","source":"extractor:a/1","mimes":[]}"#,
                "\n",
                r#"{"ossuary-extractor":1,"source":"extractor:b/1","mimes":[]}"#,
            ),
            // The same contract twice.
            concat!(
                r#"{"ossuary-extractor":1,"contract":"list","source":"extractor:a/1","mimes":[]}"#,
                "\n",
                r#"{"ossuary-extractor":1,"contract":"list","source":"extractor:b/1","mimes":[]}"#,
            ),
            // A contract outside the name grammar.
            r#"{"ossuary-extractor":1,"contract":"List","source":"extractor:a/1","mimes":[]}"#,
            // A protocol this build does not speak.
            r#"{"ossuary-extractor":2,"contract":"list","source":"extractor:a/1","mimes":[]}"#,
        ];
        for answer in refused {
            assert!(
                read_identities("x", answer).is_err(),
                "{answer:?} should be refused"
            );
        }
    }
}
