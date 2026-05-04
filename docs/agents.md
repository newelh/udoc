<div style="float: right; margin-top: .4em; display: flex; align-items: center; gap: .6em;">
  <span id="copy-agent-feedback" style="font-size: .85em; opacity: .75;"></span>
  <button id="copy-agent-instructions" class="md-button md-button--primary" style="padding: .3em 1em; margin: 0;">Copy</button>
</div>

# Agent instructions

<script>
(function () {
  const btn = document.getElementById('copy-agent-instructions');
  if (!btn || btn.dataset.bound) return;
  btn.dataset.bound = '1';
  btn.addEventListener('click', async () => {
    const span = document.getElementById('copy-agent-feedback');
    span.textContent = '';
    try {
      const url = 'https://raw.githubusercontent.com/newelh/udoc/main/docs/agents.md';
      const r = await fetch(url);
      if (!r.ok) throw new Error('HTTP ' + r.status);
      const md = await r.text();
      await navigator.clipboard.writeText(md);
      span.textContent = 'Copied.';
      setTimeout(() => (span.textContent = ''), 3000);
    } catch (e) {
      span.textContent = 'Copy failed (' + e.message + ').';
    }
  });
})();
</script>

Drop this page into an LLM's context (system prompt, SKILLS.md,
tool-use briefing) when you want the model to drive `udoc` for
itself.

udoc is a document extraction toolkit. It runs as a CLI (`udoc`)
and as a Python library (`import udoc`). It extracts text, tables,
and images from PDF, DOCX, XLSX, PPTX, the legacy binary `.doc` /
`.xls` / `.ppt`, ODT, ODS, ODP, RTF, and Markdown. `udoc <file>`
writes plain text to stdout, ready to pipe through `grep`, `less`,
`wc`, or `jq`. Pass `-j` for JSON, `-J` for streaming JSONL per
page, `-t` for tables as TSV.

## Capabilities

Python API:

```
udoc.extract(path) -> Document
udoc.extract_bytes(bytes, *, format=None, password=None) -> Document
udoc.stream(path) -> ExtractionContext   # streaming, page-by-page
udoc.Corpus(path_or_paths)               # lazy iterable of Documents
```

CLI flags:

```
-j   full JSON
-J   streaming JSONL (one record per page)
-t   tables as TSV
-p   page range (e.g. 1-5,10)
-f   format override
-o   write output to file instead of stdout
--password    PDF password
--images      extract images to disk
--errors json structured error envelope to stderr
--ocr / --layout / --annotate <hook>   attach a hook
--ocr-all     force OCR on every page (default: textless pages only)
```

Subcommands:

```
udoc render <file> -o <dir>   rasterise PDF pages to PNG
udoc fonts <file>             list fonts and per-span resolution
udoc images <file>            list or dump embedded images
udoc metadata <file>          structured metadata JSON
udoc completions <shell>      bash / zsh / fish / powershell
```

## Document API (Python)

Properties:

```
doc.metadata        DocumentMetadata
doc.format          Format | None
doc.source          Path | None
doc.warnings        list[Warning]
doc.is_encrypted    bool
```

Methods:

```
doc.pages()         Iterator[Page]
doc.blocks()        Iterator[Block]    # paragraphs, headings, tables, images, ...
doc.tables()        Iterator[Table]
doc.images()        Iterator[Image]
doc.text()          str
doc.text_chunks(by="heading", size=2000) -> Iterator[Chunk]
doc.to_markdown()   str
doc.to_json()       str
doc.render_page(i, dpi=150) -> bytes   # PNG bytes (PDF only)
```

`Block.kind` is one of: `paragraph`, `heading`, `list`, `table`,
`code_block`, `image`, `page_break`, `thematic_break`, `section`,
`shape`. Pattern-match on `block.kind` for variant fields.

## When to use which interface

- One-shot, small or medium doc, full content needed:
  ```python
  doc = udoc.extract(path)
  ```

