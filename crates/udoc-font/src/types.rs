//! Font type hierarchy for PDF text extraction and rendering.
//!
//! PDF fonts come in three flavors:
//! - Simple fonts (Type1, TrueType, MMType1): single-byte character codes
//! - Composite fonts (Type0): multi-byte CID-keyed fonts with descendant CIDFont
//! - Type3 fonts: user-defined glyph shapes, otherwise like simple fonts
//!
//! For text extraction we need the character code -> Unicode mapping.
//! For rendering we additionally need the embedded font program bytes
//! (FontFile2 for TrueType, FontFile3 for CFF, FontFile for Type1).

use std::collections::{BTreeMap, HashMap};

use super::cmap_parser::ParsedCMap;
use super::encoding::Encoding;
use super::tounicode::ToUnicodeCMap;

/// The type of embedded font program data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontProgram {
    /// TrueType outlines (from /FontFile2). Quadratic bezier glyph contours.
    TrueType,
    /// CFF (Compact Font Format) outlines (from /FontFile3). Cubic bezier charstrings.
    Cff,
    /// Type1 outlines (from /FontFile). PostScript charstrings.
    Type1,
    /// No embedded font program. The font references a standard font by name.
    None,
}

/// A loaded font, ready for character code -> Unicode mapping and optional rendering.
#[derive(Debug)]
pub enum Font {
    /// Simple (single-byte) font (Type1, TrueType, MMType1).
    Simple(SimpleFont),
    /// Composite (Type0) font with a CID descendant.
    Composite(CompositeFont),
    /// Type3 user-defined-glyph font.
    Type3(Type3FontCore),
}

/// Subtype of a simple font.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimpleSubtype {
    /// PostScript Type 1.
    Type1,
    /// TrueType.
    TrueType,
    /// Multiple Master Type 1 (legacy).
    MMType1,
}

/// A simple (single-byte) font.
#[derive(Debug)]
pub struct SimpleFont {
    /// Simple-font subtype (Type1, TrueType, MMType1).
    #[allow(dead_code)] // used by renderer (cross-crate via font_data/font_program accessors)
    pub subtype: SimpleSubtype,
    /// `/BaseFont` name from the PDF font dict, if present.
    pub base_font: Option<String>,
    /// Character-code to Unicode encoding table.
    pub encoding: Encoding,
    /// Parsed `/ToUnicode` CMap, if provided by the PDF.
    pub tounicode: Option<ToUnicodeCMap>,
    /// Parsed `/FirstChar` + `/Widths` table, if provided.
    pub widths: Option<SimpleWidths>,
    /// Raw embedded font program bytes (decompressed FontFile/FontFile2/FontFile3).
    #[allow(dead_code)] // accessed via Font::font_data() for renderer
    pub font_data: Option<Vec<u8>>,
    /// Type of embedded font program.
    #[allow(dead_code)] // accessed via Font::font_program() for renderer
    pub font_program: FontProgram,
    /// Raw (byte_code, glyph_name) pairs from /Differences array.
    /// Preserves original glyph names without Unicode/AGL roundtrip.
    /// Used by encoding_glyph_names() for accurate by-code glyph lookup.
    pub differences_names: Vec<(u8, String)>,
}

/// A Type0 (composite) font with a CID descendant.
#[derive(Debug)]
pub struct CompositeFont {
    /// `/BaseFont` name from the PDF font dict.
    pub base_font: Option<String>,
    /// Raw embedded font program bytes from the CID descendant's FontDescriptor.
    #[allow(dead_code)] // accessed via Font::font_data() for renderer
    pub font_data: Option<Vec<u8>>,
    /// Type of embedded font program.
    #[allow(dead_code)] // accessed via Font::font_program() for renderer
    pub font_program: FontProgram,
    /// CMap encoding name (e.g. "Identity-H", "UniGB-UCS2-H").
    pub encoding_name: String,
    /// Parsed `/ToUnicode` CMap, if provided by the PDF.
    pub tounicode: Option<ToUnicodeCMap>,
    /// The single descendant CID font (CIDFontType0 or CIDFontType2).
    pub descendant: CidFont,
    /// Number of bytes per character code (from predefined CMap registry).
    /// Defaults to 2 for unknown CMaps.
    pub code_length: u8,
    /// Whether this font uses vertical writing mode (CMap name ends in -V).
    pub is_vertical: bool,
    /// Parsed CMap for runtime character code decoding.
    /// When present, used for variable-length code matching and
    /// code-to-Unicode/CID mapping. When None, falls back to the
    /// predefined registry behavior.
    /// Boxed to avoid inflating the Font enum variant size.
    pub parsed_cmap: Option<Box<ParsedCMap>>,
}

