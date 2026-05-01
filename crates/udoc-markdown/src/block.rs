//! Block-level Markdown parser.
//!
//! Two-phase approach: (1) classify each line, (2) group lines into blocks
//! using a state machine. Handles ATX headings, paragraphs, fenced/indented
//! code blocks, thematic breaks, lists, block quotes, and GFM tables.

use std::collections::HashMap;

use crate::inline::{parse_inlines_with_warnings, MdInline};
use crate::table;

/// Maximum nesting depth for block quotes and lists.
const MAX_DEPTH: usize = udoc_core::MAX_NESTING_DEPTH;

/// Maximum warnings before we stop collecting them.
const MAX_WARNINGS: usize = 1000;

/// A parsed markdown block.
#[derive(Debug, Clone)]
pub enum MdBlock {
    Heading {
        level: u8,
        content: Vec<MdInline>,
    },
    Paragraph {
        content: Vec<MdInline>,
    },
    CodeBlock {
        text: String,
        language: Option<String>,
    },
    ThematicBreak,
    List {
        items: Vec<MdListItem>,
        ordered: bool,
        start: u64,
    },
    Table {
        header: Vec<Vec<MdInline>>,
        rows: Vec<Vec<Vec<MdInline>>>,
        col_count: usize,
    },
    Blockquote {
        children: Vec<MdBlock>,
    },
    // Block-level image: ![alt](url)
    Image {
        alt: String,
        url: String,
    },
}

/// A list item with content blocks.
#[derive(Debug, Clone)]
pub struct MdListItem {
    pub content: Vec<MdBlock>,
}

/// Parse markdown source text into a list of blocks.
///
/// Uses a two-pass approach: first scans for link reference definitions
/// so that forward references resolve correctly, then parses blocks.
pub fn parse_blocks(input: &str) -> ParseResult {
    // Normalize line endings once, then reuse for both pre-scan and parse.
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
    let link_defs = pre_scan_link_defs(&normalized);
    let mut parser = BlockParser::from_normalized(&normalized, &link_defs);
    parser.parse();
    let blocks = parser.blocks;
    let warnings = parser.warnings;
    ParseResult {
        blocks,
        link_defs,
        warnings,
    }
}

/// Pre-scan input for link reference definitions so forward references work.
/// Expects already-normalized input (LF line endings).
///
/// Strips blockquote prefixes (`> `) and list markers (`- `, `1. `, etc.)
/// before checking, so link ref defs inside blockquotes and lists are found
/// upfront. This means the block parser never needs to insert new defs,
/// allowing `link_defs` to be a shared `&HashMap` instead of cloned per
/// recursive call.
fn pre_scan_link_defs(input: &str) -> HashMap<String, String> {
    let mut defs = HashMap::new();
    for line in input.lines() {
        let mut trimmed = line.trim();
        // Strip blockquote markers.
        while let Some(rest) = trimmed
            .strip_prefix("> ")
            .or_else(|| trimmed.strip_prefix('>'))
        {
            trimmed = rest.trim_start();
        }
        // Strip list markers (may be nested inside blockquotes).
        while let Some(rest) = strip_list_marker_prefix(trimmed) {
            trimmed = rest.trim_start();
        }
        if let Some(LineKind::LinkRefDef { label, url }) = try_link_ref_def(trimmed) {
            defs.insert(label.to_lowercase(), url);
        }
    }
    defs
}

/// Strip a single list marker prefix from a line, returning the rest.
/// Handles `- `, `* `, `+ `, and `1. ` / `1) ` ordered markers.
fn strip_list_marker_prefix(s: &str) -> Option<&str> {
    for marker in &["- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(marker) {
            return Some(rest);
        }
    }
    // Ordered: digits followed by `. ` or `) `.
    let digit_end = s.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digit_end > 0 && digit_end <= 9 {
        let after = &s[digit_end..];
        if let Some(rest) = after
            .strip_prefix(". ")
            .or_else(|| after.strip_prefix(") "))
        {
            return Some(rest);
        }
    }
    None
}

