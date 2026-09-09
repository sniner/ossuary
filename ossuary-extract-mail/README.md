# ossuary-extract-mail

*An internet message in, its own voice out — and what it carries handed
over as files of their own.*

An [extractor](../docs/extractors.md) for ossuary. It reads a message's
bytes from stdin, answers with the headers the message speaks about
itself, and writes every attachment and every nested message into the
directory it was given, each announced as content of its own. It never
touches the archive.

## What it puts on the record

```
file:mime = "message/rfc822"
mail:from = "Erika Muster <erika@example.org>"
mail:to = "John Doe <john@example.net>"
mail:subject = "Quarterly figures"
mail:date = "Tue, 10 Mar 2026 14:22:05 +0100"
mail:message-id = "<74a2f19c@mail.example.org>"
```

Eleven headers are spoken: `from`, `sender`, `reply-to`, `to`, `cc`,
`bcc`, `subject`, `date`, `message-id`, `in-reply-to`, `references` —
the message's own voice, and the message-id family that threads it. The
transport's trail — `received`, `return-path`, the `x-` families — is the
journey, not the message, and stays untold.

Values are unfolded and their RFC 2047 encoded words decoded — that is
conversion, not tidying. Everything else stands as the mail spells it:
the date keeps its own calendar, addresses keep their display names,
commas and angle brackets. A raw byte no charset accounts for reads as
U+FFFD rather than silencing its whole header.

## What it hands over

Every attachment and every nested message becomes a derived file, taken
into the archive with `derive:derived-from` naming the mail it came out
of. Announced with the kind the mail itself declared — not a guess from
the bytes — and under the name the mail spelled, flattened to a bare file
name. A forwarded message nobody named gets `message.eml`, a nameless
attachment `attachment`. Where two names collide a counter slips in
before the extension, and the name the mail spelled goes on the record as
`file:name` beside it. An attachment's `mail:content-id` is recorded on
the attachment, not on the mail.

Body parts without a name stay inside: they are the mail speaking, not
the mail carrying.

## Kinds it reads, and the gate behind them

`message/rfc822` and — deliberately — `text/plain`. Ingest's sniff cannot
tell a mail from any other text, so every text file passes through here
once. What decides is the bytes themselves: a header section of
well-formed fields from byte zero, holding at least two distinct names
only mail uses. One is not enough — any prose may mention `Date:` at the
start of a line.

* **Not a message** → an examination with nothing found. Exit 0, no
  output; the receipt keeps those bytes from being offered again.
* **A message** → `message/rfc822` goes on the record beside the sniffed
  `text/plain`. Both stand: the record keeps every word, and choosing
  between them is the reader's business.
* **An mbox** — a `From ` separator line with a message behind it — is a
  mailbox, not a message. Nothing is unpacked and no header is spoken,
  but the recognized kind goes on the record as `application/mbox`, so a
  future mailbox reader finds its work waiting.

Only failing to read stdin or to write a carried file is a failure.

## Running it

Put the binary on the PATH beside `ossuary`, then:

```console
$ ossuary extract mail
```

Or list it under `[extract] run` in the archive's `config.toml`. A bare
`ossuary extract` runs its list in rounds, which is what a mail wants:
the attachment this extractor hands back is offered to whichever
extractor reads its kind in the next round, so mail → attachment → text
settles in one call.

Testable by hand:

```console
$ ossuary-extract-mail --identify
$ mkdir /tmp/out && ossuary-extract-mail /tmp/out < message.eml
```

## License

Apache License 2.0 — see [LICENSE](../LICENSE).
