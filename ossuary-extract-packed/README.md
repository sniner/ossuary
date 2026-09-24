# ossuary-extract-packed

Lists the entries of a zip file, or extracts them as derived files.

An [extractor](../docs/extractors.md) for ossuary with two contracts. It
reads a zip file from stdin and prints the results of one contract. It
does not access the ossuary archive; `ossuary extract` runs it and
records the results.

## Contracts

| | |
|---|---|
| `packed:list` | records every entry as a `packed:path` claim, without extracting anything |
| `packed:unpack` | extracts every entry as a derived file |

Each contract has its own source and its own receipts: a zip file that
was listed does not count as unpacked. To list zip files without ever
unpacking them:

```toml
[extract]
run = ["packed:list"]
```

## Recorded attributes

A location inside a zip file is written as a leading `@` followed by the
entry's path as stored in the zip. `list` records every file entry this
way, sorted by path. Directories are not recorded:

```
packed:path = "@invoices/2026-03.pdf"
packed:path = "@notes.txt"
```

`unpack` extracts each entry as a derived file, with `prov:origin`
pointing to the zip file. The file is written under its bare file name,
and the entry's full path is recorded as its `file:path`, the same `@`
value that `list` records. `find file:path=*/2026-03.pdf` therefore finds
the file whether it came from a disk or from a zip file. If two names
collide, a counter is added before the extension; a name too long for
the filesystem is shortened. In either case the file name from the zip
is recorded as `file:name`.

A zip file does not declare file types, so each entry's type is detected
the same way ingest detects it: magic bytes first, then a check for
UTF-8 text. Entries are streamed to disk, not loaded into memory.

## Documents in zip format

EPUB and OpenDocument files start with an entry named `mimetype` that
contains their MIME type; OOXML files contain `[Content_Types].xml` at
the root. Both contracts recognize these formats, neither list nor
unpack them, and record the more specific `file:mime` instead, in
addition to the `application/zip` detected at ingest. An extractor for
that type can then find them. An OOXML file that is not a Word, Excel or
PowerPoint document gives an empty result.

A jar file is treated as an ordinary zip file. Bytes that are not a
readable zip file give an empty result.

## Entries that are not extracted

* **Encrypted entries.** There is no way to supply a password.
* **Damaged entries**, and entries whose path has no file name.
* **Entries larger than 1 GiB** when unpacked.
* **Symbolic links.**

For each of these except symbolic links, the reason is recorded as a
`prov:note` claim on the zip file and printed on stderr:

```
prov:note = "entry secret.txt not unpacked: encrypted"
```

The other entries are extracted as usual, and the zip file gets its
receipt. Only a failure to write an extracted file is an error.

## Supported types

`application/zip`, for both contracts. Other formats (tar, 7z, rar) are
not supported.

## Running it

Put the binary on the PATH next to `ossuary`, then:

```console
$ ossuary extract packed:list      # one contract
$ ossuary extract packed           # both contracts
```

To run it by hand, give the contract name first; `unpack` takes the
output directory as second argument:

```console
$ ossuary-extract-packed --identify
$ ossuary-extract-packed list < bundle.zip
$ mkdir /tmp/out && ossuary-extract-packed unpack /tmp/out < bundle.zip
```

## License

Apache License 2.0 (see [LICENSE](../LICENSE)).
