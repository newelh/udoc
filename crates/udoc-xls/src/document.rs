//! XlsDocument and page types implementing FormatBackend/PageExtractor.
//!
//! `XlsDocument` opens an XLS file via CFB, parses the Workbook stream
//! BIFF8 globals and SST, then lazily parses each sheet's cells on demand.
//! Each sheet is a logical "page" in the FormatBackend sense.

use std::collections::HashMap;
use std::sync::Arc;

use udoc_containers::cfb::summary_info::SUMMARY_INFO_STREAM_NAME;
use udoc_containers::cfb::{parse_summary_information, CfbArchive};
use udoc_core::backend::{DocumentMetadata, FormatBackend, PageExtractor};
use udoc_core::diagnostics::{DiagnosticsSink, Warning};
use udoc_core::image::PageImage;
use udoc_core::table::{Table, TableCell, TableRow};
use udoc_core::text::{TextLine, TextSpan};

use crate::cells::{parse_sheet_cells, Cell, CellValue, MergedRange};
use crate::error::{Error, Result, ResultExt};
use crate::formats::format_cell_value;
use crate::records::{BiffReader, RT_BOF, RT_SST};
use crate::sst::parse_sst;
use crate::workbook::{parse_globals, SheetInfo, WorkbookGlobals};
use crate::MAX_FILE_SIZE;

// ---------------------------------------------------------------------------
// XlsDocument
// ---------------------------------------------------------------------------

/// A parsed XLS (BIFF8) workbook ready for content extraction.
///
/// Implements `FormatBackend` where each worksheet is a logical page.
/// Non-worksheet entries (charts, VBA modules) are silently excluded from
/// the page count.
pub struct XlsDocument {
    /// Parsed workbook globals: sheet info, formats, xf table, codepage, epoch.
    globals: WorkbookGlobals,
    /// Shared String Table (SST) entries.
    sst: Vec<String>,
    /// The raw Workbook stream bytes, kept alive for lazy sheet parsing.
    workbook_data: Vec<u8>,
    /// Document metadata extracted from the SummaryInformation stream (if any).
    metadata: DocumentMetadata,
    /// Diagnostics sink for parse warnings.
    diag: Arc<dyn DiagnosticsSink>,
}

impl XlsDocument {
    udoc_core::define_backend_constructors!(MAX_FILE_SIZE, "XLS");

    /// Parse an XLS document from in-memory bytes with a diagnostics sink.
    pub fn from_bytes_with_diag(data: &[u8], diag: Arc<dyn DiagnosticsSink>) -> Result<Self> {
        if data.len() as u64 > MAX_FILE_SIZE {
            return Err(Error::new(format!(
                "XLS file too large ({} bytes, max {} bytes)",
                data.len(),
                MAX_FILE_SIZE
            )));
        }

        // Open the CFB container.
        let cfb = CfbArchive::new(data, diag.clone()).context("opening CFB container")?;

        // Try "Workbook" stream first, fall back to "Book".
        let workbook_data = if let Some(entry) = cfb.find("Workbook") {
            cfb.read(entry).context("reading Workbook stream")?
        } else if let Some(entry) = cfb.find("Book") {
            diag.warning(Warning::new(
                "workbook_stream_fallback",
                "Workbook stream not found, falling back to Book stream (BIFF5/BIFF4 file?)",
            ));
            cfb.read(entry).context("reading Book stream")?
        } else {
            return Err(Error::new(
                "no Workbook or Book stream found in CFB container",
            ));
        };

        // Collect all records in a single pass so we can locate the SST and
        // parse globals in one sweep.
        let (globals, sst) = parse_workbook_stream(&workbook_data, &*diag)?;

        // Extract metadata from SummaryInformation if present.
        let metadata = extract_metadata(&cfb, globals.sheets.len());

        Ok(Self {
            globals,
            sst,
            workbook_data,
            metadata,
            diag,
        })
    }

