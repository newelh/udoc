//! XLSX-to-Document model conversion.
//!
//! Converts XLSX sheet data into the unified Document model. Each sheet
//! maps to a table block, separated by page breaks. This keeps XLSX
//! internals inside the XLSX crate; the facade calls `xlsx_to_document`
//! without reaching into parser types.

use std::collections::{HashMap, HashSet};

use udoc_core::backend::FormatBackend;
use udoc_core::convert::{
    alloc_id, maybe_insert_page_break, register_hyperlink, set_block_layout, set_text_styling,
};
use udoc_core::diagnostics::{DiagnosticsSink, Warning};
use udoc_core::document::*;
use udoc_core::error::{Error, Result, ResultExt};

use crate::document::{XlsxDocument, XlsxPage};
use crate::shared_strings::SharedStringEntry;
use crate::sheet::CellData;
use crate::styles::FontEntry;
use udoc_containers::opc::relationships::rel_type_matches;
use udoc_containers::opc::{rel_types, Relationship};

// ---------------------------------------------------------------------------
// XLSX-specific conversion logic
// ---------------------------------------------------------------------------

/// Convert an XLSX backend into the unified Document model.
///
/// Sheet content is mapped as one Table per sheet covering the data range.
/// Sheets are separated by PageBreak blocks. If a sheet has no tables
/// (empty sheet), text lines are emitted as paragraphs.
///
/// Cell styling (font, fill, alignment), hyperlinks, and rich text runs
/// are mapped to the presentation overlay and document model inlines.
pub fn xlsx_to_document(
    xlsx: &mut XlsxDocument,
    diagnostics: &dyn DiagnosticsSink,
    max_pages: usize,
) -> Result<Document> {
    let page_count = FormatBackend::page_count(xlsx).min(max_pages);
    let mut doc = Document::new();
    doc.metadata = FormatBackend::metadata(xlsx);

    // Dedup set for hyperlink URLs collected during conversion (#142).
    let mut hyperlink_seen: HashSet<String> = HashSet::new();

    for page_idx in 0..page_count {
        maybe_insert_page_break(&mut doc)?;

        let page = FormatBackend::page(xlsx, page_idx)
            .map_err(|e| Error::with_source(format!("opening sheet {page_idx}"), e))?;

        // Build enriched tables with styling, hyperlinks, and rich text.
        build_enriched_tables(&mut doc, &page, diagnostics, &mut hyperlink_seen)?;
    }

    Ok(doc)
}

/// Build hyperlink lookup from sheet hyperlinks + OPC relationships.
fn build_hyperlink_map(
    hyperlinks: &[crate::sheet::SheetHyperlink],
    rels: &[Relationship],
    diag: &dyn DiagnosticsSink,
) -> HashMap<(usize, usize), String> {
    // Pre-index hyperlink relationships by id for O(1) lookup per hyperlink.
    let rels_by_id: HashMap<&str, &Relationship> = rels
        .iter()
        .filter(|r| rel_type_matches(&r.rel_type, rel_types::HYPERLINK))
        .map(|r| (r.id.as_str(), r))
        .collect();

    let mut map = HashMap::new();
    for hl in hyperlinks {
        if let Some(rel) = rels_by_id.get(hl.r_id.as_str()) {
            map.insert((hl.row, hl.col), rel.target.clone());
        } else {
            diag.warning(Warning::new(
                "XlsxUnresolvedHyperlink",
                format!(
                    "hyperlink r:id '{}' at ({},{}) not found in sheet relationships",
                    hl.r_id, hl.row, hl.col
                ),
            ));
        }
    }
    map
}

/// Per-run presentation overlay entry (node_id -> ExtendedTextStyle).
struct RunOverlay {
    node_id: NodeId,
    style: ExtendedTextStyle,
}

/// Result of building cell inlines with formatting data.
struct CellInlineResult {
    inlines: Vec<Inline>,
    ext_style: Option<ExtendedTextStyle>,
    block_layout: Option<BlockLayout>,
    run_overlays: Vec<RunOverlay>,
}

/// Convert a font entry to a `SpanStyle` with bold/italic/underline/strikethrough.
fn font_to_span_style(font: Option<&FontEntry>) -> SpanStyle {
    match font {
        Some(f) => {
            let mut s = SpanStyle::default();
            s.bold = f.bold;
            s.italic = f.italic;
            s.underline = f.underline;
            s.strikethrough = f.strikethrough;
            s
        }
        None => SpanStyle::default(),
    }
}

