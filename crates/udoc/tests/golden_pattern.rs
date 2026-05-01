//! Tiling pattern pixel goldens (
//! ISO 32000-2 §8.7.3).
//!
//! A hand-crafted PDF exercises the Type 1 coloured tiling pattern
//! rasterizer: a single page whose `/Resources /ColorSpace /Cs1
//! /Pattern` and `/Resources /Pattern /P1` combine with an `scn` op
//! to paint a rectangle with a pattern fill. The tile's content
//! stream paints a solid red 10x10 square, so the fill region should
//! read as uniformly red at 150 DPI.
//!
//! Committed under `tests/corpus/goldens/pages/tiling_pattern.pdf`
//! with a MuPDF-rendered reference PNG. `assert_golden_png_ssim`
//! checks udoc's render against the reference at SSIM >= 0.98.
//!
//! ### Blessing
//!
//! `BLESS=1 cargo test -p udoc --test golden_pattern` rewrites the
//! PDF and invokes `mutool draw -r 150` to regenerate the reference
//! PNG.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use udoc::render::font_cache::FontCache;
use udoc::render::render_page_rgb;
use udoc_core::test_harness::assert_golden_png_ssim;

const SSIM_THRESHOLD: f64 = 0.98;
const DPI: u32 = 150;

fn goldens_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root reachable")
            .join("tests/corpus/goldens/pages")
    })
}

fn bless_enabled() -> bool {
    std::env::var("BLESS").is_ok_and(|v| !v.is_empty() && v != "0" && v != "false")
}

/// Minimal 1-page PDF with a Type 1 coloured tiling pattern.
///
/// Object layout:
/// * 1 = Catalog
/// * 2 = Pages
/// * 3 = Page (references content stream 4 and pattern 5)
/// * 4 = Content stream: set up Pattern CS, bind /P1, fill a rect
/// * 5 = Pattern (Type 1 coloured, tile paints solid red)
///
/// The tile bbox is 10x10 in pattern space; xstep = ystep = 10 so
/// the tile repeats seamlessly. The pattern fills a 100x80 rectangle
/// on a 200x120 page. The rest of the page is white.
fn build_tiling_pattern_pdf() -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();
    let mut offsets: Vec<(u32, usize)> = Vec::new();

    buf.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");

    let push = |buf: &mut Vec<u8>, offsets: &mut Vec<(u32, usize)>, num: u32, body: &str| {
        offsets.push((num, buf.len()));
        buf.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        buf.extend_from_slice(body.as_bytes());
        buf.extend_from_slice(b"\nendobj\n");
    };

    push(
        &mut buf,
        &mut offsets,
        1,
        "<< /Type /Catalog /Pages 2 0 R >>",
    );
    push(
        &mut buf,
        &mut offsets,
        2,
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    );
    // Page dict: 200x120, inline colorspace + pattern resources.
    push(
        &mut buf,
        &mut offsets,
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 120] \
           /Contents 4 0 R \
           /Resources << \
             /ColorSpace << /Cs1 [/Pattern] >> \
             /Pattern << /P1 5 0 R >> \
           >> >>",
    );

    // Content stream: paint the 100x80 rectangle with pattern P1.
    // `50 20 100 80 re` establishes the rect; `f` fills with the
    // active non-stroke fill (our pattern).
    let content = "q\n/Cs1 cs\n/P1 scn\n50 20 100 80 re\nf\nQ\n";
    let content_bytes = content.as_bytes();
    offsets.push((4, buf.len()));
    buf.extend_from_slice(
        format!("4 0 obj\n<< /Length {} >>\nstream\n", content_bytes.len()).as_bytes(),
    );
    buf.extend_from_slice(content_bytes);
    buf.extend_from_slice(b"\nendstream\nendobj\n");

    // Pattern object: Type 1 coloured, 10x10 tile, paints solid red.
    // Tile content stream: `1 0 0 rg 0 0 10 10 re f`.
    let tile = "1 0 0 rg\n0 0 10 10 re\nf\n";
    let tile_bytes = tile.as_bytes();
    offsets.push((5, buf.len()));
    buf.extend_from_slice(
        format!(
            "5 0 obj\n<< /Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 \
             /BBox [0 0 10 10] /XStep 10 /YStep 10 \
             /Matrix [1 0 0 1 0 0] \
             /Resources << >> \
             /Length {} >>\nstream\n",
            tile_bytes.len()
        )
        .as_bytes(),
    );
    buf.extend_from_slice(tile_bytes);
    buf.extend_from_slice(b"\nendstream\nendobj\n");

    // Xref + trailer.
    let xref_off = buf.len();
    let n_objs = 6;
    buf.extend_from_slice(format!("xref\n0 {}\n", n_objs).as_bytes());
    buf.extend_from_slice(b"0000000000 65535 f \r\n");
    let mut sorted: Vec<(u32, usize)> = offsets.clone();
    sorted.sort_by_key(|(n, _)| *n);
    for (_, off) in &sorted {
        buf.extend_from_slice(format!("{:010} 00000 n \r\n", off).as_bytes());
    }
    buf.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\n", n_objs).as_bytes());
    buf.extend_from_slice(format!("startxref\n{}\n%%EOF\n", xref_off).as_bytes());

    buf
}

