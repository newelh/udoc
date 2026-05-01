//! Hook response application: document mutation from hook output.
//!
//! Applies parsed JSON responses to the document model: spans, regions,
//! blocks, overlays, entities, labels.

use serde_json::Value;

use udoc_core::document::*;
use udoc_core::geometry::BoundingBox;

use super::protocol::{apply_entities, apply_labels, apply_overlays};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of spans, blocks, or regions a hook may return per page.
/// Prevents memory exhaustion and O(n^2) Vec insertion from a malicious hook.
pub(crate) const MAX_ITEMS_PER_PAGE: usize = 10_000;

// ---------------------------------------------------------------------------
// Response application (T-023: Document mutation)
// ---------------------------------------------------------------------------

/// Try to allocate a NodeId from the document. Returns None on exhaustion.
fn try_alloc(doc: &Document) -> Option<NodeId> {
    doc.try_alloc_node_id()
}

/// Apply a hook response to the document.
pub fn apply_response(doc: &mut Document, response: &Value, page_idx: usize) {
    // Extract page height for coordinate transforms (y-down -> y-up).
    let page_height = doc
        .presentation
        .as_ref()
        .and_then(|p| p.pages.get(page_idx))
        .map(|pg| pg.height);

    // spans -> PositionedSpan in presentation layer, build paragraphs if needed.
    if let Some(spans) = response.get("spans").and_then(|v| v.as_array()) {
        apply_spans(doc, spans, page_idx, page_height);
    }

    // regions -> Build Block tree from region labels.
    if let Some(regions) = response.get("regions").and_then(|v| v.as_array()) {
        apply_regions(doc, regions, page_idx);
    }

    // blocks -> Replace by id or append new blocks.
    if let Some(blocks) = response.get("blocks").and_then(|v| v.as_array()) {
        apply_blocks(doc, blocks, page_idx);
    }

    // overlays -> Store as named properties in metadata (simplified for v1).
    if let Some(overlays) = response.get("overlays").and_then(|v| v.as_object()) {
        apply_overlays(doc, overlays);
    }

    // entities -> Store in metadata properties (simplified for v1).
    if let Some(entities) = response.get("entities").and_then(|v| v.as_array()) {
        apply_entities(doc, entities, page_idx);
    }

    // labels -> Merge into DocumentMetadata.properties.
    if let Some(labels) = response.get("labels").and_then(|v| v.as_object()) {
        apply_labels(doc, labels);
    }
}

/// Apply span data from a hook response.
/// Adds PositionedSpans to the presentation layer and builds Paragraph
/// blocks if there is no existing content for this page.
fn apply_spans(doc: &mut Document, spans: &[Value], page_idx: usize, page_height: Option<f64>) {
    if spans.is_empty() {
        return;
    }

    let capped = if spans.len() > MAX_ITEMS_PER_PAGE {
        eprintln!(
            "hook: page {}: {} spans exceeds limit of {}, truncating",
            page_idx,
            spans.len(),
            MAX_ITEMS_PER_PAGE
        );
        &spans[..MAX_ITEMS_PER_PAGE]
    } else {
        spans
    };

    // Build PositionedSpans and add to presentation layer.
    let positioned_spans: Vec<PositionedSpan> = capped
        .iter()
        .filter_map(|v| parse_response_span(v, page_idx, page_height))
        .collect();

    // Add to presentation layer.
    let pres = doc.presentation.get_or_insert_with(Presentation::default);
    for ps in &positioned_spans {
        pres.raw_spans.push(ps.clone());
    }

    // Check if this page already has content blocks.
    let page_has_content = has_content_for_page(doc, page_idx);

    if !page_has_content {
        // Build Paragraph blocks from spans (OCR case).
        // Group spans into lines by proximity (simplified: one paragraph per span).
        let insert_pos = find_page_insert_position(&doc.content, page_idx);

        for (offset, ps) in positioned_spans.iter().enumerate() {
            let inline_id = match try_alloc(doc) {
                Some(id) => id,
                None => break,
            };
            let block_id = match try_alloc(doc) {
                Some(id) => id,
                None => break,
            };

            let mut style = SpanStyle::default();
            style.bold = ps.is_bold;
            style.italic = ps.is_italic;

            let inline = Inline::Text {
                id: inline_id,
                text: ps.text.clone(),
                style,
            };

            let block = Block::Paragraph {
                id: block_id,
                content: vec![inline],
            };

            // Set page assignment if we have a presentation layer.
            if let Some(ref mut pres) = doc.presentation {
                let _ = pres.page_assignments.try_set(block_id, page_idx);
                let _ = pres.page_assignments.try_set(inline_id, page_idx);
                let _ = pres.geometry.try_set(inline_id, ps.bbox);
            }

            doc.content.insert(insert_pos + offset, block);
        }
    }
}

