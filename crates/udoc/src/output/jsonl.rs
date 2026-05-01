//! JSONL streaming output (--jsonl).
//!
//! Emits one JSON object per line:
//! 1. Header line: `{"udoc":"header", "version":1, "format":"PDF", "metadata":{...}}`
//! 2. One block line per top-level content block
//! 3. Footer line: `{"udoc":"footer", "blocks":N, "warnings":0}`

use std::io::Write;

use serde_json::json;
use udoc_core::document::{Block, Document, Overlay};

/// Write a Document as JSONL (one JSON object per line).
///
/// `format_name` is the human-readable format string (e.g. "PDF").
/// `page_assignments` maps block NodeIds to page indices.
/// `warning_count` is the number of diagnostics warnings emitted during extraction.
pub fn write_jsonl(
    doc: &Document,
    format_name: &str,
    writer: &mut dyn Write,
    page_assignments: Option<&Overlay<usize>>,
    warning_count: usize,
) -> std::io::Result<()> {
    // Header line
    let header = json!({
        "udoc": "header",
        "version": 1,
        "format": format_name,
        "metadata": &doc.metadata,
    });
    serde_json::to_writer(&mut *writer, &header).map_err(std::io::Error::other)?;
    writeln!(writer)?;

    // Block lines (top-level content only)
    let mut block_count = 0u64;
    for block in &doc.content {
        write_block_jsonl(block, writer, page_assignments, &mut block_count)?;
    }

    // Footer line
    let footer = json!({
        "udoc": "footer",
        "blocks": block_count,
        "warnings": warning_count,
    });
    serde_json::to_writer(&mut *writer, &footer).map_err(std::io::Error::other)?;
    writeln!(writer)?;

    Ok(())
}

fn write_block_jsonl(
    block: &Block,
    writer: &mut dyn Write,
    page_assignments: Option<&Overlay<usize>>,
    count: &mut u64,
) -> std::io::Result<()> {
    let mut value = serde_json::to_value(block).map_err(std::io::Error::other)?;

    // Add "udoc": "block" discriminator and optional page field
    if let serde_json::Value::Object(ref mut map) = value {
        map.insert("udoc".to_string(), json!("block"));

        if let Some(pa) = page_assignments {
            if let Some(page) = pa.get(block.id()) {
                map.insert("page".to_string(), json!(page));
            }
        }
    }

    serde_json::to_writer(&mut *writer, &value).map_err(std::io::Error::other)?;
    writeln!(writer)?;
    *count += 1;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::document::{Block, Inline, NodeId, Overlay, SpanStyle};

    fn make_test_doc() -> Document {
        let mut doc = Document::new();
        doc.content.push(Block::Paragraph {
            id: NodeId::new(0),
            content: vec![Inline::Text {
                id: NodeId::new(1),
                text: "First paragraph".into(),
                style: SpanStyle::default(),
            }],
        });
        doc.content.push(Block::Paragraph {
            id: NodeId::new(2),
            content: vec![Inline::Text {
                id: NodeId::new(3),
                text: "Second paragraph".into(),
                style: SpanStyle::default(),
            }],
        });
        doc.metadata.title = Some("Test Doc".into());
        doc.metadata.page_count = 1;
        doc
    }

    #[test]
    fn jsonl_structure() {
        let doc = make_test_doc();
        let mut buf = Vec::new();
        write_jsonl(&doc, "PDF", &mut buf, None, 0).unwrap();
        let s = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = s.lines().collect();

        // header + 2 blocks + footer = 4 lines
        assert_eq!(lines.len(), 4);

        // Header
        let header: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(header["udoc"], "header");
        assert_eq!(header["version"], 1);
        assert_eq!(header["format"], "PDF");

        // Blocks
        let block1: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(block1["udoc"], "block");
        assert_eq!(block1["type"], "paragraph");

        let block2: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(block2["udoc"], "block");

        // Footer
        let footer: serde_json::Value = serde_json::from_str(lines[3]).unwrap();
        assert_eq!(footer["udoc"], "footer");
        assert_eq!(footer["blocks"], 2);
    }

    #[test]
    fn jsonl_with_page_assignments() {
        let doc = make_test_doc();
        let mut pa: Overlay<usize> = Overlay::new();
        pa.set(NodeId::new(0), 0);
        pa.set(NodeId::new(2), 1);

        let mut buf = Vec::new();
        write_jsonl(&doc, "PDF", &mut buf, Some(&pa), 0).unwrap();
        let s = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = s.lines().collect();

        let block1: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(block1["page"], 0);

        let block2: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(block2["page"], 1);
    }

    #[test]
    fn jsonl_empty_doc() {
        let doc = Document::new();
        let mut buf = Vec::new();
        write_jsonl(&doc, "PDF", &mut buf, None, 0).unwrap();
        let s = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = s.lines().collect();
        // header + footer only
        assert_eq!(lines.len(), 2);

        let footer: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(footer["blocks"], 0);
    }
}
