# ossuary-mailvault

*Fetch mail into an ossuary archive.*

> **Stable, but with little use behind it so far.** It has fetched far
> fewer mailboxes, from far fewer servers, than the Python tool
> [mailvault](https://github.com/sniner/mailvault), which remains the
> safer choice for mail you need to keep.

`ossuary mailvault` fetches mail over IMAP, or over MS Graph for a
Microsoft 365 mailbox. Each message is stored once, however many folders
and accounts contain it. For each folder a message was found in, a claim
records the account and the folder, so the archive can tell which mailbox
a message was in. The archive's own commands work on the messages from
there: `ossuary extract mail` reads the headers and unpacks the
attachments, `ossuary find` searches, `ossuary get` retrieves a message.

```console
$ cd /home/john/archive
$ ossuary mailvault fetch
archive /home/john/archive
example.org: 2 folders
example.org:INBOX: resuming above UID 1180, 24 messages to fetch
example.org:INBOX: 24 of 24 fetched
example.org:Sent: resuming above UID 310, 0 messages to fetch
24 messages stored; 96 claim(s) written, run 315e360b-020e-48be-8f2d-f2002a2ea9b4
```

`ossuary mailvault` runs this program as an
[outside verb](../ossuary-cli/README.md#external-commands): `ossuary` finds it
on the PATH as `ossuary-mailvault`, and it can also be called directly
under that name. It takes `--archive` and `OSSUARY_ARCHIVE` like every
ossuary command, `--cache` and `OSSUARY_CACHE` for caches kept outside
the archive, and `--accounts FILE` to read the accounts from a file
outside the archive (see [Who may write the file](#who-may-write-the-file)).

| | |
|---|---|
| `init` | writes a `mailvault.toml` with an example of each kind of account, all commented out. An existing `mailvault.toml` is not overwritten. With `--accounts FILE`, the file is written there |
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
| `user`, `password` | the login. Almost any key can be given as `KEY_cmd` instead, most often `password_cmd`: a command that prints the value on its first line, for example from a password manager. A command runs only when its account is fetched; its prompts and error messages appear on the terminal. If both `KEY` and `KEY_cmd` are set, the command's value is used. `name`, `backend`, `port`, `tls`, `folders` and `skip` cannot be given as commands |
| `folders` | the folders to fetch; all folders when omitted. Folder names differ between accounts: Gmail's `[Gmail]/All Mail` is `[Google Mail]/Alle Nachrichten` on a German account. `ossuary mailvault folders` lists the names the server uses |
| `skip` | the folders not to fetch when `folders` is omitted, such as `skip = ["Junk"]` for the spam folder of iCloud Mail. A message that is fetched stays in the archive, so leave out the folders whose messages you do not want archived. The names are compared exactly, as `ossuary mailvault folders` lists them; a name the mailbox does not have is an error, and that account is not fetched. `folders` and `skip` cannot both be given |

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
| `folders`, `skip` | as above; the folder names Outlook shows, in the mailbox's language, with a subfolder written as `Inbox/Projects` |

IMAP keys on an `msgraph` account are rejected, and `msgraph` keys on an
IMAP account.

To fetch only some accounts, name them:
`ossuary mailvault fetch example.org`.

## Who may write the file

A `KEY_cmd` is a shell command. It runs as the user who runs
`ossuary mailvault`, with everything that user can reach. Whoever can
write `mailvault.toml` can therefore run any command as that user, every
time the mail is fetched. No command is even needed: `host` can be changed
to a server the writer controls, and the password goes there, whether
written into the file or printed by `password_cmd`.

None of this needs a hostile person. An archive on a share that several
accounts can write to, or a sync client that overwrites files, has the
same effect.

The protection is the file's location. The file is as safe as the
directory it lies in, so that has to be a directory nobody else can write
to. When the archive root is not such a directory, keep the file
elsewhere and name it on every call:

```console
$ ossuary mailvault init --accounts ~/.config/ossuary/mailvault.toml
$ ossuary mailvault fetch --accounts ~/.config/ossuary/mailvault.toml
```

`init` creates the file readable and writable for its owner only (mode
0600), so a password written into it cannot be read by other users of the
machine. That does not replace the directory: whoever can write the
directory can replace the file.

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
categories were changed is included in the next fetch. Labels of Gmail and
Proton Mail are recorded as `mailbox:tag` too (see [Gmail labels](#gmail-labels)
and [Proton Mail labels](#proton-mail-labels)).

```console
$ ossuary find 'mailbox:tag=Invoice' mail:subject
```

## Gmail labels

Over IMAP, Gmail shows each label as a folder. Fetch `[Gmail]/All Mail`
alone, under whatever name the account gives it: it holds every message
except Spam and Trash. Each further label folder fetches the same
messages again and records the folder as one more `mailbox:place`.

A fetch from Gmail records the labels of each message it fetches as
`mailbox:tag`, by the name Gmail shows, such as `Work`. Gmail's own labels
begin with a backslash over IMAP (`\Inbox`, `\Important`); the tag begins
with a slash instead (`/Inbox`, `/Important`), which needs no escaping in
the shell or in a pattern. The label of the folder being read is not among
them, because that folder is recorded as the place: a message fetched from
INBOX gets no `/Inbox` tag. A message fetched from several folders gets the
tags each of them lists: fetched from INBOX and from `Work`, it has `Work`
and `/Inbox`.

```console
$ ossuary find 'mailbox:tag=Work' mail:subject
```

The labels are read when a message is fetched. A fetch asks only for the
messages that arrived since the last run, so a label added to or removed
from a message fetched earlier is not seen until the message is fetched
again, for example with `--full`. That fetch records the labels the message
has then and retracts the tags it no longer has. The tag of the folder's
own label is kept, since Gmail never lists it there. A fetch from Sent,
Starred, Drafts, Important, Spam or Trash retracts nothing: Gmail does not
tell which of its own labels such a folder stands for, so any tag missing
from the list may be that one. Fetch All Mail to have removed labels
retracted.

The Gmail API is not used. It takes only OAuth 2.0: a Google Cloud project
of your own and a consent given in the browser, which Google lets lapse
after seven days while the project is in testing. The app password that
IMAP runs on does not work there.

## Proton Mail labels

Proton Mail Bridge shows each label as a folder under `Labels/`, and `All
Mail` holds every message. Fetch `All Mail` alone.

After the folders in the account's `folders` list, a fetch from Proton Mail
Bridge reads each folder under `Labels/` that is not in the list, and
records the label's name as `mailbox:tag` on every message in it: `Sales`
for `Labels/Sales`. Nothing is downloaded from a label folder, and it is not
recorded as a place. The fetch finds each of its messages in the archive by
the message's Proton id (see below) and tags it there. A message that is in
a label folder but not yet in the archive, because it arrived after `All
Mail` was read, is tagged by the next run. If a folder of the account could
not be read, the label folders are left for the next run.

Each label folder has its own resume point, like any other folder: a run
asks only about the messages added to the label since the last run, and
about those it did not find in the archive then. A label removed from a
message is not seen then. `--full` lists every label folder whole and
retracts the tag from a message that a configured folder still holds but
the label's folder no longer does, as `--full` does for Gmail. A message
no longer in the mailbox keeps its tags.

Without a `folders` list every folder is fetched, label folders included,
and each is recorded as a `mailbox:place`.

### The Proton id

Proton Mail Bridge builds each message when it is fetched. After the Bridge
has rebuilt its local data, which also renumbers its folders, the same
message comes as other bytes: some headers and the MIME boundaries differ,
and the archive would store it a second time. For a Proton Mail account,
ossuary therefore records the id Proton gives each message, the header
`X-Pm-Internal-Id`, as `mailbox:id` (`proton:` and the id). A message whose
id is in the archive is not downloaded again, after a renumbering, with
`--full` or with `--compare`; only its place is recorded if it is missing.
With `--compare` the folder is listed like with `--full` and compared by
Proton id, so `ossuary extract mail` is not needed for it. An import of an
archive of the Python tool mailvault records the id of every message that
carries the header.

## Resuming

For each IMAP folder, the resume point is stored in `cache/`: the
folder's UIDVALIDITY and the highest UID fetched. The next run asks the
server only for messages above that UID. If the server changed the
UIDVALIDITY, the folder is fetched again in full: messages already in the
archive are not stored again, and their `mailbox:place` claims are
written again. From Proton Mail Bridge only the messages whose Proton id
is not in the archive are downloaded (see [The Proton id](#the-proton-id)).
Like everything in `cache/`, the resume points can be deleted; the next
fetch then fetches every folder in full, and no claim is lost. UIDs are
not recorded as claims.

A message the IMAP server will not hand over, because it refuses the
fetch or sends nothing for a message the folder holds, goes on the
folder's list of messages still to fetch, and the resume point moves past
it. The next run reports how many are pending and fetches those first. A
message that is no longer in the folder is taken off the list. As for
Microsoft 365 below, a message refused in three runs is given up and
named in the report, `--full` fetches it again, and ten refusals in a row
leave the rest of the folder for the next run. A UIDVALIDITY that changed
empties the list along with the resume point.

For a Microsoft 365 folder, the resume point is the delta link the server
returns at the end of a listing. The next run starts from it and gets only
what changed since; messages deleted or moved away in the meantime are
counted. Delta links expire; an expired link means one full fetch of the
folder, and the run prints how old the link was. A run first lists the
folder, then fetches the listed messages one by one. The link and the
list are saved in `cache/` as soon as the listing is complete, and each
message is taken off the list when it is stored. A run that is
interrupted, and a message the server did not return, leave messages on
the list; the next run reports how many are pending and fetches those
before it asks the server what changed. A message that is no longer in
the folder when its turn comes is skipped. A message the server refuses
in three runs is given up and named in the report; `--full` lists it
again. When the server refuses ten messages in a row, or stops answering
at all, the rest of the folder is left for the next run.

`--full` fetches every folder in full, and `--compare` fetches what is
missing (see the next section). `--dry-run` shows what a run would fetch
and writes nothing. The progress goes to stderr, one line per folder; the
result line goes to stdout.

If an account or a folder fails (a wrong password, a rejected login, no
token from the tenant, a folder that cannot be opened), the error is
printed and the run continues with the next one; the exit code is then
`1`.

## Fetching what is missing

A normal fetch asks each folder only for what arrived since the last run.
A message that an earlier run missed, or one that was given up after three
refusals, is not fetched again that way. Two options look at every message
of every folder instead:

- `--full` downloads every message. Messages already in the archive are
  not stored again, but each one costs its download, and its claims (the
  folder, the type) are written again. It depends on nothing else.
- `--compare` lists every message with its Message-ID and downloads only
  those the archive does not hold in that folder under the same
  Message-ID.

For a Proton Mail account both download only the messages whose Proton id
is not in the archive, and the rest of this section does not apply to it
(see [The Proton id](#the-proton-id)).

```console
$ ossuary extract mail
$ ossuary mailvault fetch --compare
archive /home/john/archive
example.org: 2 folders
example.org:INBOX: compared with the archive, 2 of 8010 messages to fetch
example.org:INBOX: 2 of 2 fetched
example.org:Sent: compared with the archive, 0 of 1312 messages to fetch
2 messages stored, 9320 found in the archive by Message-ID; 6 claim(s) written, run 8b0c…
```

**Run `ossuary extract mail` before `--compare`.** The Message-IDs of the
archived messages are on the record only once the mail extractor has read
them. A message without a Message-ID on record cannot be matched, so its
copy on the server is downloaded again. Nothing is stored twice, but the
download is spent and the message's claims are written again. If the
extractor has never run, that is every message, and `--compare` costs as
much as `--full`. Before it fetches anything, `--compare` prints for each
folder how many archived messages have no Message-ID on record:

```
example.org:INBOX: 312 of 8008 archived messages have no Message-ID on record, and their copies on the server are fetched again; run `ossuary extract mail` first if it has not run since the last fetch
```

Where the Message-ID cannot show whether a message is archived,
`--compare` downloads the message, and the archive decides by its content:

- The server lists a Message-ID more often than the archive holds it in
  that folder. Two different messages can share a Message-ID, and a folder
  can hold the same message twice; every message under that Message-ID is
  downloaded.
- Microsoft 365 reports a Message-ID that differs from the one in the
  message for some messages, common in Sent Items, and one for messages
  that have none.

`--compare` records in `cache/` which archived message each of these
downloads turned out to be, and a later `--compare` does not download it
again. The same holds for a message without a Message-ID on record. If
`cache/` is deleted, or the server renumbers the folder, such messages are
downloaded once more.

`--compare --dry-run` lists the folders and downloads nothing, so it
shows how many messages each folder is missing. It ends with exit code `0`
either way; the result line says how many would be fetched. That number
includes the messages `--compare` downloads only to compare them, so it
can be higher than what is really missing.

After `--compare`, a folder's resume point is set to its newest message,
and the next normal fetch continues from there. The messages to fetch go
on the folder's list of messages still to fetch first, so a run that is
interrupted continues with them.

## Importing a Python mailvault archive

An archive of the Python [mailvault](https://github.com/sniner/mailvault)
can be imported with all its messages:

```console
$ ossuary mailvault import /srv/mailvault/private
archive /home/john/archive
importing the Python mailvault archive at /srv/mailvault/private
reading the log files
131,504 messages in 3,207 log files, filed in 140,222 places
gmail.com: 41 folders not in the account's folders list, imported as labels
importing: 131,504 of 131,504 message(s), 131,504 stored
131,504 messages stored; 543,502 claim(s) written, run 0c1e…
```

Every message in the archive's log files is imported with every place the
log lists for it. The import reads `mailvault.toml`, or the file named with
`--accounts`, and needs an account there for each mailbox it imports,
under the same name:

- A folder in the account's `folders` list is recorded as `mailbox:place`
  under the name written in the list, the same value a fetch of that folder
  records. Two names count as the same folder when they differ only in the
  Unicode form of a character (`ö` as one character, or as `o` followed by
  a combining diaeresis), and `INBOX` matches in any case.
- The Python mailvault recorded each label of Gmail and Proton Mail as one
  more folder. For an account at Gmail (host `imap.gmail.com`, Google
  Workspace included), a folder that is not in the `folders` list is a
  label and is recorded as `mailbox:tag`, as a fetch from Gmail records it:
  `\Sent` as `/Sent`. A later fetch of the message retracts the tags it no
  longer has.
- For any other IMAP account, a folder under `Labels/` that is not in the
  `folders` list is a Proton Mail label and is recorded as `mailbox:tag`,
  as a fetch from Proton Mail Bridge records it: `Sales` for
  `Labels/Sales`. The import does not connect to the server, so it does
  not check that the account is at Proton Mail.
- A message whose places in the log are all labels keeps its label folders
  as places too, as a fetch does for a message that no configured folder
  holds. Without a place, `ossuary find` would show it only with `--all`.
- Every other place is recorded as `mailbox:place` as the log names it. An
  account without a `folders` list fetches every folder, so none of its
  folders is a label.

Each claim gets the time at which the Python mailvault first recorded the
message at that place, as a fetch's claim gets the time of the fetch, so
`--as-of` shows the mailboxes as the Python mailvault knew them then. Each
message is checked against its hash; a damaged file is reported and
skipped.

`--no-auto-labels` records every place as `mailbox:place`, as the log names
it, and does not read the accounts. Use it for a mailbox that is no longer
fetched and has no account: import it by name with `--no-auto-labels`, and
the other mailboxes by name without it.

An import of a large archive takes long. If it is interrupted, the next
`import` continues where it stopped. The progress is kept in `cache/`, and
`--full` ignores it. A message that was recorded in a new place since the
last import is read again and imported with the new place. To import only
some mailboxes, name them after the directory:
`ossuary mailvault import /srv/mailvault/private gmail.com`.

A later fetch of the same folder, under the same account name, finds the
imported messages already stored and records the same `mailbox:place`
value. The import sets no resume points, so the first fetch downloads
every message of every folder. To download only what the import did not
bring in, run `ossuary extract mail` and then `ossuary mailvault fetch
--compare` (see [Fetching what is missing](#fetching-what-is-missing)).

If the archive is to live on a share, run the import, `ossuary extract
mail` and the first fetch on a local disk, and copy the archive to the
share afterwards (see [An archive on a network
share](../docs/deep-dive.md#an-archive-on-a-network-share)). Each of
these runs records many claims, and every append to an archive on a
share costs a round trip to the server.

## Exit codes

`0` when everything was fetched or imported. `1` when anything failed,
and when the archive cannot be opened, the accounts cannot be read, or
the directory given to `import` is not a Python mailvault archive.

## License

Apache License 2.0, see [LICENSE](../LICENSE).
