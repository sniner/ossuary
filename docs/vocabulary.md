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

**Upgrades re-claim, deliberately.** A new extractor version runs again
and says again what it found, its source naming the new version. A
value said again lands on the element already standing; a value newly
found joins the set, and a reader that wants only the newest opinion
narrows by source — "everything from `extractor:exif/` older than
3.0" — at query time. The log keeps every word ever said, which is the
point.

**Links are values.** A relation's value is a full subject — the bare
hex, full length. There is no `link:` namespace — namespaces sort by meaning
domain, and a value's type is vocabulary metadata, declared here. There
is no `derive:generated-by` either: every claim already names its maker,
in its source.

## Namespaces

- `prov:` — provenance: the archive's own acts — who took content in,
  in which run, and what has looked at it
- `file:` — the file as observed: its places, names, size, kind and
  mtimes — what any format has on day one
- `derive:` — relations between content: what came from what
- `user:` — what the user says; the archive takes their word
- `exif:` — verbatim EXIF fields, as `ossuary-extract-exif` reads them
- `pdf:` — verbatim PDF document information, as `ossuary-extract-text`
  reads it
- `mail:` — a message's own voice, verbatim, as `ossuary-extract-mail`
  reads it — its headers standing on the mail, a part's identity on the
  part
- `zip:` — what a zip archive says about itself, verbatim, as
  `ossuary-extract-packed` reads it — the inventory standing on the
  archive, an entry's place on the unpacked file

Vocabulary for subjects that are not blobs waits for the first real
need.

## Attributes

### prov:host

- meaning: who the ingesting machine says it is; an FQDN where there is
  one
- value: string
- written by: ingest

### prov:run

- meaning: the run a record arrived in — one UUID per invocation, so
  "arrived together" is exact: ingest stamps it on every sighting,
  `ossuary extract` on every derived file it takes in, all rounds of
  one call under one id. What kind of run it was is the claim's
  source, like everything about who was acting
- value: string, a UUID
- written by: ingest; `ossuary extract`, on derived files

### prov:examined

- meaning: the receipt that an extractor has looked at this blob,
  whatever came of it — written once per blob and extractor version, on
  the original. It tells "looked, nothing found" apart from "never
  looked", and covers the extractor whose whole harvest was derived
  content standing elsewhere
- value: `true` — who looked, and with what, is the claim's source
- written by: `ossuary extract`

### file:path

- meaning: the real place a file sat when it was taken in — absolute,
  symlinks and `..` resolved, the name included: one sighting, one
  atomic value. Places accrete; the machine each one is on is
  `prov:host`, said in the same breath
- value: string, a path
- written by: ingest

### file:name

- meaning: a name the content was known by — at its place in a
  filesystem (the last element of `file:path`, said alongside it, so a
  name is askable without string surgery) or inside another format (an
  attachment's own file name). Names accrete like sightings do: bytes
  met again under another name hold both
- value: string, a bare file name
- written by: ingest; `ossuary extract`, in the deriving extractor's
  words

### file:size

- meaning: the content's size in bytes — a fact of the bytes, said once,
  on the blob's first day
- value: number
- written by: ingest

### file:mime

- meaning: what the bytes are. Ingest sniffs — magic bytes, a UTF-8
  look for plain text, and `application/octet-stream` as the honest
  shrug; a derived file's kind is announced by the extractor that wrote
  the bytes, and needs no guessing; an extractor that recognizes a
  format from the inside says the sharper word — a mail sniffed as
  `text/plain` gains `message/rfc822`, a zip that is really an epub or
  an Office document gains its declared kind. All the words stand in
  the set
- value: string, a MIME type
- written by: ingest; `ossuary extract`, in the deriving extractor's
  words; ossuary-extract-mail and ossuary-extract-packed, on what they
  recognized

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
- written by: ossuary-extract-exif

### pdf:…

- meaning: one entry of a PDF's document information dictionary,
  verbatim — the key kebab-cased (`pdf:title`, `pdf:creation-date`),
  the value as the document spells it: a date stays
  `"D:20190714110241+02'00'"`. The extracted text itself is not an
  attribute but a derived file, `text/plain`, tied to the document by
  `derive:derived-from`
- value: string
- written by: ossuary-extract-text

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
