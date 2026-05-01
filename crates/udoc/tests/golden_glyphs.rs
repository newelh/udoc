//! Per-glyph pixel goldens (byte-exact).
//!
//! Renders single glyphs via the FontCache + rasterizer pipeline (through
//! `udoc::render::render_page` on a tight 48x48 canvas at 72 DPI so the
//! output is one pixel per PDF point) and compares the PNG bytes against
//! blessed fixtures. These goldens are the mechanical oracle for the
//! udoc-font extraction: any change in byte output flags a regression in
//! the font rendering path.
//!
//! Run `BLESS=1 cargo test -p udoc --test golden_glyphs` to (re)create
//! fixtures; plain `cargo test` enforces byte-exact match.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use udoc::render::font_cache::FontCache;
use udoc::render::{render_page_with_profile, RenderingProfile};
use udoc_core::document::presentation::{PageDef, PositionedSpan, Presentation};
use udoc_core::document::Document;
use udoc_core::geometry::BoundingBox;
use udoc_core::test_harness::assert_golden_png_bytes;

fn golden_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/glyphs"))
}

/// Render one glyph to a 48x48 PNG at 72 DPI (one pixel per PDF point).
///
/// `font` selects the named font (empty string = default sans fallback).
/// Bold/italic flags are set on the span; the renderer does not yet vary
/// the fallback font by those flags, so the output equals the regular
/// variant today. The flags still get exercised on the span type so a
/// future bold/italic path change will show up as a deliberate re-bless.
fn render_glyph_png(ch: char, size_px: f64, font: &str, is_bold: bool, is_italic: bool) -> Vec<u8> {
    let page_w = 48.0;
    let page_h = 48.0;
    let mut doc = Document::new();
    let mut pres = Presentation::default();
    pres.pages.push(PageDef::new(0, page_w, page_h, 0));

    let mut span = PositionedSpan::new(
        ch.to_string(),
        BoundingBox::new(6.0, 8.0, 42.0, 8.0 + size_px),
        0,
    );
    span.font_size = Some(size_px);
    if !font.is_empty() {
        span.font_name = Some(font.to_string());
    }
    span.is_bold = is_bold;
    span.is_italic = is_italic;
    pres.raw_spans.push(span);
    doc.presentation = Some(pres);

    let mut cache = FontCache::empty();
    // Goldens capture FreeType-NORMAL byte-exact output (M-39 x-height scale
    // alignment on). That is the `Visual` profile; the OcrFriendly default
    // disables M-39 and would break the blessed byte-exact
    // fixtures. The goldens remain the renderer tripwire for FT-NORMAL
    // fidelity regressions.
    render_page_with_profile(&doc, 0, 72, &mut cache, RenderingProfile::Visual)
        .expect("render should succeed")
}

fn assert_glyph_golden(name: &str, ch: char, size_px: f64, font: &str, bold: bool, italic: bool) {
    let png = render_glyph_png(ch, size_px, font, bold, italic);
    assert_golden_png_bytes(name, &png, golden_dir());
}

// ---------------------------------------------------------------------------
// Latin (22 tests: 11 characters x 2 sizes)
// ---------------------------------------------------------------------------

#[test]
fn latin_a_12px() {
    assert_glyph_golden("latin_a_12px", 'A', 12.0, "", false, false);
}
#[test]
fn latin_a_24px() {
    assert_glyph_golden("latin_a_24px", 'A', 24.0, "", false, false);
}

#[test]
fn latin_g_12px() {
    assert_glyph_golden("latin_g_12px", 'g', 12.0, "", false, false);
}
#[test]
fn latin_g_24px() {
    assert_glyph_golden("latin_g_24px", 'g', 24.0, "", false, false);
}

#[test]
fn latin_o_12px() {
    assert_glyph_golden("latin_o_12px", 'o', 12.0, "", false, false);
}
#[test]
fn latin_o_24px() {
    assert_glyph_golden("latin_o_24px", 'o', 24.0, "", false, false);
}

#[test]
fn latin_e_12px() {
    assert_glyph_golden("latin_e_12px", 'e', 12.0, "", false, false);
}
#[test]
fn latin_e_24px() {
    assert_glyph_golden("latin_e_24px", 'e', 24.0, "", false, false);
}

#[test]
fn latin_m_12px() {
    assert_glyph_golden("latin_m_12px", 'm', 12.0, "", false, false);
}
#[test]
fn latin_m_24px() {
    assert_glyph_golden("latin_m_24px", 'm', 24.0, "", false, false);
}

#[test]
fn latin_w_12px() {
    assert_glyph_golden("latin_w_12px", 'W', 12.0, "", false, false);
}
#[test]
fn latin_w_24px() {
    assert_glyph_golden("latin_w_24px", 'W', 24.0, "", false, false);
}

