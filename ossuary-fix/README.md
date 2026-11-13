# ossuary-fix

*Repairs ossuary archives and converts archives of older versions to the
current format.*

`ossuary` only adds to an archive: it never removes a claim and never
changes the digest of a sealed segment. Some repairs cannot be made that
way. An attribute name changed between versions, the form of a segment
header changed, a sealed segment or the open segment was lost and the
gap in the chain has to be closed, the archive is to move to another
hash algorithm, the claims of the extractors are to be removed so that
the files can be examined anew, or a file is to be removed from the
archive or replaced by another. `ossuary-fix` makes these changes, each
as a subcommand. The ossuary programs contain no migration code.

A fix reads the whole log and writes only what is missing or in an old
form. Running it a second time writes nothing. With `--dry-run` it shows
what it would write and writes nothing.

Some fixes only add claims. A fix that changes a sealed segment rewrites
it: each segment is named by its hash, and every later segment names
its predecessor by that name, so the rewritten segment gets a new name,
and every segment after it is rewritten to name it. The query index in
`cache/` is removed first, the new files are written to `claims/`, and
the old ones are removed; the next command rebuilds the index. A seal id
is the name of a segment, so a seal id issued for a renamed segment no
longer passes `ossuary seal --check`; the fix prints since when seal ids
are affected, and `ossuary seal` prints the new id. Make a copy of the
archive before running such a fix.
An archive written by a version before a change in the header is not
read by the current version until the fix has run: every command stops
at the first segment in the old form.

```console
$ ossuary-fix --dry-run origin
3730 origin(s) would be copied to prov:origin
$ ossuary-fix origin
3730 origin(s) copied to prov:origin
```

A fix that restates claims written by an older version keeps their
original time, source and run. They are appended to the log like any
other claims, and the order of the record is the order of the log, so
they come after everything recorded before the fix.

## The fixes

- `open`: until 0.17 the open segment was one file, `head.jsonl`, and a
  retraction contained the value it retracted; 0.18 kept the open
  segment as a directory in the archive root. The open segment is now a
  directory under `open/` with one file per append, and a retraction
  names the claims it retracts by id. The fix moves the open segment
  into `open/`, rewrites every retraction to name the claims that were
  standing when it was written, leaves out retractions that had nothing
  to retract, and seals the open segment. Assertions are not changed.
  Other commands do not open an archive in an older format, so run this
  fix first; an archive written by 0.14 or earlier needs `ossuary-fix
  list` of ossuary 0.17 before it
- `origin`: until 0.6.3 the origin of a derived file was recorded as
  `derive:derived-from`; it is now `prov:origin`. A derived file whose
  origin is recorded only under the old attribute is still in the
  archive, but `find`, `ls` and `ossuary-mount` do not show it. The fix
  copies each standing `derive:derived-from` value to `prov:origin`. The
  old claims are not changed; origins already recorded as `prov:origin`
  are skipped
- `packed`: until 0.7.0 the packed extractor read only zip files and
  used the `zip` namespace: the entries of a zip file were recorded on
  it as `zip:entry`, the path of an unpacked entry was recorded on the
  entry as `zip:path`. Paths inside a packed file now begin with `@` and
  are recorded as `packed:path` on the packed file and as `file:path` on
  the entry, for every format, so one `find` term matches both. The fix
  copies each standing `zip:entry` value to `packed:path` and each
  standing `zip:path` value to `file:path`, with a leading `@`. The old
  claims are not changed; paths already recorded under the new attribute
  are skipped
