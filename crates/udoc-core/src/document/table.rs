//! Table types for the document model.
//!
//! These types live in the content spine. Geometry (bounding boxes) and
//! column specs live in the presentation overlay, keyed by NodeId.

use super::content::{collect_block_text, Block};
#[cfg(feature = "serde")]
use super::content::{Inline, SpanStyle};
use super::NodeId;

/// Base of the synthetic NodeId range. IDs at or above this value are
/// reserved for deserialization wrappers and are skipped by scan_blocks
/// when computing next_node_id.
#[cfg(feature = "serde")]
pub(super) const SYNTHETIC_ID_BASE: u64 = super::MAX_NODE_ID - 1_000_000;

// Thread-local counter for synthetic IDs. Reset at the start of each
// Document deserialization so long-running processes don't exhaust the
// 1M-ID range across multiple deserializations.
#[cfg(feature = "serde")]
std::thread_local! {
    static SYNTHETIC_COUNTER: std::cell::Cell<u64> = const { std::cell::Cell::new(super::MAX_NODE_ID - 1) };
}

/// Reset the synthetic ID counter. Called at the start of Document
/// deserialization to ensure each Document gets a fresh range.
#[cfg(feature = "serde")]
pub(super) fn reset_synthetic_ids() {
    SYNTHETIC_COUNTER.with(|c| c.set(super::MAX_NODE_ID - 1));
}

/// Allocate a synthetic NodeId for deserialization. Counts down from
/// MAX_NODE_ID - 1 to avoid collisions with real IDs (which count up
/// from 0). Used when reconstructing Paragraph/Inline wrappers for
/// the flattened "text" form of TableCell.
///
/// Returns IDs in the range `[SYNTHETIC_ID_BASE, MAX_NODE_ID)`.
/// Saturates at `SYNTHETIC_ID_BASE` to avoid entering the real ID space.
/// The counter is thread-local and reset per Document deserialization.
#[cfg(feature = "serde")]
fn next_synthetic_id() -> NodeId {
    SYNTHETIC_COUNTER.with(|c| {
        let current = c.get();
        if current <= SYNTHETIC_ID_BASE {
            // Saturated: return the base ID. Overlay entries keyed to this
            // ID may collide, but this only happens after 1M synthetic
            // allocations per Document, which is far beyond normal usage.
            // Step below the base to avoid re-checking the boundary.
            if current == SYNTHETIC_ID_BASE {
                c.set(SYNTHETIC_ID_BASE - 1);
            }
            return NodeId::new(SYNTHETIC_ID_BASE);
        }
        c.set(current - 1);
        NodeId::new(current)
    })
}

/// Table data in the content spine.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct TableData {
    pub rows: Vec<TableRow>,
    pub num_columns: usize,
    pub header_row_count: usize,
    pub may_continue_from_previous: bool,
    pub may_continue_to_next: bool,
}

/// A table row.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct TableRow {
    pub id: NodeId,
    pub cells: Vec<TableCell>,
    pub is_header: bool,
}

/// A table cell with rich content.
///
/// Custom serde: if the cell contains a single Paragraph with only plain-text
/// Inline::Text nodes, serialize as `{"text": "value", ...}` instead of
/// `{"content": [...], ...}`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TableCell {
    pub id: NodeId,
    /// Rich content: paragraphs, lists, nested tables.
    pub content: Vec<Block>,
    pub col_span: usize,
    pub row_span: usize,
    /// Typed value for spreadsheet cells.
    pub value: Option<CellValue>,
}

impl TableData {
    /// Create a new TableData. Computes num_columns and header_row_count from rows.
    pub fn new(rows: Vec<TableRow>) -> Self {
        let num_columns = rows
            .iter()
            .map(|r| r.cells.iter().map(|c| c.col_span).sum::<usize>())
            .max()
            .unwrap_or(0);
        let header_row_count = rows.iter().take_while(|r| r.is_header).count();
        Self {
            rows,
            num_columns,
            header_row_count,
            may_continue_from_previous: false,
            may_continue_to_next: false,
        }
    }
}

impl TableRow {
    /// Create a new non-header row.
    pub fn new(id: NodeId, cells: Vec<TableCell>) -> Self {
        Self {
            id,
            cells,
            is_header: false,
        }
    }

    /// Builder: mark this row as a header row.
    pub fn with_header(mut self) -> Self {
        self.is_header = true;
        self
    }
}

