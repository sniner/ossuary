# The ossuary attribute vocabulary

*What the attributes mean. The [format](format.md) defines how a claim
is written; this document defines what the attributes mean and how a
reader turns the standing claims into a result. It can be extended at
any time: a new attribute or namespace does not require a new format
generation. Unknown attributes are valid: a claim that no program
understands yet is kept like any other and is not an error.*

## Reading rules

**Every attribute is a set.** An assertion adds a value, and a
retraction removes exactly that value; a retraction without a value
removes every value of the attribute. No claim ever overwrites another.
The result for an attribute is its standing values, deduplicated: a
value asserted twice is in the set once. No attribute declares how many
values it may have, and none needs to: `user:tag` has several values
because several were asserted, and `file:size` has one because the
bytes have one size and every correct writer records the same value.

**Narrowing is up to the reader.** A view that needs a single MIME type
for a file picks one, for example the newest or the one from the most
trusted source. That choice is the view's own policy, applied at query
time, and never the archive's. The set is the default result. Anything
narrower is an interpretation, and interpretations are not built into
attribute names, claim fields or this software. A query that depends on
who asserted a value (the extract worklist does) reads the claims
themselves, including their source: the set is the default view of an
attribute, but not the only one.

**Extractors record verbatim.** An extractor records what the format
contains, under the format's own names and with the values as found
(`exif:date-time-original`, for example), and never normalizes. Which
value is "the creation date", when EXIF, a mail header and a PDF field
each have one, is interpretation. Interpretation is query-time policy,
and the mapping belongs in this document, where it can change without
any change to the log.

**Upgrades write their claims again, on purpose.** A new extractor
generation examines the files again and records what it finds, under a
source that names the new generation. A value that is already standing
stays one element of the set; a newly found value is added. A reader
that wants only the newest generation's values narrows by source at
query time, for example by ignoring everything from `extractor:exif/`
older than 3. The log keeps every claim ever written.

**Links are values.** The value of a relation is a full subject: the
bare hex digest, full length. There is no `link:` namespace: namespaces
group attributes by subject area, and the type of a value is vocabulary
metadata, declared in this document. The origin of a derived blob is
provenance, so it is recorded under `prov:`, next to `prov:host` and
`prov:examined`, and not in a namespace for relations. There is no
`prov:generated-by` either: every claim already names what wrote it, in
its source.

## Namespaces

- `prov:` is provenance, the archive's records of its own actions:
  which host ingested content, which extractors examined it, the notes
  they left, and which content was extracted from which. The call a
  claim was written in is not an attribute; it is the claim's `run`
  field, defined in the [format](format.md)
- `file:` describes the file as observed: its paths, names, size, type
  and mtimes, which are known for a file of any format as soon as it is
  ingested
- `user:` holds what the user states; the archive records it as given
- `exif:` holds EXIF fields verbatim, as read by `ossuary-extract-image`
- `raster:` describes the pixel grid as the file's header states it, as
  read by `ossuary-extract-image`. The values are numbers to search by,
  each with one fixed meaning: a width is always in pixels
- `xmp:` holds XMP properties verbatim, as read by
  `ossuary-extract-image`, with the schema's prefix included in the
  name: `xmp:dc-subject`, `xmp:photoshop-city`
- `iptc:` holds IPTC-IIM datasets verbatim (the press metadata format
  that predates XMP), as read by `ossuary-extract-image`
- `pdf:` holds PDF document information verbatim, as read by
  `ossuary-extract-pdf`
- `mail:` holds a message's own headers verbatim, as read by
  `ossuary-extract-mail`. The headers are recorded on the message, a
  part's identity on the part
- `packed:` holds the list of entries of a packed archive, as read by
  `ossuary-extract-packed`, recorded on the packed archive. The
  namespace is named for the kind of file, not for a format: a query
  for the archive that contains a file does not depend on whether it
  was a zip
- `mailbox:` describes the mailbox as observed by `ossuary-mailvault`:
  where a message was when it was fetched, and which tags the mailbox
  had on it there

There is no vocabulary yet for subjects that are not blobs; it will be
added when it is first needed.

## Attributes

### prov:host

- meaning: the name the ingesting machine reports for itself; an FQDN
  where there is one
- value: string
- written by: ingest

### prov:examined

- meaning: the receipt that an extractor has examined this blob,
  whatever the result. Written once per blob and contract generation,
  on the examined blob. It distinguishes "examined, nothing found" from
  "never examined", and it also records the examination when the
  extractor's only output was derived files, whose claims are on other
  subjects
