# ossuary-extract-image

*What the picture says about itself: the camera's words, the editor's
words, the press desk's words, and the grid.*

An [extractor](../docs/extractors.md) for ossuary: one program, four
trades. It reads an image's bytes from stdin and answers under one of
four contracts. `exif` is every EXIF field of the primary image in
EXIF's own words; `xmp` every property of the XMP packet in XMP's;
`iptc` every dataset of the IPTC-IIM record in IPTC's; `raster` what the
file's header says about its pixel grid. It never touches the archive,
and it derives no files; it only speaks.

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
`28/10` into f/2.8 is query-time business; the record holds what the
camera said. See [the vocabulary](../docs/vocabulary.md#exif) for how
these values are meant to be read.

Left out: the thumbnail's fields, which describe the thumbnail; tags the
EXIF reader cannot name, since numbering an unknown tag would freeze a
guess into the record; opaque blobs, `MakerNote` and its kin, which
have nothing to quote; and a TIFF's own layout, `StripOffsets`,
`StripByteCounts`, `RowsPerStrip` and the tile equivalents, which say
where bytes lie and nothing about the picture.

Kinds it reads: `image/jpeg`, `image/tiff`, `image/png`, `image/webp`,
`image/heif`, `image/heic`, `image/avif`.

## The `xmp` contract

Every property of the XMP packet, the schema's prefix and the property's
name kebab-cased and joined under `xmp:`, the value as the packet spells
it:

```
xmp:dc-subject = ["alps", "summer"]
xmp:dc-title = "Die Alpen"
xmp:dc-creator = "Someone"
xmp:xmp-rating = "5"
xmp:xmp-create-date = "2019-07-14T11:02:41+02:00"
xmp:photoshop-city = "Wien"
xmp:xmp-mm-document-id = "xmp.did:0f3c2a1e-6b7d-4e58-9a10-5c2d8e7f4b21"
```

XMP is RDF, and RDF nests; the contract flattens it. A list (`rdf:Bag`,
`rdf:Seq`) stands with every item as a value of the property. Of
language alternatives (`rdf:Alt`) the default language (`x-default`)
stands, the first where none is marked. A structure's fields join the
path with a dash, so the city of `Iptc4xmpExt:LocationCreated` stands as
`xmp:iptc4xmp-ext-location-created-iptc4xmp-ext-city`; a list of
structures, the edit history say, piles each field's values on the same
attribute, in order. Values stay the packet's text: `"5"` is text, a
date keeps its spelling.

The prefix is the one the XMP specification gives the namespace, not
the one the file declared: a writer that spells Dublin Core `dcterms`
still lands on `xmp:dc-…`, since the namespace URI is what the two have
in common. A namespace this program does not know keeps the prefix the
packet declared for it.

Kinds it reads: `image/jpeg` (the extended packet of a large one
included), `image/tiff`, `image/png`, `image/webp`, `image/heif`,
`image/heic`, `image/avif`.

## The `iptc` contract

Every dataset of the IPTC-IIM application record, the press vocabulary
that predates XMP and still rides along in JPEGs and TIFFs, the
dataset's name kebab-cased under `iptc:`:

```
iptc:object-name = "Squirrel"
iptc:keywords = ["wildlife", "park"]
iptc:by-line = "Someone"
iptc:city = "Kiel"
iptc:date-created = "20060521"
iptc:copyright-notice = "(c) 2006 by Someone"
```

A dataset the record says several times, as it says keywords, stands
with every value. The text is read as UTF-8 where the record declares it
or the bytes hold as such, as Latin-1 otherwise, which is what writers
of the undeclared kind used. Datasets this program cannot name, and the
binary ones, are left out.

Kinds it reads: `image/jpeg`, `image/tiff`.

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
is an alpha channel, and the colour model: `gray`, `rgb`, `cmyk` or
`indexed`. Each format's own reader answers from the header alone, no
pixel decoded, so the answer is the file's word and not a decoder's: a
four-bit palette PNG says `4` and `indexed`, not the eight-bit RGB it
would be expanded to. A JPEG's YCbCr counts as `rgb`, which is what it
encodes. A TIFF that names no colour model, a multiband file, gets no
`raster:color`. A HEIC or AVIF answers from its `meta` box, the
codestream unopened: the primary picture's size, its bits per channel,
an auxiliary alpha plane tied to it, and a grid of tiles as one
picture. See [the vocabulary](../docs/vocabulary.md#rasterwidth) for
the attributes.

Where EXIF has an opinion on the same matter, `exif:pixel-x-dimension`,
both stand, each under its own source. They may disagree; a picture
resized after the camera wrote its word is exactly such a case, and the
disagreement is a finding.

Kinds it reads: `image/jpeg`, `image/png`, `image/tiff`, `image/webp`,
`image/heif`, `image/heic`, `image/avif`. GIF and BMP wait for a header
reader of their own; when one arrives, the contract's generation is
raised and every image is looked at again.

## What it does not do

Bytes without readable EXIF, XMP or IPTC, or without a header the
`raster` contract reads, are an examination like any other, with
nothing found: the file gets its receipt and is not offered again. Only
failing to read stdin is a failure. The mime lists are dispatch, not a
promise: a file named outright is examined whatever its kind, and the
graceful answer to bytes it cannot make sense of is an empty one.

## Running it

Put the binary on the PATH beside `ossuary`, then:

```console
$ ossuary extract image
$ ossuary extract image:xmp
```

The first runs all four contracts, the second one of them; the same
spellings hold under `[extract] run` in the archive's `config.toml`, so
a bare `ossuary extract` runs them. `--full` looks at every image
again, which is also what a raised generation of a contract causes on
its own, since the generation is part of the source every claim
carries.

It is a plain filter and stays testable by hand:

```console
$ ossuary-extract-image --identify
$ ossuary-extract-image exif < photo.jpg
$ ossuary-extract-image xmp < photo.jpg
$ ossuary-extract-image iptc < photo.jpg
$ ossuary-extract-image raster < photo.heic
```

## License

Apache License 2.0 (see [LICENSE](../LICENSE)).
