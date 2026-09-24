# The ossuary archive format

*Format generation 1, draft. This document is the reading contract for
an ossuary archive: what is stored on disk, and how the content and
every claim ever made about it can be reconstructed from the files with
general-purpose tools, without this software. An archive must remain
readable whatever becomes of this project.*

## Principles

- **Stored data and query indexes are separate.** The data that must
  last for decades is append-only and simple. Everything used to answer
  queries is derived from it, can be deleted, and can be rebuilt at any
  time.
- **The bootstrap rule.** Nothing needed to *read* the archive may be
  stored only inside it. Files describe their own form; the per-archive
  constants are in a plain file at the root; claims describe content,
  never how the archive is to be read.
- **The fifty-year rule.** Line-delimited JSON, UTF-8, zstd, and hashes
  that coreutils can check. Reading the archive must not depend on any
  particular software project continuing to exist.

## Layout

```
archive/
    FORMAT          format generation and per-archive parameters
    config.toml     settings for writers; not needed for reading
    content/        immure store: the ingested files
    derived/        immure store: files produced by extractors
    claims/         immure store: sealed claim segments only
    head.jsonl      the open segment
    cache/          rebuildable, safe to delete at any time
```

`content/`, `derived/` and `claims/` are ordinary
[immure](https://github.com/sniner/immure) stores. Everything immure
specifies about entries (naming, sharding, the `.zst` and `.zst.enc`
forms, the sealed frame) applies here and is documented there.

The directories differ in what it costs to lose them. `content/` and
`claims/` *are* the archive: they cannot be replaced and are the first
to be replicated. Losing `derived/` is bad but not fatal: current tools can
extract it again, but what older tool generations extracted would be
lost. Losing `cache/` costs only the time to rebuild it. This order is
also the order of priority for backups.

## The FORMAT mark

One line of JSON in the archive root, so that `cat` shows what the
directory is, and whether a given build of the software may open it,
before anything else is read:

```json
{"ossuary-archive": 1, "algorithm": "sha256", "content-depth": 2, "derived-depth": 2, "claims-depth": 1}
```

- `ossuary-archive`: the format generation the archive is written in.
  A reader that does not know the number stops. It must not recognise
  the layout by its structure instead, because that only identifies the
  layouts that existed when the reader was written.
- `algorithm`: the hash that names every blob in every store: `sha256`
  (default), `sha384`, `sha512` or `blake3`. immure keeps no
  configuration of its own, so the archive records what is needed to
  open the stores.
- `content-depth`, `derived-depth`, `claims-depth`: the stores' shard
  depths.

The mark does not contain what generation 1 already defines: the
directory names, the suffixes (`content/` entries have none, `claims/`
entries have `.seg`), and the claim format below. Whether entries are
compressed or sealed is not recorded anywhere; each file shows it by
its form suffix.

## The configuration

`config.toml` in the root holds the archive's settings. As of
generation 1: which paths ingest excludes, whether new entries in the
content and derived stores are compressed, and which extractors a bare
`ossuary extract` runs. It applies only to
software that writes into the archive. Reading needs none of it,
because every stored file shows its own form, so the bootstrap rule is
not affected. A reader may ignore the file, and the file may be absent:
without it, nothing is excluded and content is stored uncompressed. New
keys may be added without a new generation, but a writer must not act
on a configuration it only partly understands.

## The content stores

`content/` holds what was ingested: the originals. `derived/` holds
what tools made from them (extracted text, unpacked attachments). This
is content as well, linked to its origin by `prov:origin` claims. The
two stores rank differently, and the directory boundary enforces this:
ingested content and content made by a tool are never mixed, so nothing
that maintains `derived/` can reach the originals. The same bytes may
be stored in both stores, for example an invoice ingested as a file and
extracted again as an attachment. A subject names the content in
whichever store it is; the log does not record which store holds it. A
reader looks in `content/` first. A copy in `derived/` of bytes that
`content/` also holds is therefore the only thing an archive can delete
without loss, and `ossuary maintain weed` deletes it after verifying
both copies against their name. The only case in which bytes are
copied from `derived/` to `content/` is the repair offered by the same
command, when the original is damaged and the derived copy is intact:
the damaged original is moved aside under its store's quarantine name,
never deleted, and the verified bytes are stored in `content/` under
the name the original had there.

Entries in both stores have no suffix, because no suffix would be
correct for all of them: the stores hold every kind of file, and the
type of a blob is recorded in the claim log (`file:mime`), never in a
file name. A raw entry is byte-identical with its content: the output
of `sha256sum <file>` is the file's own name. A store must not contain
anything whose name could collide with a bare hex name.

## Subjects

A claim is about a subject: a blob, named by its content. The name is
the digest as bare lowercase hex, full length, with nothing before or
after it:

```
9f2ac41e…      a blob, named by its content
```

The algorithm that made the name is recorded once for the archive, in
the mark; no name repeats it. The path in the store is the same hex,
sharded as the mark specifies, so the subject that `about` shows and
the entry in the store have the same name.

## Claims

All metadata is stored as claims: small, self-describing, append-only
statements. One claim is one JSON object on one line (UTF-8, LF, no
line breaks within a claim):

```json
{"subject":"9f2ac41e…","attribute":"file:path","value":"/photos/2019/crete/beach.jpg","time":"2026-09-01T21:14:03Z","source":"ingest","run":"315e360b-020e-48be-8f2d-f2002a2ea9b4"}
{"subject":"9f2ac41e…","attribute":"file:size","value":4194304,"time":"2026-09-01T21:14:03Z","source":"ingest","run":"315e360b-020e-48be-8f2d-f2002a2ea9b4"}
{"subject":"9f2ac41e…","attribute":"exif:date-time-original","value":"2019-07-14T11:02:41","time":"2026-09-22T08:30:00Z","source":"extractor:exif-rs/0.7","run":"c7a1e2d4-9b3f-4e60-8a15-2f6d0c9b7e31"}
{"subject":"9f2ac41e…","attribute":"user:tag","value":"holiday","time":"2026-10-05T19:00:00Z","source":"user","run":"a3f8c2e1-5d47-4b9a-9e02-7c1d3b6f8a54"}
{"subject":"9f2ac41e…","attribute":"user:tag","value":"holiday","time":"2030-04-01T10:00:00Z","source":"user","run":"0b9d4e7a-2c31-4f58-b6a0-e5d7f1c2a983","retract":true}
```

*(Digests shortened here for legibility; real ones are full-length hex.)*

| Field       | Required | Holds                                                        |
| ----------- | -------- | ------------------------------------------------------------ |
| `subject`   | yes      | What the claim is about; see [Subjects](#subjects)           |
| `attribute` | yes      | What the claim states; see [Attributes](#attributes)         |
| `value`     | see note | Any JSON value except `null`                                  |
| `time`      | yes      | When it was recorded: RFC 3339, UTC, `Z`, whole seconds       |
| `source`    | yes      | Who made the claim: a flat string, `kind:name/version`        |
| `run`       | see note | The call it was written in: a UUID, dashed, lowercase         |
| `retract`   | no       | `true` on a retraction; absent otherwise                      |

These seven fields are the complete set in generation 1. Nothing is
ever updated or deleted in place: a correction is a newer claim, a
deletion is a retraction, and the log only grows. A claim once written
stays in the log, so it is always possible to ask what was known about
a file at an earlier time, for example in 2027.

**Value.** A value may be any JSON type; `file:size` is a number, not a
number in quotes. `null` is not a value, and a claim that is not a
retraction always has one.

**Time** is when the claim was recorded, not when the thing it
describes happened: `exif:date-time-original` above has a value from
2019 in a claim from 2026. Within one second, claims are ordered by
their position in the log: line order within a segment, segment order
across segments.

**Source** is `ingest`, `user`, or `kind:name/version` for tools, such
as `extractor:exif-rs/0.7`. The grammar is flat so that a fold can
supersede claims by prefix: "everything from `extractor:exif-rs/` older
than 2.0".

**Run** is the call the claim was written in: one id per invocation of
a program that writes claims (an ingest, an extractor pass, a fetch, an
annotation or a retraction by the user), stored on every claim of that
call. `source` records who made a claim and `time` when; `run` records
which call wrote it. This makes "stored together" and "retracted in one
run" exact, and a point in the log can be identified by the call that
ended there. The id is a dashed lowercase UUID, a form no subject has.
Every claim this software writes has one. A claim written before runs
were recorded has none and is read like any other claim; it belongs to
no run, and a query by run does not find it.

**Retraction.** A claim with `retract: true` and a `value` retracts
exactly that value of that attribute; with the `value` field absent, it
retracts every value the attribute had for this subject. A retraction
is a claim like any other, with its own time and source, and it never
removes the retracted claim from the log.

## Attributes

`namespace:attribute`, lowercase `a-z`, `0-9` and `-`, one colon.
Unknown attributes are valid: a claim that no program understands yet
is kept like any other and is not an error. The namespaces in use
(`prov:`, `file:`, `exif:`, `user:`, `mail:`, …) and their vocabulary
are documented separately; the format does not depend on them.

## Segments

Claims are stored in segments. A segment is a JSONL file whose first
line is its header, a JSON object that names the segment's format:

```json
{"ossuary-segment": 1}
```

Every line after it is a claim, in the order recorded.

From the second segment of an archive on, the header also names the
segment sealed before it, by digest (the hex name under which the
claims store keeps it):

```json
{"ossuary-segment": 1, "previous": "3c1e…"}
```

`ossuary-segment` is the only member every header has; `previous` is
absent only from the first segment of an archive. A reader skips header
members it does not know: generation 1 may gain members that add
information to a header, and none of them changes how the claims after
it are read.

The open segment is `head.jsonl` in the archive root. It has the same
format, claims are appended to it as they are written, and it is the
only mutable file in the archive. Sealing closes it: the file is stored
in `claims/` as an ordinary immure entry (zstd-compressed, and sealed if
the store is), and a new `head.jsonl` is started. When to seal is up to
the software; a reader must not assume anything about a segment's size.

A sealed segment is immutable like everything else in a store, and the
format adds its own rule: **segments are never compacted, merged or
rewritten.** Superseded and retracted claims stay where they were
written.

The order of segments (needed only to order claims from the same second
across segments) is the order of their first claims' `time`, then the
segment digest.

**The chain.** Sealing stores the head and starts a new one whose
header names the segment just sealed. Every segment except the first
therefore names the one before it, and the open head names the last
sealed segment. A store can prove that its entries are unchanged, since
every name is a checksum, but not that none is missing; the chain shows
that. If a sealed segment goes missing, the header of the next segment,
or of the head, has a `previous` that names no existing segment. The
chain cannot show a loss at its end: the head is the only mutable file,
and a head rewritten to name an earlier segment leaves a chain that
looks complete. Keeping the digest of the latest segment somewhere
outside the archive closes that gap.

**The mend.** A break in the chain is closed by adding a segment, never
by rewriting one. A mend is a segment without claims whose header names
the two ends it joins:

```json
{"ossuary-segment": 1, "previous": "3c1e…", "mend": {"before": "9b07…", "replaces": "5d2a…"}}
```

`previous` is the last segment before the break, as in any header.
`mend` marks the segment as a mend. `before` names the segment the mend
is placed in front of; that segment is sealed and cannot be changed to
name the mend, so a reader finds the mend through this member instead.
`replaces` names the lost segment, if its name is known: it is the
segment that the segment after the break names as its `previous`. Both
members of `mend` are optional. A mend without `before` is placed in
front of the open head, which names the mend as its `previous` like any
other segment; when that head is sealed, the resulting segment names
the mend in the same way.

A reader builds the chain with mends applied: a segment whose
`previous` is absent or not in the store is preceded by the mend whose
`before` names it, if there is one, and the chain continues from that
mend's `previous`. The segment after the break still names the segment
it named before, so the record still shows what was lost and where. A
mend in front of a segment whose `previous` is in the store closes no
gap and is not part of the chain.

## Caches

Everything under `cache/` is derived from the stores and `head.jsonl`,
and deleting it loses nothing. What is kept there (a list of sealed
segments, per-segment manifests, query indexes in SQLite, full-text or
whatever the queries need) is internal to the software and deliberately
not specified: no cache is ever authoritative, and none is part of the
format.

## Recovery

Reconstructing the archive from the directory tree and, if the stores
are sealed, the key:

1. Read `FORMAT`: generation, algorithm, depths.
2. Walk `claims/`, decompress (`zstd -dc`) and unseal each entry, check
   that the first line has `ossuary-segment`, and order the segments as
   above. Every `previous` a header names must be among them, or a mend
   must be placed in front of the segment that names it; otherwise the
   named segment is missing.
3. Concatenate, append `head.jsonl`: this is the complete claim log.
4. Walk `content/` and `derived/` the same way for the content itself;
   each file can be checked against its name with the algorithm's
   checksum tool.
5. Fold the log into whatever index the current queries need.

None of these steps needs this project: a shell, `zstd`, `jq` and the
coreutils hash tools are enough. Step 5 is optional; `grep` over the
log already shows everything recorded about a blob.

## Evolution

Generation 1 is this document. Any change to how these files are
*read* (a claim field beyond the seven, a header member a reader must
understand to read the claims after it, a changed layout) requires a
new generation. The mark and the segment header exist so that a reader
rejects what it does not know instead of guessing. The following may be
extended without a new generation: the attribute vocabulary, source
kinds, the configuration's keys, header members that only add
information to a header, and everything under `cache/`.
