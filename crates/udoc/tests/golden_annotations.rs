//! Annotation-rendering pixel goldens.
//!
//! Three hand-crafted PDFs that exercise the annotation-composition code
//! path end-to-end:
//!
//! 1. `highlight_strikeout_underline.pdf` -- a page with all three
//!    text-markup annotations driven by /QuadPoints. The synthesised
//!    overlay paths (yellow highlight, black strikeout, black underline)
//!    should track MuPDF's output closely.
//! 2. `watermark_draft.pdf` -- a Watermark annotation whose /AP/N stream
//!    renders "DRAFT" in big gray letters centred on the page.
//! 3. `stamp_confidential.pdf` -- a Stamp annotation whose /AP/N stream
//!    renders "CONFIDENTIAL" in red inside a rounded border.
//!
//! Each golden asserts `mean SSIM >= 0.98` against the MuPDF-rendered
//! reference PNG. Run `BLESS=1` to (re)create the PDFs and bless new
//! reference PNGs via `mutool draw -r 150`.

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
            .join("tests/corpus/goldens/annotations")
    })
}

fn bless_enabled() -> bool {
    std::env::var("BLESS").is_ok_and(|v| !v.is_empty() && v != "0" && v != "false")
}

/// Build a PDF from a set of inline objects. Each object is a (number, body)
/// pair; the writer emits the cross-reference table automatically.
/// `streams[i]` is the stream payload for object `i+1`: when `Some`, the
/// object body is wrapped with `<< /Length N >>\nstream\n.\nendstream`.
fn build_pdf(objects: &[(u32, String, Option<Vec<u8>>)], root_obj: u32) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut offsets: Vec<(u32, usize)> = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");

    for (num, body, stream) in objects {
        offsets.push((*num, out.len()));
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        match stream {
            Some(data) => {
                let dict_body = if body.is_empty() {
                    format!("<< /Length {} >>", data.len())
                } else {
                    format!("<< {body} /Length {} >>", data.len())
                };
                out.extend_from_slice(dict_body.as_bytes());
                out.extend_from_slice(b"\nstream\n");
                out.extend_from_slice(data);
                out.extend_from_slice(b"\nendstream");
            }
            None => {
                out.extend_from_slice(body.as_bytes());
            }
        }
        out.extend_from_slice(b"\nendobj\n");
    }

    let xref_off = out.len();
    let n_objs = objects.iter().map(|(n, _, _)| *n).max().unwrap_or(0) + 1;
    out.extend_from_slice(format!("xref\n0 {n_objs}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \r\n");
    let mut sorted = offsets.clone();
    sorted.sort_by_key(|(n, _)| *n);
    let mut next_expected = 1u32;
    for (num, off) in sorted {
        while next_expected < num {
            // Gap filler -- should not happen for our hand-crafted PDFs.
            out.extend_from_slice(b"0000000000 65535 f \r\n");
            next_expected += 1;
        }
        out.extend_from_slice(format!("{:010} 00000 n \r\n", off).as_bytes());
        next_expected += 1;
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {n_objs} /Root {root_obj} 0 R >>\n").as_bytes(),
    );
    out.extend_from_slice(format!("startxref\n{xref_off}\n%%EOF\n").as_bytes());
    out
}

