# ossuary-cli

*The `ossuary` command.*

All work with an archive goes through this command: creating it, adding
files, running extractors, querying what is recorded, and getting files
back out. This crate handles argument parsing, output and exit codes;
the archive logic is in [`ossuary-core`](../ossuary-core/README.md).

```console
$ cargo build --release      # target/release/ossuary
```

## Selecting the archive

Every command takes `--archive DIR`. Without it, `OSSUARY_ARCHIVE` is
used if set, otherwise the current directory. Two more options are
global: `-q`/`--quiet` prints only results and errors, `-v`/`--verbose`
lists every item where the output would otherwise show only a count.

Results go to stdout, progress messages to stderr, so `ossuary find --id
… | …` pipes only the results. If the reading end of a pipe is closed
early, the output stops and the command still exits with success.

## Commands

| | |
|---|---|
| `init` | create an empty archive, or add missing files to an existing one. `--algorithm` sets the hash algorithm; it can only be chosen when the archive is created |
| `ingest` | add directory trees and single files; all files of one call form one run. Recorded paths under a named directory whose file no longer exists are retracted. `--tag` tags every file the run records, `--full` reads every file again, `--collect` retracts nothing (for an inbox that is emptied after every run), `--emptied` retracts the recorded paths under a directory that was emptied on purpose or removed, `--dry-run` counts what would be added and retracted, and writes nothing |
| `extract` | run extractors on files they have not examined yet, see below. `--dry-run` shows what would be recorded for the named files, and writes nothing |
| `annotate` | add `user:tag` and `user:comment` to recorded files |
| `retract` | retract values: they are no longer standing, and the record keeps them. Files and `attribute=value` pairs can be mixed; `attribute=..` retracts all standing values of the attribute; `--dry-run` shows what would be retracted |
| `seal` | close the open segment; its claims become part of the sealed log |
| `about` | all claims about one file, oldest first; attributes or a `namespace:` restrict the output |
| `standing` | the standing values of one file, with retractions applied, where `about` shows every claim. Attributes or a `namespace:` restrict the output; with exactly one attribute, only its values are printed, one per line. Exits 1 if there is no standing value, so a script can test for it |
| `find` | files that match all terms, shown with the attributes the query selects. A name without a colon is a field of the claim, such as `run`, `source`, `time` or `retract`. Only files that are still present are listed; `--all` searches every claim ever written, retractions included; `--as-of TIME` searches what was recorded up to TIME |
| `attributes` | all attributes with standing values, sorted, one per line: the attribute names a query can use. Namespaces like `exif:` restrict the list; `--count` prefixes each attribute with the number of files that have it |
| `history` | all runs, oldest first: when each run ended, its id, what it wrote, and the sources of its claims. These are the times `--as-of` accepts and the ids `export`, `extract` and `--as-of` take |
| `ls`, `tree` | the recorded files and folders at a path, one level (`ls`) or everything below it (`tree`) |
| `id` | the name a file would have in the archive, and whether the archive already contains it. Nothing is added |
| `get` | one file's content to stdout, or to `--output FILE` |
| `export` | copy files out of the archive under their recorded paths: runs by id, single files by name, in any mix. `--dry-run` shows where each file would be written |
| `audit` | check the archive: every file against its hash, every claim readable, no segment or referenced file missing. Exits 1 if there are findings |
| `maintain mend` | join a broken chain of sealed segments and record each break. Sealed segments are not rewritten; `--dry-run` shows what would be mended. Exits 1 if a break was left open |
| `maintain weed` | remove files from `derived/` that `content/` also contains, after checking both copies against their hash. `--repair` replaces a damaged file in `content/` with its sound copy from `derived/` and keeps the damaged one aside; `--dry-run` shows what would be removed. Exits 1 if a file was left in both |

`--json` on `about`, `standing`, `find`, `attributes`, `history`, `ls`
and `audit` prints JSON, one object per line, for `jq`.
`--as-of TIME` on `find`, `attributes`, `history`, `ls`, `tree`,
`standing`, `about` and `export` uses only what was recorded up to TIME.
TIME is the time a claim was written, not a time of the file itself; a
date alone means the end of that day. A run id in place of a time means
the end of that run.

Files are named by the hex digest of their content. Wherever a name is
expected, a prefix is enough as long as it matches only one file.

### Ingesting

For each file, `ingest` records its path, name, host, size, MIME type
and modification time (`file:path`, `file:name`, `prov:host`,
`file:size`, `file:mime`, `file:modified`). Files unchanged since the
last run on the same host are skipped, so a repeated ingest of the same
directory reads only new and changed files; `--full` reads everything
again. A tag given with `--tag` is added only to the files the run
records, so files skipped as unchanged are not tagged unless `--full`
is given. `--dry-run` also prints the total size, which shows a large
file included by mistake before it is hashed.

