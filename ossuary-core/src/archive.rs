//! The archive: one root, its mark, and the pieces beneath it.
//!
//! An archive is a directory that says so. The `FORMAT` mark in its root —
//! one line of JSON — names the generation the archive is written in and
//! the constants the stores need to be opened again: the hash algorithm and
//! the shard depths, which immure deliberately keeps no record of itself.
//! Everything else under the root is the pieces: `content/` and `claims/`,
//! the open head, a `config.toml` of settings that reading never needs,
//! and a `cache/` that answers questions and owes nothing.
//!
//! The mark is read before anything else is touched, and a generation this
//! build does not know is refused for what it is: a layout never seen
//! before would look familiar in exactly the wrong way — the directories it
//! knows are all present — and be read wrong with confidence.

use std::fs;
use std::path::{Path, PathBuf};

use immure::{Algorithm, Store};
use serde::{Deserialize, Serialize};

use crate::config::{self, Config};
use crate::error::{Error, Result};
use crate::index::Index;
use crate::ingest::IngestMemory;
use crate::log::{GENERATION, Log};

/// The mark's name in the archive root.
const MARK: &str = "FORMAT";

/// What generation 1 fixes without asking the mark.
const CONTENT_DEPTH: usize = 2;
const CLAIMS_DEPTH: usize = 1;

/// The one line of the mark, as `docs/format.md` draws it.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Mark {
    #[serde(rename = "ossuary-archive")]
    generation: u32,
    algorithm: String,
    #[serde(rename = "content-depth")]
    content_depth: usize,
    #[serde(rename = "claims-depth")]
    claims_depth: usize,
}

/// The generation alone, read leniently before the mark is read strictly —
/// a future generation may carry members this build has never heard of, and
/// it must be refused as newer, not reported as broken.
#[derive(Debug, Deserialize)]
struct Generation {
    #[serde(rename = "ossuary-archive")]
    generation: u32,
}

/// An archive in hand: the mark read, the settings loaded, the stores
/// described, the log ready.
#[derive(Debug)]
pub struct Archive {
    root: PathBuf,
    config: Config,
    content: Store,
    log: Log,
}

impl Archive {
    /// Begin an empty archive at `root`: the mark, a starter
    /// `config.toml`, both stores, a `cache/`.
    ///
    /// The algorithm is the one choice that is the archive's own — every
    /// blob in both stores will be named by it, for good. Everything the
    /// starter configuration spells out is changeable later, by editing
    /// it; a `config.toml` already standing at `root` is kept, not
    /// overwritten.
    ///
    /// # Errors
    ///
    /// [`Error::AlreadyArchive`] when a mark already stands at `root`,
    /// [`Error::Io`] writing the mark, [`Error::BadConfig`] when a kept
    /// configuration will not read, [`Error::Store`] making the stores.
    ///
    /// # Panics
    ///
    /// It does not, in practice: serialising the mark — numbers and a name
    /// — has no failure mode, and the panic stands in for the arm that
    /// cannot be reached.
    pub fn create(root: impl Into<PathBuf>, algorithm: Algorithm) -> Result<Archive> {
        let root = root.into();
        let path = root.join(MARK);
        if path.exists() {
            return Err(Error::AlreadyArchive(root));
        }
        let io = |context: &str| {
            let context = format!("{}: {context}", root.display());
            move |source| Error::Io { context, source }
        };
        fs::create_dir_all(&root).map_err(io("creating the archive root"))?;
        fs::create_dir_all(root.join("cache")).map_err(io("creating cache/"))?;
        let mark = Mark {
            generation: GENERATION,
            algorithm: algorithm.name().to_string(),
            content_depth: CONTENT_DEPTH,
            claims_depth: CLAIMS_DEPTH,
        };
        // A struct of numbers and a string serialises; see `Claim::to_line`.
        let mut line = serde_json::to_string(&mark).expect("a mark serialises");
        line.push('\n');
        fs::write(&path, line).map_err(io("writing the FORMAT mark"))?;

        let settings = root.join(config::CONFIG);
        if !settings.exists() {
            fs::write(&settings, config::STARTER).map_err(io("writing config.toml"))?;
        }
        let config = Config::load(&root)?;

        let content = content_store(&root, algorithm, CONTENT_DEPTH, config.compress()).create()?;
        let claims = claims_store(&root, algorithm, CLAIMS_DEPTH).create()?;
        Ok(Archive {
            log: Log::new(claims, root.join("head.jsonl")),
            config,
            content,
            root,
        })
    }

