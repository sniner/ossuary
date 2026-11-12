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
versions and the programs that wrote the files. Several programs can
add to the same archive at the same time, also over a network share
(see [How it works](#how-it-works)). Typical uses:

* **Keeping what must not get lost.** Photos, documents, saved mail,
  finished projects, added from wherever they are. Duplicates are
  detected, and adding the same directory again only costs what is new
  or changed.
* **Information about any file.** What it is, where it came from, on
  which machines it was, what arrived with it. A file is looked up by
  its content hash, not by a path that may no longer exist.
* **Metadata from inside the files.** EXIF fields, a PDF's text and
  document info, a mail's headers. What a mail or a zip contains
  (attachments, nested messages, zip entries) is stored as files of
  their own, each linked to the file it came from.
* **Your own tags and comments.** Tags when files are added, tags and
  comments at any time later, recorded next to the tools' findings under
  your own name.
* **Getting files back out.** One file by its name, or a whole batch in
  the directory layout it arrived in.

[Use cases](docs/usecases.md) shows worked examples.

## Installing

On macOS, one Homebrew command installs all programs:

```console
$ brew install sniner/tap/ossuary
```

On Linux, every [release](https://github.com/sniner/ossuary/releases)
has a tarball for x86_64 and one for arm64 (aarch64), each with all
programs in it, statically linked. Unpack it into a directory on your
`PATH`, for example `~/.local/bin`:

```console
$ tar -xzf ossuary-*-x86_64-linux-musl.tar.gz -C ~/.local/bin
```

To upgrade, unpack the tarball of the new release the same way.

With Rust installed ([rustup.rs](https://rustup.rs)), cargo builds and
installs the programs from [crates.io](https://crates.io) instead. The
build also needs a C compiler: `build-essential` or your distribution's
equivalent.

```console
$ cargo install --locked \
    ossuary-cli ossuary-mount ossuary-mailvault ossuary-sync \
    ossuary-extract-image ossuary-extract-mail \
    ossuary-extract-packed ossuary-extract-pdf
```

To upgrade, run the same command again. [Other ways to install](docs/deep-dive.md#other-ways-to-install) covers
cargo-update, a clone of the repository, and `ossuary-fix`.

All programs have to be on the `PATH`: `ossuary` finds the extractors
and outside verbs like `mount` there, they are not built in. Each
crate's README describes what it does and what it needs:

| | |
|---|---|
| [`ossuary-cli`](ossuary-cli/README.md) | the `ossuary` command with all its verbs, from `init` to `audit` |
| [`ossuary-core`](ossuary-core/README.md) | the archive library all other programs are built on |
| [`ossuary-mount`](ossuary-mount/README.md) | the archive as a read-only filesystem |
| [`ossuary-mailvault`](ossuary-mailvault/README.md) | fetches whole mailboxes into the archive over IMAP or MS Graph |
| [`ossuary-sync`](ossuary-sync/README.md) | copies the archive to a second place and keeps the copy up to date |
| [`ossuary-extract-image`](ossuary-extract-image/README.md) | image metadata (EXIF, XMP, IPTC), the GPS position and the pixel format (size, bit depth, color) |
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
`OSSUARY_ARCHIVE` once for the shell session. `--cache DIR` keeps the
query index outside the archive, for an archive that cannot be written
to or lies on a network share.

Two settings are best decided before the first file is added. The hash
algorithm that names the files is set by `init --algorithm` (default
sha256) and cannot be changed later without copying the archive (see
[Another hash algorithm](docs/deep-dive.md#another-hash-algorithm)).
Compression is set by `compress` in `config.toml`. A change applies only
to files added after it; files already in the archive stay as they are.

Leave compression off, which is the default. Then every file in the
archive is the original: any program can read it where it is (`ossuary
get --path` prints the path), `grep` searches text files and the text
extracted from PDFs, and `cp` gets a file back without ossuary or zstd.
Photos, videos, PDFs, office documents and zip files are compressed
already, so zstd saves little on them. If disk space matters, turn on
compression in the filesystem, for example ZFS or btrfs: programs do
not see it, and the archive stays uncompressed.

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

A file deleted from a directory that `ingest` walked before has its
path retracted on the next run. The file stays in the archive with
everything recorded about it, and `--as-of` with an earlier time still
shows it at its old path. For an inbox that is emptied after every run,
`ingest --collect` retracts nothing. [Files that are
gone](docs/deep-dive.md#files-that-are-gone) has the details.

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

By default, `find` returns only files that still exist somewhere. `--all`
searches every claim ever written, and `--as-of` the archive as it was
at a given time. A file extracted from another one is found like any
other file, and `--with-origin` shows where it came from:

```console
$ ossuary find 'file:name=*.pdf' --with-origin
e9ed6104
│   file:mime=message/rfc822
│   file:name=2026-03-10-quarterly.eml
└── b5743276
        file:name=report.pdf
1 file(s), 1 origin(s)
```

`--with-derived` draws the tree from the other end, from a file to
everything extracted from it. A term can also filter on the claim
itself, such as the run that wrote it (`find run=ID`), and `history`
lists the runs. `--id` and `--json` print output for scripts. [Finding
files in detail](docs/deep-dive.md#finding-files-in-detail) shows all of
it.

## Proving that nothing was changed

Where records have to be kept unchanged for years, as invoices,
contracts and business mail often must, it helps to be able to show
later that the archive still holds what it held: no file removed, none
altered, no claim about one changed. `seal` records the state of the
archive and prints an id for it:

```console
$ ossuary seal
1bbb4b0cc3a691061154ec0a228cb409aa3266c5bb90a7182c9164b02e153776
```

Keep the id outside the archive: printed and filed, or in a mail to your
accountant. At any time later, check the archive against it:

```console
$ ossuary seal --check 1bbb4b0cc3a691061154ec0a228cb409aa3266c5bb90a7182c9164b02e153776
seal 1bbb4b0cc3a691061154ec0a228cb409aa3266c5bb90a7182c9164b02e153776 verified: the claims up to 2026-10-22T02:15:07Z (312 sealed segment(s), 1308712 claim(s)) and the 131204 file(s) they refer to are present and unchanged
```

Unlike `audit`, this also finds a change that leaves the archive in
order, such as the newest part of the record removed. `--fast` checks
only that every file is there, not that it is unchanged.

Seal only when the id goes somewhere outside the archive and someone may
ask for the proof, as with a business's records. A seal after new claims
writes a new segment file to `claims/`, and segment files are never
combined. When nothing was recorded since the last seal, `seal` prints
the same id again and writes nothing.

What a seal proves and what it does not, where to keep the id, what to
do when a seal is not verified, and how to have the id timestamped:
[Sealing an archive](docs/sealing.md).

## Copying the archive to a second place

`sync TARGET` copies the archive to a second place, such as a backup
disk, a share mounted from a NAS, a folder on a filebridge server or a
WebDAV server, and keeps the copy up to date. An archive on a disk that
your backup already covers needs no `sync`. Each run seals the archive,
as `seal` does, and copies only what the copy lacks. The last line is
the seal id the copy now holds:

```console
$ mkdir /mnt/backup/archive
$ ossuary sync -q /mnt/backup/archive
1bbb4b0cc3a691061154ec0a228cb409aa3266c5bb90a7182c9164b02e153776
$ ossuary --archive /mnt/backup/archive seal --check 1bbb4b0cc3a691061154ec0a228cb409aa3266c5bb90a7182c9164b02e153776 --fast
```

Nothing in the copy is deleted. A run that is interrupted leaves the
copy at the seal it had, and the next run completes it. `cache/`,
`config.toml` and `mailvault.toml` are not copied. What a run does and
what it refuses: [ossuary-sync](ossuary-sync/README.md).

## Browsing and mounting

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

The tree comes from the record, not from a disk: the paths of all
machines are in one tree, and a disk's paths can still be browsed after
the disk is gone. Next to each file is its name in the archive, for use
with `about`, `get` and `export`. A file extracted from another one has
no path of its own and is not listed; `find` finds it.

`mount` shows the same tree as a read-only directory, for any program
and any file manager:

```console
$ ossuary mount ~/view
mounted read-only at /home/john/view: 5 file(s) in 5 folder(s); press Ctrl-C to unmount
$ open ~/view/home/john/photos/DSC_1042.jpg
```

Ctrl-C or `umount` unmounts it. `--as-of TIME` shows the archive as it
was at that moment, so today and last year can be mounted side by side.
[`ossuary-mount`](ossuary-mount/README.md) has the details.

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
as is:

```console
$ ossuary retract e9ed6104 user:tag=taxes
1 value(s) retracted from 1 file(s); 1 retraction(s) written
$ ossuary about e9ed6104 user:tag
2026-03-12T09:15:02Z  user:tag = "taxes"  [user]
2026-03-14T18:40:51Z  retracted: user:tag, 1 claim(s)  [user]
```

Any claim can be retracted, also those written by `ingest` and the
extractors (see [Retracting](docs/deep-dive.md#retracting)).

## Getting files back out

`get` writes one file's content to stdout or to `--output FILE`,
exactly as it was added. `get --path` prints the path of the stored
file in the archive instead, for a program that reads the file where it
is, without a copy. `export` writes whole batches: given a run id,
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

## How it works

The files are stored in `content/` under the hash of their content, and
what the extractors produce in `derived/`, apart from the originals.
The claims go into a record that is only ever added to, in `open/` and
`claims/`. The query index in `cache/` is rebuilt from the record when needed; deleting
it loses nothing. Several programs can write to one archive at once,
also from several machines over a network share, and nothing is locked.
[ossuary in detail](docs/deep-dive.md#what-is-stored-where) describes
this, and why a large first import belongs on a local disk.

## Further reading

* [Use cases](docs/usecases.md): worked examples, such as finding a
  document by a phrase in it
* [ossuary in detail](docs/deep-dive.md): what this README leaves out,
  from the cases `ingest` treats specially to an archive on a share
* [`ossuary-cli`](ossuary-cli/README.md): every verb and flag
* [The archive format](docs/format.md): layout, claims, segments, and
  how to recover an archive with nothing but a shell
* [Reading an archive without ossuary](docs/recovery.md): getting files
  back from an uncompressed or compressed archive, with examples
* [Sealing an archive](docs/sealing.md): what a seal proves, keeping and
  checking the id, and checking a seal without ossuary
* [The attribute vocabulary](docs/vocabulary.md): what each attribute
  means, and how current claims become an answer
* [The extractor protocol](docs/extractors.md): how an extractor, in
  any language, reports what it found

## License

`ossuary` is free software under the
[Apache License 2.0](LICENSE).