/// Apply region data from a hook response.
/// Builds Block tree from region labels.
fn apply_regions(doc: &mut Document, regions: &[Value], page_idx: usize) {
    if regions.len() > MAX_ITEMS_PER_PAGE {
        eprintln!(
            "hook: page {}: {} regions exceeds limit of {}, truncating",
            page_idx,
            regions.len(),
            MAX_ITEMS_PER_PAGE
        );
    }

    let insert_pos = find_page_insert_position(&doc.content, page_idx);
    let mut offset = 0;

    for region in regions.iter().take(MAX_ITEMS_PER_PAGE) {
        let label = match region.get("label").and_then(|v| v.as_str()) {
            Some(l) => l,
            None => continue,
        };

        let block = match label {
            "heading" => {
                let block_id = match try_alloc(doc) {
                    Some(id) => id,
                    None => continue,
                };
                let inline_id = match try_alloc(doc) {
                    Some(id) => id,
                    None => continue,
                };
                let text = extract_region_text(region);
                Block::Heading {
                    id: block_id,
                    level: 1, // Default level; hooks could provide this.
                    content: vec![Inline::Text {
                        id: inline_id,
                        text,
                        style: SpanStyle::default(),
                    }],
                }
            }
            "table" => {
                if let Some(table_data) = parse_region_table(region, doc) {
                    let block_id = match try_alloc(doc) {
                        Some(id) => id,
                        None => continue,
                    };
                    Block::Table {
                        id: block_id,
                        table: table_data,
                    }
                } else {
                    continue;
                }
            }
            "figure" | "image" => {
                // Figures from layout hooks: no actual image data, so create a
                // paragraph placeholder with region text (or a description).
                let block_id = match try_alloc(doc) {
                    Some(id) => id,
                    None => continue,
                };
                let inline_id = match try_alloc(doc) {
                    Some(id) => id,
                    None => continue,
                };
                let text = extract_region_text(region);
                let text = if text.is_empty() {
                    format!("[figure on page {}]", page_idx)
                } else {
                    text
                };
                Block::Paragraph {
                    id: block_id,
                    content: vec![Inline::Text {
                        id: inline_id,
                        text,
                        style: SpanStyle::default(),
                    }],
                }
            }
            _ => {
                // paragraph, caption, footer, header, page_number, list, etc.
                let block_id = match try_alloc(doc) {
                    Some(id) => id,
                    None => continue,
                };
                let inline_id = match try_alloc(doc) {
                    Some(id) => id,
                    None => continue,
                };
                let text = extract_region_text(region);
                Block::Paragraph {
                    id: block_id,
                    content: vec![Inline::Text {
                        id: inline_id,
                        text,
                        style: SpanStyle::default(),
                    }],
                }
            }
        };

        // Set page assignment and geometry.
        if let Some(ref mut pres) = doc.presentation {
            let _ = pres.page_assignments.try_set(block.id(), page_idx);
            if let Some(bbox) = parse_bbox(region.get("bbox")) {
                let _ = pres.geometry.try_set(block.id(), bbox);
            }
        }

        doc.content.insert(insert_pos + offset, block);
        offset += 1;
    }
}

/// Extract text from a region, using span_indices + spans or a direct text field.
fn extract_region_text(region: &Value) -> String {
    // Direct text field (simplest case).
    if let Some(text) = region.get("text").and_then(|v| v.as_str()) {
        return text.to_string();
    }
    // No text available from this region.
    String::new()
}

/// Parse a table structure from a region's "table" field.
fn parse_region_table(region: &Value, doc: &Document) -> Option<TableData> {
    let table_obj = region.get("table")?;
    let rows_arr = table_obj.get("rows")?.as_array()?;
    let header_rows = table_obj
        .get("header_rows")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    let mut rows = Vec::new();
    for (row_idx, row_arr) in rows_arr.iter().enumerate() {
        let cells_arr = row_arr.as_array()?;
        let mut cells = Vec::new();
        for cell_val in cells_arr {
            let text = cell_val
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let col_span = cell_val
                .get("col_span")
                .and_then(|v| v.as_u64())
                .unwrap_or(1) as usize;
            let row_span = cell_val
                .get("row_span")
                .and_then(|v| v.as_u64())
                .unwrap_or(1) as usize;

            let cell_id = try_alloc(doc)?;
            let para_id = try_alloc(doc)?;
            let text_id = try_alloc(doc)?;

            let content = vec![Block::Paragraph {
                id: para_id,
                content: vec![Inline::Text {
                    id: text_id,
                    text,
                    style: SpanStyle::default(),
                }],
            }];

            let mut tc = TableCell::new(cell_id, content);
            tc.col_span = col_span;
            tc.row_span = row_span;
            cells.push(tc);
        }

        let row_id = try_alloc(doc)?;
        let mut tr = TableRow::new(row_id, cells);
        tr.is_header = row_idx < header_rows;
        rows.push(tr);
    }

    Some(TableData::new(rows))
}

