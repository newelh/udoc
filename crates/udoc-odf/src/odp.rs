//! ODP (office:presentation) body parser.
//!
//! Walks office:body > office:presentation > draw:page and extracts text
//! from shapes (draw:frame > draw:text-box > text:p). Uses the
//! presentation:class attribute for heading inference.

use udoc_containers::xml::namespace::ns;
use udoc_containers::xml::{attr_value, XmlEvent, XmlReader};
use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use crate::error::{Result, ResultExt};

/// Maximum number of slides to collect (safety limit).
/// Aligned with PPTX/PPT (10,000).
const MAX_SLIDES: usize = 10_000;

/// Maximum number of elements (paragraphs) per slide (safety limit).
const MAX_SLIDE_ELEMENTS: usize = 100_000;

/// A paragraph on a slide.
#[derive(Debug, Clone)]
pub(crate) struct OdpParagraph {
    pub text: String,
    /// Heading level inferred from presentation:class.
    /// 0 = not a heading, 1 = title, 2 = subtitle.
    pub heading_level: u8,
}

/// A single slide.
#[derive(Debug, Clone)]
pub(crate) struct OdpSlide {
    #[allow(dead_code)]
    pub name: String,
    pub paragraphs: Vec<OdpParagraph>,
    /// Speaker notes text, if any.
    pub notes: Option<String>,
}

/// Parsed ODP body.
#[derive(Debug)]
pub(crate) struct OdpBody {
    pub slides: Vec<OdpSlide>,
}

