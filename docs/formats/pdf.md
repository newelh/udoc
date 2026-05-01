# PDF

The biggest, oldest, most varied format udoc handles. The PDF spec
(ISO 32000-2) is loose, real producers take liberties with it, and
extracting reliable text means handling decades of accumulated
producer quirks. udoc parses leniently, recovers from malformed
input, and reports what happened on the diagnostics sink.

## What you get

- **Text** in three tiers: `text()` (full reading-order
  reconstruction), `text_lines()` (positioned lines with baselines),
  `raw_spans()` (content-stream order, no ordering applied — the
  escape hatch when you are doing your own layout analysis or feeding
  a layout model).
- **Tables** detected via ruled-line and text-alignment heuristics.
  Tables with explicit `<<S /Table>>` tagging come through as such;
  tables that are visually-tabular text get detected and
  reconstructed.
- **Images** decoded from the page's resource dictionary, with the
  original filter chain preserved when possible. CCITT, JBIG2, JPEG,
  JPEG 2000, and Flate-based images are supported.
- **Metadata** from the document info dictionary and XMP stream when
  present.
- **Fonts** with full ToUnicode resolution. When ToUnicode is missing
  or wrong, udoc falls back to encoding-table lookup, then AGL (Adobe
  Glyph List), then the Unicode replacement character, with a
  warning at each fallback step.
- **Encryption** for the standard security handler at revisions 4
  (AES-128) and 6 (AES-256). Pass `password=` on the config.
- **Page rendering** to PNG via `udoc render`. See
  [PDF rendering & OCR](../render.md) for the detailed walkthrough.

## What you do not get

