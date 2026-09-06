# ossuary — a personal archive of everything, with everything known about it

> [!WARNING]
> This is pre-0.1.0 software. Anything may change from one commit to the
> next — the commands, the format, and any archive you have created may
> stop being readable, with no migration path. It is ready to be played
> with, and for nothing more. Once there are releases, the usual
> versioning rules take over.

`ossuary` keeps files for good and writes down everything that is ever
said about them. A file goes in once and is named by the hash of its own
content — the same bytes are never stored twice, and the name never
changes. Everything known about a file — where it came from, what a tool
read inside it, what you yourself had to say — accretes as an
append-only record of *claims*: each one sourced and dated, none ever
rewritten. Even taking a statement back is one more entry, not an
erasure.

It is an archive, not a backup tool: it does not mirror a disk or
restore the state of a particular day; it holds what you decided to
keep and answers questions about it — for decades, independent of file
formats, software generations, or the tool that wrote it. What people
would use it for:

* **A home for what must not get lost.** Photos, documents, saved mail,
  finished projects — poured in from as many places as they sit in.
  Duplicates recognise themselves, and pouring the same directory in
  again costs only what is new or changed.
* **Answers about any file.** What is this, where did it come from, on
  which machines did it sit, what arrived together with it — asked by a
  name that is the file's own, not a path that stopped existing years
  ago.
* **Metadata harvested, verbatim.** EXIF fields, a PDF's text and
  document info, a mail's headers — and what a mail or a zip *carries*
  becomes content of its own: attachments, nested messages, unpacked
  entries, each tied to its origin on the record.
* **Your own words on the record.** Tags at arrival, tags and comments
  any time later — beside the machine's findings, under your own name.
* **Everything back out again.** One file by its name, or a whole batch
  laid down the way it arrived.

## Building it

There are no releases yet; the way in is cargo:

```console
$ git clone https://github.com/sniner/ossuary
$ cd ossuary && cargo build --release
```

That builds the `ossuary` command and the extractors
(`ossuary-extract-exif`, `-text`, `-mail`, `-packed`); put them on your
`PATH` together — extractors are found there, not built in.
`ossuary-extract-text` additionally wants poppler's `pdftotext` on the
`PATH`.

## A new archive

An archive is a directory that says so — a one-line `FORMAT` file names
the layout, a `config.toml` carries the settings, and everything else
lives beneath it. `init` puts one wherever `--archive` points:

```console
$ ossuary --archive /home/john/archive init
/home/john/archive: an empty archive — its settings stand in config.toml; take files in with `ossuary ingest DIR`
```

Every command takes `--archive`; standing inside the archive is enough,
and so is setting `OSSUARY_ARCHIVE` once for the whole shell session —
which is what the examples below do.

## Taking files in

`ingest` (or `add`) takes directory trees and single files, any mix,
and everything of one call arrives as one *run* — so "these arrived
together" stays an askable fact:

```console
$ cd /home/john
$ ossuary ingest photos mail docs
archive /home/john/archive
taking in 3 paths
5 file(s) new to the archive; 35 claim(s) written as run 315e360b-020e-48be-8f2d-f2002a2ea9b4
```

Seven claims per file: its path, its name, the host, the run, its size,
its kind (sniffed from the bytes, not the file name), and when it last
changed. Files are only ever read. Run it again and nothing happens
twice:

```console
$ ossuary ingest photos mail docs
0 file(s) new to the archive, 5 unchanged since the last run and left in peace; nothing new to record
```

`--tag holiday` puts your own word on everything a run records, and
`ossuary id FILE` answers what a file would be called — and whether the
archive already holds it — without taking anything in.

## Looking inside: extractors

Extractors are separate programs, one per format family, speaking a
[small pipe protocol](docs/extractors.md) open to any language. They
never touch the archive — bytes in, findings out — and everything they
say goes on the record under their own name and version:

```console
$ ossuary extract mail
2 file(s) waiting for extractor:mail/0.1.0
2 file(s) examined by extractor:mail/0.1.0, 13 claim(s) written; 1 derived file(s) taken in; 1 had nothing to tell
$ ossuary extract packed:list
1 file(s) waiting for extractor:packed-list/0.1.0
1 file(s) examined by extractor:packed-list/0.1.0, 3 claim(s) written
```

The mail extractor read both files that sniff as text, recognised one
as a message, recorded its headers and handed its attachment over as
content of its own; the plain note "had nothing to tell" — which is an
answer too, and neither file is examined again. `packed:list`
inventoried a zip without unpacking a byte. Every examined file gets a
receipt, so a repeated run costs only what is new, and a new extractor
version looks at everything again.

List extractors under `[extract] run` in `config.toml` and a bare
`ossuary extract` runs them in rounds until nothing is left — what one
extractor hands back, the next round offers to whichever extractor
reads it, so mail → attachment → text runs to its end in one call.

## What the archive knows

`about` answers with the whole record of one file, oldest first — and a
beginning of the name is enough while it names only one file:

