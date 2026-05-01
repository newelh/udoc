//! Per-page pixel goldens (SSIM >= 0.98 threshold).
//!
//! Renders one page per document from `tests/corpus/downloaded/` at 150 DPI
//! and compares against blessed PNG fixtures using mean SSIM over 8x8
//! luminance windows (Rec.709 weights). The eight categories are deliberate:
//! each exercises a different renderer code path so a regression shows up
//! on the most relevant page.
//!
//! Run `BLESS=1 cargo test --release -p udoc --test golden_pages -- --include-ignored`
//! to (re)create fixtures; plain `cargo test --release ... --include-ignored`
//! enforces SSIM >= 0.98 per page.
//!
//! Tests are `#[ignore]` by default because debug-mode PDF parsing is slow
//! (~50s for all eight). Release mode finishes in ~5s. CI runs them with
//! `--include-ignored` in release mode; the pre-commit hook skips them.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use udoc::render::font_cache::FontCache;
use udoc::render::{render_page_with_profile, RenderingProfile};
use udoc_core::test_harness::assert_golden_png_ssim;

const SSIM_THRESHOLD: f64 = 0.98;
const DPI: u32 = 150;

fn golden_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/pages"))
}

fn corpus_root() -> PathBuf {
    // Workspace root = two levels up from crates/udoc/.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/udoc has a parent")
        .join("tests/corpus/downloaded")
}

/// Same layout but rooted at the synthetic goldens corpus (small hand-built
/// PDFs committed under `tests/corpus/goldens/pages/`).
fn goldens_pages_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/udoc has a parent")
        .join("tests/corpus/goldens/pages")
}

/// Extended corpus root: used by the Noto Arabic
/// routing golden against `pdfjs-repo/test/pdfs/ArabicCIDTrueType.pdf`.
fn extended_corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/udoc has a parent")
        .join("tests/corpus/extended")
}

fn render_goldens_page(rel: &str, page: usize) -> Vec<u8> {
    let path = goldens_pages_root().join(rel);
    assert!(path.exists(), "goldens file missing: {path:?}");
    let doc = udoc::extract(&path).expect("extract should succeed");
    let mut cache = FontCache::new(&doc.assets);
    render_page_with_profile(&doc, page, DPI, &mut cache, RenderingProfile::Visual)
        .expect("render should succeed")
}

fn assert_goldens_page(name: &str, rel: &str, page: usize) {
    let png = render_goldens_page(rel, page);
    assert_golden_png_ssim(name, &png, golden_dir(), SSIM_THRESHOLD);
}

fn render_corpus_page(rel: &str, page: usize) -> Vec<u8> {
    let path = corpus_root().join(rel);
    assert!(path.exists(), "corpus file missing: {path:?}");
    let doc = udoc::extract(&path).expect("extract should succeed");
    let mut cache = FontCache::new(&doc.assets);
    // Goldens were blessed under the `Visual` profile (M-39 x-height align on)
    // so they are the tripwire for FT-NORMAL regressions. The shipping
    // default is OcrFriendly which disables M-39 and would drift
    // below the 0.98 SSIM threshold on a CID TrueType page.
    render_page_with_profile(&doc, page, DPI, &mut cache, RenderingProfile::Visual)
        .expect("render should succeed")
}

fn assert_page_golden(name: &str, rel: &str, page: usize) {
    let png = render_corpus_page(rel, page);
    assert_golden_png_ssim(name, &png, golden_dir(), SSIM_THRESHOLD);
}

/// Render under the explicit `OcrFriendly` profile ( default,
/// re-asserted in after M-40 stayed default-off). Picks
/// up the OcrFriendly arm of `decide_enable_x_axis` and the M-39
/// x-height-align disable. uses this for the
/// `ocr_friendly_profile_dense_text` tripwire.
fn render_goldens_page_ocr(rel: &str, page: usize) -> Vec<u8> {
    let path = goldens_pages_root().join(rel);
    assert!(path.exists(), "goldens file missing: {path:?}");
    let doc = udoc::extract(&path).expect("extract should succeed");
    let mut cache = FontCache::new(&doc.assets);
    render_page_with_profile(&doc, page, DPI, &mut cache, RenderingProfile::OcrFriendly)
        .expect("render should succeed")
}

