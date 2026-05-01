use super::*;

fn corpus_path(name: &str) -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests");
    p.push("corpus");
    p.push(name);
    p
}

fn parse_corpus(name: &str) -> ParsedDocument {
    let data = std::fs::read(corpus_path(name)).expect("failed to read corpus file");
    Parser::parse(&data).expect("parse failed")
}

/// Collect all visible text from paragraphs into a single string.
fn all_paragraph_text(doc: &ParsedDocument) -> String {
    let mut out = String::new();
    for (i, para) in doc.paragraphs.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        for run in &para.runs {
            out.push_str(&run.text);
        }
    }
    out
}

#[test]
fn parse_basic_paragraphs() {
    let doc = parse_corpus("basic.rtf");
    assert_eq!(doc.paragraphs.len(), 3);

    let p0: String = doc.paragraphs[0]
        .runs
        .iter()
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(p0, "Hello, world!");

    let p1: String = doc.paragraphs[1]
        .runs
        .iter()
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(p1, "This is a simple RTF document.");

    let p2: String = doc.paragraphs[2]
        .runs
        .iter()
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(p2, "It has three paragraphs.");
}

#[test]
fn parse_basic_font_table() {
    let doc = parse_corpus("basic.rtf");
    assert_eq!(doc.fonts.len(), 1);
    assert_eq!(doc.fonts[0].name, "Times New Roman");
    assert_eq!(doc.fonts[0].family, FontFamily::Roman);
}

#[test]
fn parse_formatting_bold_italic() {
    let doc = parse_corpus("formatting.rtf");

    // First paragraph: "Normal text. Bold text. Normal again."
    let p0 = &doc.paragraphs[0];
    // Find a bold run.
    let bold_runs: Vec<_> = p0.runs.iter().filter(|r| r.bold).collect();
    assert!(!bold_runs.is_empty(), "expected bold runs in paragraph 0");
    let bold_text: String = bold_runs.iter().map(|r| r.text.as_str()).collect();
    assert!(bold_text.contains("Bold text."), "got: {bold_text}");

    // Non-bold runs should exist.
    let normal_runs: Vec<_> = p0.runs.iter().filter(|r| !r.bold).collect();
    assert!(!normal_runs.is_empty(), "expected normal runs");

    // Second paragraph: italic and bold+italic.
    let p1 = &doc.paragraphs[1];
    let italic_runs: Vec<_> = p1.runs.iter().filter(|r| r.italic).collect();
    assert!(
        !italic_runs.is_empty(),
        "expected italic runs in paragraph 1"
    );

    let bold_italic: Vec<_> = p1.runs.iter().filter(|r| r.bold && r.italic).collect();
    assert!(!bold_italic.is_empty(), "expected bold+italic runs");
}

#[test]
fn parse_formatting_font_switch() {
    let doc = parse_corpus("formatting.rtf");
    assert!(doc.fonts.len() >= 2, "expected at least 2 fonts");

    // Should have Times New Roman and Arial.
    let names: Vec<&str> = doc.fonts.iter().map(|f| f.name.as_str()).collect();
    assert!(names.contains(&"Times New Roman"), "got: {names:?}");
    assert!(names.contains(&"Arial"), "got: {names:?}");

    // The "Larger Arial text." paragraph should reference Arial.
    let p2 = &doc.paragraphs[2];
    let arial_runs: Vec<_> = p2
        .runs
        .iter()
        .filter(|r| r.font_name.as_deref() == Some("Arial"))
        .collect();
    assert!(
        !arial_runs.is_empty(),
        "expected Arial font run in paragraph 2"
    );
    // Font size should be 14pt (fs28 = 28 half-points = 14pt).
    let size = arial_runs[0].font_size_pts;
    assert!(
        (size - 14.0).abs() < f64::EPSILON,
        "expected 14pt, got {size}"
    );
}

