//! Path-rasterization pixel goldens.
//!
//! Five hand-crafted PDFs that exercise the renderer's fill / stroke /
//! winding-rule / bezier / CTM code paths. Each PDF is committed under
//! `tests/corpus/goldens/paths/*.pdf`. The expected PNG is
//! MuPDF-rendered (at 150 DPI, same as udoc) and committed alongside.
//! At test time udoc renders the PDF and the result is compared to
//! the MuPDF reference via mean SSIM >= 0.98.
//!
//! The pentagram winding invariant is asserted in code, not via
//! pixels: the NonZero-fill ink count divided by the EvenOdd-fill
//! ink count must fall outside [0.95, 1.05].
//!
//! ### Blessing
//!
//! Run `BLESS=1 cargo test -p udoc --test golden_paths` to (re)create
//! the PDFs + reference PNGs. BLESS mode writes the hand-built PDFs
//! into the corpus directory and invokes `mutool draw -r 150` to
//! produce reference PNGs. `mutool` from mupdf must be on PATH.

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
            .join("tests/corpus/goldens/paths")
    })
}

fn bless_enabled() -> bool {
    std::env::var("BLESS").is_ok_and(|v| !v.is_empty() && v != "0" && v != "false")
}

/// Build a minimal 1-page PDF with the given content stream.
fn build_path_pdf(content: &str, media_box: Option<[u32; 4]>) -> Vec<u8> {
    let mbox = media_box.unwrap_or([0, 0, 612, 792]);
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
        "<< /Type /Page /Parent 2 0 R /MediaBox [{} {} {} {}] /Contents 4 0 R /Resources << >> >>",
        mbox[0], mbox[1], mbox[2], mbox[3]
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

/// Ensure the hand-crafted PDF file is present under the goldens
/// directory. In BLESS mode the file is rewritten; otherwise the
/// existing bytes are returned.
fn ensure_pdf(name: &str, content: &str, media_box: Option<[u32; 4]>) -> PathBuf {
    let pdf_path = goldens_dir().join(format!("{name}.pdf"));
    if bless_enabled() || !pdf_path.exists() {
        std::fs::create_dir_all(goldens_dir()).expect("mkdir goldens");
        let bytes = build_path_pdf(content, media_box);
        std::fs::write(&pdf_path, bytes).expect("write pdf");
    }
    pdf_path
}

/// In BLESS mode, invoke `mutool draw -r 150 -o <out> <pdf>` to produce
/// the reference PNG. Outside BLESS mode this is a no-op.
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

/// Render `pdf_path`, bless MuPDF reference if BLESS, then SSIM-assert.
fn assert_ssim(name: &str, pdf_path: &Path) -> (Vec<u8>, u32, u32) {
    let expected_png = goldens_dir().join(format!("{name}.expected.png"));
    bless_mupdf_reference(pdf_path, &expected_png);
    let (pixels, w, h) = render_rgb(pdf_path);
    let png = rgb_to_png(&pixels, w, h);
    assert_golden_png_ssim(name, &png, goldens_dir(), SSIM_THRESHOLD);
    (pixels, w, h)
}

// --- Golden 1: filled_square --------------------------------------

const FILLED_SQUARE_CONTENT: &str = "10 10 50 50 re f\n";

#[test]
fn golden_filled_square() {
    let pdf = ensure_pdf(
        "filled_square",
        FILLED_SQUARE_CONTENT,
        Some([0, 0, 100, 100]),
    );
    let (pixels, _, _) = assert_ssim("filled_square", &pdf);
    let inked = count_inked(&pixels);
    assert!(
        inked > 1000,
        "filled square should paint many pixels, got {inked}"
    );
}

// --- Golden 2: stroked_square_miter -------------------------------

const STROKED_SQUARE_CONTENT: &str = "5 w 0 j 20 20 m 70 20 l 70 70 l 20 70 l h S\n";

#[test]
fn golden_stroked_square_miter() {
    let pdf = ensure_pdf(
        "stroked_square_miter",
        STROKED_SQUARE_CONTENT,
        Some([0, 0, 100, 100]),
    );
    let (pixels, w, h) = assert_ssim("stroked_square_miter", &pdf);
    let inked = count_inked(&pixels);
    assert!(inked > 300, "stroked ring should have ink, got {inked}");

    let cx = (w / 2) as usize;
    let cy = (h / 2) as usize;
    let idx = (cy * w as usize + cx) * 3;
    assert!(
        pixels[idx] > 240,
        "interior of stroked-only square should be white, got {}",
        pixels[idx]
    );
}

// --- Golden 3: cubic_bezier_S -------------------------------------

const CUBIC_BEZIER_CONTENT: &str = "3 w 1 J 20 20 m 20 70 70 20 70 70 c S\n";

#[test]
fn golden_cubic_bezier_s() {
    let pdf = ensure_pdf(
        "cubic_bezier_S",
        CUBIC_BEZIER_CONTENT,
        Some([0, 0, 100, 100]),
    );
    let (pixels, _, _) = assert_ssim("cubic_bezier_S", &pdf);
    let inked = count_inked(&pixels);
    assert!(inked > 100, "bezier curve should paint pixels, got {inked}");
}

// --- Golden 4: nonzero vs even-odd star (pentagram) ---------------

fn pentagram_path(center_x: f64, center_y: f64, radius: f64) -> String {
    let mut pts: Vec<(f64, f64)> = Vec::with_capacity(5);
    for i in 0..5 {
        let angle = -std::f64::consts::FRAC_PI_2 + (i as f64) * 2.0 * std::f64::consts::PI / 5.0;
        let x = center_x + angle.cos() * radius;
        let y = center_y + angle.sin() * radius;
        pts.push((x, y));
    }
    let order = [0, 2, 4, 1, 3];
    let mut s = String::new();
    for (i, &idx) in order.iter().enumerate() {
        let (x, y) = pts[idx];
        if i == 0 {
            s.push_str(&format!("{:.3} {:.3} m\n", x, y));
        } else {
            s.push_str(&format!("{:.3} {:.3} l\n", x, y));
        }
    }
    s.push_str("h\n");
    s
}

#[test]
fn golden_nonzero_vs_eo_star() {
    let star = pentagram_path(50.0, 60.0, 40.0);

    let mut nz_content = String::new();
    nz_content.push_str(&star);
    nz_content.push_str("f\n");
    let pdf_nz = ensure_pdf("pentagram_nonzero", &nz_content, Some([0, 0, 100, 100]));
    let (pixels_nz, _, _) = assert_ssim("pentagram_nonzero", &pdf_nz);
    let nz_inked = count_inked(&pixels_nz);

    let mut eo_content = String::new();
    eo_content.push_str(&star);
    eo_content.push_str("f*\n");
    let pdf_eo = ensure_pdf("pentagram_evenodd", &eo_content, Some([0, 0, 100, 100]));
    let (pixels_eo, _, _) = assert_ssim("pentagram_evenodd", &pdf_eo);
    let eo_inked = count_inked(&pixels_eo);

    let ratio = nz_inked as f64 / eo_inked.max(1) as f64;
    assert!(
        !(0.95..=1.05).contains(&ratio),
        "pentagram NonZero vs EvenOdd ratio must differ: nz={nz_inked} eo={eo_inked} ratio={ratio:.3}"
    );
}

// --- Golden 5: CTM rotated fill -----------------------------------

const CTM_ROTATED_CONTENT: &str = concat!(
    "q\n",
    "0.7071 0.7071 -0.7071 0.7071 50 50 cm\n",
    "-10 -10 20 20 re f\n",
    "Q\n"
);

#[test]
fn golden_ctm_rotated_fill() {
    let pdf = ensure_pdf(
        "ctm_rotated_fill",
        CTM_ROTATED_CONTENT,
        Some([0, 0, 100, 100]),
    );
    let (pixels, w, h) = assert_ssim("ctm_rotated_fill", &pdf);

    let cx = (w / 2) as usize;
    let cy = (h / 2) as usize;
    let idx = (cy * w as usize + cx) * 3;
    assert!(
        pixels[idx] < 50,
        "center of rotated fill should be dark, got {}",
        pixels[idx]
    );
}