/// A structured warning emitted during parsing: (kind, message).
pub type MdWarning = (String, String);

/// Result of block-level parsing.
pub struct ParseResult {
    pub blocks: Vec<MdBlock>,
    #[allow(dead_code)] // exposed for callers that need reference-link resolution metadata
    pub link_defs: HashMap<String, String>,
    pub warnings: Vec<MdWarning>,
}

/// Line classification for the block parser.
#[derive(Debug)]
enum LineKind {
    Blank,
    AtxHeading {
        level: u8,
        content: String,
    },
    ThematicBreak,
    FencedCodeOpen {
        fence_char: char,
        fence_len: usize,
        language: Option<String>,
        indent: usize,
    },
    IndentedCode {
        text: String,
    },
    UnorderedListMarker {
        indent: usize,
        content: String,
    },
    OrderedListMarker {
        indent: usize,
        start: u64,
        content: String,
    },
    BlockquotePrefix {
        rest: String,
    },
    LinkRefDef {
        label: String,
        url: String,
    },
    // Potential table row (needs lookahead to confirm).
    PipeRow {
        raw: String,
    },
    Text {
        text: String,
    },
}

struct BlockParser<'d> {
    lines: Vec<String>,
    pos: usize,
    blocks: Vec<MdBlock>,
    link_defs: &'d HashMap<String, String>,
    warnings: Vec<MdWarning>,
    /// Base nesting depth inherited from parent parser (for blockquote/list nesting).
    base_depth: usize,
}

impl<'d> BlockParser<'d> {
    /// Create a parser from already-normalized input (LF line endings).
    /// Used by `parse_blocks()` to avoid double normalization, and by
    /// recursive callers whose input is built from already-normalized lines.
    fn from_normalized(input: &str, link_defs: &'d HashMap<String, String>) -> Self {
        let lines: Vec<String> = input.split('\n').map(String::from).collect();
        Self {
            lines,
            pos: 0,
            blocks: Vec::new(),
            link_defs,
            warnings: Vec::new(),
            base_depth: 0,
        }
    }

