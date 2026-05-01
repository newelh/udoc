//! ODS (office:spreadsheet) body parser.
//!
//! Walks office:body > office:spreadsheet > table:table and extracts
//! cell values with type dispatch. Handles column/row repetition guards
//! to prevent materialization bombs.

use udoc_containers::xml::namespace::ns;
use udoc_containers::xml::{attr_value, XmlEvent, XmlReader};
use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use crate::error::{Result, ResultExt};

/// Maximum number of sheets to collect (safety limit).
const MAX_SHEETS: usize = 10_000;

/// Maximum columns to materialize from repeated empty cells.
const MAX_MATERIALIZED_COLS: usize = 16_384;

/// Maximum rows to materialize from repeated empty rows.
const MAX_MATERIALIZED_ROWS: usize = 1_000_000;

/// A cell in an ODS sheet.
#[derive(Debug, Clone)]
pub(crate) struct OdsCell {
    pub text: String,
    pub col_span: usize,
    pub row_span: usize,
}

/// A row in an ODS sheet.
#[derive(Debug, Clone)]
pub(crate) struct OdsRow {
    pub cells: Vec<OdsCell>,
}

/// A sheet (table) in the ODS spreadsheet.
#[derive(Debug, Clone)]
pub(crate) struct OdsSheet {
    #[allow(dead_code)]
    pub name: String,
    pub rows: Vec<OdsRow>,
}

/// Parsed ODS body.
#[derive(Debug)]
pub(crate) struct OdsBody {
    pub sheets: Vec<OdsSheet>,
}

