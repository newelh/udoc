//! Golden tests for PPTX Document model extraction.
//!
//! Each test extracts a PPTX from the corpus and verifies the Document model
//! content: heading levels, paragraph text, table structure, speaker notes.

use std::path::PathBuf;

fn corpus_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/pptx")
        .join(name)
}

fn require_corpus(name: &str) -> Option<PathBuf> {
    let path = corpus_path(name);
    if path.exists() {
        Some(path)
    } else {
        eprintln!(
            "SKIP: corpus file '{}' not found at {}",
            name,
            path.display()
        );
        None
    }
}

// ---------------------------------------------------------------------------
// simple_text.pptx: title (H1) + body paragraph
// ---------------------------------------------------------------------------

#[test]
fn golden_simple_text_heading() {
    let Some(path) = require_corpus("simple_text.pptx") else {
        return;
    };
    let doc = udoc::extract(&path).expect("extract should succeed");

    // Title should become H1
    let headings: Vec<(u8, String)> = doc
        .content
        .iter()
        .filter_map(|b| match b {
            udoc::Block::Heading { level, content, .. } => {
                Some((*level, content.iter().map(|i| i.text()).collect()))
            }
            _ => None,
        })
        .collect();
    assert!(
        headings.iter().any(|(l, t)| *l == 1 && t == "Hello World"),
        "expected H1 'Hello World', got headings: {:?}",
        headings
    );
}

#[test]
fn golden_simple_text_body() {
    let Some(path) = require_corpus("simple_text.pptx") else {
        return;
    };
    let doc = udoc::extract(&path).expect("extract should succeed");

    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        all_text.contains("This is a test slide with simple text content."),
        "body text missing from: {all_text}"
    );
}

// ---------------------------------------------------------------------------
// multipage.pptx: 3 slides with titles
// ---------------------------------------------------------------------------

#[test]
fn golden_multipage_slide_count() {
    let Some(path) = require_corpus("multipage.pptx") else {
        return;
    };
    let doc = udoc::extract(&path).expect("extract should succeed");
    assert_eq!(doc.metadata.page_count, 3, "should have 3 slides");
}

#[test]
fn golden_multipage_content() {
    let Some(path) = require_corpus("multipage.pptx") else {
        return;
    };
    let doc = udoc::extract(&path).expect("extract should succeed");

    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");

    // All slide text should appear in the output
    assert!(all_text.contains("Introduction"), "missing 'Introduction'");
    assert!(all_text.contains("Details"), "missing 'Details'");
    assert!(all_text.contains("Conclusion"), "missing 'Conclusion'");
    assert!(all_text.contains("Some detailed text"), "missing body text");

    // "Details" is in a title placeholder -> should be a heading
    let headings: Vec<String> = doc
        .content
        .iter()
        .filter_map(|b| match b {
            udoc::Block::Heading { content, .. } => {
                Some(content.iter().map(|i| i.text()).collect())
            }
            _ => None,
        })
        .collect();

    assert!(
        headings.iter().any(|t| t.contains("Details")),
        "title placeholder 'Details' should be an H1 heading, got: {:?}",
        headings
    );
}

#[test]
fn golden_multipage_page_breaks() {
    let Some(path) = require_corpus("multipage.pptx") else {
        return;
    };
    let doc = udoc::extract(&path).expect("extract should succeed");

    let page_break_count = doc
        .content
        .iter()
        .filter(|b| matches!(b, udoc::Block::PageBreak { .. }))
        .count();
    assert_eq!(page_break_count, 2, "3 slides should produce 2 page breaks");
}

// ---------------------------------------------------------------------------
// table.pptx: 3x3 table
// ---------------------------------------------------------------------------

#[test]
fn golden_table_structure() {
    let Some(path) = require_corpus("table.pptx") else {
        return;
    };
    let doc = udoc::extract(&path).expect("extract should succeed");

    let tables: Vec<&udoc::TableData> = doc
        .content
        .iter()
        .filter_map(|b| match b {
            udoc::Block::Table { table, .. } => Some(table),
            _ => None,
        })
        .collect();

    assert_eq!(tables.len(), 1, "should have exactly one table");
    let table = tables[0];
    assert_eq!(table.rows.len(), 3, "table should have 3 rows");

    let cell_texts: Vec<Vec<String>> = table
        .rows
        .iter()
        .map(|r| r.cells.iter().map(|c| c.text()).collect())
        .collect();

    assert_eq!(cell_texts[0], vec!["Name", "Age", "City"]);
    assert_eq!(cell_texts[1], vec!["Alice", "30", "NYC"]);
    assert_eq!(cell_texts[2], vec!["Bob", "25", "LA"]);
}