/// Build cell inlines from a cell, handling rich text runs.
fn build_cell_inlines(
    doc: &Document,
    cell: &CellData,
    sst_entries: &[SharedStringEntry],
    stylesheet: &crate::styles::StyleSheet,
) -> Result<CellInlineResult> {
    let mut ext_style: Option<ExtendedTextStyle> = None;
    let mut block_layout: Option<BlockLayout> = None;

    // Look up font entry once for both extended styling and span style flags.
    let font = cell.cell_style.and_then(|idx| stylesheet.font_entry(idx));

    if let Some(style_idx) = cell.cell_style {
        if let Some(font) = font {
            let es = ExtendedTextStyle::new()
                .font_name(font.name.clone())
                .font_size(font.size)
                .color(font.color.map(Color::from));
            if !es.is_empty() {
                ext_style = Some(es);
            }
        }
        if let Some(fill) = stylesheet.fill_color(style_idx) {
            let es = ext_style.get_or_insert_with(ExtendedTextStyle::default);
            es.background_color = Some(Color::from(fill));
        }
        if let Some(align) = stylesheet.alignment(style_idx) {
            if let Some(a) = Alignment::from_format_str(align) {
                block_layout = Some(BlockLayout::new().alignment(Some(a)));
            }
        }
    }

    // Build inlines. If this is a rich text shared string, emit per-run inlines.
    // Per the XLSX spec, rich text runs carry self-contained formatting via <rPr>.
    // Cell-level font style (from cellXfs -> fontId) applies to plain-text cells
    // only. Rich text SpanStyle comes exclusively from the run's own flags;
    // cell-level ExtendedTextStyle (font_name, font_size, color) is applied as a
    // base and then overridden per-run in the merge loop below.
    let mut run_overlays: Vec<RunOverlay> = Vec::new();
    let inlines = if let Some(sst_idx) = cell.sst_index {
        if let Some(SharedStringEntry::Rich(runs)) = sst_entries.get(sst_idx) {
            let mut inlines = Vec::with_capacity(runs.len());
            for run in runs {
                if run.text.is_empty() {
                    continue;
                }
                let id = alloc_id(doc).context("allocating rich text run node id")?;
                let mut style = SpanStyle::default();
                style.bold = run.bold;
                style.italic = run.italic;
                style.underline = run.underline;
                style.strikethrough = run.strikethrough;
                inlines.push(Inline::Text {
                    id,
                    text: run.text.clone(),
                    style,
                });
                let has_run_styling =
                    run.color.is_some() || run.font_name.is_some() || run.font_size.is_some();
                if has_run_styling {
                    run_overlays.push(RunOverlay {
                        node_id: id,
                        style: ExtendedTextStyle::new()
                            .color(run.color.map(Color::from))
                            .font_name(run.font_name.clone())
                            .font_size(run.font_size),
                    });
                }
            }
            inlines
        } else {
            // Plain SST entry (or index out of range). Apply cell-level font
            // SpanStyle the same way as a non-SST cell below.
            let id = alloc_id(doc).context("allocating plain SST cell node id")?;
            let style = font_to_span_style(font);
            vec![Inline::Text {
                id,
                text: cell.text.clone(),
                style,
            }]
        }
    } else {
        // Plain text cell. Reuse the font entry looked up above.
        let id = alloc_id(doc).context("allocating plain text cell node id")?;
        let style = font_to_span_style(font);
        vec![Inline::Text {
            id,
            text: cell.text.clone(),
            style,
        }]
    };

    Ok(CellInlineResult {
        inlines,
        ext_style,
        block_layout,
        run_overlays,
    })
}

