# udoc-image

Image decoders shared across the udoc backends. CCITT Group 3/4,
JBIG2 (own ISO 14492 decoder), JPEG, JPEG 2000, and Flate-prediction
decoders are all here.

## What

- **`ccitt`** — CCITT Group 3 1D (T.4) and Group 4 (T.6) fax decoder.
- **`jbig2`** — JBIG2 decoder per ISO 14492 (arithmetic coder plus
  segment / region parsers). Streams outside the covered subset return
  `None` so callers can emit `UnsupportedFilter` warnings.
- **`colorspace`** — CMYK / Gray / Lab to sRGB conversion helpers
  (non-ICC).
- **`transcode`** — raw-to-PNG helper used by the CLI image-dump path.

## Why this exists

Image decoding is logically independent from any one parser — a JBIG2 or
CCITT-encoded stream is the same regardless of where it came from.
Lifting these decoders out of the format backends keeps the surface
auditable on its own and lets future backends reuse the same code.

Transport-codec filters (Flate, LZW, ASCII85, RunLength) live in
`udoc-pdf` because they are not image-specific.

## Status

This crate is part of the [udoc workspace](../../README.md). For the
alpha period distribution is via PyPI only (`pip install udoc`);
per-crate publishing to crates.io lands at beta. To use the Rust API
today, depend on the workspace by git path or build from source —
see [Compiling from source](https://newelh.github.io/udoc/compiling).

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or
[Apache-2.0](../../LICENSE-APACHE) at your option. Part of the
[udoc workspace](../../README.md).
