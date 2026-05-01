//! Type1 font outline parser for PDF rendering.
//!
//! Parses embedded Type1 font programs (FontFile streams) to extract glyph
//! outlines for rasterization. Handles PFB binary envelope, eexec decryption,
//! PostScript /CharStrings extraction, and Type1 charstring interpretation.
//!
//! Type1 charstrings use cubic bezier curves (same as CFF/Type2) but with a
//! simpler operator set and no subroutine calls within charstrings.

use std::collections::HashMap;

use crate::error::{Error, Result};

use super::encoding::parse_glyph_name;
use super::postscript::discovered_stems::{discover_vertical_stems, StemLimits};
use super::ttf::{Contour, GlyphOutline, OutlinePoint, StemHints};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Type1 Private DICT hint values for blue zone alignment.
#[derive(Debug, Clone, Default)]
pub struct Type1HintValues {
    /// Alignment zone pairs from /BlueValues (baseline, x-height, cap-height, ascender).
    /// Each pair is (bottom, top) in font units.
    pub blue_values: Vec<(f64, f64)>,
    /// Descender zone pairs from /OtherBlues.
    pub other_blues: Vec<(f64, f64)>,
    /// Standard horizontal stem width in font units.
    pub std_hw: f64,
    /// Standard vertical stem width in font units.
    pub std_vw: f64,
    /// ppem threshold below which overshoot suppression applies.
    pub blue_scale: f64,
    /// Overshoot amount threshold in font units.
    pub blue_shift: f64,
    /// Zone extension for near-miss alignment in font units.
    pub blue_fuzz: f64,
}

/// Parsed Type 1 (PostScript) font program.
pub struct Type1Font {
    /// Glyph name -> charstring bytes (decrypted).
    charstrings: HashMap<String, Vec<u8>>,
    /// Subroutines (from /Subrs array), indexed by number.
    subrs: Vec<Vec<u8>>,
    /// Glyph name -> Unicode character mapping (used for advance_width lookups).
    #[allow(dead_code)]
    name_to_char: HashMap<String, char>,
    /// Unicode character -> glyph name reverse mapping.
    char_to_name: HashMap<char, String>,
    /// Per-glyph sidebearing and width from hsbw/sbw.
    widths: HashMap<String, u16>,
    /// Built-in encoding: byte code -> glyph name from the font program's
    /// /Encoding array. Empty if no encoding was found.
    builtin_encoding: HashMap<u8, String>,
    /// Private DICT hint values (blue zones, standard stems).
    hint_values: Type1HintValues,
}

impl Type1Font {
    /// Parse a Type1 font from raw FontFile stream bytes.
    ///
    /// Handles both PFB (binary, 0x80 prefix) and PFA (ASCII) formats.
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() < 16 {
            return Err(Error::new("Type1 data too short"));
        }

        // Detect PFB vs PFA format.
        let (cleartext, encrypted) = if data.first() == Some(&0x80) {
            parse_pfb(data)?
        } else {
            split_pfa(data)?
        };

        // Decrypt the eexec-encrypted section.
        let decrypted = eexec_decrypt(&encrypted);

        // Parse /lenIV from the Private dict (number of random bytes prefixing
        // each charstring). Default is 4, but some fonts (notably LinLibertine)
        // use 0.
        let len_iv = parse_len_iv(&decrypted).unwrap_or(4);

        // Extract /Subrs from the decrypted section (needed for callsubr).
        let raw_subrs = extract_subrs(&decrypted, &cleartext);

        // Decrypt subroutine charstring bytes.
        let subrs: Vec<Vec<u8>> = raw_subrs
            .into_iter()
            .map(|data| charstring_decrypt_with_len_iv(&data, len_iv))
            .collect();

        // Extract /CharStrings from the decrypted section.
        let raw_charstrings = extract_charstrings(&decrypted, &cleartext)?;

        // Decrypt individual charstring bytes.
        let mut charstrings = HashMap::new();
        for (name, cs_data) in raw_charstrings {
            let decrypted_cs = charstring_decrypt_with_len_iv(&cs_data, len_iv);
            charstrings.insert(name, decrypted_cs);
        }

        // Parse the /Encoding array from cleartext (byte code -> glyph name).
        let builtin_encoding = parse_type1_encoding(&cleartext);

        // Build glyph name <-> Unicode mappings via AGL.
        // Iterate charstrings.keys() in sorted order so that when multiple
        // glyph names map to the same Unicode codepoint (e.g. "Delta" and
        // "uni0394"), char_to_name picks deterministically. HashMap key
        // iteration order varies with the random hasher seed, and a
        // different chosen glyph means a different rendered outline.
        let mut name_to_char = HashMap::new();
        let mut char_to_name = HashMap::new();
        let mut sorted_names: Vec<&String> = charstrings.keys().collect();
        sorted_names.sort();
        for name in sorted_names {
            if let Some(ch) = parse_glyph_name(name) {
                name_to_char.insert(name.clone(), ch);
                char_to_name.insert(ch, name.clone());
            }
        }

        // Parse Private DICT hint values (blue zones, standard stems).
        let hint_values = parse_private_dict_hints(&decrypted);

