//! DOC-to-Document model conversion.
//!
//! Converts DOC paragraph data into the unified Document model. Uses
//! paragraph properties (istd) for heading inference: istd 1-9 map to
//! headings, everything else becomes a paragraph. Character properties
//! drive bold/italic on inline spans. Tables are converted via the
//! shared `push_tables` helper.

use udoc_core::backend::FormatBackend;
use udoc_core::convert::{alloc_id, push_named_section, push_tables, set_text_styling};
use udoc_core::diagnostics::DiagnosticsSink;
use udoc_core::document::*;
use udoc_core::error::Result;

use crate::document::DocDocument;
use crate::properties::{CharacterProperties, ParagraphProperties};
use crate::text::DocParagraph;

/// Populate the presentation overlay with font name and size for an inline node.
///
/// Font size is converted from half-points to points (divide by 2).
/// Font name is resolved from font_index via the font table when available.
fn maybe_set_text_styling(
    model: &mut Document,
    inline_id: NodeId,
    cp: &CharacterProperties,
    font_names: &[String],
) {
    let font_size = cp.font_size_half_pts.map(|hp| hp as f64 / 2.0);
    let font_name = cp
        .font_index
        .and_then(|idx| font_names.get(idx as usize))
        .cloned();
    set_text_styling(
        model,
        inline_id,
        ExtendedTextStyle::new()
            .font_name(font_name)
            .font_size(font_size),
    );
}

/// Convert a DOC backend into the unified Document model.
///
/// Each paragraph maps to a block. Paragraphs with istd 1-9 in their
/// ParagraphProperties become Block::Heading (level = istd, clamped to 1-6).
/// All others become Block::Paragraph. Character properties drive
/// bold/italic on inline spans. Detected tables are appended via
/// `push_tables`.
pub fn doc_to_document(
    doc: &mut DocDocument,
    diagnostics: &dyn DiagnosticsSink,
    _max_pages: usize, // DOC is always 1 logical page; kept for macro signature uniformity
) -> Result<Document> {
    let _ = diagnostics; // reserved for future warning propagation

    let mut model = Document::new();
    model.metadata = FormatBackend::metadata(doc);

    // Convert paragraphs to blocks.
    let paragraphs = doc.paragraphs();
    let para_props = doc.para_props();
    let char_props = doc.char_props();
    let font_names = doc.font_names();

    for para in paragraphs {
        if para.text.is_empty() {
            continue;
        }

        let props = find_props_for_paragraph(para, para_props);
        let heading_level = heading_level_from_istd(props);

        let block_id = alloc_id(&model)?;
        let content = build_inlines(&mut model, para, char_props, font_names)?;

        if heading_level > 0 {
            model.content.push(Block::Heading {
                id: block_id,
                level: heading_level,
                content,
            });
        } else {
            model.content.push(Block::Paragraph {
                id: block_id,
                content,
            });
        }
    }

    // Convert tables.
    push_tables(&mut model, doc.tables_ref())?;

    // Footnotes, endnotes, headers/footers as named sections.
    let footnote_blocks =
        paragraphs_to_blocks(&mut model, doc.footnotes(), char_props, font_names)?;
    push_named_section(&mut model, "footnotes", footnote_blocks)?;

    let endnote_blocks = paragraphs_to_blocks(&mut model, doc.endnotes(), char_props, font_names)?;
    push_named_section(&mut model, "endnotes", endnote_blocks)?;

    let hf_blocks =
        paragraphs_to_blocks(&mut model, doc.headers_footers(), char_props, font_names)?;
    push_named_section(&mut model, "headers-footers", hf_blocks)?;

    Ok(model)
}

/// Convert a list of DOC paragraphs to Block::Paragraph elements.
fn paragraphs_to_blocks(
    doc: &mut Document,
    paragraphs: &[DocParagraph],
    char_props: &[CharacterProperties],
    font_names: &[String],
) -> Result<Vec<Block>> {
    let mut blocks = Vec::new();
    for para in paragraphs {
        if para.text.is_empty() {
            continue;
        }
        let block_id = alloc_id(doc)?;
        let content = build_inlines(doc, para, char_props, font_names)?;
        blocks.push(Block::Paragraph {
            id: block_id,
            content,
        });
    }
    Ok(blocks)
}

