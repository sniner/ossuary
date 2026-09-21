# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [0.8.0] - 2026-09-21

### Breaking changes

- **A place inside an archive is spelled with a leading `@`, and stands as `file:path` on the
  unpacked entry and as `packed:path` on the archive; `zip:entry` and `zip:path` are gone.** One
  term now reaches a file wherever it lay: `find file:path=*/2026-03.pdf` finds it on a disk or in
  an archive, and which archive format it was is nobody's concern. The inventory and the entry say
  the same value from either end, so "which archive holds this" and "where did this lie" are one
  question. An `@`-led place places nothing by itself: the entry is present while its archive is,
  as before. `export` lays an unpacked entry out under its path in the archive, so two entries
  named alike in different folders stay apart; the mount leaves inner places out of its tree.
  Nothing on an existing record is rewritten: `ossuary-fix packed` says every old inventory line
  and entry path again in the new words, `@` in front, with the old claim's moment, source and run.
  No schema version and no extractor generation moves

## [0.7.0] - 2026-09-21

### Breaking changes

- **A derived file's origin is `prov:origin`, no longer `derive:derived-from`.** Where a blob came
  from is provenance, so it stands beside `prov:examined` instead of alone in a namespace of its
  own, and the one attribute that was a predicate is now a noun like every other, so a term reads as
  one: `find prov:origin=e9ed…`. Nothing on an existing record is rewritten, the log being what it is,
  but the present is asked in the new words: a derived file whose origin stands only under the old
  name is held, not placed, until said again. `ossuary-fix origin` says every such origin again in
  seconds, with the old claim's moment, source and run; the old claims stay on the record as they
  were said. No schema version moves

### Added

- **`ossuary-fix`, a repair tool for the scars a 0.x archive collects.** One fix per scar, each
  reading the whole log and writing exactly what is missing, a second run finding nothing to do;
  `--dry-run` says what would be written. The first fix is `origin`, above
- **`find --with-derived` answers each match with what was won out of it.** Every derived file
  stands indented beneath its origin, as far as the derivations go, showing its kind ahead of what
  the question shows: a mail, its attachment, the attachment's text, in one answer instead of a
  copied id and a second `find prov:origin=…`. Under `--id` the names come flat, ready for
  `export`; under `--json` they nest under `derived`; the count says `3 file(s), 5 derived`

### Changed

- **A derived file carries a `file:name` only where the extractor said one.** The name an
  extractor announces a file under is a handle in its working directory; the record used to learn it
  as `file:name`, so every text excerpt was called `text.txt` and an attachment whose name had to
  yield to a collision stood under the made-up one as well. Mail attachments and zip entries keep
  their names, said by the extractor; extracted text has none. Names already on the record stay
  there, the log being what it is; `ossuary retract` takes one back where it bothers

## [0.6.3] - 2026-09-21

### Fixed

- **`find`, `ls`, `tree` and the mount answer in well under a second again.** Asking the
  present, the default scope, walked from every placed file to what was derived from it without an
  index, and took the better part of a minute on an archive of some thirty thousand files; the walk
  now goes through the index the cache already had. Nothing on the record changes, and the cache
  needs no rebuild

## [0.6.2] - 2026-09-20

### Changed

- **The same programs as 0.6.1, released from a commit whose CI run passed.** 0.6.1 was tagged
  on a commit that failed CI on an unused import in a test module; nothing in the shipped binaries
  differs

## [0.6.1] - 2026-09-20

### Fixed

- **A date alone means the same at every door.** `ossuary-mount --as-of 2026-09-01` read the date
  as the day's start where `ossuary --as-of` closes at its end; both now close at the end, and the
  mount takes a run id in place of the time, as the README already said. `find time=` reads a date
  the same way: `time=..2026-09-01` now includes the first, `time=2026-09-01` is the whole day, and a
  bound that names no moment is refused instead of compared as text
- **`export RUN` and `extract RUN` no longer deny a run that named no file.** A run of retractions,
  tags or findings alone is on the record, and `history` lists it; both now say the run named no
  file, and keep "no run on the record" for an id no claim carries
- **`ingest --emptied DIR` takes a directory that is no more.** It used to fail on resolving the
  name and take nothing back; the word now covers the plainest case, and every place on record
  under the vanished directory is taken back
- **A carried file's name no longer fails the whole examination.** An attachment or zip entry
  named longer than a filesystem takes, or with control characters in its name, made
  `ossuary-extract-mail` and `ossuary-extract-packed` fail the mail or zip on every run. The file
  now waits under a name cut to fit, and the spelled name goes on the record as `file:name`, the
  way a name that had to yield to a collision always did
