//! Windows code page and charset to encoding mapping.
//!
//! Maps charset numbers and code page identifiers to `encoding_rs::Encoding`.
//! Used by RTF (`\fcharsetN`, `\ansicpgN`), DOC (BIFF/PLCF codepage fields),
//! and XLS (BIFF8 codepage records) backends.

use encoding_rs::Encoding;

/// Returns the `encoding_rs::Encoding` for a Windows charset number.
///
/// Charset numbers appear in RTF `\fcharsetN`, DOC font tables, and other
/// legacy Microsoft formats. Falls back to WINDOWS_1252 for unknown values.
pub fn encoding_for_charset(charset: u8) -> &'static Encoding {
    match charset {
        0 => encoding_rs::WINDOWS_1252, // ANSI
        1 => encoding_rs::WINDOWS_1252, // Default
        // Symbol fonts use a custom encoding that doesn't map to any standard
        // codepage. This approximation works for ASCII range but produces
        // incorrect characters for Symbol-specific glyphs (U+F020..U+F0FF).
        2 => encoding_rs::WINDOWS_1252,
        77 => encoding_rs::MACINTOSH,     // Mac Roman
        128 => encoding_rs::SHIFT_JIS,    // Japanese
        129 => encoding_rs::EUC_KR,       // Korean
        134 => encoding_rs::GBK,          // Simplified Chinese
        136 => encoding_rs::BIG5,         // Traditional Chinese
        161 => encoding_rs::WINDOWS_1253, // Greek
        162 => encoding_rs::WINDOWS_1254, // Turkish
        163 => encoding_rs::WINDOWS_1258, // Vietnamese
        177 => encoding_rs::WINDOWS_1255, // Hebrew
        178 => encoding_rs::WINDOWS_1256, // Arabic
        186 => encoding_rs::WINDOWS_1257, // Baltic
        204 => encoding_rs::WINDOWS_1251, // Cyrillic
        222 => encoding_rs::WINDOWS_874,  // Thai
        238 => encoding_rs::WINDOWS_1250, // Central European
        254 => encoding_rs::WINDOWS_1252, // OEM (CP437 not in encoding_rs)
        255 => encoding_rs::WINDOWS_1252, // OEM (approximate)
        _ => encoding_rs::WINDOWS_1252,   // Fallback
    }
}

/// Returns the `encoding_rs::Encoding` for a Windows code page number.
///
/// Code page numbers appear in RTF `\ansicpgN`, XLS BIFF8 codepage records,
/// DOC FIB fields, and other legacy Microsoft formats. Falls back to
/// WINDOWS_1252 for unknown code pages.
pub fn encoding_for_codepage(cpg: u16) -> &'static Encoding {
    match cpg {
        437 => encoding_rs::WINDOWS_1252, // CP437 has no encoding_rs equivalent; 1252 is approximate
        850 => encoding_rs::WINDOWS_1252, // CP850 has no encoding_rs equivalent; 1252 is approximate
        874 => encoding_rs::WINDOWS_874,
        932 => encoding_rs::SHIFT_JIS,
        936 => encoding_rs::GBK,
        949 => encoding_rs::EUC_KR,
        950 => encoding_rs::BIG5,
        1250 => encoding_rs::WINDOWS_1250,
        1251 => encoding_rs::WINDOWS_1251,
        1252 => encoding_rs::WINDOWS_1252,
        1253 => encoding_rs::WINDOWS_1253,
        1254 => encoding_rs::WINDOWS_1254,
        1255 => encoding_rs::WINDOWS_1255,
        1256 => encoding_rs::WINDOWS_1256,
        1257 => encoding_rs::WINDOWS_1257,
        1258 => encoding_rs::WINDOWS_1258,
        10000 => encoding_rs::MACINTOSH,
        65001 => encoding_rs::UTF_8,
        _ => encoding_rs::WINDOWS_1252,
    }
}

/// Returns true if the given code page has no exact `encoding_rs` mapping
/// and will use an approximate fallback.
pub fn is_approximate_codepage(cpg: u16) -> bool {
    matches!(cpg, 437 | 850)
}

/// Accumulates raw bytes and decodes them in batch via `encoding_rs`.
///
/// This handles multi-byte encodings (CJK double-byte) correctly because
/// encoding_rs processes the full byte sequence rather than byte-at-a-time.
pub struct CodepageDecoder {
    encoding: &'static Encoding,
    buf: Vec<u8>,
}

impl CodepageDecoder {
    pub fn new(encoding: &'static Encoding) -> Self {
        Self {
            encoding,
            buf: Vec::new(),
        }
    }

    /// Switch to a new encoding. Any unflushed bytes are silently dropped
    /// to avoid reinterpreting partial multi-byte sequences under the new
    /// encoding (which would produce garbage). Callers that want to keep
    /// the buffered bytes must call [`flush`](Self::flush) first.
    ///
    /// The previous implementation used a `debug_assert!` that callers
    /// flushed before switching; under adversarial input (RTF with mid-
    /// run `\pc`/`\mac`/`\ansi` while a multi-byte sequence is in flight)
    /// this aborted the process. finding: input
    /// `\xcf\x7b\\rtf.\\pc\xcb\\\xcb\r\x00ffr\xcb\r\x00` triggered the
    /// abort via `parser.rs::handle_control_word -> set_encoding`.
    pub fn set_encoding(&mut self, encoding: &'static Encoding) {
        // Drop any in-flight bytes rather than letting them be decoded
        // under the new encoding (which would silently produce wrong
        // characters). Real-world RTF that switches encodings mid-run
        // is already buggy; we just refuse to amplify the bug into
        // garbled text.
        self.buf.clear();
        self.encoding = encoding;
    }

