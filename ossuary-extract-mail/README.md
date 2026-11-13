# ossuary-extract-mail

Records the headers of an email message, and extracts its text, its
attachments and its nested messages as derived files.

An [extractor](../docs/extractors.md) for ossuary with three contracts.
It reads a message from stdin and prints the results of one contract.
It does not access the archive; `ossuary extract` runs it and records
the results.

## Contracts

| Contract | Source | What it does |
|---|---|---|
| `headers` | `extractor:mail-headers/1` | records the headers as `mail:` attributes |
| `attachments` | `extractor:mail-attachments/1` | writes every attachment and every nested message as a file of its own |
| `text` | `extractor:mail-text/1` | writes the text of the message as a plain text file |

Each contract has its own source and its own receipts, so each can be
run without the others. The derived files are a second copy of what the
message already contains. An archive that only keeps mail can leave out
`attachments`, or `text`, or both:

```toml
[extract]
run = ["mail:headers", "mail:text"]
```

Two things follow from leaving out a contract:

- Without `attachments`, a message that is attached to another message
  is not examined: it gets no headers and no text. The attached PDFs
  and zip files are not there for the other extractors either.
- Without `headers`, a message that `ingest` detected as `text/plain`
  is not examined by the other two contracts. They read only the types
  `message/rfc822` and `message/global`, and `headers` is the contract
  that records `message/rfc822` for such a file (see [Supported
  types](#supported-types)). Messages added by `ossuary mailvault` have
  the type from the start.

## Recorded attributes

```
file:mime = "message/rfc822"
mail:from = "Erika Muster <erika@example.org>"
mail:to = "John Doe <john@example.net>"
mail:to = "<jane@example.net>"
mail:subject = "Quarterly figures"
mail:date = "2026-03-10T13:22:05Z"
mail:message-id = "74a2f19c@mail.example.org"
```

Eleven headers are recorded: `from`, `sender`, `reply-to`, `to`, `cc`,
`bcc`, `subject`, `date`, `message-id`, `in-reply-to` and `references`.
Transport headers (`received`, `return-path`, the `x-` headers) are not
recorded.

Folded values are unfolded, and RFC 2047 encoded words are decoded. A
byte that no charset can decode is replaced with U+FFFD; the rest of the
header is kept. The values are then written in a form that can be
searched; the message itself stays in the archive with the headers as
they were.

- **Date**: in UTC, in the form `2026-03-10T13:22:05Z`, so that
  `find mail:date=2026-03-01..2026-04-01` finds the messages of March.
  A date without a time zone, or with one that cannot be read, stays
  local time and is written without a zone (`2026-03-10T14:22:05`). The
  form that asctime and date(1) write (`Fri Apr 27 19:41:26 CEST 2007`)
  is read as well.
- **Addresses** (`from`, `sender`, `reply-to`, `to`, `cc`, `bcc`): one
  value per address, with the name if there is one. The address is
  written in lower case. `find 'mail:to=*<erika@example.org>'` finds
  every message to that address, with or without a name. The members of
  a group are recorded without the group's name.
- **Message ids** (`message-id`, `in-reply-to`, `references`): one value
  per id, without the angle brackets. `find
  mail:in-reply-to=74a2f19c@mail.example.org` finds the replies to the
  message above.
- **Subject**: as written.

A value that cannot be read this way is not recorded: a date that is not
a date, or an entry of `To:` with only a name and no address. Each one is
reported with the message it is in, and the message is still examined.
An attachment's content id is recorded without angle brackets, the way
an HTML part refers to it (`cid:part2@example.com`).

## Attachments

The `attachments` contract. A part of the message is an attachment if
it has a name, or if it is a nested message. Every attachment becomes a
derived file, with `prov:origin` pointing to the message. Its type is the content type the
message declares for it, not one detected from the bytes.

The file holds the bytes that were attached. The transfer encoding is
decoded: base64, quoted-printable, and uuencode, which older mail
programs declared as `x-uuencode`. A text attachment keeps its charset.
A nested message is written exactly as it appears in the mail.

The file is written under the name given in the message, reduced to a
bare file name. An unnamed nested message is written as `message.eml`,
an unnamed attachment as `attachment`. If two names collide, a counter
is added before the extension; a name too long for the filesystem is
shortened.

The name from the message is recorded in full, whatever name the file
was written under:

- `file:name`: the last element of the name
- `file:path`: the full name with a leading `@`, which marks a location
  inside another file (`@invoice.pdf`)

`find file:path=*.pdf` therefore also finds attachments. A path with a
leading `@` is not a filesystem location: `find` returns the attachment
as long as it returns the message. An unnamed nested message gets
neither attribute, because `message.eml` is not a name from the message.
An attachment's `mail:content-id` is recorded on the attachment, not on
the message.

Body parts without a name are not extracted as files. The `text`
contract reads them.

## Text

The `text` contract writes the readable text of a message as one
derived file of type `text/plain`, with `prov:origin` pointing to the
message. `grep` and other programs can search this file, where the
message itself may be encoded as base64 or quoted-printable.

- The text comes from the body parts: parts without a name, of type
  `text/plain` or `text/html`. Attachments are not part of it, also not
  an attached text file. A nested message gets a text file of its own
  when it is examined in turn.
- Where a message has the same content as plain text and as HTML
  (`multipart/alternative`), the plain text is used. If there is none,
  or it is blank, the HTML is used.
- HTML is written without its markup. Paragraphs and line breaks stay
  line breaks; the content of `head`, `style` and `script` is left out.
- Several body parts follow each other in the order of the message.
- The file is UTF-8 with LF line breaks. Each part is read in the
  charset it declares, and a byte that cannot be read is replaced with
  U+FFFD. A part that declares no charset, or one that is not known, is
  read as UTF-8 if it is valid UTF-8, and as windows-1252 otherwise.
- Headers are not part of the text; they are recorded as attributes.
  Quoted text and signatures are kept as they are.

A message without text, or with only whitespace, gets no text file.

## Malformed mails

Where a mail can be read in more than one way, its parts are found by
these rules:

- A part ends at the next line that consists of `--` and the boundary of
  the multipart it belongs to, optionally followed by `--` and spaces.
  The boundary anywhere else, in the middle of a line or followed by
  other characters, is content.
- A damaged part affects only itself. An attached message whose own
  parts are never closed ends where the next part of the surrounding
  mail begins, and the parts after it are extracted as usual.
- Without a closing line, the last part runs to the end of the mail, or
  of the part it belongs to.
- If a part's header is not followed by an empty line, the header ends
  at the first line that is not a header field, and the content starts
  there.
- An attached message is extracted as one file. Its own attachments are
  extracted when that file is examined in turn.
- Damaged base64 or quoted-printable is decoded as far as it goes. In
  base64, characters that do not belong to it are skipped, and missing
  padding at the end loses nothing. In quoted-printable, an `=` that
  starts neither a code nor a soft line break is kept as it is.

## Supported types

`message/rfc822` and `message/global` for all three contracts, and
`text/plain` for `headers`. `message/global` is a message whose header
may contain UTF-8 (RFC 6532); Gmail, for example, returns an
undeliverable message this way. `text/plain` is included because ingest
cannot distinguish a message from other text, so `headers` examines
every text file once. A file counts as a message if it
starts with a header section of well-formed fields that contains at
least two different header names used only in mail. One is not enough,
since any text can have a line starting with `Date:`.

* **Not a message**: an empty result. The file gets its receipt and is
  not offered again.
* **A message**: `file:mime = "message/rfc822"` is recorded in addition
  to the `text/plain` detected at ingest. Both values are kept, and with
  the new type the file is examined by `attachments` and `text`.
* **An mbox file** (a `From ` separator line followed by a message):
  nothing is extracted and no headers are recorded, but
  `file:mime = "application/mbox"` is recorded.

Under `attachments` and `text`, a file that is not a message gives an
empty result as well. Only a failure to read stdin or to write an
extracted file is an error.

## Running it

Put the binary on the PATH next to `ossuary`, then:

```console
$ ossuary extract mail               # all three contracts
$ ossuary extract mail:headers       # one contract
```

Or add it to `[extract] run` in the archive's `config.toml`, as `mail`
or with single contracts. A plain `ossuary extract` runs the listed
extractors in rounds until a round examines nothing new. An attachment
extracted in one round is examined by the extractor for its type in the
next, so message, attachment and the attachment's text are all
processed in one call.

To run it by hand, give the contract name first; `attachments` and
`text` take an output directory:

```console
$ ossuary-extract-mail --identify
$ ossuary-extract-mail headers < message.eml
$ mkdir /tmp/out && ossuary-extract-mail attachments /tmp/out < message.eml
$ ossuary-extract-mail text /tmp/out < message.eml
```

## License

Apache License 2.0 (see [LICENSE](../LICENSE)).
