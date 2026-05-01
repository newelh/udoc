//! Hook request JSON construction and coordinate transforms.
//!
//! Builds JSON request payloads for hook I/O (document -> JSON).
//! Response parsing (JSON -> domain types) lives in response.rs.

use std::path::Path;

use serde_json::Value;

use udoc_core::document::*;

use super::process::HookProcess;
use super::protocol::Need;

// ---------------------------------------------------------------------------
// Request building
// ---------------------------------------------------------------------------

/// Build a JSON request for a hook based on its declared needs.
/// Returns None if the request cannot be built (e.g., needs image but no dir).
pub(crate) fn build_request(
    hook: &HookProcess,
    page_idx: usize,
    page_images: Option<&Path>,
    doc: &Document,
    page_texts: &[String],
    chain_spans: &[Value],
    image_dpi: u32,
) -> Option<Value> {
    let mut request = serde_json::Map::new();
    request.insert("page_index".into(), Value::from(page_idx));

    // Extract page height for coordinate transforms (y-up -> y-down).
    let page_height = doc
        .presentation
        .as_ref()
        .and_then(|p| p.pages.get(page_idx))
        .map(|pg| pg.height);

    for need in &hook.needs {
        match need {
            Need::Image => {
                let dir = match page_images {
                    Some(d) => d,
                    None => {
                        eprintln!(
                            "hook {}: needs image but no page_images directory provided, skipping",
                            hook.command
                        );
                        return None;
                    }
                };
                let image_path = dir.join(format!("page-{}.png", page_idx));
                if !image_path.exists() {
                    // Page was not rendered (e.g., outside page range).
                    return None;
                }
                request.insert(
                    "image_path".into(),
                    Value::String(image_path.to_string_lossy().into_owned()),
                );
                // DPI lets hooks convert pixel coordinates to point coordinates.
                request.insert("dpi".into(), Value::from(image_dpi));

                // Include page dimensions from presentation layer if available.
                if let Some(ref pres) = doc.presentation {
                    if let Some(page_def) = pres.pages.get(page_idx) {
                        request.insert("width".into(), Value::from(page_def.width));
                        request.insert("height".into(), Value::from(page_def.height));
                    }
                }
            }
            Need::Spans => {
                // Use chain_spans if available (from a previous hook in the chain),
                // otherwise use raw_spans from the presentation layer.
                if !chain_spans.is_empty() {
                    request.insert("spans".into(), Value::Array(chain_spans.to_vec()));
                } else if let Some(ref pres) = doc.presentation {
                    let spans: Vec<Value> = pres
                        .raw_spans
                        .iter()
                        .filter(|s| s.page_index == page_idx)
                        .map(|s| positioned_span_to_json(s, page_height))
                        .collect();
                    request.insert("spans".into(), Value::Array(spans));
                } else {
                    request.insert("spans".into(), Value::Array(Vec::new()));
                }
            }
            Need::Blocks => {
                let blocks = collect_page_blocks_json(doc, page_idx);
                request.insert("blocks".into(), Value::Array(blocks));
            }
            Need::Text => {
                let text = page_texts.get(page_idx).cloned().unwrap_or_default();
                request.insert("text".into(), Value::String(text));
            }
            Need::Document => {
                // Document-level hooks are dispatched via build_document_request,
                // not per-page. This arm is unreachable in normal operation because
                // mod.rs partitions hooks before calling build_request, but the
                // match must be exhaustive.
                if cfg!(debug_assertions) {
                    panic!("Need::Document hook reached build_request; should have been dispatched via build_document_request");
                }
            }
        }
    }

    Some(Value::Object(request))
}

/// Build a document-level JSON request for a hook that declared Need::Document.
///
/// Sends a single request for the whole document instead of per-page requests.
/// The hook responds with `{"pages": [{"page_index": N, "spans": [...]}, ...]}`.
pub(crate) fn build_document_request(
    doc: &Document,
    page_images: Option<&Path>,
    page_count: usize,
) -> Value {
    let mut request = serde_json::Map::new();

    // Include the document path if we have a page image directory -- hooks can
    // infer the document path from context, but providing it directly is cleaner.
    // We pass the page_images dir as a proxy since we don't have the doc path here.
    if let Some(dir) = page_images {
        request.insert(
            "page_images_dir".into(),
            Value::String(dir.to_string_lossy().into_owned()),
        );
    }

    request.insert("page_count".into(), Value::from(page_count));

    // Include format hint if available in metadata properties.
    // Hooks can use this to select the right processing path.
    if let Some(fmt) = doc.metadata.properties.get("udoc.format") {
        request.insert("format".into(), Value::String(fmt.clone()));
    }

    Value::Object(request)
}

