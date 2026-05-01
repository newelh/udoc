//! Text extraction and story assembly for DOC binary format.
//!
//! Sits between the piece table (raw text assembly) and the FormatBackend
//! (structured output). Computes story boundaries from FIB ccp* fields,
//! processes special characters (field codes, control characters), and
//! splits assembled text into paragraphs.
//!
//! Reference: MS-DOC 2.4.1 (document text), 2.16.26 (special characters)

use crate::error::{Error, Result, ResultExt};
use crate::fib::Fib;
use crate::piece_table::PieceTable;
use crate::{MAX_PARAGRAPHS, MAX_TEXT_LENGTH};

/// CP ranges for each text story in the document.
///
/// Boundaries are cumulative: footnotes start where body text ends,
/// headers start where footnotes end, etc. Each non-empty story's
/// last character is a paragraph mark (0x0D).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryBoundaries {
    /// Main document body text: CP 0..ccp_text.
    pub body: (u32, u32),
    /// Footnote text.
    pub footnotes: (u32, u32),
    /// Header/footer text.
    pub headers: (u32, u32),
    /// Annotation (comment) text.
    pub annotations: (u32, u32),
    /// Endnote text.
    pub endnotes: (u32, u32),
}

impl StoryBoundaries {
    /// Compute story boundaries from the FIB's ccp* fields.
    ///
    /// Each story occupies a contiguous CP range. The ranges are laid out
    /// sequentially: body, footnotes, headers, annotations, endnotes,
    /// textboxes, header textboxes. We only expose the five commonly
    /// needed stories here; textbox ranges can be added later if needed.
    pub fn from_fib(fib: &Fib) -> Self {
        let body_start = 0u32;
        let body_end = fib.ccp_text;

        let ftn_start = body_end;
        let ftn_end = ftn_start.saturating_add(fib.ccp_ftn);

        let hdd_start = ftn_end;
        let hdd_end = hdd_start.saturating_add(fib.ccp_hdd);

        let atn_start = hdd_end;
        let atn_end = atn_start.saturating_add(fib.ccp_atn);

        let edn_start = atn_end;
        let edn_end = edn_start.saturating_add(fib.ccp_edn);

        StoryBoundaries {
            body: (body_start, body_end),
            footnotes: (ftn_start, ftn_end),
            headers: (hdd_start, hdd_end),
            annotations: (atn_start, atn_end),
            endnotes: (edn_start, edn_end),
        }
    }
}

/// A single paragraph extracted from a DOC story.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocParagraph {
    /// Paragraph text with special characters processed.
    pub text: String,
    /// Starting character position (inclusive) in the full document.
    pub cp_start: u32,
    /// Ending character position (exclusive) in the full document.
    pub cp_end: u32,
}

/// Extract body text paragraphs from the document.
///
/// Assembles raw text via the piece table for the body story range,
/// processes special characters (field codes, control chars), and
/// splits into paragraphs on paragraph marks (0x0D).
pub fn extract_body_paragraphs(
    piece_table: &PieceTable,
    word_doc: &[u8],
    boundaries: &StoryBoundaries,
) -> Result<Vec<DocParagraph>> {
    extract_story_paragraphs(piece_table, word_doc, boundaries.body, "body")
}

/// Extract footnote paragraphs from the document.
///
/// Same pattern as body extraction but using the footnote CP range.
pub fn extract_footnote_paragraphs(
    piece_table: &PieceTable,
    word_doc: &[u8],
    boundaries: &StoryBoundaries,
) -> Result<Vec<DocParagraph>> {
    extract_story_paragraphs(piece_table, word_doc, boundaries.footnotes, "footnote")
}

/// Extract endnote paragraphs from the document.
///
/// Same pattern as body extraction but using the endnote CP range.
pub fn extract_endnote_paragraphs(
    piece_table: &PieceTable,
    word_doc: &[u8],
    boundaries: &StoryBoundaries,
) -> Result<Vec<DocParagraph>> {
    extract_story_paragraphs(piece_table, word_doc, boundaries.endnotes, "endnote")
}

/// Extract header/footer paragraphs from the document.
///
/// Same pattern as body extraction but using the headers CP range.
/// This extracts the entire header/footer story as paragraphs without
/// subdividing by PlcfHdd sections (v1 approach).
pub fn extract_header_footer_paragraphs(
    piece_table: &PieceTable,
    word_doc: &[u8],
    boundaries: &StoryBoundaries,
) -> Result<Vec<DocParagraph>> {
    extract_story_paragraphs(piece_table, word_doc, boundaries.headers, "header/footer")
}