fn assert_goldens_page_ocr(name: &str, rel: &str, page: usize) {
    let png = render_goldens_page_ocr(rel, page);
    assert_golden_png_ssim(name, &png, golden_dir(), SSIM_THRESHOLD);
}

fn render_extended_page(rel: &str, page: usize) -> Vec<u8> {
    let path = extended_corpus_root().join(rel);
    assert!(path.exists(), "extended-corpus file missing: {path:?}");
    let doc = udoc::extract(&path).expect("extract should succeed");
    let mut cache = FontCache::new(&doc.assets);
    render_page_with_profile(&doc, page, DPI, &mut cache, RenderingProfile::Visual)
        .expect("render should succeed")
}

fn assert_extended_page(name: &str, rel: &str, page: usize) {
    let png = render_extended_page(rel, page);
    assert_golden_png_ssim(name, &png, golden_dir(), SSIM_THRESHOLD);
}

// Pure-text prose, single column. Exercises the Latin fallback + hinting path
// without tables, figures, or shapes. RFC 577 is a short plain-text-style RFC.
#[test]
#[ignore = "slow in debug mode; run with cargo test --release -- --include-ignored"]
fn page_pure_text_rfc577() {
    assert_page_golden("pure_text_rfc577", "ietf-rfc/rfc577.pdf", 0);
}

// Dense 8pt text with many fonts. RFC 8705 page 0 is an IETF standards doc
// with compact prose. Substitutes for "IRS instructions" which are not in
// the corpus.
#[test]
#[ignore = "slow in debug mode; run with cargo test --release -- --include-ignored"]
fn page_dense_small_text_rfc8705() {
    assert_page_golden("dense_small_rfc8705", "ietf-rfc/rfc8705.pdf", 0);
}

// CFF subroutine-heavy glyphs. Scientific papers (arxiv-cs) embed Times-like
// CFF fonts with extensive subroutine use for width-class and accent glyphs.
#[test]
#[ignore = "slow in debug mode; run with cargo test --release -- --include-ignored"]
fn page_cff_subr_arxiv_cs() {
    assert_page_golden("cff_subr_arxiv_cs", "arxiv-cs/2601.16746.pdf", 0);
}

// Type3 / LaTeX math symbols. arxiv-math uses LaTeX, which often emits
// Type3 fonts for a subset of math symbols (integral signs, script letters).
// Closest substitute available locally; not guaranteed to hit Type3 on
// page 0 of every doc, but the class is represented.
#[test]
#[ignore = "slow in debug mode; run with cargo test --release -- --include-ignored"]
fn page_latex_math_arxiv() {
    assert_page_golden("latex_math_arxiv", "arxiv-math/2601.13127.pdf", 0);
}

// Transparency groups (modern report / SEC filing). SEC EDGAR filings use
// transparency-group PDFs exported from Word/InDesign. Substitutes for
// "Adobe marketing" PDFs which are not in the corpus.
#[test]
#[ignore = "slow in debug mode; run with cargo test --release -- --include-ignored"]
fn page_transparency_sec_edgar() {
    assert_page_golden(
        "transparency_sec_edgar",
        "sec-edgar/1000177_UPLOAD_filename1.pdf",
        0,
    );
}

// CMYK image page. USPTO patents often contain CMYK drawings and figures.
// Page 0 is the cover + first drawing for most patents in this corpus.
#[test]
#[ignore = "slow in debug mode; run with cargo test --release -- --include-ignored"]
fn page_cmyk_uspto_patent() {
    assert_page_golden("cmyk_uspto_patent", "uspto-patents/US12000000.pdf", 0);
}

// JBIG2-scanned page. Internet Archive English scans use JBIG2 for the
// monochrome page content.
#[test]
#[ignore = "slow in debug mode; run with cargo test --release -- --include-ignored"]
fn page_jbig2_ia_scan() {
    assert_page_golden(
        "jbig2_ia_scan",
        "ia-english/1900orlastpresid00lock_1900orlastpresid00lock.pdf",
        0,
    );
}

