//! ToUnicode CMap parser.
//!
//! Parses the /ToUnicode stream associated with a PDF font to build
//! a mapping from character codes to Unicode strings.
//!
//! CMap format reference: Adobe Technical Note #5411, "ToUnicode Mapping
//! File Tutorial" and the CMap specification in the PDF Reference.

use ahash::AHashMap;

/// Maximum total entries (bfchar + bfrange) before truncation.
const MAX_TOUNICODE_ENTRIES: usize = 100_000;

/// One bfrange entry, with the base-Unicode char count cached.
///
/// `base_char_count` is `base_unicode.chars().count()`, computed at parse
/// time. Pre- the lookup hot path recomputed this UTF-8 walk on
/// every range comparison for every glyph (P02 flamegraph: ~0.7% of
/// total samples in `chars().count()` callees inside lookup).
#[derive(Debug)]
struct BfRangeEntry {
    start: Vec<u8>,
    end: Vec<u8>,
    base_unicode: String,
    base_char_count: usize,
}

/// Parsed ToUnicode CMap.
///
/// Maps character codes (byte sequences) to Unicode strings via
/// bfchar (exact) and bfrange (range) mappings.
#[derive(Debug)]
pub struct ToUnicodeCMap {
    /// Exact character mappings: source code bytes -> Unicode string.
    ///
    /// `AHashMap` (per-process seeded ahash) instead of default SipHash:
    /// the `Vec<u8>` keys come from PDF ToUnicode streams, which are
    /// 100% attacker-controlled. ahash is DOS-resistant where the
    /// `rustc_hash`/FxHash family is not.
    bfchar: AHashMap<Vec<u8>, String>,
    /// Range mappings.
    bfrange: Vec<BfRangeEntry>,
}

impl ToUnicodeCMap {
    /// Parse a ToUnicode CMap from decoded stream bytes.
    pub fn parse(data: &[u8]) -> Self {
        let mut cmap = Self {
            bfchar: AHashMap::new(),
            bfrange: Vec::new(),
        };
        let text = String::from_utf8_lossy(data);

        // Parse bfchar sections
        let mut rest = text.as_ref();
        while let Some(start) = rest.find("beginbfchar") {
            if cmap.total_mappings() >= MAX_TOUNICODE_ENTRIES {
                break;
            }
            let after_begin = &rest[start + "beginbfchar".len()..];
            let end = match after_begin.find("endbfchar") {
                Some(e) => e,
                None => break,
            };
            let section = &after_begin[..end];
            let remaining = MAX_TOUNICODE_ENTRIES - cmap.total_mappings();
            parse_bfchar_section(section, &mut cmap.bfchar, remaining);
            rest = &after_begin[end..];
        }

        // Parse bfrange sections
        rest = text.as_ref();
        while let Some(start) = rest.find("beginbfrange") {
            if cmap.total_mappings() >= MAX_TOUNICODE_ENTRIES {
                break;
            }
            let after_begin = &rest[start + "beginbfrange".len()..];
            let end = match after_begin.find("endbfrange") {
                Some(e) => e,
                None => break,
            };
            let section = &after_begin[..end];
            let remaining = MAX_TOUNICODE_ENTRIES - cmap.total_mappings();
            parse_bfrange_section(section, &mut cmap.bfrange, remaining);
            rest = &after_begin[end..];
        }

        cmap
    }

    /// Look up a character code, returning the Unicode string if mapped.
    ///
    /// Checks bfchar (exact match) first, then bfrange.
    pub fn lookup(&self, code: &[u8]) -> Option<String> {
        // Exact match
        if let Some(s) = self.bfchar.get(code) {
            return Some(s.clone());
        }
        // Range lookup. base_char_count is precomputed at parse time so
        // the per-glyph hot path doesn't UTF-8-walk the base string for
        // every range it tests ( hot spot).
        for entry in &self.bfrange {
            let start = entry.start.as_slice();
            let end = entry.end.as_slice();
            if code.len() == start.len() && code >= start && code <= end {
                let offset = bytes_diff(&entry.start, code);
                if entry.base_char_count > 1 {
                    // Multi-codepoint base (e.g., ligature "fi" = U+0066 U+0069).
                    // Per PDF spec, the offset applies to the last codepoint.
                    if offset == 0 {
                        return Some(entry.base_unicode.clone());
                    }
                    let mut chars: Vec<char> = entry.base_unicode.chars().collect();
                    if let Some(last) = chars.last_mut() {
                        if let Some(c) = char::from_u32(*last as u32 + offset) {
                            *last = c;
                            return Some(chars.into_iter().collect());
                        }
                    }
                } else if let Some(base_char) = entry.base_unicode.chars().next() {
                    let target_code = base_char as u32 + offset;
                    if let Some(c) = char::from_u32(target_code) {
                        return Some(c.to_string());
                    }
                }
            }
        }
        None
    }

