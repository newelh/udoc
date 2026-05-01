//! Streaming RTF tokenizer.
//!
//! Operates on `&[u8]` and emits zero-copy tokens without interpreting
//! control word semantics. Text tokens borrow directly from the input;
//! decoding to UTF-8 is the parser's job using codepage state.
//!
//! Follows the  pattern: zero-copy in the lexer, owned types
//! at the API boundary.
//!
//! The lexer handles `\bin` specially (must skip N raw bytes in the stream),
//! and recognizes `\'XX` hex escapes as their own token type. All other
//! control words (including `\u`) are emitted as `ControlWord` for the
//! parser to interpret.

use crate::error::{Error, Result};
use crate::hex_val;

/// Maximum length for a control word name (RTF spec says 32).
const MAX_CONTROL_WORD_LEN: usize = 32;

/// A token produced by the RTF lexer.
///
/// Borrows text and binary data directly from the input slice to avoid
/// allocations in the hot path. Control word names are `&str` slices
/// into the input (guaranteed ASCII alphabetic, thus valid UTF-8).
#[derive(Debug, Clone, Eq)]
pub enum Token<'a> {
    /// `{` -- begin group.
    GroupOpen,
    /// `}` -- end group.
    GroupClose,
    /// `\word` or `\wordN` -- a control word with optional integer parameter.
    ControlWord { name: &'a str, param: Option<i32> },
    /// `\X` where X is a non-alphabetic character (e.g. `\\`, `\{`, `\}`).
    /// Does NOT include `\'` (that's HexEscape).
    ControlSymbol(u8),
    /// `\'XX` -- two hex digits decoded to a byte value.
    HexEscape(u8),
    /// Raw text bytes. NOT decoded to UTF-8; the parser handles codepage
    /// conversion. Borrows from the input slice.
    Text(&'a [u8]),
    /// `\bin N` followed by N raw bytes. Borrows from the input slice.
    BinaryData(&'a [u8]),
}

// Cross-lifetime PartialEq so test assertions work with static literals.
impl<'a, 'b> PartialEq<Token<'b>> for Token<'a> {
    fn eq(&self, other: &Token<'b>) -> bool {
        match (self, other) {
            (Token::GroupOpen, Token::GroupOpen) => true,
            (Token::GroupClose, Token::GroupClose) => true,
            (
                Token::ControlWord {
                    name: n1,
                    param: p1,
                },
                Token::ControlWord {
                    name: n2,
                    param: p2,
                },
            ) => n1 == n2 && p1 == p2,
            (Token::ControlSymbol(a), Token::ControlSymbol(b)) => a == b,
            (Token::HexEscape(a), Token::HexEscape(b)) => a == b,
            (Token::Text(a), Token::Text(b)) => *a == *b,
            (Token::BinaryData(a), Token::BinaryData(b)) => *a == *b,
            _ => false,
        }
    }
}

/// Streaming RTF lexer over a byte slice.
pub struct Lexer<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    /// Create a new lexer over the given byte slice.
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Current byte offset in the input.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Return the next token, or `None` at EOF.
    pub fn next_token(&mut self) -> Result<Option<Token<'a>>> {
        self.skip_line_endings();

        if self.pos >= self.data.len() {
            return Ok(None);
        }

        let b = self.data[self.pos];
        match b {
            b'{' => {
                self.pos += 1;
                Ok(Some(Token::GroupOpen))
            }
            b'}' => {
                self.pos += 1;
                Ok(Some(Token::GroupClose))
            }
            b'\\' => self.read_control_sequence(),
            _ => self.read_text(),
        }
    }

    /// Skip CR and LF bytes (not significant outside text runs in RTF).
    fn skip_line_endings(&mut self) {
        while self.pos < self.data.len() {
            match self.data[self.pos] {
                0x0D | 0x0A => self.pos += 1,
                _ => break,
            }
        }
    }

    /// Peek at the current byte without advancing.
    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    /// Read a control sequence starting after the `\`.
    fn read_control_sequence(&mut self) -> Result<Option<Token<'a>>> {
        let start = self.pos;
        self.pos += 1; // skip `\`

        let next = match self.peek() {
            Some(b) => b,
            None => {
                // Backslash at EOF: treat as literal backslash.
                return Ok(Some(Token::ControlSymbol(b'\\')));
            }
        };

        if next == b'\'' {
            return self.read_hex_escape();
        }

        if next.is_ascii_alphabetic() {
            return self.read_control_word(start);
        }

        // Control symbol: `\X` for non-alpha X
        self.pos += 1;
        Ok(Some(Token::ControlSymbol(next)))
    }

    /// Read `\'XX` hex escape. `self.pos` is on the `'`.
    ///
    /// On malformed hex (missing or invalid digits), emits a replacement
    /// byte (0x3F = '?') instead of returning an error. This matches the
    /// project's "be lenient, log warnings" philosophy for recoverable
    /// parse issues.
    fn read_hex_escape(&mut self) -> Result<Option<Token<'a>>> {
        self.pos += 1; // skip `'`

        let h1 = self.peek();
        let h2 = if h1.is_some() {
            self.data.get(self.pos + 1).copied()
        } else {
            None
        };

        match (h1.and_then(hex_val), h2.and_then(hex_val)) {
            (Some(hi), Some(lo)) => {
                self.pos += 2;
                Ok(Some(Token::HexEscape((hi << 4) | lo)))
            }
            _ => {
                // Malformed hex escape. Advance past a valid first hex digit
                // (if any) so we don't re-read it, then emit a replacement
                // byte (0x3F = '?'). This means \'az consumes the 'a' and
                // leaves 'z' as text, while \'zz consumes neither and both
                // 'z's become text. The asymmetry is intentional: we consume
                // as much of the malformed escape as we can identify.
                if h1.filter(|b| hex_val(*b).is_some()).is_some() {
                    self.pos += 1;
                }
                Ok(Some(Token::HexEscape(0x3F)))
            }
        }
    }

    /// Read a control word (name + optional numeric param + optional space
    /// delimiter). `self.pos` is on the first alpha char.
    fn read_control_word(&mut self, start: usize) -> Result<Option<Token<'a>>> {
        let name_start = self.pos;
        let mut name_len = 0;

        // Read [a-z]+ (lowercase only per RTF spec, but be lenient with case)
        while self.pos < self.data.len()
            && self.data[self.pos].is_ascii_alphabetic()
            && name_len < MAX_CONTROL_WORD_LEN
        {
            self.pos += 1;
            name_len += 1;
        }

        // ASCII alphabetic bytes are always valid UTF-8.
        let name = std::str::from_utf8(&self.data[name_start..name_start + name_len]).unwrap_or("");

        // Read optional signed integer parameter
        let param = self.read_optional_param();

        // Consume optional space delimiter (one space after control word
        // is a delimiter, not content)
        if self.pos < self.data.len() && self.data[self.pos] == b' ' {
            self.pos += 1;
        }

        // Special handling: \bin requires reading N raw bytes
        if name == "bin" {
            return self.read_binary_data(param, start);
        }

        Ok(Some(Token::ControlWord { name, param }))
    }

    /// Read an optional signed integer parameter after a control word name.
    fn read_optional_param(&mut self) -> Option<i32> {
        if self.pos >= self.data.len() {
            return None;
        }

        let negative = self.data[self.pos] == b'-';
        if negative {
            // Only consume '-' if followed by a digit
            if self.pos + 1 < self.data.len() && self.data[self.pos + 1].is_ascii_digit() {
                self.pos += 1;
            } else {
                return None;
            }
        }

        if self.pos >= self.data.len() || !self.data[self.pos].is_ascii_digit() {
            return None;
        }

        let mut value: i64 = 0;
        while self.pos < self.data.len() && self.data[self.pos].is_ascii_digit() {
            value = value
                .saturating_mul(10)
                .saturating_add((self.data[self.pos] - b'0') as i64);
            self.pos += 1;
            if value > i32::MAX as i64 + 1 {
                // Skip remaining digits to avoid infinite loops on huge numbers.
                while self.pos < self.data.len() && self.data[self.pos].is_ascii_digit() {
                    self.pos += 1;
                }
                break;
            }
        }

        if negative {
            value = -value;
        }

        Some(value.clamp(i32::MIN as i64, i32::MAX as i64) as i32)
    }

    /// Read N raw bytes for `\bin N`. `param` is the byte count from the
    /// control word parameter.
    fn read_binary_data(&mut self, param: Option<i32>, start: usize) -> Result<Option<Token<'a>>> {
        let count = param.unwrap_or(0);
        if count < 0 {
            return Err(Error::new(format!(
                "parse error at offset {start}: expected non-negative byte count for \\bin, found {count}"
            )));
        }
        let count = count as usize;

        if self.pos + count > self.data.len() {
            return Err(Error::new(format!(
                "parse error at offset {start}: expected {count} bytes of binary data, found only {} bytes remaining",
                self.data.len() - self.pos
            )));
        }

        let bytes = &self.data[self.pos..self.pos + count];
        self.pos += count;
        Ok(Some(Token::BinaryData(bytes)))
    }

    /// Read consecutive text bytes until a special character or EOF.
    fn read_text(&mut self) -> Result<Option<Token<'a>>> {
        let start = self.pos;
        while self.pos < self.data.len() {
            match self.data[self.pos] {
                b'{' | b'}' | b'\\' | 0x0D | 0x0A => break,
                _ => self.pos += 1,
            }
        }

        if self.pos > start {
            Ok(Some(Token::Text(&self.data[start..self.pos])))
        } else {
            // Shouldn't happen, but be safe
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lex all tokens from input, collecting into a Vec.
    fn lex_all<'a>(input: &'a [u8]) -> Result<Vec<Token<'a>>> {
        let mut lexer = Lexer::new(input);
        let mut tokens = Vec::new();
        while let Some(tok) = lexer.next_token()? {
            tokens.push(tok);
        }
        Ok(tokens)
    }

    #[test]
    fn empty_input() {
        let tokens = lex_all(b"").unwrap();
        assert!(tokens.is_empty());
    }

    #[test]
    fn group_open_close() {
        let tokens = lex_all(b"{}").unwrap();
        assert_eq!(tokens, vec![Token::GroupOpen, Token::GroupClose]);
    }

    #[test]
    fn nested_groups() {
        let tokens = lex_all(b"{{}}").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::GroupOpen,
                Token::GroupOpen,
                Token::GroupClose,
                Token::GroupClose,
            ]
        );
    }

    #[test]
    fn basic_text() {
        let tokens = lex_all(b"hello world").unwrap();
        assert_eq!(tokens, vec![Token::Text(b"hello world")]);
    }

    #[test]
    fn text_split_by_group() {
        let tokens = lex_all(b"ab{cd}ef").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Text(b"ab"),
                Token::GroupOpen,
                Token::Text(b"cd"),
                Token::GroupClose,
                Token::Text(b"ef"),
            ]
        );
    }

    #[test]
    fn control_word_no_param() {
        let tokens = lex_all(b"\\par ").unwrap();
        assert_eq!(
            tokens,
            vec![Token::ControlWord {
                name: "par",
                param: None,
            }]
        );
    }

    #[test]
    fn control_word_with_param() {
        let tokens = lex_all(b"\\fs24 ").unwrap();
        assert_eq!(
            tokens,
            vec![Token::ControlWord {
                name: "fs",
                param: Some(24),
            }]
        );
    }

    #[test]
    fn control_word_negative_param() {
        let tokens = lex_all(b"\\li-720 ").unwrap();
        assert_eq!(
            tokens,
            vec![Token::ControlWord {
                name: "li",
                param: Some(-720),
            }]
        );
    }

    #[test]
    fn control_word_zero_param() {
        let tokens = lex_all(b"\\b0").unwrap();
        assert_eq!(
            tokens,
            vec![Token::ControlWord {
                name: "b",
                param: Some(0),
            }]
        );
    }

    #[test]
    fn control_word_space_delimiter_consumed() {
        // Space after control word is delimiter, not text
        let tokens = lex_all(b"\\b text").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::ControlWord {
                    name: "b",
                    param: None,
                },
                Token::Text(b"text"),
            ]
        );
    }

    #[test]
    fn control_word_no_space_delimiter() {
        // Non-space after parameterless control word that's followed by
        // non-alpha is not consumed
        let tokens = lex_all(b"\\b{text}").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::ControlWord {
                    name: "b",
                    param: None,
                },
                Token::GroupOpen,
                Token::Text(b"text"),
                Token::GroupClose,
            ]
        );
    }

    #[test]
    fn control_word_param_no_space_followed_by_text() {
        let tokens = lex_all(b"\\fs24text").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::ControlWord {
                    name: "fs",
                    param: Some(24),
                },
                Token::Text(b"text"),
            ]
        );
    }

    #[test]
    fn unicode_control_word() {
        // \u8364 is the euro sign. Lexer emits it as ControlWord; parser
        // recognizes name="u" and treats the param as a codepoint.
        let tokens = lex_all(b"\\u8364 ").unwrap();
        assert_eq!(
            tokens,
            vec![Token::ControlWord {
                name: "u",
                param: Some(8364),
            }]
        );
    }

    #[test]
    fn unicode_negative() {
        // Codepoints > 32767 are stored as negative signed 16-bit values
        let tokens = lex_all(b"\\u-4894 ").unwrap();
        assert_eq!(
            tokens,
            vec![Token::ControlWord {
                name: "u",
                param: Some(-4894),
            }]
        );
    }

    #[test]
    fn hex_escape_valid() {
        let tokens = lex_all(b"\\'e9").unwrap();
        assert_eq!(tokens, vec![Token::HexEscape(0xe9)]);
    }

    #[test]
    fn hex_escape_uppercase() {
        let tokens = lex_all(b"\\'C0").unwrap();
        assert_eq!(tokens, vec![Token::HexEscape(0xc0)]);
    }

    #[test]
    fn hex_escape_mixed_case() {
        let tokens = lex_all(b"\\'aB").unwrap();
        assert_eq!(tokens, vec![Token::HexEscape(0xab)]);
    }

    #[test]
    fn hex_escape_invalid_emits_replacement() {
        // Malformed hex now emits a replacement byte instead of erroring.
        let tokens = lex_all(b"\\'zz").unwrap();
        assert_eq!(tokens, vec![Token::HexEscape(0x3F), Token::Text(b"zz")]);
    }

    #[test]
    fn hex_escape_truncated_emits_replacement() {
        // Truncated hex now emits replacement instead of erroring.
        let tokens = lex_all(b"\\'a").unwrap();
        assert_eq!(tokens, vec![Token::HexEscape(0x3F)]);
    }

    #[test]
    fn control_symbol_backslash() {
        let tokens = lex_all(b"\\\\").unwrap();
        assert_eq!(tokens, vec![Token::ControlSymbol(b'\\')]);
    }

    #[test]
    fn control_symbol_open_brace() {
        let tokens = lex_all(b"\\{").unwrap();
        assert_eq!(tokens, vec![Token::ControlSymbol(b'{')]);
    }

    #[test]
    fn control_symbol_close_brace() {
        let tokens = lex_all(b"\\}").unwrap();
        assert_eq!(tokens, vec![Token::ControlSymbol(b'}')]);
    }

    #[test]
    fn control_symbol_tilde() {
        // \~ = non-breaking space
        let tokens = lex_all(b"\\~").unwrap();
        assert_eq!(tokens, vec![Token::ControlSymbol(b'~')]);
    }

    #[test]
    fn control_symbol_hyphen() {
        // \- = optional hyphen
        let tokens = lex_all(b"\\-").unwrap();
        assert_eq!(tokens, vec![Token::ControlSymbol(b'-')]);
    }

    #[test]
    fn control_symbol_underscore() {
        // \_ = non-breaking hyphen
        let tokens = lex_all(b"\\_").unwrap();
        assert_eq!(tokens, vec![Token::ControlSymbol(b'_')]);
    }

    #[test]
    fn binary_data() {
        let input = b"\\bin5 ABCDE";
        let tokens = lex_all(input).unwrap();
        assert_eq!(tokens, vec![Token::BinaryData(b"ABCDE")]);
    }

    #[test]
    fn binary_data_zero_length() {
        let tokens = lex_all(b"\\bin0 rest").unwrap();
        assert_eq!(tokens, vec![Token::BinaryData(b""), Token::Text(b"rest")]);
    }

    #[test]
    fn binary_data_with_special_bytes() {
        // Binary data should include { and } without interpreting them
        let mut input = Vec::new();
        input.extend_from_slice(b"\\bin3 ");
        input.push(b'{');
        input.push(0x00);
        input.push(b'}');
        let tokens = lex_all(&input).unwrap();
        assert_eq!(tokens.len(), 1);
        match &tokens[0] {
            Token::BinaryData(data) => assert_eq!(*data, &[b'{', 0x00, b'}'][..]),
            other => panic!("expected BinaryData, got {other:?}"),
        }
    }

    #[test]
    fn binary_data_truncated() {
        let result = lex_all(b"\\bin10 AB");
        assert!(result.is_err());
    }

    #[test]
    fn binary_data_negative_count() {
        let result = lex_all(b"\\bin-5 ");
        assert!(result.is_err());
    }

    #[test]
    fn cr_lf_ignored() {
        let tokens = lex_all(b"ab\r\ncd").unwrap();
        assert_eq!(tokens, vec![Token::Text(b"ab"), Token::Text(b"cd")]);
    }

    #[test]
    fn cr_only_ignored() {
        let tokens = lex_all(b"ab\rcd").unwrap();
        assert_eq!(tokens, vec![Token::Text(b"ab"), Token::Text(b"cd")]);
    }

    #[test]
    fn lf_only_ignored() {
        let tokens = lex_all(b"ab\ncd").unwrap();
        assert_eq!(tokens, vec![Token::Text(b"ab"), Token::Text(b"cd")]);
    }

    #[test]
    fn leading_trailing_crlf() {
        let tokens = lex_all(b"\r\nhello\r\n").unwrap();
        assert_eq!(tokens, vec![Token::Text(b"hello")]);
    }

    #[test]
    fn backslash_at_eof() {
        let tokens = lex_all(b"\\").unwrap();
        assert_eq!(tokens, vec![Token::ControlSymbol(b'\\')]);
    }

    #[test]
    fn control_word_at_eof() {
        // Control word with no trailing space or content
        let tokens = lex_all(b"\\par").unwrap();
        assert_eq!(
            tokens,
            vec![Token::ControlWord {
                name: "par",
                param: None,
            }]
        );
    }

    #[test]
    fn control_word_param_at_eof() {
        let tokens = lex_all(b"\\fs24").unwrap();
        assert_eq!(
            tokens,
            vec![Token::ControlWord {
                name: "fs",
                param: Some(24),
            }]
        );
    }

    #[test]
    fn realistic_rtf_fragment() {
        let input = b"{\\rtf1\\ansi{\\b Hello} world\\par}";
        let tokens = lex_all(input).unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::GroupOpen,
                Token::ControlWord {
                    name: "rtf",
                    param: Some(1),
                },
                Token::ControlWord {
                    name: "ansi",
                    param: None,
                },
                Token::GroupOpen,
                Token::ControlWord {
                    name: "b",
                    param: None,
                },
                Token::Text(b"Hello"),
                Token::GroupClose,
                Token::Text(b" world"),
                Token::ControlWord {
                    name: "par",
                    param: None,
                },
                Token::GroupClose,
            ]
        );
    }

    #[test]
    fn hex_in_text() {
        let tokens = lex_all(b"caf\\'e9").unwrap();
        assert_eq!(tokens, vec![Token::Text(b"caf"), Token::HexEscape(0xe9)]);
    }

    #[test]
    fn multiple_control_words() {
        let tokens = lex_all(b"\\f0\\fs20\\cf1 ").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::ControlWord {
                    name: "f",
                    param: Some(0),
                },
                Token::ControlWord {
                    name: "fs",
                    param: Some(20),
                },
                Token::ControlWord {
                    name: "cf",
                    param: Some(1),
                },
            ]
        );
    }

    #[test]
    fn control_word_followed_by_group() {
        let tokens = lex_all(b"\\i{text}").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::ControlWord {
                    name: "i",
                    param: None,
                },
                Token::GroupOpen,
                Token::Text(b"text"),
                Token::GroupClose,
            ]
        );
    }

    #[test]
    fn minus_not_followed_by_digit() {
        // \li- should parse as \li (no param), then text "-"
        // If minus is not followed by digit, param is None and the "-" stays.
        let tokens = lex_all(b"\\li-x").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::ControlWord {
                    name: "li",
                    param: None,
                },
                Token::Text(b"-x"),
            ]
        );
    }

    #[test]
    fn only_whitespace_crlf() {
        let tokens = lex_all(b"\r\n\r\n").unwrap();
        assert!(tokens.is_empty());
    }

    #[test]
    fn tab_is_text() {
        let tokens = lex_all(b"\t").unwrap();
        assert_eq!(tokens, vec![Token::Text(b"\t")]);
    }

    #[test]
    fn hex_escape_00() {
        let tokens = lex_all(b"\\'00").unwrap();
        assert_eq!(tokens, vec![Token::HexEscape(0x00)]);
    }

    #[test]
    fn hex_escape_ff() {
        let tokens = lex_all(b"\\'ff").unwrap();
        assert_eq!(tokens, vec![Token::HexEscape(0xff)]);
    }

    #[test]
    fn control_symbol_asterisk() {
        // \* marks an ignorable destination
        let tokens = lex_all(b"\\*").unwrap();
        assert_eq!(tokens, vec![Token::ControlSymbol(b'*')]);
    }

    #[test]
    fn bin_followed_by_more_tokens() {
        let input = b"\\bin3 ABCmore text";
        let tokens = lex_all(input).unwrap();
        assert_eq!(
            tokens,
            vec![Token::BinaryData(b"ABC"), Token::Text(b"more text")]
        );
    }

    #[test]
    fn consecutive_hex_escapes() {
        let tokens = lex_all(b"\\'c3\\'a9").unwrap();
        assert_eq!(tokens, vec![Token::HexEscape(0xc3), Token::HexEscape(0xa9)]);
    }

    #[test]
    fn control_word_long_name_truncated() {
        // Names longer than 32 chars should stop reading at the limit
        let long_name = "a".repeat(40);
        let input = format!("\\{long_name} ");
        let tokens = lex_all(input.as_bytes()).unwrap();
        match &tokens[0] {
            Token::ControlWord { name, .. } => {
                assert_eq!(name.len(), MAX_CONTROL_WORD_LEN);
            }
            _ => panic!("expected ControlWord"),
        }
    }

    #[test]
    fn overflow_param_clamped() {
        // A param with many digits should be clamped to i32 range.
        let input = b"\\fs99999999999999 ";
        let tokens = lex_all(input).unwrap();
        match &tokens[0] {
            Token::ControlWord { param: Some(v), .. } => {
                assert_eq!(*v, i32::MAX);
            }
            _ => panic!("expected ControlWord with param"),
        }
    }
}
