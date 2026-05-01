//! Gradient-rasterization pixel goldens.
//!
//! Two hand-crafted PDFs exercise the shading rasterizer:
//!
//! * `linear_gradient.pdf` -- Type 2 (axial) shading with a 3-stop
//!   stitching function (red -> white -> blue) across the page.
//! * `radial_gradient.pdf` -- Type 3 (radial) shading from a 0-radius
//!   center to a 40-point edge circle with a 2-stop type-2 function
//!   (blue center -> yellow edge).
//!
//! Each PDF is committed under `tests/corpus/goldens/gradients/*.pdf`
//! with a MuPDF-rendered reference PNG at 150 DPI. `assert_golden_png_ssim`
//! checks udoc's render against the reference at SSIM >= 0.97.
//!
//! ### Blessing
//!
//! `BLESS=1 cargo test -p udoc --test golden_gradients` rewrites the
//! PDFs and invokes `mutool draw -r 150` to regenerate the reference
//! PNGs. `mutool` must be on PATH.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use udoc::render::font_cache::FontCache;
use udoc::render::render_page_rgb;
use udoc_core::test_harness::assert_golden_png_ssim;

const SSIM_THRESHOLD: f64 = 0.97;
const DPI: u32 = 150;

fn goldens_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root reachable")
            .join("tests/corpus/goldens/gradients")
    })
}

fn bless_enabled() -> bool {
    std::env::var("BLESS").is_ok_and(|v| !v.is_empty() && v != "0" && v != "false")
}

/// Build a minimal 1-page PDF with an inline /Resources dict containing
/// /Shading. The `resources` string should be the dictionary contents
/// for the page /Resources entry, excluding the outer `<< >>`.
fn build_pdf_with_shading(resources: &str, content: &str, media_box: [u32; 4]) -> Vec<u8> {
    let mut obj_offsets: Vec<(u32, usize)> = Vec::new();
    let mut buf: Vec<u8> = Vec::new();

    buf.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");

    let push_obj = |buf: &mut Vec<u8>, offsets: &mut Vec<(u32, usize)>, num: u32, body: &str| {
        offsets.push((num, buf.len()));
        buf.extend_from_slice(format!("{} 0 obj\n", num).as_bytes());
        buf.extend_from_slice(body.as_bytes());
        buf.extend_from_slice(b"\nendobj\n");
    };

    push_obj(
        &mut buf,
        &mut obj_offsets,
        1,
        "<< /Type /Catalog /Pages 2 0 R >>",
    );
    push_obj(
        &mut buf,
        &mut obj_offsets,
        2,
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    );
    let page_obj = format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox [{} {} {} {}] /Contents 4 0 R /Resources << {resources} >> >>",
        media_box[0], media_box[1], media_box[2], media_box[3]
    );
    push_obj(&mut buf, &mut obj_offsets, 3, &page_obj);

    let content_bytes = content.as_bytes();
    obj_offsets.push((4, buf.len()));
    buf.extend_from_slice(
        format!("4 0 obj\n<< /Length {} >>\nstream\n", content_bytes.len()).as_bytes(),
    );
    buf.extend_from_slice(content_bytes);
    buf.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_off = buf.len();
    let n_objs = 5;
    buf.extend_from_slice(format!("xref\n0 {}\n", n_objs).as_bytes());
    buf.extend_from_slice(b"0000000000 65535 f \r\n");
    let mut sorted: Vec<(u32, usize)> = obj_offsets.clone();
    sorted.sort_by_key(|(n, _)| *n);
    for (_, off) in &sorted {
        buf.extend_from_slice(format!("{:010} 00000 n \r\n", off).as_bytes());
    }
    buf.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\n", n_objs).as_bytes());
    buf.extend_from_slice(format!("startxref\n{}\n%%EOF\n", xref_off).as_bytes());

    buf
}

fn ensure_pdf(name: &str, resources: &str, content: &str, media_box: [u32; 4]) -> PathBuf {
    let pdf_path = goldens_dir().join(format!("{name}.pdf"));
    if bless_enabled() || !pdf_path.exists() {
        std::fs::create_dir_all(goldens_dir()).expect("mkdir goldens");
        let bytes = build_pdf_with_shading(resources, content, media_box);
        std::fs::write(&pdf_path, bytes).expect("write pdf");
    }
    pdf_path
}

fn bless_mupdf_reference(pdf_path: &Path, expected_png: &Path) {
    if !bless_enabled() {
        return;
    }
    let status = std::process::Command::new("mutool")
        .args([
            "draw",
            "-r",
            &format!("{DPI}"),
            "-o",
            expected_png.to_str().unwrap(),
            pdf_path.to_str().unwrap(),
        ])
        .status()
        .expect("failed to invoke mutool -- install mupdf or unset BLESS");
    assert!(status.success(), "mutool draw failed for {pdf_path:?}");
}

fn render_rgb(pdf_path: &Path) -> (Vec<u8>, u32, u32) {
    let doc = udoc::extract(pdf_path).expect("extract must succeed");
    let mut cache = FontCache::new(&doc.assets);
    render_page_rgb(&doc, 0, DPI, &mut cache).expect("render must succeed")
}

