# ossuary-extract-packed

*A zip archive in, its inventory or its files out.*

An [extractor](../docs/extractors.md) for ossuary, and the first that
carries two trades in one program. It reads a zip's bytes from stdin and
answers, depending on which contract was asked for. It never touches the
archive.

## Two contracts

| | |
|---|---|
| `packed:list` | tells every entry the archive holds, one `zip:entry` finding each, **without unpacking a byte** |
| `packed:unpack` | writes every entry out as a derived file of its own |

Each has its own source, its own worklist and its own receipts — a zip
inventoried is not a zip unpacked, and to the record that both live in
one binary is invisible. That is what makes "inventory the archives,
never unpack them" a policy one can actually state:

```toml
[extract]
run = ["packed:list"]
```

## What it puts on the record

`list` answers the inventory, entry names as the zip spells them, sorted
so the answer reads the same however the zip was written. Directories are
structure, not content, and stay untold:

```
zip:entry = "invoices/2026-03.pdf"
zip:entry = "notes.txt"
```

`unpack` announces each entry as a derived file, taken into the archive
with `derive:derived-from` naming the zip. An announced name is bare, so
inner paths are flattened and the full path goes on the record as
`zip:path`; where two names collide a counter slips in before the
extension and the name the zip spelled stands beside it as `file:name`.
A zip declares no kinds, so each announcement carries the same
magic-bytes-then-UTF-8 look ingest would take.

## Not every zip is an archive

epub and the OpenDocument family open with a first entry named `mimetype`
holding nothing but their own kind; OOXML carries `[Content_Types].xml`
at its root. These are documents wearing zip as an envelope, and nobody
wants them shredded into XML innards. Both contracts recognize them by
the container's own construction, stay shut, and answer with the sharper
`file:mime` instead — said by the bytes, standing beside the sniffed
`application/zip` — so an extractor reading the sharper kind can find
them.

A jar stays an ordinary archive: it promises nothing about its insides.
Bytes that do not read as a zip at all are an examination with nothing
found.

## What stays inside

* **An encrypted entry.** There is no password to offer, and a receipt
  beats being offered the same locked door every run.
* **A damaged entry**, or one whose spelling holds no file name.
* **A symlink**, silently — its bytes are a name rather than content, and
  nothing is lost.

For the first two the reason goes on the record as a `prov:note` finding
and onto stderr in the same words: a zip that unpacked incompletely must
not read like one that unpacked whole. One refused entry costs no other
entry its examination.

Only failing to write an entry's file is a failure. Note that unpacking
holds one entry in memory at a time and writes it out whole.

## Kinds it reads

`application/zip`, for both contracts. Other container formats — tar,
7z, rar — are another program's business.

## Running it

Put the binary on the PATH beside `ossuary`, then:

```console
$ ossuary extract packed:list      # one contract
$ ossuary extract packed           # both, each on its own worklist
```

Testable by hand — the contract's name comes first, and `unpack` takes
the output directory after it:

```console
$ ossuary-extract-packed --identify
$ ossuary-extract-packed list < bundle.zip
$ mkdir /tmp/out && ossuary-extract-packed unpack /tmp/out < bundle.zip
```

## License

Apache License 2.0 — see [LICENSE](../LICENSE).