- `list`: until 0.14 a segment header named the segments sealed before
  it as a list, `"previous":["3c1e…"]`, so that a segment could follow
  several: after `ossuary merge`, or after `ossuary maintain mend` had
  closed a break. Both commands were removed, and a header now names one
  segment, `"previous":"3c1e…"`. Such an archive was read in the order
  of its segments' first claims; the fix links the segments in that
  order, so the claims are read in the same order as before, and
  rewrites every header as described above. Segments without claims, the mends and the segments
  that joined merged archives, are removed. Claims are not changed. Run
  it before any other command on an archive written by 0.14 or
  earlier. A segment that is missing and that no mend records as lost,
  or a missing open segment, stops it; `--accept-loss` goes on without
  them. An archive in the current form is left as it is.
- `chain`: closes a broken chain. If a sealed segment is missing and no
  copy of the archive has it, the segment after the gap is linked to the
  one before it; a missing open segment is replaced by a new, empty one
  after the last sealed segment. Segments that are still linked keep
  their order, so seal ids up to the gap still pass `ossuary seal
  --check`. Segments not linked to the rest are moved to the end of the
  chain and listed. If the chain is broken in more than one place and
  the order of the pieces is unknown, the fix stops. The claims of what
  is missing are lost, so the fix lists each loss and continues only
  with `--accept-loss`. If a copy of the archive has a missing file,
  copy it back instead.

  ```console
  $ ossuary-fix chain
  segment 9ae8aebc… is missing (segment 31fc5515… names it); its claims are lost when the chain is closed without it
  ossuary-fix: nothing was written: copy the missing files back from a copy of the archive and run again, or pass --accept-loss if there is no copy
  ```
- `rehash`: copies the archive into a new one that uses another hash
  algorithm, as `ossuary init --algorithm` takes it. Every file in
  `content/` and `derived/` is hashed again and stored in the new
  archive, and every claim is written there with the new names, in the
  order of the log, with its time, source and run unchanged.
  `config.toml`, `mailvault.toml` and the fetch state of
  ossuary-mailvault in `cache/` are copied. The archive itself is only
  read; the new one is created at the directory given, which must be
  empty or not exist. A damaged file, or a claim about a file that is
  not in the archive, stops the run; `--force` leaves them out.

  ```console
  $ ossuary-fix --archive ~/archive rehash --algorithm blake3 ~/archive-blake3
  ```
- `unextract`: removes every claim written by `ossuary extract`: what
  the extractors found in the files, the records that a file was
  examined, and the claims about derived files. A derived file that no
  other claim names is removed from `derived/`. Afterwards the archive
  is as it was before `ossuary extract` first ran, and the next run
  examines every file again. Use it when an extractor writes its values
  in another form than before and the old values are not to stay in the
  archive. Claims of other sources are not changed. A retraction by
  another source is removed when every claim it names is removed. A
  derived file that another claim names, a tag of the user for example,
  stays in `derived/` without its recorded size and type; `ossuary
  maintain describe` records both again, and `ossuary extract` skips the
  file until then. Files in `content/` are not touched. With one or more
  sources, only the claims of these extractors are removed: a source as
  the claims name it (`extractor:pdf-text/1`), or without its generation
  (`extractor:pdf-text`) for every generation.

  ```console
  $ ossuary-fix --dry-run unextract
  extractor:pdf-text/1: 13 claim(s) to remove
  1 file(s) in derived/ that no other claim names to remove
  the open segment would be rewritten without the claims of extractors; 1 segment(s) without claims would be removed; 1 unchanged
  every seal id issued so far would no longer pass `ossuary seal --check`
  ```