/// Parse the ODS body from content.xml.
pub(crate) fn parse_ods_body(data: &[u8], diag: &dyn DiagnosticsSink) -> Result<OdsBody> {
    let mut reader = XmlReader::new(data).context("initializing XML parser for ODS body")?;

    let mut sheets = Vec::new();
    let mut in_spreadsheet = false;

    loop {
        let event = reader.next_element().context("parsing ODS body")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                let name = local_name.as_ref();

                if ns_str == ns::OFFICE && name == "spreadsheet" {
                    in_spreadsheet = true;
                    continue;
                }

                if !in_spreadsheet {
                    continue;
                }

                if ns_str == ns::TABLE && name == "table" {
                    if sheets.len() >= MAX_SHEETS {
                        diag.warning(Warning::new(
                            "OdsMaxSheets",
                            format!("sheet limit ({MAX_SHEETS}) exceeded, truncating"),
                        ));
                        crate::styles::skip_element(&mut reader)?;
                        break;
                    }
                    let sheet_name = attr_value(&attributes, "name")
                        .unwrap_or("Sheet")
                        .to_string();
                    let sheet = parse_sheet(&mut reader, sheet_name, diag)?;
                    sheets.push(sheet);
                }
            }
            XmlEvent::EndElement {
                local_name,
                namespace_uri,
                ..
            } => {
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                if ns_str == ns::OFFICE && local_name.as_ref() == "spreadsheet" {
                    in_spreadsheet = false;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(OdsBody { sheets })
}

/// Parse a single table:table element into an OdsSheet.
fn parse_sheet(
    reader: &mut XmlReader<'_>,
    name: String,
    diag: &dyn DiagnosticsSink,
) -> Result<OdsSheet> {
    let mut rows = Vec::new();
    let mut depth: usize = 1;

    loop {
        let event = reader.next_element().context("parsing ODS sheet")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                depth += 1;
                let ns_str = namespace_uri.as_deref().unwrap_or("");

                if ns_str == ns::TABLE && local_name.as_ref() == "table-row" {
                    let repeat = attr_value(&attributes, "number-rows-repeated")
                        .and_then(|s| s.parse::<usize>().ok())
                        .unwrap_or(1);

                    let row = parse_row(reader, diag)?;
                    depth = depth.saturating_sub(1); // parse_row consumed end element.

                    // Only expand repeated rows if they have content.
                    let has_content = row.cells.iter().any(|c| !c.text.is_empty());
                    if has_content {
                        let effective_repeat = repeat.min(MAX_MATERIALIZED_ROWS - rows.len());
                        if effective_repeat == 0 {
                            diag.warning(Warning::new(
                                "OdsRowLimit",
                                format!("row limit ({MAX_MATERIALIZED_ROWS}) reached, truncating"),
                            ));
                            break;
                        }
                        for _ in 0..effective_repeat {
                            rows.push(row.clone());
                        }
                    }
                    // Empty repeated rows are skipped entirely.
                }
            }
            XmlEvent::EndElement { .. } => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    // Trim trailing empty rows.
    while rows
        .last()
        .map(|r: &OdsRow| r.cells.iter().all(|c| c.text.is_empty()))
        .unwrap_or(false)
    {
        rows.pop();
    }

    Ok(OdsSheet { name, rows })
}

/// Parse a table:table-row element.
fn parse_row(reader: &mut XmlReader<'_>, diag: &dyn DiagnosticsSink) -> Result<OdsRow> {
    let mut cells = Vec::new();
    let mut depth: usize = 1;

    loop {
        let event = reader.next_element().context("parsing ODS row")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                depth += 1;
                let ns_str = namespace_uri.as_deref().unwrap_or("");

                if ns_str == ns::TABLE && local_name.as_ref() == "table-cell" {
                    let repeat = attr_value(&attributes, "number-columns-repeated")
                        .and_then(|s| s.parse::<usize>().ok())
                        .unwrap_or(1);
                    let col_span = attr_value(&attributes, "number-columns-spanned")
                        .and_then(|s| s.parse::<usize>().ok())
                        .unwrap_or(1);
                    let row_span = attr_value(&attributes, "number-rows-spanned")
                        .and_then(|s| s.parse::<usize>().ok())
                        .unwrap_or(1);

                    let text = extract_cell_value(&attributes, reader)?;
                    depth = depth.saturating_sub(1); // extract_cell_value consumed end element.

                    // Only expand repeated cells if they have content.
                    if !text.is_empty() {
                        let effective_repeat = repeat.min(MAX_MATERIALIZED_COLS - cells.len());
                        if effective_repeat == 0 {
                            diag.warning(Warning::new(
                                "OdsColLimit",
                                format!(
                                    "column limit ({MAX_MATERIALIZED_COLS}) reached, truncating"
                                ),
                            ));
                        }
                        for _ in 0..effective_repeat {
                            cells.push(OdsCell {
                                text: text.clone(),
                                col_span,
                                row_span,
                            });
                        }
                    }
                    // Empty repeated cells are skipped entirely.
                } else if ns_str == ns::TABLE && local_name.as_ref() == "covered-table-cell" {
                    // Spanned-over cell, skip.
                    crate::styles::skip_element(reader)?;
                    depth = depth.saturating_sub(1);
                }
            }
            XmlEvent::EndElement { .. } => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(OdsRow { cells })
}

/// Extract the cell value from attributes and child text:p elements.
///
/// Value type dispatch:
/// - float/percentage/currency -> office:value attribute
/// - date -> office:date-value attribute (ISO 8601)
/// - boolean -> office:boolean-value (true/false)
/// - string/empty -> text:p child text content
fn extract_cell_value(
    attributes: &[udoc_containers::xml::Attribute<'_>],
    reader: &mut XmlReader<'_>,
) -> Result<String> {
    let value_type = attr_value(attributes, "value-type").unwrap_or("");

    // Try to get the typed value from attributes first.
    let attr_value_str = match value_type {
        "float" | "percentage" | "currency" => {
            attr_value(attributes, "value").map(|s| s.to_string())
        }
        "date" => attr_value(attributes, "date-value").map(|s| s.to_string()),
        "boolean" => attr_value(attributes, "boolean-value").map(|v| {
            if v == "true" {
                "TRUE".to_string()
            } else {
                "FALSE".to_string()
            }
        }),
        _ => None,
    };

    // Collect text:p child content (used for string type or as fallback).
    let text_content = collect_cell_text_content(reader)?;

    // Prefer the typed attribute value; fall back to text:p content.
    Ok(attr_value_str.unwrap_or(text_content))
}

/// Collect text from text:p children inside a table-cell.
fn collect_cell_text_content(reader: &mut XmlReader<'_>) -> Result<String> {
    let mut parts = Vec::new();
    let mut depth: usize = 1;
    let mut text_buf = String::new();

    loop {
        let event = reader.next_event().context("collecting cell text")?;

        match event {
            XmlEvent::Text(text) | XmlEvent::CData(text) => {
                text_buf.push_str(text.as_ref());
            }
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                ..
            } => {
                depth += 1;
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                // New text:p = new paragraph separator.
                if ns_str == ns::TEXT && local_name.as_ref() == "p" && !text_buf.is_empty() {
                    parts.push(std::mem::take(&mut text_buf));
                }
            }
            XmlEvent::EndElement { .. } => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            XmlEvent::Eof => break,
        }
    }

    if !text_buf.is_empty() {
        parts.push(text_buf);
    }

    Ok(parts.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::diagnostics::NullDiagnostics;

    #[test]
    fn parse_basic_cells() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:spreadsheet>
      <table:table table:name="Sheet1">
        <table:table-row>
          <table:table-cell office:value-type="string"><text:p>Hello</text:p></table:table-cell>
          <table:table-cell office:value-type="float" office:value="42"><text:p>42</text:p></table:table-cell>
        </table:table-row>
      </table:table>
    </office:spreadsheet>
  </office:body>
</office:document-content>"#;

        let body = parse_ods_body(xml, &NullDiagnostics).unwrap();
        assert_eq!(body.sheets.len(), 1);
        assert_eq!(body.sheets[0].name, "Sheet1");
        assert_eq!(body.sheets[0].rows.len(), 1);
        assert_eq!(body.sheets[0].rows[0].cells.len(), 2);
        assert_eq!(body.sheets[0].rows[0].cells[0].text, "Hello");
        assert_eq!(body.sheets[0].rows[0].cells[1].text, "42");
    }

    #[test]
    fn typed_values() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:spreadsheet>
      <table:table table:name="Sheet1">
        <table:table-row>
          <table:table-cell office:value-type="date" office:date-value="2025-01-15"><text:p>Jan 15</text:p></table:table-cell>
          <table:table-cell office:value-type="boolean" office:boolean-value="true"><text:p>Yes</text:p></table:table-cell>
          <table:table-cell office:value-type="percentage" office:value="0.5"><text:p>50%</text:p></table:table-cell>
          <table:table-cell office:value-type="currency" office:value="100"><text:p>$100</text:p></table:table-cell>
        </table:table-row>
      </table:table>
    </office:spreadsheet>
  </office:body>
</office:document-content>"#;

        let body = parse_ods_body(xml, &NullDiagnostics).unwrap();
        let row = &body.sheets[0].rows[0];
        assert_eq!(row.cells[0].text, "2025-01-15");
        assert_eq!(row.cells[1].text, "TRUE");
        assert_eq!(row.cells[2].text, "0.5");
        assert_eq!(row.cells[3].text, "100");
    }

    #[test]
    fn repeated_empty_columns_skipped() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:spreadsheet>
      <table:table table:name="Sheet1">
        <table:table-row>
          <table:table-cell office:value-type="string"><text:p>A</text:p></table:table-cell>
          <table:table-cell table:number-columns-repeated="16000"/>
          <table:table-cell office:value-type="string"><text:p>B</text:p></table:table-cell>
        </table:table-row>
      </table:table>
    </office:spreadsheet>
  </office:body>
</office:document-content>"#;

        let body = parse_ods_body(xml, &NullDiagnostics).unwrap();
        let row = &body.sheets[0].rows[0];
        // Empty repeated cells should NOT be materialized; only A and B.
        assert_eq!(row.cells.len(), 2);
        assert_eq!(row.cells[0].text, "A");
        assert_eq!(row.cells[1].text, "B");
    }

    #[test]
    fn repeated_empty_rows_skipped() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:spreadsheet>
      <table:table table:name="Sheet1">
        <table:table-row>
          <table:table-cell office:value-type="string"><text:p>Data</text:p></table:table-cell>
        </table:table-row>
        <table:table-row table:number-rows-repeated="100000">
          <table:table-cell table:number-columns-repeated="256"/>
        </table:table-row>
      </table:table>
    </office:spreadsheet>
  </office:body>
</office:document-content>"#;

        let body = parse_ods_body(xml, &NullDiagnostics).unwrap();
        // Only the row with "Data" should remain.
        assert_eq!(body.sheets[0].rows.len(), 1);
        assert_eq!(body.sheets[0].rows[0].cells[0].text, "Data");
    }

    #[test]
    fn merged_cells() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:spreadsheet>
      <table:table table:name="Sheet1">
        <table:table-row>
          <table:table-cell table:number-columns-spanned="2" table:number-rows-spanned="1"
                           office:value-type="string"><text:p>Merged</text:p></table:table-cell>
          <table:covered-table-cell/>
          <table:table-cell office:value-type="string"><text:p>Right</text:p></table:table-cell>
        </table:table-row>
      </table:table>
    </office:spreadsheet>
  </office:body>
</office:document-content>"#;

        let body = parse_ods_body(xml, &NullDiagnostics).unwrap();
        let row = &body.sheets[0].rows[0];
        assert_eq!(row.cells.len(), 2); // Merged + Right (covered skipped).
        assert_eq!(row.cells[0].text, "Merged");
        assert_eq!(row.cells[0].col_span, 2);
        assert_eq!(row.cells[1].text, "Right");
    }
}