/// Convert a PositionedSpan to a JSON value for hook requests.
/// When `page_height` is provided, bbox Y coordinates are transformed from
/// PDF-native (y-up) to hook wire format (y-down).
pub(crate) fn positioned_span_to_json(span: &PositionedSpan, page_height: Option<f64>) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("text".into(), Value::String(span.text.clone()));

    let (y_min, y_max) = if let Some(h) = page_height {
        (h - span.bbox.y_max, h - span.bbox.y_min)
    } else {
        (span.bbox.y_min, span.bbox.y_max)
    };
    obj.insert(
        "bbox".into(),
        Value::Array(vec![
            Value::from(span.bbox.x_min),
            Value::from(y_min),
            Value::from(span.bbox.x_max),
            Value::from(y_max),
        ]),
    );
    if let Some(ref name) = span.font_name {
        obj.insert("font_name".into(), Value::String(name.clone()));
    }
    if let Some(size) = span.font_size {
        obj.insert("font_size".into(), Value::from(size));
    }
    obj.insert("is_bold".into(), Value::Bool(span.is_bold));
    obj.insert("is_italic".into(), Value::Bool(span.is_italic));
    Value::Object(obj)
}

/// Collect blocks that belong to a specific page as JSON values.
/// Uses page_assignments from presentation layer, or falls back to
/// distributing blocks by PageBreak boundaries.
fn collect_page_blocks_json(doc: &Document, page_idx: usize) -> Vec<Value> {
    // Strategy: walk blocks and use PageBreak markers to determine page boundaries.
    let mut current_page = 0usize;
    let mut blocks = Vec::new();

    for block in &doc.content {
        if let Block::PageBreak { .. } = block {
            current_page += 1;
            continue;
        }
        if current_page == page_idx {
            blocks.push(block_to_json(block));
        }
        if current_page > page_idx {
            break;
        }
    }

    blocks
}

/// Maximum recursion depth for block_to_json, matching the document model limit.
const MAX_JSON_DEPTH: usize = 256;

/// Convert a Block to a simplified JSON representation for hook requests.
pub(crate) fn block_to_json(block: &Block) -> Value {
    block_to_json_inner(block, 0)
}