        Ok(Type1Font {
            charstrings,
            subrs,
            name_to_char,
            char_to_name,
            widths: HashMap::new(),
            builtin_encoding,
            hint_values,
        })
    }

    /// Extract the glyph outline for a Unicode character.
    ///
    /// Interprets the Type1 charstring to produce cubic bezier contours.
    /// Returns None if the character is unmapped or the charstring fails.
    pub fn glyph_outline(&self, ch: char) -> Option<GlyphOutline> {
        let name = self.char_to_name.get(&ch)?;
        self.glyph_outline_by_name(name)
    }

    /// Extract glyph outline by PostScript glyph name.
    /// Bypasses the Unicode -> name mapping, looking up the charstring directly.
    pub fn glyph_outline_by_name(&self, name: &str) -> Option<GlyphOutline> {
        self.interpret_charstring(name, 0)
    }

    /// Interpret a charstring, handling seac composites with recursion guard.
    fn interpret_charstring(&self, name: &str, depth: u32) -> Option<GlyphOutline> {
        if depth > 2 {
            return None; // prevent infinite seac recursion
        }
        let cs_data = self.charstrings.get(name)?;
        if cs_data.is_empty() {
            return None;
        }
        let mut interp = Type1Interpreter::new(&self.subrs);
        interp.execute(cs_data)?;

        // Handle seac: composite accented character.
        if let Some((_asb, adx, ady, bchar, achar)) = interp.seac_data {
            let base_name = standard_encoding_name(bchar)?;
            let accent_name = standard_encoding_name(achar)?;
            let base = self.interpret_charstring(base_name, depth + 1)?;
            let accent = self.interpret_charstring(accent_name, depth + 1)?;
            // Offset accent contours by (adx, ady) and merge with base.
            let mut contours = base.contours;
            for ac in &accent.contours {
                let mut shifted = ac.clone();
                for pt in &mut shifted.points {
                    pt.x += adx;
                    pt.y += ady;
                }
                contours.push(shifted);
            }
            return Some(Self::build_outline(
                contours,
                interp.h_stems,
                interp.v_stems,
            ));
        }

        if interp.contours.is_empty() {
            return None;
        }
        Some(Self::build_outline(
            interp.contours,
            interp.h_stems,
            interp.v_stems,
        ))
    }

    fn build_outline(
        contours: Vec<Contour>,
        h_stems: Vec<(f64, f64)>,
        v_stems: Vec<(f64, f64)>,
    ) -> GlyphOutline {
        let mut x_min = f64::MAX;
        let mut y_min = f64::MAX;
        let mut x_max = f64::MIN;
        let mut y_max = f64::MIN;
        for contour in &contours {
            for pt in &contour.points {
                x_min = x_min.min(pt.x);
                y_min = y_min.min(pt.y);
                x_max = x_max.max(pt.x);
                y_max = y_max.max(pt.y);
            }
        }
        let mut hints = StemHints { h_stems, v_stems };
        hints
            .h_stems
            .dedup_by(|a, b| (a.0 - b.0).abs() < 0.1 && (a.1 - b.1).abs() < 0.1);
        hints
            .v_stems
            .dedup_by(|a, b| (a.0 - b.0).abs() < 0.1 && (a.1 - b.1).abs() < 0.1);

        // Scan the outline for vertical stem candidates that the
        // charstring didn't declare, following FreeType's
        // `ps_hints_stem` / `ps_builder_add_stem` pass. This fills
        // the gap for Type1 fonts (notably Computer Modern) that
        // describe only the primary stem of a glyph in hints, leaving
        // secondary edges implicit in the outline. Discovered stems
        // get prepended so the PS hint fitter's "closest reference
        // wins" interpolation still prefers declared stems when both
        // describe the same edge.
        let discovered = discover_vertical_stems(&contours, &hints.v_stems, StemLimits::default());
        if !discovered.is_empty() {
            let mut merged = discovered;
            merged.append(&mut hints.v_stems);
            hints.v_stems = merged;
        }

        GlyphOutline {
            contours,
            bounds: (x_min as i16, y_min as i16, x_max as i16, y_max as i16),
            stem_hints: hints,
        }
    }

    /// Get Type1 Private DICT hint values (blue zones, standard stems).
    pub fn hint_values(&self) -> &Type1HintValues {
        &self.hint_values
    }

    /// Get the built-in encoding (byte code -> glyph name) from the font program.
    /// Returns None if the font has no encoding or it's empty.
    pub fn builtin_encoding(&self) -> Option<&HashMap<u8, String>> {
        if self.builtin_encoding.is_empty() {
            None
        } else {
            Some(&self.builtin_encoding)
        }
    }

    /// Get advance width for a character (from hsbw in charstring).
    pub fn advance_width(&self, ch: char) -> Option<u16> {
        let name = self.char_to_name.get(&ch)?;
        // If we already cached it, return it.
        if let Some(&w) = self.widths.get(name) {
            return Some(w);
        }
        // Otherwise interpret the charstring to get the width from hsbw.
        let cs_data = self.charstrings.get(name)?;
        let mut interp = Type1Interpreter::new(&self.subrs);
        interp.execute(cs_data);
        if interp.width > 0.0 {
            Some(interp.width as u16)
        } else {
            None
        }
    }

    /// Units per em. Type1 fonts use 1000 by convention.
    pub fn units_per_em(&self) -> u16 {
        1000
    }

    /// Number of charstrings in the font.
    #[allow(dead_code)]
    pub fn num_glyphs(&self) -> usize {
        self.charstrings.len()
    }

    /// Get the glyph names in this font (for diagnostics).
    pub fn glyph_names(&self) -> Vec<&str> {
        self.charstrings.keys().map(|s| s.as_str()).collect()
    }

    /// Number of subroutines in the font.
    pub fn num_subrs(&self) -> usize {
        self.subrs.len()
    }

    /// Count non-empty subroutines.
    pub fn populated_subrs(&self) -> usize {
        self.subrs.iter().filter(|s| !s.is_empty()).count()
    }

    /// Diagnostic: check charstring data and interpretation for a glyph name.
    pub fn diagnose_glyph(&self, name: &str) -> String {
        let Some(cs_data) = self.charstrings.get(name) else {
            return format!("no charstring for '{name}'");
        };
        if cs_data.is_empty() {
            return format!("empty charstring for '{name}'");
        }
        let first_bytes: Vec<u8> = cs_data.iter().copied().take(20).collect();
        let mut interp = Type1Interpreter::new(&self.subrs);
        let result = interp.execute(cs_data);
        format!(
            "cs_len={}, first_bytes={:?}, exec={}, contours={}, width={:.0}",
            cs_data.len(),
            first_bytes,
            if result.is_some() { "ok" } else { "FAIL" },
            interp.contours.len(),
            interp.width,
        )
    }

    /// Trace charstring execution for a glyph, logging each operator.
    pub fn trace_glyph(&self, name: &str) -> Vec<String> {
        let Some(cs_data) = self.charstrings.get(name) else {
            return vec!["no charstring".to_string()];
        };
        let mut interp = Type1Interpreter::new(&self.subrs);
        interp.trace = true;
        interp.execute(cs_data);
        interp.trace_log
    }

    /// Run the charstring interpreter and return the collected hstem/vstem
    /// declarations for a glyph, without building its outline.
    ///
    /// Useful for diagnostics and regression tests that verify Type1 hint
    /// emission is intact (see the `#177` investigation, where the renderer's
    /// inspect tool surfaced empty hints due to a fallback-font lookup while
    /// the real Type1 interpreter was emitting stems correctly). Returns
    /// `None` if no charstring exists for the requested glyph name.
    pub fn glyph_stems(&self, name: &str) -> Option<StemHints> {
        let cs_data = self.charstrings.get(name)?;
        let mut interp = Type1Interpreter::new(&self.subrs);
        interp.execute(cs_data);
        Some(StemHints {
            h_stems: interp.h_stems,
            v_stems: interp.v_stems,
        })
    }
}

// ---------------------------------------------------------------------------
// PFB envelope parsing
// ---------------------------------------------------------------------------

/// Parse a PFB (Printer Font Binary) envelope.
///
/// PFB segments: each starts with 0x80, a type byte, and a 4-byte LE length.
/// Type 1 = ASCII, Type 2 = binary (encrypted), Type 3 = EOF.
fn parse_pfb(data: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut ascii_parts = Vec::new();
    let mut binary_parts = Vec::new();
    let mut pos = 0;

    while pos + 1 < data.len() {
        if data[pos] != 0x80 {
            break;
        }
        let seg_type = data[pos + 1];
        if seg_type == 3 {
            // EOF marker
            break;
        }
        if pos + 5 >= data.len() {
            break;
        }
        let length =
            u32::from_le_bytes([data[pos + 2], data[pos + 3], data[pos + 4], data[pos + 5]])
                as usize;
        pos += 6;

        let end = (pos + length).min(data.len());
        let segment = &data[pos..end];

        match seg_type {
            1 => ascii_parts.extend_from_slice(segment),
            2 => binary_parts.extend_from_slice(segment),
            _ => {}
        }
        pos = end;
    }

    if ascii_parts.is_empty() && binary_parts.is_empty() {
        return Err(Error::new("PFB contains no data segments"));
    }

    Ok((ascii_parts, binary_parts))
}

/// Split a PFA (ASCII) font program at the eexec boundary.
///
/// Looks for "eexec" followed by encrypted hex or binary data.
fn split_pfa(data: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    // Find "eexec" keyword.
    let eexec_pos = find_bytes(data, b"eexec")
        .ok_or_else(|| Error::new("Type1 PFA: no eexec keyword found"))?;

    let cleartext = data[..eexec_pos].to_vec();

    // Skip "eexec" and any trailing whitespace.
    let mut pos = eexec_pos + 5;
    while pos < data.len() && (data[pos] == b' ' || data[pos] == b'\n' || data[pos] == b'\r') {
        pos += 1;
    }

    let encrypted_section = &data[pos..];

    // PFA encrypted sections are hex-encoded. Detect and decode.
    let encrypted = if is_hex_encoded(encrypted_section) {
        decode_hex(encrypted_section)
    } else {
        encrypted_section.to_vec()
    };

    Ok((cleartext, encrypted))
}

/// Check if data looks hex-encoded (first 8 bytes are all hex digits/whitespace).
fn is_hex_encoded(data: &[u8]) -> bool {
    let check_len = data.len().min(8);
    if check_len == 0 {
        return false;
    }
    data[..check_len]
        .iter()
        .all(|&b| b.is_ascii_hexdigit() || b == b' ' || b == b'\n' || b == b'\r')
}

/// Decode hex-encoded data, skipping whitespace.
fn decode_hex(data: &[u8]) -> Vec<u8> {
    let hex_chars: Vec<u8> = data
        .iter()
        .copied()
        .filter(|b| b.is_ascii_hexdigit())
        .collect();
    let mut result = Vec::with_capacity(hex_chars.len() / 2);
    let mut i = 0;
    while i + 1 < hex_chars.len() {
        let hi = hex_digit(hex_chars[i]);
        let lo = hex_digit(hex_chars[i + 1]);
        result.push((hi << 4) | lo);
        i += 2;
    }
    result
}

