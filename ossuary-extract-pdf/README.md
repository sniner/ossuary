# ossuary-extract-pdf

*A document in, its text or its attachments out — two contracts in one
program.*

An [extractor](../docs/extractors.md) for ossuary, of two trades. Under
its `text` contract it reads a document's bytes from stdin, answers with
whatever the document's own information dictionary had to say, and
writes the extracted text as `text.txt` into the directory it was given.
Under its `attachments` contract it writes every file the document
carries embedded out as a file of its own: a ZUGFeRD or Factur-X
invoice's XML, a PDF/A-3 payload, whatever a writer put in. It never
touches the archive.

The text is what makes a PDF findable: taken in as a derived file of its
own, it is offered to whichever extractor reads plain text, and `ossuary
find` reaches it like any other file. An attachment joins the same
world: an XML invoice is a file of its kind, tied to its document by
`prov:origin`, and offered to whichever extractor reads that kind.

## It needs poppler

The extraction engine is the system's `pdftotext`, spoken to over pipes
the way ossuary speaks to this program:

```console
$ brew install poppler          # macOS
$ apt install poppler-utils     # Debian, Ubuntu
```

Without it this extractor refuses to identify itself — loudly, once,
instead of quietly failing on every file. The poppler version is
deliberately *not* part of the `text` contract's source name:
re-examination follows deliberate version bumps here, not the system's
update cadence. `ossuary extract pdf:text --full` is the lever for the
rare poppler leap that warrants a fresh look. Attachments need no
poppler; they are read in-process.

## What it puts on the record

The document information dictionary, verbatim under `pdf:`, the keys
kebab-cased and the values as the document spells them:

```
pdf:title = "Quarterly figures"
pdf:producer = "Example Writer 3.1"
pdf:creation-date = "D:20190714110241+02'00'"
```

A PDF date stays a PDF date; a key that does not fit the attribute
grammar is skipped rather than guessed at. Text strings are decoded from
UTF-16BE or UTF-8 behind their BOM, and from `PDFDocEncoding` otherwise —
conversion, not tidying: nothing is trimmed or normalized. A document the
info reader cannot open simply has no info to give, and the text
extraction is not asked for its opinion about that.

## What it brings out

Attachments are found where the format keeps them: in the catalog's
`EmbeddedFiles` name tree, and on pages as file attachment annotations;
a file reached both ways comes out once. Each is announced with the
kind the document declares for it — `text/xml` for an invoice — and
`application/octet-stream` when it declares none. On the record stands
what the document said about it, verbatim:

```
file:name = "factur-x.xml"
file:path = "@factur-x.xml"
pdf:desc = "Factur-X/ZUGFeRD Invoice Data"
pdf:af-relationship = "Alternative"
pdf:creation-date = "D:20260725120000Z"
pdf:mod-date = "D:20260725120000Z"
```

The name is the file specification's own, its Unicode spelling before
its byte spelling; the place is that name with a leading `@`, the way
every place inside another content is spelled, so `find
file:path=*/factur-x.xml` reaches it wherever it lay. `pdf:desc` and
`pdf:af-relationship` are the specification's, the two dates the
stream's own parameters; the size and checksum a stream may also carry
are facts of the bytes, and the archive says those itself.

An attachment that will not come out stays inside, and the reason goes
on the record as a `prov:note` finding beside a line on stderr — a
filter this program cannot decode, a stream larger than 1 GiB — so a
document that gave up its attachments incompletely does not read like
one that gave them whole. A specification that embeds nothing, pointing
at a file elsewhere, is not an attachment and is passed over silently.
A document that cannot be opened has no attachments to give: exit 0,
nothing found.

## When there is no text

Not every PDF has text to give, and that is an answer too — exit 0, no
file announced, the receipt written:

* **Scanned pages, an empty harvest.** Nothing but whitespace is nothing;
  page breaks are whitespace too.
* **A document that forbids extraction**, or one `pdftotext` cannot open
  at all. Both are the document's own deterministic answer, so it counts
  as examined.
* **A harvest that is mostly not text.** Fonts without a `ToUnicode` map
  hand `pdftotext` glyph numbers rather than characters. When more than
  half of the non-whitespace characters are unwritable — controls, the
  replacement character, the Private Use Areas, the noncharacters — the
  harvest is discarded. Where the line errs it errs toward keeping: a bad
  `text.txt` can be derived again, a silently discarded good one cannot.

Whenever there is a reason worth a sentence, the sentence goes on the
record as a `prov:note` finding and onto stderr, the same words in both
places. Only the environment failing — no `pdftotext`, a broken pipe
world — is a failure; the file is then named in the run's failures and
offered again.

## Kinds it reads

`application/pdf`.

## Running it

Put the binary on the PATH beside `ossuary`, then:

```console
$ ossuary extract pdf
```

That runs both contracts; `ossuary extract pdf:text` or `ossuary
extract pdf:attachments` runs one, and the same spelling holds under
`[extract] run` in the archive's `config.toml`. A bare `ossuary extract`
runs its list in rounds, so the `text.txt` and the attachments this
extractor hands back are offered to whichever extractor reads them in
the next round without a second call.

Testable by hand:

```console
$ ossuary-extract-pdf --identify
$ mkdir /tmp/out && ossuary-extract-pdf text /tmp/out < report.pdf
$ mkdir /tmp/att && ossuary-extract-pdf attachments /tmp/att < invoice.pdf
```

## License

Apache License 2.0 — see [LICENSE](../LICENSE).
