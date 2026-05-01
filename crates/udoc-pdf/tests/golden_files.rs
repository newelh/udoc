//! Golden-file tests: verify text extraction produces expected output.
//!
//! Each test opens a PDF, extracts text via the public API, and compares
//! against a `.expected.txt` file. Any change to text extraction output
//! requires updating the golden file (intentional friction).
//!
//! Note: golden files capture the current extraction output, including known
//! imperfections. They are regression-prevention snapshots, not ideal or
//! hand-corrected ground truth. Use UDOC_BLESS=1 to update them when
//! extraction improves.
//!
//! Golden files live in tests/golden/<name>.expected.txt alongside the
//! corpus PDFs in tests/corpus/minimal/<name>.pdf.

use udoc_core::test_harness::unified_diff;
use udoc_pdf::Document;

fn corpus_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/corpus/minimal")
        .join(name)
}

fn realworld_corpus_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/corpus/realworld")
        .join(name)
}

fn encrypted_corpus_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/corpus/encrypted")
        .join(name)
}

fn golden_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.expected.txt"))
}

/// Extract all text from a PDF, joining pages with a page separator.
fn extract_full_text(pdf_name: &str) -> String {
    let path = corpus_path(pdf_name);
    let mut doc =
        Document::open(&path).unwrap_or_else(|e| panic!("failed to open {pdf_name}: {e}"));

    let mut pages = Vec::new();
    for i in 0..doc.page_count() {
        let mut page = doc
            .page(i)
            .unwrap_or_else(|e| panic!("{pdf_name} page {i}: {e}"));
        let text = page
            .text()
            .unwrap_or_else(|e| panic!("{pdf_name} page {i} text: {e}"));
        pages.push(text);
    }
    pages.join("\n--- PAGE BREAK ---\n")
}

/// Extract all text from a realworld corpus PDF.
fn extract_realworld_text(pdf_name: &str) -> String {
    let path = realworld_corpus_path(pdf_name);
    let mut doc =
        Document::open(&path).unwrap_or_else(|e| panic!("failed to open {pdf_name}: {e}"));

    let mut pages = Vec::new();
    for i in 0..doc.page_count() {
        let mut page = doc
            .page(i)
            .unwrap_or_else(|e| panic!("{pdf_name} page {i}: {e}"));
        let text = page
            .text()
            .unwrap_or_else(|e| panic!("{pdf_name} page {i} text: {e}"));
        pages.push(text);
    }
    pages.join("\n--- PAGE BREAK ---\n")
}

/// Compare extracted realworld text against expected golden file.
fn assert_golden_realworld(pdf_name: &str) {
    assert_golden_impl(pdf_name, &extract_realworld_text(pdf_name));
}

/// Extract all text from an encrypted PDF, with optional password.
fn extract_encrypted_text(pdf_name: &str, password: Option<&[u8]>) -> String {
    let path = encrypted_corpus_path(pdf_name);
    let data = std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {pdf_name}: {e}"));
    let mut doc = match password {
        Some(pw) => Document::from_bytes_with_password(data, pw)
            .unwrap_or_else(|e| panic!("failed to open {pdf_name} with password: {e}")),
        None => {
            Document::from_bytes(data).unwrap_or_else(|e| panic!("failed to open {pdf_name}: {e}"))
        }
    };

    let mut pages = Vec::new();
    for i in 0..doc.page_count() {
        let mut page = doc
            .page(i)
            .unwrap_or_else(|e| panic!("{pdf_name} page {i}: {e}"));
        let text = page
            .text()
            .unwrap_or_else(|e| panic!("{pdf_name} page {i} text: {e}"));
        pages.push(text);
    }
    pages.join("\n--- PAGE BREAK ---\n")
}

/// Compare extracted text against expected golden file.
///
/// On mismatch, prints a clear diff showing what changed. Trailing
/// whitespace on each line is trimmed for comparison to avoid
/// platform-specific line ending issues.
fn assert_golden(pdf_name: &str) {
    assert_golden_impl(pdf_name, &extract_full_text(pdf_name));
}

/// Compare extracted text from an encrypted PDF against expected golden file.
fn assert_golden_encrypted(pdf_name: &str, password: Option<&[u8]>) {
    assert_golden_impl(pdf_name, &extract_encrypted_text(pdf_name, password));
}