A recorded path under a named directory whose file no longer exists is
retracted. The file's content and its other claims are kept, and
`--as-of` a time before the run still shows the path. No paths are
retracted

- under a directory that could not be read,
- under a path that `config.toml` excludes,
- for a path named as a single file,
- under a named directory in which no file was found at all, as with a
  mount point with nothing mounted. `ingest` prints a note for each
  such directory.

`--emptied` retracts the recorded paths under the named directories
even if a directory is empty or no longer exists. `--collect` retracts
nothing; it is meant for a directory that is emptied after every run,
such as an inbox, whose files are to be kept in the archive.

### Querying

`find` takes `attribute=value` terms, and a file must match all of them.
The terms also select what is shown: the attributes of the terms, unless
a bare attribute is among the terms; then only the bare attributes are
shown. `*` and `?` are wildcards in text values, `low..high` matches a
value in a range with either end open, a bare `..` matches any value
(the attribute must be present), and a value in double quotes is
literal, without wildcards or range. `--missing exif:` inverts the
query: files without EXIF data. Only standing values are searched;
retracted values are ignored.

A name without a colon is a field of the claim: `subject`, `attribute`,
`value`, `time`, `source`, `run`, `retract`. A field term applies to the
claims behind all attribute terms: `find run=RUN file:name` lists the
files a run recorded, `find source=user user:tag` the tags you set
yourself, `find time=2026-09-01..` what was recorded since September (a
date covers the whole day, as with `--as-of`), and `find --all
retract=true file:path=*` the retracted paths, where `retract=true
file:path` finds any retraction and shows the paths. A field term shows
nothing itself; a bare field name shows the field. `--all` searches
every claim ever written instead of the standing values, retractions
included, and is required for the `retract` field.

For each value, a field term uses the claim that recorded the value
last. Claims written before runs were recorded have no run. Two range
terms on the same attribute may be matched by two different values; a
single `low..high` term must be matched by one value.

`--id` prints only the full names, for piping into `about`, `get`,
`annotate` or `export`.

`attributes` lists the names a query can use: every attribute with
standing values, sorted, one per line, without values. `find` takes
these names to select what is shown, so `ossuary find $(ossuary
attributes mail:)` shows everything recorded about mail. Namespaces
restrict the list; `--count` prefixes each attribute with the number of
files that have it, as `uniq -c` does, so `sort -rn` ranks them. An
attribute whose values were all retracted is not listed, since `find`
cannot match it.

### Extracting

`ossuary extract NAME` runs `ossuary-extract-NAME` from the PATH with
every contract that program offers; `NAME:CONTRACT` runs one of them.
The archive's `[extract] run` list in `config.toml` uses the same form.
A bare `ossuary extract` runs that list in rounds until a round finds
nothing new to examine, so a chain like mail → attachment → text is
processed in one call. Every examined file is recorded as examined
(`prov:examined`), whether anything was found or not, so a repeated
call examines only new files, and an interrupted call continues where
it stopped. A new extractor version examines all files again.

Naming files runs a single round: the named files are examined once,
and a named file is passed to the extractor even if it is not of a type
the extractor reads. A run id selects every file of that run; runs and
files can be mixed, as with `export`. The last line prints the run id
of the call, which is recorded with every claim it wrote. `--dry-run`
shows what would be recorded for the named files (claims, and each
derived file with name, type and size) and writes nothing, not even the
record that a file was examined. `--full` examines files again even if they were
examined before. `--temp-dir` sets the directory where derived files
are kept until they are stored, instead of `cache/tmp` in the archive;
a local disk can be faster when the archive is on a network share.

The extractors that ship with ossuary:
[image](../ossuary-extract-image/README.md),
[mail](../ossuary-extract-mail/README.md),
[packed](../ossuary-extract-packed/README.md),
[pdf](../ossuary-extract-pdf/README.md).

## External commands

A command that `ossuary` does not know is run as a separate program
from the PATH: `ossuary mount ~/view` runs `ossuary-mount ~/view`. The
program replaces the `ossuary` process, so signals and the exit code
pass through unchanged. It receives the archive as an absolute path in
`OSSUARY_ARCHIVE`, however the archive was given.
[`ossuary-mount`](../ossuary-mount/README.md) and
[`ossuary-mailvault`](../ossuary-mailvault/README.md) are such programs.

## Exit codes

`0` means success, `1` everything else: an archive that cannot be
opened, a name that matches no file or more than one, a file an
extractor could not examine, an audit with findings, `standing` with no
standing value. Errors are printed on stderr, also with `-q`.

## License

Apache License 2.0, see [LICENSE](../LICENSE).