// Table cell borders (government form). govdocs1 has mixed-content forms
// with table rules and borders. Substitutes for "IRS form" which is not in
// the corpus.
#[test]
#[ignore = "slow in debug mode; run with cargo test --release -- --include-ignored"]
fn page_table_borders_govdocs() {
    assert_page_golden("table_borders_govdocs", "govdocs1/005010.pdf", 0);
}

// CID TrueType subsets from Microsoft: Print To PDF. MS Word export embeds
// CIDFontType2 subsets with Identity-H encoding and /W advance-width tables
// that frequently disagree with the embedded hmtx. This page was the worst
// doc in the  100-doc sample (SSIM 0.6430) until issue #182 landed the
// /W plumbing through the renderer; golden locks the fix so future renderer
// refactors can't silently regress the MS Word export genre.
#[test]
#[ignore = "slow in debug mode; run with cargo test --release -- --include-ignored"]
fn page_cid_tt_ms_word() {
    assert_page_golden("cid_tt_ms_word", "arxiv-bio/2508.05692.pdf", 0);
}

// Synthetic /Rotate 90 page: 612x792 portrait MediaBox, /Rotate 90, coloured
// rect at bottom-left + "TOP" text near top. Locks the fix:
// the buffer rotation must land the gray left-edge strip at the TOP of the
// landscape display and the coloured square at the upper-left. Regression
// tripwire for ia-english comic-book pages like /139085831eleternauta.
#[test]
fn page_rotate_90_comic() {
    assert_goldens_page("rotate_90_comic", "rotate_90_comic.pdf", 0);
}

// Synthetic /Rotate 270 page: 792x612 landscape MediaBox, /Rotate 270,
// coloured rect near top-left of content space + "TOP" text. /Rotate 270
// is CCW 90 from the content's native frame; the gray right-edge strip
// should land at TOP and the coloured box at the bottom-left after the
// rotation.
#[test]
fn page_rotate_270_landscape() {
    assert_goldens_page("rotate_270_landscape", "rotate_270_landscape.pdf", 0);
}

// Soft-mask luminosity form XObject (ISO 32000-2 §11.6.5,).
// 200x200 page with a 20x20 red square stamped in the centre, painted
// under an ExtGState whose /SMask is a /S /Luminosity form XObject. The
// form is fully white (luminance 1 -> opaque) except for a 4x4 black
// hole at (98, 98) (luminance 0 -> transparent). MuPDF applies the
// mask and punches a 4x4 white notch in the centre of the square.
//
// Our renderer's PDF interpreter does not yet lift ExtGState /SMask
// into the presentation layer (that plumbing lives in interpreter.rs,
// owned by this wave), so our render currently
// paints the full 20x20 square without the notch. The visual delta is
// a 4x4-pixel region at 72 DPI or ~8x8 at 150 DPI, which is small
// enough that SSIM vs MuPDF lands at 0.9985 -- well above the
// per-page >= 0.98 golden gate. The test serves as both a render-
// pipeline tripwire AND a sharp canary: any regression that widens
// the masked area will break it.
#[test]
fn page_softmask_luminosity() {
    assert_goldens_page("softmask_luminosity", "softmask_luminosity.pdf", 0);
}

// Soft-mask alpha form XObject (ISO 32000-2 §11.6.5,).
// Same layout as the luminosity variant but with /S /Alpha. The form
// XObject paints a full-page white rect split into four bands with an
// unpainted 4x4 hole in the centre, so the form's alpha channel is 1
// everywhere except the hole. MuPDF uses that alpha as the mask and
// punches a small white notch in the centre of the blue square; our
// renderer paints the solid square. Same SSIM 0.99+ vs MuPDF as the
// luminosity variant for the same reason. See the luminosity doc
// comment for the full rationale.
#[test]
fn page_softmask_alpha() {
    assert_goldens_page("softmask_alpha", "softmask_alpha.pdf", 0);
}

// Synthetic /Rotate 180 page with a non-zero /CropBox origin.
// MediaBox 612x792, CropBox [36 36 576 756]. /Rotate 180 flips both axes,
// so the gray bar at the top of the uncropped content should land at the
// bottom of the rendered output and the red square moves accordingly.
// Tripwire for any crop-vs-rotate ordering regression.
#[test]
fn page_rotate_180_cropbox() {
    assert_goldens_page("rotate_180_cropbox", "rotate_180_cropbox.pdf", 0);
}

