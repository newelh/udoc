# udoc-containers

Container-format readers shared across the udoc backends. Holds the ZIP
reader (used by every OOXML and ODF backend), an XML pull-parser tuned
for the OOXML / ODF subset, the CFB / OLE2 reader (used by every legacy
Office backend), and the OPC navigator for OOXML packages.

## What

- **`zip`** — ZIP archive reader (DEFLATE, ZIP64, lenient parsing).
- **`xml`** — namespace-aware XML pull-parser.
- **`cfb`** — CFB / OLE2 compound-document reader (FAT chains,
  mini-stream).
- **`opc`** — Open Packaging Conventions navigator for OOXML packages,
  including the shared Dublin Core metadata parser.

## Why this exists

DOCX, XLSX, PPTX, ODT, ODS, and ODP all use ZIP. Word 97-2003,
Excel 97-2003, and PowerPoint 97-2003 all use CFB. Centralising those
parsers in one crate means six format backends do not each ship their
own ZIP reader.

## Status

This crate is part of the [udoc workspace](../../README.md). For the
alpha period distribution is via PyPI only (`pip install udoc`);
per-crate publishing to crates.io lands at beta. To use the Rust API
today, depend on the workspace by git path or build from source —
see [Compiling from source](https://newelh.github.io/udoc/compiling).

## Example

```rust,no_run
use udoc_containers::zip::ZipArchive;

let bytes = std::fs::read("document.docx")?;
let archive = ZipArchive::new(&bytes)?;
for entry in archive.entries() {
    println!("{}", entry.name);
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or
[Apache-2.0](../../LICENSE-APACHE) at your option. Part of the
[udoc workspace](../../README.md).
