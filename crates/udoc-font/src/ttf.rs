//! TrueType font outline parser for PDF rendering.
//!
//! Parses embedded TrueType font programs (FontFile2 streams) to extract
//! glyph outlines for rasterization. Handles the subset of TrueType tables
//! needed for rendering: head, hhea, hmtx, maxp, cmap, loca, glyf.
//!
//! This is a minimal parser targeting embedded PDF fonts, not a full OpenType
//! implementation. It handles simple and compound glyphs with quadratic bezier
//! outlines sufficient for the page renderer.

use crate::error::{Error, Result, ResultExt};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Limits from the maxp table version 1.0 used by the hinting VM.
#[derive(Debug, Clone, Default)]
pub struct HintingLimits {
    /// maxTwilightPoints from maxp (twilight-zone point count).
    pub max_twilight_points: u16,
    /// maxStorage from maxp (Storage Area size, for WS/RS opcodes).
    pub max_storage: u16,
    /// maxFunctionDefs from maxp (FDEF slot count).
    pub max_function_defs: u16,
    /// maxStackElements from maxp (hinting interpreter stack depth).
    pub max_stack_elements: u16,
}

/// Raw glyph data with contour structure and instructions for the hinting VM.
#[derive(Debug, Clone)]
pub struct RawGlyphData {
    /// Glyph outline points in font units.
    pub points: Vec<OutlinePoint>,
    /// End-of-contour indices (each is the index of the last point in a contour).
    pub contour_ends: Vec<usize>,
    /// Per-glyph TrueType hinting instructions.
    pub instructions: Vec<u8>,
    /// Advance width in font units.
    pub advance_width: u16,
    /// Left side bearing in font units.
    pub lsb: i16,
    /// Glyph bounding box left (xMin in font units).
    pub x_min: i16,
    /// Glyph bounding box bottom (yMin in font units).
    pub y_min: i16,
}

/// Parsed TrueType font program with the tables needed for rasterization.
pub struct TrueTypeFont {
    /// Units per em from the head table. Typically 1000 or 2048.
    units_per_em: u16,
    /// Number of glyphs in the font.
    num_glyphs: u16,
    /// Number of horizontal metrics in hmtx (numOfLongHorMetrics from hhea).
    num_h_metrics: u16,
    /// Whether loca uses long (32-bit) offsets (head.indexToLocFormat == 1).
    loca_is_long: bool,
    /// Raw cmap table data for character-to-glyph mapping.
    cmap_data: Vec<u8>,
    /// Raw loca table data (glyph offsets into glyf).
    loca_data: Vec<u8>,
    /// Raw glyf table data (glyph outlines).
    glyf_data: Vec<u8>,
    /// Raw hmtx table data (horizontal metrics).
    hmtx_data: Vec<u8>,
    /// Raw font program (fpgm) table for TrueType hinting. Empty if absent.
    fpgm_data: Vec<u8>,
    /// Raw control value program (prep) table. Empty if absent.
    prep_data: Vec<u8>,
    /// Raw control value table (cvt). Empty if absent.
    cvt_data: Vec<u8>,
    /// Hinting VM limits from maxp v1.0.
    hinting_limits: HintingLimits,
}

/// Stem hint data from Type1/CFF charstrings for grid fitting.
#[derive(Debug, Clone, Default)]
pub struct StemHints {
    /// Horizontal stems: (y_position, height) in font units.
    pub h_stems: Vec<(f64, f64)>,
    /// Vertical stems: (x_position, width) in font units.
    pub v_stems: Vec<(f64, f64)>,
}

/// A glyph outline consisting of one or more contours.
#[derive(Debug, Clone)]
pub struct GlyphOutline {
    /// Contours making up the glyph shape. Each contour is a closed path.
    pub contours: Vec<Contour>,
    /// Glyph bounding box: (x_min, y_min, x_max, y_max) in font units.
    pub bounds: (i16, i16, i16, i16),
    /// Stem hints for grid fitting (empty for TrueType fonts).
    pub stem_hints: StemHints,
}

/// A single closed contour of a glyph outline.
#[derive(Debug, Clone)]
pub struct Contour {
    /// Points defining the contour. On-curve points are endpoints of line
    /// segments or bezier curves. Off-curve points are quadratic bezier
    /// control points.
    pub points: Vec<OutlinePoint>,
}

/// A point in a glyph outline.
#[derive(Debug, Clone, Copy)]
pub struct OutlinePoint {
    /// X coordinate in font units (post-CTM but pre-scale-to-pixels).
    pub x: f64,
    /// Y coordinate in font units (post-CTM but pre-scale-to-pixels).
    pub y: f64,
    /// True for on-curve points (line endpoints), false for quadratic
    /// bezier control points.
    pub on_curve: bool,
}

// ---------------------------------------------------------------------------
// Table directory parsing
// ---------------------------------------------------------------------------
//
// TrueType and OpenType share the same sfnt table directory layout, so the
// table lookup lives in [`crate::otf`] and both parsers delegate here. See
// issue #206 for the hoist rationale.

use crate::otf;

/// Find a table in the TrueType table directory. Delegates to the shared
/// sfnt walker in [`crate::otf`].
fn find_table(data: &[u8], tag: &[u8; 4]) -> Option<otf::TableEntry> {
    otf::find_table(data, tag)
}

/// Extract a table's raw bytes from the font data. Delegates to the shared
/// sfnt walker in [`crate::otf`]; wraps the error with the TrueType context
/// so callers still see the TrueType-flavored message.
fn table_data<'a>(data: &'a [u8], tag: &[u8; 4]) -> Result<&'a [u8]> {
    otf::table_data(data, tag).map_err(|_| {
        // Preserve the original TrueType-specific phrasing so existing
        // callers (e.g. `head table` context) match the downstream message.
        Error::new(format!(
            "missing required TrueType table '{}' or table extends past data",
            String::from_utf8_lossy(tag)
        ))
    })
}

// ---------------------------------------------------------------------------
// TrueTypeFont implementation
// ---------------------------------------------------------------------------

impl TrueTypeFont {
    /// Parse a TrueType font from raw font program bytes.
    ///
    /// The data should be a decompressed FontFile2 stream from a PDF.
    /// Validates the required tables exist and extracts their data.
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() < 12 {
            return Err(Error::new("TrueType data too short for header"));
        }

        // Validate TrueType magic: 0x00010000 or "true" or "OTTO" (OpenType/CFF)
        let magic = u32(data, 0);
        if magic != 0x00010000 && magic != 0x74727565 {
            // 0x4F54544F = "OTTO" is OpenType/CFF, not TrueType
            if magic == 0x4F54544F {
                return Err(Error::new(
                    "font is OpenType/CFF (OTTO), not TrueType; use CFF parser",
                ));
            }
            return Err(Error::new(format!(
                "unrecognized TrueType magic: 0x{magic:08X}"
            )));
        }

