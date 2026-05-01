use super::*;
use crate::hooks::response::parse_document_level_response;

#[test]
fn apply_spans_builds_paragraphs_on_empty_page() {
    let mut doc = Document::new();
    doc.metadata.page_count = 1;
    let spans = vec![
        serde_json::json!({
            "text": "Hello",
            "bbox": [72.0, 700.0, 120.0, 712.0]
        }),
        serde_json::json!({
            "text": "World",
            "bbox": [72.0, 680.0, 120.0, 692.0]
        }),
    ];
    apply_spans(&mut doc, &spans, 0, None);
    // Should have created 2 paragraphs.
    assert_eq!(doc.content.len(), 2);
    assert_eq!(doc.content[0].text(), "Hello");
    assert_eq!(doc.content[1].text(), "World");
}

#[test]
fn apply_spans_does_not_add_blocks_on_existing_content() {
    let mut doc = Document::new();
    doc.metadata.page_count = 1;
    doc.content.push(Block::Paragraph {
        id: NodeId::new(0),
        content: vec![Inline::Text {
            id: NodeId::new(1),
            text: "existing".into(),
            style: SpanStyle::default(),
        }],
    });
    let spans = vec![serde_json::json!({
        "text": "OCR text",
        "bbox": [72.0, 700.0, 200.0, 712.0]
    })];
    apply_spans(&mut doc, &spans, 0, None);
    // Should NOT create new blocks (page already has content).
    assert_eq!(doc.content.len(), 1);
    assert_eq!(doc.content[0].text(), "existing");
    // But should have added to presentation raw_spans.
    let pres = doc.presentation.as_ref().unwrap();
    assert_eq!(pres.raw_spans.len(), 1);
}

#[test]
fn apply_regions_builds_blocks() {
    let mut doc = Document::new();
    doc.metadata.page_count = 1;
    let regions = vec![
        serde_json::json!({
            "label": "heading",
            "bbox": [72.0, 700.0, 540.0, 720.0],
            "text": "Introduction"
        }),
        serde_json::json!({
            "label": "paragraph",
            "bbox": [72.0, 600.0, 540.0, 700.0],
            "text": "This is the body."
        }),
    ];
    apply_regions(&mut doc, &regions, 0);
    assert_eq!(doc.content.len(), 2);
    assert!(
        matches!(&doc.content[0], Block::Heading { level, .. } if *level == 1),
        "expected Heading(level=1), got {:?}",
        doc.content[0]
    );
    assert!(
        matches!(&doc.content[1], Block::Paragraph { .. }),
        "expected Paragraph, got {:?}",
        doc.content[1]
    );
}

#[test]
fn apply_regions_table() {
    let mut doc = Document::new();
    doc.metadata.page_count = 1;
    let regions = vec![serde_json::json!({
        "label": "table",
        "bbox": [72.0, 400.0, 540.0, 600.0],
        "table": {
            "rows": [
                [{"text": "A", "col_span": 1, "row_span": 1}, {"text": "B", "col_span": 1, "row_span": 1}],
                [{"text": "1", "col_span": 1, "row_span": 1}, {"text": "2", "col_span": 1, "row_span": 1}]
            ],
            "header_rows": 1
        }
    })];
    apply_regions(&mut doc, &regions, 0);
    assert_eq!(doc.content.len(), 1);
    let Block::Table { table, .. } = &doc.content[0] else {
        unreachable!("expected Table, got {:?}", doc.content[0]);
    };
    assert_eq!(table.rows.len(), 2);
    assert!(table.rows[0].is_header);
    assert!(!table.rows[1].is_header);
    assert_eq!(table.rows[0].cells[0].text(), "A");
    assert_eq!(table.rows[1].cells[1].text(), "2");
}

#[test]
fn apply_blocks_new_block() {
    let mut doc = Document::new();
    doc.metadata.page_count = 1;
    let blocks = vec![serde_json::json!({
        "type": "paragraph",
        "id": null,
        "text": "New paragraph from hook"
    })];
    apply_blocks(&mut doc, &blocks, 0);
    assert_eq!(doc.content.len(), 1);
    assert_eq!(doc.content[0].text(), "New paragraph from hook");
}

