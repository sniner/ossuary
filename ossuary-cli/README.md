# ossuary-cli

*The command line onto an archive — the `ossuary` binary.*

Everything a person does with an archive goes through this one command:
beginning it, taking files in, running extractors over them, asking what
is known, and getting the bytes back out. The crate is thin on purpose —
parsing, wording and exit codes live here, every decision about the
archive itself in [`ossuary-core`](../ossuary-core/README.md).

```console
$ cargo build --release      # target/release/ossuary
```

## Naming the archive

Every command takes `--archive DIR`. Standing inside the archive is
enough, and so is setting `OSSUARY_ARCHIVE` once for the shell session;
the flag wins over the environment, and `.` is the fallback. Two more
flags are global: `-q`/`--quiet` keeps the narration to itself and leaves
answers and errors alone, `-v`/`--verbose` spells out names where a count
would otherwise stand.

Answers go to stdout, narration to stderr — so `ossuary find --id … | …`
pipes the answer and nothing else. A closed pipe ends the answer without
making the run a failure.

## The verbs

| | |
|---|---|
| `init` | begin an empty archive — or complete one already standing. `--algorithm` is the one choice made for good, and only when the archive begins |
| `ingest` | take directory trees and single files in; everything of one call arrives as one run, and a file no longer at a place the record stands by has that place taken back. `--tag` puts the user's word on the whole batch, `--full` looks at every file anew, `--collect` judges nothing gone (for an inbox emptied after every run), `--emptied` takes back every place under a directory emptied on purpose or gone altogether, `--dry-run` counts and measures what would go in and what would be taken back, and writes nothing |
| `extract` | run extractors over what they have not examined — see below. `--dry-run` shows what named files' examination would record, and writes nothing |
| `annotate` | put `user:tag` and `user:comment` on files already on the record |
| `retract` | take a statement back: it no longer stands, the record keeps it. Files and `attribute=value` pairs mix freely; `attribute=..` takes back every standing value; `--dry-run` says what would fall |
| `seal` | close the open segment; its claims become part of the sealed log |
| `about` | the whole record of one file, oldest first; naming attributes or a `namespace:` narrows it |
| `standing` | what stands on one file — the outcome after retractions, where `about` tells the story. Attributes or a `namespace:` narrow it; exactly one attribute answers its values bare, one per line. Exits 1 when nothing stands, so a script can test for it |
| `find` | every file on which all the terms hold, shown with the fields the question named. A name without a colon is a field of the claim — `run`, `source`, `time`, `retract`. Only files still lying somewhere answer; `--all` asks the record, every claim ever written, retractions included; `--as-of TIME` a day's knowledge |
| `attributes` | every attribute standing on the record, sorted, one per line — the words a question can be asked in. Namespaces like `exif:` narrow it; `--count` puts the number of files each stands on in front |
| `history` | every run on the record, oldest first: when it closed, its id, what it wrote, who spoke in it — the moments `--as-of` can be asked for, and the ids `export`, `extract` and `--as-of` take |
| `ls`, `tree` | what stands at one place, one level of it — or everything below it |
| `id` | the name a file would answer to, and whether the archive already holds it. Nothing is taken in |
| `get` | one file's bytes to stdout, or to `--output FILE` |
| `export` | files back out as they arrived: whole runs by id, single files by name, mixed freely. `--dry-run` says what would land where |
| `audit` | prove the archive intact: every byte against its name, the record against the stores. Exits 1 when findings stand |
| `maintain mend` | join the pieces of a broken chain of sealed segments, and keep the break on the record. Nothing sealed is rewritten; `--dry-run` says what would be mended. Exits 1 when a break was left open |
| `maintain weed` | take out of `derived/` what `content/` holds as well, both copies proved against their name first. `--repair` mends a damaged original from its sound derived copy, the damaged one set aside; `--dry-run` says what would go. Exits 1 when a file was left standing |

