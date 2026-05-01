//! XLSX document abstraction and trait implementations.
//!
//! Provides `XlsxDocument` for opening and extracting content from XLSX files.
//! Implements `FormatBackend` and `PageExtractor` from udoc-core.
//!
//! One sheet = one logical page. `page_count()` returns the number
//! of sheets. Cell values are formatted display strings.

use std::sync::Arc;

use udoc_core::backend::{DocumentMetadata, FormatBackend, PageExtractor};
use udoc_core::diagnostics::{DiagnosticsSink, Warning};
use udoc_core::image::PageImage;
use udoc_core::table::{Table, TableCell, TableRow};
use udoc_core::text::{TextLine, TextSpan};

use crate::error::{Error, Result, ResultExt};
use crate::merge::{build_merge_cache, MergeCache};
use crate::shared_strings::{parse_shared_strings, SharedStringEntry};
use crate::sheet::{parse_sheet, SheetData};
use crate::styles::{parse_styles, StyleSheet};
use crate::workbook::{parse_workbook, SheetVisibility, WorkbookInfo};
use udoc_containers::opc::{rel_types, OpcPackage, Relationship};

use crate::MAX_FILE_SIZE;

/// Maximum grid area (rows * cols) for full-grid iteration in text()/tables().
/// Prevents DoS from sparse sheets (e.g., one cell at A1 and one at XFD1000000).
/// Sheets exceeding this area fall back to cell-only iteration without gap filling.
const MAX_GRID_AREA: usize = 20_000_000;

/// Top-level XLSX document handle.
pub struct XlsxDocument {
    /// Parsed workbook info (sheet names, ordering, epoch).
    workbook: WorkbookInfo,
    /// Shared strings table (rich text entries preserving per-run formatting).
    /// Flat text per entry lives in `shared_string_flat` so we do not rebuild
    /// a `Vec<String>` on every sheet parse.
    shared_string_entries: Vec<SharedStringEntry>,
    /// Flat `Arc<str>` view over `shared_string_entries`, built once and
    /// shared across every lazy sheet parse. Plain entries share their
    /// allocation with the SST (refcount only); rich entries synthesize
    /// a concatenated `Arc<str>` once, amortized across every reference.
    /// This replaces the prior per-parse derivation that cloned every
    /// entry text into a fresh `Vec<String>`.
    shared_string_flat: Vec<Arc<str>>,
    /// Cell styles and number formats.
    stylesheet: StyleSheet,
    /// Parsed sheet data, lazily populated from raw_sheets. Index = sheet index.
    sheets: Vec<Option<SheetData>>,
    /// Raw sheet XML bytes, read eagerly during construction so we never
    /// need to re-open the OPC/ZIP package.
    raw_sheets: Vec<Vec<u8>>,
    /// Per-sheet OPC relationships (for hyperlink r:id resolution).
    sheet_rels: Vec<Vec<Relationship>>,
    /// Document metadata from docProps/core.xml.
    metadata: DocumentMetadata,
    /// Diagnostics sink.
    diag: Arc<dyn DiagnosticsSink>,
}

impl XlsxDocument {
    udoc_core::define_backend_constructors!(MAX_FILE_SIZE, "XLSX");