fn hex_digit(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// eexec decryption
// ---------------------------------------------------------------------------

/// Decrypt eexec-encrypted data using the Adobe Type1 encryption algorithm.
///
/// For each byte: plain = cipher XOR (r >> 8), then r = (cipher + r) * 52845 + 22719.
/// Seed: 55665. First 4 decrypted bytes are random (discarded).
fn eexec_decrypt(data: &[u8]) -> Vec<u8> {
    let mut r: u16 = 55665;
    let mut result = Vec::with_capacity(data.len());

    for &cipher in data {
        let plain = cipher ^ (r >> 8) as u8;
        r = (cipher as u16)
            .wrapping_add(r)
            .wrapping_mul(52845)
            .wrapping_add(22719);
        result.push(plain);
    }

    // Discard first 4 random bytes.
    if result.len() > 4 {
        result.drain(..4);
    }
    result
}

/// Map a Standard Encoding character code to a PostScript glyph name.
/// Used by seac to look up base and accent glyphs.
fn standard_encoding_name(code: u8) -> Option<&'static str> {
    match code {
        32 => Some("space"),
        33 => Some("exclam"),
        34 => Some("quotedbl"),
        35 => Some("numbersign"),
        36 => Some("dollar"),
        37 => Some("percent"),
        38 => Some("ampersand"),
        39 => Some("quoteright"),
        40 => Some("parenleft"),
        41 => Some("parenright"),
        42 => Some("asterisk"),
        43 => Some("plus"),
        44 => Some("comma"),
        45 => Some("hyphen"),
        46 => Some("period"),
        47 => Some("slash"),
        48..=57 => {
            const DIGITS: [&str; 10] = [
                "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
            ];
            Some(DIGITS[(code - 48) as usize])
        }
        58 => Some("colon"),
        59 => Some("semicolon"),
        60 => Some("less"),
        61 => Some("equal"),
        62 => Some("greater"),
        63 => Some("question"),
        64 => Some("at"),
        65..=90 => {
            const UPPER: [&str; 26] = [
                "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P",
                "Q", "R", "S", "T", "U", "V", "W", "X", "Y", "Z",
            ];
            Some(UPPER[(code - 65) as usize])
        }
        91 => Some("bracketleft"),
        92 => Some("backslash"),
        93 => Some("bracketright"),
        94 => Some("asciicircum"),
        95 => Some("underscore"),
        96 => Some("quoteleft"),
        97..=122 => {
            const LOWER: [&str; 26] = [
                "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p",
                "q", "r", "s", "t", "u", "v", "w", "x", "y", "z",
            ];
            Some(LOWER[(code - 97) as usize])
        }
        123 => Some("braceleft"),
        124 => Some("bar"),
        125 => Some("braceright"),
        126 => Some("asciitilde"),
        // Extended Standard Encoding (128+)
        161 => Some("exclamdown"),
        162 => Some("cent"),
        163 => Some("sterling"),
        164 => Some("fraction"),
        165 => Some("yen"),
        166 => Some("florin"),
        167 => Some("section"),
        168 => Some("currency"),
        169 => Some("quotesingle"),
        170 => Some("quotedblleft"),
        171 => Some("guillemotleft"),
        172 => Some("guilsinglleft"),
        173 => Some("guilsinglright"),
        174 => Some("fi"),
        175 => Some("fl"),
        177 => Some("endash"),
        178 => Some("dagger"),
        179 => Some("daggerdbl"),
        180 => Some("periodcentered"),
        182 => Some("paragraph"),
        183 => Some("bullet"),
        184 => Some("quotesinglbase"),
        185 => Some("quotedblbase"),
        186 => Some("quotedblright"),
        187 => Some("guillemotright"),
        188 => Some("ellipsis"),
        189 => Some("perthousand"),
        191 => Some("questiondown"),
        193 => Some("grave"),
        194 => Some("acute"),
        195 => Some("circumflex"),
        196 => Some("tilde"),
        197 => Some("macron"),
        198 => Some("breve"),
        199 => Some("dotaccent"),
        200 => Some("dieresis"),
        202 => Some("ring"),
        203 => Some("cedilla"),
        205 => Some("hungarumlaut"),
        206 => Some("ogonek"),
        207 => Some("caron"),
        208 => Some("emdash"),
        225 => Some("AE"),
        227 => Some("ordfeminine"),
        232 => Some("Lslash"),
        233 => Some("Oslash"),
        234 => Some("OE"),
        235 => Some("ordmasculine"),
        241 => Some("ae"),
        245 => Some("dotlessi"),
        248 => Some("lslash"),
        249 => Some("oslash"),
        250 => Some("oe"),
        251 => Some("germandbls"),
        _ => None,
    }
}

#[cfg(test)]
fn charstring_decrypt(data: &[u8]) -> Vec<u8> {
    charstring_decrypt_with_len_iv(data, 4)
}

fn charstring_decrypt_with_len_iv(data: &[u8], len_iv: usize) -> Vec<u8> {
    let mut r: u16 = 4330;
    let mut result = Vec::with_capacity(data.len());

    for &cipher in data {
        let plain = cipher ^ (r >> 8) as u8;
        r = (cipher as u16)
            .wrapping_add(r)
            .wrapping_mul(52845)
            .wrapping_add(22719);
        result.push(plain);
    }

    // Discard first lenIV random bytes (default 4, but some fonts use 0).
    if result.len() > len_iv {
        result.drain(..len_iv);
    }
    result
}

/// Parse Private DICT hint values (blue zones, standard stems) from decrypted data.
fn parse_private_dict_hints(decrypted: &[u8]) -> Type1HintValues {
    let mut vals = Type1HintValues {
        blue_scale: 0.039625, // default per Type1 spec
        blue_shift: 7.0,
        blue_fuzz: 1.0,
        ..Default::default()
    };

    // Parse /BlueValues array
    vals.blue_values = parse_ps_number_array_pairs(decrypted, b"/BlueValues");
    vals.other_blues = parse_ps_number_array_pairs(decrypted, b"/OtherBlues");

    // Parse scalar values
    if let Some(v) = parse_ps_number(decrypted, b"/BlueScale") {
        vals.blue_scale = v;
    }
    if let Some(v) = parse_ps_number(decrypted, b"/BlueShift") {
        vals.blue_shift = v;
    }
    if let Some(v) = parse_ps_number(decrypted, b"/BlueFuzz") {
        vals.blue_fuzz = v;
    }
    if let Some(v) = parse_ps_scalar_array_first(decrypted, b"/StdHW") {
        vals.std_hw = v;
    }
    if let Some(v) = parse_ps_scalar_array_first(decrypted, b"/StdVW") {
        vals.std_vw = v;
    }

    vals
}

/// Parse a PostScript number array into pairs: [a b c d] -> [(a,b), (c,d)].
fn parse_ps_number_array_pairs(data: &[u8], key: &[u8]) -> Vec<(f64, f64)> {
    let pos = match find_bytes(data, key) {
        Some(p) => p + key.len(),
        None => return Vec::new(),
    };
    let numbers = parse_ps_number_array(data, pos);
    numbers
        .chunks(2)
        .filter_map(|c| {
            if c.len() == 2 {
                Some((c[0], c[1]))
            } else {
                None
            }
        })
        .collect()
}

