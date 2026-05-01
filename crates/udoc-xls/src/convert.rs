//! XLS-to-Document model conversion.
//!
//! Converts XLS sheet data into the unified Document model. Each sheet maps
//! to a table block, separated by page breaks. Sheet internals stay inside
//! udoc-xls; the facade calls `xls_to_document` without reaching into
//! BIFF8 parser types.

use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_core::convert::{alloc_id, maybe_insert_page_break, push_tables, text_inline};
use udoc_core::diagnostics::DiagnosticsSink;
use udoc_core::document::*;
use udoc_core::error::{Error, Result};

use crate::document::XlsDocument;

/// Convert an XLS backend into the unified Document model.
///
/// Sheet content is mapped as one Table per sheet covering the data range.
/// Sheets are separated by PageBreak blocks. Empty sheets (no cells with
/// values) emit paragraphs from text_lines() if any text is present.
///
/// The `diagnostics` parameter receives parse warnings. Page range filtering
/// is handled by the caller (the facade's `define_backend_converter!` macro
/// calls this only when at least one page is in range).
pub fn xls_to_document(
    xls: &mut XlsDocument,
    diagnostics: &dyn DiagnosticsSink,
    max_pages: usize,
) -> Result<Document> {
    let _ = diagnostics; // reserved for future warning propagation

    let page_count = FormatBackend::page_count(xls).min(max_pages);
    let mut doc = Document::new();
    doc.metadata = FormatBackend::metadata(xls);

    for page_idx in 0..page_count {
        maybe_insert_page_break(&mut doc)?;

        let mut page = FormatBackend::page(xls, page_idx)
            .map_err(|e| Error::with_source(format!("opening sheet {page_idx}"), e))?;

        let tables = page.tables().map_err(|e| {
            Error::with_source(format!("extracting tables from sheet {page_idx}"), e)
        })?;

        push_tables(&mut doc, &tables)?;

        // If the sheet produced no table (empty sheet), emit text paragraphs
        // for any text content.
        if tables.is_empty() {
            let text_lines = page.text_lines().map_err(|e| {
                Error::with_source(format!("extracting lines from sheet {page_idx}"), e)
            })?;
            for line in &text_lines {
                if line.spans.is_empty() {
                    continue;
                }
                let inlines: Vec<Inline> = line
                    .spans
                    .iter()
                    .map(|span| text_inline(&doc, span.text.clone()))
                    .collect::<Result<Vec<Inline>>>()?;
                let block_id = alloc_id(&doc)?;
                doc.content.push(Block::Paragraph {
                    id: block_id,
                    content: inlines,
                });
            }
        }
    }

    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::build_minimal_xls;
    use udoc_core::diagnostics::NullDiagnostics;

    #[test]
    fn empty_xls_converts_to_empty_document() {
        let data = build_minimal_xls(&[], &[]);
        let mut doc =
            XlsDocument::from_bytes_with_diag(&data, std::sync::Arc::new(NullDiagnostics)).unwrap();
        let model = xls_to_document(&mut doc, &NullDiagnostics, usize::MAX).unwrap();
        assert!(model.content.is_empty());
    }

    #[test]
    fn single_sheet_becomes_one_table() {
        let data = build_minimal_xls(&["A", "B"], &[("Sheet1", &[(0, 0, "A"), (0, 1, "B")])]);
        let mut doc =
            XlsDocument::from_bytes_with_diag(&data, std::sync::Arc::new(NullDiagnostics)).unwrap();
        let model = xls_to_document(&mut doc, &NullDiagnostics, usize::MAX).unwrap();

        let table_blocks: Vec<_> = model
            .content
            .iter()
            .filter(|b| matches!(b, Block::Table { .. }))
            .collect();
        assert_eq!(table_blocks.len(), 1);
    }

    #[test]
    fn two_sheets_become_two_tables_with_page_break() {
        let data = build_minimal_xls(
            &["X", "Y"],
            &[("Sheet1", &[(0, 0, "X")]), ("Sheet2", &[(0, 0, "Y")])],
        );
        let mut doc =
            XlsDocument::from_bytes_with_diag(&data, std::sync::Arc::new(NullDiagnostics)).unwrap();
        let model = xls_to_document(&mut doc, &NullDiagnostics, usize::MAX).unwrap();

        let table_count = model
            .content
            .iter()
            .filter(|b| matches!(b, Block::Table { .. }))
            .count();
        assert_eq!(table_count, 2);
    }
}
