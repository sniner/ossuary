# ossuary-extract-image

Records the EXIF, XMP and IPTC metadata of an image, the position where
it was taken, and its dimensions and pixel format as stated in the file
header.

An [extractor](../docs/extractors.md) for ossuary with four contracts. It
reads an image from stdin and prints the results of one contract:

| Contract | Records |
|---|---|
| `exif` | every EXIF field of the primary image, and the GPS position in decimal degrees |
| `xmp` | every property of the XMP packet |
| `iptc` | every dataset of the IPTC-IIM record |
| `raster` | width, height, bit depth, alpha channel and colour model |

Each contract has its own source and its own receipts. The extractor does
not access the archive and derives no files; `ossuary extract` runs it
and records the results.

## The `exif` contract

Records every field of the primary image under `exif:`, with the tag
name in kebab case and the value as stored in the file:

```
exif:make = "Example Cameras Inc."
exif:model = "EX-1"
exif:date-time-original = "2019:07:14 11:02:41"
exif:f-number = "28/10"
exif:iso-speed = 200
```

Values are not normalized. A date keeps the EXIF colons, a rational is
written as a fraction (`28/10`, not `2.8`), and text stays text. A field
with one value is recorded as that value, a field with several as a
list. Conversions such as `28/10` to f/2.8 happen at query time; see
[the vocabulary](../docs/vocabulary.md#exif).

Not recorded:

- the thumbnail's fields
- tags the EXIF reader has no name for
- opaque binary fields such as `MakerNote`
- TIFF layout fields: `StripOffsets`, `StripByteCounts`, `RowsPerStrip`
  and the tile equivalents

Supported types: `image/jpeg`, `image/tiff`, `image/png`, `image/webp`,
`image/heif`, `image/heic`, `image/avif`.

### The position

Beside the fields, the `exif` contract records where the image was
taken as `geo:position`, computed from the GPS fields:

```
geo:position = {"altitude": 519.5, "latitude": 48.13715277777778, "longitude": 11.576122222222223}
```

Latitude and longitude are in decimal degrees on WGS 84, negative to the
south and west. The altitude is in metres, negative below the reference,
and is left out when the file has none. The EXIF fields stay under
`exif:` as they are (`exif:gps-latitude = ["48/1", "8/1", "1375/100"]`).

A position is found by its parts, with ranges:

```console
$ ossuary find geo:position.latitude=48.1..48.2 geo:position.longitude=11.5..11.6
$ ossuary find geo:position.altitude=2000..
```

The first command finds the images taken in a rectangle around Munich,
the second those taken at 2000 m or higher.

No position is recorded when a field for latitude or longitude is
missing or cannot be read, when the GPS receiver marked the measurement
void (`exif:gps-status` is `V`), or when the map datum is not WGS 84.
A position whose altitude cannot be read is recorded without it. Each
case is reported on stderr, for example:

```
ossuary-extract-image: geo:position not recorded, exif:gps-map-datum is not WGS 84: TOKYO
```

EXIF does not reliably state whether an altitude is measured from sea
level or from the WGS 84 ellipsoid. The two differ by up to about 100 m.
See [the vocabulary](../docs/vocabulary.md#geoposition).

## The `xmp` contract

Records every property of the XMP packet under `xmp:`, named from the
schema prefix and the property name in kebab case, with the value as
written in the packet:

```
xmp:dc-subject = "alps"
xmp:dc-subject = "summer"
xmp:dc-title = "Die Alpen"
xmp:dc-creator = ["Someone", "Someone Else"]
xmp:xmp-rating = "5"
xmp:xmp-create-date = "2019-07-14T11:02:41+02:00"
xmp:photoshop-city = "Wien"
xmp:xmp-mm-document-id = "xmp.did:0f3c2a1e-6b7d-4e58-9a10-5c2d8e7f4b21"
```

Nested RDF structures are flattened:

- Each item of an unordered list (`rdf:Bag`) is a value of its own:
  `xmp:dc-subject` has one value per keyword, and
  `find xmp:dc-subject=alps` finds every image with that keyword. An
  item given twice is recorded once.
- The items of an ordered list (`rdf:Seq`) are one value, a list in
  their order, since the order is part of the information: the first
  entry of `xmp:dc-creator` is the first author
  (`find xmp:dc-creator.0=Someone`). A list with one item is recorded
  as that item, so `find xmp:dc-creator=Someone` finds a sole author.
- Of language alternatives (`rdf:Alt`), the default language
  (`x-default`) is recorded, or the first entry if none is marked.
- The fields of a structure are appended to the property name with a
  dash: the city of `Iptc4xmpExt:LocationCreated` becomes
  `xmp:iptc4xmp-ext-location-created-iptc4xmp-ext-city`. In a list of
  structures, the fields are recorded like items of the list: in an
  unordered list, each field's value is a value of its own; in an
  ordered list, such as the edit history, each field collects the
  values of all entries in one list, in order.

Values are the packet's text: `"5"` is a string, and a date keeps its
format.

The prefix is the one the XMP specification assigns to the namespace,
not the one declared in the file. A packet that declares Dublin Core as
`dcterms` is still recorded as `xmp:dc-…`. A namespace this extractor
does not know keeps the prefix declared in the packet.

Supported types: `image/jpeg` (including the extended XMP packet of
large files), `image/tiff`, `image/png`, `image/webp`, `image/heif`,
`image/heic`, `image/avif`.

## The `iptc` contract

Records every dataset of the IPTC-IIM application record (the press
metadata format that predates XMP and is still found in JPEG and TIFF
files) under `iptc:`, with the dataset name in kebab case:

```
iptc:object-name = "Squirrel"
iptc:keywords = "wildlife"
iptc:keywords = "park"
iptc:by-line = "Someone"
iptc:city = "Kiel"
iptc:date-created = "20060521"
iptc:copyright-notice = "(c) 2006 by Someone"
```

A repeated dataset, such as keywords, is recorded as one value per
occurrence, so `find iptc:keywords=park` finds every image with that
keyword. The same text given twice is recorded once.
Text is read as UTF-8 if the record declares UTF-8 or the bytes are
valid UTF-8, and as Latin-1 otherwise. Datasets without a known name and
binary datasets are not recorded.

Supported types: `image/jpeg`, `image/tiff`.

## The `raster` contract

Records the image dimensions and pixel format from the file header:

```
raster:width = 6000
raster:height = 4000
raster:depth = 8
raster:alpha = false
raster:color = "rgb"
```

- `raster:width`, `raster:height`: size in pixels
- `raster:depth`: bits per channel, as stored
- `raster:alpha`: whether the image has an alpha channel
- `raster:color`: the colour model, one of `gray`, `rgb`, `cmyk`,
  `indexed`

Only the header is read; no pixels are decoded. The values describe the
file as stored: a 4-bit palette PNG is recorded as `4` and `indexed`,
not as 8-bit RGB. JPEG's YCbCr is recorded as `rgb`. A TIFF that states
no colour model (a multiband file) gets no `raster:color`. For HEIC and
AVIF the values come from the `meta` box: the size and bit depth of the
primary image, an alpha plane attached to it, and a tiled grid counted
as one image. See [the vocabulary](../docs/vocabulary.md#rasterwidth)
for the attributes.

EXIF can contain the same information, for example
`exif:pixel-x-dimension`. Both are kept, each under its own source. They
can differ, for example when an image was resized after the camera
wrote the EXIF data.

Supported types: `image/jpeg`, `image/png`, `image/tiff`, `image/webp`,
`image/heif`, `image/heic`, `image/avif`. GIF and BMP are not supported
yet.

## Files without metadata

A file without readable EXIF, XMP or IPTC data, or without a header the
`raster` contract can read, is examined with an empty result: it gets
its receipt and is not offered again. Only a failure to read stdin is an
error. The supported types decide which files `ossuary extract` offers;
a file named on the command line is examined whatever its type, and
bytes the extractor cannot read give an empty result.

## Running it

Put the binary on the PATH next to `ossuary`, then:

```console
$ ossuary extract image
$ ossuary extract image:xmp
```

The first command runs all four contracts, the second only `xmp`. The
same names can be listed under `[extract] run` in the archive's
`config.toml`; a plain `ossuary extract` then runs them. `--full`
examines every image again. When a new version raises a contract's
generation, the next `ossuary extract` also examines every image again.

To run it by hand:

```console
$ ossuary-extract-image --identify
$ ossuary-extract-image exif < photo.jpg
$ ossuary-extract-image xmp < photo.jpg
$ ossuary-extract-image iptc < photo.jpg
$ ossuary-extract-image raster < photo.heic
```

## License

Apache License 2.0 (see [LICENSE](../LICENSE)).