/// Parse a PostScript number array starting at `pos`: find [ ... ] and extract numbers.
fn parse_ps_number_array(data: &[u8], pos: usize) -> Vec<f64> {
    let mut i = pos;
    // Skip to opening bracket
    while i < data.len() && data[i] != b'[' {
        i += 1;
    }
    i += 1; // skip '['

    let mut numbers = Vec::new();
    while i < data.len() && data[i] != b']' {
        // Skip whitespace
        while i < data.len() && data[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= data.len() || data[i] == b']' {
            break;
        }
        // Read number (integer or float, possibly negative)
        let start = i;
        if data[i] == b'-' || data[i] == b'+' {
            i += 1;
        }
        while i < data.len() && (data[i].is_ascii_digit() || data[i] == b'.') {
            i += 1;
        }
        if let Ok(s) = std::str::from_utf8(&data[start..i]) {
            if let Ok(n) = s.parse::<f64>() {
                numbers.push(n);
            }
        }
    }
    numbers
}

/// Parse a single PostScript number after a key: `/Key 123 def`.
fn parse_ps_number(data: &[u8], key: &[u8]) -> Option<f64> {
    let pos = find_bytes(data, key)? + key.len();
    let mut i = pos;
    while i < data.len() && data[i].is_ascii_whitespace() {
        i += 1;
    }
    let start = i;
    if i < data.len() && (data[i] == b'-' || data[i] == b'+') {
        i += 1;
    }
    while i < data.len() && (data[i].is_ascii_digit() || data[i] == b'.') {
        i += 1;
    }
    std::str::from_utf8(&data[start..i]).ok()?.parse().ok()
}

/// Parse the first value from a PostScript array: `/Key [42] def` -> 42.
fn parse_ps_scalar_array_first(data: &[u8], key: &[u8]) -> Option<f64> {
    let pos = find_bytes(data, key)? + key.len();
    let numbers = parse_ps_number_array(data, pos);
    numbers.first().copied()
}

/// Parse /lenIV from decrypted Private dict. Returns None if not found.
fn parse_len_iv(decrypted: &[u8]) -> Option<usize> {
    let pos = find_bytes(decrypted, b"/lenIV")?;
    let mut i = pos + b"/lenIV".len();
    // Skip whitespace
    while i < decrypted.len() && decrypted[i].is_ascii_whitespace() {
        i += 1;
    }
    // Read integer
    let start = i;
    while i < decrypted.len() && decrypted[i].is_ascii_digit() {
        i += 1;
    }
    let s = std::str::from_utf8(&decrypted[start..i]).ok()?;
    s.parse().ok()
}

// ---------------------------------------------------------------------------
// PostScript /CharStrings extraction
// ---------------------------------------------------------------------------

/// Extract /Subrs array from decrypted PostScript data.
///
/// Subroutines are defined as:
///
/// ```text
///   /Subrs N array
///   dup 0 <len> RD <binary_data> NP
///   dup 1 <len> RD <binary_data> NP
///   ...
/// ```
fn extract_subrs(decrypted: &[u8], cleartext: &[u8]) -> Vec<Vec<u8>> {
    // Search in decrypted section first (most common), then cleartext.
    let search_data = if find_bytes(decrypted, b"/Subrs").is_some() {
        decrypted
    } else if find_bytes(cleartext, b"/Subrs").is_some() {
        cleartext
    } else {
        return Vec::new();
    };

    let subrs_start = match find_bytes(search_data, b"/Subrs") {
        Some(pos) => pos,
        None => return Vec::new(),
    };

    let mut pos = subrs_start + b"/Subrs".len();

    // Skip whitespace.
    while pos < search_data.len() && search_data[pos].is_ascii_whitespace() {
        pos += 1;
    }

    // Read array count.
    let count_start = pos;
    while pos < search_data.len() && search_data[pos].is_ascii_digit() {
        pos += 1;
    }
    let count_str = String::from_utf8_lossy(&search_data[count_start..pos]);
    let count: usize = match count_str.parse::<usize>() {
        Ok(n) => n.min(10_000),
        Err(_) => return Vec::new(),
    };

    let mut subrs: Vec<Vec<u8>> = vec![Vec::new(); count];

    // Parse entries: dup <index> <len> RD <binary_data> NP
    const MAX_SEARCH: usize = 500_000;
    let end_pos = (pos + MAX_SEARCH).min(search_data.len());

    while pos < end_pos {
        // Find next "dup"
        match find_bytes(&search_data[pos..end_pos], b"dup") {
            Some(offset) => pos += offset + 3,
            None => break,
        }

        // Skip whitespace.
        while pos < end_pos && search_data[pos].is_ascii_whitespace() {
            pos += 1;
        }

        // Check for /CharStrings (end of Subrs section).
        if search_data[pos..].starts_with(b"/CharStrings") || search_data[pos..].starts_with(b"end")
        {
            break;
        }

        // Read index.
        let idx_start = pos;
        while pos < end_pos && search_data[pos].is_ascii_digit() {
            pos += 1;
        }
        let idx_str = String::from_utf8_lossy(&search_data[idx_start..pos]);
        let idx: usize = match idx_str.parse() {
            Ok(n) => n,
            Err(_) => continue,
        };

        // Skip whitespace.
        while pos < end_pos && search_data[pos].is_ascii_whitespace() {
            pos += 1;
        }

        // Read length.
        let len_start = pos;
        while pos < end_pos && search_data[pos].is_ascii_digit() {
            pos += 1;
        }
        let len_str = String::from_utf8_lossy(&search_data[len_start..pos]);
        let subr_len: usize = match len_str.parse() {
            Ok(n) => n,
            Err(_) => continue,
        };

        // Skip whitespace.
        while pos < end_pos && search_data[pos].is_ascii_whitespace() {
            pos += 1;
        }

        // Skip "RD" or "-|".
        if search_data[pos..].starts_with(b"RD") || search_data[pos..].starts_with(b"-|") {
            pos += 2;
        }

        // Skip exactly one whitespace byte after RD.
        if pos < end_pos
            && (search_data[pos] == b' ' || search_data[pos] == b'\n' || search_data[pos] == b'\r')
        {
            pos += 1;
        }

        // Read subroutine binary data.
        if pos + subr_len > end_pos {
            break;
        }
        let subr_data = search_data[pos..pos + subr_len].to_vec();
        pos += subr_len;

        if idx < count {
            subrs[idx] = subr_data;
        }
    }

    subrs
}

/// Extract charstring entries from decrypted PostScript data.
///
/// Searches for /CharStrings dict and parses entries in the format:
///
/// ```text
///   /<name> <len> RD <binary_bytes> ND
/// ```
///
/// or the shorthand -| and |- equivalents.
fn extract_charstrings(decrypted: &[u8], cleartext: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    // Search in decrypted section first (most common), then cleartext.
    let search_data = if find_bytes(decrypted, b"/CharStrings").is_some() {
        decrypted
    } else if find_bytes(cleartext, b"/CharStrings").is_some() {
        // Some fonts define charstrings in the cleartext section
        cleartext
    } else {
        return Err(Error::new("Type1: no /CharStrings found"));
    };

    let cs_start = find_bytes(search_data, b"/CharStrings")
        .ok_or_else(|| Error::new("Type1: /CharStrings not found"))?;

    let mut pos = cs_start + b"/CharStrings".len();
    let mut charstrings = Vec::new();

    // Skip to the "begin" keyword that starts the dict.
    if let Some(begin_pos) = find_bytes(&search_data[pos..], b"begin") {
        pos += begin_pos + 5; // skip "begin"
    }

    // Parse entries: /<name> <len> RD <bytes> ND
    const MAX_CHARSTRINGS: usize = 10_000;
    while pos < search_data.len() && charstrings.len() < MAX_CHARSTRINGS {
        // Skip whitespace.
        while pos < search_data.len()
            && (search_data[pos] == b' '
                || search_data[pos] == b'\n'
                || search_data[pos] == b'\r'
                || search_data[pos] == b'\t')
        {
            pos += 1;
        }

        if pos >= search_data.len() {
            break;
        }

        // Check for end of CharStrings dict.
        if search_data[pos..].starts_with(b"end") {
            break;
        }

        // Expect /<name>
        if search_data[pos] != b'/' {
            pos += 1;
            continue;
        }
        pos += 1; // skip '/'

        // Read glyph name.
        let name_start = pos;
        while pos < search_data.len()
            && search_data[pos] != b' '
            && search_data[pos] != b'\t'
            && search_data[pos] != b'\n'
            && search_data[pos] != b'\r'
        {
            pos += 1;
        }
        let name = String::from_utf8_lossy(&search_data[name_start..pos]).to_string();

        // Skip whitespace.
        while pos < search_data.len() && search_data[pos].is_ascii_whitespace() {
            pos += 1;
        }

        // Read charstring length (decimal integer).
        let len_start = pos;
        while pos < search_data.len() && search_data[pos].is_ascii_digit() {
            pos += 1;
        }
        let len_str = String::from_utf8_lossy(&search_data[len_start..pos]);
        let cs_len: usize = match len_str.parse() {
            Ok(n) => n,
            Err(_) => {
                // Not a valid entry, skip ahead.
                continue;
            }
        };

        // Skip whitespace.
        while pos < search_data.len() && search_data[pos].is_ascii_whitespace() {
            pos += 1;
        }

        // Skip "RD" or "-|" keyword (both are 2 bytes).
        if search_data[pos..].starts_with(b"RD") || search_data[pos..].starts_with(b"-|") {
            pos += 2;
        }

        // Skip exactly one whitespace byte after RD (the spec says exactly one).
        if pos < search_data.len()
            && (search_data[pos] == b' ' || search_data[pos] == b'\n' || search_data[pos] == b'\r')
        {
            pos += 1;
        }

        // Read charstring binary data.
        if pos + cs_len > search_data.len() {
            break;
        }
        let cs_data = search_data[pos..pos + cs_len].to_vec();
        pos += cs_len;

        if !name.is_empty() && name != ".notdef" {
            charstrings.push((name, cs_data));
        }

        // Skip "ND" or "|-" or "noaccess def" that terminates the entry.
        while pos < search_data.len() && search_data[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if search_data[pos..].starts_with(b"ND")
            || search_data[pos..].starts_with(b"|-")
            || search_data[pos..].starts_with(b"noaccess")
        {
            // Skip the terminator.
            while pos < search_data.len() && search_data[pos] != b'\n' && search_data[pos] != b'\r'
            {
                pos += 1;
            }
        }
    }

    if charstrings.is_empty() {
        return Err(Error::new("Type1: no charstrings extracted"));
    }

    Ok(charstrings)
}

/// Find a byte sequence in data.
fn find_bytes(data: &[u8], needle: &[u8]) -> Option<usize> {
    data.windows(needle.len()).position(|w| w == needle)
}

/// Parse the /Encoding array from Type1 font cleartext.
///
/// The encoding looks like:
/// ```text
/// /Encoding 256 array
/// 0 1 255 {1 index exch /.notdef put} for
/// dup 67 /C put
/// dup 70 /F put
/// ...
/// ```
fn parse_type1_encoding(cleartext: &[u8]) -> HashMap<u8, String> {
    let mut encoding = HashMap::new();

    // Find "/Encoding" in cleartext.
    let text = match std::str::from_utf8(cleartext) {
        Ok(t) => t,
        Err(_) => return encoding,
    };

    let enc_start = match text.find("/Encoding") {
        Some(pos) => pos,
        None => return encoding,
    };

    // Scan for "dup <number> /<name> put" patterns after /Encoding.
    let section = &text[enc_start..];
    // Stop at "readonly def" or "def" which ends the encoding section.
    let end = section
        .find("readonly def")
        .or_else(|| section.find("readonly put"))
        .unwrap_or(section.len().min(8000));
    let section = &section[..end];

    let mut i = 0;
    let bytes = section.as_bytes();
    while i < bytes.len() {
        // Look for "dup" token.
        if i + 3 < bytes.len()
            && &bytes[i..i + 3] == b"dup"
            && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric())
        {
            i += 3;
            // Skip whitespace.
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            // Parse number (character code).
            let num_start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i == num_start {
                continue;
            }
            let code: u8 = match section[num_start..i].parse() {
                Ok(n) => n,
                Err(_) => continue,
            };
            // Skip whitespace.
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            // Parse /name.
            if i < bytes.len() && bytes[i] == b'/' {
                i += 1;
                let name_start = i;
                while i < bytes.len()
                    && bytes[i] != b' '
                    && bytes[i] != b'\n'
                    && bytes[i] != b'\r'
                    && bytes[i] != b'\t'
                {
                    i += 1;
                }
                let name = &section[name_start..i];
                if name != ".notdef" {
                    encoding.insert(code, name.to_string());
                }
            }
        } else {
            i += 1;
        }
    }

    encoding
}

// ---------------------------------------------------------------------------
// Type1 charstring interpreter
// ---------------------------------------------------------------------------

/// Maximum number of operations to prevent infinite loops.
const MAX_OPS: usize = 50_000;

/// Type1 charstring interpreter that produces glyph outlines.
///
/// Type1 charstrings use a subset of the Type2/CFF operators with cubic bezier
/// curves. Subroutines (callsubr/return) are used extensively for shared outline
/// fragments. Explicit closepath operator, hsbw/sbw for width.
struct Type1Interpreter<'a> {
    stack: Vec<f64>,
    contours: Vec<Contour>,
    current_points: Vec<OutlinePoint>,
    x: f64,
    y: f64,
    width: f64,
    ops_count: usize,
    path_open: bool,
    subrs: &'a [Vec<u8>],
    ps_stack: Vec<f64>,
    trace: bool,
    trace_log: Vec<String>,
    /// Horizontal stem hints: (y_position, height) in font units.
    h_stems: Vec<(f64, f64)>,
    /// Vertical stem hints: (x_position, width) in font units.
    v_stems: Vec<(f64, f64)>,
    /// Flex mechanism state. When active, moveto ops just update x,y
    /// without closing contours or adding path points. Reference points
    /// are emitted directly during othersubr 2 calls (FreeType semantics).
    flex_active: bool,
    /// Count of flex reference points seen so far. Ranges 0..=7 during
    /// a flex. Index 0 is the first othersubr 2 call (positions the pen
    /// but emits no point); indices 1..6 emit points (on-curve at 3 and 6);
    /// index 7 is final (no emit) and immediately precedes othersubr 0.
    num_flex_vectors: u32,
    /// seac composite data: (asb, adx, ady, bchar, achar).
    /// Set by the seac operator, consumed by the caller to composite glyphs.
    seac_data: Option<(f64, f64, f64, u8, u8)>,
}