/// Apply block data from a hook response.
/// Blocks with existing id replace the corresponding block.
/// Blocks with id=null get new NodeIds and are appended.
fn apply_blocks(doc: &mut Document, blocks: &[Value], page_idx: usize) {
    if blocks.len() > MAX_ITEMS_PER_PAGE {
        eprintln!(
            "hook: page {}: {} blocks exceeds limit of {}, truncating",
            page_idx,
            blocks.len(),
            MAX_ITEMS_PER_PAGE
        );
    }

    // Compute insert position once; track offset so new blocks maintain order.
    let insert_pos = find_page_insert_position(&doc.content, page_idx);
    let mut new_block_offset = 0;

    for block_val in blocks.iter().take(MAX_ITEMS_PER_PAGE) {
        let block_type = match block_val.get("type").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => continue,
        };

        let existing_id = block_val
            .get("id")
            .and_then(|v| v.as_u64())
            .map(NodeId::new);

        let text = block_val
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if let Some(id) = existing_id {
            // Replace existing block by id.
            replace_block_by_id(doc, id, block_type, &text, block_val);
        } else {
            // New block: allocate id, append after existing page content.
            let block = build_block_from_json(doc, block_type, &text, block_val);
            if let Some(block) = block {
                if let Some(ref mut pres) = doc.presentation {
                    let _ = pres.page_assignments.try_set(block.id(), page_idx);
                }
                doc.content.insert(insert_pos + new_block_offset, block);
                new_block_offset += 1;
            }
        }
    }
}

/// Replace a block in the document by its NodeId.
fn replace_block_by_id(
    doc: &mut Document,
    target_id: NodeId,
    block_type: &str,
    text: &str,
    block_val: &Value,
) {
    // Check if the target block exists before allocating a NodeId.
    let target_exists = {
        let mut found = false;
        doc.walk(&mut |block| {
            if block.id() == target_id {
                found = true;
            }
        });
        found
    };

    if !target_exists {
        return;
    }

    let inline_id = match try_alloc(doc) {
        Some(id) => id,
        None => return,
    };
    let level = clamp_heading_level(block_val);
    let text = text.to_string();
    let block_type = block_type.to_string();

    doc.walk_mut(&mut |block| {
        if block.id() == target_id {
            let replacement = match block_type.as_str() {
                "heading" => Some(Block::Heading {
                    id: target_id,
                    level,
                    content: vec![Inline::Text {
                        id: inline_id,
                        text: text.clone(),
                        style: SpanStyle::default(),
                    }],
                }),
                "paragraph" => Some(Block::Paragraph {
                    id: target_id,
                    content: vec![Inline::Text {
                        id: inline_id,
                        text: text.clone(),
                        style: SpanStyle::default(),
                    }],
                }),
                _ => None,
            };
            if let Some(r) = replacement {
                *block = r;
            }
        }
    });
}

/// Clamp a heading level from hook JSON to the valid range 1-6.
fn clamp_heading_level(block_val: &Value) -> u8 {
    let raw = block_val.get("level").and_then(|v| v.as_u64()).unwrap_or(1);
    raw.clamp(1, 6) as u8
}

/// Build a new Block from JSON hook output.
fn build_block_from_json(
    doc: &Document,
    block_type: &str,
    text: &str,
    block_val: &Value,
) -> Option<Block> {
    let block_id = try_alloc(doc)?;
    let inline_id = try_alloc(doc)?;

    match block_type {
        "heading" => {
            let level = clamp_heading_level(block_val);
            Some(Block::Heading {
                id: block_id,
                level,
                content: vec![Inline::Text {
                    id: inline_id,
                    text: text.to_string(),
                    style: SpanStyle::default(),
                }],
            })
        }
        "paragraph" => Some(Block::Paragraph {
            id: block_id,
            content: vec![Inline::Text {
                id: inline_id,
                text: text.to_string(),
                style: SpanStyle::default(),
            }],
        }),
        "code_block" => {
            let language = block_val
                .get("language")
                .and_then(|v| v.as_str())
                .map(String::from);
            Some(Block::CodeBlock {
                id: block_id,
                text: text.to_string(),
                language,
            })
        }
        _ => {
            // Default to paragraph for unknown types.
            Some(Block::Paragraph {
                id: block_id,
                content: vec![Inline::Text {
                    id: inline_id,
                    text: text.to_string(),
                    style: SpanStyle::default(),
                }],
            })
        }
    }
}

// Metadata mutation functions (apply_overlays, apply_entities, apply_labels)
// live in protocol.rs alongside the key validation they depend on.

// ---------------------------------------------------------------------------
// Response JSON parsing (JSON -> domain types)
// ---------------------------------------------------------------------------

