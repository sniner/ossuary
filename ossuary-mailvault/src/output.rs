//! The run narrating itself. Narration goes to stderr and falls silent
//! under `--quiet`; the verdict goes to stdout and survives redirection.

use std::fmt::Display;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// The progress line on the terminal at the moment, if any. A line
/// printed meanwhile (a retry inside a fetch) clears it first and draws
/// it again below; otherwise the line is appended to the count. Process
/// wide, like stderr itself.
static SHOWN: Mutex<Option<String>> = Mutex::new(None);

fn shown() -> MutexGuard<'static, Option<String>> {
    SHOWN.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Writes a progress line over whatever the current line holds.
fn draw(text: &str) {
    use std::io::Write as _;
    eprint!("\r\x1b[2K{text}");
    let _ = std::io::stderr().flush();
}

#[derive(Clone, Copy)]
pub struct Say {
    quiet: bool,
}

impl Say {
    pub fn new(quiet: bool) -> Self {
        Self { quiet }
    }

    pub fn line(self, text: impl Display) {
        if self.quiet {
            return;
        }
        let shown = shown();
        match shown.as_deref() {
            Some(progress) => {
                eprintln!("\r\x1b[2K{text}");
                draw(progress);
            }
            None => eprintln!("{text}"),
        }
    }

    /// A progress line for long work: on a terminal it overwrites
    /// itself, into a log it speaks once per `SPARSE` steps — a
    /// redirected run stays readable, an attended one visibly moves.
    pub fn progress(self) -> Progress {
        use std::io::IsTerminal;
        Progress {
            active: !self.quiet,
            tty: std::io::stderr().is_terminal(),
            dirty: false,
        }
    }
}

pub struct Progress {
    active: bool,
    tty: bool,
    dirty: bool,
}

const SPARSE: usize = 500;

impl Progress {
    pub fn update(&mut self, done: usize, text: &str) {
        if !self.active {
            return;
        }
        if self.tty {
            let mut shown = shown();
            draw(text);
            *shown = Some(text.to_owned());
            self.dirty = true;
        } else if done % SPARSE == 0 {
            eprintln!("{text}");
        }
    }

    /// Ends the self-overwriting line so the next one starts fresh.
    pub fn finish(&mut self) {
        if self.dirty {
            *shown() = None;
            eprintln!();
            self.dirty = false;
        }
    }
}

impl Drop for Progress {
    // An error path leaves mid-line otherwise, and whatever is said next
    // would land on top of the count.
    fn drop(&mut self) {
        self.finish();
    }
}

/// `1 message`, `2 messages` — a count that reads as a sentence.
pub fn counted(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}
