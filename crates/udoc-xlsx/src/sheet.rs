//! Worksheet data parser for XLSX.
//!
//! Parses `xl/worksheets/sheetN.xml` to extract cell values and build
//! the in-memory grid representation.

use std::sync::Arc;

use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use crate::cell_ref::parse_cell_ref;
use crate::error::{Result, ResultExt};
use crate::formats::format_cell_value;
use crate::merge::{parse_merge_cells, MergeRegion};
use crate::styles::StyleSheet;
use udoc_containers::xml::{attr_value, prefixed_attr_value, XmlEvent, XmlReader};

/// Maximum number of rows we'll process (safety limit).
const MAX_ROWS: usize = 1_000_000;

/// Maximum number of columns per row (safety limit).
const MAX_COLS: usize = 16_384;

/// Maximum total bytes of cell text data per sheet (256 MB).
/// Prevents decompression bomb attacks where a small file references
/// the same large shared string thousands of times.
const MAX_CELL_TEXT_BYTES: usize = 256 * 1024 * 1024;

/// Maximum bytes per individual cell value or inline text buffer (1 MB).
/// Prevents a single <v> or <is><t> element from consuming unbounded memory.
const MAX_CELL_VALUE_BYTES: usize = 1_024 * 1_024;

/// Maximum merge cell refs to collect before passing to parse_merge_cells.
/// Matches the downstream MAX_MERGE_REGIONS cap in merge.rs so the
/// intermediate Vec doesn't grow unbounded on crafted input.
const MAX_MERGE_REFS: usize = 10_000;

/// Maximum hyperlinks per sheet. Prevents unbounded HashMap growth from
/// crafted XLSX files with millions of `<hyperlink>` elements.
const MAX_HYPERLINKS: usize = 50_000;

/// Conversion factor from XLSX character units to points.
/// XLSX column widths are specified in "character units" (roughly the width
/// of a digit in the default font). The standard approximation is 7.5 points
/// per character unit, matching Excel's display rendering.
const CHAR_UNITS_TO_POINTS: f64 = 7.5;

/// A parsed cell value with its position.
#[derive(Debug, Clone)]
pub(crate) struct CellData {
    pub row: usize,
    pub col: usize,
    /// The display string for this cell (formatted).
    pub text: String,
    /// The raw value, reserved for populating `CellValue` metadata in the
    /// document model once the typed-cell-value layer is implemented.
    #[allow(dead_code)] // reserved for typed-cell-value document model layer
    pub raw_value: CellRawValue,
    /// Style index (the `s` attribute on `<c>` elements), if present.
    pub cell_style: Option<u32>,
    /// SST index for shared-string cells (type "s"), used for rich text lookup.
    pub sst_index: Option<usize>,
}

/// A parsed column width specification from the `<cols>` section.
///
/// `min` and `max` are 0-based column indices (converted from XLSX's 1-based).
/// `width` is in points (converted from XLSX character units via [`CHAR_UNITS_TO_POINTS`]).
#[derive(Debug, Clone)]
pub(crate) struct ColumnWidth {
    /// First column index (0-based, inclusive).
    pub min: usize,
    /// Last column index (0-based, inclusive).
    pub max: usize,
    /// Width in points.
    pub width: f64,
}

/// A hyperlink parsed from the `<hyperlinks>` section of a worksheet.
#[derive(Debug, Clone)]
pub(crate) struct SheetHyperlink {
    pub row: usize,
    pub col: usize,
    pub r_id: String,
}

/// Raw value types from the XLSX cell.
/// Reserved for populating `CellValue` metadata in the document model
/// once the typed-cell-value layer is implemented.
#[derive(Debug, Clone)]
#[allow(dead_code)] // reserved for typed-cell-value document model layer
pub(crate) enum CellRawValue {
    Text(String),
    Number(f64),
    Boolean(bool),
    Error(String),
    Empty,
}