#[test]
fn latin_i_12px() {
    assert_glyph_golden("latin_i_12px", 'i', 12.0, "", false, false);
}
#[test]
fn latin_i_24px() {
    assert_glyph_golden("latin_i_24px", 'i', 24.0, "", false, false);
}

#[test]
fn latin_l_12px() {
    assert_glyph_golden("latin_l_12px", 'l', 12.0, "", false, false);
}
#[test]
fn latin_l_24px() {
    assert_glyph_golden("latin_l_24px", 'l', 24.0, "", false, false);
}

#[test]
fn latin_h_12px() {
    assert_glyph_golden("latin_h_12px", 'H', 12.0, "", false, false);
}
#[test]
fn latin_h_24px() {
    assert_glyph_golden("latin_h_24px", 'H', 24.0, "", false, false);
}

#[test]
fn latin_n_12px() {
    assert_glyph_golden("latin_n_12px", 'n', 12.0, "", false, false);
}
#[test]
fn latin_n_24px() {
    assert_glyph_golden("latin_n_24px", 'n', 24.0, "", false, false);
}

#[test]
fn latin_u_12px() {
    assert_glyph_golden("latin_u_12px", 'u', 12.0, "", false, false);
}
#[test]
fn latin_u_24px() {
    assert_glyph_golden("latin_u_24px", 'u', 24.0, "", false, false);
}

// ---------------------------------------------------------------------------
// Symbol (2 tests: '&' x 2 sizes)
// ---------------------------------------------------------------------------

#[test]
fn symbol_ampersand_12px() {
    assert_glyph_golden("symbol_ampersand_12px", '&', 12.0, "", false, false);
}
#[test]
fn symbol_ampersand_24px() {
    assert_glyph_golden("symbol_ampersand_24px", '&', 24.0, "", false, false);
}

// ---------------------------------------------------------------------------
// Math (6 tests: integral, summation, partial x 2 sizes)
// ---------------------------------------------------------------------------

#[test]
fn math_integral_12px() {
    assert_glyph_golden("math_integral_12px", '\u{222B}', 12.0, "", false, false);
}
#[test]
fn math_integral_24px() {
    assert_glyph_golden("math_integral_24px", '\u{222B}', 24.0, "", false, false);
}

#[test]
fn math_summation_12px() {
    assert_glyph_golden("math_summation_12px", '\u{2211}', 12.0, "", false, false);
}
#[test]
fn math_summation_24px() {
    assert_glyph_golden("math_summation_24px", '\u{2211}', 24.0, "", false, false);
}

#[test]
fn math_partial_12px() {
    assert_glyph_golden("math_partial_12px", '\u{2202}', 12.0, "", false, false);
}
#[test]
fn math_partial_24px() {
    assert_glyph_golden("math_partial_24px", '\u{2202}', 24.0, "", false, false);
}

// ---------------------------------------------------------------------------
// CJK (6 tests: 3 characters x 2 sizes). Exercises Noto Sans CJK fallback.
// U+7E41 (繁) may not be in the shipped Noto subset; an all-white PNG still
// pins that behavior.
//
// Gated on the `cjk-fonts` feature. When disabled, the CJK
// fallback bundle is absent from the binary and the test expectations
// (which bless against the Noto subset) are meaningless.
// ---------------------------------------------------------------------------

#[cfg(feature = "cjk-fonts")]
#[test]
fn cjk_yi_12px() {
    assert_glyph_golden("cjk_yi_12px", '\u{4E00}', 12.0, "", false, false);
}
#[cfg(feature = "cjk-fonts")]
#[test]
fn cjk_yi_24px() {
    assert_glyph_golden("cjk_yi_24px", '\u{4E00}', 24.0, "", false, false);
}

#[cfg(feature = "cjk-fonts")]
#[test]
fn cjk_zhong_12px() {
    assert_glyph_golden("cjk_zhong_12px", '\u{4E2D}', 12.0, "", false, false);
}
#[cfg(feature = "cjk-fonts")]
#[test]
fn cjk_zhong_24px() {
    assert_glyph_golden("cjk_zhong_24px", '\u{4E2D}', 24.0, "", false, false);
}

#[cfg(feature = "cjk-fonts")]
#[test]
fn cjk_fan_12px() {
    assert_glyph_golden("cjk_fan_12px", '\u{7E41}', 12.0, "", false, false);
}
#[cfg(feature = "cjk-fonts")]
#[test]
fn cjk_fan_24px() {
    assert_glyph_golden("cjk_fan_24px", '\u{7E41}', 24.0, "", false, false);
}

