//! ODF-to-Document model conversion.
//!
//! Converts the parsed ODF AST (ODT/ODS/ODP body) into the unified Document
//! model. This keeps ODF internals inside the ODF crate; the facade calls
//! `odf_to_document` without reaching into parser types.

use std::collections::HashSet;

use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_core::convert::{
    alloc_id, maybe_insert_page_break, push_named_section, push_tables, register_hyperlink,
    set_block_layout, set_text_styling, text_inline, text_paragraph,
};
use udoc_core::diagnostics::DiagnosticsSink;
use udoc_core::document::*;
use udoc_core::error::{Error, Result, ResultExt};

use crate::document::OdfDocument;
use crate::odt::{OdtElement, OdtParagraph, OdtRun};
use crate::styles::ResolvedTextProps;

// ---------------------------------------------------------------------------
// ODF-specific conversion logic
// ---------------------------------------------------------------------------

/// Convert an ODF backend into the unified Document model.
///
/// For ODT, iterates body elements directly to preserve heading detection.
/// For ODS/ODP, delegates to PageExtractor for each sheet/slide.
pub fn odf_to_document(
    odf: &mut OdfDocument,
    diagnostics: &dyn DiagnosticsSink,
    max_pages: usize,
) -> Result<Document> {
    let mut doc = Document::new();
    doc.metadata = FormatBackend::metadata(odf);

    udoc_core::convert::propagate_warnings(odf.warnings(), diagnostics, "OdfParse");

    // Dedup set for hyperlink URLs collected during conversion (#142).
    let mut hyperlink_seen: HashSet<String> = HashSet::new();

    if let Some(odt_body) = odf.odt_body() {
        convert_odt_body(&mut doc, odt_body, odf.styles(), &mut hyperlink_seen)?;
    } else if odf.ods_body().is_some() {
        convert_via_page_extractor(&mut doc, odf, max_pages)?;
    } else if odf.odp_body().is_some() {
        convert_odp_body(&mut doc, odf, max_pages)?;
    }

    Ok(doc)
}

/// Convert ODT body elements to Document model blocks.
fn convert_odt_body(
    doc: &mut Document,
    body: &crate::odt::OdtBody,
    styles: &crate::styles::OdfStyleMap,
    seen_urls: &mut HashSet<String>,
) -> Result<()> {
    for elem in &body.elements {
        match elem {
            OdtElement::Paragraph(para) => {
                let inlines = paragraph_to_inlines(doc, para, styles, seen_urls)?;
                if inlines.is_empty() {
                    continue;
                }
                let block_id = alloc_id(doc).context("allocating ODF node")?;

                // Heading detection: check direct outline-level first,
                // then resolve from style.
                let heading_level = if para.heading_level > 0 {
                    para.heading_level
                } else if let Some(ref style_name) = para.style_name {
                    styles.resolve_heading_level(style_name)
                } else {
                    0
                };

                // Apply paragraph-level block layout from style.
                apply_block_layout(doc, block_id, para.style_name.as_deref(), styles);

                if heading_level > 0 {
                    doc.content.push(Block::Heading {
                        id: block_id,
                        level: heading_level,
                        content: inlines,
                    });
                } else {
                    doc.content.push(Block::Paragraph {
                        id: block_id,
                        content: inlines,
                    });
                }
            }
            OdtElement::Table(tbl) => {
                let core_table = crate::document::convert_odt_table_for_convert(tbl);
                push_tables(doc, &[core_table])?;
            }
            OdtElement::List(list) => {
                let list_id = alloc_id(doc).context("allocating ODF node")?;
                let mut items = Vec::new();

                for item in &list.items {
                    let item_id = alloc_id(doc).context("allocating ODF node")?;
                    let mut item_blocks = Vec::new();

                    for para in &item.paragraphs {
                        let inlines = paragraph_to_inlines(doc, para, styles, seen_urls)?;
                        if !inlines.is_empty() {
                            let para_id = alloc_id(doc).context("allocating ODF node")?;
                            item_blocks.push(Block::Paragraph {
                                id: para_id,
                                content: inlines,
                            });
                        }
                    }

                    if !item_blocks.is_empty() {
                        items.push(ListItem::new(item_id, item_blocks));
                    }
                }

                if !items.is_empty() {
                    doc.content.push(Block::List {
                        id: list_id,
                        items,
                        kind: ListKind::Unordered,
                        start: 1,
                    });
                }
            }
        }
    }

    // Footnotes and endnotes as named sections.
    let footnote_blocks = notes_to_blocks(doc, &body.footnotes, styles, seen_urls)?;
    push_named_section(doc, "footnotes", footnote_blocks)?;

    let endnote_blocks = notes_to_blocks(doc, &body.endnotes, styles, seen_urls)?;
    push_named_section(doc, "endnotes", endnote_blocks)?;

    Ok(())
}