```console
$ ossuary about e9ed6104
2026-09-06T15:23:40Z  file:path = "/home/john/mail/2026-03-10-quarterly.eml"  [ingest]
2026-09-06T15:23:40Z  file:name = "2026-03-10-quarterly.eml"  [ingest]
2026-09-06T15:23:40Z  prov:host = "atlas.example.net"  [ingest]
2026-09-06T15:23:40Z  prov:run = "315e360b-020e-48be-8f2d-f2002a2ea9b4"  [ingest]
2026-09-06T15:23:40Z  file:size = 491  [ingest]
2026-09-06T15:23:40Z  file:mime = "text/plain"  [ingest]
2026-09-06T15:23:40Z  file:modified = "2026-09-06T15:23:40.89362092Z"  [ingest]
2026-09-06T15:23:40Z  file:mime = "message/rfc822"  [extractor:mail/0.1.0]
2026-09-06T15:23:40Z  mail:from = "Erika Muster <erika@example.org>"  [extractor:mail/0.1.0]
2026-09-06T15:23:40Z  mail:to = "John Doe <john@example.net>"  [extractor:mail/0.1.0]
2026-09-06T15:23:40Z  mail:subject = "Quarterly figures"  [extractor:mail/0.1.0]
2026-09-06T15:23:40Z  mail:date = "Tue, 10 Mar 2026 14:22:05 +0100"  [extractor:mail/0.1.0]
2026-09-06T15:23:40Z  mail:message-id = "<74a2f19c@mail.example.org>"  [extractor:mail/0.1.0]
2026-09-06T15:23:40Z  prov:examined = true  [extractor:mail/0.1.0]
```

Every line says who said it and when — the sniffed `text/plain` and the
mail extractor's sharper `message/rfc822` both stand, because the
record keeps every word and choosing between them is the reader's
business, not the archive's. Naming attributes narrows the answer:
`ossuary about e176bedf zip:` is everything the zip inventory recorded
about one archive file.

## Finding files

`find` takes `attribute=value` terms that must all hold, and the
question is also the projection — every attribute the query names is
shown on each match:

```console
$ ossuary find file:mime=message/rfc822 mail:subject mail:from
e9ed6104
  file:mime=message/rfc822
  file:mime=text/plain
  mail:subject="Quarterly figures"
  mail:from="Erika Muster <erika@example.org>"
1 file(s)
```

`*` and `?` match within text values, `low..high` asks for a value in a
range with either side open (`file:modified=2026-01-01..` is "changed
since New Year"), and `--missing exif:` turns the question around:
which photos have no EXIF on record. The attachment from the mail above
is found like any other file, with its origin one term away:

```console
$ ossuary find 'file:name=*.pdf' derive:derived-from
b5743276
  file:name=figures-q1.pdf
  derive:derived-from=e9ed6104c0bea9889000f408b6d855216f6743fa85586281c764c8c69d25a738
1 file(s)
```

For scripts: `find --id` prints full names alone, ready to pipe;
`value` answers one attribute's standing values, strings bare;
`--json` on `about`, `value` and `find` keeps the JSON spelling for
`jq`; and `-q` silences the narration everywhere.

## Your own word

`annotate` puts tags and comments on files already on the record, under
the source `user` — and a found set pipes straight into it:

```console
$ ossuary annotate e9ed6104 --tag taxes --comment "the missing form was in here after all"
1 file(s) annotated, 2 claim(s) written
$ ossuary find user:tag=taxes file:name
e9ed6104
  user:tag=taxes
  file:name=2026-03-10-quarterly.eml
1 file(s)
$ ossuary find --id 'file:name=*.jpg' | xargs ossuary annotate --tag holiday
```

## Getting things back out

`get` hands one file's bytes to stdout or `--output FILE`, exactly as
they went in. `export` lays whole batches back down: give it a run id
and every file that run recorded lands under the path the run saw it
at, kept relative — what lay side by side lands side by side:

```console
$ ossuary export /home/john/refile --dry-run 315e360b-020e-48be-8f2d-f2002a2ea9b4
docs/backup.zip  e176bedf
docs/notes.txt  719aac93
mail/2026-03-10-quarterly.eml  e9ed6104
photos/DSC_1042.jpg  bd84e795
photos/DSC_1043.jpg  6ca81e7a
would export 5 file(s) into /home/john/refile — nothing written
```

Without `--dry-run` that writes the five files. File names and run ids
mix freely in one call, `ossuary find --id … | xargs ossuary export
DIR` exports a found set, and nothing standing at the destination is
ever overwritten — a file already there with the same bytes counts as
done, one with different bytes is a named failure and stays untouched.

## The design, in three sentences

Content and claims are the archive: immutable content-addressed blobs
(via [immure](https://github.com/sniner/immure)), and an append-only
claim log sealed into the same kind of store. What tools derived —
extracted text, unpacked attachments — lives in a store of its own
rank, apart from the originals by topology. Everything else — query
index, manifests, the ingest walk's memory — is a disposable cache in
`cache/`, rebuilt from the log at any time, and deleting it costs a
slow first answer, never a fact.

The format is written down to outlast the software:

* [The archive format](docs/format.md) — the reading contract: layout,
  claims, segments, and how to recover an archive with nothing but a
  shell and patience
* [The attribute vocabulary](docs/vocabulary.md) — what the words mean,
  and how standing claims become an answer
* [The extractor protocol](docs/extractors.md) — how an extractor, in
  any language, tells the archive what it found

## License

`ossuary` is free software under the
[Apache License 2.0](LICENSE).