/// A CID-keyed descendant font.
#[derive(Debug)]
pub struct CidFont {
    /// CIDFont subtype (Type0 = CFF, Type2 = TrueType).
    #[allow(dead_code)] // parsed from /Subtype; used in tests, reserved for font rendering
    pub subtype: CidSubtype,
    /// `/BaseFont` name from the descendant CID font dict.
    #[allow(dead_code)] // parsed from /BaseFont; reserved for font-name diagnostics
    pub base_font: Option<String>,
    /// `/DW` default width used when a CID is not in the `/W` table.
    pub default_width: u32,
    /// Per-CID width table parsed from `/W`.
    pub widths: CidWidths,
}

/// CIDFont subtype.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CidSubtype {
    /// CIDFontType0 (CFF-based)
    Type0,
    /// CIDFontType2 (TrueType-based)
    Type2,
}

/// Per-CID width table parsed from the /W array of a CIDFont.
///
/// The /W array encodes widths in two forms:
/// - Individual: `cid_start [w1 w2 ...]` (consecutive CIDs from cid_start)
/// - Range: `cid_start cid_end w` (uniform width for a range of CIDs)
///
/// Widths are in glyph space units (typically 1/1000 of text space).
#[derive(Debug, Clone)]
pub struct CidWidths {
    widths: BTreeMap<u32, f64>,
}

impl Default for CidWidths {
    fn default() -> Self {
        Self::new()
    }
}

impl CidWidths {
    /// Create an empty CID width table.
    pub fn new() -> Self {
        Self {
            widths: BTreeMap::new(),
        }
    }

    /// Create from a pre-built map.
    pub fn from_map(widths: BTreeMap<u32, f64>) -> Self {
        Self { widths }
    }

    /// Number of explicit width entries.
    pub fn len(&self) -> usize {
        self.widths.len()
    }

    /// Whether the width table has no entries.
    pub fn is_empty(&self) -> bool {
        self.widths.is_empty()
    }

    /// Look up the width for a given CID. Returns None if not in the table
    /// (caller should fall back to /DW default width).
    pub fn width(&self, cid: u32) -> Option<f64> {
        self.widths.get(&cid).copied()
    }

    /// Iterate explicit (cid, width) entries in ascending CID order.
    ///
    /// Used by the renderer to build a lookup map from parsed `/W` data
    /// so CID TrueType subsets render with the PDF-declared advances even
    /// when the embedded font's `hmtx` disagrees (MS Word export, see
    /// issue #182).
    pub fn iter(&self) -> impl Iterator<Item = (u32, f64)> + '_ {
        self.widths.iter().map(|(&cid, &w)| (cid, w))
    }
}

/// Per-character-code width table for simple (single-byte) fonts.
///
/// Parsed from /FirstChar, /LastChar, and /Widths entries in the font dict.
/// Character code N maps to Widths[N - FirstChar].
///
/// Widths are in glyph space units (typically 1/1000 of text space).
#[derive(Debug, Clone)]
pub struct SimpleWidths {
    first_char: u32,
    widths: Vec<f64>,
}

impl SimpleWidths {
    /// Create from parsed values.
    pub fn new(first_char: u32, widths: Vec<f64>) -> Self {
        Self { first_char, widths }
    }

