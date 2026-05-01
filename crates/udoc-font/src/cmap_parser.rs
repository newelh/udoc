//! CMap parser for composite (CID) font character code decoding.
//!
//! Parses CMap streams (both embedded and predefined) to extract:
//! - Codespace ranges: which byte sequences form valid character codes
//! - CID mappings: character code -> CID (character identifier)
//! - Unicode mappings: character code -> Unicode string (bfchar/bfrange)
//! - Writing mode: horizontal vs vertical
//! - /UseCMap inheritance from a base CMap
//!
//! CMap format reference: Adobe Technical Note #5014, "Adobe CMap and
//! CIDFont Files Specification", and PDF Reference Chapter 5.
//!
//! Design decision : purpose-built tokenizer, not a full PS interpreter.

use ahash::AHashMap;

use super::cmap::lookup_predefined_cmap;
use super::tounicode::ToUnicodeCMap;

// -- Security limits --

/// Maximum total CID + Unicode entries before truncation.
const MAX_CMAP_ENTRIES: usize = 100_000;

/// Maximum codespace ranges.
const MAX_CODESPACE_RANGES: usize = 100;

/// Maximum /UseCMap chaining depth.
const MAX_USECMAP_DEPTH: usize = 10;

/// A codespace range defining valid byte sequences for character codes.
///
/// Character codes must fall within at least one codespace range.
/// Variable-length CMaps have ranges of different byte lengths.
#[derive(Debug, Clone)]
pub struct CodespaceRange {
    /// Inclusive low byte sequence of the range.
    pub low: Vec<u8>,
    /// Inclusive high byte sequence of the range (same length as `low`).
    pub high: Vec<u8>,
}

/// A code range -> CID range mapping from a cidrange section.
#[derive(Debug, Clone)]
pub struct CidRangeMapping {
    /// Inclusive low byte sequence of the input range.
    pub low: Vec<u8>,
    /// Inclusive high byte sequence of the input range.
    pub high: Vec<u8>,
    /// CID assigned to `low`; consecutive codes map to consecutive CIDs.
    pub cid_start: u32,
}

/// Fully parsed CMap, ready for character code decoding.
///
/// Holds codespace ranges (for variable-length code matching),
/// CID mappings, and Unicode mappings. Supports /UseCMap chaining
/// via `merge_base`.
#[derive(Debug)]
pub struct ParsedCMap {
    codespace_ranges: Vec<CodespaceRange>,
    /// Exact code -> CID mappings from cidchar sections. `AHashMap` for
    /// O(1) lookup. Per-process-seeded ahash instead of default SipHash:
    /// the `Vec<u8>` keys come from PDF CMap streams, which are 100%
    /// attacker-controlled. ahash is DOS-resistant where the
    /// `rustc_hash`/FxHash family is not.
    cid_chars: AHashMap<Vec<u8>, u32>,
    cid_ranges: Vec<CidRangeMapping>,
    /// Unicode mappings parsed from bfchar/bfrange sections.
    unicode_mappings: ToUnicodeCMap,
    /// Base CMap unicode lookups merged via /UseCMap. Checked as fallback
    /// when `unicode_mappings` doesn't have a match. Same `AHashMap`
    /// rationale as `cid_chars` -- attacker-controlled keys.
    base_bfchar: AHashMap<Vec<u8>, String>,
    base_bfrange: Vec<(Vec<u8>, Vec<u8>, String)>,
    is_vertical: bool,
}

impl ParsedCMap {
    /// Parse a CMap from raw stream bytes.
    ///
    /// Lenient: skips malformed sections and continues parsing.
    /// Enforces size limits (MAX_CMAP_ENTRIES, MAX_CODESPACE_RANGES).
    #[cfg(test)]
    pub fn parse(data: &[u8]) -> Self {
        let text = String::from_utf8_lossy(data);
        Self::parse_from_text(&text, data)
    }

    /// Internal parse from pre-converted text.
    ///
    /// Takes both `text` (for codespace/CID/WMode parsing) and `raw` (for
    /// ToUnicodeCMap::parse, which does its own UTF-8 conversion internally).
    /// This means the data is converted to UTF-8 twice: once by the caller
    /// and once inside ToUnicodeCMap::parse. Acceptable because CMap streams
    /// are typically small (a few KB) and this avoids changing the ToUnicodeCMap
    /// API surface.
    fn parse_from_text(text: &str, raw: &[u8]) -> Self {
        let codespace_ranges = parse_codespace_ranges(text);
        let (cid_chars, cid_ranges) = parse_cid_sections(text);
        let unicode_mappings = ToUnicodeCMap::parse(raw);
        let is_vertical = detect_vertical(text);

        ParsedCMap {
            codespace_ranges,
            cid_chars,
            cid_ranges,
            unicode_mappings,
            base_bfchar: AHashMap::new(),
            base_bfrange: Vec::new(),
            is_vertical,
        }
    }

    /// Parse a CMap from raw bytes and resolve /UseCMap inheritance.
    ///
    /// If the CMap references a predefined base via /UseCMap, builds
    /// a stub ParsedCMap for that base (using code_length from the
    /// predefined registry) and merges it.
    ///
    /// `depth` tracks recursion to prevent runaway chaining.
    pub fn parse_with_usecmap(data: &[u8], depth: usize) -> Self {
        let text = String::from_utf8_lossy(data);
        let mut cmap = Self::parse_from_text(&text, data);

        if depth >= MAX_USECMAP_DEPTH {
            return cmap;
        }

        if let Some(base_name) = extract_usecmap_name(&text) {
            // Only predefined CMaps are resolved by name. Embedded CMap
            // streams would need stream-ref resolution which isn't available
            // at this layer.
            if let Some(predefined) = lookup_predefined_cmap(&base_name) {
                let base = build_predefined_stub(predefined);
                // If the child has no explicit /WMode, inherit from the base.
                // Without this, a child that inherits from Identity-V would
                // default to horizontal.
                let child_has_wmode = text.contains("/WMode");
                cmap.merge_base(&base);
                if !child_has_wmode && base.is_vertical {
                    cmap.is_vertical = true;
                }
            }
        }

        cmap
    }

    /// Determine how many bytes the next character code consumes.
    ///
    /// Uses codespace ranges with longest-match-first strategy: tries
    /// the longest ranges first so multi-byte codes take precedence
    /// over single-byte ones when both could match.
    ///
    /// Returns 1 as fallback if no codespace range matches (defensive).
    pub fn code_length_for(&self, bytes: &[u8], offset: usize) -> usize {
        if self.codespace_ranges.is_empty() {
            return 1;
        }

        let remaining = bytes.len() - offset;

        // Collect distinct code lengths, sorted longest first
        let mut lengths: Vec<usize> = self.codespace_ranges.iter().map(|r| r.low.len()).collect();
        lengths.sort_unstable();
        lengths.dedup();
        lengths.reverse();

        for len in lengths {
            if len > remaining {
                continue;
            }
            let candidate = &bytes[offset..offset + len];
            for range in &self.codespace_ranges {
                if range.low.len() == len
                    && candidate >= range.low.as_slice()
                    && candidate <= range.high.as_slice()
                {
                    return len;
                }
            }
        }

        // No match. Consume 1 byte to avoid infinite loops.
        1
    }

    /// Look up a Unicode string for a character code.
    ///
    /// Checks the CMap's own bfchar/bfrange first (via `unicode_mappings`),
    /// then falls back to base CMap entries merged via /UseCMap.
    pub fn lookup_unicode(&self, code: &[u8]) -> Option<String> {
        // Child mappings take priority
        if let Some(s) = self.unicode_mappings.lookup(code) {
            return Some(s);
        }

        // Base bfchar
        if let Some(s) = self.base_bfchar.get(code) {
            return Some(s.clone());
        }

        // Base bfrange
        for (start, end, base_unicode) in &self.base_bfrange {
            if code.len() == start.len() && code >= start.as_slice() && code <= end.as_slice() {
                let offset = bytes_diff(start, code);
                if let Some(base_char) = base_unicode.chars().next() {
                    let target_code = base_char as u32 + offset;
                    if let Some(c) = char::from_u32(target_code) {
                        return Some(c.to_string());
                    }
                }
            }
        }

        None
    }