/// Parsed sheet data.
#[derive(Debug)]
pub(crate) struct SheetData {
    /// All cells in the sheet, sorted by (row, col).
    pub cells: Vec<CellData>,
    /// Maximum row index seen (0-based), or None if empty.
    pub max_row: Option<usize>,
    /// Maximum col index seen (0-based), or None if empty.
    pub max_col: Option<usize>,
    /// Merge regions in this sheet.
    pub merge_regions: Vec<MergeRegion>,
    /// Hyperlinks in this sheet (from `<hyperlinks>` element).
    pub hyperlinks: Vec<SheetHyperlink>,
    /// Column width specifications from `<cols>` section.
    pub column_widths: Vec<ColumnWidth>,
}

/// Parse a worksheet XML part into cell data.
///
/// `shared_strings` is the flat `Arc<str>` view of the SST built once per
/// document. Entries are shared by refcount; cells referencing the same
/// SST entry do not re-allocate.
pub(crate) fn parse_sheet(
    data: &[u8],
    shared_strings: &[Arc<str>],
    stylesheet: &StyleSheet,
    date_1904: bool,
    diag: &Arc<dyn DiagnosticsSink>,
) -> Result<SheetData> {
    let mut reader = XmlReader::new(data).context("creating XML reader for sheet")?;
    let mut cells = Vec::new();
    let mut max_row: Option<usize> = None;
    let mut max_col: Option<usize> = None;

    // Per-cell state
    let mut in_cell = false;
    let mut in_value = false;
    let mut in_inline_str = false;
    let mut in_inline_t = false;
    let mut cell_type: Option<String> = None;
    let mut cell_style: Option<usize> = None;
    let mut cell_ref: Option<String> = None;
    let mut cell_value_text = String::new();
    let mut inline_text = String::new();
    let mut row_count: usize = 0;
    let mut rows_truncated = false;
    let mut cols_truncated = false;
    let mut total_text_bytes: usize = 0;
    let mut text_budget_exceeded = false;
    let mut cell_value_truncated = false;

    let mut in_merge_cells = false;
    let mut merge_refs: Vec<String> = Vec::new();
    let mut merge_refs_truncated = false;

    let mut in_hyperlinks = false;
    let mut hyperlinks: Vec<SheetHyperlink> = Vec::new();

    let mut in_cols = false;
    let mut column_widths: Vec<ColumnWidth> = Vec::new();

    loop {
        match reader.next_event().context("reading sheet XML")? {
            XmlEvent::StartElement {
                local_name,
                attributes,
                ..
            } => match local_name.as_ref() {
                "row" => {
                    row_count = row_count.saturating_add(1);
                    if row_count > MAX_ROWS && !rows_truncated {
                        rows_truncated = true;
                        diag.warning(Warning::new(
                            "XlsxRowLimit",
                            format!("sheet exceeds {MAX_ROWS} rows, truncating cell data"),
                        ));
                    }
                }
                "c" if !rows_truncated && !text_budget_exceeded => {
                    in_cell = true;
                    cell_type = attr_value(&attributes, "t").map(|s| s.to_string());
                    cell_style = attr_value(&attributes, "s").and_then(|s| s.parse::<usize>().ok());
                    cell_ref = attr_value(&attributes, "r").map(|s| s.to_string());
                    cell_value_text.clear();
                    inline_text.clear();
                }
                "v" if in_cell => {
                    in_value = true;
                }
                "is" if in_cell => {
                    in_inline_str = true;
                }
                "t" if in_inline_str => {
                    in_inline_t = true;
                }
                "mergeCells" => {
                    in_merge_cells = true;
                }
                "mergeCell" if in_merge_cells => {
                    if merge_refs.len() < MAX_MERGE_REFS {
                        if let Some(r) = attr_value(&attributes, "ref") {
                            merge_refs.push(r.to_string());
                        }
                    } else if !merge_refs_truncated {
                        merge_refs_truncated = true;
                        diag.warning(Warning::new(
                            "XlsxMergeRefsLimit",
                            format!(
                                "more than {MAX_MERGE_REFS} merge cell refs, \
                                 ignoring additional entries"
                            ),
                        ));
                    }
                }
                "cols" => {
                    in_cols = true;
                }
                "col" if in_cols => {
                    let min_str = attr_value(&attributes, "min");
                    let max_str = attr_value(&attributes, "max");
                    let width_str = attr_value(&attributes, "width");
                    if let (Some(min_s), Some(max_s), Some(w_s)) = (min_str, max_str, width_str) {
                        let min_1 = min_s.parse::<usize>();
                        let max_1 = max_s.parse::<usize>();
                        let width = w_s.parse::<f64>();
                        match (min_1, max_1, width) {
                            (Ok(min_1), Ok(max_1), Ok(w)) if min_1 >= 1 && max_1 >= min_1 => {
                                // Convert 1-based to 0-based, clamping to MAX_COLS.
                                let min_0 = min_1 - 1;
                                let max_0 = (max_1 - 1).min(MAX_COLS - 1);
                                if min_0 < MAX_COLS {
                                    column_widths.push(ColumnWidth {
                                        min: min_0,
                                        max: max_0,
                                        width: w * CHAR_UNITS_TO_POINTS,
                                    });
                                }
                            }
                            _ => {
                                diag.warning(Warning::new(
                                    "XlsxInvalidColSpec",
                                    format!(
                                        "invalid <col> attributes: min={min_s}, max={max_s}, width={w_s}"
                                    ),
                                ));
                            }
                        }
                    }
                }
                "hyperlinks" => {
                    in_hyperlinks = true;
                }
                "hyperlink" if in_hyperlinks => {
                    if hyperlinks.len() >= MAX_HYPERLINKS {
                        diag.warning(Warning::new(
                            "XlsxHyperlinkLimit",
                            format!(
                                "more than {MAX_HYPERLINKS} hyperlinks per sheet, \
                                 ignoring additional entries"
                            ),
                        ));
                    } else {
                        let r_id_val = prefixed_attr_value(&attributes, "r", "id");
                        if let (Some(cell_ref_str), Some(r_id)) =
                            (attr_value(&attributes, "ref"), r_id_val)
                        {
                            // Strip range suffix (e.g. "A1:B2" -> "A1") for merged cell hyperlinks.
                            let anchor = cell_ref_str.split(':').next().unwrap_or(cell_ref_str);
                            if let Ok((row, col)) = parse_cell_ref(anchor) {
                                hyperlinks.push(SheetHyperlink {
                                    row,
                                    col,
                                    r_id: r_id.to_string(),
                                });
                            }
                        }
                    }
                }
                _ => {}
            },
            XmlEvent::EndElement { local_name, .. } => {
                match local_name.as_ref() {
                    "c" if in_cell => {
                        // Process the cell.
                        if cell_ref.is_none() {
                            diag.warning(Warning::new(
                                "XlsxMissingCellRef",
                                "cell element missing 'r' attribute, skipping",
                            ));
                        }
                        if let Some(ref r) = cell_ref {
                            match parse_cell_ref(r) {
                                Ok((row, col)) => {
                                    if col >= MAX_COLS {
                                        if !cols_truncated {
                                            cols_truncated = true;
                                            diag.warning(Warning::new(
                                                "XlsxColumnLimit",
                                                format!(
                                                    "cell column exceeds {MAX_COLS} limit, \
                                                     skipping out-of-range cells"
                                                ),
                                            ));
                                        }
                                    } else {
                                        // Pre-check text budget for shared string cells
                                        // to avoid cloning huge strings.
                                        if cell_type.as_deref() == Some("s") {
                                            if let Ok(idx) = cell_value_text.parse::<usize>() {
                                                if idx < shared_strings.len() {
                                                    let candidate_len = shared_strings[idx].len();
                                                    if total_text_bytes
                                                        .saturating_add(candidate_len)
                                                        > MAX_CELL_TEXT_BYTES
                                                    {
                                                        if !text_budget_exceeded {
                                                            text_budget_exceeded = true;
                                                            diag.warning(Warning::new(
                                                                "XlsxTextBudgetExceeded",
                                                                format!(
                                                                    "sheet cell text exceeds \
                                                                     {} MB, truncating",
                                                                    MAX_CELL_TEXT_BYTES
                                                                        / (1024 * 1024)
                                                                ),
                                                            ));
                                                        }
                                                        // Skip this cell entirely
                                                        in_cell = false;
                                                        cell_ref = None;
                                                        continue;
                                                    }
                                                }
                                            }
                                        }

                                        let text = resolve_cell_value(
                                            cell_type.as_deref(),
                                            &cell_value_text,
                                            &inline_text,
                                            shared_strings,
                                            stylesheet,
                                            cell_style,
                                            date_1904,
                                            diag,
                                        );

                                        let raw_value = parse_raw_value(
                                            cell_type.as_deref(),
                                            &cell_value_text,
                                            &inline_text,
                                            shared_strings,
                                        );

                                        // Capture SST index for rich text lookup.
                                        let sst_index = if cell_type.as_deref() == Some("s") {
                                            cell_value_text.parse::<usize>().ok()
                                        } else {
                                            None
                                        };

                                        total_text_bytes =
                                            total_text_bytes.saturating_add(text.len());
                                        cells.push(CellData {
                                            row,
                                            col,
                                            text,
                                            raw_value,
                                            cell_style: cell_style
                                                .and_then(|s| u32::try_from(s).ok()),
                                            sst_index,
                                        });

                                        max_row = Some(max_row.map_or(row, |m: usize| m.max(row)));
                                        max_col = Some(max_col.map_or(col, |m: usize| m.max(col)));

                                        // Check cumulative budget for all cell types.
                                        if total_text_bytes > MAX_CELL_TEXT_BYTES
                                            && !text_budget_exceeded
                                        {
                                            text_budget_exceeded = true;
                                            diag.warning(Warning::new(
                                                "XlsxTextBudgetExceeded",
                                                format!(
                                                    "sheet cell text exceeds {} MB, truncating",
                                                    MAX_CELL_TEXT_BYTES / (1024 * 1024)
                                                ),
                                            ));
                                        }
                                    }
                                }
                                Err(_) => {
                                    diag.warning(Warning::new(
                                        "XlsxInvalidCellRef",
                                        format!("invalid cell reference: {r}"),
                                    ));
                                }
                            }
                        }

                        in_cell = false;
                        in_value = false;
                        in_inline_str = false;
                        in_inline_t = false;
                        cell_type = None;
                        cell_style = None;
                        cell_ref = None;
                    }
                    "v" if in_value => {
                        in_value = false;
                    }
                    "is" if in_inline_str => {
                        in_inline_str = false;
                    }
                    "t" if in_inline_t => {
                        in_inline_t = false;
                    }
                    "cols" => {
                        in_cols = false;
                    }
                    "mergeCells" => {
                        in_merge_cells = false;
                    }
                    "hyperlinks" => {
                        in_hyperlinks = false;
                    }
                    _ => {}
                }
            }
            XmlEvent::Text(text) | XmlEvent::CData(text) => {
                if in_value {
                    if cell_value_text.len() < MAX_CELL_VALUE_BYTES {
                        cell_value_text.push_str(&text);
                        if cell_value_text.len() > MAX_CELL_VALUE_BYTES {
                            cell_value_text.truncate(MAX_CELL_VALUE_BYTES);
                            if !cell_value_truncated {
                                cell_value_truncated = true;
                                diag.warning(Warning::new(
                                    "XlsxCellValueTruncated",
                                    format!(
                                        "cell value exceeds {} bytes, truncating",
                                        MAX_CELL_VALUE_BYTES
                                    ),
                                ));
                            }
                        }
                    }
                } else if in_inline_t && inline_text.len() < MAX_CELL_VALUE_BYTES {
                    inline_text.push_str(&text);
                    if inline_text.len() > MAX_CELL_VALUE_BYTES {
                        inline_text.truncate(MAX_CELL_VALUE_BYTES);
                        if !cell_value_truncated {
                            cell_value_truncated = true;
                            diag.warning(Warning::new(
                                "XlsxCellValueTruncated",
                                format!(
                                    "inline string exceeds {} bytes, truncating",
                                    MAX_CELL_VALUE_BYTES
                                ),
                            ));
                        }
                    }
                }
            }
            XmlEvent::Eof => break,
        }
    }

    // Sort cells by (row, col) for consistent access.
    cells.sort_by_key(|c| (c.row, c.col));

    // Parse merge regions.
    let merge_regions = parse_merge_cells(&merge_refs, diag);

    Ok(SheetData {
        cells,
        max_row,
        max_col,
        merge_regions,
        hyperlinks,
        column_widths,
    })
}