    /// Look up the width for a given character code. Returns None if the code
    /// is outside the [FirstChar, LastChar] range.
    pub fn width(&self, code: u32) -> Option<f64> {
        if code < self.first_char {
            return None;
        }
        let index = (code - self.first_char) as usize;
        self.widths.get(index).copied()
    }
}

/// A Type3 font (user-defined glyph shapes) -- format-agnostic core.
///
/// Type3 fonts define their own glyph drawings via content streams
/// (CharProcs). The CharProc stream references live in a PDF-specific
/// wrapper (`udoc_pdf::font::type3_pdf::Type3FontPdfRefs`) since ObjRef
/// is PDF-only; this core struct holds just the metadata needed for
/// character decoding and width lookups.
#[derive(Debug)]
pub struct Type3FontCore {
    /// Character-code to Unicode encoding table.
    pub encoding: Encoding,
    /// Parsed `/ToUnicode` CMap, if provided.
    pub tounicode: Option<ToUnicodeCMap>,
    /// Parsed `/FirstChar` + `/Widths` table, if provided.
    pub widths: Option<SimpleWidths>,
    /// Transform from glyph space to text space. Default [0.001 0 0 0.001 0 0].
    pub font_matrix: [f64; 6],
    /// Reverse mapping from character code to glyph name (from /Differences).
    /// Needed for CharProc lookup: code -> glyph name -> CharProc stream.
    pub glyph_names: HashMap<u8, String>,
    /// /BaseFont name, if present (for diagnostics).
    pub base_font: Option<String>,
}

impl Font {
    /// Map a character code to a Unicode string.
    ///
    /// Fallback chain:
    /// 1. ToUnicode CMap (most authoritative)
    /// 2. AGL glyph name lookup (simple fonts only, requires glyph name)
    /// 3. Encoding table (simple fonts only)
    /// 4. U+FFFD replacement character
    pub fn decode_char(&self, code: &[u8]) -> String {
        match self {
            Font::Simple(f) => f.decode_char(code),
            Font::Composite(f) => f.decode_char(code),
            Font::Type3(f) => f.decode_char(code),
        }
    }

    /// Decode a full byte string into Unicode text.
    ///
    /// Composite fonts use variable-length code matching from the parsed CMap
    /// when available; simple and Type3 fonts decode byte-by-byte.
    pub fn decode_string(&self, bytes: &[u8]) -> String {
        self.decode_string_with(bytes, |code| self.decode_char(code))
    }

    /// Decode a byte string, routing every per-code lookup through the
    /// supplied callback.
    ///
    /// Same code-boundary logic as [`Font::decode_string`] (variable-length
    /// for composite fonts via the parsed CMap, fixed stride for
    /// simple/Type3), but lets the caller wrap each `decode_char(code)` call
    /// in a cache. The callback receives a borrow of the raw code bytes (1-4
    /// bytes typically); the returned `String` is appended to the output.
    ///
    /// This is the per-page `(font_id, glyph_code)` LRU hook used by
    /// `udoc-pdf`'s content interpreter. The font crate
    /// stays cache-agnostic; the cache lifetime is the caller's problem.
    pub fn decode_string_with<F>(&self, bytes: &[u8], mut decode: F) -> String
    where
        F: FnMut(&[u8]) -> String,
    {
        let mut result = String::with_capacity(bytes.len());
        match self {
            Font::Composite(f) => {
                let mut i = 0;
                if let Some(ref cmap) = f.parsed_cmap {
                    while i < bytes.len() {
                        let code_len = cmap.code_length_for(bytes, i);
                        let end = (i + code_len).min(bytes.len());
                        result.push_str(&decode(&bytes[i..end]));
                        i = end;
                    }
                } else {
                    let code_len = f.code_length as usize;
                    while i + code_len <= bytes.len() {
                        result.push_str(&decode(&bytes[i..i + code_len]));
                        i += code_len;
                    }
                    if i < bytes.len() {
                        result.push_str(&decode(&bytes[i..]));
                    }
                }
            }
            Font::Simple(_) | Font::Type3(_) => {
                // Single-byte stride. Reuse a one-element scratch slice via
                // the `code` arg to the callback so each iteration just
                // overwrites the byte.
                let mut code = [0u8; 1];
                for &byte in bytes {
                    code[0] = byte;
                    result.push_str(&decode(&code));
                }
            }
        }
        result
    }

