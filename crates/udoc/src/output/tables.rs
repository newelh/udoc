//! TSV table output (--tables).
//!
//! Extracts all tables from the document and renders them as TSV.
//! Each table gets a header comment with table number, page, column
//! count, and row count.

use std::io::Write;

use udoc_core::document::{Block, Document, Overlay};

/// Collected table info for rendering.
struct TableInfo<'a> {
    table: &'a udoc_core::document::TableData,
    page: Option<usize>,
}

/// Write all tables from a Document as TSV.
///
/// `page_assignments` maps block NodeIds to page indices.
pub fn write_tables(
    doc: &Document,
    writer: &mut dyn Write,
    page_assignments: Option<&Overlay<usize>>,
) -> std::io::Result<()> {
    // Collect all tables from the content spine (walk recursively).
    let mut tables: Vec<TableInfo<'_>> = Vec::new();
    collect_tables(&doc.content, page_assignments, &mut tables);

    for (i, info) in tables.iter().enumerate() {
        if i > 0 {
            writeln!(writer)?;
        }

        let page_str = match info.page {
            Some(p) => format!("page {}", p + 1), // 1-based for display
            None => "page ?".to_string(),
        };

        writeln!(
            writer,
            "# Table {} ({}, {} columns, {} rows)",
            i + 1,
            page_str,
            info.table.num_columns,
            info.table.rows.len()
        )?;

        for row in &info.table.rows {
            for (j, cell) in row.cells.iter().enumerate() {
                if j > 0 {
                    write!(writer, "\t")?;
                }
                let text = cell.text().replace(['\t', '\n'], " ");
                write!(writer, "{}", text)?;
            }
            writeln!(writer)?;
        }
    }

    Ok(())
}

/// Recursively collect tables from blocks.
fn collect_tables<'a>(
    blocks: &'a [Block],
    page_assignments: Option<&Overlay<usize>>,
    out: &mut Vec<TableInfo<'a>>,
) {
    for block in blocks {
        match block {
            Block::Table { id, table, .. } => {
                let page = page_assignments.and_then(|pa| pa.get(*id).copied());
                out.push(TableInfo { table, page });
            }
            Block::Section { children, .. } | Block::Shape { children, .. } => {
                collect_tables(children, page_assignments, out);
            }
            Block::List { items, .. } => {
                for item in items {
                    collect_tables(&item.content, page_assignments, out);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::document::{
        Block, Inline, NodeId, Overlay, SpanStyle, TableCell, TableData, TableRow,
    };

    fn make_table(id: u64, headers: &[&str], data: &[&[&str]]) -> Block {
        let mut rows = Vec::new();
        let mut next_id = id + 1;

        // Header row
        let header_cells: Vec<TableCell> = headers
            .iter()
            .map(|h| {
                let cell_id = NodeId::new(next_id);
                next_id += 1;
                let para_id = NodeId::new(next_id);
                next_id += 1;
                let text_id = NodeId::new(next_id);
                next_id += 1;
                TableCell::new(
                    cell_id,
                    vec![Block::Paragraph {
                        id: para_id,
                        content: vec![Inline::Text {
                            id: text_id,
                            text: (*h).into(),
                            style: SpanStyle::default(),
                        }],
                    }],
                )
            })
            .collect();
        let mut header_row = TableRow::new(NodeId::new(next_id), header_cells);
        header_row.is_header = true;
        next_id += 1;
        rows.push(header_row);

        // Data rows
        for row_data in data {
            let cells: Vec<TableCell> = row_data
                .iter()
                .map(|c| {
                    let cell_id = NodeId::new(next_id);
                    next_id += 1;
                    let para_id = NodeId::new(next_id);
                    next_id += 1;
                    let text_id = NodeId::new(next_id);
                    next_id += 1;
                    TableCell::new(
                        cell_id,
                        vec![Block::Paragraph {
                            id: para_id,
                            content: vec![Inline::Text {
                                id: text_id,
                                text: (*c).into(),
                                style: SpanStyle::default(),
                            }],
                        }],
                    )
                })
                .collect();
            rows.push(TableRow::new(NodeId::new(next_id), cells));
            next_id += 1;
        }

        Block::Table {
            id: NodeId::new(id),
            table: TableData::new(rows),
        }
    }

    #[test]
    fn tables_single() {
        let mut doc = Document::new();
        doc.content
            .push(make_table(0, &["A", "B"], &[&["1", "2"], &["3", "4"]]));

        let mut pa: Overlay<usize> = Overlay::new();
        pa.set(NodeId::new(0), 0);

        let mut buf = Vec::new();
        write_tables(&doc, &mut buf, Some(&pa)).unwrap();
        let s = String::from_utf8(buf).unwrap();

        assert!(s.starts_with("# Table 1 (page 1, 2 columns, 3 rows)"));
        assert!(s.contains("A\tB"));
        assert!(s.contains("1\t2"));
        assert!(s.contains("3\t4"));
    }

    #[test]
    fn tables_multiple() {
        let mut doc = Document::new();
        doc.content.push(make_table(0, &["X"], &[&["10"]]));
        doc.content
            .push(make_table(100, &["Y", "Z"], &[&["a", "b"]]));

        let mut buf = Vec::new();
        write_tables(&doc, &mut buf, None).unwrap();
        let s = String::from_utf8(buf).unwrap();

        assert!(s.contains("# Table 1"));
        assert!(s.contains("# Table 2"));
        // Blank line between tables
        assert!(s.contains("\n\n# Table 2"));
    }

    #[test]
    fn tables_no_tables() {
        let mut doc = Document::new();
        doc.content.push(Block::Paragraph {
            id: NodeId::new(0),
            content: vec![Inline::Text {
                id: NodeId::new(1),
                text: "No tables here".into(),
                style: SpanStyle::default(),
            }],
        });

        let mut buf = Vec::new();
        write_tables(&doc, &mut buf, None).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.is_empty());
    }

    #[test]
    fn tables_page_unknown_when_no_assignment() {
        let mut doc = Document::new();
        doc.content.push(make_table(0, &["Col"], &[&["val"]]));

        let mut buf = Vec::new();
        write_tables(&doc, &mut buf, None).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("page ?"));
    }

    #[test]
    fn tables_nested_in_section() {
        let mut doc = Document::new();
        doc.content.push(Block::Section {
            id: NodeId::new(500),
            role: None,
            children: vec![make_table(0, &["Nested"], &[&["val"]])],
        });

        let mut buf = Vec::new();
        write_tables(&doc, &mut buf, None).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("# Table 1"));
        assert!(s.contains("Nested"));
    }
}
