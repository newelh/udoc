//! PPT-to-Document model conversion.
//!
//! Converts PPT slide data into the unified Document model. Uses
//! `TextType` for heading inference (Title/CenterTitle -> H1,
//! Subtitle -> H2). Speaker notes are appended as named sections.
//! This keeps PPT internals inside the PPT crate; the facade calls
//! `ppt_to_document` without reaching into parser types.

use udoc_core::backend::FormatBackend;
use udoc_core::convert::{
    alloc_id, maybe_insert_page_break, push_named_section, set_text_styling, text_paragraph,
};
use udoc_core::diagnostics::DiagnosticsSink;
use udoc_core::document::*;
use udoc_core::error::{Error, Result};

use crate::document::PptDocument;
use crate::slides::TextType;
use crate::styles::CharStyleRun;

/// Populate the presentation overlay with font size for an inline node.
///
/// Font name resolution requires parsing the FontCollection container
/// from the PPT persist stream, which is not yet implemented. Only
/// font_size_pt is forwarded for now.
fn maybe_set_text_styling(doc: &mut Document, inline_id: NodeId, run: &CharStyleRun) {
    set_text_styling(
        doc,
        inline_id,
        ExtendedTextStyle::new().font_size(run.font_size_pt),
    );
}

// ---------------------------------------------------------------------------
// PPT-specific conversion logic
// ---------------------------------------------------------------------------

/// Convert a PPT backend into the unified Document model.
///
/// Each slide maps to a sequence of blocks. `TextType::Title` and
/// `TextType::CenterTitle` become `Block::Heading { level: 1 }`,
/// `TextType::Subtitle` becomes `Block::Heading { level: 2 }`, and
/// all other text types become `Block::Paragraph`. Speaker notes are
/// appended as a Section with role "notes" after each slide's content.
pub fn ppt_to_document(
    ppt: &mut PptDocument,
    diagnostics: &dyn DiagnosticsSink,
    max_pages: usize,
) -> Result<Document> {
    let _ = diagnostics; // reserved for future warning propagation

    let page_count = FormatBackend::page_count(ppt).min(max_pages);
    let mut doc = Document::new();
    doc.metadata = FormatBackend::metadata(ppt);

    for page_idx in 0..page_count {
        maybe_insert_page_break(&mut doc)?;
        convert_slide_content(&mut doc, ppt, page_idx)?;
    }

    Ok(doc)
}

/// Convert a single slide's content to Document model blocks.
fn convert_slide_content(doc: &mut Document, ppt: &mut PptDocument, page_idx: usize) -> Result<()> {
    let slide = ppt
        .slide_content(page_idx)
        .ok_or_else(|| Error::new(format!("slide {page_idx} out of range")))?;

    // Convert text blocks.
    for tb in &slide.text_blocks {
        if tb.text.is_empty() {
            continue;
        }

        let heading_level = heading_level_from_text_type(tb.text_type);
        let block_id = alloc_id(doc)?;

        // Build inline content, using char-level style runs when available.
        let content = build_inlines_from_text_block(doc, tb)?;

        if heading_level > 0 {
            doc.content.push(Block::Heading {
                id: block_id,
                level: heading_level,
                content,
            });
        } else {
            doc.content.push(Block::Paragraph {
                id: block_id,
                content,
            });
        }
    }

    // Append notes as a named Section.
    let notes_texts: Vec<String> = slide
        .notes_text
        .iter()
        .filter(|tb| !tb.text.is_empty())
        .map(|tb| tb.text.clone())
        .collect();

    if !notes_texts.is_empty() {
        let mut children = Vec::new();
        for note_text in notes_texts {
            children.push(text_paragraph(doc, note_text)?);
        }
        push_named_section(doc, "notes", children)?;
    }

    Ok(())
}

/// Build Inline nodes from a TextBlock, splitting by char-level style runs
/// when available. Falls back to TextType-based bold inference otherwise.
fn build_inlines_from_text_block(
    doc: &mut Document,
    tb: &crate::slides::TextBlock,
) -> Result<Vec<Inline>> {
    if let Some(ref style) = tb.styles {
        if style.char_runs.len() > 1 {
            let (chunks, remainder) = crate::styles::split_text_by_runs(&tb.text, &style.char_runs);
            let mut inlines = Vec::new();
            for (text, run) in chunks {
                let inline_id = alloc_id(doc)?;
                let mut s = SpanStyle::default();
                s.bold = run.bold.unwrap_or(false);
                s.italic = run.italic.unwrap_or(false);

                // Forward font size to presentation overlay.
                maybe_set_text_styling(doc, inline_id, run);

                inlines.push(Inline::Text {
                    id: inline_id,
                    text,
                    style: s,
                });
            }
            if !remainder.is_empty() {
                let inline_id = alloc_id(doc)?;
                inlines.push(Inline::Text {
                    id: inline_id,
                    text: remainder,
                    style: SpanStyle::default(),
                });
            }
            if !inlines.is_empty() {
                return Ok(inlines);
            }
        }
    }

    // Single run or no styles: one inline for the whole block.
    let inline_id = alloc_id(doc)?;
    let mut s = SpanStyle::default();

    // Prefer char-level style for single-run blocks.
    if let Some(ref style) = tb.styles {
        if let Some(run) = style.char_runs.first() {
            s.bold = run.bold.unwrap_or(false);
            s.italic = run.italic.unwrap_or(false);

            // Forward font size to presentation overlay.
            maybe_set_text_styling(doc, inline_id, run);

            return Ok(vec![Inline::Text {
                id: inline_id,
                text: tb.text.clone(),
                style: s,
            }]);
        }
    }

    // Fallback: infer from text type.
    s.bold = matches!(
        tb.text_type,
        TextType::Title | TextType::CenterTitle | TextType::Subtitle
    );
    Ok(vec![Inline::Text {
        id: inline_id,
        text: tb.text.clone(),
        style: s,
    }])
}

