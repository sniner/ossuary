# ossuary-core

*Core library of the ossuary archive: claims, segments and the claim index.*

This is the library all ossuary programs use to read and write an
archive: the content stores, the append-only claim log, and the index
built from the log. It has no command line, prints nothing and sets no
exit codes; that is left to the programs.
[`ossuary`](../ossuary-cli/README.md) and
[`ossuary-mount`](../ossuary-mount/README.md) are built on it, as is any
other program that opens an archive.

The archive format is documented separately from this code:

* [The archive format](../docs/format.md): layout, claims, segments, and
  how to read an archive with standard shell tools
* [The attribute vocabulary](../docs/vocabulary.md): the attributes and
  what they mean
* [The extractor protocol](../docs/extractors.md): how this crate
  communicates with extractor programs over pipes

## Archive layout

An archive is a directory with a one-line `FORMAT` file. The file
records the format generation and the parameters needed to open the
stores: the hash algorithm and the shard depths. The directory contains:

| | |
|---|---|
| `content/` | the ingested files, unchanged, each named by the hash of its content |
| `derived/` | files produced by extractors (unpacked attachments, extracted text), stored separately from the originals |
| `claims/`, `head.jsonl` | the log: every claim ever written, in sealed segments (`claims/`) and the open head (`head.jsonl`) |
| `config.toml` | settings for writing: which paths ingest excludes, whether new entries are compressed, which extractors `extract` runs when none is named. Not needed for reading |
| `cache/` | the query index and the list of files ingest has already seen. Can be deleted; it is rebuilt, and the next query and ingest take longer |

The stores are [immure](https://github.com/sniner/immure) stores.
`Archive::open` refuses a `FORMAT` file with an unknown generation or
content (`Error::ArchiveGeneration`, `Error::BadMark`) instead of
guessing at the layout.

## API overview

```rust
use ossuary_core::{Archive, Attribute, Scope, Term};

let archive = Archive::open("/home/john/archive")?;   // reads FORMAT and config.toml, opens the stores and the log
let mut index = archive.index()?;                     // the index in cache/index.sqlite
index.fold(archive.log())?;                           // update it: new sealed segments, then the head

let mime = Term::Attribute(Attribute::parse("file:mime")?, "image/jpeg".to_string());
for subject in index.find(&[mime], &[], Scope::Present)? {
    println!("{}", subject.as_str());
}
```

**Claims.** A `Claim` is built from a `Subject`, an `Attribute`, a
`Value`, a `Timestamp`, a `Source` and a `Run`. `Claim::assert`,
`Claim::retract_value` and `Claim::retract_attribute` create one;
`Claim::parse_line` and `Claim::to_line` read and write the JSON line
format, and round-trip. Claims are never edited: a retraction is a new
claim.

**The log.** `Log::append` appends a claim to the open head, and
`Log::seal` turns the head into a sealed segment named by its digest.
`Log::segments` lists the sealed segments and `Log::read` reads one.
`Manifests` are per-segment summaries in `cache/` (claim count, time
range, namespaces, subjects); with them, `Log::segments` does not need
to read every segment.

**The index.** `Index` folds the log into SQLite. It is a cache:
everything in it can be rebuilt from the log. `about` returns every
claim about one subject, `values` and `values_in` the standing values of
an attribute or a namespace, `find` the subjects that match query terms,
`under` the files below a path, `standing_as_of` the standing values of
an attribute at a given time, `resolve` and `matching` look up subjects
by hex prefix, and `worklist` lists the files an extractor has not
examined yet.

**Operations.** `ingest` walks the given paths and adds their files to
the archive. `examine` runs extractor programs over the files they have
not examined yet: it identifies each extractor, sends it the file
content, checks its output and records the result with a receipt. It
reports progress as `Event`s to an `Observer` and prints nothing itself.
`annotate` records user tags and comments on files already in the
archive, `retract` retracts claims, `lay_out` computes the target paths
of an export, `audit_store` and `audit_log` check the stores and the log
for damage, and `mend` repairs a break in the segment chain found by
`audit_log` without rewriting any sealed segment.

All errors are variants of one `Error` enum. `Error::spelled` returns the
error with its causes as one line, ready to show to a user.

## Using it

The crate is a workspace member and is used as a path dependency:

```toml
[dependencies]
ossuary-core = { path = "../ossuary-core" }
```

It forbids unsafe code. SQLite is bundled, so no system library is
needed to build it.

```console
$ cargo test -p ossuary-core
```

## License

Apache License 2.0, see [LICENSE](../LICENSE).
