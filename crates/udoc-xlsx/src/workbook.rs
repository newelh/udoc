//! Workbook structure parser for XLSX.
//!
//! Parses `xl/workbook.xml` to extract sheet names, ordering, visibility,
//! and the 1904 date epoch flag.

use std::sync::Arc;

use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use crate::error::{Result, ResultExt};
use udoc_containers::xml::{attr_value, XmlEvent, XmlReader};

/// Maximum number of sheets we'll process (safety limit).
const MAX_SHEETS: usize = 1_000;

/// Sheet visibility state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SheetVisibility {
    Visible,
    Hidden,
    VeryHidden,
}

/// A sheet entry from the workbook.
#[derive(Debug, Clone)]
pub(crate) struct SheetEntry {
    /// Display name of the sheet.
    pub name: String,
    /// Relationship ID linking to the worksheet part (e.g., "rId1").
    pub r_id: String,
    /// Sheet ID (sheetId attribute, used for cross-referencing).
    #[allow(dead_code)] // parsed from workbook XML; reserved for sheet cross-referencing
    pub sheet_id: u32,
    /// Visibility state.
    pub visibility: SheetVisibility,
}

/// Parsed workbook metadata.
#[derive(Debug)]
pub(crate) struct WorkbookInfo {
    /// Sheets in tab order.
    pub sheets: Vec<SheetEntry>,
    /// Whether the workbook uses the 1904 date epoch.
    pub date_1904: bool,
}

/// Parse workbook.xml to extract sheet list and epoch flag.
pub(crate) fn parse_workbook(data: &[u8], diag: &Arc<dyn DiagnosticsSink>) -> Result<WorkbookInfo> {
    let mut reader = XmlReader::new(data).context("creating XML reader for workbook")?;
    let mut sheets = Vec::new();
    let mut date_1904 = false;

    loop {
        match reader.next_event().context("reading workbook XML")? {
            XmlEvent::StartElement {
                local_name,
                attributes,
                ..
            } => match local_name.as_ref() {
                "sheet" => {
                    let name = match attr_value(&attributes, "name") {
                        Some(n) => n.to_string(),
                        None => {
                            diag.warning(Warning::new(
                                "XlsxMalformedWorkbook",
                                "sheet element missing name attribute, skipping",
                            ));
                            continue;
                        }
                    };

                    let r_id = attr_value(&attributes, "id")
                        .map(|s| s.to_string())
                        .unwrap_or_default();

                    let sheet_id: u32 = attr_value(&attributes, "sheetId")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);

                    let visibility = match attr_value(&attributes, "state") {
                        Some(s) if s.eq_ignore_ascii_case("hidden") => SheetVisibility::Hidden,
                        Some(s) if s.eq_ignore_ascii_case("veryHidden") => {
                            SheetVisibility::VeryHidden
                        }
                        _ => SheetVisibility::Visible,
                    };

                    if sheets.len() >= MAX_SHEETS {
                        diag.warning(Warning::new(
                            "XlsxSheetLimit",
                            format!("workbook has more than {MAX_SHEETS} sheets, truncating"),
                        ));
                        break;
                    }

                    sheets.push(SheetEntry {
                        name,
                        r_id,
                        sheet_id,
                        visibility,
                    });
                }
                "workbookPr" => {
                    if let Some(val) = attr_value(&attributes, "date1904") {
                        date_1904 = val == "1" || val.eq_ignore_ascii_case("true");
                    }
                }
                _ => {}
            },
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(WorkbookInfo { sheets, date_1904 })
}

#[cfg(test)]
mod tests {
    use udoc_core::diagnostics::NullDiagnostics;

    use super::*;

    fn null_diag() -> Arc<dyn DiagnosticsSink> {
        Arc::new(NullDiagnostics)
    }

    #[test]
    fn parse_basic_workbook() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
    xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
    <sheets>
        <sheet name="Sheet1" sheetId="1" r:id="rId1"/>
        <sheet name="Sheet2" sheetId="2" r:id="rId2"/>
    </sheets>
</workbook>"#;

        let info = parse_workbook(xml, &null_diag()).unwrap();
        assert_eq!(info.sheets.len(), 2);
        assert_eq!(info.sheets[0].name, "Sheet1");
        assert_eq!(info.sheets[1].name, "Sheet2");
        assert!(!info.date_1904);
    }

    #[test]
    fn parse_date_1904_flag() {
        let xml = br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <workbookPr date1904="1"/>
    <sheets>
        <sheet name="Sheet1" sheetId="1" r:id="rId1"/>
    </sheets>
</workbook>"#;

        let info = parse_workbook(xml, &null_diag()).unwrap();
        assert!(info.date_1904);
    }

    #[test]
    fn parse_hidden_sheets() {
        let xml = br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheets>
        <sheet name="Visible" sheetId="1" r:id="rId1"/>
        <sheet name="Hidden" sheetId="2" r:id="rId2" state="hidden"/>
        <sheet name="VeryHidden" sheetId="3" r:id="rId3" state="veryHidden"/>
    </sheets>
</workbook>"#;

        let info = parse_workbook(xml, &null_diag()).unwrap();
        assert_eq!(info.sheets.len(), 3);
        assert_eq!(info.sheets[0].visibility, SheetVisibility::Visible);
        assert_eq!(info.sheets[1].visibility, SheetVisibility::Hidden);
        assert_eq!(info.sheets[2].visibility, SheetVisibility::VeryHidden);
    }

    #[test]
    fn parse_missing_sheet_name_skipped() {
        let xml = br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheets>
        <sheet sheetId="1" r:id="rId1"/>
        <sheet name="Good" sheetId="2" r:id="rId2"/>
    </sheets>
</workbook>"#;

        let info = parse_workbook(xml, &null_diag()).unwrap();
        assert_eq!(info.sheets.len(), 1);
        assert_eq!(info.sheets[0].name, "Good");
    }

    #[test]
    fn parse_empty_workbook() {
        let xml = br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
    <sheets/>
</workbook>"#;

        let info = parse_workbook(xml, &null_diag()).unwrap();
        assert!(info.sheets.is_empty());
    }

    #[test]
    fn max_sheets_limit_warns() {
        // Build XML with MAX_SHEETS + 1 sheets.
        let mut xml = String::from(
            r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
                xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
    <sheets>"#,
        );
        for i in 0..=MAX_SHEETS {
            xml.push_str(&format!(
                r#"<sheet name="S{i}" sheetId="{}" r:id="rId{i}"/>"#,
                i + 1
            ));
        }
        xml.push_str("</sheets></workbook>");

        let collecting = Arc::new(udoc_core::diagnostics::CollectingDiagnostics::new());
        let diag: Arc<dyn DiagnosticsSink> = collecting.clone();
        let info = parse_workbook(xml.as_bytes(), &diag).unwrap();
        assert_eq!(info.sheets.len(), MAX_SHEETS);
        let warnings = collecting.warnings();
        assert!(warnings.iter().any(|w| w.kind == "XlsxSheetLimit"));
    }
}
