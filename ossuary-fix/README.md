# ossuary-fix

*Repair tool for 0.x archives after a breaking change.*

`ossuary-fix` updates an archive written by an older 0.x version after a
breaking change in the vocabulary or the format. It is not the same as
`ossuary maintain`, which keeps an archive in order and never rewrites a
sealed segment; a fix may have to do that. `ossuary-fix` is meant only
for 0.x archives. Once the archive format is stable, it should not be
used on an archive.

Between 0.x versions, attribute names change, an older version may
have left out claims that a newer version expects, and the form of a
segment header may change. The ossuary programs contain no migration
code; each such change has a fix here instead, run as a subcommand. A
fix reads the whole log and writes only what is missing or in an old
form. Running it a second time writes nothing. With `--dry-run` it shows
what it would write and writes nothing.

Most fixes only add claims. A fix that changes a sealed segment rewrites
it: each segment is named by its hash, and every later segment names
its predecessor by that name, so the rewritten segment gets a new name,
and every segment after it is rewritten to name it. The old files are
removed from `claims/` after the new ones are written, and the query
index in `cache/` is removed with them; the next command rebuilds it.
Make a copy of the archive before running such a fix. An archive
written by a version before the change is not read by the current
version until the fix has run: every command stops at the first segment
in the old form.

```console
$ ossuary-fix --dry-run origin
3730 origin(s) would be copied to prov:origin
$ ossuary-fix origin
3730 origin(s) copied to prov:origin, in a separate segment
```

A fix that restates claims written by an older version keeps their
original time, source and run, so a query for any past date gives the
same result as if the new attribute had been used from the start. The
fix seals the open head before and after writing these claims, so they
are in a separate segment, sorted by time among the old segments. They
do not override any claim written later.

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
- `previous`: until 0.10.1 the header of a sealed segment named the
  segment sealed before it as a string, `"previous":"3c1e…"`. It is now
  a list, `"previous":["3c1e…"]`, so that a segment can name several
  segments. The fix rewrites the header of every segment in the old form
  and of the open segment, along the whole chain, as described above.
  Claims are not changed. Run it before any other command on an archive
  written by 0.10.1 or earlier
