//! RTF-to-Document model conversion.
//!
//! Converts parsed RTF paragraphs, tables, and images into the unified
//! Document model with full formatting (colors, strikethrough, superscript,
//! subscript, paragraph alignment, spacing, indentation).

use std::collections::HashSet;

use udoc_core::backend::FormatBackend;
use udoc_core::convert::{
    alloc_id, propagate_warnings, push_run_inline, push_tables, set_block_layout, RunData,
};
use udoc_core::diagnostics::DiagnosticsSink;
use udoc_core::document::presentation::Alignment as DocAlignment;
use udoc_core::document::*;
use udoc_core::error::{Result, ResultExt};

use crate::document::RtfDocument;
use crate::parser::TextRun;
use crate::state::Alignment;

/// Convert an RTF backend into the unified Document model.
///
/// Iterates parsed paragraphs directly (not through PageExtractor) to
/// preserve full formatting data (colors, strikethrough, alignment, etc.)
/// that the PageExtractor text_lines() path cannot carry.
///
/// The `diagnostics` parameter receives parse warnings. RTF is a single-page
/// format, so `max_pages` is ignored.
pub fn rtf_to_document(
    rtf: &mut RtfDocument,
    diagnostics: &dyn DiagnosticsSink,
    _max_pages: usize,
) -> Result<Document> {
    let mut doc = Document::new();
    doc.metadata = FormatBackend::metadata(rtf);

    propagate_warnings(rtf.warnings(), diagnostics, "RtfParse");

    let parsed = rtf.parsed();

    // Dedup set for hyperlink URLs collected during conversion (#142).
    let mut hyperlink_seen: HashSet<String> = HashSet::new();

    // Convert paragraphs with full formatting.
    for para in &parsed.paragraphs {
        let visible_runs: Vec<&TextRun> = para.runs.iter().filter(|r| !r.invisible).collect();
        if visible_runs.is_empty() {
            continue;
        }

        let mut inlines = Vec::with_capacity(visible_runs.len());
        for run in &visible_runs {
            let mut style = SpanStyle::default();
            style.bold = run.bold;
            style.italic = run.italic;
            style.underline = run.underline;
            style.strikethrough = run.strikethrough;
            style.superscript = run.superscript;
            style.subscript = run.subscript;

            // Forward font size unconditionally. Consumers can decide what's
            // "default" for their use case.
            let extended = ExtendedTextStyle::new()
                .font_name(run.font_name.as_ref().map(|n| n.to_string()))
                .font_size(Some(run.font_size_pts))
                .color(run.color.map(Color::from))
                .background_color(run.bg_color.map(Color::from));

            push_run_inline(
                &mut doc,
                &mut hyperlink_seen,
                &mut inlines,
                RunData {
                    text: &run.text,
                    style,
                    extended,
                    hyperlink_url: run.hyperlink_url.as_deref(),
                },
            )
            .context("emitting RTF run")?;
        }

        let block_id = alloc_id(&doc).context("allocating RTF block node")?;
        doc.content.push(Block::Paragraph {
            id: block_id,
            content: inlines,
        });

        set_block_layout(
            &mut doc,
            block_id,
            BlockLayout::new()
                .alignment(para.alignment.map(convert_alignment))
                .indent_left(para.indent_left)
                .indent_right(para.indent_right)
                .space_before(para.space_before)
                .space_after(para.space_after),
        );
    }

    // Convert tables.
    let tables = rtf.core_tables();
    push_tables(&mut doc, &tables)?;

    // Convert images.
    push_core_images(&mut doc, rtf)?;

    Ok(doc)
}

/// Convert RTF alignment to Document model alignment.
fn convert_alignment(align: Alignment) -> DocAlignment {
    match align {
        Alignment::Left => DocAlignment::Left,
        Alignment::Center => DocAlignment::Center,
        Alignment::Right => DocAlignment::Right,
        Alignment::Justify => DocAlignment::Justify,
    }
}