    /// The workbook globals (formats, xf table, sheets, etc.).
    #[allow(dead_code)]
    pub(crate) fn globals(&self) -> &WorkbookGlobals {
        &self.globals
    }

    /// The Shared String Table entries.
    #[allow(dead_code)]
    pub(crate) fn sst(&self) -> &[String] {
        &self.sst
    }

    /// Parse cells for the sheet at `sheet_index` (into the globals.sheets vec).
    ///
    /// Seeks to the sheet's lbPlyPos in the Workbook stream and calls
    /// `parse_sheet_cells`. Returns empty vecs on seek failure.
    pub(crate) fn parse_sheet_cells_for_index(
        &self,
        sheet_index: usize,
    ) -> (Vec<Cell>, Vec<MergedRange>) {
        let sheet = match self.globals.sheets.get(sheet_index) {
            Some(s) => s,
            None => return (Vec::new(), Vec::new()),
        };
        let offset = sheet.offset as usize;
        if offset >= self.workbook_data.len() {
            self.diag.warning(Warning::new(
                "sheet_offset_out_of_bounds",
                format!(
                    "sheet {} offset {offset} is out of bounds (stream len {})",
                    sheet.name,
                    self.workbook_data.len()
                ),
            ));
            return (Vec::new(), Vec::new());
        }

        let mut reader = BiffReader::new(&self.workbook_data, &*self.diag);
        reader.seek(offset);

        // Consume the sheet BOF record.
        match reader.next_record() {
            Ok(Some(rec)) if rec.record_type == RT_BOF => {
                // BOF consumed; cells follow.
            }
            Ok(Some(rec)) => {
                self.diag.warning(Warning::new(
                    "sheet_expected_bof",
                    format!(
                        "expected BOF at sheet offset {offset}, got record type {:#06x}",
                        rec.record_type
                    ),
                ));
                return (Vec::new(), Vec::new());
            }
            Ok(None) => {
                self.diag.warning(Warning::new(
                    "sheet_stream_empty_at_offset",
                    format!("stream ended at sheet offset {offset}"),
                ));
                return (Vec::new(), Vec::new());
            }
            Err(e) => {
                self.diag.warning(Warning::new(
                    "sheet_bof_read_error",
                    format!("error reading sheet BOF at offset {offset}: {e}"),
                ));
                return (Vec::new(), Vec::new());
            }
        }

        match parse_sheet_cells(&mut reader, &self.sst, &*self.diag) {
            Ok((cells, merged_ranges)) => (cells, merged_ranges),
            Err(e) => {
                self.diag.warning(Warning::new(
                    "sheet_cell_parse_error",
                    format!("error parsing cells for sheet {}: {e}", sheet.name),
                ));
                (Vec::new(), Vec::new())
            }
        }
    }

    /// Returns the sheet info for worksheets only (the pages).
    #[allow(dead_code)]
    pub(crate) fn worksheet_sheets(&self) -> &[SheetInfo] {
        &self.globals.sheets
    }
}

// ---------------------------------------------------------------------------
// Workbook stream single-pass parse
// ---------------------------------------------------------------------------

/// Parse the Workbook stream once, extracting globals and SST.
///
/// We do two logical things:
/// 1. Hand the reader to `parse_globals` which consumes BOF + workbook-level
///    records through EOF.
/// 2. Scan the same stream again for an SST record (parse_globals skips it).
///
/// The SST must live in the globals substream (before the first sheet BOF).
/// We can re-read the same bytes after parse_globals finishes, because the
/// globals EOF record terminates parsing before the first sheet BOF.
fn parse_workbook_stream(
    data: &[u8],
    diag: &dyn DiagnosticsSink,
) -> Result<(WorkbookGlobals, Vec<String>)> {
    // First pass: globals (BOF, BOUNDSHEET, FORMAT, XF, CODEPAGE, DATEMODE).
    let mut reader = BiffReader::new(data, diag);
    let globals = parse_globals(&mut reader, diag)?;

    // Second pass: scan from the beginning for the SST record.
    // The SST lives in the globals substream and will appear before any
    // sheet BOF, so we stop at the first sheet BOF or EOF we hit.
    let sst = scan_for_sst(data, diag);

    Ok((globals, sst))
}