    /// Look up a CID for a character code.
    ///
    /// Checks cidchar (O(1) HashMap lookup) first, then cidrange.
    pub fn lookup_cid(&self, code: &[u8]) -> Option<u32> {
        // Exact match (O(1))
        if let Some(&cid) = self.cid_chars.get(code) {
            return Some(cid);
        }

        // Range lookup
        for range in &self.cid_ranges {
            if code.len() == range.low.len()
                && code >= range.low.as_slice()
                && code <= range.high.as_slice()
            {
                let offset = bytes_diff(&range.low, code);
                return Some(range.cid_start + offset);
            }
        }

        None
    }

    /// Whether this CMap uses vertical writing mode.
    #[cfg(test)]
    pub fn is_vertical(&self) -> bool {
        self.is_vertical
    }

    /// Number of codespace ranges in this CMap.
    pub fn codespace_range_count(&self) -> usize {
        self.codespace_ranges.len()
    }

    /// Decode a full byte string into Unicode text.
    ///
    /// Algorithm:
    /// 1. Use codespace ranges to determine variable-length code boundaries
    /// 2. For each code: try unicode lookup, then CID lookup with Identity
    ///    heuristic, then U+FFFD
    /// 3. Advance by the code length and repeat
    ///
    /// Not called in production (CompositeFont::decode_string handles the
    /// real path), but exercised by unit tests.
    #[cfg(test)]
    pub fn decode_string(&self, bytes: &[u8]) -> String {
        let mut result = String::new();
        let mut offset = 0;

        while offset < bytes.len() {
            let code_len = self.code_length_for(bytes, offset);
            let end = (offset + code_len).min(bytes.len());
            let code = &bytes[offset..end];

            // Try unicode mapping first
            if let Some(s) = self.lookup_unicode(code) {
                result.push_str(&s);
            } else if let Some(cid) = self.lookup_cid(code) {
                // Identity heuristic: treat CID as Unicode code point
                if let Some(c) = char::from_u32(cid) {
                    if !c.is_control() || matches!(c, '\t' | '\n' | '\r') {
                        result.push(c);
                    } else {
                        result.push('\u{FFFD}');
                    }
                } else {
                    result.push('\u{FFFD}');
                }
            } else {
                result.push('\u{FFFD}');
            }

            offset = end;
        }

        result
    }

    /// Merge entries from a base CMap (for /UseCMap inheritance).
    ///
    /// Base entries are added only where the child doesn't already
    /// have a mapping. Codespace ranges from the base are appended
    /// (up to the limit).
    pub fn merge_base(&mut self, base: &ParsedCMap) {
        // Merge codespace ranges (base ranges added if child doesn't
        // already cover those lengths, but we keep it simple: append all)
        for range in &base.codespace_ranges {
            if self.codespace_ranges.len() >= MAX_CODESPACE_RANGES {
                break;
            }
            self.codespace_ranges.push(range.clone());
        }

        // Merge CID char mappings (skip if child already has that code)
        for (code, &cid) in &base.cid_chars {
            if self.total_cid_entries() >= MAX_CMAP_ENTRIES {
                break;
            }
            self.cid_chars.entry(code.clone()).or_insert(cid);
        }

        // Merge CID range mappings (append; overlapping ranges are fine,
        // child ranges are checked first in lookup)
        for range in &base.cid_ranges {
            if self.total_cid_entries() >= MAX_CMAP_ENTRIES {
                break;
            }
            self.cid_ranges.push(range.clone());
        }

        // For base bfchar/bfrange, we'd need to extract from base.unicode_mappings,
        // but its fields are private. The base's own bfchar/bfrange were parsed by
        // ToUnicodeCMap::parse and are opaque. We store the base's ParsedCMap's
        // base_ fields AND create a lookup mechanism.
        //
        // Pragmatic approach: the base CMap's unicode_mappings will be consulted
        // via a callback pattern. Since we can't do that with the current type,
        // we note that in practice, /UseCMap for CID CMaps rarely carries bfchar/bfrange
        // (those live in the ToUnicode stream, not the encoding CMap). The main
        // value of /UseCMap is CID mappings and codespace ranges.
        //
        // If the base has base_bfchar/base_bfrange from its own merge chain,
        // propagate those.
        for (code, unicode) in &base.base_bfchar {
            if !self.base_bfchar.contains_key(code) {
                self.base_bfchar.insert(code.clone(), unicode.clone());
            }
        }
        for entry in &base.base_bfrange {
            self.base_bfrange.push(entry.clone());
        }
    }