fn ensure_pdf(name: &str, bytes: Vec<u8>) -> PathBuf {
    let pdf_path = goldens_dir().join(format!("{name}.pdf"));
    if bless_enabled() || !pdf_path.exists() {
        std::fs::create_dir_all(goldens_dir()).expect("mkdir goldens");
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

fn assert_ssim(name: &str, pdf_path: &Path) -> (Vec<u8>, u32, u32) {
    let expected_png = goldens_dir().join(format!("{name}.expected.png"));
    bless_mupdf_reference(pdf_path, &expected_png);
    let (pixels, w, h) = render_rgb(pdf_path);
    let png = rgb_to_png(&pixels, w, h);
    assert_golden_png_ssim(name, &png, goldens_dir(), SSIM_THRESHOLD);
    (pixels, w, h)
}

// ---------------------------------------------------------------------
// Golden 1: highlight + strikeout + underline over three body-text spans.
// ---------------------------------------------------------------------

fn build_highlight_strikeout_underline_pdf() -> Vec<u8> {
    // Page content: three short text spans on three lines.
    // We use the standard Helvetica 12pt so the glyph appearances are
    // identical in mupdf and udoc (both fall back to Liberation Sans).
    let content = b"\
BT\n\
/F1 14 Tf\n\
72 720 Td\n\
(First line of body text) Tj\n\
0 -30 Td\n\
(Second line of body text) Tj\n\
0 -30 Td\n\
(Third line of body text) Tj\n\
ET\n";

    // QuadPoints for each annotation (BL BR TR TL is what udoc writes,
    // but the spec says UL UR LL LR; either works via our sort).
    //
    // First line is at y=720 baseline; the text goes from x=72 to ~x=240
    // at 14pt Helvetica. We use (72, 720) to (240, 735) as the quad.
    //
    // Yellow highlight on line 1, strikeout on line 2, underline on line 3.
    let qp_line1 = "[72 720 240 720 240 735 72 735]";
    let qp_line2 = "[72 690 260 690 260 705 72 705]";
    let qp_line3 = "[72 660 245 660 245 675 72 675]";

    let objs: Vec<(u32, String, Option<Vec<u8>>)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string(), None),
        (
            2,
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            None,
        ),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> \
             /Annots [6 0 R 7 0 R 8 0 R] >>"
                .to_string(),
            None,
        ),
        (4, "".to_string(), Some(content.to_vec())),
        (
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            None,
        ),
        (
            6,
            format!(
                "<< /Type /Annot /Subtype /Highlight /Rect [72 720 240 735] \
                 /QuadPoints {qp_line1} /C [1 1 0] /F 4 >>"
            ),
            None,
        ),
        (
            7,
            format!(
                "<< /Type /Annot /Subtype /StrikeOut /Rect [72 690 260 705] \
                 /QuadPoints {qp_line2} /C [0 0 0] /F 4 >>"
            ),
            None,
        ),
        (
            8,
            format!(
                "<< /Type /Annot /Subtype /Underline /Rect [72 660 245 675] \
                 /QuadPoints {qp_line3} /C [0 0 0] /F 4 >>"
            ),
            None,
        ),
    ];
    build_pdf(&objs, 1)
}

#[test]
fn golden_highlight_strikeout_underline() {
    let pdf_bytes = build_highlight_strikeout_underline_pdf();
    let pdf = ensure_pdf("highlight_strikeout_underline", pdf_bytes);
    let (pixels, _, _) = assert_ssim("highlight_strikeout_underline", &pdf);
    // Sanity: the page must contain ink (both text and overlays).
    let inked = pixels
        .chunks_exact(3)
        .filter(|c| c.iter().any(|&b| b < 250))
        .count();
    assert!(inked > 1000, "markup page should have ink, got {inked}");
}

// ---------------------------------------------------------------------
// Golden 2: a Watermark annotation with an AP stream drawing "DRAFT".
// ---------------------------------------------------------------------

fn build_watermark_draft_pdf() -> Vec<u8> {
    // Body text on the page.
    let body = b"\
BT\n\
/F1 12 Tf\n\
72 720 Td\n\
(Body text on the page, under the watermark.) Tj\n\
ET\n";

    // AP/N stream for the watermark: draw "DRAFT" at 48pt gray.
    let ap = b"\
q\n\
0.5 0.5 0.5 rg\n\
BT\n\
/F1 48 Tf\n\
40 40 Td\n\
(DRAFT) Tj\n\
ET\n\
Q\n";

    let objs: Vec<(u32, String, Option<Vec<u8>>)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string(), None),
        (
            2,
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            None,
        ),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> \
             /Annots [6 0 R] >>"
                .to_string(),
            None,
        ),
        (4, "".to_string(), Some(body.to_vec())),
        (
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            None,
        ),
        (
            6,
            "<< /Type /Annot /Subtype /Watermark /Rect [200 400 412 460] \
             /AP << /N 7 0 R >> /F 4 >>"
                .to_string(),
            None,
        ),
        (
            7,
            "/Type /XObject /Subtype /Form /BBox [0 0 200 56] \
             /Resources << /Font << /F1 5 0 R >> >>"
                .to_string(),
            Some(ap.to_vec()),
        ),
    ];
    build_pdf(&objs, 1)
}

#[test]
fn golden_watermark_draft() {
    let pdf_bytes = build_watermark_draft_pdf();
    let pdf = ensure_pdf("watermark_draft", pdf_bytes);
    let (pixels, _, _) = assert_ssim("watermark_draft", &pdf);
    let inked = pixels
        .chunks_exact(3)
        .filter(|c| c.iter().any(|&b| b < 250))
        .count();
    assert!(inked > 500, "watermark page should have ink, got {inked}");
}

