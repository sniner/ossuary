# Reading an archive without ossuary

An ossuary archive is a directory of ordinary files, and everything in it
can be read with common tools: a shell, `zstd`, `jq` and the checksum
tools from coreutils. This document shows how, with examples for both
forms an archive can have:

- **Uncompressed**, the default: every stored file is the original, byte
  for byte.
- **Compressed**: every stored file is compressed with zstd.

[The archive format](format.md) is the complete description of the
format. This document covers the practical part: getting files back,
finding out which file is which, and checking that nothing is damaged.

Keep a copy of this document with every backup of an archive.

The examples use bash and are run in the root directory of the archive.
They assume `shopt -s nullglob`, so that a pattern that matches no file
expands to nothing. Paths and times in the sample output are examples.

## What you need

| Tool | Needed for |
| ---- | ---------- |
| `sha256sum` (coreutils) | checking a file against its name |
| `zstd` | compressed archives, and the claim log of every archive |
| `jq` | reading the claim log |

An archive may use a hash other than SHA-256; `FORMAT` names it (see
below). For `sha384` and `sha512` use `sha384sum` and `sha512sum` from
coreutils, for `blake3` use `b3sum`.

## The layout

```console
$ cat FORMAT
{"ossuary-archive":1,"algorithm":"sha256","content-depth":2,"derived-depth":2,"claims-depth":1}
$ find . -type f -not -path './cache/*' | sort
./claims/88/880387cd1af17fcaa6b19182eabe63d2d851d5647b3be472ce211ca3dfd59af0.seg.zst
./config.toml
./content/23/c1/23c187e81bb6e96bfde818940f69e5465bee875ac66efefcd9d1172b1c9f96d4
./content/93/03/9303125ef2413d42a1083e036accd9a3aeb323d2f90828fdf361273b0c8cfad5
./content/da/d5/dad58b5df9916f9338240587a484fd02aafd319e2cccff10537f31979da289ea
./FORMAT
./head.jsonl
```

- `content/` holds the files that were added to the archive. Each file is
  named after the SHA-256 checksum of its content, in hex. It lies in two
  levels of directories named after the first four hex digits, because
  `content-depth` is 2.
- `derived/` holds files that extractors produced from the files in
  `content/`, such as attachments unpacked from a mail. It is read the
  same way as `content/`.
- `claims/` and `head.jsonl` hold the claim log: everything recorded
  about the files, such as their original paths, dates and tags. A stored
  file has no name other than its checksum; its original name is in the
  log.
- `config.toml` holds settings for adding files. Reading does not need it.
- `cache/` holds only data that can be rebuilt from the rest, and the
  `tmp/` directories inside `content/`, `derived/` and `claims/` hold
  interrupted writes. Both can be ignored.

The form of each stored file shows in its suffix:

| On disk | Form | Read with |
| ------- | ---- | --------- |
| `9303125e…cfad5` | uncompressed | `cat`, or use the file as it is |
| `9303125e…cfad5.zst` | compressed | `zstd -dc` |

The name is the checksum of the original content in both forms. A
compressed archive can also hold uncompressed files, added before
compression was switched on. The segments of the claim log in `claims/`
are compressed in every archive (`.seg.zst`); `head.jsonl` is a plain
text file.

A file whose name ends in `.corrupt`, or in `.corrupt` and a number, is a
damaged copy that was set aside and replaced by an intact one. The
examples below skip these files.

## Uncompressed archive

A file in `content/` is the original. To get it back, copy it and give it
a name:

```console
$ cp content/93/03/9303125ef2413d42a1083e036accd9a3aeb323d2f90828fdf361273b0c8cfad5 ~/notes.txt
```

The name is the file's checksum, so checking a file takes one command.
If `sha256sum` prints the name of the file, the file is intact:

```console
$ sha256sum content/93/03/9303125ef2413d42a1083e036accd9a3aeb323d2f90828fdf361273b0c8cfad5
9303125ef2413d42a1083e036accd9a3aeb323d2f90828fdf361273b0c8cfad5  content/93/03/9303125ef2413d42a1083e036accd9a3aeb323d2f90828fdf361273b0c8cfad5
```