    /// Get the width of a character code in glyph space units (typically 1/1000
    /// of text space).
    ///
    /// Fallback chain:
    /// - Simple fonts: /Widths table -> 600 (reasonable default for Latin text)
    /// - Composite fonts: /W table -> /DW -> 1000 (full em)
    /// - Type3 fonts: /Widths table -> 0 (Type3 widths are in FontMatrix units)
    pub fn char_width(&self, code: u32) -> f64 {
        match self {
            Font::Simple(f) => f.char_width(code),
            Font::Composite(f) => f.char_width(code),
            Font::Type3(f) => f.char_width(code),
        }
    }

    /// Font name for display/debugging, with subset prefix stripped.
    ///
    /// Subsetted fonts have names like "ABCDEF+Helvetica". This returns
    /// just "Helvetica". Returns "unknown" if no name is available.
    pub fn name(&self) -> &str {
        strip_subset_prefix(self.raw_name())
    }

    /// Raw font name including any subset prefix (e.g. "ABCDEF+Helvetica").
    ///
    /// Each subset embedded in the PDF has a unique 6-letter prefix.
    /// Use this as a font identifier when distinguishing subsets is required
    /// (e.g., keying a renderer font cache so bytes from one subset don't
    /// resolve against another subset's glyph program).
    pub fn raw_name(&self) -> &str {
        let raw = match self {
            Font::Simple(f) => f.base_font.as_deref(),
            Font::Composite(f) => f.base_font.as_deref(),
            Font::Type3(f) => f.base_font.as_deref(),
        };
        raw.unwrap_or("unknown")
    }

    /// Number of bytes per character code.
    /// Simple and Type3 fonts: 1.
    /// Composite fonts: from CMap (typically 2).
    pub fn code_length(&self) -> u8 {
        match self {
            Font::Simple(_) | Font::Type3(_) => 1,
            Font::Composite(f) => f.code_length,
        }
    }

    /// Get Type3 font data if this is a Type3 font.
    pub fn as_type3(&self) -> Option<&Type3FontCore> {
        match self {
            Font::Type3(f) => Some(f),
            _ => None,
        }
    }

    /// Get the encoding byte->glyph_name mapping for simple fonts.
    /// Returns None for composite fonts or fonts without encoding data.
    /// Get the encoding byte->glyph_name mapping for simple/Type3 fonts.
    /// Uses the Encoding's char mapping + AGL reverse lookup for simple fonts,
    /// or the direct glyph_names map for Type3 fonts.
    pub fn encoding_glyph_names(&self) -> Option<Vec<(u8, String)>> {
        match self {
            Font::Simple(f) => {
                // Build encoding map from base encoding (AGL reverse lookup),
                // then overlay /Differences entries which take priority.
                // This ensures codes NOT in /Differences still get mapped
                // via the base encoding (e.g., WinAnsi code 0x41 -> "A").
                let mut map: std::collections::HashMap<u8, String> =
                    std::collections::HashMap::new();

                // Base encoding: code -> Unicode -> AGL glyph name.
                for code in 0u8..=255 {
                    if let Some(ch) = f.encoding.lookup(code) {
                        if let Some(name) = super::encoding::char_to_glyph_name(ch) {
                            map.insert(code, name.to_string());
                        }
                    }
                }

                // Overlay /Differences: these take priority over base encoding.
                for (code, name) in &f.differences_names {
                    map.insert(*code, name.clone());
                }

                if map.is_empty() {
                    None
                } else {
                    Some(map.into_iter().collect())
                }
            }
            Font::Type3(f) => {
                let map: Vec<(u8, String)> = f
                    .glyph_names
                    .iter()
                    .map(|(&code, name)| (code, name.clone()))
                    .collect();
                if map.is_empty() {
                    None
                } else {
                    Some(map)
                }
            }
            Font::Composite(_) => None,
        }
    }

