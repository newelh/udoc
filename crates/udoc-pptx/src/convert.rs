//! PPTX-to-Document model conversion.
//!
//! Converts PPTX slide data into the unified Document model. Uses
//! `raw_slide_shapes()` for per-run formatting (bold, italic, underline,
//! strikethrough, color, font, hyperlinks) and paragraph alignment.
//! Placeholder-based heading inference maps title -> H1,
//! subTitle -> H2. Tables are extracted via `PageExtractor::tables()`.
//! Speaker notes are appended as named sections.

use std::collections::HashSet;

use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_core::convert::{
    alloc_id, maybe_insert_page_break, push_named_section, push_run_inline, push_tables,
    set_block_layout, text_paragraph, RunData,
};
use udoc_core::diagnostics::DiagnosticsSink;
use udoc_core::document::*;
use udoc_core::error::{Error, Result, ResultExt};

use crate::document::{heading_level_from_placeholder, PptxDocument};
use crate::shapes::ShapeContent;
use crate::text::{BulletType, DrawingParagraph};

// ---------------------------------------------------------------------------
// PPTX-specific conversion logic
// ---------------------------------------------------------------------------

/// Convert a PPTX backend into the unified Document model.
///
/// Uses `PptxDocument::raw_slide_shapes()` to access per-run formatting and
/// placeholder types for heading inference (: title -> H1, subTitle ->
/// H2). Tables are extracted via `PageExtractor::tables()`. Speaker notes are
/// appended as a Section with role "notes" after each slide's content.
///
/// The `diagnostics` parameter receives parse warnings. This function does
/// not handle page range filtering; the caller is responsible for skipping
/// slides that are out of range.
pub fn pptx_to_document(
    pptx: &mut PptxDocument,
    diagnostics: &dyn DiagnosticsSink,
    max_pages: usize,
) -> Result<Document> {
    // PptxDocument emits warnings through the DiagnosticsSink passed at
    // construction time, not via a stored collection. No propagation needed
    // here (unlike DocxDocument which buffers warnings for later propagation).
    let _ = diagnostics;

    let page_count = FormatBackend::page_count(pptx).min(max_pages);
    let mut doc = Document::new();
    doc.metadata = FormatBackend::metadata(pptx);

    // Dedup set for hyperlink URLs collected during conversion (#142).
    let mut hyperlink_seen: HashSet<String> = HashSet::new();

    for page_idx in 0..page_count {
        maybe_insert_page_break(&mut doc)?;

        // Use raw_slide_shapes() for rich per-run formatting.
        let shapes = pptx.raw_slide_shapes(page_idx);
        for shape in shapes {
            match &shape.content {
                ShapeContent::Text(paras) => {
                    let heading_level =
                        heading_level_from_placeholder(shape.placeholder_type.as_deref());
                    let blocks =
                        paragraphs_to_blocks(&mut doc, paras, heading_level, &mut hyperlink_seen)?;
                    doc.content.extend(blocks);
                }
                ShapeContent::Table(_) => {
                    // Handled below via PageExtractor.
                }
                _ => {}
            }
        }

        // Extract tables via PageExtractor (preserves cell structure).
        let mut page = FormatBackend::page(pptx, page_idx)
            .map_err(|e| Error::with_source(format!("opening slide {page_idx}"), e))?;
        let tables = page.tables().map_err(|e| {
            Error::with_source(format!("extracting tables from slide {page_idx}"), e)
        })?;
        push_tables(&mut doc, &tables).context("building PPTX table blocks")?;

        // Add speaker notes as a named Section.
        if let Some(notes_text) = pptx.notes(page_idx) {
            if !notes_text.is_empty() {
                let blocks = vec![text_paragraph(&doc, notes_text)?];
                push_named_section(&mut doc, "notes", blocks)?;
            }
        }
    }

    Ok(doc)
}