#[test]
fn parse_formatting_underline() {
    let doc = parse_corpus("formatting.rtf");

    // Last paragraph has underlined text followed by non-underlined.
    let last = doc.paragraphs.last().expect("no paragraphs");
    let ul_runs: Vec<_> = last.runs.iter().filter(|r| r.underline).collect();
    assert!(!ul_runs.is_empty(), "expected underlined runs");
    let ul_text: String = ul_runs.iter().map(|r| r.text.as_str()).collect();
    assert!(ul_text.contains("Underlined text."), "got: {ul_text}");
}

#[test]
fn parse_unicode_basic() {
    let doc = parse_corpus("unicode.rtf");
    let text = all_paragraph_text(&doc);

    // Euro sign U+20AC
    assert!(text.contains('\u{20AC}'), "missing euro sign in: {text}");
    // Em dash U+2014
    assert!(text.contains('\u{2014}'), "missing em dash in: {text}");
    // Greek Gamma U+0393
    assert!(text.contains('\u{0393}'), "missing Greek Gamma in: {text}");
}

#[test]
fn parse_unicode_skip_count() {
    let doc = parse_corpus("unicode.rtf");
    let text = all_paragraph_text(&doc);

    // \uc0 means zero skip: \u169 should produce (c) with no fallback skip.
    assert!(text.contains('\u{00A9}'), "missing copyright in: {text}");

    // \uc2 means skip two bytes after \u: the \u8364 should appear
    // and the two \'xx bytes should be skipped.
    // Count how many euro signs appear. Should be at least 2
    // (one from basic euro, one from uc2 test).
    let euro_count = text.chars().filter(|&c| c == '\u{20AC}').count();
    assert!(
        euro_count >= 2,
        "expected at least 2 euro signs, got {euro_count} in: {text}"
    );
}

#[test]
fn parse_metadata() {
    let doc = parse_corpus("metadata.rtf");
    assert_eq!(doc.metadata.title.as_deref(), Some("Test Document"));
    assert_eq!(doc.metadata.author.as_deref(), Some("Test Author"));
    assert_eq!(doc.metadata.subject.as_deref(), Some("Testing RTF"));
}

#[test]
fn parse_table_basic() {
    let doc = parse_corpus("table_basic.rtf");
    assert_eq!(doc.tables.len(), 1, "expected 1 table");

    let table = &doc.tables[0];
    assert_eq!(table.rows.len(), 2, "expected 2 rows");

    // First row: Name, Age, City
    let r0 = &table.rows[0];
    assert_eq!(r0.cells.len(), 3, "expected 3 cells in row 0");

    let cell_texts: Vec<String> = r0
        .cells
        .iter()
        .map(|c| c.runs.iter().map(|r| r.text.as_str()).collect())
        .collect();
    assert_eq!(cell_texts[0], "Name");
    assert_eq!(cell_texts[1], "Age");
    assert_eq!(cell_texts[2], "City");

    // Second row: Alice, 30, Boston
    let r1 = &table.rows[1];
    let cell_texts: Vec<String> = r1
        .cells
        .iter()
        .map(|c| c.runs.iter().map(|r| r.text.as_str()).collect())
        .collect();
    assert_eq!(cell_texts[0], "Alice");
    assert_eq!(cell_texts[1], "30");
    assert_eq!(cell_texts[2], "Boston");

    // Cell boundaries
    assert_eq!(r0.cell_boundaries, vec![3000, 6000, 9000]);
}

#[test]
fn parse_table_with_surrounding_text() {
    let doc = parse_corpus("table_basic.rtf");
    let text = all_paragraph_text(&doc);

    // Should have "Text before table." and "Text after table."
    assert!(
        text.contains("Text before table."),
        "missing pre-table text in: {text}"
    );
    assert!(
        text.contains("Text after table."),
        "missing post-table text in: {text}"
    );
}

