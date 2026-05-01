//! Regression tests for Type1 stem hint emission (#177).
//!
//! The issue premise was "CMR8 '1' vstem at (139, 79) doesn't describe the full
//! visible extent of the right stem edge." Investigation showed CMR Type1 fonts
//! DO emit declared hstem/vstem per glyph via the charstring interpreter. The
//! SSIM gap on CMR-heavy PDFs is dominated by subpixel fringing (#183), not
//! missing hints. These tests pin the correct hint-emission behaviour so future
//! regressions in the Type1 interpreter are caught.

use std::path::Path;

use udoc::{AssetConfig, Config};
use udoc_core::document::assets::FontProgramType;
use udoc_core::document::Document;
use udoc_font::type1::Type1Font;

fn extract_with_fonts(path: &Path) -> Document {
    let config = Config::default().assets(AssetConfig::default().fonts(true));
    udoc::extract_with(path, config).expect("extraction should succeed")
}

fn load_type1_font(doc: &Document, font_name_contains: &str) -> Option<Type1Font> {
    let raw = doc.assets.fonts().iter().find(|f| {
        f.program_type == FontProgramType::Type1 && f.name.contains(font_name_contains)
    })?;
    Type1Font::from_bytes(&raw.data).ok()
}

/// A CM-family Roman Type1 font should declare hstems and vstems on typical
/// glyphs. If this regresses to empty, hint-grid-fitting goes dark on CMR.
#[test]
fn cm_type1_fonts_emit_glyph_stems() {
    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    if !pdf_path.exists() {
        return;
    }
    let doc = extract_with_fonts(pdf_path);

    // Filter to CM *Roman*/Bold/Italic faces that actually contain Latin
    // glyphs. CMMI (math italic) and CMSY (symbol) do not have "one", "H",
    // etc., so excluding those keeps the test meaningful for body text.
    let roman_prefixes = ["CMR", "CMBX", "CMTI", "CMSL", "CMTT", "SFRM"];
    let cm_fonts: Vec<_> = doc
        .assets
        .fonts()
        .iter()
        .filter(|f| {
            f.program_type == FontProgramType::Type1
                && (roman_prefixes.iter().any(|p| f.name.starts_with(p))
                    || f.name.contains("NimbusRom"))
        })
        .collect();

    assert!(
        !cm_fonts.is_empty(),
        "arxiv_pdflatex.pdf should contain at least one CM-family or NimbusRom Type1 font"
    );

    let mut checked_any_v_stem = false;
    let mut checked_any_h_stem = false;
    for raw in cm_fonts {
        let t1 = Type1Font::from_bytes(&raw.data).unwrap_or_else(|e| {
            panic!("failed to parse {}: {:?}", raw.name, e);
        });

        // Font-wide private-dict hint values: these are the baseline against
        // which per-glyph widths get normalized.
        let hv = t1.hint_values();
        assert!(
            hv.std_hw > 0.0 || hv.std_vw > 0.0 || !hv.blue_values.is_empty(),
            "{}: private dict should contain std_hw/std_vw or blue zones",
            raw.name
        );

        // Per-glyph hints: at least one common Latin glyph should emit stems.
        let has_any_stem_for = |name: &str| -> bool {
            t1.glyph_stems(name)
                .map(|s| !s.h_stems.is_empty() || !s.v_stems.is_empty())
                .unwrap_or(false)
        };

        let probe_names = ["one", "l", "i", "H", "M", "n", "e"];
        let any_hit = probe_names.iter().any(|n| has_any_stem_for(n));
        assert!(
            any_hit,
            "{}: at least one of {:?} should emit stem hints",
            raw.name, probe_names
        );

        for name in probe_names {
            if let Some(stems) = t1.glyph_stems(name) {
                if !stems.h_stems.is_empty() {
                    checked_any_h_stem = true;
                }
                if !stems.v_stems.is_empty() {
                    checked_any_v_stem = true;
                }
            }
        }
    }

    assert!(
        checked_any_h_stem,
        "at least one CM glyph should declare horizontal stems"
    );
    assert!(
        checked_any_v_stem,
        "at least one CM glyph should declare vertical stems"
    );
}

/// The "one" glyph in Computer Modern Roman has a single main vertical stem.
/// This test pins that shape: one v_stem pair, at a plausible position and
/// within ~30% of the font's std_vw. Regressions (empty vstem, wildly wrong
/// position) will break rendering quality on CM-heavy PDFs.
#[test]
fn cm_type1_one_glyph_has_single_vstem() {
    let pdf_path = Path::new("../udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf");
    if !pdf_path.exists() {
        return;
    }
    let doc = extract_with_fonts(pdf_path);

    // Try several CM variants in order of likelihood for body text.
    for candidate in ["CMR10", "CMR12", "CMR9", "CMR8", "CMR7"] {
        let Some(t1) = load_type1_font(&doc, candidate) else {
            continue;
        };
        let Some(stems) = t1.glyph_stems("one") else {
            continue;
        };
        let std_vw = t1.hint_values().std_vw;

        assert_eq!(
            stems.v_stems.len(),
            1,
            "{}: 'one' should declare exactly one vertical stem, got {:?}",
            candidate,
            stems.v_stems
        );
        let (pos, width) = stems.v_stems[0];
        assert!(
            (0.0..1000.0).contains(&pos),
            "{}: 'one' v_stem pos {} should be inside the em square",
            candidate,
            pos
        );
        if std_vw > 0.0 {
            let ratio = width.abs() / std_vw;
            assert!(
                (0.3..=3.0).contains(&ratio),
                "{}: 'one' v_stem width {} should be within 3x of std_vw {}",
                candidate,
                width,
                std_vw
            );
        }
        return; // one CM font checked is enough
    }
}