/// Resolve a cell's display value based on its type attribute and raw value.
#[allow(clippy::too_many_arguments)]
fn resolve_cell_value(
    cell_type: Option<&str>,
    value_text: &str,
    inline_text: &str,
    shared_strings: &[Arc<str>],
    stylesheet: &StyleSheet,
    style_index: Option<usize>,
    date_1904: bool,
    diag: &Arc<dyn DiagnosticsSink>,
) -> String {
    match cell_type {
        // Shared string reference. The SST itself holds `Arc<str>`, so there
        // is no duplicate allocation on the SST side; we copy into an owned
        // String here to keep CellData.text format-agnostic (number/boolean
        // formatting produces owned Strings too).
        Some("s") => match value_text.parse::<usize>() {
            Ok(idx) if idx < shared_strings.len() => shared_strings[idx].as_ref().to_string(),
            Ok(idx) => {
                diag.warning(Warning::new(
                    "XlsxInvalidSstIndex",
                    format!(
                        "shared string index {idx} out of range (have {})",
                        shared_strings.len()
                    ),
                ));
                String::new()
            }
            Err(_) => {
                diag.warning(Warning::new(
                    "XlsxInvalidSstIndex",
                    format!("non-numeric shared string index: {value_text}"),
                ));
                String::new()
            }
        },
        // Boolean
        Some("b") => {
            if value_text == "1" || value_text.eq_ignore_ascii_case("true") {
                "TRUE".to_string()
            } else {
                "FALSE".to_string()
            }
        }
        // Inline string
        Some("inlineStr") => inline_text.to_string(),
        // Formula cached result (string type)
        Some("str") => value_text.to_string(),
        // Error
        Some("e") => value_text.to_string(),
        // Number (explicit "n" or absent type attribute)
        Some("n") | None => {
            if value_text.is_empty() {
                return String::new();
            }
            // Apply number formatting if we have a style.
            format_cell_value(value_text, stylesheet, style_index, date_1904)
        }
        // Unknown type: return raw value
        Some(_) => value_text.to_string(),
    }
}