/// Build enriched tables from XlsxPage sheet data with full formatting support.
///
/// Every XLSX sheet is modeled as a single Table block covering its data range.
/// There is no paragraph fallback: XLSX is inherently tabular, so even single-cell
/// sheets produce a 1x1 table. Empty sheets (no cells) and sheets where all rows
/// are empty produce no content blocks.
fn build_enriched_tables(
    doc: &mut Document,
    page: &XlsxPage<'_>,
    diag: &dyn DiagnosticsSink,
    seen_urls: &mut HashSet<String>,
) -> Result<()> {
    let sheet = page.sheet;
    if sheet.cells.is_empty() {
        return Ok(());
    }

    let max_col = sheet.max_col.unwrap_or(0);

    // Build hyperlink lookup.
    let hyperlink_map = build_hyperlink_map(&sheet.hyperlinks, page.sheet_rels, diag);

    // Build merge cache.
    let merge_cache = crate::merge::build_merge_cache(&sheet.merge_regions);

    // Collect the set of rows that need to appear in the table.
    // Only rows with actual cell data or merge anchors are included,
    // avoiding O(max_row * max_col) iteration on sparse sheets.
    let mut needed_rows: Vec<usize> = Vec::with_capacity(sheet.cells.len());
    for cell in &sheet.cells {
        if needed_rows.last() != Some(&cell.row) {
            needed_rows.push(cell.row);
        }
    }
    for region in &sheet.merge_regions {
        needed_rows.push(region.start_row);
    }
    needed_rows.sort_unstable();
    needed_rows.dedup();

    let mut rows = Vec::with_capacity(needed_rows.len());
    let mut cell_idx = 0;

    for &row in &needed_rows {
        // Advance cursor past cells in skipped rows.
        while cell_idx < sheet.cells.len() && sheet.cells[cell_idx].row < row {
            cell_idx += 1;
        }
        let mut cells = Vec::new();
        for col in 0..=max_col {
            // Skip covered cells in merge regions.
            if merge_cache.covered.contains(&(row, col)) {
                continue;
            }

            // Find the cell at (row, col).
            let cell_data = loop {
                if cell_idx < sheet.cells.len()
                    && sheet.cells[cell_idx].row == row
                    && sheet.cells[cell_idx].col < col
                {
                    cell_idx += 1;
                    continue;
                }
                if cell_idx < sheet.cells.len()
                    && sheet.cells[cell_idx].row == row
                    && sheet.cells[cell_idx].col == col
                {
                    let c = &sheet.cells[cell_idx];
                    cell_idx += 1;
                    break Some(c);
                }
                break None;
            };

            let (col_span, row_span) =
                if let Some(region) = merge_cache.find_anchor(&sheet.merge_regions, row, col) {
                    (region.col_span(), region.row_span())
                } else {
                    (1, 1)
                };

            let cell_id = alloc_id(doc).context("allocating cell node id")?;

            if let Some(cell) = cell_data {
                let result = build_cell_inlines(doc, cell, page.sst_entries, page.stylesheet)
                    .context("building cell inlines")?;
                let CellInlineResult {
                    inlines,
                    ext_style,
                    block_layout,
                    run_overlays,
                } = result;

                // Set presentation overlay data before link wrapping
                // (styling targets the text inlines, not the link wrapper).
                let has_overlay =
                    ext_style.is_some() || block_layout.is_some() || !run_overlays.is_empty();

                let para_id = alloc_id(doc).context("allocating paragraph node id")?;

                if has_overlay {
                    if let Some(ref es) = ext_style {
                        for inline in &inlines {
                            set_text_styling(doc, inline.id(), es.clone());
                        }
                    }
                    if let Some(bl) = block_layout {
                        set_block_layout(doc, para_id, bl);
                    }
                    // Per-run overrides must merge with cell-level styling
                    // (so per-run color doesn't clobber cell-level font_name/font_size).
                    if !run_overlays.is_empty() {
                        let pres = doc.presentation.get_or_insert_with(Presentation::default);
                        for ro in run_overlays {
                            if let Some(existing) = pres.text_styling.get(ro.node_id) {
                                let mut merged = existing.clone();
                                if ro.style.color.is_some() {
                                    merged.color = ro.style.color;
                                }
                                if ro.style.font_name.is_some() {
                                    merged.font_name = ro.style.font_name;
                                }
                                if ro.style.font_size.is_some() {
                                    merged.font_size = ro.style.font_size;
                                }
                                if ro.style.background_color.is_some() {
                                    merged.background_color = ro.style.background_color;
                                }
                                pres.text_styling.set(ro.node_id, merged);
                            } else {
                                pres.text_styling.set(ro.node_id, ro.style);
                            }
                        }
                    }
                }

                // Check for hyperlink on this cell.
                let content_inlines = if let Some(url) = hyperlink_map.get(&(row, col)) {
                    let link_id = alloc_id(doc).context("allocating hyperlink node id")?;
                    register_hyperlink(doc, seen_urls, url);
                    vec![Inline::Link {
                        id: link_id,
                        url: url.clone(),
                        content: inlines,
                    }]
                } else {
                    inlines
                };

                let content = vec![Block::Paragraph {
                    id: para_id,
                    content: content_inlines,
                }];

                let mut tc = TableCell::new(cell_id, content);
                tc.col_span = col_span;
                tc.row_span = row_span;
                cells.push(tc);
            } else {
                // Empty cell: no content blocks to avoid unnecessary allocations.
                let mut tc = TableCell::new(cell_id, Vec::new());
                tc.col_span = col_span;
                tc.row_span = row_span;
                cells.push(tc);
            }
        }

        // Drain remaining cells in this row.
        while cell_idx < sheet.cells.len() && sheet.cells[cell_idx].row == row {
            cell_idx += 1;
        }

        let row_id = alloc_id(doc).context("allocating row node id")?;
        rows.push(TableRow::new(row_id, cells));
    }

    // Trim trailing empty rows.
    while rows
        .last()
        .map(|r| r.cells.iter().all(|c| c.text().is_empty()))
        .unwrap_or(false)
    {
        rows.pop();
    }

    if rows.is_empty() {
        return Ok(());
    }

    let table_id = alloc_id(doc).context("allocating table node id")?;
    let td = TableData::new(rows);
    doc.content.push(Block::Table {
        id: table_id,
        table: td,
    });

    // Wire column widths into the presentation overlay as ColSpecs.
    if !sheet.column_widths.is_empty() {
        let num_cols = max_col + 1;
        let mut col_specs: Vec<ColSpec> = (0..num_cols).map(|_| ColSpec::empty()).collect();
        for cw in &sheet.column_widths {
            let end = cw.max.min(num_cols - 1);
            for cs in col_specs.iter_mut().take(end + 1).skip(cw.min) {
                *cs = ColSpec::with_width(cw.width);
            }
        }
        // Only set if at least one column has a width.
        if col_specs.iter().any(|cs| cs.width.is_some()) {
            let pres = doc.presentation.get_or_insert_with(Presentation::default);
            pres.column_specs.set(table_id, col_specs);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_containers::test_util::{
        build_stored_zip, XLSX_CONTENT_TYPES, XLSX_PACKAGE_RELS, XLSX_WB_RELS_1SHEET,
        XLSX_WORKBOOK_1SHEET,
    };
    use udoc_core::diagnostics::NullDiagnostics;

    /// Build a minimal XLSX ZIP with the given sheet XML.
    fn make_xlsx(sheet_xml: &[u8]) -> Vec<u8> {
        build_stored_zip(&[
            ("[Content_Types].xml", XLSX_CONTENT_TYPES),
            ("_rels/.rels", XLSX_PACKAGE_RELS),
            ("xl/workbook.xml", XLSX_WORKBOOK_1SHEET),
            ("xl/_rels/workbook.xml.rels", XLSX_WB_RELS_1SHEET),
            ("xl/worksheets/sheet1.xml", sheet_xml),
        ])
    }

    /// Build XLSX with styles and optional shared strings.
    fn make_xlsx_with_styles(
        sheet_xml: &[u8],
        styles_xml: &[u8],
        sst_xml: Option<&[u8]>,
    ) -> Vec<u8> {
        let mut wb_rels = String::from(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
        Target="worksheets/sheet1.xml"/>
    <Relationship Id="rId3"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles"
        Target="styles.xml"/>"#,
        );
        if sst_xml.is_some() {
            wb_rels.push_str(
                r#"
    <Relationship Id="rId2"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings"
        Target="sharedStrings.xml"/>"#,
            );
        }
        wb_rels.push_str("\n</Relationships>");

        let wb_rels_bytes = wb_rels.into_bytes();

        let mut entries: Vec<(String, Vec<u8>)> = vec![
            (
                "[Content_Types].xml".to_string(),
                XLSX_CONTENT_TYPES.to_vec(),
            ),
            ("_rels/.rels".to_string(), XLSX_PACKAGE_RELS.to_vec()),
            ("xl/workbook.xml".to_string(), XLSX_WORKBOOK_1SHEET.to_vec()),
            ("xl/_rels/workbook.xml.rels".to_string(), wb_rels_bytes),
            ("xl/worksheets/sheet1.xml".to_string(), sheet_xml.to_vec()),
            ("xl/styles.xml".to_string(), styles_xml.to_vec()),
        ];
        if let Some(sst) = sst_xml {
            entries.push(("xl/sharedStrings.xml".to_string(), sst.to_vec()));
        }

        let refs: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(n, d)| (n.as_str(), d.as_slice()))
            .collect();
        build_stored_zip(&refs)
    }

    /// Build XLSX with styles, SST, and sheet-level hyperlink relationships.
    fn make_xlsx_with_hyperlinks(
        sheet_xml: &[u8],
        sst_xml: Option<&[u8]>,
        sheet_rels_xml: &[u8],
    ) -> Vec<u8> {
        let mut wb_rels = String::from(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
        Target="worksheets/sheet1.xml"/>"#,
        );
        if sst_xml.is_some() {
            wb_rels.push_str(
                r#"
    <Relationship Id="rId2"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings"
        Target="sharedStrings.xml"/>"#,
            );
        }
        wb_rels.push_str("\n</Relationships>");

        let wb_rels_bytes = wb_rels.into_bytes();

        let mut entries: Vec<(String, Vec<u8>)> = vec![
            (
                "[Content_Types].xml".to_string(),
                XLSX_CONTENT_TYPES.to_vec(),
            ),
            ("_rels/.rels".to_string(), XLSX_PACKAGE_RELS.to_vec()),
            ("xl/workbook.xml".to_string(), XLSX_WORKBOOK_1SHEET.to_vec()),
            ("xl/_rels/workbook.xml.rels".to_string(), wb_rels_bytes),
            ("xl/worksheets/sheet1.xml".to_string(), sheet_xml.to_vec()),
            (
                "xl/worksheets/_rels/sheet1.xml.rels".to_string(),
                sheet_rels_xml.to_vec(),
            ),
        ];
        if let Some(sst) = sst_xml {
            entries.push(("xl/sharedStrings.xml".to_string(), sst.to_vec()));
        }

        let refs: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(n, d)| (n.as_str(), d.as_slice()))
            .collect();
        build_stored_zip(&refs)
    }

    #[test]
    fn basic_conversion() {
        let sheet = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1"><v>1</v></c>
            <c r="B1"><v>2</v></c>
        </row>
        <row r="2">
            <c r="A2"><v>3</v></c>
            <c r="B2"><v>4</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let data = make_xlsx(sheet);
        let mut xlsx = XlsxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = xlsx_to_document(&mut xlsx, &diag, usize::MAX).unwrap();

        // Should have one table block.
        assert_eq!(result.content.len(), 1);
        assert!(matches!(&result.content[0], Block::Table { .. }));
    }

    #[test]
    fn empty_sheet_no_content() {
        let sheet = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData/>
</worksheet>"#;

        let data = make_xlsx(sheet);
        let mut xlsx = XlsxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = xlsx_to_document(&mut xlsx, &diag, usize::MAX).unwrap();

        assert!(result.content.is_empty());
    }

    #[test]
    fn metadata_preserved() {
        let sheet = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData/>
</worksheet>"#;

        let data = make_xlsx(sheet);
        let mut xlsx = XlsxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = xlsx_to_document(&mut xlsx, &diag, usize::MAX).unwrap();

        assert_eq!(result.metadata.page_count, 1);
    }

    #[test]
    fn test_xlsx_font_color() {
        let styles =
            br#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <fonts count="2">
        <font><sz val="11"/><name val="Calibri"/></font>
        <font><color rgb="FFFF0000"/><sz val="11"/><name val="Calibri"/></font>
    </fonts>
    <cellXfs count="2">
        <xf numFmtId="0" fontId="0"/>
        <xf numFmtId="0" fontId="1"/>
    </cellXfs>
</styleSheet>"#;

        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" s="1"><v>Red</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let data = make_xlsx_with_styles(sheet, styles, None);
        let mut xlsx = XlsxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = xlsx_to_document(&mut xlsx, &diag, usize::MAX).unwrap();

        // Find the inline text node in the table cell.
        let table = match &result.content[0] {
            Block::Table { table, .. } => table,
            other => panic!("expected Table, got {:?}", other),
        };
        let cell = &table.rows[0].cells[0];
        let para = &cell.content[0];
        let inline_id = match para {
            Block::Paragraph { content, .. } => content[0].id(),
            _ => panic!("expected Paragraph"),
        };

        // The text styling overlay should have a red color on the inline node.
        let pres = result.presentation.as_ref().unwrap();
        let style = pres.text_styling.get(inline_id);
        assert!(style.is_some(), "expected text styling on inline text node");
        let style = style.unwrap();
        assert_eq!(style.color, Some(Color::rgb(255, 0, 0)));
    }

    #[test]
    fn test_xlsx_font_name_size() {
        let styles =
            br#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <fonts count="2">
        <font><sz val="11"/><name val="Calibri"/></font>
        <font><sz val="16"/><name val="Helvetica"/></font>
    </fonts>
    <cellXfs count="2">
        <xf numFmtId="0" fontId="0"/>
        <xf numFmtId="0" fontId="1"/>
    </cellXfs>
</styleSheet>"#;

        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" s="1"><v>Big</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let data = make_xlsx_with_styles(sheet, styles, None);
        let mut xlsx = XlsxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = xlsx_to_document(&mut xlsx, &diag, usize::MAX).unwrap();

        let table = match &result.content[0] {
            Block::Table { table, .. } => table,
            _ => panic!("expected Table"),
        };
        let inline_id = match &table.rows[0].cells[0].content[0] {
            Block::Paragraph { content, .. } => content[0].id(),
            _ => panic!("expected Paragraph"),
        };
        let pres = result.presentation.as_ref().unwrap();
        let style = pres.text_styling.get(inline_id).unwrap();
        assert_eq!(style.font_name.as_deref(), Some("Helvetica"));
        assert_eq!(style.font_size, Some(16.0));
    }

    #[test]
    fn test_xlsx_cell_background() {
        let styles =
            br#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <fonts count="1">
        <font><sz val="11"/><name val="Calibri"/></font>
    </fonts>
    <fills count="2">
        <fill><patternFill patternType="none"/></fill>
        <fill><patternFill patternType="solid"><fgColor rgb="FF00FF00"/></patternFill></fill>
    </fills>
    <cellXfs count="2">
        <xf numFmtId="0" fontId="0" fillId="0"/>
        <xf numFmtId="0" fontId="0" fillId="1"/>
    </cellXfs>
</styleSheet>"#;

        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" s="1"><v>Green BG</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let data = make_xlsx_with_styles(sheet, styles, None);
        let mut xlsx = XlsxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = xlsx_to_document(&mut xlsx, &diag, usize::MAX).unwrap();

        let table = match &result.content[0] {
            Block::Table { table, .. } => table,
            _ => panic!("expected Table"),
        };
        let inline_id = match &table.rows[0].cells[0].content[0] {
            Block::Paragraph { content, .. } => content[0].id(),
            _ => panic!("expected Paragraph"),
        };
        let pres = result.presentation.as_ref().unwrap();
        let style = pres.text_styling.get(inline_id).unwrap();
        assert_eq!(style.background_color, Some(Color::rgb(0, 255, 0)));
    }

    #[test]
    fn test_xlsx_cell_alignment() {
        let styles =
            br#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <fonts count="1">
        <font><sz val="11"/><name val="Calibri"/></font>
    </fonts>
    <cellXfs count="2">
        <xf numFmtId="0" fontId="0"/>
        <xf numFmtId="0" fontId="0">
            <alignment horizontal="center"/>
        </xf>
    </cellXfs>
</styleSheet>"#;

        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" s="1"><v>Centered</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let data = make_xlsx_with_styles(sheet, styles, None);
        let mut xlsx = XlsxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = xlsx_to_document(&mut xlsx, &diag, usize::MAX).unwrap();

        let table = match &result.content[0] {
            Block::Table { table, .. } => table,
            _ => panic!("expected Table"),
        };
        let para_id = table.rows[0].cells[0].content[0].id();
        let pres = result.presentation.as_ref().unwrap();
        let layout = pres.block_layout.get(para_id).unwrap();
        assert_eq!(layout.alignment, Some(Alignment::Center));
    }

    #[test]
    fn test_xlsx_hyperlink() {
        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
    xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
    <sheetData>
        <row r="1">
            <c r="A1"><v>Click me</v></c>
        </row>
    </sheetData>
    <hyperlinks>
        <hyperlink ref="A1" r:id="rId1"/>
    </hyperlinks>
</worksheet>"#;

        let sheet_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink"
        Target="https://example.com" TargetMode="External"/>
</Relationships>"#;

        let data = make_xlsx_with_hyperlinks(sheet, None, sheet_rels);
        let mut xlsx = XlsxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = xlsx_to_document(&mut xlsx, &diag, usize::MAX).unwrap();

        let table = match &result.content[0] {
            Block::Table { table, .. } => table,
            _ => panic!("expected Table"),
        };
        let para = &table.rows[0].cells[0].content[0];
        match para {
            Block::Paragraph { content, .. } => {
                assert_eq!(content.len(), 1);
                match &content[0] {
                    Inline::Link { url, content, .. } => {
                        assert_eq!(url, "https://example.com");
                        assert_eq!(content[0].text(), "Click me");
                    }
                    other => panic!("expected Link, got {:?}", other),
                }
            }
            _ => panic!("expected Paragraph"),
        }
    }

    #[test]
    fn test_xlsx_unresolved_hyperlink_warning() {
        use udoc_core::diagnostics::CollectingDiagnostics;

        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
    xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
    <sheetData>
        <row r="1">
            <c r="A1"><v>Click me</v></c>
        </row>
    </sheetData>
    <hyperlinks>
        <hyperlink ref="A1" r:id="rId99"/>
    </hyperlinks>
</worksheet>"#;

        // Empty sheet rels: rId99 will not resolve.
        let sheet_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;

        let data = make_xlsx_with_hyperlinks(sheet, None, sheet_rels);
        let mut xlsx = XlsxDocument::from_bytes(&data).unwrap();
        let diag = CollectingDiagnostics::new();
        let _result = xlsx_to_document(&mut xlsx, &diag, usize::MAX).unwrap();

        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.kind == "XlsxUnresolvedHyperlink"),
            "expected XlsxUnresolvedHyperlink warning, got: {:?}",
            warnings.iter().map(|w| &w.kind).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_xlsx_rich_text() {
        let styles =
            br#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <fonts count="1">
        <font><sz val="11"/><name val="Calibri"/></font>
    </fonts>
    <cellXfs count="1">
        <xf numFmtId="0" fontId="0"/>
    </cellXfs>
</styleSheet>"#;

        let sst = br#"<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <si>
        <r><rPr><b/></rPr><t>Bold</t></r>
        <r><rPr><i/></rPr><t> Italic</t></r>
        <r><t> Plain</t></r>
    </si>
</sst>"#;

        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" t="s"><v>0</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let data = make_xlsx_with_styles(sheet, styles, Some(sst));
        let mut xlsx = XlsxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = xlsx_to_document(&mut xlsx, &diag, usize::MAX).unwrap();

        let table = match &result.content[0] {
            Block::Table { table, .. } => table,
            _ => panic!("expected Table"),
        };
        let para = &table.rows[0].cells[0].content[0];
        match para {
            Block::Paragraph { content, .. } => {
                assert_eq!(content.len(), 3, "expected 3 inline runs");

                // Run 1: Bold
                match &content[0] {
                    Inline::Text { text, style, .. } => {
                        assert_eq!(text, "Bold");
                        assert!(style.bold);
                        assert!(!style.italic);
                    }
                    other => panic!("expected Text, got {:?}", other),
                }

                // Run 2: Italic
                match &content[1] {
                    Inline::Text { text, style, .. } => {
                        assert_eq!(text, " Italic");
                        assert!(!style.bold);
                        assert!(style.italic);
                    }
                    other => panic!("expected Text, got {:?}", other),
                }

                // Run 3: Plain
                match &content[2] {
                    Inline::Text { text, style, .. } => {
                        assert_eq!(text, " Plain");
                        assert!(!style.bold);
                        assert!(!style.italic);
                    }
                    other => panic!("expected Text, got {:?}", other),
                }
            }
            _ => panic!("expected Paragraph"),
        }
    }

    #[test]
    fn test_xlsx_bold_italic_underline_strike() {
        let styles =
            br#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <fonts count="2">
        <font><sz val="11"/><name val="Calibri"/></font>
        <font><b/><i/><u/><strike/><sz val="11"/><name val="Calibri"/></font>
    </fonts>
    <cellXfs count="2">
        <xf numFmtId="0" fontId="0"/>
        <xf numFmtId="0" fontId="1"/>
    </cellXfs>
</styleSheet>"#;

        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" s="1"><v>Styled</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let data = make_xlsx_with_styles(sheet, styles, None);
        let mut xlsx = XlsxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = xlsx_to_document(&mut xlsx, &diag, usize::MAX).unwrap();

        let table = match &result.content[0] {
            Block::Table { table, .. } => table,
            _ => panic!("expected Table"),
        };
        let para = &table.rows[0].cells[0].content[0];
        match para {
            Block::Paragraph { content, .. } => match &content[0] {
                Inline::Text { style, .. } => {
                    assert!(style.bold);
                    assert!(style.italic);
                    assert!(style.underline);
                    assert!(style.strikethrough);
                }
                other => panic!("expected Text, got {:?}", other),
            },
            _ => panic!("expected Paragraph"),
        }
    }

    #[test]
    fn plain_sst_cell_inherits_font_span_style() {
        // Regression: cells referencing a Plain SST entry should still pick up
        // bold/italic/underline/strikethrough from the cell's font style.
        let styles =
            br#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <fonts count="2">
        <font><sz val="11"/><name val="Calibri"/></font>
        <font><b/><i/><sz val="11"/><name val="Calibri"/></font>
    </fonts>
    <cellXfs count="2">
        <xf numFmtId="0" fontId="0"/>
        <xf numFmtId="0" fontId="1"/>
    </cellXfs>
</styleSheet>"#;

        let sst = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="1" uniqueCount="1">
    <si><t>Hello</t></si>
</sst>"#;

        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" s="1" t="s"><v>0</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let data = make_xlsx_with_styles(sheet, styles, Some(sst));
        let mut xlsx = XlsxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = xlsx_to_document(&mut xlsx, &diag, usize::MAX).unwrap();

        let table = match &result.content[0] {
            Block::Table { table, .. } => table,
            _ => panic!("expected Table"),
        };
        let para = &table.rows[0].cells[0].content[0];
        match para {
            Block::Paragraph { content, .. } => match &content[0] {
                Inline::Text { style, text, .. } => {
                    assert_eq!(text, "Hello");
                    assert!(style.bold, "plain SST cell should inherit bold from font");
                    assert!(
                        style.italic,
                        "plain SST cell should inherit italic from font"
                    );
                }
                other => panic!("expected Text, got {:?}", other),
            },
            _ => panic!("expected Paragraph"),
        }
    }

    #[test]
    fn column_widths_flow_to_col_specs() {
        let sheet = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <cols>
        <col min="1" max="1" width="10" customWidth="1"/>
        <col min="2" max="2" width="20" customWidth="1"/>
    </cols>
    <sheetData>
        <row r="1">
            <c r="A1"><v>1</v></c>
            <c r="B1"><v>2</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let data = make_xlsx(sheet);
        let mut xlsx = XlsxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = xlsx_to_document(&mut xlsx, &diag, usize::MAX).unwrap();

        // Find the table node ID.
        let table_id = match &result.content[0] {
            Block::Table { id, .. } => *id,
            other => panic!("expected Table, got {:?}", other),
        };

        // Column specs should be set on the table node in the presentation overlay.
        let pres = result
            .presentation
            .as_ref()
            .expect("presentation overlay should exist");
        let col_specs = pres
            .column_specs
            .get(table_id)
            .expect("column_specs should be set on table node");

        assert_eq!(col_specs.len(), 2);
        // Column A: 10 char units * 7.5 = 75.0 points.
        assert!(
            (col_specs[0].width.unwrap() - 75.0).abs() < f64::EPSILON,
            "expected 75.0, got {:?}",
            col_specs[0].width
        );
        // Column B: 20 char units * 7.5 = 150.0 points.
        assert!(
            (col_specs[1].width.unwrap() - 150.0).abs() < f64::EPSILON,
            "expected 150.0, got {:?}",
            col_specs[1].width
        );
    }

    #[test]
    fn no_cols_section_no_col_specs() {
        let sheet = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1"><v>1</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let data = make_xlsx(sheet);
        let mut xlsx = XlsxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = xlsx_to_document(&mut xlsx, &diag, usize::MAX).unwrap();

        // No column widths means no presentation overlay (unless other styling exists).
        if let Some(pres) = &result.presentation {
            let table_id = match &result.content[0] {
                Block::Table { id, .. } => *id,
                _ => panic!("expected Table"),
            };
            assert!(
                pres.column_specs.get(table_id).is_none(),
                "column_specs should not be set when no <cols> section"
            );
        }
    }
}
