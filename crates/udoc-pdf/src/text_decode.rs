//! Shared PDF text string decoding (PDFDocEncoding + UTF-16BE).
//!
//! PDF "text strings" (PDF spec 7.9.2.2) can be encoded as either:
//! - UTF-16BE with a BOM (0xFE 0xFF prefix)
//! - PDFDocEncoding (single-byte, per Table D.2)
//!
//! This module provides the canonical decoder used by both the content
//! interpreter (/ActualText) and the document layer (metadata, form fields).

/// Decode PDF text string bytes to Unicode.
///
/// Handles both UTF-16BE (with BOM) and PDFDocEncoding (Table D.2).
pub(crate) fn decode_pdf_text_bytes(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        // UTF-16BE with BOM
        let u16_iter = bytes[2..]
            .chunks(2)
            .filter(|chunk| chunk.len() == 2)
            .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]));
        char::decode_utf16(u16_iter)
            .map(|r| r.unwrap_or('\u{FFFD}'))
            .collect()
    } else {
        // PDFDocEncoding: mostly ASCII for 0x00-0x7F, but 0x18-0x1F differ
        // per PDF spec Table D.2. 0x80-0xFF mapped via PDFDOC_HIGH.
        let mut result = String::with_capacity(bytes.len());
        for &b in bytes {
            match b {
                // 0x18-0x1F: PDFDocEncoding differs from ASCII (Table D.2).
                0x18 => result.push('\u{02D8}'), // BREVE
                0x19 => result.push('\u{02C7}'), // CARON
                0x1A => result.push('\u{02C6}'), // MODIFIER LETTER CIRCUMFLEX ACCENT
                0x1B => result.push('\u{02D9}'), // DOT ABOVE
                0x1C => result.push('\u{02DD}'), // DOUBLE ACUTE ACCENT
                0x1D => result.push('\u{02DB}'), // OGONEK
                0x1E => result.push('\u{02DA}'), // RING ABOVE
                0x1F => result.push('\u{02DC}'), // SMALL TILDE
                // 0x00-0x07, 0x0E-0x17: UNDEFINED per Table D.2.
                0x00..=0x07 | 0x0E..=0x17 => result.push('\u{FFFD}'),
                0x7F => result.push('\u{FFFD}'), // UNDEFINED
                0x80.. => result.push(PDFDOC_HIGH[b as usize - 0x80]),
                _ => result.push(b as char), // 0x08-0x0D (BS.CR), 0x20-0x7E: same as ASCII
            }
        }
        result
    }
}

