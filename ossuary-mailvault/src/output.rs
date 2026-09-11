//! The run narrating itself. Narration goes to stderr and falls silent
//! under `--quiet`; the verdict goes to stdout and survives redirection.

use std::fmt::Display;

#[derive(Clone, Copy)]
pub struct Say {
    quiet: bool,
}

impl Say {
    pub fn new(quiet: bool) -> Self {
        Self { quiet }
    }

    pub fn line(self, text: impl Display) {
        if !self.quiet {
            eprintln!("{text}");
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
        use std::io::Write as _;
        if !self.active {
            return;
        }
        if self.tty {
            eprint!("\r\x1b[2K{text}");
            let _ = std::io::stderr().flush();
            self.dirty = true;
        } else if done % SPARSE == 0 {
            eprintln!("{text}");
        }
    }

    /// Ends the self-overwriting line so the next one starts fresh.
    pub fn finish(&mut self) {
        if self.dirty {
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