        // Parse head table for unitsPerEm and indexToLocFormat.
        let head = table_data(data, b"head").context("reading head table")?;
        if head.len() < 54 {
            return Err(Error::new("head table too short"));
        }
        let units_per_em = u16(head, 18);
        if units_per_em == 0 {
            return Err(Error::new("head.unitsPerEm is 0"));
        }
        let index_to_loc_format = i16(head, 50);
        let loca_is_long = index_to_loc_format == 1;

        // Parse maxp table for numGlyphs.
        let maxp = table_data(data, b"maxp").context("reading maxp table")?;
        if maxp.len() < 6 {
            return Err(Error::new("maxp table too short"));
        }
        let num_glyphs = u16(maxp, 4);

        // Parse hhea table for numberOfHMetrics.
        let hhea = table_data(data, b"hhea").context("reading hhea table")?;
        if hhea.len() < 36 {
            return Err(Error::new("hhea table too short"));
        }
        let num_h_metrics = u16(hhea, 34);

        // Extract raw table data for lazy glyph parsing.
        let cmap_data = table_data(data, b"cmap")
            .context("reading cmap table")?
            .to_vec();
        let loca_data = table_data(data, b"loca")
            .context("reading loca table")?
            .to_vec();
        let glyf_data = table_data(data, b"glyf")
            .context("reading glyf table")?
            .to_vec();
        let hmtx_data = table_data(data, b"hmtx")
            .context("reading hmtx table")?
            .to_vec();

        // Optional hinting tables (not all fonts have instructions).
        let fpgm_data = find_table(data, b"fpgm")
            .and_then(|e| data.get(e.offset..e.offset + e.length))
            .map(|d| d.to_vec())
            .unwrap_or_default();
        let prep_data = find_table(data, b"prep")
            .and_then(|e| data.get(e.offset..e.offset + e.length))
            .map(|d| d.to_vec())
            .unwrap_or_default();
        let cvt_data = find_table(data, b"cvt ")
            .and_then(|e| data.get(e.offset..e.offset + e.length))
            .map(|d| d.to_vec())
            .unwrap_or_default();

        // Parse maxp v1.0 hinting limits (bytes 16+).
        let hinting_limits = if maxp.len() >= 32 {
            HintingLimits {
                max_twilight_points: u16(maxp, 18),
                max_storage: u16(maxp, 20),
                max_function_defs: u16(maxp, 22),
                max_stack_elements: u16(maxp, 26),
            }
        } else {
            HintingLimits::default()
        };