/// Check if a page already has content blocks (non-PageBreak).
pub(crate) fn has_content_for_page(doc: &Document, page_idx: usize) -> bool {
    let mut current_page = 0usize;
    for block in &doc.content {
        if let Block::PageBreak { .. } = block {
            current_page += 1;
            continue;
        }
        if current_page == page_idx {
            return true;
        }
        if current_page > page_idx {
            break;
        }
    }
    false
}

/// Find the insertion position for new blocks on a given page.
/// Returns the index just before the next PageBreak (or end of content).
pub(crate) fn find_page_insert_position(content: &[Block], page_idx: usize) -> usize {
    let mut current_page = 0usize;
    for (i, block) in content.iter().enumerate() {
        if let Block::PageBreak { .. } = block {
            if current_page == page_idx {
                return i;
            }
            current_page += 1;
        }
    }
    // If we reach the end, insert at the end.
    content.len()
}

/// Parse a span from a hook response JSON value.
/// When `page_height` is provided, bbox Y coordinates are transformed from
/// hook wire format (y-down) back to PDF-native (y-up).
///
/// The `"bbox"` key is optional. Spans without a bbox get a zero bounding box
/// (x_min=y_min=x_max=y_max=0). This handles hooks that emit positional text
/// without coordinate data (e.g., cloud OCR returning text-only results).
pub(crate) fn parse_response_span(
    v: &Value,
    page_idx: usize,
    page_height: Option<f64>,
) -> Option<PositionedSpan> {
    let text = v.get("text")?.as_str()?.to_string();

    let bbox = if let Some(bbox_val) = v.get("bbox") {
        let bbox_arr = bbox_val.as_array()?;
        if bbox_arr.len() < 4 {
            return None;
        }
        let x_min = bbox_arr[0].as_f64()?;
        let y_min_wire = bbox_arr[1].as_f64()?;
        let x_max = bbox_arr[2].as_f64()?;
        let y_max_wire = bbox_arr[3].as_f64()?;

        let (y_min, y_max) = if let Some(h) = page_height {
            (h - y_max_wire, h - y_min_wire)
        } else {
            (y_min_wire, y_max_wire)
        };
        BoundingBox::new(x_min, y_min, x_max, y_max)
    } else {
        // No bbox provided; use a zero box so the span is still usable as text.
        BoundingBox::new(0.0, 0.0, 0.0, 0.0)
    };

    let mut ps = PositionedSpan::new(text, bbox, page_idx);

    ps.font_name = v
        .get("font_name")
        .and_then(|v| v.as_str())
        .map(String::from);
    ps.font_size = v.get("font_size").and_then(|v| v.as_f64());
    ps.is_bold = v.get("is_bold").and_then(|v| v.as_bool()).unwrap_or(false);
    ps.is_italic = v
        .get("is_italic")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    Some(ps)
}

/// Parse a document-level hook response and apply all per-page data.
///
/// Expects `{"pages": [{"page_index": N, ...}, ...]}`.
/// Each page entry is processed exactly as a regular per-page response.
/// Unknown keys in each page entry are silently ignored.
pub fn parse_document_level_response(doc: &mut Document, response: &Value) {
    let pages = match response.get("pages").and_then(|v| v.as_array()) {
        Some(p) => p,
        None => {
            eprintln!("hook: document-level response missing 'pages' array, ignoring");
            return;
        }
    };

    // Count pages in the document (number of PageBreaks + 1).
    let doc_page_count = doc
        .content
        .iter()
        .filter(|b| matches!(b, udoc_core::document::Block::PageBreak { .. }))
        .count()
        + 1;

    for page_entry in pages {
        let page_idx = match page_entry.get("page_index").and_then(|v| v.as_u64()) {
            Some(i) => i as usize,
            None => {
                eprintln!(
                    "hook: document-level response page entry missing 'page_index', skipping"
                );
                continue;
            }
        };
        if page_idx >= doc_page_count {
            eprintln!(
                "hook: document-level response page_index {} out of bounds (doc has {} pages), skipping",
                page_idx, doc_page_count
            );
            continue;
        }
        apply_response(doc, page_entry, page_idx);
    }
}

/// Parse a bbox from a JSON value (array of 4 floats).
pub(crate) fn parse_bbox(v: Option<&Value>) -> Option<BoundingBox> {
    let arr = v?.as_array()?;
    if arr.len() < 4 {
        return None;
    }
    let x_min = arr[0].as_f64()?;
    let y_min = arr[1].as_f64()?;
    let x_max = arr[2].as_f64()?;
    let y_max = arr[3].as_f64()?;
    Some(BoundingBox::new(x_min, y_min, x_max, y_max))
}

#[cfg(test)]
#[path = "response_tests.rs"]
mod tests;