- **`ossuary-extract-packed` streams each entry** into its file instead of holding it in memory
  whole, and an entry that unpacks to more than 1 GiB stays inside with the reason on the record,
  like an encrypted one
- **`ossuary mailvault` gives up a server that falls silent.** A connection that accepts and then
  says nothing held the run for good; a read or write that gets nothing for five minutes now fails,
  and the folder is named in the tally
- **`ossuary history` counts claims one way.** The line said "203 claim(s), 7 taken back" where the
  JSON said 210 claims; the line now reads "210 claim(s), 7 of them taken back", the JSON's reading.
  Two runs begun in the same second are listed in the order they began even when one went on past a
  seal
- **`ossuary mailvault` refuses `tls = false` against any host but this machine.** The README
  always said plaintext is for a bridge on loopback; the config is now read that way, and a password
  never crosses the network in the clear by a slip in the file. A takeover that fails halfway keeps
  the messages it took in remembered, and a call refused for its config or its accounts no longer
  leaves an empty memo behind
- **`ossuary-mount`** never plants a `.` or `..` from a place on record as a name in the view
- **`ossuary audit`** no longer hedges its two observations with "not a finding"; the JSON
  `observation` key says it. `ossuary ingest --help` counts six claims per file, as the record has
  since 0.6.0

## [0.6.0] - 2026-09-19

### Breaking changes

- **Every claim names its run.** A claim gains a seventh field, `run`: the id of the call that wrote
  it, the same dashed id the verdicts have always named. Ingest, extract, mailvault, annotate and
  retract all stamp it, on assertions and retractions alike, so "arrived together" and "taken back
  in one sweep" are exact for every kind of call. The `prov:run` attribute is no longer written; it
  said the same thing, for files a run took in only. Claims written before this version have no
  `run`, read as they always did, and still carry their `prov:run` where they had one — but no
  `run=` term and no `history` line reaches them; `find prov:run=ID` does, as before. The index
  cache has a new column: delete `cache/index.sqlite`, and the next call folds it anew
- **`ossuary find --all` asks the record.** It used to ask the standing set for every file held, at
  a place or not; it now asks every claim ever written, retractions included, on every file held. A
  value taken back answers again under `--all`, and a shown pair is a value once said, standing or
  not; `about` tells which

### Added

- **`ossuary history`** lists every run on the record, oldest first: the moment it closed, spelled
  the way `--as-of` takes it, its id, how many files and claims it wrote, how many of them took
  something back, and who spoke in it. `--json` answers one object per run, `--as-of` narrows it to
  the runs closed by then
- **Field terms in `ossuary find`.** A term whose name has no colon names a field of the claim —
  `subject`, `attribute`, `value`, `time`, `source`, `run`, `retract` — and asks about the claim
  behind a value, for every attribute term at once: `find run=ID file:name` is what a run named,
  `find source=user user:tag` what you tagged yourself, `find time=2026-09-01..` what was written
  since September, `find --all retract=true file:path` what was ever taken back, the paths shown.
  Patterns read like attribute patterns, in the field's own spelling. A field term shows nothing of
  itself; a bare field name shows the field's values on each match. Under the standing set a value
  said twice answers for the run that said it last; `retract` is refused without `--all`, because a
  retraction never stands
- **`--as-of RUN`** wherever `--as-of` stands: a run id in place of the time closes the view after
  that run's last claim, so two runs within one second still come apart

### Changed

- **`ossuary about`** ends each line with the claim's run, `[ingest]  run 315e…`; a claim from
  before runs ends with its source as before. `--json` carries the field as the log spells it
- **Every writing verb names its run.** `extract` closes with `2 file(s) examined in all, 1 derived
  file(s) taken in; run …` whenever it examined something, where it used to name the run only when
  files were derived; `annotate` and `retract` close with `…, run …` too
- **`export RUN` and `extract RUN`** read the run field: a run's files are the ones whose place or
  name it wrote, exactly as before for runs of this version. A run from before this version is not
  found by them; `find --id prov:run=ID | xargs ossuary export DIR` still is
- **The index cache's view of runs** is `v_runs`, one row per run with first and last moment, file
  and claim counts and retractions, where `v_arrivals` counted `prov:run` claims

## [0.5.0] - 2026-09-18

### Changed

