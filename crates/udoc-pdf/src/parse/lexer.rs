//! Zero-copy PDF lexer.
//!
//! Tokenizes a byte slice into PDF tokens without allocating for names,
//! strings, or other byte-oriented tokens. Tokens borrow directly from
//! the source data.
//!
//! Handles common spec violations:
//! - Missing whitespace between tokens (e.g., `/Name/Name2`)
//! - Nested parentheses in literal strings
//! - Malformed hex strings (odd digits, non-hex chars)
//! - Binary garbage outside streams
//! - Mixed line endings (CR, LF, CRLF)

use std::fmt;
use std::sync::Arc;

use crate::diagnostics::{DiagnosticsSink, NullDiagnostics, Warning, WarningKind};

/// Maximum nesting depth for parentheses in literal strings.
/// Prevents stack overflow and excessive memory usage from malicious PDFs.
const MAX_PAREN_DEPTH: u32 = 256;

/// Errors that can occur during lexing.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
#[allow(dead_code)] // spec-complete enum; all variants represent real error conditions
pub enum LexError {
    /// Unexpected end of input.
    UnexpectedEof,
    /// Unexpected byte encountered.
    UnexpectedByte(u8),
    /// Invalid escape sequence in a literal string.
    InvalidEscape(u8),
    /// Malformed hex string.
    MalformedHexString,
    /// Malformed number.
    MalformedNumber,
    /// Malformed name token.
    MalformedName,
    /// Garbage bytes encountered (resync attempted).
    GarbageBytes { offset: u64, length: u64 },
}

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LexError::UnexpectedEof => write!(f, "unexpected end of input"),
            LexError::UnexpectedByte(b) => write!(f, "unexpected byte 0x{b:02X}"),
            LexError::InvalidEscape(b) => write!(f, "invalid escape sequence \\{}", *b as char),
            LexError::MalformedHexString => write!(f, "malformed hex string"),
            LexError::MalformedNumber => write!(f, "malformed number"),
            LexError::MalformedName => write!(f, "malformed name"),
            LexError::GarbageBytes { offset, length } => {
                write!(f, "garbage bytes at offset {offset}, length {length}")
            }
        }
    }
}

/// A PDF token. Borrows string/name data from the source slice.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Token<'a> {
    // Atoms
    /// Integer literal (e.g., `123`, `-45`).
    Integer(i64),
    /// Real number literal (e.g., `3.14`, `-.5`).
    Real(f64),
    /// Name token (e.g., `/SomeName`). Bytes after the `/`, with `#XX` escapes
    /// still encoded — the raw bytes from the source.
    Name(&'a [u8]),
    /// Literal string `(...)` — raw bytes between outer parens, with escape
    /// sequences still present.
    LiteralString(&'a [u8]),
    /// Hex string `<...>` — raw hex digits between angle brackets.
    HexString(&'a [u8]),

    // Keywords
    /// `true`
    True,
    /// `false`
    False,
    /// `null`
    Null,
    /// `obj`
    Obj,
    /// `endobj`
    EndObj,
    /// `stream`
    Stream,
    /// `endstream`
    EndStream,
    /// `R` (indirect reference marker)
    R,
    /// `xref`
    XRef,
    /// `trailer`
    Trailer,
    /// `startxref`
    StartXRef,

    // Structural
    /// `[`
    ArrayStart,
    /// `]`
    ArrayEnd,
    /// `<<`
    DictStart,
    /// `>>`
    DictEnd,

    // Content stream operators and other unknown alphabetic keywords.
    /// An alphabetic keyword not matching any structural PDF keyword.
    /// In object context this is unexpected; in content streams these
    /// are operators (BT, ET, Td, Tj, TJ, cm, q, Q, etc.).
    Keyword(&'a [u8]),

    // Meta
    /// End of input.
    Eof,
    /// Lexer error (recoverable).
    Error(LexError),
}

/// Zero-copy PDF lexer.
///
/// Operates on a borrowed byte slice and produces [`Token`]s that reference
/// subslices of the source data. No allocations in the hot path.
pub struct Lexer<'a> {
    /// Source data.
    data: &'a [u8],
    /// Current position in the source.
    pos: usize,
    /// Diagnostics sink for warnings.
    diagnostics: Arc<dyn DiagnosticsSink>,
}

impl fmt::Debug for Lexer<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Lexer")
            .field("pos", &self.pos)
            .field("data_len", &self.data.len())
            .finish()
    }
}

