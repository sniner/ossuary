# ossuary-sync

*Copy an ossuary archive to a second place, and keep the copy up to date.*

`ossuary sync TARGET` copies the archive to the directory TARGET, for
example a backup disk or a share mounted from a NAS, to a folder in a
filebridge location, or to a folder on a WebDAV server. The first run
copies everything. Each later run copies only what was recorded since
the last one, and does not have to read through the copy to find out
what that is. The copy is an archive of its own: every ossuary command
can open it, and `ossuary seal --check` shows whether it holds what it
should.

```console
$ mkdir /mnt/backup/archive
$ ossuary --archive /srv/archive sync /mnt/backup/archive
archive /srv/archive
target /mnt/backup/archive
step 1 of 4: sealing the archive, after checking every sealed segment
open segment sealed
step 2 of 4: working out what the target lacks
the target holds the seal 52e6c7bbed43568c33a419f13edc574e36cb6f95c06b9024d04b9c13896216de
the target lacks 412 file(s), 1.3 GiB, and 3 segment(s)
step 3 of 4: copying 412 file(s)
step 4 of 4: copying 3 segment(s), then the open segment
copied 412 file(s), 1.3 GiB
the target holds the seal 1bbb4b0cc3a691061154ec0a228cb409aa3266c5bb90a7182c9164b02e153776; `ossuary --archive /mnt/backup/archive seal --check 1bbb4b0cc3a691061154ec0a228cb409aa3266c5bb90a7182c9164b02e153776` checks it
1bbb4b0cc3a691061154ec0a228cb409aa3266c5bb90a7182c9164b02e153776
```