- Large doc, streaming page-by-page:
  ```python
  with udoc.stream(path) as ext:
      for i in range(len(ext)):
          ext.page_text(i)
  ```

- Text-only, fast:
  ```python
  cfg = udoc.Config(layers=udoc.LayerConfig(
      presentation=False, relationships=False, interactions=False))
  doc = udoc.extract(path, config=cfg)
  ```

- Tables only:
  ```bash
  udoc -t spreadsheet.xlsx
  ```
  ```python
  for table in doc.tables():
      ...
  ```

- Raw positioned spans for a PDF page (own layout analysis):
  ```python
  with udoc.stream(path) as ext:
      spans = ext.page_spans(i)   # list of (text, x, y, w, h)
  ```

- Batch ingest:
  ```python
  for d in udoc.Corpus("./docs"):
      ...
  ```

## Running udoc when it is not on PATH

If `udoc` is not installed in the current environment, run it via
[`uv`](https://docs.astral.sh/uv/):

```bash
uvx udoc <file>
```

`uvx` pulls a wheel into an ephemeral environment and runs udoc
once. Same shape works in pipelines:

```bash
curl -sL https://example.com/doc.pdf | uvx udoc -
```

CLI flags work the same with or without `uvx`.

## Common piping recipes (CLI)

```bash
udoc paper.pdf | grep -i 'attention'           # text grep
udoc -J docs/*.pdf | jq '.metadata.title'      # batch metadata
udoc -t big.xlsx | head                        # tables only
udoc -j paper.pdf | jq '.content[].text'       # iterate blocks
cat paper.pdf | udoc -                         # read from stdin
```

## Errors

Exit codes (stable across releases):

```
0  success
1  generic error
2  usage error (bad flags, missing file)
3  input error (corrupt or unsupported document)
4  permission denied (file access or PDF password)
5  resource limit hit (size, memory, page count)
```

Use `--errors json` for structured error envelopes on stderr.

Python exceptions inherit from `udoc.UdocError`. Specific subclasses:
`PasswordRequiredError`, `WrongPasswordError`, `UnsupportedFormatError`,
`LimitExceededError`, `HookError`, `IoError`, `ParseError`,
`InvalidDocumentError`. Each carries a stable `e.code` matching the
CLI exit codes.

## What udoc cannot do

- udoc is not an OCR engine. For scanned PDFs, attach an OCR hook
  (`udoc --ocr tesseract-hook scanned.pdf`).
- udoc does not edit, modify, or convert documents. It reads them.
- udoc does not produce styled HTML or DOCX round-trips. Use the
  Document model to render however you like.

## Tips

- Format detection runs from magic bytes by default. Pass `-f`
  only when bytes are inconclusive (e.g. an extensionless ZIP).
- For PDFs with non-Latin scripts, check the warnings. Font
  fallback warnings tell you when a glyph was substituted.
- The presentation overlay carries bounding boxes. Use them to
  extract region-of-interest crops for downstream models.
- Hooks are configured per-extraction. To use the same model
  across multiple files, run udoc once with `--ocr` / `--layout`
  / `--annotate` and pass all files together.

## When to reach for hooks

- Scanned or image-only PDF (no text in the content stream):
  attach an OCR hook.
- PDF with low reading-order coherence on multi-column layouts:
  attach a layout hook (DocLayout-YOLO or similar) to override
  the geometric reading order.
- Pages where you want named entities, tags, or regions stamped:
  attach an annotate hook.

By default OCR fires only on pages with fewer than 10 extracted
words. Pass `--ocr-all` to force OCR on every page.

## Hook protocol (one line)

A hook is any executable that:

1. writes one JSON line on stdout:
   `{"protocol":"udoc-hook-v1","capabilities":[...],"needs":[...],"provides":[...]}`
2. reads one JSON request per line on stdin
3. writes one JSON response per line on stdout

The hook is spawned once per extraction and reused for all pages.