/// Maximum subroutine call depth to prevent infinite recursion.
const MAX_SUBR_DEPTH: u32 = 10;

impl<'a> Type1Interpreter<'a> {
    fn new(subrs: &'a [Vec<u8>]) -> Self {
        Self {
            stack: Vec::with_capacity(24),
            contours: Vec::new(),
            current_points: Vec::new(),
            x: 0.0,
            y: 0.0,
            width: 0.0,
            ops_count: 0,
            path_open: false,
            subrs,
            ps_stack: Vec::new(),
            trace: false,
            trace_log: Vec::new(),
            h_stems: Vec::new(),
            v_stems: Vec::new(),
            flex_active: false,
            num_flex_vectors: 0,
            seac_data: None,
        }
    }

    /// Execute a decrypted charstring, returning None on error.
    fn execute(&mut self, data: &[u8]) -> Option<()> {
        self.execute_inner(data, 0)
    }

    fn execute_inner(&mut self, data: &[u8], depth: u32) -> Option<()> {
        if depth > MAX_SUBR_DEPTH {
            return None;
        }
        let mut i = 0;
        while i < data.len() {
            self.ops_count += 1;
            if self.ops_count > MAX_OPS {
                return None;
            }

            let b = data[i];
            i += 1;

            if self.trace {
                let op_name = match b {
                    1 => "hstem",
                    3 => "vstem",
                    4 => "vmoveto",
                    5 => "rlineto",
                    6 => "hlineto",
                    7 => "vlineto",
                    8 => "rrcurveto",
                    9 => "closepath",
                    10 => "callsubr",
                    11 => "return",
                    12 => "escape",
                    13 => "hsbw",
                    14 => "endchar",
                    21 => "rmoveto",
                    22 => "hmoveto",
                    30 => "vhcurveto",
                    31 => "hvcurveto",
                    32..=255 => "number",
                    _ => "?",
                };
                if !matches!(b, 1 | 3 | 32..=255) {
                    self.trace_log.push(format!(
                        "op {} ({}) at ({:.0},{:.0}) pts={} stack={:?}",
                        b,
                        op_name,
                        self.x,
                        self.y,
                        self.current_points.len(),
                        &self.stack[..self.stack.len().min(4)]
                    ));
                }
            }

            match b {
                // --- Hint operators ---
                1 => {
                    // hstem: pairs of (y, dy) in font units
                    let mut j = 0;
                    while j + 1 < self.stack.len() {
                        self.h_stems.push((self.stack[j], self.stack[j + 1]));
                        j += 2;
                    }
                    self.stack.clear();
                }
                3 => {
                    // vstem: pairs of (x, dx) in font units
                    let mut j = 0;
                    while j + 1 < self.stack.len() {
                        self.v_stems.push((self.stack[j], self.stack[j + 1]));
                        j += 2;
                    }
                    self.stack.clear();
                }

                // --- Path construction ---
                4 => {
                    // vmoveto: dy
                    if self.stack.is_empty() {
                        return None;
                    }
                    let dy = self.stack_pop();
                    self.y += dy;
                    if !self.flex_active {
                        self.close_contour_if_open();
                        self.current_points.push(OutlinePoint {
                            x: self.x,
                            y: self.y,
                            on_curve: true,
                        });
                        self.path_open = true;
                    }
                    self.stack.clear();
                }
                5 => {
                    // rlineto: dx dy
                    if self.stack.len() < 2 {
                        return None;
                    }
                    let dx = self.stack[0];
                    let dy = self.stack[1];
                    self.x += dx;
                    self.y += dy;
                    self.current_points.push(OutlinePoint {
                        x: self.x,
                        y: self.y,
                        on_curve: true,
                    });
                    self.stack.clear();
                }
                6 => {
                    // hlineto: dx
                    if self.stack.is_empty() {
                        return None;
                    }
                    let dx = self.stack_pop();
                    self.x += dx;
                    self.current_points.push(OutlinePoint {
                        x: self.x,
                        y: self.y,
                        on_curve: true,
                    });
                    self.stack.clear();
                }
                7 => {
                    // vlineto: dy
                    if self.stack.is_empty() {
                        return None;
                    }
                    let dy = self.stack_pop();
                    self.y += dy;
                    self.current_points.push(OutlinePoint {
                        x: self.x,
                        y: self.y,
                        on_curve: true,
                    });
                    self.stack.clear();
                }
                8 => {
                    // rrcurveto: dx1 dy1 dx2 dy2 dx3 dy3 (cubic bezier)
                    if self.stack.len() < 6 {
                        return None;
                    }
                    let dx1 = self.stack[0];
                    let dy1 = self.stack[1];
                    let dx2 = self.stack[2];
                    let dy2 = self.stack[3];
                    let dx3 = self.stack[4];
                    let dy3 = self.stack[5];

                    // Control point 1
                    let cp1x = self.x + dx1;
                    let cp1y = self.y + dy1;
                    self.current_points.push(OutlinePoint {
                        x: cp1x,
                        y: cp1y,
                        on_curve: false,
                    });
                    // Control point 2
                    let cp2x = cp1x + dx2;
                    let cp2y = cp1y + dy2;
                    self.current_points.push(OutlinePoint {
                        x: cp2x,
                        y: cp2y,
                        on_curve: false,
                    });
                    // End point
                    self.x = cp2x + dx3;
                    self.y = cp2y + dy3;
                    self.current_points.push(OutlinePoint {
                        x: self.x,
                        y: self.y,
                        on_curve: true,
                    });
                    self.stack.clear();
                }
                9 => {
                    // closepath
                    self.close_contour_if_open();
                    self.stack.clear();
                }
                10 => {
                    // callsubr: call subroutine by index from stack
                    let subr_idx = self.stack_pop() as i32;
                    if subr_idx >= 0 && (subr_idx as usize) < self.subrs.len() {
                        // Copy the shared subrs reference to satisfy the borrow
                        // checker: subrs is &'a [Vec<u8>] (shared, not owned),
                        // so copying the reference is free.
                        let subrs = self.subrs;
                        self.execute_inner(&subrs[subr_idx as usize], depth + 1)?;
                    }
                    // Don't clear stack -- the subr may have pushed values.
                }
                11 => {
                    // return: return from subroutine
                    return Some(());
                }
                13 => {
                    // hsbw: sbx wx (set sidebearing and width)
                    if self.stack.len() < 2 {
                        return None;
                    }
                    let sbx = self.stack[0];
                    let wx = self.stack[1];
                    self.x = sbx;
                    self.y = 0.0;
                    self.width = wx;
                    self.stack.clear();
                }
                14 => {
                    // endchar
                    self.close_contour_if_open();
                    return Some(());
                }
                21 => {
                    // rmoveto: dx dy
                    if self.stack.len() < 2 {
                        return None;
                    }
                    let dx = self.stack[0];
                    let dy = self.stack[1];
                    self.x += dx;
                    self.y += dy;
                    if !self.flex_active {
                        self.close_contour_if_open();
                        self.current_points.push(OutlinePoint {
                            x: self.x,
                            y: self.y,
                            on_curve: true,
                        });
                        self.path_open = true;
                    }
                    self.stack.clear();
                }
                22 => {
                    // hmoveto: dx
                    if self.stack.is_empty() {
                        return None;
                    }
                    let dx = self.stack_pop();
                    self.x += dx;
                    if !self.flex_active {
                        self.close_contour_if_open();
                        self.current_points.push(OutlinePoint {
                            x: self.x,
                            y: self.y,
                            on_curve: true,
                        });
                        self.path_open = true;
                    }
                    self.stack.clear();
                }
                30 => {
                    // vhcurveto: dy1 dx2 dy2 dx3
                    if self.stack.len() < 4 {
                        return None;
                    }
                    let dy1 = self.stack[0];
                    let dx2 = self.stack[1];
                    let dy2 = self.stack[2];
                    let dx3 = self.stack[3];

                    let cp1x = self.x;
                    let cp1y = self.y + dy1;
                    self.current_points.push(OutlinePoint {
                        x: cp1x,
                        y: cp1y,
                        on_curve: false,
                    });
                    let cp2x = cp1x + dx2;
                    let cp2y = cp1y + dy2;
                    self.current_points.push(OutlinePoint {
                        x: cp2x,
                        y: cp2y,
                        on_curve: false,
                    });
                    self.x = cp2x + dx3;
                    self.y = cp2y;
                    self.current_points.push(OutlinePoint {
                        x: self.x,
                        y: self.y,
                        on_curve: true,
                    });
                    self.stack.clear();
                }
                31 => {
                    // hvcurveto: dx1 dx2 dy2 dy3
                    if self.stack.len() < 4 {
                        return None;
                    }
                    let dx1 = self.stack[0];
                    let dx2 = self.stack[1];
                    let dy2 = self.stack[2];
                    let dy3 = self.stack[3];

                    let cp1x = self.x + dx1;
                    let cp1y = self.y;
                    self.current_points.push(OutlinePoint {
                        x: cp1x,
                        y: cp1y,
                        on_curve: false,
                    });
                    let cp2x = cp1x + dx2;
                    let cp2y = cp1y + dy2;
                    self.current_points.push(OutlinePoint {
                        x: cp2x,
                        y: cp2y,
                        on_curve: false,
                    });
                    self.x = cp2x;
                    self.y = cp2y + dy3;
                    self.current_points.push(OutlinePoint {
                        x: self.x,
                        y: self.y,
                        on_curve: true,
                    });
                    self.stack.clear();
                }
                12 => {
                    // Two-byte escape: 12 <op2>
                    if i >= data.len() {
                        return None;
                    }
                    let op2 = data[i];
                    i += 1;
                    match op2 {
                        0 => {
                            // dotsection (hint, ignore)
                            self.stack.clear();
                        }
                        1 => {
                            // vstem3: x1 dx1 x2 dx2 x3 dx3 (three vertical stems)
                            if self.stack.len() >= 6 {
                                for j in (0..6).step_by(2) {
                                    self.v_stems.push((self.stack[j], self.stack[j + 1]));
                                }
                            }
                            self.stack.clear();
                        }
                        2 => {
                            // hstem3: y1 dy1 y2 dy2 y3 dy3 (three horizontal stems)
                            if self.stack.len() >= 6 {
                                for j in (0..6).step_by(2) {
                                    self.h_stems.push((self.stack[j], self.stack[j + 1]));
                                }
                            }
                            self.stack.clear();
                        }
                        6 => {
                            // seac: asb adx ady bchar achar
                            // Standard encoding accented character.
                            // Store parameters for caller to handle composite assembly.
                            if self.stack.len() >= 5 {
                                self.seac_data = Some((
                                    self.stack[0],
                                    self.stack[1],
                                    self.stack[2],
                                    self.stack[3] as u8,
                                    self.stack[4] as u8,
                                ));
                            }
                            self.stack.clear();
                            self.close_contour_if_open();
                            return Some(());
                        }
                        7 => {
                            // sbw: sbx sby wx wy (generalized sidebearing/width)
                            if self.stack.len() < 4 {
                                return None;
                            }
                            let sbx = self.stack[0];
                            let sby = self.stack[1];
                            let wx = self.stack[2];
                            self.x = sbx;
                            self.y = sby;
                            self.width = wx;
                            self.stack.clear();
                        }
                        12 => {
                            // div: num1 num2 -> num1/num2
                            if self.stack.len() < 2 {
                                return None;
                            }
                            let b_val = self.stack.pop().unwrap_or(1.0);
                            let a_val = self.stack.pop().unwrap_or(0.0);
                            if b_val.abs() > f64::EPSILON {
                                self.stack.push(a_val / b_val);
                            } else {
                                self.stack.push(0.0);
                            }
                        }
                        16 => {
                            // callothersubr: stack is [arg1..argN, n_args, subr_num]
                            // Pop subr_num (top), then n_args, then handle
                            // based on othersubr number. Othersubrs 0-2
                            // implement the flex mechanism for curved glyphs.
                            if self.stack.len() < 2 {
                                self.stack.clear();
                            } else {
                                let subr_num = self.stack_pop() as i32;
                                let n_args = self.stack_pop() as usize;

                                match subr_num {
                                    0 => {
                                        // Flex end. By this point all 7 flex
                                        // reference points have been consumed
                                        // (6 emitted as Bezier cp/end pairs,
                                        // the first "just moves" the pen).
                                        // Drain the 3 args (flex_height,
                                        // endpoint_x, endpoint_y) but DO NOT
                                        // re-emit anything: FT's othersubr 0
                                        // only pushes current (x, y) to the
                                        // PS stack so the subsequent pop/pop/
                                        // setcurrentpoint sequence inside CM
                                        // subr 0 works.
                                        let drain_start = self.stack.len().saturating_sub(n_args);
                                        self.stack.drain(drain_start..);

                                        // Push current y then x (PS pop gets
                                        // x first). Matches FT which stores
                                        // top[0] = x, top[1] = y as known
                                        // othersubr results.
                                        self.ps_stack.push(self.y);
                                        self.ps_stack.push(self.x);

                                        self.flex_active = false;
                                        self.num_flex_vectors = 0;
                                    }
                                    1 => {
                                        // Flex start. The preceding draw op
                                        // (lineto/curveto/...) has already
                                        // emitted the flex start as an on-curve
                                        // point and opened the contour, so we
                                        // simply enter flex mode. If no path
                                        // is open yet, the implicit start point
                                        // gets added by the first subsequent
                                        // draw op. Mirrors FT semantics.
                                        self.flex_active = true;
                                        self.num_flex_vectors = 0;
                                    }
                                    2 => {
                                        // Flex add vector. Emits the point
                                        // only for indices 1..=6, marking
                                        // on-curve at 3 (midpoint) and 6
                                        // (endpoint). Index 0 is "move only";
                                        // any index >= 7 is silently dropped
                                        // in case of malformed charstrings.
                                        let idx = self.num_flex_vectors;
                                        self.num_flex_vectors =
                                            self.num_flex_vectors.saturating_add(1);
                                        if (1..=6).contains(&idx) {
                                            let on = idx == 3 || idx == 6;
                                            self.current_points.push(OutlinePoint {
                                                x: self.x,
                                                y: self.y,
                                                on_curve: on,
                                            });
                                        }
                                    }
                                    _ => {
                                        // Other othersubrs (3=hint replacement, etc):
                                        // transfer n_args to PS stack generically.
                                        let drain_start = self.stack.len().saturating_sub(n_args);
                                        let args: Vec<f64> =
                                            self.stack.drain(drain_start..).collect();
                                        for &a in args.iter().rev() {
                                            self.ps_stack.push(a);
                                        }
                                    }
                                }
                            }
                        }
                        17 => {
                            // pop: move a value from PS stack to charstring stack.
                            let val = self.ps_stack.pop().unwrap_or(0.0);
                            self.stack.push(val);
                        }
                        33 => {
                            // setcurrentpoint: x y (absolute moveto)
                            if self.stack.len() < 2 {
                                return None;
                            }
                            self.x = self.stack[0];
                            self.y = self.stack[1];
                            self.stack.clear();
                        }
                        _ => {
                            // Unknown escape operator, skip.
                            self.stack.clear();
                        }
                    }
                }
                // --- Number encoding ---
                32..=246 => {
                    // Single-byte integer: value = b - 139 (range -107..+107)
                    self.stack.push(f64::from(b as i32 - 139));
                }
                247..=250 => {
                    // Two-byte positive: (b - 247) * 256 + next + 108
                    if i >= data.len() {
                        return None;
                    }
                    let b1 = data[i] as i32;
                    i += 1;
                    let val = (b as i32 - 247) * 256 + b1 + 108;
                    self.stack.push(f64::from(val));
                }
                251..=254 => {
                    // Two-byte negative: -(b - 251) * 256 - next - 108
                    if i >= data.len() {
                        return None;
                    }
                    let b1 = data[i] as i32;
                    i += 1;
                    let val = -(b as i32 - 251) * 256 - b1 - 108;
                    self.stack.push(f64::from(val));
                }
                255 => {
                    // Five-byte integer: 4-byte signed big-endian
                    if i + 3 >= data.len() {
                        return None;
                    }
                    let val = i32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
                    i += 4;
                    self.stack.push(f64::from(val));
                }
                _ => {
                    // Unknown operator or reserved byte. Skip.
                }
            }
        }

        // If we reached end without endchar, close and succeed anyway.
        self.close_contour_if_open();
        Some(())
    }

