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
//! archive lies — the secrets themselves belong in a password manager,
//! and any key can be given as `KEY_cmd` instead: a command whose first
//! line is the value, run only under `--allow-exec`, and only when the
//! account is reached, so a command that fails costs that account and
//! nothing else.
//!
//! An account is reached one of two ways, and `backend` says which:
//! `imap`, the default, with a host and a password; or `msgraph`, a
//! Microsoft 365 mailbox read over MS Graph with an app registration
//! instead of a login. Each way has its own keys, and the keys of the
//! other are refused, not skipped.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, anyhow, bail};
use serde::Deserialize;

/// The file's name in the archive root.
pub const FILE_NAME: &str = "mailvault.toml";

/// What a `KEY_cmd` ends in.
const CMD: &str = "_cmd";

/// The file `init` writes: an example of each kind of account, every
/// one commented out, so the file as written fetches nothing.
pub const STARTER: &str = r##"# Mailboxes for `ossuary mailvault fetch`, one [[account]] table each.
# The examples below are commented out: remove the "# " in front of the
# lines of one and fill it in. Keys not shown here are rejected.
#
# Any key except name, backend and folders can be given as KEY_cmd
# instead: a command that prints the value on its first line, most
# often password_cmd. Commands run only with
# `ossuary mailvault fetch --allow-exec`.

# An IMAP mailbox with all keys.
#
# [[account]]
# name = "example.org"         # letters, digits, '.', '_' and '-'
# backend = "imap"             # the default
# host = "imap.example.org"
# port = 993                   # the default
# tls = true                   # the default; false only for a bridge on localhost
# user = "john@example.org"
# password_cmd = "pass show mail/example.org"   # or password = "..."
# folders = ["INBOX", "Sent"]  # all folders if omitted

# Gmail. All Mail contains every message; each label folder would fetch
# the same messages again. The folder name depends on the account's
# language, such as "[Google Mail]/Alle Nachrichten" on a German account.
# The password is an app password.
#
# [[account]]
# name = "gmail.com"
# host = "imap.gmail.com"
# user = "john.doe@gmail.com"
# password_cmd = "pass show mail/gmail"
# folders = ["[Gmail]/All Mail"]

# Proton Mail through Proton Bridge on this machine: plaintext IMAP on
# localhost. The password is the one Bridge shows, not the Proton password.
#
# [[account]]
# name = "proton.me"
# host = "127.0.0.1"
# port = 1143
# tls = false
# user = "john.doe@proton.me"
# password_cmd = "pass show mail/proton-bridge"
# folders = ["All Mail"]

# A Microsoft 365 mailbox, read over MS Graph. The login is an app
# registration in Azure with the Mail.Read permission; user is the
# address of the mailbox.
#
# [[account]]
# name = "m365"
# backend = "msgraph"
# tenant_id = "00000000-0000-0000-0000-000000000000"
# client_id = "11111111-1111-1111-1111-111111111111"
# client_secret_cmd = "pass show m365/client-secret"
# user = "john.doe@example.com"
# folders = ["Inbox", "Sent Items"]  # as Outlook shows them; "Inbox/Projects" for a subfolder
"##;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default, rename = "account")]
    pub accounts: Vec<Account>,
}

/// One mailbox and how to reach it.
#[derive(Debug)]
pub struct Account {
    /// What the record calls this mailbox in every sighting, and the
    /// key its resume points are kept under.
    pub name: String,
    /// The folders to fetch; every folder the server offers when absent.
    pub folders: Option<Vec<String>>,
    backend: Backend,
    /// The keys given outright.
    given: toml::Table,
    /// The keys given as `KEY_cmd`: the key, and the command whose
    /// first line is its value.
    commands: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy)]
enum Backend {
    Imap,
    Graph,
}

/// The two ways to a mailbox, each with the keys it takes — every
/// `KEY_cmd` run and its value filled in.
#[derive(Debug)]
pub enum Reach {
    /// `backend = "imap"`, or no `backend` at all.
    Imap(Imap),
    /// `backend = "msgraph"`.
    Graph(Graph),
}

