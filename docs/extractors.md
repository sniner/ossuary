# The ossuary extractor protocol

*Protocol version 1, draft. An extractor is a separate program: it is
found on PATH, communicates over pipes, and never accesses the archive.
This document is the contract between `ossuary extract` and any
extractor, whoever wrote it and in whatever language.*

## The shape

The archive itself does not interpret content. That is done by small
separate programs, each reading one family of formats. An extractor:

- is a program named `ossuary-extract-<name>`, found on PATH, the way
  git finds `git-<name>`;
- reads bytes and outputs findings, and, when it produces files, writes
  derived files into a directory it is given; it never sees the archive
  itself. `ossuary extract` owns the stores and the log, passes each
  file's bytes to the extractor, adds time, source and subject to the
  output, and writes the claims;
- parses untrusted input, which is why it runs in a separate process: a
  crash costs one file and a line in the failure list, never the
  archive. Running an extractor means trusting its binary as much as
  any other program on PATH.

## Identify

Called as `ossuary-extract-<name> --identify`, an extractor writes one
line of JSON to stdout for each contract it offers, and exits 0. Most
programs offer exactly one:

```json
{"ossuary-extractor": 1, "source": "extractor:mail/1", "mimes": ["message/rfc822"], "derives": true}
```

A program may offer several contracts: capabilities with their own
name, their own generation and their own receipts, such as an archive
reader that can list the contents and can unpack:

```json
{"ossuary-extractor": 1, "contract": "list", "source": "extractor:packed-list/1", "mimes": ["application/zip"]}
{"ossuary-extractor": 1, "contract": "unpack", "source": "extractor:packed-unpack/1", "mimes": ["application/zip"], "derives": true}
```

- `ossuary-extractor`: the protocol version the extractor implements,
  on every line. A reader that does not know the number rejects the
  line instead of guessing.
- `contract`: the contract's name, with the same grammar as the part
  after `ossuary-extract-`: lowercase `a-z`, `0-9`, `-`. An extractor
  that outputs a single line may omit it; such a program has one
  contract and is called without a contract name. With several lines,
  each must name its contract, and no name may occur twice.
- `source`: the contract's identity, in the source grammar of the claim
  format: `extractor:name/generation`. Every claim the contract causes
  has this source, and the receipt with this source keeps a file from
  being examined twice. A new generation is a new source, so it
  examines every file again. This is intended, and it is the only
  purpose of the number: the author raises the generation when the
  contract now finds more than before, or finds something different. It
  is not the program's version and does not change with a build, a
  dependency or a release, since none of these change what the same
  bytes yield. A program that reported its release number would write
  all its claims again with every release. Each contract has its own
  source; for the worklist and the record it makes no difference that
  several contracts are in one binary.
- `mimes`: the exact MIME types this contract reads, as written in
  `file:mime`. No patterns; list each type.
- `derives`: `true` when this contract writes derived files (an
  unpacked attachment, extracted text, a thumbnail) and therefore needs
  a directory for them. If absent, it means `false`.

A contract covers what is receipted together, however many formats
that is: one text contract that reads three kinds of document is one
contract, and adding a format to it means raising its generation, not
adding a contract.

## Examination

For each file, the extractor is called with the file's bytes on stdin:
the content itself, whatever form the store keeps it in. If the
contract has a name in its identify line, that name is passed as the
first argument, so the program knows which of its contracts is meant.
If the identify line has `derives`, the next argument is the path of a
new, empty directory for the files the extractor writes. The
orchestrator chooses where that directory is, and the extractor uses
only the path it was given. Both can be run by hand:

    ossuary-extract-image exif < photo.jpg
    ossuary-extract-packed unpack /tmp/out < bundle.zip

The extractor reads stdin to the end *before* writing anything, then
writes zero or more lines, one JSON object each, in three shapes:

```json
{"attribute": "mail:subject", "value": "Re: the plan"}
{"file": "report.pdf", "mime": "application/pdf"}
{"file": "report.pdf", "attribute": "mail:content-id", "value": "<part2@example.com>"}
```

