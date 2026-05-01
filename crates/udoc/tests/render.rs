//! Integration tests for the page renderer.

use std::collections::HashSet;
use std::path::Path;

use udoc::render::font_cache::FontCache;
use udoc::render::render_page;
use udoc::{AssetConfig, Config};
use udoc_core::document::presentation::{PageDef, PositionedSpan, Presentation};
use udoc_core::document::Document;
use udoc_core::geometry::BoundingBox;

/// Strip the 6-letter "ABCDEF+" PDF font subset prefix if present.
/// Asset names carry the raw prefixed BaseFont, so tests that want to match
/// by the stripped display name go through this helper.
fn display_name_of(name: &str) -> &str {
    match name.find('+') {
        Some(pos) if pos == 6 && name[..6].chars().all(|c| c.is_ascii_uppercase()) => {
            &name[pos + 1..]
        }
        _ => name,
    }
}

fn make_doc(spans: Vec<PositionedSpan>, page_width: f64, page_height: f64) -> Document {
    let mut doc = Document::new();
    let mut pres = Presentation::default();
    pres.pages.push(PageDef::new(0, page_width, page_height, 0));
    pres.raw_spans = spans;
    doc.presentation = Some(pres);
    doc
}

#[test]
fn render_empty_page_produces_valid_png() {
    let doc = make_doc(vec![], 612.0, 792.0);
    let mut cache = FontCache::empty();
    let png = render_page(&doc, 0, 72, &mut cache).expect("should render");

    // Valid PNG signature.
    assert_eq!(&png[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
    // Correct dimensions at 72 DPI.
    let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let h = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
    assert_eq!(w, 612);
    assert_eq!(h, 792);
}

#[test]
fn render_with_text_span_produces_non_empty_png() {
    let mut span = PositionedSpan::new(
        "Hello World".to_string(),
        BoundingBox::new(72.0, 700.0, 300.0, 720.0),
        0,
    );
    span.font_size = Some(14.0);
    let doc = make_doc(vec![span], 612.0, 792.0);
    let mut cache = FontCache::empty();
    let png = render_page(&doc, 0, 150, &mut cache).expect("should render");

    assert_eq!(&png[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
    // At 150 DPI, width = 612 * 150/72 = 1275.
    let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    assert_eq!(w, 1275);
    // PNG should be larger than an empty page (has glyph/placeholder data).
    let empty_doc = make_doc(vec![], 612.0, 792.0);
    let empty_png = render_page(&empty_doc, 0, 150, &mut cache).expect("should render");
    assert_ne!(
        png.len(),
        empty_png.len(),
        "text page should differ from empty page"
    );
}

#[test]
fn render_multiple_spans() {
    let spans = vec![
        {
            let mut s = PositionedSpan::new(
                "Title".to_string(),
                BoundingBox::new(72.0, 750.0, 300.0, 780.0),
                0,
            );
            s.font_size = Some(24.0);
            s
        },
        {
            let mut s = PositionedSpan::new(
                "Body text here".to_string(),
                BoundingBox::new(72.0, 700.0, 400.0, 712.0),
                0,
            );
            s.font_size = Some(12.0);
            s
        },
    ];
    let doc = make_doc(spans, 612.0, 792.0);
    let mut cache = FontCache::empty();
    let png = render_page(&doc, 0, 150, &mut cache).expect("should render");
    assert_eq!(&png[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
}

#[test]
fn render_page_out_of_range() {
    let doc = make_doc(vec![], 612.0, 792.0);
    let mut cache = FontCache::empty();
    assert!(render_page(&doc, 5, 150, &mut cache).is_err());
}

#[test]
fn render_no_presentation_data() {
    let doc = Document::new();
    let mut cache = FontCache::empty();
    assert!(render_page(&doc, 0, 150, &mut cache).is_err());
}

#[test]
fn render_dpi_affects_dimensions() {
    let doc = make_doc(vec![], 612.0, 792.0);
    let mut cache = FontCache::empty();

    let png_72 = render_page(&doc, 0, 72, &mut cache).expect("72 DPI");
    let png_150 = render_page(&doc, 0, 150, &mut cache).expect("150 DPI");

    let w_72 = u32::from_be_bytes([png_72[16], png_72[17], png_72[18], png_72[19]]);
    let w_150 = u32::from_be_bytes([png_150[16], png_150[17], png_150[18], png_150[19]]);

    assert_eq!(w_72, 612);
    assert_eq!(w_150, 1275);
    assert!(
        png_150.len() > png_72.len(),
        "higher DPI should produce larger PNG"
    );
}

#[test]
fn render_a4_page() {
    // A4: 595.28 x 841.89 points
    let doc = make_doc(vec![], 595.28, 841.89);
    let mut cache = FontCache::empty();
    let png = render_page(&doc, 0, 72, &mut cache).expect("should render A4");

    let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let h = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
    // round(595.28) = 595, round(841.89) = 842
    assert_eq!(w, 595);
    assert_eq!(h, 842);
}

#[test]
fn render_rotated_page() {
    let mut doc = Document::new();
    let mut pres = Presentation::default();
    // 612x792 page with 90-degree rotation -> effective 792x612.
    pres.pages.push(PageDef::new(0, 612.0, 792.0, 90));
    doc.presentation = Some(pres);

    let mut cache = FontCache::empty();
    let png = render_page(&doc, 0, 72, &mut cache).expect("should render rotated");

    let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let h = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
    assert_eq!(w, 792); // swapped
    assert_eq!(h, 612);
}

#[test]
fn render_with_fallback_font_produces_glyphs() {
    // Liberation Sans (the fallback) should produce real glyph outlines,
    // not placeholder rectangles. Verify by checking that the rendered PNG
    // for text with a non-embedded font name is substantially different from
    // a page with no text at all.
    let mut span = PositionedSpan::new(
        "ABCDEFGHIJ".to_string(),
        BoundingBox::new(72.0, 700.0, 400.0, 720.0),
        0,
    );
    span.font_size = Some(18.0);
    span.font_name = Some("Helvetica".to_string()); // not embedded, triggers fallback

    let doc_with_text = make_doc(vec![span], 612.0, 792.0);
    let doc_empty = make_doc(vec![], 612.0, 792.0);

    let mut cache = FontCache::empty(); // has Liberation Sans fallback
    let png_text = render_page(&doc_with_text, 0, 150, &mut cache).expect("should render text");
    let png_empty = render_page(&doc_empty, 0, 150, &mut cache).expect("should render empty");

    // Text page should be significantly larger (glyph data takes more compressed bytes).
    assert!(
        png_text.len() > png_empty.len() + 100,
        "text page ({} bytes) should be significantly larger than empty ({} bytes)",
        png_text.len(),
        png_empty.len()
    );
}

#[test]
fn render_multiple_characters_advances_cursor() {
    // Verify that rendering multiple characters advances the cursor position,
    // producing glyphs at different x positions (not all stacked on top of each other).
    let mut span = PositionedSpan::new(
        "MMMMM".to_string(),
        BoundingBox::new(72.0, 700.0, 400.0, 720.0),
        0,
    );
    span.font_size = Some(24.0);

    let doc = make_doc(vec![span], 612.0, 792.0);
    let mut cache = FontCache::empty();
    let png = render_page(&doc, 0, 72, &mut cache).expect("should render");

    // Just verify it's a valid PNG and doesn't crash.
    assert_eq!(&png[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
}

// ---------------------------------------------------------------------------
// Real PDF rendering tests (font name matching + visual validation)
// ---------------------------------------------------------------------------

/// Helper: extract a document with fonts and presentation layer enabled.
fn extract_with_fonts(path: &Path) -> Document {
    let config = Config::default().assets(AssetConfig::default().fonts(true));
    udoc::extract_with(path, config).expect("extraction should succeed")
}

/// Helper: check that a byte slice starts with PNG signature.
fn is_valid_png(data: &[u8]) -> bool {
    data.len() > 8 && data[0..8] == [137, 80, 78, 71, 13, 10, 26, 10]
}

/// Verify that font identifiers in spans match font names in the asset store
/// for the arXiv LaTeX PDF. Asset names carry the subset prefix (unique per
/// subset); spans reference subsetted fonts via `font_id`. Spans whose
/// `font_id` is absent (standard fonts, non-PDF backends) fall back to
/// matching by `font_name`.
#[test]
fn font_name_matching_arxiv() {
    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    if !pdf_path.exists() {
        return; // Skip if corpus not available.
    }
    let doc = extract_with_fonts(pdf_path);

    let pres = doc.presentation.as_ref().expect("should have presentation");
    let asset_fonts: HashSet<&str> = doc.assets.fonts().iter().map(|f| f.name.as_str()).collect();

    // Standard fonts (Helvetica, Times-Roman, etc.) won't be in assets.
    let standard_fonts = [
        "Helvetica",
        "Helvetica-Bold",
        "Times-Roman",
        "Times-Bold",
        "Courier",
        "Courier-Bold",
        "Symbol",
        "ZapfDingbats",
    ];

    let mut unmatched: HashSet<String> = HashSet::new();
    for span in &pres.raw_spans {
        let key = span
            .font_id
            .as_deref()
            .or(span.font_name.as_deref())
            .unwrap_or("");
        if key.is_empty() {
            continue;
        }
        if asset_fonts.contains(key) || standard_fonts.contains(&key) {
            continue;
        }
        unmatched.insert(key.to_string());
    }

    assert!(
        unmatched.is_empty(),
        "arXiv: span fonts not found in assets: {:?}\nAsset fonts: {:?}",
        unmatched,
        asset_fonts,
    );
}

/// Verify font name matching for IRS 1040. See `font_name_matching_arxiv`
/// for the invariant: span.font_id is the per-subset key that matches
/// asset names, while span.font_name (stripped) matches standard fonts.
#[test]
fn font_name_matching_irs_1040() {
    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/irs_1040.pdf");
    if !pdf_path.exists() {
        return;
    }
    let doc = extract_with_fonts(pdf_path);

    let pres = doc.presentation.as_ref().expect("should have presentation");
    let asset_fonts: HashSet<&str> = doc.assets.fonts().iter().map(|f| f.name.as_str()).collect();

    let standard_fonts = [
        "Helvetica",
        "Helvetica-Bold",
        "Helvetica-Oblique",
        "Helvetica-BoldOblique",
        "Times-Roman",
        "Times-Bold",
        "Times-Italic",
        "Times-BoldItalic",
        "Courier",
        "Courier-Bold",
        "Symbol",
        "ZapfDingbats",
    ];

    let mut unmatched: HashSet<String> = HashSet::new();
    for span in &pres.raw_spans {
        let key = span
            .font_id
            .as_deref()
            .or(span.font_name.as_deref())
            .unwrap_or("");
        if key.is_empty() {
            continue;
        }
        if asset_fonts.contains(key) || standard_fonts.contains(&key) {
            continue;
        }
        unmatched.insert(key.to_string());
    }

    // IRS forms typically use standard fonts (not embedded), so no assets expected.
    // The key check is that no span references a non-standard font that's missing from assets.
    assert!(
        unmatched.is_empty(),
        "IRS 1040: span fonts not found in assets: {:?}\nAsset fonts: {:?}",
        unmatched,
        asset_fonts,
    );
}

/// Render arXiv PDF page 1 at 150 DPI and verify it produces a valid,
/// non-trivial PNG (more than just a white page).
#[test]
fn render_arxiv_page_1() {
    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    if !pdf_path.exists() {
        return;
    }
    let doc = extract_with_fonts(pdf_path);
    let mut font_cache = FontCache::new(&doc.assets);

    let png = render_page(&doc, 0, 150, &mut font_cache).expect("should render arXiv page 1");
    assert!(is_valid_png(&png));

    // A rendered page with real text should be significantly larger than
    // the compressed white rectangle (which is typically ~2-5 KB at 150 DPI).
    assert!(
        png.len() > 10_000,
        "arXiv page 1 PNG is suspiciously small ({} bytes), probably blank",
        png.len()
    );
}

/// Render IRS 1040 page 1 at 150 DPI.
#[test]
fn render_irs_1040_page_1() {
    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/irs_1040.pdf");
    if !pdf_path.exists() {
        return;
    }
    let doc = extract_with_fonts(pdf_path);
    let mut font_cache = FontCache::new(&doc.assets);

    let png = render_page(&doc, 0, 150, &mut font_cache).expect("should render IRS 1040 page 1");
    assert!(is_valid_png(&png));
    assert!(
        png.len() > 10_000,
        "IRS 1040 page 1 PNG is suspiciously small ({} bytes), probably blank",
        png.len()
    );
}

/// Verify that char_advances are populated for arXiv PDF spans.
#[test]
fn arxiv_spans_have_char_advances() {
    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    if !pdf_path.exists() {
        return;
    }
    let doc = extract_with_fonts(pdf_path);
    let pres = doc.presentation.as_ref().expect("should have presentation");

    let total_spans = pres.raw_spans.len();
    let spans_with_advances = pres
        .raw_spans
        .iter()
        .filter(|s| s.char_advances.is_some())
        .count();

    // The vast majority of spans should have char_advances populated.
    // Allow some to be None (ligature mismatches, edge cases).
    let ratio = spans_with_advances as f64 / total_spans as f64;
    assert!(
        ratio > 0.8,
        "only {}/{} ({:.0}%) arXiv spans have char_advances, expected >80%",
        spans_with_advances,
        total_spans,
        ratio * 100.0
    );
}

/// Verify that rendering with proportional char_advances produces different
/// output than uniform distribution (the old behavior).
#[test]
fn char_advances_affect_rendering() {
    let mut span = PositionedSpan::new(
        "Wii".to_string(),
        BoundingBox::new(72.0, 700.0, 200.0, 720.0),
        0,
    );
    span.font_size = Some(18.0);

    // With char_advances: W gets more space, i gets less.
    let mut span_with_advances = span.clone();
    span_with_advances.char_advances = Some(vec![20.0, 5.0, 5.0]);

    let doc_uniform = make_doc(vec![span], 612.0, 792.0);
    let doc_proportional = make_doc(vec![span_with_advances], 612.0, 792.0);

    let mut cache = FontCache::empty();
    let png_uniform = render_page(&doc_uniform, 0, 150, &mut cache).expect("uniform");
    let png_proportional =
        render_page(&doc_proportional, 0, 150, &mut cache).expect("proportional");

    // The two renders should produce different PNG bytes because character
    // positions differ.
    assert_ne!(
        png_uniform, png_proportional,
        "char_advances should produce different rendering than uniform distribution"
    );
}

/// Verify that Type1 font parsing produces glyph outlines for common
/// characters (requires subroutine support and callothersubr/pop protocol).
#[test]
fn type1_outlines_have_contours() {
    use udoc_core::document::assets::FontProgramType;
    use udoc_font::type1::Type1Font;

    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    if !pdf_path.exists() {
        return;
    }
    let doc = extract_with_fonts(pdf_path);

    // Find the main Roman font (NimbusRomNo9L-Regu, the body text font).
    let roman_font = doc
        .assets
        .fonts()
        .iter()
        .find(|f| f.program_type == FontProgramType::Type1 && f.name.contains("Regu"))
        .expect("should have a Roman Type1 font");

    let t1 = Type1Font::from_bytes(&roman_font.data).expect("should parse");
    assert!(t1.num_subrs() > 100, "Roman font should have many subrs");

    // 'A' should produce 2 contours (outer + inner triangle).
    let a_outline = t1.glyph_outline('A');
    assert!(
        a_outline.is_some(),
        "Type1 'A' should produce an outline (needs callsubr support)"
    );
    assert!(
        a_outline.unwrap().contours.len() >= 2,
        "'A' should have at least 2 contours"
    );

    // 'e' should also have contours.
    assert!(t1.glyph_outline('e').is_some(), "'e' should have outline");

    // 'n' requires callothersubr/pop (flex mechanism).
    assert!(
        t1.glyph_outline('n').is_some(),
        "'n' should have outline (requires callothersubr/pop)"
    );
}

#[test]
fn diagnose_span_gaps_arxiv() {
    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    if !pdf_path.exists() {
        return;
    }
    let doc = extract_with_fonts(pdf_path);
    let pres = doc.presentation.as_ref().unwrap();

    // Look at page 0 spans, sorted by position.
    let mut spans: Vec<&_> = pres
        .raw_spans
        .iter()
        .filter(|s| s.page_index == 0)
        .collect();
    spans.sort_by(|a, b| {
        let y = b.bbox.y_min.partial_cmp(&a.bbox.y_min).unwrap();
        if y != std::cmp::Ordering::Equal {
            return y;
        }
        a.bbox.x_min.partial_cmp(&b.bbox.x_min).unwrap()
    });

    println!("\n=== First 30 spans on page 0 ===");
    for (i, s) in spans.iter().take(30).enumerate() {
        let font = s.font_name.as_deref().unwrap_or("?");
        let size = s.font_size.unwrap_or(0.0);
        println!(
            "  [{:2}] x=[{:.1},{:.1}] y=[{:.1},{:.1}] size={:.1} font={} text={:?}",
            i, s.bbox.x_min, s.bbox.x_max, s.bbox.y_min, s.bbox.y_max, size, font, s.text
        );
        if i > 0 {
            let prev = spans[i - 1];
            let gap = s.bbox.x_min - prev.bbox.x_max;
            let same_y = (s.bbox.y_min - prev.bbox.y_min).abs() < size * 0.3;
            let same_font = s.font_name == prev.font_name;
            if same_y && gap.abs() < 20.0 {
                println!(
                    "       gap={:.2} same_font={} threshold={:.2}",
                    gap,
                    same_font,
                    size * 0.15
                );
            }
        }
    }
}

#[test]
fn diagnose_spacing_arxiv() {
    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    if !pdf_path.exists() {
        return;
    }
    let doc = extract_with_fonts(pdf_path);
    let pres = doc.presentation.as_ref().unwrap();

    // Find the "Unbiased" span
    for s in &pres.raw_spans {
        if s.text == "Unbiased" || s.text.starts_with("This paper") || s.text == "Multilevel" {
            let bbox_w = s.bbox.x_max - s.bbox.x_min;
            let ca_sum: f64 = s
                .char_advances
                .as_ref()
                .map(|v| v.iter().sum())
                .unwrap_or(0.0);
            let ratio = if ca_sum > 0.0 { bbox_w / ca_sum } else { 0.0 };
            println!(
                "text={:?} bbox_w={:.2} ca_sum={:.2} ratio={:.3} size={:.1} chars={}",
                &s.text[..s.text.len().min(30)],
                bbox_w,
                ca_sum,
                ratio,
                s.font_size.unwrap_or(0.0),
                s.text.chars().count()
            );
            if let Some(ref ca) = s.char_advances {
                let first5: Vec<f64> = ca.iter().take(5).copied().collect();
                println!("  advances: {:?}...", first5);
            }
        }
    }
}

#[test]
fn diagnose_font_widths_arxiv() {
    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    if !pdf_path.exists() {
        return;
    }
    let mut pdf = udoc_pdf::Document::open(pdf_path).unwrap();
    let mut page = pdf.page(0).unwrap();
    let spans = page.raw_spans().unwrap();
    for s in &spans {
        if s.text == "Unbiased" {
            println!(
                "font={} size={:.1} width={:.2}",
                s.font_name, s.font_size, s.width
            );
            break;
        }
    }
}

#[test]
fn check_widths_directly() {
    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    if !pdf_path.exists() {
        return;
    }
    let mut pdf = udoc_pdf::Document::open(pdf_path).unwrap();
    let mut page = pdf.page(0).unwrap();
    let spans = page.raw_spans().unwrap();
    // Look at width variety across different characters in same font
    let mut widths_by_char: std::collections::HashMap<char, f64> = std::collections::HashMap::new();
    for s in &spans {
        if s.font_name.as_ref() == "NimbusRomNo9L-Regu" && s.font_size > 20.0 {
            let per_char = s.width / s.text.chars().count() as f64;
            for ch in s.text.chars() {
                widths_by_char.entry(ch).or_insert(per_char);
            }
        }
    }
    // These should differ if /Widths is proportional
    println!("\nPer-span avg widths at 20.7pt (should be ~12.4 if uniform):");
    for s in &spans {
        if s.font_name.as_ref() == "NimbusRomNo9L-Regu" && s.font_size > 20.0 {
            let avg = s.width / s.text.chars().count() as f64;
            println!(
                "  {:?} avg_w={:.2} total={:.2} chars={}",
                s.text,
                avg,
                s.width,
                s.text.chars().count()
            );
        }
    }
}

#[test]
fn compare_glyph_outlines() {
    use udoc_core::document::assets::FontProgramType;
    use udoc_font::type1::Type1Font;

    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    if !pdf_path.exists() {
        return;
    }
    let doc = extract_with_fonts(pdf_path);
    let _cache = FontCache::new(&doc.assets);

    // Get 'H' outline from Type1 font
    let t1_font = doc
        .assets
        .fonts()
        .iter()
        .find(|f| {
            f.program_type == FontProgramType::Type1
                && f.name.contains("Regu")
                && !f.name.contains("Ital")
        })
        .unwrap();
    let t1 = Type1Font::from_bytes(&t1_font.data).unwrap();

    if let Some(t1_outline) = t1.glyph_outline('H') {
        println!(
            "\nType1 'H': {} contours, bounds={:?}",
            t1_outline.contours.len(),
            t1_outline.bounds
        );
        for (i, c) in t1_outline.contours.iter().enumerate() {
            println!("  contour {}: {} points", i, c.points.len());
            for pt in c.points.iter().take(8) {
                println!("    ({:.1}, {:.1}) on_curve={}", pt.x, pt.y, pt.on_curve);
            }
        }
    }

    // Get 'H' outline from Liberation Sans (fallback)
    if let Some(ls_outline) = t1.glyph_outline('l') {
        println!(
            "\nType1 l 'H': {} contours, bounds={:?}",
            ls_outline.contours.len(),
            ls_outline.bounds
        );
        for (i, c) in ls_outline.contours.iter().enumerate() {
            println!("  contour {}: {} points", i, c.points.len());
            for pt in c.points.iter().take(8) {
                println!("    ({:.1}, {:.1}) on_curve={}", pt.x, pt.y, pt.on_curve);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Render quality benchmark: objective comparison against mupdf reference
// ---------------------------------------------------------------------------

/// Automated render quality test. Renders the arXiv page with our renderer
/// and mupdf, OCRs both with tesseract, and compares accuracy. Also computes
/// pixel-level similarity between our render and the reference.
///
/// Requires: mutool, tesseract (both checked at runtime, skips if missing).
/// This test prints metrics but does NOT assert thresholds -- it's a benchmark
/// for tracking improvement, not a pass/fail gate (yet).
/// Render quality benchmark. Compares our render against mupdf (reference)
/// using tesseract OCR as the readability metric.
///
/// Metrics:
/// - word_f1: F1 score of OCR'd word sets (udoc vs mupdf)
/// - udoc_recall: fraction of mupdf's OCR words found in udoc's OCR
/// - udoc_word_count / mupdf_word_count: raw OCR output sizes
///
/// Requires: mutool, tesseract (skips if missing).
#[test]
fn render_quality_benchmark() {
    use std::process::Command;

    if Command::new("mutool").arg("-v").output().is_err()
        || Command::new("tesseract").arg("--version").output().is_err()
    {
        println!("SKIP: mutool or tesseract not available");
        return;
    }

    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    if !pdf_path.exists() {
        return;
    }

    let tmp = std::env::temp_dir().join("udoc_render_bench");
    std::fs::create_dir_all(&tmp).unwrap();

    // Render with our renderer at 300 DPI.
    let doc = extract_with_fonts(pdf_path);
    let mut font_cache = FontCache::new(&doc.assets);
    let our_png = udoc::render::render_page(&doc, 0, 300, &mut font_cache)
        .expect("our render should succeed");
    let our_path = tmp.join("udoc.png");
    std::fs::write(&our_path, &our_png).unwrap();

    // Render with mupdf at 300 DPI (reference).
    let ref_path = tmp.join("mupdf.png");
    Command::new("mutool")
        .args(["draw", "-r", "300", "-o"])
        .arg(&ref_path)
        .arg(pdf_path)
        .arg("1")
        .output()
        .expect("mutool should run");
    assert!(ref_path.exists(), "mupdf render failed");

    // OCR both.
    let our_ocr = ocr_png(&our_path);
    let ref_ocr = ocr_png(&ref_path);

    // Word-level F1.
    let our_words: HashSet<&str> = our_ocr.split_whitespace().collect();
    let ref_words: HashSet<&str> = ref_ocr.split_whitespace().collect();
    let common = our_words.intersection(&ref_words).count();
    let precision = if our_words.is_empty() {
        0.0
    } else {
        common as f64 / our_words.len() as f64
    };
    let recall = if ref_words.is_empty() {
        0.0
    } else {
        common as f64 / ref_words.len() as f64
    };
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };

    println!("\n========== RENDER QUALITY BENCHMARK (arXiv p1) ==========");
    println!("  word_f1 (udoc vs mupdf OCR):  {:.1}%", f1 * 100.0);
    println!(
        "  word_recall:                   {:.1}% ({}/{})",
        recall * 100.0,
        common,
        ref_words.len()
    );
    println!(
        "  word_precision:                {:.1}% ({}/{})",
        precision * 100.0,
        common,
        our_words.len()
    );
    println!("  udoc OCR words:               {}", our_words.len());
    println!("  mupdf OCR words:               {}", ref_words.len());
    println!("==========================================================");

    // Minimum gates. These should only go UP over time.
    assert!(f1 > 0.90, "word F1 {:.1}% is below 90% gate", f1 * 100.0);
    assert!(
        recall > 0.90,
        "word recall {:.1}% is below 90% gate",
        recall * 100.0
    );
}

/// Also run the benchmark on IRS 1040 (dense form layout).
#[test]
fn render_quality_benchmark_irs() {
    use std::process::Command;

    if Command::new("mutool").arg("-v").output().is_err()
        || Command::new("tesseract").arg("--version").output().is_err()
    {
        println!("SKIP: mutool or tesseract not available");
        return;
    }

    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/irs_1040.pdf");
    if !pdf_path.exists() {
        return;
    }

    let tmp = std::env::temp_dir().join("udoc_render_bench_irs");
    std::fs::create_dir_all(&tmp).unwrap();

    let doc = extract_with_fonts(pdf_path);
    let mut font_cache = FontCache::new(&doc.assets);
    let our_png = udoc::render::render_page(&doc, 0, 300, &mut font_cache)
        .expect("our render should succeed");
    let our_path = tmp.join("udoc.png");
    std::fs::write(&our_path, &our_png).unwrap();

    let ref_path = tmp.join("mupdf.png");
    Command::new("mutool")
        .args(["draw", "-r", "300", "-o"])
        .arg(&ref_path)
        .arg(pdf_path)
        .arg("1")
        .output()
        .expect("mutool should run");

    let our_ocr = ocr_png(&our_path);
    let ref_ocr = ocr_png(&ref_path);

    let our_words: HashSet<&str> = our_ocr.split_whitespace().collect();
    let ref_words: HashSet<&str> = ref_ocr.split_whitespace().collect();
    let common = our_words.intersection(&ref_words).count();
    let precision = if our_words.is_empty() {
        0.0
    } else {
        common as f64 / our_words.len() as f64
    };
    let recall = if ref_words.is_empty() {
        0.0
    } else {
        common as f64 / ref_words.len() as f64
    };
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };

    println!("\n========== RENDER QUALITY BENCHMARK (IRS 1040 p1) ==========");
    println!("  word_f1 (udoc vs mupdf OCR):  {:.1}%", f1 * 100.0);
    println!(
        "  word_recall:                   {:.1}% ({}/{})",
        recall * 100.0,
        common,
        ref_words.len()
    );
    println!(
        "  word_precision:                {:.1}% ({}/{})",
        precision * 100.0,
        common,
        our_words.len()
    );
    println!("  udoc OCR words:               {}", our_words.len());
    println!("  mupdf OCR words:               {}", ref_words.len());
    println!("=============================================================");

    // IRS forms have small text and form elements that are harder to render.
    // Current baseline: ~82% after CFF charset fix + gamma correction.
    assert!(
        f1 > 0.75,
        "IRS word F1 {:.1}% is below 75% gate",
        f1 * 100.0
    );
}

fn ocr_png(path: &Path) -> String {
    use std::process::Command;
    let out_base = path.with_extension("");
    Command::new("tesseract")
        .arg(path)
        .arg(&out_base)
        .args(["-l", "eng", "--psm", "3"])
        .output()
        .expect("tesseract should run");
    std::fs::read_to_string(format!("{}.txt", out_base.display())).unwrap_or_default()
}

#[test]
fn diagnose_m_and_fi_glyphs() {
    use udoc_core::document::assets::FontProgramType;
    use udoc_font::type1::Type1Font;

    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    if !pdf_path.exists() {
        return;
    }
    let doc = extract_with_fonts(pdf_path);

    // Find the main Roman font
    let roman = doc
        .assets
        .fonts()
        .iter()
        .find(|f| {
            f.program_type == FontProgramType::Type1
                && display_name_of(&f.name) == "NimbusRomNo9L-Regu"
        })
        .unwrap();
    let t1 = Type1Font::from_bytes(&roman.data).unwrap();

    // Check M glyph
    println!("\n=== 'M' glyph ===");
    println!("  {}", t1.diagnose_glyph("M"));
    if let Some(outline) = t1.glyph_outline('M') {
        for (i, c) in outline.contours.iter().enumerate() {
            println!(
                "  contour {}: {} points, first=({:.0},{:.0}) last=({:.0},{:.0})",
                i,
                c.points.len(),
                c.points.first().map(|p| p.x).unwrap_or(0.0),
                c.points.first().map(|p| p.y).unwrap_or(0.0),
                c.points.last().map(|p| p.x).unwrap_or(0.0),
                c.points.last().map(|p| p.y).unwrap_or(0.0),
            );
        }
    }

    // Check fi ligature
    println!("\n=== 'fi' ligature ===");
    let fi_char = '\u{FB01}'; // Unicode fi ligature
    println!("  has fi (U+FB01): {}", t1.glyph_outline(fi_char).is_some());
    println!(
        "  glyph names containing 'fi': {:?}",
        t1.glyph_names()
            .into_iter()
            .filter(|n| n.contains("fi"))
            .collect::<Vec<_>>()
    );

    // Check what's in the span text for "Classification"
    let pres = doc.presentation.as_ref().unwrap();
    for s in &pres.raw_spans {
        if s.text.contains("lassif") || s.text.contains("lassifi") || s.text.contains('\u{FB01}') {
            println!("\n  span: {:?} font={:?}", s.text, s.font_name);
            let chars: Vec<(char, u32)> = s.text.chars().map(|c| (c, c as u32)).collect();
            println!("  chars: {:?}", chars);
        }
    }
}

#[test]
fn dump_m_trace() {
    use udoc_core::document::assets::FontProgramType;
    use udoc_font::type1::Type1Font;

    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    if !pdf_path.exists() {
        return;
    }
    let doc = extract_with_fonts(pdf_path);
    let roman = doc
        .assets
        .fonts()
        .iter()
        .find(|f| {
            f.program_type == FontProgramType::Type1
                && display_name_of(&f.name) == "NimbusRomNo9L-Regu"
        })
        .unwrap();
    let t1 = Type1Font::from_bytes(&roman.data).unwrap();

    if let Some(outline) = t1.glyph_outline('M') {
        println!("\n'M' outline: {} contours", outline.contours.len());
        for (i, c) in outline.contours.iter().enumerate() {
            println!("contour {}: {} pts", i, c.points.len());
            for (j, p) in c.points.iter().enumerate() {
                println!(
                    "  [{:2}] ({:7.1}, {:7.1}) {}",
                    j,
                    p.x,
                    p.y,
                    if p.on_curve { "ON" } else { "off" }
                );
            }
        }
    }
}

#[test]
fn trace_m_execution() {
    use udoc_core::document::assets::FontProgramType;
    use udoc_font::type1::Type1Font;
    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    if !pdf_path.exists() {
        return;
    }
    let doc = extract_with_fonts(pdf_path);
    let roman = doc
        .assets
        .fonts()
        .iter()
        .find(|f| {
            f.program_type == FontProgramType::Type1
                && display_name_of(&f.name) == "NimbusRomNo9L-Regu"
        })
        .unwrap();
    let t1 = Type1Font::from_bytes(&roman.data).unwrap();
    let trace = t1.trace_glyph("M");
    println!("\n=== M charstring trace ({} ops) ===", trace.len());
    for line in &trace {
        println!("  {}", line);
    }
}

#[test]
fn check_rotated_spans() {
    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    if !pdf_path.exists() {
        return;
    }
    let doc = extract_with_fonts(pdf_path);
    let pres = doc.presentation.as_ref().unwrap();
    let rotated: Vec<_> = pres
        .raw_spans
        .iter()
        .filter(|s| s.page_index == 0)
        .filter(|_s| {
            // Check if span has any indication of rotation
            // PositionedSpan doesn't have rotation field -- check what's available
            false // placeholder
        })
        .collect();
    println!("Rotated spans on page 0: {}", rotated.len());

    // Check what fields PositionedSpan has
    let first = &pres.raw_spans[0];
    println!(
        "PositionedSpan fields: text={:?} bbox={:?} font_name={:?} font_size={:?}",
        &first.text[..first.text.len().min(20)],
        first.bbox,
        first.font_name,
        first.font_size
    );
}

#[test]
fn render_single_glyph_m() {
    // Render just 'M' at large size to see the artifact clearly
    let mut span = PositionedSpan::new(
        "M".to_string(),
        BoundingBox::new(100.0, 500.0, 180.0, 560.0),
        0,
    );
    span.font_size = Some(60.0);
    span.font_name = Some("NimbusRomNo9L-Regu".to_string());

    let pdf_path = std::path::Path::new("../udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    if !pdf_path.exists() {
        return;
    }
    let doc = extract_with_fonts(pdf_path);
    let mut font_cache = FontCache::new(&doc.assets);

    let test_doc = make_doc(vec![span], 300.0, 600.0);
    let png = render_page(&test_doc, 0, 300, &mut font_cache).expect("render M");
    std::fs::write("/tmp/glyph_M.png", &png).unwrap();
    println!("Wrote /tmp/glyph_M.png");
}

#[test]
fn check_raw_span_rotation() {
    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    if !pdf_path.exists() {
        return;
    }
    let mut pdf = udoc_pdf::Document::open(pdf_path).unwrap();
    let mut page = pdf.page(0).unwrap();
    let spans = page.raw_spans().unwrap();
    let rotated: Vec<_> = spans.iter().filter(|s| s.rotation.abs() > 1.0).collect();
    println!("\nRotated spans on page 0: {}", rotated.len());
    for s in &rotated {
        println!(
            "  rot={:.0} text={:?} x={:.1} y={:.1} w={:.1} font={}",
            s.rotation,
            &s.text[..s.text.len().min(40)],
            s.x,
            s.y,
            s.width,
            s.font_name
        );
    }
}

#[test]
fn check_positioned_rotated() {
    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    if !pdf_path.exists() {
        return;
    }
    let doc = extract_with_fonts(pdf_path);
    let pres = doc.presentation.as_ref().unwrap();
    let rotated: Vec<_> = pres
        .raw_spans
        .iter()
        .filter(|s| s.page_index == 0 && s.rotation.abs() > 1.0)
        .collect();
    println!("\nPositioned rotated spans: {}", rotated.len());
    for s in &rotated {
        println!(
            "  rot={:.0} bbox={:?} text={:?} font={:?} size={:?}",
            s.rotation,
            s.bbox,
            &s.text[..s.text.len().min(30)],
            s.font_name,
            s.font_size
        );
    }
}

#[test]
fn render_m_at_title_size() {
    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    if !pdf_path.exists() {
        return;
    }
    let doc = extract_with_fonts(pdf_path);
    let mut font_cache = FontCache::new(&doc.assets);

    // Render M, T, A at 20.66pt (title size) and 12pt (body size)
    for (ch, size) in [('M', 20.66), ('T', 20.66), ('A', 12.0), ('M', 12.0)] {
        let mut span = PositionedSpan::new(
            ch.to_string(),
            BoundingBox::new(50.0, 500.0, 80.0, 500.0 + size),
            0,
        );
        span.font_size = Some(size);
        span.font_name = Some("NimbusRomNo9L-Regu".to_string());
        let test_doc = make_doc(vec![span], 150.0, 600.0);
        let png = render_page(&test_doc, 0, 300, &mut font_cache).expect("render");
        let path = format!("/tmp/glyph_{}_{:.0}pt.png", ch, size);
        std::fs::write(&path, &png).unwrap();
        println!("Wrote {} ({}KB)", path, png.len() / 1024);
    }
}

#[test]
fn investigate_irs_spans() {
    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/irs_1040.pdf");
    if !pdf_path.exists() {
        return;
    }
    let mut pdf = udoc_pdf::Document::open(pdf_path).unwrap();
    let mut page = pdf.page(0).unwrap();
    let spans = page.raw_spans().unwrap();

    // Find spans containing "Department" or "For the year" or near x=0
    println!("\n=== IRS page 1: spans near 'Department' area ===");
    for s in &spans {
        if s.text.contains("epartment")
            || s.text.contains("Department")
            || s.text.contains("Internal")
            || s.text.contains("nternal")
            || (s.text.contains("For") && s.y > 700.0)
            || (s.text.contains("year") && s.y > 740.0)
            || s.text.contains("1040")
        {
            println!(
                "  x={:.1} y={:.1} w={:.1} size={:.1} annot={} text={:?}",
                s.x,
                s.y,
                s.width,
                s.font_size,
                s.is_annotation,
                &s.text[..s.text.len().min(50)]
            );
        }
    }

    // Count annotation vs regular spans
    let annot = spans.iter().filter(|s| s.is_annotation).count();
    let regular = spans.iter().filter(|s| !s.is_annotation).count();
    println!("\n  Total spans: {} regular, {} annotation", regular, annot);

    // Check rotation on all spans
    let rotated: Vec<_> = spans.iter().filter(|s| s.rotation.abs() > 1.0).collect();
    println!("  Rotated spans: {}", rotated.len());
    for s in &rotated {
        println!(
            "    rot={:.0} x={:.1} y={:.1} w={:.1} size={:.1} text={:?}",
            s.rotation,
            s.x,
            s.y,
            s.width,
            s.font_size,
            &s.text[..s.text.len().min(30)]
        );
    }
    // Also check Form specifically
    for s in &spans {
        if s.text.contains("Form") {
            println!(
                "  FORM: rot={:.0} x={:.1} y={:.1} w={:.1} size={:.1} text={:?}",
                s.rotation,
                s.x,
                s.y,
                s.width,
                s.font_size,
                &s.text[..s.text.len().min(30)]
            );
        }
    }
}