/// An IMAP server and a login.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Imap {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Off only for a local bridge that speaks plaintext IMAP on
    /// loopback; a real server gets TLS, always.
    #[serde(default = "default_tls")]
    pub tls: bool,
    pub user: String,
    pub password: Option<String>,
}

/// A Microsoft 365 mailbox behind an app registration: the tenant, the
/// application and its secret are the login; `user` only says whose
/// mailbox to read.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Graph {
    pub tenant_id: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    /// The mailbox to read, as its address.
    pub user: String,
}

fn default_port() -> u16 {
    993
}

fn default_tls() -> bool {
    true
}

/// Whether the host is this machine itself — the one place plaintext
/// IMAP is not a password on the wire.
fn loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

impl<'de> Deserialize<'de> for Account {
    /// `backend` picks the keys the rest of the table is read with, so
    /// a key of the other way is refused by name rather than by a
    /// guess between two shapes. The `KEY_cmd` keys are set aside to
    /// run later, and the shape is tried at once with their places
    /// held, so a typo is refused here and not after the first login.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let mut table = toml::Table::deserialize(deserializer)?;
        let name = match table.remove("name") {
            Some(toml::Value::String(name)) => name,
            Some(_) => return Err(D::Error::custom("name must be a string")),
            None => return Err(D::Error::missing_field("name")),
        };
        let folders = table
            .remove("folders")
            .map(toml::Value::try_into::<Vec<String>>)
            .transpose()
            .map_err(|error| D::Error::custom(format!("{name}: folders: {error}")))?;
        let backend = match table.remove("backend") {
            None => Backend::Imap,
            Some(toml::Value::String(backend)) => match backend.as_str() {
                "imap" => Backend::Imap,
                "msgraph" => Backend::Graph,
                other => {
                    return Err(D::Error::custom(format!(
                        "{name}: unknown backend {other:?}; use imap or msgraph"
                    )));
                }
            },
            Some(_) => {
                return Err(D::Error::custom(format!(
                    "{name}: backend must be a string"
                )));
            }
        };
        let mut commands = BTreeMap::new();
        let commanded: Vec<String> = table
            .keys()
            .filter(|key| key.ends_with(CMD))
            .cloned()
            .collect();
        for key in commanded {
            let target = key[..key.len() - CMD.len()].to_string();
            if matches!(target.as_str(), "name" | "backend" | "folders") {
                return Err(D::Error::custom(format!(
                    "{name}: {key} is not supported; set {target} directly"
                )));
            }
            match table.remove(&key) {
                Some(toml::Value::String(cmd)) => {
                    commands.insert(target, cmd);
                }
                _ => {
                    return Err(D::Error::custom(format!(
                        "{name}: {key} must be a string (the command to run)"
                    )));
                }
            }
        }
        let account = Self {
            name,
            folders,
            backend,
            given: table,
            commands,
        };
        account
            .build(|_key| Ok(String::new()))
            .map_err(|error| D::Error::custom(format!("{error:#}")))?;
        Ok(account)
    }
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
                    "{}: no {FILE_NAME}; create one with `ossuary mailvault init`",
                    root.display()
                )
            } else {
                anyhow!("{}: {error}", path.display())
            }
        })?;
        let config: Config = toml::from_str(&text).with_context(|| path.display().to_string())?;
        let mut names = std::collections::HashSet::new();
        for account in &config.accounts {
            if !valid_name(&account.name) {
                bail!(
                    "{}: invalid account name {:?}; use only letters, digits, '.', '_' \
                     and '-'",
                    path.display(),
                    account.name
                );
            }
            if !names.insert(account.name.as_str()) {
                bail!(
                    "{}: two accounts named {:?}; account names must be unique",
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
                    "{name}: no such account in {FILE_NAME}; known accounts: {}",
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

/// Write [`STARTER`] at `root`. A `mailvault.toml` already there is left
/// as it is; answers whether the file was written.
///
/// # Errors
///
/// The file could not be written.
pub fn begin(root: &Path) -> Result<bool> {
    let path = root.join(FILE_NAME);
    if path.exists() {
        return Ok(false);
    }
    std::fs::write(&path, STARTER).with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
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
    /// Whether the mailbox is reached over MS Graph.
    #[must_use]
    pub fn over_graph(&self) -> bool {
        matches!(self.backend, Backend::Graph)
    }

    /// The way to the mailbox, every `KEY_cmd` run and its value filled
    /// in. A command's value wins over a key given outright.
    ///
    /// # Errors
    ///
    /// A command configured but not allowed, a command that fails or
    /// prints nothing, or plaintext IMAP to anything but this machine.
    pub fn reach(&self, allow_exec: bool) -> Result<Reach> {
        let reach = self.build(|key| self.run(key, allow_exec))?;
        if let Reach::Imap(imap) = &reach {
            if !imap.tls && !loopback(&imap.host) {
                bail!(
                    "{}: tls = false is allowed only for localhost, not for {}; remove tls = false",
                    self.name,
                    imap.host
                );
            }
        }
        Ok(reach)
    }

    /// The way to the mailbox with the commanded keys valued by
    /// `value`, read strictly for the backend's shape.
    fn build(&self, value: impl Fn(&str) -> Result<String>) -> Result<Reach> {
        let mut table = self.given.clone();
        for key in self.commands.keys() {
            table.insert(key.clone(), toml::Value::String(value(key)?));
        }
        let rest = toml::Value::Table(table);
        Ok(match self.backend {
            Backend::Imap => Reach::Imap(
                rest.try_into()
                    .map_err(|error| anyhow!("{}: {error}", self.name))?,
            ),
            Backend::Graph => Reach::Graph(
                rest.try_into()
                    .map_err(|error| anyhow!("{}: {error}", self.name))?,
            ),
        })
    }

    /// The value of `key`: the first line the `KEY_cmd` command prints.
    fn run(&self, key: &str, allow_exec: bool) -> Result<String> {
        let cmd = &self.commands[key];
        let name = &self.name;
        if !allow_exec {
            bail!("{name}: {key}{CMD} is set in {FILE_NAME}; pass --allow-exec to run it");
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
            .with_context(|| format!("{name}: {key}{CMD} could not be run"))?;
        if !out.status.success() {
            bail!(
                "{name}: {key}{CMD} failed ({}); see its output above",
                out.status
            );
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let value = stdout.lines().next().unwrap_or("").trim();
        if value.is_empty() {
            bail!("{name}: {key}{CMD} printed nothing; it must print the {key} on its first line");
        }
        Ok(value.to_string())
    }
}

impl Imap {
    /// The password, which the file may leave out only to have it
    /// commanded.
    ///
    /// # Errors
    ///
    /// Neither given nor commanded.
    pub fn password(&self, name: &str) -> Result<&str> {
        self.password.as_deref().ok_or_else(|| {
            anyhow!(
                "{name}: no password configured; set password{CMD} (a command that prints it) or password in {FILE_NAME}"
            )
        })
    }
}

impl Graph {
    /// The application's secret, which the file may leave out only to
    /// have it commanded.
    ///
    /// # Errors
    ///
    /// Neither given nor commanded.
    pub fn secret(&self, name: &str) -> Result<&str> {
        self.client_secret.as_deref().ok_or_else(|| {
            anyhow!(
                "{name}: no client_secret configured; set client_secret{CMD} (a command that prints it) or client_secret in {FILE_NAME}"
            )
        })
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

    fn imap(account: &Account, allow_exec: bool) -> Result<Imap> {
        match account.reach(allow_exec)? {
            Reach::Imap(imap) => Ok(imap),
            Reach::Graph(_) => panic!("{}: an imap account was expected", account.name),
        }
    }

    /// The starter with its examples switched on: every commented line
    /// that is a table header or a key.
    fn uncommented(text: &str) -> String {
        text.lines()
            .map(|line| match line.strip_prefix("# ") {
                Some(rest)
                    if rest.starts_with("[[account]]")
                        || rest.split_once(" = ").is_some_and(|(key, _)| {
                            key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                        }) =>
                {
                    rest
                }
                _ => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_starter_fetches_nothing_until_an_example_is_switched_on() {
        assert!(load(STARTER).unwrap().accounts.is_empty());

        let config = load(&uncommented(STARTER)).unwrap();
        let names: Vec<_> = config
            .accounts
            .iter()
            .map(|account| account.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["example.org", "gmail.com", "proton.me", "m365"],
            "every example is an account this build reads, every key one it knows"
        );
    }

    #[test]
    fn begin_writes_the_starter_once_and_leaves_a_file_standing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(begin(dir.path()).unwrap());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(FILE_NAME)).unwrap(),
            STARTER
        );

        std::fs::write(dir.path().join(FILE_NAME), "# mine\n").unwrap();
        assert!(!begin(dir.path()).unwrap());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(FILE_NAME)).unwrap(),
            "# mine\n"
        );
    }

    #[test]
    fn plaintext_is_for_loopback_only() {
        for host in ["127.0.0.1", "localhost", "::1", "[::1]", "127.0.0.2"] {
            let config = load(&format!(
                "[[account]]\nname = \"a\"\nhost = \"{host}\"\ntls = false\nuser = \"u\"\n"
            ))
            .unwrap();
            assert!(
                imap(&config.accounts[0], false).is_ok(),
                "{host} is this machine"
            );
        }
        let config = load(
            "[[account]]\nname = \"a\"\nhost = \"imap.example.org\"\ntls = false\nuser = \"u\"\n",
        )
        .unwrap();
        let refused = imap(&config.accounts[0], false).err().unwrap();
        assert!(
            refused.to_string().contains("allowed only for localhost"),
            "{refused:#}"
        );
    }

    #[test]
    fn the_defaults_fill_in_and_the_file_is_read_strictly() {
        let config = load(
            "[[account]]\nname = \"example.org\"\nhost = \"imap.example.org\"\nuser = \"john\"\n",
        )
        .unwrap();
        let account = &config.accounts[0];
        let imap = imap(account, false).unwrap();
        assert_eq!((imap.port, imap.tls), (993, true));
        assert!(account.folders.is_none());

        let refused =
            load("[[account]]\nname = \"a\"\nhost = \"h\"\nuser = \"u\"\nserver = \"x\"\n")
                .err()
                .unwrap();
        assert!(
            format!("{refused:#}").contains("server"),
            "a key this build does not know is refused by name: {refused:#}"
        );
        let refused = load("[[account]]\nname = \"a\"\nhost = \"h\"\n")
            .err()
            .unwrap();
        assert!(
            format!("{refused:#}").contains("user"),
            "a key missing is named at load, not at the login: {refused:#}"
        );
    }

    #[test]
    fn names_must_be_unique_and_fit_the_record() {
        let twice = "[[account]]\nname = \"a\"\nhost = \"h\"\nuser = \"u\"\n\
                     [[account]]\nname = \"a\"\nhost = \"h\"\nuser = \"u\"\n";
        assert!(load(twice).is_err());
        assert!(load("[[account]]\nname = \"a/b\"\nhost = \"h\"\nuser = \"u\"\n").is_err());
        assert!(load("[[account]]\nname = \"a b\"\nhost = \"h\"\nuser = \"u\"\n").is_err());
        assert!(
            load("[[account]]\nhost = \"h\"\nuser = \"u\"\n").is_err(),
            "no name, no account"
        );
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
    fn a_command_runs_only_when_allowed_and_any_key_may_be_commanded() {
        let config = load(
            "[[account]]\nname = \"a\"\nhost_cmd = \"echo imap.example.org\"\n\
             user = \"nobody\"\nuser_cmd = \"printf 'john\\\\nignored'\"\n\
             password_cmd = \"echo secret\"\n",
        )
        .unwrap();
        let account = &config.accounts[0];
        let refused = imap(account, false).err().unwrap();
        assert!(
            refused.to_string().contains("host_cmd is set in"),
            "{refused:#}"
        );

        let imap = imap(account, true).unwrap();
        assert_eq!(imap.host, "imap.example.org");
        assert_eq!(
            imap.user, "john",
            "the command's first line wins over the key"
        );
        assert_eq!(imap.password("a").unwrap(), "secret");
    }

    #[test]
    fn a_command_that_fails_or_says_nothing_is_named() {
        let config = load(
            "[[account]]\nname = \"a\"\nhost = \"h\"\nuser = \"u\"\npassword_cmd = \"false\"\n\
             [[account]]\nname = \"b\"\nhost = \"h\"\nuser = \"u\"\npassword_cmd = \"true\"\n\
             [[account]]\nname = \"c\"\nhost = \"h\"\nuser = \"u\"\n",
        )
        .unwrap();
        let failed = imap(&config.accounts[0], true).err().unwrap();
        assert!(
            failed.to_string().contains("password_cmd failed"),
            "{failed:#}"
        );
        let silent = imap(&config.accounts[1], true).err().unwrap();
        assert!(silent.to_string().contains("printed nothing"), "{silent:#}");
        let none = imap(&config.accounts[2], true).unwrap();
        let missing = none.password("c").err().unwrap();
        assert!(
            missing.to_string().contains("no password configured"),
            "{missing:#}"
        );
    }

    #[test]
    fn the_keys_that_shape_the_account_take_no_command() {
        for key in ["name", "backend", "folders"] {
            let refused = load(&format!(
                "[[account]]\nname = \"a\"\nhost = \"h\"\nuser = \"u\"\n{key}_cmd = \"echo x\"\n"
            ))
            .err()
            .unwrap();
            assert!(
                format!("{refused:#}").contains(&format!("{key}_cmd is not supported")),
                "{refused:#}"
            );
        }
        let refused = load(
            "[[account]]\nname = \"a\"\nhost = \"h\"\nuser = \"u\"\nnonsense_cmd = \"echo x\"\n",
        )
        .err()
        .unwrap();
        assert!(
            format!("{refused:#}").contains("nonsense"),
            "a command for a key the account does not have is refused at load: {refused:#}"
        );
    }

    #[test]
    fn an_msgraph_account_has_its_own_keys_and_none_of_imaps() {
        let config = load(
            "[[account]]\nname = \"m365\"\nbackend = \"msgraph\"\ntenant_id_cmd = \"echo t\"\n\
             client_id = \"c\"\nclient_secret_cmd = \"echo s3cret\"\nuser = \"john@example.com\"\n\
             folders = [\"Inbox\"]\n",
        )
        .unwrap();
        let account = &config.accounts[0];
        assert_eq!(account.folders.as_deref(), Some(&["Inbox".to_string()][..]));
        assert!(account.reach(false).is_err(), "two commands, not allowed");
        let Reach::Graph(graph) = account.reach(true).unwrap() else {
            panic!("an msgraph account was expected");
        };
        assert_eq!(
            (graph.tenant_id.as_str(), graph.client_id.as_str()),
            ("t", "c")
        );
        assert_eq!(graph.secret("m365").unwrap(), "s3cret");

        let refused = load(
            "[[account]]\nname = \"m365\"\nbackend = \"msgraph\"\ntenant_id = \"t\"\n\
             client_id = \"c\"\nuser = \"u\"\nhost = \"imap.example.com\"\n",
        )
        .err()
        .unwrap();
        assert!(
            format!("{refused:#}").contains("host"),
            "an imap key on an msgraph account is refused by name: {refused:#}"
        );

        let refused = load(
            "[[account]]\nname = \"m365\"\nbackend = \"msgraph\"\nclient_id = \"c\"\nuser = \"u\"\n",
        )
        .err()
        .unwrap();
        assert!(
            format!("{refused:#}").contains("tenant_id"),
            "a missing key is named: {refused:#}"
        );

        let refused = load("[[account]]\nname = \"x\"\nbackend = \"pop3\"\n")
            .err()
            .unwrap();
        assert!(
            format!("{refused:#}").contains("imap or msgraph"),
            "{refused:#}"
        );
    }
}