fn assert_golden_impl(pdf_name: &str, actual: &str) {
    let golden = golden_path(pdf_name);

    // BLESS=1: create or overwrite golden file
    let is_bless = std::env::var("BLESS")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));

    let expected = match std::fs::read_to_string(&golden) {
        Ok(s) => s,
        Err(_) if is_bless => {
            std::fs::write(&golden, actual).unwrap_or_else(|e| {
                panic!("failed to create golden file {}: {e}", golden.display())
            });
            eprintln!("Created golden file: {}", golden.display());
            return;
        }
        Err(e) => {
            panic!(
                "failed to read golden file {}: {e}\n\
                 Hint: run with BLESS=1 to create it,\n\
                 or create it manually with the expected text output.",
                golden.display()
            )
        }
    };

    // Normalize: trim trailing whitespace per line, trim trailing newlines
    let normalize = |s: &str| -> String {
        s.lines()
            .map(|l| l.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_string()
    };

    let actual_norm = normalize(actual);
    let expected_norm = normalize(&expected);

    if actual_norm != expected_norm {
        if is_bless {
            std::fs::write(&golden, actual).unwrap_or_else(|e| {
                panic!("failed to bless golden file {}: {e}", golden.display())
            });
            eprintln!("Blessed golden file: {}", golden.display());
            return;
        }

        // Build a unified-style diff using simple LCS to handle
        // insertions and deletions, not just same-index mismatches.
        let actual_lines: Vec<&str> = actual_norm.lines().collect();
        let expected_lines: Vec<&str> = expected_norm.lines().collect();
        let diff = unified_diff(&expected_lines, &actual_lines);

        panic!(
            "Golden file mismatch for {pdf_name}\n\
             Golden file: {}\n\
             Differences:\n{diff}\n\
             --- EXPECTED ---\n{expected_norm}\n\
             --- ACTUAL ---\n{actual_norm}\n\
             ---\n\
             To update: copy actual output to {}",
            golden.display(),
            golden.display(),
        );
    }
}

// ---------------------------------------------------------------------------
// Basic text extraction golden files
// ---------------------------------------------------------------------------

#[test]
fn golden_winansi_type1() {
    assert_golden("winansi_type1.pdf");
}

#[test]
fn golden_macroman_type1() {
    assert_golden("macroman_type1.pdf");
}

#[test]
fn golden_flate_content() {
    assert_golden("flate_content.pdf");
}

#[test]
fn golden_content_array() {
    assert_golden("content_array.pdf");
}

#[test]
fn golden_form_xobject() {
    assert_golden("form_xobject.pdf");
}

#[test]
fn golden_wrong_length() {
    assert_golden("wrong_length.pdf");
}

// ---------------------------------------------------------------------------
// Multi-page golden files
// ---------------------------------------------------------------------------

#[test]
fn golden_multipage() {
    assert_golden("multipage.pdf");
}

#[test]
fn golden_two_flate_streams() {
    assert_golden("two_flate_streams.pdf");
}

// ---------------------------------------------------------------------------
// Empty / no-text pages (verify they produce empty output)
// ---------------------------------------------------------------------------

#[test]
fn golden_empty_page() {
    assert_golden("empty_page.pdf");
}

#[test]
fn golden_minimal_14() {
    assert_golden("minimal_14.pdf");
}

// ---------------------------------------------------------------------------
// CID / CJK golden files
// ---------------------------------------------------------------------------

#[test]
fn golden_cid_cff() {
    assert_golden("cid_cff.pdf");
}

#[test]
fn golden_text_clip_cff_cid() {
    assert_golden("text_clip_cff_cid.pdf");
}

#[test]
fn golden_arabic_cid_truetype() {
    assert_golden("ArabicCIDTrueType.pdf");
}

// ---------------------------------------------------------------------------
// Complex / real-world golden files
// ---------------------------------------------------------------------------

#[test]
fn golden_xelatex() {
    assert_golden("xelatex.pdf");
}

#[test]
fn golden_ostream1() {
    assert_golden("ostream1.pdf");
}

#[test]
fn golden_ostream2() {
    assert_golden("ostream2.pdf");
}

#[test]
fn golden_xelatex_drawboard() {
    assert_golden("xelatex-drawboard.pdf");
}

// ---------------------------------------------------------------------------
// .5: ExtGState and marked content golden files
// ---------------------------------------------------------------------------

