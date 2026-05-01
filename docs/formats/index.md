# Format guides

udoc parses twelve formats end to end:

| Family            | Modern             | Legacy binary        |
|-------------------|--------------------|----------------------|
| Word processing   | DOCX               | DOC (Word 97-2003)   |
| Spreadsheet       | XLSX               | XLS (Excel 97-2003)  |
| Presentation      | PPTX               | PPT (PowerPoint 97-2003) |
| OpenDocument      | ODT / ODS / ODP    | —                    |
| PDF               | PDF                | —                    |
| Lightweight       | RTF, Markdown      | —                    |

Every backend produces the same `Document` shape (see the [library
guide](../library.md#the-document-model)). The pages here cover what
is *specific* to each format: capabilities, escape hatches, edge
cases, and known limitations.

## Pick a guide

| Format    | When you'd reach here                                                     |
|-----------|---------------------------------------------------------------------------|
| [PDF](pdf.md)            | Anything PDF: encryption, fonts, reading order, table detection, OCR triggering, rendering. |
| [DOCX](docx.md)          | Modern Word documents (`.docx`).                                          |
| [XLSX](xlsx.md)          | Modern Excel workbooks. Typed cells, formulas-as-text, multi-sheet.       |
| [PPTX](pptx.md)          | Modern PowerPoint decks. Shape trees, speaker notes.                      |
| [DOC (legacy)](doc.md)   | `.doc` binaries from Word 97-2003. Piece tables, fast-save fallbacks.     |
| [XLS (legacy)](xls.md)   | `.xls` BIFF8 workbooks (Excel 97-2003).                                   |
| [PPT (legacy)](ppt.md)   | `.ppt` binaries from PowerPoint 97-2003. PersistDirectory walking.        |
| [ODF](odf.md)            | LibreOffice / OpenOffice formats. ODT, ODS, ODP share one backend.        |
| [RTF](rtf.md)            | Rich Text Format. Codepage decoding, Unicode escapes.                     |
| [Markdown](markdown.md)  | CommonMark + a useful GFM subset.                                         |

## Cross-cutting topics

These live in their own pages because they are not specific to any one
format:

- [Font engine](../fonts.md) — how udoc parses TrueType / CFF / Type 1
  fonts, ToUnicode resolution, encoding fallback chains, the bundled
  fallback faces.
- [Image decoders](../images.md) — CCITT, JBIG2, JPEG, JPEG 2000.
  Used by PDF (always) and by the OOXML / ODF backends for embedded
  images.
- [PDF rendering & OCR](../render.md) — when to use `udoc render`,
  resolution choices, autodetecting scanned PDFs that need OCR,
  wiring layout-detection hooks for hard reading-order cases.

## Capabilities at a glance

What every backend does today. Empty cells mean "not implemented for
this format yet" — sometimes because the format does not have the
concept (Markdown has no pagination), sometimes because the work is
deferred to a later release.

| Format    | Text | Tables | Images | Metadata | Encrypt | Render |
|-----------|:----:|:------:|:------:|:--------:|:-------:|:------:|
| PDF       | ●    | ●      | ●      | ●        | ●       | ●      |
| DOCX      | ●    | ●      | ●      | ●        |         |        |
| XLSX      | ●    | ●      |        | ●        |         |        |
| PPTX      | ●    | ●      | ●      | ●        |         |        |
| DOC       | ●    | ●      |        | ●        |         |        |
| XLS       | ●    | ●      |        | ●        |         |        |
| PPT       | ●    | ●      |        | ●        |         |        |
| ODT       | ●    | ●      | ●      | ●        |         |        |
| ODS       | ●    | ●      |        | ●        |         |        |
| ODP       | ●    | ●      | ●      | ●        |         |        |
| RTF       | ●    | ●      |        | ●        |         |        |
| Markdown  | ●    | ●      |        | ●        |         |        |

Encrypted documents in formats marked blank fail with a structured
`PasswordRequired` error rather than partial output. Page rendering
for non-PDF formats is not currently supported — the format model
carries the geometry, but the rasterisation pipeline is PDF-only. If
you need rendering for a specific format, please [open a feature
request](https://github.com/newelh/udoc/issues).

## What "format" means in udoc

Format detection runs at the facade layer, not the backend. It looks
at magic bytes first (`%PDF-`, `PK\x03\x04`, `\xD0\xCF\x11\xE0`,
`{\rtf1`, etc.), inspects the OPC content-types entry inside ZIP
containers to distinguish DOCX from XLSX from PPTX, and only falls
back to file extension when bytes are inconclusive.

Pass `format=` (Python) or `--input-format` (CLI) to override. The
typed `Format` enum is part of the public API; agents can pin a
format when they have out-of-band knowledge.

## Where to start

If you are evaluating udoc for a specific format, the per-format
page is the right entry point. If you are doing PDF work, also
read [PDF rendering & OCR](../render.md) — it covers the
table-detection / reading-order / column-detection nuances that
matter for analytical pipelines.