- The first shape is a finding about the examined file. `attribute`
  must match the claim grammar (`namespace:attribute`, lowercase `a-z`,
  `0-9`, `-`); `value` may be any JSON value except `null`. Record
  verbatim, with the format's own field names and the format's own
  spelling; see the [vocabulary](vocabulary.md).
- The second shape announces a derived file: `file` is its name in the
  directory (a bare name, without a path), and `mime` is its type, as
  stated by the program that wrote the bytes instead of detected from
  them. The name is only a handle: the other lines use it to refer to
  the file, and it is never recorded. A name the content actually had,
  such as an attachment's or a zip entry's own name, is a `file:name`
  finding of the third shape, reported whenever the format gives one;
  extracted text has no name and gets none. Only announced files are
  stored; anything else in the directory is the extractor's working
  space, and is ignored and deleted.
- The third shape is a finding about an announced derived file instead
  of the examined file, for what belongs to the part and not to the
  file that contains it: an attachment's content-id, a subtitle track's
  language.

The order of the lines does not matter: the complete output is read
before any of it is checked, so a finding about a file may come before
the line that announces it. The extractor may write progress and error
messages to stderr. The orchestrator passes every stderr line on to
the user, prefixed with the examined file's name and digest (which the
extractor does not know), and removes the program's name where a line
begins with it. An extractor can therefore prefix its lines with its
name for use by hand without the name appearing twice under
`ossuary extract`. On a non-zero exit, the stderr output becomes the
reason in the run's failure list.

**Exit 0 means the examination happened**, with or without findings;
bytes the extractor cannot interpret also count as an examination, with
nothing found. A non-zero exit means it did *not* happen: the file is
listed in the run's failures, gets no receipt, and will be examined
again in a later run. Whatever was written to the directory is
discarded, and nothing is stored.

## What the orchestrator does with it

`ossuary extract <name>` runs every contract the program announces, one
after the other, each with its own worklist and its own receipts.
`ossuary extract <name>:<contract>` runs one of them, and the same form
is used in the archive's own list of extractors in config.toml; this is
how a policy such as "list the contents of packed archives, never
unpack them" is configured. The orchestrator turns every finding into a
claim in the archive's own format: it adds the time and the source from
the identify line, and as subject the examined file's subject for
findings without `file`, or the derived file's subject for findings
with `file`. The extractor cannot know the derived file's subject,
because it is the digest of the file's bytes. Each announced file is
stored in the archive's derived store, as content like any other, with
the claims that are known about it: `file:mime` as announced,
`prov:origin` naming the examined file, and, for bytes the store did not
hold before, `file:size`. No name is invented: a derived file has a
`file:name` only if the extractor reported one. Bytes the content store
already holds (an attachment that was also saved and ingested as a
file) are not copied into the derived store. The claims are written all
the same, and since a subject names content in whichever store it is,
the bytes are read from `content/`.

The output for a file is rejected completely when any line does not
parse, refers to a file that was never announced or never written,
announces a file twice, or has a path where a bare name is required.
Then nothing is recorded, there is no receipt, and the file is examined
again in a later run. Otherwise, after all other claims, the
orchestrator writes one receipt, on the examined file:

```json
{"subject": "9f2a…", "attribute": "prov:examined", "value": "extractor:mail/1", "time": "…", "source": "extractor:mail/1"}
```

The value repeats the source on purpose: the standing set has one
element per subject, attribute and value, so only the value
distinguishes this contract's receipt from another contract's there.
The receipts record what has been examined. The worklist of files still
to examine is computed from the log: every subject whose standing
`file:mime` is one of the contract's `mimes`, minus every subject that
already has its receipt. It therefore survives the loss of any cache.
The MIME list only selects files for the worklist and is no guarantee
of what the extractor receives: a user can name a file directly, and
bytes the extractor cannot interpret may arrive. The correct response
to those is an examination with nothing found. Derived files are
treated like any other content: the log has claims about them, and an
extractor that reads their type finds them on its worklist, already
within the same `ossuary extract` call, which runs in rounds until a
round examines no new file. The log determines only which files are
examined, never the content of a claim: an extractor's findings come
from the bytes alone. The extractor knows neither the archive nor the
record, and any decision that would need them is made by the
orchestrator.