/// Convert note paragraphs to Block elements for the Document model.
fn notes_to_blocks(
    doc: &mut Document,
    paragraphs: &[OdtParagraph],
    styles: &crate::styles::OdfStyleMap,
    seen_urls: &mut HashSet<String>,
) -> Result<Vec<Block>> {
    let mut blocks = Vec::new();
    for para in paragraphs {
        let inlines = paragraph_to_inlines(doc, para, styles, seen_urls)?;
        if !inlines.is_empty() {
            let block_id = alloc_id(doc).context("allocating ODF node")?;
            blocks.push(Block::Paragraph {
                id: block_id,
                content: inlines,
            });
        }
    }
    Ok(blocks)
}

/// Convert a paragraph's runs into Inline elements with style inheritance.
///
/// Hyperlink URLs are registered with `Relationships` via `seen_urls` to avoid
/// a post-hoc tree walk (#142).
fn paragraph_to_inlines(
    doc: &mut Document,
    para: &OdtParagraph,
    styles: &crate::styles::OdfStyleMap,
    seen_urls: &mut HashSet<String>,
) -> Result<Vec<Inline>> {
    // Resolve style-level formatting once for the paragraph via a single walk.
    let para_style = para.style_name.as_deref();
    let para_flags = para_style
        .map(|s| styles.resolve_span_flags(s))
        .unwrap_or_default();
    let style_bold = para_flags.bold.unwrap_or(false);
    let style_italic = para_flags.italic.unwrap_or(false);
    let style_underline = para_flags.underline.unwrap_or(false);
    let style_strikethrough = para_flags.strikethrough.unwrap_or(false);

    // Resolve paragraph-level text props once; runs without their own style
    // reuse these instead of re-walking the inheritance chain.
    let para_text_props = para_style.map(|s| styles.resolve_text_props(s));

    let mut result = Vec::new();

    // Group consecutive runs with the same link_url into a single Inline::Link.
    let mut i = 0;
    let runs: Vec<&OdtRun> = para.runs.iter().filter(|r| !r.text.is_empty()).collect();

    while i < runs.len() {
        let run = runs[i];
        if let Some(ref url) = run.link_url {
            // Collect all consecutive runs with the same URL.
            let mut link_inlines = Vec::new();
            let mut j = i;
            while j < runs.len() && runs[j].link_url.as_deref() == Some(url.as_str()) {
                let r = runs[j];
                let inline_id = alloc_id(doc).context("allocating ODF node")?;
                let style = build_span_style(
                    r,
                    style_bold,
                    style_italic,
                    style_underline,
                    style_strikethrough,
                );
                apply_text_styling(doc, inline_id, r, para_text_props.as_ref(), styles);
                link_inlines.push(Inline::Text {
                    id: inline_id,
                    text: r.text.clone(),
                    style,
                });
                j += 1;
            }
            let link_id = alloc_id(doc).context("allocating ODF node")?;
            register_hyperlink(doc, seen_urls, url);
            result.push(Inline::Link {
                id: link_id,
                url: url.clone(),
                content: link_inlines,
            });
            i = j;
        } else {
            let inline_id = alloc_id(doc).context("allocating ODF node")?;
            let style = build_span_style(
                run,
                style_bold,
                style_italic,
                style_underline,
                style_strikethrough,
            );
            apply_text_styling(doc, inline_id, run, para_text_props.as_ref(), styles);
            result.push(Inline::Text {
                id: inline_id,
                text: run.text.clone(),
                style,
            });
            i += 1;
        }
    }

    Ok(result)
}