    /// Whether this font uses vertical writing mode (CJK).
    /// Only composite fonts can be vertical (CMap name ends in -V).
    pub fn is_vertical(&self) -> bool {
        match self {
            Font::Composite(f) => f.is_vertical,
            Font::Simple(_) | Font::Type3(_) => false,
        }
    }

    /// Get the raw embedded font program bytes, if available.
    ///
    /// Returns None for fonts that reference standard fonts by name without
    /// embedding, and for Type3 fonts (which use PDF content streams, not
    /// font programs).
    #[allow(dead_code)] // will be used when renderer wires PDF font data
    pub fn font_data(&self) -> Option<&[u8]> {
        match self {
            Font::Simple(f) => f.font_data.as_deref(),
            Font::Composite(f) => f.font_data.as_deref(),
            Font::Type3(_) => None,
        }
    }

    /// Get the type of embedded font program.
    #[allow(dead_code)] // will be used when renderer wires PDF font data
    pub fn font_program(&self) -> FontProgram {
        match self {
            Font::Simple(f) => f.font_program,
            Font::Composite(f) => f.font_program,
            Font::Type3(_) => FontProgram::None,
        }
    }

    /// Parsed `/W` table + `/DW` default width for composite (Type0) fonts.
    ///
    /// Returns `None` for simple and Type3 fonts. For composite fonts, returns
    /// `(default_width, entries)` where entries is the explicit `(cid, width)`
    /// pairs from `/W`. Widths are in glyph-space units (1/1000 em).
    ///
    /// The renderer uses this to wire PDF-declared CID advances into the CID
    /// glyph-advance lookup so MS Word export PDFs, which embed CIDFontType2
    /// subsets with `/W` values that disagree with the embedded `hmtx`, render
    /// with the correct per-glyph spacing (see issue #182).
    pub fn cid_widths(&self) -> Option<(u32, Vec<(u32, f64)>)> {
        match self {
            Font::Composite(f) => {
                let entries: Vec<(u32, f64)> = f.descendant.widths.iter().collect();
                Some((f.descendant.default_width, entries))
            }
            _ => None,
        }
    }

    /// Whether the font has explicit width metrics.
    /// True when the font provides /Widths (simple/Type3), /W table
    /// (composite), or is a standard PDF font with known AFM widths.
    /// When false, char widths are estimates (default 600).
    pub fn has_metrics(&self) -> bool {
        match self {
            Font::Simple(f) => {
                if f.widths.is_some() {
                    return true;
                }
                // Standard fonts have known widths even without /Widths
                f.base_font
                    .as_deref()
                    .map(strip_subset_prefix)
                    .is_some_and(super::standard_widths::is_standard_font)
            }
            Font::Composite(f) => !f.descendant.widths.is_empty(),
            Font::Type3(f) => f.widths.is_some(),
        }
    }

    /// Raw space glyph width in glyph-space units (1/1000 of text space),
    /// only when the font has explicit metrics for the space character.
    /// Returns None when the width would be a fallback/default value.
    pub fn space_width_raw(&self) -> Option<f64> {
        match self {
            Font::Simple(f) => {
                // 1. Font's /Widths array (skip zero: TeX sets space width
                // to 0 because it positions words via Tm, never renders spaces)
                if let Some(ref w) = f.widths {
                    if let Some(width) = w.width(0x20) {
                        if width > 0.0 {
                            return Some(width);
                        }
                    }
                }
                // 2. Standard font table
                let name = strip_subset_prefix(f.base_font.as_deref()?);
                super::standard_widths::standard_width(name, 0x20).map(|w| w as f64)
            }
            Font::Composite(f) => {
                // Space is at CID 32 for Identity-H/V and most standard CMaps.
                // Some CJK CMaps map space to a different CID, in which case
                // this returns None and we fall through to Tier 2 (size-relative).
                // Space CID lookup via CMap for non-Identity encodings is
                // deferred: no corpus files have triggered this path yet.
                f.descendant.widths.width(32)
            }
            Font::Type3(f) => {
                let raw = f.widths.as_ref()?.width(0x20)?;
                // Type3 widths are in glyph space; scale through FontMatrix
                // so callers get standard 1/1000-of-text-space units.
                Some(raw * f.font_matrix[0] * 1000.0)
            }
        }
    }
}