/// Map PPT TextType to heading level (0 = not a heading).
fn heading_level_from_text_type(text_type: TextType) -> u8 {
    match text_type {
        TextType::Title | TextType::CenterTitle => 1,
        TextType::Subtitle => 2,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::diagnostics::NullDiagnostics;

    use crate::test_util::*;

    #[test]
    fn conversion_title_becomes_heading_1() {
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_header_atom(0)); // Title
        slwt.extend_from_slice(&build_text_chars_atom("My Title"));
        slwt.extend_from_slice(&build_text_header_atom(1)); // Body
        slwt.extend_from_slice(&build_text_chars_atom("Body text"));

        let cfb_data = build_ppt_cfb(&slwt, &[]);
        let mut doc = PptDocument::from_bytes(&cfb_data).expect("from_bytes");
        let diag = NullDiagnostics;
        let result = ppt_to_document(&mut doc, &diag, usize::MAX).expect("conversion");

        assert_eq!(result.content.len(), 2);
        assert!(matches!(
            &result.content[0],
            Block::Heading { level: 1, .. }
        ));
        assert_eq!(result.content[0].text(), "My Title");
        assert!(matches!(&result.content[1], Block::Paragraph { .. }));
        assert_eq!(result.content[1].text(), "Body text");
    }

    #[test]
    fn conversion_center_title_becomes_heading_1() {
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_header_atom(5)); // CenterTitle
        slwt.extend_from_slice(&build_text_chars_atom("Centered"));

        let cfb_data = build_ppt_cfb(&slwt, &[]);
        let mut doc = PptDocument::from_bytes(&cfb_data).expect("from_bytes");
        let diag = NullDiagnostics;
        let result = ppt_to_document(&mut doc, &diag, usize::MAX).expect("conversion");

        assert_eq!(result.content.len(), 1);
        assert!(matches!(
            &result.content[0],
            Block::Heading { level: 1, .. }
        ));
    }

    #[test]
    fn conversion_subtitle_becomes_heading_2() {
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_header_atom(6)); // Subtitle
        slwt.extend_from_slice(&build_text_chars_atom("Sub"));

        let cfb_data = build_ppt_cfb(&slwt, &[]);
        let mut doc = PptDocument::from_bytes(&cfb_data).expect("from_bytes");
        let diag = NullDiagnostics;
        let result = ppt_to_document(&mut doc, &diag, usize::MAX).expect("conversion");

        assert_eq!(result.content.len(), 1);
        assert!(matches!(
            &result.content[0],
            Block::Heading { level: 2, .. }
        ));
    }

    #[test]
    fn conversion_notes_become_section() {
        let mut slide_slwt = Vec::new();
        slide_slwt.extend_from_slice(&build_slide_persist_atom(1));
        slide_slwt.extend_from_slice(&build_text_header_atom(0));
        slide_slwt.extend_from_slice(&build_text_chars_atom("Title"));

        let mut notes_slwt = Vec::new();
        notes_slwt.extend_from_slice(&build_slide_persist_atom(100));
        notes_slwt.extend_from_slice(&build_text_header_atom(2));
        notes_slwt.extend_from_slice(&build_text_chars_atom("Speaker notes here"));

        let cfb_data = build_ppt_cfb(&slide_slwt, &notes_slwt);
        let mut doc = PptDocument::from_bytes(&cfb_data).expect("from_bytes");
        let diag = NullDiagnostics;
        let result = ppt_to_document(&mut doc, &diag, usize::MAX).expect("conversion");

        // Title heading + notes section.
        assert_eq!(result.content.len(), 2);
        assert!(matches!(
            &result.content[0],
            Block::Heading { level: 1, .. }
        ));
        match &result.content[1] {
            Block::Section { role, children, .. } => {
                assert_eq!(role.as_ref(), Some(&SectionRole::Notes));
                assert_eq!(children.len(), 1);
                assert_eq!(children[0].text(), "Speaker notes here");
            }
            other => panic!("expected Section, got {:?}", other),
        }
    }

    #[test]
    fn conversion_two_slides_with_page_break() {
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom("Slide 1"));
        slwt.extend_from_slice(&build_slide_persist_atom(2));
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom("Slide 2"));

        let cfb_data = build_ppt_cfb(&slwt, &[]);
        let mut doc = PptDocument::from_bytes(&cfb_data).expect("from_bytes");
        let diag = NullDiagnostics;
        let result = ppt_to_document(&mut doc, &diag, usize::MAX).expect("conversion");

        // Slide 1 heading + page break + slide 2 heading.
        assert_eq!(result.content.len(), 3);
        assert!(matches!(
            &result.content[0],
            Block::Heading { level: 1, .. }
        ));
        assert!(matches!(&result.content[1], Block::PageBreak { .. }));
        assert!(matches!(
            &result.content[2],
            Block::Heading { level: 1, .. }
        ));
    }

    #[test]
    fn conversion_empty_presentation() {
        // Empty SLWT = 0 slides.
        let cfb_data = build_ppt_cfb(&[], &[]);
        let mut doc = PptDocument::from_bytes(&cfb_data).expect("from_bytes");
        let diag = NullDiagnostics;
        let result = ppt_to_document(&mut doc, &diag, usize::MAX).expect("conversion");

        assert!(result.content.is_empty());
        assert_eq!(result.metadata.page_count, 0);
    }

    #[test]
    fn conversion_metadata_preserved() {
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom("My Presentation"));

        let cfb_data = build_ppt_cfb(&slwt, &[]);
        let mut doc = PptDocument::from_bytes(&cfb_data).expect("from_bytes");
        let diag = NullDiagnostics;
        let result = ppt_to_document(&mut doc, &diag, usize::MAX).expect("conversion");

        assert_eq!(result.metadata.page_count, 1);
        assert_eq!(result.metadata.title.as_deref(), Some("My Presentation"));
    }

    #[test]
    fn test_ppt_font_size_forwarding_single_run() {
        use crate::slides::TextBlock;
        use crate::styles::{CharStyleRun, TextStyle};

        let tb = TextBlock {
            text: "Hello".to_string(),
            text_type: TextType::Body,
            styles: Some(TextStyle {
                char_runs: vec![CharStyleRun {
                    char_count: 6, // 5 chars + implicit CR
                    bold: Some(false),
                    italic: Some(false),
                    font_size_pt: Some(24.0),
                    font_index: None,
                }],
            }),
        };

        let mut doc = Document::new();
        let inlines = build_inlines_from_text_block(&mut doc, &tb).expect("build_inlines");

        assert_eq!(inlines.len(), 1);
        let inline_id = inlines[0].id();

        let pres = doc
            .presentation
            .as_ref()
            .expect("presentation should be set");
        let ext = pres
            .text_styling
            .get(inline_id)
            .expect("text_styling should be set");
        assert_eq!(ext.font_size, Some(24.0));
        // Font name not available (requires FontCollection parsing).
        assert!(ext.font_name.is_none());
    }

    #[test]
    fn test_ppt_font_size_forwarding_multiple_runs() {
        use crate::slides::TextBlock;
        use crate::styles::{CharStyleRun, TextStyle};

        let tb = TextBlock {
            text: "BigSmall".to_string(),
            text_type: TextType::Body,
            styles: Some(TextStyle {
                char_runs: vec![
                    CharStyleRun {
                        char_count: 3,
                        bold: Some(true),
                        italic: Some(false),
                        font_size_pt: Some(36.0),
                        font_index: None,
                    },
                    CharStyleRun {
                        char_count: 6, // "Small" + CR
                        bold: Some(false),
                        italic: Some(false),
                        font_size_pt: Some(12.0),
                        font_index: None,
                    },
                ],
            }),
        };

        let mut doc = Document::new();
        let inlines = build_inlines_from_text_block(&mut doc, &tb).expect("build_inlines");

        assert_eq!(inlines.len(), 2);
        assert_eq!(inlines[0].text(), "Big");
        assert_eq!(inlines[1].text(), "Small");

        let pres = doc
            .presentation
            .as_ref()
            .expect("presentation should be set");
        let ext0 = pres
            .text_styling
            .get(inlines[0].id())
            .expect("run 0 styling");
        let ext1 = pres
            .text_styling
            .get(inlines[1].id())
            .expect("run 1 styling");
        assert_eq!(ext0.font_size, Some(36.0));
        assert_eq!(ext1.font_size, Some(12.0));
    }

    #[test]
    fn test_ppt_no_font_styling_when_absent() {
        use crate::slides::TextBlock;
        use crate::styles::{CharStyleRun, TextStyle};

        let tb = TextBlock {
            text: "Hello".to_string(),
            text_type: TextType::Body,
            styles: Some(TextStyle {
                char_runs: vec![CharStyleRun {
                    char_count: 6,
                    bold: Some(true),
                    italic: Some(false),
                    font_size_pt: None,
                    font_index: None,
                }],
            }),
        };

        let mut doc = Document::new();
        let _inlines = build_inlines_from_text_block(&mut doc, &tb).expect("build_inlines");

        // No font size: presentation overlay should not be set.
        assert!(doc.presentation.is_none());
    }
}
