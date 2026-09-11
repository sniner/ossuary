//! The mailboxes: `mailvault.toml` in the archive root.
//!
//! Its own file, never a section of `config.toml` — that one is read
//! strictly by every ossuary command, and a section it does not know
//! would refuse them all. This file is read just as strictly, by this
//! program alone: a key it does not know is a typo or a newer knob,
//! and half a mailbox is worse than none.
//!
//! It lives beside the archive it fills for the same reason
//! `config.toml` does: the two cannot drift apart, and a backup of the
//! archive carries the recipe. That puts a mailbox's login where the
//! archive lies — the password itself belongs in a password manager,
//! reached through `password_cmd`.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, anyhow, bail};
use serde::Deserialize;

/// The file's name in the archive root.
pub const FILE_NAME: &str = "mailvault.toml";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default, rename = "account")]
    pub accounts: Vec<Account>,
}

/// One mailbox and how to reach it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
    /// What the record calls this mailbox in every sighting, and the
    /// key its resume points are kept under.
    pub name: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Off only for a local bridge that speaks plaintext IMAP on
    /// loopback; a real server gets TLS, always.
    #[serde(default = "default_tls")]
    pub tls: bool,
    pub user: String,
    pub password: Option<String>,
    /// A shell command that prints the password on its first line — so
    /// the secret can live in a password manager instead of this file.
    /// Runs only under `--allow-exec`.
    pub password_cmd: Option<String>,
    /// The folders to fetch; every folder the server offers when absent.
    pub folders: Option<Vec<String>>,
}

fn default_port() -> u16 {
    993
}

fn default_tls() -> bool {
    true
}

impl Config {
    /// Read the file at `root`, strictly.
    ///
    /// # Errors
    ///
    /// No file, a file that will not read, a key this build does not
    /// know, two accounts of one name, or a name that cannot stand on
    /// the record.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(FILE_NAME);
        let text = std::fs::read_to_string(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                anyhow!(
                    "{}: no {FILE_NAME} here — the mailboxes stand in the archive's own \
                     {FILE_NAME}, one [[account]] table each; see the README for the shape",
                    root.display()
                )
            } else {
                anyhow!("{}: {error}", path.display())
            }
        })?;
        let config: Config = toml::from_str(&text)
            .with_context(|| format!("{} could not be understood", path.display()))?;
        let mut names = std::collections::HashSet::new();
        for account in &config.accounts {
            if !valid_name(&account.name) {
                bail!(
                    "{}: {:?} cannot name a mailbox on the record — use letters, digits, \
                     '.', '_' and '-' only",
                    path.display(),
                    account.name
                );
            }
            if !names.insert(account.name.as_str()) {
                bail!(
                    "{}: two accounts named {:?} — every mailbox needs a name of its own",
                    path.display(),
                    account.name
                );
            }
        }
        Ok(config)
    }

    /// The accounts asked for, in the file's order — every one when
    /// nothing was named.
    ///
    /// # Errors
    ///
    /// A name the file does not know.
    pub fn chosen(&self, names: &[String]) -> Result<Vec<&Account>> {
        if names.is_empty() {
            return Ok(self.accounts.iter().collect());
        }
        for name in names {
            if !self.accounts.iter().any(|account| &account.name == name) {
                bail!(
                    "{name}: no such account in {FILE_NAME} — it knows {}",
                    self.accounts
                        .iter()
                        .map(|account| account.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        Ok(self
            .accounts
            .iter()
            .filter(|account| names.contains(&account.name))
            .collect())
    }
}

/// A name opens every `mailbox:place` value, and the first slash ends
/// it — so a name holds no slash, and nothing else that would make a
/// place hard to read back. The same rule holds for a mailbox taken
/// over from a vault.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

impl Account {
    /// The account's password: `password_cmd` first, `password` second.
    ///
    /// # Errors
    ///
    /// A command configured but not allowed, a command that fails or
    /// prints nothing, or no password at all.
    pub fn password(&self, allow_exec: bool) -> Result<String> {
        if let Some(cmd) = &self.password_cmd {
            if !allow_exec {
                bail!(
                    "{}: password_cmd stands in {FILE_NAME} and runs only under --allow-exec",
                    self.name
                );
            }
            // Only stdout is the command's answer. Its stderr and its
            // stdin stay with the terminal: a password manager asks for
            // a passphrase there, and says there what went wrong.
            let out = Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .stdin(Stdio::inherit())
                .stderr(Stdio::inherit())
                .output()
                .with_context(|| format!("{}: password_cmd could not be run", self.name))?;
            if !out.status.success() {
                bail!(
                    "{}: password_cmd failed ({}) — what it said stands above",
                    self.name,
                    out.status
                );
            }
            let stdout = String::from_utf8_lossy(&out.stdout);
            let password = stdout.lines().next().unwrap_or("").trim();
            if password.is_empty() {
                bail!(
                    "{}: password_cmd printed nothing — it has to print the password on its first line",
                    self.name
                );
            }
            return Ok(password.to_string());
        }
        if let Some(password) = &self.password {
            return Ok(password.clone());
        }
        bail!(
            "{}: no password configured — set password_cmd (a command that prints it) \
             or password in {FILE_NAME}",
            self.name
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(text: &str) -> Result<Config> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE_NAME), text).unwrap();
        Config::load(dir.path())
    }

    #[test]
    fn the_defaults_fill_in_and_the_file_is_read_strictly() {
        let config = load(
            "[[account]]\nname = \"example.org\"\nhost = \"imap.example.org\"\nuser = \"john\"\n",
        )
        .unwrap();
        let account = &config.accounts[0];
        assert_eq!((account.port, account.tls), (993, true));
        assert!(account.folders.is_none());

        assert!(
            load("[[account]]\nname = \"a\"\nhost = \"h\"\nuser = \"u\"\nserver = \"x\"\n")
                .is_err(),
            "a key this build does not know is refused"
        );
    }

    #[test]
    fn names_must_be_unique_and_fit_the_record() {
        let twice = "[[account]]\nname = \"a\"\nhost = \"h\"\nuser = \"u\"\n\
                     [[account]]\nname = \"a\"\nhost = \"h\"\nuser = \"u\"\n";
        assert!(load(twice).is_err());
        assert!(load("[[account]]\nname = \"a/b\"\nhost = \"h\"\nuser = \"u\"\n").is_err());
        assert!(load("[[account]]\nname = \"a b\"\nhost = \"h\"\nuser = \"u\"\n").is_err());
    }

    #[test]
    fn chosen_keeps_the_files_order_and_knows_every_name() {
        let config = load(
            "[[account]]\nname = \"a\"\nhost = \"h\"\nuser = \"u\"\n\
             [[account]]\nname = \"b\"\nhost = \"h\"\nuser = \"u\"\n",
        )
        .unwrap();
        let names: Vec<_> = config
            .chosen(&["b".to_string(), "a".to_string()])
            .unwrap()
            .iter()
            .map(|account| account.name.as_str())
            .collect();
        assert_eq!(names, ["a", "b"]);
        assert!(config.chosen(&["c".to_string()]).is_err());
    }

    #[test]
    fn a_password_command_runs_only_when_allowed() {
        let config = load(
            "[[account]]\nname = \"a\"\nhost = \"h\"\nuser = \"u\"\npassword_cmd = \"echo secret\"\n",
        )
        .unwrap();
        let account = &config.accounts[0];
        assert!(account.password(false).is_err());
        assert_eq!(account.password(true).unwrap(), "secret");
    }
}