    fn warn(&mut self, kind: &str, msg: String) {
        if self.warnings.len() < MAX_WARNINGS {
            self.warnings.push((kind.to_string(), msg));
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.lines.len()
    }

    fn current_line(&self) -> &str {
        &self.lines[self.pos]
    }

    fn advance(&mut self) {
        self.pos += 1;
    }

    fn parse(&mut self) {
        while !self.at_end() {
            self.parse_block(self.base_depth);
        }
    }

    fn parse_block(&mut self, depth: usize) {
        if depth > MAX_DEPTH {
            self.warn(
                "MaxDepthExceeded",
                format!(
                    "nesting depth exceeded {} at line {}",
                    MAX_DEPTH,
                    self.pos + 1
                ),
            );
            // Skip the line to avoid infinite loop.
            self.advance();
            return;
        }

        if self.at_end() {
            return;
        }

        let kind = self.classify_line(self.current_line());

        match kind {
            LineKind::Blank => {
                self.advance();
            }
            LineKind::AtxHeading { level, content } => {
                self.advance();
                let inlines =
                    parse_inlines_with_warnings(&content, self.link_defs, &mut self.warnings);
                self.blocks.push(MdBlock::Heading {
                    level,
                    content: inlines,
                });
            }
            LineKind::ThematicBreak => {
                self.advance();
                self.blocks.push(MdBlock::ThematicBreak);
            }
            LineKind::FencedCodeOpen {
                fence_char,
                fence_len,
                language,
                indent,
            } => {
                self.advance();
                let (text, _closed) = self.consume_fenced_code(fence_char, fence_len, indent);
                self.blocks.push(MdBlock::CodeBlock { text, language });
            }
            LineKind::IndentedCode { text } => {
                let mut code_lines = vec![text];
                self.advance();
                while !self.at_end() {
                    let line = self.current_line();
                    if line.trim().is_empty() {
                        // Blank line inside indented code: include it but
                        // check if code continues after.
                        code_lines.push(String::new());
                        self.advance();
                    } else if let Some(stripped) = line
                        .strip_prefix('\t')
                        .or_else(|| line.strip_prefix("    "))
                    {
                        code_lines.push(stripped.to_string());
                        self.advance();
                    } else {
                        break;
                    }
                }
                // Trim trailing blank lines.
                while code_lines.last().is_some_and(|l| l.is_empty()) {
                    code_lines.pop();
                }
                self.blocks.push(MdBlock::CodeBlock {
                    text: code_lines.join("\n"),
                    language: None,
                });
            }
            LineKind::UnorderedListMarker { indent, content } => {
                self.parse_list(false, 1, indent, content, depth);
            }
            LineKind::OrderedListMarker {
                indent,
                start,
                content,
            } => {
                self.parse_list(true, start, indent, content, depth);
            }
            LineKind::BlockquotePrefix { rest } => {
                self.parse_blockquote(rest, depth);
            }
            LineKind::LinkRefDef { .. } => {
                // Already captured by pre_scan_link_defs; just skip the line.
                self.advance();
            }
            LineKind::PipeRow { raw } => {
                self.parse_possible_table(raw);
            }
            LineKind::Text { text } => {
                self.parse_paragraph(text);
            }
        }
    }

    fn classify_line(&self, line: &str) -> LineKind {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            return LineKind::Blank;
        }

        // ATX heading: # through ######
        if let Some(heading) = try_atx_heading(trimmed) {
            return heading;
        }

        // Thematic break: ---, ***, ___ (3+ of same char, optional spaces)
        if is_thematic_break(trimmed) {
            return LineKind::ThematicBreak;
        }

        // Fenced code block open/close.
        if let Some(fence) = try_fenced_code(line) {
            return fence;
        }

        // Indented code block (4 spaces or 1 tab, not inside list/quote).
        if !trimmed.starts_with('>') {
            if let Some(stripped) = line
                .strip_prefix('\t')
                .or_else(|| line.strip_prefix("    "))
            {
                // Don't treat list markers as indented code.
                if !is_list_marker(trimmed) {
                    return LineKind::IndentedCode {
                        text: stripped.to_string(),
                    };
                }
            }
        }

        // Block quote.
        if let Some(rest) = try_blockquote(trimmed) {
            return LineKind::BlockquotePrefix {
                rest: rest.to_string(),
            };
        }

        // List markers.
        if let Some(list) = try_list_marker(line) {
            return list;
        }

        // Link reference definition: [label]: url
        if let Some(def) = try_link_ref_def(trimmed) {
            return def;
        }

        // Pipe row (potential table): must start or end with `|` to avoid
        // false positives on prose like "a | b" or code containing pipes.
        if trimmed.starts_with('|') || trimmed.ends_with('|') {
            return LineKind::PipeRow {
                raw: trimmed.to_string(),
            };
        }

        LineKind::Text {
            text: line.to_string(),
        }
    }

    fn consume_fenced_code(
        &mut self,
        fence_char: char,
        fence_len: usize,
        indent: usize,
    ) -> (String, bool) {
        let mut code_lines = Vec::new();
        let mut closed = false;
        while !self.at_end() {
            let line = self.current_line();
            let trimmed = line.trim();
            // Check for closing fence: same char, >= fence_len, no other content.
            // len() counts bytes, but fence chars are always ASCII (` or ~) so
            // byte count == char count.
            if !trimmed.is_empty()
                && trimmed.len() >= fence_len
                && trimmed.chars().all(|c| c == fence_char)
            {
                self.advance();
                closed = true;
                break;
            }
            // Strip up to `indent` spaces from the start.
            let stripped = strip_leading_spaces(line, indent);
            code_lines.push(stripped.to_string());
            self.advance();
        }
        if !closed {
            self.warn(
                "UnclosedCodeFence",
                format!("unclosed fenced code block at line {}", self.pos + 1),
            );
        }
        (code_lines.join("\n"), closed)
    }