/// Scan the beginning of the Workbook stream for an SST record.
///
/// Reads records until it finds RT_SST, a sheet-type BOF (dt != 0x0005),
/// or EOF. Returns an empty Vec if no SST is found.
fn scan_for_sst(data: &[u8], diag: &dyn DiagnosticsSink) -> Vec<String> {
    use crate::records::{RT_EOF, RT_EXTSST};
    use crate::workbook::BOF_DT_WORKSHEET;

    let mut reader = BiffReader::new(data, diag);

    // Skip the first BOF (globals BOF).
    match reader.next_record() {
        Ok(Some(rec)) if rec.record_type == RT_BOF => {}
        _ => return Vec::new(),
    }

    loop {
        let rec = match reader.next_record() {
            Ok(Some(r)) => r,
            _ => return Vec::new(),
        };

        match rec.record_type {
            RT_EOF => return Vec::new(),

            RT_SST => {
                return parse_sst(&rec, diag).unwrap_or_else(|e| {
                    diag.warning(Warning::new(
                        "sst_parse_error",
                        format!("failed to parse SST: {e}"),
                    ));
                    Vec::new()
                });
            }

            // EXTSST is always paired with SST; skip it.
            RT_EXTSST => continue,

            RT_BOF => {
                // A second BOF means we've entered a sheet substream. The SST
                // should have appeared earlier; give up.
                if rec.data.len() >= 4 {
                    let dt = u16::from_le_bytes([rec.data[2], rec.data[3]]);
                    if dt == BOF_DT_WORKSHEET {
                        return Vec::new();
                    }
                }
                // Unknown BOF type; keep scanning.
            }

            _ => continue,
        }
    }
}

// ---------------------------------------------------------------------------
// Metadata extraction
// ---------------------------------------------------------------------------

/// Extract DocumentMetadata from the SummaryInformation stream in the CFB.
fn extract_metadata(cfb: &CfbArchive<'_>, sheet_count: usize) -> DocumentMetadata {
    let mut meta = DocumentMetadata::with_page_count(sheet_count);

    if let Some(entry) = cfb.find(SUMMARY_INFO_STREAM_NAME) {
        if let Ok(data) = cfb.read(entry) {
            if let Ok(info) = parse_summary_information(&data) {
                meta.title = info.title;
                meta.author = info.author;
            }
        }
    }

    meta
}

// ---------------------------------------------------------------------------
// FormatBackend impl
// ---------------------------------------------------------------------------

impl FormatBackend for XlsDocument {
    type Page<'a> = XlsPage<'a>;

    fn page_count(&self) -> usize {
        self.globals.sheets.len()
    }

    fn page(&mut self, index: usize) -> Result<XlsPage<'_>> {
        if index >= self.globals.sheets.len() {
            return Err(Error::new(format!(
                "sheet {index} out of range (workbook has {} sheets)",
                self.globals.sheets.len()
            )));
        }
        let (cells, merged_ranges) = self.parse_sheet_cells_for_index(index);
        Ok(XlsPage {
            cells,
            merged_ranges,
            formats: &self.globals.formats,
            xf_table: &self.globals.xf_table,
            date_1904: self.globals.date_1904,
        })
    }

    fn metadata(&self) -> DocumentMetadata {
        self.metadata.clone()
    }
}

// ---------------------------------------------------------------------------
// XlsPage + PageExtractor
// ---------------------------------------------------------------------------

/// A page view for extracting content from a single XLS worksheet.
pub struct XlsPage<'a> {
    cells: Vec<Cell>,
    /// Merged cell ranges from MERGEDCELLS records in this sheet.
    merged_ranges: Vec<MergedRange>,
    formats: &'a HashMap<u16, String>,
    xf_table: &'a [crate::workbook::XfEntry],
    date_1904: bool,
}

