# ossuary-mailvault

*Fetch mail into an ossuary archive.*

> **An early proof of concept.** This program is for trying out, not for
> archiving mail that matters. It may change at any time, in any way,
> without regard for what an earlier build wrote into an archive. For
> mail you need to keep, use the Python tool
> [mailvault](https://github.com/sniner/mailvault).

`ossuary mailvault` fetches mail over IMAP, or over MS Graph for a
Microsoft 365 mailbox. Each message is stored once, however many folders
and accounts contain it. For each folder a message was found in, a claim
records the account and the folder, so the archive can tell which mailbox
a message was in. The archive's own commands work on the messages from
there: `ossuary extract mail` reads the headers and unpacks the
attachments, `ossuary find` searches, `ossuary get` retrieves a message.

```console
$ cd /home/john/archive
$ ossuary mailvault fetch --allow-exec
archive /home/john/archive
example.org: 2 folders
example.org:INBOX: resuming above UID 1180, 24 messages to fetch
example.org:INBOX: 24 of 24 fetched
example.org:Sent: resuming above UID 310, 0 messages to fetch
24 messages stored; 96 claim(s) written, run 315e360b-020e-48be-8f2d-f2002a2ea9b4
```

`ossuary mailvault` runs this program as an
[outside verb](../ossuary-cli/README.md#outside-verbs): `ossuary` finds it
on the PATH as `ossuary-mailvault`, and it can also be called directly
under that name. It takes `--archive` and `OSSUARY_ARCHIVE` like every
ossuary command.

| | |
|---|---|
| `init` | writes a `mailvault.toml` with an example of each kind of account, all commented out. An existing `mailvault.toml` is not overwritten |
| `folders` | lists the folders of the accounts in `mailvault.toml`, one `account:folder` per line |
| `fetch` | fetches the mailboxes listed in `mailvault.toml` |
| `import` | imports an archive of the Python mailvault |

## The mailboxes

The mailboxes are configured in `mailvault.toml` in the archive root, one
`[[account]]` table per mailbox. Unknown keys are rejected, not ignored.
`ossuary mailvault init` writes a file to start from.

```toml
[[account]]
name = "example.org"                  # the account name in mailbox:place
host = "imap.example.org"
user = "john@example.org"
password_cmd = "pass show mail/example.org"
folders = ["INBOX", "Sent"]

[[account]]
name = "bridge"                       # a local bridge, plaintext IMAP on localhost
host = "127.0.0.1"
port = 1143
tls = false
user = "john@example.com"
password = "bridge-password"
```

| | |
|---|---|
| `name` | the account name used in every `mailbox:place` claim. After a rename, new claims use the new name and existing claims keep the old one. Resume points are stored by name, so the first fetch after a rename fetches every folder in full |
| `host`, `port`, `tls` | the server; port `993` with TLS by default. `tls = false` is allowed only for a bridge on localhost and is rejected for any other host |
| `user`, `password` | the login. Any key can be given as `KEY_cmd` instead, most often `password_cmd`: a command that prints the value on its first line, for example from a password manager. Commands run only with `--allow-exec`, and only when their account is fetched; their prompts and error messages appear on the terminal. If both `KEY` and `KEY_cmd` are set, the command's value is used. `name`, `backend` and `folders` cannot be given as commands |
| `folders` | the folders to fetch; all folders when omitted. Folder names differ between accounts: Gmail's `[Gmail]/All Mail` is `[Google Mail]/Alle Nachrichten` on a German account. `ossuary mailvault folders` lists the names the server uses |

A Microsoft 365 mailbox is fetched over Microsoft's MS Graph API instead of
IMAP, set with `backend = "msgraph"`. Such an account has no host and no
password: it logs in with an app registration in Azure, and `user` only
names the mailbox to read.

```toml
[[account]]
name = "m365"
backend = "msgraph"
tenant_id = "00000000-0000-0000-0000-000000000000"
client_id = "11111111-1111-1111-1111-111111111111"
client_secret_cmd = "pass show m365/client-secret"
user = "john.doe@example.com"
folders = ["Inbox", "Sent Items"]
```

The tenant and client ids can also come from a command, as
`tenant_id_cmd` and `client_id_cmd`.

| | |
|---|---|
| `tenant_id`, `client_id` | the application registered in Azure, with the `Mail.Read` application permission granted by an administrator. That permission covers every mailbox in the tenant; an administrator can restrict it to this mailbox with an application access policy |
| `client_secret` | the application's secret. Keep it in a password manager and use `client_secret_cmd` |
| `user` | the address of the mailbox to read. It is not a login |
| `folders` | as above; the folder names Outlook shows, in the mailbox's language, with a subfolder written as `Inbox/Projects` |

IMAP keys on an `msgraph` account are rejected, and `msgraph` keys on an
IMAP account.

To fetch only some accounts, name them:
`ossuary mailvault fetch example.org`.

## What is recorded

Each message is stored like a file added by `ossuary ingest`, with its
size, its type (`message/rfc822`, set by the fetch, not detected) and the
run it was fetched in. For each folder it was found in, one claim records
the place:

```
mailbox:place = "example.org:INBOX"
```

The value is the account name, a colon, and the folder name as the server
reports it, the same form `ossuary mailvault folders` prints. A message in two folders gets two such claims; a message
fetched from two accounts gets one from each. Like `file:path`, it records
where the message was when it was fetched. If an account is renamed later,
new claims use the new name and existing claims keep the old one.

```console
$ ossuary find 'mailbox:place=example.org:Sent' mail:subject
```

For a Microsoft 365 mailbox, the message's Outlook categories are recorded
as `mailbox:tag`, by the name Outlook shows. Each fetch records the
categories a message has and retracts those it no longer has, so the
standing `mailbox:tag` claims match the last fetch. A message whose
categories were changed is included in the next fetch.

```console
$ ossuary find 'mailbox:tag=Invoice' mail:subject
```

## Gmail labels

Gmail labels are not recorded. Microsoft 365 categories are, as
`mailbox:tag` (see above).

Over IMAP, Gmail shows each label as a folder. Fetch `[Gmail]/All Mail`
alone, under whatever name the account gives it: it holds every message
except Spam and Trash. Each further label folder fetches the same
messages again and records the folder as one more `mailbox:place`.

Both ways to the labels themselves are a pain for an archive:

- IMAP has them only through Gmail's extension `X-GM-LABELS`, which
  leaves out the label of the folder being read. A run asks only for
  messages above the last UID, so a label added to or removed from a
  message fetched earlier is never seen.
- The Gmail API has proper labels, but takes only OAuth 2.0: a Google
  Cloud project of your own and a consent given in the browser, which
  Google lets lapse after seven days while the project is in testing.
  The app password that IMAP runs on does not work there.

Support for Gmail labels is therefore not expected.

## Resuming

For each IMAP folder, the resume point is stored in `cache/`: the
folder's UIDVALIDITY and the highest UID fetched. The next run asks the
server only for messages above that UID. If the server changed the
UIDVALIDITY, the folder is fetched again in full: messages already in the
archive are not stored again, and their `mailbox:place` claims are
written again. Like everything in `cache/`, the resume points can be
deleted; the next fetch then fetches every folder in full, and no claim is
lost. UIDs are not recorded as claims.

For a Microsoft 365 folder, the resume point is the delta link the server
returns at the end of a fetch. The next run starts from it and gets only
what changed since; messages deleted or moved away in the meantime are
counted. Delta links expire; an expired link means one full fetch of the
folder, and the run prints how old the link was. The link is saved only
when every listed message was fetched. A message the server did not
return is reported, and the next run tries it again.

`--full` fetches every folder in full. `--dry-run` shows what a run would
fetch and writes nothing. The progress of both goes to stderr, one line
per folder; the result line goes to stdout.

If an account or a folder fails (a wrong password, a rejected login, no
token from the tenant, a folder that cannot be opened), the error is
printed and the run continues with the next one; the exit code is then
`1`.

## Importing a Python mailvault archive

An archive of the Python [mailvault](https://github.com/sniner/mailvault)
can be imported with all its messages:

```console
$ ossuary mailvault import /srv/mailvault/private
archive /home/john/archive
importing the Python mailvault archive at /srv/mailvault/private
reading the log files
131,504 messages in 3,207 log files, filed in 140,222 places
importing: 131,504 of 131,504 message(s), 131,504 stored
131,504 messages stored; 543,502 claim(s) written, run 0c1e…
```

Every message in the archive's log files is imported with every place the
log lists for it, as `mailbox:place` (the same claim a fetch writes), and
with the date the Python mailvault first recorded it there, as
`mailbox:seen`. For a fetched message, the claim's own time is when it was
seen; for an imported one, `mailbox:seen` holds the original date. Each
message is checked against its hash; a damaged file is reported and
skipped.

An import of a large archive takes long. If it is interrupted, the next
`import` continues where it stopped. The progress is kept in `cache/`, and
`--full` ignores it. A message that was recorded in a new place since the
last import is read again and imported with the new place. To import only
some mailboxes, name them after the directory:
`ossuary mailvault import /srv/mailvault/private gmail.com`.

A later fetch of the same folder, under the same account name, finds the
imported messages already stored and records the same `mailbox:place`
value.

## Exit codes

`0` when everything was fetched or imported. `1` when anything failed,
and when the archive cannot be opened, `mailvault.toml` cannot be read, or
the directory given to `import` is not a Python mailvault archive.

## License

Apache License 2.0, see [LICENSE](../LICENSE).
