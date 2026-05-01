//! Chunked NDJSON output for LLM ingest pipelines.
//!
//! Emits one JSON record per chunk on its own line, flushed as it is
//! produced so streaming consumers (Python `for line in p.stdout:` etc.)
//! see results without buffering the whole document.
//!
//! Four strategies (`--chunk-by`):
//!
//! - [`ChunkBy::Page`] -- one chunk per `Block::PageBreak`-bounded run
//!   (or per page if `presentation.page_assignments` is populated).
//!   Citation is `{"page": N}`.
//! - [`ChunkBy::Heading`] -- chunk boundary at every `Block::Heading`
//!   (any rank). Empty-body chunks are dropped. Citation carries
//!   `{"heading_path": [...], "page_range": [N, M]}`.
//! - [`ChunkBy::Section`] -- only top-level (rank 1) heading boundaries.
//!   Same citation shape.
//! - [`ChunkBy::Size`] -- target N chars (`--chunk-size`, default 2000).
//!   Breaks at paragraph boundaries; never splits a paragraph. If a
//!   single paragraph exceeds the budget, it is emitted as-is with
//!   `oversize: true` in metadata. Citation `{"page_range": [N, M]}`.
//!
//! Body shape: plain text derived from each block's `Block::text()`.
//! NOT the full Block tree -- callers wanting fidelity reach for
//! `--out json`. The chunks output is sized for retrieval/embedding.
//!
//! Per  §5.3 + Domain Expert spec.

use std::io::Write;

use serde::Serialize;

use udoc_core::document::{Block, Document, ListItem, NodeId};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Chunking strategy for [`emit_chunks`]. Mirrors the CLI `--chunk-by`
/// flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkBy {
    /// One chunk per page-bounded run.
    Page,
    /// Chunk at every Block::Heading boundary (any rank).
    Heading,
    /// Chunk at top-level (rank 1) heading boundaries only.
    Section,
    /// Target N chars per chunk; breaks at paragraph boundaries.
    Size,
}

/// Configuration for [`emit_chunks`].
#[derive(Debug, Clone)]
pub struct ChunkOptions {
    /// Strategy.
    pub strategy: ChunkBy,
    /// Target chars per chunk under [`ChunkBy::Size`]. Default 2000.
    pub size: usize,
}

impl Default for ChunkOptions {
    fn default() -> Self {
        Self {
            strategy: ChunkBy::Page,
            size: 2000,
        }
    }
}

/// One emitted chunk. Serialized one-per-line as NDJSON.
#[derive(Debug, Serialize)]
pub struct Chunk {
    /// Plain-text body (derived from Block::text()).
    pub text: String,
    /// Citation payload. Shape varies by strategy:
    /// - `Page`: `{"page": N}`
    /// - `Heading` / `Section`: `{"heading_path": [...], "page_range": [N, M]}`
    /// - `Size`: `{"page_range": [N, M]}`
    pub citation: serde_json::Value,
    /// Per-chunk metadata (strategy + sequence + oversize flag).
    pub metadata: ChunkMetadata,
}

/// Metadata block carried with every chunk.
#[derive(Debug, Serialize)]
pub struct ChunkMetadata {
    /// Strategy name: "page" | "heading" | "section" | "size".
    pub strategy: &'static str,
    /// Sequential chunk index, 0-based.
    pub chunk_index: usize,
    /// True when a single paragraph exceeded the size budget under the
    /// `Size` strategy. False otherwise (always false for non-Size
    /// strategies).
    pub oversize: bool,
}

// ---------------------------------------------------------------------------
// Top-level entry point
// ---------------------------------------------------------------------------

