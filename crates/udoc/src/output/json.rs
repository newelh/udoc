//! JSON output (--json).
//!
//! Writes the full Document model as a single JSON object.
//! Uses a zero-copy serialization wrapper to avoid cloning the
//! entire Document (which includes images) when stripping layers.

use std::io::Write;

use serde::ser::SerializeMap;
use udoc_core::document::{Document, Presentation};

/// Write a Document as JSON.
///
/// Options:
/// - `pretty`: use indented output (default when stdout is a tty)
/// - `include_presentation`: include the presentation layer
/// - `include_raw_spans`: include raw positioned spans in presentation
pub fn write_json(
    doc: &Document,
    writer: &mut dyn Write,
    pretty: bool,
    include_presentation: bool,
    include_raw_spans: bool,
) -> std::io::Result<()> {
    let view = DocumentJsonView {
        doc,
        include_presentation,
        include_raw_spans,
    };

    if pretty {
        serde_json::to_writer_pretty(&mut *writer, &view).map_err(std::io::Error::other)?;
    } else {
        serde_json::to_writer(&mut *writer, &view).map_err(std::io::Error::other)?;
    }
    writeln!(writer)?;
    Ok(())
}

/// Zero-copy serialization wrapper that selectively includes/excludes layers
/// without cloning the Document.
struct DocumentJsonView<'a> {
    doc: &'a Document,
    include_presentation: bool,
    include_raw_spans: bool,
}

impl serde::Serialize for DocumentJsonView<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let doc = self.doc;
        let mut count = 4; // version, content, metadata, images
        let has_pres = self.include_presentation && doc.presentation.is_some();
        if has_pres {
            count += 1;
        }
        if doc.relationships.is_some() {
            count += 1;
        }
        if doc.interactions.is_some() {
            count += 1;
        }
        let mut map = serializer.serialize_map(Some(count))?;
        map.serialize_entry("version", &1u32)?;
        map.serialize_entry("content", &doc.content)?;
        if let Some(pres) = doc.presentation.as_ref().filter(|_| has_pres) {
            if self.include_raw_spans {
                map.serialize_entry("presentation", pres)?;
            } else {
                map.serialize_entry("presentation", &PresentationNoSpans(pres))?;
            }
        }
        if let Some(ref r) = doc.relationships {
            map.serialize_entry("relationships", r)?;
        }
        map.serialize_entry("metadata", &doc.metadata)?;
        if let Some(ref i) = doc.interactions {
            map.serialize_entry("interactions", i)?;
        }
        map.serialize_entry("images", doc.assets.images())?;
        map.end()
    }
}

/// Wrapper that serializes Presentation with raw_spans replaced by an empty array.
///
/// SYNC: Must list all Presentation fields. If Presentation gains a field,
/// add it here. The test `presentation_no_spans_field_count` catches drift.
struct PresentationNoSpans<'a>(&'a Presentation);

impl serde::Serialize for PresentationNoSpans<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let p = self.0;
        let mut map = serializer.serialize_map(Some(13))?;
        map.serialize_entry("pages", &p.pages)?;
        map.serialize_entry("page_assignments", &p.page_assignments)?;
        map.serialize_entry("geometry", &p.geometry)?;
        map.serialize_entry("text_styling", &p.text_styling)?;
        map.serialize_entry("block_layout", &p.block_layout)?;
        map.serialize_entry("column_specs", &p.column_specs)?;
        map.serialize_entry("layout_info", &p.layout_info)?;
        let empty: &[(); 0] = &[];
        map.serialize_entry("raw_spans", &empty)?;
        map.serialize_entry("shapes", &p.shapes)?;
        map.serialize_entry("image_placements", &p.image_placements)?;
        map.serialize_entry("paint_paths", &p.paint_paths)?;
        map.serialize_entry("shadings", &p.shadings)?;
        map.serialize_entry("patterns", &p.patterns)?;
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::document::{Block, Inline, NodeId, Presentation, SpanStyle};

    fn make_test_doc() -> Document {
        let mut doc = Document::new();
        doc.content.push(Block::Paragraph {
            id: NodeId::new(0),
            content: vec![Inline::Text {
                id: NodeId::new(1),
                text: "Hello world".into(),
                style: SpanStyle::default(),
            }],
        });
        doc.metadata.title = Some("Test".into());
        doc.metadata.page_count = 1;
        doc
    }

    #[test]
    fn json_compact() {
        let doc = make_test_doc();
        let mut buf = Vec::new();
        write_json(&doc, &mut buf, false, true, false).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\"version\":1"));
        assert!(s.contains("Hello world"));
        assert!(s.ends_with('\n'));
        // Should be a single line (compact)
        assert_eq!(s.lines().count(), 1);
    }

    #[test]
    fn json_pretty() {
        let doc = make_test_doc();
        let mut buf = Vec::new();
        write_json(&doc, &mut buf, true, true, false).unwrap();
        let s = String::from_utf8(buf).unwrap();
        // Pretty should be multiple lines
        assert!(s.lines().count() > 1);
    }

    #[test]
    fn json_strips_presentation() {
        let mut doc = make_test_doc();
        doc.presentation = Some(Presentation::default());

        let mut buf = Vec::new();
        write_json(&doc, &mut buf, false, false, false).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(!s.contains("\"presentation\""));
    }

    #[test]
    fn json_strips_raw_spans() {
        let mut doc = make_test_doc();
        let mut pres = Presentation::default();
        pres.raw_spans
            .push(udoc_core::document::PositionedSpan::new(
                "span".into(),
                udoc_core::geometry::BoundingBox::new(0.0, 0.0, 10.0, 10.0),
                0,
            ));
        doc.presentation = Some(pres);

        // With presentation but no raw spans
        let mut buf = Vec::new();
        write_json(&doc, &mut buf, false, true, false).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\"presentation\""));
        assert!(s.contains("\"raw_spans\":[]"));
    }

    #[test]
    fn json_includes_raw_spans() {
        let mut doc = make_test_doc();
        let mut pres = Presentation::default();
        pres.raw_spans
            .push(udoc_core::document::PositionedSpan::new(
                "span".into(),
                udoc_core::geometry::BoundingBox::new(0.0, 0.0, 10.0, 10.0),
                0,
            ));
        doc.presentation = Some(pres);

        let mut buf = Vec::new();
        write_json(&doc, &mut buf, false, true, true).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\"raw_spans\":[{"));
    }

    #[test]
    fn presentation_no_spans_field_count() {
        let pres = Presentation::default();
        let full: serde_json::Value = serde_json::to_value(&pres).unwrap();
        let wrapper = PresentationNoSpans(&pres);
        let stripped: serde_json::Value = serde_json::to_value(&wrapper).unwrap();
        let full_keys = full.as_object().unwrap().len();
        let stripped_keys = stripped.as_object().unwrap().len();
        assert_eq!(
            full_keys, stripped_keys,
            "PresentationNoSpans has {} keys but Presentation has {}. \
             A field was added to Presentation but not to PresentationNoSpans.",
            stripped_keys, full_keys
        );
    }
}