- The public-key security handler (PKCS#7 / PubKey) is not
  implemented. Documents encrypted with this handler fail with a
  structured error rather than partial output.
- Some rare optional content stream constructs and a few
  transparency-group corner cases produce warnings; the page text and
  structure are still extracted, but image fidelity may degrade.
- Inline CCITT and JBIG2 images with a `/Decode` array do not invert
  the colour mapping when applied. External CCITT and JBIG2 streams
  handle `/Decode` correctly; only inline images are affected.

If you need any of the items marked "not currently supported" or
"not implemented", please [open a feature
request](https://github.com/newelh/udoc/issues).

## Reading order

Reading order is a layout problem, not a parser problem. PDF gives
you a content stream that draws glyphs in some order chosen by the
producer; that order may or may not be the order a human reads the
page. udoc runs a four-tier cascade and surfaces which tier fired on
the diagnostics sink as a `TierSelection` info-level warning.

| Tier   | Source                                     | Used when                                                  |
|--------|--------------------------------------------|------------------------------------------------------------|
| 0      | Structure tree (tagged PDF MCIDs)          | Document is properly tagged. Highest confidence.           |
| 1      | Stream-order coherence                     | Y-monotonicity within detected column regions ≥ 0.75.      |
| 2      | X-Y cut geometric partition                | Multi-column or interleaved layouts; coherence below 0.75. |
| 3      | Y-X simple sort                            | Degenerate cases (< 3 spans).                              |

LaTeX, Word, and InDesign output usually clears the tier-1 coherence
bar without geometric reordering, which is the common fast path.
Two-column papers with figures interleaved between columns and most
scanned-then-OCRed PDFs land in tier 2 (X-Y cut). The X-Y cut
implementation pre-masks full-width spans (headers, footers) before
recursion, partitions on vertical and horizontal whitespace gaps with
a configurable minimum-gap threshold, and reorders partitions per
Breuel's spatial rules.

**When to override.** If your input is consistently mis-ordered (rare
producer that emits content in z-order, comic-book panels, complex
multi-page tables that span columns), the X-Y cut heuristic will
disagree with reader intent. The fix is a layout-detection hook: see
[PDF rendering & OCR](../render.md#layout-hooks-for-hard-reading-order)
for the wiring.

## Table detection

PDF has no first-class table type the way HTML and DOCX do. ISO
32000-2 defines a Tagged-PDF structure tree where a `<<S /Table>>`
element can wrap rows and cells, but tagging is optional and rarely
complete: most PDFs in circulation are untagged entirely, and even
tagged ones often use generic `Sect` / `Div` / `P` markers instead
of `Table` / `TR` / `TD`. More fundamentally, the structure tree is
*metadata*, not a constraint on drawing — the actual visual content
is whatever lines, glyphs, and rectangles the producer chose to draw,
and the structure tree can disagree with the rendering. A "table" in
a PDF is therefore a visual convention, not a data type. Even when a
producer embeds tagged-table metadata, udoc still has to validate it
against the geometry, because plenty of LaTeX / Word / InDesign
exports tag tables as plain text or tag plain text as tables.

Tables are recovered heuristically. Three strategies run, applied in
order and unioned at the page level:

1. **Ruled lattices.** Extract horizontal and vertical line segments
   from stroked or filled paths in the content stream, snap to a
   grid, find intersections, build cells. The strongest signal — if
   the table has visible borders, this gets it right almost always.
2. **Text-edge columns.** When ruling is sparse, cluster spans by
   x-coordinate gaps to infer column boundaries. Validates with a
   minimum-words-per-column threshold so it does not report
   left-aligned paragraph text as a one-column table. Handles
   financial tables with decimal-point alignment when the numbers
   themselves provide the column edge.
3. **Tagged tables.** Marked-content `<<S /Table>>` ranges in tagged
   PDFs come through as tables directly.

**Failure modes worth knowing about:**

- **Misaligned ruling.** Tables drawn with per-cell rectangles
  instead of full grid lines may not snap cleanly. The lattice
  detector merges collinear segments within a tolerance to handle
  this; if it still misses, fall back to text-edge detection.
- **Unbordered tables with prose-like cells.** When cells contain
  full sentences, the words-per-column threshold can suppress
  detection. If you know a region is tabular, reach for a layout
  hook to label it as such.
- **Mixed ruled and unruled tables on the same page.** Each region
  is detected independently; the output may interleave them in the
  order they appear vertically.
- **Rotated text.** Tables with rotated headers (vertical column
  labels) are detected at the cell level but rotated text is
  grouped separately in the output.

For analytical pipelines that need stricter table fidelity than the
heuristics provide, reach for the [text-edge detector
directly](../library.md#raw-spans-pdf) and run your own column
inference.

## Column detection

Column detection is the recursive step inside the X-Y cut reading
order. It detects vertical whitespace gaps spanning at least 30% of
the content height, snaps to a configurable minimum-gap floor (15
points by default — Poppler uses ~7 pt; we use a wider floor to
handle wide word spacing), and recurses up to four levels deep.
Adversarial inputs are bounded with hard limits on the number of
grid rows / columns / baselines.

The pre-masking step is what handles full-width headers and footers:
spans whose width exceeds 1.3× the median line width are removed
before recursion, then re-inserted at their natural Y position in
the output. Without this step, column detection on academic papers
with wide title blocks splits the title into one-word "columns".

## Reading order failure observability

The diagnostics sink emits one `TierSelection` info-level warning per
page with the tier that fired and (for tier 2) the partition tree
shape. Use this to identify pages that took the geometric-reorder
path and verify the output looks right:

```python
warnings = []
doc = udoc.extract("paper.pdf", diagnostics=warnings.append)

for w in warnings:
    if w.kind == "TierSelection":
        print(f"page {w.page}: tier {w.context['tier']} "
              f"(coherence={w.context.get('coherence', 'n/a')})")
```

Pages that consistently land in tier 2 with low coherence and where
the output looks wrong are candidates for a layout hook.

## Triggering OCR

PDFs that are scans rather than digitally-generated have no text in
the content stream — only images. udoc extracts an empty `text()` on
those pages and emits a `LikelyScanned` warning when:

- The page has at least one large image (> 50% of the page area).
- The page produced fewer than 5 spans of text.
- (Optionally) the OCG / layer dictionaries indicate a scan-only
  layer.

To enable OCR globally, attach an OCR hook with `--ocr <command>`
(see [hooks](../hooks.md)). To do it conditionally — most pages have
text, only some need OCR — use `--ocr-all` to force OCR on every
page or write a layout-stage decision hook that inspects the page
and chooses. Recipes are in [PDF rendering & OCR](../render.md).

## Escape hatches

When the facade is not enough, drop down a level:

```python
import udoc

with udoc.open("paper.pdf") as ext:
    page = ext.page(0)

    # Raw spans, content-stream order, no ordering heuristic.
    for span in page.raw_spans:
        print(f"({span.x:.1f},{span.y:.1f}) {span.text}")

    # Bounding box of the page.
    print(page.media_box)

    # All declared fonts on this page.
    for font in page.fonts:
        print(f"{font.name} ({font.kind})")
```

<details>
<summary>Rust: drop into <code>udoc-pdf</code> directly</summary>

```rust
let mut doc = udoc_pdf::Document::open("paper.pdf")?;
let mut page = doc.page(0)?;

for span in page.raw_spans()? {
    println!("({:.1},{:.1}) {}", span.x, span.y, span.text);
}
println!("{:?}", page.media_box());
for font in page.fonts()? {
    println!("{} ({})", font.name, font.kind);
}
# Ok::<(), udoc_pdf::Error>(())
```
</details>

The PDF backend's internal types (`PdfObject`, `PdfDictionary`,
`PdfStream`, `Lexer`, `ObjectResolver`) are public Rust API. Use them
when you need parser-level access.

## Layers within udoc-pdf

The PDF backend is the largest crate by code size. It is layered,
and layers do not skip — each strictly depends on what's below it:

```
io        random-access source: mmap, buffer, chunked
parse     lexer, object parser, xref/trailer parsing
object    object resolver, lazy loading + cycle detection, stream decoding
crypt     standard security handler (AES-128 R=4, AES-256 R=6)
font      font loading, ToUnicode, encoding, font program parsing
content   content-stream interpreter, graphics state, path construction
text      text-extraction output: spans, lines, reading order
table     ruled-line + text-edge table detection
document  public API surface (Document, Page, Config)
convert   PDF types -> unified Document model (the boundary out of the crate)
```

Each layer is independently testable. The lexer is fuzzed extensively
because it is the boundary between bytes and structured tokens; the
object resolver is fuzzed because cycle detection there is the
difference between an extraction and a stack overflow. Font program
parsing lives in `udoc-font`; this crate's `font` layer is the
glue that loads fonts referenced from PDF object dictionaries and
delegates parsing.

## Performance

Per-document time scales with content-stream size, not page count. A
1000-page PDF of pure text is roughly an order of magnitude faster
than a 100-page PDF dense with content-stream operations. The biggest
costs on most documents are content-stream interpretation and font
resolution.

To skip overlays and shave 5-15%:

```python
doc = udoc.extract("paper.pdf",
    presentation=False, relationships=False, interactions=False)
```

To skip table detection (15-20% of extraction time on
table-heavy documents):

```python
doc = udoc.extract("paper.pdf", tables=False)
```

## Common diagnostics

These show up on the diagnostics sink for typical real-world PDFs and
are usually safe to ignore:

- `StreamLengthMismatch` — the `/Length` in a stream dictionary does
  not match the actual content length. udoc scans for `endstream` and
  recovers.
- `ToUnicodeMissing` — the font lacks a ToUnicode CMap; udoc falls
  back to encoding-table or AGL lookup.
- `MalformedXref` — an xref entry is missing or malformed. udoc skips
  it and uses the rest of the table.
- `UnsupportedFilter` — a stream uses a filter udoc does not
  implement (e.g. an esoteric ASCII85 variant). The stream is
  dropped; other streams on the same page extract normally.
- `TierSelection` — info-level, fires once per page with the
  reading-order tier that ran.

For extraction-blocking issues (encrypted document with no password,
truncated trailer, etc.) the extraction returns a structured `Error`
with a stable `code()`.
