//! Default markdown-ish text output.
//!
//! Renders the Document model as readable text with light Markdown
//! formatting: headings get `#` prefixes, bold/italic get `**`/`*`,
//! tables are TSV, lists get `-` or `N.` prefixes.

use std::io::Write;

use udoc_core::document::{Block, Document, Inline, ListKind};

/// Write a Document as human-readable text with Markdown-ish formatting.
///
/// Between two consecutive `Paragraph` blocks we emit only a single
/// trailing newline (not a blank line). PDF reading-order pipelines
/// often produce one `Paragraph` per visual line, and a blank line
/// between every line makes the output unreadable. A blank line still
/// fires whenever a paragraph is followed by a structurally different
/// block (heading, table, list, image, code, page break) so genuine
/// semantic transitions stay visible.
pub fn write_text(doc: &Document, writer: &mut dyn Write) -> std::io::Result<()> {
    let mut prev: Option<&Block> = None;
    for block in &doc.content {
        if let Some(p) = prev {
            if needs_blank_line_between(p, block) {
                writeln!(writer)?;
            }
        }
        write_block(block, writer)?;
        prev = Some(block);
    }
    Ok(())
}

/// Decide whether to emit an extra blank line between two consecutive
/// content-spine blocks. Conservative: only suppress the blank when
/// both sides are paragraphs (the common PDF reading-order shape).
fn needs_blank_line_between(prev: &Block, next: &Block) -> bool {
    !matches!(
        (prev, next),
        (Block::Paragraph { .. }, Block::Paragraph { .. })
    )
}