    fn parse_paragraph(&mut self, first_line: String) {
        let mut lines = vec![first_line];
        self.advance();

        // Collect continuation lines (lazy continuation).
        while !self.at_end() {
            let line = self.current_line();
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            // Don't absorb lines that start a new block.
            let kind = self.classify_line(line);
            match kind {
                LineKind::Text { text } => {
                    lines.push(text);
                    self.advance();
                }
                // A pipe row might be a table separator, check lookahead.
                LineKind::PipeRow { raw } => {
                    if table::is_separator(&raw) {
                        // The previous lines were a table header. Reparse.
                        self.reparse_as_table(lines, raw);
                        return;
                    }
                    lines.push(raw);
                    self.advance();
                }
                // A thematic break line (---) might be a setext underline.
                // This is safe because parse_paragraph is only entered when
                // the first line is Text, so standalone --- (no preceding
                // paragraph text) is handled as ThematicBreak in parse_block.
                LineKind::ThematicBreak => {
                    let raw = line.trim().to_string();
                    lines.push(raw);
                    self.advance();
                    // Setext underline terminates the paragraph, don't continue.
                    break;
                }
                _ => break,
            }
        }

        // Check for setext heading: last line is === or --- underline.
        if lines.len() >= 2 {
            let last = lines.last().expect("len >= 2");
            if let Some(level) = try_setext_underline(last) {
                // Everything except the underline is heading content.
                let heading_lines = &lines[..lines.len() - 1];
                let combined = heading_lines.join("\n");
                let inlines =
                    parse_inlines_with_warnings(&combined, self.link_defs, &mut self.warnings);
                self.blocks.push(MdBlock::Heading {
                    level,
                    content: inlines,
                });
                return;
            }
        }

        let combined = lines.join("\n");
        let inlines = parse_inlines_with_warnings(&combined, self.link_defs, &mut self.warnings);

        // Check if this is a standalone image block: a single image inline
        // possibly followed by trailing soft/hard breaks from newlines.
        let mut non_break = inlines
            .iter()
            .filter(|i| !matches!(i, MdInline::SoftBreak | MdInline::LineBreak));
        if let Some(first) = non_break.next() {
            if non_break.next().is_none() {
                if let MdInline::Image { alt, url } = first {
                    self.blocks.push(MdBlock::Image {
                        alt: alt.clone(),
                        url: url.clone(),
                    });
                    return;
                }
            }
        }

        self.blocks.push(MdBlock::Paragraph { content: inlines });
    }

    fn parse_possible_table(&mut self, first_row: String) {
        // Need to look ahead for a separator row.
        let saved_pos = self.pos;
        self.advance();
        if !self.at_end() {
            let next = self.current_line().trim().to_string();
            if table::is_separator(&next) {
                // It's a table. Parse header from first_row.
                let header_cells = table::parse_row(&first_row, self.link_defs);
                let col_count = table::count_columns(&next);
                self.advance(); // consume separator

                let mut rows = Vec::new();
                while !self.at_end() {
                    let line = self.current_line().trim().to_string();
                    if line.is_empty() || !line.contains('|') {
                        break;
                    }
                    let row = table::parse_row(&line, self.link_defs);
                    rows.push(row);
                    self.advance();
                }

                self.warn_table_cell_mismatch(&header_cells, &rows, col_count);
                self.blocks.push(MdBlock::Table {
                    header: header_cells,
                    rows,
                    col_count,
                });
                return;
            }
            // Not a table separator. The first row was just a text line with pipes.
            // Restore to the lookahead line so it gets parsed normally next.
            self.pos = saved_pos + 1;
            let inlines =
                parse_inlines_with_warnings(&first_row, self.link_defs, &mut self.warnings);
            self.blocks.push(MdBlock::Paragraph { content: inlines });
            return;
        }
        // End of input after a pipe row: treat as paragraph.
        let inlines = parse_inlines_with_warnings(&first_row, self.link_defs, &mut self.warnings);
        self.blocks.push(MdBlock::Paragraph { content: inlines });
    }

