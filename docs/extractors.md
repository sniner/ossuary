# The ossuary extractor protocol

*Protocol version 1 — draft. An extractor is its own program: found on
PATH, spoken to over pipes, and never touching the archive. This
document is the contract between `ossuary extract` and any extractor,
whoever wrote it, in whatever language.*

## The shape

Understanding content is not the archive's job — it is the job of many
small programs, each reading one family of formats. An extractor:

- is a program named `ossuary-extract-<name>`, found on PATH — the way
  git finds `git-<name>`;
- reads bytes and answers with findings; it never sees the archive
  itself. `ossuary extract` owns the stores and the log, hands each
  file's bytes over, stamps what comes back, and writes the record;
- parses hostile input, which is exactly why it is out of process: a
  crash costs one file and a line in the failure list, never the
  archive. Running an extractor is trusting its binary the way any
  program on PATH is trusted — no more, and no less.

## Identify

Called as `ossuary-extract-<name> --identify`, an extractor answers
with one line of JSON on stdout and exits 0:

```json
{"ossuary-extractor": 1, "source": "extractor:exif/0.1.0", "mimes": ["image/jpeg", "image/tiff"]}
```

- `ossuary-extractor` — the protocol version this extractor speaks. A
  reader that does not know the number refuses instead of guessing.
- `source` — the extractor's identity, in the claim format's source
  grammar: `extractor:name/version`. Every claim it causes carries this
  source, and the receipt under it is what keeps a file from being
  examined twice — so a new version, being a new source, examines
  everything again. That is intended: a new version may see more.
- `mimes` — the exact MIME types it reads, as `file:mime` spells them.
  No patterns; name each one.

## Examination

For each file, the extractor is called without arguments, the file's
bytes on stdin — the content itself, whatever form the store keeps it
in. The extractor reads stdin to its end *before* writing anything, then
answers with zero or more findings, one JSON object per line on stdout:

```json
{"attribute": "exif:date-time-original", "value": "2019:07:14 11:02:41"}
{"attribute": "exif:f-number", "value": "f/2.8"}
```

- `attribute` must fit the claim grammar (`namespace:attribute`,
  lowercase `a-z`, `0-9`, `-`), `value` may be any JSON value except
  `null`. Speak verbatim: the format's own field names, the format's
  own spelling — see the [vocabulary](vocabulary.md).
- stderr is the extractor's to narrate or complain on; it is passed
  through to the user.
- **Exit 0 means the examination happened**, findings or none — bytes
  the extractor cannot make sense of are an examination too, with
  nothing found. A non-zero exit means it did *not* happen: the file is
  named in the run's failures, gets no receipt, and will be offered
  again.

## What the orchestrator does with it

`ossuary extract <name>` funnels everything through the archive's own
grammar: it stamps each finding with the file's subject, the moment,
and the identify line's source, and refuses the whole file if any line
does not parse — nothing half-recorded, no receipt, offered again.
After the findings it writes one receipt:

```json
{"subject": "sha256:9f2a…", "attribute": "prov:examined", "value": true, "time": "…", "source": "extractor:exif/0.1.0"}
```

The receipt is the memory. What still needs examining is a fold over
the log — every subject whose standing `file:mime` is one the extractor
named, minus every subject already carrying its receipt — so the
worklist survives anything a cache would not. The log informs the
*effort* here, never the content of a claim: what an extractor says
comes from the bytes alone.

## Evolution

This document is protocol version 1. Anything that changes what the
pipes carry — new identify fields with meaning, a new finding shape, an
extractor delivering derived *content* (extracted text, thumbnails)
rather than claims — is a new protocol version, announced in the
identify line. Derived content is the known next candidate and
deliberately not in version 1.
