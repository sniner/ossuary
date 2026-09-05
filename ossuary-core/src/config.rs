//! The archive's own settings: `config.toml` in the root.
//!
//! Where the `FORMAT` mark holds the constants an archive cannot be *read*
//! without, `config.toml` holds the policy it is *written* by: what ingest
//! leaves out, whether new store entries are compressed. Losing it loses no data,
//! only preferences — which is why it may live outside the stores, be
//! edited freely, and be absent altogether: no file means the defaults.
//!
//! It is read strictly. A key this build does not know is refused rather
//! than skipped, because a half-applied write policy writes the wrong
//! things into a log that keeps them forever.

use std::fs;
use std::path::Path;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Deserialize;

use crate::error::{Error, Result};

/// The configuration's name in the archive root.
pub(crate) const CONFIG: &str = "config.toml";

/// What `ossuary init` writes when no configuration stands yet: the
/// defaults, spelled out where an editor finds them, so that changing the
/// archive's behaviour is changing a line, not learning a scheme.
pub(crate) const STARTER: &str = r#"# This archive's own settings: what goes in, and how it is written.
# Reading the archive never needs this file. Without it - or without a
# key - nothing is excluded and new content is stored as it came.

[ingest]
# What never goes in. Glob patterns: one without a slash matches a file
# or directory name at any level, one with a slash matches the path
# below the ingested directory (`*` stays within one level, `**`
# crosses levels). A directory left out is not walked at all.
exclude = [".DS_Store", "._*", "Thumbs.db", "desktop.ini"]

[store]
# Compress new entries with zstd - in content/ and derived/ alike, one
# word for both. What is already stored keeps its form either way, and
# reading understands both. Claim segments are always compressed - that
# is the format's choice, not this file's.
compress = false

[extract]
# What a bare `ossuary extract` runs, in order - each a program
# `ossuary-extract-<name>` found on PATH, like ["mail", "exif", "text"].
# A program offering several contracts runs them all; name:contract
# runs one of them, like "packed:list". Empty means: nothing runs
# unless named outright.
run = []
"#;

/// `config.toml` as it lies in the file, every part optional.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    ingest: Ingest,
    #[serde(default)]
    store: StoreSection,
    #[serde(default)]
    extract: Extract,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ingest {
    #[serde(default)]
    exclude: Vec<String>,
}

/// The `[store]` section — one word for `content/` and `derived/` both:
/// what one holds is as sensitive as the other, so they are written the
/// same way.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreSection {
    #[serde(default)]
    compress: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Extract {
    #[serde(default)]
    run: Vec<String>,
}

/// The archive's settings, read and ready to ask.
#[derive(Debug, Default)]
pub struct Config {
    excludes: Excludes,
    compress: bool,
    extractors: Vec<String>,
}

impl Config {
    /// Read `config.toml` under `root`. No file is no trouble: the
    /// defaults — nothing excluded, nothing compressed.
    ///
    /// # Errors
    ///
    /// [`Error::BadConfig`] when the file will not parse or an exclude
    /// pattern will not compile, [`Error::Io`] when it cannot be read for
    /// any other reason.
    pub fn load(root: &Path) -> Result<Config> {
        let path = root.join(CONFIG);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Config::default());
            }
            Err(source) => {
                return Err(Error::Io {
                    context: format!("{}: reading", path.display()),
                    source,
                });
            }
        };
        let bad = |trouble: String| Error::BadConfig {
            path: path.clone(),
            trouble,
        };
        let file: File = toml::from_str(&text).map_err(|error| bad(error.to_string()))?;
        let excludes = Excludes::compile(&file.ingest.exclude).map_err(|error| match error {
            Error::Pattern { .. } => bad(error.to_string()),
            other => other,
        })?;
        Ok(Config {
            excludes,
            compress: file.store.compress,
            extractors: file.extract.run,
        })
    }

    /// What ingest leaves out.
    #[must_use]
    pub fn excludes(&self) -> &Excludes {
        &self.excludes
    }

    /// Whether new store entries are compressed — content and derived
    /// alike.
    #[must_use]
    pub fn compress(&self) -> bool {
        self.compress
    }

    /// The extractors a bare `ossuary extract` runs, in the archive's
    /// own order — empty when the archive names none.
    #[must_use]
    pub fn extractors(&self) -> &[String] {
        &self.extractors
    }
}

/// What never goes in, compiled and ready to ask per file.
///
/// Two kinds of pattern, told apart by the slash. A pattern without one
/// names files and directories wherever they stand — `.DS_Store` at any
/// level. A pattern with one names a place: matched against the path below
/// the ingested directory, where `*` stays within one level and `**`
/// crosses them. A leading slash only says the same thing again and is
/// dropped.
#[derive(Debug)]
pub struct Excludes {
    /// Patterns without a slash, matched against the bare name.
    names: GlobSet,
    /// Patterns with a slash, matched against the relative path.
    paths: GlobSet,
    empty: bool,
}

impl Default for Excludes {
    fn default() -> Excludes {
        Excludes::none()
    }
}

impl Excludes {
    /// No patterns: nothing is excluded.
    #[must_use]
    pub fn none() -> Excludes {
        Excludes {
            names: GlobSet::empty(),
            paths: GlobSet::empty(),
            empty: true,
        }
    }