// ---------------------------------------------------------------------
// Golden 3: a Stamp annotation with an AP stream drawing "CONFIDENTIAL".
// ---------------------------------------------------------------------

fn build_stamp_confidential_pdf() -> Vec<u8> {
    let body = b"\
BT\n\
/F1 12 Tf\n\
72 720 Td\n\
(Internal memo. Do not distribute.) Tj\n\
ET\n";

    // AP/N stream for the stamp: red border box + "CONFIDENTIAL" text.
    let ap = b"\
q\n\
0.8 0 0 RG\n\
2 w\n\
5 5 230 40 re\n\
S\n\
0.8 0 0 rg\n\
BT\n\
/F1 22 Tf\n\
15 18 Td\n\
(CONFIDENTIAL) Tj\n\
ET\n\
Q\n";

    let objs: Vec<(u32, String, Option<Vec<u8>>)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string(), None),
        (
            2,
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            None,
        ),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> \
             /Annots [6 0 R] >>"
                .to_string(),
            None,
        ),
        (4, "".to_string(), Some(body.to_vec())),
        (
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            None,
        ),
        (
            6,
            "<< /Type /Annot /Subtype /Stamp /Rect [180 520 420 570] \
             /AP << /N 7 0 R >> /F 4 >>"
                .to_string(),
            None,
        ),
        (
            7,
            "/Type /XObject /Subtype /Form /BBox [0 0 240 50] \
             /Resources << /Font << /F1 5 0 R >> >>"
                .to_string(),
            Some(ap.to_vec()),
        ),
    ];
    build_pdf(&objs, 1)
}

#[test]
fn golden_stamp_confidential() {
    let pdf_bytes = build_stamp_confidential_pdf();
    let pdf = ensure_pdf("stamp_confidential", pdf_bytes);
    let (pixels, _, _) = assert_ssim("stamp_confidential", &pdf);
    let inked = pixels
        .chunks_exact(3)
        .filter(|c| c.iter().any(|&b| b < 250))
        .count();
    assert!(inked > 500, "stamp page should have ink, got {inked}");
}

// ---------------------------------------------------------------------
// Unit test: /F bit 2 (Hidden) suppresses rendering.
// ---------------------------------------------------------------------

#[test]
fn hidden_annotation_is_skipped() {
    let body = b"BT /F1 12 Tf 72 720 Td (visible body text) Tj ET\n";
    let ap = b"q 0 0 0 rg BT /F1 48 Tf 0 0 Td (HIDDEN) Tj ET Q";
    let objs: Vec<(u32, String, Option<Vec<u8>>)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string(), None),
        (
            2,
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            None,
        ),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> \
             /Annots [6 0 R] >>"
                .to_string(),
            None,
        ),
        (4, "".to_string(), Some(body.to_vec())),
        (
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            None,
        ),
        (
            6,
            // /F = 2 sets the Hidden bit (bit 2). Annotation must not
            // produce any ink or spans on the rendered page.
            "<< /Type /Annot /Subtype /Stamp /Rect [100 100 500 200] \
             /AP << /N 7 0 R >> /F 2 >>"
                .to_string(),
            None,
        ),
        (
            7,
            "/Type /XObject /Subtype /Form /BBox [0 0 400 100] \
             /Resources << /Font << /F1 5 0 R >> >>"
                .to_string(),
            Some(ap.to_vec()),
        ),
    ];
    let pdf_bytes = build_pdf(&objs, 1);
    let tmp = std::env::temp_dir().join("udoc-hidden-annot-test.pdf");
    std::fs::write(&tmp, pdf_bytes).expect("write tmp pdf");

    let doc = udoc::extract(&tmp).expect("extract must succeed");
    let pres = doc.presentation.as_ref().expect("presentation expected");
    // No paint_paths should live in the annotation z-band (u32::MAX/2 ..).
    let annot_band_start: u32 = u32::MAX / 2;
    let has_annot_paths = pres
        .paint_paths
        .iter()
        .any(|p| p.z_index >= annot_band_start);
    assert!(
        !has_annot_paths,
        "hidden annotation emitted {} paint_paths in the annotation z-band",
        pres.paint_paths
            .iter()
            .filter(|p| p.z_index >= annot_band_start)
            .count(),
    );
    let has_annot_spans = pres.raw_spans.iter().any(|s| s.z_index >= annot_band_start);
    assert!(
        !has_annot_spans,
        "hidden annotation emitted spans in the annotation z-band"
    );

    let _ = std::fs::remove_file(&tmp);
}