/// Parse the raw typed value for CellValue metadata.
fn parse_raw_value(
    cell_type: Option<&str>,
    value_text: &str,
    inline_text: &str,
    shared_strings: &[Arc<str>],
) -> CellRawValue {
    match cell_type {
        Some("s") => match value_text.parse::<usize>() {
            Ok(idx) if idx < shared_strings.len() => {
                CellRawValue::Text(shared_strings[idx].as_ref().to_string())
            }
            _ => CellRawValue::Empty,
        },
        Some("b") => {
            CellRawValue::Boolean(value_text == "1" || value_text.eq_ignore_ascii_case("true"))
        }
        Some("inlineStr") => CellRawValue::Text(inline_text.to_string()),
        Some("str") => CellRawValue::Text(value_text.to_string()),
        Some("e") => CellRawValue::Error(value_text.to_string()),
        Some("n") | None => {
            if value_text.is_empty() {
                CellRawValue::Empty
            } else {
                match value_text.parse::<f64>() {
                    Ok(n) => CellRawValue::Number(n),
                    Err(_) => CellRawValue::Text(value_text.to_string()),
                }
            }
        }
        Some(_) => {
            if value_text.is_empty() {
                CellRawValue::Empty
            } else {
                CellRawValue::Text(value_text.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use udoc_core::diagnostics::NullDiagnostics;

    use super::*;

    fn null_diag() -> Arc<dyn DiagnosticsSink> {
        Arc::new(NullDiagnostics)
    }

    fn empty_sst() -> Vec<Arc<str>> {
        Vec::new()
    }

    fn empty_styles() -> StyleSheet {
        StyleSheet::default()
    }

    #[test]
    fn parse_number_cells() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1"><v>42</v></c>
            <c r="B1" t="n"><v>3.14</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let data = parse_sheet(xml, &empty_sst(), &empty_styles(), false, &null_diag()).unwrap();
        assert_eq!(data.cells.len(), 2);
        assert_eq!(data.cells[0].text, "42");
        assert_eq!(data.cells[0].row, 0);
        assert_eq!(data.cells[0].col, 0);
        assert_eq!(data.cells[1].text, "3.14");
        assert_eq!(data.cells[1].col, 1);
    }

    #[test]
    fn parse_shared_string_cells() {
        let sst: Vec<Arc<str>> = vec![Arc::<str>::from("Hello"), Arc::<str>::from("World")];
        let xml = br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" t="s"><v>0</v></c>
            <c r="B1" t="s"><v>1</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let data = parse_sheet(xml, &sst, &empty_styles(), false, &null_diag()).unwrap();
        assert_eq!(data.cells[0].text, "Hello");
        assert_eq!(data.cells[1].text, "World");
    }

    #[test]
    fn parse_boolean_cells() {
        let xml = br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" t="b"><v>1</v></c>
            <c r="B1" t="b"><v>0</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let data = parse_sheet(xml, &empty_sst(), &empty_styles(), false, &null_diag()).unwrap();
        assert_eq!(data.cells[0].text, "TRUE");
        assert_eq!(data.cells[1].text, "FALSE");
    }

    #[test]
    fn parse_inline_string() {
        let xml = br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" t="inlineStr"><is><t>Inline text</t></is></c>
        </row>
    </sheetData>
</worksheet>"#;

        let data = parse_sheet(xml, &empty_sst(), &empty_styles(), false, &null_diag()).unwrap();
        assert_eq!(data.cells[0].text, "Inline text");
    }

    #[test]
    fn parse_formula_cached_result() {
        let xml = br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" t="str"><v>Result</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let data = parse_sheet(xml, &empty_sst(), &empty_styles(), false, &null_diag()).unwrap();
        assert_eq!(data.cells[0].text, "Result");
    }

    #[test]
    fn parse_error_cell() {
        let xml = br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" t="e"><v>#REF!</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let data = parse_sheet(xml, &empty_sst(), &empty_styles(), false, &null_diag()).unwrap();
        assert_eq!(data.cells[0].text, "#REF!");
    }

    #[test]
    fn parse_empty_sheet() {
        let xml = br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData/>
</worksheet>"#;

        let data = parse_sheet(xml, &empty_sst(), &empty_styles(), false, &null_diag()).unwrap();
        assert!(data.cells.is_empty());
        assert!(data.max_row.is_none());
        assert!(data.max_col.is_none());
    }

    #[test]
    fn parse_multi_row() {
        let xml = br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
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

        let data = parse_sheet(xml, &empty_sst(), &empty_styles(), false, &null_diag()).unwrap();
        assert_eq!(data.cells.len(), 4);
        assert_eq!(data.max_row, Some(1));
        assert_eq!(data.max_col, Some(1));
    }

    #[test]
    fn cells_sorted_by_position() {
        // Cells given out of order should be sorted.
        let xml = br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="2">
            <c r="B2"><v>4</v></c>
            <c r="A2"><v>3</v></c>
        </row>
        <row r="1">
            <c r="A1"><v>1</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let data = parse_sheet(xml, &empty_sst(), &empty_styles(), false, &null_diag()).unwrap();
        assert_eq!(data.cells[0].row, 0); // A1
        assert_eq!(data.cells[1].row, 1); // A2
        assert_eq!(data.cells[1].col, 0);
        assert_eq!(data.cells[2].row, 1); // B2
        assert_eq!(data.cells[2].col, 1);
    }

    #[test]
    fn invalid_sst_index_warns() {
        let sst: Vec<Arc<str>> = vec![Arc::<str>::from("only one")];
        let xml = br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1" t="s"><v>999</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let collecting = Arc::new(udoc_core::diagnostics::CollectingDiagnostics::new());
        let diag: Arc<dyn DiagnosticsSink> = collecting.clone();
        let data = parse_sheet(xml, &sst, &empty_styles(), false, &diag).unwrap();
        assert_eq!(data.cells[0].text, "");
        let warnings = collecting.warnings();
        assert!(warnings.iter().any(|w| w.kind == "XlsxInvalidSstIndex"));
    }

    #[test]
    fn max_column_boundary_accepted() {
        // XFD = column 16383 (0-indexed), which is the maximum valid column.
        // parse_cell_ref enforces this limit, so cells at XFD are accepted.
        let xml = br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1"><v>first</v></c>
            <c r="XFD1"><v>last col</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let data = parse_sheet(xml, &empty_sst(), &empty_styles(), false, &null_diag()).unwrap();
        assert_eq!(data.cells.len(), 2);
        assert_eq!(data.cells[0].text, "first");
        assert_eq!(data.cells[1].text, "last col");
        assert_eq!(data.cells[1].col, 16383);
    }

    #[test]
    fn over_max_column_ref_warns_invalid() {
        // XFE1 = column 16384, which exceeds the cell ref parser's limit.
        // This produces an XlsxInvalidCellRef warning (defense-in-depth).
        let xml = br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1"><v>kept</v></c>
            <c r="XFE1"><v>rejected</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let collecting = Arc::new(udoc_core::diagnostics::CollectingDiagnostics::new());
        let diag: Arc<dyn DiagnosticsSink> = collecting.clone();
        let data = parse_sheet(xml, &empty_sst(), &empty_styles(), false, &diag).unwrap();
        assert_eq!(data.cells.len(), 1);
        assert_eq!(data.cells[0].text, "kept");
        let warnings = collecting.warnings();
        assert!(warnings.iter().any(|w| w.kind == "XlsxInvalidCellRef"));
    }

    #[test]
    fn missing_cell_ref_warns() {
        let xml = br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c><v>no ref</v></c>
            <c r="B1"><v>has ref</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let collecting = Arc::new(udoc_core::diagnostics::CollectingDiagnostics::new());
        let diag: Arc<dyn DiagnosticsSink> = collecting.clone();
        let data = parse_sheet(xml, &empty_sst(), &empty_styles(), false, &diag).unwrap();
        // Only B1 should have been parsed.
        assert_eq!(data.cells.len(), 1);
        assert_eq!(data.cells[0].text, "has ref");
        let warnings = collecting.warnings();
        assert!(warnings.iter().any(|w| w.kind == "XlsxMissingCellRef"));
    }

    #[test]
    fn cell_value_truncation_warns() {
        // Build a cell value that exceeds MAX_CELL_VALUE_BYTES (1 MB).
        let big_value = "x".repeat(1_024 * 1_024 + 100);
        let xml = format!(
            r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1"><v>{big_value}</v></c>
        </row>
    </sheetData>
</worksheet>"#
        );

        let collecting = Arc::new(udoc_core::diagnostics::CollectingDiagnostics::new());
        let diag: Arc<dyn DiagnosticsSink> = collecting.clone();
        let data =
            parse_sheet(xml.as_bytes(), &empty_sst(), &empty_styles(), false, &diag).unwrap();
        assert_eq!(data.cells.len(), 1);
        // The cell text should be truncated to at most MAX_CELL_VALUE_BYTES.
        assert!(data.cells[0].text.len() <= 1_024 * 1_024);
        let warnings = collecting.warnings();
        assert!(
            warnings.iter().any(|w| w.kind == "XlsxCellValueTruncated"),
            "expected XlsxCellValueTruncated warning, got: {:?}",
            warnings.iter().map(|w| &w.kind).collect::<Vec<_>>()
        );
    }

    #[test]
    fn parse_column_widths() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <cols>
        <col min="1" max="1" width="10" customWidth="1"/>
        <col min="2" max="5" width="20" customWidth="1"/>
    </cols>
    <sheetData>
        <row r="1">
            <c r="A1"><v>1</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let data = parse_sheet(xml, &empty_sst(), &empty_styles(), false, &null_diag()).unwrap();
        assert_eq!(data.column_widths.len(), 2);

        // First col: min=0 (1-based 1), max=0, width=10*7.5=75.0 points.
        assert_eq!(data.column_widths[0].min, 0);
        assert_eq!(data.column_widths[0].max, 0);
        assert!((data.column_widths[0].width - 75.0).abs() < f64::EPSILON);

        // Second col: min=1 (1-based 2), max=4 (1-based 5), width=20*7.5=150.0 points.
        assert_eq!(data.column_widths[1].min, 1);
        assert_eq!(data.column_widths[1].max, 4);
        assert!((data.column_widths[1].width - 150.0).abs() < f64::EPSILON);
    }

    #[test]
    fn no_cols_section_empty_widths() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheetData>
        <row r="1">
            <c r="A1"><v>1</v></c>
        </row>
    </sheetData>
</worksheet>"#;

        let data = parse_sheet(xml, &empty_sst(), &empty_styles(), false, &null_diag()).unwrap();
        assert!(data.column_widths.is_empty());
    }

    #[test]
    fn invalid_col_attributes_warns() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <cols>
        <col min="0" max="1" width="10"/>
        <col min="abc" max="1" width="10"/>
    </cols>
    <sheetData/>
</worksheet>"#;

        let collecting = Arc::new(udoc_core::diagnostics::CollectingDiagnostics::new());
        let diag: Arc<dyn DiagnosticsSink> = collecting.clone();
        let data = parse_sheet(xml, &empty_sst(), &empty_styles(), false, &diag).unwrap();

        // min=0 is invalid (1-based), and min="abc" is unparseable.
        // Both should produce warnings.
        let warnings = collecting.warnings();
        assert_eq!(
            warnings
                .iter()
                .filter(|w| w.kind == "XlsxInvalidColSpec")
                .count(),
            2,
            "expected 2 XlsxInvalidColSpec warnings, got: {:?}",
            warnings.iter().map(|w| &w.kind).collect::<Vec<_>>()
        );
        assert!(data.column_widths.is_empty());
    }

    #[test]
    fn col_max_clamped_to_limit() {
        // A <col> spanning beyond MAX_COLS should be clamped.
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <cols>
        <col min="16380" max="99999" width="12"/>
    </cols>
    <sheetData/>
</worksheet>"#;

        let data = parse_sheet(xml, &empty_sst(), &empty_styles(), false, &null_diag()).unwrap();
        assert_eq!(data.column_widths.len(), 1);
        // min=16379 (0-based from 16380), max clamped to 16383 (MAX_COLS-1).
        assert_eq!(data.column_widths[0].min, 16379);
        assert_eq!(data.column_widths[0].max, 16383);
    }
}