#[test]
fn golden_extgstate_font() {
    assert_golden("extgstate_font.pdf");
}

#[test]
fn golden_marked_content() {
    assert_golden("marked_content.pdf");
}

#[test]
fn golden_flate_extgstate() {
    assert_golden("flate_extgstate.pdf");
}

#[test]
fn golden_invisible_text() {
    assert_golden("invisible_text.pdf");
}

// ---------------------------------------------------------------------------
// .5: Text operator coverage golden files
// ---------------------------------------------------------------------------

#[test]
fn golden_multiple_fonts() {
    assert_golden("multiple_fonts.pdf");
}

#[test]
fn golden_tj_array() {
    assert_golden("tj_array.pdf");
}

#[test]
fn golden_text_state() {
    assert_golden("text_state.pdf");
}

#[test]
fn golden_single_quote() {
    assert_golden("single_quote.pdf");
}

#[test]
fn golden_double_quote() {
    assert_golden("double_quote.pdf");
}

#[test]
fn golden_multi_bt() {
    assert_golden("multi_bt.pdf");
}

#[test]
fn golden_text_matrix() {
    assert_golden("text_matrix.pdf");
}

#[test]
fn golden_ctm_text() {
    assert_golden("ctm_text.pdf");
}

#[test]
fn golden_hex_string() {
    assert_golden("hex_string.pdf");
}

#[test]
fn golden_escaped_parens() {
    assert_golden("escaped_parens.pdf");
}

#[test]
fn golden_td_operator() {
    assert_golden("td_operator.pdf");
}

#[test]
fn golden_nested_xobject() {
    assert_golden("nested_xobject.pdf");
}

#[test]
fn golden_save_restore() {
    assert_golden("save_restore.pdf");
}

// ---------------------------------------------------------------------------
// Reading order v2 golden files (O-006)
// ---------------------------------------------------------------------------

#[test]
fn golden_two_column() {
    assert_golden("two_column.pdf");
}

#[test]
fn golden_three_column() {
    assert_golden("three_column.pdf");
}

#[test]
fn golden_rotated_text() {
    assert_golden("rotated_text.pdf");
}

#[test]
fn golden_table_layout() {
    assert_golden("table_layout.pdf");
}

// ---------------------------------------------------------------------------
// Image extraction golden files (I-003)
// ---------------------------------------------------------------------------

#[test]
fn golden_inline_image() {
    assert_golden("inline_image.pdf");
}

#[test]
fn golden_image_xobject() {
    assert_golden("image_xobject.pdf");
}

// ---------------------------------------------------------------------------
// CMap parser integration tests (CM-011)
// ---------------------------------------------------------------------------

#[test]
fn golden_embedded_cmap_bfchar() {
    assert_golden("embedded_cmap_bfchar.pdf");
}

#[test]
fn golden_embedded_cmap_cidrange() {
    assert_golden("embedded_cmap_cidrange.pdf");
}

// ---------------------------------------------------------------------------
// Structure tree / marked content integration tests (MC-008)
// ---------------------------------------------------------------------------

#[test]
fn golden_tagged_structure_tree() {
    assert_golden("tagged_structure_tree.pdf");
}

// ---------------------------------------------------------------------------
// Type3 font golden files (T3-009)
// ---------------------------------------------------------------------------

#[test]
fn golden_simpletype3font() {
    assert_golden("simpletype3font.pdf");
}

#[test]
fn golden_type3_word_spacing() {
    assert_golden("type3_word_spacing.pdf");
}

#[test]
fn golden_type3_shapes() {
    assert_golden("type3_shapes.pdf");
}

#[test]
fn golden_type3_tounicode() {
    // Type3 font with ToUnicode CMap mapping shape glyphs to real characters.
    // Tests that ToUnicode takes priority in the Type3 fallback chain.
    assert_golden("type3_tounicode.pdf");
}

#[test]
fn golden_type3_agl_encoding() {
    // Type3 font using standard AGL glyph names (/A, /B, /C) in /Differences.
    // Tests the AGL lookup path for Type3 fonts.
    assert_golden("type3_agl_encoding.pdf");
}

#[test]
fn golden_type3_shapes_only() {
    // Type3 font with character codes in the control range (1-2) where
    // the base encoding has no mapping. Glyph names (/blob, /splat)
    // are not in the AGL and there is no ToUnicode. All 6 glyphs
    // produce U+FFFD (replacement character).
    assert_golden("type3_shapes_only.pdf");
}