impl SimpleFont {
    fn char_width(&self, code: u32) -> f64 {
        // 1. Font's /Widths array (most authoritative)
        if let Some(ref w) = self.widths {
            if let Some(width) = w.width(code) {
                return width;
            }
        }
        // 2. Standard font width table (Adobe AFM data for the 14 base fonts)
        if code <= 255 {
            if let Some(name) = &self.base_font {
                let stripped = strip_subset_prefix(name);
                if let Some(w) = super::standard_widths::standard_width(stripped, code as u8) {
                    return w as f64;
                }
            }
        }
        // 3. Default fallback. 600 is a reasonable average for Latin text.
        600.0
    }

    fn decode_char(&self, code: &[u8]) -> String {
        // 1. ToUnicode
        if let Some(ref cmap) = self.tounicode {
            if let Some(s) = cmap.lookup(code) {
                return s;
            }
        }

        // For single-byte codes, try encoding
        if code.len() == 1 {
            let byte = code[0];
            // 2. Encoding table (which includes AGL via /Differences glyph names)
            if let Some(c) = self.encoding.lookup(byte) {
                // Decompose Unicode ligatures (U+FB00-FB06) to plain ASCII.
                // The encoding may map codes to ligature chars via AGL or
                // standard encoding tables. Text extraction needs "fi" not U+FB01.
                if let Some(decomposed) = super::encoding::decompose_ligature(c) {
                    return decomposed.to_string();
                }
                return c.to_string();
            }

            // 3. MacRoman fallback for unmapped high-byte codes.
            // Many older PDFs and some TeX outputs use MacRoman-compatible
            // encodings without declaring it. Better to guess MacRoman than
            // emit U+FFFD for the 128-255 range.
            // Diagnostic warning for MacRoman fallback is deferred: would
            // require threading DiagnosticsSink through decode_char().
            if byte >= 0x80 {
                if let Some(c) = super::encoding::StandardEncoding::MacRoman.lookup(byte) {
                    if let Some(decomposed) = super::encoding::decompose_ligature(c) {
                        return decomposed.to_string();
                    }
                    return c.to_string();
                }
            }
        }

        // 4. Replacement character
        "\u{FFFD}".to_string()
    }
}

impl CompositeFont {
    fn char_width(&self, code: u32) -> f64 {
        // Try per-CID width table first, fall back to /DW
        self.descendant
            .widths
            .width(code)
            .unwrap_or(self.descendant.default_width as f64)
    }

    fn decode_char(&self, code: &[u8]) -> String {
        // 1. ToUnicode (most authoritative)
        if let Some(ref cmap) = self.tounicode {
            if let Some(s) = cmap.lookup(code) {
                return s;
            }
        }

        // 2. Parsed CMap unicode lookup
        if let Some(ref parsed) = self.parsed_cmap {
            if let Some(s) = parsed.lookup_unicode(code) {
                return s;
            }
            // Try CID lookup and convert via cid_to_unicode_fallback
            if let Some(cid) = parsed.lookup_cid(code) {
                if let Some(c) = super::cmap::cid_to_unicode_fallback(cid, &self.encoding_name) {
                    return c.to_string();
                }
            }
        }

        // 3. CMap-based fallback: interpret CID as Unicode for Identity encodings.
        // Converts the code bytes to a CID (big-endian), then tries direct
        // Unicode mapping. Works for most modern PDF generators.
        let cid = code_to_u32(code);
        if let Some(c) = super::cmap::cid_to_unicode_fallback(cid, &self.encoding_name) {
            return c.to_string();
        }

        // 4. Replacement character
        "\u{FFFD}".to_string()
    }

