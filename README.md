# ossuary

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

What runs today is the walking skeleton: `ossuary-core` (claims, the log,
ingest, and a disposable query index) and the `ossuary` command line —
`init`, `ingest`, `seal`, `about`. Enough to take a directory tree in and
ask what the archive knows about any file in it. Extractors, and with them
the metadata that lives *inside* the files, come next.

The blob layer is [immure](https://github.com/sniner/immure), content-addressed
storage with deduplication, zstd compression and encryption.
