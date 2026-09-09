# ossuary-core

*The archive itself: claims, segments, and the fold.*

This is the library every ossuary program stands on. It owns the archive
— the content stores, the append-only claim log, the index folded out of
that log — and nothing above it: no command line, no wording, no exit
codes. [`ossuary`](../ossuary-cli/README.md) and
[`ossuary-mount`](../ossuary-mount/README.md) are readers of this crate,
and so is anything else that opens an archive.

What this crate implements is written down separately, because the format
has to outlive the software that writes it:

* [The archive format](../docs/format.md) — layout, claims, segments, and
  how to read an archive with nothing but a shell
* [The attribute vocabulary](../docs/vocabulary.md) — what the words mean
* [The extractor protocol](../docs/extractors.md) — the contract this
  crate speaks to extractor programs over pipes

## What an archive is

A directory that says so. A one-line `FORMAT` mark names the generation
and the constants the stores cannot be opened without — the hash
algorithm and the shard depths. Beneath it:

| | |
|---|---|
| `content/` | the files as they arrived, each named by the hash of its own bytes |
| `derived/` | what tools made of them — unpacked attachments, extracted text — apart from the originals by topology, not by flag |
| `claims/`, `head.jsonl` | the log: every claim ever made, sealed segment by sealed segment |
| `config.toml` | policy for writing — what ingest leaves out, whether new entries are compressed, which extractors a bare `extract` runs. Reading never needs it |
| `cache/` | the query index and the ingest walk's memory. Disposable: deleting it costs a slow first answer, never a fact |

The blob stores are [immure](https://github.com/sniner/immure). A mark
this build does not know is refused rather than guessed at — a layout
never seen before would look familiar in exactly the wrong way.

## The shape of the API

```rust
use ossuary_core::{Archive, Attribute};

let archive = Archive::open("/home/john/archive")?;   // the mark, the config, the stores, the log
let mut index = archive.index()?;                     // the cache in cache/index.sqlite
index.fold(archive.log())?;                           // catch it up: new segments once, the head afresh

let mime = (Attribute::parse("file:mime")?, "image/jpeg".to_string());
for subject in index.find(&[mime], &[])? {
    println!("{}", subject.as_str());
}
```

**Claims.** `Claim` with its `Subject`, `Attribute`, `Value`, `Timestamp`
and `Source` is the whole vocabulary of the record. `Claim::assert`,
`Claim::retract_value` and `Claim::retract_attribute` build one;
`parse_line` and `to_line` are the format's own JSON spelling, and they
agree with each other. Nothing is ever edited: taking a statement back is
one more claim.

**The log.** `Log::append` writes to the open head, `Log::seal` closes it
into a sealed segment named by its own digest, `Log::segments` and
`Log::read` read them back. `Manifests` keep the segment list answerable
without walking the store.

**The index.** `Index` is a fold of the log into SQLite and is a cache in
the strict sense — every answer it gives is derivable from the log alone.
`about` answers the whole history of one subject, `values` and `values_in`
what stands, `find` the query language's terms, `under` a place in the
forest, `standing_as_of` the record as it was known at a moment,
`resolve` and `matching` a shortened hex name, `worklist` what an
extractor has not examined yet.

**The verbs.** `ingest` walks roots and takes files in; `examine` runs
extractor programs over their worklists — identify, hand over the bytes,
funnel the answer through the claim grammar, write the receipt — and
narrates itself to an `Observer` rather than to a terminal; `annotate`
puts the user's word on files already on the record; `lay_out` decides
where an export's files land; `audit_store` and `audit_log` prove the
archive against itself.

Errors are one `Error` enum. `Error::spelled` is the sentence a caller
can hand to a user without rewording it.

## Using it

The crate is a workspace member and is depended on by path:

```toml
[dependencies]
ossuary-core = { path = "../ossuary-core" }
```

It forbids unsafe code, and the SQLite it uses is bundled — no system
library is needed to build it.

```console
$ cargo test -p ossuary-core
```

## License

Apache License 2.0 — see [LICENSE](../LICENSE).
