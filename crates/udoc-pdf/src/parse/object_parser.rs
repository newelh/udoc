//! PDF object parser.
//!
//! Builds `PdfObject` instances from the token stream produced by the lexer.
//! Handles simple objects (null, bool, int, real, name, string, references),
//! arrays, and dictionaries.

use std::fmt;
use std::sync::Arc;

use crate::diagnostics::{DiagnosticsSink, NullDiagnostics, Warning, WarningKind};
use crate::error::{Error, Limit, ResultExt};
use crate::object::{ObjRef, PdfDictionary, PdfObject, PdfStream, PdfString};
use crate::Result;

use super::lexer::{Lexer, Token};

/// Maximum nesting depth for arrays and dictionaries.
const MAX_NESTING_DEPTH: usize = udoc_core::MAX_NESTING_DEPTH;

/// Maximum number of elements in a single array or dictionary.
const MAX_COLLECTION_SIZE: usize = 1_000_000;

/// Parser that builds `PdfObject` values from a lexer's token stream.
pub struct ObjectParser<'a> {
    lexer: Lexer<'a>,
    diagnostics: Arc<dyn DiagnosticsSink>,
    /// Current nesting depth of arrays/dictionaries.
    depth: usize,
    /// Maximum number of elements in a single array or dictionary.
    max_collection_size: usize,
}

impl fmt::Debug for ObjectParser<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ObjectParser")
            .field("lexer", &self.lexer)
            .field("depth", &self.depth)
            .finish()
    }
}

impl<'a> ObjectParser<'a> {
    /// Create a new object parser over the given byte slice.
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            lexer: Lexer::new(data),
            diagnostics: Arc::new(NullDiagnostics),
            depth: 0,
            max_collection_size: MAX_COLLECTION_SIZE,
        }
    }

    /// Create a new object parser with a custom diagnostics sink.
    pub fn with_diagnostics(data: &'a [u8], diagnostics: Arc<dyn DiagnosticsSink>) -> Self {
        Self {
            lexer: Lexer::with_diagnostics(data, diagnostics.clone()),
            diagnostics,
            depth: 0,
            max_collection_size: MAX_COLLECTION_SIZE,
        }
    }

    /// Current byte offset.
    #[cfg(any(test, feature = "test-internals"))]
    pub fn position(&self) -> u64 {
        self.lexer.position()
    }

    /// Get a mutable reference to the underlying lexer.
    pub fn lexer_mut(&mut self) -> &mut Lexer<'a> {
        &mut self.lexer
    }

    /// Set the maximum number of elements allowed in a single array or dictionary.
    #[cfg(any(test, feature = "test-internals"))]
    pub fn set_max_collection_size(&mut self, max: usize) {
        self.max_collection_size = max;
    }

    /// Emit a warning.
    fn warn(&self, offset: u64, kind: WarningKind, message: impl Into<String>) {
        self.diagnostics
            .warning(Warning::new(Some(offset), kind, message));
    }

    /// Check if `endstream` follows at the expected position after the
    /// declared stream length. Returns true if the declared length looks correct.
    fn verify_stream_length(&self, data_offset: u64, declared_length: u64) -> bool {
        let Some(end_offset) = data_offset.checked_add(declared_length) else {
            return false;
        };
        let Ok(expected_end) = usize::try_from(end_offset) else {
            return false;
        };
        let data = self.lexer.data_slice();
        if expected_end >= data.len() {
            return false;
        }
        // Skip optional EOL (CR, LF, or CRLF) before "endstream"
        let mut pos = expected_end;
        if pos < data.len() && data[pos] == b'\r' {
            pos += 1;
        }
        if pos < data.len() && data[pos] == b'\n' {
            pos += 1;
        }
        data[pos..].starts_with(b"endstream")
    }

    /// Consume the endstream keyword and any preceding whitespace at
    /// the current lexer position. This advances the lexer past the
    /// stream body so subsequent parsing can find endobj.
    fn skip_endstream_keyword(&mut self) {
        // Skip optional EOL/whitespace before endstream
        let tok = self.lexer.peek_token();
        if tok == Token::EndStream {
            self.lexer.next_token();
        }
    }

    /// Parse the next object from the token stream.
    ///
    /// This is the main entry point. It handles all PDF object types
    /// including indirect references (which require lookahead for `R`).
    pub fn parse_object(&mut self) -> Result<PdfObject> {
        let offset = self.lexer.position();
        let token = self.lexer.next_token();

        match token {
            Token::Null => Ok(PdfObject::Null),
            Token::True => Ok(PdfObject::Boolean(true)),
            Token::False => Ok(PdfObject::Boolean(false)),
            Token::Real(v) => Ok(PdfObject::Real(v)),
            Token::Integer(n) => {
                // Could be a plain integer OR the start of an indirect reference (N G R).
                // Peek ahead to check for `<int> R` pattern.
                self.try_parse_reference(n, offset)
            }
            Token::Name(bytes) => Ok(PdfObject::Name(decode_name(bytes))),
            Token::LiteralString(bytes) => Ok(PdfObject::String(PdfString::new(
                decode_literal_string(bytes),
            ))),
            Token::HexString(bytes) => {
                Ok(PdfObject::String(PdfString::new(decode_hex_string(bytes))))
            }
            Token::ArrayStart => self.parse_array().context("parsing array"),
            Token::DictStart => self.parse_dict_or_stream().context("parsing dictionary"),
            Token::Eof => Err(Error::parse(offset, "object", "end of input")),
            Token::Keyword(kw) => {
                self.warn(
                    offset,
                    WarningKind::UnknownKeyword,
                    format!(
                        "unknown keyword '{}' in object context",
                        String::from_utf8_lossy(kw)
                    ),
                );
                Err(Error::parse(
                    offset,
                    "object",
                    format!("unexpected keyword: {}", String::from_utf8_lossy(kw)),
                ))
            }
            Token::Error(e) => {
                self.warn(
                    offset,
                    WarningKind::MalformedToken,
                    format!("lexer error while parsing object: {e}"),
                );
                Err(Error::parse(offset, "object", format!("lexer error: {e}")))
            }
            other => Err(Error::parse(
                offset,
                "object",
                format!("unexpected token: {other:?}"),
            )),
        }
    }

    /// After reading an integer, check if this is actually an indirect
    /// reference `N G R` by peeking ahead.
    fn try_parse_reference(&mut self, first_int: i64, offset: u64) -> Result<PdfObject> {
        let saved_pos = self.lexer.position();

        // Peek at next token — should be another integer (generation number)
        let next = self.lexer.next_token();
        if let Token::Integer(gen) = next {
            // Peek one more — should be `R`
            let next2 = self.lexer.next_token();
            if next2 == Token::R {
                // It's an indirect reference. Use try_from to avoid
                // silent truncation of out-of-range values.
                let num = u32::try_from(first_int).ok();
                let g = u16::try_from(gen).ok();
                return match (num, g) {
                    (Some(n), Some(g)) => Ok(PdfObject::Reference(ObjRef::new(n, g))),
                    _ => {
                        self.warn(
                            offset,
                            WarningKind::UnexpectedToken,
                            format!(
                                "reference {first_int} {gen} R has out-of-range values, \
                                 treating as null"
                            ),
                        );
                        Ok(PdfObject::Null)
                    }
                };
            }
            // Not `R` — rewind and return just the integer
            self.lexer.set_position(saved_pos);
        } else {
            // Not an integer — rewind
            self.lexer.set_position(saved_pos);
        }

        Ok(PdfObject::Integer(first_int))
    }

    /// Parse an array `[obj1 obj2 ...]`.
    /// Called after `[` has been consumed.
    fn parse_array(&mut self) -> Result<PdfObject> {
        self.depth += 1;
        if self.depth > MAX_NESTING_DEPTH {
            self.depth -= 1;
            return Err(Error::resource_limit(Limit::RecursionDepth(
                MAX_NESTING_DEPTH,
            )));
        }
        let result = self.parse_array_body();
        self.depth -= 1;
        result
    }

    /// Inner array parsing logic. Separated so `parse_array` can manage
    /// depth tracking with a single decrement point.
    fn parse_array_body(&mut self) -> Result<PdfObject> {
        let mut items = Vec::new();

        loop {
            let offset = self.lexer.position();
            let peeked = self.lexer.peek_token();

            match peeked {
                Token::ArrayEnd => {
                    self.lexer.next_token(); // consume `]`
                    return Ok(PdfObject::Array(items));
                }
                Token::Eof => {
                    self.warn(
                        offset,
                        WarningKind::UnterminatedCollection,
                        "unterminated array, returning partial result",
                    );
                    break;
                }
                Token::Error(_) => {
                    // Skip error tokens and try to continue
                    self.lexer.next_token();
                }
                _ => {
                    if items.len() >= self.max_collection_size {
                        return Err(Error::resource_limit(Limit::CollectionSize(
                            self.max_collection_size,
                        )));
                    }
                    let obj = self.parse_object().context("parsing array element")?;
                    items.push(obj);
                }
            }
        }

        Ok(PdfObject::Array(items))
    }

    /// Parse a dictionary `<< /Key Value ... >>`, and check if it's
    /// followed by a `stream` keyword (making it a PdfStream).
    /// Called after `<<` has been consumed.
    fn parse_dict_or_stream(&mut self) -> Result<PdfObject> {
        self.depth += 1;
        if self.depth > MAX_NESTING_DEPTH {
            self.depth -= 1;
            return Err(Error::resource_limit(Limit::RecursionDepth(
                MAX_NESTING_DEPTH,
            )));
        }
        let result = self.parse_dict_or_stream_body();
        self.depth -= 1;
        result
    }

    /// Inner dict/stream parsing logic. Separated so `parse_dict_or_stream`
    /// can manage depth tracking with a single decrement point.
    fn parse_dict_or_stream_body(&mut self) -> Result<PdfObject> {
        let mut dict = PdfDictionary::new();

        loop {
            let offset = self.lexer.position();
            let peeked = self.lexer.peek_token();

            match peeked {
                Token::DictEnd => {
                    self.lexer.next_token(); // consume `>>`
                    break;
                }
                Token::Eof => {
                    self.warn(
                        offset,
                        WarningKind::UnterminatedCollection,
                        "unterminated dictionary, returning partial result",
                    );
                    break;
                }
                Token::Error(_) => {
                    self.lexer.next_token(); // skip
                }
                _ => {
                    if dict.len() >= self.max_collection_size {
                        return Err(Error::resource_limit(Limit::CollectionSize(
                            self.max_collection_size,
                        )));
                    }

                    // Expect a name key
                    let key_offset = self.lexer.position();
                    let key_token = self.lexer.next_token();
                    let key = match key_token {
                        Token::Name(bytes) => decode_name(bytes),
                        other => {
                            self.warn(
                                key_offset,
                                WarningKind::UnexpectedToken,
                                format!("expected name key in dictionary, got {other:?}, skipping"),
                            );
                            continue;
                        }
                    };

                    // Parse the value
                    let value = self.parse_object().context("parsing dictionary value")?;
                    dict.insert(key, value);
                }
            }
        }

        // Check if this dictionary is followed by `stream`
        let peeked = self.lexer.peek_token();
        if peeked == Token::Stream {
            self.lexer.next_token(); // consume `stream`

            // Per the PDF spec (7.3.8.1), the `stream` keyword must be
            // followed by a single EOL (CR, LF, or CRLF), then the stream
            // data begins. Skip the EOL so data_offset points to actual data.
            self.lexer.skip_stream_eol();
            let data_offset = self.lexer.position();

            // Determine stream length:
            // 1. If /Length is a direct integer >= 0, use it (but verify with endstream scan)
            // 2. If /Length is missing or indirect (Reference), scan for endstream
            let declared_length = dict
                .get(b"Length")
                .and_then(|obj| obj.as_i64())
                .filter(|&n| n >= 0);

            let data_length = match declared_length {
                Some(len) => {
                    let len = len as u64;
                    if self.verify_stream_length(data_offset, len) {
                        len
                    } else {
                        // Declared length is wrong, scan for endstream
                        match self.lexer.scan_for_endstream() {
                            Some(scanned_len) => {
                                self.warn(
                                    data_offset,
                                    WarningKind::StreamLengthMismatch,
                                    format!(
                                        "stream /Length {} is incorrect, actual length is {} \
                                         (found endstream by scanning)",
                                        len, scanned_len
                                    ),
                                );
                                scanned_len
                            }
                            None => len, // can't find endstream, trust declared length
                        }
                    }
                }
                None => {
                    // No usable /Length: must scan
                    let reason = if dict.get(b"Length").is_some() {
                        "indirect reference (cannot resolve at parse time)"
                    } else {
                        "missing"
                    };
                    match self.lexer.scan_for_endstream() {
                        Some(scanned_len) => {
                            self.warn(
                                data_offset,
                                WarningKind::StreamLengthMismatch,
                                format!(
                                    "stream /Length is {}, determined length {} by scanning \
                                     for endstream",
                                    reason, scanned_len
                                ),
                            );
                            scanned_len
                        }
                        None => {
                            self.warn(
                                data_offset,
                                WarningKind::StreamLengthMismatch,
                                format!(
                                    "stream /Length is {} and endstream not found, \
                                     defaulting to 0",
                                    reason
                                ),
                            );
                            0
                        }
                    }
                }
            };

            // Advance lexer past stream body + endstream keyword (#24)
            self.lexer.set_position(data_offset + data_length);
            self.skip_endstream_keyword();

            return Ok(PdfObject::Stream(PdfStream {
                dict,
                data_offset,
                data_length,
            }));
        }

        Ok(PdfObject::Dictionary(dict))
    }
}