    /// Parse XLSX from in-memory bytes with a diagnostics sink.
    pub fn from_bytes_with_diag(data: &[u8], diag: Arc<dyn DiagnosticsSink>) -> Result<Self> {
        let pkg =
            OpcPackage::new(data, Arc::clone(&diag)).context("opening XLSX as OPC package")?;

        // Find the workbook part via package relationships.
        let wb_rel = pkg
            .find_package_rel_by_type(rel_types::OFFICE_DOCUMENT)
            .ok_or_else(|| Error::new("XLSX missing officeDocument relationship"))?;
        let wb_target = wb_rel.target.clone();

        // Parse workbook.xml.
        let wb_data = pkg.read_part(&wb_target).context("reading workbook.xml")?;
        let workbook = parse_workbook(&wb_data, &diag).context("parsing workbook.xml")?;

        // Resolve sheet targets via workbook relationships.
        let wb_part = format!("/{}", wb_target.trim_start_matches('/'));

        // Parse shared strings table (optional, some files don't have one).
        let shared_string_entries =
            match pkg.find_part_rel_by_type(&wb_part, rel_types::SHARED_STRINGS) {
                Some(sst_rel) => {
                    let sst_uri = pkg.resolve_uri(&wb_part, &sst_rel.target);
                    let sst_data = pkg.read_part(&sst_uri).context("reading shared strings")?;
                    parse_shared_strings(&sst_data, &diag).context("parsing shared strings")?
                }
                None => Vec::new(),
            };

        // Parse styles (optional).
        let stylesheet = match pkg.find_part_rel_by_type(&wb_part, rel_types::STYLES) {
            Some(styles_rel) => {
                let styles_uri = pkg.resolve_uri(&wb_part, &styles_rel.target);
                let styles_data = pkg.read_part(&styles_uri).context("reading styles.xml")?;
                parse_styles(&styles_data, &diag).context("parsing styles.xml")?
            }
            None => StyleSheet::default(),
        };

        // Read all sheet XML bytes eagerly so we can drop the OPC package
        // (and the raw ZIP data) immediately. This avoids re-opening the
        // ZIP on every lazy sheet parse.
        let wb_rels = pkg.part_rels(&wb_part);
        let mut raw_sheets = Vec::with_capacity(workbook.sheets.len());
        let mut per_sheet_rels: Vec<Vec<Relationship>> = Vec::with_capacity(workbook.sheets.len());
        for sheet_entry in &workbook.sheets {
            let (sheet_bytes, s_rels) = match wb_rels.iter().find(|r| r.id == sheet_entry.r_id) {
                Some(rel) => {
                    let uri = pkg.resolve_uri(&wb_part, &rel.target);
                    let data = pkg
                        .read_part(&uri)
                        .context(format!("reading sheet '{}'", sheet_entry.name))?;
                    // Read sheet-level relationships (for hyperlinks).
                    let sheet_part = format!("/{}", uri.trim_start_matches('/'));
                    let rels = pkg.part_rels(&sheet_part).to_vec();
                    (data, rels)
                }
                None => {
                    diag.warning(Warning::new(
                        "XlsxMissingSheetRel",
                        format!(
                            "no relationship found for sheet '{}' (r:id={})",
                            sheet_entry.name, sheet_entry.r_id
                        ),
                    ));
                    (Vec::new(), Vec::new())
                }
            };
            raw_sheets.push(sheet_bytes);
            per_sheet_rels.push(s_rels);
        }

        // Parse metadata from docProps/core.xml before dropping the package.
        let mut metadata = match pkg.find_package_rel_by_type(rel_types::CORE_PROPERTIES) {
            Some(rel) => match pkg.read_part(&rel.target) {
                Ok(core_xml) => udoc_containers::opc::metadata::parse_core_properties(&core_xml),
                Err(e) => {
                    diag.warning(Warning::new(
                        "XlsxMetadataReadFailed",
                        format!("could not read docProps/core.xml: {e}, using defaults"),
                    ));
                    DocumentMetadata::default()
                }
            },
            None => DocumentMetadata::default(),
        };
        metadata.page_count = workbook.sheets.len();

        let sheet_count = workbook.sheets.len();

        // OPC package (and its borrow of `data`) is dropped here.
        drop(pkg);

        // Build the flat `Arc<str>` view once. Plain entries share their
        // allocation with the SST (refcount bump only). Rich entries
        // synthesize a concatenated Arc<str>, amortized across all sheets.
        let shared_string_flat: Vec<Arc<str>> =
            shared_string_entries.iter().map(|e| e.text_arc()).collect();

        Ok(Self {
            workbook,
            shared_string_entries,
            shared_string_flat,
            stylesheet,
            sheets: (0..sheet_count).map(|_| None).collect(),
            raw_sheets,
            sheet_rels: per_sheet_rels,
            metadata,
            diag,
        })
    }

    /// Parse a specific sheet's data lazily.
    fn ensure_sheet_parsed(&mut self, index: usize) -> Result<()> {
        if index >= self.sheets.len() {
            return Err(Error::new(format!(
                "sheet {index} out of range (have {})",
                self.sheets.len()
            )));
        }

        if self.sheets[index].is_some() {
            return Ok(());
        }

        let sheet_entry = &self.workbook.sheets[index];

        // Log info for hidden sheets.
        if sheet_entry.visibility != SheetVisibility::Visible {
            self.diag.info(Warning::new(
                "XlsxHiddenSheet",
                format!(
                    "extracting {:?} sheet: {}",
                    sheet_entry.visibility, sheet_entry.name
                ),
            ));
        }

        // Use the pre-read sheet XML bytes.
        let raw_xml = &self.raw_sheets[index];
        if raw_xml.is_empty() {
            return Err(Error::new(format!(
                "no data for sheet '{}' (r:id={})",
                sheet_entry.name, sheet_entry.r_id
            )));
        }

        // Reuse the flat Arc<str> view built once at construction time.
        // Previously we allocated a fresh `Vec<String>` per sheet parse,
        // duplicating every SST entry text. Arc-sharing avoids both the
        // double storage and the per-parse rebuild.
        let parsed = parse_sheet(
            raw_xml,
            &self.shared_string_flat,
            &self.stylesheet,
            self.workbook.date_1904,
            &self.diag,
        )
        .context(format!("parsing sheet '{}'", sheet_entry.name))?;

        self.sheets[index] = Some(parsed);
        Ok(())
    }

    /// Get the sheet name for a given index.
    pub fn sheet_name(&self, index: usize) -> Option<&str> {
        self.workbook.sheets.get(index).map(|s| s.name.as_str())
    }
}

