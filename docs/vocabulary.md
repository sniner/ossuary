# The ossuary attribute vocabulary

*What the attributes mean. The [format](format.md) fixes how a claim is
spelled; this document fixes what the words mean and how a reader turns
standing claims into an answer. It grows freely — a new attribute or
namespace is not a new format generation — and unknown attributes stay
legal: a claim nobody understands yet is a queue entry, not an error.*

## Reading rules

**Truth knows no cardinality.** In the log, every attribute is the same
thing: a bag of standing values. An assertion puts a value in, a
retraction takes one out, and nothing ever overwrites anything. Whether
a reader answers with all standing values or only one is a *read rule*,
declared here per attribute and applied at fold time:

- **many** — every standing value counts.
- **one** — the newest standing value wins; log order breaks ties.

The rule lives here and nowhere else, deliberately. Not in the name
(`user:tag*`): identifiers carry meaning taxonomy, never read rules — a
spelling in the log is forever, and a rule must be revisable without the
log ever having been wrong. Not in a claim field either: cardinality
depends on the attribute alone, and a fact of the attribute is stated
once, here — not repeated on every line that uses it.

**Extractors speak verbatim.** An extractor is an observer: it records
what the format says, in the format's own terms —
`exif:date-time-original`, value as found — and never normalizes. What
"the creation date" means, when EXIF, a mail header and a PDF field all
have an opinion, is interpretation; interpretation is query-time policy,
and the mapping stands in this document, where it can change without
touching the log.

**Upgrades re-claim, deliberately.** A new extractor version runs again
and says again what it found, its source naming the new version. The
fold supersedes by source prefix — "everything from `extractor:exif/`
older than 3.0" — and the log keeps every word ever said, which is the
point.

**Links are values.** A relation's value is a full subject
(`sha256:…`). There is no `link:` namespace — namespaces sort by meaning
domain, and a value's type is vocabulary metadata, declared here. There
is no `derive:generated-by` either: every claim already names its maker,
in its source.

## Namespaces

- `prov:` — provenance: where content was met, and what has looked at it
- `file:` — the bytes as observed: the facts any format has on day one
- `derive:` — relations between content: what came from what
- `user:` — what the user says; the archive takes their word
- `exif:` — verbatim EXIF fields, as the first extractor reads them
  (upcoming)

Subject prefixes other than hash algorithms stay reserved (see the
format paper); vocabulary for subjects that are not blobs waits for the
first real need.

## Attributes

### prov:ingest-path

- meaning: the real place a file sat when it was taken in — absolute,
  symlinks and `..` resolved
- value: string, a path
- cardinality: many
- written by: ingest

### prov:host

- meaning: who the ingesting machine says it is; an FQDN where there is
  one
- value: string
- cardinality: many
- written by: ingest

### prov:ingest-run

- meaning: the run a sighting arrived in — one UUID per run, so
  "arrived together" is exact
- value: string, a UUID
- cardinality: many
- written by: ingest

### prov:examined

- meaning: the receipt that an extractor has looked at this blob,
  whatever came of it — written once per blob and extractor version, on
  the original. It tells "looked, nothing found" apart from "never
  looked", and covers the extractor whose whole harvest was derived
  content standing elsewhere
- value: `true` — who looked, and with what, is the claim's source
- cardinality: many — one standing receipt per source
- written by: the extract orchestrator (upcoming)

### file:size

- meaning: the content's size in bytes — a fact of the bytes, said once,
  on the blob's first day
- value: number
- cardinality: one
- written by: ingest

### file:mime

- meaning: what the bytes are, by sniffing — magic bytes, a UTF-8 look
  for plain text, and `application/octet-stream` as the honest shrug
- value: string, a MIME type
- cardinality: one
- written by: ingest; extractors may know better later

### file:modified

- meaning: the mtime a sighting observed, repeated verbatim — RFC 3339
  UTC with exactly the fractional digits the filesystem told, trailing
  zeros trimmed, no fraction on a whole second
- value: string
- cardinality: many — sightings accrete, and different places tell
  different times
- written by: ingest

### derive:derived-from

- meaning: what this content came from — stands on the derived blob and
  points at its origin
- value: string, a full subject
- cardinality: many
- written by: extractors (upcoming)

### user:tag

- meaning: a label the user put on the content
- value: string
- cardinality: many
- written by: user