        Ok(Self {
            units_per_em,
            num_glyphs,
            num_h_metrics,
            loca_is_long,
            cmap_data,
            loca_data,
            glyf_data,
            hmtx_data,
            fpgm_data,
            prep_data,
            cvt_data,
            hinting_limits,
        })
    }

    /// Units per em for coordinate scaling.
    pub fn units_per_em(&self) -> u16 {
        self.units_per_em
    }

    /// Number of glyphs in the font.
    #[allow(dead_code)]
    pub fn num_glyphs(&self) -> u16 {
        self.num_glyphs
    }

    /// Raw font program (fpgm) table for hinting VM.
    pub fn fpgm_data(&self) -> &[u8] {
        &self.fpgm_data
    }

    /// Raw control value program (prep) table for hinting VM.
    pub fn prep_data(&self) -> &[u8] {
        &self.prep_data
    }

    /// Control value table entries as signed 16-bit font-unit values.
    pub fn cvt_values(&self) -> Vec<i16> {
        self.cvt_data
            .chunks_exact(2)
            .map(|c| i16::from_be_bytes([c[0], c[1]]))
            .collect()
    }

    /// Hinting VM limits from maxp table.
    pub fn hinting_limits(&self) -> &HintingLimits {
        &self.hinting_limits
    }

    /// Whether this font has hinting instructions.
    pub fn has_hinting(&self) -> bool {
        !self.fpgm_data.is_empty() || !self.prep_data.is_empty()
    }

    /// Map a Unicode character to a glyph ID via the cmap table.
    ///
    /// Tries format 4 (BMP) and format 12 (full Unicode) subtables.
    /// Returns None if the character has no mapping.
    pub fn glyph_id(&self, ch: char) -> Option<u16> {
        let codepoint = ch as u32;
        let data = &self.cmap_data;
        if data.len() < 4 {
            return None;
        }
        let num_subtables = u16(data, 2) as usize;

        // First pass: look for format 12 (full Unicode range).
        for i in 0..num_subtables {
            let record = 4 + i * 8;
            if record + 8 > data.len() {
                break;
            }
            let _platform = u16(data, record);
            let _encoding = u16(data, record + 2);
            let offset = u32(data, record + 4) as usize;
            if offset + 4 > data.len() {
                continue;
            }
            let format = u16(data, offset);
            if format == 12 {
                if let Some(gid) = cmap_format12_lookup(data, offset, codepoint) {
                    if gid != 0 {
                        return Some(gid);
                    }
                }
            }
        }

        // Second pass: look for format 4 (BMP only).
        if codepoint <= 0xFFFF {
            for i in 0..num_subtables {
                let record = 4 + i * 8;
                if record + 8 > data.len() {
                    break;
                }
                let offset = u32(data, record + 4) as usize;
                if offset + 4 > data.len() {
                    continue;
                }
                let format = u16(data, offset);
                if format == 4 {
                    if let Some(gid) = cmap_format4_lookup(data, offset, codepoint as u16) {
                        if gid != 0 {
                            return Some(gid);
                        }
                    }
                }
            }
        }

        // Third pass: Symbol cmap retry. PDFTeX embeds TrueType subsets with
        // a (3,0) Microsoft Symbol cmap whose codepoints sit in 0xF000-0xF0FF.
        // For ASCII chars not matched in Unicode cmaps, retry with the 0xF000
        // prefix on (3,0) format-4 subtables. This recovers fonts like
        // Sora/Inter embedded by pdfTeX, which would otherwise fall through
        // to the Liberation fallback (wrong shape AND wrong scale because
        // Liberation UPM=2048 vs typical embedded UPM=1000).
        if codepoint <= 0xFF {
            let symbol_cp = (codepoint | 0xF000) as u16;
            for i in 0..num_subtables {
                let record = 4 + i * 8;
                if record + 8 > data.len() {
                    break;
                }
                let platform = u16(data, record);
                let encoding = u16(data, record + 2);
                if platform != 3 || encoding != 0 {
                    continue;
                }
                let offset = u32(data, record + 4) as usize;
                if offset + 4 > data.len() {
                    continue;
                }
                let format = u16(data, offset);
                if format == 4 {
                    if let Some(gid) = cmap_format4_lookup(data, offset, symbol_cp) {
                        if gid != 0 {
                            return Some(gid);
                        }
                    }
                }
            }
        }

        None
    }

    /// Look up a glyph by raw 8-bit character code via the font's built-in
    /// cmap. This is the correct path for a "simple" TrueType PDF font that
    /// has no explicit `/Encoding` entry: the font ships with its own byte
    /// ordering (commonly Macintosh Roman, cmap(1,0) format 6 or format 0)
    /// and the content stream's byte indexes into that table directly.
    ///
    /// This intentionally differs from `glyph_id(char)`, which looks up a
    /// Unicode codepoint via the (3,1) / (3,0) / (0,*) Unicode cmaps. For
    /// subset fonts embedded by macOS Quartz the ToUnicode map is often
    /// "creative" (e.g. byte 0x5e -> U+2019 when the glyph itself is a
    /// "ti" ligature) so the Unicode cmap gives the wrong glyph.
    pub fn glyph_id_by_byte(&self, byte: u8) -> Option<u16> {
        let data = &self.cmap_data;
        if data.len() < 4 {
            return None;
        }
        let num_subtables = u16(data, 2) as usize;

        // Prefer Mac Roman (1,0) since that's what Quartz/PDFTeX TT
        // subsets typically emit. Fall back to (3,0) Symbol or any other
        // byte-indexed table.
        let priorities = [(1u16, 0u16), (3, 0), (0, 0)];
        for &(want_plat, want_enc) in &priorities {
            for i in 0..num_subtables {
                let record = 4 + i * 8;
                if record + 8 > data.len() {
                    break;
                }
                let plat = u16(data, record);
                let enc = u16(data, record + 2);
                if plat != want_plat || enc != want_enc {
                    continue;
                }
                let offset = u32(data, record + 4) as usize;
                if offset + 2 > data.len() {
                    continue;
                }
                let format = u16(data, offset);
                match format {
                    0 => {
                        // Format 0: fixed 262-byte subtable, byte -> gid.
                        // offset+0..2 format, +2..4 length, +4..6 lang,
                        // +6..262 glyphIdArray [256 bytes].
                        let idx = offset + 6 + byte as usize;
                        if idx < data.len() {
                            let gid = data[idx] as u16;
                            if gid != 0 {
                                return Some(gid);
                            }
                        }
                    }
                    6 => {
                        // Format 6: trimmed table, first/count + gid array.
                        if offset + 10 > data.len() {
                            continue;
                        }
                        let first = u16(data, offset + 6);
                        let count = u16(data, offset + 8) as usize;
                        let b = byte as u16;
                        if b < first || b >= first + count as u16 {
                            continue;
                        }
                        let gi = (b - first) as usize;
                        let idx = offset + 10 + gi * 2;
                        if idx + 2 <= data.len() {
                            let gid = u16(data, idx);
                            if gid != 0 {
                                return Some(gid);
                            }
                        }
                    }
                    4 => {
                        // Format 4: binary-search ranges. Reuse the
                        // existing helper.
                        if let Some(gid) = cmap_format4_lookup(data, offset, byte as u16) {
                            if gid != 0 {
                                return Some(gid);
                            }
                        }
                    }
                    _ => continue,
                }
            }
        }
        None
    }

    /// Get the horizontal advance width for a glyph in font units.
    pub fn advance_width(&self, glyph_id: u16) -> u16 {
        let data = &self.hmtx_data;
        let nhm = self.num_h_metrics as usize;
        let gid = glyph_id as usize;

        if nhm == 0 {
            return 0;
        }

        if gid < nhm {
            // Each longHorMetric is 4 bytes: u16 advanceWidth + i16 lsb.
            let offset = gid * 4;
            if offset + 2 <= data.len() {
                return u16(data, offset);
            }
        } else {
            // Glyphs beyond numOfLongHorMetrics share the last advance width.
            let offset = (nhm - 1) * 4;
            if offset + 2 <= data.len() {
                return u16(data, offset);
            }
        }
        0
    }

    /// Extract the glyph outline for a glyph ID.
    ///
    /// Returns None for empty glyphs (e.g., space). Handles both simple
    /// glyphs (direct contour data) and compound glyphs (references to
    /// other glyphs with transforms).
    pub fn glyph_outline(&self, glyph_id: u16) -> Option<GlyphOutline> {
        let gid = glyph_id as usize;
        if gid >= self.num_glyphs as usize {
            return None;
        }

        // Look up glyph offset in loca table.
        let (offset, next_offset) = if self.loca_is_long {
            let base = gid * 4;
            if base + 8 > self.loca_data.len() {
                return None;
            }
            (
                u32(&self.loca_data, base) as usize,
                u32(&self.loca_data, base + 4) as usize,
            )
        } else {
            let base = gid * 2;
            if base + 4 > self.loca_data.len() {
                return None;
            }
            (
                u16(&self.loca_data, base) as usize * 2,
                u16(&self.loca_data, base + 2) as usize * 2,
            )
        };

        // Empty glyph (space, etc.): offset == next_offset.
        if offset == next_offset || offset >= self.glyf_data.len() {
            return None;
        }

        self.parse_glyph(offset, 0)
    }

    /// Get raw glyph data with contour structure and instructions for hinting.
    /// Returns points as a flat array with contour-end indices (not split into
    /// `Vec<Contour>`), plus the per-glyph instruction bytes.
    pub fn glyph_raw_data(&self, glyph_id: u16) -> Option<RawGlyphData> {
        let gid = glyph_id as usize;
        if gid >= self.num_glyphs as usize {
            return None;
        }

        let (offset, next_offset) = if self.loca_is_long {
            let base = gid * 4;
            if base + 8 > self.loca_data.len() {
                return None;
            }
            (
                u32(&self.loca_data, base) as usize,
                u32(&self.loca_data, base + 4) as usize,
            )
        } else {
            let base = gid * 2;
            if base + 4 > self.loca_data.len() {
                return None;
            }
            (
                u16(&self.loca_data, base) as usize * 2,
                u16(&self.loca_data, base + 2) as usize * 2,
            )
        };

        if offset == next_offset || offset >= self.glyf_data.len() {
            return None;
        }

        let data = &self.glyf_data;
        if offset + 10 > data.len() {
            return None;
        }
        let num_contours = i16(data, offset);
        let x_min = i16(data, offset + 2);
        let y_min = i16(data, offset + 4);

        // Only simple glyphs for now (compound glyph hinting is more complex).
        if num_contours < 0 {
            return None;
        }
        let nc = num_contours as usize;
        let aw = self.advance_width(glyph_id);
        let lsb = x_min; // approximation

        parse_simple_glyph_raw(data, offset + 10, nc, x_min, y_min, aw, lsb)
    }

    /// Parse a glyph at the given offset in the glyf table.
    /// `depth` guards against infinite recursion in compound glyphs.
    fn parse_glyph(&self, offset: usize, depth: u32) -> Option<GlyphOutline> {
        const MAX_COMPOUND_DEPTH: u32 = 16;
        if depth > MAX_COMPOUND_DEPTH {
            return None;
        }

        let data = &self.glyf_data;
        if offset + 10 > data.len() {
            return None;
        }

        let num_contours = i16(data, offset);
        let x_min = i16(data, offset + 2);
        let y_min = i16(data, offset + 4);
        let x_max = i16(data, offset + 6);
        let y_max = i16(data, offset + 8);
        let bounds = (x_min, y_min, x_max, y_max);

        if num_contours >= 0 {
            // Simple glyph.
            parse_simple_glyph(data, offset + 10, num_contours as usize, bounds)
        } else {
            // Compound glyph.
            self.parse_compound_glyph(data, offset + 10, bounds, depth)
        }
    }

    /// Parse a compound glyph (references to other glyphs with transforms).
    fn parse_compound_glyph(
        &self,
        data: &[u8],
        mut pos: usize,
        bounds: (i16, i16, i16, i16),
        depth: u32,
    ) -> Option<GlyphOutline> {
        let mut contours = Vec::new();

        loop {
            if pos + 4 > data.len() {
                break;
            }
            let flags = u16(data, pos);
            let glyph_index = u16(data, pos + 2);
            pos += 4;

            // Read translation offsets.
            let (dx, dy, bytes_read) = read_compound_offsets(data, pos, flags);
            pos += bytes_read;

            // Read scale/transform (we only use translation for now).
            let (sx, sy, bytes_read) = read_compound_scale(data, pos, flags);
            pos += bytes_read;

            // Recursively get the component glyph outline.
            if let Some(component) = self.glyph_outline_inner(glyph_index, depth + 1) {
                for contour in component.contours {
                    let transformed = Contour {
                        points: contour
                            .points
                            .iter()
                            .map(|p| OutlinePoint {
                                x: p.x * sx + dx,
                                y: p.y * sy + dy,
                                on_curve: p.on_curve,
                            })
                            .collect(),
                    };
                    contours.push(transformed);
                }
            }

            // MORE_COMPONENTS flag (bit 5).
            if flags & 0x0020 == 0 {
                break;
            }
        }

        if contours.is_empty() {
            None
        } else {
            Some(GlyphOutline {
                contours,
                bounds,
                stem_hints: StemHints::default(),
            })
        }
    }

    /// Internal helper that takes depth for compound recursion.
    fn glyph_outline_inner(&self, glyph_id: u16, depth: u32) -> Option<GlyphOutline> {
        let gid = glyph_id as usize;
        if gid >= self.num_glyphs as usize {
            return None;
        }

        let (offset, next_offset) = if self.loca_is_long {
            let base = gid * 4;
            if base + 8 > self.loca_data.len() {
                return None;
            }
            (
                u32(&self.loca_data, base) as usize,
                u32(&self.loca_data, base + 4) as usize,
            )
        } else {
            let base = gid * 2;
            if base + 4 > self.loca_data.len() {
                return None;
            }
            (
                u16(&self.loca_data, base) as usize * 2,
                u16(&self.loca_data, base + 2) as usize * 2,
            )
        };

        if offset == next_offset || offset >= self.glyf_data.len() {
            return None;
        }

        self.parse_glyph(offset, depth)
    }
}

