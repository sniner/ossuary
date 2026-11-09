# Use cases

The [README](../README.md) shows what each command does. This page
shows how the commands and ordinary tools work together for a task,
one task per section.

* [Finding a document by a phrase in it](#finding-a-document-by-a-phrase-in-it)
* [Archiving a mailbox on a schedule](#archiving-a-mailbox-on-a-schedule)
* [An inbox that is emptied after every run](#an-inbox-that-is-emptied-after-every-run)
* [Where a file was, and on which machine](#where-a-file-was-and-on-which-machine)
* [The archive as it was last year](#the-archive-as-it-was-last-year)

## Finding a document by a phrase in it

**The situation.** You remember a phrase from a document, say the
address on an invoice, but not the file name or where the file was. It
may have arrived as a mail attachment.

**What it needs.** An uncompressed archive (the default, see [Creating
an archive](../README.md#creating-an-archive)) and the mail and PDF
extractors run over it: `ossuary extract mail` and `ossuary extract
pdf`, or both listed under `[extract] run` in `config.toml`. The mail
extractor stores each attachment as a file of its own, and the PDF
extractor stores the text of each PDF as a plain text file. The
examples use [ripgrep](https://github.com/BurntSushi/ripgrep) (`rg`);
`grep -rl` works the same way.

**1. Search the stored files.** The files are in `content/` and
`derived/`; `claims/` and `cache/` hold the record and the query index.

```console
$ rg -l "Elm Street" ~/archive/content ~/archive/derived
/home/john/archive/derived/6b/32/6b320d682c6be758c9e345bf1086be76da702924022858c57012a14b428ce4ec
```

The file name of a hit is the file's name in the archive. A hit in
`derived/` is a file that an extractor produced from another one.

**2. Find where it came from.** The field `subject` selects one file by
its full name, and `--with-origin` shows the files it was derived from:

```console
$ ossuary find subject=6b320d682c6be758c9e345bf1086be76da702924022858c57012a14b428ce4ec --with-origin file:name
9a9274ef
│   file:mime=message/rfc822
│   file:mime=text/plain
│   file:name=2026-03-17-invoice.eml
└── fb1c6c49
    │   file:mime=application/pdf
    │   file:name=invoice-2026-0412.pdf
    └── 6b320d68
1 file(s), 2 origin(s)
```

The hit is the text of the PDF `invoice-2026-0412.pdf`, which was
attached to the mail `2026-03-17-invoice.eml`.

**3. Get the original.** `get` writes the PDF to a file, and `about`
shows where the mail was when it was added:

```console
$ ossuary get fb1c6c49 --output invoice.pdf
invoice.pdf: 28416 byte(s) of fb1c6c499cd6d943a903ce9b2e618dbbd24ccfc435ee07bcf8fce4966ffdaf3b
$ ossuary about 9a9274ef file:path
2026-11-09T11:11:31Z  file:path = "/home/john/mail/2026-03-17-invoice.eml"  [ingest] [run:38151950-b95c-4c30-bc63-d698520e1c02]
```

For a program that reads the file in the archive directly, `ossuary get
--path fb1c6c49` prints its path.

**Several hits.** Names in the archive contain no spaces, so a shell
loop is enough:

```console
$ for name in $(rg -l "Elm Street" ~/archive/content ~/archive/derived | xargs -n1 basename); do
>   ossuary find subject=$name --with-origin file:name
> done
```

**Limits.**

- The body of a mail is often encoded (base64 or quoted-printable), and
  then the search does not find its text. A mail's headers are recorded
  as claims: `ossuary find 'mail:subject=*roof*'` searches the subjects.
- A scanned PDF without a text layer has no text to extract.
- In a compressed archive, `rg -z` searches the compressed files; it
  needs `zstd`. The name of a hit then ends in `.zst`, and the file's
  name is the part before it.

## Archiving a mailbox on a schedule

**The situation.** A mailbox is to be kept in the archive, every new
message soon after it arrives, with its attachments searchable like any
other file.

**What it needs.** The account in `mailvault.toml` in the archive:
`ossuary mailvault init` writes the file with an example of each kind of
account, and [ossuary-mailvault](../ossuary-mailvault/README.md)
describes every key. The extractors are listed in `config.toml`, so that
a bare `ossuary extract` runs them:

```toml
[extract]
run = ["mail", "pdf"]
```

**1. The first fetch, by hand.** The first fetch downloads the whole
mailbox and can take hours. Run it in a terminal, where you see its
progress and its errors:

```console
$ cd /home/john/archive
$ ossuary mailvault fetch
$ ossuary extract
```

If the archive is to be kept on a network share, make these first runs
on a local disk and copy the archive afterwards (see [An archive on a
network share](deep-dive.md#an-archive-on-a-network-share)).

**2. The job.** Every later fetch continues where the last one stopped,
and `extract` examines only what is new, so an hourly job costs little:

```console
$ crontab -l
PATH=/home/john/.local/bin:/usr/bin:/bin
OSSUARY_ARCHIVE=/home/john/archive
0 * * * *  ossuary mailvault fetch -q >/dev/null; ossuary extract -q >/dev/null
```

cron starts a job with a short `PATH`, so the first line names the
directory the programs are in. With `-q` and `>/dev/null` only errors
are printed, and cron mails you only when something failed. The `;`
runs `extract` also after a fetch that failed in one folder, for the
messages that did arrive.

**3. Finding a message.** The mail extractor records the headers, and
the fetch records where a message was found (`mailbox:place`, the
account and the folder):

```console
$ ossuary find 'mail:subject=*invoice*' mail:from mail:date mailbox:place
```

An attachment is a file of its own, and [Finding a document by a phrase
in it](#finding-a-document-by-a-phrase-in-it) leads from its text to the
message.

**No seal and no sync.** Keep the archive on a disk that your backup
covers, and the backup has the mail as well. `seal` is for ids that you
send to someone who may later ask for proof (see [Sealing on a
schedule](sealing.md#sealing-on-a-schedule)). A seal in the hourly job
would write a new segment file to `claims/` every hour that brought
mail. `sync` seals the same way on every run; it is for a copy in a
second place, in a job of its own.

**Limits.** ossuary-mailvault has fetched far fewer mailboxes than the
Python tool [mailvault](https://github.com/sniner/mailvault), which
remains the safer choice for mail you need to keep.

## An inbox that is emptied after every run

**The situation.** A scanner or a phone app puts documents into a
folder, here `/home/john/inbox`. They are to go into the archive, and
the folder is to be empty again afterwards.

**Why `--collect`.** A plain `ingest` treats a file that is gone from a
folder it walked before as deleted and retracts its path. The file stays
in the archive, but `find` and `tree` no longer show it by default.
`ingest --collect` retracts nothing, so every document keeps the path it
had in the inbox, and `ossuary tree /home/john/inbox` lists everything
that went through it.

**The job.** This script ingests the inbox and then deletes the files
that were there before the run began:

```sh
#!/bin/sh
stamp=$(mktemp) || exit 1
ossuary ingest -q --collect /home/john/inbox >/dev/null &&
    find /home/john/inbox -type f ! -cnewer "$stamp" -delete
status=$?
rm -f "$stamp"
exit $status
```

`find` deletes only after an `ingest` without errors: an `ingest` that
could not read a file exits non-zero, and nothing is deleted. A file
that arrives while the job runs has a later change time than the stamp
and stays for the next run, also when it was copied with its old
modification time. Run the script from cron as in the job above.

**Limits.**

- Every `ingest` of the inbox needs `--collect`. A single plain
  `ingest` retracts the paths of everything that is no longer there;
  `find --all` still finds those files.
- A path keeps every file recorded at it. When the scanner names every
  scan `scan.pdf`, `tree` shows all of them at that path, each with its
  name in the archive.
- A file that is still being written while `ingest` reads it is stored
  as far as it was written, and the complete file is stored by the next
  run, at the same path. If the scanner or the app can write the file
  elsewhere and move it into the inbox when it is complete, let it.

## Where a file was, and on which machine

**The situation.** You have a copy of a photo, or only its name, and
want to know where it was kept, on which machine, and whether it still
is.

**1. Find the file.** By its name, with `--all`, so that paths which
no longer exist count as well:

```console
$ ossuary find --all 'file:name=DSC_1043.jpg' file:path prov:host
639514a1
  file:path=/home/john/photos/DSC_1043.jpg
  file:path=/srv/photos/2026/DSC_1043.jpg
  prov:host=atlas.example.net
  prov:host=nas.example.net
1 file(s)
```

With a copy of the file, `ossuary id FILE` prints its name in the
archive, whatever the copy is called.

**2. Which path on which machine.** `about` shows the claims in the
order they were written, each with its run. A path and a host from the
same run belong together:

```console
$ ossuary about 639514a1 file:path prov:host
2026-01-20T18:02:11Z  file:path = "/home/john/photos/DSC_1043.jpg"  [ingest] [run:c36ec53e-f486-4c74-ac19-d6dfb6341072]
2026-01-20T18:02:11Z  prov:host = "atlas.example.net"  [ingest] [run:c36ec53e-f486-4c74-ac19-d6dfb6341072]
2026-03-02T03:00:40Z  file:path = "/srv/photos/2026/DSC_1043.jpg"  [ingest] [run:434b8d04-416c-4d3d-9119-728ba2d48078]
2026-03-02T03:00:40Z  prov:host = "nas.example.net"  [ingest] [run:434b8d04-416c-4d3d-9119-728ba2d48078]
2026-10-30T19:45:07Z  retracted: file:path, 1 claim(s)  [ingest] [run:4241fed2-0c48-44ff-b3a8-384538619415]
```

The photo was on the laptop `atlas` from January and on the NAS from
March. The last line is the run that found it gone from the laptop and
retracted that path. `standing` prints the paths that are current:

```console
$ ossuary standing 639514a1 file:path
/srv/photos/2026/DSC_1043.jpg
```

**Limits.**

- `prov:host` records that the file was seen on a machine, and it is
  not retracted with the path. Where the file is now is in `file:path`.
- `ingest` records the name of the machine it runs on. A share of the
  NAS ingested from the laptop is recorded with the laptop's name and
  the path under the mount point. To record the NAS as the machine, run
  `ingest` on the NAS.

## The archive as it was last year

**The situation.** A folder looked different last year: some files have
been deleted since, a document has been changed. You want to see what
changed, and read the old version.

**1. What changed.** `tree --as-of` shows a folder as the archive
recorded it at a given time. A date means the end of that day, in UTC.
`diff` compares it with today:

```console
$ diff <(ossuary tree -q --as-of 2025-11-09 /home/john/docs) <(ossuary tree -q /home/john/docs)
2c2
< ├── draft.txt  [cba06b57]
---
> ├── letter.txt  [11507a0e]
4c4
< └── plan.txt  [3bfc2695]
---
> └── plan.txt  [3bfc2695] [a0e17e2b]
```

`draft.txt` has been deleted since, `letter.txt` is new, and `plan.txt`
has new content. A path keeps every file recorded at it, so today's
tree shows the old and the new name. The trees come from the record,
so this reads none of the stored files and is quick on a large archive.

**2. The old version.** `mount --as-of` shows the archive as it was at
that time, as a read-only directory that any program can read:

```console
$ mkdir ~/then
$ ossuary mount --as-of 2025-11-09 ~/then
mounted read-only at /home/john/then: 3 file(s) in 3 folder(s), as of 2025-11-09T23:59:59Z; press Ctrl-C to unmount
```

In a second terminal, compare the old state with the folder on the
disk, file by file:

```console
$ diff -rq ~/then/home/john/docs /home/john/docs
Only in /home/john/then/home/john/docs: draft.txt
Only in /home/john/docs: letter.txt
Files /home/john/then/home/john/docs/plan.txt and /home/john/docs/plan.txt differ
```

Ctrl-C in the first terminal, or `umount ~/then`, unmounts it.

**Limits.** `--as-of` shows what the archive had recorded at that time,
not what was on the disk. A file that was never ingested is not there,
and for a folder that is ingested once a month, the view shows the last
ingest before the date.