#[test]
fn parse_hidden_text() {
    let doc = parse_corpus("hidden_text.rtf");

    // Should have visible and invisible runs.
    let all_runs: Vec<&TextRun> = doc.paragraphs.iter().flat_map(|p| &p.runs).collect();

    let visible: Vec<_> = all_runs.iter().filter(|r| !r.invisible).collect();
    let hidden: Vec<_> = all_runs.iter().filter(|r| r.invisible).collect();

    assert!(!visible.is_empty(), "expected visible runs");
    assert!(!hidden.is_empty(), "expected hidden runs");

    let hidden_text: String = hidden.iter().map(|r| r.text.as_str()).collect();
    assert!(
        hidden_text.contains("Hidden text."),
        "got hidden: {hidden_text}"
    );

    let visible_text: String = visible.iter().map(|r| r.text.as_str()).collect();
    assert!(
        visible_text.contains("Visible text."),
        "got visible: {visible_text}"
    );
    assert!(
        visible_text.contains("More visible text."),
        "got visible: {visible_text}"
    );
}

#[test]
fn parse_special_chars_braces() {
    let doc = parse_corpus("special_chars.rtf");
    let text = all_paragraph_text(&doc);
    assert!(text.contains('{'), "missing open brace in: {text}");
    assert!(text.contains('}'), "missing close brace in: {text}");
}

#[test]
fn parse_special_chars_backslash() {
    let doc = parse_corpus("special_chars.rtf");
    let text = all_paragraph_text(&doc);
    assert!(text.contains('\\'), "missing backslash in: {text}");
}

#[test]
fn parse_special_chars_nbsp() {
    let doc = parse_corpus("special_chars.rtf");
    let text = all_paragraph_text(&doc);
    assert!(
        text.contains('\u{00A0}'),
        "missing non-breaking space in: {text}"
    );
}

#[test]
fn parse_special_chars_dashes() {
    let doc = parse_corpus("special_chars.rtf");
    let text = all_paragraph_text(&doc);
    assert!(text.contains('\u{2014}'), "missing em dash in: {text}");
    assert!(text.contains('\u{2013}'), "missing en dash in: {text}");
}

#[test]
fn parse_special_chars_quotes() {
    let doc = parse_corpus("special_chars.rtf");
    let text = all_paragraph_text(&doc);
    assert!(
        text.contains('\u{2018}'),
        "missing left single quote in: {text}"
    );
    assert!(
        text.contains('\u{2019}'),
        "missing right single quote in: {text}"
    );
    assert!(
        text.contains('\u{201C}'),
        "missing left double quote in: {text}"
    );
    assert!(
        text.contains('\u{201D}'),
        "missing right double quote in: {text}"
    );
}

#[test]
fn parse_special_chars_bullet() {
    let doc = parse_corpus("special_chars.rtf");
    let text = all_paragraph_text(&doc);
    assert!(text.contains('\u{2022}'), "missing bullet in: {text}");
}

#[test]
fn parse_special_chars_tab() {
    let doc = parse_corpus("special_chars.rtf");
    let text = all_paragraph_text(&doc);
    assert!(text.contains('\t'), "missing tab in: {text}");
}

#[test]
fn parse_image_png() {
    let doc = parse_corpus("image_png.rtf");
    assert_eq!(doc.images.len(), 1, "expected 1 image");

    let img = &doc.images[0];
    assert_eq!(img.format, ImageFormat::Png);
    assert!(img.goal_width > 0, "goal_width should be > 0");
    assert!(img.goal_height > 0, "goal_height should be > 0");
    // PNG signature starts with 0x89 0x50 0x4E 0x47
    assert!(
        img.data.len() >= 4,
        "image data too short: {} bytes",
        img.data.len()
    );
    assert_eq!(img.data[0], 0x89, "not a PNG signature");
    assert_eq!(img.data[1], 0x50, "not a PNG signature");
    assert_eq!(img.data[2], 0x4E, "not a PNG signature");
    assert_eq!(img.data[3], 0x47, "not a PNG signature");
}

#[test]
fn parse_minimal_rtf() {
    // Minimal valid RTF.
    let data = b"{\\rtf1 Hello}";
    let doc = Parser::parse(data).expect("parse failed");
    let text = all_paragraph_text(&doc);
    assert_eq!(text, "Hello");
}

#[test]
fn parse_empty_document() {
    let data = b"{\\rtf1}";
    let doc = Parser::parse(data).expect("parse failed");
    assert!(doc.paragraphs.is_empty());
}