    fn reparse_as_table(&mut self, header_lines: Vec<String>, separator: String) {
        // The last header line is the actual header row.
        // Previous lines, if any, are a paragraph before the table.
        let header_row = if header_lines.len() > 1 {
            let para_lines = &header_lines[..header_lines.len() - 1];
            let combined = para_lines.join("\n");
            let inlines =
                parse_inlines_with_warnings(&combined, self.link_defs, &mut self.warnings);
            self.blocks.push(MdBlock::Paragraph { content: inlines });
            // len() > 1 guarantees last() is Some.
            header_lines.last().expect("checked len > 1").clone()
        } else {
            // header_lines always has at least one element (populated by caller).
            header_lines.last().expect("non-empty header_lines").clone()
        };

        let header_cells = table::parse_row(&header_row, self.link_defs);
        let col_count = table::count_columns(&separator);
        self.advance(); // consume separator line

        let mut rows = Vec::new();
        while !self.at_end() {
            let line = self.current_line().trim().to_string();
            if line.is_empty() || !line.contains('|') {
                break;
            }
            let row = table::parse_row(&line, self.link_defs);
            rows.push(row);
            self.advance();
        }

        self.warn_table_cell_mismatch(&header_cells, &rows, col_count);
        self.blocks.push(MdBlock::Table {
            header: header_cells,
            rows,
            col_count,
        });
    }

    fn warn_table_cell_mismatch(
        &mut self,
        header: &[Vec<MdInline>],
        rows: &[Vec<Vec<MdInline>>],
        col_count: usize,
    ) {
        if header.len() != col_count {
            self.warn(
                "TableCellMismatch",
                format!(
                    "table header has {} cells but separator defines {} columns",
                    header.len(),
                    col_count
                ),
            );
        }
        for (i, row) in rows.iter().enumerate() {
            if row.len() != col_count {
                self.warn(
                    "TableCellMismatch",
                    format!(
                        "table row {} has {} cells but expected {}",
                        i + 1,
                        row.len(),
                        col_count
                    ),
                );
            }
        }
    }

    fn parse_blockquote(&mut self, first_rest: String, depth: usize) {
        let mut inner_lines = vec![first_rest];
        self.advance();

        // Collect blockquote lines (including lazy continuation for paragraphs).
        while !self.at_end() {
            let line = self.current_line();
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(rest) = try_blockquote(trimmed) {
                inner_lines.push(rest.to_string());
                self.advance();
            } else {
                // Lazy continuation: only if current block is a paragraph.
                // For simplicity, allow lazy continuation for any text line.
                let kind = self.classify_line(line);
                match kind {
                    LineKind::Text { text } => {
                        inner_lines.push(text);
                        self.advance();
                    }
                    _ => break,
                }
            }
        }

        // Enforce depth limit for blockquotes.
        if depth + 1 > MAX_DEPTH {
            self.warn(
                "MaxDepthExceeded",
                format!("blockquote nesting depth exceeded at line {}", self.pos + 1),
            );
            let inner_text = inner_lines.join("\n");
            let inlines =
                parse_inlines_with_warnings(&inner_text, self.link_defs, &mut self.warnings);
            self.blocks.push(MdBlock::Blockquote {
                children: vec![MdBlock::Paragraph { content: inlines }],
            });
            return;
        }

        // Recursively parse the inner content. link_defs is shared by
        // reference (all defs were found in pre_scan_link_defs).
        let inner_text = inner_lines.join("\n");
        let mut inner_parser = BlockParser::from_normalized(&inner_text, self.link_defs);
        inner_parser.base_depth = depth + 1;
        inner_parser.parse();
        self.warnings.extend(inner_parser.warnings);

        self.blocks.push(MdBlock::Blockquote {
            children: inner_parser.blocks,
        });
    }

