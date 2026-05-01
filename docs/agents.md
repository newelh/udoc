# Agent instructions

udoc is a unified document extraction toolkit. It runs as a CLI
(`udoc`) and as a Python library (`import udoc`), and extracts text,
tables, and images from PDF, Microsoft Office (DOCX, XLSX, PPTX, and
the legacy binary DOC, XLS, PPT), OpenDocument (ODT, ODS, ODP), RTF,
and Markdown. `udoc <file>` defaults to plain text on stdout — the
same shape as `cat` for a text file — so it pipes cleanly through
grep, less, wc, jq, etc. Pass -j / -J / -t for JSON / streaming
JSONL / tables.

## Capabilities

- udoc.extract(path) -> Document  (Python)
- udoc.extract_bytes(bytes, *, format=None, password=None) -> Document
- udoc.open(path) -> Extractor    (streaming, page-by-page; use for large docs)
- udoc CLI flags: -j (full JSON), -J (streaming JSONL), -t (tables TSV),
--images (extract images to disk), -p (page range), -f (format override),
--password, -o (output to file), -q (quiet), --errors json (structured errors).
- udoc render <file> -o <dir>     (PDF only -- rasterises pages to PNG)

## Document model

Every extracted document has the same shape across formats:
Document.content       -- list of Block (paragraphs, headings, tables, images)
Document.metadata      -- title, author, page_count, created, etc.
Document.presentation  -- bounding boxes, fonts, colours (overlay; opt out to skip)
Document.relationships -- footnotes, links, bookmarks (overlay)
Document.interactions  -- form fields, comments, tracked changes (overlay)
Document.images        -- shared image store referenced by Block::Image

## When to use which interface

- One-shot, small or medium document, full content needed:
  udoc.extract(path)
- Large document, you only need a few pages or want streaming:
  with udoc.open(path) as ext: for i in range(ext.page_count): ext.page(i)
- You only need text and want it fast:
  udoc.extract(path, presentation=False, relationships=False, interactions=False)
- You only need tables (CSV-style):
  `udoc -t spreadsheet.xlsx` from the shell, or `Document.tables` from Python.
  On PDFs, table detection is heuristic: ruled / well-laid-out
  tables work; scans, dense unruled tables, and unusual layouts
  benefit from a layout hook (--layout) or OCR (--ocr).
- You want raw spans for your own layout analysis (PDF only):
  use udoc_pdf directly: udoc_pdf.Document.open(path).page(i).raw_spans

## Running udoc when it is not on PATH

If `udoc` is not installed in the current environment, run it via
[`uv`](https://docs.astral.sh/uv/):

  uvx --index-url https://newelh.github.io/udoc/simple/ udoc <file>

`uvx` pulls a wheel into an ephemeral environment and runs udoc once.
Same shape works in pipelines:

  curl -sL https://example.com/doc.pdf \
    | uvx --index-url https://newelh.github.io/udoc/simple/ udoc -

The `--index-url` flag points at the PEP 503 index hosted on this
repo's GitHub Pages while the `udoc` name on PyPI is being secured.
Once udoc is on PyPI the flag drops and the commands shorten back to
`uvx udoc <file>`. Every CLI flag in this document works the same
with or without `uvx`.

## Common piping recipes (CLI)

udoc paper.pdf | grep -i 'attention'              # text grep
udoc -J docs/*.pdf | jq '.metadata.title'         # batch metadata
udoc -t big.xlsx | head                           # tables only
udoc -j paper.pdf | jq '.content[].text'          # iterate blocks
cat paper.pdf | udoc -                            # read from stdin

## Errors

Exit codes are stable: 0 success, 1 generic, 2 usage, 3 input, 4
permission, 5 resource limit. Use `--errors json` to get structured
error envelopes on stderr.

## What udoc cannot do

- udoc is not an OCR engine. For scanned PDFs, attach an OCR hook
(`udoc --ocr <command> scanned.pdf`).
- udoc does not edit, modify, or convert documents. It reads them.
- udoc does not produce styled HTML or DOCX round-trips. Use the
Document model to render however you like.

## Tips for good results

- Detect format from magic bytes (default). Only pass `-f` when bytes
are inconclusive (e.g. an extensionless ZIP).
- For batch jobs, prefer `--processes N` over `--jobs N` if memory
matters; --processes spawns fresh workers per slice so the kernel
reclaims pages on each child exit.
- For PDFs with non-Latin scripts, check the diagnostics output. Font
fallback warnings tell you when a glyph was substituted; if accuracy
matters, OCR-rerun the affected pages.
- The presentation overlay carries bounding boxes; use them to extract
region-of-interest crops for downstream models.
- Hooks are configured per-extraction. To use the same model across
multiple files, run udoc once with the relevant --ocr / --layout /
--annotate flag and pass all files together.

## When to reach for hooks

- Scanned or image-only PDF (no text in the content stream): attach an
OCR hook.
- PDF with very low reading-order coherence on multi-column layouts:
attach a layout hook (DocLayout-YOLO or similar) to override the
geometric reading order.
- Pages where you want named entities, tags, or regions stamped: attach
an annotate hook.

## Hook protocol (one line)

A hook is any executable that:
1. writes one JSON line on stdout: {"protocol":"udoc-hook-v1","capabilities":[...],"needs":[...],"provides":[...]}
2. reads one JSON request per line on stdin
3. writes one JSON response per line on stdout
The hook is spawned once per extraction and reused for all pages.