    fn stack_pop(&mut self) -> f64 {
        self.stack.pop().unwrap_or(0.0)
    }

    fn close_contour_if_open(&mut self) {
        if !self.current_points.is_empty() {
            // Match FreeType's t1_builder_close_contour: drop the trailing
            // on-curve point when it coincides with the first point of the
            // contour. Type1 charstrings routinely emit an rlineto that lands
            // back on the moveto start, which a naive implementation would
            // duplicate and every subsequent rasteriser would draw as a
            // zero-length segment.
            if self.current_points.len() > 1 {
                let first = self.current_points[0];
                let last = *self.current_points.last().unwrap();
                if last.on_curve
                    && (first.x - last.x).abs() < f64::EPSILON
                    && (first.y - last.y).abs() < f64::EPSILON
                {
                    self.current_points.pop();
                }
            }
            self.contours.push(Contour {
                points: std::mem::take(&mut self.current_points),
            });
        }
        self.path_open = false;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eexec_decrypt_known_vector() {
        // The standard eexec encryption with seed 55665.
        // Test that decrypt(encrypt(data)) round-trips.
        let plaintext = b"test data here";
        let encrypted = eexec_encrypt(plaintext);
        let decrypted = eexec_decrypt(&encrypted);
        assert_eq!(&decrypted, plaintext);
    }

    /// Encrypt for testing (inverse of eexec_decrypt).
    fn eexec_encrypt(data: &[u8]) -> Vec<u8> {
        let mut r: u16 = 55665;
        let mut result = Vec::with_capacity(data.len() + 4);

        // 4 random prefix bytes.
        let random_prefix = [0xAA, 0xBB, 0xCC, 0xDD];
        for &plain in &random_prefix {
            let cipher = plain ^ (r >> 8) as u8;
            r = (cipher as u16)
                .wrapping_add(r)
                .wrapping_mul(52845)
                .wrapping_add(22719);
            result.push(cipher);
        }
        for &plain in data {
            let cipher = plain ^ (r >> 8) as u8;
            r = (cipher as u16)
                .wrapping_add(r)
                .wrapping_mul(52845)
                .wrapping_add(22719);
            result.push(cipher);
        }
        result
    }

    #[test]
    fn charstring_decrypt_roundtrip() {
        let plaintext = b"\x0d\x00\x6b"; // hsbw 0 107
        let encrypted = charstring_encrypt(plaintext);
        let decrypted = charstring_decrypt(&encrypted);
        assert_eq!(&decrypted, plaintext);
    }

    fn charstring_encrypt(data: &[u8]) -> Vec<u8> {
        let mut r: u16 = 4330;
        let mut result = Vec::with_capacity(data.len() + 4);
        let random_prefix = [0x11, 0x22, 0x33, 0x44];
        for &plain in &random_prefix {
            let cipher = plain ^ (r >> 8) as u8;
            r = (cipher as u16)
                .wrapping_add(r)
                .wrapping_mul(52845)
                .wrapping_add(22719);
            result.push(cipher);
        }
        for &plain in data {
            let cipher = plain ^ (r >> 8) as u8;
            r = (cipher as u16)
                .wrapping_add(r)
                .wrapping_mul(52845)
                .wrapping_add(22719);
            result.push(cipher);
        }
        result
    }

    #[test]
    fn type1_interpreter_hsbw_rmoveto_rlineto() {
        // hsbw 0 500, rmoveto 100 200, rlineto 50 50, endchar
        // push 0: byte 139
        // push 500: byte 255, then i32 500 in big-endian
        // hsbw: byte 13
        // push 100: byte 239 (100 + 139)
        // push 200: byte 255, then i32 200
        // rmoveto: byte 21
        // push 50: byte 189 (50 + 139)
        // push 50: byte 189
        // rlineto: byte 5
        // endchar: byte 14
        let cs = vec![
            139, // push 0
            255, 0, 0, 1, 244, // push 500
            13,  // hsbw
            239, // push 100
            255, 0, 0, 0, 200, // push 200
            21,  // rmoveto
            189, // push 50
            189, // push 50
            5,   // rlineto
            14,  // endchar
        ];

        let mut interp = Type1Interpreter::new(&[]);
        let result = interp.execute(&cs);
        assert!(result.is_some());
        assert_eq!(interp.width, 500.0);
        assert!(!interp.contours.is_empty());
        // The line should end at (100+50, 200+50) = (150, 250)
        let last = interp.contours[0].points.last().unwrap();
        assert_eq!(last.x, 150.0);
        assert_eq!(last.y, 250.0);
        assert!(last.on_curve);
    }

    #[test]
    fn type1_interpreter_rrcurveto() {
        // hsbw 0 600, rmoveto 0 0, rrcurveto 10 20 30 40 50 60, endchar
        let cs = vec![
            139, // push 0
            255, 0, 0, 2, 88,  // push 600
            13,  // hsbw
            139, // push 0
            139, // push 0
            21,  // rmoveto
            149, // push 10
            159, // push 20
            169, // push 30
            179, // push 40
            189, // push 50
            199, // push 60
            8,   // rrcurveto
            14,  // endchar
        ];

        let mut interp = Type1Interpreter::new(&[]);
        let result = interp.execute(&cs);
        assert!(result.is_some());
        assert_eq!(interp.contours.len(), 1);
        // Should have 4 points: moveto + cp1, cp2, endpoint
        assert_eq!(interp.contours[0].points.len(), 4);
        // First point is moveto (on-curve), then two off-curve, then endpoint.
        assert!(interp.contours[0].points[0].on_curve);
        assert!(!interp.contours[0].points[1].on_curve);
        assert!(!interp.contours[0].points[2].on_curve);
        assert!(interp.contours[0].points[3].on_curve);
        // Endpoint: (0+10+30+50, 0+20+40+60) = (90, 120)
        let ep = &interp.contours[0].points[3];
        assert_eq!(ep.x, 90.0);
        assert_eq!(ep.y, 120.0);
    }

    #[test]
    fn pfb_parse_minimal() {
        // Minimal PFB: one ASCII segment, one binary segment, EOF.
        let mut pfb = Vec::new();
        // ASCII segment
        let ascii = b"cleartext header";
        pfb.push(0x80);
        pfb.push(1); // type 1 = ASCII
        pfb.extend_from_slice(&(ascii.len() as u32).to_le_bytes());
        pfb.extend_from_slice(ascii);
        // Binary segment
        let binary = b"encrypted data";
        pfb.push(0x80);
        pfb.push(2); // type 2 = binary
        pfb.extend_from_slice(&(binary.len() as u32).to_le_bytes());
        pfb.extend_from_slice(binary);
        // EOF
        pfb.push(0x80);
        pfb.push(3);

        let (cleartext, encrypted) = parse_pfb(&pfb).unwrap();
        assert_eq!(cleartext, ascii);
        assert_eq!(encrypted, binary);
    }

    #[test]
    fn hex_decode() {
        let input = b"48 65 6C 6C 6F";
        let result = decode_hex(input);
        assert_eq!(result, b"Hello");
    }

    #[test]
    fn number_encoding_single_byte() {
        // Byte 139 = value 0, byte 240 = value 101, byte 32 = value -107
        let cs = vec![
            139, // push 0
            240, // push 101
            139, // push 0
            13,  // hsbw (consumes 2)
            14,  // endchar
        ];
        let mut interp = Type1Interpreter::new(&[]);
        interp.execute(&cs);
        assert_eq!(interp.width, 101.0);
    }

    #[test]
    fn callothersubr_arg_order() {
        // Verify callothersubr pops subr_num first, then n_args.
        // Use othersubr 5 (generic, non-flex) to test the generic path.
        // Stack layout: [arg1, arg2, arg3, n_args=3, subr_num=5]
        //
        // Charstring: hsbw 0 500, push 10 20 30 3 5 callothersubr,
        //             pop pop pop (3x escape 17), endchar
        let cs = vec![
            139, // push 0
            255, 0, 0, 1, 244, // push 500
            13,  // hsbw
            149, // push 10
            159, // push 20
            169, // push 30
            142, // push 3 (n_args)
            144, // push 5 (subr_num)
            12, 16, // callothersubr
            12, 17, // pop -> should get 10
            12, 17, // pop -> should get 20
            12, 17, // pop -> should get 30
            14, // endchar
        ];

        let mut interp = Type1Interpreter::new(&[]);
        let result = interp.execute(&cs);
        assert!(result.is_some());
        // After 3 pops, the charstring stack should have [10, 20, 30].
        assert_eq!(interp.stack, vec![10.0, 20.0, 30.0]);
    }

    #[test]
    fn flex_mechanism_two_curves() {
        // Simulate a flex sequence matching FreeType's semantics (t1decode.c):
        // othersubr 1 (start), 7 rmoveto+othersubr 2 pairs (only idx 1..=6
        // emit points, with on-curve at idx 3 and 6), then othersubr 0 (end).
        // Flex reference vectors (captured by each othersubr 2 idx):
        //   idx 0: (102, 200) -- just moves the pen, not emitted
        //   idx 1: (110, 220) -- cp1 of curve 1
        //   idx 2: (130, 230) -- cp2 of curve 1
        //   idx 3: (150, 200) -- midpoint (on-curve)
        //   idx 4: (170, 170) -- cp1 of curve 2
        //   idx 5: (190, 180) -- cp2 of curve 2
        //   idx 6: (200, 200) -- endpoint (on-curve)
        let cs = vec![
            // hsbw 100 500
            239, // push 100
            255, 0, 0, 1, 244, // push 500
            13,  // hsbw -> x=100, y=0
            // rmoveto 0 200 to get to (100, 200)
            139, // push 0
            255, 0, 0, 0, 200, // push 200
            21,  // rmoveto -> (100, 200), starts contour
            // othersubr 1: flex start (0 args, subr 1)
            139, // push 0 (n_args)
            140, // push 1 (subr_num)
            12, 16, // callothersubr -> flex_active=true
            // idx 0: move to (102, 200) -- first vector, does not emit
            141, // push 2
            139, // push 0
            21,  // rmoveto
            139, 141, 12, 16, // othersubr 2 (idx 0, no emit)
            // idx 1: cp1 of curve 1 -> (110, 220)
            147, // push 8
            159, // push 20
            21,  // rmoveto
            139, 141, 12, 16, // othersubr 2 (idx 1, off)
            // idx 2: cp2 of curve 1 -> (130, 230)
            159, // push 20
            149, // push 10
            21,  // rmoveto
            139, 141, 12, 16, // othersubr 2 (idx 2, off)
            // idx 3: midpoint -> (150, 200)
            159, // push 20
            109, // push -30
            21,  // rmoveto
            139, 141, 12, 16, // othersubr 2 (idx 3, ON)
            // idx 4: cp1 of curve 2 -> (170, 170)
            159, // push 20
            109, // push -30
            21,  // rmoveto
            139, 141, 12, 16, // othersubr 2 (idx 4, off)
            // idx 5: cp2 of curve 2 -> (190, 180)
            159, // push 20
            149, // push 10
            21,  // rmoveto
            139, 141, 12, 16, // othersubr 2 (idx 5, off)
            // idx 6: endpoint -> (200, 200)
            149, // push 10
            159, // push 20
            21,  // rmoveto
            139, 141, 12, 16, // othersubr 2 (idx 6, ON)
            // othersubr 0: flex end (3 args: flex_depth, endpoint_x, endpoint_y)
            189, // push 50 (flex_depth)
            255, 0, 0, 0, 200, // push 200 (endpoint_x)
            255, 0, 0, 0, 200, // push 200 (endpoint_y)
            142, // push 3 (n_args)
            139, // push 0 (subr_num)
            12, 16, // callothersubr -> pushes (200,200) to PS stack
            // pop pop setcurrentpoint
            12, 17, // pop -> stack has x
            12, 17, // pop -> stack has [x, y]
            12, 33, // setcurrentpoint
            14, // endchar
        ];

        let mut interp = Type1Interpreter::new(&[]);
        let result = interp.execute(&cs);
        assert!(result.is_some());
        assert!(!interp.flex_active);

        // Should have exactly 1 contour.
        assert_eq!(interp.contours.len(), 1);
        let pts = &interp.contours[0].points;

        // Moveto start + 6 emitted flex points (idx 1..=6) = 7 points.
        assert_eq!(pts.len(), 7, "expected 7 points, got {:?}", pts);

        // First point: moveto (100, 200), on-curve.
        assert!(pts[0].on_curve);
        assert_eq!(pts[0].x, 100.0);
        assert_eq!(pts[0].y, 200.0);

        // Curve 1: cp1 (110, 220), cp2 (130, 230), midpoint (150, 200).
        assert!(!pts[1].on_curve);
        assert_eq!((pts[1].x, pts[1].y), (110.0, 220.0));
        assert!(!pts[2].on_curve);
        assert_eq!((pts[2].x, pts[2].y), (130.0, 230.0));
        assert!(pts[3].on_curve);
        assert_eq!((pts[3].x, pts[3].y), (150.0, 200.0));

        // Curve 2: cp1 (170, 170), cp2 (190, 180), endpoint (200, 200).
        assert!(!pts[4].on_curve);
        assert_eq!((pts[4].x, pts[4].y), (170.0, 170.0));
        assert!(!pts[5].on_curve);
        assert_eq!((pts[5].x, pts[5].y), (190.0, 180.0));
        assert!(pts[6].on_curve);
        assert_eq!((pts[6].x, pts[6].y), (200.0, 200.0));

        // Current position should be at the endpoint.
        assert_eq!(interp.x, 200.0);
        assert_eq!(interp.y, 200.0);
    }
}
