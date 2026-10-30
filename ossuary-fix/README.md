# ossuary-fix

*The changes ossuary itself never makes.*

`ossuary` only adds to an archive: it never removes a claim and never
changes the digest of a sealed segment. Some repairs cannot be made that
way. An attribute name changed between versions, the form of a segment
header changed, a sealed segment or the open segment was lost and the
chain has to be closed over the gap, or the archive is to move to
another hash algorithm. `ossuary-fix` makes these changes, each as a
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
is the name of a segment, so every seal id given out for a renamed
segment no longer checks; the fix says from when on, and `ossuary seal`
gives the new id. Make a copy of the archive before running such a fix.
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
  closed a break. Both are gone, and a header now names one segment,
  `"previous":"3c1e…"`. Such an archive was read in the order of its
  segments' first claims; the fix links the segments in that order, so
  the record says what it said before, and rewrites every header as
  described above. Segments without claims, the mends and the segments
  that joined merged archives, are removed. Claims are not changed. Run
  it before any other command on an archive written by 0.14 or
  earlier. A segment that is missing and that no mend records as lost,
  or a missing open segment, stops it; `--accept-loss` goes on without
  them. An archive in the current form is left as it is.
- `chain`: closes a broken chain, for a sealed segment that is missing
  and that no copy of the archive has. The segment after the gap is
  made to name the one before it, and a missing open segment is begun
  anew after the last sealed one. Nothing that is still linked changes
  its place, because the order of the chain is the order of the record:
  seal ids up to the gap still check. Segments that nothing links to the
  rest are put at the end and named. Where the chain is broken in more
  than one place and the order of the pieces cannot be told, the fix
  stops. The claims of what is missing are lost, so the fix names each
  loss and goes on only with `--accept-loss`. Copy a missing file back
  from a copy of the archive instead, if there is one.

  ```console
  $ ossuary-fix chain
  segment 9ae8aebc… is missing (segment 31fc5515… names it); the chain is closed over it, and the claims it held are lost
  ossuary-fix: nothing was written: copy the missing files back from a copy of the archive and run again, or pass --accept-loss if there is no copy
  ```
- `rehash`: copies the archive into a new one that uses another hash
  algorithm, as `ossuary init --algorithm` takes it. Every file in
  `content/` and `derived/` is hashed anew and stored in the new archive,
  and every claim is written there with the new names, in the order of
  the log, with its time, source and run unchanged. `config.toml`,
  `mailvault.toml` and the mail fetcher's memory of where each folder's
  fetch stands are copied. The archive itself is only read; the new one
  is created at the directory given, which must not exist or be empty.
  A damaged file, or a claim about a file the archive does not hold,
  stops the run; `--force` leaves them out.

  ```console
  $ ossuary-fix --archive ~/archive rehash --algorithm blake3 ~/archive-blake3
  ```