fn ensure_pdf() -> PathBuf {
    let dir = goldens_dir();
    std::fs::create_dir_all(dir).expect("mkdir goldens");
    let pdf = dir.join("tiling_pattern.pdf");
    if bless_enabled() || !pdf.exists() {
        std::fs::write(&pdf, build_tiling_pattern_pdf()).expect("write pdf");
    }
    pdf
}

fn bless_reference(pdf: &Path, png: &Path) {
    if !bless_enabled() {
        return;
    }
    let status = std::process::Command::new("mutool")
        .args([
            "draw",
            "-r",
            &format!("{DPI}"),
            "-o",
            png.to_str().unwrap(),
            pdf.to_str().unwrap(),
        ])
        .status()
        .expect("failed to invoke mutool -- install mupdf or unset BLESS");
    assert!(status.success(), "mutool draw failed for {pdf:?}");
}

fn render_rgb(pdf: &Path) -> (Vec<u8>, u32, u32) {
    let doc = udoc::extract(pdf).expect("extract must succeed");
    let mut cache = FontCache::new(&doc.assets);
    render_page_rgb(&doc, 0, DPI, &mut cache).expect("render must succeed")
}

fn rgb_to_png(pixels: &[u8], w: u32, h: u32) -> Vec<u8> {
    udoc::render::png::encode_rgb_png(pixels, w, h)
}

fn count_red(pixels: &[u8]) -> usize {
    pixels
        .chunks_exact(3)
        .filter(|c| c[0] > 200 && c[1] < 80 && c[2] < 80)
        .count()
}

#[test]
fn golden_tiling_pattern() {
    let pdf = ensure_pdf();
    let expected = goldens_dir().join("tiling_pattern.expected.png");
    bless_reference(&pdf, &expected);

    // Sanity: the pattern must come through the presentation overlay
    // with a populated fill region. Without this assertion a silent
    // regression (e.g. name_refers_to_pattern_cs broken) would still
    // produce a green SSIM check via the fallback shape pipeline.
    {
        let doc = udoc::extract(&pdf).expect("extract");
        let pres = doc.presentation.as_ref().expect("presentation");
        assert_eq!(pres.patterns.len(), 1, "expected one PaintPattern");
        let p = &pres.patterns[0];
        assert_eq!(p.resource_name, "P1");
        assert_eq!(p.xstep, 10.0);
        assert_eq!(p.ystep, 10.0);
        assert!(!p.fill_subpaths.is_empty(), "fill region not captured");
    }

    let (pixels, w, h) = render_rgb(&pdf);
    let png = rgb_to_png(&pixels, w, h);

    // Invariant: the fill region should paint many red pixels. The
    // 100x80 user-space rect at 150 DPI is ~208x166 px, and with AA
    // coverage we expect close to 30K solidly-red pixels.
    let red_count = count_red(&pixels);
    assert!(
        red_count > 15_000,
        "expected pattern fill to paint red pixels, got {red_count}; \
         image {w}x{h}"
    );

    if expected.exists() {
        assert_golden_png_ssim("tiling_pattern", &png, goldens_dir(), SSIM_THRESHOLD);
    } else {
        eprintln!(
            "warning: {expected:?} missing; run BLESS=1 to generate \
             the MuPDF reference"
        );
    }
}