/// Build a SpanStyle from a run's formatting, falling back to paragraph defaults.
fn build_span_style(
    run: &OdtRun,
    style_bold: bool,
    style_italic: bool,
    style_underline: bool,
    style_strikethrough: bool,
) -> SpanStyle {
    let mut style = SpanStyle::default();
    style.bold = run.bold.unwrap_or(style_bold);
    style.italic = run.italic.unwrap_or(style_italic);
    style.underline = run.underline.unwrap_or(style_underline);
    style.strikethrough = run.strikethrough.unwrap_or(style_strikethrough);
    style
}

/// Apply extended text styling (color, font, size) to the presentation overlay.
///
/// When the run has its own style, resolves that. Otherwise reuses the
/// pre-resolved paragraph props to avoid a redundant inheritance walk.
fn apply_text_styling(
    doc: &mut Document,
    inline_id: NodeId,
    run: &OdtRun,
    para_props: Option<&ResolvedTextProps>,
    styles: &crate::styles::OdfStyleMap,
) {
    let props_owned;
    let props = if let Some(effective) = run.style_name.as_deref() {
        // Run has its own style override; resolve its full chain.
        props_owned = styles.resolve_text_props(effective);
        &props_owned
    } else {
        match para_props {
            Some(p) => p,
            None => return,
        }
    };

    set_text_styling(
        doc,
        inline_id,
        ExtendedTextStyle::new()
            .font_name(props.font_name.clone())
            .font_size(props.font_size)
            .color(props.color.map(Color::from))
            .background_color(props.background_color.map(Color::from)),
    );
}

/// Apply block-level layout (alignment, spacing, indentation) to the presentation overlay.
/// Uses resolve_block_props for a single inheritance walk instead of 5 separate walks.
fn apply_block_layout(
    doc: &mut Document,
    block_id: NodeId,
    style_name: Option<&str>,
    styles: &crate::styles::OdfStyleMap,
) {
    let style_name = match style_name {
        Some(s) => s,
        None => return,
    };

    let props = styles.resolve_block_props(style_name);
    set_block_layout(
        doc,
        block_id,
        BlockLayout::new()
            .alignment(props.alignment.and_then(|s| Alignment::from_format_str(&s)))
            .indent_left(props.indent_left)
            .indent_right(props.indent_right)
            .space_before(props.space_before)
            .space_after(props.space_after),
    );
}

/// Convert ODS/ODT via PageExtractor (for spreadsheets).
fn convert_via_page_extractor(
    doc: &mut Document,
    odf: &mut OdfDocument,
    max_pages: usize,
) -> Result<()> {
    let page_count = FormatBackend::page_count(odf).min(max_pages);

    for page_idx in 0..page_count {
        maybe_insert_page_break(doc)?;

        let mut page = FormatBackend::page(odf, page_idx)
            .map_err(|e| Error::with_source(format!("opening sheet {page_idx}"), e))?;

        let tables = page.tables().map_err(|e| {
            Error::with_source(format!("extracting tables from sheet {page_idx}"), e)
        })?;

        push_tables(doc, &tables)?;
    }

    Ok(())
}