impl<'a> XlsPage<'a> {
    /// Format a cell's value to a display string.
    fn format_cell(&self, cell: &Cell) -> Option<String> {
        match &cell.value {
            CellValue::String(s) => {
                if s.is_empty() {
                    None
                } else {
                    Some(s.clone())
                }
            }
            CellValue::Number(n) => {
                let ifmt = self
                    .xf_table
                    .get(cell.ixfe as usize)
                    .map(|xf| xf.ifmt)
                    .unwrap_or(0);
                Some(format_cell_value(*n, ifmt, self.formats, self.date_1904))
            }
            CellValue::Bool(b) => Some(if *b {
                "TRUE".to_string()
            } else {
                "FALSE".to_string()
            }),
            CellValue::Error(code) => Some(format!("#ERR{code}")),
            CellValue::Empty => None,
        }
    }

    /// Build rows: a sorted map from row -> (col -> display string).
    fn build_rows(&self) -> Vec<(u16, Vec<(u16, String)>)> {
        // Collect all non-empty formatted cells.
        let mut row_map: HashMap<u16, Vec<(u16, String)>> = HashMap::new();
        for cell in &self.cells {
            if let Some(text) = self.format_cell(cell) {
                row_map.entry(cell.row).or_default().push((cell.col, text));
            }
        }

        // Sort rows, and within each row sort by column.
        let mut rows: Vec<(u16, Vec<(u16, String)>)> = row_map.into_iter().collect();
        rows.sort_by_key(|(r, _)| *r);
        for (_, cols) in &mut rows {
            cols.sort_by_key(|(c, _)| *c);
        }
        rows
    }
}

impl<'a> PageExtractor for XlsPage<'a> {
    fn text(&mut self) -> Result<String> {
        let rows = self.build_rows();
        let mut lines: Vec<String> = Vec::with_capacity(rows.len());
        for (_row_idx, cols) in &rows {
            let parts: Vec<&str> = cols.iter().map(|(_, s)| s.as_str()).collect();
            lines.push(parts.join("\t"));
        }
        Ok(lines.join("\n"))
    }

    fn text_lines(&mut self) -> Result<Vec<TextLine>> {
        let rows = self.build_rows();
        let mut lines: Vec<TextLine> = Vec::with_capacity(rows.len());
        for (row_idx, cols) in &rows {
            let spans: Vec<TextSpan> = cols
                .iter()
                .enumerate()
                .map(|(col_pos, (_col_idx, text))| {
                    // No geometry for XLS cells; use synthetic positions.
                    TextSpan::new(
                        text.clone(),
                        col_pos as f64,
                        *row_idx as f64,
                        text.len() as f64,
                        1.0,
                    )
                })
                .collect();
            if !spans.is_empty() {
                lines.push(TextLine::new(spans, *row_idx as f64, false));
            }
        }
        Ok(lines)
    }

    fn raw_spans(&mut self) -> Result<Vec<TextSpan>> {
        let mut spans = Vec::new();
        for cell in &self.cells {
            if let Some(text) = self.format_cell(cell) {
                spans.push(TextSpan::new(
                    text,
                    cell.col as f64,
                    cell.row as f64,
                    1.0,
                    1.0,
                ));
            }
        }
        Ok(spans)
    }

