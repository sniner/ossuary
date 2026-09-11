# ossuary-mailvault

*Mail into the archive.*

> **An early proof of concept.** This program is here to be tried and
> played with, not to archive anything that matters. It may change at any
> time, in any way, with no regard for what an earlier build wrote into an
> archive. Mail that is to be kept is still the job of
> [mailvault](https://github.com/sniner/mailvault), the other project.

`ossuary mailvault` fetches whole mailboxes over IMAP into an ossuary
archive. A message goes in once, however many folders and accounts carry
it, and onto the record goes where it was seen — account and folder — so
"which mailbox was this in" is a question the archive answers for good.
Everything after that is ossuary's: `extract mail` reads the headers and
unpacks what a message carries, `find` asks, `get` answers.

```console
$ cd /home/john/archive
$ ossuary mailvault --allow-exec
archive /home/john/archive
example.org: 2 folders
example.org/INBOX: carrying on above UID 1180 — 24 messages to fetch
example.org/INBOX: 24 of 24 fetched
example.org/Sent: carrying on above UID 310 — 0 messages to fetch
24 messages new to the archive; 96 claim(s) written as run 315e360b-020e-48be-8f2d-f2002a2ea9b4
```

`ossuary mailvault` is this program: an
[outside verb](../ossuary-cli/README.md#outside-verbs), found on the PATH
as `ossuary-mailvault`, and callable directly under that name. It takes
`--archive` and `OSSUARY_ARCHIVE` the way every ossuary command does.

## The mailboxes

They stand in `mailvault.toml` in the archive root — its own file, one
`[[account]]` table per mailbox, read strictly: a key this program does
not know is refused rather than skipped.

```toml
[[account]]
name = "example.org"                  # what the record calls this mailbox
host = "imap.example.org"
user = "john@example.org"
password_cmd = "pass show mail/example.org"
folders = ["INBOX", "Sent"]

[[account]]
name = "bridge"                       # a local bridge, plaintext on loopback
host = "127.0.0.1"
port = 1143
tls = false
user = "john@example.com"
password = "bridge-password"
```

| | |
|---|---|
| `name` | how the record names this mailbox in every sighting. Renamed, new sightings carry the new name and the old ones keep the old — and the folders are fetched whole once, since the resume points are kept by name |
| `host`, `port`, `tls` | the server; `993` and TLS unless said otherwise. Plaintext is for a bridge on loopback, never a real server |
| `user`, `password` | the login. `password_cmd` names a command that prints the password on its first line instead — a password manager — and runs only under `--allow-exec`; what it says on stderr, and what it asks for, reaches the terminal |
| `folders` | which to fetch; every folder the server offers when absent. Ask the server what it calls them — Gmail's `[Gmail]/All Mail` is `[Google Mail]/Alle Nachrichten` on another account |

Naming accounts on the command line fetches those alone:
`ossuary mailvault example.org`.

## What goes on the record

Each message is taken in like a file `ingest` finds — its size, its
kind (`message/rfc822`, said outright: the fetcher knows what it holds),
the run it arrived in — and one claim per place it was seen:

```
mailbox:place = "example.org/INBOX"
```

The account's name, a slash, the folder as the server spells it, in one
value because the two belong together: a message in two folders holds
two, a message fetched from two accounts holds one from each. It is
information, the way `file:path` is — where the message was when it was
fetched. An account renamed later is a new name in new sightings; the
old ones stay true for their time.

```console
$ ossuary find 'mailbox:place=example.org/Sent' mail:subject
```

## Where a run carries on

Each folder's resume point lives in `cache/`: the UIDVALIDITY the
server promised last time, and the highest UID fetched under it. The
next run asks the server only for what lies above, and the server
answers with the new messages alone. A folder whose UIDVALIDITY the
server changed is fetched again whole — the bytes are not stored twice,
and the places are said again. The resume point is a cache like every
file in `cache/`: losing it costs one whole fetch of the folder, never
a claim. The server's numbering stands nowhere on a message's record;
it means nothing once the UIDVALIDITY changes.

`--full` fetches every folder whole regardless. `--dry-run` says what a
run would fetch and writes nothing. Both narrate folder by folder on
stderr; the verdict on stdout says which clean outcome it was.

An account that will not answer — the password, the login, a folder
that will not open — is named and costs only itself; the run goes on to
the next, and exits `1` at the end.

## Taking a mailvault archive over

An archive of the Python [mailvault](https://github.com/sniner/mailvault)
can be taken over whole, message for message:

```console
$ ossuary mailvault --from-vault /srv/mailvault/private
archive /home/john/archive
taking over the vault at /srv/mailvault/private
reading the vault's log: where every message was seen
131,504 messages in 3,207 log files, filed in 140,222 places
taking over: 131,504 of 131,504 message(s), 131,504 new to the archive
131,504 messages new to the archive; 543,502 claim(s) written as run 0c1e…
```

Every message the vault's log names goes in with every place the log
saw it in, as `mailbox:place` — the same fact a fetch records — and
with the date the log first saw it there, as `mailbox:seen`: a fetch's
sighting is its claim's own time, a takeover's lies years before it.
Each message is held against its own name on the way; a damaged file is
named and left out. A takeover is long, and interrupted it simply
carries on: a memo in `cache/` remembers which places are said, and
`--full` ignores it. A message that gained a place since is read again
and said with the new one. Naming mailboxes takes over those alone.

The seam does not show afterwards: a fetch of the same folder later
finds the same messages already held and says the same place again.

## Exit codes

`0` when everything asked for went in, `1` when anything was named as
failed — and for an archive that will not open, a `mailvault.toml` that
will not read, or a directory that is no vault.

## License

Apache License 2.0 — see [LICENSE](../LICENSE).
