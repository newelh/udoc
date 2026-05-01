# CLI reference

`udoc` is one binary that handles every supported format. Format detection
is automatic from magic bytes; pass `-f` to override.

## Synopsis

```
udoc [OPTIONS] <FILE>
udoc <SUBCOMMAND> [OPTIONS] <FILE>
```

Pass `-` for `<FILE>` to read from stdin. Output goes to stdout by default;
pass `-o` to write to a file.

## Output modes

The default output is plain text. Switches change the shape:

| Flag           | Output                                    |
|----------------|-------------------------------------------|
| _(default)_    | Plain text                                |
| `-j, --json`   | Full document as JSON                     |
| `-J, --jsonl`  | Streaming JSONL, one record per page      |
| `-t, --tables` | Tables only, as TSV                       |
| `--images`     | Extract embedded images to disk           |

`--json` and `--jsonl` differ in framing. `-j` produces one well-formed
JSON document (the whole `Document` model). `-J` writes one JSON object per
line, streamed page-by-page; ideal for piping into `jq`, `grep`, or a
language-model worker without loading the full document.

> **Heuristic extraction on PDFs.** `-t` (tables) and reading-order
> reconstruction on PDFs are best-effort without help from OCR or
> layout detection. Born-digital PDFs with ruled tables and clean
> single-column or two-column flow extract cleanly; scans, dense
> unruled tables, rotated headers, and broken-but-correct producers
> can disagree with udoc's defaults. For hard cases, attach a
> layout-detection or OCR hook
> (`udoc --layout doclayout-yolo paper.pdf`,
> `udoc --ocr tesseract-hook scanned.pdf`). Background and the
> failure modes worth knowing about live in the
> [PDF format guide](formats/pdf.md#table-detection) and
> [PDF rendering & OCR](render.md).

## Common options

| Option                | Purpose                                                |
|-----------------------|--------------------------------------------------------|
| `-p, --pages <SPEC>`  | Page range, e.g. `1-5`, `3,7,9-12`                     |
| `-f, --input-format`  | Force input format instead of auto-detecting           |
| `--password <PW>`     | Document password (PDF encryption)                     |
| `-o, --output <FILE>` | Write output to file instead of stdout                 |
| `--pretty`            | Pretty-print JSON output                               |
| `--raw-spans`         | Include raw positioned spans in JSON                   |
| `--no-presentation`   | Omit the presentation overlay (geometry + fonts)       |
| `--no-relationships`  | Omit the relationships overlay (footnotes, links)      |
| `--no-interactions`   | Omit the interactions overlay (forms, comments)        |
| `-q, --quiet`         | No-op kept for compatibility (silent is the default)   |
| `-v, --verbose`       | Show warnings on stderr (`-v`) or warnings + info-level progress (`-vv`); duplicates are deduped |
| `--errors json`       | Emit structured error JSON to stderr instead of prose  |
| `--jobs <N>`          | Worker thread count for batch input (`udoc dir/*.pdf`) |

## Subcommands

```
udoc <file>                  # bare-file shortcut, equivalent to udoc extract
udoc extract <file>          # text / tables / images / JSON extraction
udoc render <file> -o <dir>  # rasterise PDF pages to PNG
udoc fonts <file>            # list fonts and per-span resolution
udoc images <file>           # list or dump embedded images
udoc metadata <file>         # structured metadata JSON
udoc completions <shell>     # emit shell completions for bash/zsh/fish/powershell
udoc --help                  # full reference
udoc --version
```

## Exit codes

Stable across releases; agents and shell pipelines can rely on them:

| Code | Meaning                                         |
|------|-------------------------------------------------|
| `0`  | Success                                         |
| `1`  | Generic error                                   |
| `2`  | Usage error (bad flags, missing file)           |
| `3`  | Input error (corrupt or unsupported document)   |
| `4`  | Permission denied (file access or PDF password) |
| `5`  | Resource limit hit (size, memory, page count)   |

`--errors json` emits a structured error envelope on stderr in addition to
the exit code, so agents grep-ing under `2>&1` can recover the typed error
without parsing prose.

## Piping recipes

Grep for a pattern in a PDF:

```bash
udoc paper.pdf | grep -i 'attention'
```

Read a PDF straight from a URL:

```bash
curl -sL https://arxiv.org/pdf/1706.03762 | udoc -
```

Extract all titles from a directory of documents:

```bash
udoc -J docs/*.pdf | jq -r '.metadata.title // empty'
```

Stream tables out of a workbook into a Python pipeline:

```bash
udoc -t big.xlsx | python summarise.py
```

Rasterise a PDF page-by-page into per-page PNG files:

```bash
udoc render paper.pdf -o ./pages --dpi 150
```

Extract everything as one streaming JSONL into a database loader:

```bash
udoc -J ingest.pdf | duckdb -c "COPY (SELECT * FROM read_json_auto('/dev/stdin')) TO 'pages.parquet'"
```

## Format detection

Detection runs in this order, stopping at the first match:

1. Explicit `-f`/`--input-format` flag.
2. Magic bytes at the start of the file (`%PDF-`, `PK\x03\x04` for OOXML
   and ODF, `\xD0\xCF\x11\xE0` for legacy CFB Office, `{\rtf1`, etc).
3. For ZIP-based formats, the OPC content-types entry inside the
   archive distinguishes DOCX/XLSX/PPTX/ODT/ODS/ODP.
4. Filename extension as a last resort, when bytes are inconclusive.

A format-detection failure is exit code `3` with a structured error.

## Diagnostics on stderr

By default, stderr is silent. The recoveries that happen routinely
during extraction — font fallbacks, malformed-xref recovery, stream-
length scans, table-detection skips on edge cases — fire as structured
warnings into the diagnostics sink, but the CLI swallows them so the
common case (`udoc paper.pdf | grep ...`, `| less`, `| wc`) just shows
the document.

Pass `-v` to print warnings, or `-vv` to also print info-level progress
(font loads, ToUnicode resolution, per-page reading-order tier).
Duplicate (level, kind, message) tuples are deduplicated within one
extraction so a document with the same fallback firing 30 times does
not flood the terminal.

If you are scripting, `--errors json` turns warnings into JSON objects:

```json
{"level":"warning","kind":"StreamLengthMismatch","page":12,"message":"..."}
```
