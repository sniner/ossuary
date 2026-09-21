# The ossuary attribute vocabulary

*What the attributes mean. The [format](format.md) fixes how a claim is
spelled; this document fixes what the words mean and how a reader turns
standing claims into an answer. It grows freely — a new attribute or
namespace is not a new format generation — and unknown attributes stay
legal: a claim nobody understands yet is a queue entry, not an error.*

## Reading rules

**Every attribute is a set.** An assertion puts a value in, a
retraction takes exactly that value out — valueless, it empties the
attribute — and nothing ever overwrites anything. The answer for an
attribute is its standing values, deduplicated: what was said twice is
in the set once. Nothing anywhere declares how many values an attribute
may hold, because nothing has to: `user:tag` holds several values
because several were said, and `file:size` holds one because the bytes
have one size and every honest writer lands on the same element.

**Narrowing is the reader's business.** A view that wants a single MIME
type for a file picks one — the newest, say, or the best-trusted
source — and that choice is the view's own policy, applied at query
time, never the archive's. The set is the honest default answer;
anything sharper is interpretation, and interpretation does not harden
into the platform — not into attribute names, not into claim fields,
not into this software. A question that cares who said a value — the
extract worklist does — asks the claims themselves, source and all: the
set is the default view of an attribute, not the only one.

**Extractors speak verbatim.** An extractor is an observer: it records
what the format says, in the format's own terms —
`exif:date-time-original`, value as found — and never normalizes. What
"the creation date" means, when EXIF, a mail header and a PDF field all
have an opinion, is interpretation; interpretation is query-time policy,
and the mapping stands in this document, where it can change without
touching the log.

**Upgrades re-claim, deliberately.** A new extractor generation runs
again and says again what it found, its source naming the new
generation. A value said again lands on the element already standing; a
value newly found joins the set, and a reader that wants only the newest
opinion narrows by source — "everything from `extractor:exif/` older
than 3" — at query time. The log keeps every word ever said, which is the
point.

**Links are values.** A relation's value is a full subject — the bare
hex, full length. There is no `link:` namespace — namespaces sort by meaning
domain, and a value's type is vocabulary metadata, declared here. There
is no `derive:generated-by` either: every claim already names its maker,
in its source.

## Namespaces

- `prov:` — provenance: the archive's own acts — who took content in,
  what has looked at it and what the looking remarked. In which call a
  claim was written is not an attribute but the claim's own `run`
  field, spelled in the [format](format.md)
- `file:` — the file as observed: its places, names, size, kind and
  mtimes — what any format has on day one
- `derive:` — relations between content: what came from what
- `user:` — what the user says; the archive takes their word
- `exif:` — verbatim EXIF fields, as `ossuary-extract-image` reads them
- `raster:` — the pixel grid as the file's header describes it, as
  `ossuary-extract-image` reads it — numbers to search by, each with
  one meaning: a width here is always pixels
- `pdf:` — verbatim PDF document information, as `ossuary-extract-pdf`
  reads it
- `mail:` — a message's own voice, verbatim, as `ossuary-extract-mail`
  reads it — its headers standing on the mail, a part's identity on the
  part
- `zip:` — what a zip archive says about itself, verbatim, as
  `ossuary-extract-packed` reads it — the inventory standing on the
  archive, an entry's place on the unpacked file
- `mailbox:` — the mailbox as observed: where a message was when it
  was fetched, as `ossuary-mailvault` saw it

Vocabulary for subjects that are not blobs waits for the first real
need.

## Attributes

### prov:host

- meaning: who the ingesting machine says it is; an FQDN where there is
  one
- value: string
- written by: ingest

### prov:examined

- meaning: the receipt that an extractor has looked at this blob,
  whatever came of it — written once per blob and contract generation,
  on the original. It tells "looked, nothing found" apart from "never
  looked", and covers the extractor whose whole harvest was derived
  content standing elsewhere
- value: string, the source of the extractor that looked —
  `"extractor:image-exif/1"`, the same word the claim's source says.
  Said as the value so the standing set, which knows subject, attribute
  and value and nothing of who said it, tells one extractor's receipt
  from another's, and a retraction can take back exactly one
- written by: `ossuary extract`

### prov:note

- meaning: a remark an examiner left about this content — what the
  harvest itself cannot say: why one was discarded, figures included,
  or that the document refused to be read. Said only when there is
  something to say; an empty harvest is no event. Remarks accrete, and
  who remarked is the claim's source — `prov:note` under
  `extractor:pdf/…` read together is "this PDF gave no text", with no
  narrower attribute needed