/// Build Inline nodes from a paragraph, splitting by character property runs.
/// When font_size or font_name are available in character properties, they
/// are written to the presentation overlay as ExtendedTextStyle.
fn build_inlines(
    doc: &mut Document,
    para: &DocParagraph,
    char_props: &[CharacterProperties],
    font_names: &[String],
) -> Result<Vec<Inline>> {
    // Find overlapping character property runs.
    let overlapping: Vec<&CharacterProperties> = char_props
        .iter()
        .filter(|cp| cp.cp_start < para.cp_end && cp.cp_end > para.cp_start)
        .collect();

    if overlapping.is_empty() {
        // No character properties: single plain inline
        let id = alloc_id(doc)?;
        return Ok(vec![Inline::Text {
            id,
            text: para.text.clone(),
            style: SpanStyle::default(),
        }]);
    }

    let para_chars: Vec<char> = para.text.chars().collect();
    let para_len = para_chars.len() as u32;
    let mut inlines = Vec::new();

    for cp in &overlapping {
        let run_cp_start = cp.cp_start.max(para.cp_start);
        let run_cp_end = cp.cp_end.min(para.cp_end);
        if run_cp_start >= run_cp_end {
            continue;
        }

        let char_start = (run_cp_start - para.cp_start) as usize;
        let char_end = ((run_cp_end - para.cp_start) as usize).min(para_len as usize);
        if char_start >= char_end || char_start >= para_chars.len() {
            continue;
        }

        let text: String = para_chars[char_start..char_end].iter().collect();
        if text.is_empty() {
            continue;
        }

        let id = alloc_id(doc)?;
        let mut style = SpanStyle::default();
        style.bold = cp.bold.unwrap_or(false);
        style.italic = cp.italic.unwrap_or(false);

        // Forward font size and name to presentation overlay.
        maybe_set_text_styling(doc, id, cp, font_names);

        inlines.push(Inline::Text { id, text, style });
    }

    if inlines.is_empty() {
        let id = alloc_id(doc)?;
        return Ok(vec![Inline::Text {
            id,
            text: para.text.clone(),
            style: SpanStyle::default(),
        }]);
    }

    Ok(inlines)
}

/// Find the ParagraphProperties for a paragraph by CP range overlap.
fn find_props_for_paragraph<'a>(
    para: &DocParagraph,
    props: &'a [ParagraphProperties],
) -> Option<&'a ParagraphProperties> {
    props
        .iter()
        .find(|p| para.cp_start < p.cp_end && para.cp_end > p.cp_start)
}