    /// Total number of mappings (bfchar + bfrange entries).
    pub fn total_mappings(&self) -> usize {
        self.bfchar.len() + self.bfrange.len()
    }
}

/// Parse a bfchar section into the mapping (up to `max_entries` new entries).
fn parse_bfchar_section(section: &str, map: &mut AHashMap<Vec<u8>, String>, max_entries: usize) {
    let mut chars = section.chars().peekable();
    let mut added = 0;
    while let Some(src) = next_hex_token(&mut chars) {
        if added >= max_entries {
            break;
        }
        let dst = match next_hex_token(&mut chars) {
            Some(h) => h,
            None => break,
        };
        let unicode = hex_to_unicode_string(&dst);
        map.insert(src, unicode);
        added += 1;
    }
}

/// Parse a bfrange section into the range list (up to `max_entries` new entries).
fn parse_bfrange_section(section: &str, ranges: &mut Vec<BfRangeEntry>, max_entries: usize) {
    let mut chars = section.chars().peekable();
    let mut added = 0;
    while let Some(start) = next_hex_token(&mut chars) {
        if added >= max_entries {
            break;
        }
        let end = match next_hex_token(&mut chars) {
            Some(h) => h,
            None => break,
        };
        let base = match next_hex_token(&mut chars) {
            Some(h) => h,
            None => break,
        };
        let base_unicode = hex_to_unicode_string(&base);
        let base_char_count = base_unicode.chars().count();
        ranges.push(BfRangeEntry {
            start,
            end,
            base_unicode,
            base_char_count,
        });
        added += 1;
    }
}

/// Extract the next `<hex>` token, returning decoded bytes.
///
/// Hex tokens in CMap/ToUnicode streams encode character codes (typically
/// 1-4 bytes). We cap at 64 hex digits (32 bytes) to prevent OOM from
/// maliciously crafted streams with unbounded hex content.
///
/// Decode happens as digits arrive: two nibbles per byte, no intermediate
/// String. Pre- the function allocated a hex String of digits then
/// re-parsed it via `u8::from_str_radix`; that showed up at ~1% of total
/// samples in the extraction flamegraph (every CMap entry in every font
/// in every PDF page hits this).
pub(super) fn next_hex_token(chars: &mut std::iter::Peekable<std::str::Chars>) -> Option<Vec<u8>> {
    // Max hex digits per token. 64 digits = 32 bytes, far beyond any
    // real CMap code (typically 2-4 bytes). Prevents OOM on malicious input.
    const MAX_HEX_DIGITS: usize = 64;

    // Skip to next '<'
    loop {
        match chars.peek() {
            Some('<') => {
                chars.next();
                break;
            }
            Some(_) => {
                chars.next();
            }
            None => return None,
        }
    }
    // Read hex digits until '>' (capped at MAX_HEX_DIGITS).
    // Accumulate bytes inline: pair adjacent nibbles, no String allocation.
    let mut bytes: Vec<u8> = Vec::with_capacity(4);
    let mut digits_read: usize = 0;
    let mut high_nibble: Option<u8> = None;
    loop {
        match chars.next() {
            Some('>') => break,
            Some(c) if c.is_ascii_hexdigit() => {
                if digits_read >= MAX_HEX_DIGITS {
                    // Beyond cap: keep consuming to find '>', but don't store.
                    continue;
                }
                let nibble = hex_nibble(c);
                digits_read += 1;
                match high_nibble.take() {
                    None => high_nibble = Some(nibble),
                    Some(hi) => bytes.push((hi << 4) | nibble),
                }
            }
            Some(_) => {} // skip whitespace inside hex token
            None => break,
        }
    }
    // Odd trailing nibble: per the original behaviour, treat as X0 (left-pad).
    if let Some(hi) = high_nibble {
        bytes.push(hi << 4);
    }
    Some(bytes)
}

#[inline(always)]
fn hex_nibble(c: char) -> u8 {
    // Caller guarantees c is an ASCII hex digit.
    match c {
        '0'..='9' => (c as u8) - b'0',
        'a'..='f' => (c as u8) - b'a' + 10,
        'A'..='F' => (c as u8) - b'A' + 10,
        _ => 0,
    }
}