#[test]
fn parse_ignorable_destination_skipped() {
    // Unknown \* destinations should be silently skipped.
    let data = b"{\\rtf1 Before{\\*\\fldinst HYPERLINK}After}";
    let doc = Parser::parse(data).expect("parse failed");
    let text = all_paragraph_text(&doc);
    assert_eq!(text, "BeforeAfter");
}

#[test]
fn parse_nested_groups_formatting() {
    // Formatting reverts when group closes.
    let data = b"{\\rtf1 normal{\\b bold}normal}";
    let doc = Parser::parse(data).expect("parse failed");
    assert!(!doc.paragraphs.is_empty());
    let runs = &doc.paragraphs[0].runs;
    assert!(
        runs.len() >= 3,
        "expected at least 3 runs, got {}",
        runs.len()
    );

    // First run: not bold
    assert!(!runs[0].bold, "first run should not be bold");
    assert_eq!(runs[0].text, "normal");

    // Second run: bold
    let bold_run = runs.iter().find(|r| r.bold).expect("no bold run");
    assert!(
        bold_run.text.contains("bold"),
        "bold run text: {}",
        bold_run.text
    );

    // Third run: not bold again
    let last = runs.last().expect("no last run");
    assert!(!last.bold, "last run should not be bold");
}

#[test]
fn parse_unicode_negative_codepoint() {
    // \u-4894 should decode to U+ECDE (65536 - 4894 = 60642 = 0xECE2)
    // Wait: -4894 + 65536 = 60642 = 0xECE2
    let data = b"{\\rtf1\\uc1 \\u-4894?}";
    let doc = Parser::parse(data).expect("parse failed");
    let text = all_paragraph_text(&doc);
    let expected = char::from_u32(60642).expect("invalid codepoint");
    assert!(
        text.contains(expected),
        "expected U+{:04X} in: {text}",
        60642
    );
}

#[test]
fn parse_plain_resets_formatting() {
    let data = b"{\\rtf1\\b\\i Bold italic\\plain  Normal}";
    let doc = Parser::parse(data).expect("parse failed");
    let runs = &doc.paragraphs[0].runs;

    let last = runs.last().expect("no runs");
    assert!(!last.bold, "plain should reset bold");
    assert!(!last.italic, "plain should reset italic");
}

#[test]
fn post_table_text_without_pard() {
    // Text after \row without \pard should still produce a paragraph,
    // not get trapped in table mode.
    let data = b"{\\rtf1\\trowd\\cellx5000 A\\cell\\row After table.\\par More text.}";
    let doc = Parser::parse(data).expect("parse failed");
    let text = all_paragraph_text(&doc);
    assert!(
        text.contains("After table."),
        "missing post-table text in: {text}"
    );
    assert!(
        text.contains("More text."),
        "missing second paragraph in: {text}"
    );
    // Should have 2 paragraphs (After table. and More text.)
    assert!(
        doc.paragraphs.len() >= 2,
        "expected at least 2 paragraphs, got {}",
        doc.paragraphs.len()
    );
}

#[test]
fn warning_for_unknown_star_destination() {
    let data = b"{\\rtf1 Before{\\*\\unknowndest Content}After}";
    let doc = Parser::parse(data).expect("parse failed");
    let has_warning = doc.warnings.iter().any(|w| w.contains("unknowndest"));
    assert!(
        has_warning,
        "expected warning for unknown destination, got: {:?}",
        doc.warnings
    );
}

#[test]
fn line_break_within_paragraph() {
    // \line should produce a line break within the same paragraph,
    // not start a new paragraph like \par does.
    let data = b"{\\rtf1 Line one\\line Line two\\par Next para.}";
    let doc = Parser::parse(data).expect("parse failed");
    assert_eq!(
        doc.paragraphs.len(),
        2,
        "expected 2 paragraphs, got {}",
        doc.paragraphs.len()
    );
    let p0: String = doc.paragraphs[0]
        .runs
        .iter()
        .map(|r| r.text.as_str())
        .collect();
    assert!(
        p0.contains("Line one"),
        "first paragraph should contain 'Line one', got: {p0}"
    );
    assert!(
        p0.contains("Line two"),
        "first paragraph should contain 'Line two', got: {p0}"
    );
    assert!(
        p0.contains('\n'),
        "first paragraph should contain a newline, got: {p0}"
    );
}