- `remove`: removes files from the archive: each named file from
  `content/`, and every claim about it from the log. Afterwards no
  command shows that the file was ever stored. Use it for a file that
  should not be in the archive. The files are named by their full names,
  as `ossuary find --id` prints them, on the command line or one per line
  in a file given with `--list`. Files extracted from a removed file are
  removed as `unextract` removes them: the claims of the extractors about
  them, and the file in `derived/` unless another claim names it. A file
  that was also extracted from a file that stays, or that is in
  `content/` itself, only loses the claim that it was extracted from the
  removed one. Removing a file does not keep it out: if it is still in a
  directory that is ingested or in a mailbox that is fetched, a later run
  can add it again (`ossuary ingest` once the file has changed or the
  caches were deleted, `ossuary mailvault fetch --full`).

  ```console
  $ ossuary find --id file:name=draft.txt
  ed0f32d539e78005662fbdc31a020c7f7c4b8c389a0b02f00e4b3ba4beb9870a
  $ ossuary-fix --dry-run remove ed0f32d539e78005662fbdc31a020c7f7c4b8c389a0b02f00e4b3ba4beb9870a
  7 claim(s) about the file(s) to remove
  1 file(s) to remove from content/
  2 segment(s), and the open segment would be rewritten without the claims about the removed files; 1 file(s) would be removed
  every seal id issued so far would no longer pass `ossuary seal --check`
  ```