/// Convert ODP via slide_shapes-like approach.
fn convert_odp_body(doc: &mut Document, odf: &mut OdfDocument, max_pages: usize) -> Result<()> {
    let page_count = FormatBackend::page_count(odf).min(max_pages);

    let odp_body = match odf.odp_body() {
        Some(body) => body,
        None => return Ok(()),
    };

    for page_idx in 0..page_count {
        let slide = match odp_body.slides.get(page_idx) {
            Some(s) => s,
            None => break,
        };

        maybe_insert_page_break(doc)?;

        for para in &slide.paragraphs {
            if para.text.is_empty() {
                continue;
            }
            let block_id = alloc_id(doc).context("allocating ODF node")?;
            // ODP slide paragraphs currently produce plain Inline::Text
            // without per-run formatting (bold/italic/color). This is a
            // known gap; ODP style resolution would require propagating
            // style refs from the slide master through shape text runs.
            let content = vec![text_inline(doc, para.text.clone())?];

            if para.heading_level > 0 {
                doc.content.push(Block::Heading {
                    id: block_id,
                    level: para.heading_level,
                    content,
                });
            } else {
                doc.content.push(Block::Paragraph {
                    id: block_id,
                    content,
                });
            }
        }

        // Add speaker notes as a named Section.
        if let Some(ref notes_text) = slide.notes {
            if !notes_text.is_empty() {
                let section_id = alloc_id(doc).context("allocating ODF node")?;
                doc.content.push(Block::Section {
                    id: section_id,
                    role: Some(SectionRole::Notes),
                    children: vec![text_paragraph(doc, notes_text.clone())?],
                });
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_containers::test_util::build_stored_zip;
    use udoc_core::diagnostics::NullDiagnostics;

    fn make_odt_bytes(content_xml: &[u8]) -> Vec<u8> {
        build_stored_zip(&[
            (
                "mimetype",
                b"application/vnd.oasis.opendocument.text" as &[u8],
            ),
            ("content.xml", content_xml),
        ])
    }

    #[test]
    fn basic_conversion() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:p>Hello World</text:p>
      <text:h text:outline-level="1">Title</text:h>
    </office:text>
  </office:body>
</office:document-content>"#;

        let data = make_odt_bytes(content);
        let mut odf = OdfDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = odf_to_document(&mut odf, &diag, usize::MAX).unwrap();

        assert_eq!(result.content.len(), 2);
        assert!(matches!(&result.content[0], Block::Paragraph { .. }));
        assert!(matches!(
            &result.content[1],
            Block::Heading { level: 1, .. }
        ));
    }

    #[test]
    fn conversion_with_table() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
    xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0">
  <office:body>
    <office:text>
      <text:p>Before table</text:p>
      <table:table>
        <table:table-row>
          <table:table-cell><text:p>A1</text:p></table:table-cell>
          <table:table-cell><text:p>B1</text:p></table:table-cell>
        </table:table-row>
      </table:table>
      <text:p>After table</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let data = make_odt_bytes(content);
        let mut odf = OdfDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = odf_to_document(&mut odf, &diag, usize::MAX).unwrap();

        assert_eq!(result.content.len(), 3);
        assert!(matches!(&result.content[0], Block::Paragraph { .. }));
        assert!(matches!(&result.content[1], Block::Table { .. }));
        assert!(matches!(&result.content[2], Block::Paragraph { .. }));
    }

    #[test]
    fn conversion_with_list() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:list>
        <text:list-item><text:p>Item 1</text:p></text:list-item>
        <text:list-item><text:p>Item 2</text:p></text:list-item>
      </text:list>
    </office:text>
  </office:body>
</office:document-content>"#;

        let data = make_odt_bytes(content);
        let mut odf = OdfDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = odf_to_document(&mut odf, &diag, usize::MAX).unwrap();

        assert_eq!(result.content.len(), 1);
        match &result.content[0] {
            Block::List { items, kind, .. } => {
                assert_eq!(*kind, ListKind::Unordered);
                assert_eq!(items.len(), 2);
            }
            other => panic!("expected List, got {:?}", other),
        }
    }

    #[test]
    fn conversion_with_footnotes() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:p>Body text<text:note text:note-class="footnote">
        <text:note-citation>1</text:note-citation>
        <text:note-body><text:p>FnText</text:p></text:note-body>
      </text:note></text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let data = make_odt_bytes(content);
        let mut odf = OdfDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = odf_to_document(&mut odf, &diag, usize::MAX).unwrap();

        assert!(
            result.content.len() >= 2,
            "expected at least 2 blocks, got {}",
            result.content.len()
        );
        assert_eq!(result.content[0].text(), "Body text");

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
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:p>Body<text:note text:note-class="endnote">
        <text:note-citation>i</text:note-citation>
        <text:note-body><text:p>EnText</text:p></text:note-body>
      </text:note></text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let data = make_odt_bytes(content);
        let mut odf = OdfDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = odf_to_document(&mut odf, &diag, usize::MAX).unwrap();

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
    fn conversion_no_notes_no_sections() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:p>Just text</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let data = make_odt_bytes(content);
        let mut odf = OdfDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = odf_to_document(&mut odf, &diag, usize::MAX).unwrap();

        for block in &result.content {
            assert!(
                !matches!(block, Block::Section { .. }),
                "unexpected Section block: {block:?}"
            );
        }
    }

    #[test]
    fn conversion_with_both_footnotes_and_endnotes() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:p>Body<text:note text:note-class="footnote">
        <text:note-citation>1</text:note-citation>
        <text:note-body><text:p>Fn1</text:p></text:note-body>
      </text:note><text:note text:note-class="endnote">
        <text:note-citation>i</text:note-citation>
        <text:note-body><text:p>En1</text:p></text:note-body>
      </text:note></text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let data = make_odt_bytes(content);
        let mut odf = OdfDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = odf_to_document(&mut odf, &diag, usize::MAX).unwrap();

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
    }

    #[test]
    fn test_odt_text_color() {
        let content = br##"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:automatic-styles>
    <style:style style:name="RedText" style:family="text">
      <style:text-properties fo:color="#FF0000"/>
    </style:style>
  </office:automatic-styles>
  <office:body>
    <office:text>
      <text:p><text:span text:style-name="RedText">red text</text:span></text:p>
    </office:text>
  </office:body>
</office:document-content>"##;

        let data = make_odt_bytes(content);
        let mut odf = OdfDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = odf_to_document(&mut odf, &diag, usize::MAX).unwrap();

        let pres = result
            .presentation
            .as_ref()
            .expect("should have presentation");
        if let Block::Paragraph { content, .. } = &result.content[0] {
            let inline_id = content[0].id();
            let ext = pres
                .text_styling
                .get(inline_id)
                .expect("should have text_styling");
            assert_eq!(ext.color, Some(Color::rgb(255, 0, 0)));
        } else {
            panic!("expected paragraph");
        }
    }

    #[test]
    fn test_odt_background_color() {
        let content = br##"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:automatic-styles>
    <style:style style:name="Highlight" style:family="text">
      <style:text-properties fo:background-color="#FFFF00"/>
    </style:style>
  </office:automatic-styles>
  <office:body>
    <office:text>
      <text:p><text:span text:style-name="Highlight">highlighted</text:span></text:p>
    </office:text>
  </office:body>
</office:document-content>"##;

        let data = make_odt_bytes(content);
        let mut odf = OdfDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = odf_to_document(&mut odf, &diag, usize::MAX).unwrap();

        let pres = result
            .presentation
            .as_ref()
            .expect("should have presentation");
        if let Block::Paragraph { content, .. } = &result.content[0] {
            let ext = pres.text_styling.get(content[0].id()).expect("styling");
            assert_eq!(ext.background_color, Some(Color::rgb(255, 255, 0)));
        } else {
            panic!("expected paragraph");
        }
    }

    #[test]
    fn test_odt_font_name_size() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:automatic-styles>
    <style:style style:name="FontStyle" style:family="text">
      <style:text-properties style:font-name="Courier" fo:font-size="16pt"/>
    </style:style>
  </office:automatic-styles>
  <office:body>
    <office:text>
      <text:p><text:span text:style-name="FontStyle">monospaced</text:span></text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let data = make_odt_bytes(content);
        let mut odf = OdfDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = odf_to_document(&mut odf, &diag, usize::MAX).unwrap();

        let pres = result
            .presentation
            .as_ref()
            .expect("should have presentation");
        if let Block::Paragraph { content, .. } = &result.content[0] {
            let ext = pres.text_styling.get(content[0].id()).expect("styling");
            assert_eq!(ext.font_name.as_deref(), Some("Courier"));
            assert_eq!(ext.font_size, Some(16.0));
        } else {
            panic!("expected paragraph");
        }
    }

    #[test]
    fn test_odt_underline() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0">
  <office:automatic-styles>
    <style:style style:name="Underlined" style:family="text">
      <style:text-properties style:text-underline-style="solid"/>
    </style:style>
  </office:automatic-styles>
  <office:body>
    <office:text>
      <text:p><text:span text:style-name="Underlined">underlined text</text:span></text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let data = make_odt_bytes(content);
        let mut odf = OdfDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = odf_to_document(&mut odf, &diag, usize::MAX).unwrap();

        if let Block::Paragraph { content, .. } = &result.content[0] {
            if let Inline::Text { style, .. } = &content[0] {
                assert!(style.underline, "expected underline=true");
            } else {
                panic!("expected Text inline");
            }
        } else {
            panic!("expected paragraph");
        }
    }

    #[test]
    fn test_odt_strikethrough() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0">
  <office:automatic-styles>
    <style:style style:name="Struck" style:family="text">
      <style:text-properties style:text-line-through-style="solid"/>
    </style:style>
  </office:automatic-styles>
  <office:body>
    <office:text>
      <text:p><text:span text:style-name="Struck">struck text</text:span></text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let data = make_odt_bytes(content);
        let mut odf = OdfDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = odf_to_document(&mut odf, &diag, usize::MAX).unwrap();

        if let Block::Paragraph { content, .. } = &result.content[0] {
            if let Inline::Text { style, .. } = &content[0] {
                assert!(style.strikethrough, "expected strikethrough=true");
            } else {
                panic!("expected Text inline");
            }
        } else {
            panic!("expected paragraph");
        }
    }

    #[test]
    fn test_odt_hyperlink() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
    xmlns:xlink="http://www.w3.org/1999/xlink">
  <office:body>
    <office:text>
      <text:p>Click <text:a xlink:href="https://example.com">here</text:a> now</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let data = make_odt_bytes(content);
        let mut odf = OdfDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = odf_to_document(&mut odf, &diag, usize::MAX).unwrap();

        assert_eq!(result.content.len(), 1);
        if let Block::Paragraph { content, .. } = &result.content[0] {
            // Should be: Text("Click "), Link("here"), Text(" now")
            assert_eq!(content.len(), 3, "expected 3 inlines, got {:?}", content);
            assert!(matches!(&content[0], Inline::Text { text, .. } if text == "Click "));
            if let Inline::Link {
                url,
                content: link_content,
                ..
            } = &content[1]
            {
                assert_eq!(url, "https://example.com");
                assert_eq!(link_content.len(), 1);
                assert_eq!(link_content[0].text(), "here");
            } else {
                panic!("expected Link inline, got: {:?}", content[1]);
            }
            assert!(matches!(&content[2], Inline::Text { text, .. } if text == " now"));
        } else {
            panic!("expected paragraph");
        }
    }

    #[test]
    fn test_odt_paragraph_alignment() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:automatic-styles>
    <style:style style:name="Centered" style:family="paragraph">
      <style:paragraph-properties fo:text-align="center"/>
    </style:style>
  </office:automatic-styles>
  <office:body>
    <office:text>
      <text:p text:style-name="Centered">centered text</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let data = make_odt_bytes(content);
        let mut odf = OdfDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = odf_to_document(&mut odf, &diag, usize::MAX).unwrap();

        let pres = result
            .presentation
            .as_ref()
            .expect("should have presentation");
        let block_id = result.content[0].id();
        let layout = pres
            .block_layout
            .get(block_id)
            .expect("should have block_layout");
        assert_eq!(layout.alignment, Some(Alignment::Center));
    }

    #[test]
    fn test_odt_paragraph_spacing() {
        let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
    xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
    xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
  <office:automatic-styles>
    <style:style style:name="Spaced" style:family="paragraph">
      <style:paragraph-properties fo:margin-top="12pt" fo:margin-bottom="6pt"/>
    </style:style>
  </office:automatic-styles>
  <office:body>
    <office:text>
      <text:p text:style-name="Spaced">spaced text</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let data = make_odt_bytes(content);
        let mut odf = OdfDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = odf_to_document(&mut odf, &diag, usize::MAX).unwrap();

        let pres = result
            .presentation
            .as_ref()
            .expect("should have presentation");
        let block_id = result.content[0].id();
        let layout = pres
            .block_layout
            .get(block_id)
            .expect("should have block_layout");
        assert!((layout.space_before.unwrap() - 12.0).abs() < 0.01);
        assert!((layout.space_after.unwrap() - 6.0).abs() < 0.01);
    }
}
