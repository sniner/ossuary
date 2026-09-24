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

## Supported types

`message/rfc822` and `text/plain`. `text/plain` is included because
ingest cannot distinguish a message from other text, so every text
file is examined once. A file counts as a message if it starts with a
header section of well-formed fields that contains at least two
different header names used only in mail. One is not enough, since any text can
have a line starting with `Date:`.

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
