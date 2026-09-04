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
- reads bytes and answers with findings — and, when it has more than
  words to give, writes derived files into a directory handed to it; it
  never sees the archive itself. `ossuary extract` owns the stores and
  the log, hands each file's bytes over, stamps what comes back, and
  writes the record;
- parses hostile input, which is exactly why it is out of process: a
  crash costs one file and a line in the failure list, never the
  archive. Running an extractor is trusting its binary the way any
  program on PATH is trusted — no more, and no less.

## Identify

Called as `ossuary-extract-<name> --identify`, an extractor answers
with one line of JSON on stdout and exits 0:

```json
{"ossuary-extractor": 1, "source": "extractor:mail/0.1.0", "mimes": ["message/rfc822"], "derives": true}
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
- `derives` — `true` when the extractor writes derived files — an
  unpacked attachment, extracted text, a thumbnail — and so needs a
  directory to put them in. Left out, it means `false`: spoken to
  exactly as an extractor always was, no directory involved.

## Examination

For each file, the extractor is called with the file's bytes on
stdin — the content itself, whatever form the store keeps it in — and,
when its identify line said `derives`, with one argument: the path of a
fresh, empty directory for the files it makes. Where that directory
lies is the orchestrator's business; the extractor uses the path it was
given and nothing else. It reads stdin to its end *before* writing
anything, then answers with zero or more lines, one JSON object each,
in three shapes:

```json
{"attribute": "mail:subject", "value": "Re: the plan"}
{"file": "report.pdf", "mime": "application/pdf"}
{"file": "report.pdf", "attribute": "mail:content-id", "value": "<part2@example.com>"}
```

- The first shape is a finding about the examined file. `attribute`
  must fit the claim grammar (`namespace:attribute`, lowercase `a-z`,
  `0-9`, `-`), `value` may be any JSON value except `null`. Speak
  verbatim: the format's own field names, the format's own spelling —
  see the [vocabulary](vocabulary.md).
- The second announces a derived file: `file` is its name in the
  directory — bare, no path in it — and `mime` is what it is, said by
  the one who wrote the bytes instead of guessed from them. Only
  announced files are taken in; anything else in the directory is the
  extractor's workspace, ignored and swept.
- The third is a finding about an announced derived file rather than
  the examined one — for what belongs to the part, not the whole: an
  attachment's content-id, a subtitle track's language.

The order of the lines does not matter: the answer is read whole before
anything is judged, so speaking about a file before announcing it is
legal. stderr is the extractor's to narrate or complain on; it is
passed through to the user.

**Exit 0 means the examination happened**, findings or none — bytes the
extractor cannot make sense of are an examination too, with nothing
found. A non-zero exit means it did *not* happen: the file is named in
the run's failures, gets no receipt, and will be offered again — and
whatever was written to the directory is thrown away, nothing taken in.

## What the orchestrator does with it

`ossuary extract <name>` funnels everything through the archive's own
grammar: it stamps each finding with the moment and the identify line's
source, findings without `file` with the examined file's subject, and
findings with `file` with the derived file's — the extractor cannot
know that name, since it is the bytes' own. Each announced file goes
into the archive's derived store, content with a record like any other,
and onto that record what is known: `file:mime` and `file:name` as announced,
`derive:derived-from` naming the examined file, and — for bytes the
store meets for the first time — `file:size`.

The whole file is refused when any line does not parse, names a file
never announced or never written, announces one twice, or puts a path
where a bare name belongs — nothing half-recorded, no receipt, offered
again. After everything else it writes one receipt, on the examined
file:

```json
{"subject": "sha256:9f2a…", "attribute": "prov:examined", "value": true, "time": "…", "source": "extractor:mail/0.1.0"}
```

The receipt is the memory. What still needs examining is a fold over
the log — every subject whose standing `file:mime` is one the extractor
named, minus every subject already carrying its receipt — so the
worklist survives anything a cache would not. The mime list is
dispatch, not a promise: a user can name a file outright, and bytes
the extractor cannot make sense of may arrive — the graceful answer is
an examination with nothing found. Derived files join that
world like anything else: the log speaks about them, and an extractor
reading their kind will find them on its worklist. The log informs the
*effort* here, never the content of a claim: what an extractor says
comes from the bytes alone — it knows neither the archive nor the
record, and a decision that would need them is the orchestrator's to
make, not the extractor's.
