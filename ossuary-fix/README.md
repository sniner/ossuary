# ossuary-fix

*Repair tool for 0.x archives after a breaking change.*

A 0.x archive collects scars: a word in the vocabulary that changed, a
claim an older version should have said. The programs stay free of
migration code; this one knows each scar by name and closes it. Each
fix reads the whole log, works out what is missing, and writes exactly
that; run twice, the second run finds nothing to do. `--dry-run` says
what would be written and writes nothing.

So far every fix closes its scar the way the record closes everything,
by adding. A scar that can only be closed by rewriting what is sealed
is not ruled out: a segment is named by its bytes and chained by that
name, so such a fix would re-seal the chain from there on. That is not
a clean job, which is why this program lives beside the archive and
not in it.

```console
$ ossuary-fix --dry-run origin
3730 origin(s) would be said again as prov:origin
$ ossuary-fix origin
3730 origin(s) said again as prov:origin, in a segment of their own
```

A fix that restates what an older version said keeps the original
moment, source and run, so as of any day the record reads as if the
right word had been used from the start. Such claims stand in a
segment of their own, sealed before and after, sorted among the old
ones; they override nothing.

## The fixes

- `origin` — until 0.6.3 a derived file's origin was recorded as
  `derive:derived-from`; the word is `prov:origin` now, and the present
  is asked in the new word, so a derived file whose origin stands only
  under the old one is held but not placed: `find`, `ls` and the mount
  no longer reach it. The fix says each origin still standing under the
  old word again under the new one. The old claims stay as they were
  said; an origin already standing as `prov:origin` is left alone
