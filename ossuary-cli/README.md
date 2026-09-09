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
| `ingest` | take directory trees and single files in; everything of one call arrives as one run. `--tag` puts the user's word on the whole batch, `--full` looks at every file anew |
| `extract` | run extractors over what they have not examined — see below |
| `annotate` | put `user:tag` and `user:comment` on files already on the record |
| `seal` | close the open segment; its claims become part of the sealed log |
| `about` | the whole record of one file, oldest first; naming attributes or a `namespace:` narrows it |
| `standing` | what stands on one file — the outcome after retractions, where `about` tells the story. Attributes or a `namespace:` narrow it; exactly one attribute answers its values bare, one per line. Exits 1 when nothing stands, so a script can test for it |
| `find` | every file on which all the terms hold, shown with the fields the question named |
| `ls`, `tree` | what stands at one place, one level of it — or everything below it |
| `id` | the name a file would answer to, and whether the archive already holds it. Nothing is taken in |
| `get` | one file's bytes to stdout, or to `--output FILE` |
| `export` | files back out as they arrived: whole runs by id, single files by name, mixed freely. `--dry-run` says what would land where |
| `audit` | prove the archive intact: every byte against its name, the record against the stores. Exits 1 when findings stand |

`--json` on `about`, `standing`, `find`, `ls` and `audit` keeps the JSON
spelling, one object per line, for `jq`.

Files are named by the hex digest of their content. A beginning of it is
enough wherever a name is asked for, as long as it names only one file.

### Asking

`find` takes `attribute=value` terms that must all hold, and the question
is also the projection: every attribute the query names is shown on each
match. `*` and `?` match within text values, `low..high` asks for a value
in a range with either side open, a bare `..` asks only that the
attribute stands at all, and a value in double quotes is literal — no
glob, no range. `--missing exif:` turns the question around: which files
have no EXIF on record. Only standing values answer; what was retracted
no longer counts.

`--id` prints the full names alone, ready to pipe into `about`, `get`,
`annotate` or `export`.

### Extracting

`ossuary extract NAME` runs `ossuary-extract-NAME` from the PATH and
every contract that program offers; `NAME:CONTRACT` runs one of them, and
the same spelling holds in the archive's `[extract] run` list. A bare
`ossuary extract` runs that list in rounds until a whole round examines
nothing new — so mail → attachment → text runs to its end in one call.
Naming subjects runs no rounds: the named files are examined once, now,
and a named file is handed over even when its kind is not one the
extractor reads. `--full` ignores standing receipts; `--temp-dir` moves
the place derived files wait in off `cache/tmp`.

The extractors that ship with ossuary:
[exif](../ossuary-extract-exif/README.md),
[mail](../ossuary-extract-mail/README.md),
[packed](../ossuary-extract-packed/README.md),
[pdf](../ossuary-extract-pdf/README.md).

## Outside verbs

A verb this command does not know is looked for on the PATH: `ossuary mount
~/view` becomes `ossuary-mount ~/view`. The child *becomes* this process —
signals and exit code included — and inherits the archive resolved: however it
was named, the child sees one absolute `OSSUARY_ARCHIVE` and resolves nothing
itself. That is how [`ossuary-mount`](../ossuary-mount/README.md) arrives
without weighing this crate down.

## Exit codes

`0` is the answer given, `1` everything else: an archive that will not
open, a name that matches nothing or too much, a file an extractor could
not examine, an audit with findings, `standing` with nothing standing.
Failures are named on stderr and survive `-q`.

## License

Apache License 2.0 — see [LICENSE](../LICENSE).