    // Note: `decode_string` for composites lives on `Font::decode_string_with`
    //. The variable-length code-boundary walk used to live here
    // but was hoisted up so callers can interpose a per-glyph cache without
    // duplicating the walk.
}

/// Strip the 6-letter subset prefix from a font name (e.g. "ABCDEF+Helvetica" -> "Helvetica").
/// Returns the name unchanged if no subset prefix is present.
pub fn strip_subset_prefix(name: &str) -> &str {
    match name.find('+') {
        Some(pos) if pos == 6 && name[..6].chars().all(|c| c.is_ascii_uppercase()) => {
            &name[pos + 1..]
        }
        _ => name,
    }
}

/// Convert a big-endian byte sequence to a u32 code value.
pub fn code_to_u32(bytes: &[u8]) -> u32 {
    let mut val = 0u32;
    for &b in bytes {
        val = (val << 8) | b as u32;
    }
    val
}

impl Type3FontCore {
    fn char_width(&self, code: u32) -> f64 {
        if let Some(ref w) = self.widths {
            if let Some(width) = w.width(code) {
                // /Widths values are in glyph space. Scale by font_matrix[0]
                // and multiply by 1000 so the interpreter's universal
                // `w0 / 1000.0 * font_size` formula works for Type3 too.
                // With default matrix (0.001): 1000 * 0.001 * 1000 = 1000,
                // interpreter computes 1000 / 1000 = 1.0 text unit. Correct.
                return width * self.font_matrix[0] * 1000.0;
            }
        }
        // No width table or code out of range. 0.0 since we can't assume
        // glyph-space units without knowing the font's design.
        0.0
    }