/// Map DOC istd (style index) to heading level.
///
/// In DOC binary format, built-in heading styles use istd 1-9 for
/// Heading 1-9. We clamp to 1-6 since the Document model supports
/// levels 1-6 (matching HTML h1-h6).
fn heading_level_from_istd(props: Option<&ParagraphProperties>) -> u8 {
    match props {
        Some(p) if (1..=9).contains(&p.istd) => (p.istd as u8).min(6),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::build_minimal_doc;
    use udoc_core::diagnostics::NullDiagnostics;

    #[test]
    fn conversion_plain_text_becomes_paragraph() {
        let doc_bytes = build_minimal_doc("Hello World");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let diag = NullDiagnostics;
        let result = doc_to_document(&mut doc, &diag, usize::MAX).expect("conversion");

        assert_eq!(result.content.len(), 1);
        assert!(matches!(&result.content[0], Block::Paragraph { .. }));
        assert_eq!(result.content[0].text(), "Hello World");
    }

    #[test]
    fn conversion_multiple_paragraphs() {
        let doc_bytes = build_minimal_doc("First\rSecond\rThird");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let diag = NullDiagnostics;
        let result = doc_to_document(&mut doc, &diag, usize::MAX).expect("conversion");

        assert_eq!(result.content.len(), 3);
        assert_eq!(result.content[0].text(), "First");
        assert_eq!(result.content[1].text(), "Second");
        assert_eq!(result.content[2].text(), "Third");
    }

    #[test]
    fn conversion_empty_document() {
        let doc_bytes = build_minimal_doc("");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let diag = NullDiagnostics;
        let result = doc_to_document(&mut doc, &diag, usize::MAX).expect("conversion");

        assert!(result.content.is_empty());
        assert_eq!(result.metadata.page_count, 1);
    }

    #[test]
    fn conversion_metadata_preserved() {
        let doc_bytes = build_minimal_doc("test");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let diag = NullDiagnostics;
        let result = doc_to_document(&mut doc, &diag, usize::MAX).expect("conversion");

        assert_eq!(result.metadata.page_count, 1);
    }

    #[test]
    fn conversion_with_footnotes() {
        use crate::test_util::build_minimal_doc_with_notes;

        let doc_bytes = build_minimal_doc_with_notes("Body\r", "FnText\r", "");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let diag = NullDiagnostics;
        let result = doc_to_document(&mut doc, &diag, usize::MAX).expect("conversion");

        // Body paragraph + footnotes section
        assert!(result.content.len() >= 2);
        assert_eq!(result.content[0].text(), "Body");

        // Last block should be a section with role Footnotes.
        let last = result.content.last().unwrap();
        if let Block::Section { role, children, .. } = last {
            assert_eq!(role.as_ref().unwrap(), &SectionRole::Footnotes);
            assert_eq!(children.len(), 1);
            assert_eq!(children[0].text(), "FnText");
        } else {
            panic!("expected Section block, got: {last:?}");
        }
    }

    #[test]
    fn conversion_with_endnotes() {
        use crate::test_util::build_minimal_doc_with_notes;

        let doc_bytes = build_minimal_doc_with_notes("Body\r", "", "EnText\r");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let diag = NullDiagnostics;
        let result = doc_to_document(&mut doc, &diag, usize::MAX).expect("conversion");

        let last = result.content.last().unwrap();
        if let Block::Section { role, children, .. } = last {
            assert_eq!(role.as_ref().unwrap(), &SectionRole::Endnotes);
            assert_eq!(children.len(), 1);
            assert_eq!(children[0].text(), "EnText");
        } else {
            panic!("expected Section block, got: {last:?}");
        }
    }

    #[test]
    fn conversion_no_notes_no_section() {
        let doc_bytes = build_minimal_doc("Body text");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let diag = NullDiagnostics;
        let result = doc_to_document(&mut doc, &diag, usize::MAX).expect("conversion");

        // No Section blocks should exist for empty footnotes/endnotes
        for block in &result.content {
            assert!(
                !matches!(block, Block::Section { .. }),
                "unexpected Section block: {block:?}"
            );
        }
    }

    #[test]
    fn conversion_with_headers_footers() {
        use crate::test_util::build_minimal_doc_with_all_stories;

        let doc_bytes = build_minimal_doc_with_all_stories("Body\r", "", "HdrText\r", "");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let diag = NullDiagnostics;
        let result = doc_to_document(&mut doc, &diag, usize::MAX).expect("conversion");

        // Find the headers-footers section
        let hf_section = result
            .content
            .iter()
            .find(|b| {
                matches!(
                    b,
                    Block::Section {
                        role: Some(SectionRole::HeadersFooters),
                        ..
                    }
                )
            })
            .expect("headers-footers section should exist");

        if let Block::Section { children, .. } = hf_section {
            assert_eq!(children.len(), 1);
            assert_eq!(children[0].text(), "HdrText");
        } else {
            panic!("expected Section block");
        }
    }

    #[test]
    fn conversion_no_headers_footers_no_section() {
        let doc_bytes = build_minimal_doc("Body text");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let diag = NullDiagnostics;
        let result = doc_to_document(&mut doc, &diag, usize::MAX).expect("conversion");

        // No headers-footers section should exist
        let hf_section = result.content.iter().find(|b| {
            matches!(
                b,
                Block::Section {
                    role: Some(SectionRole::HeadersFooters),
                    ..
                }
            )
        });
        assert!(hf_section.is_none(), "unexpected headers-footers section");
    }

    #[test]
    fn conversion_all_stories() {
        use crate::test_util::build_minimal_doc_with_all_stories;

        let doc_bytes =
            build_minimal_doc_with_all_stories("Body\r", "FnText\r", "HdrText\r", "EnText\r");
        let mut doc = DocDocument::from_bytes(&doc_bytes).expect("from_bytes");
        let diag = NullDiagnostics;
        let result = doc_to_document(&mut doc, &diag, usize::MAX).expect("conversion");

        // Should have body paragraph + 3 sections (footnotes, endnotes, headers-footers)
        let section_roles: Vec<SectionRole> = result
            .content
            .iter()
            .filter_map(|b| {
                if let Block::Section {
                    role: Some(role), ..
                } = b
                {
                    Some(role.clone())
                } else {
                    None
                }
            })
            .collect();

        assert!(
            section_roles.contains(&SectionRole::Footnotes),
            "missing footnotes section: {section_roles:?}"
        );
        assert!(
            section_roles.contains(&SectionRole::Endnotes),
            "missing endnotes section: {section_roles:?}"
        );
        assert!(
            section_roles.contains(&SectionRole::HeadersFooters),
            "missing headers-footers section: {section_roles:?}"
        );
    }

    #[test]
    fn heading_level_from_istd_mapping() {
        // istd 1-6 map directly
        for istd in 1..=6u16 {
            let p = ParagraphProperties {
                cp_start: 0,
                cp_end: 10,
                istd,
                in_table: false,
                table_row_end: false,
            };
            assert_eq!(heading_level_from_istd(Some(&p)), istd as u8);
        }
        // istd 7-9 clamp to 6
        for istd in 7..=9u16 {
            let p = ParagraphProperties {
                cp_start: 0,
                cp_end: 10,
                istd,
                in_table: false,
                table_row_end: false,
            };
            assert_eq!(heading_level_from_istd(Some(&p)), 6);
        }
        // istd 0 (Normal) is not a heading
        let p = ParagraphProperties {
            cp_start: 0,
            cp_end: 10,
            istd: 0,
            in_table: false,
            table_row_end: false,
        };
        assert_eq!(heading_level_from_istd(Some(&p)), 0);
        // No props = not a heading
        assert_eq!(heading_level_from_istd(None), 0);
    }

    #[test]
    fn test_doc_font_size_forwarding() {
        let para = DocParagraph {
            text: "Hello".to_string(),
            cp_start: 0,
            cp_end: 5,
        };
        let char_props = vec![CharacterProperties {
            cp_start: 0,
            cp_end: 5,
            bold: Some(false),
            italic: Some(false),
            font_size_half_pts: Some(24), // 24 half-points = 12pt
            font_index: None,
        }];
        let font_names: Vec<String> = Vec::new();

        let mut model = Document::new();
        let inlines =
            build_inlines(&mut model, &para, &char_props, &font_names).expect("build_inlines");

        assert_eq!(inlines.len(), 1);
        let inline_id = inlines[0].id();

        let pres = model
            .presentation
            .as_ref()
            .expect("presentation should be set");
        let ext = pres
            .text_styling
            .get(inline_id)
            .expect("text_styling should be set for this node");
        assert_eq!(ext.font_size, Some(12.0));
        assert!(ext.font_name.is_none());
    }

    #[test]
    fn test_doc_font_name_forwarding() {
        let para = DocParagraph {
            text: "Hello".to_string(),
            cp_start: 0,
            cp_end: 5,
        };
        let char_props = vec![CharacterProperties {
            cp_start: 0,
            cp_end: 5,
            bold: Some(false),
            italic: Some(false),
            font_size_half_pts: Some(28), // 14pt
            font_index: Some(1),
        }];
        let font_names = vec!["Arial".to_string(), "Times New Roman".to_string()];

        let mut model = Document::new();
        let inlines =
            build_inlines(&mut model, &para, &char_props, &font_names).expect("build_inlines");

        assert_eq!(inlines.len(), 1);
        let inline_id = inlines[0].id();

        let pres = model
            .presentation
            .as_ref()
            .expect("presentation should be set");
        let ext = pres
            .text_styling
            .get(inline_id)
            .expect("text_styling should be set for this node");
        assert_eq!(ext.font_size, Some(14.0));
        assert_eq!(ext.font_name.as_deref(), Some("Times New Roman"));
    }

    #[test]
    fn test_doc_no_font_styling_when_absent() {
        let para = DocParagraph {
            text: "Hello".to_string(),
            cp_start: 0,
            cp_end: 5,
        };
        let char_props = vec![CharacterProperties {
            cp_start: 0,
            cp_end: 5,
            bold: Some(true),
            italic: Some(false),
            font_size_half_pts: None,
            font_index: None,
        }];
        let font_names: Vec<String> = Vec::new();

        let mut model = Document::new();
        let inlines =
            build_inlines(&mut model, &para, &char_props, &font_names).expect("build_inlines");

        assert_eq!(inlines.len(), 1);
        assert!(model.presentation.is_none());
    }

    #[test]
    fn test_doc_font_size_multiple_runs() {
        let para = DocParagraph {
            text: "BigSmall".to_string(),
            cp_start: 0,
            cp_end: 8,
        };
        let char_props = vec![
            CharacterProperties {
                cp_start: 0,
                cp_end: 3,
                bold: Some(true),
                italic: Some(false),
                font_size_half_pts: Some(48), // 24pt
                font_index: None,
            },
            CharacterProperties {
                cp_start: 3,
                cp_end: 8,
                bold: Some(false),
                italic: Some(false),
                font_size_half_pts: Some(20), // 10pt
                font_index: None,
            },
        ];
        let font_names: Vec<String> = Vec::new();

        let mut model = Document::new();
        let inlines =
            build_inlines(&mut model, &para, &char_props, &font_names).expect("build_inlines");

        assert_eq!(inlines.len(), 2);
        assert_eq!(inlines[0].text(), "Big");
        assert_eq!(inlines[1].text(), "Small");

        let pres = model
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
        assert_eq!(ext0.font_size, Some(24.0));
        assert_eq!(ext1.font_size, Some(10.0));
    }
}