    /// Total CID entries (chars + ranges) for limit checking.
    fn total_cid_entries(&self) -> usize {
        self.cid_chars.len() + self.cid_ranges.len()
    }
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/// Parse all codespacerange sections from the CMap text.
fn parse_codespace_ranges(text: &str) -> Vec<CodespaceRange> {
    let mut ranges = Vec::new();
    let mut rest = text;

    while let Some(start) = rest.find("begincodespacerange") {
        if ranges.len() >= MAX_CODESPACE_RANGES {
            break;
        }
        let after = &rest[start + "begincodespacerange".len()..];
        let end = match after.find("endcodespacerange") {
            Some(e) => e,
            None => break,
        };
        let section = &after[..end];
        parse_codespace_section(section, &mut ranges);
        rest = &after[end..];
    }

    ranges
}

/// Parse hex pairs from a codespacerange section.
fn parse_codespace_section(section: &str, ranges: &mut Vec<CodespaceRange>) {
    let mut chars = section.chars().peekable();
    while ranges.len() < MAX_CODESPACE_RANGES {
        let low = match next_hex_token(&mut chars) {
            Some(h) => h,
            None => break,
        };
        let high = match next_hex_token(&mut chars) {
            Some(h) => h,
            None => break,
        };
        // Low and high must have the same byte length
        if low.len() != high.len() || low.is_empty() {
            continue;
        }
        ranges.push(CodespaceRange { low, high });
    }
}

/// Parse all cidchar and cidrange sections.
fn parse_cid_sections(text: &str) -> (AHashMap<Vec<u8>, u32>, Vec<CidRangeMapping>) {
    let mut cid_chars = AHashMap::new();
    let mut cid_ranges = Vec::new();
    let total = |c: &AHashMap<Vec<u8>, u32>, r: &[CidRangeMapping]| c.len() + r.len();

    // Parse cidchar sections
    let mut rest = text;
    while let Some(start) = rest.find("begincidchar") {
        if total(&cid_chars, &cid_ranges) >= MAX_CMAP_ENTRIES {
            break;
        }
        let after = &rest[start + "begincidchar".len()..];
        let end = match after.find("endcidchar") {
            Some(e) => e,
            None => break,
        };
        let section = &after[..end];
        let remaining = MAX_CMAP_ENTRIES - total(&cid_chars, &cid_ranges);
        parse_cidchar_section(section, &mut cid_chars, remaining);
        rest = &after[end..];
    }

    // Parse cidrange sections
    rest = text;
    while let Some(start) = rest.find("begincidrange") {
        if total(&cid_chars, &cid_ranges) >= MAX_CMAP_ENTRIES {
            break;
        }
        let after = &rest[start + "begincidrange".len()..];
        let end = match after.find("endcidrange") {
            Some(e) => e,
            None => break,
        };
        let section = &after[..end];
        let remaining = MAX_CMAP_ENTRIES - total(&cid_chars, &cid_ranges);
        parse_cidrange_section(section, &mut cid_ranges, remaining);
        rest = &after[end..];
    }

    (cid_chars, cid_ranges)
}

/// Parse a single cidchar section.
fn parse_cidchar_section(section: &str, mappings: &mut AHashMap<Vec<u8>, u32>, max_entries: usize) {
    let mut chars = section.chars().peekable();
    let mut added = 0;
    while added < max_entries {
        let code = match next_hex_token(&mut chars) {
            Some(h) => h,
            None => break,
        };
        let cid = match next_integer_token(&mut chars) {
            Some(n) => n,
            None => break,
        };
        mappings.insert(code, cid);
        added += 1;
    }
}

/// Parse a single cidrange section.
fn parse_cidrange_section(section: &str, ranges: &mut Vec<CidRangeMapping>, max_entries: usize) {
    let mut chars = section.chars().peekable();
    let mut added = 0;
    while added < max_entries {
        let low = match next_hex_token(&mut chars) {
            Some(h) => h,
            None => break,
        };
        let high = match next_hex_token(&mut chars) {
            Some(h) => h,
            None => break,
        };
        let cid_start = match next_integer_token(&mut chars) {
            Some(n) => n,
            None => break,
        };
        if low.len() != high.len() || low.is_empty() {
            continue;
        }
        ranges.push(CidRangeMapping {
            low,
            high,
            cid_start,
        });
        added += 1;
    }
}

/// Detect vertical writing mode from the CMap stream.
///
/// Checks for `/WMode 1` in the CMap dictionary section. The WMode
/// entry is typically written as `/WMode 1` for vertical.
fn detect_vertical(text: &str) -> bool {
    // Look for /WMode followed by whitespace and a digit
    if let Some(pos) = text.find("/WMode") {
        let after = &text[pos + "/WMode".len()..];
        let trimmed = after.trim_start();
        return trimmed.starts_with('1');
    }
    false
}

/// Extract the /UseCMap name from a CMap stream.
///
/// The pattern in CMap files is typically:
///   /BaseCMapName usecmap
/// where BaseCMapName is a name token preceding the `usecmap` operator.
fn extract_usecmap_name(text: &str) -> Option<String> {
    // Look for "usecmap" keyword (case-sensitive per CMap spec)
    let pos = text.find("usecmap")?;
    let before = text[..pos].trim_end();

    // Walk backwards to find the /Name token
    // CMap syntax: /SomeName usecmap
    let name_start = before.rfind('/')?;
    let name = &before[name_start + 1..];
    let name = name.trim();

    if name.is_empty() {
        return None;
    }

    Some(name.to_string())
}

/// Build a stub ParsedCMap for a predefined CMap.
///
/// Predefined CMaps (Identity-H, Identity-V, etc.) have known properties
/// but we don't have their full CID tables compiled in. We create a stub
/// with the right codespace ranges so code_length_for works correctly.
fn build_predefined_stub(predefined: &super::cmap::PredefinedCMap) -> ParsedCMap {
    let code_len = predefined.code_length as usize;

    // Build codespace range: <00..00> to <FF.FF> for the code length
    let low = vec![0x00; code_len];
    let high = vec![0xFF; code_len];

    // For identity CMaps, add a CID range covering the full codespace
    // (code == CID identity mapping)
    let mut cid_ranges = Vec::new();
    if predefined.is_identity {
        cid_ranges.push(CidRangeMapping {
            low: low.clone(),
            high: high.clone(),
            cid_start: 0,
        });
    }

    ParsedCMap {
        codespace_ranges: vec![CodespaceRange { low, high }],
        cid_chars: AHashMap::new(),
        cid_ranges,
        unicode_mappings: ToUnicodeCMap::parse(b""),
        base_bfchar: AHashMap::new(),
        base_bfrange: Vec::new(),
        is_vertical: predefined.is_vertical,
    }
}

// ---------------------------------------------------------------------------
// Token extraction helpers
// ---------------------------------------------------------------------------

// Reuse hex parsing helpers from tounicode.rs.
#[cfg(test)]
use super::tounicode::hex_to_unicode_string;
use super::tounicode::{bytes_diff, next_hex_token};

/// Extract the next decimal integer token, skipping whitespace.
///
/// Used for CID values in cidchar/cidrange sections.
fn next_integer_token(chars: &mut std::iter::Peekable<std::str::Chars>) -> Option<u32> {
    // Skip whitespace
    while let Some(&c) = chars.peek() {
        if c.is_ascii_whitespace() {
            chars.next();
        } else {
            break;
        }
    }

    let mut digits = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            digits.push(c);
            chars.next();
        } else {
            break;
        }
    }

    if digits.is_empty() {
        return None;
    }

    digits.parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ====================================================================
    // 1. Codespace range parsing
    // ====================================================================

    #[test]
    fn test_single_codespace_range() {
        let data = b"\
/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        assert_eq!(cmap.codespace_ranges.len(), 1);
        assert_eq!(cmap.codespace_ranges[0].low, vec![0x00]);
        assert_eq!(cmap.codespace_ranges[0].high, vec![0xFF]);
    }

    #[test]
    fn test_multiple_codespace_ranges() {
        let data = b"\
begincmap
2 begincodespacerange
<00> <80>
<8140> <9FFC>
endcodespacerange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        assert_eq!(cmap.codespace_ranges.len(), 2);
        assert_eq!(cmap.codespace_ranges[0].low, vec![0x00]);
        assert_eq!(cmap.codespace_ranges[0].high, vec![0x80]);
        assert_eq!(cmap.codespace_ranges[1].low, vec![0x81, 0x40]);
        assert_eq!(cmap.codespace_ranges[1].high, vec![0x9F, 0xFC]);
    }

    #[test]
    fn test_variable_length_codespace_ranges() {
        // Mixed 1-byte and 2-byte ranges (common in CJK CMaps)
        let data = b"\
begincmap
3 begincodespacerange
<00> <80>
<8140> <9FFC>
<E040> <FCFC>
endcodespacerange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        assert_eq!(cmap.codespace_ranges.len(), 3);
        // 1-byte range
        assert_eq!(cmap.codespace_ranges[0].low.len(), 1);
        // 2-byte ranges
        assert_eq!(cmap.codespace_ranges[1].low.len(), 2);
        assert_eq!(cmap.codespace_ranges[2].low.len(), 2);
    }

    // ====================================================================
    // 2. bfchar and bfrange parsing (via ToUnicodeCMap delegation)
    // ====================================================================

    #[test]
    fn test_bfchar_parsing() {
        let data = b"\
begincmap
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
2 beginbfchar
<0041> <0048>
<0042> <0049>
endbfchar
endcmap
";
        let cmap = ParsedCMap::parse(data);
        assert_eq!(cmap.lookup_unicode(&[0x00, 0x41]), Some("H".to_string()));
        assert_eq!(cmap.lookup_unicode(&[0x00, 0x42]), Some("I".to_string()));
        assert_eq!(cmap.lookup_unicode(&[0x00, 0x43]), None);
    }

    #[test]
    fn test_bfrange_parsing() {
        let data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
1 beginbfrange
<41> <43> <0041>
endbfrange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        assert_eq!(cmap.lookup_unicode(&[0x41]), Some("A".to_string()));
        assert_eq!(cmap.lookup_unicode(&[0x42]), Some("B".to_string()));
        assert_eq!(cmap.lookup_unicode(&[0x43]), Some("C".to_string()));
        assert_eq!(cmap.lookup_unicode(&[0x44]), None);
    }

    // ====================================================================
    // 3. cidchar and cidrange parsing
    // ====================================================================

    #[test]
    fn test_cidchar_parsing() {
        let data = b"\
begincmap
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
2 begincidchar
<0041> 100
<0042> 200
endcidchar
endcmap
";
        let cmap = ParsedCMap::parse(data);
        assert_eq!(cmap.lookup_cid(&[0x00, 0x41]), Some(100));
        assert_eq!(cmap.lookup_cid(&[0x00, 0x42]), Some(200));
        assert_eq!(cmap.lookup_cid(&[0x00, 0x43]), None);
    }

    #[test]
    fn test_cidrange_parsing() {
        let data = b"\
begincmap
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
1 begincidrange
<0100> <0105> 500
endcidrange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        assert_eq!(cmap.lookup_cid(&[0x01, 0x00]), Some(500));
        assert_eq!(cmap.lookup_cid(&[0x01, 0x01]), Some(501));
        assert_eq!(cmap.lookup_cid(&[0x01, 0x05]), Some(505));
        assert_eq!(cmap.lookup_cid(&[0x01, 0x06]), None);
    }

    #[test]
    fn test_cidrange_offset_calculation() {
        let data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
1 begincidrange
<20> <7E> 1
endcidrange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        // <20> maps to CID 1, <21> to CID 2, etc.
        assert_eq!(cmap.lookup_cid(&[0x20]), Some(1));
        assert_eq!(cmap.lookup_cid(&[0x41]), Some(34)); // 0x41 - 0x20 + 1 = 34
        assert_eq!(cmap.lookup_cid(&[0x7E]), Some(95)); // 0x7E - 0x20 + 1 = 95
        assert_eq!(cmap.lookup_cid(&[0x7F]), None);
    }

    // ====================================================================
    // 4. Variable-length code matching
    // ====================================================================

    #[test]
    fn test_variable_length_code_matching() {
        // CMap with both 1-byte and 2-byte codes
        let data = b"\
begincmap
2 begincodespacerange
<00> <80>
<8140> <9FFC>
endcodespacerange
endcmap
";
        let cmap = ParsedCMap::parse(data);

        // Single-byte code in the 1-byte range
        assert_eq!(cmap.code_length_for(&[0x41], 0), 1);

        // Two-byte code in the 2-byte range
        assert_eq!(cmap.code_length_for(&[0x81, 0x40], 0), 2);
        assert_eq!(cmap.code_length_for(&[0x90, 0x80], 0), 2);

        // Byte that starts a 2-byte code: 0x81 followed by 0x40
        let bytes = [0x41, 0x81, 0x40, 0x20];
        assert_eq!(cmap.code_length_for(&bytes, 0), 1); // 0x41 is 1-byte
        assert_eq!(cmap.code_length_for(&bytes, 1), 2); // 0x81 0x40 is 2-byte
        assert_eq!(cmap.code_length_for(&bytes, 3), 1); // 0x20 is 1-byte
    }

    #[test]
    fn test_code_length_empty_codespace() {
        let cmap = ParsedCMap::parse(b"");
        // No codespace ranges: fallback to 1
        assert_eq!(cmap.code_length_for(&[0x41, 0x42], 0), 1);
    }

    #[test]
    fn test_code_length_no_match() {
        let data = b"\
begincmap
1 begincodespacerange
<20> <7E>
endcodespacerange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        // 0xFF is outside the codespace range, fallback to 1
        assert_eq!(cmap.code_length_for(&[0xFF], 0), 1);
    }

    #[test]
    fn test_code_length_longest_match_first() {
        // If a byte could be the start of both a 1-byte and 2-byte code,
        // longest match wins
        let data = b"\
begincmap
2 begincodespacerange
<00> <FF>
<0000> <FFFF>
endcodespacerange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        // With two bytes available, the 2-byte range should match first
        assert_eq!(cmap.code_length_for(&[0x41, 0x42], 0), 2);
        // With only one byte remaining, the 1-byte range matches
        assert_eq!(cmap.code_length_for(&[0x41], 0), 1);
    }

    // ====================================================================
    // 5. /UseCMap name extraction
    // ====================================================================

    #[test]
    fn test_usecmap_extraction() {
        let text = "/CIDInit /ProcSet findresource begin\n\
                    /Identity-H usecmap\n\
                    begincmap\n";
        assert_eq!(extract_usecmap_name(text), Some("Identity-H".to_string()));
    }

    #[test]
    fn test_usecmap_no_name() {
        let text = "begincmap\nendcmap";
        assert_eq!(extract_usecmap_name(text), None);
    }

    #[test]
    fn test_usecmap_complex_name() {
        let text = "/90ms-RKSJ-H usecmap\nbegincmap";
        assert_eq!(extract_usecmap_name(text), Some("90ms-RKSJ-H".to_string()));
    }

    // ====================================================================
    // 6. decode_string with variable-length codes
    // ====================================================================

    #[test]
    fn test_decode_string_simple() {
        let data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
2 beginbfchar
<41> <0048>
<42> <0049>
endbfchar
endcmap
";
        let cmap = ParsedCMap::parse(data);
        assert_eq!(cmap.decode_string(&[0x41, 0x42]), "HI");
    }

    #[test]
    fn test_decode_string_variable_length() {
        let data = b"\
begincmap
2 begincodespacerange
<00> <80>
<8100> <9FFF>
endcodespacerange
3 beginbfchar
<41> <0041>
<8100> <4E2D>
<8101> <6587>
endbfchar
endcmap
";
        let cmap = ParsedCMap::parse(data);
        // Mixed: 1-byte 'A', then 2-byte Chinese chars
        let bytes = [0x41, 0x81, 0x00, 0x81, 0x01];
        let result = cmap.decode_string(&bytes);
        assert_eq!(result, "A\u{4E2D}\u{6587}");
    }

    #[test]
    fn test_decode_string_with_cid_fallback() {
        let data = b"\
begincmap
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
1 begincidrange
<0000> <FFFF> 0
endcidrange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        // CID 0x0041 = 65 = 'A' via identity heuristic
        assert_eq!(cmap.decode_string(&[0x00, 0x41]), "A");
    }

    #[test]
    fn test_decode_string_unknown_code() {
        let data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        // No mappings at all: everything becomes U+FFFD
        assert_eq!(cmap.decode_string(&[0x41]), "\u{FFFD}");
    }

    #[test]
    fn test_decode_string_empty_input() {
        let cmap = ParsedCMap::parse(b"");
        assert_eq!(cmap.decode_string(&[]), "");
    }

    // ====================================================================
    // 7. Size limit enforcement
    // ====================================================================

    #[test]
    fn test_cid_entry_limit() {
        // Build a CMap with more than MAX_CMAP_ENTRIES cidchar entries
        let mut cmap_text = String::from("begincmap\n");
        cmap_text.push_str("1 begincodespacerange\n<000000> <FFFFFF>\nendcodespacerange\n");

        let total = MAX_CMAP_ENTRIES + 100;
        let block_size = 100;
        let mut written = 0;
        while written < total {
            let this_block = block_size.min(total - written);
            cmap_text.push_str(&format!("{} begincidchar\n", this_block));
            for j in 0..this_block {
                let code = written + j;
                cmap_text.push_str(&format!("<{:06X}> {}\n", code, code));
            }
            cmap_text.push_str("endcidchar\n");
            written += this_block;
        }
        cmap_text.push_str("endcmap\n");

        let cmap = ParsedCMap::parse(cmap_text.as_bytes());
        assert!(
            cmap.cid_chars.len() <= MAX_CMAP_ENTRIES,
            "got {} CID char entries, expected <= {}",
            cmap.cid_chars.len(),
            MAX_CMAP_ENTRIES
        );
    }

    #[test]
    fn test_codespace_range_limit() {
        let mut cmap_text = String::from("begincmap\n");
        // Write more than MAX_CODESPACE_RANGES ranges in a single section
        let total = MAX_CODESPACE_RANGES + 20;
        cmap_text.push_str(&format!("{} begincodespacerange\n", total));
        for i in 0..total {
            let low = (i & 0xFF) as u8;
            let high = low;
            cmap_text.push_str(&format!("<{:02X}> <{:02X}>\n", low, high));
        }
        cmap_text.push_str("endcodespacerange\nendcmap\n");

        let cmap = ParsedCMap::parse(cmap_text.as_bytes());
        assert!(
            cmap.codespace_ranges.len() <= MAX_CODESPACE_RANGES,
            "got {} codespace ranges, expected <= {}",
            cmap.codespace_ranges.len(),
            MAX_CODESPACE_RANGES
        );
    }

    // ====================================================================
    // 8. Empty and malformed input
    // ====================================================================

    #[test]
    fn test_empty_input() {
        let cmap = ParsedCMap::parse(b"");
        assert!(cmap.codespace_ranges.is_empty());
        assert!(cmap.cid_chars.is_empty());
        assert!(cmap.cid_ranges.is_empty());
        assert_eq!(cmap.lookup_unicode(&[0x41]), None);
        assert_eq!(cmap.lookup_cid(&[0x41]), None);
        assert!(!cmap.is_vertical());
    }

    #[test]
    fn test_malformed_missing_end_marker() {
        // begincodespacerange without endcodespacerange
        let data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcmap
";
        let cmap = ParsedCMap::parse(data);
        // Should not crash. No ranges parsed because end marker is missing.
        assert!(cmap.codespace_ranges.is_empty());
    }

    #[test]
    fn test_malformed_odd_hex_token() {
        // Odd-length hex token in codespace range
        let data = b"\
begincmap
1 begincodespacerange
<0> <F>
endcodespacerange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        // Odd hex gets padded: <0> -> 0x00, <F> -> 0xF0
        assert_eq!(cmap.codespace_ranges.len(), 1);
        assert_eq!(cmap.codespace_ranges[0].low, vec![0x00]);
        assert_eq!(cmap.codespace_ranges[0].high, vec![0xF0]);
    }

    #[test]
    fn test_malformed_cidchar_missing_cid() {
        // cidchar with hex code but missing CID integer
        let data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
1 begincidchar
<41>
endcidchar
endcmap
";
        let cmap = ParsedCMap::parse(data);
        // Missing CID should be skipped gracefully
        assert!(cmap.cid_chars.is_empty());
    }

    #[test]
    fn test_garbage_between_sections() {
        // Random garbage between valid sections should be skipped
        let data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
some random garbage here
1 begincidchar
<41> 65
endcidchar
more garbage
endcmap
";
        let cmap = ParsedCMap::parse(data);
        assert_eq!(cmap.codespace_ranges.len(), 1);
        assert_eq!(cmap.lookup_cid(&[0x41]), Some(65));
    }

    // ====================================================================
    // 9. Overlapping codespace ranges
    // ====================================================================

    #[test]
    fn test_overlapping_codespace_ranges() {
        // Two ranges that overlap in byte values
        let data = b"\
begincmap
2 begincodespacerange
<00> <FF>
<40> <80>
endcodespacerange
1 beginbfchar
<50> <0041>
endbfchar
endcmap
";
        let cmap = ParsedCMap::parse(data);
        // Both ranges are 1-byte, code 0x50 falls in both
        assert_eq!(cmap.code_length_for(&[0x50], 0), 1);
        assert_eq!(cmap.lookup_unicode(&[0x50]), Some("A".to_string()));
    }

    // ====================================================================
    // 10. CID lookup for range mappings
    // ====================================================================

    #[test]
    fn test_cid_range_boundary_values() {
        let data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
1 begincidrange
<10> <20> 100
endcidrange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        assert_eq!(cmap.lookup_cid(&[0x0F]), None); // just below
        assert_eq!(cmap.lookup_cid(&[0x10]), Some(100)); // low bound
        assert_eq!(cmap.lookup_cid(&[0x18]), Some(108)); // middle
        assert_eq!(cmap.lookup_cid(&[0x20]), Some(116)); // high bound
        assert_eq!(cmap.lookup_cid(&[0x21]), None); // just above
    }

    #[test]
    fn test_cid_char_takes_priority_over_range() {
        // If both cidchar and cidrange match, cidchar is checked first
        let data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
1 begincidchar
<15> 999
endcidchar
1 begincidrange
<10> <20> 100
endcidrange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        // <15> has an exact cidchar mapping to 999
        assert_eq!(cmap.lookup_cid(&[0x15]), Some(999));
        // <16> falls through to cidrange: CID = 100 + (0x16 - 0x10) = 106
        assert_eq!(cmap.lookup_cid(&[0x16]), Some(106));
    }

    // ====================================================================
    // Writing mode detection
    // ====================================================================

    #[test]
    fn test_vertical_wmode() {
        let data = b"\
begincmap
/CMapName /TestV def
/WMode 1 def
1 begincodespacerange
<00> <FF>
endcodespacerange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        assert!(cmap.is_vertical());
    }

    #[test]
    fn test_horizontal_wmode() {
        let data = b"\
begincmap
/WMode 0 def
1 begincodespacerange
<00> <FF>
endcodespacerange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        assert!(!cmap.is_vertical());
    }

    #[test]
    fn test_no_wmode_defaults_horizontal() {
        let data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        assert!(!cmap.is_vertical());
    }

    // ====================================================================
    // merge_base
    // ====================================================================

    #[test]
    fn test_merge_base_codespace_ranges() {
        let child_data = b"\
begincmap
1 begincodespacerange
<00> <7F>
endcodespacerange
endcmap
";
        let base_data = b"\
begincmap
1 begincodespacerange
<8000> <FFFF>
endcodespacerange
endcmap
";
        let mut child = ParsedCMap::parse(child_data);
        let base = ParsedCMap::parse(base_data);
        child.merge_base(&base);

        assert_eq!(child.codespace_ranges.len(), 2);
        // Child's range
        assert_eq!(child.codespace_ranges[0].low, vec![0x00]);
        // Base's range
        assert_eq!(child.codespace_ranges[1].low, vec![0x80, 0x00]);
    }

    #[test]
    fn test_merge_base_cid_no_override() {
        let child_data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
1 begincidchar
<41> 100
endcidchar
endcmap
";
        let base_data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
2 begincidchar
<41> 200
<42> 300
endcidchar
endcmap
";
        let mut child = ParsedCMap::parse(child_data);
        let base = ParsedCMap::parse(base_data);
        child.merge_base(&base);

        // Child's <41> -> 100 should NOT be overridden by base's <41> -> 200
        assert_eq!(child.lookup_cid(&[0x41]), Some(100));
        // Base's <42> -> 300 should be merged in
        assert_eq!(child.lookup_cid(&[0x42]), Some(300));
    }

    // ====================================================================
    // parse_with_usecmap
    // ====================================================================

    #[test]
    fn test_parse_with_usecmap_identity() {
        let data = b"\
/Identity-H usecmap
begincmap
/CMapName /TestCMap def
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
1 begincidchar
<0041> 999
endcidchar
endcmap
";
        let cmap = ParsedCMap::parse_with_usecmap(data, 0);
        // Child's explicit mapping
        assert_eq!(cmap.lookup_cid(&[0x00, 0x41]), Some(999));
        // Base Identity-H provides a full range CID mapping
        // (but child's explicit mapping takes priority)
        assert_eq!(cmap.lookup_cid(&[0x00, 0x42]), Some(0x0042));
    }

    // ====================================================================
    // bytes_diff
    // ====================================================================

    #[test]
    fn test_bytes_diff_simple() {
        assert_eq!(bytes_diff(&[0x10], &[0x15]), 5);
        assert_eq!(bytes_diff(&[0x00], &[0xFF]), 255);
        assert_eq!(bytes_diff(&[0x00, 0x00], &[0x00, 0x0A]), 10);
        assert_eq!(bytes_diff(&[0x01, 0x00], &[0x01, 0x05]), 5);
    }

    // ====================================================================
    // Hex token extraction
    // ====================================================================

    #[test]
    fn test_next_hex_token_basic() {
        let mut chars = "<4142>".chars().peekable();
        let result = next_hex_token(&mut chars);
        assert_eq!(result, Some(vec![0x41, 0x42]));
    }

    #[test]
    fn test_next_hex_token_with_whitespace() {
        let mut chars = "< 41 42 >".chars().peekable();
        let result = next_hex_token(&mut chars);
        assert_eq!(result, Some(vec![0x41, 0x42]));
    }

    #[test]
    fn test_next_hex_token_odd_length() {
        let mut chars = "<F>".chars().peekable();
        let result = next_hex_token(&mut chars);
        // Odd-length: trailing nibble padded to F0
        assert_eq!(result, Some(vec![0xF0]));
    }

    #[test]
    fn test_next_hex_token_empty() {
        let mut chars = "<>".chars().peekable();
        let result = next_hex_token(&mut chars);
        assert_eq!(result, Some(vec![]));
    }

    #[test]
    fn test_next_hex_token_no_angle() {
        let mut chars = "hello".chars().peekable();
        let result = next_hex_token(&mut chars);
        assert_eq!(result, None);
    }

    // ====================================================================
    // Integer token extraction
    // ====================================================================

    #[test]
    fn test_next_integer_token_basic() {
        let mut chars = " 42 ".chars().peekable();
        assert_eq!(next_integer_token(&mut chars), Some(42));
    }

    #[test]
    fn test_next_integer_token_no_digits() {
        let mut chars = " abc ".chars().peekable();
        assert_eq!(next_integer_token(&mut chars), None);
    }

    #[test]
    fn test_next_integer_token_at_end() {
        let mut chars = "123".chars().peekable();
        assert_eq!(next_integer_token(&mut chars), Some(123));
    }

    // ====================================================================
    // detect_vertical
    // ====================================================================

    #[test]
    fn test_detect_vertical_true() {
        assert!(detect_vertical("/WMode 1 def"));
        assert!(detect_vertical("/WMode  1"));
        assert!(detect_vertical("stuff /WMode 1 more stuff"));
    }

    #[test]
    fn test_detect_vertical_false() {
        assert!(!detect_vertical("/WMode 0 def"));
        assert!(!detect_vertical("no wmode here"));
        assert!(!detect_vertical(""));
    }

    // ====================================================================
    // Full integration: realistic CMap
    // ====================================================================

    #[test]
    fn test_realistic_cjk_cmap() {
        // Simulate a simplified Shift-JIS-like CMap
        let data = b"\
/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo 3 dict dup begin
  /Registry (Adobe) def
  /Ordering (Japan1) def
  /Supplement 6 def
end def
/CMapName /90ms-RKSJ-H def
/WMode 0 def
3 begincodespacerange
<00> <80>
<8140> <9FFC>
<E040> <FCFC>
endcodespacerange
2 begincidchar
<20> 1
<5C> 97
endcidchar
1 begincidrange
<8140> <817E> 633
endcidrange
endcmap
CMapName currentdict /CMap defineresource pop
end
end
";
        let cmap = ParsedCMap::parse(data);

        // Codespace ranges
        assert_eq!(cmap.codespace_ranges.len(), 3);

        // CID char lookups
        assert_eq!(cmap.lookup_cid(&[0x20]), Some(1));
        assert_eq!(cmap.lookup_cid(&[0x5C]), Some(97));

        // CID range lookup
        assert_eq!(cmap.lookup_cid(&[0x81, 0x40]), Some(633));
        assert_eq!(cmap.lookup_cid(&[0x81, 0x41]), Some(634));
        assert_eq!(cmap.lookup_cid(&[0x81, 0x7E]), Some(695));

        // Variable-length code matching
        assert_eq!(cmap.code_length_for(&[0x20], 0), 1);
        assert_eq!(cmap.code_length_for(&[0x81, 0x40], 0), 2);

        // Not vertical
        assert!(!cmap.is_vertical());
    }

    #[test]
    fn test_multiple_cidchar_sections() {
        // Some CMaps split entries across multiple sections
        let data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
2 begincidchar
<10> 1
<11> 2
endcidchar
2 begincidchar
<20> 10
<21> 11
endcidchar
endcmap
";
        let cmap = ParsedCMap::parse(data);
        assert_eq!(cmap.lookup_cid(&[0x10]), Some(1));
        assert_eq!(cmap.lookup_cid(&[0x11]), Some(2));
        assert_eq!(cmap.lookup_cid(&[0x20]), Some(10));
        assert_eq!(cmap.lookup_cid(&[0x21]), Some(11));
    }

    #[test]
    fn test_decode_string_with_mixed_mappings() {
        // CMap with both unicode (bfchar) and CID mappings
        let data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
1 beginbfchar
<41> <0048>
endbfchar
1 begincidrange
<50> <5F> 80
endcidrange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        // <41> has unicode mapping -> "H"
        // <50> has CID mapping -> CID 80 -> 'P' via identity heuristic
        let result = cmap.decode_string(&[0x41, 0x50]);
        assert_eq!(result, "HP");
    }

    // ====================================================================
    // hex_to_unicode_string
    // ====================================================================

    #[test]
    fn test_hex_to_unicode_bmp() {
        assert_eq!(hex_to_unicode_string(&[0x00, 0x41]), "A");
        assert_eq!(hex_to_unicode_string(&[0x4E, 0x2D]), "\u{4E2D}");
    }

    #[test]
    fn test_hex_to_unicode_surrogate_pair() {
        // U+1F600 = D83D DE00 in UTF-16
        let bytes = vec![0xD8, 0x3D, 0xDE, 0x00];
        assert_eq!(hex_to_unicode_string(&bytes), "\u{1F600}");
    }

    #[test]
    fn test_hex_to_unicode_empty() {
        assert_eq!(hex_to_unicode_string(&[]), "");
    }

    #[test]
    fn test_hex_to_unicode_single_byte() {
        // Single byte is insufficient for a UTF-16 code unit
        assert_eq!(hex_to_unicode_string(&[0x41]), "");
    }

    // ====================================================================
    // Edge cases
    // ====================================================================

    #[test]
    fn test_codespace_range_mismatched_lengths() {
        // Low and high with different byte lengths should be skipped
        let data = b"\
begincmap
1 begincodespacerange
<00> <FFFF>
endcodespacerange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        // Mismatched lengths (1 vs 2) should be rejected
        assert!(cmap.codespace_ranges.is_empty());
    }

    #[test]
    fn test_cidrange_mismatched_lengths() {
        let data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
1 begincidrange
<00> <FFFF> 0
endcidrange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        // Mismatched lengths should be skipped
        assert!(cmap.cid_ranges.is_empty());
    }

    #[test]
    fn test_build_predefined_stub_identity() {
        let predefined = lookup_predefined_cmap("Identity-H");
        assert!(predefined.is_some());
        let stub = build_predefined_stub(predefined.unwrap());
        assert_eq!(stub.codespace_ranges.len(), 1);
        assert_eq!(stub.codespace_ranges[0].low.len(), 2);
        assert!(!stub.cid_ranges.is_empty()); // identity mapping
        assert!(!stub.is_vertical());
    }

    #[test]
    fn test_build_predefined_stub_vertical() {
        let predefined = lookup_predefined_cmap("Identity-V");
        assert!(predefined.is_some());
        let stub = build_predefined_stub(predefined.unwrap());
        assert!(stub.is_vertical());
    }

    // ====================================================================
    // Hardened edge case tests
    // ====================================================================

    // 1. CMap with both bfchar AND cidrange -- unicode lookups take priority
    #[test]
    fn test_unicode_takes_priority_over_cid_in_decode() {
        // Both bfchar and cidrange map code <0041>. The bfchar unicode
        // mapping should win in decode_string (lookup_unicode is checked
        // before lookup_cid).
        let data = b"\
begincmap
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
1 beginbfchar
<0041> <4E2D>
endbfchar
1 begincidrange
<0040> <0050> 64
endcidrange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        // bfchar maps <0041> -> U+4E2D (Chinese character)
        assert_eq!(
            cmap.lookup_unicode(&[0x00, 0x41]),
            Some("\u{4E2D}".to_string())
        );
        // cidrange also covers <0041> -> CID 65
        assert_eq!(cmap.lookup_cid(&[0x00, 0x41]), Some(65));
        // decode_string should use the unicode mapping, not the CID
        assert_eq!(cmap.decode_string(&[0x00, 0x41]), "\u{4E2D}");
        // Code <0042> has no bfchar, so it falls through to CID 66 -> 'B'
        assert_eq!(cmap.decode_string(&[0x00, 0x42]), "B");
    }

    // 2. Large CMap near MAX_CMAP_ENTRIES -- verify truncation is exact
    #[test]
    fn test_cid_entry_limit_exact_boundary() {
        // Fill cidchar to exactly MAX_CMAP_ENTRIES, then add cidranges.
        // The cidranges should be rejected since we're at the limit.
        let mut cmap_text = String::from("begincmap\n");
        cmap_text.push_str("1 begincodespacerange\n<000000> <FFFFFF>\nendcodespacerange\n");

        // Write exactly MAX_CMAP_ENTRIES cidchar entries
        let block_size = 100;
        let mut written = 0;
        while written < MAX_CMAP_ENTRIES {
            let this_block = block_size.min(MAX_CMAP_ENTRIES - written);
            cmap_text.push_str(&format!("{} begincidchar\n", this_block));
            for j in 0..this_block {
                let code = written + j;
                cmap_text.push_str(&format!("<{:06X}> {}\n", code, code));
            }
            cmap_text.push_str("endcidchar\n");
            written += this_block;
        }

        // Add a cidrange that should be rejected (at limit)
        cmap_text.push_str("1 begincidrange\n<FF0000> <FF00FF> 9000\nendcidrange\n");
        cmap_text.push_str("endcmap\n");

        let cmap = ParsedCMap::parse(cmap_text.as_bytes());
        assert_eq!(cmap.cid_chars.len(), MAX_CMAP_ENTRIES);
        // The cidrange might or might not be added depending on when the
        // limit check fires, but total should not exceed MAX_CMAP_ENTRIES
        assert!(
            cmap.total_cid_entries() <= MAX_CMAP_ENTRIES,
            "total CID entries {} exceeded limit {}",
            cmap.total_cid_entries(),
            MAX_CMAP_ENTRIES
        );
    }

    // 3. Hex tokens with odd lengths (multi-nibble)
    #[test]
    fn test_hex_token_three_nibble_padding() {
        // <123> has 3 hex chars. Should produce [0x12, 0x30] (last nibble padded).
        let mut chars = "<123>".chars().peekable();
        let result = next_hex_token(&mut chars);
        assert_eq!(result, Some(vec![0x12, 0x30]));
    }

    #[test]
    fn test_hex_token_five_nibble_padding() {
        // <ABCDE> has 5 hex chars. Should produce [0xAB, 0xCD, 0xE0].
        let mut chars = "<ABCDE>".chars().peekable();
        let result = next_hex_token(&mut chars);
        assert_eq!(result, Some(vec![0xAB, 0xCD, 0xE0]));
    }

    #[test]
    fn test_codespace_range_with_odd_hex() {
        // Odd-length hex in codespace range: <0> pads to 0x00, <F> pads to 0xF0
        let data = b"\
begincmap
1 begincodespacerange
<0> <F>
endcodespacerange
1 begincidrange
<00> <F0> 0
endcidrange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        assert_eq!(cmap.codespace_ranges.len(), 1);
        assert_eq!(cmap.codespace_ranges[0].low, vec![0x00]);
        assert_eq!(cmap.codespace_ranges[0].high, vec![0xF0]);
        // A code that falls in the padded range should work
        assert_eq!(cmap.lookup_cid(&[0x50]), Some(0x50));
    }

    // 4. Empty codespace range count (count says 0)
    #[test]
    fn test_empty_codespace_range_section() {
        // The count line says 0 but we still have begincodespacerange/end markers.
        // Parser just parses what's between the markers (nothing).
        let data = b"\
begincmap
0 begincodespacerange
endcodespacerange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        assert!(cmap.codespace_ranges.is_empty());
        // code_length_for falls back to 1 with no codespace ranges
        assert_eq!(cmap.code_length_for(&[0x41], 0), 1);
    }

    // 5. Codespace range with mismatched byte lengths (already tested for
    //    the basic case; test that valid ranges after a bad one are still parsed)
    #[test]
    fn test_codespace_range_bad_then_good() {
        let data = b"\
begincmap
2 begincodespacerange
<00> <FFFF>
<0000> <FFFF>
endcodespacerange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        // First range is mismatched (1 byte vs 2 bytes) and skipped.
        // Second range is valid (both 2 bytes).
        assert_eq!(cmap.codespace_ranges.len(), 1);
        assert_eq!(cmap.codespace_ranges[0].low, vec![0x00, 0x00]);
        assert_eq!(cmap.codespace_ranges[0].high, vec![0xFF, 0xFF]);
    }

    // 6. CMap with /WMode 1 -- verify is_vertical returns true
    #[test]
    fn test_wmode_1_with_extra_whitespace() {
        let data = b"\
begincmap
/WMode   1 def
1 begincodespacerange
<00> <FF>
endcodespacerange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        assert!(cmap.is_vertical());
    }

    #[test]
    fn test_wmode_1_at_end_of_stream() {
        // /WMode 1 with no trailing content
        let data = b"/WMode 1";
        let cmap = ParsedCMap::parse(data);
        assert!(cmap.is_vertical());
    }

    // 7. CMap with only cidchar (no bfchar/bfrange/cidrange) -- decode_string
    //    uses CID-to-Unicode identity heuristic
    #[test]
    fn test_cidchar_only_decode_string() {
        let data = b"\
begincmap
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
3 begincidchar
<0041> 65
<0042> 66
<0043> 67
endcidchar
endcmap
";
        let cmap = ParsedCMap::parse(data);
        // No bfchar or bfrange, so decode_string falls through to CID lookup.
        // CID 65 = 'A', CID 66 = 'B', CID 67 = 'C' via identity heuristic.
        assert_eq!(
            cmap.decode_string(&[0x00, 0x41, 0x00, 0x42, 0x00, 0x43]),
            "ABC"
        );
    }

    #[test]
    fn test_cidchar_only_control_char_becomes_replacement() {
        // CID mapping to a control character (not tab/newline/CR) should
        // produce U+FFFD in decode_string.
        let data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
1 begincidchar
<01> 1
endcidchar
endcmap
";
        let cmap = ParsedCMap::parse(data);
        // CID 1 is a control character (U+0001), so identity heuristic
        // produces U+FFFD
        assert_eq!(cmap.decode_string(&[0x01]), "\u{FFFD}");
    }

    // 8. Multiple codespacerange sections -- verify all are collected
    #[test]
    fn test_multiple_codespace_range_sections() {
        let data = b"\
begincmap
1 begincodespacerange
<00> <7F>
endcodespacerange
1 begincodespacerange
<8000> <FFFF>
endcodespacerange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        assert_eq!(cmap.codespace_ranges.len(), 2);
        // First section
        assert_eq!(cmap.codespace_ranges[0].low, vec![0x00]);
        assert_eq!(cmap.codespace_ranges[0].high, vec![0x7F]);
        // Second section
        assert_eq!(cmap.codespace_ranges[1].low, vec![0x80, 0x00]);
        assert_eq!(cmap.codespace_ranges[1].high, vec![0xFF, 0xFF]);
    }

    #[test]
    fn test_three_codespace_range_sections() {
        let data = b"\
begincmap
1 begincodespacerange
<00> <80>
endcodespacerange
1 begincodespacerange
<8140> <9FFC>
endcodespacerange
1 begincodespacerange
<E040> <FCFC>
endcodespacerange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        assert_eq!(cmap.codespace_ranges.len(), 3);
        assert_eq!(cmap.codespace_ranges[0].low, vec![0x00]);
        assert_eq!(cmap.codespace_ranges[1].low, vec![0x81, 0x40]);
        assert_eq!(cmap.codespace_ranges[2].low, vec![0xE0, 0x40]);
    }

    // 9. decode_string with empty byte array
    #[test]
    fn test_decode_string_empty_bytes_with_mappings() {
        // Even with mappings present, empty input produces empty output
        let data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
1 beginbfchar
<41> <0041>
endbfchar
endcmap
";
        let cmap = ParsedCMap::parse(data);
        assert_eq!(cmap.decode_string(&[]), "");
    }

    // 10. decode_string where some codes match unicode and others fall through to CID
    #[test]
    fn test_decode_string_mixed_unicode_and_cid_paths() {
        let data = b"\
begincmap
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
2 beginbfchar
<0041> <0048>
<0043> <004A>
endbfchar
1 begincidrange
<0040> <00FF> 64
endcidrange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        // <0041> -> unicode "H" (bfchar hit)
        // <0042> -> no bfchar, CID 66 via cidrange -> 'B' identity
        // <0043> -> unicode "J" (bfchar hit)
        // <0044> -> no bfchar, CID 68 via cidrange -> 'D' identity
        let result = cmap.decode_string(&[0x00, 0x41, 0x00, 0x42, 0x00, 0x43, 0x00, 0x44]);
        assert_eq!(result, "HBJD");
    }

    #[test]
    fn test_decode_string_unicode_then_unknown() {
        // Some codes have unicode, some have neither unicode nor CID
        let data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
1 beginbfchar
<41> <0041>
endbfchar
endcmap
";
        let cmap = ParsedCMap::parse(data);
        // <41> -> "A", <42> -> no mapping at all -> U+FFFD
        assert_eq!(cmap.decode_string(&[0x41, 0x42]), "A\u{FFFD}");
    }

    // 11. merge_base: verify child entries override base entries (exact collision)
    #[test]
    fn test_merge_base_child_cidchar_overrides_base() {
        let child_data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
1 begincidchar
<41> 100
endcidchar
endcmap
";
        let base_data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
1 begincidchar
<41> 999
endcidchar
endcmap
";
        let mut child = ParsedCMap::parse(child_data);
        let base = ParsedCMap::parse(base_data);
        child.merge_base(&base);

        // Child's <41> -> 100 should NOT be overridden by base's <41> -> 999
        assert_eq!(child.lookup_cid(&[0x41]), Some(100));
        // HashMap guarantees only one entry per key, so the base's <41> -> 999
        // was correctly rejected by entry().or_insert().
        assert_eq!(child.cid_chars.get(&vec![0x41]), Some(&100));
    }

    #[test]
    fn test_merge_base_base_bfchar_propagates() {
        // Test that base_bfchar entries from the base are carried into the child
        let mut child = ParsedCMap::parse(
            b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
endcmap
",
        );
        let mut base = ParsedCMap::parse(
            b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
endcmap
",
        );
        // Manually inject base_bfchar into the base (simulating a chain)
        base.base_bfchar.insert(vec![0x50], "Q".to_string());
        child.merge_base(&base);

        // Child should now be able to look up <50> via the inherited base_bfchar
        assert_eq!(child.lookup_unicode(&[0x50]), Some("Q".to_string()));
    }

    #[test]
    fn test_merge_base_child_base_bfchar_not_overridden() {
        // If the child already has a base_bfchar entry, the base's version
        // should NOT replace it
        let mut child = ParsedCMap::parse(
            b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
endcmap
",
        );
        child.base_bfchar.insert(vec![0x60], "CHILD".to_string());

        let mut base = ParsedCMap::parse(
            b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
endcmap
",
        );
        base.base_bfchar.insert(vec![0x60], "BASE".to_string());

        child.merge_base(&base);
        // Child's entry should persist
        assert_eq!(child.lookup_unicode(&[0x60]), Some("CHILD".to_string()));
    }

    #[test]
    fn test_merge_base_cidrange_appended() {
        // Base cidranges are appended (not deduplicated). Child ranges are
        // checked first in lookup, so child's CID takes priority.
        let child_data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
1 begincidrange
<10> <20> 100
endcidrange
endcmap
";
        let base_data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
1 begincidrange
<10> <20> 500
endcidrange
endcmap
";
        let mut child = ParsedCMap::parse(child_data);
        let base = ParsedCMap::parse(base_data);
        child.merge_base(&base);

        // Both ranges are present, but lookup finds child's first (index 0)
        assert_eq!(child.cid_ranges.len(), 2);
        // lookup_cid iterates in order, so child's range (cid_start=100) wins
        assert_eq!(child.lookup_cid(&[0x15]), Some(105));
    }

    // 12. parse_with_usecmap with "Identity-H" -- 2-byte codespace
    #[test]
    fn test_parse_with_usecmap_identity_h_codespace() {
        let data = b"\
/Identity-H usecmap
begincmap
/CMapName /TestChild def
endcmap
";
        let cmap = ParsedCMap::parse_with_usecmap(data, 0);
        // Identity-H stub should contribute a 2-byte codespace range
        let has_2byte = cmap.codespace_ranges.iter().any(|r| r.low.len() == 2);
        assert!(
            has_2byte,
            "expected a 2-byte codespace range from Identity-H stub"
        );

        // The Identity-H stub should also provide identity CID mapping
        // so code <0041> -> CID 0x0041 = 65
        assert_eq!(cmap.lookup_cid(&[0x00, 0x41]), Some(0x0041));

        // code_length_for should detect 2-byte codes
        assert_eq!(cmap.code_length_for(&[0x00, 0x41], 0), 2);
    }

    #[test]
    fn test_parse_with_usecmap_identity_v_is_vertical() {
        // Child without explicit /WMode inherits is_vertical from base.
        // Identity-V is a vertical CMap, so the child should be vertical.
        let data = b"\
/Identity-V usecmap
begincmap
endcmap
";
        let cmap = ParsedCMap::parse_with_usecmap(data, 0);
        assert!(cmap.is_vertical());
    }

    #[test]
    fn test_parse_with_usecmap_child_wmode_overrides_base() {
        // Child with explicit /WMode 0 should NOT inherit base's is_vertical.
        let data = b"\
/Identity-V usecmap
begincmap
/WMode 0 def
endcmap
";
        let cmap = ParsedCMap::parse_with_usecmap(data, 0);
        assert!(!cmap.is_vertical());
    }

    #[test]
    fn test_parse_with_usecmap_depth_limit() {
        // At depth >= MAX_USECMAP_DEPTH, /UseCMap should be ignored
        let data = b"\
/Identity-H usecmap
begincmap
endcmap
";
        let cmap = ParsedCMap::parse_with_usecmap(data, MAX_USECMAP_DEPTH);
        // No base merged, so no codespace ranges from Identity-H
        assert!(cmap.codespace_ranges.is_empty());
    }

    #[test]
    fn test_parse_with_usecmap_unknown_base() {
        // Unknown base CMap name should be silently ignored
        let data = b"\
/Nonexistent-CMap-XYZ usecmap
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
endcmap
";
        let cmap = ParsedCMap::parse_with_usecmap(data, 0);
        // Should have child's own range, nothing from base
        assert_eq!(cmap.codespace_ranges.len(), 1);
    }

    // Decode string edge case: truncated final code
    #[test]
    fn test_decode_string_truncated_final_code() {
        // 2-byte codespace but only 1 byte remaining at the end
        let data = b"\
begincmap
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
1 begincidrange
<0000> <FFFF> 0
endcidrange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        // 3 bytes: first 2 bytes form a code, last byte is truncated.
        // The truncated byte gets code_length_for -> 1 (no 1-byte codespace
        // range matches, so fallback 1). Then lookup_cid on a single byte
        // fails (cidrange is 2-byte), producing U+FFFD.
        let result = cmap.decode_string(&[0x00, 0x41, 0xFF]);
        assert_eq!(result, "A\u{FFFD}");
    }

    // Multiple cidrange sections (separate begin/end blocks)
    #[test]
    fn test_multiple_cidrange_sections() {
        let data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
1 begincidrange
<10> <1F> 100
endcidrange
1 begincidrange
<20> <2F> 200
endcidrange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        assert_eq!(cmap.cid_ranges.len(), 2);
        assert_eq!(cmap.lookup_cid(&[0x10]), Some(100));
        assert_eq!(cmap.lookup_cid(&[0x1F]), Some(115));
        assert_eq!(cmap.lookup_cid(&[0x20]), Some(200));
        assert_eq!(cmap.lookup_cid(&[0x2F]), Some(215));
    }

    // codespace_range_count accessor
    #[test]
    fn test_codespace_range_count() {
        let data = b"\
begincmap
2 begincodespacerange
<00> <80>
<8140> <9FFC>
endcodespacerange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        assert_eq!(cmap.codespace_range_count(), 2);
    }

    #[test]
    fn test_codespace_range_count_empty() {
        let cmap = ParsedCMap::parse(b"");
        assert_eq!(cmap.codespace_range_count(), 0);
    }

    // CID identity heuristic: tab/newline/CR pass through
    #[test]
    fn test_decode_string_cid_whitespace_passthrough() {
        let data = b"\
begincmap
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
1 begincidrange
<0000> <FFFF> 0
endcidrange
endcmap
";
        let cmap = ParsedCMap::parse(data);
        // CID 9 = tab, CID 10 = newline, CID 13 = CR
        assert_eq!(cmap.decode_string(&[0x00, 0x09]), "\t");
        assert_eq!(cmap.decode_string(&[0x00, 0x0A]), "\n");
        assert_eq!(cmap.decode_string(&[0x00, 0x0D]), "\r");
    }

    // Bytes_diff with 2-byte values
    #[test]
    fn test_bytes_diff_two_byte() {
        // 0x0100 - 0x00FF = 1
        assert_eq!(bytes_diff(&[0x00, 0xFF], &[0x01, 0x00]), 1);
        // 0x0200 - 0x0100 = 256
        assert_eq!(bytes_diff(&[0x01, 0x00], &[0x02, 0x00]), 256);
        // Same value
        assert_eq!(bytes_diff(&[0x41, 0x42], &[0x41, 0x42]), 0);
    }

    // extract_usecmap_name edge cases
    #[test]
    fn test_usecmap_empty_name() {
        // /  usecmap with no actual name after the slash
        let text = "/ usecmap\nbegincmap";
        assert_eq!(extract_usecmap_name(text), None);
    }

    #[test]
    fn test_usecmap_multiple_occurrences() {
        // Only the first usecmap is found by the current implementation
        let text = "/First-H usecmap\n/Second-V usecmap\nbegincmap";
        assert_eq!(extract_usecmap_name(text), Some("First-H".to_string()));
    }

    // Full integration: CMap with bfchar + bfrange + cidchar + cidrange
    #[test]
    fn test_full_integration_all_section_types() {
        let data = b"\
begincmap
/WMode 0 def
1 begincodespacerange
<00> <FF>
endcodespacerange
1 beginbfchar
<41> <0058>
endbfchar
1 beginbfrange
<50> <52> <0070>
endbfrange
2 begincidchar
<60> 100
<61> 101
endcidchar
1 begincidrange
<70> <7F> 200
endcidrange
endcmap
";
        let cmap = ParsedCMap::parse(data);

        // bfchar: <41> -> "X"
        assert_eq!(cmap.lookup_unicode(&[0x41]), Some("X".to_string()));
        // bfrange: <50> -> "p", <51> -> "q", <52> -> "r"
        assert_eq!(cmap.lookup_unicode(&[0x50]), Some("p".to_string()));
        assert_eq!(cmap.lookup_unicode(&[0x51]), Some("q".to_string()));
        assert_eq!(cmap.lookup_unicode(&[0x52]), Some("r".to_string()));
        // cidchar: <60> -> 100, <61> -> 101
        assert_eq!(cmap.lookup_cid(&[0x60]), Some(100));
        assert_eq!(cmap.lookup_cid(&[0x61]), Some(101));
        // cidrange: <70> -> 200, <75> -> 205
        assert_eq!(cmap.lookup_cid(&[0x70]), Some(200));
        assert_eq!(cmap.lookup_cid(&[0x75]), Some(205));

        // decode_string exercises all paths
        let result = cmap.decode_string(&[0x41, 0x50, 0x60, 0x70]);
        // <41> -> "X" (bfchar), <50> -> "p" (bfrange),
        // <60> -> CID 100 -> 'd' (identity), <70> -> CID 200 -> char(200)
        assert_eq!(&result[..2], "Xp");
        assert_eq!(result.chars().nth(2), Some('d')); // CID 100 = 'd'
    }
}