/// Convert raw bytes (big-endian UTF-16BE) to a Unicode string.
///
/// Handles BMP characters (2 bytes) and surrogate pairs (4 bytes)
/// for supplementary plane characters.
pub(super) fn hex_to_unicode_string(bytes: &[u8]) -> String {
    let mut result = String::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        let high = u16::from_be_bytes([bytes[i], bytes[i + 1]]);
        // Check for surrogate pair
        if (0xD800..=0xDBFF).contains(&high) && i + 3 < bytes.len() {
            let low = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]);
            if (0xDC00..=0xDFFF).contains(&low) {
                let code_point = 0x10000 + ((high as u32 - 0xD800) << 10) + (low as u32 - 0xDC00);
                if let Some(c) = char::from_u32(code_point) {
                    result.push(c);
                }
                i += 4;
                continue;
            }
        }
        if let Some(c) = char::from_u32(high as u32) {
            result.push(c);
        }
        i += 2;
    }
    result
}

/// Compute the integer difference between two byte sequences of equal length.
///
/// Precondition: `code >= start` byte-wise (as big-endian unsigned integers).
/// Caller (bfrange lookup) guarantees this via the range bounds check.
pub(super) fn bytes_diff(start: &[u8], code: &[u8]) -> u32 {
    debug_assert!(
        code >= start,
        "bytes_diff called with code < start: {:?} < {:?}",
        code,
        start
    );
    let mut diff: u32 = 0;
    for (s, c) in start.iter().zip(code.iter()) {
        diff = diff
            .wrapping_mul(256)
            .wrapping_add((*c as u32).wrapping_sub(*s as u32));
    }
    diff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_bfchar() {
        let cmap_data = b"\
/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
2 beginbfchar
<0041> <0041>
<0042> <0042>
endbfchar
endcmap
";
        let cmap = ToUnicodeCMap::parse(cmap_data);
        assert_eq!(cmap.bfchar.len(), 2);
        assert_eq!(cmap.lookup(&[0x00, 0x41]), Some("A".to_string()));
        assert_eq!(cmap.lookup(&[0x00, 0x42]), Some("B".to_string()));
    }

    #[test]
    fn test_parse_bfrange() {
        let cmap_data = b"\
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
1 beginbfrange
<43> <45> <0043>
endbfrange
endcmap
";
        let cmap = ToUnicodeCMap::parse(cmap_data);
        assert_eq!(cmap.bfrange.len(), 1);
        assert_eq!(cmap.lookup(&[0x43]), Some("C".to_string()));
        assert_eq!(cmap.lookup(&[0x44]), Some("D".to_string()));
        assert_eq!(cmap.lookup(&[0x45]), Some("E".to_string()));
        assert_eq!(cmap.lookup(&[0x46]), None);
    }

    #[test]
    fn test_surrogate_pair() {
        // U+1F600 (grinning face) = D83D DE00 in UTF-16
        let bytes = vec![0xD8, 0x3D, 0xDE, 0x00];
        let s = hex_to_unicode_string(&bytes);
        assert_eq!(s, "\u{1F600}");
    }

    #[test]
    fn test_total_mappings() {
        let cmap_data = b"\
begincmap
2 beginbfchar
<01> <0041>
<02> <0042>
endbfchar
1 beginbfrange
<10> <12> <0050>
endbfrange
endcmap
";
        let cmap = ToUnicodeCMap::parse(cmap_data);
        assert_eq!(cmap.total_mappings(), 3);
    }

    #[test]
    fn test_tounicode_entry_limit() {
        // Build a CMap with MAX_TOUNICODE_ENTRIES + 100 bfchar entries.
        // The parser should cap at MAX_TOUNICODE_ENTRIES.
        let mut cmap_text = String::from("begincmap\n");

        // Write entries in blocks of 100 to avoid huge beginbfchar counts
        let total = super::MAX_TOUNICODE_ENTRIES + 100;
        let block_size = 100;
        let mut written = 0;
        while written < total {
            let this_block = block_size.min(total - written);
            cmap_text.push_str(&format!("{} beginbfchar\n", this_block));
            for j in 0..this_block {
                let code = written + j;
                // Use 3-byte codes to avoid collisions
                cmap_text.push_str(&format!("<{:06X}> <{:04X}>\n", code, (code % 0xFFFF) + 1));
            }
            cmap_text.push_str("endbfchar\n");
            written += this_block;
        }
        cmap_text.push_str("endcmap\n");

        let cmap = ToUnicodeCMap::parse(cmap_text.as_bytes());
        assert!(
            cmap.total_mappings() <= super::MAX_TOUNICODE_ENTRIES,
            "got {} mappings, expected <= {}",
            cmap.total_mappings(),
            super::MAX_TOUNICODE_ENTRIES
        );
    }
}
