# ossuary: a personal archive of files and everything known about them

> [!NOTE]
> This is 0.x software. Commands and the format may still change between
> minor versions. A breaking change raises the minor version and is listed
> in the [CHANGELOG](CHANGELOG.md).

`ossuary` keeps files permanently and records everything known about
them. A file is stored once and named by the hash of its content: the
same bytes are never stored twice, and the name never changes.
Everything known about a file (where it came from, what a tool read from
it, what you added yourself) is stored as *claims* in an append-only
record. Each claim has a source and a time, and no claim is ever
changed. Retracting a claim adds one more claim; nothing is deleted.

It is an archive, not a backup tool. It does not mirror a disk or
restore the state of a given day. It keeps what you decide to keep and
lets you query it for decades, independent of file formats, software
versions and the programs that wrote the files. Typical uses:

* **Keeping what must not get lost.** Photos, documents, saved mail,
  finished projects, added from wherever they are. Duplicates are
  detected, and adding the same directory again only costs what is new
  or changed.
* **Information about any file.** What it is, where it came from, on
  which machines it was, what arrived with it. A file is looked up by
  its content hash, not by a path that may no longer exist.
* **Metadata, extracted verbatim.** EXIF fields, a PDF's text and
  document info, a mail's headers. What a mail or a zip contains
  (attachments, nested messages, zip entries) is stored as files of
  their own, each linked to the file it came from.
* **Your own tags and comments.** Tags when files are added, tags and
  comments at any time later, recorded next to the tools' findings under
  your own name.
* **Getting files back out.** One file by its name, or a whole batch in
  the directory layout it arrived in.

## Installing

With Homebrew on macOS or Linux, one command installs all programs:

```console
$ brew install sniner/tap/ossuary
```