- `replace`: replaces a file by another file that is already in the
  archive. Use it when the archive holds a damaged copy and the intact
  file has been found: a message that was fetched without its body, a
  download that was cut short. `replace OLD NEW` names one pair;
  `--list FILE` reads pairs from a file, one per line, the two names
  separated by a space or a tab. OLD is removed as `remove` removes it.
  NEW gets the standing claims that say where OLD was seen and what was
  recorded about it by hand: `file:path`, `file:name`, `file:modified`,
  `prov:host`, `mailbox:place`, `mailbox:tag`, `mailbox:id`, `user:tag`,
  `user:comment` and every other attribute that no extractor wrote. Each
  is written with the time, source and run of the old claim; a value NEW
  already has is not written again. The size and type of OLD and what
  the extractors found in it describe its bytes and are not copied; run
  `ossuary extract` for NEW. Several files can have the same
  replacement. The fix does not know whether OLD and NEW are the same
  document. With `--same ATTRIBUTE` it replaces only if both files have
  the same values of that attribute, `mail:message-id` for example, and
  writes nothing if one pair does not. [The example
  below](#example-messages-archived-without-their-bodies) shows a whole
  run.

Both fixes can be run again with the same names: a file that is already
gone is counted and skipped.

## Example: messages archived without their bodies

John moved his mail to another provider in 2019. The move copied some
messages of the folder `Archive/2010` without their bodies, and
`ossuary mailvault fetch` archived them as the new mailbox has them:
the headers and nothing else. A backup of the old mail server from
before the move has the complete messages, one file per message, in
`/srv/backup/mail-2019`. The archive is to have the complete messages
in place of the damaged ones.

**1. Add the messages of the backup.** `ossuary mailvault add` stores
each file as a message and records the directory it was in as its place
(see [ossuary-mailvault](../ossuary-mailvault/README.md#adding-messages-from-files)).
`--seen` gives the claims the date of the backup.

```console
$ cd /home/john/archive
$ ossuary mailvault add /srv/backup/mail-2019 --account backup-2019 --seen 2019-10-05
archive /home/john/archive
adding the messages in /srv/backup/mail-2019 as account backup-2019
3 files in /srv/backup/mail-2019
3 messages stored; 9 claim(s) written, run e63f464b-37d0-4a54-b4c2-d96c8b947d59
```

A message of the backup that the archive did not have at all is now
simply in the archive. A message that is intact in both is one file
with two places. A damaged message is in the archive twice, as two
files:

```console
$ ossuary extract mail:headers
$ ossuary find 'mail:message-id=1@example.org' mailbox:place file:size
c4c22d52
  mailbox:place=example.org:Archive/2010
  file:size=152
dd43448e
  mailbox:place=backup-2019:Archive.2010
  file:size=186
2 file(s)
```

**2. List the pairs.** The list has one line per damaged file: its full
name, then the full name of its replacement. For a few files, write it
by hand with the names `ossuary find --id` prints. For a whole folder,
join the two listings on the Message-ID:

```console
$ ossuary find --json 'mailbox:place=example.org:Archive/2010' mail:message-id \
    | jq -r '[."mail:message-id"[0], .subject] | @tsv' | sort > old.tsv
$ ossuary find --json 'mailbox:place=backup-2019:Archive.2010' mail:message-id \
    | jq -r '[."mail:message-id"[0], .subject] | @tsv' | sort > new.tsv
$ join -t "$(printf '\t')" -o 1.2,2.2 old.tsv new.tsv | awk '$1 != $2' > pairs.txt
$ cat pairs.txt
c4c22d52b9d5742f852fe6bc2252b34127f52c7b3fafef27735361879673a5de	dd43448ed7ea3583e43ee5a5b1fdf0ac7ab9c164bdd7cc823e59409637818499
9eb15a4427ada90d027b47e176205e5fcaa70565a7e5744a9b476930f0ec00bc	437ef63b4e1afce5f5d1310b34a1a4af9c8fa5fb64af2b9ae3407001eef578ef
```

`awk` leaves out the messages that are one file in both listings. Read
the list before using it. It pairs every message of the folder whose
copy in the backup has other bytes, whether it is damaged or only
differs in a header that a server added. Which of the files are damaged
is not on record; here they are the ones without a body, the smaller
file of each pair.

**3. Look at what the fix would do.**

```console
$ ossuary-fix --dry-run replace --list pairs.txt --same mail:message-id
mailbox:place: 2 claim(s) to move to the replacement(s)
user:tag: 1 claim(s) to move to the replacement(s)
18 other claim(s) about the replaced file(s) to remove
2 file(s) to remove from content/
2 segment(s), and the open segment would be rewritten with the replacements in place of the replaced files; 2 file(s) would be removed
every seal id issued so far would no longer pass `ossuary seal --check`
```

The two places in the mailbox move to the complete messages, and so
does a tag John had put on one of the damaged ones. The 18 other claims
are the sizes and types of the damaged files and what the mail
extractor read from them. `--same mail:message-id` stops the run if a
line of the list pairs two different messages:

```console
ossuary-fix: 1 pair(s) do not have the same mail:message-id, the first is 63196f41… and 6ce3a189…: the values differ; nothing was written
```

**4. Copy the archive, then run the fix.**

```console
$ ossuary-fix replace --list pairs.txt --same mail:message-id
mailbox:place: 2 claim(s) to move to the replacement(s)
user:tag: 1 claim(s) to move to the replacement(s)
18 other claim(s) about the replaced file(s) to remove
2 file(s) to remove from content/
2 segment(s), and the open segment rewritten with the replacements in place of the replaced files; the query index in cache/ was removed and is rebuilt by the next command; 2 file(s) removed
every seal id issued so far no longer passes `ossuary seal --check`; run `ossuary seal` and keep the new id instead
```

The complete message now has the place in the mailbox beside the place
in the backup, with the time at which the damaged one was fetched:

```console
$ ossuary about dd43448e mailbox: user:
2019-10-05T00:00:00Z  mailbox:place = "backup-2019:Archive.2010"  [mailvault] [run:e63f464b-37d0-4a54-b4c2-d96c8b947d59]
2026-08-02T10:15:00Z  mailbox:place = "example.org:Archive/2010"  [mailvault] [run:f2045009-29c5-426d-887a-ca737222dbde]
2026-09-14T18:02:11Z  user:tag = "tax"  [user] [run:95d2c8a3-b412-4200-9575-a1791bea53fe]
```

**5. Finish.** `ossuary extract` examines the new files, and `ossuary
seal` prints the new seal id.

The mailbox still has the damaged messages. A fetch asks only for
messages that arrived since the last run and does not see them, and
`fetch --compare` finds each of them in the archive: the complete
message has the same Message-ID at the same place. `fetch --full`
downloads every message and stores the damaged ones again. Keep
`pairs.txt` and run the same `replace` once more after such a fetch, or
put the complete messages back into the mailbox.
