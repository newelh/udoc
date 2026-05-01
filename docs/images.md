# Image decoders

`udoc-image` owns format-level image decoding: turning a stream of
bytes (with a known filter chain and colourspace) into a pixel buffer.
The decoders are independent of where the image came from — a JBIG2
stream is the same regardless of whether it lived in a PDF, a TIFF
attached to an OOXML package, or a fax archive.

## Why a separate crate

PDF inline images, OOXML embedded images, and PowerPoint slide
backgrounds all hit the same decoder code paths. Lifting the heavy
decoders (JBIG2 in particular) into their own crate lets format
backends share them without each pulling in the same dependencies.
It also makes the surface independently auditable, which matters
because image decoders are common attack surface.

## What it decodes

| Format         | Module                          | Notes                                                                     |
|----------------|---------------------------------|---------------------------------------------------------------------------|
| CCITT Group 3/4 | [`ccitt`](#ccitt-group-34-fax) | T.4 1D and T.6 2D fax compression. The fastest fallback for B&W scans.    |
| JBIG2          | [`jbig2`](#jbig2)               | ISO 14492. Own decoder — no `libjbig2dec` dependency.                     |
| JPEG           | [`transcode`](#jpeg-and-jpeg-2000) | Baseline DCT via `jpeg-decoder`.                                       |
| JPEG 2000      | [`transcode`](#jpeg-and-jpeg-2000) | Via `hayro-jpeg2000`.                                                  |
| Flate + predictor | (in `udoc-pdf`)              | Transport codec; not image-specific. PNG-style row predictors handled.    |

The colourspace helpers live alongside (`colorspace` module): CMYK,
Gray, and Lab to sRGB conversion. These are non-ICC fast paths; for
calibrated colour you would hand the raw pixels to LittleCMS or
similar.

## CCITT Group 3/4 fax

CCITT decoders are the fastest path for B&W document scans —
historically the dominant image format in scanned-PDF archives. udoc
implements both 1D Group 3 (T.4) and 2D Group 4 (T.6) decoders. They
return a row-major 8-bit gray buffer (0x00 = white, 0x01 = black) so
the outer pipeline can binarise consistently.

## JBIG2

JBIG2 is the modern bitonal compression for scanned text. It uses
arithmetic coding plus a region-and-symbol model — common glyphs are
extracted as reusable symbols and re-blitted rather than re-encoded —
which gets compression ratios scanned-PDF producers love.

udoc ships its own JBIG2 decoder in `udoc-image::jbig2` rather than
linking to `libjbig2dec`. The decoder covers the arithmetic-coded
subset that real PDFs actually use: generic regions, symbol
dictionaries, text regions, halftone regions, pattern dictionaries.
Streams outside the covered subset (Huffman-coded variants of
symbol-dict and refinement aggregation are the main ones) return
`None` so the caller can emit an `UnsupportedFilter` warning and fall
back gracefully.

The decoder has been a meaningful source of fuzzing findings. Every
fixed JBIG2 finding has a regression seed in the corpus.

## JPEG and JPEG 2000

JPEG decoding wraps `jpeg-decoder` (pure-Rust, no `libjpeg`); JPEG
2000 wraps `hayro-jpeg2000`. Both produce raw pixel buffers that the
colourspace helpers convert to sRGB.

The transcoding entry point in `udoc-image::transcode` takes a raw
stream + filter chain + colourspace and produces a PNG-encoded byte
string. This is what powers `udoc images --extract` (CLI dump path)
and the OCR-hook image payload (rendered page → PNG).

## Resource budgets

Image decoders are common DoS vectors: a small input with extreme
declared dimensions can drive an unbounded allocation. udoc applies
a per-stream cap (`max_decompressed_size` on the document `Limits`)
and pre-flight checks on declared dimensions:

- **Bitmap allocation guard.** `width * height * bytes-per-pixel`
  is checked against the per-stream cap before allocation. JBIG2
  symbol dictionaries with unbounded `SDNUMNEWSYMS` are caught here.
- **Width/height integer-overflow guards.** All `width * height`
  multiplies use `u64` and are validated against `usize::MAX` before
  cast.
- **Per-page image-byte budget.** If a page declares more total
  image bytes than the budget allows, later images are skipped with
  a warning and the page text and structure still extract.

## Public surface

The crate is published mainly so the udoc backends share decoders.
The Rust API is stable enough for that purpose; for general-purpose
use, depend on more focused libraries (`image`, `jpeg-decoder`,
`tiff`).

The high-leverage entry points are:

- `decode_ccitt(data, width, height, encoding) -> Result<Vec<u8>>`
- `decode_jbig2(data, segment_kind) -> Result<Option<Bitmap>>`
- `transcode::raw_to_png(data, filter_chain, colorspace) -> Result<Vec<u8>>`

End-to-end usage flows through `udoc::extract(...)` which calls these
internally for any embedded image it encounters.