    fn parse_list(
        &mut self,
        ordered: bool,
        start: u64,
        _marker_indent: usize,
        first_content: String,
        depth: usize,
    ) {
        let mut items = Vec::new();

        // Advance past the first marker line.
        self.advance();

        // Parse first item.
        let first_blocks = self.parse_list_item_content(first_content, ordered, depth);
        items.push(MdListItem {
            content: first_blocks,
        });

        // Parse subsequent items.
        while !self.at_end() {
            let line = self.current_line();
            let trimmed = line.trim();
            if trimmed.is_empty() {
                // Blank line: might be loose list or end of list.
                // Check if next non-blank line is a list marker.
                let saved = self.pos;
                self.advance();
                while !self.at_end() && self.current_line().trim().is_empty() {
                    self.advance();
                }
                if self.at_end() {
                    break;
                }
                let next_kind = self.classify_line(self.current_line());
                let is_same_list_marker = match &next_kind {
                    LineKind::UnorderedListMarker { .. } if !ordered => true,
                    LineKind::OrderedListMarker { .. } if ordered => true,
                    _ => false,
                };
                if !is_same_list_marker {
                    self.pos = saved;
                    break;
                }
                // Continue the list after blank line.
                continue;
            }

            match self.classify_line(line) {
                LineKind::UnorderedListMarker { content, .. } if !ordered => {
                    self.advance();
                    let blocks = self.parse_list_item_content(content, ordered, depth);
                    items.push(MdListItem { content: blocks });
                }
                LineKind::OrderedListMarker { content, .. } if ordered => {
                    self.advance();
                    let blocks = self.parse_list_item_content(content, ordered, depth);
                    items.push(MdListItem { content: blocks });
                }
                _ => break,
            }
        }

        self.blocks.push(MdBlock::List {
            items,
            ordered,
            start,
        });
    }

    fn parse_list_item_content(
        &mut self,
        first_line: String,
        ordered: bool,
        depth: usize,
    ) -> Vec<MdBlock> {
        let mut content_lines = vec![first_line];

        // Collect continuation lines (indented or belonging to this item).
        while !self.at_end() {
            let line = self.current_line();
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            // Check if this is a new list marker at the same level.
            let kind = self.classify_line(line);
            match &kind {
                LineKind::UnorderedListMarker { .. } if !ordered => break,
                LineKind::OrderedListMarker { .. } if ordered => break,
                LineKind::AtxHeading { .. }
                | LineKind::ThematicBreak
                | LineKind::FencedCodeOpen { .. }
                | LineKind::BlockquotePrefix { .. } => break,
                _ => {
                    // Continuation line: strip up to 4 spaces of indent.
                    let stripped = line
                        .strip_prefix("    ")
                        .or_else(|| line.strip_prefix("   "))
                        .or_else(|| line.strip_prefix("  "))
                        .unwrap_or(line);
                    content_lines.push(stripped.to_string());
                    self.advance();
                }
            }
        }

        let inner_text = content_lines.join("\n");

        // Parse the content as markdown if it's simple (just a paragraph),
        // or recursively for nested structures.
        if depth + 1 > MAX_DEPTH {
            self.warn(
                "MaxDepthExceeded",
                format!("list nesting depth exceeded at line {}", self.pos + 1),
            );
            let inlines =
                parse_inlines_with_warnings(&inner_text, self.link_defs, &mut self.warnings);
            return vec![MdBlock::Paragraph { content: inlines }];
        }

        let mut inner_parser = BlockParser::from_normalized(&inner_text, self.link_defs);
        inner_parser.base_depth = depth + 1;
        inner_parser.parse();
        self.warnings.extend(inner_parser.warnings);

        if inner_parser.blocks.is_empty() {
            let inlines =
                parse_inlines_with_warnings(&inner_text, self.link_defs, &mut self.warnings);
            vec![MdBlock::Paragraph { content: inlines }]
        } else {
            inner_parser.blocks
        }
    }
}

