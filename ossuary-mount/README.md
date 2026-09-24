# ossuary-mount

*Mount an ossuary archive as a read-only filesystem.*

`ossuary mount` shows every file the archive has a path for, at that
path, in a directory that any program can read. It is the same tree
`ossuary ls` and `ossuary tree` show. Photos open in the image viewer, a
file manager can browse them, previews work. Nothing can be written.

```console
$ ossuary mount ~/view
mounted read-only at /home/john/view: 5 file(s) in 5 folder(s); press Ctrl-C to unmount
$ open ~/view/home/john/photos/DSC_1042.jpg
```

`ossuary mount` runs this program as an
[outside verb](../ossuary-cli/README.md#outside-verbs): `ossuary` finds it
on the PATH as `ossuary-mount`, and it can also be called directly under
that name. It takes `--archive` and `OSSUARY_ARCHIVE` like every ossuary
command.

The command runs in the foreground. Ctrl-C unmounts, and so does `umount`
from another terminal. A mountpoint that does not exist is created, and
removed again when the mount ends.

## What is shown

Every path that `ossuary ingest` recorded, from every machine, in one
tree. A file is shown as long as its `file:path` claim is standing, even
if the disk it was on is gone. Derived files, such as attachments
unpacked from a mail, have no path and are not shown; `ossuary find`
finds them.

Writes fail with the operating system's read-only error. The tree is
built once when the mount starts and does not change while mounted;
files ingested in the meantime appear in the next mount.

## Past states

```console
$ ossuary mount ~/last-year --as-of 2026-01-01
```

`--as-of TIME` shows the archive as it was at that moment, in UTC. TIME
is `2026-01-01T08:00:00` (a trailing `Z` is optional) or `2026-01-01`,
which means the end of that day, as everywhere in `ossuary`. A run id, as
listed by `ossuary history`, means the moment after that run's last
claim. Files retracted since then are shown, files added since then are
not, and a path whose file has changed since shows the old content. Two
mounts at two moments can be compared with any tool that reads files.

## Conflicts

A filesystem cannot show everything the record holds. Two rules decide
what is shown; they apply only to the mount and do not change the
record:

* **One file per path.** If several files were recorded at the same path
  (the same path with different content over time), the file with the
  newest standing claim is shown; within the same second, the claim
  written later to the log wins.
* **A path is either a file or a folder.** If a path was recorded both as
  a file and as a folder, the folder is shown, so the files in it stay
  reachable. The file is shown next to it under a name with the start of
  its digest added (`notes-1f4c2a9b.txt`), the same naming that
  `ossuary export` uses for name collisions.

## Platforms

Neither platform needs root, and neither installs a kernel extension or
module.

* **macOS**: an NFS server on `127.0.0.1`, on a port the system assigns,
  mounted by the system's own NFS client (`mount -t nfs`, read-only,
  NFSv3 over TCP). Unmounting uses `umount`, and `diskutil unmount force`
  if a program still has a file open.
* **Linux**: a FUSE filesystem, mounted with `fusermount3` from the fuse3
  package that every distribution ships. The mount is read-only, so the
  kernel rejects every write before it reaches this program.

On other platforms the build fails with an error saying so.

## Memory use

Files are read directly from the archive, so a mount needs little
memory. At most 256 archive files are kept open; a file closed to stay
under that limit is opened again on its next read. Files the archive
stores compressed cannot be read in place: they are loaded completely,
and at most 512 MiB of them are kept in memory, the least recently
loaded removed first.

## License

Apache License 2.0, see [LICENSE](../LICENSE).
