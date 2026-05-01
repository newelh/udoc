//! CFF (Compact Font Format) outline parser for PDF rendering.
//!
//! Parses embedded CFF font programs (FontFile3 streams) to extract glyph
//! outlines for rasterization. Interprets Type 2 CharString bytecode to
//! produce cubic bezier contours.
//!
//! Handles both simple CFF fonts and CID-keyed CFF fonts (FDArray/FDSelect).
//! Subroutine calls (callsubr, callgsubr) are supported with depth limiting.

use std::collections::HashMap;

use crate::error::{Error, Result};

use super::ttf::{Contour, GlyphOutline, OutlinePoint, StemHints};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Per-FD Private DICT data (local subrs, default/nominal widths, hint values).
/// Simple CFF fonts have one of these; CID-keyed fonts have one per FD.
#[derive(Debug)]
struct FdPrivateData {
    default_width: f64,
    nominal_width: f64,
    local_subrs: Vec<Vec<u8>>,
    // PostScript hint values (blue zones, standard stems).
    blue_values: Vec<(f64, f64)>,
    other_blues: Vec<(f64, f64)>,
    std_hw: f64,
    std_vw: f64,
    blue_scale: f64,
    blue_shift: f64,
    blue_fuzz: f64,
}

/// A parsed CFF font ready for glyph outline extraction.
#[derive(Debug)]
pub struct CffFont {
    /// Number of glyphs in the font.
    num_glyphs: usize,
    /// Per-glyph CharString data (raw bytecode).
    charstrings: Vec<Vec<u8>>,
    /// Global subroutines.
    global_subrs: Vec<Vec<u8>>,
    /// Per-FD Private DICT data. Simple fonts: 1 entry. CID: one per FD.
    fd_data: Vec<FdPrivateData>,
    /// GID -> FD index mapping. None for simple CFF fonts.
    fd_select: Option<Vec<u8>>,
    /// Glyph name -> glyph ID mapping built from charset table.
    name_to_gid: HashMap<String, u16>,
    /// CID -> GID mapping for CID-keyed fonts. Built from the charset table
    /// where entries are CIDs rather than SIDs.
    cid_to_gid: HashMap<u16, u16>,
    /// Whether this is a CID-keyed font.
    is_cid: bool,
}

// ---------------------------------------------------------------------------
// CFF parsing
// ---------------------------------------------------------------------------

impl CffFont {
    /// Parse a CFF font from raw font program bytes.
    ///
    /// The data should be a decompressed FontFile3 stream from a PDF.
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() < 4 {
            return Err(Error::new("CFF data too short"));
        }

        // CFF header: major, minor, hdrSize, offSize
        let hdr_size = data[2] as usize;
        if hdr_size > data.len() {
            return Err(Error::new("CFF header size exceeds data"));
        }

        let mut pos = hdr_size;

        // Name INDEX
        let (_, name_end) = parse_index(data, pos)?;
        pos = name_end;

        // Top DICT INDEX
        let (top_dict_entries, top_dict_end) = parse_index(data, pos)?;
        pos = top_dict_end;

        // String INDEX
        let (string_entries, string_end) = parse_index(data, pos)?;
        pos = string_end;

        // Global Subr INDEX
        let (global_subr_entries, _gsubr_end) = parse_index(data, pos)?;
        let global_subrs: Vec<Vec<u8>> = global_subr_entries;

        // Parse Top DICT to find CharStrings and Private DICT offsets.
        let top_dict_data = top_dict_entries
            .first()
            .ok_or_else(|| Error::new("CFF has no Top DICT"))?;
        let top_dict = parse_dict(top_dict_data);

        // CharStrings INDEX offset (operator 17)
        let charstrings_offset = dict_int(&top_dict, 17)
            .ok_or_else(|| Error::new("CFF Top DICT missing CharStrings offset"))?
            as usize;

        // Parse CharStrings INDEX
        let (charstrings, _) = if charstrings_offset < data.len() {
            parse_index(data, charstrings_offset)?
        } else {
            return Err(Error::new("CFF CharStrings offset out of bounds"));
        };
        let num_glyphs = charstrings.len();

        // Detect CID-keyed font: operator 12 30 (ROS = Registry-Ordering-Supplement).
        let is_cid = dict_int(&top_dict, 1230).is_some();

        let (fd_data, fd_select) = if is_cid {
            // CID-keyed font: parse FDArray and FDSelect.
            let fd_array = parse_fd_array(data, &top_dict)?;
            let fd_sel = parse_fd_select(data, &top_dict, num_glyphs)?;
            (fd_array, Some(fd_sel))
        } else {
            // Simple CFF: single Private DICT.
            let fd = parse_private_dict(data, &top_dict)?;
            (vec![fd], None)
        };

        // Parse charset to build glyph name -> glyph ID mapping.
        let charset_offset = dict_int(&top_dict, 15).unwrap_or(0.0) as usize;
        let name_to_gid = parse_charset(data, charset_offset, num_glyphs, &string_entries);

        // For CID fonts, build a CID->GID mapping from the charset.
        // In CID fonts, charset entries are CIDs (not SIDs).
        let cid_to_gid = if is_cid {
            parse_cid_charset(data, charset_offset, num_glyphs)
        } else {
            HashMap::new()
        };

        Ok(CffFont {
            num_glyphs,
            charstrings,
            global_subrs,
            fd_data,
            fd_select,
            name_to_gid,
            cid_to_gid,
            is_cid,
        })
    }

    /// Number of glyphs in the font.
    #[allow(dead_code)]
    pub fn num_glyphs(&self) -> usize {
        self.num_glyphs
    }

    /// Get PS hint values (blue zones, standard stems) from the first FD.
    /// Used by the PS hint interpreter for grid-fitting.
    pub fn ps_hint_values(&self) -> Option<super::type1::Type1HintValues> {
        let fd = self.fd_data.first()?;
        if fd.blue_values.is_empty() && fd.std_hw == 0.0 && fd.std_vw == 0.0 {
            return None;
        }
        Some(super::type1::Type1HintValues {
            blue_values: fd.blue_values.clone(),
            other_blues: fd.other_blues.clone(),
            std_hw: fd.std_hw,
            std_vw: fd.std_vw,
            blue_scale: fd.blue_scale,
            blue_shift: fd.blue_shift,
            blue_fuzz: fd.blue_fuzz,
        })
    }

    /// Look up a glyph ID by PostScript glyph name.
    /// Falls back to stripping the stylistic suffix (e.g. ".alt", ".sc")
    /// for subset fonts that use alternate glyph names.
    pub fn glyph_id_for_name(&self, name: &str) -> Option<u16> {
        if let Some(&gid) = self.name_to_gid.get(name) {
            return Some(gid);
        }
        // Fallback: strip suffix after last '.' and retry with base name.
        if let Some(dot_pos) = name.rfind('.') {
            let base = &name[..dot_pos];
            if !base.is_empty() {
                return self.name_to_gid.get(base).copied();
            }
        }
        None
    }

    /// Look up a glyph ID for a Unicode character.
    ///
    /// Maps the character to its standard PostScript glyph name, then looks
    /// up the name in the charset. Falls back to trying the Unicode codepoint
    /// as a raw glyph ID for fonts without a charset.
    pub fn glyph_id_for_char(&self, ch: char) -> Option<u16> {
        let name = unicode_to_glyph_name(ch)?;
        if let Some(&gid) = self.name_to_gid.get(name) {
            return Some(gid);
        }
        // Fallback: raw codepoint as GID (for fonts without proper charset).
        let gid = ch as u16;
        if (gid as usize) < self.num_glyphs {
            Some(gid)
        } else {
            None
        }
    }

    /// Get the advance width of a glyph in font units.
    ///
    /// Interprets the charstring to extract the width operand. Returns the
    /// width from the charstring (width + nominal_width) or the default_width
    /// if no explicit width is specified.
    pub fn advance_width(&self, glyph_id: u16) -> Option<u16> {
        let gid = glyph_id as usize;
        if gid >= self.charstrings.len() {
            return None;
        }
        let cs_data = &self.charstrings[gid];
        if cs_data.is_empty() {
            return None;
        }

        let fd_idx = self
            .fd_select
            .as_ref()
            .and_then(|s| s.get(gid).copied())
            .unwrap_or(0) as usize;
        let fd = &self.fd_data[fd_idx.min(self.fd_data.len().saturating_sub(1))];

        let mut interp = CharStringInterpreter::new(
            &self.global_subrs,
            &fd.local_subrs,
            fd.default_width,
            fd.nominal_width,
        );

        // Execute just enough to extract the width (first operand before first path op).
        let _ = interp.execute(cs_data);
        Some(interp.advance_width.round().max(0.0) as u16)
    }

    /// Get advance width by Unicode character.
    pub fn advance_width_for_char(&self, ch: char) -> Option<u16> {
        let gid = self.glyph_id_for_char(ch)?;
        self.advance_width(gid)
    }

    /// Extract the glyph outline by CID (for CID fonts) or GID (for simple fonts).
    /// For CID-keyed fonts, maps CID to internal GID via the charset table.
    /// For simple CFF fonts, the value is used as GID directly.
    pub fn glyph_outline_by_gid(&self, cid: u16) -> Option<GlyphOutline> {
        if self.is_cid {
            // CID-keyed: map CID to internal GID via charset.
            let gid = self.cid_to_gid.get(&cid).copied().unwrap_or(cid);
            self.glyph_outline(gid)
        } else {
            // Simple CFF: GID used directly.
            self.glyph_outline(cid)
        }
    }

    /// Extract glyph outline by internal GID, bypassing CID-to-GID mapping.
    /// Used for standalone CFF fonts with an external cmap (e.g., CJK fallback).
    pub fn glyph_outline_by_internal_gid(&self, gid: u16) -> Option<GlyphOutline> {
        self.glyph_outline(gid)
    }

    /// Extract the glyph outline for a glyph ID.
    ///
    /// Interprets the Type 2 CharString bytecode to produce cubic bezier
    /// contours. Returns None for empty glyphs or on interpretation failure.
    pub fn glyph_outline(&self, glyph_id: u16) -> Option<GlyphOutline> {
        let gid = glyph_id as usize;
        if gid >= self.charstrings.len() {
            return None;
        }

        let cs_data = &self.charstrings[gid];
        if cs_data.is_empty() {
            return None;
        }

        // Pick the right FD for this glyph.
        let fd_idx = self
            .fd_select
            .as_ref()
            .and_then(|s| s.get(gid).copied())
            .unwrap_or(0) as usize;
        let fd = &self.fd_data[fd_idx.min(self.fd_data.len().saturating_sub(1))];

        let mut interp = CharStringInterpreter::new(
            &self.global_subrs,
            &fd.local_subrs,
            fd.default_width,
            fd.nominal_width,
        );

        interp.execute(cs_data)?;

        if interp.contours.is_empty() {
            return None;
        }

        // Compute bounding box from all points.
        let mut x_min = f64::MAX;
        let mut y_min = f64::MAX;
        let mut x_max = f64::MIN;
        let mut y_max = f64::MIN;
        for contour in &interp.contours {
            for pt in &contour.points {
                x_min = x_min.min(pt.x);
                y_min = y_min.min(pt.y);
                x_max = x_max.max(pt.x);
                y_max = y_max.max(pt.y);
            }
        }

        Some(GlyphOutline {
            contours: interp.contours,
            bounds: (x_min as i16, y_min as i16, x_max as i16, y_max as i16),
            stem_hints: StemHints {
                h_stems: interp.h_stems,
                v_stems: interp.v_stems,
            },
        })
    }

    /// Extract glyph outline by PostScript glyph name.
    /// Uses the charset name_to_gid mapping to find the glyph ID,
    /// with suffix-stripping fallback for stylistic alternates.
    pub fn glyph_outline_by_name(&self, name: &str) -> Option<GlyphOutline> {
        let gid = self.glyph_id_for_name(name)?;
        self.glyph_outline(gid)
    }
}