// ---------------------------------------------------------------------------
// Encrypted PDF golden files
// ---------------------------------------------------------------------------

#[test]
fn golden_rc4_40_empty_password() {
    assert_golden_encrypted("rc4_40_empty_password.pdf", None);
}

#[test]
fn golden_rc4_128_user_password() {
    assert_golden_encrypted("rc4_128_user_password.pdf", Some(b"test123"));
}

#[test]
fn golden_rc4_128_owner_only() {
    assert_golden_encrypted("rc4_128_owner_only.pdf", None);
}

#[test]
fn golden_rc4_128_both_passwords() {
    assert_golden_encrypted("rc4_128_both_passwords.pdf", Some(b"user_pw"));
}

#[test]
fn golden_rc4_128_objstm() {
    assert_golden_encrypted("rc4_128_objstm.pdf", None);
}

#[test]
fn golden_unencrypted_baseline() {
    assert_golden_encrypted("unencrypted_baseline.pdf", None);
}

#[test]
fn golden_aes128_empty_password() {
    assert_golden_encrypted("aes128_empty_password.pdf", None);
}

#[test]
fn golden_aes128_user_password() {
    assert_golden_encrypted("aes128_user_password.pdf", Some(b"aespass"));
}

#[test]
fn golden_aes128_both_passwords() {
    assert_golden_encrypted("aes128_both_passwords.pdf", Some(b"user_aes"));
}

#[test]
fn golden_aes256_empty_password() {
    assert_golden_encrypted("aes256_empty_password.pdf", None);
}

#[test]
fn golden_aes256_user_password() {
    assert_golden_encrypted("aes256_user_password.pdf", Some(b"test256"));
}

#[test]
fn golden_aes256_owner_password() {
    assert_golden_encrypted("aes256_owner_password.pdf", None);
}

#[test]
fn golden_aes256_both_passwords() {
    assert_golden_encrypted("aes256_both_passwords.pdf", Some(b"user256"));
}

// --- Benchmark-derived golden files ---
// Files scoring 1.0 char accuracy in benchmark baseline.
// These catch regressions in cargo test (seconds) without
// needing external tools.

#[test]
fn golden_truetype_without_cmap() {
    assert_golden("TrueType_without_cmap.pdf");
}

#[test]
fn golden_minimal_10() {
    assert_golden("minimal_10.pdf");
}

#[test]
fn golden_minimal_20() {
    assert_golden("minimal_20.pdf");
}

#[test]
fn golden_nested_pages() {
    assert_golden("nested_pages.pdf");
}

#[test]
fn golden_pseudo_linearized() {
    assert_golden("pseudo_linearized.pdf");
}

#[test]
fn golden_with_info() {
    assert_golden("with_info.pdf");
}

// ---------------------------------------------------------------------------
// X-Y cut multi-column golden files ()
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Multi-column and parse recovery golden files (T2-GOLDEN)
// ---------------------------------------------------------------------------

#[test]
fn golden_twocol_header_footer() {
    // Two-column with full-width title and footer (academic paper layout).
    // Stream-coherent ordering interleaves left and right column rows,
    // with the footer correctly placed at the end of the page.
    assert_golden("twocol_header_footer.pdf");
}

#[test]
fn golden_render_mode3_ocr() {
    // All text in Tr=3 (invisible OCR layer). Currently produces empty output
    // because the interpreter skips Tr=3 spans.
    assert_golden("render_mode3_ocr.pdf");
}

#[test]
fn golden_twocol_unequal() {
    // Two columns with unequal widths (narrow sidebar + wide main content).
    // Note: the narrow gap (~28pt) between sidebar and main content is below
    // the X-Y cut column detection threshold, so lines merge across columns.
    // See issue #58 for improving narrow-gap column detection.
    assert_golden("twocol_unequal.pdf");
}

#[test]
fn golden_missing_type_pages() {
    // Page tree node missing /Type, recovered via /Kids heuristic.
    assert_golden("missing_type_pages.pdf");
}

#[test]
fn golden_multipage_twocol() {
    // Two pages, each with two-column layout.
    assert_golden("multipage_twocol.pdf");
}

#[test]
fn golden_mixed_render_modes() {
    // Mix of visible (Tr=0) and invisible (Tr=3) text on the same page.
    // Both visible and invisible text should appear in output (is_invisible flag
    // is set on Tr=3 spans, but text() includes all spans).
    assert_golden("mixed_render_modes.pdf");
}