    fn decode_char(&self, code: &[u8]) -> String {
        // Same fallback as simple fonts
        if let Some(ref tounicode) = self.tounicode {
            if let Some(s) = tounicode.lookup(code) {
                return s;
            }
        }

        if code.len() == 1 {
            if let Some(c) = self.encoding.lookup(code[0]) {
                // Decompose Unicode ligatures to plain ASCII (same as SimpleFont)
                if let Some(decomposed) = super::encoding::decompose_ligature(c) {
                    return decomposed.to_string();
                }
                return c.to_string();
            }
        }

        // No MacRoman fallback for Type3 fonts. Unlike SimpleFont, Type3 glyphs
        // are defined by CharProc streams with arbitrary drawing commands, so
        // guessing MacRoman encoding for unmapped high bytes would be wrong more
        // often than right. CharProc text extraction is the appropriate
        // fallback for Type3.
        "\u{FFFD}".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::{Encoding, StandardEncoding};

    /// Helper: build a SimpleFont with the given encoding for testing.
    fn simple_font_with_encoding(encoding: Encoding) -> SimpleFont {
        SimpleFont {
            subtype: SimpleSubtype::Type1,
            base_font: None,
            encoding,
            tounicode: None,
            widths: None,
            font_data: None,
            font_program: FontProgram::None,
            differences_names: Vec::new(),
        }
    }

    #[test]
    fn test_composite_font_cid_widths_accessor() {
        // Font::cid_widths should expose the /DW default and /W entries for
        // composite fonts, and return None for simple/Type3.
        let mut widths = BTreeMap::new();
        widths.insert(1, 500.0);
        widths.insert(42, 750.0);
        let composite = Font::Composite(CompositeFont {
            base_font: Some("CIDFont+F3".into()),
            font_data: None,
            font_program: FontProgram::TrueType,
            encoding_name: "Identity-H".into(),
            tounicode: None,
            descendant: CidFont {
                subtype: CidSubtype::Type2,
                base_font: None,
                default_width: 1000,
                widths: CidWidths::from_map(widths),
            },
            code_length: 2,
            is_vertical: false,
            parsed_cmap: None,
        });
        let (dw, entries) = composite.cid_widths().expect("composite should expose /W");
        assert_eq!(dw, 1000);
        assert_eq!(entries.len(), 2);
        assert!(entries.contains(&(1, 500.0)));
        assert!(entries.contains(&(42, 750.0)));

        let simple = Font::Simple(simple_font_with_encoding(Encoding::Standard(
            StandardEncoding::Standard,
        )));
        assert!(simple.cid_widths().is_none());
    }

    #[test]
    fn test_simple_font_ligature_decomposition() {
        // StandardEncoding maps 0xAE -> U+FB01 (fi), 0xAF -> U+FB02 (fl)
        let font = simple_font_with_encoding(Encoding::Standard(StandardEncoding::Standard));
        assert_eq!(font.decode_char(&[0xAE]), "fi");
        assert_eq!(font.decode_char(&[0xAF]), "fl");
    }

    #[test]
    fn test_simple_font_macroman_ligature_decomposition() {
        // MacRoman maps 0xDE -> U+FB01 (fi), 0xDF -> U+FB02 (fl)
        let font = simple_font_with_encoding(Encoding::Standard(StandardEncoding::MacRoman));
        assert_eq!(font.decode_char(&[0xDE]), "fi");
        assert_eq!(font.decode_char(&[0xDF]), "fl");
    }

    #[test]
    fn test_simple_font_custom_encoding_with_ligature() {
        // Custom encoding with f_i mapped via /Differences -> U+FB01
        let enc = Encoding::custom(StandardEncoding::Standard, &[(42, '\u{FB01}')]);
        let font = simple_font_with_encoding(enc);
        // Should decompose to "fi", not the raw ligature char
        assert_eq!(font.decode_char(&[42]), "fi");
    }

    #[test]
    fn test_simple_font_macroman_fallback_for_unmapped_high_byte() {
        // StandardEncoding has gaps (e.g. 0x80 is undefined).
        // The MacRoman fallback should kick in for these unmapped high-byte codes.
        let font = simple_font_with_encoding(Encoding::Standard(StandardEncoding::Standard));
        // 0x80 is undefined in StandardEncoding but maps to A-diaeresis in MacRoman
        assert_eq!(font.decode_char(&[0x80]), "\u{00C4}");
    }

    #[test]
    fn test_simple_font_macroman_fallback_does_not_override_primary() {
        // WinAnsi maps 0x80 -> Euro sign. MacRoman fallback should NOT activate.
        let font = simple_font_with_encoding(Encoding::Standard(StandardEncoding::WinAnsi));
        assert_eq!(font.decode_char(&[0x80]), "\u{20AC}"); // Euro, not A-diaeresis
    }

    #[test]
    fn test_simple_font_ascii_range_no_fallback() {
        // ASCII range should work from primary encoding, no MacRoman fallback
        let font = simple_font_with_encoding(Encoding::Standard(StandardEncoding::WinAnsi));
        assert_eq!(font.decode_char(&[0x41]), "A");
        assert_eq!(font.decode_char(&[0x20]), " ");
    }

    #[test]
    fn test_simple_font_replacement_char_for_control_codes() {
        // Control codes (0x00-0x1F) should still produce U+FFFD even with
        // MacRoman fallback, since MacRoman also returns None for control chars.
        let font = simple_font_with_encoding(Encoding::Standard(StandardEncoding::Standard));
        assert_eq!(font.decode_char(&[0x00]), "\u{FFFD}");
        assert_eq!(font.decode_char(&[0x1F]), "\u{FFFD}");
    }

    #[test]
    fn test_type3_font_ligature_decomposition() {
        let font = Type3FontCore {
            encoding: Encoding::Standard(StandardEncoding::Standard),
            tounicode: None,
            widths: None,
            font_matrix: [0.001, 0.0, 0.0, 0.001, 0.0, 0.0],
            glyph_names: HashMap::new(),
            base_font: None,
        };
        // StandardEncoding maps 0xAE -> U+FB01 (fi)
        assert_eq!(font.decode_char(&[0xAE]), "fi");
    }
}
