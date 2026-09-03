# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- **Archive format, generation 1** — the reading contract in [docs/format.md](docs/format.md):
  content and claims as [immure](https://github.com/sniner/immure) stores, an append-only claim
  log in sealed segments, disposable caches, and recovery with shell tools alone
- **Attribute vocabulary** — [docs/vocabulary.md](docs/vocabulary.md): what each attribute means
  and how standing claims become an answer — every attribute is a set of standing values,
  deduplicated at fold time; narrowing to one value is the reader's own policy; extractors
  record verbatim, interpretation stays at query time
- **`ossuary init`** begins an archive — or completes one already standing: nothing standing is
  remade or edited, and what is missing (today: `config.toml`) is added
- **`config.toml`** in the archive root: `[ingest] exclude` glob patterns (`.DS_Store` and
  friends never go in; an excluded directory is not walked) and `[content] compress` (zstd for
  new content; what is already stored keeps its form, and reading understands both). A missing
  file means the defaults; an unknown key is refused rather than half-applied
- **`ossuary ingest PATH`** takes a directory tree — or a single file — in: every regular file
  into the content store, seven day-one claims per new blob into the log — a blob met again gets
  its sighting only, and every place and name it sat under goes on the record; the mtime is
  recorded at the precision the filesystem observed it. A repeated run remembers what
  it already observed (in `cache/`) and leaves unchanged files in peace: not read, not hashed,
  no claims; `--full` looks at everything anew
- **`ossuary seal`** closes the open segment; its claims become part of the sealed log
- **`ossuary about SUBJECT`** answers everything on the record about one file, oldest first; a
  beginning of the digest is enough while it names only one file, and naming attributes (or a
  namespace, like `exif:`) narrows the answer to them
- **`ossuary find TERM…`** answers which files match: terms are `attribute=value` and must all
  hold, `*` and `?` match within text values, `low..high` asks for a value inside the range
  (either side open — `file:modified=2026-09-01..` is "changed since September"; bounds compare
  in the attribute's own spelling; a value in double quotes is literal), and
  `--missing ATTRIBUTE` (or a namespace like `exif:`) asks for what a file lacks. Only standing
  values count — a retracted value no longer answers — and the matches come out one name per
  line, ready for `about` or `get`
- **`ossuary value SUBJECT ATTRIBUTE`** answers what currently stands for one attribute —
  retractions applied, repeats collapsed — one value per line, strings bare, ready for a script;
  several lines mean the attribute honestly holds several values, and choosing among them stays
  the caller's policy. Exits 1 when nothing stands, so a script can test for it
- **`--json`** on `about` and `value` answers in JSON lines ready for `jq`: `about --json` gives
  each claim exactly as the log spells it, `value --json` keeps the values' JSON spelling
- **`--quiet`** on every command keeps the run's narration off stderr — counts, progress, hints;
  answers and errors still come
- **`ossuary get SUBJECT`** hands a file's bytes back, to stdout or `--output FILE`; short
  digests resolve against the content store, from six characters up
- **`ossuary id FILE`** names a file the way the archive would — hashed from its bytes, the file
  only read — and says whether the archive already holds it; works before an ingest as well as
  after
- **`ossuary extract NAME`** runs one extractor — its own program, `ossuary-extract-NAME` found
  on PATH — over every file of a kind it reads that it has not examined yet: findings go on the
  record under the extractor's name, and every examined file gets a receipt, found something or
  not, so a repeated run costs only what is new and a new extractor version looks at everything
  again. An extractor can hand back files as well as findings — an unpacked attachment,
  extracted text — and each goes into the archive as content of its own, named and typed in the
  extractor's words (`file:name`, `file:mime`) and tied to its origin (`derive:derived-from`);
  `--temp-dir` says where derived files wait on their way in, for when the archive sits on a
  slow share. The pipe protocol, open to any language, is [docs/extractors.md](docs/extractors.md)
- **`ossuary-extract-exif`** — the first extractor: EXIF fields verbatim, tag names kebab-cased
  under `exif:`, values as the format stores them (`"2019:07:14 11:02:41"`, `"28/10"`)
