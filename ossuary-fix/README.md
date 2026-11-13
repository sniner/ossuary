# ossuary-fix

*Repairs ossuary archives and converts archives of older versions to the
current format.*

`ossuary` only adds to an archive: it never removes a claim and never
changes the digest of a sealed segment. Some repairs cannot be made that
way. An attribute name changed between versions, the form of a segment
header changed, a sealed segment or the open segment was lost and the
gap in the chain has to be closed, the archive is to move to another
hash algorithm, or the claims of the extractors are to be removed so
that the files can be examined anew. `ossuary-fix` makes these changes, each as a
subcommand. The ossuary programs contain no migration code.

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