/// Convert a DrawingParagraph's runs into document model Inline elements.
///
/// Wires SpanStyle (bold, italic, underline, strikethrough), extended text
/// styling (font, size, color) to the presentation overlay, and hyperlinks
/// (Inline::Link) when present. Hyperlink URLs are registered with
/// `Relationships` via `seen_urls` to avoid a post-hoc tree walk (#142).
fn paragraph_to_inlines(
    doc: &mut Document,
    para: &DrawingParagraph,
    seen_urls: &mut HashSet<String>,
) -> Result<Vec<Inline>> {
    let mut inlines = Vec::new();

    for run in para.runs.iter().filter(|r| !r.text.is_empty()) {
        let mut style = SpanStyle::default();
        style.bold = run.bold;
        style.italic = run.italic;
        style.underline = run.underline;
        style.strikethrough = run.strikethrough;

        let extended = ExtendedTextStyle::new()
            .font_name(run.font_name.clone())
            .font_size(run.font_size_pt)
            .color(run.color.map(Color::from));

        push_run_inline(
            doc,
            seen_urls,
            &mut inlines,
            RunData {
                text: &run.text,
                style,
                extended,
                hyperlink_url: run.hyperlink_url.as_deref(),
            },
        )
        .context("emitting PPTX run")?;
    }

    Ok(inlines)
}

/// Classify a paragraph's bullet type into a list kind for grouping.
///
/// Returns `Some(ListKind)` for paragraphs that should become list items,
/// `None` for plain paragraphs or `BulletType::None` (which explicitly
/// suppresses inherited bullets).
fn bullet_list_kind(bullet: &Option<BulletType>) -> Option<ListKind> {
    match bullet {
        Some(BulletType::Char(_)) => Some(ListKind::Unordered),
        Some(BulletType::AutoNum) => Some(ListKind::Ordered),
        Some(BulletType::None) | None => None,
    }
}

/// Convert a slice of DrawingParagraphs into Blocks, grouping consecutive
/// bulleted paragraphs into `Block::List` nodes.
///
/// Non-bulleted paragraphs emit as `Block::Paragraph` or `Block::Heading`
/// (when `heading_level > 0`). Consecutive paragraphs with the same bullet
/// kind are merged into a single list.
fn paragraphs_to_blocks(
    doc: &mut Document,
    paras: &[DrawingParagraph],
    heading_level: u8,
    seen_urls: &mut HashSet<String>,
) -> Result<Vec<Block>> {
    let mut blocks = Vec::new();
    // Accumulator for consecutive bulleted paragraphs.
    let mut pending_items: Vec<ListItem> = Vec::new();
    let mut pending_kind: Option<ListKind> = None;

    for para in paras {
        if para.is_empty() {
            continue;
        }
        let inlines = paragraph_to_inlines(doc, para, seen_urls)
            .context("converting PPTX paragraph to inlines")?;
        if inlines.is_empty() {
            continue;
        }

        let lk = bullet_list_kind(&para.bullet);

        match lk {
            Some(kind) => {
                // This paragraph is a list item.
                if pending_kind == Some(kind) {
                    // Same list kind as pending: append to current run.
                } else {
                    // Different kind or no pending list: flush previous.
                    flush_pending_list(doc, &mut blocks, &mut pending_items, &mut pending_kind)?;
                    pending_kind = Some(kind);
                }

                let item_id = alloc_id(doc).context("allocating PPTX list item id")?;
                let para_id = alloc_id(doc).context("allocating PPTX list para id")?;

                set_block_layout(
                    doc,
                    para_id,
                    BlockLayout::new().alignment(
                        para.alignment
                            .as_deref()
                            .and_then(Alignment::from_format_str),
                    ),
                );

                pending_items.push(ListItem::new(
                    item_id,
                    vec![Block::Paragraph {
                        id: para_id,
                        content: inlines,
                    }],
                ));
            }
            None => {
                // Not a list item: flush any pending list, then emit block.
                flush_pending_list(doc, &mut blocks, &mut pending_items, &mut pending_kind)?;

                let block_id = alloc_id(doc).context("allocating PPTX block id")?;

                if heading_level > 0 {
                    blocks.push(Block::Heading {
                        id: block_id,
                        level: heading_level,
                        content: inlines,
                    });
                } else {
                    blocks.push(Block::Paragraph {
                        id: block_id,
                        content: inlines,
                    });
                }

                set_block_layout(
                    doc,
                    block_id,
                    BlockLayout::new().alignment(
                        para.alignment
                            .as_deref()
                            .and_then(Alignment::from_format_str),
                    ),
                );
            }
        }
    }

    // Flush any remaining list items.
    flush_pending_list(doc, &mut blocks, &mut pending_items, &mut pending_kind)?;

    Ok(blocks)
}

