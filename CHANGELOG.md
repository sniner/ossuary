# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- **Archive format, generation 1** — the reading contract in [docs/format.md](docs/format.md):
  content, derived content and claims as three [immure](https://github.com/sniner/immure) stores
  ranked by what losing them costs — what was taken in and what tools made of it never mingle —
  an append-only claim log in sealed segments, disposable caches, and recovery with shell
  tools alone
- **Attribute vocabulary** — [docs/vocabulary.md](docs/vocabulary.md): what each attribute means
  and how standing claims become an answer — every attribute is a set of standing values,
  deduplicated at fold time; narrowing to one value is the reader's own policy; extractors
  record verbatim, interpretation stays at query time
- **`ossuary init`** begins an archive — or completes one already standing: nothing standing is
  remade or edited, and what is missing (today: `config.toml`) is added
- **`config.toml`** in the archive root: `[ingest] exclude` glob patterns (`.DS_Store` and
  friends never go in; an excluded directory is not walked), `[store] compress` (zstd for new
  entries, in the content and derived stores alike — what one holds is as sensitive as the
  other; what is already stored keeps its form, and reading understands both) and
  `[extract] run` (the extractors a bare `ossuary extract` runs, in order). A missing
  file means the defaults; an unknown key is refused rather than half-applied
- **`ossuary ingest PATH…`** (callable as `add` — what `get` hands out, `add` takes in) takes
  directory trees and single files in, any mix, several per
  call — a glob's expansion included — all under one run id, so "arrived together" stays an
  askable fact; a path that will not resolve is named in the verdict and costs only itself.
  Every regular file goes
  into the content store, seven day-one claims per new blob into the log — a blob met again gets
  its sighting only, and every place and name it sat under goes on the record; the mtime is
  recorded at the precision the filesystem observed it. `--tag TAG` (repeatable) says the
  user's own word at arrival: a `user:tag` claim under the source `user` on every file the run
  records. A repeated run remembers what
  it already observed (in `cache/`) and leaves unchanged files in peace: not read, not hashed,
  no claims — tags among them, and the verdict says so; `--full` looks at everything anew
- **`ossuary seal`** closes the open segment; its claims become part of the sealed log. The open
  segment also closes itself once it grows to 1 MiB — a few thousand claims — no matter which
  command was writing; the command remains for sealing on demand, before a backup or right away
- **Per-segment manifests** in `cache/manifests/` — one small memo per sealed segment (claim
  count, time range, namespaces, a bloom filter over the subjects), filed at sealing time and
  rebuilt in passing when missing: commands that walk the log no longer read every sealed
  segment just to put them in order. Like every cache, deletable at any time — the next walk
  refills it
- **`ossuary about SUBJECT`** answers everything on the record about one file, oldest first; a
  beginning of the digest is enough while it names only one file, and naming attributes (or a
  namespace, like `exif:`) narrows the answer to them
- **`ossuary find TERM…`** answers which files match: terms are `attribute=value` and must all
  hold, `*` and `?` match within text values, `low..high` asks for a value inside the range
  (either side open — `file:modified=2026-09-01..` is "changed since September"; bounds compare
  in the attribute's own spelling; a value in double quotes is literal), and
  `--missing ATTRIBUTE` (or a namespace like `exif:`) asks for what a file lacks. Only standing
  values count — a retracted value no longer answers. The question is also the projection: each
  match answers as a block — the file's name (shortened like a git hash, growing as the archive
  does) on a line of its own, every attribute the query named indented beneath it as the
  `attribute=value` pairs a query would use — a pair pastes back into a refined query, quotes
  and all, and every standing value is shown. A bare attribute among the terms is shown without filtering
  (`find file:name=*.pdf file:modified`), a namespace like `exif:` shows all of it, and with
  only bare attributes every file on the record answers. `--id` answers with the full names
  alone, one per line, ready to pipe; `--json` answers one JSON object per match with the
  values as lists
- **`ossuary annotate SUBJECT… --comment TEXT --tag TAG`** puts the user's own word on files
  already on the record: each comment and tag becomes a claim (`user:comment`, `user:tag`)
  under the source `user`, on every named file — beside what `ingest --tag` said at arrival.
  Both options repeat, several files go in one call, and every name is resolved before
  anything is written, so a mistyped name refuses the whole call.
  `ossuary find --id … | xargs ossuary annotate --tag …` is the after-the-fact batch tagging
- **`ossuary value SUBJECT ATTRIBUTE`** answers what currently stands for one attribute —
  retractions applied, repeats collapsed — one value per line, strings bare, ready for a script;
  several lines mean the attribute honestly holds several values, and choosing among them stays
  the caller's policy. Exits 1 when nothing stands, so a script can test for it
- **`--json`** on `about` and `value` answers in JSON lines ready for `jq`: `about --json` gives
  each claim exactly as the log spells it, `value --json` keeps the values' JSON spelling
- **`--archive DIR`** on every command names the archive to work in; standing in it is enough,
  and so is the environment — `OSSUARY_ARCHIVE` holds the name for a whole shell session, the
  flag still outranking it
- **`--quiet`** on every command keeps the run's narration off stderr — counts, progress, hints;
  answers and errors still come
- **`ossuary get SUBJECT`** hands a file's bytes back, to stdout or `--output FILE`; short
  digests resolve against the content and derived stores, from six characters up
- **`ossuary export PATH ID…`** lays files back out of the archive, as they arrived: each ID is
  one file (its hex name, or a beginning) or a whole run — the id an ingest or extract verdict
  names, spelled out whole — and both kinds mix in one call. A run's files land under the paths
  that run recorded, kept relative: the folders all the exported files share are trimmed away,
  unrelated places become sibling folders under PATH, and what lay side by side lands side by
  side. A file named alone lands at every place still standing on its record — no place is
  preferred over another, and a retracted one no longer counts; a derived file, which never sat
  anywhere, flat under its recorded names; the same bytes standing at two places come out as two
  files, the way they stand. Nothing standing at PATH is ever overwritten — a
  file already there with the same bytes counts as done, different bytes are a named failure —
  and `--dry-run` answers what would land where without writing anything.
  `ossuary find --id … | xargs ossuary export DIR` exports a found set
- **`ossuary audit`** proves the archive intact from its own files alone — the cache plays no
  part. Every file in the content and derived stores is read back whole and its bytes re-hashed
  against its name; every sealed segment and the open head must read back claim by claim; and
  every file the claims speak of — as a claim's subject, or named as what a derived file came
  from — must be held by a store, because nothing is ever deliberately removed from an archive.
  Files held that no claim speaks of are noted, not counted as findings: an interrupted run
  leaves such files, and the next arrival records them. The answer counts what it finds, up to a
  handful of names spelled out right there; `--json` answers one object per finding, ready for a
  script, and a sound archive answers an empty stream. Exits 0 when the archive is sound, 1 when
  findings stand
- **`--verbose`** on every command: answers in full, every name spelled out where a count would
  stand — today `audit` is the one command with more to say
- **`ossuary id FILE`** names a file the way the archive would — hashed from its bytes, the file
  only read — and says whether the archive already holds it; works before an ingest as well as
  after
- **`ossuary extract [NAME]`** runs one extractor — its own program, `ossuary-extract-NAME`
  found on PATH — or, with no NAME, the archive's own list from `[extract] run`, in rounds
  until a whole round finds nothing left to examine: what one extractor hands back, the next
  round offers to whichever extractor reads it, so a single call drives a chain like
  mail → attachment → text to its end (list order costs at most an extra round, and a call cut
  short continues next time — the worklist is refolded from log and receipts, not queued). A
  listed extractor that cannot answer `--identify` is skipped with its reason while the healthy
  ones still run. Each goes over every file of a kind it reads that it has not examined yet:
  findings go on the record under the extractor's name, and every examined file gets a receipt,
  found something or not, so a repeated run costs only what is new and a new extractor version
  looks at everything again. Every derived file taken in is stamped `prov:run` with the call's
  own run id — the same anchor an ingest run leaves — so "won together" stays an askable fact. An extractor can hand back files as well as findings — an unpacked attachment,
  extracted text — and each goes into the archive's derived store as content of its own, named
  and typed in the extractor's words (`file:name`, `file:mime`) and tied to its origin
  (`derive:derived-from`) — apart from the originals, so nothing that ever tidies derived
  content can reach what was taken in. Bytes the archive already holds as an ingested
  original — the attachment that was also saved as a file — are recorded without a second
  copy: a digest is store-agnostic, and the bytes answer from the content store;
  `--temp-dir` says where derived files wait on their way in, for when the archive sits on a
  slow share. Naming files narrows the run to them — `extract text SUBJECT` examines one file
  now instead of everything that waits, a beginning of the digest is enough, and a named file
  is handed over even when its kind is not one the extractor reads. `--full` examines anew,
  receipted or not: the named files, or everything of a kind the extractor reads — for the
  extractor upgrade that is worth a fresh look at the archive. The pipe protocol, open to any
  language, is [docs/extractors.md](docs/extractors.md)
- **Extractor contracts** — one program may carry several separately receipted capabilities:
  `--identify` answers one JSON line per contract, each with its own name, source, MIME list and
  `derives` flag, and each runs over its own worklist onto its own receipts. `ossuary extract
  NAME` runs every contract the program offers; `ossuary extract NAME:CONTRACT` runs one, and
  the same spelling holds in `[extract] run` — so "inventory the archives, never unpack them"
  can stand in `config.toml` as `"packed:list"`. Single-contract extractors are untouched: one
  identify line, spoken to exactly as before
- **`ossuary-extract-exif`** — the first extractor: EXIF fields verbatim, tag names kebab-cased
  under `exif:`, values as the format stores them (`"2019:07:14 11:02:41"`, `"28/10"`)
- **`ossuary-extract-text`** — the first deriving extractor: a PDF's plain text, extracted
  through poppler's `pdftotext` (which must be on PATH), goes into the archive as a `text/plain`
  file of its own beside the document information verbatim under `pdf:` (`pdf:title`,
  `pdf:creation-date` — dates as the document spells them). A document with no text to give —
  scanned pages, an unreadable file — is examined all the same, with nothing found
- **`ossuary-extract-mail`** — the mail extractor: a message's own headers verbatim under
  `mail:` (`mail:from`, `mail:subject`, `mail:date` — unfolded and their encoded words decoded,
  otherwise as the mail spells them; the transport's trail stays untold), and what the mail
  carries handed over as content of its own — named attachments and nested messages, each typed
  as the mail declared it, an attachment's content-id on its record. It reads `text/plain`
  deliberately, because a mail on disk sniffs as plain text: bytes that are no message are
  examined with nothing found, and a recognized mail gains the sharper `file:mime` of
  `message/rfc822` beside the sniffed word. An mbox is a mailbox, not a message, and stays
  untouched
- **`ossuary-extract-packed`** — the archive extractor, and the first program of two contracts:
  `list` inventories a zip without unpacking a byte — one `zip:entry` claim per entry, standing
  on the archive itself, so "which zip holds a file so named" becomes a question the record
  answers — and `unpack` hands every entry over as content of its own, flattened to its bare
  name (colliding names yield to a counter, the spelled name kept as `file:name`), its place
  inside the zip on the record as `zip:path`, its kind sniffed the way ingest sniffs, since a
  zip declares none. A zip that is really a document wearing zip as its envelope — epub and the
  OpenDocument family by their `mimetype` first entry, Word, Excel and PowerPoint by their type
  manifest — is recognized from the bytes, left shut, and gains its sharper `file:mime` instead;
  a jar promises nothing about its insides and is treated as the archive it is. Encrypted or
  damaged entries stay inside with a note — there is no password to offer — and bytes that do
  not read as a zip are examined with nothing found