- **`ossuary ingest` notices what is gone.** A file no longer at a place the record stands by, under a
  directory the run walked, has that `file:path` taken back: one retraction under the run's own
  source, nothing erased. The bytes stay, everything else known about the file stays, and
  `--as-of` before the run still shows it where it was. What the walk did not cover is not judged:
  a directory that would not open, a path config.toml excludes, a root named as a single file, a
  place last seen on another host, and a directory the walk meets not one file under while places
  stand on record there, which is what a mount point looks like with nothing mounted. `--dry-run`
  counts what would be taken back. The verdict reads `2 no longer at their place, taken off the
  record`, or `1 directory met empty, nothing judged there`
- **`ossuary find` answers with the present.** Only files still lying somewhere answer: a
  `file:path` or `mailbox:place` standing on them, or, for what a tool won out of another file, on
  their origin. A file gone from every place it was seen at is still held and still answers
  `--as-of` a day it lay there, but not a question about today

### Added

- **`ossuary find --all`** asks for every file the archive holds, at a place or not
- **`ossuary ingest --collect`** takes in what is there and judges nothing gone: for a directory
  that is emptied after every run, an inbox, whose files are meant to live on in the archive
- **`ossuary ingest --emptied`** is the word that the named directories were emptied on purpose:
  every place on record under them is taken back, however little the walk meets

- **`ossuary maintain weed`** takes out of `derived/` what `content/` holds as well: a file won as
  an attachment first and taken in as an original later stands in both stores under the same name,
  and only the original ever answers. Both copies are read whole and proved against their name
  before anything goes; a damaged derived copy of a sound original goes too. Where the original is
  the damaged one and the derived copy sound, the file stays and says so, and `--repair` sets the
  damaged original aside under its `.corrupt` name and stores the sound bytes in its place. Damaged
  in both stores, or unreadable, it stays. `--dry-run` says what would go; `--verbose` names every
  file taken out. Exits 1 when a file was left standing
- **`ossuary audit` notes files held by both stores**, not as a finding, with the way to `maintain
  weed`, and says how many of them have a damaged original beside a sound derived copy. `--json`
  answers each as `{"observation":"twin", …}` with how both copies fared

## [0.4.1] - 2026-09-18

### Changed

- **Verdicts read plainer.** `ossuary ingest` closes with `5 file(s) stored; 35 claim(s) written, run
  …`, a repeated run with `0 file(s) stored, 5 unchanged since the last run; 0 claims written`, and
  `ossuary mailvault` counts `stored`, `already stored` and `already on record` the same way; an
  archive met on the walk reads `ossuary archive, skipped`

## [0.4.0] - 2026-09-17

### Changed

- **The receipt names who looked.** A `prov:examined` claim's value is now the extractor's source,
  `"extractor:image-exif/1"`, where it was `true`; the claim's own source says the same word. The
  worklist, `--full`'s list and the one-file check read the standing set for it, so a retracted
  `file:mime` takes a file off an extractor's list and a retracted receipt puts it back, which
  neither did before. Receipts reading `true` no longer count: the next `ossuary extract` examines
  every file once more, and thereafter only when a generation is raised
- **The index cache has a new shape.** Subjects, attributes, sources and segments now stand in
  tables of their own and appear in the claim history and the standing set as integer ids; the
  standing set carries each value's newest moment, so a mount without `--as-of` reads it instead of
  replaying the history; and the connection waits up to five seconds for a fold running beside it
  instead of failing at once. Four views are for a look with `sqlite3`: `v_claims` and `v_standing` show both
  tables with names in place of the ids, `v_places` every standing `file:path` bare, `v_arrivals` what
  each run took in. The cache is a cache: delete `cache/index.sqlite` before the first
  call of this version, and the next call folds it anew. An old file left in place fails with an
  SQLite error naming a missing column, which is the same message

- **`ossuary extract` names the file an extractor speaks about.** What an extractor writes on
  stderr â the PDF extractor's "mostly not text, discarded", say â used to pass through bare, and
  no reader could tell which file it meant. Every line now comes with the file's digest and name in
  front, the program's own name dropped where it prefixed the line; `-q` silences it like the rest
  of the narration. When an extractor gives up on a file, what it said is the reason in the
  failure list

### Fixed

- **`find` with a single term** named a file once per standing value that matched, so a file
  under two names ending in `.jpg` answered twice and was counted twice. Every file answers once

## [0.3.0] - 2026-09-16

### Added

- **`raster` contract of the image extractor** â what a file's header says about its pixel grid,
  as numbers to search by: `raster:width` and `raster:height` in pixels, `raster:depth` in bits
  per channel, `raster:alpha`, and `raster:color` (`gray`, `rgb`, `cmyk`, `indexed`). Read from the
  header alone, no pixel decoded, for JPEG, PNG, TIFF and WebP; `ossuary extract image:raster` runs
  it on its own. See [the vocabulary](docs/vocabulary.md)