/// Extract paragraphs from a story CP range.
///
/// Shared implementation for body, footnote, and endnote extraction.
/// Assembles text via the piece table, processes special characters,
/// and splits into paragraphs on paragraph marks (0x0D).
fn extract_story_paragraphs(
    piece_table: &PieceTable,
    word_doc: &[u8],
    range: (u32, u32),
    story_name: &str,
) -> Result<Vec<DocParagraph>> {
    let (start, end) = range;
    if start >= end {
        return Ok(Vec::new());
    }

    let raw = piece_table
        .assemble_text(word_doc, start, end)
        .context(format!("assembling {story_name} text from piece table"))?;

    if raw.len() > MAX_TEXT_LENGTH {
        return Err(Error::new(format!(
            "{story_name} text too large: {} bytes, maximum is {MAX_TEXT_LENGTH}",
            raw.len()
        )));
    }

    let processed = process_text(&raw);
    let paragraphs = split_paragraphs(&processed, start);

    if paragraphs.len() > MAX_PARAGRAPHS {
        return Err(Error::new(format!(
            "too many {story_name} paragraphs: {}, maximum is {MAX_PARAGRAPHS}",
            paragraphs.len()
        )));
    }

    Ok(paragraphs)
}

/// Process raw text by handling field codes and special characters.
///
/// Field handling: strips the field code (between \x13 and \x14), keeps
/// the display result (between \x14 and \x15), strips markers. If no
/// separator (\x14) is found before the field end (\x15), the field code
/// text is kept as fallback.
///
/// Special characters:
/// - \x01 (embedded object): dropped
/// - \x08 (drawn object anchor): dropped
/// - \x0B (vertical tab / column break): converted to newline
/// - \x0C (page break): converted to newline
/// - \x07 (cell/row mark): preserved for table detection
/// - \x0D (paragraph mark): preserved for splitting
/// - \x13/\x14/\x15 (field markers): handled by field stripping logic
pub fn process_text(raw: &str) -> String {
    let field_processed = strip_field_codes(raw);
    let mut result = String::with_capacity(field_processed.len());

    for ch in field_processed.chars() {
        match ch {
            '\x01' | '\x08' => {
                // Embedded object / drawn object anchor: drop
            }
            '\x0B' => {
                // Vertical tab / column break: treat as line break
                result.push('\n');
            }
            '\x0C' => {
                // Page break: convert to newline
                result.push('\n');
            }
            _ => {
                result.push(ch);
            }
        }
    }

    result
}

/// Strip field codes from text, keeping display results.
///
/// Fields in DOC are delimited by:
/// - \x13: field begin
/// - \x14: field separator (between code and result)
/// - \x15: field end
///
/// We strip the code portion and markers, keeping only the display result.
/// Handles nested fields by tracking nesting depth.
fn strip_field_codes(raw: &str) -> String {
    let mut result = String::with_capacity(raw.len());
    let chars: Vec<char> = raw.chars().collect();
    let len = chars.len();
    let mut i = 0;

    // Track field nesting. When depth > 0, we're inside at least one field.
    // We accumulate "code" text and "result" text separately for each level.
    let mut depth: usize = 0;
    // For each nesting level, track whether we've seen the separator.
    // We use a simple stack: when we see \x13, push false (no separator yet).
    // When we see \x14 at current depth, mark it. When we see \x15, pop.
    let mut seen_separator: Vec<bool> = Vec::new();
    // Accumulated text per level: (code_text, result_text)
    let mut level_text: Vec<(String, String)> = Vec::new();

    while i < len {
        let ch = chars[i];
        match ch {
            '\x13' => {
                // Field begin: push new nesting level
                depth += 1;
                seen_separator.push(false);
                level_text.push((String::new(), String::new()));
            }
            '\x14' if depth > 0 => {
                // Field separator: mark that we've seen it at current depth
                if let Some(last) = seen_separator.last_mut() {
                    *last = true;
                }
            }
            '\x15' if depth > 0 => {
                // Field end: pop level, emit result (or code as fallback)
                depth -= 1;
                let had_separator = seen_separator.pop().unwrap_or(false);
                let (code, display) = level_text.pop().unwrap_or_default();

                let emit = if had_separator { &display } else { &code };

                // Emit into parent level or top-level result
                if depth > 0 {
                    let parent = level_text.last_mut().unwrap();
                    let parent_has_sep = *seen_separator.last().unwrap_or(&false);
                    if parent_has_sep {
                        parent.1.push_str(emit);
                    } else {
                        parent.0.push_str(emit);
                    }
                } else {
                    result.push_str(emit);
                }
            }
            _ if depth > 0 => {
                // Inside a field: accumulate to code or result
                let current = level_text.last_mut().unwrap();
                let has_sep = *seen_separator.last().unwrap_or(&false);
                if has_sep {
                    current.1.push(ch);
                } else {
                    current.0.push(ch);
                }
            }
            _ => {
                // Outside all fields: emit directly
                result.push(ch);
            }
        }
        i += 1;
    }

    // If we exit with unclosed fields, emit whatever we have as fallback
    while let Some((code, display)) = level_text.pop() {
        let had_sep = seen_separator.pop().unwrap_or(false);
        let emit = if had_sep { &display } else { &code };
        result.push_str(emit);
    }

    result
}