/// Parse the ODP body from content.xml.
pub(crate) fn parse_odp_body(data: &[u8], diag: &dyn DiagnosticsSink) -> Result<OdpBody> {
    let mut reader = XmlReader::new(data).context("initializing XML parser for ODP body")?;

    let mut slides = Vec::new();
    let mut in_presentation = false;

    loop {
        let event = reader.next_element().context("parsing ODP body")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                let name = local_name.as_ref();

                if ns_str == ns::OFFICE && name == "presentation" {
                    in_presentation = true;
                    continue;
                }

                if !in_presentation {
                    continue;
                }

                if ns_str == ns::DRAW && name == "page" {
                    if slides.len() >= MAX_SLIDES {
                        diag.warning(Warning::new(
                            "OdpMaxSlides",
                            format!("slide limit ({MAX_SLIDES}) exceeded, truncating"),
                        ));
                        crate::styles::skip_element(&mut reader)?;
                        break;
                    }
                    let slide_name = attr_value(&attributes, "name")
                        .unwrap_or("Slide")
                        .to_string();
                    let slide = parse_slide(&mut reader, slide_name, diag)?;
                    slides.push(slide);
                }
            }
            XmlEvent::EndElement {
                local_name,
                namespace_uri,
                ..
            } => {
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                if ns_str == ns::OFFICE && local_name.as_ref() == "presentation" {
                    in_presentation = false;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(OdpBody { slides })
}

/// Parse a draw:page element into an OdpSlide.
fn parse_slide(
    reader: &mut XmlReader<'_>,
    name: String,
    diag: &dyn DiagnosticsSink,
) -> Result<OdpSlide> {
    let mut paragraphs = Vec::new();
    let mut notes = None;
    let mut depth: usize = 1;
    // Track the current presentation:class for heading inference.
    let mut current_class: Option<String> = None;
    let mut elements_capped = false;

    loop {
        let event = reader.next_element().context("parsing ODP slide")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                depth += 1;
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                let ename = local_name.as_ref();

                match (ns_str, ename) {
                    (ns_draw, "frame") if ns_draw == ns::DRAW => {
                        // Check for presentation:class on the frame.
                        let class = attr_value(&attributes, "class").map(|s| s.to_string());
                        if class.is_some() {
                            current_class = class;
                        }
                    }
                    (ns_draw, "text-box") if ns_draw == ns::DRAW => {
                        // Collect paragraphs from the text box.
                        let heading_level = heading_level_from_class(current_class.as_deref());
                        let box_paras = collect_text_box_paras(reader, heading_level)?;
                        if !elements_capped {
                            if paragraphs.len() + box_paras.len() > MAX_SLIDE_ELEMENTS {
                                diag.warning(Warning::new(
                                    "OdpMaxSlideElements",
                                    format!(
                                        "slide element limit ({MAX_SLIDE_ELEMENTS}) exceeded, \
                                         skipping remaining elements"
                                    ),
                                ));
                                elements_capped = true;
                            } else {
                                paragraphs.extend(box_paras);
                            }
                        }
                        depth = depth.saturating_sub(1); // collect consumed end element.
                        current_class = None;
                    }
                    (ns_pres, "notes") if ns_pres == ns::PRESENTATION => {
                        let notes_text = collect_notes(reader)?;
                        if !notes_text.is_empty() {
                            notes = Some(notes_text);
                        }
                        depth = depth.saturating_sub(1); // collect consumed end element.
                    }
                    _ => {}
                }
            }
            XmlEvent::EndElement {
                local_name,
                namespace_uri,
                ..
            } => {
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                // Reset class when leaving a frame.
                if ns_str == ns::DRAW && local_name.as_ref() == "frame" {
                    current_class = None;
                }
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(OdpSlide {
        name,
        paragraphs,
        notes,
    })
}

/// Infer heading level from presentation:class.
fn heading_level_from_class(class: Option<&str>) -> u8 {
    match class {
        Some("title") => 1,
        Some("subtitle") => 2,
        _ => 0,
    }
}

/// Collect paragraphs from a draw:text-box.
fn collect_text_box_paras(
    reader: &mut XmlReader<'_>,
    heading_level: u8,
) -> Result<Vec<OdpParagraph>> {
    let mut paras = Vec::new();
    let mut depth: usize = 1;
    let mut text_buf = String::new();
    let mut in_paragraph = false;

    loop {
        let event = reader.next_event().context("collecting text-box")?;

        match event {
            XmlEvent::Text(text) | XmlEvent::CData(text) => {
                if in_paragraph {
                    text_buf.push_str(text.as_ref());
                }
            }
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                ..
            } => {
                depth += 1;
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                // New text:p means a new paragraph.
                if ns_str == ns::TEXT && local_name.as_ref() == "p" {
                    // Flush previous paragraph if any.
                    if !text_buf.is_empty() {
                        paras.push(OdpParagraph {
                            text: std::mem::take(&mut text_buf),
                            heading_level,
                        });
                    }
                    in_paragraph = true;
                }
            }
            XmlEvent::EndElement {
                local_name,
                namespace_uri,
                ..
            } => {
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                if ns_str == ns::TEXT && local_name.as_ref() == "p" {
                    in_paragraph = false;
                }
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            XmlEvent::Eof => break,
        }
    }

    if !text_buf.is_empty() {
        paras.push(OdpParagraph {
            text: text_buf,
            heading_level,
        });
    }

    Ok(paras)
}

/// Collect text from a presentation:notes element.
fn collect_notes(reader: &mut XmlReader<'_>) -> Result<String> {
    let mut parts = Vec::new();
    let mut depth: usize = 1;
    let mut text_buf = String::new();
    let mut in_paragraph = false;

    loop {
        let event = reader.next_event().context("collecting notes")?;

        match event {
            XmlEvent::Text(text) | XmlEvent::CData(text) => {
                if in_paragraph {
                    text_buf.push_str(text.as_ref());
                }
            }
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                ..
            } => {
                depth += 1;
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                if ns_str == ns::TEXT && local_name.as_ref() == "p" {
                    if !text_buf.is_empty() {
                        parts.push(std::mem::take(&mut text_buf));
                    }
                    in_paragraph = true;
                }
            }
            XmlEvent::EndElement {
                local_name,
                namespace_uri,
                ..
            } => {
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                if ns_str == ns::TEXT && local_name.as_ref() == "p" {
                    in_paragraph = false;
                }
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            XmlEvent::Eof => break,
        }
    }

    if !text_buf.is_empty() {
        parts.push(text_buf);
    }

    Ok(parts.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::diagnostics::NullDiagnostics;

    #[test]
    fn parse_slides_with_shapes() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
    xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:presentation>
      <draw:page draw:name="Slide1">
        <draw:frame presentation:class="title">
          <draw:text-box>
            <text:p>Slide Title</text:p>
          </draw:text-box>
        </draw:frame>
        <draw:frame presentation:class="subtitle">
          <draw:text-box>
            <text:p>Subtitle</text:p>
          </draw:text-box>
        </draw:frame>
        <draw:frame>
          <draw:text-box>
            <text:p>Body text</text:p>
          </draw:text-box>
        </draw:frame>
      </draw:page>
    </office:presentation>
  </office:body>
</office:document-content>"#;

        let body = parse_odp_body(xml, &NullDiagnostics).unwrap();
        assert_eq!(body.slides.len(), 1);
        let slide = &body.slides[0];
        assert_eq!(slide.name, "Slide1");
        assert_eq!(slide.paragraphs.len(), 3);
        assert_eq!(slide.paragraphs[0].text, "Slide Title");
        assert_eq!(slide.paragraphs[0].heading_level, 1);
        assert_eq!(slide.paragraphs[1].text, "Subtitle");
        assert_eq!(slide.paragraphs[1].heading_level, 2);
        assert_eq!(slide.paragraphs[2].text, "Body text");
        assert_eq!(slide.paragraphs[2].heading_level, 0);
    }

    #[test]
    fn parse_notes() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
    xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:presentation>
      <draw:page draw:name="Slide1">
        <draw:frame presentation:class="title">
          <draw:text-box>
            <text:p>Title</text:p>
          </draw:text-box>
        </draw:frame>
        <presentation:notes>
          <draw:frame>
            <draw:text-box>
              <text:p>Speaker notes here</text:p>
            </draw:text-box>
          </draw:frame>
        </presentation:notes>
      </draw:page>
    </office:presentation>
  </office:body>
</office:document-content>"#;

        let body = parse_odp_body(xml, &NullDiagnostics).unwrap();
        let slide = &body.slides[0];
        assert_eq!(slide.notes.as_deref(), Some("Speaker notes here"));
    }

    #[test]
    fn empty_presentation() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0">
  <office:body>
    <office:presentation/>
  </office:body>
</office:document-content>"#;

        let body = parse_odp_body(xml, &NullDiagnostics).unwrap();
        assert!(body.slides.is_empty());
    }
}