// Synthetic /Rotate 0 with a /CropBox significantly smaller than the
// MediaBox. The MediaBox is filled gray; the CropBox
// [80 80 420 420] should clip output to the 340x340 sub-region. Locks
// the crop-without-rotation code path so it can't regress after the
// rotation work.
#[test]
fn page_rotate_0_cropbox_offset() {
    assert_goldens_page("rotate_0_cropbox_offset", "rotate_0_cropbox_offset.pdf", 0);
}

// Self-intersecting 5-point star filled with the even-odd rule (
//non-regression coverage for). Star
// vertices wind twice, so even-odd carves out a pentagon in the centre
// that non-zero fill would flood. Any regression in the winding-rule
// dispatch will blow out the centre region.
#[test]
fn page_path_selfintersect_evenodd() {
    assert_goldens_page(
        "path_selfintersect_evenodd",
        "path_selfintersect_evenodd.pdf",
        0,
    );
}

// Stroked open polyline with an acute corner and miter join (
//). 8pt stroke, miter limit 10, so the join extends well
// past the stroke width -- any regression in miter-clip fallback will
// produce a truncated bevel instead of a miter spike.
#[test]
fn page_path_stroke_miter() {
    assert_goldens_page("path_stroke_miter", "path_stroke_miter.pdf", 0);
}

// Nested q/Q clip stack with intersecting rectangular clips (
//). Outer clip takes left half, inner clip takes top half;
// a full-page red fill inside both clips must only land in the top-left
// quadrant. Exercises clip-stack push/pop correctness across nested q
// saves.
#[test]
fn page_path_nested_clip() {
    assert_goldens_page("path_nested_clip", "path_nested_clip.pdf", 0);
}

// Base14 Helvetica with a user-declared /FontDescriptor that overrides
// /Ascent and /Descent. Spec values for Helvetica are
// 718/-207; the descriptor declares 900/-250. Renderer must honour the
// descriptor, not the base14 backstop, for line height + positioning.
#[test]
fn page_fontdesc_custom_metrics() {
    assert_goldens_page("fontdesc_custom_metrics", "fontdesc_custom_metrics.pdf", 0);
}

// Courier with no /Widths array and /MissingWidth 600 in the descriptor
//. All characters must render at the declared 600-unit
// advance, exercising the missing-widths fallback path.
#[test]
fn page_fontdesc_missing_widths() {
    assert_goldens_page("fontdesc_missing_widths", "fontdesc_missing_widths.pdf", 0);
}

// CIDFontType0 with a /W override that maps CIDs 65..69 to 1500-unit
// advances ( stress variant, second beside cid_tt_ms_word).
// /DW 500 is the default; /W wins for the named CIDs. Text uses an
// Identity-H hex string <00410042004300440045> so each glyph step must
// advance wide, not default.
#[test]
fn page_cid_wide_w_table() {
    assert_goldens_page("cid_wide_w_table", "cid_wide_w_table.pdf", 0);
}

// Dense 9pt Helvetica prose, five lines. Short words
// with many vertical stems at small ppem sizes. This is the regime
// where the FT-port stem-fit regressions show up first -- the per-line
// SSIM is the canary.
#[test]
fn page_stemfit_textheavy() {
    assert_goldens_page("stemfit_textheavy", "stemfit_textheavy.pdf", 0);
}

// Three base14 fonts in a single page (Helvetica + Times-Roman +
// Courier) with Helvetica reused at a second size.
// Exercises stem-fit state isolation across font switches within one
// content stream.
#[test]
fn page_stemfit_mixedfonts() {
    assert_goldens_page("stemfit_mixedfonts", "stemfit_mixedfonts.pdf", 0);
}

// ---------------------------------------------------------------------------
// 5 new page goldens covering  surge fixes.
// ---------------------------------------------------------------------------