/// Convert RTF images into Block::Image and push onto doc.content.
fn push_core_images(doc: &mut Document, rtf: &mut RtfDocument) -> Result<()> {
    use udoc_core::backend::PageExtractor;

    let mut page = FormatBackend::page(rtf, 0).context("opening RTF page for images")?;
    let images = page.images().context("extracting RTF images")?;

    for img in &images {
        let asset_ref = doc.assets.add_image(ImageData::from(img));

        let block_id = alloc_id(doc)?;
        doc.content.push(Block::Image {
            id: block_id,
            image_ref: asset_ref,
            alt_text: None,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::diagnostics::NullDiagnostics;

    fn parse_rtf(data: &[u8]) -> RtfDocument {
        RtfDocument::from_bytes(data).expect("from_bytes should succeed")
    }

    #[test]
    fn test_rtf_color_table_parsing() {
        let data = b"{\\rtf1{\\colortbl;\\red255\\green0\\blue0;\\red0\\green255\\blue0;\\red0\\green0\\blue255;} text}";
        let doc = parse_rtf(data);
        let parsed = doc.parsed();
        assert_eq!(parsed.color_table.len(), 4);
        // Index 0: auto (None)
        assert_eq!(parsed.color_table[0], None);
        // Index 1: red
        assert_eq!(parsed.color_table[1], Some([255, 0, 0]));
        // Index 2: green
        assert_eq!(parsed.color_table[2], Some([0, 255, 0]));
        // Index 3: blue
        assert_eq!(parsed.color_table[3], Some([0, 0, 255]));
    }

    #[test]
    fn test_rtf_text_color() {
        let data = b"{\\rtf1{\\colortbl;\\red255\\green0\\blue0;}\\cf1 Red text}";
        let mut doc = parse_rtf(data);
        let diag = NullDiagnostics;
        let result = rtf_to_document(&mut doc, &diag, usize::MAX).unwrap();

        // Find the text node and check its styling.
        let para = match &result.content[0] {
            Block::Paragraph { content, .. } => content,
            _ => panic!("expected paragraph"),
        };
        assert_eq!(para.len(), 1);
        let inline_id = para[0].id();
        let pres = result.presentation.as_ref().expect("expected presentation");
        let styling = pres.text_styling.get(inline_id);
        assert!(styling.is_some(), "expected text styling for red text");
        let style = styling.unwrap();
        assert_eq!(style.color, Some(Color::rgb(255, 0, 0)));
    }

    #[test]
    fn test_rtf_auto_color() {
        // \cf0 means auto color (index 0 is always auto).
        let data = b"{\\rtf1{\\colortbl;\\red255\\green0\\blue0;}\\cf0 Auto text}";
        let mut doc = parse_rtf(data);
        let diag = NullDiagnostics;
        let result = rtf_to_document(&mut doc, &diag, usize::MAX).unwrap();

        let para = match &result.content[0] {
            Block::Paragraph { content, .. } => content,
            _ => panic!("expected paragraph"),
        };
        let inline_id = para[0].id();
        // Auto color means no color in styling. The styling may or may not exist
        // depending on whether other styling data (font, size) is present.
        if let Some(pres) = result.presentation.as_ref() {
            if let Some(style) = pres.text_styling.get(inline_id) {
                assert_eq!(style.color, None, "cf0 should produce no color");
            }
        }
    }

    #[test]
    fn test_rtf_strikethrough() {
        let data = b"{\\rtf1\\strike Struck text}";
        let mut doc = parse_rtf(data);
        let diag = NullDiagnostics;
        let result = rtf_to_document(&mut doc, &diag, usize::MAX).unwrap();

        let para = match &result.content[0] {
            Block::Paragraph { content, .. } => content,
            _ => panic!("expected paragraph"),
        };
        match &para[0] {
            Inline::Text { style, .. } => {
                assert!(style.strikethrough, "expected strikethrough");
            }
            _ => panic!("expected text inline"),
        }
    }

    #[test]
    fn test_rtf_superscript() {
        let data = b"{\\rtf1 normal{\\super sup}normal}";
        let mut doc = parse_rtf(data);
        let diag = NullDiagnostics;
        let result = rtf_to_document(&mut doc, &diag, usize::MAX).unwrap();

        let para = match &result.content[0] {
            Block::Paragraph { content, .. } => content,
            _ => panic!("expected paragraph"),
        };
        // Should have 3 inlines: normal, sup, normal
        assert!(
            para.len() >= 3,
            "expected at least 3 inlines, got {}",
            para.len()
        );
        match &para[1] {
            Inline::Text { style, text, .. } => {
                assert!(style.superscript, "expected superscript on '{text}'");
                assert!(!style.subscript, "should not be subscript");
            }
            _ => panic!("expected text inline"),
        }
    }

    #[test]
    fn test_rtf_subscript() {
        let data = b"{\\rtf1 normal{\\sub sub}normal}";
        let mut doc = parse_rtf(data);
        let diag = NullDiagnostics;
        let result = rtf_to_document(&mut doc, &diag, usize::MAX).unwrap();

        let para = match &result.content[0] {
            Block::Paragraph { content, .. } => content,
            _ => panic!("expected paragraph"),
        };
        assert!(
            para.len() >= 3,
            "expected at least 3 inlines, got {}",
            para.len()
        );
        match &para[1] {
            Inline::Text { style, text, .. } => {
                assert!(style.subscript, "expected subscript on '{text}'");
                assert!(!style.superscript, "should not be superscript");
            }
            _ => panic!("expected text inline"),
        }
    }

    #[test]
    fn test_rtf_nosupersub() {
        let data = b"{\\rtf1{\\super sup}{\\nosupersub normal}{\\sub sub}{\\nosupersub normal}}";
        let mut doc = parse_rtf(data);
        let diag = NullDiagnostics;
        let result = rtf_to_document(&mut doc, &diag, usize::MAX).unwrap();

        let para = match &result.content[0] {
            Block::Paragraph { content, .. } => content,
            _ => panic!("expected paragraph"),
        };
        assert!(
            para.len() >= 4,
            "expected at least 4 inlines, got {}",
            para.len()
        );
        // After \nosupersub, both super and subscript should be false.
        match &para[1] {
            Inline::Text { style, text, .. } => {
                assert!(
                    !style.superscript,
                    "should not be superscript after \\nosupersub on '{text}'"
                );
                assert!(
                    !style.subscript,
                    "should not be subscript after \\nosupersub on '{text}'"
                );
            }
            _ => panic!("expected text inline"),
        }
        match &para[3] {
            Inline::Text { style, text, .. } => {
                assert!(
                    !style.superscript,
                    "should not be superscript after \\nosupersub on '{text}'"
                );
                assert!(
                    !style.subscript,
                    "should not be subscript after \\nosupersub on '{text}'"
                );
            }
            _ => panic!("expected text inline"),
        }
    }

    #[test]
    fn test_rtf_underline_forwarding() {
        let data = b"{\\rtf1\\ul Underlined text}";
        let mut doc = parse_rtf(data);
        let diag = NullDiagnostics;
        let result = rtf_to_document(&mut doc, &diag, usize::MAX).unwrap();

        let para = match &result.content[0] {
            Block::Paragraph { content, .. } => content,
            _ => panic!("expected paragraph"),
        };
        match &para[0] {
            Inline::Text { style, .. } => {
                assert!(style.underline, "expected underline");
            }
            _ => panic!("expected text inline"),
        }
    }

    #[test]
    fn test_rtf_paragraph_alignment() {
        let data = b"{\\rtf1\\qc Centered text}";
        let mut doc = parse_rtf(data);
        let diag = NullDiagnostics;
        let result = rtf_to_document(&mut doc, &diag, usize::MAX).unwrap();

        let block_id = result.content[0].id();
        let pres = result.presentation.as_ref().expect("expected presentation");
        let layout = pres.block_layout.get(block_id);
        assert!(
            layout.is_some(),
            "expected block layout for centered paragraph"
        );
        assert_eq!(
            layout.unwrap().alignment,
            Some(DocAlignment::Center),
            "expected Center alignment"
        );
    }

    #[test]
    fn test_rtf_paragraph_spacing() {
        // \sb240 = 240 twips = 12pt before, \sa120 = 120 twips = 6pt after
        let data = b"{\\rtf1\\sb240\\sa120 Spaced text}";
        let mut doc = parse_rtf(data);
        let diag = NullDiagnostics;
        let result = rtf_to_document(&mut doc, &diag, usize::MAX).unwrap();

        let block_id = result.content[0].id();
        let pres = result.presentation.as_ref().expect("expected presentation");
        let layout = pres.block_layout.get(block_id);
        assert!(
            layout.is_some(),
            "expected block layout for spaced paragraph"
        );
        let layout = layout.unwrap();
        assert!(
            (layout.space_before.unwrap() - 12.0).abs() < f64::EPSILON,
            "expected 12pt space before, got {:?}",
            layout.space_before
        );
        assert!(
            (layout.space_after.unwrap() - 6.0).abs() < f64::EPSILON,
            "expected 6pt space after, got {:?}",
            layout.space_after
        );
    }

    #[test]
    fn test_rtf_paragraph_indent() {
        // \li720 = 720 twips = 36pt left indent
        let data = b"{\\rtf1\\li720 Indented text}";
        let mut doc = parse_rtf(data);
        let diag = NullDiagnostics;
        let result = rtf_to_document(&mut doc, &diag, usize::MAX).unwrap();

        let block_id = result.content[0].id();
        let pres = result.presentation.as_ref().expect("expected presentation");
        let layout = pres.block_layout.get(block_id);
        assert!(
            layout.is_some(),
            "expected block layout for indented paragraph"
        );
        let layout = layout.unwrap();
        assert!(
            (layout.indent_left.unwrap() - 36.0).abs() < f64::EPSILON,
            "expected 36pt left indent, got {:?}",
            layout.indent_left
        );
    }

    #[test]
    fn test_rtf_hyperlink_produces_inline_link() {
        let data = br#"{\rtf1 Before {\field{\*\fldinst HYPERLINK "http://example.com"}{\fldrslt Click here}} After}"#;
        let mut doc = parse_rtf(data);
        let diag = NullDiagnostics;
        let result = rtf_to_document(&mut doc, &diag, usize::MAX).unwrap();

        assert_eq!(result.content.len(), 1);
        let para = match &result.content[0] {
            Block::Paragraph { content, .. } => content,
            _ => panic!("expected paragraph"),
        };

        // Should have: Text("Before "), Link("Click here"), Text(" After")
        assert!(
            para.len() >= 3,
            "expected at least 3 inlines, got {}",
            para.len()
        );

        // Find the Link inline.
        let link = para.iter().find(|i| matches!(i, Inline::Link { .. }));
        assert!(link.is_some(), "expected Inline::Link, got {:?}", para);

        match link.unwrap() {
            Inline::Link { url, content, .. } => {
                assert_eq!(url, "http://example.com");
                assert_eq!(content.len(), 1);
                match &content[0] {
                    Inline::Text { text, .. } => {
                        assert_eq!(text, "Click here");
                    }
                    other => panic!("expected Text inside Link, got {:?}", other),
                }
            }
            other => panic!("expected Link, got {:?}", other),
        }
    }

    #[test]
    fn test_rtf_non_hyperlink_field_no_link() {
        let data = br#"{\rtf1{\field{\*\fldinst PAGE}{\fldrslt 1}}}"#;
        let mut doc = parse_rtf(data);
        let diag = NullDiagnostics;
        let result = rtf_to_document(&mut doc, &diag, usize::MAX).unwrap();

        // All inlines should be plain Text, no Link.
        for block in &result.content {
            if let Block::Paragraph { content, .. } = block {
                for inline in content {
                    assert!(
                        !matches!(inline, Inline::Link { .. }),
                        "non-HYPERLINK field should not produce Inline::Link"
                    );
                }
            }
        }
    }
}
