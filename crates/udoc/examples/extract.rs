//! Open a PDF, walk the Document tree, print structured output.
//!
//! Demonstrates the one-shot extraction path: `udoc::extract(path)` returns a
//! unified [`Document`](udoc::Document) regardless of source format. The same
//! shape is produced for DOCX, XLSX, PPTX, RTF, ODF, DOC, XLS, PPT, and Markdown.
//!
//! Run with:
//!
//! ```text
//! cargo run -p udoc --example extract
//! ```
//!
//! Override the fixture path with the first argument:
//!
//! ```text
//! cargo run -p udoc --example extract -- path/to/your.pdf
//! ```
//!
//! Ends with assertions so `cargo test --examples` exercises this as a smoke
//! gate.

use std::path::PathBuf;

use udoc::{Block, Document, Inline};

/// Format the inline-span's variant as a short tag for the preview output.
fn inline_kind(inline: &Inline) -> &'static str {
    match inline {
        Inline::Text { style, .. } => {
            if style.bold && style.italic {
                "bold-italic"
            } else if style.bold {
                "bold"
            } else if style.italic {
                "italic"
            } else {
                "text"
            }
        }
        Inline::Code { .. } => "code",
        Inline::Link { .. } => "link",
        Inline::FootnoteRef { .. } => "footnote-ref",
        Inline::InlineImage { .. } => "image",
        Inline::SoftBreak { .. } => "soft-break",
        Inline::LineBreak { .. } => "line-break",
        _ => "other",
    }
}

/// Default in-tree fixture used when no path argument is provided.
///
/// Path is resolved from `CARGO_MANIFEST_DIR` so the example works regardless
/// of where `cargo run` is invoked from.
fn default_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/udoc -> crates")
        .parent()
        .expect("crates -> repo root")
        .join("crates/udoc-pdf/tests/corpus/realworld/arxiv_pdflatex.pdf")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(default_fixture);

    println!("opening {}", path.display());

    // One-shot extraction: detect format, parse, build the unified Document.
    let doc: Document = udoc::extract(&path)?;

    // Metadata is always present, even when fields are unset.
    println!("pages:  {}", doc.metadata.page_count);
    if let Some(title) = &doc.metadata.title {
        println!("title:  {title}");
    }
    if let Some(author) = &doc.metadata.author {
        println!("author: {author}");
    }
    println!();

    // Walk the content spine. Block variants cover paragraphs, headings,
    // tables, images, page breaks, lists, footnotes, etc. Inlines (text,
    // bold, italic, links) live inside paragraph/heading blocks.
    let mut blocks = 0usize;
    let mut paragraphs = 0usize;
    let mut headings = 0usize;
    let mut tables = 0usize;
    let mut chars = 0usize;
    let mut inlines = 0usize;

    for block in &doc.content {
        blocks += 1;
        match block {
            Block::Heading { level, content, .. } => {
                headings += 1;
                inlines += content.len();
                println!("[h{level}] {}", block.text());
            }
            Block::Paragraph { content, .. } => {
                paragraphs += 1;
                inlines += content.len();
                // Show inline span types for the first few paragraphs only,
                // so output stays readable on long documents.
                if paragraphs <= 3 {
                    let kinds: Vec<&'static str> = content.iter().map(inline_kind).collect();
                    println!("[p] {} inlines: {:?}", content.len(), kinds);
                }
            }
            Block::Table { table, .. } => {
                tables += 1;
                println!(
                    "[table] {} rows x {} cols",
                    table.rows.len(),
                    table.num_columns
                );
            }
            Block::PageBreak { .. } => {
                println!("--- page break ---");
            }
            _ => {}
        }
        chars += block.text().chars().count();
    }

    println!();
    println!(
        "summary: {blocks} blocks ({paragraphs} para, {headings} heading, {tables} table), \
         {inlines} inline spans, {chars} chars"
    );

    // Smoke assertions. These run under `cargo test --examples` and ensure
    // the extraction pipeline returned a non-trivial Document.
    assert!(
        doc.metadata.page_count > 0,
        "expected non-zero page count, got {}",
        doc.metadata.page_count
    );
    assert!(blocks > 0, "expected non-empty content spine");
    assert!(
        chars > 100,
        "expected >100 chars of extracted text, got {chars}"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    /// Drive `main()` from a test so `cargo test --examples` exercises the
    /// full extract pipeline + assertions, not just a compile check.
    #[test]
    fn example_runs() {
        super::main().expect("extract example should succeed");
    }
}
