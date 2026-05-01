//! PDF font encoding tables and Adobe Glyph List.
//!
//! Provides the standard encoding tables (WinAnsi, MacRoman, Standard)
//! and the Adobe Glyph List (AGL) for mapping glyph names to Unicode.

mod agl_data;

/// A font encoding for mapping byte codes to characters.
#[derive(Debug)]
pub enum Encoding {
    /// One of the predefined standard encodings.
    Standard(StandardEncoding),
    /// A precomputed 256-entry lookup table merging base encoding with /Differences.
    Custom {
        /// Byte code -> Unicode character (None when no mapping is defined).
        table: Box<[Option<char>; 256]>,
    },
}

/// Predefined encoding types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardEncoding {
    /// WinAnsiEncoding (Windows code page 1252 variant).
    WinAnsi,
    /// MacRomanEncoding.
    MacRoman,
    /// MacExpertEncoding.
    MacExpert,
    /// StandardEncoding (Adobe standard).
    Standard,
    /// Font's built-in encoding (no explicit /Encoding).
    BuiltIn,
}

impl Encoding {
    /// Build a custom encoding by overlaying differences onto a base encoding.
    ///
    /// Precomputes a 256-entry lookup table so that `lookup()` is O(1).
    pub fn custom(base: StandardEncoding, differences: &[(u8, char)]) -> Encoding {
        let mut table = [None; 256];
        for code in 0..=255u8 {
            table[code as usize] = base.lookup(code);
        }
        for &(code, ch) in differences {
            table[code as usize] = Some(ch);
        }
        Encoding::Custom {
            table: Box::new(table),
        }
    }

    /// Look up a single-byte character code.
    pub fn lookup(&self, code: u8) -> Option<char> {
        match self {
            Encoding::Standard(std_enc) => std_enc.lookup(code),
            Encoding::Custom { table } => table[code as usize],
        }
    }
}

impl StandardEncoding {
    /// Look up a byte code in this standard encoding.
    pub fn lookup(self, code: u8) -> Option<char> {
        match self {
            StandardEncoding::WinAnsi => winansi_lookup(code),
            StandardEncoding::MacRoman => macroman_lookup(code),
            StandardEncoding::Standard => standard_lookup(code),
            StandardEncoding::MacExpert => None, // deferred: no corpus files use MacExpert
            StandardEncoding::BuiltIn => None,   // no mapping available
        }
    }
}

/// Look up a glyph name in the Adobe Glyph List.
///
/// Returns the Unicode character for a standard glyph name.
/// Uses binary search on a sorted static table.
pub fn agl_lookup(name: &str) -> Option<char> {
    agl_data::AGL_TABLE
        .binary_search_by_key(&name, |&(n, _)| n)
        .ok()
        .and_then(|idx| char::from_u32(agl_data::AGL_TABLE[idx].1))
}

/// Reverse AGL: map a Unicode character to its PostScript glyph name.
/// Returns the first matching AGL entry.
pub fn char_to_glyph_name(ch: char) -> Option<&'static str> {
    let code = ch as u32;
    agl_data::AGL_TABLE
        .iter()
        .find(|&&(_, c)| c == code)
        .map(|&(name, _)| name)
}

/// Parse a glyph name to a Unicode character.
///
/// Tries multiple strategies per Adobe Tech Note #5094:
/// 1. AGL lookup (standard glyph names)
/// 2. Underscore-separated ligature names (TeX convention, e.g. "f_i" -> fi)
/// 3. "uniXXXX" pattern (4+ hex digits, BMP or supplementary)
/// 4. "u" + 4-6 hex digits (alternate Unicode prefix)
/// 5. TeX-specific glyph names not in AGL (CMSY/CMMI /Differences)
///
/// Returns None if the name can't be resolved.
pub fn parse_glyph_name(name: &str) -> Option<char> {
    // 1. Try AGL first (fast path, covers ~600 standard names)
    if let Some(c) = agl_lookup(name) {
        return Some(c);
    }

    // 2. Underscore-separated ligature names (TeX convention).
    // TeX fonts commonly use names like "f_i", "f_f_l" instead of "fi", "ffl".
    // Map these to the corresponding Unicode ligature codepoints.
    if let Some(c) = parse_underscore_ligature(name) {
        return Some(c);
    }

    // 3. "uniXXXX" or "uniXXXXXX" pattern (Adobe Tech Note #5094)
    // Must be exactly 4, 5, or 6 hex digits after "uni"
    if let Some(hex) = name.strip_prefix("uni") {
        if (4..=6).contains(&hex.len()) && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            if let Ok(code) = u32::from_str_radix(hex, 16) {
                return char::from_u32(code);
            }
        }
    }

    // 4. "uXXXX" to "uXXXXXX" pattern (alternate prefix, some PDF generators)
    if let Some(hex) = name.strip_prefix('u') {
        if (4..=6).contains(&hex.len()) && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            if let Ok(code) = u32::from_str_radix(hex, 16) {
                return char::from_u32(code);
            }
        }
    }

    // 5. TeX-specific glyph names not in AGL. CM math fonts use names like
    // "minusplus", "circledot", "mapsto" in /Differences arrays that the
    // Adobe Glyph List doesn't cover.
    if let Some(c) = super::math_encodings::tex_glyph_lookup(name) {
        return Some(c);
    }

    None
}

/// Parse underscore-separated ligature glyph names to Unicode ligature chars.
///
/// TeX fonts use names like "f_i" instead of the AGL standard "fi".
/// These map to the Unicode Alphabetic Presentation Forms block (U+FB00-FB04).
fn parse_underscore_ligature(name: &str) -> Option<char> {
    match name {
        "f_f" => Some('\u{FB00}'),   // ff
        "f_i" => Some('\u{FB01}'),   // fi
        "f_l" => Some('\u{FB02}'),   // fl
        "f_f_i" => Some('\u{FB03}'), // ffi
        "f_f_l" => Some('\u{FB04}'), // ffl
        "s_t" => Some('\u{FB06}'),   // st
        "f_t" => None,               // no Unicode ligature for ft
        _ => None,
    }
}

/// Decompose a Unicode ligature character into its component characters.
///
/// Unicode Alphabetic Presentation Forms (U+FB00-FB06) are compatibility
/// ligatures that should be decomposed to plain ASCII for text extraction.
/// Returns None if the character is not a decomposable ligature.
pub fn decompose_ligature(c: char) -> Option<&'static str> {
    match c {
        '\u{FB00}' => Some("ff"),
        '\u{FB01}' => Some("fi"),
        '\u{FB02}' => Some("fl"),
        '\u{FB03}' => Some("ffi"),
        '\u{FB04}' => Some("ffl"),
        '\u{FB05}' => Some("st"), // long s + t
        '\u{FB06}' => Some("st"),
        _ => None,
    }
}

