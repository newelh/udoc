//! Speaker notes extraction from PPTX notes slides.
//!
//! Notes slides are stored in `ppt/notesSlides/notesSlideN.xml` and linked
//! from slides via per-slide relationships of type `notesSlide`.
//!
//! Only the body placeholder text is extracted (skipping slide image and
//! slide number placeholders).

use udoc_containers::xml::{attr_value, XmlEvent, XmlReader};
use udoc_core::error::{Result, ResultExt};

use crate::shapes::SlideContext;
use crate::text::{is_pml, parse_text_body_with_depth, skip_element, DrawingParagraph};

/// Parse speaker notes from a notes slide XML part.
///
/// Returns the extracted paragraphs from the body placeholder only.
/// Returns an empty vec if no body placeholder is found.
pub(crate) fn parse_notes_slide(
    xml_data: &[u8],
    ctx: &SlideContext<'_>,
) -> Result<Vec<DrawingParagraph>> {
    let mut reader = XmlReader::new(xml_data).context("parsing notes slide XML")?;
    let mut notes_paragraphs = Vec::new();

    // Walk the shape tree looking for the body placeholder
    let mut depth: u32 = 0;
    let mut in_body_placeholder = false;
    loop {
        let event = reader.next_event().context("reading notes slide")?;
        match event {
            XmlEvent::StartElement {
                ref local_name,
                ref namespace_uri,
                ref attributes,
                ..
            } => {
                depth = depth.saturating_add(1);
                let name = local_name.as_ref();

                if is_pml(namespace_uri) {
                    match name {
                        "sp" => {
                            // Start of a new shape, reset placeholder tracking
                            in_body_placeholder = false;
                        }
                        "ph" => {
                            let ph_type = attr_value(attributes, "type").unwrap_or("obj");
                            if ph_type == "body" {
                                in_body_placeholder = true;
                            }
                            skip_element(&mut reader, &mut depth)?;
                        }
                        "txBody" if in_body_placeholder => {
                            let paras = parse_text_body_with_depth(&mut reader, &mut depth, ctx)?;
                            notes_paragraphs.extend(paras);
                            in_body_placeholder = false;
                        }
                        _ => {}
                    }
                }
            }
            XmlEvent::EndElement {
                ref local_name,
                ref namespace_uri,
                ..
            } => {
                depth = depth.saturating_sub(1);
                if is_pml(namespace_uri) && local_name.as_ref() == "sp" {
                    in_body_placeholder = false;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(notes_paragraphs)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use udoc_core::diagnostics::NullDiagnostics;

    #[test]
    fn extract_notes_body() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:notes xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
         xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
      <p:grpSpPr/>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="2" name="Slide Image"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="sldImg"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr/>
      </p:sp>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="3" name="Notes Placeholder"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="body" idx="1"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr/>
        <p:txBody>
          <a:bodyPr/>
          <a:p><a:r><a:t>These are speaker notes</a:t></a:r></a:p>
          <a:p><a:r><a:t>Second paragraph of notes</a:t></a:r></a:p>
        </p:txBody>
      </p:sp>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="4" name="Slide Number"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="sldNum"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr/>
        <p:txBody>
          <a:bodyPr/>
          <a:p><a:fld type="slidenum"><a:t>1</a:t></a:fld></a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:notes>"#;

        let empty_theme = std::collections::HashMap::new();
        let ctx = SlideContext {
            diag: &NullDiagnostics,
            slide_index: 0,
            slide_rels: &[],
            scheme_color_warned: Cell::new(false),
            theme_colors: &empty_theme,
        };
        let paras = parse_notes_slide(xml, &ctx).unwrap();
        assert_eq!(paras.len(), 2);
        assert_eq!(paras[0].text(), "These are speaker notes");
        assert_eq!(paras[1].text(), "Second paragraph of notes");
    }

    #[test]
    fn no_body_placeholder() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:notes xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
         xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
      <p:grpSpPr/>
      <p:sp>
        <p:nvSpPr>
          <p:cNvPr id="2" name="Slide Image"/>
          <p:cNvSpPr/>
          <p:nvPr><p:ph type="sldImg"/></p:nvPr>
        </p:nvSpPr>
        <p:spPr/>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:notes>"#;

        let empty_theme = std::collections::HashMap::new();
        let ctx = SlideContext {
            diag: &NullDiagnostics,
            slide_index: 0,
            slide_rels: &[],
            scheme_color_warned: Cell::new(false),
            theme_colors: &empty_theme,
        };
        let paras = parse_notes_slide(xml, &ctx).unwrap();
        assert!(paras.is_empty());
    }
}