impl TableCell {
    /// Create a new cell with default spans (1x1) and no value.
    pub fn new(id: NodeId, content: Vec<Block>) -> Self {
        Self {
            id,
            content,
            col_span: 1,
            row_span: 1,
            value: None,
        }
    }

    /// Flatten cell content to plain text.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for (i, block) in self.content.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            collect_block_text(block, &mut out);
        }
        out
    }

    /// Whether this cell contains only a single plain-text paragraph.
    #[cfg(all(feature = "serde", test))]
    fn is_simple_text_cell(&self) -> bool {
        if self.content.len() != 1 {
            return false;
        }
        match &self.content[0] {
            Block::Paragraph { content, .. } => content
                .iter()
                .all(|inline| matches!(inline, Inline::Text { style, .. } if style.is_plain())),
            _ => false,
        }
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for TableCell {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;

        // Always use full form with content array. The flattened "text" form
        // loses inner NodeIds on round-trip (synthetic IDs replace originals),
        // which orphans any Overlay entries keyed to those inner nodes.
        let mut count = 4; // id, content, col_span, row_span
        if self.value.is_some() {
            count += 1;
        }
        let mut map = serializer.serialize_map(Some(count))?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("content", &self.content)?;
        map.serialize_entry("col_span", &self.col_span)?;
        map.serialize_entry("row_span", &self.row_span)?;
        if let Some(ref v) = self.value {
            map.serialize_entry("value", v)?;
        }
        map.end()
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for TableCell {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{MapAccess, Visitor};
        use std::fmt;

        struct TableCellVisitor;

        impl<'de> Visitor<'de> for TableCellVisitor {
            type Value = TableCell;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a table cell object")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut access: M) -> Result<Self::Value, M::Error> {
                let mut id: Option<NodeId> = None;
                let mut content: Option<Vec<Block>> = None;
                let mut text: Option<String> = None;
                let mut col_span: Option<usize> = None;
                let mut row_span: Option<usize> = None;
                let mut value: Option<CellValue> = None;

                while let Some(key) = access.next_key::<String>()? {
                    match key.as_str() {
                        "id" => id = Some(access.next_value()?),
                        "content" => content = Some(access.next_value()?),
                        "text" => text = Some(access.next_value()?),
                        "col_span" => col_span = Some(access.next_value()?),
                        "row_span" => row_span = Some(access.next_value()?),
                        "value" => value = Some(access.next_value()?),
                        _ => {
                            let _ = access.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }

                let id = id.ok_or_else(|| serde::de::Error::missing_field("id"))?;
                let col_span = col_span.unwrap_or(1);
                let row_span = row_span.unwrap_or(1);

                // If "text" key was present (flattened form), reconstruct
                // as a single Paragraph with an Inline::Text node.
                // Synthetic IDs count down from MAX_NODE_ID - 1 to avoid
                // collisions with real IDs (which count up from 0).
                let content = if let Some(text_val) = text {
                    vec![Block::Paragraph {
                        id: next_synthetic_id(),
                        content: vec![Inline::Text {
                            id: next_synthetic_id(),
                            text: text_val,
                            style: SpanStyle::default(),
                        }],
                    }]
                } else {
                    content.unwrap_or_default()
                };

                Ok(TableCell {
                    id,
                    content,
                    col_span,
                    row_span,
                    value,
                })
            }
        }

        deserializer.deserialize_map(TableCellVisitor)
    }
}

/// Typed cell value from spreadsheet formats.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum CellValue {
    Text(String),
    Number(f64),
    Boolean(bool),
    /// ISO 8601 date/datetime string. Stored as string because source
    /// formats use different date representations.
    Date(String),
    /// Formula with optional cached result.
    Formula {
        expression: String,
        result: Option<Box<CellValue>>,
    },
    /// Error value (#REF!, #VALUE!, etc.).
    Error(String),
}

// Custom serde for CellValue: internally tagged with "type" field.
// Can't use derive(Serialize) with #[serde(tag = "type")] because
// serde doesn't support tagged newtype variants containing non-string
// primitives (Number(f64), Boolean(bool)).
#[cfg(feature = "serde")]
impl serde::Serialize for CellValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            CellValue::Text(s) => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "text")?;
                map.serialize_entry("value", s)?;
                map.end()
            }
            CellValue::Number(n) => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "number")?;
                map.serialize_entry("value", n)?;
                map.end()
            }
            CellValue::Boolean(b) => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "boolean")?;
                map.serialize_entry("value", b)?;
                map.end()
            }
            CellValue::Date(s) => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "date")?;
                map.serialize_entry("value", s)?;
                map.end()
            }
            CellValue::Formula { expression, result } => {
                let mut count = 2; // type + expression
                if result.is_some() {
                    count += 1;
                }
                let mut map = serializer.serialize_map(Some(count))?;
                map.serialize_entry("type", "formula")?;
                map.serialize_entry("expression", expression)?;
                if let Some(ref r) = result {
                    map.serialize_entry("result", r)?;
                }
                map.end()
            }
            CellValue::Error(s) => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "error")?;
                map.serialize_entry("value", s)?;
                map.end()
            }
        }
    }
}

