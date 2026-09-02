# ossuary

> [!WARNING]
> This is a proof of concept on its way to a first release, and in
> constant flux until then: the format, the commands and any archive you
> create may change or break from one commit to the next, with no
> migration path. Nothing here is ready for data you care about — read,
> play, open issues, but wait for a tagged release before real use.

A personal archive of everything: every file worth keeping, stored immutably,
forever, with queryable metadata — independent of file format, software
generation, or decade. Not a backup tool, not a sync tool, not a filesystem:
an archive.

The design in one line: immutable content-addressed blobs, an append-only log
of metadata claims sealed into the same store, and disposable query indexes
folded from that log. What must survive decades is dumb and append-only; what
answers questions is derived and rebuilt at will.

**Status: young.** The format came first, because it is meant to outlast
everything else:

- [The archive format](docs/format.md) — the reading contract: layout,
  claims, segments, and how to recover an archive with nothing but a shell
  and patience.
- [The attribute vocabulary](docs/vocabulary.md) — what the words mean,
  and how standing claims become an answer.
- [The extractor protocol](docs/extractors.md) — how an extractor, in any
  language, tells the archive what it found.

What runs today is the walking skeleton: `ossuary-core` (claims, the log,
ingest, and a disposable query index), the `ossuary` command line —
`init`, `ingest`, `extract`, `seal`, `about`, `find`, `id`, `get` — and
the first extractor, `ossuary-extract-exif`, recording what EXIF says in
EXIF's own words. Enough to take a directory tree in, ask what the
archive knows about any file in it, search for files by what stands on
the record, and get any file back out. Each archive carries its own
`config.toml` — what ingest leaves out (`.DS_Store` and friends), whether
content is compressed. Derived content — extracted text, thumbnails, and
with them full-text search — comes next.

The blob layer is [immure](https://github.com/sniner/immure), content-addressed
storage with deduplication, zstd compression and encryption.