Without the log, `file` shows what kind of file each one is:

```console
$ file content/*/*/*
content/23/c1/23c187e8…f96d4: JPEG image data, JFIF standard 1.01, aspect ratio, density 1x1, segment length 16, baseline, precision 8, 400x300, components 3
content/93/03/9303125e…cfad5: ASCII text
content/da/d5/dad58b5d…289ea: news or mail, ASCII text
```

The original names and paths are in the claim log. Its sealed segments
are compressed with zstd, `head.jsonl` is plain text:

```console
$ zstd -dc claims/88/880387cd1af17fcaa6b19182eabe63d2d851d5647b3be472ce211ca3dfd59af0.seg.zst | head -3
{"ossuary-segment":1}
{"subject":"9303125ef2413d42a1083e036accd9a3aeb323d2f90828fdf361273b0c8cfad5","attribute":"file:path","value":"/home/john/docs/notes.txt","time":"2026-03-12T19:04:11Z","source":"ingest","run":"b6940c7e-4fe1-4bf1-a814-83561ef71788"}
{"subject":"9303125ef2413d42a1083e036accd9a3aeb323d2f90828fdf361273b0c8cfad5","attribute":"file:name","value":"notes.txt","time":"2026-03-12T19:04:11Z","source":"ingest","run":"b6940c7e-4fe1-4bf1-a814-83561ef71788"}
```