    /// Open the archive at `root`: read the mark and the settings,
    /// describe the stores.
    ///
    /// Touches nothing beyond the mark and `config.toml` — describing a
    /// store is not making one, all the way down.
    ///
    /// # Errors
    ///
    /// [`Error::NoArchive`] when there is no mark, [`Error::BadMark`] when
    /// it will not read, [`Error::ArchiveGeneration`] when a newer ossuary
    /// wrote it, [`Error::BadConfig`] when `config.toml` stands but will
    /// not read, and [`Error::Store`] when the mark names an algorithm
    /// this build does not know.
    pub fn open(root: impl Into<PathBuf>) -> Result<Archive> {
        let root = root.into();
        let text = match fs::read_to_string(root.join(MARK)) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NoArchive(root));
            }
            Err(source) => {
                return Err(Error::Io {
                    context: format!("{}: reading the FORMAT mark", root.display()),
                    source,
                });
            }
        };
        let line = text.lines().next().unwrap_or_default();
        let generation: Generation =
            serde_json::from_str(line).map_err(|_| Error::BadMark(root.clone()))?;
        if generation.generation != GENERATION {
            return Err(Error::ArchiveGeneration(generation.generation));
        }
        let mark: Mark = serde_json::from_str(line).map_err(|_| Error::BadMark(root.clone()))?;
        let algorithm: Algorithm = mark.algorithm.parse()?;
        let config = Config::load(&root)?;

        let content =
            content_store(&root, algorithm, mark.content_depth, config.compress()).build()?;
        let claims = claims_store(&root, algorithm, mark.claims_depth).build()?;
        Ok(Archive {
            log: Log::new(claims, root.join("head.jsonl")),
            config,
            content,
            root,
        })
    }

    /// Complete the archive: add what [`create`](Archive::create) would
    /// have made and `root` no longer holds — the starter `config.toml`,
    /// an empty `cache/`. What stands is left standing, whatever state it
    /// is in: completing an archive never edits one.
    ///
    /// Answers whether a `config.toml` was written. The settings in hand
    /// stay as they were read at opening time — an archive completed just
    /// now was opened without a configuration, and runs on the defaults
    /// until reopened.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] writing what is missing.
    pub fn complete(&self) -> Result<bool> {
        let io = |context: &str| {
            let context = format!("{}: {context}", self.root.display());
            move |source| Error::Io { context, source }
        };
        fs::create_dir_all(self.root.join("cache")).map_err(io("creating cache/"))?;
        let settings = self.root.join(config::CONFIG);
        if settings.exists() {
            return Ok(false);
        }
        fs::write(&settings, config::STARTER).map_err(io("writing config.toml"))?;
        Ok(true)
    }

    /// Where the archive stands.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The archive's settings, as `config.toml` holds them — the defaults
    /// where there is no file to hold them.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The content store: what is kept.
    #[must_use]
    pub fn content(&self) -> &Store {
        &self.content
    }

    /// The claim log: what is known about it.
    #[must_use]
    pub fn log(&self) -> &Log {
        &self.log
    }

    /// The query index in `cache/`, opened — created, folded and thrown
    /// away as needed; nothing about it is part of the archive.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] making `cache/`, [`Error::Index`] opening the file.
    pub fn index(&self) -> Result<Index> {
        let cache = self.cache()?;
        Index::open(cache.join("index.sqlite"))
    }

    /// The ingest walk's memory in `cache/`, opened — what earlier runs
    /// already observed, so a repeated sweep leaves unchanged files in
    /// peace. A cache like the index: deleting it costs one noisy run,
    /// never a claim.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] making `cache/`, [`Error::Index`] opening the file.
    pub fn ingest_memory(&self) -> Result<IngestMemory> {
        let cache = self.cache()?;
        IngestMemory::open(cache.join("ingest.sqlite"))
    }

    /// `cache/`, made if it is not there — it owes nothing and may vanish
    /// between any two commands.
    fn cache(&self) -> Result<PathBuf> {
        let cache = self.root.join("cache");
        fs::create_dir_all(&cache).map_err(|source| Error::Io {
            context: format!("{}: creating cache/", self.root.display()),
            source,
        })?;
        Ok(cache)
    }
}

/// How generation 1 lays out the content store: entries named by digest
/// alone — what a blob is lives in the log, never in a file name. Whether
/// new entries are compressed is the configuration's word; what is already
/// stored keeps its form, and reading understands both.
fn content_store(
    root: &Path,
    algorithm: Algorithm,
    depth: usize,
    compress: bool,
) -> immure::Builder {
    Store::builder(root.join("content"))
        .suffix("")
        .depth(depth)
        .algorithm(algorithm)
        .compress(compress)
}