// ---------------------------------------------------------------------------
// Bold + italic variants (4 tests). Renderer does not yet vary fallback
// font on these flags; goldens pin current output.
// ---------------------------------------------------------------------------

#[test]
fn bold_g_12px() {
    assert_glyph_golden("bold_g_12px", 'g', 12.0, "", true, false);
}
#[test]
fn bold_g_24px() {
    assert_glyph_golden("bold_g_24px", 'g', 24.0, "", true, false);
}

#[test]
fn italic_o_12px() {
    assert_glyph_golden("italic_o_12px", 'o', 12.0, "", false, true);
}
#[test]
fn italic_o_24px() {
    assert_glyph_golden("italic_o_24px", 'o', 24.0, "", false, true);
}

// ---------------------------------------------------------------------------
// Tier 1 routing goldens (M-33, ~30 tests).
//
// These pin the M-35 Tier 1 bundle + M-36 font-name-aware routing. Each
// group targets one specific routing path so future regressions in the
// bundle contents, the name-sniff rules, the Unicode-range sniff, or the
// rasterizer will surface byte-exact.
// ---------------------------------------------------------------------------

// LM Roman via CMR routing (regular + bold + italic flavors).
#[test]
fn cmr10_a_12px() {
    assert_glyph_golden("cmr10_a_12px", 'a', 12.0, "CMR10", false, false);
}
#[test]
fn cmr10_a_24px() {
    assert_glyph_golden("cmr10_a_24px", 'a', 24.0, "CMR10", false, false);
}

#[test]
fn cmbx12_a_cap_12px() {
    assert_glyph_golden("cmbx12_a_cap_12px", 'A', 12.0, "CMBX12", false, false);
}
#[test]
fn cmbx12_a_cap_24px() {
    assert_glyph_golden("cmbx12_a_cap_24px", 'A', 24.0, "CMBX12", false, false);
}

#[test]
fn cmti10_e_12px() {
    assert_glyph_golden("cmti10_e_12px", 'e', 12.0, "CMTI10", false, false);
}
#[test]
fn cmti10_e_24px() {
    assert_glyph_golden("cmti10_e_24px", 'e', 24.0, "CMTI10", false, false);
}

// LM Math via CMMI / CMSY / CMEX routing.
#[test]
fn cmmi10_alpha_12px() {
    assert_glyph_golden(
        "cmmi10_alpha_12px",
        '\u{03B1}',
        12.0,
        "CMMI10",
        false,
        false,
    );
}
#[test]
fn cmmi10_alpha_24px() {
    assert_glyph_golden(
        "cmmi10_alpha_24px",
        '\u{03B1}',
        24.0,
        "CMMI10",
        false,
        false,
    );
}

#[test]
fn cmsy10_infinity_12px() {
    assert_glyph_golden(
        "cmsy10_infinity_12px",
        '\u{221E}',
        12.0,
        "CMSY10",
        false,
        false,
    );
}
#[test]
fn cmsy10_infinity_24px() {
    assert_glyph_golden(
        "cmsy10_infinity_24px",
        '\u{221E}',
        24.0,
        "CMSY10",
        false,
        false,
    );
}

#[test]
fn cmex10_sum_12px() {
    assert_glyph_golden("cmex10_sum_12px", '\u{2211}', 12.0, "CMEX10", false, false);
}
#[test]
fn cmex10_sum_24px() {
    assert_glyph_golden("cmex10_sum_24px", '\u{2211}', 24.0, "CMEX10", false, false);
}

// Liberation Sans Bold via Helvetica-Bold routing.
#[test]
fn helvetica_bold_a_12px() {
    assert_glyph_golden(
        "helvetica_bold_a_12px",
        'a',
        12.0,
        "Helvetica-Bold",
        false,
        false,
    );
}
#[test]
fn helvetica_bold_a_24px() {
    assert_glyph_golden(
        "helvetica_bold_a_24px",
        'a',
        24.0,
        "Helvetica-Bold",
        false,
        false,
    );
}

#[test]
fn helvetica_bold_a_cap_12px() {
    assert_glyph_golden(
        "helvetica_bold_a_cap_12px",
        'A',
        12.0,
        "Helvetica-Bold",
        false,
        false,
    );
}
#[test]
fn helvetica_bold_a_cap_24px() {
    assert_glyph_golden(
        "helvetica_bold_a_cap_24px",
        'A',
        24.0,
        "Helvetica-Bold",
        false,
        false,
    );
}