#[test]
fn apply_blocks_replace_existing() {
    let mut doc = Document::new();
    doc.metadata.page_count = 1;
    doc.content.push(Block::Paragraph {
        id: NodeId::new(5),
        content: vec![Inline::Text {
            id: NodeId::new(6),
            text: "original".into(),
            style: SpanStyle::default(),
        }],
    });
    let blocks = vec![serde_json::json!({
        "type": "paragraph",
        "id": 5,
        "text": "replaced"
    })];
    apply_blocks(&mut doc, &blocks, 0);
    assert_eq!(doc.content.len(), 1);
    assert_eq!(doc.content[0].text(), "replaced");
    assert_eq!(doc.content[0].id(), NodeId::new(5));
}

// -----------------------------------------------------------------------
// Response JSON parsing (moved from request.rs)
// -----------------------------------------------------------------------

#[test]
fn has_content_for_page_yes() {
    let mut doc = Document::new();
    doc.content.push(Block::Paragraph {
        id: NodeId::new(0),
        content: vec![],
    });
    assert!(has_content_for_page(&doc, 0));
}

#[test]
fn has_content_for_page_no() {
    let mut doc = Document::new();
    doc.content.push(Block::PageBreak { id: NodeId::new(0) });
    assert!(!has_content_for_page(&doc, 0));
    assert!(!has_content_for_page(&doc, 1));
}

#[test]
fn find_insert_position_single_page() {
    let content = vec![
        Block::Paragraph {
            id: NodeId::new(0),
            content: vec![],
        },
        Block::Paragraph {
            id: NodeId::new(1),
            content: vec![],
        },
    ];
    // Page 0: insert at end.
    assert_eq!(find_page_insert_position(&content, 0), 2);
}

#[test]
fn find_insert_position_multi_page() {
    let content = vec![
        Block::Paragraph {
            id: NodeId::new(0),
            content: vec![],
        },
        Block::PageBreak { id: NodeId::new(1) },
        Block::Paragraph {
            id: NodeId::new(2),
            content: vec![],
        },
    ];
    // Page 0: insert before the PageBreak.
    assert_eq!(find_page_insert_position(&content, 0), 1);
    // Page 1: insert at end.
    assert_eq!(find_page_insert_position(&content, 1), 3);
}

#[test]
fn parse_bbox_valid() {
    let v = serde_json::json!([72.0, 700.0, 540.0, 720.0]);
    let bbox = parse_bbox(Some(&v));
    assert!(bbox.is_some());
    let bbox = bbox.unwrap();
    assert!((bbox.x_min - 72.0).abs() < f64::EPSILON);
    assert!((bbox.y_max - 720.0).abs() < f64::EPSILON);
}

#[test]
fn parse_bbox_too_short() {
    let v = serde_json::json!([72.0, 700.0]);
    assert!(parse_bbox(Some(&v)).is_none());
}

#[test]
fn parse_bbox_none() {
    assert!(parse_bbox(None).is_none());
}

#[test]
fn parse_response_span_valid() {
    let v = serde_json::json!({
        "text": "Hello",
        "bbox": [72.0, 700.0, 120.0, 712.0],
        "confidence": 0.95,
        "font_name": "Helvetica",
        "font_size": 12.0,
        "is_bold": true,
        "is_italic": false
    });
    let span = parse_response_span(&v, 0, None);
    assert!(span.is_some());
    let span = span.unwrap();
    assert_eq!(span.text, "Hello");
    assert_eq!(span.page_index, 0);
    assert_eq!(span.font_name.as_deref(), Some("Helvetica"));
    assert_eq!(span.font_size, Some(12.0));
    assert!(span.is_bold);
    assert!(!span.is_italic);
}

#[test]
fn parse_response_span_minimal() {
    let v = serde_json::json!({
        "text": "Hi",
        "bbox": [0.0, 0.0, 10.0, 10.0]
    });
    let span = parse_response_span(&v, 3, None);
    assert!(span.is_some());
    let span = span.unwrap();
    assert_eq!(span.text, "Hi");
    assert_eq!(span.page_index, 3);
    assert!(!span.is_bold);
    assert!(!span.is_italic);
}

#[test]
fn parse_response_span_missing_text() {
    let v = serde_json::json!({
        "bbox": [0.0, 0.0, 10.0, 10.0]
    });
    assert!(parse_response_span(&v, 0, None).is_none());
}

