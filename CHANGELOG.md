# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- **Archive format, generation 1** — the reading contract in [docs/format.md](docs/format.md):
  content and claims as [immure](https://github.com/sniner/immure) stores, an append-only claim
  log in sealed segments, disposable caches, and recovery with shell tools alone
- **`ossuary init`** begins an archive — or completes one already standing: nothing standing is
  remade or edited, and what is missing (today: `config.toml`) is added
- **`config.toml`** in the archive root: `[ingest] exclude` glob patterns (`.DS_Store` and
  friends never go in; an excluded directory is not walked) and `[content] compress` (zstd for
  new content; what is already stored keeps its form, and reading understands both). A missing
  file means the defaults; an unknown key is refused rather than half-applied
- **`ossuary ingest DIR`** takes a directory tree in: every regular file into the content
  store, six day-one facts per file into the claim log; re-ingesting stores nothing twice but
  records every place a file sat
- **`ossuary seal`** closes the open segment; its facts become part of the sealed log
- **`ossuary about SUBJECT`** answers everything on the record about one file, oldest first; a
  beginning of the digest is enough while it names only one file
- **`ossuary get SUBJECT`** hands a file's bytes back, to stdout or `--output FILE`; short
  digests resolve against the content store, from six characters up