#[test]
fn pard_between_table_rows_produces_single_table() {
    // Some RTF producers emit \pard between rows as a formatting reset.
    // This should NOT split the table into multiple ParsedTables.
    let data = b"{\\rtf1\
        \\trowd\\cellx5000 A\\cell\\row\
        \\pard\
        \\trowd\\cellx5000 B\\cell\\row\
        \\pard Text after table.}";
    let doc = Parser::parse(data).expect("parse failed");
    assert_eq!(
        doc.tables.len(),
        1,
        "\\pard between rows should not split the table, got {} tables",
        doc.tables.len()
    );
    assert_eq!(doc.tables[0].rows.len(), 2, "expected 2 rows");
    let text = all_paragraph_text(&doc);
    assert!(
        text.contains("Text after table."),
        "missing post-table text in: {text}"
    );
}

#[test]
fn unicode_skip_counts_control_words() {
    // \uc1 means skip 1 character after \u. When the fallback is a
    // control word (\f0), that control word should be skipped, not
    // the following text.
    let data = b"{\\rtf1\\uc1 \\u8364\\f0 text}";
    let doc = Parser::parse(data).expect("parse failed");
    let text = all_paragraph_text(&doc);
    // Euro sign should appear, and "text" should not be eaten.
    assert!(text.contains('\u{20AC}'), "missing euro in: {text}");
    assert!(text.contains("text"), "text eaten by skip in: {text}");
}

#[test]
fn unicode_skip_counts_control_symbols() {
    // \uc1: the fallback after \u is \~ (control symbol), should be skipped.
    let data = b"{\\rtf1\\uc1 \\u8364\\~after}";
    let doc = Parser::parse(data).expect("parse failed");
    let text = all_paragraph_text(&doc);
    assert!(text.contains('\u{20AC}'), "missing euro in: {text}");
    // \~ should be skipped, not emitted as NBSP
    assert!(
        !text.contains('\u{00A0}'),
        "NBSP should have been skipped in: {text}"
    );
    assert!(text.contains("after"), "text missing in: {text}");
}

#[test]
fn unicode_skip_does_not_leak_across_groups() {
    // \u8364 with \uc1: the skip count should not eat text in the parent
    // group after the child group closes.
    let data = b"{\\rtf1\\uc1{\\u8364?}safe}";
    let doc = Parser::parse(data).expect("parse failed");
    let text = all_paragraph_text(&doc);
    assert!(text.contains('\u{20AC}'), "missing euro in: {text}");
    assert!(text.contains("safe"), "skip leaked across group: {text}");
}

#[test]
fn deep_nesting_does_not_corrupt_state() {
    // Build a deeply nested document and verify the state is correct
    // after closing all groups.
    let mut data = Vec::new();
    data.extend_from_slice(b"{\\rtf1 outer");
    data.extend(std::iter::repeat_n(b'{', 300));
    data.extend_from_slice(b"inner");
    data.extend(std::iter::repeat_n(b'}', 300));
    data.extend_from_slice(b" still outer}");
    let doc = Parser::parse(&data).expect("parse failed");
    let text = all_paragraph_text(&doc);
    assert!(text.contains("outer"), "outer text lost: {text}");
    assert!(
        text.contains("still outer"),
        "text after deep nesting lost: {text}"
    );
}

#[test]
fn negative_font_index_ignored() {
    let data = b"{\\rtf1\\deff-1 Hello}";
    let doc = Parser::parse(data).expect("parse failed");
    let text = all_paragraph_text(&doc);
    assert_eq!(text, "Hello");
}

#[test]
fn image_count_limit() {
    // Build RTF with many pict groups to verify the limit.
    let mut data = Vec::new();
    data.extend_from_slice(b"{\\rtf1");
    for _ in 0..1005 {
        data.extend_from_slice(b"{\\pict\\pngblip 89504e47}");
    }
    data.push(b'}');
    let doc = Parser::parse(&data).expect("parse failed");
    assert!(
        doc.images.len() <= 1000,
        "images should be capped at 1000, got {}",
        doc.images.len()
    );
}

