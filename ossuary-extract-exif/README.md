# ossuary-extract-exif

*What a camera wrote into the picture, verbatim.*

An [extractor](../docs/extractors.md) for ossuary: one program, one
trade. It reads an image's bytes from stdin and answers with every EXIF
field of the primary image, in EXIF's own words. It never touches the
archive, and it derives no files — it only speaks.

## What it puts on the record

Every field of the primary image, the tag name kebab-cased under `exif:`
and the value as the format stores it:

```
exif:make = "Example Cameras Inc."
exif:model = "EX-1"
exif:date-time-original = "2019:07:14 11:02:41"
exif:f-number = "28/10"
exif:iso-speed = 200
```

Nothing is normalized. The date keeps EXIF's own colons, a rational
stays the fraction it is (`28/10`, not `2.8`), text stays text, one value
comes bare and several as a list. Turning `2019:07:14` into a date and
`28/10` into f/2.8 is query-time business — the record holds what the
camera said. See [the vocabulary](../docs/vocabulary.md#exif) for how
these values are meant to be read.

## What it leaves out

* **The thumbnail's fields.** They describe the thumbnail, not the
  picture.
* **Tags the EXIF reader cannot name.** An attribute has to be a word;
  numbering an unknown tag would freeze a guess into the record.
* **Opaque blobs** — `MakerNote` and its kin. There is nothing to quote.

Bytes without readable EXIF are an examination like any other, with
nothing found: the file gets its receipt and is not offered again. Only
failing to read stdin is a failure.

## Kinds it reads

`image/jpeg`, `image/tiff`, `image/png`, `image/webp`, `image/heif`,
`image/heic`, `image/avif` — as `file:mime` spells them. The list is
dispatch, not a promise: a file named outright is examined whatever its
kind, and the graceful answer to bytes it cannot make sense of is an
empty one.

## Running it

Put the binary on the PATH beside `ossuary`, then:

```console
$ ossuary extract exif
```

Or list it under `[extract] run` in the archive's `config.toml` so a bare
`ossuary extract` runs it. `--full` looks at every image again — which is
also what a new version of this extractor causes on its own, since the
version is part of the source name every claim carries.

It is a plain filter and stays testable by hand:

```console
$ ossuary-extract-exif --identify
$ ossuary-extract-exif < photo.jpg
```

## License

Apache License 2.0 — see [LICENSE](../LICENSE).
