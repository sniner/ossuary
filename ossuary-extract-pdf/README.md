# ossuary-extract-pdf

Records the document information of a PDF, and extracts its text and
its embedded files as derived files.

An [extractor](../docs/extractors.md) for ossuary with two contracts. It
reads a PDF from stdin and writes into the output directory it was
given:

- `text` records the document information dictionary and writes the
  extracted text as `text.txt`.
- `attachments` writes every embedded file: the XML of a ZUGFeRD or
  Factur-X invoice, the attachments of a PDF/A-3 file, or any other
  embedded file.

It does not access the archive; `ossuary extract` runs it and records
the results. The text becomes a derived file of type `text/plain` and
is offered to any extractor that reads plain text. An attachment becomes
a derived file of its declared type, with `prov:origin` pointing to the
PDF, and is offered to any extractor that reads that type.

## Requirements

The `text` contract uses `pdftotext` from poppler:

```console
$ brew install poppler          # macOS
$ apt install poppler-utils     # Debian, Ubuntu
```

Without `pdftotext` on the PATH, `--identify` fails with a message to
install poppler, and neither contract runs. The `attachments` contract
itself does not use poppler.

The poppler version is not part of the `text` contract's source, so an
update of poppler does not cause PDFs to be examined again. To examine
all PDFs again after a poppler update, run
`ossuary extract pdf:text --full`.

## Recorded attributes (`text`)

The document information dictionary is recorded under `pdf:`, with the
keys in kebab case and the values as stored in the document:

```
pdf:title = "Quarterly figures"
pdf:producer = "Example Writer 3.1"
pdf:creation-date = "D:20190714110241+02'00'"
```

Dates keep the PDF date format. Keys that are not valid attribute names
are skipped. Text strings are decoded from UTF-16BE or UTF-8 when they
start with a byte order mark, and from `PDFDocEncoding` otherwise;
nothing is trimmed or normalized. If the document information cannot be
read, nothing is recorded for it, and the text is extracted regardless.

## When no text is extracted

In these cases no `text.txt` is written, the extractor exits with 0, and
the PDF gets its receipt:

* **No text.** Scanned pages, or text that is only whitespace (page
  breaks count as whitespace).
* **Text extraction not possible.** The document does not permit text
  extraction, or `pdftotext` cannot open it.
* **Mostly invalid characters.** Fonts without a `ToUnicode` map give
  `pdftotext` glyph numbers instead of characters. If more than half of
  the non-whitespace characters are control characters, replacement
  characters, private-use characters or noncharacters, the text is
  discarded.

In the last two cases the reason is recorded as a `prov:note` claim on
the PDF and printed on stderr:

```
prov:note = "the document does not permit text extraction"
```

If `pdftotext` cannot be run or fails for another reason, the PDF is
listed among the run's failures and offered again in the next run.

## Extracted files (`attachments`)

Embedded files are read from the document's `EmbeddedFiles` name tree
and from file attachment annotations on pages; a file referenced from
both is extracted once. Each file gets the type the document declares
for it (`text/xml` for an invoice), or `application/octet-stream` if the
document declares none. The following is recorded on the extracted
file, as written in the document:

```
file:name = "factur-x.xml"
file:path = "@factur-x.xml"
pdf:desc = "Factur-X/ZUGFeRD Invoice Data"
pdf:af-relationship = "Alternative"
pdf:creation-date = "D:20260725120000Z"
pdf:mod-date = "D:20260725120000Z"
```

`file:name` is the name from the file specification (the Unicode name
if there is one, otherwise the byte name). `file:path` is that name
with a leading `@`, which marks a location inside another file, so
`find file:path=*/factur-x.xml` finds it wherever it came from.
`pdf:desc` and `pdf:af-relationship` come from the file specification,
the two dates from the embedded file's parameters. Size and checksum
stated in the PDF are not recorded; the archive records the size
itself.

An embedded file that cannot be extracted (for example because it is
damaged, compressed with a filter this extractor does not support, or
larger than 1 GiB) is skipped. The
reason is recorded as a `prov:note` claim on the PDF and printed on
stderr, and the other files are extracted as usual. A file
specification that refers to an external file instead of embedding it
is ignored. A document that cannot be opened gives an empty result,
with exit code 0.

## Supported types

`application/pdf`.

## Running it

Put the binary on the PATH next to `ossuary`, then:

```console
$ ossuary extract pdf
```

This runs both contracts; `ossuary extract pdf:text` or
`ossuary extract pdf:attachments` runs one. The same names can be listed
under `[extract] run` in the archive's `config.toml`. A plain
`ossuary extract` runs the listed extractors in rounds, so `text.txt`
and the extracted files are examined by the extractors for their types
in the same call.

To run it by hand:

```console
$ ossuary-extract-pdf --identify
$ mkdir /tmp/out && ossuary-extract-pdf text /tmp/out < report.pdf
$ mkdir /tmp/att && ossuary-extract-pdf attachments /tmp/att < invoice.pdf
```

## License

Apache License 2.0 (see [LICENSE](../LICENSE)).