#[test]
fn backslash_newline_is_paragraph_break() {
    // \<CR> and \<LF> are equivalent to \par (RTF spec).
    // Common in macOS TextEdit output.
    let data = b"{\\rtf1 First\\\nSecond\\\nThird}";
    let doc = Parser::parse(data).expect("parse failed");
    assert_eq!(
        doc.paragraphs.len(),
        3,
        "\\<newline> should produce paragraph breaks, got {} paragraphs",
        doc.paragraphs.len()
    );
    let text = all_paragraph_text(&doc);
    assert!(text.contains("First"), "got: {text}");
    assert!(text.contains("Second"), "got: {text}");
    assert!(text.contains("Third"), "got: {text}");
}

#[test]
fn backslash_cr_is_paragraph_break() {
    // \<CR> variant.
    let data = b"{\\rtf1 Line A\\\rLine B}";
    let doc = Parser::parse(data).expect("parse failed");
    assert_eq!(doc.paragraphs.len(), 2);
}

#[test]
fn fldrslt_text_extracted() {
    // \fldrslt is not a \*-prefixed destination, so its text content
    // should flow through to the output. This is how hyperlink display
    // text appears in RTF.
    let data = b"{\\rtf1 Before {\\field{\\*\\fldinst HYPERLINK \"http://example.com\"}{\\fldrslt Click here}}After}";
    let doc = Parser::parse(data).expect("parse failed");
    let text = all_paragraph_text(&doc);
    assert!(text.contains("Before"), "got: {text}");
    assert!(text.contains("Click here"), "got: {text}");
    assert!(text.contains("After"), "got: {text}");
    // \fldinst should be skipped (it's a \* destination).
    assert!(!text.contains("HYPERLINK"), "got: {text}");
}

#[test]
fn vertical_merge_emits_warning() {
    let data = b"{\\rtf1\\trowd\\clvmgf\\cellx5000 A\\cell\\row}";
    let doc = Parser::parse(data).expect("parse failed");
    assert!(
        doc.warnings.iter().any(|w| w.contains("clvmgf")),
        "expected warning about vertical merge, got: {:?}",
        doc.warnings
    );
}

// ---------------------------------------------------------------------------
// \highlight fixed palette tests
// ---------------------------------------------------------------------------

#[test]
fn resolve_highlight_palette() {
    // Index 0 is auto (no highlight).
    assert_eq!(resolve_highlight(0), None);
    // Known palette entries.
    assert_eq!(resolve_highlight(1), Some([0, 0, 0])); // black
    assert_eq!(resolve_highlight(2), Some([0, 0, 255])); // blue
    assert_eq!(resolve_highlight(3), Some([0, 255, 255])); // cyan
    assert_eq!(resolve_highlight(4), Some([0, 255, 0])); // green
    assert_eq!(resolve_highlight(5), Some([255, 0, 255])); // magenta
    assert_eq!(resolve_highlight(6), Some([255, 0, 0])); // red
    assert_eq!(resolve_highlight(7), Some([255, 255, 0])); // yellow
    assert_eq!(resolve_highlight(8), Some([255, 255, 255])); // white
    assert_eq!(resolve_highlight(9), Some([0, 0, 128])); // dark blue
    assert_eq!(resolve_highlight(10), Some([0, 128, 128])); // dark cyan
    assert_eq!(resolve_highlight(11), Some([0, 128, 0])); // dark green
    assert_eq!(resolve_highlight(12), Some([128, 0, 128])); // dark magenta
    assert_eq!(resolve_highlight(13), Some([128, 0, 0])); // dark red
    assert_eq!(resolve_highlight(14), Some([128, 128, 0])); // dark yellow
    assert_eq!(resolve_highlight(15), Some([128, 128, 128])); // dark gray
    assert_eq!(resolve_highlight(16), Some([192, 192, 192])); // light gray

    // Out-of-range returns None.
    assert_eq!(resolve_highlight(17), None);
    assert_eq!(resolve_highlight(255), None);
}