#[test]
fn golden_realworld_multicolumn() {
    assert_golden_realworld("multicolumn.pdf");
}

#[test]
fn golden_realworld_nist_sp1300() {
    assert_golden_realworld("nist_sp1300.pdf");
}

#[test]
fn golden_realworld_irs_w9() {
    assert_golden_realworld("irs_w9.pdf");
}

#[test]
fn golden_realworld_udoc_beats_both() {
    // arXiv 2601.20662: udoc=0.978, poppler=0.845, pdfium=0.926.
    // Multi-column academic paper where our X-Y cut ordering outperforms
    // both poppler and pdfium.
    assert_golden_realworld("udoc_beats_both.pdf");
}

#[test]
fn golden_realworld_poppler_failure() {
    // arXiv 2601.04335: udoc=0.969, poppler=0.286, pdfium=0.925.
    // Paper where poppler's ordering completely breaks down but our
    // stream-sequential approach handles correctly.
    assert_golden_realworld("poppler_failure.pdf");
}

#[test]
fn golden_realworld_xy_cut_win() {
    // HF-PDFA 7688214: udoc=0.993, poppler=0.688, pdfium=0.723.
    // Document where our X-Y cut ordering significantly outperforms both.
    assert_golden_realworld("udoc_xy_cut_win.pdf");
}

#[test]
fn golden_realworld_worldbank_win() {
    // WorldBank 19603493: udoc=0.931, poppler=0.709, pdfium=0.821.
    // World Bank document where udoc ordering is more accurate.
    assert_golden_realworld("udoc_worldbank_win.pdf");
}

#[test]
fn golden_realworld_verapdf_win() {
    // VeraPDF 7.2-t30-fail-a: udoc=1.000, poppler=0.167, pdfium=0.167.
    // Validation test file where udoc produces perfect extraction.
    // Expected output is "Text" (not "Text object (ActualText)") because
    // the PDF wraps the content in a /ActualText marked content sequence
    // whose value is "Text", which correctly overrides glyph-decoded text.
    assert_golden_realworld("udoc_verapdf_win.pdf");
}

// ---------------------------------------------------------------------------
// Diverse producer golden files (H-014)
// PDFs from the 20K corpus covering different PDF producers.
// ---------------------------------------------------------------------------

#[test]
fn golden_realworld_arxiv_pdflatex() {
    // arXiv econ paper produced by pdfTeX/pikepdf (arXiv GenPDF pipeline).
    // Exercises pdfLaTeX font subsetting, Type1 fonts, math formulae.
    assert_golden_realworld("arxiv_pdflatex.pdf");
}

#[test]
fn golden_realworld_google_docs_skia() {
    // Google Docs PDF rendered via Skia/PDF (Chrome print engine).
    // Single-page document with simple text layout.
    assert_golden_realworld("google_docs_skia.pdf");
}

#[test]
fn golden_realworld_ms_print_to_pdf() {
    // Microsoft Print to PDF (Windows built-in virtual printer).
    // Parish council agenda with some garbled column ordering.
    assert_golden_realworld("ms_print_to_pdf.pdf");
}

#[test]
fn golden_realworld_itextsharp_report() {
    // iTextSharp 4.1.6 generated financial report.
    // Tables with numeric data, tests column-aligned text extraction.
    assert_golden_realworld("itextsharp_report.pdf");
}

#[test]
fn golden_realworld_cjk_japanese() {
    // Japanese text (Internet Archive). CJK fullwidth characters,
    // hiragana, katakana, and kanji. Validates CJK visual width spacing.
    assert_golden_realworld("cjk_japanese.pdf");
}

#[test]
fn golden_realworld_cjk_korean() {
    // Korean quantum mechanics textbook (Internet Archive).
    // Hangul text with mathematical notation. Validates CJK extraction
    // and the H-008 CJK visual width fix.
    assert_golden_realworld("cjk_korean.pdf");
}

#[test]
fn golden_realworld_cjk_chinese() {
    // Chinese Buddhist text with pinyin (Internet Archive).
    // Mix of CJK ideographs and Latin pinyin text. Validates CJK
    // character mapping and mixed-script line assembly.
    assert_golden_realworld("cjk_chinese.pdf");
}