/// WinAnsiEncoding: Windows code page 1252 variant for PDF.
///
/// Codes 0x00-0x1F and 0x7F-0x9F have special mappings per PDF spec.
/// See PDF Reference Table D.1.
fn winansi_lookup(code: u8) -> Option<char> {
    // The 0x80-0x9F range has special WinAnsi mappings
    match code {
        0x00..=0x1F => None, // control chars, undefined
        0x7F => None,        // DEL, undefined
        // WinAnsi special range (0x80-0x9F)
        0x80 => Some('\u{20AC}'), // Euro sign
        0x81 => None,             // undefined
        0x82 => Some('\u{201A}'), // single low-9 quotation mark
        0x83 => Some('\u{0192}'), // Latin small letter f with hook
        0x84 => Some('\u{201E}'), // double low-9 quotation mark
        0x85 => Some('\u{2026}'), // horizontal ellipsis
        0x86 => Some('\u{2020}'), // dagger
        0x87 => Some('\u{2021}'), // double dagger
        0x88 => Some('\u{02C6}'), // modifier letter circumflex accent
        0x89 => Some('\u{2030}'), // per mille sign
        0x8A => Some('\u{0160}'), // Latin capital letter S with caron
        0x8B => Some('\u{2039}'), // single left-pointing angle quotation mark
        0x8C => Some('\u{0152}'), // Latin capital ligature OE
        0x8D => None,             // undefined
        0x8E => Some('\u{017D}'), // Latin capital letter Z with caron
        0x8F => None,             // undefined
        0x90 => None,             // undefined
        0x91 => Some('\u{2018}'), // left single quotation mark
        0x92 => Some('\u{2019}'), // right single quotation mark
        0x93 => Some('\u{201C}'), // left double quotation mark
        0x94 => Some('\u{201D}'), // right double quotation mark
        0x95 => Some('\u{2022}'), // bullet
        0x96 => Some('\u{2013}'), // en dash
        0x97 => Some('\u{2014}'), // em dash
        0x98 => Some('\u{02DC}'), // small tilde
        0x99 => Some('\u{2122}'), // trade mark sign
        0x9A => Some('\u{0161}'), // Latin small letter s with caron
        0x9B => Some('\u{203A}'), // single right-pointing angle quotation mark
        0x9C => Some('\u{0153}'), // Latin small ligature oe
        0x9D => None,             // undefined
        0x9E => Some('\u{017E}'), // Latin small letter z with caron
        0x9F => Some('\u{0178}'), // Latin capital letter Y with diaeresis
        // 0x20-0x7E and 0xA0-0xFF: same as ISO 8859-1 / Unicode
        _ => char::from_u32(code as u32),
    }
}

/// MacRomanEncoding: Mac OS Roman for PDF.
///
/// See PDF Reference Table D.2.
fn macroman_lookup(code: u8) -> Option<char> {
    match code {
        0x00..=0x1F => None,
        0x7F => None,
        // Standard ASCII range
        0x20..=0x7E => char::from_u32(code as u32),
        // Mac Roman special characters (0x80-0xFF)
        0x80 => Some('\u{00C4}'), // A-diaeresis
        0x81 => Some('\u{00C5}'), // A-ring
        0x82 => Some('\u{00C7}'), // C-cedilla
        0x83 => Some('\u{00C9}'), // E-acute
        0x84 => Some('\u{00D1}'), // N-tilde
        0x85 => Some('\u{00D6}'), // O-diaeresis
        0x86 => Some('\u{00DC}'), // U-diaeresis
        0x87 => Some('\u{00E1}'), // a-acute
        0x88 => Some('\u{00E0}'), // a-grave
        0x89 => Some('\u{00E2}'), // a-circumflex
        0x8A => Some('\u{00E4}'), // a-diaeresis
        0x8B => Some('\u{00E3}'), // a-tilde
        0x8C => Some('\u{00E5}'), // a-ring
        0x8D => Some('\u{00E7}'), // c-cedilla
        0x8E => Some('\u{00E9}'), // e-acute
        0x8F => Some('\u{00E8}'), // e-grave
        0x90 => Some('\u{00EA}'), // e-circumflex
        0x91 => Some('\u{00EB}'), // e-diaeresis
        0x92 => Some('\u{00ED}'), // i-acute
        0x93 => Some('\u{00EC}'), // i-grave
        0x94 => Some('\u{00EE}'), // i-circumflex
        0x95 => Some('\u{00EF}'), // i-diaeresis
        0x96 => Some('\u{00F1}'), // n-tilde
        0x97 => Some('\u{00F3}'), // o-acute
        0x98 => Some('\u{00F2}'), // o-grave
        0x99 => Some('\u{00F4}'), // o-circumflex
        0x9A => Some('\u{00F6}'), // o-diaeresis
        0x9B => Some('\u{00F5}'), // o-tilde
        0x9C => Some('\u{00FA}'), // u-acute
        0x9D => Some('\u{00F9}'), // u-grave
        0x9E => Some('\u{00FB}'), // u-circumflex
        0x9F => Some('\u{00FC}'), // u-diaeresis
        0xA0 => Some('\u{2020}'), // dagger
        0xA1 => Some('\u{00B0}'), // degree sign
        0xA2 => Some('\u{00A2}'), // cent sign
        0xA3 => Some('\u{00A3}'), // pound sign
        0xA4 => Some('\u{00A7}'), // section sign
        0xA5 => Some('\u{2022}'), // bullet
        0xA6 => Some('\u{00B6}'), // pilcrow sign
        0xA7 => Some('\u{00DF}'), // sharp s
        0xA8 => Some('\u{00AE}'), // registered sign
        0xA9 => Some('\u{00A9}'), // copyright sign
        0xAA => Some('\u{2122}'), // trade mark sign
        0xAB => Some('\u{00B4}'), // acute accent
        0xAC => Some('\u{00A8}'), // diaeresis
        0xAD => Some('\u{2260}'), // not equal to
        0xAE => Some('\u{00C6}'), // AE
        0xAF => Some('\u{00D8}'), // O-stroke
        0xB0 => Some('\u{221E}'), // infinity
        0xB1 => Some('\u{00B1}'), // plus-minus sign
        0xB2 => Some('\u{2264}'), // less-than or equal to
        0xB3 => Some('\u{2265}'), // greater-than or equal to
        0xB4 => Some('\u{00A5}'), // yen sign
        0xB5 => Some('\u{00B5}'), // micro sign
        0xB6 => Some('\u{2202}'), // partial differential
        0xB7 => Some('\u{2211}'), // n-ary summation
        0xB8 => Some('\u{220F}'), // n-ary product
        0xB9 => Some('\u{03C0}'), // pi
        0xBA => Some('\u{222B}'), // integral
        0xBB => Some('\u{00AA}'), // feminine ordinal indicator
        0xBC => Some('\u{00BA}'), // masculine ordinal indicator
        0xBD => Some('\u{2126}'), // ohm sign
        0xBE => Some('\u{00E6}'), // ae
        0xBF => Some('\u{00F8}'), // o-stroke
        0xC0 => Some('\u{00BF}'), // inverted question mark
        0xC1 => Some('\u{00A1}'), // inverted exclamation mark
        0xC2 => Some('\u{00AC}'), // not sign
        0xC3 => Some('\u{221A}'), // square root
        0xC4 => Some('\u{0192}'), // f with hook
        0xC5 => Some('\u{2248}'), // almost equal to
        0xC6 => Some('\u{2206}'), // increment
        0xC7 => Some('\u{00AB}'), // left guillemet
        0xC8 => Some('\u{00BB}'), // right guillemet
        0xC9 => Some('\u{2026}'), // horizontal ellipsis
        0xCA => Some('\u{00A0}'), // no-break space
        0xCB => Some('\u{00C0}'), // A-grave
        0xCC => Some('\u{00C3}'), // A-tilde
        0xCD => Some('\u{00D5}'), // O-tilde
        0xCE => Some('\u{0152}'), // OE
        0xCF => Some('\u{0153}'), // oe
        0xD0 => Some('\u{2013}'), // en dash
        0xD1 => Some('\u{2014}'), // em dash
        0xD2 => Some('\u{201C}'), // left double quotation mark
        0xD3 => Some('\u{201D}'), // right double quotation mark
        0xD4 => Some('\u{2018}'), // left single quotation mark
        0xD5 => Some('\u{2019}'), // right single quotation mark
        0xD6 => Some('\u{00F7}'), // division sign
        0xD7 => Some('\u{25CA}'), // lozenge
        0xD8 => Some('\u{00FF}'), // y-diaeresis
        0xD9 => Some('\u{0178}'), // Y-diaeresis
        0xDA => Some('\u{2044}'), // fraction slash
        0xDB => Some('\u{20AC}'), // Euro sign
        0xDC => Some('\u{2039}'), // single left-pointing angle quotation mark
        0xDD => Some('\u{203A}'), // single right-pointing angle quotation mark
        0xDE => Some('\u{FB01}'), // fi ligature
        0xDF => Some('\u{FB02}'), // fl ligature
        0xE0 => Some('\u{2021}'), // double dagger
        0xE1 => Some('\u{00B7}'), // middle dot
        0xE2 => Some('\u{201A}'), // single low-9 quotation mark
        0xE3 => Some('\u{201E}'), // double low-9 quotation mark
        0xE4 => Some('\u{2030}'), // per mille sign
        0xE5 => Some('\u{00C2}'), // A-circumflex
        0xE6 => Some('\u{00CA}'), // E-circumflex
        0xE7 => Some('\u{00C1}'), // A-acute
        0xE8 => Some('\u{00CB}'), // E-diaeresis
        0xE9 => Some('\u{00C8}'), // E-grave
        0xEA => Some('\u{00CD}'), // I-acute
        0xEB => Some('\u{00CE}'), // I-circumflex
        0xEC => Some('\u{00CF}'), // I-diaeresis
        0xED => Some('\u{00CC}'), // I-grave
        0xEE => Some('\u{00D3}'), // O-acute
        0xEF => Some('\u{00D4}'), // O-circumflex
        0xF0 => Some('\u{F8FF}'), // Apple logo
        0xF1 => Some('\u{00D2}'), // O-grave
        0xF2 => Some('\u{00DA}'), // U-acute
        0xF3 => Some('\u{00DB}'), // U-circumflex
        0xF4 => Some('\u{00D9}'), // U-grave
        0xF5 => Some('\u{0131}'), // dotless i
        0xF6 => Some('\u{02C6}'), // circumflex accent
        0xF7 => Some('\u{02DC}'), // small tilde
        0xF8 => Some('\u{00AF}'), // macron
        0xF9 => Some('\u{02D8}'), // breve
        0xFA => Some('\u{02D9}'), // dot above
        0xFB => Some('\u{02DA}'), // ring above
        0xFC => Some('\u{00B8}'), // cedilla
        0xFD => Some('\u{02DD}'), // double acute accent
        0xFE => Some('\u{02DB}'), // ogonek
        0xFF => Some('\u{02C7}'), // caron
    }
}

