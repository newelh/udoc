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
the content stream — only images. There are two firing modes for
the OCR hook:

```python
# Mixed input: most pages digital, some scanned (insurance forms,
# legal exhibits, government archives). Default behaviour: OCR
# fires only on pages with fewer than 10 words of extracted text.
import udoc

cfg = udoc.Config(
    hooks=udoc.Hooks(ocr="tesseract-hook"),
)
doc = udoc.extract("mixed.pdf", config=cfg)
```

```bash
# Whole document is a scan, or you want a sanity-check pass on a
# digital document. Force OCR on every page.
udoc --ocr tesseract-hook --ocr-all scanned.pdf
```

The default firing rule is "OCR pages with fewer than 10
whitespace-separated words of extracted text." The threshold lives
on `HookConfig.min_words_to_skip_ocr` on the Rust runner;
`HookConfig.ocr_all_pages` (CLI: `--ocr-all`) bypasses the gate
entirely. The Python `udoc.Hooks` config currently uses the
defaults; force-on-every-page from Python today by shelling out to
`udoc --ocr ... --ocr-all` or using the Rust API directly.

Layout and annotate hooks fire on every page when configured, with
no equivalent gate. To skip pages from inside a layout / annotate
hook, return `{"error": {"kind": "unsupported", ...}}` for the
sequence — udoc passes those pages through unchanged.

The full firing-rules table and the knobs that change them live in
[Hooks / When does a hook fire?](hooks.md#when-does-a-hook-fire).

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
