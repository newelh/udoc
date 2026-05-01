# udoc-core

Format-agnostic types and traits for the [`udoc`](../udoc/) document
extraction toolkit. Defines the unified document model, the backend
trait, error types, and the diagnostics protocol.

## What

This crate is the contract between every format-specific backend and
the user-facing facade. It contains:

- The 5-layer **document model**: content spine + presentation,
  relationships, and interactions overlays + shared image store.
- The `FormatBackend` and `PageExtractor` traits that every backend
  implements.
- Core types: `Document`, `Block`, `Inline`, `NodeId`, `BoundingBox`,
  `TextSpan`, `TextLine`, `Table`, `PageImage`.
- The structured error type with context chaining.
- The `DiagnosticsSink` trait for recoverable-warning reporting.
- Codepage decoding helpers shared across legacy formats.

## When to depend on this directly

Most callers want the [`udoc`](../udoc/) facade, which re-exports
everything from `udoc-core` that you need. Depend on `udoc-core`
directly only if:

- You are implementing a custom format backend.
- You want to manipulate the `Document` model after extraction
  (rewrites, transformations) and you only need the type definitions.
- You are writing a `DiagnosticsSink` implementation that ships in
  multiple binaries and you do not want to pull in the facade's CLI
  dependencies.

## Status

This crate is part of the [udoc workspace](../../README.md). For the
alpha period distribution is via PyPI only (`pip install udoc`);
per-crate publishing to crates.io lands at beta. To use the Rust API
today, depend on the workspace by git path or build from source —
see [Compiling from source](https://newelh.github.io/udoc/compiling).

## Example

```rust
use udoc_core::geometry::BoundingBox;
use udoc_core::text::TextSpan;

let span = TextSpan {
    text: "hello".into(),
    bbox: BoundingBox { x_min: 0.0, y_min: 0.0, x_max: 30.0, y_max: 12.0 },
    font_name: None,
    font_size: 12.0,
    is_bold: false,
    is_italic: false,
    is_invisible: false,
};
assert_eq!(span.text, "hello");
```

## More

- Architecture overview: <https://newelh.github.io/udoc/architecture.html>

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or
[Apache-2.0](../../LICENSE-APACHE) at your option. Part of the
[udoc workspace](../../README.md).