    fn tables(&mut self) -> Result<Vec<Table>> {
        let rows = self.build_rows();
        if rows.is_empty() {
            return Ok(Vec::new());
        }

        // Find the column range.
        let max_col = rows
            .iter()
            .flat_map(|(_, cols)| cols.iter().map(|(c, _)| *c))
            .max()
            .unwrap_or(0) as usize
            + 1;

        // Build merge lookup structures.
        //
        // merge_topleft: (first_row, first_col) -> (col_span, row_span)
        // merge_covered: set of (row, col) positions covered by a merge but
        //                NOT the top-left cell. These cells are omitted from
        //                their TableRow so the top-left span accounts for them.
        let mut merge_topleft: HashMap<(u16, u16), (usize, usize)> = HashMap::new();
        let mut merge_covered: std::collections::HashSet<(u16, u16)> = Default::default();
        for mr in &self.merged_ranges {
            let col_span = mr.col_span();
            let row_span = mr.row_span();
            if row_span * col_span > 100_000 {
                // Pathological merge: skip to avoid O(n^2) HashSet inflation.
                // A legitimate merge will never span 100K cells.
                continue;
            }
            merge_topleft.insert((mr.first_row, mr.first_col), (col_span, row_span));
            for r in mr.first_row..=mr.last_row {
                for c in mr.first_col..=mr.last_col {
                    if r != mr.first_row || c != mr.first_col {
                        merge_covered.insert((r, c));
                    }
                }
            }
        }

        // Build table rows.
        let mut table_rows: Vec<TableRow> = Vec::with_capacity(rows.len());
        for (row_idx, cols) in &rows {
            let mut col_map: HashMap<u16, &str> = HashMap::new();
            for (c, s) in cols {
                col_map.insert(*c, s.as_str());
            }

            let mut cells: Vec<TableCell> = Vec::with_capacity(max_col);
            let mut c = 0usize;
            while c < max_col {
                let col = c as u16;

                if merge_covered.contains(&(*row_idx, col)) {
                    // This position is the non-top-left part of a merged range.
                    // Skip it -- the top-left cell in an earlier row/col already
                    // claims these positions via its row_span/col_span.
                    c += 1;
                    continue;
                }

                let text = col_map.get(&col).copied().unwrap_or("").to_string();

                if let Some(&(col_span, row_span)) = merge_topleft.get(&(*row_idx, col)) {
                    cells.push(TableCell::with_spans(text, None, col_span, row_span));
                    // Advance past all columns consumed by this merge's col_span.
                    c += col_span;
                } else {
                    cells.push(TableCell::new(text, None));
                    c += 1;
                }
            }
            table_rows.push(TableRow::new(cells));
        }

        Ok(vec![Table::new(table_rows, None)])
    }