// ---------------------------------------------------------------------------
// speaker_notes.pptx: notes as Section
// ---------------------------------------------------------------------------

#[test]
fn golden_speaker_notes_content() {
    let Some(path) = require_corpus("speaker_notes.pptx") else {
        return;
    };
    let doc = udoc::extract(&path).expect("extract should succeed");

    // Find the notes section
    let notes_text: Vec<String> = doc
        .content
        .iter()
        .filter_map(|b| match b {
            udoc::Block::Section {
                role: Some(udoc::SectionRole::Notes),
                children,
                ..
            } => Some(
                children
                    .iter()
                    .map(|c| c.text())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        })
        .collect();

    assert_eq!(notes_text.len(), 1, "should have exactly one notes section");
    assert!(
        notes_text[0].contains("These are the speaker notes for this slide."),
        "notes text mismatch: {}",
        notes_text[0]
    );
}

// ---------------------------------------------------------------------------
// text_formatting.pptx: bold, italic, bold+italic runs
// ---------------------------------------------------------------------------

#[test]
fn golden_text_formatting() {
    let Some(path) = require_corpus("text_formatting.pptx") else {
        return;
    };
    let doc = udoc::extract(&path).expect("extract should succeed");

    let all_text: String = doc
        .content
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all_text.contains("bold"), "should contain 'bold' text");
    assert!(all_text.contains("italic"), "should contain 'italic' text");
}

// ---------------------------------------------------------------------------
// multiple_shapes.pptx: reading order (top -> middle -> bottom)
// ---------------------------------------------------------------------------

#[test]
fn golden_reading_order() {
    let Some(path) = require_corpus("multiple_shapes.pptx") else {
        return;
    };
    let doc = udoc::extract(&path).expect("extract should succeed");

    let texts: Vec<String> = doc
        .content
        .iter()
        .filter_map(|b| match b {
            udoc::Block::Paragraph { content, .. } | udoc::Block::Heading { content, .. } => {
                let t: String = content.iter().map(|i| i.text()).collect();
                if t.trim().is_empty() {
                    None
                } else {
                    Some(t)
                }
            }
            _ => None,
        })
        .collect();

    // Y-then-X sorting should produce top -> middle -> bottom order
    assert!(
        texts.len() >= 3,
        "expected at least 3 text blocks, got {}",
        texts.len()
    );

    let top_pos = texts.iter().position(|t| t.contains("top"));
    let mid_pos = texts.iter().position(|t| t.contains("middle"));
    let bot_pos = texts.iter().position(|t| t.contains("bottom"));

    assert!(
        top_pos.is_some() && mid_pos.is_some() && bot_pos.is_some(),
        "expected 'top', 'middle', 'bottom' texts, got: {:?}",
        texts
    );
    assert!(
        top_pos < mid_pos && mid_pos < bot_pos,
        "reading order should be top < middle < bottom, got positions: top={:?} mid={:?} bot={:?}",
        top_pos,
        mid_pos,
        bot_pos
    );
}

// ---------------------------------------------------------------------------
// empty_slide.pptx: no content
// ---------------------------------------------------------------------------

#[test]
fn golden_empty_slide() {
    let Some(path) = require_corpus("empty_slide.pptx") else {
        return;
    };
    let doc = udoc::extract(&path).expect("extract should succeed");
    assert_eq!(
        doc.metadata.page_count, 1,
        "empty slide still counts as 1 page"
    );

    let text_blocks: Vec<&udoc::Block> = doc
        .content
        .iter()
        .filter(|b| !matches!(b, udoc::Block::PageBreak { .. }))
        .collect();
    assert!(
        text_blocks.is_empty() || text_blocks.iter().all(|b| b.text().trim().is_empty()),
        "empty slide should produce no content or empty blocks"
    );
}

// ---------------------------------------------------------------------------
// merged_cells.pptx: gridSpan/rowSpan table merging
// ---------------------------------------------------------------------------

#[test]
fn golden_merged_cells() {
    let Some(path) = require_corpus("merged_cells.pptx") else {
        return;
    };
    let doc = udoc::extract(&path).expect("extract should succeed");

    let tables: Vec<&udoc::TableData> = doc
        .content
        .iter()
        .filter_map(|b| match b {
            udoc::Block::Table { table, .. } => Some(table),
            _ => None,
        })
        .collect();

    assert!(!tables.is_empty(), "should have at least one table");
    let table = tables[0];

    // Table should have the expected cell content
    let all_text = table
        .rows
        .iter()
        .flat_map(|r| r.cells.iter())
        .map(|c| c.text())
        .collect::<Vec<_>>()
        .join(" ");

    assert!(
        all_text.contains("Name") && all_text.contains("Department"),
        "merged table should contain header text, got: {all_text}"
    );
}