/// Adobe StandardEncoding.
///
/// See PDF Reference Table D.3.
fn standard_lookup(code: u8) -> Option<char> {
    match code {
        0x20 => Some(' '),
        0x21 => Some('!'),
        0x22 => Some('"'),
        0x23 => Some('#'),
        0x24 => Some('$'),
        0x25 => Some('%'),
        0x26 => Some('&'),
        0x27 => Some('\u{2019}'), // quoteright
        0x28 => Some('('),
        0x29 => Some(')'),
        0x2A => Some('*'),
        0x2B => Some('+'),
        0x2C => Some(','),
        0x2D => Some('-'),
        0x2E => Some('.'),
        0x2F => Some('/'),
        0x30..=0x39 => char::from_u32(code as u32), // 0-9
        0x3A => Some(':'),
        0x3B => Some(';'),
        0x3C => Some('<'),
        0x3D => Some('='),
        0x3E => Some('>'),
        0x3F => Some('?'),
        0x40 => Some('@'),
        0x41..=0x5A => char::from_u32(code as u32), // A-Z
        0x5B => Some('['),
        0x5C => Some('\\'),
        0x5D => Some(']'),
        0x5E => Some('^'),
        0x5F => Some('_'),
        0x60 => Some('\u{2018}'),                   // quoteleft
        0x61..=0x7A => char::from_u32(code as u32), // a-z
        0x7B => Some('{'),
        0x7C => Some('|'),
        0x7D => Some('}'),
        0x7E => Some('~'),
        0xA1 => Some('\u{00A1}'), // exclamdown
        0xA2 => Some('\u{00A2}'), // cent
        0xA3 => Some('\u{00A3}'), // sterling
        0xA4 => Some('\u{2044}'), // fraction
        0xA5 => Some('\u{00A5}'), // yen
        0xA6 => Some('\u{0192}'), // florin
        0xA7 => Some('\u{00A7}'), // section
        0xA8 => Some('\u{00A4}'), // currency
        0xA9 => Some('\u{0027}'), // quotesingle
        0xAA => Some('\u{201C}'), // quotedblleft
        0xAB => Some('\u{00AB}'), // guillemotleft
        0xAC => Some('\u{2039}'), // guilsinglleft
        0xAD => Some('\u{203A}'), // guilsinglright
        0xAE => Some('\u{FB01}'), // fi
        0xAF => Some('\u{FB02}'), // fl
        0xB1 => Some('\u{2013}'), // endash
        0xB2 => Some('\u{2020}'), // dagger
        0xB3 => Some('\u{2021}'), // daggerdbl
        0xB4 => Some('\u{00B7}'), // periodcentered
        0xB6 => Some('\u{00B6}'), // paragraph
        0xB7 => Some('\u{2022}'), // bullet
        0xB8 => Some('\u{201A}'), // quotesinglbase
        0xB9 => Some('\u{201E}'), // quotedblbase
        0xBA => Some('\u{201D}'), // quotedblright
        0xBB => Some('\u{00BB}'), // guillemotright
        0xBC => Some('\u{2026}'), // ellipsis
        0xBD => Some('\u{2030}'), // perthousand
        0xC1 => Some('\u{0060}'), // grave
        0xC2 => Some('\u{00B4}'), // acute
        0xC3 => Some('\u{02C6}'), // circumflex
        0xC4 => Some('\u{02DC}'), // tilde
        0xC5 => Some('\u{00AF}'), // macron
        0xC6 => Some('\u{02D8}'), // breve
        0xC7 => Some('\u{02D9}'), // dotaccent
        0xC8 => Some('\u{00A8}'), // dieresis
        0xCA => Some('\u{02DA}'), // ring
        0xCB => Some('\u{00B8}'), // cedilla
        0xCD => Some('\u{02DD}'), // hungarumlaut
        0xCE => Some('\u{02DB}'), // ogonek
        0xCF => Some('\u{02C7}'), // caron
        0xD0 => Some('\u{2014}'), // emdash
        0xE1 => Some('\u{00C6}'), // AE
        0xE3 => Some('\u{00AA}'), // ordfeminine
        0xE8 => Some('\u{0141}'), // Lslash
        0xE9 => Some('\u{00D8}'), // Oslash
        0xEA => Some('\u{0152}'), // OE
        0xEB => Some('\u{00BA}'), // ordmasculine
        0xF1 => Some('\u{00E6}'), // ae
        0xF5 => Some('\u{0131}'), // dotlessi
        0xF8 => Some('\u{0142}'), // lslash
        0xF9 => Some('\u{00F8}'), // oslash
        0xFA => Some('\u{0153}'), // oe
        0xFB => Some('\u{00DF}'), // germandbls
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_winansi_ascii() {
        assert_eq!(winansi_lookup(0x41), Some('A'));
        assert_eq!(winansi_lookup(0x61), Some('a'));
        assert_eq!(winansi_lookup(0x20), Some(' '));
        assert_eq!(winansi_lookup(0x7E), Some('~'));
    }

    #[test]
    fn test_winansi_special() {
        assert_eq!(winansi_lookup(0x80), Some('\u{20AC}')); // Euro
        assert_eq!(winansi_lookup(0x93), Some('\u{201C}')); // left double quote
        assert_eq!(winansi_lookup(0x94), Some('\u{201D}')); // right double quote
        assert_eq!(winansi_lookup(0x85), Some('\u{2026}')); // ellipsis
    }

    #[test]
    fn test_winansi_undefined() {
        assert_eq!(winansi_lookup(0x00), None);
        assert_eq!(winansi_lookup(0x7F), None);
        assert_eq!(winansi_lookup(0x81), None);
    }

    #[test]
    fn test_macroman_ascii() {
        assert_eq!(macroman_lookup(0x41), Some('A'));
        assert_eq!(macroman_lookup(0x20), Some(' '));
    }

    #[test]
    fn test_macroman_special() {
        assert_eq!(macroman_lookup(0x80), Some('\u{00C4}')); // A-diaeresis
        assert_eq!(macroman_lookup(0xD0), Some('\u{2013}')); // en dash
        assert_eq!(macroman_lookup(0xD1), Some('\u{2014}')); // em dash
    }

    #[test]
    fn test_standard_encoding() {
        assert_eq!(standard_lookup(0x41), Some('A'));
        assert_eq!(standard_lookup(0x61), Some('a'));
        assert_eq!(standard_lookup(0x27), Some('\u{2019}')); // quoteright
        assert_eq!(standard_lookup(0xD0), Some('\u{2014}')); // emdash
    }

    #[test]
    fn test_agl_lookup() {
        assert_eq!(agl_lookup("space"), Some(' '));
        assert_eq!(agl_lookup("A"), Some('A'));
        assert_eq!(agl_lookup("period"), Some('.'));
        assert_eq!(agl_lookup("nonexistent_glyph_xyz"), None);
    }

    #[test]
    fn test_encoding_custom_differences() {
        let enc = Encoding::custom(StandardEncoding::WinAnsi, &[(0x41, 'X')]);
        assert_eq!(enc.lookup(0x41), Some('X')); // overridden
        assert_eq!(enc.lookup(0x42), Some('B')); // from base
        assert_eq!(enc.lookup(0x20), Some(' ')); // from base
    }

    #[test]
    fn test_macexpert_returns_none() {
        assert_eq!(StandardEncoding::MacExpert.lookup(0x41), None);
        assert_eq!(StandardEncoding::MacExpert.lookup(0x80), None);
    }

    #[test]
    fn test_builtin_returns_none() {
        assert_eq!(StandardEncoding::BuiltIn.lookup(0x41), None);
        assert_eq!(StandardEncoding::BuiltIn.lookup(0x00), None);
    }

    #[test]
    fn test_parse_glyph_name_uni_pattern() {
        assert_eq!(parse_glyph_name("uni0041"), Some('A'));
        assert_eq!(parse_glyph_name("uni00E9"), Some('\u{00E9}')); // e-acute
        assert_eq!(parse_glyph_name("uni2014"), Some('\u{2014}')); // em dash
                                                                   // 5 hex digits
        assert_eq!(parse_glyph_name("uni1F600"), Some('\u{1F600}')); // emoji
                                                                     // Too short or too long
        assert_eq!(parse_glyph_name("uni004"), None);
        assert_eq!(parse_glyph_name("uni0041FF0"), None);
        // Non-hex
        assert_eq!(parse_glyph_name("uniGGGG"), None);
    }

    #[test]
    fn test_parse_glyph_name_u_pattern() {
        assert_eq!(parse_glyph_name("u0041"), Some('A'));
        assert_eq!(parse_glyph_name("u00E9"), Some('\u{00E9}'));
        assert_eq!(parse_glyph_name("u1F600"), Some('\u{1F600}'));
        // Too short
        assert_eq!(parse_glyph_name("u004"), None);
    }

    #[test]
    fn test_parse_glyph_name_unknown() {
        assert_eq!(parse_glyph_name("nonexistent_glyph_name_xyz"), None);
    }

    #[test]
    fn test_macroman_full_range() {
        // Control chars and DEL
        assert_eq!(macroman_lookup(0x00), None);
        assert_eq!(macroman_lookup(0x1F), None);
        assert_eq!(macroman_lookup(0x7F), None);
        // ASCII range
        assert_eq!(macroman_lookup(0x41), Some('A'));
        assert_eq!(macroman_lookup(0x7E), Some('~'));
        // Extended range spot checks
        assert_eq!(macroman_lookup(0x80), Some('\u{00C4}')); // A-diaeresis
        assert_eq!(macroman_lookup(0x8D), Some('\u{00E7}')); // c-cedilla
        assert_eq!(macroman_lookup(0x9C), Some('\u{00FA}')); // u-acute
        assert_eq!(macroman_lookup(0xA0), Some('\u{2020}')); // dagger
        assert_eq!(macroman_lookup(0xB0), Some('\u{221E}')); // infinity
        assert_eq!(macroman_lookup(0xB9), Some('\u{03C0}')); // pi
        assert_eq!(macroman_lookup(0xC0), Some('\u{00BF}')); // inverted question
        assert_eq!(macroman_lookup(0xCE), Some('\u{0152}')); // OE ligature
        assert_eq!(macroman_lookup(0xD0), Some('\u{2013}')); // en dash
        assert_eq!(macroman_lookup(0xDE), Some('\u{FB01}')); // fi ligature
        assert_eq!(macroman_lookup(0xDF), Some('\u{FB02}')); // fl ligature
        assert_eq!(macroman_lookup(0xE0), Some('\u{2021}')); // double dagger
        assert_eq!(macroman_lookup(0xF0), Some('\u{F8FF}')); // Apple logo
        assert_eq!(macroman_lookup(0xFF), Some('\u{02C7}')); // caron
    }

    #[test]
    fn test_standard_encoding_full_range() {
        // Undefined ranges return None
        assert_eq!(standard_lookup(0x00), None);
        assert_eq!(standard_lookup(0x1F), None);
        assert_eq!(standard_lookup(0x7F), None);
        assert_eq!(standard_lookup(0x80), None);
        assert_eq!(standard_lookup(0xB0), None);
        assert_eq!(standard_lookup(0xB5), None);
        // ASCII-like range
        assert_eq!(standard_lookup(0x20), Some(' '));
        assert_eq!(standard_lookup(0x21), Some('!'));
        assert_eq!(standard_lookup(0x23), Some('#'));
        assert_eq!(standard_lookup(0x24), Some('$'));
        assert_eq!(standard_lookup(0x25), Some('%'));
        assert_eq!(standard_lookup(0x26), Some('&'));
        assert_eq!(standard_lookup(0x28), Some('('));
        assert_eq!(standard_lookup(0x29), Some(')'));
        assert_eq!(standard_lookup(0x2A), Some('*'));
        assert_eq!(standard_lookup(0x2B), Some('+'));
        assert_eq!(standard_lookup(0x2C), Some(','));
        assert_eq!(standard_lookup(0x2D), Some('-'));
        assert_eq!(standard_lookup(0x2E), Some('.'));
        assert_eq!(standard_lookup(0x2F), Some('/'));
        assert_eq!(standard_lookup(0x30), Some('0'));
        assert_eq!(standard_lookup(0x39), Some('9'));
        assert_eq!(standard_lookup(0x3A), Some(':'));
        assert_eq!(standard_lookup(0x3B), Some(';'));
        assert_eq!(standard_lookup(0x3C), Some('<'));
        assert_eq!(standard_lookup(0x3D), Some('='));
        assert_eq!(standard_lookup(0x3E), Some('>'));
        assert_eq!(standard_lookup(0x3F), Some('?'));
        assert_eq!(standard_lookup(0x40), Some('@'));
        assert_eq!(standard_lookup(0x5B), Some('['));
        assert_eq!(standard_lookup(0x5C), Some('\\'));
        assert_eq!(standard_lookup(0x5D), Some(']'));
        assert_eq!(standard_lookup(0x5E), Some('^'));
        assert_eq!(standard_lookup(0x5F), Some('_'));
        assert_eq!(standard_lookup(0x60), Some('\u{2018}')); // quoteleft
        assert_eq!(standard_lookup(0x7B), Some('{'));
        assert_eq!(standard_lookup(0x7C), Some('|'));
        assert_eq!(standard_lookup(0x7D), Some('}'));
        assert_eq!(standard_lookup(0x7E), Some('~'));
        // Extended chars
        assert_eq!(standard_lookup(0xA1), Some('\u{00A1}')); // exclamdown
        assert_eq!(standard_lookup(0xA2), Some('\u{00A2}')); // cent
        assert_eq!(standard_lookup(0xA4), Some('\u{2044}')); // fraction
        assert_eq!(standard_lookup(0xA9), Some('\u{0027}')); // quotesingle
        assert_eq!(standard_lookup(0xAA), Some('\u{201C}')); // quotedblleft
        assert_eq!(standard_lookup(0xAB), Some('\u{00AB}')); // guillemotleft
        assert_eq!(standard_lookup(0xAC), Some('\u{2039}')); // guilsinglleft
        assert_eq!(standard_lookup(0xAD), Some('\u{203A}')); // guilsinglright
        assert_eq!(standard_lookup(0xAE), Some('\u{FB01}')); // fi
        assert_eq!(standard_lookup(0xAF), Some('\u{FB02}')); // fl
        assert_eq!(standard_lookup(0xB1), Some('\u{2013}')); // endash
        assert_eq!(standard_lookup(0xB2), Some('\u{2020}')); // dagger
        assert_eq!(standard_lookup(0xB3), Some('\u{2021}')); // daggerdbl
        assert_eq!(standard_lookup(0xB4), Some('\u{00B7}')); // periodcentered
        assert_eq!(standard_lookup(0xB6), Some('\u{00B6}')); // paragraph
        assert_eq!(standard_lookup(0xB7), Some('\u{2022}')); // bullet
        assert_eq!(standard_lookup(0xB8), Some('\u{201A}')); // quotesinglbase
        assert_eq!(standard_lookup(0xB9), Some('\u{201E}')); // quotedblbase
        assert_eq!(standard_lookup(0xBA), Some('\u{201D}')); // quotedblright
        assert_eq!(standard_lookup(0xBB), Some('\u{00BB}')); // guillemotright
        assert_eq!(standard_lookup(0xBC), Some('\u{2026}')); // ellipsis
        assert_eq!(standard_lookup(0xBD), Some('\u{2030}')); // perthousand
        assert_eq!(standard_lookup(0xC1), Some('\u{0060}')); // grave
        assert_eq!(standard_lookup(0xC2), Some('\u{00B4}')); // acute
        assert_eq!(standard_lookup(0xC3), Some('\u{02C6}')); // circumflex
        assert_eq!(standard_lookup(0xC4), Some('\u{02DC}')); // tilde
        assert_eq!(standard_lookup(0xC5), Some('\u{00AF}')); // macron
        assert_eq!(standard_lookup(0xC6), Some('\u{02D8}')); // breve
        assert_eq!(standard_lookup(0xC7), Some('\u{02D9}')); // dotaccent
        assert_eq!(standard_lookup(0xC8), Some('\u{00A8}')); // dieresis
        assert_eq!(standard_lookup(0xCA), Some('\u{02DA}')); // ring
        assert_eq!(standard_lookup(0xCB), Some('\u{00B8}')); // cedilla
        assert_eq!(standard_lookup(0xCD), Some('\u{02DD}')); // hungarumlaut
        assert_eq!(standard_lookup(0xCE), Some('\u{02DB}')); // ogonek
        assert_eq!(standard_lookup(0xCF), Some('\u{02C7}')); // caron
        assert_eq!(standard_lookup(0xD0), Some('\u{2014}')); // emdash
        assert_eq!(standard_lookup(0xE1), Some('\u{00C6}')); // AE
        assert_eq!(standard_lookup(0xE3), Some('\u{00AA}')); // ordfeminine
        assert_eq!(standard_lookup(0xE8), Some('\u{0141}')); // Lslash
        assert_eq!(standard_lookup(0xE9), Some('\u{00D8}')); // Oslash
        assert_eq!(standard_lookup(0xEA), Some('\u{0152}')); // OE
        assert_eq!(standard_lookup(0xEB), Some('\u{00BA}')); // ordmasculine
        assert_eq!(standard_lookup(0xF1), Some('\u{00E6}')); // ae
        assert_eq!(standard_lookup(0xF5), Some('\u{0131}')); // dotlessi
        assert_eq!(standard_lookup(0xF8), Some('\u{0142}')); // lslash
        assert_eq!(standard_lookup(0xF9), Some('\u{00F8}')); // oslash
        assert_eq!(standard_lookup(0xFA), Some('\u{0153}')); // oe
        assert_eq!(standard_lookup(0xFB), Some('\u{00DF}')); // germandbls
    }

    #[test]
    fn test_encoding_standard_variant_dispatch() {
        // Exercise the Encoding::Standard() dispatch path for all variants
        let enc = Encoding::Standard(StandardEncoding::MacRoman);
        assert_eq!(enc.lookup(0x80), Some('\u{00C4}'));

        let enc = Encoding::Standard(StandardEncoding::Standard);
        assert_eq!(enc.lookup(0xD0), Some('\u{2014}'));

        let enc = Encoding::Standard(StandardEncoding::MacExpert);
        assert_eq!(enc.lookup(0x41), None);

        let enc = Encoding::Standard(StandardEncoding::BuiltIn);
        assert_eq!(enc.lookup(0x41), None);
    }

    // ========================================================================
    // Additional coverage: WinAnsi full range
    // ========================================================================

    #[test]
    fn test_winansi_full_special_range() {
        // Cover every branch in the 0x80-0x9F range
        assert_eq!(winansi_lookup(0x82), Some('\u{201A}')); // single low-9 quotation mark
        assert_eq!(winansi_lookup(0x83), Some('\u{0192}')); // f with hook
        assert_eq!(winansi_lookup(0x84), Some('\u{201E}')); // double low-9 quotation mark
        assert_eq!(winansi_lookup(0x86), Some('\u{2020}')); // dagger
        assert_eq!(winansi_lookup(0x87), Some('\u{2021}')); // double dagger
        assert_eq!(winansi_lookup(0x88), Some('\u{02C6}')); // circumflex accent
        assert_eq!(winansi_lookup(0x89), Some('\u{2030}')); // per mille sign
        assert_eq!(winansi_lookup(0x8A), Some('\u{0160}')); // S with caron
        assert_eq!(winansi_lookup(0x8B), Some('\u{2039}')); // single left angle quote
        assert_eq!(winansi_lookup(0x8C), Some('\u{0152}')); // OE ligature
        assert_eq!(winansi_lookup(0x8D), None); // undefined
        assert_eq!(winansi_lookup(0x8E), Some('\u{017D}')); // Z with caron
        assert_eq!(winansi_lookup(0x8F), None); // undefined
        assert_eq!(winansi_lookup(0x90), None); // undefined
        assert_eq!(winansi_lookup(0x91), Some('\u{2018}')); // left single quote
        assert_eq!(winansi_lookup(0x92), Some('\u{2019}')); // right single quote
        assert_eq!(winansi_lookup(0x95), Some('\u{2022}')); // bullet
        assert_eq!(winansi_lookup(0x96), Some('\u{2013}')); // en dash
        assert_eq!(winansi_lookup(0x97), Some('\u{2014}')); // em dash
        assert_eq!(winansi_lookup(0x98), Some('\u{02DC}')); // small tilde
        assert_eq!(winansi_lookup(0x99), Some('\u{2122}')); // trade mark
        assert_eq!(winansi_lookup(0x9A), Some('\u{0161}')); // s with caron
        assert_eq!(winansi_lookup(0x9B), Some('\u{203A}')); // single right angle quote
        assert_eq!(winansi_lookup(0x9C), Some('\u{0153}')); // oe ligature
        assert_eq!(winansi_lookup(0x9D), None); // undefined
        assert_eq!(winansi_lookup(0x9E), Some('\u{017E}')); // z with caron
        assert_eq!(winansi_lookup(0x9F), Some('\u{0178}')); // Y with diaeresis
    }

    #[test]
    fn test_winansi_high_latin_range() {
        // The 0xA0-0xFF range should map via char::from_u32
        assert_eq!(winansi_lookup(0xA0), Some('\u{00A0}')); // no-break space
        assert_eq!(winansi_lookup(0xA9), Some('\u{00A9}')); // copyright
        assert_eq!(winansi_lookup(0xC0), Some('\u{00C0}')); // A-grave
        assert_eq!(winansi_lookup(0xE9), Some('\u{00E9}')); // e-acute
        assert_eq!(winansi_lookup(0xFF), Some('\u{00FF}')); // y-diaeresis
    }

    #[test]
    fn test_winansi_control_chars() {
        // 0x01 through 0x1F are all None
        for code in 0x01..=0x1F {
            assert_eq!(
                winansi_lookup(code),
                None,
                "code 0x{:02X} should be None",
                code
            );
        }
    }

    // ========================================================================
    // Additional coverage: MacRoman gaps
    // ========================================================================

    #[test]
    fn test_macroman_mid_range() {
        // Cover codes in 0xA0-0xBF range that weren't hit
        assert_eq!(macroman_lookup(0xA1), Some('\u{00B0}')); // degree
        assert_eq!(macroman_lookup(0xA2), Some('\u{00A2}')); // cent
        assert_eq!(macroman_lookup(0xA3), Some('\u{00A3}')); // pound
        assert_eq!(macroman_lookup(0xA4), Some('\u{00A7}')); // section
        assert_eq!(macroman_lookup(0xA5), Some('\u{2022}')); // bullet
        assert_eq!(macroman_lookup(0xA6), Some('\u{00B6}')); // pilcrow
        assert_eq!(macroman_lookup(0xA7), Some('\u{00DF}')); // sharp s
        assert_eq!(macroman_lookup(0xA8), Some('\u{00AE}')); // registered
        assert_eq!(macroman_lookup(0xA9), Some('\u{00A9}')); // copyright
        assert_eq!(macroman_lookup(0xAA), Some('\u{2122}')); // trademark
        assert_eq!(macroman_lookup(0xAB), Some('\u{00B4}')); // acute accent
        assert_eq!(macroman_lookup(0xAC), Some('\u{00A8}')); // diaeresis
        assert_eq!(macroman_lookup(0xAD), Some('\u{2260}')); // not equal
        assert_eq!(macroman_lookup(0xAE), Some('\u{00C6}')); // AE
        assert_eq!(macroman_lookup(0xAF), Some('\u{00D8}')); // O-stroke
        assert_eq!(macroman_lookup(0xB1), Some('\u{00B1}')); // plus-minus
        assert_eq!(macroman_lookup(0xB2), Some('\u{2264}')); // less-than or equal
        assert_eq!(macroman_lookup(0xB3), Some('\u{2265}')); // greater-than or equal
        assert_eq!(macroman_lookup(0xB4), Some('\u{00A5}')); // yen
        assert_eq!(macroman_lookup(0xB5), Some('\u{00B5}')); // micro
        assert_eq!(macroman_lookup(0xB6), Some('\u{2202}')); // partial differential
        assert_eq!(macroman_lookup(0xB7), Some('\u{2211}')); // summation
        assert_eq!(macroman_lookup(0xB8), Some('\u{220F}')); // product
        assert_eq!(macroman_lookup(0xBA), Some('\u{222B}')); // integral
        assert_eq!(macroman_lookup(0xBB), Some('\u{00AA}')); // feminine ordinal
        assert_eq!(macroman_lookup(0xBC), Some('\u{00BA}')); // masculine ordinal
        assert_eq!(macroman_lookup(0xBD), Some('\u{2126}')); // ohm
        assert_eq!(macroman_lookup(0xBE), Some('\u{00E6}')); // ae
        assert_eq!(macroman_lookup(0xBF), Some('\u{00F8}')); // o-stroke
    }

    #[test]
    fn test_macroman_c0_to_cf() {
        assert_eq!(macroman_lookup(0xC1), Some('\u{00A1}')); // inverted exclamation
        assert_eq!(macroman_lookup(0xC2), Some('\u{00AC}')); // not sign
        assert_eq!(macroman_lookup(0xC3), Some('\u{221A}')); // square root
        assert_eq!(macroman_lookup(0xC4), Some('\u{0192}')); // f with hook
        assert_eq!(macroman_lookup(0xC5), Some('\u{2248}')); // almost equal
        assert_eq!(macroman_lookup(0xC6), Some('\u{2206}')); // increment
        assert_eq!(macroman_lookup(0xC7), Some('\u{00AB}')); // left guillemet
        assert_eq!(macroman_lookup(0xC8), Some('\u{00BB}')); // right guillemet
        assert_eq!(macroman_lookup(0xC9), Some('\u{2026}')); // ellipsis
        assert_eq!(macroman_lookup(0xCA), Some('\u{00A0}')); // no-break space
        assert_eq!(macroman_lookup(0xCB), Some('\u{00C0}')); // A-grave
        assert_eq!(macroman_lookup(0xCC), Some('\u{00C3}')); // A-tilde
        assert_eq!(macroman_lookup(0xCD), Some('\u{00D5}')); // O-tilde
        assert_eq!(macroman_lookup(0xCF), Some('\u{0153}')); // oe
    }

    #[test]
    fn test_macroman_d0_to_ff() {
        assert_eq!(macroman_lookup(0xD2), Some('\u{201C}')); // left double quote
        assert_eq!(macroman_lookup(0xD3), Some('\u{201D}')); // right double quote
        assert_eq!(macroman_lookup(0xD4), Some('\u{2018}')); // left single quote
        assert_eq!(macroman_lookup(0xD5), Some('\u{2019}')); // right single quote
        assert_eq!(macroman_lookup(0xD6), Some('\u{00F7}')); // division sign
        assert_eq!(macroman_lookup(0xD7), Some('\u{25CA}')); // lozenge
        assert_eq!(macroman_lookup(0xD8), Some('\u{00FF}')); // y-diaeresis
        assert_eq!(macroman_lookup(0xD9), Some('\u{0178}')); // Y-diaeresis
        assert_eq!(macroman_lookup(0xDA), Some('\u{2044}')); // fraction slash
        assert_eq!(macroman_lookup(0xDB), Some('\u{20AC}')); // Euro
        assert_eq!(macroman_lookup(0xDC), Some('\u{2039}')); // single left angle
        assert_eq!(macroman_lookup(0xDD), Some('\u{203A}')); // single right angle
        assert_eq!(macroman_lookup(0xE1), Some('\u{00B7}')); // middle dot
        assert_eq!(macroman_lookup(0xE2), Some('\u{201A}')); // single low-9 quote
        assert_eq!(macroman_lookup(0xE3), Some('\u{201E}')); // double low-9 quote
        assert_eq!(macroman_lookup(0xE4), Some('\u{2030}')); // per mille
        assert_eq!(macroman_lookup(0xE5), Some('\u{00C2}')); // A-circumflex
        assert_eq!(macroman_lookup(0xE6), Some('\u{00CA}')); // E-circumflex
        assert_eq!(macroman_lookup(0xE7), Some('\u{00C1}')); // A-acute
        assert_eq!(macroman_lookup(0xE8), Some('\u{00CB}')); // E-diaeresis
        assert_eq!(macroman_lookup(0xE9), Some('\u{00C8}')); // E-grave
        assert_eq!(macroman_lookup(0xEA), Some('\u{00CD}')); // I-acute
        assert_eq!(macroman_lookup(0xEB), Some('\u{00CE}')); // I-circumflex
        assert_eq!(macroman_lookup(0xEC), Some('\u{00CF}')); // I-diaeresis
        assert_eq!(macroman_lookup(0xED), Some('\u{00CC}')); // I-grave
        assert_eq!(macroman_lookup(0xEE), Some('\u{00D3}')); // O-acute
        assert_eq!(macroman_lookup(0xEF), Some('\u{00D4}')); // O-circumflex
        assert_eq!(macroman_lookup(0xF1), Some('\u{00D2}')); // O-grave
        assert_eq!(macroman_lookup(0xF2), Some('\u{00DA}')); // U-acute
        assert_eq!(macroman_lookup(0xF3), Some('\u{00DB}')); // U-circumflex
        assert_eq!(macroman_lookup(0xF4), Some('\u{00D9}')); // U-grave
        assert_eq!(macroman_lookup(0xF5), Some('\u{0131}')); // dotless i
        assert_eq!(macroman_lookup(0xF6), Some('\u{02C6}')); // circumflex accent
        assert_eq!(macroman_lookup(0xF7), Some('\u{02DC}')); // small tilde
        assert_eq!(macroman_lookup(0xF8), Some('\u{00AF}')); // macron
        assert_eq!(macroman_lookup(0xF9), Some('\u{02D8}')); // breve
        assert_eq!(macroman_lookup(0xFA), Some('\u{02D9}')); // dot above
        assert_eq!(macroman_lookup(0xFB), Some('\u{02DA}')); // ring above
        assert_eq!(macroman_lookup(0xFC), Some('\u{00B8}')); // cedilla
        assert_eq!(macroman_lookup(0xFD), Some('\u{02DD}')); // double acute
        assert_eq!(macroman_lookup(0xFE), Some('\u{02DB}')); // ogonek
    }

    // ========================================================================
    // Additional coverage: StandardEncoding gaps
    // ========================================================================

    #[test]
    fn test_standard_encoding_gap_codes() {
        // Codes that exist in std encoding but with gaps (None) around them
        assert_eq!(standard_lookup(0xA3), Some('\u{00A3}')); // sterling
        assert_eq!(standard_lookup(0xA5), Some('\u{00A5}')); // yen
        assert_eq!(standard_lookup(0xA6), Some('\u{0192}')); // florin
        assert_eq!(standard_lookup(0xA7), Some('\u{00A7}')); // section
        assert_eq!(standard_lookup(0xA8), Some('\u{00A4}')); // currency
                                                             // Gaps in the standard encoding that should be None
        assert_eq!(standard_lookup(0xA0), None);
        assert_eq!(standard_lookup(0xBE), None);
        assert_eq!(standard_lookup(0xBF), None);
        assert_eq!(standard_lookup(0xC0), None);
        assert_eq!(standard_lookup(0xC9), None);
        assert_eq!(standard_lookup(0xCC), None);
        assert_eq!(standard_lookup(0xD1), None);
        assert_eq!(standard_lookup(0xE0), None);
        assert_eq!(standard_lookup(0xE2), None);
        assert_eq!(standard_lookup(0xF0), None);
        assert_eq!(standard_lookup(0xFC), None);
        assert_eq!(standard_lookup(0xFD), None);
        assert_eq!(standard_lookup(0xFE), None);
        assert_eq!(standard_lookup(0xFF), None);
    }

    #[test]
    fn test_standard_encoding_letter_ranges() {
        // Uppercase A-Z
        for code in 0x41..=0x5A {
            assert!(
                standard_lookup(code).is_some(),
                "code 0x{:02X} should map to a char",
                code
            );
        }
        // Lowercase a-z
        for code in 0x61..=0x7A {
            assert!(
                standard_lookup(code).is_some(),
                "code 0x{:02X} should map to a char",
                code
            );
        }
        // Digits 0-9
        for code in 0x30..=0x39 {
            assert!(
                standard_lookup(code).is_some(),
                "code 0x{:02X} should map to a char",
                code
            );
        }
    }

    // ========================================================================
    // Additional coverage: Encoding::Custom lookup
    // ========================================================================

    #[test]
    fn test_encoding_custom_lookup_all_overridden() {
        // All 256 codes overridden, base encoding doesn't matter
        let diffs: Vec<(u8, char)> = (0..=255u8).map(|c| (c, 'Z')).collect();
        let enc = Encoding::custom(StandardEncoding::BuiltIn, &diffs);
        for code in 0..=255u8 {
            assert_eq!(
                enc.lookup(code),
                Some('Z'),
                "code {} should be overridden",
                code
            );
        }
    }

    #[test]
    fn test_encoding_custom_empty_diffs() {
        // No differences: should behave like the base encoding
        let enc = Encoding::custom(StandardEncoding::WinAnsi, &[]);
        assert_eq!(enc.lookup(0x41), Some('A'));
        assert_eq!(enc.lookup(0x00), None);
    }

    #[test]
    fn test_encoding_custom_with_standard_base() {
        // Custom encoding on top of Standard (not WinAnsi)
        let enc = Encoding::custom(StandardEncoding::Standard, &[(0x60, 'X')]);
        // 0x60 in Standard is quoteleft, but we override it
        assert_eq!(enc.lookup(0x60), Some('X'));
        // 0x41 is still 'A' from StandardEncoding base
        assert_eq!(enc.lookup(0x41), Some('A'));
    }

    #[test]
    fn test_encoding_custom_with_macroman_base() {
        let enc = Encoding::custom(StandardEncoding::MacRoman, &[(0xFF, 'Q')]);
        // 0xFF in MacRoman is caron, but overridden
        assert_eq!(enc.lookup(0xFF), Some('Q'));
        // 0x80 still from MacRoman
        assert_eq!(enc.lookup(0x80), Some('\u{00C4}'));
    }

    // ========================================================================
    // Additional coverage: parse_glyph_name edge cases
    // ========================================================================

    #[test]
    fn test_parse_glyph_name_agl_common_names() {
        // Test some common AGL names that should definitely work
        assert_eq!(parse_glyph_name("comma"), Some(','));
        assert_eq!(parse_glyph_name("hyphen"), Some('-'));
        assert_eq!(parse_glyph_name("colon"), Some(':'));
        assert_eq!(parse_glyph_name("semicolon"), Some(';'));
        assert_eq!(parse_glyph_name("zero"), Some('0'));
        assert_eq!(parse_glyph_name("one"), Some('1'));
    }

    #[test]
    fn test_parse_glyph_name_uni_six_hex_digits() {
        // 6 hex digits after "uni" (supplementary plane, leading zero)
        assert_eq!(parse_glyph_name("uni01F600"), Some('\u{1F600}')); // 6 digits, valid
        assert_eq!(parse_glyph_name("uni10000"), Some('\u{10000}')); // 5 digits, first supplementary
                                                                     // 7 digits is too long
        assert_eq!(parse_glyph_name("uni001F600"), None);
    }

    #[test]
    fn test_parse_glyph_name_u_too_long() {
        // 7 hex digits after "u" (too many)
        assert_eq!(parse_glyph_name("u0041000"), None);
    }

    #[test]
    fn test_parse_glyph_name_u_non_hex() {
        assert_eq!(parse_glyph_name("uZZZZ"), None);
    }

    #[test]
    fn test_parse_glyph_name_uni_surrogate_range() {
        // U+D800 is a surrogate, char::from_u32 returns None
        assert_eq!(parse_glyph_name("uniD800"), None);
        assert_eq!(parse_glyph_name("uniDFFF"), None);
    }

    #[test]
    fn test_parse_glyph_name_u_surrogate_range() {
        assert_eq!(parse_glyph_name("uD800"), None);
    }

    #[test]
    fn test_parse_glyph_name_empty() {
        assert_eq!(parse_glyph_name(""), None);
    }

    #[test]
    fn test_parse_glyph_name_single_char_agl() {
        // Single letters like "u" are valid AGL glyph names
        assert_eq!(parse_glyph_name("u"), Some('u'));
        assert_eq!(parse_glyph_name("a"), Some('a'));
    }

    // ========================================================================
    // Additional coverage: agl_lookup edge cases
    // ========================================================================

    #[test]
    fn test_agl_lookup_more_glyphs() {
        assert_eq!(agl_lookup("Euro"), Some('\u{20AC}'));
        assert_eq!(agl_lookup("endash"), Some('\u{2013}'));
        assert_eq!(agl_lookup("emdash"), Some('\u{2014}'));
        assert_eq!(agl_lookup("bullet"), Some('\u{2022}'));
        assert_eq!(agl_lookup("quoteleft"), Some('\u{2018}'));
        assert_eq!(agl_lookup("quoteright"), Some('\u{2019}'));
    }

    #[test]
    fn test_agl_lookup_case_sensitive() {
        // AGL is case-sensitive
        assert_eq!(agl_lookup("Space"), None); // "space" works, "Space" doesn't
        assert_eq!(agl_lookup("SPACE"), None);
    }

    // ========================================================================
    // Underscore-separated ligature names (TeX convention)
    // ========================================================================

    #[test]
    fn test_underscore_ligature_names() {
        // TeX fonts use underscore-separated names instead of AGL standard names
        assert_eq!(parse_underscore_ligature("f_i"), Some('\u{FB01}'));
        assert_eq!(parse_underscore_ligature("f_l"), Some('\u{FB02}'));
        assert_eq!(parse_underscore_ligature("f_f"), Some('\u{FB00}'));
        assert_eq!(parse_underscore_ligature("f_f_i"), Some('\u{FB03}'));
        assert_eq!(parse_underscore_ligature("f_f_l"), Some('\u{FB04}'));
        assert_eq!(parse_underscore_ligature("s_t"), Some('\u{FB06}'));
    }

    #[test]
    fn test_underscore_ligature_no_match() {
        assert_eq!(parse_underscore_ligature("f_t"), None);
        assert_eq!(parse_underscore_ligature("a_b"), None);
        assert_eq!(parse_underscore_ligature("fi"), None); // not underscore-separated
        assert_eq!(parse_underscore_ligature(""), None);
    }

    #[test]
    fn test_parse_glyph_name_underscore_ligatures() {
        // parse_glyph_name should resolve underscore-separated ligatures
        assert_eq!(parse_glyph_name("f_i"), Some('\u{FB01}'));
        assert_eq!(parse_glyph_name("f_l"), Some('\u{FB02}'));
        assert_eq!(parse_glyph_name("f_f"), Some('\u{FB00}'));
        assert_eq!(parse_glyph_name("f_f_i"), Some('\u{FB03}'));
        assert_eq!(parse_glyph_name("f_f_l"), Some('\u{FB04}'));
    }

    #[test]
    fn test_agl_standard_ligatures_still_work() {
        // Standard AGL names (without underscores) should still resolve
        assert_eq!(parse_glyph_name("fi"), Some('\u{FB01}'));
        assert_eq!(parse_glyph_name("fl"), Some('\u{FB02}'));
        assert_eq!(parse_glyph_name("ff"), Some('\u{FB00}'));
        assert_eq!(parse_glyph_name("ffi"), Some('\u{FB03}'));
        assert_eq!(parse_glyph_name("ffl"), Some('\u{FB04}'));
    }

    // ========================================================================
    // Ligature decomposition (Unicode -> ASCII)
    // ========================================================================

    #[test]
    fn test_decompose_ligature() {
        assert_eq!(decompose_ligature('\u{FB00}'), Some("ff"));
        assert_eq!(decompose_ligature('\u{FB01}'), Some("fi"));
        assert_eq!(decompose_ligature('\u{FB02}'), Some("fl"));
        assert_eq!(decompose_ligature('\u{FB03}'), Some("ffi"));
        assert_eq!(decompose_ligature('\u{FB04}'), Some("ffl"));
        assert_eq!(decompose_ligature('\u{FB05}'), Some("st"));
        assert_eq!(decompose_ligature('\u{FB06}'), Some("st"));
    }

    #[test]
    fn test_decompose_ligature_non_ligature() {
        assert_eq!(decompose_ligature('A'), None);
        assert_eq!(decompose_ligature('f'), None);
        assert_eq!(decompose_ligature('\u{0152}'), None); // OE is not decomposed here
    }
}
