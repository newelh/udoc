//! Inline-level Markdown parser.
//!
//! Implements the CommonMark delimiter-run algorithm for emphasis,
//! plus code spans, links, images, autolinks, backslash escapes,
//! hard line breaks, and GFM strikethrough.

use std::collections::HashMap;

/// Maximum inline nesting depth to prevent stack overflow from pathological input.
const MAX_INLINE_DEPTH: usize = 128;

/// A parsed inline element.
#[derive(Debug, Clone)]
pub enum MdInline {
    Text {
        text: String,
        bold: bool,
        italic: bool,
        strikethrough: bool,
    },
    Code {
        text: String,
    },
    Link {
        url: String,
        content: Vec<MdInline>,
    },
    Image {
        alt: String,
        url: String,
    },
    SoftBreak,
    LineBreak,
}

/// Parse inline markdown content into a list of inline elements.
pub fn parse_inlines(input: &str, link_defs: &HashMap<String, String>) -> Vec<MdInline> {
    parse_inlines_inner(input, link_defs, 0, None)
}

/// Parse inline markdown content, collecting warnings into the provided vec.
pub fn parse_inlines_with_warnings(
    input: &str,
    link_defs: &HashMap<String, String>,
    warnings: &mut Vec<(String, String)>,
) -> Vec<MdInline> {
    parse_inlines_inner(input, link_defs, 0, Some(warnings))
}

fn parse_inlines_with_depth(
    input: &str,
    link_defs: &HashMap<String, String>,
    depth: usize,
    warnings: Option<&mut Vec<(String, String)>>,
) -> Vec<MdInline> {
    parse_inlines_inner(input, link_defs, depth, warnings)
}

fn parse_inlines_inner(
    input: &str,
    link_defs: &HashMap<String, String>,
    depth: usize,
    warnings: Option<&mut Vec<(String, String)>>,
) -> Vec<MdInline> {
    if depth >= MAX_INLINE_DEPTH {
        // Bail out: emit remaining input as plain text to avoid stack overflow.
        if input.is_empty() {
            return Vec::new();
        }
        return vec![MdInline::Text {
            text: input.to_string(),
            bold: false,
            italic: false,
            strikethrough: false,
        }];
    }
    let mut parser = InlineParser::new(input, link_defs, depth, warnings);
    parser.parse();
    parser.result
}

/// An entry on the delimiter stack, tracking a run of `*` or `_` chars
/// that might become emphasis openers or closers.
struct DelimiterEntry {
    /// Index into `InlineParser::result` where this delimiter's text node lives.
    result_idx: usize,
    /// The delimiter character (`*` or `_`).
    marker: u8,
    /// Original run length (for multiple-of-3 rule).
    orig_count: usize,
    /// Remaining unmatched delimiter characters.
    remaining: usize,
    /// Whether this run can open emphasis (left-flanking per CommonMark).
    can_open: bool,
    /// Whether this run can close emphasis (right-flanking per CommonMark).
    can_close: bool,
    /// Whether this delimiter is still active for matching.
    active: bool,
}

struct InlineParser<'a, 'w> {
    input: &'a str,
    bytes: &'a [u8],
    pos: usize,
    link_defs: &'a HashMap<String, String>,
    result: Vec<MdInline>,
    depth: usize,
    delimiters: Vec<DelimiterEntry>,
    warnings: Option<&'w mut Vec<(String, String)>>,
}