// ---------------------------------------------------------------------------
// INDEX parsing
// ---------------------------------------------------------------------------

/// Parse a CFF INDEX structure. Returns (entries, end_position).
fn parse_index(data: &[u8], pos: usize) -> Result<(Vec<Vec<u8>>, usize)> {
    if pos + 2 > data.len() {
        return Err(Error::new("CFF INDEX: truncated count"));
    }
    let count = u16_be(data, pos) as usize;
    if count == 0 {
        return Ok((Vec::new(), pos + 2));
    }

    if pos + 3 > data.len() {
        return Err(Error::new("CFF INDEX: truncated offSize"));
    }
    let off_size = data[pos + 2] as usize;
    if off_size == 0 || off_size > 4 {
        return Err(Error::new(format!("CFF INDEX: invalid offSize {off_size}")));
    }

    let offsets_start = pos + 3;
    let num_offsets = count + 1;
    let offsets_end = offsets_start + num_offsets * off_size;
    if offsets_end > data.len() {
        return Err(Error::new("CFF INDEX: truncated offset array"));
    }

    // Read offsets (1-based per CFF spec).
    let mut offsets = Vec::with_capacity(num_offsets);
    for i in 0..num_offsets {
        let o = read_offset(data, offsets_start + i * off_size, off_size);
        offsets.push(o);
    }

    // Data starts at offsets_end, but offsets are 1-based relative to this base.
    let data_base = offsets_end - 1; // -1 because offsets are 1-based
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let start = data_base + offsets[i];
        let end = data_base + offsets[i + 1];
        if start > data.len() || end > data.len() || start > end {
            entries.push(Vec::new());
        } else {
            entries.push(data[start..end].to_vec());
        }
    }

    let total_end = data_base + offsets[count];
    Ok((entries, total_end.min(data.len())))
}

/// Read an offset of given size from data.
fn read_offset(data: &[u8], pos: usize, size: usize) -> usize {
    let mut val: usize = 0;
    for i in 0..size {
        val = (val << 8) | data[pos + i] as usize;
    }
    val
}

// ---------------------------------------------------------------------------
// DICT parsing
// ---------------------------------------------------------------------------

/// A parsed CFF DICT: list of (operator, operands) pairs.
type Dict = Vec<(u16, Vec<f64>)>;