#[test]
fn highlight_uses_fixed_palette_not_color_table() {
    // \highlight7 should produce yellow [255,255,0] regardless of color table contents.
    // The color table here has index 1 = red (255,0,0).
    let data = b"{\\rtf1{\\colortbl;\\red255\\green0\\blue0;}\\highlight7 Yellow text}";
    let doc = Parser::parse(data).expect("parse failed");
    let run = &doc.paragraphs[0].runs[0];
    assert_eq!(
        run.bg_color,
        Some([255, 255, 0]),
        "highlight should use fixed yellow, not color table"
    );
    assert_eq!(run.text, "Yellow text");
}

#[test]
fn highlight_zero_means_no_highlight() {
    let data = b"{\\rtf1\\highlight0 No highlight}";
    let doc = Parser::parse(data).expect("parse failed");
    let run = &doc.paragraphs[0].runs[0];
    assert_eq!(run.bg_color, None, "highlight 0 should be no highlight");
}

#[test]
fn highlight_takes_precedence_over_cb() {
    // When both \cb and \highlight are set, \highlight wins.
    let data = b"{\\rtf1{\\colortbl;\\red0\\green0\\blue255;}\\cb1\\highlight6 Red highlight}";
    let doc = Parser::parse(data).expect("parse failed");
    let run = &doc.paragraphs[0].runs[0];
    assert_eq!(
        run.bg_color,
        Some([255, 0, 0]),
        "highlight should take precedence over cb"
    );
}

#[test]
fn plain_resets_highlight() {
    let data = b"{\\rtf1\\highlight7 Yellow\\plain Normal}";
    let doc = Parser::parse(data).expect("parse failed");
    assert!(
        doc.paragraphs[0].runs.len() >= 2,
        "expected at least 2 runs"
    );
    let yellow_run = &doc.paragraphs[0].runs[0];
    assert_eq!(yellow_run.bg_color, Some([255, 255, 0]));
    // After \plain, highlight should be cleared.
    let normal_run = doc.paragraphs[0].runs.last().expect("no runs");
    assert_eq!(normal_run.bg_color, None, "plain should reset highlight");
}

#[test]
fn color_table_missing_trailing_semicolon() {
    // Malformed: last entry has no trailing ';'. The group close should
    // flush the pending RGB values so the entry is not silently dropped.
    let data = b"{\\rtf1{\\colortbl;\\red0\\green128\\blue255}\\cf1 Blue text}";
    let doc = Parser::parse(data).expect("parse failed");
    let run = &doc.paragraphs[0].runs[0];
    assert_eq!(
        run.color,
        Some([0, 128, 255]),
        "color table entry without trailing semicolon should still be usable"
    );
}

#[test]
fn color_table_empty_pending_no_spurious_entry() {
    // Well-formed color table with trailing semicolon. The group close
    // should NOT add a spurious extra entry since pending is all-None.
    let data = b"{\\rtf1{\\colortbl;\\red255\\green0\\blue0;}\\cf1 Red text}";
    let doc = Parser::parse(data).expect("parse failed");
    let run = &doc.paragraphs[0].runs[0];
    assert_eq!(
        run.color,
        Some([255, 0, 0]),
        "well-formed color table should not gain an extra entry from group close"
    );
}

#[test]
fn striked_is_alias_for_strike() {
    // \striked is the "double strikethrough" control word in some RTF producers,
    // but our parser treats it identically to \strike (sets strikethrough = true).
    let data = b"{\\rtf1\\striked Struck text\\striked0  Normal text}";
    let doc = Parser::parse(data).expect("parse failed");
    let runs = &doc.paragraphs[0].runs;
    assert!(
        runs.len() >= 2,
        "expected at least 2 runs, got {}",
        runs.len()
    );

    // First run: strikethrough on.
    let struck = &runs[0];
    assert!(
        struck.strikethrough,
        "\\striked should enable strikethrough"
    );
    assert_eq!(struck.text, "Struck text");

    // Second run: strikethrough off via \striked0.
    let normal = runs.last().expect("no runs");
    assert!(
        !normal.strikethrough,
        "\\striked0 should disable strikethrough"
    );
}

