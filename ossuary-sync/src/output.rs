//! The run narrating itself. Narration goes to stderr and is silent
//! under `--quiet`; the verdict goes to stdout.

use std::fmt::Display;
use std::io::{IsTerminal as _, Write as _};

/// Up to this many names are listed without `--verbose`.
const HANDFUL: usize = 5;

/// A redirected run prints a progress line once per this many steps.
const SPARSE: usize = 500;

#[derive(Clone, Copy)]
pub struct Say {
    quiet: bool,
}

impl Say {
    pub fn new(quiet: bool) -> Self {
        Say { quiet }
    }

    pub fn line(self, text: impl Display) {
        if !self.quiet {
            eprintln!("{text}");
        }
    }

    /// A progress line for long work: on a terminal it overwrites
    /// itself, into a log it is printed once per [`SPARSE`] steps.
    pub fn progress(self) -> Progress {
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

impl Progress {
    pub fn update(&mut self, done: usize, text: &str) {
        if !self.active {
            return;
        }
        if self.tty {
            eprint!("\r\x1b[2K{text}");
            std::io::stderr().flush().ok();
            self.dirty = true;
        } else if done.is_multiple_of(SPARSE) {
            eprintln!("{text}");
        }
    }

    /// End the line that overwrites itself, so the next one starts on a
    /// line of its own.
    pub fn finish(&mut self) {
        if self.dirty {
            eprintln!();
            self.dirty = false;
        }
    }
}

impl Drop for Progress {
    // An error leaves the line half written otherwise, and the message
    // would be printed on top of the count.
    fn drop(&mut self) {
        self.finish();
    }
}

/// A heading and the names under it: all of them with `verbose` or when
/// there are only a few, otherwise the heading alone with a hint.
pub fn listing(heading: &str, names: &[String], verbose: bool) {
    if verbose || names.len() <= HANDFUL {
        println!("{heading}:");
        for name in names {
            println!("  {name}");
        }
    } else {
        println!("{heading}; --verbose lists them");
    }
}

/// A byte count the way a human sizes one: binary steps, one decimal
/// under ten.
pub fn human_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "display only, a tenth of a unit is the finest this prints"
    )]
    let mut size = bytes as f64;
    let mut unit = "B";
    for step in ["KiB", "MiB", "GiB", "TiB", "PiB"] {
        size /= 1024.0;
        unit = step;
        if size < 1024.0 {
            break;
        }
    }
    if size < 10.0 {
        format!("{size:.1} {unit}")
    } else {
        format!("{size:.0} {unit}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_read_the_way_a_human_sizes_them() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(4096), "4.0 KiB");
        assert_eq!(human_bytes(3_971_934_208), "3.7 GiB");
    }
}