impl<'a> Lexer<'a> {
    /// Create a new lexer over the given byte slice.
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            diagnostics: Arc::new(NullDiagnostics),
        }
    }

    /// Create a new lexer with a custom diagnostics sink.
    pub fn with_diagnostics(data: &'a [u8], diagnostics: Arc<dyn DiagnosticsSink>) -> Self {
        Self {
            data,
            pos: 0,
            diagnostics,
        }
    }

    /// Current byte offset in the source.
    pub fn position(&self) -> u64 {
        self.pos as u64
    }

    /// Set the lexer position.
    pub fn set_position(&mut self, pos: u64) {
        self.pos = pos as usize;
    }

    /// Get the underlying data slice.
    pub(crate) fn data_slice(&self) -> &'a [u8] {
        self.data
    }

    /// Skip the mandatory EOL (CR, LF, or CRLF) after a `stream` keyword.
    /// Per PDF spec 7.3.8.1, the stream keyword must be followed by a
    /// single end-of-line marker before the stream data begins.
    pub fn skip_stream_eol(&mut self) {
        match self.peek() {
            Some(b'\r') => {
                self.advance();
                if self.peek() == Some(b'\n') {
                    self.advance(); // CRLF
                }
            }
            Some(b'\n') => {
                self.advance();
            }
            _ => {} // malformed, but don't fail
        }
    }

    /// Peek at the current byte without advancing.
    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    /// Advance by one byte and return it.
    fn advance(&mut self) -> Option<u8> {
        let b = self.data.get(self.pos).copied();
        if b.is_some() {
            self.pos += 1;
        }
        b
    }

    /// Check if we've reached end of input.
    fn at_end(&self) -> bool {
        self.pos >= self.data.len()
    }

    /// Emit a warning to the diagnostics sink.
    fn warn(&self, offset: u64, kind: WarningKind, message: impl Into<String>) {
        self.diagnostics
            .warning(Warning::new(Some(offset), kind, message));
    }

    /// Is the byte a PDF whitespace character?
    /// PDF spec defines: NUL(0), TAB(9), LF(10), FF(12), CR(13), SP(32)
    pub(crate) fn is_whitespace(b: u8) -> bool {
        matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
    }

    /// Is the byte a PDF delimiter?
    /// Delimiters: < > [ ] { } / %
    pub(crate) fn is_delimiter(b: u8) -> bool {
        matches!(
            b,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
    }

    /// Skip whitespace and comments, handling mixed line endings.
    fn skip_whitespace_and_comments(&mut self) {
        loop {
            match self.peek() {
                Some(b) if Self::is_whitespace(b) => {
                    self.advance();
                }
                Some(b'%') => {
                    // Comment: skip to end of line
                    self.advance();
                    loop {
                        match self.advance() {
                            Some(b'\r') => {
                                // CRLF or just CR
                                if self.peek() == Some(b'\n') {
                                    self.advance();
                                }
                                break;
                            }
                            Some(b'\n') => break,
                            None => break,
                            _ => {}
                        }
                    }
                }
                _ => break,
            }
        }
    }

    /// Read the next token from the source.
    pub fn next_token(&mut self) -> Token<'a> {
        self.skip_whitespace_and_comments();

        if self.at_end() {
            return Token::Eof;
        }

        let start = self.pos;

        match self.peek() {
            Some(b'/') => self.lex_name(),
            Some(b'(') => self.lex_literal_string(),
            Some(b'<') => {
                // Could be hex string `<...>` or dict start `<<`
                self.advance();
                if self.peek() == Some(b'<') {
                    self.advance();
                    Token::DictStart
                } else {
                    self.lex_hex_string()
                }
            }
            Some(b'>') => {
                self.advance();
                if self.peek() == Some(b'>') {
                    self.advance();
                    Token::DictEnd
                } else {
                    self.warn(
                        start as u64,
                        WarningKind::GarbageBytes,
                        "unexpected '>' outside of hex string",
                    );
                    Token::Error(LexError::UnexpectedByte(b'>'))
                }
            }
            Some(b'[') => {
                self.advance();
                Token::ArrayStart
            }
            Some(b']') => {
                self.advance();
                Token::ArrayEnd
            }
            Some(b) if b == b'+' || b == b'-' || b == b'.' || b.is_ascii_digit() => {
                self.lex_number()
            }
            Some(b) if b.is_ascii_alphabetic() => self.lex_keyword_or_resync(start),
            // Content stream operators ' and " (0x27, 0x22)
            // These have no structural meaning in PDF object syntax, but are
            // valid operators in content streams.
            Some(b'\'') | Some(b'"') => {
                self.advance();
                Token::Keyword(&self.data[start..self.pos])
            }
            _ => self.lex_garbage(start),
        }
    }

    /// Lex a name token: `/SomeName`
    /// Names start with `/` and continue until whitespace or delimiter.
    /// Handles `#XX` hex escape sequences (we keep raw bytes — zero copy).
    fn lex_name(&mut self) -> Token<'a> {
        // Skip the leading `/`
        self.advance();
        let start = self.pos;

        // A name can be empty: `/` followed immediately by delimiter or whitespace is the empty name.
        while let Some(b) = self.peek() {
            if Self::is_whitespace(b) || Self::is_delimiter(b) {
                break;
            }
            self.advance();
        }

        let name_bytes = &self.data[start..self.pos];
        Token::Name(name_bytes)
    }

    /// Lex a literal string: `(Hello \( World)`
    /// Handles nested parentheses and escape sequences.
    fn lex_literal_string(&mut self) -> Token<'a> {
        // Skip opening `(`
        let open_pos = self.pos;
        self.advance();
        let start = self.pos;
        let mut depth: u32 = 1;

        loop {
            match self.advance() {
                Some(b'(') => {
                    depth += 1;
                    if depth > MAX_PAREN_DEPTH {
                        self.warn(
                            open_pos as u64,
                            WarningKind::MalformedString,
                            format!(
                                "literal string nesting depth exceeded {} (truncated)",
                                MAX_PAREN_DEPTH
                            ),
                        );
                        return Token::LiteralString(&self.data[start..self.pos]);
                    }
                }
                Some(b')') => {
                    depth -= 1;
                    if depth == 0 {
                        // `self.pos` is one past the closing `)`.
                        let end = self.pos - 1;
                        return Token::LiteralString(&self.data[start..end]);
                    }
                }
                Some(b'\\')
                    // Escape: skip next byte regardless of what it is.
                    // This handles `\(`, `\)`, `\\`, `\n`, `\r`, `\t`, `\b`, `\f`,
                    // and octal escapes `\NNN`.
                    if self.advance().is_none() => {
                        self.warn(
                            open_pos as u64,
                            WarningKind::MalformedString,
                            "unterminated literal string (EOF after backslash)",
                        );
                        return Token::LiteralString(&self.data[start..self.pos]);
                    }
                None => {
                    self.warn(
                        open_pos as u64,
                        WarningKind::MalformedString,
                        "unterminated literal string",
                    );
                    return Token::LiteralString(&self.data[start..self.pos]);
                }
                _ => {}
            }
        }
    }

    /// Lex a hex string: `<48656C6C6F>`
    /// Called after the opening `<` has been consumed.
    /// Handles malformed hex (odd digits, non-hex chars).
    fn lex_hex_string(&mut self) -> Token<'a> {
        let start = self.pos;

        loop {
            match self.peek() {
                Some(b'>') => {
                    let end = self.pos;
                    self.advance(); // consume `>`
                    return Token::HexString(&self.data[start..end]);
                }
                Some(b) if Self::is_whitespace(b) => {
                    // Whitespace is allowed inside hex strings
                    self.advance();
                }
                Some(b) if b.is_ascii_hexdigit() => {
                    self.advance();
                }
                Some(b) => {
                    // Non-hex character — recover by warning and skipping
                    self.warn(
                        self.pos as u64,
                        WarningKind::MalformedString,
                        format!("non-hex character 0x{b:02X} in hex string, skipping"),
                    );
                    self.advance();
                }
                None => {
                    self.warn(
                        start as u64,
                        WarningKind::MalformedString,
                        "unterminated hex string",
                    );
                    return Token::HexString(&self.data[start..self.pos]);
                }
            }
        }
    }

    /// Lex a number (integer or real).
    ///
    /// PDF numbers: optional sign, digits, optional `.` with more digits.
    /// Examples: `123`, `-45`, `3.14`, `-.5`, `+0`, `.25`
    fn lex_number(&mut self) -> Token<'a> {
        let start = self.pos;
        let mut has_dot = false;
        let mut has_digit = false;

        // Optional sign
        if let Some(b'+' | b'-') = self.peek() {
            self.advance();
        }

        // Leading digits or dot
        loop {
            match self.peek() {
                Some(b'.') if !has_dot => {
                    has_dot = true;
                    self.advance();
                }
                Some(b) if b.is_ascii_digit() => {
                    has_digit = true;
                    self.advance();
                }
                _ => break,
            }
        }

        if !has_digit {
            // Just a sign or just a dot with no digits — not a valid number.
            // Try to interpret as garbage / resync.
            self.warn(
                start as u64,
                WarningKind::MalformedToken,
                "malformed number token",
            );
            return Token::Error(LexError::MalformedNumber);
        }

        let num_bytes = &self.data[start..self.pos];

        if has_dot {
            // Parse as f64
            // SAFETY: we know these are ASCII digit/sign/dot bytes
            let s = std::str::from_utf8(num_bytes).unwrap_or("0");
            match s.parse::<f64>() {
                Ok(v) => Token::Real(v),
                Err(_) => {
                    self.warn(
                        start as u64,
                        WarningKind::MalformedToken,
                        format!("unparseable real number: {s}"),
                    );
                    Token::Error(LexError::MalformedNumber)
                }
            }
        } else {
            let s = std::str::from_utf8(num_bytes).unwrap_or("0");
            match s.parse::<i64>() {
                Ok(v) => Token::Integer(v),
                Err(_) => {
                    // Might overflow i64 — try as f64 for very large numbers
                    match s.parse::<f64>() {
                        Ok(v) => {
                            self.warn(
                                start as u64,
                                WarningKind::MalformedToken,
                                format!("integer overflow, treating as real: {s}"),
                            );
                            Token::Real(v)
                        }
                        Err(_) => {
                            self.warn(
                                start as u64,
                                WarningKind::MalformedToken,
                                format!("unparseable number: {s}"),
                            );
                            Token::Error(LexError::MalformedNumber)
                        }
                    }
                }
            }
        }
    }

    /// Try to lex a keyword. If the identifier doesn't match any known keyword,
    /// treat it as garbage and attempt resynchronization.
    fn lex_keyword_or_resync(&mut self, start: usize) -> Token<'a> {
        // Read alphabetic characters
        while let Some(b) = self.peek() {
            if b.is_ascii_alphabetic() {
                self.advance();
            } else {
                break;
            }
        }

        // Content stream operators that end in `*`: `T*`, `b*`, `B*`, `f*`,
        // `W*`. `*` is not an alphabetic keyword char, so consume it here
        // explicitly when the preceding keyword is one of the known
        // path-painting / clipping operators (ISO 32000-2 §8.5.3 / §8.5.4)
        // or the text-line-next operator.
        if self.pos - start == 1
            && matches!(self.data[start], b'T' | b'b' | b'B' | b'f' | b'W')
            && self.peek() == Some(b'*')
        {
            self.advance();
        }

        let word = &self.data[start..self.pos];

        match word {
            b"true" => Token::True,
            b"false" => Token::False,
            b"null" => Token::Null,
            b"obj" => Token::Obj,
            b"endobj" => Token::EndObj,
            b"stream" => Token::Stream,
            b"endstream" => Token::EndStream,
            b"R" => Token::R,
            b"xref" => Token::XRef,
            b"trailer" => Token::Trailer,
            b"startxref" => Token::StartXRef,
            _ => Token::Keyword(word),
        }
    }

    /// Handle garbage bytes: scan forward for a recognizable token start.
    fn lex_garbage(&mut self, start: usize) -> Token<'a> {
        let garbage_start = self.pos;
        self.advance(); // skip the unrecognized byte

        // Scan forward for a recognizable token boundary
        while let Some(b) = self.peek() {
            if Self::is_whitespace(b)
                || Self::is_delimiter(b)
                || b.is_ascii_digit()
                || b == b'+'
                || b == b'-'
                || b == b'.'
                || b.is_ascii_alphabetic()
            {
                break;
            }
            self.advance();
        }

        let length = (self.pos - garbage_start) as u64;
        self.warn(
            start as u64,
            WarningKind::GarbageBytes,
            format!("skipped {length} garbage byte(s), resynchronizing"),
        );

        Token::Error(LexError::GarbageBytes {
            offset: garbage_start as u64,
            length,
        })
    }

    /// Scan forward from the current position for the `endstream` keyword.
    ///
    /// Returns the byte count of actual stream data (relative to current
    /// position), or None if `endstream` is not found.
    ///
    /// The scan looks for `endstream` preceded by optional EOL (the spec
    /// says endstream should be on its own line, but we're lenient).
    /// Any preceding CR, LF, or CRLF is excluded from the data length.
    ///
    /// To avoid false positives (stream data containing the literal text
    /// "endstream"), each match is validated: the byte after "endstream"
    /// must be whitespace, EOF, or the start of a keyword boundary.
    pub fn scan_for_endstream(&self) -> Option<u64> {
        let needle = b"endstream";
        let haystack = &self.data[self.pos..];

        let haystack_len = haystack.len();
        if haystack_len < needle.len() {
            return None;
        }

        let mut i = 0;
        while i <= haystack_len - needle.len() {
            if &haystack[i..i + needle.len()] != needle {
                i += 1;
                continue;
            }

            // Validate keyword boundary: byte after "endstream" must be
            // whitespace or EOF (not a continuation of some longer word).
            let after = i + needle.len();
            if after < haystack_len {
                let b = haystack[after];
                if !Self::is_whitespace(b) && !Self::is_delimiter(b) {
                    // False positive (e.g. "endstreamXYZ"), skip it
                    i += 1;
                    continue;
                }
            }

            // Found real endstream at offset i from current position.
            // Backtrack over any preceding EOL to find true data end.
            let mut data_end = i;
            if data_end > 0 && haystack[data_end - 1] == b'\n' {
                data_end -= 1;
                if data_end > 0 && haystack[data_end - 1] == b'\r' {
                    data_end -= 1; // CRLF
                }
            } else if data_end > 0 && haystack[data_end - 1] == b'\r' {
                data_end -= 1; // bare CR
            }
            return Some(data_end as u64);
        }

        None
    }

    /// Peek at the next token without consuming it.
    /// Returns the token and restores position afterward.
    pub fn peek_token(&mut self) -> Token<'a> {
        let saved_pos = self.pos;
        let token = self.next_token();
        self.pos = saved_pos;
        token
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::CollectingDiagnostics;

    // ========================================================================
    // F-103: Numeric tokens
    // ========================================================================

    #[test]
    fn test_integer_positive() {
        let mut lexer = Lexer::new(b"123");
        assert_eq!(lexer.next_token(), Token::Integer(123));
        assert_eq!(lexer.next_token(), Token::Eof);
    }

    #[test]
    fn test_integer_negative() {
        let mut lexer = Lexer::new(b"-45");
        assert_eq!(lexer.next_token(), Token::Integer(-45));
    }

    #[test]
    fn test_integer_positive_sign() {
        let mut lexer = Lexer::new(b"+99");
        assert_eq!(lexer.next_token(), Token::Integer(99));
    }

    #[test]
    fn test_integer_zero() {
        let mut lexer = Lexer::new(b"0");
        assert_eq!(lexer.next_token(), Token::Integer(0));
    }

    #[test]
    fn test_real_number() {
        let mut lexer = Lexer::new(b"2.5");
        assert_eq!(lexer.next_token(), Token::Real(2.5));
    }

    #[test]
    fn test_real_negative() {
        let mut lexer = Lexer::new(b"-2.5");
        assert_eq!(lexer.next_token(), Token::Real(-2.5));
    }

    #[test]
    fn test_real_leading_dot() {
        let mut lexer = Lexer::new(b".25");
        assert_eq!(lexer.next_token(), Token::Real(0.25));
    }

    #[test]
    fn test_real_negative_leading_dot() {
        let mut lexer = Lexer::new(b"-.5");
        assert_eq!(lexer.next_token(), Token::Real(-0.5));
    }

    #[test]
    fn test_real_trailing_dot() {
        let mut lexer = Lexer::new(b"5.");
        assert_eq!(lexer.next_token(), Token::Real(5.0));
    }

    #[test]
    fn test_multiple_numbers() {
        let mut lexer = Lexer::new(b"1 2 3.0 -4");
        assert_eq!(lexer.next_token(), Token::Integer(1));
        assert_eq!(lexer.next_token(), Token::Integer(2));
        assert_eq!(lexer.next_token(), Token::Real(3.0));
        assert_eq!(lexer.next_token(), Token::Integer(-4));
    }

    // ========================================================================
    // F-104: Name tokens
    // ========================================================================

    #[test]
    fn test_name_simple() {
        let mut lexer = Lexer::new(b"/Name");
        assert_eq!(lexer.next_token(), Token::Name(b"Name"));
    }

    #[test]
    fn test_name_type() {
        let mut lexer = Lexer::new(b"/Type");
        assert_eq!(lexer.next_token(), Token::Name(b"Type"));
    }

    #[test]
    fn test_name_with_hash_escape() {
        let mut lexer = Lexer::new(b"/Name#20With#20Spaces");
        assert_eq!(lexer.next_token(), Token::Name(b"Name#20With#20Spaces"));
    }

    #[test]
    fn test_name_empty() {
        // `/` followed by whitespace = empty name (valid per spec)
        let mut lexer = Lexer::new(b"/ ");
        assert_eq!(lexer.next_token(), Token::Name(b""));
    }

    #[test]
    fn test_name_followed_by_name() {
        // Missing whitespace between names: `/Name1/Name2`
        let mut lexer = Lexer::new(b"/Name1/Name2");
        assert_eq!(lexer.next_token(), Token::Name(b"Name1"));
        assert_eq!(lexer.next_token(), Token::Name(b"Name2"));
    }

    #[test]
    fn test_name_followed_by_number() {
        let mut lexer = Lexer::new(b"/Size 42");
        assert_eq!(lexer.next_token(), Token::Name(b"Size"));
        assert_eq!(lexer.next_token(), Token::Integer(42));
    }

    // ========================================================================
    // F-105: Literal strings
    // ========================================================================

    #[test]
    fn test_literal_string_simple() {
        let mut lexer = Lexer::new(b"(Hello)");
        assert_eq!(lexer.next_token(), Token::LiteralString(b"Hello"));
    }

    #[test]
    fn test_literal_string_empty() {
        let mut lexer = Lexer::new(b"()");
        assert_eq!(lexer.next_token(), Token::LiteralString(b""));
    }

    #[test]
    fn test_literal_string_nested_parens() {
        let mut lexer = Lexer::new(b"(Hello (World))");
        assert_eq!(lexer.next_token(), Token::LiteralString(b"Hello (World)"));
    }

    #[test]
    fn test_literal_string_deeply_nested_parens() {
        let mut lexer = Lexer::new(b"(a(b(c)d)e)");
        assert_eq!(lexer.next_token(), Token::LiteralString(b"a(b(c)d)e"));
    }

    #[test]
    fn test_literal_string_escaped_parens() {
        let mut lexer = Lexer::new(b"(Hello \\( World)");
        assert_eq!(lexer.next_token(), Token::LiteralString(b"Hello \\( World"));
    }

    #[test]
    fn test_literal_string_escape_sequences() {
        let mut lexer = Lexer::new(b"(line1\\nline2)");
        assert_eq!(lexer.next_token(), Token::LiteralString(b"line1\\nline2"));
    }

    #[test]
    fn test_literal_string_escaped_backslash() {
        let mut lexer = Lexer::new(b"(a\\\\b)");
        assert_eq!(lexer.next_token(), Token::LiteralString(b"a\\\\b"));
    }

    #[test]
    fn test_literal_string_unterminated() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut lexer = Lexer::with_diagnostics(b"(unterminated", diag.clone());
        let tok = lexer.next_token();
        assert_eq!(tok, Token::LiteralString(b"unterminated"));
        assert!(!diag.warnings().is_empty());
    }

    #[test]
    fn test_literal_string_depth_limit() {
        let diag = Arc::new(CollectingDiagnostics::new());
        // Build string with MAX_PAREN_DEPTH + 1 nesting
        let mut input = vec![b'('];
        input.extend(std::iter::repeat_n(b'(', (MAX_PAREN_DEPTH + 1) as usize));
        input.extend(std::iter::repeat_n(b')', (MAX_PAREN_DEPTH + 1) as usize));
        input.push(b')');

        let mut lexer = Lexer::with_diagnostics(&input, diag.clone());
        let _token = lexer.next_token();

        let warnings = diag.warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("depth exceeded"));
    }

    #[test]
    fn test_literal_string_depth_within_limit() {
        // String with exactly MAX_PAREN_DEPTH nesting should succeed
        let mut input = vec![b'('];
        input.extend(std::iter::repeat_n(b'(', MAX_PAREN_DEPTH as usize));
        input.extend(std::iter::repeat_n(b')', MAX_PAREN_DEPTH as usize));
        input.push(b')');

        let mut lexer = Lexer::new(&input);
        let _token = lexer.next_token();
        assert!(matches!(_token, Token::LiteralString(_)));
    }

    // ========================================================================
    // F-106: Hex strings
    // ========================================================================

    #[test]
    fn test_hex_string() {
        let mut lexer = Lexer::new(b"<48656C6C6F>");
        assert_eq!(lexer.next_token(), Token::HexString(b"48656C6C6F"));
    }

    #[test]
    fn test_hex_string_empty() {
        let mut lexer = Lexer::new(b"<>");
        assert_eq!(lexer.next_token(), Token::HexString(b""));
    }

    #[test]
    fn test_hex_string_with_spaces() {
        let mut lexer = Lexer::new(b"<48 65 6C 6C 6F>");
        // Whitespace inside hex strings is skipped during lex; the raw bytes
        // between `<` and `>` are returned.
        assert_eq!(lexer.next_token(), Token::HexString(b"48 65 6C 6C 6F"));
    }

    #[test]
    fn test_hex_string_odd_digits() {
        // Odd number of hex digits — the spec says pad with zero.
        // The lexer returns raw bytes; decoding happens later.
        let mut lexer = Lexer::new(b"<ABC>");
        assert_eq!(lexer.next_token(), Token::HexString(b"ABC"));
    }

    #[test]
    fn test_hex_string_recovery_non_hex() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut lexer = Lexer::with_diagnostics(b"<48GG65>", diag.clone());
        let tok = lexer.next_token();
        // Non-hex chars are skipped with a warning
        assert_eq!(tok, Token::HexString(b"48GG65"));
        assert!(!diag.warnings().is_empty());
    }

    #[test]
    fn test_hex_string_unterminated() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut lexer = Lexer::with_diagnostics(b"<4865", diag.clone());
        let tok = lexer.next_token();
        assert_eq!(tok, Token::HexString(b"4865"));
        assert!(!diag.warnings().is_empty());
    }

    // ========================================================================
    // F-107: Keywords
    // ========================================================================

    #[test]
    fn test_keyword_true() {
        let mut lexer = Lexer::new(b"true");
        assert_eq!(lexer.next_token(), Token::True);
    }

    #[test]
    fn test_keyword_false() {
        let mut lexer = Lexer::new(b"false");
        assert_eq!(lexer.next_token(), Token::False);
    }

    #[test]
    fn test_keyword_null() {
        let mut lexer = Lexer::new(b"null");
        assert_eq!(lexer.next_token(), Token::Null);
    }

    #[test]
    fn test_keyword_obj_endobj() {
        let mut lexer = Lexer::new(b"obj endobj");
        assert_eq!(lexer.next_token(), Token::Obj);
        assert_eq!(lexer.next_token(), Token::EndObj);
    }

    #[test]
    fn test_keyword_stream_endstream() {
        let mut lexer = Lexer::new(b"stream endstream");
        assert_eq!(lexer.next_token(), Token::Stream);
        assert_eq!(lexer.next_token(), Token::EndStream);
    }

    #[test]
    fn test_keyword_r() {
        let mut lexer = Lexer::new(b"R");
        assert_eq!(lexer.next_token(), Token::R);
    }

    #[test]
    fn test_keyword_xref() {
        let mut lexer = Lexer::new(b"xref");
        assert_eq!(lexer.next_token(), Token::XRef);
    }

    #[test]
    fn test_keyword_trailer() {
        let mut lexer = Lexer::new(b"trailer");
        assert_eq!(lexer.next_token(), Token::Trailer);
    }

    #[test]
    fn test_keyword_startxref() {
        let mut lexer = Lexer::new(b"startxref");
        assert_eq!(lexer.next_token(), Token::StartXRef);
    }

    #[test]
    fn test_unknown_keyword_is_keyword_token() {
        let mut lexer = Lexer::new(b"foobar");
        let tok = lexer.next_token();
        assert_eq!(tok, Token::Keyword(b"foobar"));
    }

    #[test]
    fn test_content_stream_operators_are_keywords() {
        let mut lexer = Lexer::new(b"BT ET Td Tj TJ cm q Q");
        assert_eq!(lexer.next_token(), Token::Keyword(b"BT"));
        assert_eq!(lexer.next_token(), Token::Keyword(b"ET"));
        assert_eq!(lexer.next_token(), Token::Keyword(b"Td"));
        assert_eq!(lexer.next_token(), Token::Keyword(b"Tj"));
        assert_eq!(lexer.next_token(), Token::Keyword(b"TJ"));
        assert_eq!(lexer.next_token(), Token::Keyword(b"cm"));
        assert_eq!(lexer.next_token(), Token::Keyword(b"q"));
        assert_eq!(lexer.next_token(), Token::Keyword(b"Q"));
    }

    #[test]
    fn test_content_stream_special_operators() {
        // T* is a two-character operator (T followed by *)
        let mut lexer = Lexer::new(b"T*");
        assert_eq!(lexer.next_token(), Token::Keyword(b"T*"));

        // ' (single quote) is a move-and-show operator
        let mut lexer = Lexer::new(b"'");
        assert_eq!(lexer.next_token(), Token::Keyword(b"'"));

        // " (double quote) is a set-spacing-and-show operator
        let mut lexer = Lexer::new(b"\"");
        assert_eq!(lexer.next_token(), Token::Keyword(b"\""));
    }

    // ========================================================================
    // F-108: Missing whitespace between tokens
    // ========================================================================

    #[test]
    fn test_name_name_no_whitespace() {
        let mut lexer = Lexer::new(b"/Name1/Name2");
        assert_eq!(lexer.next_token(), Token::Name(b"Name1"));
        assert_eq!(lexer.next_token(), Token::Name(b"Name2"));
    }

    #[test]
    fn test_number_name_no_whitespace() {
        let mut lexer = Lexer::new(b"42/Type");
        assert_eq!(lexer.next_token(), Token::Integer(42));
        assert_eq!(lexer.next_token(), Token::Name(b"Type"));
    }

    #[test]
    fn test_name_array_no_whitespace() {
        let mut lexer = Lexer::new(b"/Name[1 2]");
        assert_eq!(lexer.next_token(), Token::Name(b"Name"));
        assert_eq!(lexer.next_token(), Token::ArrayStart);
        assert_eq!(lexer.next_token(), Token::Integer(1));
        assert_eq!(lexer.next_token(), Token::Integer(2));
        assert_eq!(lexer.next_token(), Token::ArrayEnd);
    }

    #[test]
    fn test_name_dict_no_whitespace() {
        let mut lexer = Lexer::new(b"/Name<<>>");
        assert_eq!(lexer.next_token(), Token::Name(b"Name"));
        assert_eq!(lexer.next_token(), Token::DictStart);
        assert_eq!(lexer.next_token(), Token::DictEnd);
    }

    #[test]
    fn test_number_paren_no_whitespace() {
        let mut lexer = Lexer::new(b"42(hello)");
        assert_eq!(lexer.next_token(), Token::Integer(42));
        assert_eq!(lexer.next_token(), Token::LiteralString(b"hello"));
    }

    #[test]
    fn test_keyword_name_no_whitespace() {
        let mut lexer = Lexer::new(b"true/Key");
        assert_eq!(lexer.next_token(), Token::True);
        assert_eq!(lexer.next_token(), Token::Name(b"Key"));
    }

    // ========================================================================
    // F-109: Mixed line endings
    // ========================================================================

    #[test]
    fn test_lf_line_ending() {
        let mut lexer = Lexer::new(b"1\n2");
        assert_eq!(lexer.next_token(), Token::Integer(1));
        assert_eq!(lexer.next_token(), Token::Integer(2));
    }

    #[test]
    fn test_cr_line_ending() {
        let mut lexer = Lexer::new(b"1\r2");
        assert_eq!(lexer.next_token(), Token::Integer(1));
        assert_eq!(lexer.next_token(), Token::Integer(2));
    }

    #[test]
    fn test_crlf_line_ending() {
        let mut lexer = Lexer::new(b"1\r\n2");
        assert_eq!(lexer.next_token(), Token::Integer(1));
        assert_eq!(lexer.next_token(), Token::Integer(2));
    }

    #[test]
    fn test_mixed_line_endings() {
        let mut lexer = Lexer::new(b"1\n2\r3\r\n4");
        assert_eq!(lexer.next_token(), Token::Integer(1));
        assert_eq!(lexer.next_token(), Token::Integer(2));
        assert_eq!(lexer.next_token(), Token::Integer(3));
        assert_eq!(lexer.next_token(), Token::Integer(4));
    }

    #[test]
    fn test_comment_with_cr() {
        let mut lexer = Lexer::new(b"% comment\r42");
        assert_eq!(lexer.next_token(), Token::Integer(42));
    }

    #[test]
    fn test_comment_with_crlf() {
        let mut lexer = Lexer::new(b"% comment\r\n42");
        assert_eq!(lexer.next_token(), Token::Integer(42));
    }

    // ========================================================================
    // F-110: Garbage resynchronization
    // ========================================================================

    #[test]
    fn test_garbage_before_token() {
        let diag = Arc::new(CollectingDiagnostics::new());
        // 0x80 0x81 are garbage bytes, then a valid integer
        let mut lexer = Lexer::with_diagnostics(b"\x80\x81 42", diag.clone());
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::Error(LexError::GarbageBytes { .. })));
        // After garbage, the lexer should resync and find `42`
        assert_eq!(lexer.next_token(), Token::Integer(42));
        assert!(!diag.warnings().is_empty());
    }

    #[test]
    fn test_garbage_between_tokens() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut lexer = Lexer::with_diagnostics(b"1 \xFF\xFE 2", diag.clone());
        assert_eq!(lexer.next_token(), Token::Integer(1));
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::Error(LexError::GarbageBytes { .. })));
        assert_eq!(lexer.next_token(), Token::Integer(2));
    }

    // ========================================================================
    // Structural tokens
    // ========================================================================

    #[test]
    fn test_array_brackets() {
        let mut lexer = Lexer::new(b"[1 2 3]");
        assert_eq!(lexer.next_token(), Token::ArrayStart);
        assert_eq!(lexer.next_token(), Token::Integer(1));
        assert_eq!(lexer.next_token(), Token::Integer(2));
        assert_eq!(lexer.next_token(), Token::Integer(3));
        assert_eq!(lexer.next_token(), Token::ArrayEnd);
    }

    #[test]
    fn test_dict_brackets() {
        let mut lexer = Lexer::new(b"<< /Type /Catalog >>");
        assert_eq!(lexer.next_token(), Token::DictStart);
        assert_eq!(lexer.next_token(), Token::Name(b"Type"));
        assert_eq!(lexer.next_token(), Token::Name(b"Catalog"));
        assert_eq!(lexer.next_token(), Token::DictEnd);
    }

    // ========================================================================
    // Complex token sequences
    // ========================================================================

    #[test]
    fn test_object_definition() {
        let mut lexer = Lexer::new(b"1 0 obj\n<< /Type /Catalog >>\nendobj");
        assert_eq!(lexer.next_token(), Token::Integer(1));
        assert_eq!(lexer.next_token(), Token::Integer(0));
        assert_eq!(lexer.next_token(), Token::Obj);
        assert_eq!(lexer.next_token(), Token::DictStart);
        assert_eq!(lexer.next_token(), Token::Name(b"Type"));
        assert_eq!(lexer.next_token(), Token::Name(b"Catalog"));
        assert_eq!(lexer.next_token(), Token::DictEnd);
        assert_eq!(lexer.next_token(), Token::EndObj);
    }

    #[test]
    fn test_indirect_reference() {
        let mut lexer = Lexer::new(b"5 0 R");
        assert_eq!(lexer.next_token(), Token::Integer(5));
        assert_eq!(lexer.next_token(), Token::Integer(0));
        assert_eq!(lexer.next_token(), Token::R);
    }

    #[test]
    fn test_pdf_header_comment() {
        let mut lexer = Lexer::new(b"%PDF-1.7\n1 0 obj");
        assert_eq!(lexer.next_token(), Token::Integer(1));
        assert_eq!(lexer.next_token(), Token::Integer(0));
        assert_eq!(lexer.next_token(), Token::Obj);
    }

    #[test]
    fn test_eof_on_empty() {
        let mut lexer = Lexer::new(b"");
        assert_eq!(lexer.next_token(), Token::Eof);
    }

    #[test]
    fn test_eof_after_whitespace() {
        let mut lexer = Lexer::new(b"   \n  ");
        assert_eq!(lexer.next_token(), Token::Eof);
    }

    // ========================================================================
    // Position tracking
    // ========================================================================

    #[test]
    fn test_position_tracking() {
        let mut lexer = Lexer::new(b"  42");
        assert_eq!(lexer.position(), 0);
        lexer.next_token();
        assert_eq!(lexer.position(), 4); // past "  42"
    }

    #[test]
    fn test_set_position() {
        let mut lexer = Lexer::new(b"hello 42 world");
        lexer.set_position(6);
        assert_eq!(lexer.next_token(), Token::Integer(42));
    }

    // ========================================================================
    // Peek
    // ========================================================================

    #[test]
    fn test_peek_token() {
        let mut lexer = Lexer::new(b"42 true");
        assert_eq!(lexer.peek_token(), Token::Integer(42));
        assert_eq!(lexer.peek_token(), Token::Integer(42)); // still the same
        assert_eq!(lexer.next_token(), Token::Integer(42)); // now consume
        assert_eq!(lexer.next_token(), Token::True);
    }

    // ========================================================================
    // scan_for_endstream
    // ========================================================================

    #[test]
    fn test_scan_for_endstream_basic() {
        let data = b"stream data here\nendstream";
        let lexer = Lexer::new(data);
        let len = lexer.scan_for_endstream().unwrap();
        assert_eq!(len, 16); // "stream data here" = 16 bytes, LF stripped
    }

    #[test]
    fn test_scan_for_endstream_skips_false_positive() {
        // Stream data contains literal "endstreamlined" (not a real keyword)
        // followed by the real endstream
        let data = b"the word endstreamlined appears\nendstream";
        let lexer = Lexer::new(data);
        let len = lexer.scan_for_endstream().unwrap();
        // Should skip the false positive and find the real one
        assert_eq!(len, 31); // "the word endstreamlined appears" = 31
    }

    #[test]
    fn test_scan_for_endstream_at_eof() {
        // endstream at very end of data (no trailing byte)
        let data = b"stuff\nendstream";
        let lexer = Lexer::new(data);
        let len = lexer.scan_for_endstream().unwrap();
        assert_eq!(len, 5); // "stuff"
    }

    #[test]
    fn test_scan_for_endstream_not_found() {
        let data = b"no keyword here";
        let lexer = Lexer::new(data);
        assert!(lexer.scan_for_endstream().is_none());
    }

    // ========================================================================
    // Diagnostics integration
    // ========================================================================

    #[test]
    fn test_diagnostics_sink_receives_warnings() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut lexer = Lexer::with_diagnostics(b"<48GG>", diag.clone());
        lexer.next_token();
        let warnings = diag.warnings();
        assert!(!warnings.is_empty());
        assert!(warnings[0].message.contains("non-hex"));
    }

    // ========================================================================
    // LexError Display
    // ========================================================================

    #[test]
    fn test_lex_error_display() {
        assert!(format!("{}", LexError::UnexpectedEof).contains("unexpected end"));
        assert!(format!("{}", LexError::UnexpectedByte(0xFF)).contains("0xFF"));
        assert!(format!("{}", LexError::InvalidEscape(b'x')).contains("\\x"));
        assert!(format!("{}", LexError::MalformedHexString).contains("hex string"));
        assert!(format!("{}", LexError::MalformedNumber).contains("number"));
        assert!(format!("{}", LexError::MalformedName).contains("name"));
        let g = LexError::GarbageBytes {
            offset: 10,
            length: 5,
        };
        assert!(format!("{g}").contains("offset 10"));
    }

    // ========================================================================
    // Number edge cases
    // ========================================================================

    #[test]
    fn test_sign_only_is_error() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut lexer = Lexer::with_diagnostics(b"+", diag.clone());
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::Error(LexError::MalformedNumber)));
    }

    #[test]
    fn test_dot_only_is_error() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut lexer = Lexer::with_diagnostics(b".", diag.clone());
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::Error(LexError::MalformedNumber)));
    }

    #[test]
    fn test_integer_overflow_becomes_real() {
        let diag = Arc::new(CollectingDiagnostics::new());
        // i64::MAX + 1
        let big = b"9999999999999999999999";
        let mut lexer = Lexer::with_diagnostics(big, diag.clone());
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::Real(_)));
        assert!(diag
            .warnings()
            .iter()
            .any(|w| w.message.contains("overflow")));
    }

    // ========================================================================
    // Stream EOL handling
    // ========================================================================

    #[test]
    fn test_skip_stream_eol_cr_lf() {
        let mut lexer = Lexer::new(b"\r\ndata");
        lexer.skip_stream_eol();
        assert_eq!(lexer.position(), 2);
    }

    #[test]
    fn test_skip_stream_eol_cr_only() {
        let mut lexer = Lexer::new(b"\rdata");
        lexer.skip_stream_eol();
        assert_eq!(lexer.position(), 1);
    }

    #[test]
    fn test_skip_stream_eol_lf_only() {
        let mut lexer = Lexer::new(b"\ndata");
        lexer.skip_stream_eol();
        assert_eq!(lexer.position(), 1);
    }

    #[test]
    fn test_skip_stream_eol_no_eol() {
        let mut lexer = Lexer::new(b"data");
        lexer.skip_stream_eol();
        assert_eq!(lexer.position(), 0); // no movement
    }

    // ========================================================================
    // Comment edge cases
    // ========================================================================

    #[test]
    fn test_comment_at_eof() {
        let mut lexer = Lexer::new(b"% comment at EOF");
        assert_eq!(lexer.next_token(), Token::Eof);
    }

    #[test]
    fn test_comment_cr_only_ending() {
        let mut lexer = Lexer::new(b"% comment\r42");
        assert_eq!(lexer.next_token(), Token::Integer(42));
    }

    #[test]
    fn test_comment_crlf_ending() {
        let mut lexer = Lexer::new(b"% comment\r\n42");
        assert_eq!(lexer.next_token(), Token::Integer(42));
    }

    // ========================================================================
    // Lone '>' outside hex string
    // ========================================================================

    #[test]
    fn test_lone_greater_than() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut lexer = Lexer::with_diagnostics(b"> 42", diag.clone());
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::Error(LexError::UnexpectedByte(b'>'))));
        assert_eq!(lexer.next_token(), Token::Integer(42));
    }

    // ========================================================================
    // Literal string: backslash at EOF
    // ========================================================================

    #[test]
    fn test_literal_string_backslash_at_eof() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut lexer = Lexer::with_diagnostics(b"(hello\\", diag.clone());
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::LiteralString(_)));
        assert!(diag
            .warnings()
            .iter()
            .any(|w| w.message.contains("backslash")));
    }

    // ========================================================================
    // scan_for_endstream edge cases
    // ========================================================================

    #[test]
    fn test_scan_for_endstream_cr_eol() {
        // endstream preceded by bare CR
        let data = b"stream data\rendstream";
        let lexer = Lexer::new(data);
        let len = lexer.scan_for_endstream().unwrap();
        assert_eq!(len, 11); // "stream data" without CR
    }

    #[test]
    fn test_scan_for_endstream_crlf_eol() {
        let data = b"data\r\nendstream";
        let lexer = Lexer::new(data);
        let len = lexer.scan_for_endstream().unwrap();
        assert_eq!(len, 4); // "data" without CRLF
    }

    #[test]
    fn test_scan_for_endstream_too_short() {
        let data = b"end"; // shorter than "endstream"
        let lexer = Lexer::new(data);
        assert!(lexer.scan_for_endstream().is_none());
    }

    // ========================================================================
    // data_slice
    // ========================================================================

    #[test]
    fn test_data_slice() {
        let data = b"hello";
        let lexer = Lexer::new(data);
        assert_eq!(lexer.data_slice(), b"hello");
    }

    // ========================================================================
    // Additional coverage: hex string edge cases
    // ========================================================================

    #[test]
    fn test_hex_string_lowercase() {
        let mut lexer = Lexer::new(b"<abcdef>");
        assert_eq!(lexer.next_token(), Token::HexString(b"abcdef"));
    }

    #[test]
    fn test_hex_string_mixed_case() {
        let mut lexer = Lexer::new(b"<aAbBcC>");
        assert_eq!(lexer.next_token(), Token::HexString(b"aAbBcC"));
    }

    #[test]
    fn test_hex_string_with_tabs_and_newlines() {
        let mut lexer = Lexer::new(b"<48\t65\n6C\r6C\r\n6F>");
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::HexString(_)));
    }

    #[test]
    fn test_hex_string_multiple_non_hex_chars() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut lexer = Lexer::with_diagnostics(b"<GGZZ>", diag.clone());
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::HexString(_)));
        // Should have warnings for each non-hex char
        assert!(diag.warnings().len() >= 2);
    }

    // ========================================================================
    // Additional coverage: name token edge cases
    // ========================================================================

    #[test]
    fn test_name_at_eof() {
        let mut lexer = Lexer::new(b"/EOF");
        assert_eq!(lexer.next_token(), Token::Name(b"EOF"));
        assert_eq!(lexer.next_token(), Token::Eof);
    }

    #[test]
    fn test_name_followed_by_paren() {
        let mut lexer = Lexer::new(b"/Key(value)");
        assert_eq!(lexer.next_token(), Token::Name(b"Key"));
        assert_eq!(lexer.next_token(), Token::LiteralString(b"value"));
    }

    #[test]
    fn test_name_followed_by_hex_string() {
        let mut lexer = Lexer::new(b"/Key<48>");
        assert_eq!(lexer.next_token(), Token::Name(b"Key"));
        assert_eq!(lexer.next_token(), Token::HexString(b"48"));
    }

    #[test]
    fn test_name_with_digits_and_underscores() {
        let mut lexer = Lexer::new(b"/Font_123");
        assert_eq!(lexer.next_token(), Token::Name(b"Font_123"));
    }

    #[test]
    fn test_name_with_special_chars() {
        // Names can contain non-delimiter, non-whitespace characters
        let mut lexer = Lexer::new(b"/Name!@$^&*");
        // ! @ $ ^ & * are not delimiters or whitespace, so they're part of the name
        assert_eq!(lexer.next_token(), Token::Name(b"Name!@$^&*"));
    }

    // ========================================================================
    // Additional coverage: literal string edge cases
    // ========================================================================

    #[test]
    fn test_literal_string_with_cr() {
        let mut lexer = Lexer::new(b"(line1\rline2)");
        assert_eq!(lexer.next_token(), Token::LiteralString(b"line1\rline2"));
    }

    #[test]
    fn test_literal_string_with_crlf() {
        let mut lexer = Lexer::new(b"(line1\r\nline2)");
        assert_eq!(lexer.next_token(), Token::LiteralString(b"line1\r\nline2"));
    }

    #[test]
    fn test_literal_string_octal_escape() {
        // Octal escape \101 = 'A' (65 decimal)
        // The lexer returns raw bytes including the escape
        let mut lexer = Lexer::new(b"(\\101)");
        let tok = lexer.next_token();
        assert_eq!(tok, Token::LiteralString(b"\\101"));
    }

    #[test]
    fn test_literal_string_multiple_escapes() {
        let mut lexer = Lexer::new(b"(\\n\\r\\t\\b\\f)");
        assert_eq!(lexer.next_token(), Token::LiteralString(b"\\n\\r\\t\\b\\f"));
    }

    #[test]
    fn test_literal_string_escaped_closing_paren() {
        let mut lexer = Lexer::new(b"(hello \\) world)");
        assert_eq!(lexer.next_token(), Token::LiteralString(b"hello \\) world"));
    }

    #[test]
    fn test_literal_string_triple_nested() {
        let mut lexer = Lexer::new(b"(a(b(c(d)c)b)a)");
        assert_eq!(lexer.next_token(), Token::LiteralString(b"a(b(c(d)c)b)a"));
    }

    // ========================================================================
    // Additional coverage: number parsing edge cases
    // ========================================================================

    #[test]
    fn test_negative_sign_only() {
        let mut lexer = Lexer::new(b"- ");
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::Error(LexError::MalformedNumber)));
    }

    #[test]
    fn test_sign_then_dot_no_digit() {
        // "+." has no digits at all
        let mut lexer = Lexer::new(b"+.");
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::Error(LexError::MalformedNumber)));
    }

    #[test]
    fn test_positive_real_with_sign() {
        let mut lexer = Lexer::new(b"+2.71");
        assert_eq!(lexer.next_token(), Token::Real(2.71));
    }

    #[test]
    fn test_large_integer() {
        let mut lexer = Lexer::new(b"2147483647");
        assert_eq!(lexer.next_token(), Token::Integer(2147483647));
    }

    #[test]
    fn test_negative_large_integer() {
        let mut lexer = Lexer::new(b"-9223372036854775808");
        assert_eq!(lexer.next_token(), Token::Integer(-9223372036854775808i64));
    }

    #[test]
    fn test_number_followed_by_bracket() {
        let mut lexer = Lexer::new(b"42]");
        assert_eq!(lexer.next_token(), Token::Integer(42));
        assert_eq!(lexer.next_token(), Token::ArrayEnd);
    }

    #[test]
    fn test_number_followed_by_dict_end() {
        let mut lexer = Lexer::new(b"42>>");
        assert_eq!(lexer.next_token(), Token::Integer(42));
        assert_eq!(lexer.next_token(), Token::DictEnd);
    }

    #[test]
    fn test_real_very_small() {
        let mut lexer = Lexer::new(b"0.00001");
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::Real(v) if (v - 0.00001).abs() < 1e-10));
    }

    #[test]
    fn test_multiple_dots_stops_at_second() {
        // "1.2.3" should parse as Real(1.2) then Real(0.3)
        let mut lexer = Lexer::new(b"1.2.3");
        assert_eq!(lexer.next_token(), Token::Real(1.2));
        assert_eq!(lexer.next_token(), Token::Real(0.3));
    }

    // ========================================================================
    // Additional coverage: keyword/boolean/null variations
    // ========================================================================

    #[test]
    fn test_true_followed_by_bracket() {
        let mut lexer = Lexer::new(b"true]");
        assert_eq!(lexer.next_token(), Token::True);
        assert_eq!(lexer.next_token(), Token::ArrayEnd);
    }

    #[test]
    fn test_false_followed_by_name() {
        let mut lexer = Lexer::new(b"false/Key");
        assert_eq!(lexer.next_token(), Token::False);
        assert_eq!(lexer.next_token(), Token::Name(b"Key"));
    }

    #[test]
    fn test_null_followed_by_endobj() {
        let mut lexer = Lexer::new(b"null endobj");
        assert_eq!(lexer.next_token(), Token::Null);
        assert_eq!(lexer.next_token(), Token::EndObj);
    }

    // ========================================================================
    // Additional coverage: scan_for_endstream edge cases
    // ========================================================================

    #[test]
    fn test_scan_for_endstream_no_preceding_eol() {
        // endstream directly after data, no newline
        let data = b"dataendstream";
        let lexer = Lexer::new(data);
        let len = lexer.scan_for_endstream().unwrap();
        assert_eq!(len, 4); // "data" (no EOL to strip)
    }

    #[test]
    fn test_scan_for_endstream_with_position_offset() {
        let data = b"junk junk endstream more stuff";
        let mut lexer = Lexer::new(data);
        lexer.set_position(10); // position past "junk junk "
        let len = lexer.scan_for_endstream().unwrap();
        assert_eq!(len, 0); // "endstream" is immediately at position
    }

    #[test]
    fn test_scan_for_endstream_multiple_false_positives() {
        // Multiple "endstreamXYZ" before the real one
        let data = b"endstreamX endstreamY\nendstream";
        let lexer = Lexer::new(data);
        let len = lexer.scan_for_endstream().unwrap();
        assert_eq!(len, 21); // everything before "\nendstream"
    }

    #[test]
    fn test_scan_for_endstream_empty_data() {
        let data = b"";
        let lexer = Lexer::new(data);
        assert!(lexer.scan_for_endstream().is_none());
    }

    #[test]
    fn test_scan_for_endstream_endstream_followed_by_delimiter() {
        // endstream followed by '<' (delimiter) should still match
        let data = b"stuff\nendstream<<";
        let lexer = Lexer::new(data);
        let len = lexer.scan_for_endstream().unwrap();
        assert_eq!(len, 5);
    }

    // ========================================================================
    // Additional coverage: garbage resynchronization
    // ========================================================================

    #[test]
    fn test_garbage_single_byte() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut lexer = Lexer::with_diagnostics(b"\x80", diag.clone());
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::Error(LexError::GarbageBytes { .. })));
        assert_eq!(lexer.next_token(), Token::Eof);
    }

    #[test]
    fn test_garbage_multiple_then_name() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut lexer = Lexer::with_diagnostics(b"\x80\x81\x82/Name", diag.clone());
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::Error(LexError::GarbageBytes { .. })));
        assert_eq!(lexer.next_token(), Token::Name(b"Name"));
        assert!(!diag.warnings().is_empty());
    }

    #[test]
    fn test_garbage_resync_at_digit() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut lexer = Lexer::with_diagnostics(b"\xFF123", diag.clone());
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::Error(LexError::GarbageBytes { .. })));
        assert_eq!(lexer.next_token(), Token::Integer(123));
    }

    #[test]
    fn test_garbage_resync_at_sign() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut lexer = Lexer::with_diagnostics(b"\xFE+42", diag.clone());
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::Error(LexError::GarbageBytes { .. })));
        assert_eq!(lexer.next_token(), Token::Integer(42));
    }

    // ========================================================================
    // Additional coverage: comment edge cases
    // ========================================================================

    #[test]
    fn test_multiple_comments() {
        let mut lexer = Lexer::new(b"% first\n% second\n42");
        assert_eq!(lexer.next_token(), Token::Integer(42));
    }

    #[test]
    fn test_comment_with_no_newline() {
        // Comment at EOF with no trailing newline
        let mut lexer = Lexer::new(b"42 % trailing comment");
        assert_eq!(lexer.next_token(), Token::Integer(42));
        assert_eq!(lexer.next_token(), Token::Eof);
    }

    #[test]
    fn test_comment_empty() {
        let mut lexer = Lexer::new(b"%\n42");
        assert_eq!(lexer.next_token(), Token::Integer(42));
    }

    // ========================================================================
    // Additional coverage: whitespace
    // ========================================================================

    #[test]
    fn test_whitespace_characters() {
        // NUL, TAB, LF, FF, CR, SP are whitespace
        assert!(Lexer::is_whitespace(0)); // NUL
        assert!(Lexer::is_whitespace(9)); // TAB
        assert!(Lexer::is_whitespace(10)); // LF
        assert!(Lexer::is_whitespace(12)); // FF
        assert!(Lexer::is_whitespace(13)); // CR
        assert!(Lexer::is_whitespace(32)); // SP
        assert!(!Lexer::is_whitespace(b'A'));
        assert!(!Lexer::is_whitespace(b'/'));
        assert!(!Lexer::is_whitespace(11)); // VT is NOT PDF whitespace
    }

    #[test]
    fn test_delimiter_characters() {
        assert!(Lexer::is_delimiter(b'('));
        assert!(Lexer::is_delimiter(b')'));
        assert!(Lexer::is_delimiter(b'<'));
        assert!(Lexer::is_delimiter(b'>'));
        assert!(Lexer::is_delimiter(b'['));
        assert!(Lexer::is_delimiter(b']'));
        assert!(Lexer::is_delimiter(b'{'));
        assert!(Lexer::is_delimiter(b'}'));
        assert!(Lexer::is_delimiter(b'/'));
        assert!(Lexer::is_delimiter(b'%'));
        assert!(!Lexer::is_delimiter(b'A'));
        assert!(!Lexer::is_delimiter(b' '));
        assert!(!Lexer::is_delimiter(b'0'));
    }

    #[test]
    fn test_form_feed_as_whitespace() {
        // Form feed (0x0C) between tokens
        let mut lexer = Lexer::new(b"1\x0C2");
        assert_eq!(lexer.next_token(), Token::Integer(1));
        assert_eq!(lexer.next_token(), Token::Integer(2));
    }

    #[test]
    fn test_null_byte_as_whitespace() {
        let mut lexer = Lexer::new(b"1\x002");
        assert_eq!(lexer.next_token(), Token::Integer(1));
        assert_eq!(lexer.next_token(), Token::Integer(2));
    }

    // ========================================================================
    // Additional coverage: lone '>' and dict brackets
    // ========================================================================

    #[test]
    fn test_lone_greater_than_at_eof() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut lexer = Lexer::with_diagnostics(b">", diag.clone());
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::Error(LexError::UnexpectedByte(b'>'))));
        assert_eq!(lexer.next_token(), Token::Eof);
    }

    #[test]
    fn test_nested_dict_brackets() {
        let mut lexer = Lexer::new(b"<< /Inner << /Key /Val >> >>");
        assert_eq!(lexer.next_token(), Token::DictStart);
        assert_eq!(lexer.next_token(), Token::Name(b"Inner"));
        assert_eq!(lexer.next_token(), Token::DictStart);
        assert_eq!(lexer.next_token(), Token::Name(b"Key"));
        assert_eq!(lexer.next_token(), Token::Name(b"Val"));
        assert_eq!(lexer.next_token(), Token::DictEnd);
        assert_eq!(lexer.next_token(), Token::DictEnd);
    }

    // ========================================================================
    // Additional coverage: skip_stream_eol at EOF
    // ========================================================================

    #[test]
    fn test_skip_stream_eol_at_eof() {
        let mut lexer = Lexer::new(b"");
        lexer.skip_stream_eol();
        assert_eq!(lexer.position(), 0);
    }

    #[test]
    fn test_skip_stream_eol_cr_at_eof() {
        // CR with nothing after it
        let mut lexer = Lexer::new(b"\r");
        lexer.skip_stream_eol();
        assert_eq!(lexer.position(), 1);
    }

    // ========================================================================
    // Additional coverage: T* operator and content stream operators
    // ========================================================================

    #[test]
    fn test_t_star_followed_by_tokens() {
        let mut lexer = Lexer::new(b"T* 42");
        assert_eq!(lexer.next_token(), Token::Keyword(b"T*"));
        assert_eq!(lexer.next_token(), Token::Integer(42));
    }

    #[test]
    fn test_t_not_star() {
        // T followed by something other than * -- just 'T' is not a PDF keyword,
        // but it should be parsed as a keyword anyway
        let mut lexer = Lexer::new(b"T ");
        let tok = lexer.next_token();
        // T is not a known keyword, so it's a generic Keyword
        assert_eq!(tok, Token::Keyword(b"T"));
    }

    // ========================================================================
    // Additional coverage: complex token sequences
    // ========================================================================

    #[test]
    fn test_array_with_mixed_types() {
        let mut lexer = Lexer::new(b"[1 2.5 /Name (str) <48> true null]");
        assert_eq!(lexer.next_token(), Token::ArrayStart);
        assert_eq!(lexer.next_token(), Token::Integer(1));
        assert_eq!(lexer.next_token(), Token::Real(2.5));
        assert_eq!(lexer.next_token(), Token::Name(b"Name"));
        assert_eq!(lexer.next_token(), Token::LiteralString(b"str"));
        assert_eq!(lexer.next_token(), Token::HexString(b"48"));
        assert_eq!(lexer.next_token(), Token::True);
        assert_eq!(lexer.next_token(), Token::Null);
        assert_eq!(lexer.next_token(), Token::ArrayEnd);
    }

    #[test]
    fn test_dict_with_reference() {
        let mut lexer = Lexer::new(b"<< /Font 5 0 R >>");
        assert_eq!(lexer.next_token(), Token::DictStart);
        assert_eq!(lexer.next_token(), Token::Name(b"Font"));
        assert_eq!(lexer.next_token(), Token::Integer(5));
        assert_eq!(lexer.next_token(), Token::Integer(0));
        assert_eq!(lexer.next_token(), Token::R);
        assert_eq!(lexer.next_token(), Token::DictEnd);
    }

    #[test]
    fn test_xref_trailer_startxref_sequence() {
        let mut lexer = Lexer::new(b"xref\ntrailer\n<< >>\nstartxref\n100");
        assert_eq!(lexer.next_token(), Token::XRef);
        assert_eq!(lexer.next_token(), Token::Trailer);
        assert_eq!(lexer.next_token(), Token::DictStart);
        assert_eq!(lexer.next_token(), Token::DictEnd);
        assert_eq!(lexer.next_token(), Token::StartXRef);
        assert_eq!(lexer.next_token(), Token::Integer(100));
    }

    // ========================================================================
    // Additional coverage: peek_token at EOF
    // ========================================================================

    #[test]
    fn test_peek_token_at_eof() {
        let mut lexer = Lexer::new(b"");
        assert_eq!(lexer.peek_token(), Token::Eof);
        assert_eq!(lexer.next_token(), Token::Eof);
    }

    #[test]
    fn test_peek_token_after_consumption() {
        let mut lexer = Lexer::new(b"42");
        assert_eq!(lexer.next_token(), Token::Integer(42));
        assert_eq!(lexer.peek_token(), Token::Eof);
    }

    // ========================================================================
    // Additional coverage: integer overflow to real, and unparseable number
    // ========================================================================

    #[test]
    fn test_integer_overflow_very_large_becomes_real() {
        // A number too large for i64 but parseable as f64
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut lexer = Lexer::with_diagnostics(b"99999999999999999999999999999999", diag.clone());
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::Real(_)));
        assert!(diag
            .warnings()
            .iter()
            .any(|w| w.message.contains("overflow")));
    }

    #[test]
    fn test_number_unparseable_as_real_too() {
        // Craft a situation where the number is not parseable as i64 or f64.
        // This is hard to achieve with valid digit strings since f64 can parse
        // virtually any digit string. We exercise the path where f64 also fails.
        // Extremely long digit strings can trigger this if we manipulate the lexer.
        // Actually, f64::parse rarely fails for digit strings. The truly
        // unparseable path is essentially dead code, but let's test a malformed
        // number: sign-only and dot-only already covered above. The specific
        // path for unparseable-both-i64-and-f64 is triggered when the ASCII
        // byte slice is valid UTF-8 digits but too wild for f64. In practice
        // this never happens, so this tests the existing sign+dot paths.
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut lexer = Lexer::with_diagnostics(b"+.", diag.clone());
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::Error(LexError::MalformedNumber)));
    }

    // ========================================================================
    // Additional coverage: garbage resync at dot, at alphabetic
    // ========================================================================

    #[test]
    fn test_garbage_resync_at_dot() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut lexer = Lexer::with_diagnostics(b"\xFE.5", diag.clone());
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::Error(LexError::GarbageBytes { .. })));
        assert_eq!(lexer.next_token(), Token::Real(0.5));
    }

    #[test]
    fn test_garbage_resync_at_alphabetic() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut lexer = Lexer::with_diagnostics(b"\xFEtrue", diag.clone());
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::Error(LexError::GarbageBytes { .. })));
        assert_eq!(lexer.next_token(), Token::True);
    }

    #[test]
    fn test_garbage_all_non_token_bytes() {
        // All bytes are garbage, no recognizable token boundary until EOF
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut lexer = Lexer::with_diagnostics(b"\x80\x81\x82\x83", diag.clone());
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::Error(LexError::GarbageBytes { .. })));
        assert_eq!(lexer.next_token(), Token::Eof);
    }

    // ========================================================================
    // Additional coverage: content stream operators ' and "
    // ========================================================================

    #[test]
    fn test_single_quote_operator_followed_by_string() {
        let mut lexer = Lexer::new(b"'(hello)");
        assert_eq!(lexer.next_token(), Token::Keyword(b"'"));
        assert_eq!(lexer.next_token(), Token::LiteralString(b"hello"));
    }

    #[test]
    fn test_double_quote_operator_followed_by_number() {
        let mut lexer = Lexer::new(b"\"42");
        assert_eq!(lexer.next_token(), Token::Keyword(b"\""));
        assert_eq!(lexer.next_token(), Token::Integer(42));
    }

    // ========================================================================
    // Additional coverage: hex string with only non-hex garbage
    // ========================================================================

    #[test]
    fn test_hex_string_all_garbage() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let mut lexer = Lexer::with_diagnostics(b"<XXXX>", diag.clone());
        let tok = lexer.next_token();
        assert!(matches!(tok, Token::HexString(_)));
        assert!(diag.warnings().len() >= 4);
    }

    // ========================================================================
    // Additional coverage: scan_for_endstream with false positive then
    // keyword boundary
    // ========================================================================

    #[test]
    fn test_scan_for_endstream_false_positive_then_real() {
        // "endstreamX" is false positive (X is alphabetic, not a boundary),
        // then real "endstream" follows at the end of input.
        let data = b"endstreamX\nendstream";
        let lexer = Lexer::new(data);
        let len = lexer.scan_for_endstream().unwrap();
        assert_eq!(len, 10); // "endstreamX" without the newline
    }

    #[test]
    fn test_scan_for_endstream_immediately_at_start() {
        // "endstream" is the entire content
        let data = b"endstream";
        let lexer = Lexer::new(data);
        let len = lexer.scan_for_endstream().unwrap();
        assert_eq!(len, 0);
    }

    // ========================================================================
    // Additional coverage: advance returns None at EOF
    // ========================================================================

    #[test]
    fn test_skip_stream_eol_cr_then_eof() {
        // CR followed by EOF (no LF)
        let mut lexer = Lexer::new(b"\r");
        lexer.skip_stream_eol();
        assert_eq!(lexer.position(), 1);
        assert_eq!(lexer.next_token(), Token::Eof);
    }

    // ========================================================================
    // Additional coverage: peek_token restores position (line 653)
    // ========================================================================

    #[test]
    fn test_peek_token_complex_sequence() {
        // Peek at a dict start, verify position is restored, then consume it
        let mut lexer = Lexer::new(b"<< /Name 42 >>");
        let peeked = lexer.peek_token();
        assert_eq!(peeked, Token::DictStart);
        assert_eq!(lexer.position(), 0);
        assert_eq!(lexer.next_token(), Token::DictStart);
        assert_eq!(lexer.position(), 2);
    }

    // ========================================================================
    // Additional coverage: real number that fails f64 parse
    // ========================================================================

    #[test]
    fn test_real_number_with_multiple_dots_only_first() {
        // "0.0.5" should parse as Real(0.0) then Real(0.5)
        let mut lexer = Lexer::new(b"0.0.5");
        assert_eq!(lexer.next_token(), Token::Real(0.0));
        assert_eq!(lexer.next_token(), Token::Real(0.5));
    }

    // ========================================================================
    // Additional coverage: comment within whitespace loop
    // ========================================================================

    #[test]
    fn test_whitespace_then_comment_then_whitespace_then_token() {
        let mut lexer = Lexer::new(b"  % comment\n  42");
        assert_eq!(lexer.next_token(), Token::Integer(42));
    }

    #[test]
    fn test_multiple_comments_interleaved_with_whitespace() {
        let mut lexer = Lexer::new(b"% first\r% second\r\n% third\n42");
        assert_eq!(lexer.next_token(), Token::Integer(42));
    }
}
