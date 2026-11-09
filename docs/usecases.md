# Use cases

The [README](../README.md) shows what each command does. This page
shows how the commands and ordinary tools work together for a task,
one task per section.

## Finding a document by a phrase in it

**The situation.** You remember a phrase from a document, say the
address on an invoice, but not the file name or where the file was. It
may have arrived as a mail attachment.

**What it needs.** An uncompressed archive (the default, see [Creating
an archive](../README.md#creating-an-archive)) and the mail and PDF
extractors run over it: `ossuary extract mail` and `ossuary extract
pdf`, or both listed under `[extract] run` in `config.toml`. The mail
extractor stores each attachment as a file of its own, and the PDF
extractor stores the text of each PDF as a plain text file. The
examples use [ripgrep](https://github.com/BurntSushi/ripgrep) (`rg`);
`grep -rl` works the same way.

**1. Search the stored files.** The files are in `content/` and
`derived/`; `claims/` and `cache/` hold the record and the query index.

```console
$ rg -l "Elm Street" ~/archive/content ~/archive/derived
/home/john/archive/derived/6b/32/6b320d682c6be758c9e345bf1086be76da702924022858c57012a14b428ce4ec
```

The file name of a hit is the file's name in the archive. A hit in
`derived/` is a file that an extractor produced from another one.

**2. Find where it came from.** The field `subject` selects one file by
its full name, and `--with-origin` shows the files it was derived from:

```console
$ ossuary find subject=6b320d682c6be758c9e345bf1086be76da702924022858c57012a14b428ce4ec --with-origin file:name
9a9274ef
│   file:mime=message/rfc822
│   file:mime=text/plain
│   file:name=2026-03-17-invoice.eml
└── fb1c6c49
    │   file:mime=application/pdf
    │   file:name=invoice-2026-0412.pdf
    └── 6b320d68
1 file(s), 2 origin(s)
```

The hit is the text of the PDF `invoice-2026-0412.pdf`, which was
attached to the mail `2026-03-17-invoice.eml`.

**3. Get the original.** `get` writes the PDF to a file, and `about`
shows where the mail was when it was added:

```console
$ ossuary get fb1c6c49 --output invoice.pdf
invoice.pdf: 28416 byte(s) of fb1c6c499cd6d943a903ce9b2e618dbbd24ccfc435ee07bcf8fce4966ffdaf3b
$ ossuary about 9a9274ef file:path
2026-11-09T11:11:31Z  file:path = "/home/john/mail/2026-03-17-invoice.eml"  [ingest] [run:38151950-b95c-4c30-bc63-d698520e1c02]
```

For a program that reads the file in the archive directly, `ossuary get
--path fb1c6c49` prints its path.

**Several hits.** Names in the archive contain no spaces, so a shell
loop is enough:

```console
$ for name in $(rg -l "Elm Street" ~/archive/content ~/archive/derived | xargs -n1 basename); do
>   ossuary find subject=$name --with-origin file:name
> done
```

**Limits.**

- The body of a mail is often encoded (base64 or quoted-printable), and
  then the search does not find its text. A mail's headers are recorded
  as claims: `ossuary find 'mail:subject=*roof*'` searches the subjects.
- A scanned PDF without a text layer has no text to extract.
- In a compressed archive, `rg -z` searches the compressed files; it
  needs `zstd`. The name of a hit then ends in `.zst`, and the file's
  name is the part before it.
