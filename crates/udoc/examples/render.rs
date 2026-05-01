//! Render the first page of a PDF to a PNG byte buffer.
//!
//! Demonstrates the rendering API: open with `assets.fonts = true` so font
//! programs are retained, build a [`FontCache`](udoc::render::font_cache::FontCache),
//! call [`udoc::render::render_page`] to get encoded PNG bytes.
//!
//! Run with:
//!
//! ```text
//! cargo run -p udoc --example render
//! ```
//!
//! Override the fixture path with the first argument:
//!
//! ```text
//! cargo run -p udoc --example render -- path/to/your.pdf
//! ```
//!
//! Writes the rendered PNG to a temp file and asserts it has a valid PNG
//! magic header and non-trivial size, so `cargo test --examples` exercises
//! the full extract -> render -> encode pipeline.

use std::path::PathBuf;

use udoc::render::font_cache::FontCache;
use udoc::{render, Config};

/// Rendering DPI. 150 is the default OCR-friendly setting; 300 is also
/// supported. Lower DPIs render faster.
const DPI: u32 = 150;

/// PNG file signature. First 8 bytes of every PNG file.
const PNG_MAGIC: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

fn default_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/udoc -> crates")
        .parent()
        .expect("crates -> repo root")
        .join("crates/udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(default_fixture);

    println!("rendering page 0 of {} at {DPI} DPI", path.display());

    // The renderer needs font assets to draw glyphs. Without this opt-in,
    // assets.fonts is filtered to None to keep extract-only callers cheap.
    let mut config = Config::new();
    config.assets.fonts = true;

    let doc = udoc::extract_with(&path, config)?;
    assert!(
        doc.metadata.page_count > 0,
        "fixture has no pages, cannot render"
    );

    // FontCache is reused across pages to avoid reparsing font programs
    // on every render call. Construct once per Document.
    let mut font_cache = FontCache::new(&doc.assets);

    let png: Vec<u8> = render::render_page(&doc, 0, DPI, &mut font_cache)?;

    // Persist to a tmp file so the user can eyeball the output. Errors are
    // best-effort; the assertion is the real gate.
    let out = std::env::temp_dir().join(format!("udoc-example-render-{}.png", std::process::id()));
    if let Err(e) = std::fs::write(&out, &png) {
        eprintln!("warning: could not write {}: {e}", out.display());
    } else {
        println!("wrote {} bytes to {}", png.len(), out.display());
    }

    // Smoke assertions: PNG magic + nontrivial size.
    assert!(
        png.len() > 1000,
        "rendered PNG suspiciously small: {} bytes",
        png.len()
    );
    assert_eq!(
        &png[..8],
        PNG_MAGIC,
        "output is not a PNG (bad magic header)"
    );

    println!("ok: {} byte PNG, valid signature", png.len());

    Ok(())
}

#[cfg(test)]
mod tests {
    /// Drive `main()` from a test so `cargo test --examples` exercises the
    /// full render pipeline + assertions, not just a compile check.
    #[test]
    fn example_runs() {
        super::main().expect("render example should succeed");
    }
}
