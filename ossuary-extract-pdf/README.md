# ossuary-extract-pdf

*PDFs in, plain text out — as a derived file.*

An [extractor](../docs/extractors.md) for ossuary. It reads a document's
bytes from stdin, answers with whatever the document's own information
dictionary had to say, and writes the extracted text as `text.txt` into
the directory it was given. It never touches the archive.

The text is what makes a PDF findable: taken in as a derived file of its
own, it is offered to whichever extractor reads plain text, and `ossuary
find` reaches it like any other file.

## It needs poppler

The extraction engine is the system's `pdftotext`, spoken to over pipes
the way ossuary speaks to this program:

```console
$ brew install poppler          # macOS
$ apt install poppler-utils     # Debian, Ubuntu
```

Without it this extractor refuses to identify itself — loudly, once,
instead of quietly failing on every file. The poppler version is
deliberately *not* part of this extractor's source name: re-examination
follows deliberate version bumps here, not the system's update cadence.
`ossuary extract pdf --full` is the lever for the rare poppler leap that
warrants a fresh look.

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

Or list it under `[extract] run` in the archive's `config.toml`. A bare
`ossuary extract` runs its list in rounds, so the `text.txt` this
extractor hands back is offered to the text-reading extractors in the
next round without a second call.

Testable by hand:

```console
$ ossuary-extract-pdf --identify
$ mkdir /tmp/out && ossuary-extract-pdf /tmp/out < report.pdf
```

## License

Apache License 2.0 — see [LICENSE](../LICENSE).
