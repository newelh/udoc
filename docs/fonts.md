# Font engine

Every PDF text-extraction problem turns into a font problem eventually.
This page covers what `udoc-font` does, what its escape hatches look
like, and the failure modes you will hit on real corpora.

## Why a font crate at all

PDF text is glyphs, not characters. The content stream draws "glyph
index 73 of font F1 at position (210, 480)"; turning that back into
the Unicode character `e` requires walking the font's encoding,
ToUnicode CMap, or — when both lie — falling back to glyph-name
heuristics. The same engine drives the renderer, where you need not
just the character but the glyph outline and hinting to put pixels
on the page.

The engine lives in `udoc-font` rather than `udoc-pdf` because PDF
shares it with the renderer (`udoc-render`) and, eventually, with any
other backend that needs typed font information (DOCX rendering, slide
thumbnails). Keeping it standalone means a project that wants only
font parsing — without dragging in the whole PDF parser — can depend
on it directly.

## What it parses

- **TrueType** (`glyf` / `loca` outlines, classic hinting via the VM).
- **CFF / OpenType-CFF** (Type 2 charstrings, including Type 1C as
  embedded in PDFs).
- **Type 1 / Adobe PostScript** (decrypted CharStrings, Subrs arrays).
- **Encoding tables** (StandardEncoding, MacRomanEncoding,
  WinAnsiEncoding, custom Differences arrays).
- **ToUnicode CMaps** (PDF `/ToUnicode` streams; Identity-H,
  Identity-V; the Adobe predefined CMaps for CJK).
- **Hinting** for grid-fitted rasterisation (TrueType VM, plus an
  auto-hinter for unhinted CFF / Type 1 faces).

## Resolution chain

Turning a glyph code into Unicode walks a defined fallback chain. Each
step that fires emits a `ToUnicodeFallback` warning at the level
indicated:

| Step                                      | Warning level | Notes                                                    |
|-------------------------------------------|---------------|----------------------------------------------------------|
| 1. ToUnicode CMap                         | (silent)      | The spec-compliant, highest-confidence path.             |
| 2. Adobe predefined CMap (CJK fonts)      | Info          | Identity-H/Identity-V or `UniJIS-UCS2-H` etc.            |
| 3. Encoding-table lookup                  | Info          | StandardEncoding / WinAnsi / MacRoman + Differences.     |
| 4. Adobe Glyph List (AGL)                 | Warning       | Glyph-name heuristic. Reliable for Latin, sketchy for CJK. |
| 5. Replacement character (U+FFFD)         | Warning       | We tried everything; the document does not have enough information. |

If you see step 4 or 5 firing on a document where text accuracy
matters, the right move is usually OCR (the rendered glyph survived;
the encoding metadata did not). See [PDF rendering &
OCR](render.md#triggering-ocr).

## Bundled fallback faces

When a document references a font by name and does not embed the
program (PDF allows this for the 14 "standard" fonts and any font
listed in `/FontDescriptor` without a `/FontFile*`), udoc reaches
into a small bundled-fallbacks directory:

| Face                       | Used as fallback for           |
|----------------------------|--------------------------------|
| Liberation Sans Regular    | Helvetica, Arial, sans-serif   |
| Liberation Serif Regular   | Times Roman, Times New Roman   |
| Liberation Mono Regular    | Courier, monospaced            |
| Liberation Sans Bold/Italic family    | Same as above with style |
| Latin Modern Roman + Italic + Math    | Mathematical notation     |
| Noto Sans Arabic           | Arabic glyphs not in Liberation |
| Noto Sans CJK SC (optional)| Chinese / Japanese / Korean fallback when a CJK font is referenced but not embedded |

The terminal fallback (Liberation Sans / Serif Regular) is always
linked and adds about 800 KB combined. Other faces are gated behind
Cargo features so a binary that knows it will only see Western
English text can drop them. The CJK feature is off by default
because it adds 2.1 MB and most pipelines do not need it; turn it
on via `--features cjk-fonts` at build time.

## Escape hatches

The font crate's public surface is intentionally small for the alpha
release. Most modules are `#[doc(hidden)]` because they are wired up
to PDF parsing and renderer details; we keep the ability to refactor
internals while the document model stabilises.

The CLI exposes per-document font listings:

```bash
udoc fonts paper.pdf
```

Output lists each font's name, kind, and ToUnicode status. The
`kind` is one of `TrueType`, `Type1`, `CFF`, `Type3`, or
`Composite` (Type 0); the ToUnicode column shows where encoding
fallbacks will fire before you read the spans.

For programmatic access, drop into `udoc_pdf::font::loader` from
Rust — the streaming Python wrapper does not currently expose
per-page font enumeration.

## Common diagnostics

| Kind                       | What it means                                                |
|----------------------------|--------------------------------------------------------------|
| `ToUnicodeMissing`         | The font has no ToUnicode CMap; the engine fell back to encoding tables. Common for ancient PDFs. |
| `EncodingDifferenceUnknown`| The font's `/Differences` array referenced a glyph name not in the AGL. |
| `GlyphNameUnknown`         | AGL fallback failed; the glyph was substituted with U+FFFD. |
| `FontProgramMalformed`     | The embedded font program was structurally invalid. Recovered to the fallback face. |
| `Type3CharProcCycle`       | A Type 3 font's CharProc references itself (rare but seen in adversarial PDFs); cycle detection broke the loop. |

## Performance notes

- **CMap parse caching.** ToUnicode CMaps and the Adobe predefined
  CMaps (Identity-H, UniJIS-UCS2-H, …) parse once per document and
  cache by `(font_obj_ref, glyph_name)`. Multiple fonts sharing the
  same predefined CMap (common in CJK documents) parse it once.
- **Per-page (font, glyph) decode cache.** A 256-entry approximate-LRU
  in `udoc-pdf::content::decode_cache` caches decoded Unicode strings
  by `(font_obj_ref, packed_code)`. Per-page rather than per-document
  to bound memory on multi-thousand-page reports.
- **Standard-font AFM widths.** udoc embeds the Adobe Font Metrics
  width tables for the 14 standard fonts so a document that
  references them without embedding does not require a full font
  program for text-extraction layout.