Every [release](https://github.com/sniner/ossuary/releases) contains the
built programs: for Linux on x86_64 and arm64, statically linked, and
for macOS as universal binaries. There is one tarball per platform,
named after the version and the platform, with all programs in it. To
build from source:

```console
$ git clone https://github.com/sniner/ossuary
$ cd ossuary && cargo build --release
```

Either way, put all programs on your `PATH`: extractors and outside
verbs like `mount` are found there, they are not built in. Each crate's
README describes what it does and what it needs:

| | |
|---|---|
| [`ossuary-cli`](ossuary-cli/README.md) | the `ossuary` command with all its verbs, from `init` to `audit` |
| [`ossuary-core`](ossuary-core/README.md) | the archive library all other programs are built on |
| [`ossuary-mount`](ossuary-mount/README.md) | the archive as a read-only filesystem |
| [`ossuary-mailvault`](ossuary-mailvault/README.md) | fetches whole mailboxes into the archive over IMAP or MS Graph |
| [`ossuary-extract-image`](ossuary-extract-image/README.md) | image metadata (EXIF, XMP, IPTC) and the pixel format (size, bit depth, color) |
| [`ossuary-extract-mail`](ossuary-extract-mail/README.md) | a mail's headers, and its attachments and nested messages as files of their own |
| [`ossuary-extract-packed`](ossuary-extract-packed/README.md) | a zip file's list of contents, or its unpacked files |
| [`ossuary-extract-pdf`](ossuary-extract-pdf/README.md) | a PDF's text and document info, or its attachments; needs poppler's `pdftotext` |

One program is missing from this list on purpose:
[`ossuary-fix`](ossuary-fix/README.md) repairs 0.x archives after a
breaking change and is not a regular part of the package. Its README
describes what it does and when not to use it.

## Creating an archive

An archive is a directory with a one-line `FORMAT` file that names the
layout, a `config.toml` with the settings, and everything else below
it. `init` creates one where `--archive` points:

```console
$ ossuary --archive /home/john/archive init
/home/john/archive: empty archive created, settings in config.toml; add files with `ossuary ingest DIR`
```

Every command takes `--archive`. Without it, `OSSUARY_ARCHIVE` is used
if it is set, otherwise the current directory. The examples below set
`OSSUARY_ARCHIVE` once for the shell session.

## Adding files

`ingest` adds directory trees and single files, in any mix. Everything
from one call is recorded as one *run*, so you can later ask which files
arrived together:

```console
$ cd /home/john
$ ossuary ingest photos mail docs
archive /home/john/archive
ingesting 3 paths
5 file(s) stored; 30 claim(s) written, run 315e360b-020e-48be-8f2d-f2002a2ea9b4
```

Each file gets six claims: its path, its name, the host, its size, its
type (detected from the content, not the file name) and its modification
time. Every claim records the run it was written in. Files are only
read, never changed. Running the same command again adds nothing:

```console
$ ossuary ingest photos mail docs
0 file(s) stored, 5 unchanged since the last run; 0 claims written
```

`--tag holiday` adds your own tag to every file the run records.
`ossuary id FILE` shows the name a file would get, and whether the
archive already contains it, without adding anything.

A run also detects files that are gone. Delete two of the files and run
again:

```console
$ ossuary ingest photos mail docs
0 file(s) stored, 3 unchanged since the last run, 2 path(s) no longer found, retracted; 2 claim(s) written, run 9c1d0f3e-…
```

The two files stay in the archive with everything recorded about them,
and `--as-of` with a time before that run still shows them at their old
paths. Only their paths were retracted, with one more claim each, so
queries about the present no longer return them.

Some cases are not treated as deletions, and the run reports them: a
directory that could not be opened, a path excluded in `config.toml`,
and a directory in which the walk finds no file at all while paths are
recorded under it (an empty mount point looks like that). If you did
empty such a directory on purpose, `ingest --emptied DIR` retracts every
path under it, also when the directory no longer exists. For a
directory you empty after every run, such as an inbox whose files are
meant to stay in the archive, use `ingest --collect`: it retracts
nothing.

## Extracting metadata

Extractors are separate programs, one per format family. They talk to
`ossuary` over a [small pipe protocol](docs/extractors.md), so they can
be written in any language. They never access the archive themselves:
they get the bytes and return findings, and everything they find is
recorded with the extractor as the source (`extractor:mail/1`):

```console
$ ossuary extract mail
2 file(s) waiting for extractor:mail/1
2 file(s) examined by extractor:mail/1, 12 claim(s) written; 1 derived file(s) added; 1 found nothing
2 file(s) examined in total, 1 derived file(s) added; run c7a1e2d4-9b3f-4e60-8a15-2f6d0c9b7e31
$ ossuary extract packed:list
1 file(s) waiting for extractor:packed-list/1
1 file(s) examined by extractor:packed-list/1, 3 claim(s) written
1 file(s) examined in total, 0 derived file(s) added; run 8d2f6b1c-4a7e-4c93-b5d0-1e9a3f7c2b64
```

The mail extractor read both files detected as text, recognised one of
them as a mail, recorded its headers and stored its attachment as a
file of its own. In the plain note it found nothing. That result is
recorded as well, and neither file is examined again. `packed:list`
recorded a zip file's list of contents without unpacking it. Every
examined file gets a receipt (`prov:examined`), so a repeated run only
processes new files, and a new extractor version examines all files
again.

If you list extractors under `[extract] run` in `config.toml`, a bare
`ossuary extract` runs them in rounds until nothing is left. Files that
one extractor produces are offered to the matching extractors in the
next round, so mail, attachment and the attachment's text are all
processed in one call. What each of the four extractors reads, records
and needs is in its README: [image](ossuary-extract-image/README.md),
[mail](ossuary-extract-mail/README.md),
[packed](ossuary-extract-packed/README.md),
[pdf](ossuary-extract-pdf/README.md).

## Showing what is known about a file

`about` shows every claim about one file, oldest first. A prefix of the
file's name is enough as long as it matches only one file:

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

Each line shows the time, the source and the run. Both the detected
`text/plain` and the mail extractor's more specific `message/rfc822` are
kept: the record keeps every claim, and which one to use is up to the
reader. Naming attributes narrows the output: `ossuary about e176bedf
packed:` shows only what the zip extractor recorded about one zip file.

## Finding files

`find` takes `attribute=value` terms, and a file must match all of them.
The attributes of the filter terms are shown in the output, unless you
also name bare attributes; then only those are shown:

```console
$ ossuary find file:mime=message/rfc822 mail:subject mail:from
e9ed6104
  mail:subject="Quarterly figures"
  mail:from="Erika Muster <erika@example.org>"
1 file(s)
```

`*` and `?` match within text values. `low..high` matches a range, with
either end open (`file:modified=2026-01-01..` means "changed since New
Year"). `--missing exif:` inverts the question: which photos have no
EXIF data recorded.

By default, `find` only returns files that still exist somewhere: files
with a current path, and files extracted from a file with a current
path. A file that is gone from every path it was seen at is still
stored. `--as-of` with a time when it was there finds it, and so does
`--all`, which searches every claim ever written, retractions included.
The attachment from the mail above is found like any other file, and
`prov:origin` names the mail it came from:

```console
$ ossuary find 'file:name=*.pdf' prov:origin
b5743276
  prov:origin=e9ed6104c0bea9889000f408b6d855216f6743fa85586281c764c8c69d25a738
1 file(s)
```

In the other direction, from a file to everything extracted from it, use
`--with-derived`. Each match is shown with its derived files as a tree
below it, drawn like `tree` draws a directory, as deep as they go. Each
derived file shows its type before the requested attributes. With
`--id` the names are listed flat, ready for `export`; with `--json` they
are nested under `derived`:

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

`--with-origin` goes from a file up to where it came from. It draws the
same tree from the other end, with the most distant origin at the top and the
match at the bottom, so a mail and its attachment appear in the same
tree whichever of them matched. A file extracted from two others (the
same attachment in two mails) appears once per origin chain, each in a
tree of its own. Together with `--with-derived`, the whole chain is
shown in one tree. With `--id` the names are listed flat, origin first,
in the order `export` expects:

```console
$ ossuary find 'file:name=*.pdf' --with-origin
e9ed6104
│   file:mime=message/rfc822
│   file:name=2026-03-10-quarterly.eml
└── b5743276
        file:name=report.pdf
1 file(s), 1 origin(s)
```

A name without a colon is a field of the claim itself (`run`, `source`,
`time`, `retract`). It filters on the claim behind a value: what a run
recorded, what you tagged yourself, what was written since a date, what
was ever retracted:

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

The last query lists files that are gone, with one catch. A field term
applies to every attribute term at once, so `retract=true file:path=*`
asks for a retracted *path*. That is what a deleted file has, because
`ingest` retracts the path it no longer finds, not the name.
`retract=true file:name=*` asks for a retracted name and finds nothing,
because no name was ever retracted. A bare `file:name` next to
`retract=true` does not filter; it only shows the names of files that
have any retraction recorded. A field term shows nothing by itself, but
a bare field name does, so `find run=… run` lists the runs a file was
written in.

`attributes` lists every attribute that has current claims, sorted. A
namespace narrows the list:

```console
$ ossuary attributes mail:
mail:date
mail:from
mail:message-id
mail:subject
mail:to
5 attribute(s) in 1 namespace(s)
```

The output is one name per line, so `ossuary find $(ossuary attributes
mail:)` shows everything recorded about mail. `--count` shows on how
many files each attribute is set.

For scripts: `find --id` prints only full names, ready to pipe.
`standing SUBJECT ATTRIBUTE` prints one attribute's current values,
strings without quotes; without an attribute, it prints all current
values of the file, while `about` shows the full history including
retractions. `--json` on `about`, `standing`, `find`, `attributes` and
`ls` prints JSON for `jq`, and `-q` turns off progress output
everywhere. All verbs and flags are documented in
[`ossuary-cli`](ossuary-cli/README.md).

## History of the archive

`history` lists the runs: one line per call that wrote something (an
ingest, an extract, a fetch, your tags and comments, a retraction), with
the time it finished, its id, what it wrote and its source:

```console
$ ossuary history
2026-09-06T15:23:40Z  315e360b-020e-48be-8f2d-f2002a2ea9b4  5 file(s), 30 claim(s)  [ingest]
2026-09-06T15:25:02Z  c7a1e2d4-9b3f-4e60-8a15-2f6d0c9b7e31  3 file(s), 18 claim(s)  [extractor:mail/1]
2026-09-06T15:26:11Z  8d2f6b1c-4a7e-4c93-b5d0-1e9a3f7c2b64  1 file(s), 3 claim(s)  [extractor:packed-list/1]
2026-09-07T09:02:55Z  9c1d0f3e-6b2a-4d81-a7f4-3e5c8d1b0a26  2 file(s), 2 claim(s), 2 of them retractions  [ingest]
4 run(s)
```

The time is printed in the format `--as-of` takes. `--as-of` also
accepts a run id; the view then ends after that run's last claim.
`ossuary ls --as-of 315e360b-020e-48be-8f2d-f2002a2ea9b4` shows the
archive as it was when the first ingest finished, even if two runs
finished in the same second. `export` and `extract` also take a run id
for all files of a run, and `find run=ID` shows what a run wrote.

## Browsing

`ls` shows one directory level of the recorded paths, `tree` shows
everything below a path:

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

The tree comes from the record, not from a disk: every current path a
file was seen at is shown, the paths of all machines in one tree, and a
disk's paths can still be browsed after the disk is gone. Next to each
file is its name in the archive, for use with `about`, `get` and
`export`; `ls` puts the name first, like `export --dry-run`. The mail's
attachment is not listed, because a derived file has no path of its
own. `find` finds it, as shown above.

## Mounting the archive

`mount` shows the same tree as a read-only directory. Any program can
read the archive's files there, and a file manager can browse them,
previews included:

```console
$ ossuary mount ~/view
mounted read-only at /home/john/view: 5 file(s) in 5 folder(s); press Ctrl-C to unmount
$ open ~/view/home/john/photos/DSC_1042.jpg
```

The command stays in the foreground. Ctrl-C unmounts the directory, and
so does a plain `umount`. Writes fail with the operating system's
read-only error. Where the record holds more than a filesystem can
show, the view is reduced: if several files have the same path, the
newest one is shown. `--as-of TIME` shows the record as it was at that
moment (UTC): files retracted since then are back, files added since
are missing, and a path whose file changed shows the old content. You
can mount today and last year side by side and compare them. The mount
uses NFS on macOS and FUSE on Linux; neither needs root, and nothing is
installed in the kernel. [`ossuary-mount`](ossuary-mount/README.md) has
the details.

## Tags and comments

`annotate` adds tags and comments to files in the archive, with the
source `user`. The output of `find` can be piped into it:

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

`retract` retracts a value: it is no longer current, and the record
keeps the full history, because the retraction is one more claim, not
an edit. An `attribute=value` pair from `find`'s output can be pasted
as is, and `attribute=..` retracts every current value of an attribute
at once:

```console
$ ossuary retract e9ed6104 user:tag=taxes
1 value(s) retracted from 1 file(s); 1 retraction(s) written
$ ossuary about e9ed6104 user:tag
2026-03-12T09:15:02Z  user:tag = "taxes"  [user]
2026-03-14T18:40:51Z  retracted: user:tag = "taxes"  [user]
```

Any claim can be retracted, including those written by `ingest` and the
extractors. A later `extract --full` may record it again, and `--as-of`
still shows the state before the retraction.

## Getting files back out

`get` writes one file's content to stdout or to `--output FILE`,
exactly as it was added. `export` writes whole batches: given a run id,
it writes every file that run recorded under the path the run saw,
relative to the target directory, so files that were next to each
other end up next to each other again:

```console
$ ossuary export /home/john/refile --dry-run 315e360b-020e-48be-8f2d-f2002a2ea9b4
e176bedf  docs/backup.zip
719aac93  docs/notes.txt
e9ed6104  mail/2026-03-10-quarterly.eml
bd84e795  photos/DSC_1042.jpg
6ca81e7a  photos/DSC_1043.jpg
would export 5 file(s) into /home/john/refile; nothing written
```

Without `--dry-run` this writes the five files. File names and run ids
can be mixed in one call, and `ossuary find --id … | xargs ossuary
export DIR` exports the result of a query. Each file is written to
*every* current path on its record, so nothing the query matched is
left out. Existing files at the destination are never overwritten: a
file with the same content counts as done, a file with different
content is reported as a failure and left untouched.

## Design

The archive consists of content and claims: immutable blobs stored
under their content hash (via [immure](https://github.com/sniner/immure)),
and an append-only claim log sealed into the same kind of store. What
tools derived (extracted text, unpacked attachments) is kept in a
separate store, apart from the originals. Everything else (the query
index, manifests, what `ingest` remembers of earlier runs) is a cache in `cache/` that is
rebuilt from the log whenever needed; deleting it makes the next query
slow, but loses no data.

The format is documented so that an archive can be read without this
software:

* [The archive format](docs/format.md): layout, claims, segments, and
  how to recover an archive with nothing but a shell
* [The attribute vocabulary](docs/vocabulary.md): what each attribute
  means, and how current claims become an answer
* [The extractor protocol](docs/extractors.md): how an extractor, in
  any language, reports what it found

## License

`ossuary` is free software under the
[Apache License 2.0](LICENSE).