// ---------------------------------------------------------------------------
// Simple glyph parsing
// ---------------------------------------------------------------------------

/// Parse a simple (non-compound) glyph from the glyf table.
fn parse_simple_glyph(
    data: &[u8],
    pos: usize,
    num_contours: usize,
    bounds: (i16, i16, i16, i16),
) -> Option<GlyphOutline> {
    if num_contours == 0 {
        return None;
    }

    // Read endPtsOfContours array.
    let end_pts_start = pos;
    let end_pts_end = end_pts_start + num_contours * 2;
    if end_pts_end > data.len() {
        return None;
    }

    let mut end_pts = Vec::with_capacity(num_contours);
    for i in 0..num_contours {
        end_pts.push(u16(data, end_pts_start + i * 2) as usize);
    }

    let total_points = match end_pts.last() {
        Some(&last) => last + 1,
        None => return None,
    };

    // Sanity limit: don't allocate huge buffers for malformed fonts.
    if total_points > 65536 {
        return None;
    }

    // Skip instruction length + instructions.
    let instr_len_pos = end_pts_end;
    if instr_len_pos + 2 > data.len() {
        return None;
    }
    let instruction_length = u16(data, instr_len_pos) as usize;
    let flags_start = instr_len_pos + 2 + instruction_length;
    if flags_start > data.len() {
        return None;
    }

    // Parse flags (run-length encoded).
    let mut flags = Vec::with_capacity(total_points);
    let mut fi = flags_start;
    while flags.len() < total_points {
        if fi >= data.len() {
            return None;
        }
        let flag = data[fi];
        fi += 1;
        flags.push(flag);

        // Repeat flag (bit 3).
        if flag & 0x08 != 0 {
            if fi >= data.len() {
                return None;
            }
            let repeat_count = data[fi] as usize;
            fi += 1;
            for _ in 0..repeat_count {
                if flags.len() >= total_points {
                    break;
                }
                flags.push(flag);
            }
        }
    }

    // Parse x coordinates.
    let mut x_coords = Vec::with_capacity(total_points);
    let mut x: i16 = 0;
    let mut xi = fi;
    for &flag in &flags {
        let x_short = flag & 0x02 != 0; // bit 1
        let x_same_or_positive = flag & 0x10 != 0; // bit 4

        if x_short {
            if xi >= data.len() {
                return None;
            }
            let dx = data[xi] as i16;
            xi += 1;
            x += if x_same_or_positive { dx } else { -dx };
        } else if !x_same_or_positive {
            if xi + 2 > data.len() {
                return None;
            }
            x += i16(data, xi);
            xi += 2;
        }
        // else: x_same_or_positive && !x_short -> x unchanged (delta = 0)
        x_coords.push(x);
    }

    // Parse y coordinates.
    let mut y_coords = Vec::with_capacity(total_points);
    let mut y: i16 = 0;
    let mut yi = xi;
    for &flag in &flags {
        let y_short = flag & 0x04 != 0; // bit 2
        let y_same_or_positive = flag & 0x20 != 0; // bit 5

        if y_short {
            if yi >= data.len() {
                return None;
            }
            let dy = data[yi] as i16;
            yi += 1;
            y += if y_same_or_positive { dy } else { -dy };
        } else if !y_same_or_positive {
            if yi + 2 > data.len() {
                return None;
            }
            y += i16(data, yi);
            yi += 2;
        }
        y_coords.push(y);
    }

    // Build contours from the parsed points.
    let mut contours = Vec::with_capacity(num_contours);
    let mut start = 0;
    for &end in &end_pts {
        if end >= total_points {
            break;
        }
        let mut points = Vec::with_capacity(end - start + 1);
        for j in start..=end {
            if j >= x_coords.len() || j >= y_coords.len() || j >= flags.len() {
                break;
            }
            points.push(OutlinePoint {
                x: x_coords[j] as f64,
                y: y_coords[j] as f64,
                on_curve: flags[j] & 0x01 != 0,
            });
        }
        if !points.is_empty() {
            contours.push(Contour { points });
        }
        start = end + 1;
    }

    if contours.is_empty() {
        None
    } else {
        Some(GlyphOutline {
            contours,
            bounds,
            stem_hints: StemHints::default(),
        })
    }
}