fn write_block(block: &Block, writer: &mut dyn Write) -> std::io::Result<()> {
    match block {
        Block::Heading { level, content, .. } => {
            for _ in 0..*level {
                write!(writer, "#")?;
            }
            write!(writer, " ")?;
            write_inlines(content, writer)?;
            writeln!(writer)?;
        }
        Block::Paragraph { content, .. } => {
            write_inlines(content, writer)?;
            writeln!(writer)?;
        }
        Block::Table { table, .. } => {
            for (i, row) in table.rows.iter().enumerate() {
                if i > 0 {
                    writeln!(writer)?;
                }
                for (j, cell) in row.cells.iter().enumerate() {
                    if j > 0 {
                        write!(writer, "\t")?;
                    }
                    // Write cell text, replacing tabs/newlines with spaces.
                    // Uses write_all per char to avoid Display formatting overhead.
                    let text = cell.text();
                    let mut buf = [0u8; 4];
                    for ch in text.chars() {
                        match ch {
                            '\t' | '\n' => writer.write_all(b" ")?,
                            _ => writer.write_all(ch.encode_utf8(&mut buf).as_bytes())?,
                        }
                    }
                }
            }
            writeln!(writer)?;
        }
        Block::List {
            items, kind, start, ..
        } => {
            for (i, item) in items.iter().enumerate() {
                match kind {
                    ListKind::Unordered => write!(writer, "- ")?,
                    ListKind::Ordered => write!(writer, "{}. ", start.saturating_add(i as u64))?,
                    _ => write!(writer, "- ")?,
                }
                for (j, child) in item.content.iter().enumerate() {
                    if j > 0 {
                        writeln!(writer)?;
                        write!(writer, "  ")?;
                    }
                    write_block_inline(child, writer)?;
                }
                writeln!(writer)?;
            }
        }
        Block::CodeBlock { text, language, .. } => {
            write!(writer, "```")?;
            if let Some(lang) = language {
                write!(writer, "{}", lang)?;
            }
            writeln!(writer)?;
            write!(writer, "{}", text)?;
            if !text.ends_with('\n') {
                writeln!(writer)?;
            }
            writeln!(writer, "```")?;
        }
        Block::Image { alt_text, .. } => {
            if let Some(alt) = alt_text {
                writeln!(writer, "[Image: {}]", alt)?;
            } else {
                writeln!(writer, "[Image]")?;
            }
        }
        Block::PageBreak { .. } => {
            // Handled by inter-block blank line spacing
        }
        Block::ThematicBreak { .. } => {
            writeln!(writer, "---")?;
        }
        Block::Section { children, .. } => {
            for (i, child) in children.iter().enumerate() {
                if i > 0 {
                    writeln!(writer)?;
                }
                write_block(child, writer)?;
            }
        }
        Block::Shape {
            children, alt_text, ..
        } => {
            if let Some(alt) = alt_text {
                writeln!(writer, "[Shape: {}]", alt)?;
            }
            for child in children {
                write_block(child, writer)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Write a block inline (for list item content, no trailing newline).
fn write_block_inline(block: &Block, writer: &mut dyn Write) -> std::io::Result<()> {
    match block {
        Block::Paragraph { content, .. } | Block::Heading { content, .. } => {
            write_inlines(content, writer)?;
        }
        _ => {
            write_block(block, writer)?;
        }
    }
    Ok(())
}

fn write_inlines(inlines: &[Inline], writer: &mut dyn Write) -> std::io::Result<()> {
    for inline in inlines {
        write_inline(inline, writer)?;
    }
    Ok(())
}

fn write_inline(inline: &Inline, writer: &mut dyn Write) -> std::io::Result<()> {
    match inline {
        Inline::Text { text, style, .. } => {
            let marker = match (style.bold, style.italic) {
                (true, true) => "***",
                (true, false) => "**",
                (false, true) => "*",
                _ => "",
            };
            write!(writer, "{}{}{}", marker, text, marker)?;
        }
        Inline::Code { text, .. } => {
            write!(writer, "`{}`", text)?;
        }
        Inline::Link { url, content, .. } => {
            write!(writer, "[")?;
            write_inlines(content, writer)?;
            write!(writer, "]({})", url)?;
        }
        Inline::FootnoteRef { label, .. } => {
            write!(writer, "[^{}]", label)?;
        }
        Inline::InlineImage { alt_text, .. } => {
            if let Some(alt) = alt_text {
                write!(writer, "[Image: {}]", alt)?;
            } else {
                write!(writer, "[Image]")?;
            }
        }
        Inline::SoftBreak { .. } => write!(writer, " ")?,
        Inline::LineBreak { .. } => writeln!(writer)?,
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::document::{
        Block, Inline, ListItem, ListKind, NodeId, SpanStyle, TableCell, TableData, TableRow,
    };

    fn render(doc: &Document) -> String {
        let mut buf = Vec::new();
        write_text(doc, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn text_paragraph() {
        let mut doc = Document::new();
        doc.content.push(Block::Paragraph {
            id: NodeId::new(0),
            content: vec![Inline::Text {
                id: NodeId::new(1),
                text: "Hello world".into(),
                style: SpanStyle::default(),
            }],
        });
        assert_eq!(render(&doc), "Hello world\n");
    }

    #[test]
    fn text_heading() {
        let mut doc = Document::new();
        doc.content.push(Block::Heading {
            id: NodeId::new(0),
            level: 2,
            content: vec![Inline::Text {
                id: NodeId::new(1),
                text: "Title".into(),
                style: SpanStyle::default(),
            }],
        });
        assert_eq!(render(&doc), "## Title\n");
    }

    #[test]
    fn text_bold_italic() {
        let mut doc = Document::new();
        doc.content.push(Block::Paragraph {
            id: NodeId::new(0),
            content: vec![
                Inline::Text {
                    id: NodeId::new(1),
                    text: "normal ".into(),
                    style: SpanStyle::default(),
                },
                Inline::Text {
                    id: NodeId::new(2),
                    text: "bold".into(),
                    style: SpanStyle::default().with_bold(),
                },
                Inline::Text {
                    id: NodeId::new(3),
                    text: " ".into(),
                    style: SpanStyle::default(),
                },
                Inline::Text {
                    id: NodeId::new(4),
                    text: "italic".into(),
                    style: SpanStyle::default().with_italic(),
                },
                Inline::Text {
                    id: NodeId::new(5),
                    text: " ".into(),
                    style: SpanStyle::default(),
                },
                Inline::Text {
                    id: NodeId::new(6),
                    text: "both".into(),
                    style: SpanStyle::default().with_bold().with_italic(),
                },
            ],
        });
        assert_eq!(render(&doc), "normal **bold** *italic* ***both***\n");
    }

    #[test]
    fn text_table() {
        let mut doc = Document::new();
        doc.content.push(Block::Table {
            id: NodeId::new(0),
            table: TableData::new(vec![
                TableRow::new(
                    NodeId::new(1),
                    vec![
                        TableCell::new(
                            NodeId::new(2),
                            vec![Block::Paragraph {
                                id: NodeId::new(3),
                                content: vec![Inline::Text {
                                    id: NodeId::new(4),
                                    text: "A".into(),
                                    style: SpanStyle::default(),
                                }],
                            }],
                        ),
                        TableCell::new(
                            NodeId::new(5),
                            vec![Block::Paragraph {
                                id: NodeId::new(6),
                                content: vec![Inline::Text {
                                    id: NodeId::new(7),
                                    text: "B".into(),
                                    style: SpanStyle::default(),
                                }],
                            }],
                        ),
                    ],
                )
                .with_header(),
                TableRow::new(
                    NodeId::new(8),
                    vec![
                        TableCell::new(
                            NodeId::new(9),
                            vec![Block::Paragraph {
                                id: NodeId::new(10),
                                content: vec![Inline::Text {
                                    id: NodeId::new(11),
                                    text: "1".into(),
                                    style: SpanStyle::default(),
                                }],
                            }],
                        ),
                        TableCell::new(
                            NodeId::new(12),
                            vec![Block::Paragraph {
                                id: NodeId::new(13),
                                content: vec![Inline::Text {
                                    id: NodeId::new(14),
                                    text: "2".into(),
                                    style: SpanStyle::default(),
                                }],
                            }],
                        ),
                    ],
                ),
            ]),
        });
        assert_eq!(render(&doc), "A\tB\n1\t2\n");
    }

    #[test]
    fn text_list_unordered() {
        let mut doc = Document::new();
        doc.content.push(Block::List {
            id: NodeId::new(0),
            kind: ListKind::Unordered,
            start: 1,
            items: vec![
                ListItem::new(
                    NodeId::new(1),
                    vec![Block::Paragraph {
                        id: NodeId::new(2),
                        content: vec![Inline::Text {
                            id: NodeId::new(3),
                            text: "first".into(),
                            style: SpanStyle::default(),
                        }],
                    }],
                ),
                ListItem::new(
                    NodeId::new(4),
                    vec![Block::Paragraph {
                        id: NodeId::new(5),
                        content: vec![Inline::Text {
                            id: NodeId::new(6),
                            text: "second".into(),
                            style: SpanStyle::default(),
                        }],
                    }],
                ),
            ],
        });
        assert_eq!(render(&doc), "- first\n- second\n");
    }

    #[test]
    fn text_list_ordered() {
        let mut doc = Document::new();
        doc.content.push(Block::List {
            id: NodeId::new(0),
            kind: ListKind::Ordered,
            start: 3,
            items: vec![
                ListItem::new(
                    NodeId::new(1),
                    vec![Block::Paragraph {
                        id: NodeId::new(2),
                        content: vec![Inline::Text {
                            id: NodeId::new(3),
                            text: "third".into(),
                            style: SpanStyle::default(),
                        }],
                    }],
                ),
                ListItem::new(
                    NodeId::new(4),
                    vec![Block::Paragraph {
                        id: NodeId::new(5),
                        content: vec![Inline::Text {
                            id: NodeId::new(6),
                            text: "fourth".into(),
                            style: SpanStyle::default(),
                        }],
                    }],
                ),
            ],
        });
        assert_eq!(render(&doc), "3. third\n4. fourth\n");
    }

    #[test]
    fn text_code_block() {
        let mut doc = Document::new();
        doc.content.push(Block::CodeBlock {
            id: NodeId::new(0),
            text: "fn main() {}".into(),
            language: Some("rust".into()),
        });
        assert_eq!(render(&doc), "```rust\nfn main() {}\n```\n");
    }

    #[test]
    fn text_image() {
        let mut doc = Document::new();
        doc.content.push(Block::Image {
            id: NodeId::new(0),
            image_ref: udoc_core::document::ImageRef::new(0),
            alt_text: Some("A photo".into()),
        });
        assert_eq!(render(&doc), "[Image: A photo]\n");
    }

    #[test]
    fn text_thematic_break() {
        let mut doc = Document::new();
        doc.content
            .push(Block::ThematicBreak { id: NodeId::new(0) });
        assert_eq!(render(&doc), "---\n");
    }

    #[test]
    fn text_inter_paragraph_spacing_no_blank_line() {
        // Consecutive paragraphs stack tightly (one newline per block),
        // not a blank line each. Otherwise PDFs that emit one paragraph
        // per visual line are unreadable.
        let mut doc = Document::new();
        doc.content.push(Block::Paragraph {
            id: NodeId::new(0),
            content: vec![Inline::Text {
                id: NodeId::new(1),
                text: "First".into(),
                style: SpanStyle::default(),
            }],
        });
        doc.content.push(Block::Paragraph {
            id: NodeId::new(2),
            content: vec![Inline::Text {
                id: NodeId::new(3),
                text: "Second".into(),
                style: SpanStyle::default(),
            }],
        });
        assert_eq!(render(&doc), "First\nSecond\n");
    }

    #[test]
    fn text_paragraph_to_heading_blank_line() {
        // Paragraph -> Heading IS a structural transition; keep the blank.
        let mut doc = Document::new();
        doc.content.push(Block::Paragraph {
            id: NodeId::new(0),
            content: vec![Inline::Text {
                id: NodeId::new(1),
                text: "Intro line".into(),
                style: SpanStyle::default(),
            }],
        });
        doc.content.push(Block::Heading {
            id: NodeId::new(2),
            level: 1,
            content: vec![Inline::Text {
                id: NodeId::new(3),
                text: "Section".into(),
                style: SpanStyle::default(),
            }],
        });
        assert_eq!(render(&doc), "Intro line\n\n# Section\n");
    }

    #[test]
    fn text_link() {
        let mut doc = Document::new();
        doc.content.push(Block::Paragraph {
            id: NodeId::new(0),
            content: vec![Inline::Link {
                id: NodeId::new(1),
                url: "https://example.com".into(),
                content: vec![Inline::Text {
                    id: NodeId::new(2),
                    text: "click".into(),
                    style: SpanStyle::default(),
                }],
            }],
        });
        assert_eq!(render(&doc), "[click](https://example.com)\n");
    }

    #[test]
    fn text_inline_code() {
        let mut doc = Document::new();
        doc.content.push(Block::Paragraph {
            id: NodeId::new(0),
            content: vec![Inline::Code {
                id: NodeId::new(1),
                text: "foo()".into(),
            }],
        });
        assert_eq!(render(&doc), "`foo()`\n");
    }

    #[test]
    fn text_empty_doc() {
        let doc = Document::new();
        assert_eq!(render(&doc), "");
    }
}