/// Depth-limited inner implementation.
fn block_to_json_inner(block: &Block, depth: usize) -> Value {
    let mut obj = serde_json::Map::new();

    match block {
        Block::Heading {
            id, level, content, ..
        } => {
            obj.insert("type".into(), Value::String("heading".into()));
            obj.insert("id".into(), Value::from(id.value()));
            obj.insert("level".into(), Value::from(*level));
            obj.insert("text".into(), Value::String(block.text()));
            let _ = content; // text() already collects inline content
        }
        Block::Paragraph { id, .. } => {
            obj.insert("type".into(), Value::String("paragraph".into()));
            obj.insert("id".into(), Value::from(id.value()));
            obj.insert("text".into(), Value::String(block.text()));
        }
        Block::Table { id, .. } => {
            obj.insert("type".into(), Value::String("table".into()));
            obj.insert("id".into(), Value::from(id.value()));
            obj.insert("text".into(), Value::String(block.text()));
        }
        Block::List { id, .. } => {
            obj.insert("type".into(), Value::String("list".into()));
            obj.insert("id".into(), Value::from(id.value()));
            obj.insert("text".into(), Value::String(block.text()));
        }
        Block::CodeBlock { id, text, .. } => {
            obj.insert("type".into(), Value::String("code_block".into()));
            obj.insert("id".into(), Value::from(id.value()));
            obj.insert("text".into(), Value::String(text.clone()));
        }
        Block::Image { id, .. } => {
            obj.insert("type".into(), Value::String("image".into()));
            obj.insert("id".into(), Value::from(id.value()));
        }
        Block::Section { id, children, .. } => {
            obj.insert("type".into(), Value::String("section".into()));
            obj.insert("id".into(), Value::from(id.value()));
            obj.insert("text".into(), Value::String(block.text()));
            if depth < MAX_JSON_DEPTH {
                let child_json: Vec<Value> = children
                    .iter()
                    .map(|b| block_to_json_inner(b, depth + 1))
                    .collect();
                obj.insert("children".into(), Value::Array(child_json));
            }
        }
        Block::Shape { id, .. } => {
            obj.insert("type".into(), Value::String("shape".into()));
            obj.insert("id".into(), Value::from(id.value()));
            obj.insert("text".into(), Value::String(block.text()));
        }
        Block::ThematicBreak { id } => {
            obj.insert("type".into(), Value::String("thematic_break".into()));
            obj.insert("id".into(), Value::from(id.value()));
        }
        Block::PageBreak { id } => {
            obj.insert("type".into(), Value::String("page_break".into()));
            obj.insert("id".into(), Value::from(id.value()));
        }
        _ => {
            // Future block variants: include type and id if possible.
            obj.insert("type".into(), Value::String("unknown".into()));
        }
    }

    Value::Object(obj)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Collect plain text for each page, using PageBreak markers as boundaries.
///
/// Builds all page strings eagerly. This is intentional: hooks iterate over
/// every page anyway, and the text cost is small relative to hook I/O.
pub(crate) fn collect_page_texts(doc: &Document) -> Vec<String> {
    let mut pages: Vec<String> = Vec::new();
    let mut current = String::new();

    for block in &doc.content {
        if let Block::PageBreak { .. } = block {
            pages.push(std::mem::take(&mut current));
            continue;
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(&block.text());
    }
    pages.push(current);

    pages
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::geometry::BoundingBox;

    #[test]
    fn collect_page_texts_single_page() {
        let mut doc = Document::new();
        doc.content.push(Block::Paragraph {
            id: NodeId::new(0),
            content: vec![Inline::Text {
                id: NodeId::new(1),
                text: "hello".into(),
                style: SpanStyle::default(),
            }],
        });
        let texts = collect_page_texts(&doc);
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0], "hello");
    }

    #[test]
    fn collect_page_texts_multi_page() {
        let mut doc = Document::new();
        doc.content.push(Block::Paragraph {
            id: NodeId::new(0),
            content: vec![Inline::Text {
                id: NodeId::new(1),
                text: "page one".into(),
                style: SpanStyle::default(),
            }],
        });
        doc.content.push(Block::PageBreak { id: NodeId::new(2) });
        doc.content.push(Block::Paragraph {
            id: NodeId::new(3),
            content: vec![Inline::Text {
                id: NodeId::new(4),
                text: "page two".into(),
                style: SpanStyle::default(),
            }],
        });
        let texts = collect_page_texts(&doc);
        assert_eq!(texts.len(), 2);
        assert_eq!(texts[0], "page one");
        assert_eq!(texts[1], "page two");
    }

    #[test]
    fn collect_page_texts_empty_doc() {
        let doc = Document::new();
        let texts = collect_page_texts(&doc);
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0], "");
    }

    #[test]
    fn block_to_json_paragraph() {
        let block = Block::Paragraph {
            id: NodeId::new(5),
            content: vec![Inline::Text {
                id: NodeId::new(6),
                text: "hello world".into(),
                style: SpanStyle::default(),
            }],
        };
        let json = block_to_json(&block);
        assert_eq!(json["type"], "paragraph");
        assert_eq!(json["id"], 5);
        assert_eq!(json["text"], "hello world");
    }

    #[test]
    fn block_to_json_heading() {
        let block = Block::Heading {
            id: NodeId::new(10),
            level: 2,
            content: vec![Inline::Text {
                id: NodeId::new(11),
                text: "Title".into(),
                style: SpanStyle::default(),
            }],
        };
        let json = block_to_json(&block);
        assert_eq!(json["type"], "heading");
        assert_eq!(json["id"], 10);
        assert_eq!(json["level"], 2);
        assert_eq!(json["text"], "Title");
    }

    #[test]
    fn build_document_request_no_images() {
        let doc = Document::new();
        let req = build_document_request(&doc, None, 5);
        assert_eq!(req["page_count"], 5);
        assert!(req.get("page_images_dir").is_none());
        assert!(req.get("format").is_none());
    }

    #[test]
    fn build_document_request_with_images_dir() {
        let doc = Document::new();
        let dir = std::path::Path::new("/tmp/pages");
        let req = build_document_request(&doc, Some(dir), 3);
        assert_eq!(req["page_count"], 3);
        assert_eq!(req["page_images_dir"], "/tmp/pages");
    }

    #[test]
    fn build_document_request_with_format_property() {
        let mut doc = Document::new();
        doc.metadata
            .properties
            .insert("udoc.format".into(), "pdf".into());
        let req = build_document_request(&doc, None, 2);
        assert_eq!(req["format"], "pdf");
    }

    #[test]
    fn positioned_span_to_json_roundtrip() {
        use crate::hooks::response::parse_response_span;
        let mut span = PositionedSpan::new(
            "Hello".into(),
            BoundingBox::new(72.0, 700.0, 120.0, 712.0),
            0,
        );
        span.font_name = Some("Helvetica".into());
        span.font_size = Some(12.0);
        span.is_bold = true;
        span.is_italic = false;
        let json = positioned_span_to_json(&span, None);
        let parsed = parse_response_span(&json, 0, None).unwrap();
        assert_eq!(parsed.text, "Hello");
        assert_eq!(parsed.font_name.as_deref(), Some("Helvetica"));
        assert!(parsed.is_bold);
    }

    #[test]
    fn positioned_span_to_json_roundtrip_with_page_height() {
        use crate::hooks::response::parse_response_span;
        let page_height = Some(792.0);
        let mut span = PositionedSpan::new(
            "Test".into(),
            BoundingBox::new(72.0, 700.0, 120.0, 712.0),
            0,
        );
        span.font_name = Some("Arial".into());
        span.font_size = Some(10.0);
        // Encode with y-up -> y-down transform
        let json = positioned_span_to_json(&span, page_height);
        // Wire format should have flipped Y: y_min = 792-712 = 80, y_max = 792-700 = 92
        let bbox_arr = json["bbox"].as_array().unwrap();
        assert!((bbox_arr[1].as_f64().unwrap() - 80.0).abs() < f64::EPSILON);
        assert!((bbox_arr[3].as_f64().unwrap() - 92.0).abs() < f64::EPSILON);
        // Decode with y-down -> y-up inverse transform
        let parsed = parse_response_span(&json, 0, page_height).unwrap();
        assert!((parsed.bbox.y_min - 700.0).abs() < f64::EPSILON);
        assert!((parsed.bbox.y_max - 712.0).abs() < f64::EPSILON);
        assert_eq!(parsed.text, "Test");
    }
}