#[test]
fn parse_response_span_no_bbox() {
    // bbox is absent: span should still parse, using a zero bounding box.
    let v = serde_json::json!({
        "text": "no position",
        "font_name": "Arial",
        "is_bold": false,
        "is_italic": true
    });
    let span = parse_response_span(&v, 2, None);
    assert!(span.is_some(), "expected Some, got None");
    let span = span.unwrap();
    assert_eq!(span.text, "no position");
    assert_eq!(span.page_index, 2);
    assert_eq!(span.font_name.as_deref(), Some("Arial"));
    assert!(!span.is_bold);
    assert!(span.is_italic);
    // Zero bounding box.
    assert_eq!(span.bbox.x_min, 0.0);
    assert_eq!(span.bbox.y_min, 0.0);
    assert_eq!(span.bbox.x_max, 0.0);
    assert_eq!(span.bbox.y_max, 0.0);
}

#[test]
fn parse_response_span_bbox_too_short_still_fails() {
    // bbox present but too short is still an error (malformed, not missing).
    let v = serde_json::json!({
        "text": "partial",
        "bbox": [1.0, 2.0]
    });
    assert!(parse_response_span(&v, 0, None).is_none());
}

#[test]
fn full_response_apply() {
    let mut doc = Document::new();
    doc.metadata.page_count = 1;
    let response = serde_json::json!({
        "page_index": 0,
        "spans": [
            {"text": "Hello", "bbox": [72.0, 700.0, 120.0, 712.0]}
        ],
        "labels": {
            "document_type": "report"
        }
    });
    apply_response(&mut doc, &response, 0);
    // Spans should create a paragraph (no existing content).
    assert_eq!(doc.content.len(), 1);
    assert_eq!(doc.content[0].text(), "Hello");
    // Labels should be in metadata (namespaced with hook.label. prefix).
    assert_eq!(
        doc.metadata.properties.get("hook.label.document_type"),
        Some(&"report".to_string())
    );
}

// ---------------------------------------------------------------------------
// Document-level response parsing (Need::Document)
// ---------------------------------------------------------------------------

#[test]
fn parse_document_level_response_applies_per_page_spans() {
    let mut doc = Document::new();
    doc.metadata.page_count = 2;
    // Two-page document with a PageBreak separating them.
    doc.content.push(Block::PageBreak { id: NodeId::new(0) });

    let response = serde_json::json!({
        "pages": [
            {
                "page_index": 0,
                "spans": [
                    {"text": "Page one text", "bbox": [72.0, 700.0, 300.0, 712.0]}
                ]
            },
            {
                "page_index": 1,
                "spans": [
                    {"text": "Page two text", "bbox": [72.0, 700.0, 300.0, 712.0]}
                ]
            }
        ]
    });

    parse_document_level_response(&mut doc, &response);

    // Each page should have one paragraph inserted.
    let texts: Vec<String> = doc
        .content
        .iter()
        .filter(|b| !matches!(b, Block::PageBreak { .. }))
        .map(|b| b.text())
        .collect();
    assert_eq!(texts.len(), 2, "expected 2 paragraphs, got {:?}", texts);
    assert!(texts.iter().any(|t| t == "Page one text"));
    assert!(texts.iter().any(|t| t == "Page two text"));
}

#[test]
fn parse_document_level_response_missing_pages_key() {
    // Missing "pages" key: should silently do nothing (logs to stderr).
    let mut doc = Document::new();
    doc.metadata.page_count = 1;
    let response = serde_json::json!({"status": "ok"});
    parse_document_level_response(&mut doc, &response);
    assert!(doc.content.is_empty());
}

#[test]
fn parse_document_level_response_entry_missing_page_index() {
    // Page entry without page_index is skipped.
    let mut doc = Document::new();
    doc.metadata.page_count = 1;
    let response = serde_json::json!({
        "pages": [
            {
                // No page_index key
                "spans": [{"text": "Ghost", "bbox": [0.0, 0.0, 10.0, 10.0]}]
            },
            {
                "page_index": 0,
                "spans": [{"text": "Real", "bbox": [0.0, 0.0, 10.0, 10.0]}]
            }
        ]
    });
    parse_document_level_response(&mut doc, &response);
    // Only the entry with page_index=0 should have been applied.
    assert_eq!(doc.content.len(), 1);
    assert_eq!(doc.content[0].text(), "Real");
}

#[test]
fn parse_document_level_response_empty_pages() {
    let mut doc = Document::new();
    doc.metadata.page_count = 1;
    let response = serde_json::json!({"pages": []});
    parse_document_level_response(&mut doc, &response);
    assert!(doc.content.is_empty());
}