/// Split processed text into paragraphs on paragraph marks (0x0D = '\r').
///
/// Each paragraph's cp_start/cp_end corresponds to the CP range in the
/// full document. The paragraph mark itself is consumed (not included in
/// the paragraph text). Empty trailing paragraphs from a final \r are
/// not emitted.
fn split_paragraphs(text: &str, base_cp: u32) -> Vec<DocParagraph> {
    let mut paragraphs = Vec::new();
    let mut cp = base_cp;

    for segment in text.split('\r') {
        let char_count = segment.chars().count() as u32;
        let cp_start = cp;
        // +1 for the paragraph mark that was split on (except possibly the last)
        let cp_end = cp.saturating_add(char_count);

        if !segment.is_empty() {
            paragraphs.push(DocParagraph {
                text: segment.to_string(),
                cp_start,
                cp_end,
            });
        }

        // Advance past the segment chars + the paragraph mark
        cp = cp_end.saturating_add(1);
    }

    paragraphs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::*;

    // ---------------------------------------------------------------
    // StoryBoundaries tests
    // ---------------------------------------------------------------

    #[test]
    fn story_boundaries_from_fib_basic() {
        let fib_data = build_fib(100, 0, 0, false);
        let fib = crate::fib::parse_fib(&fib_data).unwrap();
        let bounds = StoryBoundaries::from_fib(&fib);

        assert_eq!(bounds.body, (0, 100));
        assert_eq!(bounds.footnotes, (100, 100));
        assert_eq!(bounds.headers, (100, 100));
        assert_eq!(bounds.annotations, (100, 100));
        assert_eq!(bounds.endnotes, (100, 100));
    }

    #[test]
    fn story_boundaries_from_fib_with_stories() {
        // Build a FIB with multiple ccp fields populated
        let mut fib_data = build_fib(500, 0, 0, false);

        // Write ccp fields into FibRgLw97 manually
        // FibRgLw97 base = 0x22 + 14 + 2 = 0x32
        let rglw_base = 0x22 + 14 + 2;
        fib_data[rglw_base + 4 * 4..rglw_base + 4 * 4 + 4].copy_from_slice(&10u32.to_le_bytes()); // ccpFtn
        fib_data[rglw_base + 5 * 4..rglw_base + 5 * 4 + 4].copy_from_slice(&20u32.to_le_bytes()); // ccpHdd
        fib_data[rglw_base + 6 * 4..rglw_base + 6 * 4 + 4].copy_from_slice(&30u32.to_le_bytes()); // ccpAtn
        fib_data[rglw_base + 11 * 4..rglw_base + 11 * 4 + 4].copy_from_slice(&40u32.to_le_bytes()); // ccpEdn

        let fib = crate::fib::parse_fib(&fib_data).unwrap();
        let bounds = StoryBoundaries::from_fib(&fib);

        assert_eq!(bounds.body, (0, 500));
        assert_eq!(bounds.footnotes, (500, 510));
        assert_eq!(bounds.headers, (510, 530));
        assert_eq!(bounds.annotations, (530, 560));
        assert_eq!(bounds.endnotes, (560, 600));
    }

    // ---------------------------------------------------------------
    // Single paragraph extraction (no special chars)
    // ---------------------------------------------------------------

    #[test]
    fn single_paragraph_no_special_chars() {
        let text = b"Hello World";
        let clx = build_clx(&[(0, text.len() as u32, text, true)]);
        let pt = crate::piece_table::PieceTable::parse(&clx, 0, clx.len() as u32).unwrap();

        let bounds = StoryBoundaries {
            body: (0, text.len() as u32),
            footnotes: (text.len() as u32, text.len() as u32),
            headers: (text.len() as u32, text.len() as u32),
            annotations: (text.len() as u32, text.len() as u32),
            endnotes: (text.len() as u32, text.len() as u32),
        };

        let paragraphs = extract_body_paragraphs(&pt, text, &bounds).unwrap();
        assert_eq!(paragraphs.len(), 1);
        assert_eq!(paragraphs[0].text, "Hello World");
        assert_eq!(paragraphs[0].cp_start, 0);
        assert_eq!(paragraphs[0].cp_end, 11);
    }

    // ---------------------------------------------------------------
    // Multiple paragraphs (splitting on \r)
    // ---------------------------------------------------------------

    #[test]
    fn multiple_paragraphs_split_on_cr() {
        let text = b"First\rSecond\rThird";
        let clx = build_clx(&[(0, text.len() as u32, text, true)]);
        let pt = crate::piece_table::PieceTable::parse(&clx, 0, clx.len() as u32).unwrap();

        let bounds = StoryBoundaries {
            body: (0, text.len() as u32),
            footnotes: (text.len() as u32, text.len() as u32),
            headers: (text.len() as u32, text.len() as u32),
            annotations: (text.len() as u32, text.len() as u32),
            endnotes: (text.len() as u32, text.len() as u32),
        };

        let paragraphs = extract_body_paragraphs(&pt, text, &bounds).unwrap();
        assert_eq!(paragraphs.len(), 3);
        assert_eq!(paragraphs[0].text, "First");
        assert_eq!(paragraphs[0].cp_start, 0);
        assert_eq!(paragraphs[0].cp_end, 5);
        assert_eq!(paragraphs[1].text, "Second");
        assert_eq!(paragraphs[1].cp_start, 6);
        assert_eq!(paragraphs[1].cp_end, 12);
        assert_eq!(paragraphs[2].text, "Third");
        assert_eq!(paragraphs[2].cp_start, 13);
        assert_eq!(paragraphs[2].cp_end, 18);
    }

    // ---------------------------------------------------------------
    // Trailing paragraph mark produces no empty trailing paragraph
    // ---------------------------------------------------------------

    #[test]
    fn trailing_paragraph_mark_no_empty() {
        let text = b"Content\r";
        let clx = build_clx(&[(0, text.len() as u32, text, true)]);
        let pt = crate::piece_table::PieceTable::parse(&clx, 0, clx.len() as u32).unwrap();

        let bounds = StoryBoundaries {
            body: (0, text.len() as u32),
            footnotes: (text.len() as u32, text.len() as u32),
            headers: (text.len() as u32, text.len() as u32),
            annotations: (text.len() as u32, text.len() as u32),
            endnotes: (text.len() as u32, text.len() as u32),
        };

        let paragraphs = extract_body_paragraphs(&pt, text, &bounds).unwrap();
        assert_eq!(paragraphs.len(), 1);
        assert_eq!(paragraphs[0].text, "Content");
    }

    // ---------------------------------------------------------------
    // Special character handling: field codes stripped
    // ---------------------------------------------------------------

    #[test]
    fn field_codes_stripped() {
        // Field with code and result: \x13 HYPERLINK "url" \x14 display text \x15
        let input = "before \x13 HYPERLINK \"url\" \x14click here\x15 after";
        let result = process_text(input);
        assert_eq!(result, "before click here after");
    }

    #[test]
    fn field_code_no_separator_fallback() {
        // Field with no separator: use code text as fallback
        let input = "before \x13PAGE\x15 after";
        let result = process_text(input);
        assert_eq!(result, "before PAGE after");
    }

    #[test]
    fn nested_field_codes() {
        // Nested field: outer has result containing inner field
        let input = "\x13outer code\x14prefix \x13inner code\x14inner result\x15 suffix\x15";
        let result = process_text(input);
        assert_eq!(result, "prefix inner result suffix");
    }

    // ---------------------------------------------------------------
    // Empty body text (ccp_text = 0)
    // ---------------------------------------------------------------

    #[test]
    fn empty_body_text() {
        let clx = build_clx(&[]);
        let pt = crate::piece_table::PieceTable::parse(&clx, 0, clx.len() as u32).unwrap();

        let bounds = StoryBoundaries {
            body: (0, 0),
            footnotes: (0, 0),
            headers: (0, 0),
            annotations: (0, 0),
            endnotes: (0, 0),
        };

        let paragraphs = extract_body_paragraphs(&pt, &[], &bounds).unwrap();
        assert!(paragraphs.is_empty());
    }

    // ---------------------------------------------------------------
    // Cell marks preserved (0x07 stays in text for table detection)
    // ---------------------------------------------------------------

    #[test]
    fn cell_marks_preserved() {
        let input = "cell1\x07cell2\x07\x07";
        let result = process_text(input);
        assert_eq!(result, "cell1\x07cell2\x07\x07");
    }

    // ---------------------------------------------------------------
    // Object placeholders dropped (0x01, 0x08 removed)
    // ---------------------------------------------------------------

    #[test]
    fn object_placeholders_dropped() {
        let input = "before\x01middle\x08after";
        let result = process_text(input);
        assert_eq!(result, "beforemiddleafter");
    }

    // ---------------------------------------------------------------
    // Page break and vertical tab converted
    // ---------------------------------------------------------------

    #[test]
    fn page_break_and_vtab_converted() {
        let input = "line1\x0Bline2\x0Cline3";
        let result = process_text(input);
        assert_eq!(result, "line1\nline2\nline3");
    }

    // ---------------------------------------------------------------
    // Integration: build_minimal_doc -> PieceTable -> extract
    // ---------------------------------------------------------------

    #[test]
    fn integration_minimal_doc_roundtrip() {
        use std::sync::Arc;
        use udoc_containers::cfb::CfbArchive;
        use udoc_core::diagnostics::NullDiagnostics;

        let doc_bytes = build_minimal_doc("Hello\rWorld\rDOC");
        let diag = Arc::new(NullDiagnostics);
        let archive = CfbArchive::new(&doc_bytes, diag).unwrap();

        let wd_entry = archive.find("WordDocument").unwrap();
        let wd_data = archive.read(wd_entry).unwrap();
        let fib = crate::fib::parse_fib(&wd_data).unwrap();

        let tbl_entry = archive.find("0Table").unwrap();
        let tbl_data = archive.read(tbl_entry).unwrap();
        let pt = crate::piece_table::PieceTable::parse(&tbl_data, fib.fc_clx, fib.lcb_clx).unwrap();

        let bounds = StoryBoundaries::from_fib(&fib);
        let paragraphs = extract_body_paragraphs(&pt, &wd_data, &bounds).unwrap();

        assert_eq!(paragraphs.len(), 3);
        assert_eq!(paragraphs[0].text, "Hello");
        assert_eq!(paragraphs[1].text, "World");
        assert_eq!(paragraphs[2].text, "DOC");
    }

    // ---------------------------------------------------------------
    // Unit tests for internal helpers
    // ---------------------------------------------------------------

    #[test]
    fn split_paragraphs_tracks_cp_offsets() {
        let text = "abc\rde\rf";
        let paras = split_paragraphs(text, 10);
        assert_eq!(paras.len(), 3);
        // "abc" at CP 10..13, then \r at 13, then "de" at 14..16, \r at 16, "f" at 17..18
        assert_eq!(paras[0].cp_start, 10);
        assert_eq!(paras[0].cp_end, 13);
        assert_eq!(paras[1].cp_start, 14);
        assert_eq!(paras[1].cp_end, 16);
        assert_eq!(paras[2].cp_start, 17);
        assert_eq!(paras[2].cp_end, 18);
    }

    #[test]
    fn strip_field_codes_unclosed_field() {
        // Unclosed field: emit code text as fallback
        let input = "before \x13orphan code";
        let result = strip_field_codes(input);
        assert_eq!(result, "before orphan code");
    }

    #[test]
    fn process_text_combined() {
        // Combine multiple special chars in one pass
        let input = "A\x01B\x08C\x0BD\x0CE\x13code\x14result\x15F\x07G\rH";
        let result = process_text(input);
        // \x01 and \x08 dropped, \x0B and \x0C become \n, field -> "result",
        // \x07 and \r preserved
        assert_eq!(result, "ABC\nD\nEresultF\x07G\rH");
    }

    // ---------------------------------------------------------------
    // Footnote/endnote extraction from story boundaries
    // ---------------------------------------------------------------

    #[test]
    fn footnote_extraction_from_story() {
        // Build text: body "Hello\r" + footnote "Note1\r"
        let body_text = b"Hello\r";
        let ftn_text = b"Note1\r";
        let mut all_text = Vec::new();
        all_text.extend_from_slice(body_text);
        all_text.extend_from_slice(ftn_text);

        let clx = build_clx(&[(0, all_text.len() as u32, &all_text, true)]);
        let pt = crate::piece_table::PieceTable::parse(&clx, 0, clx.len() as u32).unwrap();

        let bounds = StoryBoundaries {
            body: (0, 6),       // "Hello\r"
            footnotes: (6, 12), // "Note1\r"
            headers: (12, 12),
            annotations: (12, 12),
            endnotes: (12, 12),
        };

        let body = extract_body_paragraphs(&pt, &all_text, &bounds).unwrap();
        assert_eq!(body.len(), 1);
        assert_eq!(body[0].text, "Hello");

        let footnotes = extract_footnote_paragraphs(&pt, &all_text, &bounds).unwrap();
        assert_eq!(footnotes.len(), 1);
        assert_eq!(footnotes[0].text, "Note1");
        assert_eq!(footnotes[0].cp_start, 6);
        assert_eq!(footnotes[0].cp_end, 11);
    }

    #[test]
    fn endnote_extraction_from_story() {
        // Build text: body "Doc\r" + endnote "End1\r"
        let body_text = b"Doc\r";
        let edn_text = b"End1\r";
        let mut all_text = Vec::new();
        all_text.extend_from_slice(body_text);
        all_text.extend_from_slice(edn_text);

        let clx = build_clx(&[(0, all_text.len() as u32, &all_text, true)]);
        let pt = crate::piece_table::PieceTable::parse(&clx, 0, clx.len() as u32).unwrap();

        let bounds = StoryBoundaries {
            body: (0, 4),
            footnotes: (4, 4),
            headers: (4, 4),
            annotations: (4, 4),
            endnotes: (4, 9), // "End1\r"
        };

        let endnotes = extract_endnote_paragraphs(&pt, &all_text, &bounds).unwrap();
        assert_eq!(endnotes.len(), 1);
        assert_eq!(endnotes[0].text, "End1");
    }

    #[test]
    fn empty_footnote_range() {
        let text = b"Hello";
        let clx = build_clx(&[(0, text.len() as u32, text, true)]);
        let pt = crate::piece_table::PieceTable::parse(&clx, 0, clx.len() as u32).unwrap();

        let bounds = StoryBoundaries {
            body: (0, 5),
            footnotes: (5, 5),
            headers: (5, 5),
            annotations: (5, 5),
            endnotes: (5, 5),
        };

        let footnotes = extract_footnote_paragraphs(&pt, text, &bounds).unwrap();
        assert!(footnotes.is_empty());

        let endnotes = extract_endnote_paragraphs(&pt, text, &bounds).unwrap();
        assert!(endnotes.is_empty());
    }

    // ---------------------------------------------------------------
    // Header/footer extraction from story boundaries
    // ---------------------------------------------------------------

    #[test]
    fn header_footer_extraction_from_story() {
        // Build text: body "Hello\r" + hdd "Header1\r"
        let body_text = b"Hello\r";
        let hdd_text = b"Header1\r";
        let mut all_text = Vec::new();
        all_text.extend_from_slice(body_text);
        all_text.extend_from_slice(hdd_text);

        let clx = build_clx(&[(0, all_text.len() as u32, &all_text, true)]);
        let pt = crate::piece_table::PieceTable::parse(&clx, 0, clx.len() as u32).unwrap();

        let bounds = StoryBoundaries {
            body: (0, 6), // "Hello\r"
            footnotes: (6, 6),
            headers: (6, 14), // "Header1\r"
            annotations: (14, 14),
            endnotes: (14, 14),
        };

        let body = extract_body_paragraphs(&pt, &all_text, &bounds).unwrap();
        assert_eq!(body.len(), 1);
        assert_eq!(body[0].text, "Hello");

        let headers = extract_header_footer_paragraphs(&pt, &all_text, &bounds).unwrap();
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].text, "Header1");
        assert_eq!(headers[0].cp_start, 6);
        assert_eq!(headers[0].cp_end, 13);
    }

    #[test]
    fn empty_header_footer_range() {
        let text = b"Hello";
        let clx = build_clx(&[(0, text.len() as u32, text, true)]);
        let pt = crate::piece_table::PieceTable::parse(&clx, 0, clx.len() as u32).unwrap();

        let bounds = StoryBoundaries {
            body: (0, 5),
            footnotes: (5, 5),
            headers: (5, 5),
            annotations: (5, 5),
            endnotes: (5, 5),
        };

        let headers = extract_header_footer_paragraphs(&pt, text, &bounds).unwrap();
        assert!(headers.is_empty());
    }

    #[test]
    fn multiple_header_footer_paragraphs() {
        let body_text = b"Body\r";
        let hdd_text = b"Hdr1\rFtr1\r";
        let mut all_text = Vec::new();
        all_text.extend_from_slice(body_text);
        all_text.extend_from_slice(hdd_text);

        let clx = build_clx(&[(0, all_text.len() as u32, &all_text, true)]);
        let pt = crate::piece_table::PieceTable::parse(&clx, 0, clx.len() as u32).unwrap();

        let bounds = StoryBoundaries {
            body: (0, 5),
            footnotes: (5, 5),
            headers: (5, 15), // "Hdr1\rFtr1\r"
            annotations: (15, 15),
            endnotes: (15, 15),
        };

        let headers = extract_header_footer_paragraphs(&pt, &all_text, &bounds).unwrap();
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].text, "Hdr1");
        assert_eq!(headers[1].text, "Ftr1");
    }

    #[test]
    fn header_footer_with_footnotes_and_endnotes() {
        // Full story layout: body + footnotes + headers + endnotes
        let body_text = b"Doc\r";
        let ftn_text = b"Fn1\r";
        let hdd_text = b"Hdr\r";
        let edn_text = b"En1\r";
        let mut all_text = Vec::new();
        all_text.extend_from_slice(body_text);
        all_text.extend_from_slice(ftn_text);
        all_text.extend_from_slice(hdd_text);
        all_text.extend_from_slice(edn_text);

        let clx = build_clx(&[(0, all_text.len() as u32, &all_text, true)]);
        let pt = crate::piece_table::PieceTable::parse(&clx, 0, clx.len() as u32).unwrap();

        let bounds = StoryBoundaries {
            body: (0, 4),
            footnotes: (4, 8),
            headers: (8, 12),
            annotations: (12, 12),
            endnotes: (12, 16),
        };

        let body = extract_body_paragraphs(&pt, &all_text, &bounds).unwrap();
        assert_eq!(body.len(), 1);
        assert_eq!(body[0].text, "Doc");

        let ftn = extract_footnote_paragraphs(&pt, &all_text, &bounds).unwrap();
        assert_eq!(ftn.len(), 1);
        assert_eq!(ftn[0].text, "Fn1");

        let hdd = extract_header_footer_paragraphs(&pt, &all_text, &bounds).unwrap();
        assert_eq!(hdd.len(), 1);
        assert_eq!(hdd[0].text, "Hdr");

        let edn = extract_endnote_paragraphs(&pt, &all_text, &bounds).unwrap();
        assert_eq!(edn.len(), 1);
        assert_eq!(edn[0].text, "En1");
    }

    #[test]
    fn multiple_footnote_paragraphs() {
        let body_text = b"Body\r";
        let ftn_text = b"Fn1\rFn2\r";
        let mut all_text = Vec::new();
        all_text.extend_from_slice(body_text);
        all_text.extend_from_slice(ftn_text);

        let clx = build_clx(&[(0, all_text.len() as u32, &all_text, true)]);
        let pt = crate::piece_table::PieceTable::parse(&clx, 0, clx.len() as u32).unwrap();

        let bounds = StoryBoundaries {
            body: (0, 5),
            footnotes: (5, 13), // "Fn1\rFn2\r"
            headers: (13, 13),
            annotations: (13, 13),
            endnotes: (13, 13),
        };

        let footnotes = extract_footnote_paragraphs(&pt, &all_text, &bounds).unwrap();
        assert_eq!(footnotes.len(), 2);
        assert_eq!(footnotes[0].text, "Fn1");
        assert_eq!(footnotes[1].text, "Fn2");
    }
}
