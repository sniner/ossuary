# The ossuary archive format

*Format generation 1 — draft. This document is the reading contract for an
ossuary archive: what lies on disk, and how anyone with the files, patience
and general-purpose tools reconstructs everything — the content and everything
ever claimed about it — without this software. Whatever this project becomes,
an archive must survive it.*

## Principles

- **Truth and query are separate systems.** What must survive decades is
  append-only and dumb; what answers questions is derived, disposable, and
  rebuilt at will.
- **The bootstrap rule.** Nothing needed to *read* the archive may live only
  inside it. Files describe themselves; the per-archive constants stand in a
  plain file at the root; claims speak about content, never about how the
  archive is to be read.
- **The fifty-year rule.** Line-delimited JSON, UTF-8, zstd, and hashes that
  coreutils can check. Every dependency on a living software project is a bet
  the archive must not make.

## Layout

```
archive/
    FORMAT          the mark: which generation, and the per-archive constants
    config.toml     the archive's settings — writers obey it, readers never need it
    content/        immure store — what was taken in
    derived/        immure store — what extractors made of the content
    claims/         immure store — sealed claim segments, and nothing else
    head.jsonl      the one open segment
    cache/          disposable, safe to delete at any time
```

`content/`, `derived/` and `claims/` are ordinary
[immure](https://github.com/sniner/immure) stores; everything immure
promises about entries — naming, sharding, the `.zst` and `.zst.enc`
forms, the sealed frame — holds here and is documented there, not
repeated here.

The three directories rank by what losing them costs. `content/` and
`claims/` *are* the archive: irreplaceable, replicated first. `derived/`
is regrettable to lose but not fatal — today's tools can harvest it
again, though what older tool generations once saw would be gone.
`cache/` costs only the time to rebuild it. The ladder is also a backup
policy.

## The FORMAT mark

One line of JSON in the archive root, so that `cat` answers "what is this,
and may this build touch it?" before anything is opened:

```json
{"ossuary-archive": 1, "algorithm": "sha256", "content-depth": 2, "derived-depth": 2, "claims-depth": 1}
```

- `ossuary-archive` — the format generation this archive is written in. A
  reader that does not know the number stops; recognising a layout by its
  structure only ever identifies the layouts that existed when the reader was
  written.
- `algorithm` — the hash that names every blob in every store: `sha256`
  (default), `sha384`, `sha512` or `blake3`. immure deliberately keeps no
  configuration of its own, so the archive carries what the stores need to be
  opened.
- `content-depth`, `derived-depth`, `claims-depth` — the stores' shard depths.

What is *not* in the mark is everything generation 1 already fixes: the
directory names, the suffixes (`content/` entries carry none, `claims/`
entries carry `.seg`), and the claim format below. Whether entries are
compressed or sealed is not recorded anywhere at all — each file says so
itself, by its form suffix.

## The configuration

`config.toml` in the root is the archive's own settings — as of generation
1: which paths ingest leaves out, and whether new entries in the content
and derived stores are compressed. It is write policy, for software putting things *in*; the
bootstrap rule stays untouched because reading needs none of it — every
stored file already says its own form. A reader may ignore the file
entirely, and the file may be absent entirely: no file means nothing is
excluded and content is stored as it came. Its keys may grow without a new
generation, but a writer must not act on a config it only partly
understands.

## The content stores

`content/` holds what was taken in: the originals. `derived/` holds what
tools made of them — extracted text, unpacked attachments — content too,
linked to its origin by `derive:` claims. The split is a rank, and the
directory boundary enforces it: what was taken in and what a tool made
do not mingle, so nothing that ever maintains `derived/` can reach the
originals. The same bytes may lawfully stand in both stores — an invoice
taken in as a file and won again as an attachment — and a subject names
content wherever it lies; the log neither knows nor cares which store
answers.

Entries in both stores carry no suffix, because there is nothing truthful
to write: the stores are heterogeneous by design, and what a blob *is*
lives in the claim log (`file:mime`), never in a file name. A raw entry is
byte-identical with its content — `sha256sum <file>` answers to the file's
own name — and what a bare hex name would collide with, a store simply
must not contain.

## Subjects

A claim is about a subject: a blob, named by its content — the digest as
bare lowercase hex, full length, nothing around it:

```
9f2ac41e…      a blob, named by its content
```

Which algorithm made the name is the mark's word, said once for the whole
archive — no name repeats it. The path in the store is this same hex,
sharded as the mark says: what `about` answers for and what the store
holds go by one spelling.

## Claims

All metadata is claims: small, self-describing, append-only facts. One claim
is one JSON object on one line — UTF-8, LF, no line breaks within a claim:

```json
{"subject":"9f2ac41e…","attribute":"file:path","value":"/photos/2019/crete/beach.jpg","time":"2026-09-01T21:14:03Z","source":"ingest"}
{"subject":"9f2ac41e…","attribute":"file:size","value":4194304,"time":"2026-09-01T21:14:03Z","source":"ingest"}
{"subject":"9f2ac41e…","attribute":"exif:date-time-original","value":"2019-07-14T11:02:41","time":"2026-09-22T08:30:00Z","source":"extractor:exif-rs/0.7"}
{"subject":"9f2ac41e…","attribute":"user:tag","value":"holiday","time":"2026-10-05T19:00:00Z","source":"user"}
{"subject":"9f2ac41e…","attribute":"user:tag","value":"holiday","time":"2030-04-01T10:00:00Z","source":"user","retract":true}
```

*(Digests shortened here for legibility; real ones are full-length hex.)*

| Field       | Required | Holds                                                        |
| ----------- | -------- | ------------------------------------------------------------ |
| `subject`   | yes      | What the claim is about — see [Subjects](#subjects)           |
| `attribute` | yes      | What is being said — see [Attributes](#attributes)            |
| `value`     | see note | Any JSON value except `null`                                  |
| `time`      | yes      | When it was said: RFC 3339, UTC, `Z`, whole seconds           |
| `source`    | yes      | Who says so — a flat string, `kind:name/version`              |
| `retract`   | no       | `true` on a retraction; absent otherwise                      |

These six fields are the complete set in generation 1. Nothing is ever
updated or deleted in place: a correction is a newer claim, a deletion is a
retraction, and the log only grows. That a claim was once made remains true
forever — which is what makes "what did I know about this in 2027?" a valid
question.

**Value.** `file:size` is a number, not a number in quotes; a value may be
any JSON type. `null` is not a value, and a normal claim always carries one.

**Time** is when the claim was recorded, not when whatever it describes
happened — `exif:date-time-original` above says 2019 inside a claim from
2026. Within one second the order of claims is the order of the log: line
order within a segment, segment order across them.

**Source** is `ingest`, `user`, or `kind:name/version` for tooling —
`extractor:exif-rs/0.7`. The grammar stays flat so a fold can supersede by
prefix: "everything from `extractor:exif-rs/` older than 2.0".

**Retraction.** A claim with `retract: true` and a `value` retracts exactly
that value of that attribute; with the `value` field absent entirely, it
retracts every value the attribute had for this subject. A retraction is a
claim like any other — stamped, sourced, and never removing what it retracts
from the log.

## Attributes

`namespace:attribute`, lowercase `a-z`, `0-9` and `-`, one colon. Unknown
attributes are legal — a claim nobody understands yet is a queue entry, not
an error. The namespaces in use (`prov:`, `file:`, `exif:`, `user:`,
`derive:`, …) and their vocabulary are documented separately; the format
does not depend on them.

## Segments

Claims live in segments. A segment is a JSONL file whose first line names
its own format:

```json
{"ossuary-segment": 1}
```

Every line after it is a claim, in the order recorded.

The one open segment is `head.jsonl` in the archive root — the same format,
appended to as claims arrive, and the only mutable file in the archive.
Sealing closes it: the file is stored into `claims/` as an ordinary immure
entry (zstd-compressed, sealed if the store is), and a fresh `head.jsonl`
begins. When to seal is the software's business; a reader assumes nothing
about a segment's size.

A sealed segment is immutable like everything else in a store, and this
archive adds its own vow: **segments are never compacted, merged or
rewritten.** Superseded and retracted claims stay where they were written —
for an archive that is not a limitation but the point.

The order of segments — needed only to break same-second ties across them —
is the order of their first claims' `time`, then the segment digest.

## Caches

Everything under `cache/` is derived from the stores and `head.jsonl`, and
deleting it loses nothing. What accumulates there — a list of sealed
segments, per-segment manifests, query indexes (SQLite, full-text, whatever
the questions call for) — is a private matter of the software and
deliberately unspecified: no cache is ever authoritative, and none survives
a format discussion.

## Recovery

The whole archive, from the tree and (if sealed) the key:

1. Read `FORMAT`: generation, algorithm, depths.
2. Walk `claims/`, decompress (`zstd -dc`) and unseal each entry, check the
   first line says `ossuary-segment`, order the segments as above.
3. Concatenate, append `head.jsonl`: this is the complete claim log.
4. Walk `content/` and `derived/` the same way for the content itself;
   every byte answers to its name via the algorithm's checksum tool.
5. Fold the log into whatever index answers today's questions.

Nothing above needs this project — a shell, `zstd`, `jq` and the coreutils
hash tools suffice, and step 5 is optional: `grep` over the log already
answers "what do I know about this blob".

## Evolution

Generation 1 is this document. Anything that would change how these files
are *read* — a claim field beyond the six, a different segment header, a
changed layout — is a new generation: the mark and the segment header exist
so that a reader refuses what it does not know instead of guessing. What may
grow freely without a new generation: attribute vocabulary, source kinds,
the configuration's keys, and everything under `cache/`.
