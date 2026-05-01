# udoc-font

The font engine that the udoc PDF backend and renderer use. Parses
TrueType, CFF / OpenType, and Type 1 fonts; provides cmap and ToUnicode
resolution; bundles fallback faces.

## What

- TrueType glyph and outline parsing.
- CFF / OpenType (CFF flavour) parsing.
- Type 1 (Adobe PostScript) parsing.
- Encoding-table and cmap resolution.
- ToUnicode CMap parsing for PDF text extraction.
- Hinting infrastructure (used by `udoc-render`).
- A small set of bundled fallback faces (Liberation Serif / Sans / Mono,
  Latin Modern, Noto Sans Arabic, optional Noto Sans CJK).

## Status

This crate is part of the [udoc workspace](../../README.md). For the
alpha period distribution is via PyPI only (`pip install udoc`);
per-crate publishing to crates.io lands at beta. To use the Rust API
today, depend on the workspace by git path or build from source —
see [Compiling from source](https://newelh.github.io/udoc/compiling).

## Stability

All modules are `#[doc(hidden)]` for the alpha. The font engine is
consumed exclusively by `udoc-pdf` and `udoc-render` and is not part of
the SemVer surface library users should depend on. A proper
internal-only re-export boundary lands in a later release.

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or
[Apache-2.0](../../LICENSE-APACHE) at your option. Part of the
[udoc workspace](../../README.md).
