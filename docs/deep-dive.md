# ossuary in detail

The [README](../README.md) shows what the commands are for, with an
example each, and [Use cases](usecases.md) shows them working together.
This page has what the README leaves out. Every verb and flag is listed
in [`ossuary-cli`](../ossuary-cli/README.md).

* [Other ways to install](#other-ways-to-install)
* [Files that are gone](#files-that-are-gone)
* [Finding files in detail](#finding-files-in-detail)
* [History of the archive](#history-of-the-archive)
* [Retracting](#retracting)
* [Another hash algorithm](#another-hash-algorithm)
* [What is stored where](#what-is-stored-where)
* [Several programs at once](#several-programs-at-once)
* [An archive on a network share](#an-archive-on-a-network-share)


## Other ways to install

`cargo install` upgrades a program when the same command is run again.
The package [cargo-update](https://crates.io/crates/cargo-update)
upgrades all programs installed with cargo at once:

```console
$ cargo install cargo-update
$ cargo install-update --all
```

`ossuary-fix` is in the release tarballs and in the Homebrew package,
but not on crates.io. To install it with cargo, install it from the
repository:

```console
$ cargo install --locked --git https://github.com/sniner/ossuary ossuary-fix
```

In a clone of the repository, `make install` builds and installs all
programs from the clone. `make install-crates.io` and `make install-git`
install them from crates.io or from GitHub instead, and `make uninstall`
removes them.


## Files that are gone

`ingest` remembers the paths it recorded under each directory it walks.
A file that is no longer there on the next run is treated as deleted.
Delete two of the files and run again:

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
path under it, also when the directory no longer exists.

For a directory you empty after every run, such as an inbox whose files
are meant to stay in the archive, use `ingest --collect`: it retracts
nothing, and the files keep their paths in the inbox. [Use
cases](usecases.md#an-inbox-that-is-emptied-after-every-run) shows such a
job.


## Finding files in detail

### What `find` returns

By default, `find` only returns files that still exist somewhere: files
with a current path, and files extracted from a file with a current
path. A file that is gone from every path it was seen at is still
stored. `--as-of` with a time when it was there finds it, and so does
`--all`, which searches every claim ever written, retractions included.

### From a file to what was extracted from it

`--with-derived` shows each match with its derived files as a tree below
it, drawn like `tree` draws a directory, as deep as they go. Each
derived file shows its type before the requested attributes. With `--id`
the names are listed flat, ready for `export`; with `--json` they are
nested under `derived`:

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

### From a file to where it came from

`--with-origin` draws the same tree from the other end, with the most
distant origin at the top and the match at the bottom, so a mail and its
attachment appear in the same tree whichever of them matched. A file
extracted from two others (the same attachment in two mails) appears
once per origin chain, each in a tree of its own. Together with
`--with-derived`, the whole chain is shown in one tree. With `--id` the
names are listed flat, origin first, in the order `export` expects.

Both trees follow `prov:origin`, the claim on a derived file that names
the file it came from:

```console
$ ossuary find 'file:name=*.pdf' prov:origin
b5743276
  prov:origin=e9ed6104c0bea9889000f408b6d855216f6743fa85586281c764c8c69d25a738
1 file(s)
```

### Fields of the claim

A name without a colon is a field of the claim itself (`run`, `source`,
`time`, `retract`). It filters on the claim behind a value: what a run
recorded, what you tagged yourself, what was written since a date, which
files have a retraction recorded:

```console
$ ossuary find run=315e360b-020e-48be-8f2d-f2002a2ea9b4 file:name
719aac93
  file:name=notes.txt
bd84e795
  file:name=DSC_1042.jpg
…
5 file(s)
$ ossuary find --all retract=true file:path
6ca81e7a
  file:path=/home/john/photos/DSC_1043.jpg
1 file(s)
```

The last query lists the files that had something retracted, with every
path ever recorded for them, which is where a deleted file turns up:
`ingest` retracts the path it no longer finds. A retraction carries no
value of its own, it names the claims it retracts, so a field term
`retract=true` matches no `attribute=value` term; a bare attribute next
to it shows that attribute's values of the files found. Which values a
retraction took back is what `about` shows. A field term shows nothing
by itself, but a bare field name does, so `find run=… run` lists the
runs a file was written in.

### Which attributes are in use

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

### Output for scripts

`find --id` prints only full names, ready to pipe. `standing SUBJECT
ATTRIBUTE` prints one attribute's current values, strings without
quotes; without an attribute, it prints all current values of the file,
while `about` shows the full history including retractions. `--json` on
`about`, `standing`, `find`, `attributes` and `ls` prints JSON for `jq`,
and `-q` turns off progress output everywhere.


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


## Retracting

`retract` works on any claim, including those written by `ingest` and
the extractors. The retraction is one more claim, so `--as-of` still
shows the state before it, and `about` shows both. `attribute=..`
retracts every current value of an attribute at once. A later `extract
--full` may record a retracted finding again.


## Another hash algorithm

The hash algorithm is chosen when an archive is created
(`init --algorithm`), and every file and segment is named by it. To
move an archive to another algorithm, `ossuary-fix rehash` copies it
into a new archive: every file is hashed anew, and every claim is
written there with the new names, with its time, source and run
unchanged. `config.toml` and `mailvault.toml` are copied along. The
archive itself is only read.

```console
$ ossuary-fix --archive ~/archive rehash --algorithm blake3 ~/archive-blake3
```


## What is stored where

The archive consists of content and claims: immutable blobs stored
under their content hash (via [immure](https://github.com/sniner/immure)),
and an append-only claim log sealed into the same kind of store. What
tools derived (extracted text, unpacked attachments) is kept in a
separate store, apart from the originals. Everything else (the query
index, what `ingest` remembers of earlier runs) is a cache in `cache/`
that is rebuilt from the log whenever needed; deleting it makes the next
query slow, but loses no data. [The archive format](format.md) describes
every file.


## Several programs at once

Several programs can write to one archive at once, on one machine or on
several, with the archive on a NAS or any other shared folder: a fetch
from cron, an ingest started by hand, an extractor running in the
background. Nothing is locked. Each append is a file that is written
once and never changed, and sealing is decided by one atomic rename, so
two runs that happen to meet cannot damage the archive or lose a claim.
The one thing they can damage is the query index in `cache/`, which is
then rebuilt by the next command; that takes a moment on a large
archive and loses nothing.


## An archive on a network share

Each append is written to the disk before the program goes on. On a
share, that is a round trip to the server for every append, so a run
that records many claims is slow there: an import, the first `ossuary
extract` of a large archive, the first fetch of a mailbox. Build such an
archive on a local disk and copy it to the share after the last of these
runs. Copy the whole archive directory, `cache/` included, for example
with `rsync -rlt archive/ /mnt/nas/archive/`. Without `cache/`, the next
command rebuilds the query index, and on a share that takes long as
well. Later runs record a few claims at a time and work well on the
share.