/// Maximum nesting depth for CellValue::Formula results. Prevents stack
/// overflow from pathological JSON with deeply nested formula chains.
/// Serde_json also enforces a 128-level limit, but this is an explicit guard.
///
/// not part of the public API. Re-exported as
/// `pub` from `udoc_core::document` only under the `test-internals`
/// feature. The `#[allow(dead_code)]` covers the build matrix where
/// neither `serde` (the only consumer of this constant inside the
/// crate) nor `test-internals` is enabled.
#[cfg(feature = "test-internals")]
pub const MAX_CELL_VALUE_DEPTH: usize = 32;
#[cfg(not(feature = "test-internals"))]
#[allow(dead_code)]
pub(crate) const MAX_CELL_VALUE_DEPTH: usize = 32;

#[cfg(feature = "serde")]
std::thread_local! {
    static CELL_VALUE_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for CellValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{MapAccess, Visitor};
        use std::fmt;

        /// A helper enum to capture the polymorphic "value" field, which
        /// can be a string, number, or boolean depending on the CellValue type.
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum RawValue {
            Bool(bool),
            Number(f64),
            Str(String),
        }

        struct CellValueVisitor;

        impl<'de> Visitor<'de> for CellValueVisitor {
            type Value = CellValue;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a cell value object with 'type' field")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut access: M) -> Result<Self::Value, M::Error> {
                let depth = CELL_VALUE_DEPTH.with(|d| d.get());
                if depth >= MAX_CELL_VALUE_DEPTH {
                    return Err(serde::de::Error::custom(format!(
                        "CellValue nesting depth exceeds maximum ({})",
                        MAX_CELL_VALUE_DEPTH
                    )));
                }

                let mut typ: Option<String> = None;
                let mut raw_value: Option<RawValue> = None;
                let mut expression: Option<String> = None;

                // Deserialize the "result" field with incremented depth.
                let mut result: Option<Box<CellValue>> = None;

                while let Some(key) = access.next_key::<String>()? {
                    match key.as_str() {
                        "type" => typ = Some(access.next_value()?),
                        "value" => raw_value = Some(access.next_value()?),
                        "expression" => expression = Some(access.next_value()?),
                        "result" => {
                            CELL_VALUE_DEPTH.with(|d| d.set(depth + 1));
                            let r = access.next_value();
                            CELL_VALUE_DEPTH.with(|d| d.set(depth));
                            result = Some(r?);
                        }
                        _ => {
                            let _ = access.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }

                let typ = typ.ok_or_else(|| serde::de::Error::missing_field("type"))?;

                match typ.as_str() {
                    "text" => match raw_value {
                        Some(RawValue::Str(s)) => Ok(CellValue::Text(s)),
                        Some(_) => Err(serde::de::Error::custom(
                            "expected string value for type 'text'",
                        )),
                        None => Err(serde::de::Error::missing_field("value")),
                    },
                    "number" => match raw_value {
                        Some(RawValue::Number(n)) => Ok(CellValue::Number(n)),
                        Some(_) => Err(serde::de::Error::custom(
                            "expected numeric value for type 'number'",
                        )),
                        None => Err(serde::de::Error::missing_field("value")),
                    },
                    "boolean" => match raw_value {
                        Some(RawValue::Bool(b)) => Ok(CellValue::Boolean(b)),
                        Some(_) => Err(serde::de::Error::custom(
                            "expected boolean value for type 'boolean'",
                        )),
                        None => Err(serde::de::Error::missing_field("value")),
                    },
                    "date" => match raw_value {
                        Some(RawValue::Str(s)) => Ok(CellValue::Date(s)),
                        Some(_) => Err(serde::de::Error::custom(
                            "expected string value for type 'date'",
                        )),
                        None => Err(serde::de::Error::missing_field("value")),
                    },
                    "formula" => {
                        let expr = expression
                            .ok_or_else(|| serde::de::Error::missing_field("expression"))?;
                        Ok(CellValue::Formula {
                            expression: expr,
                            result,
                        })
                    }
                    "error" => match raw_value {
                        Some(RawValue::Str(s)) => Ok(CellValue::Error(s)),
                        Some(_) => Err(serde::de::Error::custom(
                            "expected string value for type 'error'",
                        )),
                        None => Err(serde::de::Error::missing_field("value")),
                    },
                    other => Err(serde::de::Error::unknown_variant(
                        other,
                        &["text", "number", "boolean", "date", "formula", "error"],
                    )),
                }
            }
        }

        deserializer.deserialize_map(CellValueVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::content::{Inline, SpanStyle};

    #[test]
    fn cell_value_variants() {
        let text = CellValue::Text("hello".into());
        let number = CellValue::Number(3.15);
        let boolean = CellValue::Boolean(true);
        let date = CellValue::Date("2026-03-09".into());
        let formula = CellValue::Formula {
            expression: "=A1+B1".into(),
            result: Some(Box::new(CellValue::Number(42.0))),
        };
        let error = CellValue::Error("#REF!".into());

        // Verify they all clone and debug
        let _ = format!("{:?}", text.clone());
        let _ = format!("{:?}", number.clone());
        let _ = format!("{:?}", boolean.clone());
        let _ = format!("{:?}", date.clone());
        let _ = format!("{:?}", formula.clone());
        let _ = format!("{:?}", error.clone());
    }

    #[test]
    fn cell_value_equality() {
        assert_eq!(CellValue::Number(1.0), CellValue::Number(1.0));
        assert_ne!(CellValue::Number(1.0), CellValue::Number(2.0));
        assert_eq!(CellValue::Boolean(true), CellValue::Boolean(true));
        assert_eq!(CellValue::Text("x".into()), CellValue::Text("x".into()));
    }

    #[test]
    fn table_cell_text_empty() {
        let cell = TableCell {
            id: NodeId::new(0),
            content: vec![],
            col_span: 1,
            row_span: 1,
            value: None,
        };
        assert_eq!(cell.text(), "");
    }

    #[test]
    fn table_cell_text_single_paragraph() {
        let cell = TableCell {
            id: NodeId::new(0),
            content: vec![Block::Paragraph {
                id: NodeId::new(1),
                content: vec![Inline::Text {
                    id: NodeId::new(2),
                    text: "hello".into(),
                    style: SpanStyle::default(),
                }],
            }],
            col_span: 1,
            row_span: 1,
            value: Some(CellValue::Text("hello".into())),
        };
        assert_eq!(cell.text(), "hello");
    }

    #[test]
    fn table_data_num_columns_with_col_span() {
        let td = TableData::new(vec![TableRow::new(
            NodeId::new(0),
            vec![
                TableCell {
                    id: NodeId::new(1),
                    content: vec![],
                    col_span: 3,
                    row_span: 1,
                    value: None,
                },
                TableCell {
                    id: NodeId::new(2),
                    content: vec![],
                    col_span: 1,
                    row_span: 1,
                    value: None,
                },
            ],
        )]);
        // 2 cells but 4 logical columns (3 + 1)
        assert_eq!(td.num_columns, 4);
    }

    #[test]
    fn table_data_fields() {
        let td = TableData {
            rows: vec![TableRow {
                id: NodeId::new(0),
                cells: vec![
                    TableCell {
                        id: NodeId::new(1),
                        content: vec![],
                        col_span: 1,
                        row_span: 1,
                        value: None,
                    },
                    TableCell {
                        id: NodeId::new(2),
                        content: vec![],
                        col_span: 1,
                        row_span: 1,
                        value: None,
                    },
                ],
                is_header: true,
            }],
            num_columns: 2,
            header_row_count: 1,
            may_continue_from_previous: false,
            may_continue_to_next: true,
        };
        assert_eq!(td.rows.len(), 1);
        assert_eq!(td.num_columns, 2);
        assert!(td.rows[0].is_header);
        assert!(td.may_continue_to_next);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn simple_text_cell_detection() {
        // Simple: single paragraph, plain text
        let simple = TableCell {
            id: NodeId::new(0),
            content: vec![Block::Paragraph {
                id: NodeId::new(1),
                content: vec![Inline::Text {
                    id: NodeId::new(2),
                    text: "hello".into(),
                    style: SpanStyle::default(),
                }],
            }],
            col_span: 1,
            row_span: 1,
            value: None,
        };
        assert!(simple.is_simple_text_cell());

        // Not simple: bold text
        let styled = TableCell {
            id: NodeId::new(0),
            content: vec![Block::Paragraph {
                id: NodeId::new(1),
                content: vec![Inline::Text {
                    id: NodeId::new(2),
                    text: "hello".into(),
                    style: SpanStyle {
                        bold: true,
                        ..Default::default()
                    },
                }],
            }],
            col_span: 1,
            row_span: 1,
            value: None,
        };
        assert!(!styled.is_simple_text_cell());

        // Not simple: multiple paragraphs
        let multi = TableCell {
            id: NodeId::new(0),
            content: vec![
                Block::Paragraph {
                    id: NodeId::new(1),
                    content: vec![],
                },
                Block::Paragraph {
                    id: NodeId::new(2),
                    content: vec![],
                },
            ],
            col_span: 1,
            row_span: 1,
            value: None,
        };
        assert!(!multi.is_simple_text_cell());

        // Not simple: empty content
        let empty = TableCell {
            id: NodeId::new(0),
            content: vec![],
            col_span: 1,
            row_span: 1,
            value: None,
        };
        assert!(!empty.is_simple_text_cell());
    }

    #[cfg(feature = "serde")]
    #[test]
    fn synthetic_node_ids_no_collision() {
        // Adjacent cells with sequential IDs should not produce
        // colliding synthetic child IDs when deserialized from
        // flattened "text" form.
        let json = r#"[
            {"id": 0, "text": "Cell A"},
            {"id": 1, "text": "Cell B"}
        ]"#;
        let cells: Vec<TableCell> = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(cells.len(), 2);

        // Collect all NodeIds from both cells and their children.
        let mut ids = std::collections::HashSet::new();
        for cell in &cells {
            ids.insert(cell.id.value());
            for block in &cell.content {
                ids.insert(block.id().value());
                if let Block::Paragraph { content, .. } = block {
                    for inline in content {
                        ids.insert(inline.id().value());
                    }
                }
            }
        }
        // 2 cells + 2 paragraphs + 2 text inlines = 6 unique IDs
        assert_eq!(ids.len(), 6, "all NodeIds should be unique, got: {:?}", ids);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn cell_value_type_mismatch_errors() {
        // Type says "text" but value is a number.
        let json = r#"{"type": "text", "value": 42}"#;
        let result = serde_json::from_str::<CellValue>(json);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("expected string value for type 'text'"),
            "error was: {err}"
        );

        // Type says "number" but value is a string.
        let json = r#"{"type": "number", "value": "oops"}"#;
        let result = serde_json::from_str::<CellValue>(json);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("expected numeric value for type 'number'"),
            "error was: {err}"
        );

        // Type says "boolean" but value is a string.
        let json = r#"{"type": "boolean", "value": "true"}"#;
        let result = serde_json::from_str::<CellValue>(json);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("expected boolean value for type 'boolean'"),
            "error was: {err}"
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn cell_value_formula_depth_limit() {
        // Build a JSON with formulas nested deeper than MAX_CELL_VALUE_DEPTH.
        let depth = MAX_CELL_VALUE_DEPTH + 5;
        let mut json = String::from(r#"{"type":"formula","expression":"=A1","result":"#);
        for _ in 1..depth {
            json.push_str(r#"{"type":"formula","expression":"=A1","result":"#);
        }
        json.push_str(r#"{"type":"number","value":1}"#);
        for _ in 0..depth {
            json.push('}');
        }

        let result = serde_json::from_str::<CellValue>(&json);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("nesting depth exceeds maximum"),
            "error was: {err}"
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn cell_value_formula_within_depth_limit() {
        // A few levels of nesting should work fine.
        let json = r#"{
            "type": "formula",
            "expression": "=A1",
            "result": {
                "type": "formula",
                "expression": "=B1",
                "result": {"type": "number", "value": 42}
            }
        }"#;
        let val: CellValue = serde_json::from_str(json).expect("should deserialize");
        match val {
            CellValue::Formula { result, .. } => {
                assert!(result.is_some());
                match *result.unwrap() {
                    CellValue::Formula { result, .. } => {
                        assert_eq!(*result.unwrap(), CellValue::Number(42.0));
                    }
                    _ => panic!("expected nested formula"),
                }
            }
            _ => panic!("expected formula"),
        }
    }
}