/// Parse a simple glyph preserving instruction bytes and flat point array.
/// Used by the hinting VM which needs contour-end indices and per-glyph instructions.
#[allow(clippy::too_many_arguments)]
fn parse_simple_glyph_raw(
    data: &[u8],
    pos: usize,
    num_contours: usize,
    x_min: i16,
    y_min: i16,
    advance_width: u16,
    lsb: i16,
) -> Option<RawGlyphData> {
    if num_contours == 0 {
        return None;
    }

    let end_pts_start = pos;
    let end_pts_end = end_pts_start + num_contours * 2;
    if end_pts_end > data.len() {
        return None;
    }

    let mut contour_ends = Vec::with_capacity(num_contours);
    for i in 0..num_contours {
        contour_ends.push(u16(data, end_pts_start + i * 2) as usize);
    }

    let total_points = match contour_ends.last() {
        Some(&last) => last + 1,
        None => return None,
    };
    if total_points > 65536 {
        return None;
    }

    // Extract instruction bytes (instead of skipping them).
    let instr_len_pos = end_pts_end;
    if instr_len_pos + 2 > data.len() {
        return None;
    }
    let instruction_length = u16(data, instr_len_pos) as usize;
    let instr_start = instr_len_pos + 2;
    let instr_end = instr_start + instruction_length;
    if instr_end > data.len() {
        return None;
    }
    let instructions = data[instr_start..instr_end].to_vec();

    let flags_start = instr_end;
    if flags_start > data.len() {
        return None;
    }

    // Parse flags (same as parse_simple_glyph).
    let mut flags = Vec::with_capacity(total_points);
    let mut fi = flags_start;
    while flags.len() < total_points {
        if fi >= data.len() {
            return None;
        }
        let flag = data[fi];
        fi += 1;
        flags.push(flag);
        if flag & 0x08 != 0 {
            if fi >= data.len() {
                return None;
            }
            let repeat_count = data[fi] as usize;
            fi += 1;
            for _ in 0..repeat_count {
                if flags.len() >= total_points {
                    break;
                }
                flags.push(flag);
            }
        }
    }

    // Parse x coordinates.
    let mut x_coords = Vec::with_capacity(total_points);
    let mut x: i16 = 0;
    let mut xi = fi;
    for &flag in &flags {
        let x_short = flag & 0x02 != 0;
        let x_same_or_positive = flag & 0x10 != 0;
        if x_short {
            if xi >= data.len() {
                return None;
            }
            let dx = data[xi] as i16;
            xi += 1;
            x += if x_same_or_positive { dx } else { -dx };
        } else if !x_same_or_positive {
            if xi + 2 > data.len() {
                return None;
            }
            x += i16(data, xi);
            xi += 2;
        }
        x_coords.push(x);
    }

    // Parse y coordinates.
    let mut y_coords = Vec::with_capacity(total_points);
    let mut y: i16 = 0;
    let mut yi = xi;
    for &flag in &flags {
        let y_short = flag & 0x04 != 0;
        let y_same_or_positive = flag & 0x20 != 0;
        if y_short {
            if yi >= data.len() {
                return None;
            }
            let dy = data[yi] as i16;
            yi += 1;
            y += if y_same_or_positive { dy } else { -dy };
        } else if !y_same_or_positive {
            if yi + 2 > data.len() {
                return None;
            }
            y += i16(data, yi);
            yi += 2;
        }
        y_coords.push(y);
    }

    // Build flat point array.
    let mut points = Vec::with_capacity(total_points);
    for j in 0..total_points {
        if j >= x_coords.len() || j >= y_coords.len() || j >= flags.len() {
            break;
        }
        points.push(OutlinePoint {
            x: x_coords[j] as f64,
            y: y_coords[j] as f64,
            on_curve: flags[j] & 0x01 != 0,
        });
    }

    if points.is_empty() {
        return None;
    }

    Some(RawGlyphData {
        points,
        contour_ends,
        instructions,
        advance_width,
        lsb,
        x_min,
        y_min,
    })
}

// ---------------------------------------------------------------------------
// Compound glyph helpers
// ---------------------------------------------------------------------------

/// Read translation offsets from a compound glyph component.
/// Returns (dx, dy, bytes_consumed).
fn read_compound_offsets(data: &[u8], pos: usize, flags: u16) -> (f64, f64, usize) {
    let arg1_and_2_are_words = flags & 0x0001 != 0;
    let args_are_xy_values = flags & 0x0002 != 0;

    if arg1_and_2_are_words {
        if pos + 4 > data.len() {
            return (0.0, 0.0, 0);
        }
        let a = i16(data, pos) as f64;
        let b = i16(data, pos + 2) as f64;
        if args_are_xy_values {
            (a, b, 4)
        } else {
            (0.0, 0.0, 4) // point indices, not offsets
        }
    } else {
        if pos + 2 > data.len() {
            return (0.0, 0.0, 0);
        }
        let a = data[pos] as i8 as f64;
        let b = data[pos + 1] as i8 as f64;
        if args_are_xy_values {
            (a, b, 2)
        } else {
            (0.0, 0.0, 2)
        }
    }
}