`--json` on `about`, `standing`, `find`, `attributes`, `history`, `ls`
and `audit` keeps the JSON spelling, one object per line, for `jq`.
`--as-of TIME` on `find`, `attributes`, `history`, `ls`, `tree`,
`standing`, `about` and `export` answers with what the archive knew at
TIME — the axis is claim time, never the file's own, and a date alone
closes at that day's end. A run id in place of the time closes the view
after that run's last claim.

Files are named by the hex digest of their content. A beginning of it is
enough wherever a name is asked for, as long as it names only one file.

### Asking

`find` takes `attribute=value` terms that must all hold, and the question
is also the projection: the filters show themselves until a bare
attribute stands among the terms — then only the bare ones show,
explicit beats implicit. `*` and `?` match within text values, `low..high` asks for a value
in a range with either side open, a bare `..` asks only that the
attribute stands at all, and a value in double quotes is literal — no
glob, no range. `--missing exif:` turns the question around: which files
have no EXIF on record. Only standing values answer; what was retracted
no longer counts.

A name without a colon is a field of the claim itself — `subject`,
`attribute`, `value`, `time`, `source`, `run`, `retract` — and a field
term asks about the claim behind a value, for every attribute term at
once: `find run=RUN file:name` is what a run named, `find source=user
user:tag` what you tagged yourself, `find time=2026-09-01..` what was
written since September, a date read as `--as-of` reads it, the whole
day — and `retract=true file:path=*` a path that
was taken back, where `retract=true file:path` is any retraction with
the paths shown. A field term shows nothing of itself; a bare field
name shows the field. `--all` asks the record instead of the standing
set — every claim ever written, retractions included — and is the one
way to `retract=true`, what was ever taken back.

`--id` prints the full names alone, ready to pipe into `about`, `get`,
`annotate` or `export`.

`attributes` answers the question before the question: which words are
there to ask in. Every attribute standing on the record, sorted, one
per line and bare — the tokens `find` takes as a projection, so
`ossuary find $(ossuary attributes mail:)` shows everything known about
mail. Naming namespaces narrows the list to them; `--count` puts the
number of files each attribute stands on in front of it, the way
`uniq -c` speaks, so `sort -rn` ranks them. Only standing values speak:
an attribute every value of which was retracted is not among the words,
because no `find` could reach it.

### Extracting

`ossuary extract NAME` runs `ossuary-extract-NAME` from the PATH and
every contract that program offers; `NAME:CONTRACT` runs one of them, and
the same spelling holds in the archive's `[extract] run` list. A bare
`ossuary extract` runs that list in rounds until a whole round examines
nothing new — so mail → attachment → text runs to its end in one call.
Naming subjects runs no rounds: the named files are examined once, now,
and a named file is handed over even when its kind is not one the
extractor reads. A whole run's files are named by its dashed id — runs
and files mix freely, the grammar `export` speaks — and the closing line
names the call's own run id, which every claim it wrote carries. `--dry-run`
shows what the named files' examination would record — claims, and each
derived file with name, kind and size — and writes nothing, receipt
included. `--full` ignores standing receipts; `--temp-dir` moves the
place derived files wait in off `cache/tmp`.

The extractors that ship with ossuary:
[image](../ossuary-extract-image/README.md),
[mail](../ossuary-extract-mail/README.md),
[packed](../ossuary-extract-packed/README.md),
[pdf](../ossuary-extract-pdf/README.md).

## Outside verbs

A verb this command does not know is looked for on the PATH: `ossuary mount
~/view` becomes `ossuary-mount ~/view`. The child *becomes* this process —
signals and exit code included — and inherits the archive resolved: however it
was named, the child sees one absolute `OSSUARY_ARCHIVE` and resolves nothing
itself. That is how [`ossuary-mount`](../ossuary-mount/README.md) and
[`ossuary-mailvault`](../ossuary-mailvault/README.md) arrive without
weighing this crate down.

## Exit codes

`0` is the answer given, `1` everything else: an archive that will not
open, a name that matches nothing or too much, a file an extractor could
not examine, an audit with findings, `standing` with nothing standing.
Failures are named on stderr and survive `-q`.

## License

Apache License 2.0 — see [LICENSE](../LICENSE).