/// Flush accumulated list items into a `Block::List` and reset the accumulators.
fn flush_pending_list(
    doc: &Document,
    blocks: &mut Vec<Block>,
    items: &mut Vec<ListItem>,
    kind: &mut Option<ListKind>,
) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }

    let list_id = alloc_id(doc).context("allocating PPTX list id")?;
    let list_kind = kind.take().unwrap_or(ListKind::Unordered);
    let start = 1;

    blocks.push(Block::List {
        id: list_id,
        items: std::mem::take(items),
        kind: list_kind,
        start,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_containers::test_util::build_stored_zip;
    use udoc_core::diagnostics::NullDiagnostics;

    /// Build a minimal PPTX ZIP with a single slide containing the given shapes XML.
    fn make_pptx(slide_xml: &[u8]) -> Vec<u8> {
        make_pptx_with_rels(slide_xml, &[])
    }

    /// Build a PPTX ZIP with a single slide and optional slide-level relationships.
    fn make_pptx_with_rels(slide_xml: &[u8], extra_entries: &[(&str, &[u8])]) -> Vec<u8> {
        let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Default Extension="png" ContentType="image/png"/>
    <Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
    <Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
</Types>"#;

        let package_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
        Target="ppt/presentation.xml"/>
</Relationships>"#;

        let presentation_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
                xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
    <p:sldIdLst>
        <p:sldId id="256" r:id="rId2"/>
    </p:sldIdLst>
</p:presentation>"#;

        let pres_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId2"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide"
        Target="slides/slide1.xml"/>
</Relationships>"#;

        let mut entries: Vec<(&str, &[u8])> = vec![
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", package_rels),
            ("ppt/presentation.xml", presentation_xml),
            ("ppt/_rels/presentation.xml.rels", pres_rels),
            ("ppt/slides/slide1.xml", slide_xml),
        ];
        entries.extend_from_slice(extra_entries);

        build_stored_zip(&entries)
    }

    #[test]
    fn basic_conversion() {
        let slide_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="2" name="Title"/>
          <p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr>
          <p:nvPr><p:ph type="title"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr/>
        <p:txBody>
          <a:p><a:r><a:t>Slide Title</a:t></a:r></a:p>
        </p:txBody>
      </p:sp>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="3" name="Body"/>
          <p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr>
          <p:nvPr><p:ph type="body"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr/>
        <p:txBody>
          <a:p><a:r><a:t>Body text</a:t></a:r></a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let data = make_pptx(slide_xml);
        let mut pptx = PptxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = pptx_to_document(&mut pptx, &diag, usize::MAX).unwrap();

        // Should have heading (title) + paragraph (body).
        assert_eq!(result.content.len(), 2);
        assert!(matches!(
            &result.content[0],
            Block::Heading { level: 1, .. }
        ));
        assert!(matches!(&result.content[1], Block::Paragraph { .. }));
    }

    #[test]
    fn empty_slide_no_content() {
        let slide_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:cSld>
    <p:spTree/>
  </p:cSld>
</p:sld>"#;

        let data = make_pptx(slide_xml);
        let mut pptx = PptxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = pptx_to_document(&mut pptx, &diag, usize::MAX).unwrap();

        assert!(result.content.is_empty());
    }

    #[test]
    fn metadata_preserved() {
        let slide_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:cSld>
    <p:spTree/>
  </p:cSld>
</p:sld>"#;

        let data = make_pptx(slide_xml);
        let mut pptx = PptxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = pptx_to_document(&mut pptx, &diag, usize::MAX).unwrap();

        assert_eq!(result.metadata.page_count, 1);
    }

    #[test]
    fn bold_italic_underline_strikethrough_in_conversion() {
        let slide_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="2" name="Body"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="body"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr/>
        <p:txBody>
          <a:p>
            <a:r>
              <a:rPr b="1" i="1" u="sng" strike="sngStrike"/>
              <a:t>Styled text</a:t>
            </a:r>
          </a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let data = make_pptx(slide_xml);
        let mut pptx = PptxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = pptx_to_document(&mut pptx, &diag, usize::MAX).unwrap();

        match &result.content[0] {
            Block::Paragraph { content, .. } => match &content[0] {
                Inline::Text { style, text, .. } => {
                    assert_eq!(text, "Styled text");
                    assert!(style.bold);
                    assert!(style.italic);
                    assert!(style.underline);
                    assert!(style.strikethrough);
                }
                other => panic!("expected Text, got {:?}", other),
            },
            other => panic!("expected Paragraph, got {:?}", other),
        }
    }

    #[test]
    fn font_and_color_in_presentation_overlay() {
        let slide_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="2" name="Body"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="body"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr/>
        <p:txBody>
          <a:p>
            <a:r>
              <a:rPr sz="2400">
                <a:solidFill><a:srgbClr val="FF0000"/></a:solidFill>
                <a:latin typeface="Arial"/>
              </a:rPr>
              <a:t>Red Arial</a:t>
            </a:r>
          </a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let data = make_pptx(slide_xml);
        let mut pptx = PptxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = pptx_to_document(&mut pptx, &diag, usize::MAX).unwrap();

        match &result.content[0] {
            Block::Paragraph { content, .. } => {
                let text_id = content[0].id();
                let pres = result.presentation.as_ref().expect("presentation layer");
                let ext = pres.text_styling.get(text_id).expect("text styling");
                assert_eq!(ext.font_name.as_deref(), Some("Arial"));
                assert_eq!(ext.font_size, Some(24.0));
                assert_eq!(ext.color, Some(Color::rgb(255, 0, 0)));
            }
            other => panic!("expected Paragraph, got {:?}", other),
        }
    }

    #[test]
    fn alignment_in_presentation_overlay() {
        let slide_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="2" name="Body"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="body"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr/>
        <p:txBody>
          <a:p>
            <a:pPr algn="ctr"/>
            <a:r><a:t>Centered text</a:t></a:r>
          </a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let data = make_pptx(slide_xml);
        let mut pptx = PptxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = pptx_to_document(&mut pptx, &diag, usize::MAX).unwrap();

        match &result.content[0] {
            Block::Paragraph { id, .. } => {
                let pres = result.presentation.as_ref().expect("presentation layer");
                let layout = pres.block_layout.get(*id).expect("block layout");
                assert_eq!(layout.alignment, Some(Alignment::Center));
            }
            other => panic!("expected Paragraph, got {:?}", other),
        }
    }

    #[test]
    fn hyperlink_emits_link_inline() {
        let slide_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="2" name="Body"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="body"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr/>
        <p:txBody>
          <a:p>
            <a:r>
              <a:rPr>
                <a:hlinkClick r:id="rId3"/>
              </a:rPr>
              <a:t>Click me</a:t>
            </a:r>
          </a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let slide_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId3"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink"
        Target="https://example.com" TargetMode="External"/>
</Relationships>"#;

        let data = make_pptx_with_rels(
            slide_xml,
            &[("ppt/slides/_rels/slide1.xml.rels", slide_rels)],
        );
        let mut pptx = PptxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = pptx_to_document(&mut pptx, &diag, usize::MAX).unwrap();

        match &result.content[0] {
            Block::Paragraph { content, .. } => match &content[0] {
                Inline::Link { url, content, .. } => {
                    assert_eq!(url, "https://example.com");
                    match &content[0] {
                        Inline::Text { text, .. } => {
                            assert_eq!(text, "Click me");
                        }
                        other => panic!("expected Text inside Link, got {:?}", other),
                    }
                }
                other => panic!("expected Link, got {:?}", other),
            },
            other => panic!("expected Paragraph, got {:?}", other),
        }
    }

    #[test]
    fn image_extraction_via_page_extractor() {
        // PNG magic bytes (minimal 1x1 PNG header)
        let png_data: Vec<u8> = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
            0x00, 0x00, 0x00, 0x0D, // IHDR chunk length
            0x49, 0x48, 0x44, 0x52, // "IHDR"
            0x00, 0x00, 0x00, 0x01, // width: 1
            0x00, 0x00, 0x00, 0x01, // height: 1
            0x08, 0x02, // bit depth: 8, color type: RGB
            0x00, 0x00, 0x00, // compression, filter, interlace
        ];

        let slide_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <p:cSld>
    <p:spTree>
      <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
      <p:grpSpPr/>
      <p:pic>
        <p:nvPicPr>
          <p:cNvPr id="4" name="Picture 1" descr="A test image"/>
          <p:cNvPicPr/>
          <p:nvPr/>
        </p:nvPicPr>
        <p:blipFill>
          <a:blip r:embed="rId4"/>
        </p:blipFill>
        <p:spPr>
          <a:xfrm><a:off x="0" y="0"/><a:ext cx="5000" cy="5000"/></a:xfrm>
        </p:spPr>
      </p:pic>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let slide_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId4"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image"
        Target="../media/image1.png"/>
</Relationships>"#;

        let data = make_pptx_with_rels(
            slide_xml,
            &[
                ("ppt/slides/_rels/slide1.xml.rels", slide_rels),
                ("ppt/media/image1.png", &png_data),
            ],
        );
        let mut pptx = PptxDocument::from_bytes(&data).unwrap();
        let mut page = FormatBackend::page(&mut pptx, 0).unwrap();
        let images = page.images().unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].filter, udoc_core::image::ImageFilter::Png);
        assert_eq!(images[0].data, png_data);
    }

    #[test]
    fn consecutive_bullet_chars_become_unordered_list() {
        let slide_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="2" name="Body"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="body"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr/>
        <p:txBody>
          <a:p>
            <a:pPr><a:buChar char="-"/></a:pPr>
            <a:r><a:t>Item one</a:t></a:r>
          </a:p>
          <a:p>
            <a:pPr><a:buChar char="-"/></a:pPr>
            <a:r><a:t>Item two</a:t></a:r>
          </a:p>
          <a:p>
            <a:pPr><a:buChar char="-"/></a:pPr>
            <a:r><a:t>Item three</a:t></a:r>
          </a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let data = make_pptx(slide_xml);
        let mut pptx = PptxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = pptx_to_document(&mut pptx, &diag, usize::MAX).unwrap();

        assert_eq!(result.content.len(), 1);
        match &result.content[0] {
            Block::List {
                items, kind, start, ..
            } => {
                assert_eq!(*kind, ListKind::Unordered);
                assert_eq!(*start, 1);
                assert_eq!(items.len(), 3);
                assert_eq!(items[0].content[0].text(), "Item one");
                assert_eq!(items[1].content[0].text(), "Item two");
                assert_eq!(items[2].content[0].text(), "Item three");
            }
            other => panic!("expected List, got {:?}", other),
        }
    }

    #[test]
    fn autonum_bullets_become_ordered_list() {
        let slide_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="2" name="Body"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="body"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr/>
        <p:txBody>
          <a:p>
            <a:pPr><a:buAutoNum type="arabicPeriod"/></a:pPr>
            <a:r><a:t>First</a:t></a:r>
          </a:p>
          <a:p>
            <a:pPr><a:buAutoNum type="arabicPeriod"/></a:pPr>
            <a:r><a:t>Second</a:t></a:r>
          </a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let data = make_pptx(slide_xml);
        let mut pptx = PptxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = pptx_to_document(&mut pptx, &diag, usize::MAX).unwrap();

        assert_eq!(result.content.len(), 1);
        match &result.content[0] {
            Block::List {
                items, kind, start, ..
            } => {
                assert_eq!(*kind, ListKind::Ordered);
                assert_eq!(*start, 1);
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].content[0].text(), "First");
                assert_eq!(items[1].content[0].text(), "Second");
            }
            other => panic!("expected List, got {:?}", other),
        }
    }

    #[test]
    fn mixed_bullets_and_plain_paragraphs() {
        let slide_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="2" name="Body"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="body"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr/>
        <p:txBody>
          <a:p>
            <a:r><a:t>Intro text</a:t></a:r>
          </a:p>
          <a:p>
            <a:pPr><a:buChar char="-"/></a:pPr>
            <a:r><a:t>Bullet A</a:t></a:r>
          </a:p>
          <a:p>
            <a:pPr><a:buChar char="-"/></a:pPr>
            <a:r><a:t>Bullet B</a:t></a:r>
          </a:p>
          <a:p>
            <a:r><a:t>Outro text</a:t></a:r>
          </a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let data = make_pptx(slide_xml);
        let mut pptx = PptxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = pptx_to_document(&mut pptx, &diag, usize::MAX).unwrap();

        // Paragraph + List(2 items) + Paragraph
        assert_eq!(result.content.len(), 3);
        assert!(matches!(&result.content[0], Block::Paragraph { .. }));
        match &result.content[1] {
            Block::List { items, kind, .. } => {
                assert_eq!(*kind, ListKind::Unordered);
                assert_eq!(items.len(), 2);
            }
            other => panic!("expected List, got {:?}", other),
        }
        assert!(matches!(&result.content[2], Block::Paragraph { .. }));
    }

    #[test]
    fn bunone_breaks_list_grouping() {
        let slide_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="2" name="Body"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="body"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr/>
        <p:txBody>
          <a:p>
            <a:pPr><a:buChar char="-"/></a:pPr>
            <a:r><a:t>Bullet</a:t></a:r>
          </a:p>
          <a:p>
            <a:pPr><a:buNone/></a:pPr>
            <a:r><a:t>Not a bullet</a:t></a:r>
          </a:p>
          <a:p>
            <a:pPr><a:buChar char="-"/></a:pPr>
            <a:r><a:t>Another bullet</a:t></a:r>
          </a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let data = make_pptx(slide_xml);
        let mut pptx = PptxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = pptx_to_document(&mut pptx, &diag, usize::MAX).unwrap();

        // List(1) + Paragraph + List(1)
        assert_eq!(result.content.len(), 3);
        assert!(matches!(
            &result.content[0],
            Block::List {
                kind: ListKind::Unordered,
                ..
            }
        ));
        assert!(matches!(&result.content[1], Block::Paragraph { .. }));
        assert!(matches!(
            &result.content[2],
            Block::List {
                kind: ListKind::Unordered,
                ..
            }
        ));
    }

    #[test]
    fn different_bullet_kinds_split_into_separate_lists() {
        let slide_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="2" name="Body"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="body"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr/>
        <p:txBody>
          <a:p>
            <a:pPr><a:buChar char="-"/></a:pPr>
            <a:r><a:t>Unordered</a:t></a:r>
          </a:p>
          <a:p>
            <a:pPr><a:buAutoNum type="arabicPeriod"/></a:pPr>
            <a:r><a:t>Ordered</a:t></a:r>
          </a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let data = make_pptx(slide_xml);
        let mut pptx = PptxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = pptx_to_document(&mut pptx, &diag, usize::MAX).unwrap();

        // Two separate lists with different kinds.
        assert_eq!(result.content.len(), 2);
        match &result.content[0] {
            Block::List { kind, items, .. } => {
                assert_eq!(*kind, ListKind::Unordered);
                assert_eq!(items.len(), 1);
            }
            other => panic!("expected unordered List, got {:?}", other),
        }
        match &result.content[1] {
            Block::List { kind, items, .. } => {
                assert_eq!(*kind, ListKind::Ordered);
                assert_eq!(items.len(), 1);
            }
            other => panic!("expected ordered List, got {:?}", other),
        }
    }
}