// ---------------------------------------------------------------------------
// Line classification helpers
// ---------------------------------------------------------------------------

fn try_atx_heading(trimmed: &str) -> Option<LineKind> {
    if !trimmed.starts_with('#') {
        return None;
    }
    let hash_count = trimmed.bytes().take_while(|&b| b == b'#').count();
    if hash_count > 6 {
        return None;
    }
    let rest = &trimmed[hash_count..];
    if rest.is_empty() {
        return Some(LineKind::AtxHeading {
            level: hash_count as u8,
            content: String::new(),
        });
    }
    if !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None; // `#foo` is not a heading.
    }
    // Trim trailing `#` sequence (closing ATX).
    let content = rest.trim();
    let content = content.trim_end_matches('#').trim_end();
    Some(LineKind::AtxHeading {
        level: hash_count as u8,
        content: content.to_string(),
    })
}

fn is_thematic_break(trimmed: &str) -> bool {
    if trimmed.len() < 3 {
        return false;
    }
    // Check without allocating: find the first non-whitespace char, verify
    // at least 3 of that char, and all non-whitespace chars are the same.
    let mut first = None;
    let mut count = 0;
    for c in trimmed.chars() {
        if c.is_whitespace() {
            continue;
        }
        match first {
            None => {
                if c != '-' && c != '*' && c != '_' {
                    return false;
                }
                first = Some(c);
                count = 1;
            }
            Some(f) => {
                if c != f {
                    return false;
                }
                count += 1;
            }
        }
    }
    count >= 3
}

fn try_fenced_code(line: &str) -> Option<LineKind> {
    let indent = line.len() - line.trim_start().len();
    if indent > 3 {
        return None;
    }
    let trimmed = line.trim_start();
    let fence_char = trimmed.chars().next()?;
    if fence_char != '`' && fence_char != '~' {
        return None;
    }
    let fence_len = trimmed
        .bytes()
        .take_while(|&b| b == fence_char as u8)
        .count();
    if fence_len < 3 {
        return None;
    }
    let after_fence = trimmed[fence_len..].trim();
    // Closing fence: only fence chars (and optional spaces).
    if after_fence.is_empty() && line.trim().chars().all(|c| c == fence_char) {
        // Could be opening (no info) or closing. We treat it as opening
        // if no code block is active (handled by caller context).
        // For classification purposes, check if there's content after fence.
        return Some(LineKind::FencedCodeOpen {
            fence_char,
            fence_len,
            language: None,
            indent,
        });
    }
    // Opening fence with optional language tag.
    // Backtick fences cannot contain backticks in the info string.
    if fence_char == '`' && after_fence.contains('`') {
        return None;
    }
    let language = if after_fence.is_empty() {
        None
    } else {
        Some(
            after_fence
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string(),
        )
    };
    Some(LineKind::FencedCodeOpen {
        fence_char,
        fence_len,
        language,
        indent,
    })
}

fn is_list_marker(trimmed: &str) -> bool {
    // Delegate to strip_list_marker_prefix for `marker + space` patterns.
    if strip_list_marker_prefix(trimmed).is_some() {
        return true;
    }
    // Bare markers without trailing content (e.g. `-`, `*`, `+`, `1.`, `1)`).
    if trimmed == "-" || trimmed == "*" || trimmed == "+" {
        return true;
    }
    let digit_end = trimmed.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digit_end > 0 && digit_end <= 9 {
        let after = &trimmed[digit_end..];
        if after == "." || after == ")" {
            return true;
        }
    }
    false
}