- value: string, the source of the extractor that examined the blob,
  `"extractor:image-exif/1"`: the same string as the claim's source.
  The source is repeated as the value because the standing set holds
  subject, attribute and value but not the source. With the source in
  the value, the receipts of different extractors are separate
  elements, and a retraction can remove exactly one of them
- written by: `ossuary extract`

### prov:note

- meaning: a note an extractor left about this content, for what its
  findings cannot express: why its output was discarded, with the
  figures, or that the document could not be read. Written only when
  there is something to report; an examination that found nothing
  writes no note. Notes accumulate, and the extractor that wrote a note
  is the claim's source: `prov:note` with the source
  `extractor:pdf-text/…` means "this PDF gave no text", so no narrower
  attribute is needed
- value: string, one sentence written by the extractor
- written by: extractors, as an ordinary finding. Currently
  ossuary-extract-pdf; also ossuary-extract-packed and the
  `attachments` contract of ossuary-extract-pdf, about entries they
  could not extract

### file:path

- meaning: a path at which the content was seen, written the way its
  container writes paths, with the container recorded alongside. On a
  filesystem: the real path of the file when it was ingested, absolute,
  with symlinks and `..` resolved, and including the file name. Each
  sighting is one value; the machine is recorded as `prov:host`. Inside
  other content: the path an entry had in its archive, or the name an
  attachment had in its document, written as a leading `@` followed by
  the entry's path verbatim (`@invoices/2026-03.pdf`, `@factur-x.xml`;
  an entry whose path itself begins with `@` becomes `@@…`); the
  container is recorded as `prov:origin`.
  Paths accumulate. One search term finds a file wherever it was:
  `file:path=*/2026-03.pdf` matches it on a disk or inside an archive,
  whatever the archive format. How an inner path is used is up to the
  reader: `export` writes the entry under the path it had in its
  archive, and the mount leaves it out of its tree.
  A standing filesystem path is what makes a file present: by default,
  `find` returns only files that have one, and files extracted from
  such a file (`prov:origin`). An `@`-led path alone does not make a
  file present: an entry is present as long as its archive is. `--all`
  searches the record instead: every claim ever written about every
  stored file, retracted paths included
- value: string, a path
- written by: ingest; ossuary-extract-packed, on an unpacked entry;
  ossuary-extract-pdf and ossuary-extract-mail, on an attachment
- retracted by: ingest, when a later run over the same directory on the
  same host finds no file at that path, because the file is no longer
  there. The run retracts nothing in what it did not cover: a directory
  that could not be opened, an excluded path, a root given as a single
  file, and a directory in which the run finds no file at all or which
  no longer exists, unless the run is told with `--emptied` that it was
  emptied on purpose. A run with `--collect` retracts nothing. With
  `--as-of` set to a time before the run, the file is still shown at
  its old path

### file:name