    /// Compile a pattern list, as `config.toml` holds it.
    ///
    /// # Errors
    ///
    /// [`Error::Pattern`] naming the pattern that would not compile.
    pub fn compile<I>(patterns: I) -> Result<Excludes>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        let mut names = GlobSetBuilder::new();
        let mut paths = GlobSetBuilder::new();
        let mut empty = true;
        for pattern in patterns {
            let pattern = pattern.as_ref();
            let bad = |error: globset::Error| Error::Pattern {
                pattern: pattern.to_string(),
                trouble: error.kind().to_string(),
            };
            empty = false;
            if pattern.contains('/') {
                let glob = GlobBuilder::new(pattern.trim_start_matches('/'))
                    .literal_separator(true)
                    .build()
                    .map_err(bad)?;
                paths.add(glob);
            } else {
                names.add(GlobBuilder::new(pattern).build().map_err(bad)?);
            }
        }
        let build = |set: GlobSetBuilder| {
            set.build().map_err(|error| Error::Pattern {
                pattern: error.glob().unwrap_or_default().to_string(),
                trouble: error.kind().to_string(),
            })
        };
        Ok(Excludes {
            names: build(names)?,
            paths: build(paths)?,
            empty,
        })
    }

    /// Whether this path stays out. `relative` is the path below the
    /// ingested directory — never absolute, or the path patterns would
    /// have nothing true to say.
    #[must_use]
    pub fn excluded(&self, relative: &Path) -> bool {
        if self.empty {
            return false;
        }
        if let Some(name) = relative.file_name() {
            if self.names.is_match(name) {
                return true;
            }
        }
        self.paths.is_match(relative)
    }

    /// Whether there is nothing to leave out.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.empty
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn write(dir: &TempDir, text: &str) {
        fs::write(dir.path().join(CONFIG), text).unwrap();
    }

    #[test]
    fn no_file_is_the_defaults() {
        let dir = TempDir::new().unwrap();

        let config = Config::load(dir.path()).unwrap();

        assert!(config.excludes().is_empty());
        assert!(!config.compress());
    }

    #[test]
    fn a_partial_file_fills_in_the_rest() {
        let dir = TempDir::new().unwrap();
        write(&dir, "[store]\ncompress = true\n");

        let config = Config::load(dir.path()).unwrap();

        assert!(config.compress());
        assert!(config.excludes().is_empty());
    }

    #[test]
    fn the_starter_reads_back_as_its_own_defaults() {
        let dir = TempDir::new().unwrap();
        write(&dir, STARTER);

        let config = Config::load(dir.path()).unwrap();

        assert!(!config.compress(), "the starter spells the default out");
        assert!(config.excludes().excluded(Path::new("a/b/.DS_Store")));
        assert!(
            config.extractors().is_empty(),
            "an empty run list is the spelled-out default too"
        );
    }

    #[test]
    fn the_run_list_comes_back_in_its_own_order() {
        let dir = TempDir::new().unwrap();
        write(&dir, "[extract]\nrun = [\"text\", \"exif\"]\n");

        let config = Config::load(dir.path()).unwrap();

        assert_eq!(config.extractors(), ["text", "exif"]);
    }

    #[test]
    fn an_unknown_key_is_refused_not_skipped() {
        let dir = TempDir::new().unwrap();
        write(&dir, "[store]\ncompres = true\n");

        assert!(
            matches!(Config::load(dir.path()), Err(Error::BadConfig { .. })),
            "a policy half understood must not be applied at all"
        );
    }

    #[test]
    fn a_broken_pattern_is_named() {
        let dir = TempDir::new().unwrap();
        write(&dir, "[ingest]\nexclude = [\"a[\"]\n");

        let error = Config::load(dir.path()).unwrap_err();
        assert!(matches!(error, Error::BadConfig { .. }));
        assert!(error.to_string().contains("a["), "the pattern is named");
    }

    #[test]
    fn a_name_pattern_matches_at_any_level() {
        let excludes = Excludes::compile([".DS_Store", "*.tmp"]).unwrap();

        assert!(excludes.excluded(Path::new(".DS_Store")));
        assert!(excludes.excluded(Path::new("deep/below/.DS_Store")));
        assert!(excludes.excluded(Path::new("a/scratch.tmp")));
        assert!(!excludes.excluded(Path::new("a/DS_Store.txt")));
    }

    #[test]
    fn a_path_pattern_matches_the_place() {
        let excludes = Excludes::compile(["build/*", "/vendor/**"]).unwrap();

        assert!(excludes.excluded(Path::new("build/out.o")));
        assert!(
            !excludes.excluded(Path::new("build/sub/out.o")),
            "one star stays within one level"
        );
        assert!(
            excludes.excluded(Path::new("vendor/a/b/c")),
            "two stars cross levels, and the leading slash changes nothing"
        );
        assert!(!excludes.excluded(Path::new("other/build/out.o")));
    }

    #[test]
    fn nothing_is_excluded_by_nothing() {
        assert!(!Excludes::none().excluded(Path::new(".DS_Store")));
    }
}