    pub fn push_byte(&mut self, byte: u8) {
        self.buf.push(byte);
    }

    /// Decodes accumulated bytes using the current encoding, returns a UTF-8
    /// string, and clears the internal buffer.
    pub fn flush(&mut self) -> String {
        if self.buf.is_empty() {
            return String::new();
        }
        let (decoded, _, _) = self.encoding.decode(&self.buf);
        let result = decoded.into_owned();
        self.buf.clear();
        result
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

/// RTF-compatible alias for [`encoding_for_codepage`].
///
/// RTF uses `\ansicpgN` to specify the document code page, but the mapping
/// is identical to the general Windows code page table.
#[inline]
pub fn encoding_for_ansicpg(cpg: u16) -> &'static Encoding {
    encoding_for_codepage(cpg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charset_ansi() {
        assert_eq!(encoding_for_charset(0), encoding_rs::WINDOWS_1252);
    }

    #[test]
    fn charset_japanese() {
        assert_eq!(encoding_for_charset(128), encoding_rs::SHIFT_JIS);
    }

    #[test]
    fn charset_cyrillic() {
        assert_eq!(encoding_for_charset(204), encoding_rs::WINDOWS_1251);
    }

    #[test]
    fn charset_unknown_falls_back() {
        assert_eq!(encoding_for_charset(99), encoding_rs::WINDOWS_1252);
    }

    #[test]
    fn codepage_1252() {
        assert_eq!(encoding_for_codepage(1252), encoding_rs::WINDOWS_1252);
    }

    #[test]
    fn codepage_shift_jis() {
        assert_eq!(encoding_for_codepage(932), encoding_rs::SHIFT_JIS);
    }

    #[test]
    fn codepage_utf8() {
        assert_eq!(encoding_for_codepage(65001), encoding_rs::UTF_8);
    }

    #[test]
    fn codepage_unknown_falls_back() {
        assert_eq!(encoding_for_codepage(9999), encoding_rs::WINDOWS_1252);
    }

    #[test]
    fn ansicpg_alias() {
        // encoding_for_ansicpg is just an alias for encoding_for_codepage
        assert_eq!(encoding_for_ansicpg(932), encoding_for_codepage(932));
        assert_eq!(encoding_for_ansicpg(1252), encoding_for_codepage(1252));
    }

    #[test]
    fn decoder_single_byte() {
        let mut dec = CodepageDecoder::new(encoding_rs::WINDOWS_1252);
        dec.push_byte(b'H');
        dec.push_byte(b'i');
        assert_eq!(dec.flush(), "Hi");
    }

    #[test]
    fn decoder_flush_clears_buffer() {
        let mut dec = CodepageDecoder::new(encoding_rs::WINDOWS_1252);
        dec.push_byte(b'A');
        assert!(!dec.is_empty());
        let s = dec.flush();
        assert_eq!(s, "A");
        assert!(dec.is_empty());
        assert_eq!(dec.flush(), "");
    }

    #[test]
    fn decoder_shift_jis() {
        // Shift-JIS encoding of the katakana "a" (U+30A2): 0x83 0x41
        let mut dec = CodepageDecoder::new(encoding_rs::SHIFT_JIS);
        dec.push_byte(0x83);
        dec.push_byte(0x41);
        let s = dec.flush();
        assert_eq!(s, "\u{30A2}");
    }

    #[test]
    fn decoder_encoding_switch() {
        let mut dec = CodepageDecoder::new(encoding_rs::WINDOWS_1252);
        // Windows-1252: 0xE9 is "e with acute"
        dec.push_byte(0xE9);
        assert_eq!(dec.flush(), "\u{00E9}");

        // Switch to Windows-1251 (Cyrillic): 0xE9 is Cyrillic short I (U+0439)
        dec.set_encoding(encoding_rs::WINDOWS_1251);
        dec.push_byte(0xE9);
        assert_eq!(dec.flush(), "\u{0439}");
    }

    /// Regression: found that adversarial RTF with a mid-
    /// stream encoding switch (`\pc`/`\mac`/`\ansicpg`) while bytes
    /// were already buffered would trip a `debug_assert!` and abort
    /// the process. Now `set_encoding` defensively clears the buffer.
    #[test]
    fn decoder_set_encoding_with_unflushed_bytes_does_not_panic() {
        let mut dec = CodepageDecoder::new(encoding_rs::SHIFT_JIS);
        // Push a partial multi-byte Shift-JIS lead byte. In real RTF
        // this would be the first half of a CJK character.
        dec.push_byte(0x83);
        assert!(!dec.is_empty());

        // Switching encodings mid-sequence used to panic. Now it
        // silently drops the partial byte (preferable to silently
        // producing wrong characters under the new encoding).
        dec.set_encoding(encoding_rs::WINDOWS_1252);
        assert!(dec.is_empty());

        // Subsequent push/flush works normally under the new encoding.
        dec.push_byte(0xE9);
        assert_eq!(dec.flush(), "\u{00E9}");
    }
}