/// How generation 1 lays out the claims store: only segments, compressed —
/// they are text, and they are forever.
fn claims_store(root: &Path, algorithm: Algorithm, depth: usize) -> immure::Builder {
    Store::builder(root.join("claims"))
        .suffix(".seg")
        .depth(depth)
        .algorithm(algorithm)
        .compress(true)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn a_new_archive_carries_its_mark_and_its_pieces() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("archive");

        let archive = Archive::create(&root, Algorithm::Sha256).unwrap();

        assert_eq!(
            fs::read_to_string(root.join("FORMAT")).unwrap(),
            "{\"ossuary-archive\":1,\"algorithm\":\"sha256\",\"content-depth\":2,\"claims-depth\":1}\n",
            "one line, and cat answers what this directory is"
        );
        assert!(root.join("content").is_dir());
        assert!(root.join("claims").is_dir());
        assert!(root.join("cache").is_dir());
        assert_eq!(archive.root(), root);
    }

    #[test]
    fn a_new_archive_spells_its_settings_out() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("archive");

        let archive = Archive::create(&root, Algorithm::Sha256).unwrap();

        let starter = fs::read_to_string(root.join("config.toml")).unwrap();
        assert!(starter.contains(".DS_Store"), "the defaults stand visible");
        assert!(
            archive.config().excludes().excluded(Path::new(".DS_Store")),
            "and they are in force from the first ingest"
        );
        assert!(!archive.content().compresses());
    }

    #[test]
    fn a_config_already_standing_is_kept() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("archive");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("config.toml"), "[content]\ncompress = true\n").unwrap();

        let archive = Archive::create(&root, Algorithm::Sha256).unwrap();

        assert!(
            archive.content().compresses(),
            "init obeys the settings it found, and does not overwrite them"
        );
    }

    #[test]
    fn the_settings_reach_the_content_store() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("archive");
        Archive::create(&root, Algorithm::Sha256).unwrap();
        fs::write(root.join("config.toml"), "[content]\ncompress = true\n").unwrap();

        let archive = Archive::open(&root).unwrap();

        assert!(archive.content().compresses());
        let (_, entry) = archive.content().add(b"hello world").unwrap();
        assert!(entry.is_compressed(), "new entries take the settings' form");
    }

    #[test]
    fn a_broken_config_is_refused_with_its_name() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("archive");
        Archive::create(&root, Algorithm::Sha256).unwrap();
        fs::write(root.join("config.toml"), "[content]\ncompres = true\n").unwrap();

        assert!(matches!(Archive::open(&root), Err(Error::BadConfig { .. })));
    }

    #[test]
    fn completing_adds_what_is_missing_and_only_that() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("archive");
        Archive::create(&root, Algorithm::Sha256).unwrap();
        fs::remove_file(root.join("config.toml")).unwrap();
        fs::remove_dir(root.join("cache")).unwrap();

        let archive = Archive::open(&root).unwrap();
        assert!(
            archive.complete().unwrap(),
            "the missing config.toml is written"
        );
        assert!(root.join("cache").is_dir());
        assert!(
            fs::read_to_string(root.join("config.toml"))
                .unwrap()
                .contains(".DS_Store"),
            "and it is the starter, defaults spelled out"
        );

        assert!(
            !archive.complete().unwrap(),
            "a second completion finds nothing to add"
        );
    }

    #[test]
    fn completing_never_edits_what_stands() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("archive");
        Archive::create(&root, Algorithm::Sha256).unwrap();
        let own = "[content]\ncompress = true\n";
        fs::write(root.join("config.toml"), own).unwrap();

        let archive = Archive::open(&root).unwrap();
        assert!(!archive.complete().unwrap());
        assert_eq!(
            fs::read_to_string(root.join("config.toml")).unwrap(),
            own,
            "the archive's own settings outrank the starter"
        );
    }

    #[test]
    fn an_archive_opens_by_its_mark_alone() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("archive");
        Archive::create(&root, Algorithm::Blake3).unwrap();

        let archive = Archive::open(&root).unwrap();

        assert_eq!(
            archive.content().algorithm(),
            Algorithm::Blake3,
            "the mark remembered what immure deliberately does not"
        );
    }

    #[test]
    fn a_directory_without_a_mark_is_not_an_archive() {
        let dir = TempDir::new().unwrap();
        assert!(matches!(
            Archive::open(dir.path()),
            Err(Error::NoArchive(_))
        ));
    }

    #[test]
    fn an_archive_is_not_begun_twice() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("archive");
        Archive::create(&root, Algorithm::Sha256).unwrap();

        assert!(matches!(
            Archive::create(&root, Algorithm::Sha256),
            Err(Error::AlreadyArchive(_))
        ));
    }

    #[test]
    fn a_mark_from_the_future_is_refused_for_what_it_is() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("FORMAT"),
            "{\"ossuary-archive\":2,\"algorithm\":\"sha256\",\"content-depth\":2,\"claims-depth\":1,\"salt\":\"pepper\"}\n",
        )
        .unwrap();

        assert!(
            matches!(Archive::open(dir.path()), Err(Error::ArchiveGeneration(2))),
            "members this build never heard of do not turn newer into broken"
        );
    }

    #[test]
    fn a_mangled_mark_is_named_for_what_it_is() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("FORMAT"), "not a mark\n").unwrap();

        assert!(matches!(Archive::open(dir.path()), Err(Error::BadMark(_))));
    }
}