- meaning: a name the content had: at its path in a filesystem (the
  last component of `file:path`, recorded together with it so that a
  name can be searched without parsing paths) or inside another format
  (an attachment's own file name). Names accumulate like paths: bytes
  found again under another name have both names
- value: string, a bare file name
- written by: ingest; ossuary-extract-mail, ossuary-extract-packed and
  ossuary-extract-pdf, for a derived file that has a name in the mail,
  the zip or the document. A derived file without such a name, such as
  extracted text, has none: the name under which an extractor
  announces a file is only a handle and is never recorded

### file:size

- meaning: the content's size in bytes, a property of the bytes,
  recorded when the bytes are first stored in a store. Bytes stored in
  `derived/` first and in `content/` later get the claim twice, and the
  set has one value
- value: number
- written by: ingest; `ossuary mailvault`

### file:mime

- meaning: the type of the bytes. Ingest detects it from magic bytes
  and a UTF-8 check for plain text, and records
  `application/octet-stream` when neither identifies the type. The type
  of a derived file is announced by the extractor that wrote the bytes
  and is not detected. An extractor that recognizes a format from its
  content records the more specific type: a mail detected as
  `text/plain` gets `message/rfc822`, a mailbox `application/mbox`, and
  a zip that is an epub or an Office document gets its declared type.
  All these values are in the set
- value: string, a MIME type
- written by: ingest, when the blob is first stored; `ossuary extract`,
  with the type the deriving extractor announced; ossuary-extract-mail
  and ossuary-extract-packed, for formats they recognized;
  `ossuary mailvault`, which records `message/rfc822` on every sighting
  because it knows the file is a message. That knowledge does not come
  from the bytes, and detection from the bytes may have got exactly
  this wrong

### file:modified

- meaning: the mtime observed at a sighting, recorded verbatim: RFC 3339
  UTC with exactly the fractional digits the filesystem reported,
  trailing zeros removed, and no fraction for a whole second. Values
  accumulate: after a touch, after a restore from backup, or at another
  path, each sighting records its own mtime
- value: string
- written by: ingest

### prov:origin

- meaning: the content this content was derived from. Recorded on the
  derived blob, with the origin as its value. The claim's source is the
  extractor that made the derivation
- value: string, a full subject
- written by: `ossuary extract`, when it stores a derived file

### user:tag

- meaning: a label the user put on the content. It is the user's own
  word and cannot be derived from the bytes, so no extractor will ever
  write it
- value: string
- written by: user, through `ingest --tag` (at ingest, on every file
  the run records) and `annotate --tag` (later, on the named files).
  The claim's source is `user` in both cases, because the user makes
  the assertion and the command only records it

### user:comment

- meaning: a comment the user attached to the content, in their own
  words. Comments accumulate: a second comment is added next to the
  first, and the claim history shows who wrote which one and when
- value: string
- written by: user, through `annotate --comment`

### exif:…

- meaning: one EXIF field, verbatim: the tag name in kebab case
  (`exif:date-time-original`, `exif:f-number`), and the value as the
  format stores it: `"2019:07:14 11:02:41"`, `"28/10"`. Values are never
  normalized here; which field is "the creation date" is a query-time
  mapping
- value: text as strings, numbers as numbers, rationals as
  `numerator/denominator`; a single value on its own, several as a list
- written by: ossuary-extract-image, under its `exif` contract

### xmp:…

- meaning: one property of the XMP packet, verbatim. The name is the
  schema's prefix and the property's name, each in kebab case, joined
  with a dash: `dc:subject` becomes `xmp:dc-subject`,
  `xmpMM:DocumentID` becomes `xmp:xmp-mm-document-id`, `photoshop:City`
  becomes `xmp:photoshop-city`. The prefix is the one the XMP
  specification assigns to the namespace, whatever prefix the file
  declared; for a namespace the extractor does not know, the file's
  prefix is used. Nested XMP is flattened: each item of a list
  (`rdf:Bag`, `rdf:Seq`) is a value of the property; of language
  alternatives (`rdf:Alt`), the default (`x-default`) is recorded, or
  the first one if none is marked; a field of a structure is appended
  to the name with a dash, so `Iptc4xmpExt:City` in
  `Iptc4xmpExt:LocationCreated` is recorded as
  `xmp:iptc4xmp-ext-location-created-iptc4xmp-ext-city`; and for a
  list of structures, the values of each field are collected on the
  same attribute, in order
- value: string, the packet's text unchanged (`"5"` and
  `"2019-07-14T11:02:41+02:00"` as written); a single value on its own,
  several as a list
- written by: ossuary-extract-image, under its `xmp` contract

### iptc:…

- meaning: one dataset of the IPTC-IIM application record, verbatim,
  with the dataset's name in kebab case: `iptc:keywords`,
  `iptc:by-line`, `iptc:caption-abstract`, `iptc:copyright-notice`,
  `iptc:date-created`. A dataset that occurs several times in the
  record, as keywords do, is recorded with every value. Datasets the
  extractor has no name for, and binary datasets, are left out
- value: string, the record's text: a date stays `"20190714"`. Decoded
  as UTF-8 where the record declares UTF-8 or the bytes are valid
  UTF-8, as Latin-1 otherwise; a single value on its own, several as a
  list
- written by: ossuary-extract-image, under its `iptc` contract

### raster:width

- meaning: the width of the pixel grid, as stated in the file's header.
  This is not the width EXIF gives: `exif:pixel-x-dimension` is written
  by the camera and may be out of date after a resize, while this value
  is taken from the file itself
- value: integer, pixels
- written by: ossuary-extract-image, under its `raster` contract

### raster:height

- meaning: the height of the pixel grid, as stated in the file's header
- value: integer, pixels
- written by: ossuary-extract-image, under its `raster` contract

### raster:depth

- meaning: bits per channel, as stored: 4 for a four-bit palette PNG,
  16 for a sixteen-bit TIFF, 8 for a JPEG, 10 for a ten-bit HEIC
- value: integer, bits
- written by: ossuary-extract-image, under its `raster` contract

### raster:alpha

- meaning: whether the file has transparency: an alpha channel, a PNG
  tRNS chunk that makes palette entries or a colour transparent, or a
  HEIF auxiliary alpha plane attached to the image
- value: boolean
- written by: ossuary-extract-image, under its `raster` contract

### raster:color