/// Read scale/transform from a compound glyph component.
/// Returns (scale_x, scale_y, bytes_consumed).
fn read_compound_scale(data: &[u8], pos: usize, flags: u16) -> (f64, f64, usize) {
    let we_have_a_scale = flags & 0x0008 != 0;
    let we_have_an_xy_scale = flags & 0x0040 != 0;
    let we_have_a_two_by_two = flags & 0x0080 != 0;

    if we_have_a_scale {
        if pos + 2 > data.len() {
            return (1.0, 1.0, 0);
        }
        let s = f2dot14(data, pos);
        (s, s, 2)
    } else if we_have_an_xy_scale {
        if pos + 4 > data.len() {
            return (1.0, 1.0, 0);
        }
        let sx = f2dot14(data, pos);
        let sy = f2dot14(data, pos + 2);
        (sx, sy, 4)
    } else if we_have_a_two_by_two {
        if pos + 8 > data.len() {
            return (1.0, 1.0, 0);
        }
        // 2x2 matrix: we only extract the diagonal (scale) for simplicity.
        let sx = f2dot14(data, pos);
        let sy = f2dot14(data, pos + 6);
        (sx, sy, 8)
    } else {
        (1.0, 1.0, 0)
    }
}

// ---------------------------------------------------------------------------
// cmap subtable lookups
// ---------------------------------------------------------------------------

/// Look up a character in a cmap format 4 subtable (BMP characters).
fn cmap_format4_lookup(data: &[u8], offset: usize, ch: u16) -> Option<u16> {
    if offset + 14 > data.len() {
        return None;
    }
    let seg_count = u16(data, offset + 6) as usize / 2;
    let end_codes_start = offset + 14;
    let start_codes_start = end_codes_start + seg_count * 2 + 2; // +2 for reservedPad
    let deltas_start = start_codes_start + seg_count * 2;
    let offsets_start = deltas_start + seg_count * 2;

    for seg in 0..seg_count {
        let end_code = u16(data, end_codes_start + seg * 2);
        if ch > end_code {
            continue;
        }
        let start_code = u16(data, start_codes_start + seg * 2);
        if ch < start_code {
            return Some(0); // .notdef
        }
        let delta = i16(data, deltas_start + seg * 2);
        let range_offset_pos = offsets_start + seg * 2;
        if range_offset_pos + 2 > data.len() {
            return None;
        }
        let range_offset = u16(data, range_offset_pos);

        if range_offset == 0 {
            // Direct: glyph = char + delta
            return Some((ch as i32 + delta as i32) as u16);
        }
        // Indirect: look up in glyphIdArray.
        let glyph_offset =
            range_offset_pos + range_offset as usize + (ch - start_code) as usize * 2;
        if glyph_offset + 2 > data.len() {
            return None;
        }
        let glyph = u16(data, glyph_offset);
        if glyph == 0 {
            return Some(0);
        }
        return Some((glyph as i32 + delta as i32) as u16);
    }
    Some(0) // .notdef
}

/// Look up a character in a cmap format 12 subtable (full Unicode).
fn cmap_format12_lookup(data: &[u8], offset: usize, ch: u32) -> Option<u16> {
    if offset + 16 > data.len() {
        return None;
    }
    // Cap n_groups at the bound enforced by the data itself (each group is 12
    // bytes; we cannot read more groups than the table can physically hold).
    // Without this, a u32 n_groups field of 4 billion drives a 4 billion-iter
    // loop even though the inner bounds check stops actual reads -- 4B-iter
    // hot loop on every cmap lookup is a CPU DoS vector ( round-3 audit).
    let declared_n_groups = u32(data, offset + 12) as usize;
    let groups_start = offset + 16;
    let max_groups = data.len().saturating_sub(groups_start) / 12;
    let n_groups = declared_n_groups.min(max_groups);

    for i in 0..n_groups {
        let group_offset = groups_start + i * 12;
        if group_offset + 12 > data.len() {
            break;
        }
        let start = u32(data, group_offset);
        let end = u32(data, group_offset + 4);
        let start_glyph = u32(data, group_offset + 8);

        if ch < start {
            return Some(0); // .notdef (groups are sorted)
        }
        if ch <= end {
            let gid = start_glyph + (ch - start);
            return Some(gid.min(0xFFFF) as u16);
        }
    }
    Some(0)
}

// ---------------------------------------------------------------------------
// Binary reading helpers
// ---------------------------------------------------------------------------

/// Read a big-endian u16 from a byte slice.
#[inline]
fn u16(data: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([data[offset], data[offset + 1]])
}

/// Read a big-endian i16 from a byte slice.
#[inline]
fn i16(data: &[u8], offset: usize) -> i16 {
    i16::from_be_bytes([data[offset], data[offset + 1]])
}