// W1S-CHARTER fix #1: /Mask image-stencil extraction (commit 3ddae421).
// Seed PDF: ia-french/LinterpretationDuCoranIbnKathir_00 - PREFACE.pdf.
// This was the worst-doc winner from the W1S-CHARTER bench, jumping from
// SSIM 0.8441 -> 0.9692 (+0.125) once the image-mask form of /Mask started
// flowing through the soft-mask compositor. Locks the new XObject /Mask
// indirect-ref decode path so future content-interpreter refactors can't
// silently drop the mask and re-paint the underlay over the cover image.
#[test]
#[ignore = "slow in debug mode; run with cargo test --release -- --include-ignored"]
fn page_mask_image_stencil_ia_french() {
    assert_page_golden(
        "mask_image_stencil_ia_french",
        "ia-french/LinterpretationDuCoranIbnKathir_00 - PREFACE.pdf",
        0,
    );
}

// W1S-CHARTER fix #2: AP-less /Link annotation suppression (commit 7270e84d).
// Seed PDF: arxiv-physics/0812.2693.pdf -- this was the biggest per-doc win
// in the link-suppression bench (SSIM 0.9304 -> 0.9516, +0.0211). MuPDF
// suppresses borders on /Link annotations whose appearance stream is missing
// (ISO 32000-1 12.5.6.5); we now match that default. The arxiv watermark
// link in the left margin is the regression canary -- prior to the fix we
// double-stroked a yellow border that fringed the text below.
#[test]
#[ignore = "slow in debug mode; run with cargo test --release -- --include-ignored"]
fn page_link_suppress_arxiv_physics() {
    assert_page_golden(
        "link_suppress_arxiv_physics",
        "arxiv-physics/0812.2693.pdf",
        0,
    );
}

// M-40 OcrFriendly profile path (W1S-MNORM, commit bef9dbdd).
// Seed PDF: synthetic stemfit_textheavy.pdf (5 lines of dense 9pt Helvetica
// prose). M-40 X-axis hinting stayed default-off after the W1S-MNORM bench
// fired its kill-check (SSIM regressed -0.005 with M-40 on), but the
// `RenderingProfile::OcrFriendly` arm of `decide_enable_x_axis`
// is the path the production default flows through. This golden renders
// the same dense-text doc under the explicit OcrFriendly profile so any
// regression in the profile dispatch (or the M-39 x-height-align disable
// arm) shows up byte-for-byte. The Visual variant of this same seed lives
// as `stemfit_textheavy` above.
#[test]
fn page_ocr_friendly_profile_dense_text() {
    assert_goldens_page_ocr(
        "ocr_friendly_profile_dense_text",
        "stemfit_textheavy.pdf",
        0,
    );
}

// W1S-NOTOA: Noto Sans Arabic routing (commit 2b570b58, closes #205).
// Seed PDF: extended/pdfjs-repo/test/pdfs/ArabicCIDTrueType.pdf. Prior to
// the Tier 2 Arabic bundle the routing layer fell through to .notdef
// boxes for Arabic codepoints when the source font lacked coverage; the
// `audit-fonts` CLI dropped from N>0 missing-glyph pairs to 0 once Noto
// Sans Arabic Regular + Bold routed via `route_by_unicode` for U+0600..06FF
// + U+0750..077F + U+FB50..FDFF + U+FE70..FEFF. Locks the routing dispatch
// + the bundle inclusion under the `tier2-arabic` cargo feature.
#[test]
#[cfg(feature = "tier2-arabic")]
fn page_noto_arabic_routing() {
    assert_extended_page(
        "noto_arabic_routing",
        "pdfjs-repo/test/pdfs/ArabicCIDTrueType.pdf",
        0,
    );
}

// Soft-mask compositing edge case (PMC1079898 banner).
// Seed PDF: pubmed-oa/PMC1079898.pdf. The  residual-gaps audit flagged
// this as one of the seven <0.85 outliers ("title banner pink renders wrong;
// possibly an ExtGState blend mode or /Type /Pattern colorspace"); pattern
// colorspace remains a follow-up gap, but the soft-mask + ExtGState blend
// path on the banner element does flow through the compositor. Locks the
// current renderer output for that mixed soft-mask + blend path so future
// pattern-colorspace work has a fixed before-state to diff against.
#[test]
#[ignore = "slow in debug mode; run with cargo test --release -- --include-ignored"]
fn page_softmask_compositing_pubmed() {
    assert_page_golden("softmask_compositing_pubmed", "pubmed-oa/PMC1079898.pdf", 0);
}