`ossuary sync` runs this program as an
[outside verb](../ossuary-cli/README.md#external-commands): `ossuary`
finds it on the PATH as `ossuary-sync`, and it can also be called
directly under that name. It takes `--archive` and `OSSUARY_ARCHIVE`
like every ossuary command. Its own options, `-q`, `-v`, `--dry-run`
and `--accept-any-certificate`, go after `sync`.

## What a run does

1. It seals the archive, as `ossuary seal` does. If the record has
   findings, no seal is made and nothing is copied; `ossuary seal` lists
   the findings and the way out.
2. It reads the copy's open segment, which names the seal the copy
   holds, and works out from the archive's record which files and
   sealed segments the copy lacks.
3. It copies those files, each one checked against its hash first.
4. It copies the sealed segments, and last replaces the copy's open
   segment, so that it names the new seal.

The copy holds the new seal only after the last step. A run that is
interrupted, by a full disk or a lost connection, leaves the copy at the
seal it had. The next run copies what is still missing and skips what
arrived.

On a WebDAV server, a run interrupted at the very end, while it replaces
the copy's open segment, can leave the copy without one. The next run
then finds every file already in the copy and writes the open segment
again.

The only line on stdout is the seal id the copy now holds, as with
`ossuary seal`, so a script can check the copy right away:

```console
$ id=$(ossuary sync -q /mnt/backup/archive)
$ ossuary --archive /mnt/backup/archive seal --check "$id" --fast
```

`--dry-run` shows how much would be copied and writes nothing, not even
the seal.

## The target

The target is a directory, a folder in a filebridge location, or a
folder on a WebDAV server. It must exist, and it must be empty for the
first sync. For a share, name a directory on it, not the mount point
itself: when the share is not mounted, the mount point is an empty
directory on the local disk, and the archive would be copied there.

A target that holds other files is refused. So is a copy that was
changed by other means than `ossuary sync`: one with a different
`FORMAT`, such as another hash algorithm, one whose open segment holds
claims, and one at a seal that the archive's record does not lead to.
Another kind of server, such as SFTP or SMB, can be used once it is
mounted.

### A folder in a filebridge location

[filebridge](https://github.com/sniner/filebridge) serves directories
over HTTP. A folder in one of its locations is named as
`filebridge+http://HOST:PORT/LOCATION/FOLDER`, or with
`filebridge+https://` for a server behind a proxy that does TLS. The
proxy's certificate is checked against the system's certificate store,
so a certificate from an internal certificate authority works once that
authority is installed there:

```console
$ read -rs OSSUARY_SYNC_TOKEN && export OSSUARY_SYNC_TOKEN
$ ossuary sync filebridge+http://backup.example.com:8000/archive/ossuary
```

For a location with a token, set `OSSUARY_SYNC_TOKEN`. filebridge then
signs every request and encrypts the files and their names, over
`http://` as well. Without a token everything goes in the clear; use
such a location only on a network you trust.

The location must allow reading, creating, replacing and deleting
files, listing them, and making folders: set `allow_read`,
`allow_create`, `allow_replace`, `allow_delete`, `allow_inspect`,
`allow_recurse` and `allow_mkdir` to true in the configuration of
filebridged. sync checks this before it copies anything. A file is
written under a temporary name and renamed into place, which takes
replacing and deleting, so whoever has the token can also delete files
in the copy.

To check the copy, run `ossuary seal --check` on the server, in the
location's directory.

### A folder on a WebDAV server

Prefer a directory or filebridge where you can. Over WebDAV each file
takes several requests, so a first sync is slow, and a server that
limits the size of an upload refuses large files; sync gets around that
only on Nextcloud, which it recognises by its address.

A folder on a WebDAV server, such as Nextcloud or a NAS, is named by its
URL. The login is read from `OSSUARY_SYNC_USERNAME` and
`OSSUARY_SYNC_PASSWORD`:

```console
$ export OSSUARY_SYNC_USERNAME=backup
$ read -rs OSSUARY_SYNC_PASSWORD && export OSSUARY_SYNC_PASSWORD
$ ossuary sync https://cloud.example.com/remote.php/dav/files/backup/archive
```

Without the two variables, sync sends no login. A URL with a login in it
is refused, and so is a password over `http://` to another machine; name
the `https://` URL.

A server whose certificate is not signed by a known certificate
authority, such as a NAS with a self-signed one, is refused.
`--accept-any-certificate` accepts its certificate without checking it.
sync then cannot tell the server from another machine answering in its
place, which would receive the login, so use it on a network you trust.

A large file needs no more memory than a small one.

On Nextcloud, name the folder by the WebDAV address that Nextcloud
shows in the settings of its Files app,
`https://HOST/remote.php/dav/files/USER/…`. A file larger than 10 MiB
then goes up in parts of 10 MiB, as with Nextcloud's own apps, so a
limit on the size of an upload, as a proxy in front of Nextcloud
commonly sets one, does not stop it. If the server refuses parts of
10 MiB, the rest of the run sends parts of 1 MiB.

`ossuary seal --check` and the other ossuary commands open a directory.
To check a copy on a WebDAV server, mount the folder, for example with
`rclone mount` or davfs2, and name the mounted directory.

## What is copied

`FORMAT`, `content/`, `derived/`, `claims/` and the open segment. Files
are copied as they are stored, compressed or not.

Not copied are `cache/`, which ossuary rebuilds when it needs it,
`mailvault.toml`, which may hold passwords, and `config.toml`. A file
that no claim refers to, as an interrupted run can leave one, is not
copied either; a seal does not cover it.

Nothing in the copy is deleted, and nothing but its open segment is
changed. A file that `ossuary maintain weed` removes from `derived/`
stays in the copy.

## When a file is missing or damaged

A file that the record refers to and that is missing or damaged in the
archive is not copied, and the copy is not brought up to the new seal:

```console
$ ossuary sync /mnt/backup/archive
…
1 file(s) missing here:
  800a3760695414c4e2ff62055f94c7d7acf2ba441314e41d647f679878c81842
the target was not brought up to the seal 1bbb4b0cc3a691061154ec0a228cb409aa3266c5bb90a7182c9164b02e153776: 1 file(s) above; it still holds the seal 52e6c7bbed43568c33a419f13edc574e36cb6f95c06b9024d04b9c13896216de; the 411 file(s) copied stay there; `ossuary audit` lists what is missing or damaged here, sync again once it is restored
```

The command exits 1. Once the file is back, the next run copies it and
everything else the copy still lacks.

## What sync does not check

To find out what the copy lacks, sync reads only the copy's open
segment. A file that was deleted or damaged in the copy afterwards goes
unnoticed. `ossuary --archive DIR seal --check ID` on the copy finds
it; for a filebridge or WebDAV folder, run it on the server or on the
mounted folder. Run it from time to time, with `--fast` for a quick
look.

## License

Apache License 2.0, see [LICENSE](../LICENSE).