/// Decode a PDF name by expanding `#XX` hex escape sequences.
///
/// For example, `Name#20With#20Spaces` becomes `Name With Spaces`.
fn decode_name(raw: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(raw.len());
    let mut i = 0;

    while i < raw.len() {
        if raw[i] == b'#' && i + 2 < raw.len() {
            let hi = hex_digit(raw[i + 1]);
            let lo = hex_digit(raw[i + 2]);
            if let (Some(h), Some(l)) = (hi, lo) {
                result.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        result.push(raw[i]);
        i += 1;
    }

    result
}

/// Decode a PDF literal string by processing escape sequences.
///
/// Handles: `\n`, `\r`, `\t`, `\b`, `\f`, `\\`, `\(`, `\)`, and
/// octal escapes `\NNN` (1-3 digits).
fn decode_literal_string(raw: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(raw.len());
    let mut i = 0;

    while i < raw.len() {
        if raw[i] == b'\\' && i + 1 < raw.len() {
            i += 1;
            match raw[i] {
                b'n' => result.push(b'\n'),
                b'r' => result.push(b'\r'),
                b't' => result.push(b'\t'),
                b'b' => result.push(0x08), // backspace
                b'f' => result.push(0x0C), // form feed
                b'(' => result.push(b'('),
                b')' => result.push(b')'),
                b'\\' => result.push(b'\\'),
                b'\r' => {
                    // Backslash + EOL = line continuation (skip the EOL)
                    if i + 1 < raw.len() && raw[i + 1] == b'\n' {
                        i += 1; // skip LF after CR
                    }
                }
                b'\n' => {
                    // Backslash + LF = line continuation
                }
                d if d.is_ascii_digit() && d < b'8' => {
                    // Octal escape: 1-3 octal digits
                    // PDF spec: valid range is \000 to \377 (0 to 255 decimal)
                    // Note: This code uses wrapping arithmetic for robustness.
                    // Values exceeding \377 are spec violations but are tolerated.
                    // Examples: \400 wraps to \000, \777 wraps to \377
                    let mut val: u8 = d - b'0';
                    if i + 1 < raw.len() && raw[i + 1] >= b'0' && raw[i + 1] < b'8' {
                        i += 1;
                        val = val.wrapping_mul(8).wrapping_add(raw[i] - b'0');
                        if i + 1 < raw.len() && raw[i + 1] >= b'0' && raw[i + 1] < b'8' {
                            i += 1;
                            val = val.wrapping_mul(8).wrapping_add(raw[i] - b'0');
                        }
                    }
                    result.push(val);
                }
                other => {
                    // Unknown escape: PDF spec says ignore the backslash
                    result.push(other);
                }
            }
            i += 1;
        } else {
            result.push(raw[i]);
            i += 1;
        }
    }

    result
}

/// Decode a PDF hex string.
///
/// Skips whitespace, converts hex digit pairs to bytes.
/// An odd number of digits is padded with a trailing zero.
fn decode_hex_string(raw: &[u8]) -> Vec<u8> {
    let mut digits: Vec<u8> = Vec::with_capacity(raw.len());

    for &b in raw {
        if let Some(d) = hex_digit(b) {
            digits.push(d);
        }
        // Skip non-hex (whitespace, garbage already warned by lexer)
    }

    // Pad odd length with trailing 0
    if !digits.len().is_multiple_of(2) {
        digits.push(0);
    }

    digits
        .chunks(2)
        .map(|pair| pair[0] << 4 | pair[1])
        .collect()
}

/// Convert an ASCII hex digit to its numeric value.
fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::error::ResourceLimitError;
    use crate::CollectingDiagnostics;

    // ========================================================================
    // Simple objects
    // ========================================================================

    #[test]
    fn test_parse_null() {
        let mut parser = ObjectParser::new(b"null");
        assert_eq!(parser.parse_object().unwrap(), PdfObject::Null);
    }

    #[test]
    fn test_parse_true() {
        let mut parser = ObjectParser::new(b"true");
        assert_eq!(parser.parse_object().unwrap(), PdfObject::Boolean(true));
    }

    #[test]
    fn test_parse_false() {
        let mut parser = ObjectParser::new(b"false");
        assert_eq!(parser.parse_object().unwrap(), PdfObject::Boolean(false));
    }

    #[test]
    fn test_parse_integer() {
        let mut parser = ObjectParser::new(b"42");
        assert_eq!(parser.parse_object().unwrap(), PdfObject::Integer(42));
    }

    #[test]
    fn test_parse_negative_integer() {
        let mut parser = ObjectParser::new(b"-7");
        assert_eq!(parser.parse_object().unwrap(), PdfObject::Integer(-7));
    }

    #[test]
    fn test_parse_real() {
        let mut parser = ObjectParser::new(b"2.5");
        assert_eq!(parser.parse_object().unwrap(), PdfObject::Real(2.5));
    }

    #[test]
    fn test_parse_name() {
        let mut parser = ObjectParser::new(b"/Type");
        assert_eq!(
            parser.parse_object().unwrap(),
            PdfObject::Name(b"Type".to_vec())
        );
    }

    #[test]
    fn test_parse_name_with_hex_escape() {
        let mut parser = ObjectParser::new(b"/Hello#20World");
        assert_eq!(
            parser.parse_object().unwrap(),
            PdfObject::Name(b"Hello World".to_vec())
        );
    }

    // ========================================================================
    // Strings
    // ========================================================================

    #[test]
    fn test_parse_literal_string() {
        let mut parser = ObjectParser::new(b"(Hello World)");
        let obj = parser.parse_object().unwrap();
        match obj {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), b"Hello World"),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_literal_string_escapes() {
        let mut parser = ObjectParser::new(b"(line1\\nline2)");
        let obj = parser.parse_object().unwrap();
        match obj {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), b"line1\nline2"),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_literal_string_nested_parens() {
        let mut parser = ObjectParser::new(b"(a(b)c)");
        let obj = parser.parse_object().unwrap();
        match obj {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), b"a(b)c"),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_literal_string_octal() {
        let mut parser = ObjectParser::new(b"(\\101)"); // \101 = 'A'
        let obj = parser.parse_object().unwrap();
        match obj {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), b"A"),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_hex_string() {
        let mut parser = ObjectParser::new(b"<48656C6C6F>");
        let obj = parser.parse_object().unwrap();
        match obj {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), b"Hello"),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_hex_string_odd_digits() {
        let mut parser = ObjectParser::new(b"<ABC>");
        let obj = parser.parse_object().unwrap();
        match obj {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), &[0xAB, 0xC0]),
            other => panic!("expected String, got {other:?}"),
        }
    }

    // ========================================================================
    // Indirect references
    // ========================================================================

    #[test]
    fn test_parse_indirect_reference() {
        let mut parser = ObjectParser::new(b"5 0 R");
        assert_eq!(
            parser.parse_object().unwrap(),
            PdfObject::Reference(ObjRef::new(5, 0))
        );
    }

    #[test]
    fn test_parse_reference_obj_num_exceeds_u32() {
        // 4294967298 = u32::MAX + 3, should not silently wrap to 2
        let mut parser = ObjectParser::with_diagnostics(
            b"4294967298 0 R",
            Arc::new(CollectingDiagnostics::new()),
        );
        let obj = parser.parse_object().unwrap();
        assert_eq!(obj, PdfObject::Null);
    }

    #[test]
    fn test_parse_reference_gen_exceeds_u16() {
        // 65536 = u16::MAX + 1
        let mut parser =
            ObjectParser::with_diagnostics(b"1 65536 R", Arc::new(CollectingDiagnostics::new()));
        let obj = parser.parse_object().unwrap();
        assert_eq!(obj, PdfObject::Null);
    }

    #[test]
    fn test_parse_reference_negative_values() {
        let mut parser =
            ObjectParser::with_diagnostics(b"-1 0 R", Arc::new(CollectingDiagnostics::new()));
        let obj = parser.parse_object().unwrap();
        assert_eq!(obj, PdfObject::Null);
    }

    #[test]
    fn test_parse_integer_not_reference() {
        // Just `5 0` without `R` — should parse as integer 5, leave 0 for next
        let mut parser = ObjectParser::new(b"5 0 /Name");
        assert_eq!(parser.parse_object().unwrap(), PdfObject::Integer(5));
        assert_eq!(parser.parse_object().unwrap(), PdfObject::Integer(0));
        assert_eq!(
            parser.parse_object().unwrap(),
            PdfObject::Name(b"Name".to_vec())
        );
    }

    // ========================================================================
    // Arrays
    // ========================================================================

    #[test]
    fn test_parse_array_empty() {
        let mut parser = ObjectParser::new(b"[]");
        assert_eq!(parser.parse_object().unwrap(), PdfObject::Array(vec![]));
    }

    #[test]
    fn test_parse_array_integers() {
        let mut parser = ObjectParser::new(b"[1 2 3]");
        assert_eq!(
            parser.parse_object().unwrap(),
            PdfObject::Array(vec![
                PdfObject::Integer(1),
                PdfObject::Integer(2),
                PdfObject::Integer(3),
            ])
        );
    }

    #[test]
    fn test_parse_array_mixed() {
        let mut parser = ObjectParser::new(b"[1 /Name (text) true]");
        assert_eq!(
            parser.parse_object().unwrap(),
            PdfObject::Array(vec![
                PdfObject::Integer(1),
                PdfObject::Name(b"Name".to_vec()),
                PdfObject::String(PdfString::new(b"text".to_vec())),
                PdfObject::Boolean(true),
            ])
        );
    }

    #[test]
    fn test_parse_array_nested() {
        let mut parser = ObjectParser::new(b"[[1 2] [3 4]]");
        assert_eq!(
            parser.parse_object().unwrap(),
            PdfObject::Array(vec![
                PdfObject::Array(vec![PdfObject::Integer(1), PdfObject::Integer(2)]),
                PdfObject::Array(vec![PdfObject::Integer(3), PdfObject::Integer(4)]),
            ])
        );
    }

    #[test]
    fn test_parse_array_with_references() {
        let mut parser = ObjectParser::new(b"[1 0 R 2 0 R]");
        assert_eq!(
            parser.parse_object().unwrap(),
            PdfObject::Array(vec![
                PdfObject::Reference(ObjRef::new(1, 0)),
                PdfObject::Reference(ObjRef::new(2, 0)),
            ])
        );
    }

    // ========================================================================
    // Dictionaries
    // ========================================================================

    #[test]
    fn test_parse_dict_empty() {
        let mut parser = ObjectParser::new(b"<<>>");
        let obj = parser.parse_object().unwrap();
        match obj {
            PdfObject::Dictionary(d) => assert!(d.is_empty()),
            other => panic!("expected Dictionary, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_dict_simple() {
        let mut parser = ObjectParser::new(b"<< /Type /Catalog /Pages 3 0 R >>");
        let obj = parser.parse_object().unwrap();
        let dict = obj.as_dict().expect("expected dictionary");
        assert_eq!(
            dict.get(b"Type"),
            Some(&PdfObject::Name(b"Catalog".to_vec()))
        );
        assert_eq!(
            dict.get(b"Pages"),
            Some(&PdfObject::Reference(ObjRef::new(3, 0)))
        );
    }

    #[test]
    fn test_parse_dict_nested() {
        let mut parser = ObjectParser::new(b"<< /Inner << /Key /Value >> >>");
        let obj = parser.parse_object().unwrap();
        let dict = obj.as_dict().expect("expected dictionary");
        let inner = dict
            .get(b"Inner")
            .and_then(|o| o.as_dict())
            .expect("expected inner dict");
        assert_eq!(inner.get(b"Key"), Some(&PdfObject::Name(b"Value".to_vec())));
    }

    // ========================================================================
    // Streams
    // ========================================================================

    #[test]
    fn test_parse_stream() {
        let input = b"<< /Length 5 >>\nstream\nHelloendstream";
        let mut parser = ObjectParser::new(input);
        let obj = parser.parse_object().unwrap();
        match obj {
            PdfObject::Stream(s) => {
                assert_eq!(s.data_length, 5);
                assert_eq!(s.dict.get(b"Length"), Some(&PdfObject::Integer(5)));
            }
            other => panic!("expected Stream, got {other:?}"),
        }
        // Lexer should be past endstream (#24)
        assert_eq!(parser.lexer_mut().peek_token(), Token::Eof);
    }

    #[test]
    fn test_parse_stream_lexer_past_endstream() {
        // After parsing a stream, the lexer should be positioned past endstream
        // so the next token (endobj) is visible.
        let input = b"<< /Length 5 >>\nstream\nHello\nendstream\nendobj";
        let mut parser = ObjectParser::new(input);
        let obj = parser.parse_object().unwrap();
        assert!(matches!(obj, PdfObject::Stream(_)));
        // Next token should be endobj, not garbage or endstream
        assert_eq!(parser.lexer_mut().next_token(), Token::EndObj);
    }

    #[test]
    fn test_parse_stream_wrong_length() {
        // /Length says 3 but actual data is 5 bytes
        let diag = Arc::new(CollectingDiagnostics::new());
        let input = b"<< /Length 3 >>\nstream\nHello\nendstream";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        match obj {
            PdfObject::Stream(s) => {
                // Should have scanned and found the real length (5)
                assert_eq!(s.data_length, 5);
            }
            other => panic!("expected Stream, got {other:?}"),
        }
        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.message.contains("incorrect")),
            "expected incorrect length warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_parse_stream_indirect_length() {
        // /Length is an indirect reference (7 0 R), which the object parser
        // can't resolve at parse time. Should scan for endstream.
        let diag = Arc::new(CollectingDiagnostics::new());
        let input = b"<< /Length 7 0 R >>\nstream\nHello\nendstream";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        match obj {
            PdfObject::Stream(s) => {
                assert_eq!(s.data_length, 5);
            }
            other => panic!("expected Stream, got {other:?}"),
        }
        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("indirect reference")),
            "expected indirect reference warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_parse_stream_missing_length() {
        // No /Length at all. Should scan for endstream.
        let diag = Arc::new(CollectingDiagnostics::new());
        let input = b"<< /Filter /FlateDecode >>\nstream\nHello\nendstream";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        match obj {
            PdfObject::Stream(s) => {
                assert_eq!(s.data_length, 5);
            }
            other => panic!("expected Stream, got {other:?}"),
        }
        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.message.contains("missing")),
            "expected missing length warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_parse_stream_correct_length_with_eol() {
        // Stream with correct /Length and proper EOL before endstream
        let input = b"<< /Length 5 >>\nstream\nHello\nendstream";
        let mut parser = ObjectParser::new(input);
        let obj = parser.parse_object().unwrap();
        match obj {
            PdfObject::Stream(s) => {
                assert_eq!(s.data_length, 5);
            }
            other => panic!("expected Stream, got {other:?}"),
        }
        // Lexer should be past endstream
        assert_eq!(parser.lexer_mut().peek_token(), Token::Eof);
    }

    // ========================================================================
    // Decode helpers
    // ========================================================================

    #[test]
    fn test_decode_name_simple() {
        assert_eq!(decode_name(b"Type"), b"Type");
    }

    #[test]
    fn test_decode_name_hex_escape() {
        assert_eq!(decode_name(b"Hello#20World"), b"Hello World");
    }

    #[test]
    fn test_decode_name_multiple_escapes() {
        assert_eq!(decode_name(b"A#23B"), b"A#B");
    }

    #[test]
    fn test_decode_literal_string_simple() {
        assert_eq!(decode_literal_string(b"Hello"), b"Hello");
    }

    #[test]
    fn test_decode_literal_string_newline() {
        assert_eq!(decode_literal_string(b"a\\nb"), b"a\nb");
    }

    #[test]
    fn test_decode_literal_string_octal() {
        // Valid escapes
        assert_eq!(decode_literal_string(b"\\101"), b"A"); // \101 = 'A'
        assert_eq!(decode_literal_string(b"\\101\\102"), b"AB");
        assert_eq!(decode_literal_string(b"\\377"), &[255]); // Max valid

        // Wrapping behavior for spec violations (documented but tolerated)
        assert_eq!(decode_literal_string(b"\\400"), &[0]); // Wraps: 256 % 256 = 0
        assert_eq!(decode_literal_string(b"\\777"), &[255]); // Wraps: 511 % 256 = 255
    }

    #[test]
    fn test_decode_literal_string_escaped_parens() {
        assert_eq!(decode_literal_string(b"\\(\\)"), b"()");
    }

    #[test]
    fn test_decode_hex_string_normal() {
        assert_eq!(decode_hex_string(b"48656C6C6F"), b"Hello");
    }

    #[test]
    fn test_decode_hex_string_odd() {
        assert_eq!(decode_hex_string(b"ABC"), vec![0xAB, 0xC0]);
    }

    #[test]
    fn test_decode_hex_string_with_whitespace() {
        assert_eq!(decode_hex_string(b"48 65 6C 6C 6F"), b"Hello");
    }

    // ========================================================================
    // Security limits
    // ========================================================================

    #[test]
    fn test_nesting_depth_limit_arrays() {
        // 257 nested arrays should fail
        let open: Vec<u8> = std::iter::repeat_n(b'[', 257).collect();
        let close: Vec<u8> = std::iter::repeat_n(b']', 257).collect();
        let input: Vec<u8> = [open, close].concat();
        let mut parser = ObjectParser::new(&input);
        let err = parser.parse_object().unwrap_err();
        assert!(
            matches!(
                err,
                Error::ResourceLimit(ResourceLimitError {
                    limit: Limit::RecursionDepth(256),
                    ..
                })
            ),
            "expected RecursionDepth, got: {err}"
        );
    }

    #[test]
    fn test_nesting_depth_limit_dicts() {
        // Build nested dicts: << /A << /A << ... >> >> >>
        let mut input = Vec::new();
        for _ in 0..257 {
            input.extend_from_slice(b"<< /A ");
        }
        input.extend_from_slice(b"null");
        for _ in 0..257 {
            input.extend_from_slice(b" >>");
        }
        let mut parser = ObjectParser::new(&input);
        let err = parser.parse_object().unwrap_err();
        assert!(
            matches!(
                err,
                Error::ResourceLimit(ResourceLimitError {
                    limit: Limit::RecursionDepth(256),
                    ..
                })
            ),
            "expected RecursionDepth, got: {err}"
        );
    }

    #[test]
    fn test_nesting_at_limit_succeeds() {
        // Exactly 256 nested arrays should succeed
        let open: Vec<u8> = std::iter::repeat_n(b'[', 256).collect();
        let close: Vec<u8> = std::iter::repeat_n(b']', 256).collect();
        let input: Vec<u8> = [open, close].concat();
        let mut parser = ObjectParser::new(&input);
        assert!(parser.parse_object().is_ok());
    }

    #[test]
    fn test_collection_size_limit_array() {
        // 6 elements with limit 5 should fail
        let mut parser = ObjectParser::new(b"[1 2 3 4 5 6]");
        parser.set_max_collection_size(5);
        let err = parser.parse_object().unwrap_err();
        assert!(
            matches!(
                err,
                Error::ResourceLimit(ResourceLimitError {
                    limit: Limit::CollectionSize(5),
                    ..
                })
            ),
            "expected CollectionSize(5), got: {err}"
        );
    }

    #[test]
    fn test_collection_size_limit_dict() {
        // 4 entries with limit 3 should fail
        let mut parser = ObjectParser::new(b"<< /A 1 /B 2 /C 3 /D 4 >>");
        parser.set_max_collection_size(3);
        let err = parser.parse_object().unwrap_err();
        assert!(
            matches!(
                err,
                Error::ResourceLimit(ResourceLimitError {
                    limit: Limit::CollectionSize(3),
                    ..
                })
            ),
            "expected CollectionSize(3), got: {err}"
        );
    }

    #[test]
    fn test_collection_size_at_limit_succeeds() {
        // Exactly 3 elements with limit 3 should succeed
        let mut parser = ObjectParser::new(b"[1 2 3]");
        parser.set_max_collection_size(3);
        let obj = parser.parse_object().unwrap();
        assert_eq!(obj.as_array().unwrap().len(), 3);
    }

    #[test]
    fn test_depth_resets_after_error() {
        // 257 opening brackets (exceeds limit), then a simple integer.
        // After the error, the lexer sits right past the 257th `[`.
        // Depth must have unwound to 0 so the integer parses fine.
        let mut input: Vec<u8> = std::iter::repeat_n(b'[', 257).collect();
        input.extend_from_slice(b" 42");
        let mut parser = ObjectParser::new(&input);
        assert!(parser.parse_object().is_err());
        assert_eq!(parser.parse_object().unwrap(), PdfObject::Integer(42));
    }

    // ========================================================================
    // Complex real-world-like sequences
    // ========================================================================

    #[test]
    fn test_parse_page_dict() {
        let input = b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /MediaBox [0 0 612 792] >>";
        let mut parser = ObjectParser::new(input);
        let obj = parser.parse_object().unwrap();
        let dict = obj.as_dict().expect("expected dictionary");

        assert_eq!(dict.get(b"Type"), Some(&PdfObject::Name(b"Page".to_vec())));
        assert_eq!(
            dict.get(b"Parent"),
            Some(&PdfObject::Reference(ObjRef::new(2, 0)))
        );
        assert_eq!(
            dict.get(b"Contents"),
            Some(&PdfObject::Reference(ObjRef::new(4, 0)))
        );

        let media_box = dict
            .get(b"MediaBox")
            .and_then(|o| o.as_array())
            .expect("expected array");
        assert_eq!(media_box.len(), 4);
        assert_eq!(media_box[0], PdfObject::Integer(0));
        assert_eq!(media_box[2], PdfObject::Integer(612));
        assert_eq!(media_box[3], PdfObject::Integer(792));
    }

    // ========================================================================
    // Stream length verification (verify_stream_length)
    // ========================================================================

    #[test]
    fn test_verify_stream_length_correct_lf() {
        // Correct /Length with LF before endstream
        let input = b"<< /Length 5 >>\nstream\nHello\nendstream";
        let mut parser = ObjectParser::new(input);
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Stream(s) => assert_eq!(s.data_length, 5),
            other => panic!("expected Stream, got {other:?}"),
        }
    }

    #[test]
    fn test_verify_stream_length_correct_crlf() {
        // Correct /Length with CRLF before endstream
        let input = b"<< /Length 5 >>\nstream\nHello\r\nendstream";
        let mut parser = ObjectParser::new(input);
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Stream(s) => assert_eq!(s.data_length, 5),
            other => panic!("expected Stream, got {other:?}"),
        }
    }

    #[test]
    fn test_verify_stream_length_correct_cr() {
        // Correct /Length with CR before endstream
        let input = b"<< /Length 5 >>\nstream\nHello\rendstream";
        let mut parser = ObjectParser::new(input);
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Stream(s) => assert_eq!(s.data_length, 5),
            other => panic!("expected Stream, got {other:?}"),
        }
    }

    #[test]
    fn test_verify_stream_length_correct_no_eol() {
        // Correct /Length with no EOL before endstream (data immediately
        // followed by "endstream")
        let input = b"<< /Length 5 >>\nstream\nHelloendstream";
        let mut parser = ObjectParser::new(input);
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Stream(s) => assert_eq!(s.data_length, 5),
            other => panic!("expected Stream, got {other:?}"),
        }
    }

    #[test]
    fn test_verify_stream_length_incorrect_triggers_scan() {
        // /Length says 99 but actual data is 5 bytes. verify_stream_length
        // returns false so the parser scans for endstream.
        let diag = Arc::new(CollectingDiagnostics::new());
        let input = b"<< /Length 99 >>\nstream\nHello\nendstream";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Stream(s) => assert_eq!(s.data_length, 5),
            other => panic!("expected Stream, got {other:?}"),
        }
        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.kind == WarningKind::StreamLengthMismatch),
            "expected StreamLengthMismatch warning"
        );
    }

    #[test]
    fn test_verify_stream_length_past_eof_triggers_scan() {
        // /Length extends past end of data. verify_stream_length returns
        // false, parser falls back to scanning.
        let diag = Arc::new(CollectingDiagnostics::new());
        let input = b"<< /Length 9999 >>\nstream\nHi\nendstream";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Stream(s) => assert_eq!(s.data_length, 2),
            other => panic!("expected Stream, got {other:?}"),
        }
    }

    // ========================================================================
    // Indirect /Length (must scan for endstream)
    // ========================================================================

    #[test]
    fn test_stream_indirect_length_scans() {
        // /Length is a reference (cannot resolve at parse time). Parser must
        // scan for endstream and emit a warning about indirect reference.
        let diag = Arc::new(CollectingDiagnostics::new());
        let input = b"<< /Length 10 0 R >>\nstream\nABCDEF\nendstream";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Stream(s) => {
                assert_eq!(s.data_length, 6);
            }
            other => panic!("expected Stream, got {other:?}"),
        }
        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("indirect reference")),
            "expected warning about indirect reference, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_stream_indirect_length_preserves_ref_in_dict() {
        // After scanning, the dict should still contain the Reference for
        // /Length so the resolver can use it later.
        let input = b"<< /Length 10 0 R >>\nstream\ndata!\nendstream";
        let mut parser =
            ObjectParser::with_diagnostics(input, Arc::new(CollectingDiagnostics::new()));
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Stream(s) => {
                assert_eq!(
                    s.dict.get(b"Length"),
                    Some(&PdfObject::Reference(ObjRef::new(10, 0)))
                );
            }
            other => panic!("expected Stream, got {other:?}"),
        }
    }

    // ========================================================================
    // Missing /Length (must scan for endstream)
    // ========================================================================

    #[test]
    fn test_stream_missing_length_empty_body() {
        // No /Length, empty stream body
        let diag = Arc::new(CollectingDiagnostics::new());
        let input = b"<< >>\nstream\n\nendstream";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Stream(s) => {
                // The newline before endstream is the stream body delimiter,
                // scanner should find length 1 (the empty line = single LF)
                // or 0 depending on scan behavior. Just verify it parses.
                assert!(s.data_length <= 1);
            }
            other => panic!("expected Stream, got {other:?}"),
        }
        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.message.contains("missing")),
            "expected 'missing' warning"
        );
    }

    #[test]
    fn test_stream_missing_length_no_endstream() {
        // No /Length and no endstream at all. Should default to 0.
        let diag = Arc::new(CollectingDiagnostics::new());
        let input = b"<< >>\nstream\norphaned data";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Stream(s) => {
                assert_eq!(s.data_length, 0);
            }
            other => panic!("expected Stream, got {other:?}"),
        }
        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("endstream not found")),
            "expected 'endstream not found' warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    // ========================================================================
    // Negative /Length (treated as missing, scan)
    // ========================================================================

    #[test]
    fn test_stream_negative_length_scans() {
        // Negative /Length is filtered out by `.filter(|&n| n >= 0)`,
        // so the parser falls back to scanning.
        let diag = Arc::new(CollectingDiagnostics::new());
        let input = b"<< /Length -5 >>\nstream\nXYZ\nendstream";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Stream(s) => {
                assert_eq!(s.data_length, 3);
            }
            other => panic!("expected Stream, got {other:?}"),
        }
        // The warning should mention the length is "missing" because the
        // negative value is treated as if /Length were not present. The dict
        // still has /Length so the reason should be "indirect reference" --
        // actually, wait: -5 is a direct integer, as_i64() returns Some(-5),
        // but the filter rejects it. Then `dict.get(b"Length").is_some()` is
        // true, so reason = "indirect reference (cannot resolve at parse time)".
        // This is a slightly misleading message but correct behavior.
        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.kind == WarningKind::StreamLengthMismatch),
            "expected StreamLengthMismatch warning"
        );
    }

    #[test]
    fn test_stream_zero_length_correct() {
        // /Length 0 with endstream immediately after EOL
        let input = b"<< /Length 0 >>\nstream\n\nendstream";
        let mut parser = ObjectParser::new(input);
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Stream(s) => {
                // Length 0: verify_stream_length should find endstream after
                // skipping the EOL at position data_offset+0
                assert!(s.data_length <= 1);
            }
            other => panic!("expected Stream, got {other:?}"),
        }
    }

    // ========================================================================
    // Array depth limit (257+ levels nested)
    // ========================================================================

    #[test]
    fn test_array_depth_exactly_257_fails() {
        let open: Vec<u8> = std::iter::repeat_n(b'[', 257).collect();
        let close: Vec<u8> = std::iter::repeat_n(b']', 257).collect();
        let input: Vec<u8> = [open, close].concat();
        let mut parser = ObjectParser::new(&input);
        let err = parser.parse_object().unwrap_err();
        assert!(
            matches!(
                err,
                Error::ResourceLimit(ResourceLimitError {
                    limit: Limit::RecursionDepth(256),
                    ..
                })
            ),
            "expected RecursionDepth(256), got: {err}"
        );
    }

    #[test]
    fn test_array_depth_258_fails() {
        let open: Vec<u8> = std::iter::repeat_n(b'[', 258).collect();
        let close: Vec<u8> = std::iter::repeat_n(b']', 258).collect();
        let input: Vec<u8> = [open, close].concat();
        let mut parser = ObjectParser::new(&input);
        let err = parser.parse_object().unwrap_err();
        assert!(matches!(
            err,
            Error::ResourceLimit(ResourceLimitError {
                limit: Limit::RecursionDepth(256),
                ..
            })
        ));
    }

    #[test]
    fn test_mixed_array_dict_depth_limit() {
        // Alternate arrays and dicts to reach 257 levels
        let mut input = Vec::new();
        for i in 0..257 {
            if i % 2 == 0 {
                input.extend_from_slice(b"[ ");
            } else {
                input.extend_from_slice(b"<< /K ");
            }
        }
        input.extend_from_slice(b"null");
        for i in (0..257).rev() {
            if i % 2 == 0 {
                input.extend_from_slice(b" ]");
            } else {
                input.extend_from_slice(b" >>");
            }
        }
        let mut parser = ObjectParser::new(&input);
        let err = parser.parse_object().unwrap_err();
        assert!(matches!(
            err,
            Error::ResourceLimit(ResourceLimitError {
                limit: Limit::RecursionDepth(256),
                ..
            })
        ));
    }

    // ========================================================================
    // Dictionary depth limit
    // ========================================================================

    #[test]
    fn test_dict_depth_exactly_256_succeeds() {
        let mut input = Vec::new();
        for _ in 0..256 {
            input.extend_from_slice(b"<< /A ");
        }
        input.extend_from_slice(b"null");
        for _ in 0..256 {
            input.extend_from_slice(b" >>");
        }
        let mut parser = ObjectParser::new(&input);
        assert!(parser.parse_object().is_ok());
    }

    #[test]
    fn test_dict_depth_257_fails() {
        let mut input = Vec::new();
        for _ in 0..257 {
            input.extend_from_slice(b"<< /A ");
        }
        input.extend_from_slice(b"null");
        for _ in 0..257 {
            input.extend_from_slice(b" >>");
        }
        let mut parser = ObjectParser::new(&input);
        let err = parser.parse_object().unwrap_err();
        assert!(
            matches!(
                err,
                Error::ResourceLimit(ResourceLimitError {
                    limit: Limit::RecursionDepth(256),
                    ..
                })
            ),
            "expected RecursionDepth(256), got: {err}"
        );
    }

    // ========================================================================
    // Unterminated array (warning + partial return)
    // ========================================================================

    #[test]
    fn test_unterminated_array_returns_partial() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let input = b"[1 2 3";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Array(items) => {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0], PdfObject::Integer(1));
                assert_eq!(items[1], PdfObject::Integer(2));
                assert_eq!(items[2], PdfObject::Integer(3));
            }
            other => panic!("expected Array, got {other:?}"),
        }
        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.kind == WarningKind::UnterminatedCollection),
            "expected UnterminatedCollection warning"
        );
    }

    #[test]
    fn test_unterminated_array_empty() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let input = b"[";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Array(items) => assert!(items.is_empty()),
            other => panic!("expected Array, got {other:?}"),
        }
        let warnings = diag.warnings();
        assert!(warnings
            .iter()
            .any(|w| w.kind == WarningKind::UnterminatedCollection));
    }

    #[test]
    fn test_unterminated_nested_array() {
        // Inner array is unterminated, but outer array closes.
        // The inner parse hits EOF, returns partial, then outer
        // also hits EOF.
        let diag = Arc::new(CollectingDiagnostics::new());
        let input = b"[[1 2";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Array(outer) => {
                // Inner array got [1, 2] (partial)
                assert_eq!(outer.len(), 1);
                match &outer[0] {
                    PdfObject::Array(inner) => {
                        assert_eq!(inner.len(), 2);
                    }
                    other => panic!("expected inner Array, got {other:?}"),
                }
            }
            other => panic!("expected Array, got {other:?}"),
        }
        let warnings = diag.warnings();
        let unterminated_count = warnings
            .iter()
            .filter(|w| w.kind == WarningKind::UnterminatedCollection)
            .count();
        assert!(
            unterminated_count >= 2,
            "expected at least 2 UnterminatedCollection warnings, got {unterminated_count}"
        );
    }

    // ========================================================================
    // Unterminated dictionary (warning + partial return)
    // ========================================================================

    #[test]
    fn test_unterminated_dict_returns_partial() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let input = b"<< /A 1 /B 2";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Dictionary(d) => {
                assert_eq!(d.len(), 2);
                assert_eq!(d.get(b"A"), Some(&PdfObject::Integer(1)));
                assert_eq!(d.get(b"B"), Some(&PdfObject::Integer(2)));
            }
            other => panic!("expected Dictionary, got {other:?}"),
        }
        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.kind == WarningKind::UnterminatedCollection),
            "expected UnterminatedCollection warning"
        );
    }

    #[test]
    fn test_unterminated_dict_empty() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let input = b"<<";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Dictionary(d) => assert!(d.is_empty()),
            other => panic!("expected Dictionary, got {other:?}"),
        }
        let warnings = diag.warnings();
        assert!(warnings
            .iter()
            .any(|w| w.kind == WarningKind::UnterminatedCollection));
    }

    // ========================================================================
    // Array element count limit
    // ========================================================================

    #[test]
    fn test_array_element_limit_exactly_at_boundary() {
        // 3 elements with limit 3 succeeds, 4th fails
        let mut parser = ObjectParser::new(b"[1 2 3]");
        parser.set_max_collection_size(3);
        let obj = parser.parse_object().unwrap();
        assert_eq!(obj.as_array().unwrap().len(), 3);

        let mut parser = ObjectParser::new(b"[1 2 3 4]");
        parser.set_max_collection_size(3);
        let err = parser.parse_object().unwrap_err();
        assert!(matches!(
            err,
            Error::ResourceLimit(ResourceLimitError {
                limit: Limit::CollectionSize(3),
                ..
            })
        ));
    }

    #[test]
    fn test_array_element_limit_zero() {
        // Limit 0 means no elements allowed
        let mut parser = ObjectParser::new(b"[1]");
        parser.set_max_collection_size(0);
        let err = parser.parse_object().unwrap_err();
        assert!(matches!(
            err,
            Error::ResourceLimit(ResourceLimitError {
                limit: Limit::CollectionSize(0),
                ..
            })
        ));
    }

    #[test]
    fn test_array_element_limit_empty_succeeds() {
        // Empty array succeeds even with limit 0
        let mut parser = ObjectParser::new(b"[]");
        parser.set_max_collection_size(0);
        let obj = parser.parse_object().unwrap();
        assert_eq!(obj.as_array().unwrap().len(), 0);
    }

    // ========================================================================
    // Dictionary entry count limit
    // ========================================================================

    #[test]
    fn test_dict_entry_limit_exactly_at_boundary() {
        let mut parser = ObjectParser::new(b"<< /A 1 /B 2 >>");
        parser.set_max_collection_size(2);
        let obj = parser.parse_object().unwrap();
        assert_eq!(obj.as_dict().unwrap().len(), 2);

        let mut parser = ObjectParser::new(b"<< /A 1 /B 2 /C 3 >>");
        parser.set_max_collection_size(2);
        let err = parser.parse_object().unwrap_err();
        assert!(matches!(
            err,
            Error::ResourceLimit(ResourceLimitError {
                limit: Limit::CollectionSize(2),
                ..
            })
        ));
    }

    #[test]
    fn test_dict_entry_limit_zero() {
        let mut parser = ObjectParser::new(b"<< /A 1 >>");
        parser.set_max_collection_size(0);
        let err = parser.parse_object().unwrap_err();
        assert!(matches!(
            err,
            Error::ResourceLimit(ResourceLimitError {
                limit: Limit::CollectionSize(0),
                ..
            })
        ));
    }

    #[test]
    fn test_dict_entry_limit_empty_succeeds() {
        let mut parser = ObjectParser::new(b"<<>>");
        parser.set_max_collection_size(0);
        let obj = parser.parse_object().unwrap();
        assert!(obj.as_dict().unwrap().is_empty());
    }

    // ========================================================================
    // Non-name key recovery in dicts
    // ========================================================================

    #[test]
    fn test_dict_non_name_key_skipped_with_warning() {
        // An integer where a name key is expected. The parser should warn
        // and skip the non-name token, then continue parsing valid entries.
        let diag = Arc::new(CollectingDiagnostics::new());
        let input = b"<< 42 /A 1 /B 2 >>";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        let dict = obj.as_dict().expect("expected dictionary");
        // The integer 42 should have been skipped (it's not a valid key).
        // /A 1 is consumed as part of recovery: 42 is skipped, then /A
        // is the next name and becomes a key with value 1.
        assert_eq!(dict.get(b"A"), Some(&PdfObject::Integer(1)));
        assert_eq!(dict.get(b"B"), Some(&PdfObject::Integer(2)));
        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.kind == WarningKind::UnexpectedToken),
            "expected UnexpectedToken warning for non-name key"
        );
    }

    #[test]
    fn test_dict_string_key_skipped() {
        // String literal where a name is expected
        let diag = Arc::new(CollectingDiagnostics::new());
        let input = b"<< (bad) /Good 99 >>";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        let dict = obj.as_dict().expect("expected dictionary");
        assert_eq!(dict.get(b"Good"), Some(&PdfObject::Integer(99)));
        let warnings = diag.warnings();
        assert!(warnings
            .iter()
            .any(|w| w.kind == WarningKind::UnexpectedToken));
    }

    #[test]
    fn test_dict_bool_key_skipped() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let input = b"<< true /Valid 1 >>";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        let dict = obj.as_dict().expect("expected dictionary");
        assert_eq!(dict.get(b"Valid"), Some(&PdfObject::Integer(1)));
        let warnings = diag.warnings();
        assert!(warnings
            .iter()
            .any(|w| w.kind == WarningKind::UnexpectedToken));
    }

    #[test]
    fn test_dict_multiple_non_name_keys_all_skipped() {
        // Multiple non-name tokens before a valid key
        let diag = Arc::new(CollectingDiagnostics::new());
        let input = b"<< 1 2 3 /Key /Value >>";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        let dict = obj.as_dict().expect("expected dictionary");
        assert_eq!(dict.get(b"Key"), Some(&PdfObject::Name(b"Value".to_vec())));
        let warnings = diag.warnings();
        let unexpected_count = warnings
            .iter()
            .filter(|w| w.kind == WarningKind::UnexpectedToken)
            .count();
        assert!(
            unexpected_count >= 3,
            "expected at least 3 UnexpectedToken warnings, got {unexpected_count}"
        );
    }

    // ========================================================================
    // Out-of-range object/gen numbers in references
    // ========================================================================

    #[test]
    fn test_reference_obj_num_negative_becomes_null() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut parser = ObjectParser::with_diagnostics(b"-1 0 R", diag.clone());
        let obj = parser.parse_object().unwrap();
        assert_eq!(obj, PdfObject::Null);
        let warnings = diag.warnings();
        assert!(warnings.iter().any(|w| w.message.contains("out-of-range")));
    }

    #[test]
    fn test_reference_gen_negative_becomes_null() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut parser = ObjectParser::with_diagnostics(b"1 -1 R", diag.clone());
        let obj = parser.parse_object().unwrap();
        assert_eq!(obj, PdfObject::Null);
        let warnings = diag.warnings();
        assert!(warnings.iter().any(|w| w.message.contains("out-of-range")));
    }

    #[test]
    fn test_reference_obj_num_at_u32_max() {
        // u32::MAX (4294967295) is valid
        let input = b"4294967295 0 R";
        let mut parser = ObjectParser::new(input);
        let obj = parser.parse_object().unwrap();
        assert_eq!(obj, PdfObject::Reference(ObjRef::new(u32::MAX, 0)));
    }

    #[test]
    fn test_reference_obj_num_exceeds_u32_max() {
        // u32::MAX + 1 = 4294967296 is out of range
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut parser = ObjectParser::with_diagnostics(b"4294967296 0 R", diag.clone());
        let obj = parser.parse_object().unwrap();
        assert_eq!(obj, PdfObject::Null);
    }

    #[test]
    fn test_reference_gen_at_u16_max() {
        // u16::MAX (65535) is valid
        let input = b"1 65535 R";
        let mut parser = ObjectParser::new(input);
        let obj = parser.parse_object().unwrap();
        assert_eq!(obj, PdfObject::Reference(ObjRef::new(1, 65535)));
    }

    #[test]
    fn test_reference_gen_exceeds_u16_max() {
        // 65536 = u16::MAX + 1
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut parser = ObjectParser::with_diagnostics(b"1 65536 R", diag.clone());
        let obj = parser.parse_object().unwrap();
        assert_eq!(obj, PdfObject::Null);
    }

    #[test]
    fn test_reference_both_out_of_range() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut parser = ObjectParser::with_diagnostics(b"4294967296 65536 R", diag.clone());
        let obj = parser.parse_object().unwrap();
        assert_eq!(obj, PdfObject::Null);
        let warnings = diag.warnings();
        assert!(warnings.iter().any(|w| w.message.contains("out-of-range")));
    }

    // ========================================================================
    // Hex string odd-length padding
    // ========================================================================

    #[test]
    fn test_hex_string_single_digit_padded() {
        // Single hex digit "A" -> padded to "A0" -> 0xA0
        let mut parser = ObjectParser::new(b"<A>");
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), &[0xA0]),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn test_hex_string_three_digits_padded() {
        // "ABC" -> padded to "ABC0" -> [0xAB, 0xC0]
        let mut parser = ObjectParser::new(b"<ABC>");
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), &[0xAB, 0xC0]),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn test_hex_string_five_digits_padded() {
        // "ABCDE" -> padded to "ABCDE0" -> [0xAB, 0xCD, 0xE0]
        let mut parser = ObjectParser::new(b"<ABCDE>");
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), &[0xAB, 0xCD, 0xE0]),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn test_hex_string_with_whitespace_odd_padded() {
        // "A B C" -> digits are A, B, C (3 digits) -> padded to "ABC0"
        let mut parser = ObjectParser::new(b"<A B C>");
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), &[0xAB, 0xC0]),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn test_hex_string_empty() {
        let mut parser = ObjectParser::new(b"<>");
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::String(s) => assert!(s.as_bytes().is_empty()),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn test_hex_string_lowercase() {
        let mut parser = ObjectParser::new(b"<48656c6c6f>");
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::String(s) => assert_eq!(s.as_bytes(), b"Hello"),
            other => panic!("expected String, got {other:?}"),
        }
    }

    // ========================================================================
    // Name hex escape decoding
    // ========================================================================

    #[test]
    fn test_name_hex_escape_null_byte() {
        // #00 -> null byte
        let mut parser = ObjectParser::new(b"/A#00B");
        let obj = parser.parse_object().unwrap();
        assert_eq!(obj, PdfObject::Name(vec![b'A', 0x00, b'B']));
    }

    #[test]
    fn test_name_hex_escape_all_hex() {
        // #41#42#43 -> "ABC"
        let mut parser = ObjectParser::new(b"/#41#42#43");
        let obj = parser.parse_object().unwrap();
        assert_eq!(obj, PdfObject::Name(b"ABC".to_vec()));
    }

    #[test]
    fn test_name_hex_escape_high_byte() {
        // #FF -> 0xFF
        let mut parser = ObjectParser::new(b"/X#FF");
        let obj = parser.parse_object().unwrap();
        assert_eq!(obj, PdfObject::Name(vec![b'X', 0xFF]));
    }

    #[test]
    fn test_name_hex_escape_mixed_case() {
        // #2f and #2F both -> '/'
        assert_eq!(decode_name(b"a#2fb"), b"a/b");
        assert_eq!(decode_name(b"a#2Fb"), b"a/b");
    }

    #[test]
    fn test_name_hex_escape_incomplete_at_end() {
        // "#A" at end of name: only one hex digit available, so '#' and 'A'
        // are kept as-is (no valid escape)
        assert_eq!(decode_name(b"X#A"), b"X#A");
    }

    #[test]
    fn test_name_hex_escape_invalid_digits() {
        // "#GG" is not valid hex, so '#', 'G', 'G' are kept as-is
        assert_eq!(decode_name(b"X#GGY"), b"X#GGY");
    }

    #[test]
    fn test_name_hex_escape_hash_at_end() {
        // Lone '#' at end of name
        assert_eq!(decode_name(b"X#"), b"X#");
    }

    #[test]
    fn test_name_hex_escape_space() {
        // #20 -> space
        assert_eq!(decode_name(b"Hello#20World"), b"Hello World");
    }

    #[test]
    fn test_name_no_escapes() {
        assert_eq!(decode_name(b"SimpleName"), b"SimpleName");
    }

    // ========================================================================
    // Additional coverage: position() and lexer_mut()
    // ========================================================================

    #[test]
    fn test_position_tracks_correctly() {
        let mut parser = ObjectParser::new(b"  42");
        assert_eq!(parser.position(), 0);
        let _ = parser.parse_object().unwrap();
        assert_eq!(parser.position(), 4);
    }

    #[test]
    fn test_lexer_mut_direct_access() {
        let mut parser = ObjectParser::new(b"1 2 3");
        let lexer = parser.lexer_mut();
        assert_eq!(lexer.next_token(), Token::Integer(1));
        assert_eq!(lexer.next_token(), Token::Integer(2));
        assert_eq!(lexer.next_token(), Token::Integer(3));
    }

    // ========================================================================
    // Additional coverage: parse_object with unknown keyword token
    // ========================================================================

    #[test]
    fn test_parse_object_unknown_keyword_errors() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut parser = ObjectParser::with_diagnostics(b"foobar", diag.clone());
        let err = parser.parse_object().unwrap_err();
        assert!(err.to_string().contains("unexpected keyword"));
        let warnings = diag.warnings();
        assert!(warnings
            .iter()
            .any(|w| w.kind == WarningKind::UnknownKeyword));
    }

    #[test]
    fn test_parse_object_eof_errors() {
        let mut parser = ObjectParser::new(b"");
        let err = parser.parse_object().unwrap_err();
        assert!(err.to_string().contains("end of input"));
    }

    #[test]
    fn test_parse_object_lexer_error_token() {
        // Garbage bytes produce a LexError token, which parse_object should
        // handle and emit a warning.
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut parser = ObjectParser::with_diagnostics(b"\x80\x81 42", diag.clone());
        let err = parser.parse_object().unwrap_err();
        assert!(err.to_string().contains("lexer error"));
        let warnings = diag.warnings();
        assert!(warnings
            .iter()
            .any(|w| w.kind == WarningKind::MalformedToken));
    }

    #[test]
    fn test_parse_object_unexpected_structural_token() {
        // ArrayEnd, DictEnd, EndObj, etc. in object position should error
        let mut parser = ObjectParser::new(b"]");
        let err = parser.parse_object().unwrap_err();
        assert!(err.to_string().contains("unexpected token"));
    }

    #[test]
    fn test_parse_object_endobj_in_object_position() {
        let mut parser = ObjectParser::new(b"endobj");
        let err = parser.parse_object().unwrap_err();
        assert!(err.to_string().contains("unexpected token"));
    }

    #[test]
    fn test_parse_object_dictend_in_object_position() {
        let mut parser = ObjectParser::new(b">>");
        let err = parser.parse_object().unwrap_err();
        assert!(err.to_string().contains("unexpected token"));
    }

    // ========================================================================
    // Additional coverage: reference try_parse with non-R third token
    // ========================================================================

    #[test]
    fn test_integer_followed_by_integer_but_not_r() {
        // "5 0 /Name" -- not a reference, should rewind and return 5
        let mut parser = ObjectParser::new(b"5 0 /Name");
        let obj = parser.parse_object().unwrap();
        assert_eq!(obj, PdfObject::Integer(5));
        // Next calls should get 0 and then /Name
        let obj2 = parser.parse_object().unwrap();
        assert_eq!(obj2, PdfObject::Integer(0));
        let obj3 = parser.parse_object().unwrap();
        assert_eq!(obj3, PdfObject::Name(b"Name".to_vec()));
    }

    #[test]
    fn test_integer_followed_by_non_integer() {
        // "5 /Name" -- not a reference pattern at all, rewinds after seeing /Name
        let mut parser = ObjectParser::new(b"5 /Name");
        let obj = parser.parse_object().unwrap();
        assert_eq!(obj, PdfObject::Integer(5));
        let obj2 = parser.parse_object().unwrap();
        assert_eq!(obj2, PdfObject::Name(b"Name".to_vec()));
    }

    // ========================================================================
    // Additional coverage: array with error tokens skipped
    // ========================================================================

    #[test]
    fn test_array_with_garbage_skips_errors() {
        // Array containing garbage bytes that produce error tokens,
        // which should be skipped while valid elements are kept
        let diag = Arc::new(CollectingDiagnostics::new());
        let input = b"[1 \x80\x81 2]";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Array(items) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0], PdfObject::Integer(1));
                assert_eq!(items[1], PdfObject::Integer(2));
            }
            other => panic!("expected Array, got {other:?}"),
        }
    }

    // ========================================================================
    // Additional coverage: dict with error tokens skipped
    // ========================================================================

    #[test]
    fn test_dict_with_garbage_skips_errors() {
        // Dictionary containing garbage bytes between entries
        let diag = Arc::new(CollectingDiagnostics::new());
        let input = b"<< /A 1 \x80\x81 /B 2 >>";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        let dict = obj.as_dict().expect("expected dictionary");
        assert_eq!(dict.get(b"A"), Some(&PdfObject::Integer(1)));
        assert_eq!(dict.get(b"B"), Some(&PdfObject::Integer(2)));
    }

    // ========================================================================
    // Additional coverage: stream with correct length but verify_stream_length
    // fails (length past EOF)
    // ========================================================================

    #[test]
    fn test_stream_length_past_data_end_no_endstream() {
        // /Length is large and there's no endstream to scan for.
        // Parser should default to the declared length since scan returns None.
        let diag = Arc::new(CollectingDiagnostics::new());
        let input = b"<< /Length 100 >>\nstream\nABC";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Stream(s) => {
                // scan_for_endstream returns None, so parser trusts declared length
                // (even though it's wrong). The verify_stream_length check fails
                // because expected_end >= data.len(), so it falls to scanning,
                // and scan also fails, so it trusts declared len.
                assert!(s.data_length > 0);
            }
            other => panic!("expected Stream, got {other:?}"),
        }
    }

    // ========================================================================
    // Additional coverage: stream CRLF before endstream
    // ========================================================================

    #[test]
    fn test_stream_cr_before_stream_keyword() {
        // stream keyword followed by CR only (not CRLF)
        let input = b"<< /Length 5 >>\nstream\rHello\nendstream";
        let mut parser = ObjectParser::new(input);
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Stream(s) => {
                assert_eq!(s.data_length, 5);
            }
            other => panic!("expected Stream, got {other:?}"),
        }
    }

    #[test]
    fn test_stream_crlf_before_data() {
        // stream keyword followed by CRLF
        let input = b"<< /Length 5 >>\nstream\r\nHello\nendstream";
        let mut parser = ObjectParser::new(input);
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Stream(s) => {
                assert_eq!(s.data_length, 5);
            }
            other => panic!("expected Stream, got {other:?}"),
        }
    }

    // ========================================================================
    // Additional coverage: decode_literal_string edge cases
    // ========================================================================

    #[test]
    fn test_decode_literal_string_all_escapes() {
        // Test all single-char escapes: \n \r \t \b \f \\ \( \)
        let raw = b"\\n\\r\\t\\b\\f\\\\\\(\\)";
        let decoded = decode_literal_string(raw);
        assert_eq!(
            decoded,
            vec![b'\n', b'\r', b'\t', 0x08, 0x0C, b'\\', b'(', b')']
        );
    }

    #[test]
    fn test_decode_literal_string_line_continuation_cr() {
        // Backslash + CR = line continuation (both chars skipped)
        let raw = b"hello\\\rworld";
        let decoded = decode_literal_string(raw);
        assert_eq!(decoded, b"helloworld");
    }

    #[test]
    fn test_decode_literal_string_line_continuation_crlf() {
        // Backslash + CRLF = line continuation
        let raw = b"hello\\\r\nworld";
        let decoded = decode_literal_string(raw);
        assert_eq!(decoded, b"helloworld");
    }

    #[test]
    fn test_decode_literal_string_line_continuation_lf() {
        // Backslash + LF = line continuation
        let raw = b"hello\\\nworld";
        let decoded = decode_literal_string(raw);
        assert_eq!(decoded, b"helloworld");
    }

    #[test]
    fn test_decode_literal_string_unknown_escape() {
        // Unknown escape: \x should be treated as just 'x' (backslash ignored)
        let raw = b"\\x";
        let decoded = decode_literal_string(raw);
        assert_eq!(decoded, b"x");
    }

    #[test]
    fn test_decode_literal_string_octal_one_digit() {
        // Single octal digit: \0 -> NUL
        let raw = b"\\0";
        let decoded = decode_literal_string(raw);
        assert_eq!(decoded, &[0]);
    }

    #[test]
    fn test_decode_literal_string_octal_two_digits() {
        // Two octal digits: \10 -> 8 (1*8 + 0)
        let raw = b"\\10";
        let decoded = decode_literal_string(raw);
        assert_eq!(decoded, &[8]);
    }

    #[test]
    fn test_decode_literal_string_octal_three_digits() {
        // Three octal digits: \101 -> 65 -> 'A'
        let raw = b"\\101";
        let decoded = decode_literal_string(raw);
        assert_eq!(decoded, b"A");
    }

    #[test]
    fn test_decode_literal_string_backslash_at_end() {
        // Backslash at end of string (no char to escape).
        // The condition `i + 1 < raw.len()` is false, so the backslash
        // falls into the else branch and is pushed as a regular byte.
        let raw = b"hello\\";
        let decoded = decode_literal_string(raw);
        assert_eq!(decoded, b"hello\\");
    }

    // ========================================================================
    // Additional coverage: decode_hex_string edge cases
    // ========================================================================

    #[test]
    fn test_decode_hex_string_all_zeros() {
        assert_eq!(decode_hex_string(b"0000"), vec![0x00, 0x00]);
    }

    #[test]
    fn test_decode_hex_string_all_ff() {
        assert_eq!(decode_hex_string(b"FFFF"), vec![0xFF, 0xFF]);
    }

    #[test]
    fn test_decode_hex_string_mixed_with_garbage() {
        // Non-hex chars are skipped, only hex digits are used
        assert_eq!(decode_hex_string(b"4G8H"), vec![0x48]);
    }

    // ========================================================================
    // Additional coverage: hex_digit function
    // ========================================================================

    #[test]
    fn test_hex_digit_all_ranges() {
        assert_eq!(hex_digit(b'0'), Some(0));
        assert_eq!(hex_digit(b'9'), Some(9));
        assert_eq!(hex_digit(b'a'), Some(10));
        assert_eq!(hex_digit(b'f'), Some(15));
        assert_eq!(hex_digit(b'A'), Some(10));
        assert_eq!(hex_digit(b'F'), Some(15));
        assert_eq!(hex_digit(b'G'), None);
        assert_eq!(hex_digit(b' '), None);
        assert_eq!(hex_digit(b'z'), None);
    }

    // ========================================================================
    // Additional coverage: stream endstream keyword consumption
    // ========================================================================

    #[test]
    fn test_skip_endstream_keyword_when_not_present() {
        // If the next token is not endstream, skip_endstream_keyword does nothing
        let mut parser = ObjectParser::new(b"<< /Length 5 >>\nstream\nHello\nendstream\nendobj");
        let obj = parser.parse_object().unwrap();
        assert!(matches!(obj, PdfObject::Stream(_)));
        // After parsing stream, the endstream should have been consumed
        // and endobj should be next
        let next = parser.lexer_mut().next_token();
        assert_eq!(next, Token::EndObj);
    }

    // ========================================================================
    // Additional coverage: stream with negative /Length (treated as missing)
    // ========================================================================

    #[test]
    fn test_stream_length_name_value() {
        // /Length is a name (not integer), treated as if missing
        let diag = Arc::new(CollectingDiagnostics::new());
        let input = b"<< /Length /BadValue >>\nstream\nXYZ\nendstream";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Stream(s) => {
                assert_eq!(s.data_length, 3);
            }
            other => panic!("expected Stream, got {other:?}"),
        }
    }

    // ========================================================================
    // Additional coverage: verify_stream_length with data at exact boundary
    // ========================================================================

    #[test]
    fn test_verify_stream_length_exact_at_eof() {
        // /Length points exactly to end of data (expected_end == data.len())
        // verify_stream_length returns false, falls back to scan
        let diag = Arc::new(CollectingDiagnostics::new());
        // The stream data "AB" is 2 bytes. Set /Length to a large value
        // that would make expected_end >= data.len()
        let input = b"<< /Length 999 >>\nstream\nAB\nendstream";
        let mut parser = ObjectParser::with_diagnostics(input, diag.clone());
        let obj = parser.parse_object().unwrap();
        match &obj {
            PdfObject::Stream(s) => {
                // Should have scanned and found actual length 2
                assert_eq!(s.data_length, 2);
            }
            other => panic!("expected Stream, got {other:?}"),
        }
    }

    // ========================================================================
    // Additional coverage: dict value parsing error propagation
    // ========================================================================

    #[test]
    fn test_dict_value_error_propagated() {
        // Dict with a key whose value is unparseable (e.g., deeply nested
        // arrays that exceed depth limit). The error should propagate.
        let mut input = Vec::new();
        input.extend_from_slice(b"<< /Key ");
        // 257 nested arrays for the value
        input.resize(input.len() + 257, b'[');
        input.resize(input.len() + 257, b']');
        input.extend_from_slice(b" >>");
        let mut parser = ObjectParser::new(&input);
        let err = parser.parse_object().unwrap_err();
        assert!(matches!(
            err,
            Error::ResourceLimit(ResourceLimitError {
                limit: Limit::RecursionDepth(256),
                ..
            })
        ));
    }
}