/// Read a big-endian u32 from a byte slice.
#[inline]
fn u32(data: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// Read a 2.14 fixed-point number (F2Dot14) from a byte slice.
#[inline]
fn f2dot14(data: &[u8], offset: usize) -> f64 {
    let raw = i16(data, offset);
    raw as f64 / 16384.0
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid TrueType font with the required tables.
    /// Contains a single glyph (glyph 0 = .notdef, glyph 1 = simple triangle).
    fn build_minimal_ttf() -> Vec<u8> {
        let mut buf = Vec::new();

        // Table directory: sfVersion + numTables(7) + searchRange + entrySelector + rangeShift
        let num_tables: u16 = 7;
        buf.extend_from_slice(&0x00010000u32.to_be_bytes()); // sfVersion
        buf.extend_from_slice(&num_tables.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes()); // searchRange (unused)
        buf.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
        buf.extend_from_slice(&0u16.to_be_bytes()); // rangeShift

        // We'll build tables and fill in the directory after.
        // Table record entries: tag(4) + checksum(4) + offset(4) + length(4) = 16 bytes each
        let dir_end = 12 + num_tables as usize * 16; // 12 + 112 = 124

        // Build each table and track offsets.
        struct Tab {
            tag: [u8; 4],
            data: Vec<u8>,
        }
        let mut tables: Vec<Tab> = Vec::new();

        // head table (54 bytes minimum)
        let mut head = vec![0u8; 54];
        // version = 1.0
        head[0..4].copy_from_slice(&0x00010000u32.to_be_bytes());
        // unitsPerEm = 1000
        head[18..20].copy_from_slice(&1000u16.to_be_bytes());
        // indexToLocFormat = 0 (short)
        head[50..52].copy_from_slice(&0i16.to_be_bytes());
        tables.push(Tab {
            tag: *b"head",
            data: head,
        });

        // hhea table (36 bytes)
        let mut hhea = vec![0u8; 36];
        hhea[0..4].copy_from_slice(&0x00010000u32.to_be_bytes());
        // numberOfHMetrics = 2
        hhea[34..36].copy_from_slice(&2u16.to_be_bytes());
        tables.push(Tab {
            tag: *b"hhea",
            data: hhea,
        });

        // maxp table (6 bytes for version 0.5)
        let mut maxp = vec![0u8; 6];
        maxp[0..4].copy_from_slice(&0x00005000u32.to_be_bytes()); // version 0.5
        maxp[4..6].copy_from_slice(&2u16.to_be_bytes()); // numGlyphs = 2
        tables.push(Tab {
            tag: *b"maxp",
            data: maxp,
        });

        // hmtx table: 2 entries * 4 bytes = 8 bytes
        let mut hmtx = Vec::new();
        // glyph 0: advance=500, lsb=0
        hmtx.extend_from_slice(&500u16.to_be_bytes());
        hmtx.extend_from_slice(&0i16.to_be_bytes());
        // glyph 1: advance=600, lsb=50
        hmtx.extend_from_slice(&600u16.to_be_bytes());
        hmtx.extend_from_slice(&50i16.to_be_bytes());
        tables.push(Tab {
            tag: *b"hmtx",
            data: hmtx,
        });

        // cmap table: format 4 with one segment mapping char 65 ('A') -> glyph 1
        let mut cmap = Vec::new();
        cmap.extend_from_slice(&0u16.to_be_bytes()); // version
        cmap.extend_from_slice(&1u16.to_be_bytes()); // numTables = 1
                                                     // Subtable record: platform=3, encoding=1, offset=12
        cmap.extend_from_slice(&3u16.to_be_bytes());
        cmap.extend_from_slice(&1u16.to_be_bytes());
        cmap.extend_from_slice(&12u32.to_be_bytes());
        // Format 4 subtable at offset 12
        let seg_count: u16 = 2; // one real segment + sentinel 0xFFFF
        cmap.extend_from_slice(&4u16.to_be_bytes()); // format
        cmap.extend_from_slice(&0u16.to_be_bytes()); // length (unused in our parser)
        cmap.extend_from_slice(&0u16.to_be_bytes()); // language
        cmap.extend_from_slice(&(seg_count * 2).to_be_bytes()); // segCountX2
        cmap.extend_from_slice(&0u16.to_be_bytes()); // searchRange
        cmap.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
        cmap.extend_from_slice(&0u16.to_be_bytes()); // rangeShift
                                                     // endCode array
        cmap.extend_from_slice(&65u16.to_be_bytes()); // segment 0 end = 'A'
        cmap.extend_from_slice(&0xFFFFu16.to_be_bytes()); // sentinel
                                                          // reservedPad
        cmap.extend_from_slice(&0u16.to_be_bytes());
        // startCode array
        cmap.extend_from_slice(&65u16.to_be_bytes()); // segment 0 start = 'A'
        cmap.extend_from_slice(&0xFFFFu16.to_be_bytes());
        // idDelta array
        // delta = glyph_id - char_code = 1 - 65 = -64
        cmap.extend_from_slice(&(-64i16).to_be_bytes());
        cmap.extend_from_slice(&1i16.to_be_bytes()); // sentinel delta
                                                     // idRangeOffset array
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&0u16.to_be_bytes());
        tables.push(Tab {
            tag: *b"cmap",
            data: cmap,
        });

        // loca table (short format): 3 entries for 2 glyphs (each u16, offset/2)
        let mut loca = Vec::new();
        // glyph 0 starts at glyf offset 0 (-> loca value 0)
        loca.extend_from_slice(&0u16.to_be_bytes());
        // glyph 1 starts after glyph 0. We'll make glyph 0 empty (offset 0 == offset 0).
        // Actually let's make glyph 0 have zero length (empty .notdef)
        // glyph 0: offset 0, glyph 1: offset 0 (glyph 0 is empty)
        loca.extend_from_slice(&0u16.to_be_bytes()); // glyph 1 offset/2 = 0
                                                     // end sentinel
        let glyph1_end: u16 = 14; // glyph 1 is 28 bytes, 28/2 = 14
        loca.extend_from_slice(&glyph1_end.to_be_bytes());
        tables.push(Tab {
            tag: *b"loca",
            data: loca,
        });

        // glyf table: glyph 1 = simple triangle (3 points, 1 contour)
        let mut glyf = Vec::new();
        // numberOfContours = 1
        glyf.extend_from_slice(&1i16.to_be_bytes());
        // xMin, yMin, xMax, yMax
        glyf.extend_from_slice(&0i16.to_be_bytes());
        glyf.extend_from_slice(&0i16.to_be_bytes());
        glyf.extend_from_slice(&500i16.to_be_bytes());
        glyf.extend_from_slice(&700i16.to_be_bytes());
        // endPtsOfContours[0] = 2 (3 points: 0, 1, 2)
        glyf.extend_from_slice(&2u16.to_be_bytes());
        // instructionLength = 0
        glyf.extend_from_slice(&0u16.to_be_bytes());
        // flags: 3 points, all on-curve (flag = 0x01), no repeats
        glyf.push(0x01); // point 0: on-curve
        glyf.push(0x01); // point 1: on-curve
        glyf.push(0x01); // point 2: on-curve
                         // x coordinates (deltas): 0, 500, -250 (using i16 format, bit1=0, bit4=0)
                         // point 0: x=0 (delta=0, flag x_same_or_positive=0, x_short=0 -> 2-byte delta)
                         // Actually, let's use short coordinates for simplicity
                         // Rewrite flags to use short positive x deltas
        let glyf_len = glyf.len();
        glyf[glyf_len - 3] = 0x01 | 0x02 | 0x10; // on-curve, x-short, x-positive
        glyf[glyf_len - 2] = 0x01 | 0x02 | 0x10 | 0x04 | 0x20; // on-curve, x-short+pos, y-short+pos
        glyf[glyf_len - 1] = 0x01 | 0x04 | 0x20; // on-curve, y-short+pos (x uses 2-byte)
                                                 // x deltas: point 0 = 0 (short positive), point 1 = 500-0=500... too big for u8
                                                 // Let's use simpler coordinates. Use 2-byte deltas.
                                                 // Reset flags: all use 2-byte x and y (simplest)
        glyf[glyf_len - 3] = 0x01; // on-curve, 2-byte x delta, 2-byte y delta
        glyf[glyf_len - 2] = 0x01;
        glyf[glyf_len - 1] = 0x01;
        // x deltas (i16): 0, 500, -500
        glyf.extend_from_slice(&0i16.to_be_bytes());
        glyf.extend_from_slice(&500i16.to_be_bytes());
        glyf.extend_from_slice(&(-500i16).to_be_bytes());
        // y deltas (i16): 0, 700, -700
        glyf.extend_from_slice(&0i16.to_be_bytes());
        glyf.extend_from_slice(&700i16.to_be_bytes());
        glyf.extend_from_slice(&(-700i16).to_be_bytes());

        // Update loca for actual glyph 1 size
        let glyf_len_actual = glyf.len();
        let loca_idx = tables.len() - 1; // loca is second to last
        tables[loca_idx].data[4..6].copy_from_slice(&((glyf_len_actual / 2) as u16).to_be_bytes());

        tables.push(Tab {
            tag: *b"glyf",
            data: glyf,
        });

        // Now build the final buffer: directory + tables with 4-byte alignment.
        // First pass: compute offsets.
        let mut current_offset = dir_end;
        let mut table_offsets = Vec::new();
        for t in &tables {
            table_offsets.push(current_offset);
            current_offset += (t.data.len() + 3) & !3; // 4-byte align
        }

        // Build directory entries.
        for (i, t) in tables.iter().enumerate() {
            buf.extend_from_slice(&t.tag);
            buf.extend_from_slice(&0u32.to_be_bytes()); // checksum (unused)
            buf.extend_from_slice(&(table_offsets[i] as u32).to_be_bytes());
            buf.extend_from_slice(&(t.data.len() as u32).to_be_bytes());
        }

        // Pad to dir_end.
        while buf.len() < dir_end {
            buf.push(0);
        }

        // Write table data.
        for t in &tables {
            buf.extend_from_slice(&t.data);
            // Pad to 4-byte alignment.
            while buf.len() % 4 != 0 {
                buf.push(0);
            }
        }

        buf
    }

    #[test]
    fn parse_minimal_ttf() {
        let data = build_minimal_ttf();
        let font = TrueTypeFont::from_bytes(&data).expect("should parse minimal TTF");
        assert_eq!(font.units_per_em(), 1000);
        assert_eq!(font.num_glyphs(), 2);
    }

    #[test]
    fn glyph_id_lookup() {
        let data = build_minimal_ttf();
        let font = TrueTypeFont::from_bytes(&data).expect("should parse");
        // 'A' should map to glyph 1
        assert_eq!(font.glyph_id('A'), Some(1));
        // 'B' should map to .notdef (0) or None
        let gid = font.glyph_id('B');
        assert!(gid.is_none() || gid == Some(0));
    }

    #[test]
    fn advance_width_lookup() {
        let data = build_minimal_ttf();
        let font = TrueTypeFont::from_bytes(&data).expect("should parse");
        assert_eq!(font.advance_width(0), 500);
        assert_eq!(font.advance_width(1), 600);
    }

    #[test]
    fn glyph_outline_simple() {
        let data = build_minimal_ttf();
        let font = TrueTypeFont::from_bytes(&data).expect("should parse");

        // Glyph 0 is empty (space-like).
        assert!(font.glyph_outline(0).is_none());

        // Glyph 1 should have a triangle outline.
        let outline = font.glyph_outline(1).expect("glyph 1 should have outline");
        assert_eq!(outline.contours.len(), 1);
        assert_eq!(outline.contours[0].points.len(), 3);

        // Verify triangle points: (0,0), (500,700), (0,0) -- cumulative deltas
        let pts = &outline.contours[0].points;
        assert_eq!(pts[0].x, 0.0);
        assert_eq!(pts[0].y, 0.0);
        assert_eq!(pts[1].x, 500.0);
        assert_eq!(pts[1].y, 700.0);
        assert_eq!(pts[2].x, 0.0); // 500 + (-500) = 0
        assert_eq!(pts[2].y, 0.0); // 700 + (-700) = 0
        assert!(pts[0].on_curve);
    }

    #[test]
    fn reject_too_short() {
        assert!(TrueTypeFont::from_bytes(&[0; 5]).is_err());
    }

    #[test]
    fn reject_cff_magic() {
        let mut data = vec![0u8; 100];
        data[0..4].copy_from_slice(b"OTTO");
        assert!(TrueTypeFont::from_bytes(&data).is_err());
    }

    #[test]
    fn glyph_id_symbol_cmap() {
        // Build a TTF where the cmap is (3,0) Microsoft Symbol with codepoint
        // 0xF041 -> glyph 1. Looking up Unicode 'A' (0x41) should succeed via
        // the Symbol-cmap retry path that prefixes with 0xF000. Mirrors the
        // pdfTeX subset embedding convention exposed by Round 2C.
        let mut data = build_minimal_ttf();
        // Find the cmap table directory entry and rewrite encoding 1 -> 0
        // and the segment startCode/endCode 65 -> 0xF041.
        let n_tables = u16(&data, 4) as usize;
        for i in 0..n_tables {
            let rec = 12 + i * 16;
            if &data[rec..rec + 4] == b"cmap" {
                let cmap_off = u32(&data, rec + 8) as usize;
                // Subtable record at cmap_off + 4: platform(2) + encoding(2) + offset(4)
                // Set encoding to 0 (Symbol).
                data[cmap_off + 4 + 2..cmap_off + 4 + 4].copy_from_slice(&0u16.to_be_bytes());
                let sub_off = cmap_off + u32(&data, cmap_off + 4 + 4) as usize;
                // Format-4 layout: format(2) length(2) lang(2) segCountX2(2)
                //                  searchRange(2) entrySelector(2) rangeShift(2)
                //                  endCode[seg_count](2*sc) reservedPad(2)
                //                  startCode[seg_count](2*sc) idDelta[seg_count](2*sc)
                let seg_count_x2 = u16(&data, sub_off + 6) as usize;
                let seg_count = seg_count_x2 / 2;
                let header = 14;
                let end_off = sub_off + header;
                // First segment is the meaningful one. Rewrite endCode[0] = 0xF041,
                // startCode[0] = 0xF041, idDelta[0] = 1 - 0xF041.
                let new_cp: u16 = 0xF041;
                data[end_off..end_off + 2].copy_from_slice(&new_cp.to_be_bytes());
                let start_off = end_off + 2 * seg_count + 2;
                data[start_off..start_off + 2].copy_from_slice(&new_cp.to_be_bytes());
                let delta_off = start_off + 2 * seg_count;
                let new_delta = (1i32 - new_cp as i32) as i16;
                data[delta_off..delta_off + 2].copy_from_slice(&new_delta.to_be_bytes());
                break;
            }
        }
        let font = TrueTypeFont::from_bytes(&data).expect("should parse");
        // Direct lookup at 0xF041 should also work (format4 path).
        assert_eq!(font.glyph_id('\u{F041}'), Some(1));
        // Symbol-cmap retry: ASCII 'A' (0x41) finds 0xF041 via prefix.
        assert_eq!(font.glyph_id('A'), Some(1));
    }

    #[test]
    fn out_of_range_glyph() {
        let data = build_minimal_ttf();
        let font = TrueTypeFont::from_bytes(&data).expect("should parse");
        assert!(font.glyph_outline(999).is_none());
        assert_eq!(font.advance_width(999), 600); // falls back to last metric
    }
}
