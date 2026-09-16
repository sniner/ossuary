# ossuary-extract-image

*What the picture says about itself: the camera's words, and the grid.*

An [extractor](../docs/extractors.md) for ossuary: one program, two
trades. It reads an image's bytes from stdin and answers under one of
two contracts — `exif`, every EXIF field of the primary image in EXIF's
own words, or `raster`, what the file's header says about its pixel
grid. It never touches the archive, and it derives no files — it only
speaks.

## The `exif` contract

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

Left out: the thumbnail's fields, which describe the thumbnail; tags the
EXIF reader cannot name, since numbering an unknown tag would freeze a
guess into the record; and opaque blobs — `MakerNote` and its kin —
which have nothing to quote.

Kinds it reads: `image/jpeg`, `image/tiff`, `image/png`, `image/webp`,
`image/heif`, `image/heic`, `image/avif`.

## The `raster` contract

What the header says about the pixel grid, as numbers to search by:

```
raster:width = 6000
raster:height = 4000
raster:depth = 8
raster:alpha = false
raster:color = "rgb"
```

Width and height in pixels, bits per channel as stored, whether there
is an alpha channel, and the colour model — `gray`, `rgb`, `cmyk` or
`indexed`. Each format's own reader answers from the header alone, no
pixel decoded, so the answer is the file's word and not a decoder's: a
four-bit palette PNG says `4` and `indexed`, not the eight-bit RGB it
would be expanded to. A JPEG's YCbCr counts as `rgb`, which is what it
encodes. A TIFF that names no colour model — a multiband file — gets no
`raster:color`. See [the vocabulary](../docs/vocabulary.md#rasterwidth) for
the attributes.

Where EXIF has an opinion on the same matter — `exif:pixel-x-dimension`
— both stand, each under its own source. They may disagree; a picture
resized after the camera wrote its word is exactly such a case, and the
disagreement is a finding.

Kinds it reads: `image/jpeg`, `image/png`, `image/tiff`, `image/webp`.
GIF, BMP and the HEIF family wait for a header reader of their own; when
one arrives, the contract's generation is raised and every image is
looked at again.

## What it does not do

Bytes without readable EXIF, or without a header the `raster` contract
reads, are an examination like any other, with nothing found: the file
gets its receipt and is not offered again. Only failing to read stdin
is a failure. The mime lists are dispatch, not a promise: a file named
outright is examined whatever its kind, and the graceful answer to
bytes it cannot make sense of is an empty one.

## Running it

Put the binary on the PATH beside `ossuary`, then:

```console
$ ossuary extract image
$ ossuary extract image:raster
```

The first runs both contracts, the second one of them; the same
spellings hold under `[extract] run` in the archive's `config.toml`, so
a bare `ossuary extract` runs them. `--full` looks at every image
again — which is also what a raised generation of a contract causes on
its own, since the generation is part of the source every claim
carries.

It is a plain filter and stays testable by hand:

```console
$ ossuary-extract-image --identify
$ ossuary-extract-image exif < photo.jpg
$ ossuary-extract-image raster < photo.jpg
```

## License

Apache License 2.0 — see [LICENSE](../LICENSE).
