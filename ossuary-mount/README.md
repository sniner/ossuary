# ossuary-mount

*The record as a read-only filesystem.*

A reading room for every place the record knows: the same forest
`ossuary ls` and `ossuary tree` answer with, grafted onto a directory so
any program can read the archive's files where they once stood. Photos
open in the viewer, a file manager browses them, previews work — and
nothing can be written.

```console
$ ossuary mount ~/view
the record stands at /home/john/view — read-only, 5 file(s) in 5 folder(s); Ctrl-C gives it back
$ open ~/view/home/john/photos/DSC_1042.jpg
```

`ossuary mount` is this program: an
[outside verb](../ossuary-cli/README.md#outside-verbs), found on the PATH
as `ossuary-mount`, and callable directly under that name. It takes
`--archive` and `OSSUARY_ARCHIVE` the way every ossuary command does.

The command stays in the foreground. Ctrl-C gives the directory back, and
so does a plain `umount` from another terminal. A mountpoint that was not
there is created, and one this command created goes with the mount when
it ends.

## What is in the room

Every place any ingest ever saw, from every machine, in one forest — not
a disk. A place answers as long as its claim stands, however long the
disk it named is gone. Beside the files stand no others: derived files
never sat anywhere, so an unpacked attachment is reachable through
`ossuary find`, not here.

Writing is answered by the operating system itself: read-only
filesystem. The view is grown once, when the mount begins, and does not
change for the life of the mount — an ingest in another terminal is seen
by the next mount, not by this one.

## The moment it answers for

```console
$ ossuary mount ~/last-year --as-of 2026-01-01
```

`--as-of TIME` is the record as it was known at that moment, UTC —
`2026-01-01` or `2026-01-01T08:00:00`, a trailing `Z` welcome. Files
since retracted stand again, files since arrived are absent, and a place
whose file changed shows the old bytes. Two mounts side by side are two
moments side by side, comparable with any tool that reads files.

## Where the view narrows

A filesystem can say less than the record does, and both narrowings are
this program's declared policy, never the record's:

* **One file per name.** Of several files standing at one place — the
  same path, different bytes over time — the newest surviving assertion
  wins, log order breaking ties within a second.
* **One thing per name.** Where a name stands as file and folder at
  once, the folder keeps it, because its children must stay reachable;
  the file steps aside under a name carrying the start of its digest
  (`notes-1f4c2a9b.txt`), the spelling `export` uses for its collision
  bumps.

## The doors

Neither door asks for root, and neither installs anything kernel-side.

* **macOS** — an NFS server on `127.0.0.1`, on a port the system hands
  out, mounted by the operating system's own client (`mount -t nfs`,
  read-only, NFSv3 over TCP). Giving the room back is `umount`, and
  `diskutil unmount force` when something still reads it.
* **Linux** — a FUSE filesystem, mounted through `fusermount3` from the
  fuse3 package every distribution ships. The mount is declared
  read-only, so the kernel answers every write before it reaches this
  program.

No other platform has a door; building elsewhere refuses with that in
words.

## What it holds while it serves

Files are read in place out of the store, so a mounted archive costs
little memory. Two bounds keep it that way: at most 256 store files stay
open — a closed one is simply opened anew on its next read — and entries
the store keeps compressed, which cannot be read in place, are loaded
whole with at most 512 MiB of them kept at hand, least recently loaded
let go first.

## License

Apache License 2.0 — see [LICENSE](../LICENSE).