/// Page handle for XLSX. Each sheet is one logical page.
pub struct XlsxPage<'a> {
    pub(crate) sheet: &'a SheetData,
    /// Style information for font/fill/alignment lookup.
    pub(crate) stylesheet: &'a StyleSheet,
    /// Rich text shared string entries.
    pub(crate) sst_entries: &'a [SharedStringEntry],
    /// Sheet-level OPC relationships for hyperlink r:id resolution.
    pub(crate) sheet_rels: &'a [Relationship],
    /// Cached merge lookup tables. Built lazily on first access via
    /// `ensure_merge_cache()`, then reused across text(), text_lines(),
    /// tables(), etc. to avoid redundant O(covered_cells) construction.
    merge_cache: Option<MergeCache>,
}

impl FormatBackend for XlsxDocument {
    type Page<'a> = XlsxPage<'a>;

    fn page_count(&self) -> usize {
        self.workbook.sheets.len()
    }

    fn page(&mut self, index: usize) -> Result<XlsxPage<'_>> {
        self.ensure_sheet_parsed(index)?;

        let sheet = self.sheets[index]
            .as_ref()
            .ok_or_else(|| Error::new(format!("sheet {index} not parsed")))?;

        let rels = self
            .sheet_rels
            .get(index)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);

        Ok(XlsxPage {
            sheet,
            stylesheet: &self.stylesheet,
            sst_entries: &self.shared_string_entries,
            sheet_rels: rels,
            merge_cache: None,
        })
    }

    fn metadata(&self) -> DocumentMetadata {
        self.metadata.clone()
    }
}

impl XlsxPage<'_> {
    /// Ensures the merge cache is populated (built exactly once per page)
    /// and returns a reference to it.
    fn ensure_merge_cache(&mut self) -> &MergeCache {
        self.merge_cache
            .get_or_insert_with(|| build_merge_cache(&self.sheet.merge_regions))
    }

    /// Fallback text extraction for sheets with a grid area exceeding MAX_GRID_AREA.
    /// Iterates only over actual cells, tab-separating values per row. Inserts
    /// separator tabs for gap columns so column positions stay consistent with text().
    /// Gap-fill is bounded: we emit at most MAX_GAP_FILL tabs between cells to avoid
    /// O(max_col) allocations per row in extreme sparse-grid cases.
    fn text_cell_only(&mut self) -> Result<String> {
        // Cap gap-fill to avoid reintroducing the sparse-grid allocation problem.
        const MAX_GAP_FILL: usize = 256;

        let sheet = self.sheet;
        let cache = self.ensure_merge_cache();
        let mut lines: Vec<String> = Vec::new();
        let mut current_row = None;
        let mut row_buf = String::new();
        // Tracks the column of the last emitted cell (not the next expected column).
        // gap = (cell.col - last_col) gives exactly the number of tabs needed:
        // one separator after the previous cell, plus one per empty gap column.
        let mut last_col: usize = 0;

        for cell in &sheet.cells {
            if cache.covered.contains(&(cell.row, cell.col)) {
                continue;
            }
            if current_row != Some(cell.row) {
                if current_row.is_some() {
                    let trimmed = row_buf.trim_end_matches('\t');
                    lines.push(trimmed.to_string());
                }
                current_row = Some(cell.row);
                row_buf.clear();
                last_col = 0;
            }
            // Insert tabs for gap columns, capped to avoid excessive allocation.
            let gap = cell.col.saturating_sub(last_col).min(MAX_GAP_FILL);
            for _ in 0..gap {
                row_buf.push('\t');
            }
            if gap == 0 && cell.col > last_col {
                // Gap exceeded MAX_GAP_FILL; still need at least one separator.
                row_buf.push('\t');
            }
            row_buf.push_str(&cell.text);
            last_col = cell.col;
        }
        if current_row.is_some() {
            let trimmed = row_buf.trim_end_matches('\t');
            lines.push(trimmed.to_string());
        }

        while lines.last().map(|s| s.is_empty()).unwrap_or(false) {
            lines.pop();
        }
        Ok(lines.join("\n"))
    }

    /// Fallback table extraction for sheets with a grid area exceeding MAX_GRID_AREA.
    /// Iterates only over actual cells, grouping by row. Merge span info is preserved
    /// but gap columns between cells are not filled. Rows with no cells are omitted
    /// (unlike the primary tables() path which preserves interior empty rows).
    fn tables_cell_only(&mut self) -> Result<Vec<Table>> {
        let sheet = self.sheet;
        let cache = self.ensure_merge_cache();
        let mut rows: Vec<TableRow> = Vec::new();
        let mut current_row: Option<usize> = None;
        let mut row_cells: Vec<TableCell> = Vec::new();

        for cell in &sheet.cells {
            if cache.covered.contains(&(cell.row, cell.col)) {
                continue;
            }
            if current_row != Some(cell.row) {
                if current_row.is_some() && !row_cells.is_empty() {
                    // Match tables() heuristic: only row 0 is header.
                    let is_header = current_row == Some(0);
                    let table_row = if is_header {
                        TableRow::with_header(std::mem::take(&mut row_cells), true)
                    } else {
                        TableRow::new(std::mem::take(&mut row_cells))
                    };
                    rows.push(table_row);
                } else {
                    row_cells.clear();
                }
                current_row = Some(cell.row);
            }

            let (col_span, row_span) =
                if let Some(region) = cache.find_anchor(&sheet.merge_regions, cell.row, cell.col) {
                    (region.col_span(), region.row_span())
                } else {
                    (1, 1)
                };
            row_cells.push(TableCell::with_spans(
                cell.text.clone(),
                None,
                col_span,
                row_span,
            ));
        }
        if !row_cells.is_empty() {
            let is_header = current_row == Some(0);
            let table_row = if is_header {
                TableRow::with_header(row_cells, true)
            } else {
                TableRow::new(row_cells)
            };
            rows.push(table_row);
        }

        if rows.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![Table::new(rows, None)])
    }
}