fn rgb_to_png(pixels: &[u8], w: u32, h: u32) -> Vec<u8> {
    udoc::render::png::encode_rgb_png(pixels, w, h)
}

fn count_inked(pixels: &[u8]) -> usize {
    pixels
        .chunks_exact(3)
        .filter(|c| c.iter().any(|&b| b < 250))
        .count()
}

fn assert_ssim(name: &str, pdf_path: &Path) -> (Vec<u8>, u32, u32) {
    let expected_png = goldens_dir().join(format!("{name}.expected.png"));
    bless_mupdf_reference(pdf_path, &expected_png);
    let (pixels, w, h) = render_rgb(pdf_path);
    let png = rgb_to_png(&pixels, w, h);
    assert_golden_png_ssim(name, &png, goldens_dir(), SSIM_THRESHOLD);
    (pixels, w, h)
}

// --- Golden 1: linear_gradient (Type 2 axial, 3-stop stitching) ------

/// 200x100 page. Axial gradient from left edge (red) through the middle
/// (white) to the right edge (blue). The Type 3 stitching /Function is
/// two Type 2 legs: [red->white] over [0, 0.5], [white->blue] over
/// [0.5, 1].
const LINEAR_GRADIENT_RESOURCES: &str = "/Shading << /Sh1 \
    << /ShadingType 2 \
       /ColorSpace /DeviceRGB \
       /Coords [0 0 200 0] \
       /Extend [true true] \
       /Function << /FunctionType 3 \
                    /Domain [0 1] \
                    /Bounds [0.5] \
                    /Encode [0 1 0 1] \
                    /Functions [ \
                       << /FunctionType 2 /Domain [0 1] /N 1 /C0 [1 0 0] /C1 [1 1 1] >> \
                       << /FunctionType 2 /Domain [0 1] /N 1 /C0 [1 1 1] /C1 [0 0 1] >> \
                    ] \
                 >> \
    >> \
>>";

#[test]
fn golden_linear_gradient() {
    let pdf = ensure_pdf(
        "linear_gradient",
        LINEAR_GRADIENT_RESOURCES,
        "/Sh1 sh\n",
        [0, 0, 200, 100],
    );
    let (pixels, w, _) = assert_ssim("linear_gradient", &pdf);
    let inked = count_inked(&pixels);
    assert!(
        inked > 1000,
        "linear gradient should paint lots of non-white pixels, got {inked}"
    );

    // Invariant: left edge should be dominantly red; right edge dominantly blue.
    // Sample middle row.
    let w_us = w as usize;
    let mid_row_y = 50;
    let left_idx = (mid_row_y * w_us + 5) * 3; // 5 px in from left
    let right_idx = (mid_row_y * w_us + w_us - 6) * 3; // 5 px in from right
    let lr = pixels[left_idx];
    let lg = pixels[left_idx + 1];
    let lb = pixels[left_idx + 2];
    let rr = pixels[right_idx];
    let rg = pixels[right_idx + 1];
    let rb = pixels[right_idx + 2];
    assert!(
        lr > 200 && lg < 60 && lb < 60,
        "left should be red-dominant, got rgb=({lr},{lg},{lb})"
    );
    assert!(
        rb > 200 && rr < 60 && rg < 60,
        "right should be blue-dominant, got rgb=({rr},{rg},{rb})"
    );
}

// --- Golden 2: radial_gradient (Type 3 radial, 2-stop Type 2) --------

/// 100x100 page. Radial gradient from center (blue) to edge circle
/// (yellow). r0=0, r1=40 puts the gradient inside most of the page.
const RADIAL_GRADIENT_RESOURCES: &str = "/Shading << /Sh1 \
    << /ShadingType 3 \
       /ColorSpace /DeviceRGB \
       /Coords [50 50 0 50 50 40] \
       /Extend [true true] \
       /Function << /FunctionType 2 /Domain [0 1] /N 1 \
                    /C0 [0 0 1] /C1 [1 1 0] \
                 >> \
    >> \
>>";

#[test]
fn golden_radial_gradient() {
    let pdf = ensure_pdf(
        "radial_gradient",
        RADIAL_GRADIENT_RESOURCES,
        "/Sh1 sh\n",
        [0, 0, 100, 100],
    );
    let (pixels, w, h) = assert_ssim("radial_gradient", &pdf);

    let inked = count_inked(&pixels);
    assert!(
        inked > 1000,
        "radial gradient should paint lots of pixels, got {inked}"
    );

    // Center pixel should be blue-dominant.
    let cx = (w / 2) as usize;
    let cy = (h / 2) as usize;
    let idx = (cy * w as usize + cx) * 3;
    let cr = pixels[idx];
    let cg = pixels[idx + 1];
    let cb = pixels[idx + 2];
    assert!(
        cb > 150 && cr < 90,
        "center should be blue-dominant, got rgb=({cr},{cg},{cb})"
    );
}
