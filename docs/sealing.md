# Sealing an archive: proving that nothing was changed

An ossuary archive is often where records are kept for years: invoices,
contracts, correspondence, the mail of a business. Sometimes you have to
show someone else, an auditor, a tax office, a court, that it still
holds what it held at a certain time: that no file was removed or
altered since, and no claim about one.

A seal does that. `ossuary seal` records the state of the archive and
prints an id for it. Whoever keeps that id can later check the archive
against it. Either everything the archive held then is still there and
unchanged, or the check names what is missing or different.

* [What a seal is for](#what-a-seal-is-for)
* [What a seal proves, and what it does not](#what-a-seal-proves-and-what-it-does-not)
* [Sealing the archive](#sealing-the-archive)
* [Keeping the id](#keeping-the-id)
* [Checking a seal](#checking-a-seal)
* [Timestamping the id](#timestamping-the-id)
* [Checking without ossuary](#checking-without-ossuary)
* [What changes for the archive](#what-changes-for-the-archive)


## What a seal is for

Businesses often have to keep their records for years, and keep them
unchanged. In Germany the GoBD, the tax administration's rules for
keeping business records in electronic form, cover invoices, contracts
and business correspondence, mail included. Among other things, a record
must not be changed in a way that makes its original content impossible
to determine, or that leaves it uncertain whether a change was made at
the start or later. Other countries have comparable rules.

> **This page is not legal advice.** ossuary does not claim that an
> archive meets the GoBD or any other rule. Whether it does depends on
> the whole procedure: who runs the archive, where it is kept, who can
> write to it, how all of this is documented, and what else is in place.
> A seal adds one thing: a way to show whether the archive has changed
> since a given moment. Ask whoever is responsible for compliance in
> your organisation whether that serves your purpose.

A seal does not stop anyone from changing the archive. The archive is
ordinary files on ordinary storage, and whoever can write to it can
change it. What a seal provides is that a change can be shown
afterwards, or ruled out.

`ossuary audit` checks that the archive agrees with its own record:
every file matches its name, every claim can be read, every segment the
record names is there. A change made with care leaves nothing for it to
find: the newest segments of the record removed, and the open segment
made to name an older one; or every segment from some point on written
anew. A seal finds both.


## What a seal proves, and what it does not

A seal that holds proves that **everything the archive held when the
seal was made is still there, unchanged**: every claim recorded up to
that moment, and every file those claims refer to. That includes changes
that leave the archive looking perfectly in order.

It does **not** prove:

* **Anything about the time before sealing.** Whoever had the archive
  before the seal was made could have changed it. Sealing early and
  regularly keeps that gap small.
* **When the seal was made, on its own.** A check names the time of the
  last claim the seal covers. That time was written by the program that
  recorded the claim, from the clock of the machine it ran on. It cannot
  be changed later without changing the id, but it can have been wrong
  from the start. What proves the time is where the id went: see
  [Keeping the id](#keeping-the-id).
* **Anything about what was recorded later.** Files and claims added
  after the seal are not covered. A check counts them as "recorded
  since" and does not hold them against the seal. It cannot tell a
  claim from a later run from one somebody added by hand.
* **Anything about files without claims.** A file in the archive that
  no claim refers to is not part of the record, and the seal does not
  cover it. An interrupted run can leave such files; `ossuary audit`
  lists them. The same goes for the files beside the record:
  `config.toml`, `mailvault.toml` and `cache/`.
* **That the archive is complete.** A seal records what the archive
  held, not what the disks or mailboxes held.

A seal shows *that* something changed and exactly *which* segments and
files, but it cannot bring back what is gone. For that you need a backup
of the archive. Because the seal names every missing or changed file
precisely, the right copy can be found in a backup, and once it is
restored the seal holds again. **Seal and backup belong together.**


## Sealing the archive

```console
$ ossuary --archive /srv/archive seal
archive /srv/archive
checking every sealed segment against its hash, and the chain they form
open segment sealed
the seal covers 312 sealed segment(s) with 1308712 claim(s) and every file they refer to
the id is the sha256 hash of the last sealed segment; keep it outside the archive, `ossuary seal --check ID` checks it
1bbb4b0cc3a691061154ec0a228cb409aa3266c5bb90a7182c9164b02e153776
```

The long line is the seal id. It is the only line on stdout, so a script
can take it with `id=$(ossuary seal)`; everything else goes to stderr.

The claims are kept in segments, and every segment names the segments
recorded before it by their hash. Every claim names the file it is
about by the file's hash. `seal` closes the open segment, and the hash
of that last segment is the id: it fixes every segment before it, every
claim in them, and every file those claims refer to. Sealing reads every
segment, but none of the files, and is quick. Checking a seal reads the
files as well.

**Run it again** when nothing was recorded since, and it prints the same
id. Nothing is stored for a seal apart from the segment it names, so
sealing after every run piles nothing up.

**Before the first seal**, run `ossuary audit`. A seal records the
archive as it is, damage included, and a file that was already damaged
would later look as if it had been changed after sealing.

`--dry-run` checks the same and says what the seal would cover, and
writes nothing:

```console
$ ossuary -q seal --dry-run
would seal the open segment; the seal would cover 312 sealed segment(s) with 1308712 claim(s) and every file they refer to; --dry-run, nothing written
```

`--json` prints the id as an object of two members, for a script that
wants the algorithm as well:

```console
$ ossuary -q seal --json
{"algorithm":"sha256","segment":"1bbb4b0cc3a691061154ec0a228cb409aa3266c5bb90a7182c9164b02e153776"}
```

### When sealing is refused

A seal is not made over a record that is already disturbed, because the
seal could never be checked, or would cover up a loss. `seal` lists what
it found, in the words `ossuary audit` uses, and each line names the way
out:

| What it found | Way out |
|---|---|
| A sealed segment is damaged or cannot be read | Restore it from a backup of the archive and seal again |
| A sealed segment is missing | Restore it from a backup and seal again. Without a backup, `ossuary maintain mend` records the loss; seal again after that |
| The first segment of the archive, or the last segment of a line merged in, is missing | Restore it from a backup and seal again. No mend can stand in for it |
| The open segment, `head.jsonl`, is missing | `ossuary maintain mend` records the loss and starts a new one; record the lost claims again if possible, then seal again |
| A mend that the open segment does not name, left by an interrupted `ossuary maintain mend` | Run `ossuary maintain mend` again, then seal again |

```console
$ ossuary seal
archive /srv/archive
checking every sealed segment against its hash, and the chain they form
claims: segment 880088be7f0533a23c4f7efe001bd2baa69640518c543bbca7910051c64228fe is missing; 7922c85f074e31c04b15931157af38610b09d45cc346e1e686d964649b231ce0 names it as its predecessor
claims: 311 sealed segment(s) and the open segment, 1304544 claim(s), 1 missing
the chain of sealed segments is broken: 2 separate chains, expected one
  chain 1: 117 segment(s), 489310 claim(s), 2024-01-08T09:12:44Z to 2025-02-17T16:40:02Z, 9a71459e9cfc2c6570ada7147da5693a73631c13c5c4a20bb822b8be5d0bef88 to 52e6c7bbed43568c33a419f13edc574e36cb6f95c06b9024d04b9c13896216de
  chain 2: 194 segment(s), 815234 claim(s), 2025-02-18T08:01:15Z to 2026-10-22T02:15:07Z, 7922c85f074e31c04b15931157af38610b09d45cc346e1e686d964649b231ce0 to the open segment
  chain 2 starts after segment 880088be7f0533a23c4f7efe001bd2baa69640518c543bbca7910051c64228fe, which is missing; restore it from a backup of the archive, or run `ossuary maintain mend` to join the chains (the mend records the missing segment's name)
no seal made: a seal now would not cover what is listed above; seal again once that is resolved
```

The command exits non-zero and prints no id.

A seal made after a mend covers the loss as the mend records it: a check
later reports the missing segment as already missing when the seal was
made, and does not count it against the seal.


## Keeping the id

The id is worth as much as the place it is kept. Inside the archive it
proves nothing: whoever can change the archive can change the id as
well. Keep it where the people who can write to the archive cannot
change it:

* printed, dated and signed, filed with your other records
* in a mail to someone else: the accountant, the auditor, a colleague
  who does not run the archive
* with a timestamp service (see [Timestamping the id](#timestamping-the-id))

That place is also what proves **when** the seal was made. A seal whose
last claim is later than the date on the printout, or than the mail it
was sent in, is a finding in itself.

### Sealing after every run

The simplest routine is a seal after every run that adds to the archive,
in the same scheduled job:

```console
$ crontab -l
15 2 * * *  cd /srv/archive && ossuary mailvault fetch && ossuary seal
```

With `&&` the archive is sealed only after a run without errors; with
`;` it is sealed either way. If cron mails the output of its jobs to
someone, that mail carries every night's id out of the archive. A night
without new mail produces no new seal, only the same id again.


## Checking a seal

```console
$ ossuary --archive /srv/archive seal --check 1bbb4b0cc3a691061154ec0a228cb409aa3266c5bb90a7182c9164b02e153776
archive /srv/archive
step 1 of 3: the sealed segments, each one checked against its hash
step 2 of 3: the 131204 file(s) the sealed claims refer to, each one read in full
step 3 of 3: the record since the seal
recorded since: 6311 claim(s) in 2 sealed segment(s) and the open segment; the seal does not cover them
the seal 1bbb4b0cc3a691061154ec0a228cb409aa3266c5bb90a7182c9164b02e153776 holds: the record up to 2026-10-22T02:15:07Z (312 sealed segment(s), 1308712 claim(s)) and the 131204 file(s) it refers to are present and unchanged
```

The last line is the verdict, and it names the time of the record it
covers, because that is what it is about: the archive still holds what
it held at that time. The line before it is what the archive recorded
beyond the seal; the seal says nothing about that.

The check reads every file the seal covers, in full. On a large archive
on a network share that takes a while, about as long as `ossuary audit`.
The exit code is zero when the seal holds and non-zero otherwise.

**A quick check** with `--fast` checks every sealed segment as before,
but only looks for each file and does not read it. That takes a
fraction of the time and shows a file that was removed, not one that was
changed. The verdict says so:

```console
$ ossuary -q seal --check 1bbb4b0cc3a691061154ec0a228cb409aa3266c5bb90a7182c9164b02e153776 --fast
recorded since: 6311 claim(s) in 2 sealed segment(s) and the open segment; the seal does not cover them
the seal 1bbb4b0cc3a691061154ec0a228cb409aa3266c5bb90a7182c9164b02e153776 holds as far as --fast checks: the record up to 2026-10-22T02:15:07Z (312 sealed segment(s), 1308712 claim(s)) is present and unchanged, and the 131204 file(s) it refers to are present; their contents were not read
```

**The id is checked only in full.** The first few characters of an id
prove nothing: a forged seal can be made to start the same way. ossuary
refuses a shortened id.

**Any copy of the archive can be checked,** a restored backup included:

```console
$ ossuary --archive /mnt/restore/archive seal --check 1bbb4b0cc3a691061154ec0a228cb409aa3266c5bb90a7182c9164b02e153776
```

The archive's `FORMAT` file names the hash the id is made with, so
nothing else is needed. An id checked against an archive that uses
another hash names no segment there, and does not hold.

### When a seal does not hold

```console
$ ossuary -q seal --check 1bbb4b0cc3a691061154ec0a228cb409aa3266c5bb90a7182c9164b02e153776
1 sealed segment(s) gone:
  880088be7f0533a23c4f7efe001bd2baa69640518c543bbca7910051c64228fe
1 file(s) gone:
  800a3760695414c4e2ff62055f94c7d7acf2ba441314e41d647f679878c81842
the seal 1bbb4b0cc3a691061154ec0a228cb409aa3266c5bb90a7182c9164b02e153776 does not hold: 2 finding(s) above; restore what is named from a backup of the archive, then check again
```

A finding is a sealed segment or a file that is gone, or whose content no
longer matches its name, or that cannot be read. Up to five names are
listed; `-v` lists all of them. Two questions follow from a finding:
what happened, and how to get the files back. The second is answered by
a backup of the archive. Restore the named files, check again, and the
seal holds again. A file that is damaged in `content/` but has a sound
copy in `derived/` is held unchanged; `ossuary maintain weed --repair`
puts the sound copy back.

The check also finds a record that no longer leads to the seal:

```
the open segment no longer leads to the seal: the record was cut back to an earlier point, or begun anew, after the seal was made
```

Everything the seal covers may still be there, but the record was cut
off after it: the segments recorded after the seal were removed, and the
open segment was made to name an earlier one. Restore `claims/` and
`head.jsonl` from a backup made after the seal.

If the archive does not hold the seal's segment at all, the check says
so:

```console
$ ossuary -q seal --check 1bbb4b0cc3a691061154ec0a228cb409aa3266c5bb90a7182c9164b02e153776
this archive holds no segment 1bbb4b0cc3a691061154ec0a228cb409aa3266c5bb90a7182c9164b02e153776, so the seal cannot be checked; it was removed, or the id belongs to another archive
```

That is a finding as well: either somebody removed the segment, or the
id belongs to a different archive.


## Timestamping the id

A printout with a date and a signature is often enough. Where it is not,
a timestamp service can confirm the time instead: it signs "this id
existed at this moment" with its own certificate. Such services follow a
standard, RFC 3161, and they receive only the id, nothing from the
archive. In the EU, a qualified electronic timestamp under the eIDAS
regulation carries a legal presumption that its time is correct;
ordinary services are free but carry no such presumption.

ossuary does not do this itself. Which service to use, whether it has to
be a qualified one, and where the receipts are kept is your
organisation's decision, and it is easy to do from a script, for example
with `openssl`:

```console
$ id=$(ossuary seal)
$ openssl ts -query -digest "$id" -sha256 -cert -out seal.tsq
$ curl -s -H 'Content-Type: application/timestamp-query' \
      --data-binary @seal.tsq https://tsa.example.org/tsr > seal.tsr
$ openssl ts -verify -in seal.tsr -queryfile seal.tsq -CAfile tsa-ca.pem -untrusted tsa.pem
Verification: OK
```

Keep the receipt (`seal.tsr`) and the service's certificates outside the
archive, for the same reason as the id: the signature protects the
receipt against being changed, not against being deleted. `seal` names
the hash on stderr. For an archive in `sha384` or `sha512`, write
`-sha384` or `-sha512`. `openssl` does not know `blake3`; for an archive
in `blake3`, have a file holding the id timestamped instead: `printf
'%s\n' "$id" > seal.txt` and `openssl ts -query -data seal.txt -sha256
-cert -out seal.tsq`.


## Checking without ossuary

An auditor's IT department may want to check a seal without relying on
the program that made it. Everything needed is plain files and standard
checksums, with `zstd` for the segments, which are always stored
compressed. In an archive whose `FORMAT` names `sha256`, that is
`sha256sum`.

1. **The seal** is a segment in `claims/`, in a directory named after
   the first two characters of its id:
   `claims/1b/1bbb4b0c….seg.zst`. Uncompressed, its checksum is the id.

   ```console
   $ zstd -dc claims/1b/1bbb4b0cc3a691061154ec0a228cb409aa3266c5bb90a7182c9164b02e153776.seg.zst | sha256sum
   1bbb4b0cc3a691061154ec0a228cb409aa3266c5bb90a7182c9164b02e153776  -
   ```

2. **Its first line** names the segments recorded before it,
   `{"ossuary-segment": 1, "previous": ["52e6c7bb…"]}`, each of which
   lies in `claims/` the same way. Following every name in every
   `previous` reaches every segment the seal covers.

3. **Every line after the first** of a segment is a claim. Its
   `subject` names a file, and so does the value of a `prov:origin`
   claim. The file lies at `content/80/0a/800a3760…`, or the same path
   under `derived/`, and its checksum is its name. A file stored
   compressed ends in `.zst` and is checked uncompressed.

If every segment and every file is there and every checksum matches its
name, the seal holds. [Reading an archive without
ossuary](recovery.md#checking-a-seal) does all of this in one script,
and [the archive format](format.md#the-seal) is the exact description.


## What changes for the archive

* **Nothing in ossuary removes or rewrites what a seal covers.**
  `ingest`, `extract`, `annotate`, `retract`, `merge` and `maintain mend`
  only add to the archive; `maintain weed` removes a copy in `derived/`
  only when `content/` holds the same file. Every command keeps running
  on a sealed archive, and none of them breaks a seal.
* **A seal is not stored apart.** It is the name of a sealed segment,
  and segments are never removed. Every segment sealed so far, also those
  the open segment sealed on its own as it grew, is a valid seal, so
  there is no list of seals to keep in the archive: the list is the ids
  you kept outside it.
* **A lost open segment stops the archive.** While `head.jsonl` is
  missing, every command that records claims refuses to run until
  `ossuary maintain mend` has recorded the loss. The record cannot start
  over unnoticed.
* **`ossuary-fix` breaks seals.** It is the repair tool for archives
  written by older 0.x versions, and a fix that rewrites sealed segments
  renames them. It says from which time on seal ids no longer check.
  Seal again afterwards, and keep the new id.
* **`ossuary merge --rehash` gives the other archive's record new
  names.** Its seals still check against the other archive, not against
  the one it was merged into. A merge without `--rehash` keeps the other
  archive's segments as they are, and their seals check in both.