- value: string, one sentence in the examiner's own words
- written by: extractors, as an ordinary finding — today
  ossuary-extract-pdf and ossuary-extract-packed, on entries its
  unpacking could not bring out

### file:path

- meaning: the real place a file sat when it was taken in — absolute,
  symlinks and `..` resolved, the name included: one sighting, one
  atomic value. Places accrete; the machine each one is on is
  `prov:host`, said in the same breath. A standing place is what makes
  a file part of the present: `find` answers only with files a place
  stands on, or that were won out of one (`derive:derived-from`);
  `--all` asks for every file held
- value: string, a path
- written by: ingest
- taken back by: ingest, when a later run over the same directory on
  the same host meets no file there — the file no longer lies at that
  place. What the run did not cover it does not judge: a directory
  that would not open, a path the excludes leave out, a root named as
  a single file, a directory met with not one file under it or no
  longer there at all, unless the run is told it was emptied on
  purpose (`--emptied`). A run told
  to collect (`--collect`) judges nothing. `--as-of` before the run
  still shows the file where it was

### file:name

- meaning: a name the content was known by — at its place in a
  filesystem (the last element of `file:path`, said alongside it, so a
  name is askable without string surgery) or inside another format (an
  attachment's own file name). Names accrete like sightings do: bytes
  met again under another name hold both
- value: string, a bare file name
- written by: ingest; ossuary-extract-mail and ossuary-extract-packed,
  for a derived file the mail or the zip named. A derived file nobody
  named, extracted text for one, carries none: the name an extractor
  announces it under is a handle, and the record never learns it

### file:size

- meaning: the content's size in bytes — a fact of the bytes, said on
  the bytes' first day in a store; bytes that reach `derived/` first and
  `content/` later are said twice, and the set holds one
- value: number
- written by: ingest; `ossuary mailvault`

### file:mime

- meaning: what the bytes are. Ingest sniffs — magic bytes, a UTF-8
  look for plain text, and `application/octet-stream` as the honest
  shrug; a derived file's kind is announced by the extractor that wrote
  the bytes, and needs no guessing; an extractor that recognizes a
  format from the inside says the sharper word — a mail sniffed as
  `text/plain` gains `message/rfc822`, a mailbox `application/mbox`, a
  zip that is really an epub or an Office document gains its declared
  kind. All the words stand in the set
- value: string, a MIME type
- written by: ingest, on the blob's first day; `ossuary extract`, in
  the deriving extractor's words; ossuary-extract-mail and
  ossuary-extract-packed, on what they recognized; `ossuary mailvault`,
  which knows it holds a message and says `message/rfc822` on every
  sighting — what a taker knows is not answered by the bytes, and may
  be the one thing the sniff got wrong

### file:modified

- meaning: the mtime a sighting observed, repeated verbatim — RFC 3339
  UTC with exactly the fractional digits the filesystem told, trailing
  zeros trimmed, no fraction on a whole second. Sightings accrete — a
  touch, a backup restore, another place: each tells its own time
- value: string
- written by: ingest

### derive:derived-from

- meaning: what this content came from — stands on the derived blob and
  points at its origin. Who made the derivation is the claim's source
- value: string, a full subject
- written by: `ossuary extract`, when it takes a derived file in

### user:tag

- meaning: a label the user put on the content — their own word, not
  derivable from the bytes, so no extractor will ever re-say it
- value: string
- written by: user, through `ingest --tag` — said at arrival, on every
  file the run records — and `annotate --tag`, said later on named
  files; the claim's source is `user` either way, because the human
  asserts and the command is only the pen

### user:comment

- meaning: a remark the user attached to the content, in their own
  words. Remarks accrete — a second one stands beside the first, and
  who said what when is the claim history's answer
- value: string
- written by: user, through `annotate --comment`

### exif:…

- meaning: one EXIF field, verbatim — the tag name kebab-cased
  (`exif:date-time-original`, `exif:f-number`), the value as the format
  stores it: `"2019:07:14 11:02:41"`, `"28/10"`. Never normalized here;
  what "the creation date" is stays a query-time mapping
- value: text as text, numbers as numbers, rationals as
  `numerator/denominator` — one value bare, several as a list
- written by: ossuary-extract-image, under its `exif` contract

### raster:width

- meaning: the pixel grid's width, as the file's header states it. Not
  what EXIF says the width is — `exif:pixel-x-dimension` is the
  camera's word and may be stale after a resize; this is the file's
- value: integer, pixels
- written by: ossuary-extract-image, under its `raster` contract

### raster:height

- meaning: the pixel grid's height, as the header states it
- value: integer, pixels
- written by: ossuary-extract-image, under its `raster` contract

### raster:depth

- meaning: bits per channel, as stored — a four-bit palette PNG says 4,
  a sixteen-bit TIFF 16, a JPEG 8
- value: integer, bits
- written by: ossuary-extract-image, under its `raster` contract

### raster:alpha

- meaning: whether the file carries transparency — an alpha channel, or
  a PNG's tRNS chunk giving a palette or a colour its transparency
- value: boolean
- written by: ossuary-extract-image, under its `raster` contract

### raster:color

- meaning: the colour model the pixels are stored in. A JPEG's YCbCr is
  `rgb`, which is what it encodes; a palette is `indexed` whatever its
  entries hold. Absent where the format names no model, as a multiband
  TIFF does not
- value: string, one of `gray`, `rgb`, `cmyk`, `indexed`
- written by: ossuary-extract-image, under its `raster` contract

### pdf:…

- meaning: one entry of a PDF's document information dictionary,
  verbatim — the key kebab-cased (`pdf:title`, `pdf:creation-date`),
  the value as the document spells it: a date stays
  `"D:20190714110241+02'00'"`. The extracted text itself is not an
  attribute but a derived file, `text/plain`, tied to the document by
  `derive:derived-from`
- value: string
- written by: ossuary-extract-pdf

### mail:…

- meaning: one header of a message's own voice, verbatim — `mail:` plus
  the header's name, lowercased: from, sender, reply-to, to, cc, bcc,
  subject, date, message-id, in-reply-to and references, each claimed as
  often as it stands. The value is unfolded and its RFC 2047 encoded
  words are decoded — conversion, not tidying — and otherwise spelled as
  the mail spells it: the date keeps its own calendar
  (`"Thu, 4 Sep 2026 12:34:56 +0200"`), addresses their display names,
  commas and angle brackets. The transport's trail — received,
  return-path, the x- families — describes the journey, not the message,
  and stays untold. What the mail carries — named attachments, nested
  messages — is not an attribute but derived files, tied to the mail by
  `derive:derived-from`; on such a part stands `mail:content-id`, the
  part's identity as the mail spelled it (`"<part2@example.com>"`)
- value: string
- written by: ossuary-extract-mail

### zip:entry

- meaning: one entry a zip archive holds, spelled as the archive spells
  it, inner path included — the inventory, standing on the archive
  itself whether or not anything was unpacked, so "which zip holds a
  file so named" is a question the record answers. Directories are
  structure, not content, and stay untold
- value: string, the entry's path inside the archive
- written by: ossuary-extract-packed, under its `list` contract

### zip:path

- meaning: where an unpacked file sat inside its zip — the full entry
  path, standing on the derived file, whose bare name is `file:name`
  and whose origin is `derive:derived-from`
- value: string, the entry's path inside the archive
- written by: ossuary-extract-packed, under its `unpack` contract

### mailbox:place

- meaning: where a message was seen — the account it was fetched from
  and the folder in it, as one value: the account's name as
  `mailvault.toml` gives it, a slash, the folder as the server spells
  it (`"example.org/INBOX"`, `"example.org/[Gmail]/All Mail"`). The two
  stay together because a message in two folders of two accounts must
  not lose which was where. Information, like `file:path`: a sighting,
  true for its time — an account renamed later is a new name in new
  sightings. Places accrete. Taken over from a mailvault archive, a
  place may name the account alone (`"example.org"`, folder unknown)
  or a folder alone (`"/old mail"`, no account behind it). The
  server's numbering of a message is not here and nowhere on the
  record: it is temporary, and lives in the fetcher's own memory in
  `cache/`. A standing place here makes a message part of the present
  the way `file:path` makes a file one
- value: string
- written by: `ossuary mailvault`

### mailbox:seen

- meaning: when a place was seen holding the message, where that is
  not the claim's own time. A fetch says nothing here: its claim's time
  is the sighting. A takeover repeats what a mailvault archive's log saw
  years earlier, and carries the date the log file was sealed, in the
  log's own spelling. Like `file:modified` beside `file:path`, it stands
  beside `mailbox:place` in the set without being paired to one place;
  a message seen in two places at two dates holds both dates
- value: string, the date as the vault's log wrote it
- written by: `ossuary mailvault --from-vault`