/// Emit chunks for `doc` to `out` as NDJSON, flushing each line as it
/// is produced.
///
/// Returns I/O errors verbatim; chunk-shape errors do not exist (every
/// strategy degrades to "no chunks emitted" rather than failing).
pub fn emit_chunks<W: Write>(
    doc: &Document,
    opts: &ChunkOptions,
    out: &mut W,
) -> std::io::Result<()> {
    let chunks = match opts.strategy {
        ChunkBy::Page => chunks_by_page(doc),
        ChunkBy::Heading => chunks_by_heading(doc, /*top_level_only=*/ false),
        ChunkBy::Section => chunks_by_heading(doc, /*top_level_only=*/ true),
        ChunkBy::Size => chunks_by_size(doc, opts.size),
    };
    for chunk in chunks {
        let line = serde_json::to_string(&chunk).map_err(std::io::Error::other)?;
        out.write_all(line.as_bytes())?;
        out.write_all(b"\n")?;
        out.flush()?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Strategy: page
// ---------------------------------------------------------------------------

fn chunks_by_page(doc: &Document) -> Vec<Chunk> {
    let mut buckets: Vec<(Option<usize>, String)> = Vec::new();
    let mut current_page: Option<usize> = None;
    let mut current_text = String::new();

    for block in &doc.content {
        // Page boundary signals: explicit PageBreak block OR a change in
        // the presentation overlay's page_assignments value at this block's
        // root NodeId.
        let resolved_page = page_for(doc, block.id());
        if let Block::PageBreak { .. } = block {
            // Flush whatever's accumulated and reset.
            if !current_text.trim().is_empty() {
                buckets.push((current_page, std::mem::take(&mut current_text)));
            }
            // The next block's page_assignment will set the new page.
            continue;
        }
        if let Some(p) = resolved_page {
            if current_page.is_none() {
                current_page = Some(p);
            } else if current_page != Some(p) {
                if !current_text.trim().is_empty() {
                    buckets.push((current_page, std::mem::take(&mut current_text)));
                }
                current_page = Some(p);
            }
        }
        if !current_text.is_empty() {
            current_text.push('\n');
        }
        current_text.push_str(&block.text());
    }
    if !current_text.trim().is_empty() {
        buckets.push((current_page, current_text));
    }

    buckets
        .into_iter()
        .enumerate()
        .map(|(i, (page, text))| Chunk {
            text,
            citation: page
                .map(|p| serde_json::json!({ "page": p }))
                .unwrap_or_else(|| serde_json::json!({})),
            metadata: ChunkMetadata {
                strategy: "page",
                chunk_index: i,
                oversize: false,
            },
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Strategy: heading / section
// ---------------------------------------------------------------------------

fn chunks_by_heading(doc: &Document, top_level_only: bool) -> Vec<Chunk> {
    let strategy = if top_level_only { "section" } else { "heading" };
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut heading_path: Vec<String> = Vec::new();
    let mut current_text = String::new();
    let mut current_pages: Option<(usize, usize)> = None; // (min, max)
    let mut chunk_index: usize = 0;

    for block in &doc.content {
        let block_page = page_for(doc, block.id());
        let is_boundary = match block {
            Block::Heading { level, content, .. } => {
                // Always flush at section boundaries when top_level_only=true
                // and rank == 1; flush at any heading otherwise.
                let rank_match = if top_level_only { *level == 1 } else { true };
                if rank_match {
                    // Update heading_path BEFORE emitting the chunk, but the
                    // about-to-flush chunk uses the OLD heading_path.
                    let new_label: String = content.iter().flat_map(inline_text).collect();
                    if !current_text.trim().is_empty() {
                        let cite = build_heading_citation(&heading_path, current_pages);
                        chunks.push(Chunk {
                            text: std::mem::take(&mut current_text),
                            citation: cite,
                            metadata: ChunkMetadata {
                                strategy,
                                chunk_index,
                                oversize: false,
                            },
                        });
                        chunk_index += 1;
                    }
                    // Replace heading_path at this rank.
                    update_heading_path(&mut heading_path, *level, new_label);
                    current_pages = None;
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        if is_boundary {
            // Heading itself does not contribute to the new chunk body
            // (it's metadata via heading_path); skip.
            continue;
        }
        // Append text; track page range.
        let body = block.text();
        if !body.trim().is_empty() {
            if !current_text.is_empty() {
                current_text.push('\n');
            }
            current_text.push_str(&body);
        }
        if let Some(p) = block_page {
            current_pages = Some(match current_pages {
                None => (p, p),
                Some((lo, hi)) => (lo.min(p), hi.max(p)),
            });
        }
    }
    if !current_text.trim().is_empty() {
        let cite = build_heading_citation(&heading_path, current_pages);
        chunks.push(Chunk {
            text: current_text,
            citation: cite,
            metadata: ChunkMetadata {
                strategy,
                chunk_index,
                oversize: false,
            },
        });
    }
    chunks
}

fn update_heading_path(path: &mut Vec<String>, level: u8, label: String) {
    let level = level.max(1) as usize;
    // Truncate to (level-1) entries, then push.
    if path.len() >= level {
        path.truncate(level - 1);
    } else {
        // Pad with empty strings so depth always equals (level-1) before push.
        while path.len() + 1 < level {
            path.push(String::new());
        }
    }
    path.push(label);
}

fn build_heading_citation(path: &[String], pages: Option<(usize, usize)>) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "heading_path".into(),
        serde_json::Value::Array(
            path.iter()
                .map(|s| serde_json::Value::String(s.clone()))
                .collect(),
        ),
    );
    if let Some((lo, hi)) = pages {
        obj.insert(
            "page_range".into(),
            serde_json::Value::Array(vec![
                serde_json::Value::Number(lo.into()),
                serde_json::Value::Number(hi.into()),
            ]),
        );
    }
    serde_json::Value::Object(obj)
}

// ---------------------------------------------------------------------------
// Strategy: size
// ---------------------------------------------------------------------------

fn chunks_by_size(doc: &Document, target: usize) -> Vec<Chunk> {
    // Walk top-level blocks; each contributes one paragraph-sized
    // text chunk. Aggregate into chunks until we approach the target;
    // never split a single block. Track pages along the way.
    let target = target.max(1);
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut current_text = String::new();
    let mut current_pages: Option<(usize, usize)> = None;
    let mut chunk_index: usize = 0;

    let push_chunk = |chunks: &mut Vec<Chunk>,
                      idx: &mut usize,
                      text: &mut String,
                      pages: &mut Option<(usize, usize)>,
                      oversize: bool| {
        if text.trim().is_empty() {
            text.clear();
            *pages = None;
            return;
        }
        let cite = match *pages {
            Some((lo, hi)) => serde_json::json!({ "page_range": [lo, hi] }),
            None => serde_json::json!({}),
        };
        chunks.push(Chunk {
            text: std::mem::take(text),
            citation: cite,
            metadata: ChunkMetadata {
                strategy: "size",
                chunk_index: *idx,
                oversize,
            },
        });
        *idx += 1;
        *pages = None;
    };

    for block in &doc.content {
        let body = block.text();
        if body.trim().is_empty() {
            continue;
        }
        let block_page = page_for(doc, block.id());

        // If this single block exceeds the budget AND the current chunk
        // is empty, emit the block as an oversize chunk.
        if body.chars().count() > target && current_text.is_empty() {
            current_text.push_str(&body);
            if let Some(p) = block_page {
                current_pages = Some(match current_pages {
                    None => (p, p),
                    Some((lo, hi)) => (lo.min(p), hi.max(p)),
                });
            }
            push_chunk(
                &mut chunks,
                &mut chunk_index,
                &mut current_text,
                &mut current_pages,
                /*oversize=*/ true,
            );
            continue;
        }

        // If appending would exceed the budget, flush first.
        let projected = current_text.chars().count() + 1 + body.chars().count();
        if !current_text.is_empty() && projected > target {
            push_chunk(
                &mut chunks,
                &mut chunk_index,
                &mut current_text,
                &mut current_pages,
                false,
            );
        }
        if !current_text.is_empty() {
            current_text.push('\n');
        }
        current_text.push_str(&body);
        if let Some(p) = block_page {
            current_pages = Some(match current_pages {
                None => (p, p),
                Some((lo, hi)) => (lo.min(p), hi.max(p)),
            });
        }
    }
    push_chunk(
        &mut chunks,
        &mut chunk_index,
        &mut current_text,
        &mut current_pages,
        false,
    );
    chunks
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn page_for(doc: &Document, id: NodeId) -> Option<usize> {
    doc.presentation.as_ref()?.page_assignments.get(id).copied()
}

fn inline_text(inline: &udoc_core::document::Inline) -> Option<String> {
    use udoc_core::document::Inline;
    match inline {
        Inline::Text { text, .. } => Some(text.clone()),
        Inline::Link { content, .. } => Some(
            content
                .iter()
                .filter_map(inline_text)
                .collect::<Vec<_>>()
                .join(""),
        ),
        Inline::Code { text, .. } => Some(text.clone()),
        _ => None,
    }
}

// Suppress unused-warning when the helper isn't needed (e.g. by future
// refactors that drop list-item iteration).
#[allow(dead_code)]
fn list_items_text(items: &[ListItem]) -> String {
    let mut out = String::new();
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        for (j, block) in item.content.iter().enumerate() {
            if j > 0 {
                out.push('\n');
            }
            out.push_str(&block.text());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::document::{Block, Document, Inline, NodeId, Presentation, SpanStyle};

    fn text_inline(text: &str) -> Inline {
        Inline::Text {
            id: NodeId::new(0),
            text: text.into(),
            style: SpanStyle::default(),
        }
    }

    fn paragraph(id: u64, text: &str) -> Block {
        Block::Paragraph {
            id: NodeId::new(id),
            content: vec![text_inline(text)],
        }
    }

    fn heading(id: u64, level: u8, text: &str) -> Block {
        Block::Heading {
            id: NodeId::new(id),
            level,
            content: vec![text_inline(text)],
        }
    }

    fn doc_with(blocks: Vec<Block>) -> Document {
        let mut d = Document::default();
        d.content = blocks;
        d
    }

    fn doc_with_pages(blocks: Vec<Block>, page_for_each: &[usize]) -> Document {
        let mut d = doc_with(blocks);
        let mut p = Presentation::default();
        let ids: Vec<NodeId> = d.content.iter().map(|b| b.id()).collect();
        for (id, page) in ids.iter().zip(page_for_each.iter()) {
            p.page_assignments.set(*id, *page);
        }
        d.presentation = Some(p);
        d
    }

    fn parse_lines(buf: &[u8]) -> Vec<serde_json::Value> {
        std::str::from_utf8(buf)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn chunks_by_page_groups_blocks() {
        let doc = doc_with_pages(
            vec![
                paragraph(1, "first page para 1"),
                paragraph(2, "first page para 2"),
                paragraph(3, "second page para"),
            ],
            &[0, 0, 1],
        );
        let mut buf = Vec::new();
        emit_chunks(
            &doc,
            &ChunkOptions {
                strategy: ChunkBy::Page,
                size: 2000,
            },
            &mut buf,
        )
        .unwrap();
        let lines = parse_lines(&buf);
        assert_eq!(lines.len(), 2, "two pages -> two chunks");
        assert_eq!(lines[0]["citation"]["page"], 0);
        assert_eq!(lines[1]["citation"]["page"], 1);
        assert_eq!(lines[0]["metadata"]["strategy"], "page");
    }

    #[test]
    fn chunks_by_page_handles_pagebreak() {
        let doc = doc_with(vec![
            paragraph(1, "before break"),
            Block::PageBreak { id: NodeId::new(2) },
            paragraph(3, "after break"),
        ]);
        let mut buf = Vec::new();
        emit_chunks(
            &doc,
            &ChunkOptions {
                strategy: ChunkBy::Page,
                size: 2000,
            },
            &mut buf,
        )
        .unwrap();
        let lines = parse_lines(&buf);
        assert_eq!(lines.len(), 2);
        assert!(lines[0]["text"].as_str().unwrap().contains("before"));
        assert!(lines[1]["text"].as_str().unwrap().contains("after"));
    }

    #[test]
    fn chunks_by_heading_splits_at_each_heading() {
        let doc = doc_with(vec![
            heading(1, 1, "Intro"),
            paragraph(2, "intro body"),
            heading(3, 2, "Sub-section"),
            paragraph(4, "sub body"),
        ]);
        let mut buf = Vec::new();
        emit_chunks(
            &doc,
            &ChunkOptions {
                strategy: ChunkBy::Heading,
                size: 2000,
            },
            &mut buf,
        )
        .unwrap();
        let lines = parse_lines(&buf);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0]["citation"]["heading_path"]
                .as_array()
                .unwrap()
                .last()
                .unwrap(),
            "Intro"
        );
        assert_eq!(
            lines[1]["citation"]["heading_path"]
                .as_array()
                .unwrap()
                .last()
                .unwrap(),
            "Sub-section"
        );
    }

    #[test]
    fn chunks_by_section_only_splits_top_level() {
        let doc = doc_with(vec![
            heading(1, 1, "Intro"),
            paragraph(2, "intro body"),
            heading(3, 2, "Sub"),
            paragraph(4, "sub body"),
            heading(5, 1, "Next"),
            paragraph(6, "next body"),
        ]);
        let mut buf = Vec::new();
        emit_chunks(
            &doc,
            &ChunkOptions {
                strategy: ChunkBy::Section,
                size: 2000,
            },
            &mut buf,
        )
        .unwrap();
        let lines = parse_lines(&buf);
        assert_eq!(lines.len(), 2, "two top-level sections -> two chunks");
        assert!(lines[0]["text"].as_str().unwrap().contains("intro body"));
        assert!(lines[0]["text"].as_str().unwrap().contains("sub body"));
        assert!(lines[1]["text"].as_str().unwrap().contains("next body"));
    }

    #[test]
    fn chunks_by_heading_drops_empty_body() {
        let doc = doc_with(vec![
            heading(1, 1, "Empty"),
            heading(2, 1, "Has Body"),
            paragraph(3, "body"),
        ]);
        let mut buf = Vec::new();
        emit_chunks(
            &doc,
            &ChunkOptions {
                strategy: ChunkBy::Heading,
                size: 2000,
            },
            &mut buf,
        )
        .unwrap();
        let lines = parse_lines(&buf);
        assert_eq!(lines.len(), 1, "empty headings get no chunk");
    }

    #[test]
    fn chunks_by_size_packs_paragraphs() {
        // Each paragraph ~20 chars. Target 50 -> 2 paragraphs per chunk.
        let doc = doc_with(vec![
            paragraph(1, "aaaaaaaaaaaaaaaaaaaaa"),
            paragraph(2, "bbbbbbbbbbbbbbbbbbbbb"),
            paragraph(3, "ccccccccccccccccccccc"),
        ]);
        let mut buf = Vec::new();
        emit_chunks(
            &doc,
            &ChunkOptions {
                strategy: ChunkBy::Size,
                size: 50,
            },
            &mut buf,
        )
        .unwrap();
        let lines = parse_lines(&buf);
        assert_eq!(lines.len(), 2, "should produce 2 chunks");
        assert!(!lines[0]["metadata"]["oversize"].as_bool().unwrap());
    }

    #[test]
    fn chunks_by_size_oversize_paragraph_not_split() {
        let big = "x".repeat(500);
        let doc = doc_with(vec![paragraph(1, &big)]);
        let mut buf = Vec::new();
        emit_chunks(
            &doc,
            &ChunkOptions {
                strategy: ChunkBy::Size,
                size: 100,
            },
            &mut buf,
        )
        .unwrap();
        let lines = parse_lines(&buf);
        assert_eq!(lines.len(), 1);
        assert!(lines[0]["metadata"]["oversize"].as_bool().unwrap());
        assert_eq!(lines[0]["text"].as_str().unwrap().len(), 500);
    }

    #[test]
    fn each_line_is_valid_ndjson() {
        let doc = doc_with(vec![
            paragraph(1, "one"),
            paragraph(2, "two"),
            paragraph(3, "three"),
        ]);
        let mut buf = Vec::new();
        emit_chunks(
            &doc,
            &ChunkOptions {
                strategy: ChunkBy::Size,
                size: 5,
            },
            &mut buf,
        )
        .unwrap();
        for line in std::str::from_utf8(&buf).unwrap().lines() {
            // Each line must be valid JSON; no embedded newlines.
            let _: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(!line.contains('\n'));
        }
    }

    #[test]
    fn chunk_index_is_sequential() {
        let doc = doc_with(vec![
            paragraph(1, "a"),
            paragraph(2, "b"),
            paragraph(3, "c"),
        ]);
        let mut buf = Vec::new();
        emit_chunks(
            &doc,
            &ChunkOptions {
                strategy: ChunkBy::Size,
                size: 1,
            },
            &mut buf,
        )
        .unwrap();
        let lines = parse_lines(&buf);
        for (i, line) in lines.iter().enumerate() {
            assert_eq!(line["metadata"]["chunk_index"].as_u64(), Some(i as u64));
        }
    }

    #[test]
    fn empty_doc_emits_zero_lines() {
        let doc = doc_with(vec![]);
        let mut buf = Vec::new();
        emit_chunks(
            &doc,
            &ChunkOptions {
                strategy: ChunkBy::Page,
                size: 2000,
            },
            &mut buf,
        )
        .unwrap();
        assert!(buf.is_empty());
    }

    #[test]
    fn page_strategy_includes_page_in_citation() {
        let doc = doc_with_pages(vec![paragraph(1, "hi")], &[7]);
        let mut buf = Vec::new();
        emit_chunks(
            &doc,
            &ChunkOptions {
                strategy: ChunkBy::Page,
                size: 2000,
            },
            &mut buf,
        )
        .unwrap();
        let lines = parse_lines(&buf);
        assert_eq!(lines[0]["citation"]["page"], 7);
    }
}