// -- Hyperlink field extraction -----------------------------------------------

#[test]
fn hyperlink_field_quoted_url() {
    // Standard RTF HYPERLINK field with quoted URL.
    let data = br#"{\rtf1 Before {\field{\*\fldinst HYPERLINK "http://example.com"}{\fldrslt Click here}} After}"#;
    let doc = Parser::parse(data).expect("parse failed");

    // Should have one paragraph with three runs: "Before ", "Click here", " After"
    assert_eq!(doc.paragraphs.len(), 1);
    let runs = &doc.paragraphs[0].runs;
    assert!(
        runs.len() >= 3,
        "expected at least 3 runs, got {}",
        runs.len()
    );

    // "Before " should have no hyperlink.
    assert!(
        runs[0].hyperlink_url.is_none(),
        "non-link run should have no URL"
    );

    // "Click here" should have the hyperlink URL.
    let link_run = &runs[1];
    assert_eq!(link_run.text, "Click here");
    assert_eq!(
        link_run.hyperlink_url.as_deref(),
        Some("http://example.com"),
        "link run should carry the URL"
    );

    // " After" should have no hyperlink.
    let after_run = runs.last().expect("no runs");
    assert!(
        after_run.hyperlink_url.is_none(),
        "post-link run should have no URL"
    );
}

#[test]
fn hyperlink_field_unquoted_url() {
    // Some RTF producers omit quotes around the URL.
    let data = br#"{\rtf1{\field{\*\fldinst HYPERLINK http://example.com}{\fldrslt Link}}}"#;
    let doc = Parser::parse(data).expect("parse failed");

    let runs = &doc.paragraphs[0].runs;
    let link_run = runs.iter().find(|r| r.text == "Link").expect("no Link run");
    assert_eq!(
        link_run.hyperlink_url.as_deref(),
        Some("http://example.com"),
        "unquoted URL should be extracted"
    );
}

#[test]
fn hyperlink_field_with_datafield() {
    // Real-world pattern: \fldinst contains nested {\*\datafield ...} group.
    let data = br#"{\rtf1{\field{\*\fldinst { HYPERLINK "http://example.com" }{{\*\datafield 00d0c9ea79f9}}}{\fldrslt {\ul Link text}}}}"#;
    let doc = Parser::parse(data).expect("parse failed");

    let runs = &doc.paragraphs[0].runs;
    let link_run = runs
        .iter()
        .find(|r| r.text == "Link text")
        .expect("no link run");
    assert_eq!(
        link_run.hyperlink_url.as_deref(),
        Some("http://example.com"),
    );
}

#[test]
fn hyperlink_field_non_hyperlink_ignored() {
    // Non-HYPERLINK field instructions should not produce hyperlink URLs.
    let data = br#"{\rtf1{\field{\*\fldinst PAGE}{\fldrslt 1}}}"#;
    let doc = Parser::parse(data).expect("parse failed");

    for para in &doc.paragraphs {
        for run in &para.runs {
            assert!(
                run.hyperlink_url.is_none(),
                "non-HYPERLINK field should not produce URLs, got {:?} on '{}'",
                run.hyperlink_url,
                run.text
            );
        }
    }
}

#[test]
fn hyperlink_multiple_fields() {
    // Multiple HYPERLINK fields in one document.
    let data = br#"{\rtf1 A {\field{\*\fldinst HYPERLINK "http://one.com"}{\fldrslt One}} B {\field{\*\fldinst HYPERLINK "http://two.com"}{\fldrslt Two}} C}"#;
    let doc = Parser::parse(data).expect("parse failed");

    let all_runs: Vec<&TextRun> = doc.paragraphs.iter().flat_map(|p| &p.runs).collect();
    let urls: Vec<&str> = all_runs
        .iter()
        .filter_map(|r| r.hyperlink_url.as_deref())
        .collect();
    assert_eq!(urls, vec!["http://one.com", "http://two.com"]);
}