// Liberation Sans Italic via Helvetica-Italic routing.
#[test]
fn helvetica_italic_a_12px() {
    assert_glyph_golden(
        "helvetica_italic_a_12px",
        'a',
        12.0,
        "Helvetica-Italic",
        false,
        false,
    );
}
#[test]
fn helvetica_italic_a_24px() {
    assert_glyph_golden(
        "helvetica_italic_a_24px",
        'a',
        24.0,
        "Helvetica-Italic",
        false,
        false,
    );
}

#[test]
fn helvetica_italic_f_12px() {
    assert_glyph_golden(
        "helvetica_italic_f_12px",
        'f',
        12.0,
        "Helvetica-Italic",
        false,
        false,
    );
}
#[test]
fn helvetica_italic_f_24px() {
    assert_glyph_golden(
        "helvetica_italic_f_24px",
        'f',
        24.0,
        "Helvetica-Italic",
        false,
        false,
    );
}

// Liberation Mono via Courier routing.
#[test]
fn courier_a_12px() {
    assert_glyph_golden("courier_a_12px", 'a', 12.0, "Courier", false, false);
}
#[test]
fn courier_a_24px() {
    assert_glyph_golden("courier_a_24px", 'a', 24.0, "Courier", false, false);
}

#[test]
fn courier_m_cap_12px() {
    assert_glyph_golden("courier_m_cap_12px", 'M', 12.0, "Courier", false, false);
}
#[test]
fn courier_m_cap_24px() {
    assert_glyph_golden("courier_m_cap_24px", 'M', 24.0, "Courier", false, false);
}

// STIX fallback (name-sniff) routes to LM Math.
#[test]
fn stix_math_integral_12px() {
    assert_glyph_golden(
        "stix_math_integral_12px",
        '\u{222B}',
        12.0,
        "STIXTwoMath-Regular",
        false,
        false,
    );
}
#[test]
fn stix_math_integral_24px() {
    assert_glyph_golden(
        "stix_math_integral_24px",
        '\u{222B}',
        24.0,
        "STIXTwoMath-Regular",
        false,
        false,
    );
}

// Unicode-range sniff on an unrouted font name. `MysteryFont` has no
// prefix match in route_tier1, so the per-glyph Unicode sniff should
// kick in and route math codepoints to LM Math.
#[test]
fn mystery_math_sum_12px() {
    assert_glyph_golden(
        "mystery_math_sum_12px",
        '\u{2211}',
        12.0,
        "MysteryFont",
        false,
        false,
    );
}
#[test]
fn mystery_math_sum_24px() {
    assert_glyph_golden(
        "mystery_math_sum_24px",
        '\u{2211}',
        24.0,
        "MysteryFont",
        false,
        false,
    );
}

#[test]
fn mystery_math_partial_12px() {
    assert_glyph_golden(
        "mystery_math_partial_12px",
        '\u{2202}',
        12.0,
        "MysteryFont",
        false,
        false,
    );
}
#[test]
fn mystery_math_partial_24px() {
    assert_glyph_golden(
        "mystery_math_partial_24px",
        '\u{2202}',
        24.0,
        "MysteryFont",
        false,
        false,
    );
}

// ---------------------------------------------------------------------------
// Liberation Serif Bold / Italic / BoldItalic via Times* routing (T1-SERIF, #193).
//
// These pin the new Tier 1 Serif weights added to the bundle. The
// routing rule extends the Times/serif block in `route_tier1`: Times* with
// "Bold"/"Italic"/"Oblique" tokens lands on the matching Liberation Serif
// face. Prior to all Times variants routed to SerifRegular with
// synthetic stem-widening; now each weight has a real face.
// ---------------------------------------------------------------------------

#[test]
fn times_bold_g_12px() {
    assert_glyph_golden("times_bold_g_12px", 'g', 12.0, "Times-Bold", false, false);
}
#[test]
fn times_bold_g_24px() {
    assert_glyph_golden("times_bold_g_24px", 'g', 24.0, "Times-Bold", false, false);
}

#[test]
fn times_italic_q_12px() {
    assert_glyph_golden(
        "times_italic_q_12px",
        'q',
        12.0,
        "Times-Italic",
        false,
        false,
    );
}
#[test]
fn times_italic_q_24px() {
    assert_glyph_golden(
        "times_italic_q_24px",
        'q',
        24.0,
        "Times-Italic",
        false,
        false,
    );
}

#[test]
fn times_bolditalic_a_cap_12px() {
    assert_glyph_golden(
        "times_bolditalic_a_cap_12px",
        'A',
        12.0,
        "Times-BoldItalic",
        false,
        false,
    );
}
#[test]
fn times_bolditalic_a_cap_24px() {
    assert_glyph_golden(
        "times_bolditalic_a_cap_24px",
        'A',
        24.0,
        "Times-BoldItalic",
        false,
        false,
    );
}
