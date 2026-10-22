# ossuary-extract-mail

Records the headers of an email message, and extracts its attachments
and nested messages as derived files.

An [extractor](../docs/extractors.md) for ossuary. It reads a message
from stdin, prints its headers, and writes every attachment and every
nested message into the output directory it was given. It does not
access the archive; `ossuary extract` runs it and records the results.

## Recorded attributes

```
file:mime = "message/rfc822"
mail:from = "Erika Muster <erika@example.org>"
mail:to = "John Doe <john@example.net>"
mail:subject = "Quarterly figures"
mail:date = "Tue, 10 Mar 2026 14:22:05 +0100"
mail:message-id = "<74a2f19c@mail.example.org>"
```

Eleven headers are recorded: `from`, `sender`, `reply-to`, `to`, `cc`,
`bcc`, `subject`, `date`, `message-id`, `in-reply-to` and `references`.
Transport headers (`received`, `return-path`, the `x-` headers) are not
recorded.

Folded values are unfolded, and RFC 2047 encoded words are decoded.
Nothing else is changed: the date keeps its original format, and
addresses keep their display names, commas and angle brackets. A byte
that no charset can decode is replaced with U+FFFD; the rest of the
header is kept.

## Attachments

Every attachment and every nested message becomes a derived file, with
`prov:origin` pointing to the message. Its type is the content type the
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

Body parts without a name are not extracted.

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

`message/rfc822`, `message/global` and `text/plain`. `message/global`
is a message whose header may contain UTF-8 (RFC 6532); Gmail, for
example, returns an undeliverable message this way. `text/plain` is
included because ingest cannot distinguish a message from other text,
so every text file is examined once. A file counts as a message if it
starts with a header section of well-formed fields that contains at
least two different header names used only in mail. One is not enough,
since any text can have a line starting with `Date:`.

* **Not a message**: an empty result. The file gets its receipt and is
  not offered again.
* **A message**: `file:mime = "message/rfc822"` is recorded in addition
  to the `text/plain` detected at ingest. Both values are kept.
* **An mbox file** (a `From ` separator line followed by a message):
  nothing is extracted and no headers are recorded, but
  `file:mime = "application/mbox"` is recorded.

Only a failure to read stdin or to write an extracted file is an error.

## Running it

Put the binary on the PATH next to `ossuary`, then:

```console
$ ossuary extract mail
```

Or add it to `[extract] run` in the archive's `config.toml`. A plain
`ossuary extract` runs the listed extractors in rounds until a round
examines nothing new. An attachment extracted in one round is examined
by the extractor for its type in the next, so message, attachment and
the attachment's text are all processed in one call.

To run it by hand:

```console
$ ossuary-extract-mail --identify
$ mkdir /tmp/out && ossuary-extract-mail /tmp/out < message.eml
```

## License

Apache License 2.0 (see [LICENSE](../LICENSE)).
