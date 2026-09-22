# ossuary — a personal archive of everything, with everything known about it

> [!NOTE]
> This is 0.x software. Commands and format may still change between
> minor versions — a breaking change bumps the minor and names itself
> in the [CHANGELOG](CHANGELOG.md).

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

## Getting it

Every [release](https://github.com/sniner/ossuary/releases) carries the
programs built: for Linux on x86_64 and arm64, statically linked, and
for macOS as universal binaries — one file per program, named after the
program, the version and the platform. Building it yourself is the
other way in:

```console
$ git clone https://github.com/sniner/ossuary
$ cd ossuary && cargo build --release
```

Either way, put all of them on your `PATH` together — extractors and
outside verbs like `mount` are found there, not built in. Each crate
says for itself what it is and what it needs:

| | |
|---|---|
| [`ossuary-cli`](ossuary-cli/README.md) | the `ossuary` command — every verb, from `init` to `audit` |
| [`ossuary-core`](ossuary-core/README.md) | the archive itself: claims, segments, the fold. What everything else stands on |
| [`ossuary-mount`](ossuary-mount/README.md) | the record as a read-only filesystem |
| [`ossuary-mailvault`](ossuary-mailvault/README.md) | mail into the archive: whole mailboxes fetched over IMAP |
| [`ossuary-extract-image`](ossuary-extract-image/README.md) | what a camera wrote into the picture, and the pixel grid |
| [`ossuary-extract-mail`](ossuary-extract-mail/README.md) | a message's own voice, and what it carries |
| [`ossuary-extract-packed`](ossuary-extract-packed/README.md) | a zip's inventory, or its files |
| [`ossuary-extract-pdf`](ossuary-extract-pdf/README.md) | a document's text and info, or its attachments — wants poppler's `pdftotext` |

One program is missing from that list on purpose: [`ossuary-fix`](ossuary-fix/README.md)
repairs 0.x archives after a breaking change and is not a regular part
of the package. Its own README says what it does and when it should not
be used.

## A new archive

An archive is a directory that says so — a one-line `FORMAT` file names
the layout, a `config.toml` carries the settings, and everything else
lives beneath it. `init` puts one wherever `--archive` points:

```console
$ ossuary --archive /home/john/archive init
/home/john/archive: an empty archive, settings in config.toml; take files in with `ossuary ingest DIR`
```

Every command takes `--archive`; standing inside the archive is enough,
and so is setting `OSSUARY_ARCHIVE` once for the whole shell session —
which is what the examples below do.

## Taking files in

`ingest` takes directory trees and single files, any mix,
and everything of one call arrives as one *run* — so "these arrived
together" stays an askable fact:

```console
$ cd /home/john
$ ossuary ingest photos mail docs
archive /home/john/archive
taking in 3 paths
5 file(s) stored; 30 claim(s) written, run 315e360b-020e-48be-8f2d-f2002a2ea9b4
```

Six claims per file: its path, its name, the host, its size, its kind
(sniffed from the bytes, not the file name), and when it last changed —
and every claim carries the run it was written in. Files are only ever
read. Run it again and nothing happens twice:

```console
$ ossuary ingest photos mail docs
0 file(s) stored, 5 unchanged since the last run; 0 claims written
```

`--tag holiday` puts your own word on everything a run records, and
`ossuary id FILE` answers what a file would be called — and whether the
archive already holds it — without taking anything in.

A run also notices what is gone. Delete two of the files and run again:

```console
$ ossuary ingest photos mail docs
0 file(s) stored, 3 unchanged since the last run, 2 no longer at their place, taken off the record; 2 claim(s) written, run 9c1d0f3e-…
```

The files are still in the archive, with everything ever recorded about
them, and `--as-of` before that run still shows them where they were —
only their *place* was taken back, one more claim in the log, so
questions about today no longer answer with them. Not seen is not gone:
a directory that would not open, a path `config.toml` excludes, and a
directory the walk meets not one file under — what a mount point looks
like with nothing mounted — are left as they are, and the run says so.
If you did empty that directory on purpose, `ingest --emptied DIR` is
the word for it and takes every place under it back, a directory that
is no more included. And a directory
you empty after every run, an inbox whose files are meant to live on in
the archive, is taken in with `ingest --collect`, which judges nothing
gone.

## Looking inside: extractors

Extractors are separate programs, one per format family, speaking a
[small pipe protocol](docs/extractors.md) open to any language. They
never touch the archive — bytes in, findings out — and everything they
say goes on the record under their own name and generation:

```console
$ ossuary extract mail
2 file(s) waiting for extractor:mail/1
2 file(s) examined by extractor:mail/1, 12 claim(s) written; 1 derived file(s) taken in; 1 had nothing to tell
2 file(s) examined in all, 1 derived file(s) taken in; run c7a1e2d4-9b3f-4e60-8a15-2f6d0c9b7e31
$ ossuary extract packed:list
1 file(s) waiting for extractor:packed-list/1
1 file(s) examined by extractor:packed-list/1, 3 claim(s) written
1 file(s) examined in all, 0 derived file(s) taken in; run 8d2f6b1c-4a7e-4c93-b5d0-1e9a3f7c2b64
```

The mail extractor read both files that sniff as text, recognised one
as a message, recorded its headers and handed its attachment over as
content of its own; the plain note "had nothing to tell" — an answer
too, and neither file is examined again. `packed:list` inventoried a
zip without unpacking a byte. Every examined file gets a receipt, so a
repeated run costs only what is new, and a new extractor version looks
at everything again.

List extractors under `[extract] run` in `config.toml` and a bare
`ossuary extract` runs them in rounds until nothing is left — what one
extractor hands back, the next round offers to whichever extractor
reads it, so mail → attachment → text runs to its end in one call.
What each of the four reads, what it says and what it needs stands in
its own README: [image](ossuary-extract-image/README.md),
[mail](ossuary-extract-mail/README.md),
[packed](ossuary-extract-packed/README.md),
[pdf](ossuary-extract-pdf/README.md).

## What the archive knows

`about` answers with the whole record of one file, oldest first — and a
beginning of the name is enough while it names only one file:

```console
$ ossuary about e9ed6104
2026-09-06T15:23:40Z  file:path = "/home/john/mail/2026-03-10-quarterly.eml"  [ingest] [run:315e360b-020e-48be-8f2d-f2002a2ea9b4]
2026-09-06T15:23:40Z  file:name = "2026-03-10-quarterly.eml"  [ingest] [run:315e360b-020e-48be-8f2d-f2002a2ea9b4]
2026-09-06T15:23:40Z  prov:host = "atlas.example.net"  [ingest] [run:315e360b-020e-48be-8f2d-f2002a2ea9b4]
2026-09-06T15:23:40Z  file:size = 491  [ingest] [run:315e360b-020e-48be-8f2d-f2002a2ea9b4]
2026-09-06T15:23:40Z  file:mime = "text/plain"  [ingest] [run:315e360b-020e-48be-8f2d-f2002a2ea9b4]
2026-09-06T15:23:40Z  file:modified = "2026-09-06T15:23:40.89362092Z"  [ingest] [run:315e360b-020e-48be-8f2d-f2002a2ea9b4]
2026-09-06T15:25:02Z  file:mime = "message/rfc822"  [extractor:mail/1] [run:c7a1e2d4-9b3f-4e60-8a15-2f6d0c9b7e31]
2026-09-06T15:25:02Z  mail:from = "Erika Muster <erika@example.org>"  [extractor:mail/1] [run:c7a1e2d4-9b3f-4e60-8a15-2f6d0c9b7e31]
2026-09-06T15:25:02Z  mail:to = "John Doe <john@example.net>"  [extractor:mail/1] [run:c7a1e2d4-9b3f-4e60-8a15-2f6d0c9b7e31]
2026-09-06T15:25:02Z  mail:subject = "Quarterly figures"  [extractor:mail/1] [run:c7a1e2d4-9b3f-4e60-8a15-2f6d0c9b7e31]
2026-09-06T15:25:02Z  mail:date = "Tue, 10 Mar 2026 14:22:05 +0100"  [extractor:mail/1] [run:c7a1e2d4-9b3f-4e60-8a15-2f6d0c9b7e31]
2026-09-06T15:25:02Z  mail:message-id = "<74a2f19c@mail.example.org>"  [extractor:mail/1] [run:c7a1e2d4-9b3f-4e60-8a15-2f6d0c9b7e31]
2026-09-06T15:25:02Z  prov:examined = "extractor:mail/1"  [extractor:mail/1] [run:c7a1e2d4-9b3f-4e60-8a15-2f6d0c9b7e31]
```

Every line says who said it, when, and in which run — the sniffed `text/plain` and the
mail extractor's sharper `message/rfc822` both stand, because the
record keeps every word and choosing between them is the reader's
business, not the archive's. Naming attributes narrows the answer:
`ossuary about e176bedf packed:` is everything the inventory recorded
about one archive file.

## Finding files

`find` takes `attribute=value` terms that must all hold, and the
question is also the projection — a filter shows itself until a bare
attribute names what to show; then only the named ones show:

```console
$ ossuary find file:mime=message/rfc822 mail:subject mail:from
e9ed6104
  mail:subject="Quarterly figures"
  mail:from="Erika Muster <erika@example.org>"
1 file(s)
```

`*` and `?` match within text values, `low..high` asks for a value in a
range with either side open (`file:modified=2026-01-01..` is "changed
since New Year"), and `--missing exif:` turns the question around:
which photos have no EXIF on record. Only files that still lie
somewhere answer — a place of their own on the record, or their
origin's, for what a tool won out of another file; a file gone from
every place it was seen at is still held, answers `--as-of` a day it
lay there, and answers `--all`, which asks the record itself: every
claim ever written, retractions included. The attachment from the mail
above is found like any other file, with its origin one term away:

```console
$ ossuary find 'file:name=*.pdf' prov:origin
b5743276
  prov:origin=e9ed6104c0bea9889000f408b6d855216f6743fa85586281c764c8c69d25a738
1 file(s)
```

The other way round, from a file down to everything won out of it,
is `--with-derived`: each match with its derivations drawn beneath it
the way `tree` draws a disk, as far as they go, each showing its kind
ahead of what the question shows. Under `--id` the names come flat,
ready for `export`; under `--json` they nest under `derived`:

```console
$ ossuary find 'file:name=*.eml' --with-derived
e9ed6104
│   file:name=2026-03-10-quarterly.eml
└── b5743276
    │   file:mime=application/pdf
    │   file:name=report.pdf
    └── 3f0c91aa
            file:mime=text/plain
1 file(s), 2 derived
```

And from a file up to where it came from is `--with-origin`: the same
shape read from the other end, the farthest origin at the head of the
tree and the match at its bottom, so a mail and its attachment stand
the same way whichever of them was asked for. A file won out of two
others, the same attachment in two mails, answers once per line of
descent, each a tree of its own. With `--with-derived` as well, the
whole line stands in one tree; under `--id` the names come flat,
origin first, the order `export` wants:

```console
$ ossuary find 'file:name=*.pdf' --with-origin
e9ed6104
│   file:mime=message/rfc822
│   file:name=2026-03-10-quarterly.eml
└── b5743276
        file:name=report.pdf
1 file(s), 1 origin(s)
```

A name without a colon is a field of the claim itself — `run`,
`source`, `time`, `retract` — and asks about the claim behind a value:
what a run named, what you tagged yourself, what was written since a
date, what was ever taken back:

```console
$ ossuary find run=315e360b-020e-48be-8f2d-f2002a2ea9b4 file:name
719aac93
  file:name=notes.txt
bd84e795
  file:name=DSC_1042.jpg
…
5 file(s)
$ ossuary find --all retract=true file:path=*
6ca81e7a
  file:path=/home/john/photos/DSC_1043.jpg
1 file(s)
```

The last one is the question "show me the files that are gone", and
it has a catch. A field term holds for every attribute term at once,
so `retract=true file:path=*` asks for a *path* that was taken back —
which is what a gone file has on the record, because `ingest` takes
back the place it no longer finds, not the name. `retract=true
file:name=*` asks for a name that was taken back, and finds nothing:
no name ever was. A bare `file:name` beside `retract=true` narrows
nothing and only shows the names of files with any retraction on
record. A field term shows nothing of itself; a bare field name does,
so `find run=… run` lists the runs a file was written in.

Which words there are to ask in at all is a question of its own —
`attributes` lists every one standing on the record, sorted, and a
namespace narrows it:

```console
$ ossuary attributes mail:
mail:date
mail:from
mail:message-id
mail:subject
mail:to
5 attribute(s) standing in 1 namespace(s)
```

The list is bare on purpose: `ossuary find $(ossuary attributes mail:)`
shows everything known about mail, and `--count` says on how many files
each attribute stands.

For scripts: `find --id` prints full names alone, ready to pipe;
`standing SUBJECT ATTRIBUTE` answers one attribute's standing values,
strings bare — and without an attribute, everything standing on the
file, where `about` tells the whole story, retractions included;
`--json` on `about`, `standing`, `find`, `attributes` and `ls` keeps the
JSON spelling for `jq`; and `-q` silences the narration everywhere. Every verb and
every flag stands in [`ossuary-cli`](ossuary-cli/README.md).

## When the archive changed

`history` is the record's own timeline: one line per run — every call
that wrote, an ingest, an extract, a fetch, your own words, a
retraction — with the moment it closed, its id, what it wrote and who
spoke in it:

```console
$ ossuary history
2026-09-06T15:23:40Z  315e360b-020e-48be-8f2d-f2002a2ea9b4  5 file(s), 30 claim(s)  [ingest]
2026-09-06T15:25:02Z  c7a1e2d4-9b3f-4e60-8a15-2f6d0c9b7e31  3 file(s), 18 claim(s)  [extractor:mail/1]
2026-09-06T15:26:11Z  8d2f6b1c-4a7e-4c93-b5d0-1e9a3f7c2b64  1 file(s), 3 claim(s)  [extractor:packed-list/1]
2026-09-07T09:02:55Z  9c1d0f3e-6b2a-4d81-a7f4-3e5c8d1b0a26  2 file(s), 2 claim(s), 2 of them taken back  [ingest]
4 run(s)
```

The time is spelled the way `--as-of` takes it, and so is the id:
`--as-of` accepts either, and a run closes the view after its last
claim — `ossuary ls --as-of 315e360b-020e-48be-8f2d-f2002a2ea9b4` is the archive as it stood when
the first ingest finished, two runs in the same second notwithstanding.
The id is also what `export` and `extract` take for a whole run, and
`find run=ID` asks what a run wrote about.

## Looking around

`ls` shows what stands at one place, one level of it; `tree` draws
everything below:

```console
$ ossuary tree /home/john
/home/john
├── docs/
│   ├── backup.zip  [e176bedf]
│   └── notes.txt  [719aac93]
├── mail/
│   └── 2026-03-10-quarterly.eml  [e9ed6104]
└── photos/
    ├── DSC_1042.jpg  [bd84e795]
    └── DSC_1043.jpg  [6ca81e7a]
3 folder(s), 5 file(s) under /home/john
```

What answers is the record, not a disk: every place a file was ever
seen at answers as long as it stands, every machine's paths in one
forest — and after a disk is gone, its places remain browsable.
Beside each file stands its name in the archive, ready for `about`,
`get` and `export`; `ls` puts that name first, the way
`export --dry-run` speaks. The mail's attachment is not here: a
derived file never sat at any place — `find` reaches it, as above.

## The reading room

`mount` grafts that same forest onto a directory, read-only — the
archive's files readable in place by any program, browsable in a
file manager, previews and all:

```console
$ ossuary mount ~/view
the record stands at /home/john/view, read-only, 5 file(s) in 5 folder(s); Ctrl-C gives it back
$ open ~/view/home/john/photos/DSC_1042.jpg
```

The command stays in the foreground; Ctrl-C gives the directory back,
and so does a plain `umount`. Writing is answered by the operating
system itself: read-only filesystem. Where the record says more than
a filesystem can, the view narrows — of several files standing at one
name, the newest wins — and `--as-of TIME` turns that dial back: the
record as it was known at that moment, UTC. Files since retracted
stand again, files since arrived are absent, and a place whose file
changed shows the old bytes — mount today and last year side by side
and compare. The room has a door for each platform — NFS on macOS,
FUSE on Linux — and neither asks for root or installs anything
kernel-side; [`ossuary-mount`](ossuary-mount/README.md) spells both
out.

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

`retract` takes a statement back: what it names no longer stands, and
the record keeps the whole story — the retraction is one more entry,
never an edit. A pair from an answer pastes straight back, and
`attribute=..` takes back every standing value at once:

```console
$ ossuary retract e9ed6104 user:tag=taxes
1 value(s) no longer stand on 1 file(s); 1 retraction(s) written
$ ossuary about e9ed6104 user:tag
2026-03-12T09:15:02Z  user:tag = "taxes"  [user]
2026-03-14T18:40:51Z  retracted: user:tag = "taxes"  [user]
```

Anything on the record can be taken back, the machines' word included —
and a later `extract --full` may honestly assert it again; `--as-of`
still answers for the day before.

## Getting things back out

`get` hands one file's bytes to stdout or `--output FILE`, exactly as
they went in. `export` lays whole batches back down: give it a run id
and every file that run recorded lands under the path the run saw it
at, kept relative — what lay side by side lands side by side:

```console
$ ossuary export /home/john/refile --dry-run 315e360b-020e-48be-8f2d-f2002a2ea9b4
e176bedf  docs/backup.zip
719aac93  docs/notes.txt
e9ed6104  mail/2026-03-10-quarterly.eml
bd84e795  photos/DSC_1042.jpg
6ca81e7a  photos/DSC_1043.jpg
would export 5 file(s) into /home/john/refile; nothing written
```

Without `--dry-run` that writes the five files. File names and run ids
mix freely in one call, and `ossuary find --id … | xargs ossuary
export DIR` exports a found set — each file landing at *every* place
still standing on its record, so nothing the question matched goes
missing. Nothing standing at the destination is ever overwritten — a
file already there with the same bytes counts as done, one with
different bytes is a named failure and stays untouched.

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