/// PDFDocEncoding mapping for bytes 0x80-0xFF (PDF spec Table D.2).
/// Undefined positions map to U+FFFD (replacement character).
#[rustfmt::skip]
static PDFDOC_HIGH: [char; 128] = [
    // 0x80-0x8F
    '\u{2022}', '\u{2020}', '\u{2021}', '\u{2026}',
    '\u{2014}', '\u{2013}', '\u{0192}', '\u{2044}',
    '\u{2039}', '\u{203A}', '\u{2212}', '\u{2030}',
    '\u{201E}', '\u{201C}', '\u{201D}', '\u{2018}',
    // 0x90-0x9F
    '\u{2019}', '\u{201A}', '\u{2122}', '\u{FB01}',
    '\u{FB02}', '\u{0141}', '\u{0152}', '\u{0160}',
    '\u{0178}', '\u{017D}', '\u{0131}', '\u{0142}',
    '\u{0153}', '\u{0161}', '\u{017E}', '\u{FFFD}',
    // 0xA0-0xAF: same as Unicode (Latin-1 Supplement)
    '\u{00A0}', '\u{00A1}', '\u{00A2}', '\u{00A3}',
    '\u{00A4}', '\u{00A5}', '\u{00A6}', '\u{00A7}',
    '\u{00A8}', '\u{00A9}', '\u{00AA}', '\u{00AB}',
    '\u{00AC}', '\u{00AD}', '\u{00AE}', '\u{00AF}',
    // 0xB0-0xBF
    '\u{00B0}', '\u{00B1}', '\u{00B2}', '\u{00B3}',
    '\u{00B4}', '\u{00B5}', '\u{00B6}', '\u{00B7}',
    '\u{00B8}', '\u{00B9}', '\u{00BA}', '\u{00BB}',
    '\u{00BC}', '\u{00BD}', '\u{00BE}', '\u{00BF}',
    // 0xC0-0xCF
    '\u{00C0}', '\u{00C1}', '\u{00C2}', '\u{00C3}',
    '\u{00C4}', '\u{00C5}', '\u{00C6}', '\u{00C7}',
    '\u{00C8}', '\u{00C9}', '\u{00CA}', '\u{00CB}',
    '\u{00CC}', '\u{00CD}', '\u{00CE}', '\u{00CF}',
    // 0xD0-0xDF
    '\u{00D0}', '\u{00D1}', '\u{00D2}', '\u{00D3}',
    '\u{00D4}', '\u{00D5}', '\u{00D6}', '\u{00D7}',
    '\u{00D8}', '\u{00D9}', '\u{00DA}', '\u{00DB}',
    '\u{00DC}', '\u{00DD}', '\u{00DE}', '\u{00DF}',
    // 0xE0-0xEF
    '\u{00E0}', '\u{00E1}', '\u{00E2}', '\u{00E3}',
    '\u{00E4}', '\u{00E5}', '\u{00E6}', '\u{00E7}',
    '\u{00E8}', '\u{00E9}', '\u{00EA}', '\u{00EB}',
    '\u{00EC}', '\u{00ED}', '\u{00EE}', '\u{00EF}',
    // 0xF0-0xFF
    '\u{00F0}', '\u{00F1}', '\u{00F2}', '\u{00F3}',
    '\u{00F4}', '\u{00F5}', '\u{00F6}', '\u{00F7}',
    '\u{00F8}', '\u{00F9}', '\u{00FA}', '\u{00FB}',
    '\u{00FC}', '\u{00FD}', '\u{00FE}', '\u{00FF}',
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_ascii() {
        assert_eq!(decode_pdf_text_bytes(b"hello"), "hello");
    }

    #[test]
    fn decode_utf16be_with_bom() {
        let bytes = [0xFE, 0xFF, 0x00, 0x41, 0x00, 0x42];
        assert_eq!(decode_pdf_text_bytes(&bytes), "AB");
    }

    #[test]
    fn decode_pdfdoc_low_range() {
        // Bytes 0x18-0x1F map to special Unicode characters per PDF spec Table D.2.
        let bytes = [0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F];
        let result = decode_pdf_text_bytes(&bytes);
        assert_eq!(
            result,
            "\u{02D8}\u{02C7}\u{02C6}\u{02D9}\u{02DD}\u{02DB}\u{02DA}\u{02DC}"
        );
    }

    #[test]
    fn decode_0x7f_undefined() {
        let bytes = [0x41, 0x7F, 0x42]; // A, DEL, B
        let result = decode_pdf_text_bytes(&bytes);
        assert_eq!(result, "A\u{FFFD}B");
    }

    #[test]
    fn decode_pdfdoc_high_range() {
        // 0x80 = BULLET (U+2022)
        let bytes = [0x80];
        assert_eq!(decode_pdf_text_bytes(&bytes), "\u{2022}");
    }

    #[test]
    fn decode_empty() {
        assert_eq!(decode_pdf_text_bytes(b""), "");
    }

    #[test]
    fn decode_utf16be_odd_trailing_byte() {
        // Trailing odd byte should be ignored (filtered by chunks(2) + len==2 check)
        let bytes = [0xFE, 0xFF, 0x00, 0x41, 0x00];
        assert_eq!(decode_pdf_text_bytes(&bytes), "A");
    }
}