impl<'a, 'w> InlineParser<'a, 'w> {
    fn new(
        input: &'a str,
        link_defs: &'a HashMap<String, String>,
        depth: usize,
        warnings: Option<&'w mut Vec<(String, String)>>,
    ) -> Self {
        Self {
            input,
            bytes: input.as_bytes(),
            pos: 0,
            link_defs,
            result: Vec::new(),
            depth,
            delimiters: Vec::new(),
            warnings,
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn parse(&mut self) {
        let mut text_start = self.pos;

        while !self.at_end() {
            let b = self.bytes[self.pos];
            match b {
                b'\\'
                    // Backslash escape.
                    if self.pos + 1 < self.bytes.len() => {
                        let next = self.bytes[self.pos + 1];
                        if is_escapable(next) {
                            // Flush text before the escape.
                            self.flush_text(text_start, self.pos);
                            self.pos += 1; // skip backslash
                            text_start = self.pos;
                            self.pos += 1; // include the escaped char
                        } else if next == b'\n' {
                            // Hard line break.
                            self.flush_text(text_start, self.pos);
                            self.pos += 2;
                            text_start = self.pos;
                            self.result.push(MdInline::LineBreak);
                        } else {
                            self.pos += 1;
                        }
                    }
                b'`' => {
                    self.flush_text(text_start, self.pos);
                    if let Some(code) = self.try_code_span() {
                        self.result.push(MdInline::Code { text: code });
                    } else {
                        // Not a valid code span; include the backtick as text.
                        text_start = self.pos;
                        self.pos += 1;
                        continue;
                    }
                    text_start = self.pos;
                }
                b'*' | b'_' => {
                    self.flush_text(text_start, self.pos);
                    self.push_delimiter();
                    text_start = self.pos;
                }
                b'~' if self.pos + 1 < self.bytes.len() && self.bytes[self.pos + 1] == b'~' => {
                    self.flush_text(text_start, self.pos);
                    self.parse_strikethrough();
                    text_start = self.pos;
                }
                b'!' if self.pos + 1 < self.bytes.len() && self.bytes[self.pos + 1] == b'[' => {
                    self.flush_text(text_start, self.pos);
                    if let Some(image) = self.try_image() {
                        self.result.push(image);
                    } else {
                        text_start = self.pos;
                        self.pos += 1;
                        continue;
                    }
                    text_start = self.pos;
                }
                b'[' => {
                    self.flush_text(text_start, self.pos);
                    if let Some(link) = self.try_link() {
                        self.result.push(link);
                    } else {
                        text_start = self.pos;
                        self.pos += 1;
                        continue;
                    }
                    text_start = self.pos;
                }
                b'<' => {
                    self.flush_text(text_start, self.pos);
                    if let Some(autolink) = self.try_autolink() {
                        self.result.push(autolink);
                    } else {
                        // Might be HTML; skip it as text.
                        text_start = self.pos;
                        self.pos += 1;
                        continue;
                    }
                    text_start = self.pos;
                }
                b'&' => {
                    if let Some(decoded) = try_entity(&self.bytes[self.pos..]) {
                        self.flush_text(text_start, self.pos);
                        // Advance past the entity (& ... ).
                        let entity_end = memchr_byte(b';', &self.bytes[self.pos..])
                            .map(|i| self.pos + i + 1)
                            .unwrap_or(self.pos + 1);
                        self.pos = entity_end;
                        self.result.push(MdInline::Text {
                            text: decoded,
                            bold: false,
                            italic: false,
                            strikethrough: false,
                        });
                        text_start = self.pos;
                    } else {
                        self.pos += 1;
                    }
                }
                b'\n' => {
                    // Check for hard line break (two trailing spaces before newline).
                    let end = self.pos;
                    let has_trailing_spaces =
                        end >= 2 && self.bytes[end - 1] == b' ' && self.bytes[end - 2] == b' ';
                    if has_trailing_spaces {
                        // Trim trailing spaces from text.
                        let trimmed_end = end - count_trailing_spaces(self.bytes, end);
                        self.flush_text(text_start, trimmed_end);
                        self.result.push(MdInline::LineBreak);
                    } else {
                        self.flush_text(text_start, self.pos);
                        self.result.push(MdInline::SoftBreak);
                    }
                    self.pos += 1;
                    text_start = self.pos;
                }
                _ => {
                    self.pos += 1;
                }
            }
        }

        self.flush_text(text_start, self.pos);
        self.process_emphasis();
    }

    fn flush_text(&mut self, start: usize, end: usize) {
        if start < end && end <= self.input.len() {
            let text = &self.input[start..end];
            if !text.is_empty() {
                self.result.push(MdInline::Text {
                    text: text.to_string(),
                    bold: false,
                    italic: false,
                    strikethrough: false,
                });
            }
        }
    }

    fn try_code_span(&mut self) -> Option<String> {
        // Count opening backticks.
        let start = self.pos;
        let open_count = self.bytes[start..]
            .iter()
            .take_while(|&&b| b == b'`')
            .count();
        let content_start = start + open_count;

        // Find matching closing backticks.
        let mut scan = content_start;
        loop {
            let close_pos = memchr_byte(b'`', &self.bytes[scan..])?;
            let close_start = scan + close_pos;
            let close_count = self.bytes[close_start..]
                .iter()
                .take_while(|&&b| b == b'`')
                .count();
            if close_count == open_count {
                let content = &self.input[content_start..close_start];
                // Normalize: strip single leading/trailing space if content
                // has at least one non-space character and both ends have spaces.
                let normalized = normalize_code_content(content);
                self.pos = close_start + close_count;
                return Some(normalized);
            }
            scan = close_start + close_count;
            if scan >= self.bytes.len() {
                return None;
            }
        }
    }

    /// Push a delimiter run (`*` or `_`) onto the stack for later processing.
    fn push_delimiter(&mut self) {
        let marker = self.bytes[self.pos];
        let run_len = self.bytes[self.pos..]
            .iter()
            .take_while(|&&b| b == marker)
            .count();

        let (can_open, can_close) = compute_flanking(self.bytes, self.pos, marker, run_len);

        let result_idx = self.result.len();
        let delim_str = &self.input[self.pos..self.pos + run_len];
        self.result.push(MdInline::Text {
            text: delim_str.to_string(),
            bold: false,
            italic: false,
            strikethrough: false,
        });

        self.delimiters.push(DelimiterEntry {
            result_idx,
            marker,
            orig_count: run_len,
            remaining: run_len,
            can_open,
            can_close,
            active: true,
        });

        self.pos += run_len;
    }

    /// Match delimiter openers with closers and apply emphasis flags.
    /// Implements the CommonMark delimiter-run algorithm (spec section 6.4).
    fn process_emphasis(&mut self) {
        let mut closer_idx = 0;
        while closer_idx < self.delimiters.len() {
            if !self.delimiters[closer_idx].active
                || !self.delimiters[closer_idx].can_close
                || self.delimiters[closer_idx].remaining == 0
            {
                closer_idx += 1;
                continue;
            }

            let closer_marker = self.delimiters[closer_idx].marker;

            // Scan backward for a matching opener.
            let mut opener_idx = None;
            for j in (0..closer_idx).rev() {
                let opener = &self.delimiters[j];
                if !opener.active || opener.remaining == 0 || opener.marker != closer_marker {
                    continue;
                }
                if !opener.can_open {
                    continue;
                }
                // Multiple-of-3 rule (CommonMark spec): when either delimiter
                // can function as both opener and closer, skip the match if the
                // sum of original run lengths is a multiple of 3 (unless both
                // individual lengths are multiples of 3).
                let closer = &self.delimiters[closer_idx];
                if (closer.can_open || opener.can_close)
                    && (opener.orig_count + closer.orig_count).is_multiple_of(3)
                    && !opener.orig_count.is_multiple_of(3)
                    && !closer.orig_count.is_multiple_of(3)
                {
                    continue;
                }
                opener_idx = Some(j);
                break;
            }

            if let Some(oi) = opener_idx {
                // Determine emphasis strength: strong if both have >= 2 remaining.
                let use_count = if self.delimiters[oi].remaining >= 2
                    && self.delimiters[closer_idx].remaining >= 2
                {
                    2
                } else {
                    1
                };
                let bold = use_count == 2;
                let italic = use_count == 1;

                let opener_result = self.delimiters[oi].result_idx;
                let closer_result = self.delimiters[closer_idx].result_idx;

                // Apply emphasis to all nodes between opener and closer.
                for k in (opener_result + 1)..closer_result {
                    apply_emphasis_in_place(&mut self.result[k], bold, italic);
                }

                // Consume delimiter characters.
                self.delimiters[oi].remaining -= use_count;
                self.delimiters[closer_idx].remaining -= use_count;

                // Deactivate any delimiters between opener and closer.
                for j in (oi + 1)..closer_idx {
                    self.delimiters[j].active = false;
                }

                // If closer still has remaining chars, try matching again.
                if self.delimiters[closer_idx].remaining > 0 {
                    continue;
                }
            } else {
                // No matching opener found. Per CommonMark spec: only deactivate
                // if this closer is NOT also a potential opener (it may still open
                // for a later closer).
                if !self.delimiters[closer_idx].can_open {
                    self.delimiters[closer_idx].active = false;
                }
            }
            closer_idx += 1;
        }

        // Update delimiter text nodes: trim consumed characters.
        for delim in &self.delimiters {
            let consumed = delim.orig_count - delim.remaining;
            if consumed > 0 {
                if let MdInline::Text { text, .. } = &mut self.result[delim.result_idx] {
                    if delim.remaining == 0 {
                        text.clear();
                    } else {
                        // Keep only the unconsumed delimiter chars.
                        *text = text.chars().take(delim.remaining).collect();
                    }
                }
            }
        }

        // Remove empty text nodes left by fully consumed delimiters.
        self.result.retain(|inline| {
            !matches!(
                inline,
                MdInline::Text {
                    text,
                    bold: false,
                    italic: false,
                    strikethrough: false,
                } if text.is_empty()
            )
        });
    }

    fn parse_strikethrough(&mut self) {
        // ~~text~~
        self.pos += 2; // skip opening ~~

        // Find closing ~~.
        if let Some(close) = find_pattern(&self.bytes[self.pos..], b"~~") {
            let content_end = self.pos + close;
            let content = &self.input[self.pos..content_end];
            let inner_inlines =
                parse_inlines_with_depth(content, self.link_defs, self.depth + 1, None);

            let wrapped = apply_strikethrough(inner_inlines);
            self.result.extend(wrapped);
            self.pos = content_end + 2;
        } else {
            // No closing ~~ found.
            self.result.push(MdInline::Text {
                text: "~~".to_string(),
                bold: false,
                italic: false,
                strikethrough: false,
            });
        }
    }

    fn try_link(&mut self) -> Option<MdInline> {
        // [text](url) or [text][ref]
        let start = self.pos;
        self.pos += 1; // skip [

        let text_end = match self.find_matching_bracket() {
            Some(end) => end,
            None => {
                self.pos = start;
                return None;
            }
        };
        let text = self.input[start + 1..text_end].to_string();
        self.pos = text_end + 1; // skip ]

        if self.pos < self.bytes.len() && self.bytes[self.pos] == b'(' {
            // Inline link: [text](url)
            self.pos += 1;
            let url_end = match memchr_byte(b')', &self.bytes[self.pos..]) {
                Some(i) => self.pos + i,
                None => {
                    self.pos = start;
                    return None;
                }
            };
            let url = self.input[self.pos..url_end].trim().to_string();
            let url = extract_link_url(&url);
            self.pos = url_end + 1;
            let content = parse_inlines_with_depth(&text, self.link_defs, self.depth + 1, None);
            return Some(MdInline::Link { url, content });
        }

        if self.pos < self.bytes.len() && self.bytes[self.pos] == b'[' {
            // Reference link: [text][ref]
            self.pos += 1;
            let ref_end = match memchr_byte(b']', &self.bytes[self.pos..]) {
                Some(i) => self.pos + i,
                None => {
                    self.pos = start;
                    return None;
                }
            };
            let ref_label = self.input[self.pos..ref_end].to_string();
            self.pos = ref_end + 1;
            let label = if ref_label.is_empty() {
                text.to_lowercase()
            } else {
                ref_label.to_lowercase()
            };
            if let Some(url) = self.link_defs.get(&label) {
                let content = parse_inlines_with_depth(&text, self.link_defs, self.depth + 1, None);
                return Some(MdInline::Link {
                    url: url.clone(),
                    content,
                });
            }
            if let Some(ref mut w) = self.warnings {
                w.push((
                    "UnresolvedReferenceLink".to_string(),
                    format!("unresolved reference link [{}][{}]", text, label),
                ));
            }
            self.pos = start;
            return None;
        }

        // Collapsed/shortcut reference: [text]
        let label = text.to_lowercase();
        if let Some(url) = self.link_defs.get(&label) {
            let content = parse_inlines_with_depth(&text, self.link_defs, self.depth + 1, None);
            return Some(MdInline::Link {
                url: url.clone(),
                content,
            });
        }

        // Not a link; reset.
        self.pos = start;
        None
    }

    fn try_image(&mut self) -> Option<MdInline> {
        // ![alt](url)
        let start = self.pos;
        self.pos += 2; // skip ![

        let alt_end = match self.find_matching_bracket() {
            Some(end) => end,
            None => {
                self.pos = start;
                return None;
            }
        };
        let alt = self.input[start + 2..alt_end].to_string();
        self.pos = alt_end + 1;

        if self.pos < self.bytes.len() && self.bytes[self.pos] == b'(' {
            self.pos += 1;
            let url_end = match memchr_byte(b')', &self.bytes[self.pos..]) {
                Some(i) => self.pos + i,
                None => {
                    self.pos = start;
                    return None;
                }
            };
            let url = self.input[self.pos..url_end].trim().to_string();
            let url = extract_link_url(&url);
            self.pos = url_end + 1;
            return Some(MdInline::Image { alt, url });
        }

        // Reference image: ![alt][ref]
        if self.pos < self.bytes.len() && self.bytes[self.pos] == b'[' {
            self.pos += 1;
            let ref_end = match memchr_byte(b']', &self.bytes[self.pos..]) {
                Some(i) => self.pos + i,
                None => {
                    self.pos = start;
                    return None;
                }
            };
            let ref_label = self.input[self.pos..ref_end].to_string();
            self.pos = ref_end + 1;
            let label = if ref_label.is_empty() {
                alt.to_lowercase()
            } else {
                ref_label.to_lowercase()
            };
            if let Some(url) = self.link_defs.get(&label) {
                return Some(MdInline::Image {
                    alt,
                    url: url.clone(),
                });
            }
        }

        self.pos = start;
        None
    }

    fn try_autolink(&mut self) -> Option<MdInline> {
        // <url> where url contains ://
        let start = self.pos;
        self.pos += 1; // skip <
        let close = memchr_byte(b'>', &self.bytes[self.pos..])?;
        let content = &self.input[self.pos..self.pos + close];

        // Must look like a URL (contains ://) or email (contains @).
        if content.contains("://") || content.contains('@') {
            self.pos += close + 1;
            let url = if content.contains('@') && !content.contains("://") {
                format!("mailto:{content}")
            } else {
                content.to_string()
            };
            let display_text = content.to_string();
            return Some(MdInline::Link {
                url,
                content: vec![MdInline::Text {
                    text: display_text,
                    bold: false,
                    italic: false,
                    strikethrough: false,
                }],
            });
        }

        // Not an autolink. Check for inline HTML tag to skip.
        self.pos = start;
        None
    }

    fn find_matching_bracket(&self) -> Option<usize> {
        let mut depth = 1;
        let mut scan = self.pos;
        while scan < self.bytes.len() {
            match self.bytes[scan] {
                b'\\' if scan + 1 < self.bytes.len() => scan += 2,
                b'[' => {
                    depth += 1;
                    scan += 1;
                }
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(scan);
                    }
                    scan += 1;
                }
                b'`' => {
                    // Skip code spans inside brackets.
                    let tick_count = self.bytes[scan..]
                        .iter()
                        .take_while(|&&b| b == b'`')
                        .count();
                    if let Some(close) =
                        find_matching_backticks(&self.bytes[scan + tick_count..], tick_count)
                    {
                        scan = scan + tick_count + close + tick_count;
                    } else {
                        scan += tick_count;
                    }
                }
                _ => scan += 1,
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn is_escapable(b: u8) -> bool {
    matches!(
        b,
        b'\\'
            | b'`'
            | b'*'
            | b'_'
            | b'{'
            | b'}'
            | b'['
            | b']'
            | b'('
            | b')'
            | b'#'
            | b'+'
            | b'-'
            | b'.'
            | b'!'
            | b'|'
            | b'~'
            | b'<'
            | b'>'
            | b'&'
    )
}

/// ASCII punctuation characters per CommonMark spec (U+0021-002F, U+003A-0040,
/// U+005B-0060, U+007B-007E).
fn is_ascii_punctuation(b: u8) -> bool {
    matches!(b, 0x21..=0x2F | 0x3A..=0x40 | 0x5B..=0x60 | 0x7B..=0x7E)
}

fn memchr_byte(needle: u8, haystack: &[u8]) -> Option<usize> {
    haystack.iter().position(|&b| b == needle)
}

fn find_pattern(haystack: &[u8], pattern: &[u8]) -> Option<usize> {
    haystack.windows(pattern.len()).position(|w| w == pattern)
}

fn find_matching_backticks(haystack: &[u8], count: usize) -> Option<usize> {
    let mut pos = 0;
    while pos < haystack.len() {
        if haystack[pos] == b'`' {
            let run = haystack[pos..].iter().take_while(|&&b| b == b'`').count();
            if run == count {
                return Some(pos);
            }
            pos += run;
        } else {
            pos += 1;
        }
    }
    None
}

fn normalize_code_content(content: &str) -> String {
    // Replace newlines with spaces.
    let s = content.replace('\n', " ");
    // If both first and last char are spaces and content is not all spaces,
    // strip one space from each end.
    if s.len() >= 2
        && s.starts_with(' ')
        && s.ends_with(' ')
        && s.trim().len() < s.len()
        && !s.chars().all(|c| c == ' ')
    {
        s[1..s.len() - 1].to_string()
    } else {
        s
    }
}

fn extract_link_url(raw: &str) -> String {
    let raw = raw.trim();
    // Handle optional title in quotes after URL.
    let url = if let Some(space_pos) = raw.find([' ', '\t']) {
        &raw[..space_pos]
    } else {
        raw
    };
    // Strip angle brackets.
    url.trim_start_matches('<')
        .trim_end_matches('>')
        .to_string()
}

/// Decode the character immediately before `pos` in a UTF-8 byte slice.
/// Walks backwards past continuation bytes (0x80..0xBF) to find the char start.
fn char_before(bytes: &[u8], pos: usize) -> Option<char> {
    if pos == 0 {
        return None;
    }
    let mut i = pos - 1;
    while i > 0 && bytes[i] & 0xC0 == 0x80 {
        i -= 1;
    }
    std::str::from_utf8(&bytes[i..pos])
        .ok()
        .and_then(|s| s.chars().next())
}

/// Decode the character at `pos` in a UTF-8 byte slice.
fn char_at(bytes: &[u8], pos: usize) -> Option<char> {
    if pos >= bytes.len() {
        return None;
    }
    let len = match bytes[pos] {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xFF => 4,
        _ => return None,
    };
    let end = (pos + len).min(bytes.len());
    std::str::from_utf8(&bytes[pos..end])
        .ok()
        .and_then(|s| s.chars().next())
}

/// Check if a char is ASCII punctuation (same ranges as the byte version).
fn is_ascii_punctuation_char(c: char) -> bool {
    c.is_ascii() && is_ascii_punctuation(c as u8)
}

/// Compute left-flanking (can_open) and right-flanking (can_close) status
/// for a delimiter run per CommonMark spec.
fn compute_flanking(bytes: &[u8], pos: usize, marker: u8, run_len: usize) -> (bool, bool) {
    let before = char_before(bytes, pos);
    let after = char_at(bytes, pos + run_len);

    // Per CommonMark: beginning/end of line count as whitespace.
    let followed_by_ws = after.is_none_or(|c| c.is_ascii_whitespace());
    let followed_by_punct = after.is_some_and(is_ascii_punctuation_char);
    let preceded_by_ws = before.is_none_or(|c| c.is_ascii_whitespace());
    let preceded_by_punct = before.is_some_and(is_ascii_punctuation_char);

    // Left-flanking: not followed by whitespace AND
    // (not followed by punctuation OR preceded by whitespace or punctuation).
    let left_flanking =
        !followed_by_ws && (!followed_by_punct || preceded_by_ws || preceded_by_punct);

    // Right-flanking: not preceded by whitespace AND
    // (not preceded by punctuation OR followed by whitespace or punctuation).
    let right_flanking =
        !preceded_by_ws && (!preceded_by_punct || followed_by_ws || followed_by_punct);

    // For underscore delimiters, additional restrictions apply:
    // can_open requires left-flanking AND (not right-flanking OR preceded by punctuation)
    // can_close requires right-flanking AND (not left-flanking OR followed by punctuation)
    if marker == b'_' {
        (
            left_flanking && (!right_flanking || preceded_by_punct),
            right_flanking && (!left_flanking || followed_by_punct),
        )
    } else {
        (left_flanking, right_flanking)
    }
}

/// Apply emphasis flags to a single inline node in place.
fn apply_emphasis_in_place(inline: &mut MdInline, bold: bool, italic: bool) {
    match inline {
        MdInline::Text {
            bold: b, italic: i, ..
        } => {
            *b = *b || bold;
            *i = *i || italic;
        }
        MdInline::Link { content, .. } => {
            for inner in content {
                apply_emphasis_in_place(inner, bold, italic);
            }
        }
        _ => {}
    }
}

/// Try to decode an HTML entity reference starting at `&`.
/// Returns the decoded string if valid, None otherwise.
fn try_entity(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() || bytes[0] != b'&' {
        return None;
    }
    // Find the closing semicolon (entities are short, cap scan at 32 bytes).
    let limit = bytes.len().min(32);
    let semi_pos = bytes[1..limit].iter().position(|&b| b == b';')?;
    let entity_body = &bytes[1..semi_pos + 1];
    if entity_body.is_empty() {
        return None;
    }

    // Numeric character reference: &#123; or &#x1A;
    if entity_body[0] == b'#' {
        let num_part = &entity_body[1..];
        if num_part.is_empty() {
            return None;
        }
        let codepoint = if num_part[0] == b'x' || num_part[0] == b'X' {
            // Hex: &#x1F600;
            let hex = std::str::from_utf8(&num_part[1..]).ok()?;
            u32::from_str_radix(hex, 16).ok()?
        } else {
            // Decimal: &#123;
            let dec = std::str::from_utf8(num_part).ok()?;
            dec.parse::<u32>().ok()?
        };
        // U+0000 is replaced with U+FFFD per spec.
        let ch = if codepoint == 0 {
            '\u{FFFD}'
        } else {
            char::from_u32(codepoint)?
        };
        return Some(ch.to_string());
    }

    // Named entity reference.
    let name = std::str::from_utf8(entity_body).ok()?;
    lookup_named_entity(name).map(|s| s.to_string())
}

/// Lookup a named HTML entity. Covers the CommonMark-required subset plus
/// the most commonly used HTML entities.
fn lookup_named_entity(name: &str) -> Option<&'static str> {
    // CommonMark spec references the full HTML5 entity list, but for a text
    // extraction tool we cover the most common ones. This is ~80 entities
    // which handles real-world markdown well.
    match name {
        // Core XML entities
        "amp" => Some("&"),
        "lt" => Some("<"),
        "gt" => Some(">"),
        "quot" => Some("\""),
        "apos" => Some("'"),
        // Whitespace / special
        "nbsp" => Some("\u{00A0}"),
        "ensp" => Some("\u{2002}"),
        "emsp" => Some("\u{2003}"),
        "thinsp" => Some("\u{2009}"),
        "zwj" => Some("\u{200D}"),
        "zwnj" => Some("\u{200C}"),
        "lrm" => Some("\u{200E}"),
        "rlm" => Some("\u{200F}"),
        // Punctuation
        "ndash" => Some("\u{2013}"),
        "mdash" => Some("\u{2014}"),
        "lsquo" => Some("\u{2018}"),
        "rsquo" => Some("\u{2019}"),
        "sbquo" => Some("\u{201A}"),
        "ldquo" => Some("\u{201C}"),
        "rdquo" => Some("\u{201D}"),
        "bdquo" => Some("\u{201E}"),
        "laquo" => Some("\u{00AB}"),
        "raquo" => Some("\u{00BB}"),
        "lsaquo" => Some("\u{2039}"),
        "rsaquo" => Some("\u{203A}"),
        "hellip" => Some("\u{2026}"),
        "bull" => Some("\u{2022}"),
        "middot" => Some("\u{00B7}"),
        // Dashes / hyphens
        "minus" => Some("\u{2212}"),
        "hyphen" => Some("\u{2010}"),
        // Math / symbols
        "times" => Some("\u{00D7}"),
        "divide" => Some("\u{00F7}"),
        "plusmn" => Some("\u{00B1}"),
        "le" => Some("\u{2264}"),
        "ge" => Some("\u{2265}"),
        "ne" => Some("\u{2260}"),
        "asymp" => Some("\u{2248}"),
        "infin" => Some("\u{221E}"),
        "sum" => Some("\u{2211}"),
        "prod" => Some("\u{220F}"),
        "radic" => Some("\u{221A}"),
        "part" => Some("\u{2202}"),
        "nabla" => Some("\u{2207}"),
        "int" => Some("\u{222B}"),
        "prime" => Some("\u{2032}"),
        "Prime" => Some("\u{2033}"),
        "deg" => Some("\u{00B0}"),
        "micro" => Some("\u{00B5}"),
        "permil" => Some("\u{2030}"),
        // Currency
        "cent" => Some("\u{00A2}"),
        "pound" => Some("\u{00A3}"),
        "yen" => Some("\u{00A5}"),
        "euro" => Some("\u{20AC}"),
        "curren" => Some("\u{00A4}"),
        // Arrows
        "larr" => Some("\u{2190}"),
        "uarr" => Some("\u{2191}"),
        "rarr" => Some("\u{2192}"),
        "darr" => Some("\u{2193}"),
        "harr" => Some("\u{2194}"),
        "lArr" => Some("\u{21D0}"),
        "rArr" => Some("\u{21D2}"),
        "hArr" => Some("\u{21D4}"),
        // Typography
        "copy" => Some("\u{00A9}"),
        "reg" => Some("\u{00AE}"),
        "trade" => Some("\u{2122}"),
        "sect" => Some("\u{00A7}"),
        "para" => Some("\u{00B6}"),
        "dagger" => Some("\u{2020}"),
        "Dagger" => Some("\u{2021}"),
        // Latin accented (most common)
        "Agrave" => Some("\u{00C0}"),
        "Aacute" => Some("\u{00C1}"),
        "Acirc" => Some("\u{00C2}"),
        "Atilde" => Some("\u{00C3}"),
        "Auml" => Some("\u{00C4}"),
        "Aring" => Some("\u{00C5}"),
        "AElig" => Some("\u{00C6}"),
        "Ccedil" => Some("\u{00C7}"),
        "Egrave" => Some("\u{00C8}"),
        "Eacute" => Some("\u{00C9}"),
        "Euml" => Some("\u{00CB}"),
        "Ntilde" => Some("\u{00D1}"),
        "Ouml" => Some("\u{00D6}"),
        "Uuml" => Some("\u{00DC}"),
        "szlig" => Some("\u{00DF}"),
        "agrave" => Some("\u{00E0}"),
        "aacute" => Some("\u{00E1}"),
        "acirc" => Some("\u{00E2}"),
        "atilde" => Some("\u{00E3}"),
        "auml" => Some("\u{00E4}"),
        "aring" => Some("\u{00E5}"),
        "aelig" => Some("\u{00E6}"),
        "ccedil" => Some("\u{00E7}"),
        "egrave" => Some("\u{00E8}"),
        "eacute" => Some("\u{00E9}"),
        "euml" => Some("\u{00EB}"),
        "ntilde" => Some("\u{00F1}"),
        "ouml" => Some("\u{00F6}"),
        "uuml" => Some("\u{00FC}"),
        // Greek (commonly used in technical docs)
        "Alpha" => Some("\u{0391}"),
        "Beta" => Some("\u{0392}"),
        "Gamma" => Some("\u{0393}"),
        "Delta" => Some("\u{0394}"),
        "Epsilon" => Some("\u{0395}"),
        "Theta" => Some("\u{0398}"),
        "Lambda" => Some("\u{039B}"),
        "Pi" => Some("\u{03A0}"),
        "Sigma" => Some("\u{03A3}"),
        "Omega" => Some("\u{03A9}"),
        "alpha" => Some("\u{03B1}"),
        "beta" => Some("\u{03B2}"),
        "gamma" => Some("\u{03B3}"),
        "delta" => Some("\u{03B4}"),
        "epsilon" => Some("\u{03B5}"),
        "theta" => Some("\u{03B8}"),
        "lambda" => Some("\u{03BB}"),
        "mu" => Some("\u{03BC}"),
        "pi" => Some("\u{03C0}"),
        "sigma" => Some("\u{03C3}"),
        "tau" => Some("\u{03C4}"),
        "phi" => Some("\u{03C6}"),
        "omega" => Some("\u{03C9}"),
        _ => None,
    }
}

fn apply_strikethrough(inlines: Vec<MdInline>) -> Vec<MdInline> {
    inlines
        .into_iter()
        .map(|inline| match inline {
            MdInline::Text {
                text, bold, italic, ..
            } => MdInline::Text {
                text,
                bold,
                italic,
                strikethrough: true,
            },
            MdInline::Code { text } => MdInline::Code { text },
            MdInline::Link { url, content } => MdInline::Link {
                url,
                content: apply_strikethrough(content),
            },
            other => other,
        })
        .collect()
}

fn count_trailing_spaces(bytes: &[u8], end: usize) -> usize {
    let mut count = 0;
    let mut pos = end;
    while pos > 0 && bytes[pos - 1] == b' ' {
        count += 1;
        pos -= 1;
    }
    count
}

#[cfg(test)]
#[path = "inline_tests.rs"]
mod tests;
