# udoc-render

PDF page rasteriser for the [`udoc`](../udoc/) toolkit. Powers the
`udoc render` CLI subcommand. Not a full PDF renderer in the
print-fidelity sense — viewer-grade output suitable for OCR pipelines,
thumbnails, and document-vision tasks.

## What

- Path raster with a software auto-hinter.
- Font cache shared with `udoc-font`.
- Patterns, shadings, and clipping.
- PNG output at user-configurable DPI.

## When to use

The `udoc` facade re-exports the render entry points; `udoc render` on
the CLI is the common path. Depend on `udoc-render` directly only if
you are integrating rendering into something other than the standard
extractor pipeline.

## Status

This crate is part of the [udoc workspace](../../README.md). For the
alpha period distribution is via PyPI only (`pip install udoc`);
per-crate publishing to crates.io lands at beta. To use the Rust API
today, depend on the workspace by git path or build from source —
see [Compiling from source](https://newelh.github.io/udoc/compiling).

## Example

```bash
udoc render paper.pdf --page 0 --dpi 150 --out page.png
```

## Stability

Many internal modules are `#[doc(hidden)]` for the alpha.

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or
[Apache-2.0](../../LICENSE-APACHE) at your option. Part of the
[udoc workspace](../../README.md).
