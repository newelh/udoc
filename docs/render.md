# PDF rendering & OCR

Rasterising PDF pages to PNG is part of the udoc pipeline because OCR
and layout-detection hooks need the page image. This page covers when
to use rendering, how to autodetect pages that need OCR, and how to
wire layout hooks for hard reading-order cases.

## CLI

```bash
udoc render paper.pdf -o ./pages
udoc render paper.pdf -o ./pages --dpi 300
udoc render paper.pdf -o ./pages --pages 1-5
udoc render paper.pdf -o ./pages --profile ocr
```

Output is one PNG per rendered page (`page-001.png`, `page-002.png`, …).
Default DPI is 150. Bump to 300 for OCR-quality output; the file size
roughly quadruples.

The `--profile` flag picks a rendering preset:

- `viewer` (default) — looks like a desktop PDF viewer. Sub-pixel
  glyph positioning, anti-aliasing, light hinting.
- `ocr` — flattens to the binarisation-friendly side. Stronger
  hinting, pixel-aligned glyph stems, no sub-pixel positioning.

For programmatic rendering use the `udoc::render::render_page` Rust
API or call `udoc render` from a subprocess.

## When to render

Three reasons to render in practice:

1. **OCR.** The PDF is a scan and you need text. Render at 300 DPI
   with `--profile ocr`, hand the PNGs to your OCR engine. See
   [Triggering OCR](#triggering-ocr) below for the wiring.
2. **Page thumbnails / preview UIs.** 150 DPI viewer profile, often
   downscaled.
3. **Layout-detection input.** Document-vision models like
   DocLayout-YOLO need a page image. The hook protocol passes the
   rendered PNG to the model alongside the spans we already
   extracted; the model returns labelled regions and we fold them
   back into the document.

If your goal is text extraction from a born-digital PDF (LaTeX, Word,
InDesign output), you do not need rendering. udoc parses the content
stream directly. Rendering exists for the cases where the parser
alone is not enough.

## Triggering OCR

PDFs that are scans rather than digitally-generated have no text in
the content stream — only images. A few patterns:

```python
# Force OCR on every page. Right when you know the input is a scan.
import udoc

cfg = udoc.Config(
    hooks=[udoc.Hook(phase="ocr", command="tesseract-hook", every_page=True)],
)
doc = udoc.extract("scanned.pdf", config=cfg)
```

```python
# OCR only the pages udoc thinks are scans. Right for mixed inputs
# where most pages are digital and some are scanned (insurance forms,
# legal exhibits, government archives).
cfg = udoc.Config(
    hooks=[udoc.Hook(phase="ocr", command="tesseract-hook")],
)
doc = udoc.extract("mixed.pdf", config=cfg)
```

When `every_page` is not set, the hook only fires on pages udoc has
flagged as likely scanned. The flag fires when:

- The page has at least one large image (more than 50% of page area).
- Fewer than five text spans extracted.
- Optionally, the OCG / layer dictionary indicates a scan-only layer.

Pages that meet the criteria emit a `LikelyScanned` info-level
warning before the hook decision, so you can audit detection
afterward:

```python
warnings = []
doc = udoc.extract("mixed.pdf", config=cfg, diagnostics=warnings.append)
scanned_pages = {w.page for w in warnings if w.kind == "LikelyScanned"}
print(f"OCR fired on pages: {sorted(scanned_pages)}")
```

If autodetection misses a scan udoc thought was digital, force OCR
with `every_page=True` and re-extract; if it fires false-positives
on a digital page that happens to have a big image, attach the hook
with a custom predicate via the layout phase to gate it.

## Resolution choice

| DPI  | Use for                                         | File size (US-Letter, mostly text) |
|------|-------------------------------------------------|------------------------------------|
| 96   | Quick visual checks, low-fidelity thumbnails    | ~50 KB                             |
| 150  | Default. Good for human reading, layout debug   | ~120 KB                            |
| 200  | Mid-range OCR; balance speed and accuracy       | ~210 KB                            |
| 300  | OCR ground truth, archive-grade output          | ~480 KB                            |
| 600  | OCR for difficult scans, very small print       | ~1.9 MB                            |

OCR engines benchmarked against modern Tesseract usually crest at
300 DPI; bumping to 600 helps only on small-print documents (legal
filings, receipts) where stem widths near the binarisation threshold
need the extra pixels.

## Layout hooks for hard reading order

Tier 2 of the reading-order cascade (X-Y cut, see [PDF format
guide](formats/pdf.md#reading-order)) handles most multi-column
layouts. When it does not — comic-book panels, complex spread
layouts, broken-but-correct producers — a layout-detection hook can
override the geometric reorder.

```bash
udoc --layout doclayout-yolo paper.pdf
```

The hook receives the page image plus the spans udoc has already
extracted; it returns labelled regions (`title`, `paragraph`,
`figure`, `table`, `list`). udoc uses the region order to build the
final reading order and stamps the labels on each block.

A minimal layout hook handshake declares `phase: layout`,
`needs: ["spans", "image"]`, and `provides: ["regions"]`. The
[hooks chapter](hooks.md) has the full protocol.

## Render quality nuances

Rendering is viewer-grade, not pixel-equal-to-Acrobat. Specifically:

- **Font hinting.** udoc uses TrueType VM hinting for hinted faces
  and a software auto-hinter for unhinted CFF / Type 1. The
  auto-hinter is tuned for OCR friendliness; viewer mode disables
  some of its more aggressive grid-fitting.
- **Sub-pixel positioning.** On in viewer profile, off in OCR
  profile. OCR engines binarise; sub-pixel positioning produces
  fractional-pixel stems that hurt rather than help.
- **Anti-aliasing.** Standard 8-bit grayscale coverage, no LCD/RGB
  sub-pixel AA. Produces a clean grayscale page image regardless of
  display.
- **Patterns and shadings.** Type 1 (coloured tiling), Type 2 (axial
  shading), Type 3 (radial) supported. Function-based shadings
  (Type 4, 5, 6, 7) are partial — gradients render but
  function-domain edge cases fall back.
- **Transparency.** Alpha compositing is supported; some
  transparency-group corner cases (knockout groups with
  isolated alpha) emit warnings and degrade.

For pixel-perfect output, MuPDF or Acrobat are the reference
implementations. udoc rendering targets "looks right to a human" and
"good enough for OCR / layout models", in that order.
