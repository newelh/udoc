//! GFM table parsing helper.
//!
//! Detects and parses GFM-style tables with pipe delimiters and
//! separator rows. Used by the block parser when a pipe row is
//! followed by a valid separator line.

use std::collections::HashMap;

use crate::inline::{parse_inlines, MdInline};

/// Maximum cells per GFM table row (SEC #62  round-2 audit).
///
/// A markdown row of `|` * 100_000 forces the parser to allocate one
/// cell per `|` and call `parse_inlines` on each, which in turn runs the
/// emphasis-flanking algorithm per cell. CVSS 5.3 DoS otherwise; cap
/// at 1024 (real GFM tables almost never exceed 20 columns).
pub const MAX_TABLE_CELLS_PER_ROW: usize = 1024;

/// Count the number of columns from the separator line.
pub fn count_columns(separator: &str) -> usize {
    let trimmed = separator
        .trim()
        .trim_start_matches('|')
        .trim_end_matches('|');
    trimmed.split('|').count().min(MAX_TABLE_CELLS_PER_ROW)
}

/// Parse a single table row into cells of inline content.
///
/// Handles escaped pipes (`\|`) within cells, splitting only on unescaped `|`.
/// Caps cell count at [`MAX_TABLE_CELLS_PER_ROW`].
pub fn parse_row(line: &str, link_defs: &HashMap<String, String>) -> Vec<Vec<MdInline>> {
    let trimmed = line.trim();
    // Strip optional leading/trailing pipes.
    let inner = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    split_pipe_aware(inner)
        .iter()
        .take(MAX_TABLE_CELLS_PER_ROW)
        .map(|cell| {
            let cell = cell.trim();
            parse_inlines(cell, link_defs)
        })
        .collect()
}

/// Split a string on unescaped `|` characters, respecting backslash escapes.
///
/// Uses byte-range tracking instead of building a String per cell. Safe because
/// `|` and `\` are single-byte ASCII, so byte indices from scanning always land
/// on char boundaries.
fn split_pipe_aware(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut cells = Vec::new();
    let mut cell_start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'|' {
            // Escaped pipe: skip both characters.
            i += 2;
        } else if bytes[i] == b'|' {
            cells.push(&s[cell_start..i]);
            cell_start = i + 1;
            i += 1;
        } else {
            i += 1;
        }
    }
    cells.push(&s[cell_start..]);
    cells
}

/// Check if a line is a valid GFM table separator.
pub fn is_separator(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return false;
    }
    let inner = trimmed.trim_start_matches('|').trim_end_matches('|');
    if inner.is_empty() {
        return false;
    }
    let mut has_valid_cell = false;
    for cell in inner.split('|') {
        let cell = cell.trim();
        if cell.is_empty() {
            continue;
        }
        let stripped = cell.trim_start_matches(':').trim_end_matches(':');
        if stripped.is_empty() || !stripped.chars().all(|c| c == '-') {
            return false;
        }
        has_valid_cell = true;
    }
    has_valid_cell
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Result of parsing a GFM table (test-only convenience).
    struct TableParseResult {
        header: Vec<Vec<MdInline>>,
        rows: Vec<Vec<Vec<MdInline>>>,
        col_count: usize,
    }

    /// Parse a GFM table from header, separator, and data lines (test-only).
    fn parse_table(
        header_line: &str,
        separator_line: &str,
        data_lines: &[&str],
        link_defs: &HashMap<String, String>,
    ) -> TableParseResult {
        let col_count = count_columns(separator_line);
        let header = parse_row(header_line, link_defs);
        let rows: Vec<Vec<Vec<MdInline>>> = data_lines
            .iter()
            .map(|line| parse_row(line, link_defs))
            .collect();
        TableParseResult {
            header,
            rows,
            col_count,
        }
    }

    fn no_defs() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn separator_detection() {
        assert!(is_separator("| --- | --- |"));
        assert!(is_separator("|---|---|"));
        assert!(is_separator("| :--- | ---: | :---: |"));
        assert!(is_separator("| --- |"));
        assert!(!is_separator("| abc | def |"));
        assert!(!is_separator("no pipes here"));
        assert!(!is_separator("|  |"));
    }

    #[test]
    fn column_count() {
        assert_eq!(count_columns("| --- | --- |"), 2);
        assert_eq!(count_columns("|---|---|---|"), 3);
        assert_eq!(count_columns("| --- |"), 1);
    }

    #[test]
    fn parse_simple_row() {
        let cells = parse_row("| A | B | C |", &no_defs());
        assert_eq!(cells.len(), 3);
    }

    #[test]
    fn parse_row_without_outer_pipes() {
        let cells = parse_row("A | B | C", &no_defs());
        assert_eq!(cells.len(), 3);
    }

    #[test]
    fn parse_full_table() {
        let result = parse_table(
            "| Name | Age |",
            "| --- | --- |",
            &["| Alice | 30 |", "| Bob | 25 |"],
            &no_defs(),
        );
        assert_eq!(result.col_count, 2);
        assert_eq!(result.header.len(), 2);
        assert_eq!(result.rows.len(), 2);
    }

    #[test]
    fn table_with_formatting() {
        let cells = parse_row("| **bold** | *italic* |", &no_defs());
        assert_eq!(cells.len(), 2);
    }

    #[test]
    fn empty_cells() {
        let cells = parse_row("| | data | |", &no_defs());
        assert_eq!(cells.len(), 3);
    }

    #[test]
    fn multibyte_utf8_in_cells() {
        // Regression: split_pipe_aware used byte indexing which panicked on
        // multi-byte UTF-8 characters near pipe delimiters.
        let cells = parse_row(
            "| cafe\u{0301} | \u{00FC}ber | \u{4e16}\u{754c} |",
            &no_defs(),
        );
        assert_eq!(cells.len(), 3, "got {cells:?}");
    }

    #[test]
    fn escaped_pipe_in_cell() {
        let cells = parse_row("| a \\| b | c |", &no_defs());
        assert_eq!(
            cells.len(),
            2,
            "escaped pipe should not split: got {cells:?}"
        );
    }
}