[Which file is which](#which-file-is-which) shows how to read the log as
a whole.

## Compressed archive

Every file in `content/` has the suffix `.zst`, and `zstd -dc` writes the
original to stdout:

```console
$ zstd -dc content/93/03/9303125ef2413d42a1083e036accd9a3aeb323d2f90828fdf361273b0c8cfad5.zst > ~/notes.txt
```

The name is the checksum of the original content, not of the compressed
file. To check a file, decompress it and compare the checksum of the
output with the name:

```console
$ zstd -dc content/93/03/9303125ef2413d42a1083e036accd9a3aeb323d2f90828fdf361273b0c8cfad5.zst | sha256sum
9303125ef2413d42a1083e036accd9a3aeb323d2f90828fdf361273b0c8cfad5  -
```

A copy of a compressed archive can be turned into an uncompressed one.
This decompresses every file in `content/` and `derived/` of the copy and
removes the `.zst` files:

```console
$ cp -a /srv/archive ~/archive-copy
$ zstd -dqr --rm ~/archive-copy/content ~/archive-copy/derived
```

The claim log is read as in the uncompressed archive.

## Which file is which

The claim log records what is known about each file: one claim per line,
each a JSON object. `subject` is the name of the file the claim is about,
`attribute` and `value` are what it states, and `time` is when it was
recorded. [The archive format](format.md#claims) describes all fields,
and [the attribute vocabulary](vocabulary.md) describes the attributes.

The following function reads a stored file in either form. The examples
in the rest of this document use it, so that they work for every
archive:

```bash
read_entry() {
    case "$1" in
        *.zst) zstd -dc "$1" ;;
        *)     cat "$1" ;;
    esac
}
```

The complete log is every sealed segment in `claims/`, in order, followed
by `head.jsonl`. Segments are ordered by the `time` of their first claim,
which is on their second line. This writes the log to
`~/recovery/log.jsonl`:

```bash
out=~/recovery
mkdir -p "$out/segments"
for segment in claims/*/*.seg.zst; do
    read_entry "$segment" > "$out/segments/next"
    time=$(sed -n 2p "$out/segments/next" | jq -r .time)
    mv "$out/segments/next" "$out/segments/$time ${segment##*/}"
done
cat "$out/segments/"* head.jsonl > "$out/log.jsonl"
```

The order matters when a claim was retracted later. For simple searches,
`grep` over the log is enough.

Everything known about one file:

```console
$ jq -r --arg file 9303125ef2413d42a1083e036accd9a3aeb323d2f90828fdf361273b0c8cfad5 \
    'select(.subject == $file) | "\(.time)  \(.attribute)  \(.value)\(if .retract then "  (retracted)" else "" end)"' \
    ~/recovery/log.jsonl
2026-03-12T19:04:11Z  file:path  /home/john/docs/notes.txt
2026-03-12T19:04:11Z  file:name  notes.txt
2026-03-12T19:04:11Z  prov:host  laptop
2026-03-12T19:04:11Z  file:modified  2026-03-10T08:31:02.114520983Z
2026-03-12T19:04:11Z  file:size  44
2026-03-12T19:04:11Z  file:mime  text/plain
2026-03-14T10:20:05Z  user:tag  meetings
```

The files with a given name, or with a tag:

```console
$ jq -r 'select(.attribute == "file:name" and .value == "notes.txt") | .subject' ~/recovery/log.jsonl
9303125ef2413d42a1083e036accd9a3aeb323d2f90828fdf361273b0c8cfad5
$ jq -r 'select(.attribute == "user:tag" and .value == "meetings") | .subject' ~/recovery/log.jsonl
9303125ef2413d42a1083e036accd9a3aeb323d2f90828fdf361273b0c8cfad5
```

A claim with `"retract": true` withdraws an earlier one: with a `value`,
that value of the attribute; without one, every value of the attribute.
The two searches above do not take retractions into account. The next
section does.

## Restoring all files under their paths

A file's paths are its `file:path` claims. A file can have several, for
example when the same content was added from two places, and a path can
have been retracted since. This lists the path of every file that is
still current, as the hex name and the path separated by a tab:

```bash
jq -n -r '
    reduce (inputs | select(.attribute == "file:path")) as $claim ({};
        if ($claim.retract | not) then .[$claim.subject][$claim.value] = true
        elif ($claim | has("value")) then del(.[$claim.subject][$claim.value])
        else del(.[$claim.subject])
        end)
    | to_entries[] | .key as $file | .value | keys[] | "\($file)\t\(.)"
' "$out/log.jsonl" > "$out/paths.tsv"
```

```console
$ cat ~/recovery/paths.tsv
9303125ef2413d42a1083e036accd9a3aeb323d2f90828fdf361273b0c8cfad5	/home/john/docs/notes.txt
dad58b5df9916f9338240587a484fd02aafd319e2cccff10537f31979da289ea	/home/john/mail/2026-03-10-quarterly.eml
23c187e81bb6e96bfde818940f69e5465bee875ac66efefcd9d1172b1c9f96d4	/home/john/photos/DSC_1042.jpg
```

This writes every file to its path below `~/recovery/files`, in whichever
form it is stored. A file with several paths is written to each of them:

```bash
while IFS=$'\t' read -r file path; do
    for entry in "content/${file:0:2}/${file:2:2}/$file"{,.zst}; do
        if [ -e "$entry" ]; then
            mkdir -p "$out/files${path%/*}"
            read_entry "$entry" > "$out/files$path"
            break
        fi
    done
done < "$out/paths.tsv"
```

```console
$ find ~/recovery/files -type f
/home/john/recovery/files/home/john/docs/notes.txt
/home/john/recovery/files/home/john/mail/2026-03-10-quarterly.eml
/home/john/recovery/files/home/john/photos/DSC_1042.jpg
```

`content/${file:0:2}/${file:2:2}/` is the directory for `content-depth`
2. For another depth, add or remove levels accordingly.

## Checking the whole archive

This checks every file in `content/` against its name and lists the
damaged ones. Replace `content` with `derived` to check the files
produced by extractors:

```bash
for entry in content/*/*/*; do
    name=${entry##*/}
    case "$name" in *.corrupt*) continue ;; esac
    if [ "$(read_entry "$entry" | sha256sum | cut -d' ' -f1)" != "${name%%.*}" ]; then
        echo "damaged: $entry"
    fi
done
```

No output means that every file is intact. Whether a sealed segment of
the claim log is missing can be seen from the segment headers, as
described in [the archive format](format.md#segments).