    fn images(&mut self) -> Result<Vec<PageImage>> {
        Ok(Vec::new())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{build_biff_record, build_minimal_xls};
    use udoc_core::backend::PageExtractor;
    use udoc_core::diagnostics::CollectingDiagnostics;

    fn null_diag() -> Arc<dyn DiagnosticsSink> {
        Arc::new(udoc_core::diagnostics::NullDiagnostics)
    }

    #[allow(dead_code)]
    fn collecting_diag() -> Arc<CollectingDiagnostics> {
        Arc::new(CollectingDiagnostics::new())
    }

    // -- Test 1: minimal valid XLS with 1 sheet and 1 string cell --------

    #[test]
    fn single_sheet_one_string_cell() {
        let data = build_minimal_xls(&["hello"], &[("Sheet1", &[(0, 0, "hello")])]);
        let doc = XlsDocument::from_bytes_with_diag(&data, null_diag())
            .expect("should parse minimal XLS");

        assert_eq!(FormatBackend::page_count(&doc), 1);
    }

    // -- Test 2: page_count correct for multi-sheet workbook --------------

    #[test]
    fn multi_sheet_page_count() {
        let data = build_minimal_xls(
            &["a", "b"],
            &[("Sheet1", &[(0, 0, "a")]), ("Sheet2", &[(0, 0, "b")])],
        );
        let mut doc = XlsDocument::from_bytes_with_diag(&data, null_diag())
            .expect("should parse multi-sheet XLS");
        assert_eq!(FormatBackend::page_count(&doc), 2);

        let mut page0 = doc.page(0).unwrap();
        assert_eq!(page0.text().unwrap(), "a");

        let mut page1 = doc.page(1).unwrap();
        assert_eq!(page1.text().unwrap(), "b");
    }

    // -- Test 3: text() output with tab-separated cells -------------------

    #[test]
    fn text_output_tab_separated() {
        let data = build_minimal_xls(
            &["Name", "Value"],
            &[("Sheet1", &[(0, 0, "Name"), (0, 1, "Value")])],
        );
        let mut doc = XlsDocument::from_bytes_with_diag(&data, null_diag()).expect("should parse");
        let mut page = doc.page(0).unwrap();
        let text = page.text().unwrap();
        assert_eq!(text, "Name\tValue");
    }

    // -- Test 4: empty workbook (no sheets) -------------------------------

    #[test]
    fn empty_workbook_no_sheets() {
        let data = build_minimal_xls(&[], &[]);
        let doc =
            XlsDocument::from_bytes_with_diag(&data, null_diag()).expect("should parse empty XLS");
        assert_eq!(FormatBackend::page_count(&doc), 0);
    }

    // -- Test 5: metadata returns sensible values -------------------------

    #[test]
    fn metadata_page_count_matches_sheet_count() {
        let data = build_minimal_xls(&["x"], &[("Sheet1", &[(0, 0, "x")]), ("Sheet2", &[])]);
        let doc = XlsDocument::from_bytes_with_diag(&data, null_diag()).expect("should parse");
        let meta = FormatBackend::metadata(&doc);
        assert_eq!(meta.page_count, 2);
    }

    // -- Test 6: tables() returns one table with the cells ----------------

    #[test]
    fn tables_output() {
        let data = build_minimal_xls(
            &["A", "B", "C", "D"],
            &[(
                "Sheet1",
                &[(0, 0, "A"), (0, 1, "B"), (1, 0, "C"), (1, 1, "D")],
            )],
        );
        let mut doc = XlsDocument::from_bytes_with_diag(&data, null_diag()).expect("should parse");
        let mut page = doc.page(0).unwrap();
        let tables = page.tables().unwrap();
        assert_eq!(tables.len(), 1);
        let table = &tables[0];
        assert_eq!(table.rows.len(), 2);
        // First row: A, B
        assert_eq!(table.rows[0].cells[0].text, "A");
        assert_eq!(table.rows[0].cells[1].text, "B");
        // Second row: C, D
        assert_eq!(table.rows[1].cells[0].text, "C");
        assert_eq!(table.rows[1].cells[1].text, "D");
    }

    // -- Test 7: images() always returns empty ----------------------------

    #[test]
    fn images_always_empty() {
        let data = build_minimal_xls(&["x"], &[("Sheet1", &[(0, 0, "x")])]);
        let mut doc = XlsDocument::from_bytes_with_diag(&data, null_diag()).unwrap();
        let mut page = doc.page(0).unwrap();
        assert!(page.images().unwrap().is_empty());
    }

    // -- Test 8: non-CFB data returns an error ----------------------------

    #[test]
    fn non_cfb_data_returns_error() {
        let garbage = b"this is not a CFB file at all";
        let result = XlsDocument::from_bytes_with_diag(garbage, null_diag());
        assert!(result.is_err());
    }

    // -- Test 9: out-of-range page index returns an error -----------------

    #[test]
    fn out_of_range_page_index_returns_error() {
        let data = build_minimal_xls(&[], &[]);
        let mut doc = XlsDocument::from_bytes_with_diag(&data, null_diag()).unwrap();
        assert!(doc.page(0).is_err());
    }

    // -- Test 10: build_biff_record helper produces parseable records -----

    #[test]
    fn build_biff_record_produces_valid_header() {
        let rec = build_biff_record(0x0809, &[0x00, 0x06, 0x05, 0x00]);
        assert_eq!(rec.len(), 8); // 4-byte header + 4-byte body
        assert_eq!(&rec[0..2], &0x0809u16.to_le_bytes());
        assert_eq!(&rec[2..4], &4u16.to_le_bytes());
    }
}