/// Parse a CFF DICT from raw bytes.
fn parse_dict(data: &[u8]) -> Dict {
    let mut result = Vec::new();
    let mut operands: Vec<f64> = Vec::new();
    let mut i = 0;

    while i < data.len() {
        let b = data[i];
        match b {
            // Operators
            0..=21 => {
                let op = if b == 12 {
                    i += 1;
                    if i >= data.len() {
                        break;
                    }
                    1200 + data[i] as u16
                } else {
                    b as u16
                };
                result.push((op, operands.clone()));
                operands.clear();
                i += 1;
            }
            // Integer operands
            28 => {
                if i + 2 < data.len() {
                    let val = i16_be(data, i + 1) as f64;
                    operands.push(val);
                }
                i += 3;
            }
            29 => {
                if i + 4 < data.len() {
                    let val = i32_be(data, i + 1) as f64;
                    operands.push(val);
                }
                i += 5;
            }
            30 => {
                // Real number (BCD encoded). Parse nibbles.
                let (val, consumed) = parse_real_number(data, i + 1);
                operands.push(val);
                i += 1 + consumed;
            }
            32..=246 => {
                operands.push((b as i32 - 139) as f64);
                i += 1;
            }
            247..=250 => {
                if i + 1 < data.len() {
                    let val = ((b as i32 - 247) * 256 + data[i + 1] as i32 + 108) as f64;
                    operands.push(val);
                }
                i += 2;
            }
            251..=254 => {
                if i + 1 < data.len() {
                    let val = (-(b as i32 - 251) * 256 - data[i + 1] as i32 - 108) as f64;
                    operands.push(val);
                }
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }

    result
}

/// Parse a BCD-encoded real number. Returns (value, bytes_consumed).
fn parse_real_number(data: &[u8], start: usize) -> (f64, usize) {
    let mut s = String::new();
    let mut i = start;
    loop {
        if i >= data.len() {
            break;
        }
        let byte = data[i];
        i += 1;
        let mut done = false;
        for nibble in [byte >> 4, byte & 0x0F] {
            match nibble {
                0..=9 => s.push((b'0' + nibble) as char),
                0xA => s.push('.'),
                0xB => s.push('E'),
                0xC => s.push_str("E-"),
                0xE => s.push('-'),
                0xF => {
                    done = true;
                    break;
                }
                _ => {}
            }
        }
        if done {
            break;
        }
    }
    let val = s.parse::<f64>().unwrap_or(0.0);
    (val, i - start)
}

/// Get an integer value for a DICT operator.
fn dict_int(dict: &Dict, op: u16) -> Option<f64> {
    dict.iter()
        .find(|(o, _)| *o == op)
        .and_then(|(_, ops)| ops.first().copied())
}

/// Extract DICT operator operands as pairs: [a b c d] -> [(a,b), (c,d)].
fn dict_array_pairs(dict: &Dict, op: u16) -> Vec<(f64, f64)> {
    dict.iter()
        .find(|(o, _)| *o == op)
        .map(|(_, ops)| {
            ops.chunks(2)
                .filter_map(|c| {
                    if c.len() == 2 {
                        Some((c[0], c[1]))
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Get two integer values for a DICT operator (e.g., Private DICT [size, offset]).
fn dict_pair(dict: &Dict, op: u16) -> Option<(f64, f64)> {
    dict.iter().find(|(o, _)| *o == op).and_then(|(_, ops)| {
        if ops.len() >= 2 {
            Some((ops[0], ops[1]))
        } else {
            None
        }
    })
}

/// Parse the Private DICT to extract widths, local subrs, and hint values.
fn parse_private_dict(data: &[u8], top_dict: &Dict) -> Result<FdPrivateData> {
    let (priv_size, priv_offset) = match dict_pair(top_dict, 18) {
        Some(p) => (p.0 as usize, p.1 as usize),
        None => {
            return Ok(FdPrivateData {
                default_width: 0.0,
                nominal_width: 0.0,
                local_subrs: Vec::new(),
                blue_values: Vec::new(),
                other_blues: Vec::new(),
                std_hw: 0.0,
                std_vw: 0.0,
                blue_scale: 0.039625,
                blue_shift: 7.0,
                blue_fuzz: 1.0,
            });
        }
    };

    if priv_offset + priv_size > data.len() {
        return Ok(FdPrivateData {
            default_width: 0.0,
            nominal_width: 0.0,
            local_subrs: Vec::new(),
            blue_values: Vec::new(),
            other_blues: Vec::new(),
            std_hw: 0.0,
            std_vw: 0.0,
            blue_scale: 0.039625,
            blue_shift: 7.0,
            blue_fuzz: 1.0,
        });
    }

    let priv_data = &data[priv_offset..priv_offset + priv_size];
    let priv_dict = parse_dict(priv_data);

    let default_width = dict_int(&priv_dict, 20).unwrap_or(0.0);
    let nominal_width = dict_int(&priv_dict, 21).unwrap_or(0.0);

    // Local subrs: operator 19 gives offset relative to start of Private DICT.
    let local_subrs = if let Some(subr_offset) = dict_int(&priv_dict, 19) {
        let abs_offset = priv_offset + subr_offset as usize;
        if abs_offset < data.len() {
            parse_index(data, abs_offset)
                .map(|(entries, _)| entries)
                .unwrap_or_default()
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    // Blue zones and standard stems (CFF DICT operators).
    // Op 6: BlueValues, Op 7: OtherBlues, Op 10: StdHW, Op 11: StdVW
    // Op 1209 (12 9): BlueScale, Op 1210 (12 10): BlueShift, Op 1211 (12 11): BlueFuzz
    let blue_values = dict_array_pairs(&priv_dict, 6);
    let other_blues = dict_array_pairs(&priv_dict, 7);
    let std_hw = dict_int(&priv_dict, 10).unwrap_or(0.0);
    let std_vw = dict_int(&priv_dict, 11).unwrap_or(0.0);
    let blue_scale = dict_int(&priv_dict, 1209).unwrap_or(0.039625);
    let blue_shift = dict_int(&priv_dict, 1210).unwrap_or(7.0);
    let blue_fuzz = dict_int(&priv_dict, 1211).unwrap_or(1.0);

    Ok(FdPrivateData {
        default_width,
        nominal_width,
        local_subrs,
        blue_values,
        other_blues,
        std_hw,
        std_vw,
        blue_scale,
        blue_shift,
        blue_fuzz,
    })
}

/// Parse the FDArray for CID-keyed CFF fonts.
///
/// The FDArray is an INDEX of Font DICTs. Each Font DICT has its own
/// Private DICT with local subrs, default width, and nominal width.
/// Operator 12 36 in the Top DICT gives the FDArray offset.
fn parse_fd_array(data: &[u8], top_dict: &Dict) -> Result<Vec<FdPrivateData>> {
    let fd_array_offset = dict_int(top_dict, 1236)
        .ok_or_else(|| Error::new("CID CFF missing FDArray offset (operator 12 36)"))?
        as usize;

    if fd_array_offset >= data.len() {
        return Err(Error::new("CFF FDArray offset out of bounds"));
    }

    let (fd_entries, _) = parse_index(data, fd_array_offset)?;
    let mut fd_data = Vec::with_capacity(fd_entries.len());

    for fd_entry in &fd_entries {
        let fd_dict = parse_dict(fd_entry);
        // Each FD has its own Private DICT (operator 18: [size, offset]).
        let fd = parse_private_dict_from(data, &fd_dict)?;
        fd_data.push(fd);
    }

    if fd_data.is_empty() {
        // Fallback: empty FD so glyph_outline doesn't panic on empty fd_data.
        fd_data.push(FdPrivateData {
            default_width: 0.0,
            nominal_width: 0.0,
            local_subrs: Vec::new(),
            blue_values: Vec::new(),
            other_blues: Vec::new(),
            std_hw: 0.0,
            std_vw: 0.0,
            blue_scale: 0.039625,
            blue_shift: 7.0,
            blue_fuzz: 1.0,
        });
    }

    Ok(fd_data)
}

/// Parse Private DICT from a Font DICT (shared logic for both Top DICT and FD entries).
fn parse_private_dict_from(data: &[u8], dict: &Dict) -> Result<FdPrivateData> {
    parse_private_dict(data, dict)
}

/// Parse the FDSelect table for CID-keyed CFF fonts.
///
/// Maps each GID to its FD index. Supports format 0 (byte array) and
/// format 3 (range-based).
/// Operator 12 37 in the Top DICT gives the FDSelect offset.
fn parse_fd_select(data: &[u8], top_dict: &Dict, num_glyphs: usize) -> Result<Vec<u8>> {
    let fd_select_offset = dict_int(top_dict, 1237)
        .ok_or_else(|| Error::new("CID CFF missing FDSelect offset (operator 12 37)"))?
        as usize;

    if fd_select_offset >= data.len() {
        return Err(Error::new("CFF FDSelect offset out of bounds"));
    }

    let format = data[fd_select_offset];
    match format {
        0 => {
            // Format 0: one byte per glyph.
            let start = fd_select_offset + 1;
            let end = start + num_glyphs;
            if end > data.len() {
                return Err(Error::new("CFF FDSelect format 0 truncated"));
            }
            Ok(data[start..end].to_vec())
        }
        3 => {
            // Format 3: range-based.
            let pos = fd_select_offset + 1;
            if pos + 2 > data.len() {
                return Err(Error::new("CFF FDSelect format 3 truncated"));
            }
            let n_ranges = u16_be(data, pos) as usize;
            let ranges_start = pos + 2;
            // Each range: first_gid (u16) + fd (u8) = 3 bytes, plus sentinel u16.
            if ranges_start + n_ranges * 3 + 2 > data.len() {
                return Err(Error::new("CFF FDSelect format 3 ranges truncated"));
            }

            let mut result = vec![0u8; num_glyphs];
            for i in 0..n_ranges {
                let range_offset = ranges_start + i * 3;
                let first_gid = u16_be(data, range_offset) as usize;
                let fd = data[range_offset + 2];
                let next_gid = if i + 1 < n_ranges {
                    u16_be(data, range_offset + 3) as usize
                } else {
                    // Sentinel: last entry in the range array.
                    u16_be(data, ranges_start + n_ranges * 3) as usize
                };
                for slot in result
                    .iter_mut()
                    .take(next_gid.min(num_glyphs))
                    .skip(first_gid)
                {
                    *slot = fd;
                }
            }
            Ok(result)
        }
        _ => {
            // Unknown format: default all to FD 0.
            Ok(vec![0u8; num_glyphs])
        }
    }
}

// ---------------------------------------------------------------------------
// Type 2 CharString interpreter
// ---------------------------------------------------------------------------

/// Maximum call stack depth for subroutine calls.
const MAX_SUBR_DEPTH: u32 = 10;
/// Maximum number of operations to prevent infinite loops.
const MAX_OPS: usize = 100_000;

/// Type 2 CharString interpreter that produces glyph outlines.
struct CharStringInterpreter<'a> {
    stack: Vec<f64>,
    contours: Vec<Contour>,
    current_points: Vec<OutlinePoint>,
    x: f64,
    y: f64,
    has_width: bool,
    /// Advance width in font units. Set from the optional width operand
    /// in the charstring (first operand before first path op). If absent,
    /// defaults to default_width.
    advance_width: f64,
    global_subrs: &'a [Vec<u8>],
    local_subrs: &'a [Vec<u8>],
    #[allow(dead_code)]
    default_width: f64,
    nominal_width: f64,
    ops_count: usize,
    /// Total stem hints declared so far (for hintmask byte count).
    total_stems: usize,
    /// Horizontal stem hints: (y_position, height) in font units.
    h_stems: Vec<(f64, f64)>,
    /// Vertical stem hints: (x_position, width) in font units.
    v_stems: Vec<(f64, f64)>,
}

impl<'a> CharStringInterpreter<'a> {
    fn new(
        global_subrs: &'a [Vec<u8>],
        local_subrs: &'a [Vec<u8>],
        default_width: f64,
        nominal_width: f64,
    ) -> Self {
        Self {
            stack: Vec::with_capacity(48),
            contours: Vec::new(),
            current_points: Vec::new(),
            x: 0.0,
            y: 0.0,
            has_width: false,
            advance_width: default_width,
            global_subrs,
            local_subrs,
            default_width,
            nominal_width,
            ops_count: 0,
            total_stems: 0,
            h_stems: Vec::new(),
            v_stems: Vec::new(),
        }
    }

    /// Execute a CharString, returning None on error.
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

            match b {
                // Operators
                1 | 18 => {
                    // hstem, hstemhm: pairs of (dy, ddy) as cumulative deltas
                    self.maybe_consume_width();
                    let mut j = 0;
                    let mut y_acc = 0.0;
                    while j + 1 < self.stack.len() {
                        y_acc += self.stack[j];
                        let dy = self.stack[j + 1];
                        self.h_stems.push((y_acc, dy));
                        y_acc += dy;
                        j += 2;
                    }
                    self.total_stems += self.stack.len() / 2;
                    self.stack.clear();
                }
                3 | 23 => {
                    // vstem, vstemhm: pairs of (dx, ddx) as cumulative deltas
                    self.maybe_consume_width();
                    let mut j = 0;
                    let mut x_acc = 0.0;
                    while j + 1 < self.stack.len() {
                        x_acc += self.stack[j];
                        let dx = self.stack[j + 1];
                        self.v_stems.push((x_acc, dx));
                        x_acc += dx;
                        j += 2;
                    }
                    self.total_stems += self.stack.len() / 2;
                    self.stack.clear();
                }
                4 => {
                    // vmoveto
                    self.maybe_consume_width_one_extra(1);
                    if self.stack.is_empty() {
                        return None;
                    }
                    self.close_contour();
                    let dy = self.stack.remove(0);
                    self.y += dy;
                    self.current_points.push(OutlinePoint {
                        x: self.x,
                        y: self.y,
                        on_curve: true,
                    });
                    self.stack.clear();
                }
                5 => {
                    // rlineto
                    let mut j = 0;
                    while j + 1 < self.stack.len() {
                        self.x += self.stack[j];
                        self.y += self.stack[j + 1];
                        self.current_points.push(OutlinePoint {
                            x: self.x,
                            y: self.y,
                            on_curve: true,
                        });
                        j += 2;
                    }
                    self.stack.clear();
                }
                6 => {
                    // hlineto: alternating horizontal/vertical lines
                    let mut horiz = true;
                    for &d in &self.stack.clone() {
                        if horiz {
                            self.x += d;
                        } else {
                            self.y += d;
                        }
                        self.current_points.push(OutlinePoint {
                            x: self.x,
                            y: self.y,
                            on_curve: true,
                        });
                        horiz = !horiz;
                    }
                    self.stack.clear();
                }
                7 => {
                    // vlineto: alternating vertical/horizontal lines
                    let mut vert = true;
                    for &d in &self.stack.clone() {
                        if vert {
                            self.y += d;
                        } else {
                            self.x += d;
                        }
                        self.current_points.push(OutlinePoint {
                            x: self.x,
                            y: self.y,
                            on_curve: true,
                        });
                        vert = !vert;
                    }
                    self.stack.clear();
                }
                8 => {
                    // rrcurveto: relative cubic bezier curves
                    let mut j = 0;
                    while j + 5 < self.stack.len() {
                        let dx1 = self.stack[j];
                        let dy1 = self.stack[j + 1];
                        let dx2 = self.stack[j + 2];
                        let dy2 = self.stack[j + 3];
                        let dx3 = self.stack[j + 4];
                        let dy3 = self.stack[j + 5];
                        self.cubic_to(dx1, dy1, dx2, dy2, dx3, dy3);
                        j += 6;
                    }
                    self.stack.clear();
                }
                10 => {
                    // callsubr
                    if let Some(idx_raw) = self.stack.pop() {
                        let bias = subr_bias(self.local_subrs.len());
                        let idx = (idx_raw as i32 + bias as i32) as usize;
                        if idx < self.local_subrs.len() {
                            let subr_data = self.local_subrs[idx].clone();
                            self.execute_inner(&subr_data, depth + 1)?;
                        }
                    }
                }
                11 => {
                    // return
                    return Some(());
                }
                12 => {
                    // Two-byte operators
                    if i >= data.len() {
                        return None;
                    }
                    let op2 = data[i];
                    i += 1;
                    match op2 {
                        34 => {
                            // hflex
                            if self.stack.len() >= 7 {
                                let s = self.stack.clone();
                                self.cubic_to(s[0], 0.0, s[1], s[2], s[3], 0.0);
                                self.cubic_to(s[4], 0.0, s[5], 0.0, s[6], 0.0);
                            }
                            self.stack.clear();
                        }
                        35 => {
                            // flex
                            if self.stack.len() >= 13 {
                                let s = self.stack.clone();
                                self.cubic_to(s[0], s[1], s[2], s[3], s[4], s[5]);
                                self.cubic_to(s[6], s[7], s[8], s[9], s[10], s[11]);
                            }
                            self.stack.clear();
                        }
                        36 => {
                            // hflex1
                            if self.stack.len() >= 9 {
                                let s = self.stack.clone();
                                self.cubic_to(s[0], s[1], s[2], s[3], s[4], 0.0);
                                self.cubic_to(s[5], 0.0, s[6], s[7], s[8], 0.0);
                            }
                            self.stack.clear();
                        }
                        37 => {
                            // flex1
                            if self.stack.len() >= 11 {
                                let s = self.stack.clone();
                                self.cubic_to(s[0], s[1], s[2], s[3], s[4], s[5]);
                                // Last argument is dx or dy depending on direction
                                let dx = s[0] + s[2] + s[4] + s[6] + s[8];
                                let dy = s[1] + s[3] + s[5] + s[7] + s[9];
                                if dx.abs() > dy.abs() {
                                    self.cubic_to(s[6], s[7], s[8], s[9], s[10], -(dy));
                                } else {
                                    self.cubic_to(s[6], s[7], s[8], s[9], -(dx), s[10]);
                                }
                            }
                            self.stack.clear();
                        }
                        _ => {
                            // Unknown extended operator: clear stack
                            self.stack.clear();
                        }
                    }
                }
                14 => {
                    // endchar
                    self.maybe_consume_width();
                    self.close_contour();
                    return Some(());
                }
                19 | 20 => {
                    // hintmask, cntrmask: any remaining stack values are
                    // implicit vstem hints, then skip the mask bytes.
                    self.maybe_consume_width();
                    let mut j = 0;
                    let mut x_acc = self.v_stems.last().map_or(0.0, |&(p, w)| p + w);
                    while j + 1 < self.stack.len() {
                        x_acc += self.stack[j];
                        let dx = self.stack[j + 1];
                        self.v_stems.push((x_acc, dx));
                        x_acc += dx;
                        j += 2;
                    }
                    self.total_stems += self.stack.len() / 2;
                    self.stack.clear();
                    let mask_bytes = self.total_stems.div_ceil(8).max(1);
                    i += mask_bytes;
                }
                21 => {
                    // rmoveto
                    self.maybe_consume_width_one_extra(2);
                    if self.stack.len() < 2 {
                        self.stack.clear();
                        return None;
                    }
                    self.close_contour();
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
                22 => {
                    // hmoveto
                    self.maybe_consume_width_one_extra(1);
                    if self.stack.is_empty() {
                        return None;
                    }
                    self.close_contour();
                    let dx = self.stack.remove(0);
                    self.x += dx;
                    self.current_points.push(OutlinePoint {
                        x: self.x,
                        y: self.y,
                        on_curve: true,
                    });
                    self.stack.clear();
                }
                24 => {
                    // rcurveline: curves then line
                    let n = self.stack.len();
                    if n >= 8 {
                        let mut j = 0;
                        while j + 7 < n {
                            self.cubic_to(
                                self.stack[j],
                                self.stack[j + 1],
                                self.stack[j + 2],
                                self.stack[j + 3],
                                self.stack[j + 4],
                                self.stack[j + 5],
                            );
                            j += 6;
                        }
                        if j + 1 < n {
                            self.x += self.stack[j];
                            self.y += self.stack[j + 1];
                            self.current_points.push(OutlinePoint {
                                x: self.x,
                                y: self.y,
                                on_curve: true,
                            });
                        }
                    }
                    self.stack.clear();
                }
                25 => {
                    // rlinecurve: lines then curve
                    let n = self.stack.len();
                    if n >= 8 {
                        let mut j = 0;
                        let line_end = n - 6;
                        while j + 1 < line_end {
                            self.x += self.stack[j];
                            self.y += self.stack[j + 1];
                            self.current_points.push(OutlinePoint {
                                x: self.x,
                                y: self.y,
                                on_curve: true,
                            });
                            j += 2;
                        }
                        self.cubic_to(
                            self.stack[j],
                            self.stack[j + 1],
                            self.stack[j + 2],
                            self.stack[j + 3],
                            self.stack[j + 4],
                            self.stack[j + 5],
                        );
                    }
                    self.stack.clear();
                }
                26 => {
                    // vvcurveto
                    let s = self.stack.clone();
                    let mut j = 0;
                    if !s.len().is_multiple_of(4) {
                        // Extra dx1 at start
                        if !s.is_empty() {
                            let dx1 = s[0];
                            j = 1;
                            if j + 3 < s.len() {
                                self.cubic_to(dx1, s[j], s[j + 1], s[j + 2], 0.0, s[j + 3]);
                                j += 4;
                            }
                        }
                    }
                    while j + 3 < s.len() {
                        self.cubic_to(0.0, s[j], s[j + 1], s[j + 2], 0.0, s[j + 3]);
                        j += 4;
                    }
                    self.stack.clear();
                }
                27 => {
                    // hhcurveto
                    let s = self.stack.clone();
                    let mut j = 0;
                    if !s.len().is_multiple_of(4) {
                        // Extra dy1 at start
                        if !s.is_empty() {
                            let dy1 = s[0];
                            j = 1;
                            if j + 3 < s.len() {
                                self.cubic_to(s[j], dy1, s[j + 1], s[j + 2], s[j + 3], 0.0);
                                j += 4;
                            }
                        }
                    }
                    while j + 3 < s.len() {
                        self.cubic_to(s[j], 0.0, s[j + 1], s[j + 2], s[j + 3], 0.0);
                        j += 4;
                    }
                    self.stack.clear();
                }
                29 => {
                    // callgsubr
                    if let Some(idx_raw) = self.stack.pop() {
                        let bias = subr_bias(self.global_subrs.len());
                        let idx = (idx_raw as i32 + bias as i32) as usize;
                        if idx < self.global_subrs.len() {
                            let subr_data = self.global_subrs[idx].clone();
                            self.execute_inner(&subr_data, depth + 1)?;
                        }
                    }
                }
                30 => {
                    // vhcurveto
                    self.vh_curve(true);
                }
                31 => {
                    // hvcurveto
                    self.vh_curve(false);
                }
                // Number operands
                28
                    if i + 1 < data.len() => {
                        let val = i16_be(data, i) as f64;
                        self.stack.push(val);
                        i += 2;
                    }
                32..=246 => {
                    self.stack.push((b as i32 - 139) as f64);
                }
                247..=250
                    if i < data.len() => {
                        let val = ((b as i32 - 247) * 256 + data[i] as i32 + 108) as f64;
                        self.stack.push(val);
                        i += 1;
                    }
                251..=254
                    if i < data.len() => {
                        let val = (-(b as i32 - 251) * 256 - data[i] as i32 - 108) as f64;
                        self.stack.push(val);
                        i += 1;
                    }
                255
                    // 16.16 fixed point
                    if i + 3 < data.len() => {
                        let val = i32_be(data, i) as f64 / 65536.0;
                        self.stack.push(val);
                        i += 4;
                    }
                _ => {
                    // Unknown operator: skip
                }
            }
        }

        Some(())
    }

    /// Handle vhcurveto/hvcurveto.
    fn vh_curve(&mut self, start_vert: bool) {
        let s = self.stack.clone();
        let mut j = 0;
        let mut vert = start_vert;
        while j + 3 < s.len() {
            let remaining = s.len() - j;
            if vert {
                if remaining == 5 {
                    // Last curve: dy1 dx2 dy2 dx3 dy3
                    self.cubic_to(0.0, s[j], s[j + 1], s[j + 2], s[j + 3], s[j + 4]);
                    j += 5;
                } else {
                    self.cubic_to(0.0, s[j], s[j + 1], s[j + 2], s[j + 3], 0.0);
                    j += 4;
                }
            } else if remaining == 5 {
                // Last curve: dx1 dx2 dy2 dy3 dx3
                self.cubic_to(s[j], 0.0, s[j + 1], s[j + 2], s[j + 4], s[j + 3]);
                j += 5;
            } else {
                self.cubic_to(s[j], 0.0, s[j + 1], s[j + 2], 0.0, s[j + 3]);
                j += 4;
            }
            vert = !vert;
        }
        self.stack.clear();
    }

    /// Draw a relative cubic bezier curve.
    fn cubic_to(&mut self, dx1: f64, dy1: f64, dx2: f64, dy2: f64, dx3: f64, dy3: f64) {
        let x1 = self.x + dx1;
        let y1 = self.y + dy1;
        let x2 = x1 + dx2;
        let y2 = y1 + dy2;
        let x3 = x2 + dx3;
        let y3 = y2 + dy3;

        // Cubic bezier: two off-curve control points + on-curve endpoint.
        self.current_points.push(OutlinePoint {
            x: x1,
            y: y1,
            on_curve: false,
        });
        self.current_points.push(OutlinePoint {
            x: x2,
            y: y2,
            on_curve: false,
        });
        self.current_points.push(OutlinePoint {
            x: x3,
            y: y3,
            on_curve: true,
        });

        self.x = x3;
        self.y = y3;
    }

    /// Close the current contour if non-empty.
    fn close_contour(&mut self) {
        if !self.current_points.is_empty() {
            let points = std::mem::take(&mut self.current_points);
            self.contours.push(Contour { points });
        }
    }

    /// Consume an optional width from the stack before the first drawing op.
    fn maybe_consume_width(&mut self) {
        if !self.has_width && !self.stack.is_empty() {
            // If stack has an odd number of values, first is width
            if !self.stack.len().is_multiple_of(2) {
                let w = self.stack.remove(0);
                self.advance_width = w + self.nominal_width;
            }
            self.has_width = true;
        }
    }

    /// Consume width when expecting `n` operands (vmoveto=1, rmoveto=2, hmoveto=1).
    fn maybe_consume_width_one_extra(&mut self, expected: usize) {
        if !self.has_width && self.stack.len() > expected {
            let w = self.stack.remove(0);
            self.advance_width = w + self.nominal_width;
            self.has_width = true;
        } else if !self.has_width {
            self.has_width = true;
        }
    }
}

/// Calculate the subroutine bias per CFF spec.
fn subr_bias(count: usize) -> usize {
    if count < 1240 {
        107
    } else if count < 33900 {
        1131
    } else {
        32768
    }
}

// ---------------------------------------------------------------------------
// Charset parsing
// ---------------------------------------------------------------------------

/// Parse the CFF charset table and build a glyph name -> glyph ID map.
///
/// The charset maps glyph IDs (1..num_glyphs-1) to SIDs (string IDs).
/// GID 0 is always .notdef. SIDs 0-390 are standard CFF strings; SIDs >= 391
/// index into the String INDEX.
/// Parse charset for CID fonts: builds CID -> GID mapping.
/// In CID-keyed CFF, charset entries are CIDs rather than SIDs.
fn parse_cid_charset(data: &[u8], charset_offset: usize, num_glyphs: usize) -> HashMap<u16, u16> {
    let mut map = HashMap::new();
    // GID 0 = CID 0 (.notdef)
    map.insert(0, 0);

    if num_glyphs <= 1 || charset_offset <= 2 || charset_offset >= data.len() {
        return map;
    }

    let format = data[charset_offset];
    let mut pos = charset_offset + 1;
    let glyphs_to_read = num_glyphs - 1;

    match format {
        0 => {
            for gid_offset in 0..glyphs_to_read {
                if pos + 1 >= data.len() {
                    break;
                }
                let cid = u16_be(data, pos);
                pos += 2;
                let gid = (gid_offset + 1) as u16;
                map.insert(cid, gid);
            }
        }
        1 => {
            let mut gid: u16 = 1;
            while (gid as usize) < num_glyphs && pos + 2 < data.len() {
                let first_cid = u16_be(data, pos);
                let n_left = data[pos + 2] as u16;
                pos += 3;
                for delta in 0..=n_left {
                    if (gid as usize) >= num_glyphs {
                        break;
                    }
                    map.insert(first_cid + delta, gid);
                    gid += 1;
                }
            }
        }
        2 => {
            let mut gid: u16 = 1;
            while (gid as usize) < num_glyphs && pos + 3 < data.len() {
                let first_cid = u16_be(data, pos);
                let n_left = u16_be(data, pos + 2);
                pos += 4;
                for delta in 0..=n_left {
                    if (gid as usize) >= num_glyphs {
                        break;
                    }
                    map.insert(first_cid + delta, gid);
                    gid += 1;
                }
            }
        }
        _ => {}
    }

    map
}

fn parse_charset(
    data: &[u8],
    charset_offset: usize,
    num_glyphs: usize,
    string_index: &[Vec<u8>],
) -> HashMap<String, u16> {
    let mut map = HashMap::new();
    map.insert(".notdef".to_string(), 0);

    if num_glyphs <= 1 {
        return map;
    }

    // Predefined charsets (offset 0, 1, 2).
    if charset_offset <= 2 {
        // ISOAdobe (0), Expert (1), ExpertSubset (2). For subset PDF fonts
        // these are rarely used; build map from standard SID order.
        let sids = predefined_charset_sids(charset_offset, num_glyphs);
        for (gid_minus_1, sid) in sids.iter().enumerate() {
            let gid = (gid_minus_1 + 1) as u16;
            if let Some(name) = sid_to_name(*sid, string_index) {
                map.insert(name, gid);
            }
        }
        return map;
    }

    if charset_offset >= data.len() {
        return map;
    }

    let format = data[charset_offset];
    let mut pos = charset_offset + 1;
    let glyphs_to_read = num_glyphs - 1; // GID 0 is .notdef

    match format {
        0 => {
            // Format 0: array of SIDs
            for gid_offset in 0..glyphs_to_read {
                if pos + 1 >= data.len() {
                    break;
                }
                let sid = u16_be(data, pos);
                pos += 2;
                let gid = (gid_offset + 1) as u16;
                if let Some(name) = sid_to_name(sid, string_index) {
                    map.insert(name, gid);
                }
            }
        }
        1 => {
            // Format 1: ranges with 1-byte count
            let mut gid: u16 = 1;
            while (gid as usize) < num_glyphs && pos + 2 < data.len() {
                let first_sid = u16_be(data, pos);
                let n_left = data[pos + 2] as u16;
                pos += 3;
                for delta in 0..=n_left {
                    if (gid as usize) >= num_glyphs {
                        break;
                    }
                    let sid = first_sid + delta;
                    if let Some(name) = sid_to_name(sid, string_index) {
                        map.insert(name, gid);
                    }
                    gid += 1;
                }
            }
        }
        2 => {
            // Format 2: ranges with 2-byte count
            let mut gid: u16 = 1;
            while (gid as usize) < num_glyphs && pos + 3 < data.len() {
                let first_sid = u16_be(data, pos);
                let n_left = u16_be(data, pos + 2);
                pos += 4;
                for delta in 0..=n_left {
                    if (gid as usize) >= num_glyphs {
                        break;
                    }
                    let sid = first_sid + delta;
                    if let Some(name) = sid_to_name(sid, string_index) {
                        map.insert(name, gid);
                    }
                    gid += 1;
                }
            }
        }
        _ => {
            // Unknown charset format; map will only have .notdef.
        }
    }

    map
}

/// Resolve a CFF SID to a glyph name string.
fn sid_to_name(sid: u16, string_index: &[Vec<u8>]) -> Option<String> {
    if (sid as usize) < CFF_STANDARD_STRINGS.len() {
        Some(CFF_STANDARD_STRINGS[sid as usize].to_string())
    } else {
        let idx = sid as usize - CFF_STANDARD_STRINGS.len();
        string_index
            .get(idx)
            .map(|data| String::from_utf8_lossy(data).into_owned())
    }
}

/// Return SIDs for a predefined charset (0=ISOAdobe, 1=Expert, 2=ExpertSubset).
fn predefined_charset_sids(charset_id: usize, num_glyphs: usize) -> Vec<u16> {
    let max = num_glyphs.saturating_sub(1);
    match charset_id {
        0 => {
            // ISOAdobe: SIDs 1..228
            (1..=228).take(max).collect()
        }
        1 => {
            // Expert: first 87 glyphs cover expert encoding. Simplified.
            (1..=228).take(max).collect()
        }
        _ => {
            // ExpertSubset: similar.
            (1..=228).take(max).collect()
        }
    }
}

/// Map a Unicode character to its standard PostScript glyph name.
fn unicode_to_glyph_name(ch: char) -> Option<&'static str> {
    // Fast path for ASCII letters and digits.
    match ch {
        'A'..='Z' | 'a'..='z' => {
            // PostScript names for letters are the letter itself.
            let idx = ch as usize;
            UNICODE_TO_PS_NAME.get(&(idx as u32)).copied()
        }
        _ => UNICODE_TO_PS_NAME.get(&(ch as u32)).copied(),
    }
}

use std::sync::LazyLock;

/// Unicode codepoint -> PostScript glyph name mapping.
/// Covers Latin-1 and common punctuation/symbols used in PDF fonts.
static UNICODE_TO_PS_NAME: LazyLock<HashMap<u32, &'static str>> = LazyLock::new(|| {
    let mut m = HashMap::with_capacity(256);
    // ASCII control and space
    m.insert(0x0020, "space");
    m.insert(0x0021, "exclam");
    m.insert(0x0022, "quotedbl");
    m.insert(0x0023, "numbersign");
    m.insert(0x0024, "dollar");
    m.insert(0x0025, "percent");
    m.insert(0x0026, "ampersand");
    m.insert(0x0027, "quotesingle");
    m.insert(0x0028, "parenleft");
    m.insert(0x0029, "parenright");
    m.insert(0x002A, "asterisk");
    m.insert(0x002B, "plus");
    m.insert(0x002C, "comma");
    m.insert(0x002D, "hyphen");
    m.insert(0x002E, "period");
    m.insert(0x002F, "slash");
    // Digits
    m.insert(0x0030, "zero");
    m.insert(0x0031, "one");
    m.insert(0x0032, "two");
    m.insert(0x0033, "three");
    m.insert(0x0034, "four");
    m.insert(0x0035, "five");
    m.insert(0x0036, "six");
    m.insert(0x0037, "seven");
    m.insert(0x0038, "eight");
    m.insert(0x0039, "nine");
    m.insert(0x003A, "colon");
    m.insert(0x003B, "semicolon");
    m.insert(0x003C, "less");
    m.insert(0x003D, "equal");
    m.insert(0x003E, "greater");
    m.insert(0x003F, "question");
    m.insert(0x0040, "at");
    // Uppercase letters
    for c in b'A'..=b'Z' {
        m.insert(c as u32, LETTER_NAMES[(c - b'A') as usize]);
    }
    m.insert(0x005B, "bracketleft");
    m.insert(0x005C, "backslash");
    m.insert(0x005D, "bracketright");
    m.insert(0x005E, "asciicircum");
    m.insert(0x005F, "underscore");
    m.insert(0x0060, "grave");
    // Lowercase letters
    for c in b'a'..=b'z' {
        m.insert(c as u32, LOWERCASE_NAMES[(c - b'a') as usize]);
    }
    m.insert(0x007B, "braceleft");
    m.insert(0x007C, "bar");
    m.insert(0x007D, "braceright");
    m.insert(0x007E, "asciitilde");
    // Latin-1 supplement (common in PDF)
    m.insert(0x00A0, "nbspace");
    m.insert(0x00A1, "exclamdown");
    m.insert(0x00A2, "cent");
    m.insert(0x00A3, "sterling");
    m.insert(0x00A4, "currency");
    m.insert(0x00A5, "yen");
    m.insert(0x00A6, "brokenbar");
    m.insert(0x00A7, "section");
    m.insert(0x00A8, "dieresis");
    m.insert(0x00A9, "copyright");
    m.insert(0x00AA, "ordfeminine");
    m.insert(0x00AB, "guillemotleft");
    m.insert(0x00AC, "logicalnot");
    m.insert(0x00AD, "softhyphen");
    m.insert(0x00AE, "registered");
    m.insert(0x00AF, "macron");
    m.insert(0x00B0, "degree");
    m.insert(0x00B1, "plusminus");
    m.insert(0x00B2, "twosuperior");
    m.insert(0x00B3, "threesuperior");
    m.insert(0x00B4, "acute");
    m.insert(0x00B5, "mu");
    m.insert(0x00B6, "paragraph");
    m.insert(0x00B7, "periodcentered");
    m.insert(0x00B8, "cedilla");
    m.insert(0x00B9, "onesuperior");
    m.insert(0x00BA, "ordmasculine");
    m.insert(0x00BB, "guillemotright");
    m.insert(0x00BC, "onequarter");
    m.insert(0x00BD, "onehalf");
    m.insert(0x00BE, "threequarters");
    m.insert(0x00BF, "questiondown");
    // Latin accented (selected)
    m.insert(0x00C0, "Agrave");
    m.insert(0x00C1, "Aacute");
    m.insert(0x00C2, "Acircumflex");
    m.insert(0x00C3, "Atilde");
    m.insert(0x00C4, "Adieresis");
    m.insert(0x00C5, "Aring");
    m.insert(0x00C6, "AE");
    m.insert(0x00C7, "Ccedilla");
    m.insert(0x00C8, "Egrave");
    m.insert(0x00C9, "Eacute");
    m.insert(0x00CA, "Ecircumflex");
    m.insert(0x00CB, "Edieresis");
    m.insert(0x00CC, "Igrave");
    m.insert(0x00CD, "Iacute");
    m.insert(0x00CE, "Icircumflex");
    m.insert(0x00CF, "Idieresis");
    m.insert(0x00D0, "Eth");
    m.insert(0x00D1, "Ntilde");
    m.insert(0x00D2, "Ograve");
    m.insert(0x00D3, "Oacute");
    m.insert(0x00D4, "Ocircumflex");
    m.insert(0x00D5, "Otilde");
    m.insert(0x00D6, "Odieresis");
    m.insert(0x00D7, "multiply");
    m.insert(0x00D8, "Oslash");
    m.insert(0x00D9, "Ugrave");
    m.insert(0x00DA, "Uacute");
    m.insert(0x00DB, "Ucircumflex");
    m.insert(0x00DC, "Udieresis");
    m.insert(0x00DD, "Yacute");
    m.insert(0x00DE, "Thorn");
    m.insert(0x00DF, "germandbls");
    m.insert(0x00E0, "agrave");
    m.insert(0x00E1, "aacute");
    m.insert(0x00E2, "acircumflex");
    m.insert(0x00E3, "atilde");
    m.insert(0x00E4, "adieresis");
    m.insert(0x00E5, "aring");
    m.insert(0x00E6, "ae");
    m.insert(0x00E7, "ccedilla");
    m.insert(0x00E8, "egrave");
    m.insert(0x00E9, "eacute");
    m.insert(0x00EA, "ecircumflex");
    m.insert(0x00EB, "edieresis");
    m.insert(0x00EC, "igrave");
    m.insert(0x00ED, "iacute");
    m.insert(0x00EE, "icircumflex");
    m.insert(0x00EF, "idieresis");
    m.insert(0x00F0, "eth");
    m.insert(0x00F1, "ntilde");
    m.insert(0x00F2, "ograve");
    m.insert(0x00F3, "oacute");
    m.insert(0x00F4, "ocircumflex");
    m.insert(0x00F5, "otilde");
    m.insert(0x00F6, "odieresis");
    m.insert(0x00F7, "divide");
    m.insert(0x00F8, "oslash");
    m.insert(0x00F9, "ugrave");
    m.insert(0x00FA, "uacute");
    m.insert(0x00FB, "ucircumflex");
    m.insert(0x00FC, "udieresis");
    m.insert(0x00FD, "yacute");
    m.insert(0x00FE, "thorn");
    m.insert(0x00FF, "ydieresis");
    // Common typographic characters
    m.insert(0x2013, "endash");
    m.insert(0x2014, "emdash");
    m.insert(0x2018, "quoteleft");
    m.insert(0x2019, "quoteright");
    m.insert(0x201A, "quotesinglbase");
    m.insert(0x201C, "quotedblleft");
    m.insert(0x201D, "quotedblright");
    m.insert(0x201E, "quotedblbase");
    m.insert(0x2020, "dagger");
    m.insert(0x2021, "daggerdbl");
    m.insert(0x2022, "bullet");
    m.insert(0x2026, "ellipsis");
    m.insert(0x2030, "perthousand");
    m.insert(0x2039, "guilsinglleft");
    m.insert(0x203A, "guilsinglright");
    m.insert(0x2044, "fraction");
    m.insert(0x20AC, "Euro");
    m.insert(0x2122, "trademark");
    m.insert(0xFB01, "fi");
    m.insert(0xFB02, "fl");
    // Greek capital letters
    m.insert(0x0391, "Alpha");
    m.insert(0x0392, "Beta");
    m.insert(0x0393, "Gamma");
    m.insert(0x0394, "Delta");
    m.insert(0x0395, "Epsilon");
    m.insert(0x0396, "Zeta");
    m.insert(0x0397, "Eta");
    m.insert(0x0398, "Theta");
    m.insert(0x0399, "Iota");
    m.insert(0x039A, "Kappa");
    m.insert(0x039B, "Lambda");
    m.insert(0x039C, "Mu");
    m.insert(0x039D, "Nu");
    m.insert(0x039E, "Xi");
    m.insert(0x039F, "Omicron");
    m.insert(0x03A0, "Pi");
    m.insert(0x03A1, "Rho");
    m.insert(0x03A3, "Sigma");
    m.insert(0x03A4, "Tau");
    m.insert(0x03A5, "Upsilon");
    m.insert(0x03A6, "Phi");
    m.insert(0x03A7, "Chi");
    m.insert(0x03A8, "Psi");
    m.insert(0x03A9, "Omega");
    // Greek lowercase letters
    m.insert(0x03B1, "alpha");
    m.insert(0x03B2, "beta");
    m.insert(0x03B3, "gamma");
    m.insert(0x03B4, "delta");
    m.insert(0x03B5, "epsilon");
    m.insert(0x03B6, "zeta");
    m.insert(0x03B7, "eta");
    m.insert(0x03B8, "theta");
    m.insert(0x03B9, "iota");
    m.insert(0x03BA, "kappa");
    m.insert(0x03BB, "lambda");
    m.insert(0x03BC, "mu");
    m.insert(0x03BD, "nu");
    m.insert(0x03BE, "xi");
    m.insert(0x03BF, "omicron");
    m.insert(0x03C0, "pi");
    m.insert(0x03C1, "rho");
    m.insert(0x03C2, "sigma1");
    m.insert(0x03C3, "sigma");
    m.insert(0x03C4, "tau");
    m.insert(0x03C5, "upsilon");
    m.insert(0x03C6, "phi");
    m.insert(0x03C7, "chi");
    m.insert(0x03C8, "psi");
    m.insert(0x03C9, "omega");
    // Math operators and symbols
    m.insert(0x2202, "partialdiff");
    m.insert(0x2205, "emptyset");
    m.insert(0x2206, "Delta");
    m.insert(0x2207, "gradient");
    m.insert(0x2208, "element");
    m.insert(0x2209, "notelement");
    m.insert(0x220B, "suchthat");
    m.insert(0x220F, "product");
    m.insert(0x2211, "summation");
    m.insert(0x2212, "minus");
    m.insert(0x2215, "fraction");
    m.insert(0x2217, "asteriskmath");
    m.insert(0x221A, "radical");
    m.insert(0x221D, "proportional");
    m.insert(0x221E, "infinity");
    m.insert(0x2220, "angle");
    m.insert(0x2227, "logicaland");
    m.insert(0x2228, "logicalor");
    m.insert(0x2229, "intersection");
    m.insert(0x222A, "union");
    m.insert(0x222B, "integral");
    m.insert(0x2234, "therefore");
    m.insert(0x223C, "similar");
    m.insert(0x2245, "congruent");
    m.insert(0x2248, "approxequal");
    m.insert(0x2260, "notequal");
    m.insert(0x2261, "equivalence");
    m.insert(0x2264, "lessequal");
    m.insert(0x2265, "greaterequal");
    m.insert(0x2282, "propersubset");
    m.insert(0x2283, "propersuperset");
    m.insert(0x2284, "notsubset");
    m.insert(0x2286, "reflexsubset");
    m.insert(0x2287, "reflexsuperset");
    m.insert(0x2295, "circleplus");
    m.insert(0x2297, "circlemultiply");
    m.insert(0x22A5, "perpendicular");
    // Arrows
    m.insert(0x2190, "arrowleft");
    m.insert(0x2191, "arrowup");
    m.insert(0x2192, "arrowright");
    m.insert(0x2193, "arrowdown");
    m.insert(0x2194, "arrowboth");
    m.insert(0x21D0, "arrowdblleft");
    m.insert(0x21D1, "arrowdblup");
    m.insert(0x21D2, "arrowdblright");
    m.insert(0x21D3, "arrowdbldown");
    m.insert(0x21D4, "arrowdblboth");
    // Miscellaneous symbols
    m.insert(0x2032, "minute");
    m.insert(0x2033, "second");
    m.insert(0x25CA, "lozenge");
    m.insert(0x2660, "spade");
    m.insert(0x2663, "club");
    m.insert(0x2665, "heart");
    m.insert(0x2666, "diamond");
    m
});

static LETTER_NAMES: [&str; 26] = [
    "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S",
    "T", "U", "V", "W", "X", "Y", "Z",
];

static LOWERCASE_NAMES: [&str; 26] = [
    "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s",
    "t", "u", "v", "w", "x", "y", "z",
];

/// CFF standard strings (SIDs 0-390). The full list per the CFF spec.
/// Only the first 229 are commonly needed (ISOAdobe charset range).
static CFF_STANDARD_STRINGS: &[&str] = &[
    ".notdef",
    "space",
    "exclam",
    "quotedbl",
    "numbersign",
    "dollar",
    "percent",
    "ampersand",
    "quoteright",
    "parenleft",
    "parenright",
    "asterisk",
    "plus",
    "comma",
    "hyphen",
    "period",
    "slash",
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "colon",
    "semicolon",
    "less",
    "equal",
    "greater",
    "question",
    "at",
    "A",
    "B",
    "C",
    "D",
    "E",
    "F",
    "G",
    "H",
    "I",
    "J",
    "K",
    "L",
    "M",
    "N",
    "O",
    "P",
    "Q",
    "R",
    "S",
    "T",
    "U",
    "V",
    "W",
    "X",
    "Y",
    "Z",
    "bracketleft",
    "backslash",
    "bracketright",
    "asciicircum",
    "underscore",
    "quoteleft",
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "braceleft",
    "bar",
    "braceright",
    "asciitilde",
    // SIDs 95-228 (ISOAdobe range continued)
    "exclamdown",
    "cent",
    "sterling",
    "fraction",
    "yen",
    "florin",
    "section",
    "currency",
    "quotesingle",
    "quotedblleft",
    "guillemotleft",
    "guilsinglleft",
    "guilsinglright",
    "fi",
    "fl",
    "endash",
    "dagger",
    "daggerdbl",
    "periodcentered",
    "paragraph",
    "bullet",
    "quotesinglbase",
    "quotedblbase",
    "quotedblright",
    "guillemotright",
    "ellipsis",
    "perthousand",
    "questiondown",
    "grave",
    "acute",
    "circumflex",
    "tilde",
    "macron",
    "breve",
    "dotaccent",
    "dieresis",
    "ring",
    "cedilla",
    "hungarumlaut",
    "ogonek",
    "caron",
    "emdash",
    "AE",
    "ordfeminine",
    "Lslash",
    "Oslash",
    "OE",
    "ordmasculine",
    "ae",
    "dotlessi",
    "lslash",
    "oslash",
    "oe",
    "germandbls",
    // SIDs 149-228
    "onesuperior",
    "logicalnot",
    "mu",
    "trademark",
    "Eth",
    "onehalf",
    "plusminus",
    "Thorn",
    "onequarter",
    "divide",
    "brokenbar",
    "degree",
    "thorn",
    "threequarters",
    "twosuperior",
    "registered",
    "minus",
    "eth",
    "multiply",
    "threesuperior",
    "copyright",
    "Aacute",
    "Acircumflex",
    "Adieresis",
    "Agrave",
    "Aring",
    "Atilde",
    "Ccedilla",
    "Eacute",
    "Ecircumflex",
    "Edieresis",
    "Egrave",
    "Iacute",
    "Icircumflex",
    "Idieresis",
    "Igrave",
    "Ntilde",
    "Oacute",
    "Ocircumflex",
    "Odieresis",
    "Ograve",
    "Otilde",
    "Scaron",
    "Uacute",
    "Ucircumflex",
    "Udieresis",
    "Ugrave",
    "Yacute",
    "Ydieresis",
    "Zcaron",
    "aacute",
    "acircumflex",
    "adieresis",
    "agrave",
    "aring",
    "atilde",
    "ccedilla",
    "eacute",
    "ecircumflex",
    "edieresis",
    "egrave",
    "iacute",
    "icircumflex",
    "idieresis",
    "igrave",
    "ntilde",
    "oacute",
    "ocircumflex",
    "odieresis",
    "ograve",
    "otilde",
    "scaron",
    "uacute",
    "ucircumflex",
    "udieresis",
    "ugrave",
    "yacute",
    "ydieresis",
    "zcaron",
    // SIDs 229-390 (Expert encoding and extras)
    "exclamsmall",
    "Hungarumlautsmall",
    "dollaroldstyle",
    "dollarsuperior",
    "ampersandsmall",
    "Acutesmall",
    "parenleftsuperior",
    "parenrightsuperior",
    "twodotenleader",
    "onedotenleader",
    "zerooldstyle",
    "oneoldstyle",
    "twooldstyle",
    "threeoldstyle",
    "fouroldstyle",
    "fiveoldstyle",
    "sixoldstyle",
    "sevenoldstyle",
    "eightoldstyle",
    "nineoldstyle",
    "commasuperior",
    "threequartersemdash",
    "periodsuperior",
    "questionsmall",
    "asuperior",
    "bsuperior",
    "centsuperior",
    "dsuperior",
    "esuperior",
    "isuperior",
    "lsuperior",
    "msuperior",
    "nsuperior",
    "osuperior",
    "rsuperior",
    "ssuperior",
    "tsuperior",
    "ff",
    "ffi",
    "ffl",
    "parenleftinferior",
    "parenrightinferior",
    "Circumflexsmall",
    "hyphensuperior",
    "Gravesmall",
    "Asmall",
    "Bsmall",
    "Csmall",
    "Dsmall",
    "Esmall",
    "Fsmall",
    "Gsmall",
    "Hsmall",
    "Ismall",
    "Jsmall",
    "Ksmall",
    "Lsmall",
    "Msmall",
    "Nsmall",
    "Osmall",
    "Psmall",
    "Qsmall",
    "Rsmall",
    "Ssmall",
    "Tsmall",
    "Usmall",
    "Vsmall",
    "Wsmall",
    "Xsmall",
    "Ysmall",
    "Zsmall",
    "colonmonetary",
    "onefitted",
    "rupiah",
    "Tildesmall",
    "exclamdownsmall",
    "centoldstyle",
    "Lslashsmall",
    "Scaronsmall",
    "Zcaronsmall",
    "Dieresissmall",
    "Brevesmall",
    "Caronsmall",
    "Dotaccentsmall",
    "Macronsmall",
    "figuredash",
    "hypheninferior",
    "Ogoneksmall",
    "Ringsmall",
    "Cedillasmall",
    "questiondownsmall",
    "oneeighth",
    "threeeighths",
    "fiveeighths",
    "seveneighths",
    "onethird",
    "twothirds",
    "zerosuperior",
    "foursuperior",
    "fivesuperior",
    "sixsuperior",
    "sevensuperior",
    "eightsuperior",
    "ninesuperior",
    "zeroinferior",
    "oneinferior",
    "twoinferior",
    "threeinferior",
    "fourinferior",
    "fiveinferior",
    "sixinferior",
    "seveninferior",
    "eightinferior",
    "nineinferior",
    "centinferior",
    "dollarinferior",
    "periodinferior",
    "commainferior",
    "Agravesmall",
    "Aacutesmall",
    "Acircumflexsmall",
    "Atildesmall",
    "Adieresissmall",
    "Aringsmall",
    "AEsmall",
    "Ccedillasmall",
    "Egravesmall",
    "Eacutesmall",
    "Ecircumflexsmall",
    "Edieresissmall",
    "Igravesmall",
    "Iacutesmall",
    "Icircumflexsmall",
    "Idieresissmall",
    "Ethsmall",
    "Ntildesmall",
    "Ogravesmall",
    "Oacutesmall",
    "Ocircumflexsmall",
    "Otildesmall",
    "Odieresissmall",
    "OEsmall",
    "Oslashsmall",
    "Ugravesmall",
    "Uacutesmall",
    "Ucircumflexsmall",
    "Udieresissmall",
    "Yacutesmall",
    "Thornsmall",
    "Ydieresissmall",
    "001.000",
    "001.001",
    "001.002",
    "001.003",
    "Black",
    "Bold",
    "Book",
    "Light",
    "Medium",
    "Regular",
    "Roman",
    "Semibold",
];

// ---------------------------------------------------------------------------
// Binary reading helpers
// ---------------------------------------------------------------------------

#[inline]
fn u16_be(data: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([data[offset], data[offset + 1]])
}

#[inline]
fn i16_be(data: &[u8], offset: usize) -> i16 {
    i16::from_be_bytes([data[offset], data[offset + 1]])
}

#[inline]
fn i32_be(data: &[u8], offset: usize) -> i32 {
    i32::from_be_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid CFF font with one glyph.
    fn build_minimal_cff() -> Vec<u8> {
        let mut buf = Vec::new();

        // CFF header: major=1, minor=0, hdrSize=4, offSize=1
        buf.extend_from_slice(&[1, 0, 4, 1]);

        // Name INDEX: 1 entry "Test"
        buf.extend_from_slice(&[0, 1]); // count = 1
        buf.push(1); // offSize = 1
        buf.push(1); // offset[0] = 1
        buf.push(5); // offset[1] = 5
        buf.extend_from_slice(b"Test");

        // Top DICT INDEX: 1 entry with CharStrings offset and Private DICT
        // Top DICT will be built below with the correct CharStrings offset.

        // We need to know the CharStrings offset. Let's build forward.
        // Top DICT will contain just the CharStrings offset.
        // Build the rest first to calculate offsets.

        // For a minimal CFF, place data in order:
        // [header][name INDEX][top dict INDEX][string INDEX][gsubr INDEX][charstrings INDEX]
        // Top DICT points to charstrings offset.

        // String INDEX: empty
        let string_idx: Vec<u8> = vec![0, 0]; // count = 0

        // Global Subr INDEX: empty
        let gsubr_idx: Vec<u8> = vec![0, 0]; // count = 0

        // CharStrings INDEX: 2 entries (glyph 0 = .notdef empty, glyph 1 = simple path)
        // Glyph 1: rmoveto(100, 200) rlineto(300, 0) rlineto(0, 400) endchar
        let cs0: Vec<u8> = vec![14]; // endchar
                                     // Build glyph 1 using 28 (i16) encoding for clarity.
        let mut cs1 = Vec::new();
        push_i16(&mut cs1, 100); // x
        push_i16(&mut cs1, 200); // y
        cs1.push(21); // rmoveto
        push_i16(&mut cs1, 300); // dx
        push_i16(&mut cs1, 0); // dy
        cs1.push(5); // rlineto
        push_i16(&mut cs1, 0); // dx
        push_i16(&mut cs1, 400); // dy
        cs1.push(5); // rlineto
        cs1.push(14); // endchar

        let charstrings = build_index(&[&cs0, &cs1]);

        // Now build Top DICT with charstrings offset.
        // The charstrings come after: header(4) + name_idx + top_dict_idx + string_idx(2) + gsubr_idx(2)
        // We need to know the top_dict_idx size to compute charstrings_offset.
        // Circular dependency! Let's use a fixed-size encoding.

        // Top DICT: encode charstrings_offset as 5-byte int (op 29 = 4 bytes)
        // We'll fill in the offset after computing it.
        let mut top_dict_entry = Vec::new();
        // Placeholder for charstrings offset (29 + 4 bytes + operator 17)
        top_dict_entry.push(29); // 4-byte int follows
        let charstrings_offset_pos = top_dict_entry.len();
        top_dict_entry.extend_from_slice(&[0, 0, 0, 0]); // placeholder
        top_dict_entry.push(17); // CharStrings operator

        let top_dict_idx = build_index(&[&top_dict_entry]);

        // Calculate actual charstrings offset
        let cs_offset = 4 + // header
            name_index_size() +
            top_dict_idx.len() +
            string_idx.len() +
            gsubr_idx.len();

        // Patch the offset in top_dict_entry
        let mut top_dict_entry_patched = top_dict_entry.clone();
        let offset_bytes = (cs_offset as i32).to_be_bytes();
        top_dict_entry_patched[charstrings_offset_pos..charstrings_offset_pos + 4]
            .copy_from_slice(&offset_bytes);

        let top_dict_idx_patched = build_index(&[&top_dict_entry_patched]);

        // Reassemble with correct offsets
        buf.clear();
        buf.extend_from_slice(&[1, 0, 4, 1]); // header
                                              // Name INDEX
        buf.extend_from_slice(&[0, 1, 1, 1, 5]); // count=1, offSize=1, offsets=[1,5]
        buf.extend_from_slice(b"Test");
        // Top DICT INDEX
        buf.extend_from_slice(&top_dict_idx_patched);
        // String INDEX (empty)
        buf.extend_from_slice(&string_idx);
        // Global Subr INDEX (empty)
        buf.extend_from_slice(&gsubr_idx);
        // CharStrings INDEX
        buf.extend_from_slice(&charstrings);

        buf
    }

    fn name_index_size() -> usize {
        2 + 1 + 2 + 4 // count(2) + offSize(1) + offsets(2) + data(4)
    }

    fn push_i16(buf: &mut Vec<u8>, val: i16) {
        buf.push(28);
        buf.extend_from_slice(&val.to_be_bytes());
    }

    fn build_index(entries: &[&[u8]]) -> Vec<u8> {
        let mut buf = Vec::new();
        let count = entries.len() as u16;
        buf.extend_from_slice(&count.to_be_bytes());
        if count == 0 {
            return buf;
        }
        buf.push(1); // offSize = 1 (works for small entries)

        // Build offsets (1-based)
        let mut offset: u8 = 1;
        for entry in entries {
            buf.push(offset);
            offset = offset.saturating_add(entry.len() as u8);
        }
        buf.push(offset); // final offset

        // Data
        for entry in entries {
            buf.extend_from_slice(entry);
        }
        buf
    }

    #[test]
    fn parse_minimal_cff() {
        let data = build_minimal_cff();
        let font = CffFont::from_bytes(&data).expect("should parse minimal CFF");
        assert_eq!(font.num_glyphs(), 2);
    }

    #[test]
    fn cff_glyph_outline() {
        let data = build_minimal_cff();
        let font = CffFont::from_bytes(&data).expect("should parse");

        // Glyph 0 (.notdef) is just endchar, should be empty
        assert!(font.glyph_outline(0).is_none());

        // Glyph 1 should have contours from rmoveto + rlineto + rlineto
        let outline = font.glyph_outline(1).expect("glyph 1 should have outline");
        assert!(!outline.contours.is_empty());
        // Should have on-curve points from the lineto operations
        let total_points: usize = outline.contours.iter().map(|c| c.points.len()).sum();
        assert!(
            total_points >= 2,
            "expected at least 2 points, got {total_points}"
        );
    }

    #[test]
    fn cff_reject_too_short() {
        assert!(CffFont::from_bytes(&[1, 0]).is_err());
    }

    #[test]
    fn cff_out_of_range_glyph() {
        let data = build_minimal_cff();
        let font = CffFont::from_bytes(&data).expect("should parse");
        assert!(font.glyph_outline(999).is_none());
    }

    #[test]
    fn glyph_name_suffix_fallback() {
        let data = build_minimal_cff();
        let mut font = CffFont::from_bytes(&data).expect("should parse");
        // Insert a base glyph name mapping.
        font.name_to_gid.insert("g".to_string(), 1);
        // Exact match works.
        assert_eq!(font.glyph_id_for_name("g"), Some(1));
        // Suffix-stripped fallback finds base name.
        assert_eq!(font.glyph_id_for_name("g.alt"), Some(1));
        assert_eq!(font.glyph_id_for_name("g.sc"), Some(1));
        // Non-existent base name returns None.
        assert_eq!(font.glyph_id_for_name("z.alt"), None);
        // No suffix returns None for missing names.
        assert_eq!(font.glyph_id_for_name("z"), None);
        // Exact match takes priority over suffix stripping.
        font.name_to_gid.insert("g.alt".to_string(), 0);
        assert_eq!(font.glyph_id_for_name("g.alt"), Some(0));
    }

    #[test]
    fn subr_bias_values() {
        assert_eq!(subr_bias(0), 107);
        assert_eq!(subr_bias(100), 107);
        assert_eq!(subr_bias(1239), 107);
        assert_eq!(subr_bias(1240), 1131);
        assert_eq!(subr_bias(33899), 1131);
        assert_eq!(subr_bias(33900), 32768);
    }
}