fn try_list_marker(line: &str) -> Option<LineKind> {
    let indent = line.len() - line.trim_start().len();
    let trimmed = line.trim_start();

    // Unordered: -, *, +
    for marker in &['-', '*', '+'] {
        if trimmed.starts_with(*marker) {
            let rest = &trimmed[1..];
            if rest.is_empty() {
                return Some(LineKind::UnorderedListMarker {
                    indent,
                    content: String::new(),
                });
            }
            if rest.starts_with(' ') || rest.starts_with('\t') {
                // Make sure it's not a thematic break.
                if (*marker == '-' || *marker == '*') && is_thematic_break(trimmed) {
                    return None; // It's a thematic break, not a list.
                }
                return Some(LineKind::UnorderedListMarker {
                    indent,
                    content: rest.trim_start().to_string(),
                });
            }
        }
    }

    // Ordered: 1. or 1)
    let digit_end = trimmed.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digit_end > 0 && digit_end <= 9 {
        let after = &trimmed[digit_end..];
        if let Some(content) = after
            .strip_prefix(". ")
            .or_else(|| after.strip_prefix(") "))
        {
            let num: u64 = trimmed[..digit_end].parse().unwrap_or(1);
            return Some(LineKind::OrderedListMarker {
                indent,
                start: num,
                content: content.trim_start().to_string(),
            });
        }
        if after == "." || after == ")" {
            let num: u64 = trimmed[..digit_end].parse().unwrap_or(1);
            return Some(LineKind::OrderedListMarker {
                indent,
                start: num,
                content: String::new(),
            });
        }
    }

    None
}

fn try_blockquote(trimmed: &str) -> Option<&str> {
    if let Some(rest) = trimmed.strip_prefix("> ") {
        Some(rest)
    } else if trimmed == ">" {
        Some("")
    } else if let Some(rest) = trimmed.strip_prefix('>') {
        // `>text` without space is also valid.
        Some(rest)
    } else {
        None
    }
}

fn try_link_ref_def(trimmed: &str) -> Option<LineKind> {
    // [label]: url
    if !trimmed.starts_with('[') {
        return None;
    }
    let close = trimmed.find("]:")?;
    let label = &trimmed[1..close];
    if label.is_empty() {
        return None;
    }
    let url = trimmed[close + 2..].trim();
    if url.is_empty() {
        return None;
    }
    // Strip optional angle brackets.
    let url = url.trim_start_matches('<').trim_end_matches('>');
    Some(LineKind::LinkRefDef {
        label: label.to_string(),
        url: url.to_string(),
    })
}

/// Check if a line is a setext heading underline.
/// Returns Some(1) for `===` (H1) or Some(2) for `---` (H2).
fn try_setext_underline(line: &str) -> Option<u8> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let first = trimmed.as_bytes()[0];
    if first != b'=' && first != b'-' {
        return None;
    }
    // All characters must be the same (= or -), with optional whitespace.
    if !trimmed.bytes().all(|b| b == first || b == b' ') {
        return None;
    }
    // Must have at least one = or -.
    let marker_count = trimmed.bytes().filter(|&b| b == first).count();
    if marker_count == 0 {
        return None;
    }
    Some(if first == b'=' { 1 } else { 2 })
}

fn strip_leading_spaces(line: &str, count: usize) -> &str {
    let mut stripped = 0;
    let mut idx = 0;
    for (i, c) in line.char_indices() {
        if stripped >= count {
            break;
        }
        if c == ' ' {
            stripped += 1;
            idx = i + 1;
        } else if c == '\t' {
            stripped += 4; // tab = 4 spaces
            idx = i + 1;
        } else {
            break;
        }
    }
    if idx <= line.len() {
        &line[idx..]
    } else {
        line
    }
}

#[cfg(test)]
#[path = "block_tests.rs"]
mod tests;