- meaning: the colour model in which the pixels are stored. A JPEG's
  YCbCr is recorded as `rgb`, since that is what it encodes; a palette
  image is `indexed`, whatever its entries contain. Not recorded when
  the format names no model, as with a multiband TIFF
- value: string, one of `gray`, `rgb`, `cmyk`, `indexed`
- written by: ossuary-extract-image, under its `raster` contract

### pdf:…

- meaning: one entry of a PDF's document information dictionary,
  verbatim: the key in kebab case (`pdf:title`, `pdf:creation-date`),
  and the value as written in the document; a date stays
  `"D:20190714110241+02'00'"`. The extracted text is not an attribute;
  it is a derived file of type `text/plain`, linked to the document by
  `prov:origin`. On an attachment, the same attributes record what its
  document states about it: `pdf:desc` and `pdf:af-relationship` from
  the file specification, and `pdf:creation-date` and `pdf:mod-date`
  from the stream's parameters. The attachment's name and path are
  recorded as `file:name` and `file:path`; its size is recorded by
  `ossuary extract`, not by the extractor
- value: string
- written by: ossuary-extract-pdf, under its `text` contract on the
  document and under its `attachments` contract on an attachment

### mail:…

- meaning: one header of the message itself, verbatim: `mail:`
  followed by the header's name in lower case. The headers recorded are
  from, sender, reply-to, to, cc, bcc, subject, date, message-id,
  in-reply-to and references, with one claim for each occurrence. The
  value is unfolded and its RFC 2047 encoded words are decoded (a
  conversion of the encoding, not a cleanup); otherwise it is kept as
  written in the mail: the date keeps its original format
  (`"Thu, 4 Sep 2026 12:34:56 +0200"`), and addresses keep their
  display names, commas and angle brackets. Transport headers
  (received, return-path, the x- headers) describe the delivery, not
  the message, and are not recorded. Named attachments and nested
  messages are not attributes; they are derived files, linked to the
  mail by `prov:origin`. Such a part has `mail:content-id`, the part's
  identifier as written in the mail (`"<part2@example.com>"`)
- value: string
- written by: ossuary-extract-mail

### packed:path

- meaning: one entry of a packed archive. The list of entries is
  recorded on the packed archive itself, whether or not anything was
  unpacked, so the record can be searched for the archive that contains
  a file of a given name. The value is written like an inner path in
  `file:path`: a leading `@` followed by the entry's path verbatim. An
  unpacked entry has the same value as its own `file:path`, with
  `prov:origin` naming the packed archive. Since both use the same
  form, an entry that was not unpacked is one whose `packed:path` is
  not the `file:path` of any file derived from the packed archive.
  Directories are not recorded
- value: string, an `@`-led path inside the archive
- written by: ossuary-extract-packed, under its `list` contract

### mailbox:place

- meaning: where a message was seen: the account it was fetched from
  and the folder in that account, as one value. The value is the
  account's name as given in `mailvault.toml`, a colon, and the folder
  as the server names it (`"example.org:INBOX"`,
  `"example.org:[Gmail]/All Mail"`). Account and folder are one value
  so that for a message in two folders of two accounts it stays known
  which folder was in which account. Like `file:path`, the value
  records a sighting and is true for the time of that sighting: if an
  account is renamed later, new sightings record the new name. Places
  accumulate. The claim's time is the time of the sighting: for a
  fetch the time of the fetch, for an import from an archive of the
  Python tool mailvault the time that archive's log gives for it. Such
  an imported place may also name only the account (`"example.org"`,
  folder unknown) or only a folder (`":old mail"`, no account). The number the
  server gives a message is not recorded here or anywhere else in the
  record: it is temporary, and the fetcher keeps it in its own memory
  in `cache/`. A standing `mailbox:place` makes a message present in
  the same way as a standing `file:path` makes a file present
- value: string
- written by: `ossuary mailvault`

### mailbox:tag

- meaning: a tag the mailbox had on a message, in addition to its
  place: an Outlook category, under the name the mailbox shows for it.
  It is not part of the message's bytes, so no extractor can write it;
  it records what the mailbox had on its copy when the message was
  fetched, and it is lost when the mailbox is. Unlike places, tags do
  not accumulate: a fetch records the tags it sees and retracts
  standing tags that are no longer there. The standing set is
  therefore the tags as of the last fetch, and `--as-of` shows the tags
  as of the given time. A message fetched from two accounts that tag it
  differently has one set of tags, and a fetch retracts a tag it no
  longer sees, whichever account it came from. The category's colour
  belongs to the mailbox's display and is not recorded
- value: string
- written by: `ossuary mailvault`, for a mailbox fetched over MS Graph;
  a mailbox fetched over IMAP gets no tags