- **`ossuary maintain mend`** â the first verb of a new family, repairs that add to the archive and
  rewrite nothing. Where the audit finds the chain of sealed segments in pieces, this closes each
  break with a mend: a segment of no claims that names the two ends it joins, stored like any
  other. Nothing already sealed is touched, and the segment after the break still names what it
  named, so the record keeps saying what was lost and where. A break behind a segment that is
  held but damaged is left open, and the run exits 1. `--dry-run` says what would be mended
- **Mends in the segment format** â a header member `mend`, with `before` (the segment the mend
  stands in front of) and `replaces` (the lost segment's name, when known); see
  [`docs/format.md`](docs/format.md). Generation 1 gains a member, as it was allowed to
- **Library: `Log::mend`, `Log::head_follows`, `mend`** â store a mend; make the open head name a
  segment; close one `Break` the audit reported
- **`ossuary attributes`** â every attribute standing on the record, sorted, one per line: the
  words a question can be asked in, bare so they paste into `find`. Namespaces like `exif:`
  narrow the answer, `--count` puts the number of files each attribute stands on in front of it,
  `--json` answers one object per attribute, `--as-of TIME` answers for a moment past. An
  attribute every value of which was retracted is not among the words, since no `find` could
  reach it
- **Library: `Index::attributes`** â every standing attribute with the number of subjects it
  stands on

### Changed

- **Extractors say their generation, not their release.** The number in an extractor's source,
  `extractor:mail/1` where it read `extractor:mail/0.2.0`, is now raised by hand and only when the
  extractor sees more or differently than before; it no longer moves with each ossuary release,
  which had every release write the whole extractor record anew. The next `ossuary extract`
  examines every file once more under the new sources, and thereafter only when a generation is
  raised. Every claim already written stands as it was said
- **`ossuary-extract-exif` is now `ossuary-extract-image`**, a program of two contracts: `exif`,
  which is the old extractor unchanged, and `raster`. `ossuary extract image` runs both,
  `ossuary extract image:exif` the one; a config naming `exif` under `[extract] run` names `image`
  now. The EXIF source reads `extractor:image-exif/1`, so the next `ossuary extract` examines every
  image once more
- **`ossuary audit`** now names a broken chain for what it is. A segment naming no predecessor,
  beyond the one the archive begins with, stands where the open head was lost and begun anew, and
  the lost head's claims went with it: a finding (`head-lost` under `--json`), where it was an
  observation before. The answer lists every piece of the chain with its ends, its claim count
  and its span, and each break with what the record says of it and what to do. A break a mend
  has closed is noted, not counted (`mended` under `--json`), and so is a mend whose loss was
  made good since, the segment restored from a copy (`restored`); a chain that runs in a circle
  is a finding (`chain-loop`). The `unchained` observation is gone
- **Library: `LogAudit`** answers `chains`, `breaks`, `mended`, `restored`, `idle_mends` and
  `looped` in place of `unchained`; `predecessor_missing` leaves out what a mend stands in for

## [0.2.0] - 2026-09-11

### Added

- **Segment chain** â from the second segment of an archive on, a segment's header names the
  one sealed before it (`previous`, its digest), so the record hangs together from the open head
  back to the first segment
- **`ossuary audit`** follows the chain: a segment named as sealed before another, or as the last
  one sealed before the open head, that no store holds is a finding â a sealed segment is gone
  (`missing-segment` under `--json`). More than one segment naming no predecessor is noted, not
  counted (`unchained` under `--json`)
- **`ossuary mailvault`** â a new outside verb, [`ossuary-mailvault`](ossuary-mailvault/README.md):
  whole mailboxes fetched over IMAP into the archive. The mailboxes stand in `mailvault.toml` in
  the archive root, one `[[account]]` table each (`password_cmd` runs only under `--allow-exec`);
  naming accounts fetches those alone. Every message goes in once, however many folders carry it,
  with the place it was seen in as a `mailbox:place` claim â account and folder, one value. Each
  folder's fetch carries on where the last one left off: its UIDVALIDITY and highest fetched UID
  are remembered in `cache/`, and the server is asked only for what lies above; `--full` fetches
  every folder whole, `--dry-run` says what a run would fetch and writes nothing
- **`ossuary mailvault --from-vault DIR`** takes an archive of the Python mailvault over whole:
  every message with every mailbox and folder its log saw it in, and the date the log first saw
  it there, held against its own name on the way. Interrupted, the next call carries on where it
  left off
- **`mailbox:` namespace** in the [vocabulary](docs/vocabulary.md): `mailbox:place`, where a
  message was seen; `mailbox:seen`, when a takeover's vault saw it there
- **Library: `admit` and `record`** â the two steps through which content enters an ossuary
  archive: `admit` streams bytes into the content store and answers what it saw (subject, whether
  new, size, sniffed kind); `record` puts an admission on the record with the taker's
  `Sighting` â its facts, the run, the user's tags â and the day-one facts. `ingest` goes through
  both now. A taker that knows what it holds says the kind itself, on every sighting, so a message
  first taken in as a file and later fetched from its server is `message/rfc822` all the same

### Changed

- **Segment header** â a reader passes over header members it does not know; generation 1 may
  gain members that add to what a header says without a new generation. What a reader must
  understand to read the claims stays a matter for a new generation ([docs/format.md](docs/format.md)).
  0.1.0 refused every member it did not know, so a 0.1.0 build reads no archive this version has
  sealed in â the way back is this version, which reads 0.1.0 archives unchanged

## [0.1.0] - 2026-09-09

### Added

- **Archive format, generation 1** â the reading contract in [docs/format.md](docs/format.md):
  content, derived content and claims as three [immure](https://github.com/sniner/immure) stores
  ranked by what losing them costs â what was taken in and what tools made of it never mingle â
  an append-only claim log in sealed segments, disposable caches, and recovery with shell
  tools alone
- **Attribute vocabulary** â [docs/vocabulary.md](docs/vocabulary.md): what each attribute means
  and how standing claims become an answer â every attribute is a set of standing values,
  deduplicated at fold time; narrowing to one value is the reader's own policy; extractors
  record verbatim, interpretation stays at query time â and when an examiner has something to
  say that its harvest cannot, one sentence in its own words goes on the record as `prov:note`
- **`ossuary init`** begins an archive â or completes one already standing: nothing standing is
  remade or edited, and what is missing (today: `config.toml`) is added
- **`config.toml`** in the archive root: `[ingest] exclude` glob patterns (`.DS_Store` and
  friends never go in; an excluded directory is not walked), `[store] compress` (zstd for new
  entries, in the content and derived stores alike â what one holds is as sensitive as the
  other; what is already stored keeps its form, and reading understands both) and
  `[extract] run` (the extractors a bare `ossuary extract` runs, in order). A missing
  file means the defaults; an unknown key is refused rather than half-applied
- **`ossuary ingest PATHâ¦`** takes directory trees and single files in, any mix, several per
  call â a glob's expansion included â all under one run id, so "arrived together" stays an
  askable fact; a path that will not resolve is named in the verdict and costs only itself.
  Every regular file goes
  into the content store, seven day-one claims per new blob into the log â a blob met again gets
  its sighting only, and every place and name it sat under goes on the record; the mtime is
  recorded at the precision the filesystem observed it. `--tag TAG` (repeatable) says the
  user's own word at arrival: a `user:tag` claim under the source `user` on every file the run
  records. A repeated run remembers what
  it already observed (in `cache/`) and leaves unchanged files in peace: not read, not hashed,
  no claims â tags among them, and the verdict says so; `--full` looks at everything anew. An
  archive met on the walk is left whole and counted in the verdict, and naming one â or a path
  inside one â refuses the call: an archive never takes in an archive, its own least of all.
  `--dry-run` walks, counts and measures â same excludes, same memory â and writes nothing:
  "would take in 1,204 file(s), 3.7 GiB" is the answer a forgotten ISO shows up in
- **`ossuary seal`** closes the open segment; its claims become part of the sealed log. The open
  segment also closes itself once it grows to 1 MiB â a few thousand claims â no matter which
  command was writing; the command remains for sealing on demand, before a backup or right away
- **Per-segment manifests** in `cache/manifests/` â one small memo per sealed segment (claim
  count, time range, namespaces, a bloom filter over the subjects), filed at sealing time and
  rebuilt in passing when missing: commands that walk the log no longer read every sealed
  segment just to put them in order. Like every cache, deletable at any time â the next walk
  refills it
- **`ossuary about SUBJECT`** answers everything on the record about one file, oldest first; a
  beginning of the digest is enough while it names only one file, and naming attributes (or a
  namespace, like `exif:`) narrows the answer to them
- **`ossuary find TERMâ¦`** answers which files match: terms are `attribute=value` and must all
  hold, `*` and `?` match within text values, `low..high` asks for a value inside the range
  (either side open â `file:modified=2026-09-01..` is "changed since September"; bounds compare
  in the attribute's own spelling; a value in double quotes is literal), and
  `--missing ATTRIBUTE` (or a namespace like `exif:`) asks for what a file lacks. Only standing
  values count â a retracted value no longer answers. The question is also the projection: each
  match answers as a block â the file's name (shortened like a git hash, growing as the archive
  does) on a line of its own, the shown attributes indented beneath it as the
  `attribute=value` pairs a query would use â a pair pastes back into a refined query, quotes
  and all, and every standing value is shown. The filters show themselves until a bare
  attribute stands among the terms; then only the bare ones show â explicit beats implicit,
  and `find file:name=*.pdf file:modified` answers with the times alone. A namespace like
  `exif:` shows all of it, and with only bare attributes every file on the record answers. `--id` answers with the full names
  alone, one per line, ready to pipe; `--json` answers one JSON object per match with the
  values as lists
- **`ossuary ls [PLACE]`** and **`ossuary tree [PLACE]`** browse the record's places: every place
  a file was ever seen at and still stands answers â the record, not a disk, so every machine's
  paths stand in one forest and outlive the disks they were on. `ls` shows one level â folders
  with a trailing slash, one line per file with its short name first, the way `export --dry-run`
  speaks â and `--json` answers one object per entry, the names spelled in full. `tree` draws
  the whole subtree, short names bracketed beside the files. Without a PLACE the roots answer;
  one name may honestly carry several files, when different bytes stood there over time, and a
  retracted place no longer answers
- **`ossuary mount DIR`** grafts the whole forest onto a directory, read-only â the archive's
  files readable in place by any program, browsable in a file manager. The command stays in the
  foreground and Ctrl-C gives the directory back; `umount` and the Finder's eject work too. A
  mountpoint the command created goes with the mount when it ends. A door for each platform,
  neither asking for root nor installing anything kernel-side: on macOS the room is served over
  NFS on 127.0.0.1 and mounted by the system's own client, on Linux it is a FUSE filesystem
  mounted through `fusermount3` from the fuse3 package every distribution ships.
  Where the record says more than a filesystem can, the view narrows by declared policy: of
  several files standing at one name the newest wins, and a name standing as file and folder at
  once keeps the folder, the file stepping aside under a name carrying its digest.
  `--as-of TIME` shows the record as it was known at that moment (UTC): files since retracted
  stand again, files since arrived are absent, and a place whose file changed shows the old
  bytes â mount today and last year side by side and compare
- **Outside verbs**: an unrecognised command looks for its own program â `ossuary mount â¦` runs
  `ossuary-mount â¦` from the `PATH`, the resolved archive travelling as `OSSUARY_ARCHIVE` in
  the environment and the rest of the line passed through word for word â so heavier tools
  arrive without weighing `ossuary` itself down
- **`ossuary annotate SUBJECTâ¦ --comment TEXT --tag TAG`** puts the user's own word on files
  already on the record: each comment and tag becomes a claim (`user:comment`, `user:tag`)
  under the source `user`, on every named file â beside what `ingest --tag` said at arrival.
  Both options repeat, several files go in one call, and every name is resolved before
  anything is written, so a mistyped name refuses the whole call.
  `ossuary find --id â¦ | xargs ossuary annotate --tag â¦` is the after-the-fact batch tagging
- **`ossuary retract TARGETâ¦`** takes a statement back: what it names no longer stands, and the
  record keeps the whole story â the retraction is one more claim, under the source `user`,
  never an edit. Targets mix freely, told apart by shape: a hex name (or a beginning) names a
  file, `attribute=value` names what to take back on every named file, `attribute=..` takes
  back every standing value of the attribute. A pair speaks the answers' own language â a line
  from `standing` pastes back â and is taken literally: double quotes mean the characters
  themselves, globs and ranges are refused, and `find --id â¦ | xargs ossuary retract` takes
  back across a found set. Everything resolves before anything is written: a pair standing on
  none of the named files refuses the whole call. Anything on the record can be taken back,
  the machines' word included â a later `extract --full` or a new extractor version may
  honestly assert it again; the user's own word returns with `annotate`. `--as-of` still
  answers for the day before, and exporting a whole run keeps speaking what the run recorded.
  `--dry-run` shows what would fall, and writes nothing
- **`ossuary standing SUBJECT [ATTRIBUTEâ¦]`** answers what currently stands on one file â
  retractions applied, repeats collapsed â where `about` answers with everything ever said.
  Without attributes the whole standing comes, one `attribute=value` pair per line in the query
  spelling, so a pair pastes into `find` as a term; attributes narrow it, `exif:` names a whole
  namespace, and exactly one attribute answers bare â the values alone, one per line, strings
  without quotes, ready for a script. Several lines mean the attribute honestly holds several
  values, and choosing among them stays the caller's policy. Exits 1 when nothing stands, so a
  script can test for it
- **`--json`** on `about` and `standing` answers ready for `jq`: `about --json` gives each claim
  exactly as the log spells it, `standing --json` one object with every shown attribute's
  values as a list
- **`--archive DIR`** on every command names the archive to work in; standing in it is enough,
  and so is the environment â `OSSUARY_ARCHIVE` holds the name for a whole shell session, the
  flag still outranking it
- **`--quiet`** on every command keeps the run's narration off stderr â counts, progress, hints;
  answers and errors still come
- **`ossuary get SUBJECT`** hands a file's bytes back, to stdout or `--output FILE`; short
  digests resolve against the content and derived stores, from six characters up
- **`ossuary export PATH IDâ¦`** lays files back out of the archive, as they arrived: each ID is
  one file (its hex name, or a beginning) or a whole run â the id an ingest or extract verdict
  names, spelled out whole â and both kinds mix in one call. A run's files land under the paths
  that run recorded, kept relative: the folders all the exported files share are trimmed away,
  unrelated places become sibling folders under PATH, and what lay side by side lands side by
  side. A file named alone lands at every place still standing on its record â no place is
  preferred over another, and a retracted one no longer counts; a derived file, which never sat
  anywhere, flat under its recorded names; the same bytes standing at two places come out as two
  files, the way they stand. Nothing standing at PATH is ever overwritten â a
  file already there with the same bytes counts as done, different bytes are a named failure â
  and `--dry-run` answers what would land where without writing anything. A destination inside
  the archive is refused â exports land outside it.
  `ossuary find --id â¦ | xargs ossuary export DIR` exports a found set
- **`--as-of TIME`** on `find`, `ls`, `tree`, `standing`, `about` and `export`: the same
  question, answered with what the archive knew at TIME â claims recorded later sit out,
  assertions and retractions alike. The axis is claim time, never the file's own: a file
  changed in March and taken in come June appears in the June view. RFC 3339 UTC; a date
  alone closes at that day's end â "as of the first" means the first has happened. A re-ingested
  file's earlier bytes answer at its path, because the newest claim before TIME still carries
  the old digest
- **`ossuary audit`** proves the archive intact from its own files alone â the cache plays no
  part. Every file in the content and derived stores is read back whole and its bytes re-hashed
  against its name; every sealed segment and the open head must read back claim by claim; and
  every file the claims speak of â as a claim's subject, or named as what a derived file came
  from â must be held by a store, because nothing is ever deliberately removed from an archive.
  Files held that no claim speaks of are noted, not counted as findings: an interrupted run
  leaves such files, and the next arrival records them. The answer counts what it finds, up to a
  handful of names spelled out right there; `--json` answers one object per finding, ready for a
  script, and a sound archive answers an empty stream. Exits 0 when the archive is sound, 1 when
  findings stand
- **`--verbose`** on every command: answers in full, every name spelled out where a count would
  stand â today `audit` is the one command with more to say
- **`ossuary id FILE`** names a file the way the archive would â hashed from its bytes, the file
  only read â and says whether the archive already holds it; works before an ingest as well as
  after
- **`ossuary extract [NAME]`** runs one extractor â its own program, `ossuary-extract-NAME`
  found on PATH â or, with no NAME, the archive's own list from `[extract] run`, in rounds
  until a whole round finds nothing left to examine: what one extractor hands back, the next
  round offers to whichever extractor reads it, so a single call drives a chain like
  mail â attachment â text to its end (list order costs at most an extra round, and a call cut
  short continues next time â the worklist is refolded from log and receipts, not queued). A
  listed extractor that cannot answer `--identify` is skipped with its reason while the healthy
  ones still run. Each goes over every file of a kind it reads that it has not examined yet:
  findings go on the record under the extractor's name, and every examined file gets a receipt,
  found something or not, so a repeated run costs only what is new and a new extractor version
  looks at everything again. Every derived file taken in is stamped `prov:run` with the call's
  own run id â the same anchor an ingest run leaves â so "won together" stays an askable fact. An extractor can hand back files as well as findings â an unpacked attachment,
  extracted text â and each goes into the archive's derived store as content of its own, named
  and typed in the extractor's words (`file:name`, `file:mime`) and tied to its origin
  (`derive:derived-from`) â apart from the originals, so nothing that ever tidies derived
  content can reach what was taken in. Bytes the archive already holds as an ingested
  original â the attachment that was also saved as a file â are recorded without a second
  copy: a digest is store-agnostic, and the bytes answer from the content store;
  `--temp-dir` says where derived files wait on their way in, for when the archive sits on a
  slow share. The closing line names the call's run id whenever files were derived, so the
  batch is one paste away from `export` or another `extract`. Naming files narrows the run to
  them â `extract pdf SUBJECT` examines one file
  now instead of everything that waits, a beginning of the digest is enough, and a named file
  is handed over even when its kind is not one the extractor reads; a whole run's files are
  named by its dashed id, runs and files mixed freely â the grammar `export` speaks.
  `--dry-run` runs the extractor over the named files and shows what it would record â the
  claims, and each derived file with name, kind and size â then drops it all, receipt
  included: nothing written. It demands named files, because a rehearsal over everything that
  waits would examine the whole archive and keep none of it. `--full` examines anew,
  receipted or not: the named files, or everything of a kind the extractor reads â for the
  extractor upgrade that is worth a fresh look at the archive. The pipe protocol, open to any
  language, is [docs/extractors.md](docs/extractors.md)
- **Extractor contracts** â one program may carry several separately receipted capabilities:
  `--identify` answers one JSON line per contract, each with its own name, source, MIME list and
  `derives` flag, and each runs over its own worklist onto its own receipts. `ossuary extract
  NAME` runs every contract the program offers; `ossuary extract NAME:CONTRACT` runs one, and
  the same spelling holds in `[extract] run` â so "inventory the archives, never unpack them"
  can stand in `config.toml` as `"packed:list"`. Single-contract extractors are untouched: one
  identify line, spoken to exactly as before
- **`ossuary-extract-exif`** â the first extractor: EXIF fields verbatim, tag names kebab-cased
  under `exif:`, values as the format stores them (`"2019:07:14 11:02:41"`, `"28/10"`)
- **`ossuary-extract-pdf`** â the first deriving extractor: a PDF's plain text, extracted
  through poppler's `pdftotext` (which must be on PATH), goes into the archive as a `text/plain`
  file of its own beside the document information verbatim under `pdf:` (`pdf:title`,
  `pdf:creation-date` â dates as the document spells them). A document with no text to give â
  scanned pages, an unreadable file â is examined all the same, with nothing found; a harvest
  that is mostly not text at all â glyph numbers from fonts that do not say what they spell â
  is discarded rather than archived as noise, and the reason goes on the record as a
  `prov:note`
- **`ossuary-extract-mail`** â the mail extractor: a message's own headers verbatim under
  `mail:` (`mail:from`, `mail:subject`, `mail:date` â unfolded and their encoded words decoded,
  otherwise as the mail spells them; the transport's trail stays untold), and what the mail
  carries handed over as content of its own â named attachments and nested messages, each typed
  as the mail declared it, an attachment's content-id on its record. It reads `text/plain`
  deliberately, because a mail on disk sniffs as plain text: bytes that are no message are
  examined with nothing found, and a recognized mail gains the sharper `file:mime` of
  `message/rfc822` beside the sniffed word. An mbox is a mailbox, not a message: it stays
  shut, but gains its own kind, `application/mbox`, the same way
- **`ossuary-extract-packed`** â the archive extractor, and the first program of two contracts:
  `list` inventories a zip without unpacking a byte â one `zip:entry` claim per entry, standing
  on the archive itself, so "which zip holds a file so named" becomes a question the record
  answers â and `unpack` hands every entry over as content of its own, flattened to its bare
  name (colliding names yield to a counter, the spelled name kept as `file:name`), its place
  inside the zip on the record as `zip:path`, its kind sniffed the way ingest sniffs, since a
  zip declares none. A zip that is really a document wearing zip as its envelope â epub and the
  OpenDocument family by their `mimetype` first entry, Word, Excel and PowerPoint by their type
  manifest â is recognized from the bytes, left shut, and gains its sharper `file:mime` instead;
  a jar promises nothing about its insides and is treated as the archive it is. Encrypted or
  damaged entries stay inside â there is no password to offer â and each says why as a
  `prov:note` on the record, so a zip that unpacked incompletely never reads like one that
  unpacked whole; bytes that do not read as a zip are examined with nothing found