impl PageExtractor for XlsxPage<'_> {
    fn text(&mut self) -> Result<String> {
        let sheet = self.sheet;
        if sheet.cells.is_empty() {
            return Ok(String::new());
        }

        let max_row = sheet.max_row.unwrap_or(0);
        let max_col = sheet.max_col.unwrap_or(0);

        // Guard against sparse-grid DoS: if the grid area is too large,
        // fall back to cell-only iteration without gap filling.
        let grid_area = (max_row + 1).saturating_mul(max_col + 1);
        if grid_area > MAX_GRID_AREA {
            return self.text_cell_only();
        }

        let cache = self.ensure_merge_cache();
        let mut lines = Vec::with_capacity(max_row + 1);
        let mut cell_idx = 0;

        for row in 0..=max_row {
            // Short-circuit fully-empty rows without walking 0..=max_col.
            // An empty row has no cells and is not touched by any merge
            // region spanning into it (merge regions are rare; check via
            // cache.covered only when needed). Even in the merge case a
            // covered cell produces an empty `col_values` entry, so an
            // empty row with no merge coverage simply emits an empty line.
            let row_has_cells = cell_idx < sheet.cells.len() && sheet.cells[cell_idx].row == row;
            if !row_has_cells && !cache.row_has_coverage(row) {
                lines.push(String::new());
                continue;
            }

            let mut col_values = Vec::with_capacity(max_col + 1);
            for col in 0..=max_col {
                if cache.covered.contains(&(row, col)) {
                    col_values.push(String::new());
                    continue;
                }
                // Find the cell at (row, col).
                let text = loop {
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
                        let t = sheet.cells[cell_idx].text.clone();
                        cell_idx += 1;
                        break t;
                    }
                    break String::new();
                };
                col_values.push(text);
            }
            // Drain any remaining cells in this row so the cursor is correctly
            // positioned for the next row.
            while cell_idx < sheet.cells.len() && sheet.cells[cell_idx].row == row {
                cell_idx += 1;
            }

            // Trim trailing empty cells.
            while col_values.last().map(|s| s.is_empty()).unwrap_or(false) {
                col_values.pop();
            }
            lines.push(col_values.join("\t"));
        }

        // Trim trailing empty lines.
        while lines.last().map(|s| s.is_empty()).unwrap_or(false) {
            lines.pop();
        }

        Ok(lines.join("\n"))
    }

    fn text_lines(&mut self) -> Result<Vec<TextLine>> {
        let sheet = self.sheet;
        if sheet.cells.is_empty() {
            return Ok(Vec::new());
        }

        // Iterate cells directly (O(cells), not O(max_row)) to avoid
        // wasting cycles on empty row indices in sparse sheets.
        let cache = self.ensure_merge_cache();
        let mut lines = Vec::new();
        let mut spans = Vec::new();
        let mut current_row: Option<usize> = None;

        for cell in &sheet.cells {
            if cell.text.is_empty() || cache.covered.contains(&(cell.row, cell.col)) {
                continue;
            }
            if current_row != Some(cell.row) {
                if let Some(prev_row) = current_row {
                    if !spans.is_empty() {
                        lines.push(TextLine::new(
                            std::mem::take(&mut spans),
                            prev_row as f64,
                            false,
                        ));
                    }
                }
                current_row = Some(cell.row);
            }
            spans.push(TextSpan::new(
                cell.text.clone(),
                cell.col as f64,
                cell.row as f64,
                0.0,
                0.0,
            ));
        }
        if let Some(row) = current_row {
            if !spans.is_empty() {
                lines.push(TextLine::new(spans, row as f64, false));
            }
        }

        Ok(lines)
    }

    /// Returns all non-empty cells as raw spans, without merge-region filtering.
    /// This is intentional: raw_spans() is the "escape hatch" for consumers
    /// that want unprocessed cell data (consistent with the trait contract).
    fn raw_spans(&mut self) -> Result<Vec<TextSpan>> {
        let sheet = self.sheet;
        let spans: Vec<TextSpan> = sheet
            .cells
            .iter()
            .filter(|c| !c.text.is_empty())
            .map(|c| TextSpan::new(c.text.clone(), c.col as f64, c.row as f64, 0.0, 0.0))
            .collect();
        Ok(spans)
    }

    fn tables(&mut self) -> Result<Vec<Table>> {
        let sheet = self.sheet;
        if sheet.cells.is_empty() {
            return Ok(Vec::new());
        }

        let max_row = sheet.max_row.unwrap_or(0);
        let max_col = sheet.max_col.unwrap_or(0);

        // Guard against sparse-grid DoS.
        let grid_area = (max_row + 1).saturating_mul(max_col + 1);
        if grid_area > MAX_GRID_AREA {
            return self.tables_cell_only();
        }

        let cache = self.ensure_merge_cache();
        let mut rows = Vec::with_capacity(max_row + 1);
        let mut cell_idx = 0;

        for row in 0..=max_row {
            // Short-circuit fully-empty rows. Row-index stability still
            // requires emitting a placeholder TableRow, but we don't need
            // to walk `0..=max_col` when the row has no cells AND no merge
            // region reaches it. For A1..C3 + Z1000 sheets this converts
            // the 1000-row walk into 4 real iterations + 996 stubs.
            let row_has_cells = cell_idx < sheet.cells.len() && sheet.cells[cell_idx].row == row;
            if !row_has_cells && !cache.row_has_coverage(row) {
                rows.push(TableRow::new(Vec::new()));
                continue;
            }

            let mut cells = Vec::with_capacity(max_col + 1);
            for col in 0..=max_col {
                // Skip covered cells in merge regions.
                if cache.covered.contains(&(row, col)) {
                    continue;
                }

                // Find the cell value at (row, col).
                let text = loop {
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
                        let t = sheet.cells[cell_idx].text.clone();
                        cell_idx += 1;
                        break t;
                    }
                    break String::new();
                };

                // Check for merge anchor at this position (O(1) via cache).
                let (col_span, row_span) =
                    if let Some(region) = cache.find_anchor(&sheet.merge_regions, row, col) {
                        (region.col_span(), region.row_span())
                    } else {
                        (1, 1)
                    };

                cells.push(TableCell::with_spans(text, None, col_span, row_span));
            }

            // Drain any remaining cells in this row for cursor correctness.
            while cell_idx < sheet.cells.len() && sheet.cells[cell_idx].row == row {
                cell_idx += 1;
            }

            // All rows are preserved (including interior empty rows) to maintain
            // row-index stability. Trailing empty rows are trimmed below.
            // MAX_GRID_AREA bounds the total rows+cols we iterate.
            let has_content = cells
                .iter()
                .any(|c| !c.text.is_empty() || c.col_span > 1 || c.row_span > 1);
            let table_row = if row == 0 && has_content {
                // Heuristic: first row is header if it has content.
                TableRow::with_header(cells, true)
            } else {
                TableRow::new(cells)
            };
            rows.push(table_row);
        }

        // Trim trailing empty rows.
        while rows
            .last()
            .map(|r| r.cells.iter().all(|c| c.text.is_empty()))
            .unwrap_or(false)
        {
            rows.pop();
        }

        if rows.is_empty() {
            return Ok(Vec::new());
        }

        // bbox is None for XLSX (no geometry).
        Ok(vec![Table::new(rows, None)])
    }

    fn images(&mut self) -> Result<Vec<PageImage>> {
        // XLSX image extraction is not implemented in this phase.
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_containers::test_util::build_stored_zip;

    /// Build a minimal XLSX ZIP with the given sheet XML.
    fn make_xlsx(
        sheet_xml: &[u8],
        shared_strings_xml: Option<&[u8]>,
        styles_xml: Option<&[u8]>,
    ) -> Vec<u8> {
        let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
</Types>"#;

        let package_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
        Target="xl/workbook.xml"/>
</Relationships>"#;

        let workbook_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
    xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
    <sheets>
        <sheet name="Sheet1" sheetId="1" r:id="rId1"/>
    </sheets>
</workbook>"#;

        let mut wb_rels = String::from(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
        Target="worksheets/sheet1.xml"/>"#,
        );
        if shared_strings_xml.is_some() {
            wb_rels.push_str(
                r#"
    <Relationship Id="rId2"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings"
        Target="sharedStrings.xml"/>"#,
            );
        }
        if styles_xml.is_some() {
            wb_rels.push_str(
                r#"
    <Relationship Id="rId3"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles"
        Target="styles.xml"/>"#,
            );
        }
        wb_rels.push_str("\n</Relationships>");

        // We need to handle the owned data lifetimes carefully.
        // Build the zip with all entries.

        let wb_rels_bytes = wb_rels.into_bytes();

        let mut entry_vec: Vec<(String, Vec<u8>)> = vec![
            ("[Content_Types].xml".to_string(), content_types.to_vec()),
            ("_rels/.rels".to_string(), package_rels.to_vec()),
            ("xl/workbook.xml".to_string(), workbook_xml.to_vec()),
            ("xl/_rels/workbook.xml.rels".to_string(), wb_rels_bytes),
            ("xl/worksheets/sheet1.xml".to_string(), sheet_xml.to_vec()),
        ];

        if let Some(sst) = shared_strings_xml {
            entry_vec.push(("xl/sharedStrings.xml".to_string(), sst.to_vec()));
        }
        if let Some(styles) = styles_xml {
            entry_vec.push(("xl/styles.xml".to_string(), styles.to_vec()));
        }

        let refs: Vec<(&str, &[u8])> = entry_vec
            .iter()
            .map(|(name, data)| (name.as_str(), data.as_slice()))
            .collect();

        build_stored_zip(&refs)
    }

    #[test]
    fn open_basic_xlsx() {
        let sheet = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1"><v>42</v></c>
            <c r="B1"><v>3.14</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let xlsx = make_xlsx(sheet, None, None);
        let mut doc = XlsxDocument::from_bytes(&xlsx).unwrap();
        assert_eq!(doc.page_count(), 1);
        assert_eq!(doc.sheet_name(0), Some("Sheet1"));

        let mut page = doc.page(0).unwrap();
        let text = page.text().unwrap();
        assert!(text.contains("42"));
        assert!(text.contains("3.14"));
    }

    #[test]
    fn shared_string_extraction() {
        let sst = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <si><t>Hello</t></si>
    <si><t>World</t></si>
</sst>"#;

        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" t="s"><v>0</v></c>
            <c r="B1" t="s"><v>1</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let xlsx = make_xlsx(sheet, Some(sst), None);
        let mut doc = XlsxDocument::from_bytes(&xlsx).unwrap();
        let mut page = doc.page(0).unwrap();
        let text = page.text().unwrap();
        assert_eq!(text, "Hello\tWorld");
    }

    #[test]
    fn boolean_cells() {
        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" t="b"><v>1</v></c>
            <c r="B1" t="b"><v>0</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let xlsx = make_xlsx(sheet, None, None);
        let mut doc = XlsxDocument::from_bytes(&xlsx).unwrap();
        let mut page = doc.page(0).unwrap();
        let text = page.text().unwrap();
        assert_eq!(text, "TRUE\tFALSE");
    }

    #[test]
    fn table_extraction() {
        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
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

        let xlsx = make_xlsx(sheet, None, None);
        let mut doc = XlsxDocument::from_bytes(&xlsx).unwrap();
        let mut page = doc.page(0).unwrap();
        let tables = page.tables().unwrap();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].rows.len(), 2);
        assert_eq!(tables[0].rows[0].cells.len(), 2);
        assert_eq!(tables[0].rows[0].cells[0].text, "1");
        assert_eq!(tables[0].rows[0].cells[1].text, "2");
        assert_eq!(tables[0].rows[1].cells[0].text, "3");
        assert_eq!(tables[0].rows[1].cells[1].text, "4");
    }

    #[test]
    fn empty_sheet() {
        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData/>
</worksheet>"#;

        let xlsx = make_xlsx(sheet, None, None);
        let mut doc = XlsxDocument::from_bytes(&xlsx).unwrap();
        let mut page = doc.page(0).unwrap();
        assert_eq!(page.text().unwrap(), "");
        assert!(page.tables().unwrap().is_empty());
        assert!(page.text_lines().unwrap().is_empty());
    }

    #[test]
    fn page_out_of_range() {
        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData/>
</worksheet>"#;

        let xlsx = make_xlsx(sheet, None, None);
        let mut doc = XlsxDocument::from_bytes(&xlsx).unwrap();
        assert!(doc.page(1).is_err());
        assert!(doc.page(100).is_err());
    }

    #[test]
    fn metadata_page_count() {
        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData/>
</worksheet>"#;

        let xlsx = make_xlsx(sheet, None, None);
        let doc = XlsxDocument::from_bytes(&xlsx).unwrap();
        let meta = doc.metadata();
        assert_eq!(meta.page_count, 1);
    }

    #[test]
    fn metadata_from_core_properties() {
        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData/>
</worksheet>"#;

        let core_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"
                   xmlns:dc="http://purl.org/dc/elements/1.1/"
                   xmlns:dcterms="http://purl.org/dc/terms/">
  <dc:title>Quarterly Report</dc:title>
  <dc:creator>Alice</dc:creator>
  <dc:subject>Finance</dc:subject>
  <dcterms:created>2025-06-01T09:00:00Z</dcterms:created>
  <dcterms:modified>2025-06-02T15:30:00Z</dcterms:modified>
  <cp:lastModifiedBy>Bob</cp:lastModifiedBy>
</cp:coreProperties>"#;

        // Build an XLSX ZIP that includes docProps/core.xml and the
        // package relationship pointing to it.
        let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
</Types>"#;

        let package_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
        Target="xl/workbook.xml"/>
    <Relationship Id="rId2"
        Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties"
        Target="docProps/core.xml"/>
</Relationships>"#;

        let workbook_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
    xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
    <sheets>
        <sheet name="Sheet1" sheetId="1" r:id="rId1"/>
    </sheets>
</workbook>"#;

        let wb_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
        Target="worksheets/sheet1.xml"/>
</Relationships>"#;

        let entries: Vec<(&str, &[u8])> = vec![
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", package_rels),
            ("xl/workbook.xml", workbook_xml),
            ("xl/_rels/workbook.xml.rels", wb_rels),
            ("xl/worksheets/sheet1.xml", sheet),
            ("docProps/core.xml", core_xml),
        ];
        let xlsx = build_stored_zip(&entries);

        let doc = XlsxDocument::from_bytes(&xlsx).unwrap();
        let meta = doc.metadata();
        assert_eq!(meta.title.as_deref(), Some("Quarterly Report"));
        assert_eq!(meta.author.as_deref(), Some("Alice"));
        assert_eq!(meta.creator.as_deref(), Some("Alice"));
        assert_eq!(meta.subject.as_deref(), Some("Finance"));
        assert_eq!(meta.creation_date.as_deref(), Some("2025-06-01T09:00:00Z"));
        assert_eq!(
            meta.modification_date.as_deref(),
            Some("2025-06-02T15:30:00Z")
        );
        assert_eq!(
            meta.properties.get("lastModifiedBy").map(|s| s.as_str()),
            Some("Bob")
        );
        assert_eq!(meta.page_count, 1);
    }

    #[test]
    fn text_lines_from_cells() {
        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1"><v>hello</v></c>
        </row>
        <row r="2">
            <c r="A2"><v>world</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let xlsx = make_xlsx(sheet, None, None);
        let mut doc = XlsxDocument::from_bytes(&xlsx).unwrap();
        let mut page = doc.page(0).unwrap();
        let lines = page.text_lines().unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans[0].text, "hello");
        assert_eq!(lines[1].spans[0].text, "world");
    }

    #[test]
    fn merge_cells_in_table() {
        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1"><v>Merged</v></c>
            <c r="C1"><v>Right</v></c>
        </row>
        <row r="2">
            <c r="A2"><v>Below</v></c>
        </row>
    </sheetData>
    <mergeCells count="1">
        <mergeCell ref="A1:B1"/>
    </mergeCells>
</worksheet>"#;

        let xlsx = make_xlsx(sheet, None, None);
        let mut doc = XlsxDocument::from_bytes(&xlsx).unwrap();
        let mut page = doc.page(0).unwrap();
        let tables = page.tables().unwrap();
        assert_eq!(tables.len(), 1);

        // First row should have Merged (col_span=2) and Right
        let row0 = &tables[0].rows[0];
        let merged_cell = row0.cells.iter().find(|c| c.text == "Merged");
        assert!(merged_cell.is_some());
        assert_eq!(merged_cell.unwrap().col_span, 2);
    }

    #[test]
    fn date_formatting() {
        let styles =
            br#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <cellXfs count="2">
        <xf numFmtId="0"/>
        <xf numFmtId="14"/>
    </cellXfs>
</styleSheet>"#;

        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" s="1"><v>43831</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let xlsx = make_xlsx(sheet, None, Some(styles));
        let mut doc = XlsxDocument::from_bytes(&xlsx).unwrap();
        let mut page = doc.page(0).unwrap();
        let text = page.text().unwrap();
        assert_eq!(text, "2020-01-01");
    }

    #[test]
    fn from_bytes_round_trip() {
        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1"><c r="A1"><v>test</v></c></row>
    </sheetData>
</worksheet>"#;

        let xlsx = make_xlsx(sheet, None, None);
        let mut doc = XlsxDocument::from_bytes(&xlsx).unwrap();
        assert_eq!(doc.page_count(), 1);
        let mut page = doc.page(0).unwrap();
        let text = page.text().unwrap();
        assert_eq!(text, "test");
    }

    // -- Fallback path tests (text_cell_only / tables_cell_only) --

    use crate::sheet::{CellData, CellRawValue};

    /// Build a minimal SheetData from (row, col, text) tuples.
    fn make_sheet_data(cells: &[(usize, usize, &str)]) -> SheetData {
        let mut cell_vec: Vec<CellData> = cells
            .iter()
            .map(|(r, c, t)| CellData {
                row: *r,
                col: *c,
                text: t.to_string(),
                raw_value: CellRawValue::Empty,
                cell_style: None,
                sst_index: None,
            })
            .collect();
        cell_vec.sort_by_key(|c| (c.row, c.col));
        let max_row = cell_vec.iter().map(|c| c.row).max();
        let max_col = cell_vec.iter().map(|c| c.col).max();
        SheetData {
            cells: cell_vec,
            max_row,
            max_col,
            merge_regions: Vec::new(),
            hyperlinks: Vec::new(),
            column_widths: Vec::new(),
        }
    }

    /// Create a test XlsxPage with default stylesheet/sst/rels.
    fn make_test_page(sheet: &SheetData) -> XlsxPage<'_> {
        static DEFAULT_STYLESHEET: std::sync::LazyLock<StyleSheet> =
            std::sync::LazyLock::new(StyleSheet::default);
        XlsxPage {
            sheet,
            stylesheet: &DEFAULT_STYLESHEET,
            sst_entries: &[],
            sheet_rels: &[],
            merge_cache: None,
        }
    }

    #[test]
    fn text_cell_only_fills_gap_columns() {
        // Cells at A1 (col 0) and C1 (col 2) -- col 1 is a gap.
        let sheet = make_sheet_data(&[(0, 0, "foo"), (0, 2, "bar")]);
        let mut page = make_test_page(&sheet);
        let text = page.text_cell_only().unwrap();
        assert_eq!(text, "foo\t\tbar");
    }

    #[test]
    fn text_cell_only_trims_trailing_empty() {
        let sheet = make_sheet_data(&[(0, 0, "a"), (0, 1, "")]);
        let mut page = make_test_page(&sheet);
        let text = page.text_cell_only().unwrap();
        assert_eq!(text, "a");
    }

    #[test]
    fn text_cell_only_multi_row() {
        let sheet = make_sheet_data(&[(0, 0, "r1c1"), (1, 0, "r2c1"), (1, 2, "r2c3")]);
        let mut page = make_test_page(&sheet);
        let text = page.text_cell_only().unwrap();
        assert_eq!(text, "r1c1\nr2c1\t\tr2c3");
    }

    #[test]
    fn tables_cell_only_basic() {
        let sheet = make_sheet_data(&[(0, 0, "a"), (0, 1, "b"), (1, 0, "c"), (1, 1, "d")]);
        let mut page = make_test_page(&sheet);
        let tables = page.tables_cell_only().unwrap();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].rows.len(), 2);
        assert_eq!(tables[0].rows[0].cells[0].text, "a");
        assert_eq!(tables[0].rows[0].cells[1].text, "b");
        assert_eq!(tables[0].rows[1].cells[0].text, "c");
        assert_eq!(tables[0].rows[1].cells[1].text, "d");
    }

    #[test]
    fn tables_cell_only_empty() {
        let sheet = make_sheet_data(&[]);
        let mut page = make_test_page(&sheet);
        let tables = page.tables_cell_only().unwrap();
        assert!(tables.is_empty());
    }

    use crate::merge::MergeRegion;

    /// Build SheetData with merge regions from (row, col, text) tuples.
    fn make_sheet_data_with_merges(
        cells: &[(usize, usize, &str)],
        merge_regions: Vec<MergeRegion>,
    ) -> SheetData {
        let mut sheet = make_sheet_data(cells);
        sheet.merge_regions = merge_regions;
        sheet
    }

    #[test]
    fn text_lines_skips_merge_covered_cells() {
        // A1:B1 merged, C1 normal, A2 normal.
        // B1 is covered by the merge and should be skipped.
        let sheet = make_sheet_data_with_merges(
            &[
                (0, 0, "Merged"),
                (0, 1, "covered"),
                (0, 2, "Right"),
                (1, 0, "Below"),
            ],
            vec![MergeRegion {
                start_row: 0,
                start_col: 0,
                end_row: 0,
                end_col: 1,
            }],
        );
        let mut page = make_test_page(&sheet);
        let lines = page.text_lines().unwrap();
        // Row 0: "Merged" (anchor) and "Right"; "covered" is skipped.
        assert_eq!(lines.len(), 2);
        let row0_texts: Vec<&str> = lines[0].spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(row0_texts, vec!["Merged", "Right"]);
        assert_eq!(lines[1].spans[0].text, "Below");
    }

    #[test]
    fn raw_spans_includes_merge_covered_cells() {
        // raw_spans() intentionally does NOT filter merge-covered cells.
        let sheet = make_sheet_data_with_merges(
            &[(0, 0, "Anchor"), (0, 1, "Covered"), (1, 0, "Normal")],
            vec![MergeRegion {
                start_row: 0,
                start_col: 0,
                end_row: 0,
                end_col: 1,
            }],
        );
        let mut page = make_test_page(&sheet);
        let spans = page.raw_spans().unwrap();
        let texts: Vec<&str> = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["Anchor", "Covered", "Normal"]);
    }

    /// After dedup, rich text SST entries still produce correct flat text
    /// and the entries are available for per-run formatting lookups.
    #[test]
    fn rich_text_shared_strings_after_dedup() {
        let sst = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <si>
        <r><rPr><b/></rPr><t>Bold</t></r>
        <r><t> and plain</t></r>
    </si>
    <si><t>Simple</t></si>
</sst>"#;

        let sheet =
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" t="s"><v>0</v></c>
            <c r="B1" t="s"><v>1</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let xlsx = make_xlsx(sheet, Some(sst), None);
        let mut doc = XlsxDocument::from_bytes(&xlsx).unwrap();

        // Flat text is derived from entries, not stored separately.
        let mut page = doc.page(0).unwrap();
        let text = page.text().unwrap();
        assert_eq!(text, "Bold and plain\tSimple");

        // Rich text entries are still preserved for per-run formatting.
        assert_eq!(doc.shared_string_entries.len(), 2);
        match &doc.shared_string_entries[0] {
            SharedStringEntry::Rich(runs) => {
                assert_eq!(runs.len(), 2);
                assert_eq!(runs[0].text, "Bold");
                assert!(runs[0].bold);
                assert_eq!(runs[1].text, " and plain");
                assert!(!runs[1].bold);
            }
            other => panic!("expected Rich entry, got {:?}", other),
        }
        match &doc.shared_string_entries[1] {
            SharedStringEntry::Plain(s) => assert_eq!(s.as_ref(), "Simple"),
            other => panic!("expected Plain entry, got {:?}", other),
        }
    }
}
