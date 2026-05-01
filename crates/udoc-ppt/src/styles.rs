//! StyleTextPropAtom parser for PPT binary files.
//!
//! Parses paragraph and character formatting runs from the StyleTextPropAtom
//! record (recType 0x0FA1). This atom follows a text atom and describes
//! per-character and per-paragraph styling.
//!
//! We use a pragmatic approach: paragraph properties are skipped (only their
//! byte sizes are accounted for), and character properties are parsed for
//! bold, italic, font size, and font index.

use std::sync::Arc;

use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use crate::error::{Error, Result, ResultExt};
use crate::MAX_STYLE_RUNS;

/// A character-level style run from StyleTextPropAtom.
#[derive(Debug, Clone, Default)]
pub struct CharStyleRun {
    /// Number of characters this run covers.
    pub char_count: u32,
    /// Whether text is bold (None if not specified in this run).
    pub bold: Option<bool>,
    /// Whether text is italic (None if not specified in this run).
    pub italic: Option<bool>,
    /// Font size in points (None if not specified in this run).
    pub font_size_pt: Option<f64>,
    /// Font index into the font collection (None if not specified).
    pub font_index: Option<u16>,
}

/// Parsed result of a StyleTextPropAtom.
#[derive(Debug, Clone)]
pub struct TextStyle {
    /// Character-level style runs. Each run describes formatting for
    /// `char_count` consecutive characters.
    pub char_runs: Vec<CharStyleRun>,
}

/// A simple cursor into a byte slice, tracking position.
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn read_u16(&mut self) -> Result<u16> {
        if self.remaining() < 2 {
            return Err(Error::new(format!(
                "unexpected end of data at offset {}: need 2 bytes, have {}",
                self.pos,
                self.remaining()
            )));
        }
        let val = u16::from_le_bytes([self.data[self.pos], self.data[self.pos + 1]]);
        self.pos += 2;
        Ok(val)
    }

    fn read_u32(&mut self) -> Result<u32> {
        if self.remaining() < 4 {
            return Err(Error::new(format!(
                "unexpected end of data at offset {}: need 4 bytes, have {}",
                self.pos,
                self.remaining()
            )));
        }
        let val = u32::from_le_bytes([
            self.data[self.pos],
            self.data[self.pos + 1],
            self.data[self.pos + 2],
            self.data[self.pos + 3],
        ]);
        self.pos += 4;
        Ok(val)
    }

    fn skip(&mut self, n: usize) -> Result<()> {
        if self.remaining() < n {
            return Err(Error::new(format!(
                "unexpected end of data at offset {}: need to skip {} bytes, have {}",
                self.pos,
                n,
                self.remaining()
            )));
        }
        self.pos += n;
        Ok(())
    }
}

/// Skip paragraph properties based on the paragraph format mask.
///
/// Each set bit indicates a property is present and must be skipped.
/// The sizes vary by property; see the MS-PPT spec for StyleTextPropAtom.
fn skip_paragraph_properties(
    cursor: &mut Cursor,
    pf_mask: u32,
    diag: &Arc<dyn DiagnosticsSink>,
) -> Result<()> {
    // Bits 0-3: if ANY of these are set, a u16 flags field is present
    if pf_mask & 0x000F != 0 {
        cursor.skip(2).context("skipping paragraph flags")?;
    }

    // Bit 4: bulletChar (u16)
    if pf_mask & (1 << 4) != 0 {
        cursor.skip(2).context("skipping bulletChar")?;
    }

    // Bit 5: bulletFontRef (u16)
    if pf_mask & (1 << 5) != 0 {
        cursor.skip(2).context("skipping bulletFontRef")?;
    }

    // Bit 6: bulletSize (u16)
    if pf_mask & (1 << 6) != 0 {
        cursor.skip(2).context("skipping bulletSize")?;
    }

    // Bit 7: bulletColor (u32)
    if pf_mask & (1 << 7) != 0 {
        cursor.skip(4).context("skipping bulletColor")?;
    }

    // Bit 8: alignment (u16)
    if pf_mask & (1 << 8) != 0 {
        cursor.skip(2).context("skipping alignment")?;
    }

    // Bit 9: lineSpacing (u16)
    if pf_mask & (1 << 9) != 0 {
        cursor.skip(2).context("skipping lineSpacing")?;
    }

    // Bit 10: spaceBefore (u16)
    if pf_mask & (1 << 10) != 0 {
        cursor.skip(2).context("skipping spaceBefore")?;
    }

    // Bit 11: spaceAfter (u16)
    if pf_mask & (1 << 11) != 0 {
        cursor.skip(2).context("skipping spaceAfter")?;
    }

    // Bit 12: leftMargin (u16)
    if pf_mask & (1 << 12) != 0 {
        cursor.skip(2).context("skipping leftMargin")?;
    }

    // Bit 13: indent (u16)
    if pf_mask & (1 << 13) != 0 {
        cursor.skip(2).context("skipping indent")?;
    }

    // Bit 14: defaultTabSize (u16)
    if pf_mask & (1 << 14) != 0 {
        cursor.skip(2).context("skipping defaultTabSize")?;
    }

    // Bit 15: tabStops -- variable length (u16 count, then count * 4 bytes)
    if pf_mask & (1 << 15) != 0 {
        let tab_count = cursor.read_u16().context("reading tabStops count")? as usize;
        let tab_bytes = tab_count
            .checked_mul(4)
            .ok_or_else(|| Error::new(format!("tabStops byte count overflow: {tab_count} tabs")))?;
        cursor.skip(tab_bytes).context("skipping tabStops data")?;
    }

    // Bit 16: fontAlign (u16)
    if pf_mask & (1 << 16) != 0 {
        cursor.skip(2).context("skipping fontAlign")?;
    }

    // Bits 17-18: wrapFlags (u16 if either set)
    if pf_mask & (0x3 << 17) != 0 {
        cursor.skip(2).context("skipping wrapFlags")?;
    }

    // Bit 19: textDirection (u16)
    if pf_mask & (1 << 19) != 0 {
        cursor.skip(2).context("skipping textDirection")?;
    }

    // Bits 20+: unknown. Warn and try to skip u16 per unknown bit.
    let unknown_mask = pf_mask & !((1 << 20) - 1);
    if unknown_mask != 0 {
        let unknown_count = unknown_mask.count_ones();
        diag.warning(Warning::new(
            "UnknownParagraphMaskBits",
            format!(
                "paragraph mask has {unknown_count} unknown bits set (mask=0x{pf_mask:08X}), skipping as u16 each"
            ),
        ));
        for bit in 20..32 {
            if pf_mask & (1 << bit) != 0 {
                cursor
                    .skip(2)
                    .context("skipping unknown paragraph property")?;
            }
        }
    }

    Ok(())
}

/// Parse character properties from the character format mask.
///
/// Returns a `CharStyleRun` with the properties we care about extracted.
fn parse_character_properties(
    cursor: &mut Cursor,
    cf_mask: u32,
    diag: &Arc<dyn DiagnosticsSink>,
) -> Result<CharStyleRun> {
    let mut run = CharStyleRun::default();

    // Bits 0-3: if ANY of these are set, a u16 flags field is present
    if cf_mask & 0x000F != 0 {
        let flags = cursor.read_u16().context("reading character flags")?;
        run.bold = Some(flags & 0x0001 != 0);
        run.italic = Some(flags & 0x0002 != 0);
    }

    // Bit 4: fontRef (u16) -- font index
    if cf_mask & (1 << 4) != 0 {
        let font_index = cursor.read_u16().context("reading fontRef")?;
        run.font_index = Some(font_index);
    }

    // Bit 5: oldFontRef (u16) -- skip
    if cf_mask & (1 << 5) != 0 {
        cursor.skip(2).context("skipping oldFontRef")?;
    }

    // Bit 6: ansiFont (u16) -- skip
    if cf_mask & (1 << 6) != 0 {
        cursor.skip(2).context("skipping ansiFont")?;
    }

    // Bit 7: fontSymbol (u16) -- skip
    if cf_mask & (1 << 7) != 0 {
        cursor.skip(2).context("skipping fontSymbol")?;
    }

    // Bit 8: fontSize (u16, in half-points)
    if cf_mask & (1 << 8) != 0 {
        let half_pts = cursor.read_u16().context("reading fontSize")?;
        run.font_size_pt = Some(half_pts as f64 / 2.0);
    }

    // Bit 9: fontColor (u32)
    if cf_mask & (1 << 9) != 0 {
        cursor.skip(4).context("skipping fontColor")?;
    }

    // Bit 10: position (u16) -- superscript/subscript offset
    if cf_mask & (1 << 10) != 0 {
        cursor.skip(2).context("skipping position")?;
    }

    // Bits 11+: unknown. Warn and skip u16 per unknown bit.
    let unknown_mask = cf_mask & !((1 << 11) - 1);
    if unknown_mask != 0 {
        let unknown_count = unknown_mask.count_ones();
        diag.warning(Warning::new(
            "UnknownCharacterMaskBits",
            format!(
                "character mask has {unknown_count} unknown bits set (mask=0x{cf_mask:08X}), skipping as u16 each"
            ),
        ));
        for bit in 11..32 {
            if cf_mask & (1 << bit) != 0 {
                cursor
                    .skip(2)
                    .context("skipping unknown character property")?;
            }
        }
    }

    Ok(run)
}

/// Parse a StyleTextPropAtom's raw data bytes.
///
/// `data` is the atom's data (after the 8-byte record header).
/// `text_length` is the character count of the preceding text atom.
///
/// Returns character-level style runs with bold/italic/fontSize/fontIndex
/// extracted. Paragraph properties are skipped.
pub fn parse_style_text_prop(
    data: &[u8],
    text_length: usize,
    diag: &Arc<dyn DiagnosticsSink>,
) -> Result<TextStyle> {
    if data.is_empty() {
        return Ok(TextStyle {
            char_runs: Vec::new(),
        });
    }

    let mut cursor = Cursor::new(data);

    // skip paragraph runs.
    // Each paragraph run: u32 charCount + u16 indentLevel + u32 pfMask + properties.
    // The total charCount across all paragraph runs should equal text_length (+1 for CR
    // that PPT implicitly appends, but the style runs include it in their count).
    let mut para_chars = 0u64;
    let mut para_run_count = 0usize;

    while para_chars < text_length as u64 {
        if cursor.remaining() < 10 {
            // Not enough data for another paragraph run (u32 + u16 + u32 = 10 bytes)
            diag.warning(Warning::new(
                "TruncatedParagraphRuns",
                format!(
                    "paragraph runs truncated at offset {}: covered {para_chars}/{text_length} chars, {} bytes remaining",
                    cursor.pos,
                    cursor.remaining()
                ),
            ));
            break;
        }

        para_run_count += 1;
        if para_run_count > MAX_STYLE_RUNS {
            diag.warning(Warning::new(
                "TooManyParagraphRuns",
                format!("exceeded {MAX_STYLE_RUNS} paragraph runs, stopping"),
            ));
            break;
        }

        let char_count = cursor.read_u32().context("reading paragraph charCount")?;
        let _indent_level = cursor.read_u16().context("reading paragraph indentLevel")?;
        let pf_mask = cursor.read_u32().context("reading paragraph pfMask")?;

        if pf_mask != 0 {
            skip_paragraph_properties(&mut cursor, pf_mask, diag)
                .context("skipping paragraph properties")?;
        }

        para_chars = para_chars.saturating_add(char_count as u64);
    }

    if para_chars != text_length as u64 {
        diag.warning(Warning::new(
            "ParagraphCharCountMismatch",
            format!("paragraph runs cover {para_chars} chars, expected {text_length}"),
        ));
    }

    // parse character runs.
    let mut char_runs = Vec::new();
    let mut char_chars = 0u64;

    while char_chars < text_length as u64 {
        if cursor.remaining() < 8 {
            // Not enough data for another char run (u32 + u32 = 8 bytes minimum)
            if char_runs.is_empty() && cursor.remaining() == 0 {
                // No character runs at all is normal for unstyled text
                break;
            }
            diag.warning(Warning::new(
                "TruncatedCharacterRuns",
                format!(
                    "character runs truncated at offset {}: covered {char_chars}/{text_length} chars, {} bytes remaining",
                    cursor.pos,
                    cursor.remaining()
                ),
            ));
            break;
        }

        if char_runs.len() >= MAX_STYLE_RUNS {
            diag.warning(Warning::new(
                "TooManyCharacterRuns",
                format!("exceeded {MAX_STYLE_RUNS} character runs, stopping"),
            ));
            break;
        }

        let char_count = cursor
            .read_u32()
            .context("reading character run charCount")?;
        let cf_mask = cursor.read_u32().context("reading character run cfMask")?;

        let mut run = if cf_mask != 0 {
            parse_character_properties(&mut cursor, cf_mask, diag)
                .context("parsing character properties")?
        } else {
            CharStyleRun::default()
        };

        run.char_count = char_count;
        char_chars = char_chars.saturating_add(char_count as u64);
        char_runs.push(run);
    }

    if !char_runs.is_empty() && char_chars != text_length as u64 {
        diag.warning(Warning::new(
            "CharRunCountMismatch",
            format!("character runs cover {char_chars} chars, expected {text_length}"),
        ));
    }

    Ok(TextStyle { char_runs })
}

/// Split text into chunks aligned to character style runs.
///
/// `char_count` in each run is in UTF-16 code units, not Unicode scalars,
/// so this function counts code units when advancing through the text.
///
/// Returns `(styled_chunks, remainder)` where each styled chunk is paired
/// with its `CharStyleRun`, and `remainder` is any text beyond the runs'
/// coverage (which can happen when char_runs undercount).
pub fn split_text_by_runs<'a>(
    text: &str,
    runs: &'a [CharStyleRun],
) -> (Vec<(String, &'a CharStyleRun)>, String) {
    let mut chunks = Vec::with_capacity(runs.len());
    let mut char_iter = text.chars();

    for run in runs {
        let target = run.char_count as usize;
        let mut chunk = String::new();
        let mut consumed = 0usize;
        for ch in char_iter.by_ref() {
            consumed += ch.len_utf16();
            chunk.push(ch);
            if consumed >= target {
                break;
            }
        }
        if !chunk.is_empty() {
            chunks.push((chunk, run));
        }
    }

    let remainder: String = char_iter.collect();
    (chunks, remainder)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use udoc_core::diagnostics::{CollectingDiagnostics, DiagnosticsSink, NullDiagnostics};

    use super::*;

    fn null_diag() -> Arc<dyn DiagnosticsSink> {
        Arc::new(NullDiagnostics)
    }

    /// Helper: build a raw paragraph run with no properties.
    /// Layout: u32 charCount + u16 indentLevel(0) + u32 pfMask(0)
    fn para_run_plain(char_count: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&char_count.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // indentLevel = 0
        buf.extend_from_slice(&0u32.to_le_bytes()); // pfMask = 0
        buf
    }

    /// Helper: build a character run with the given mask and property bytes.
    fn char_run(char_count: u32, cf_mask: u32, properties: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&char_count.to_le_bytes());
        buf.extend_from_slice(&cf_mask.to_le_bytes());
        buf.extend_from_slice(properties);
        buf
    }

    #[test]
    fn empty_data_returns_empty_runs() {
        let result = parse_style_text_prop(&[], 0, &null_diag()).unwrap();
        assert!(result.char_runs.is_empty());
    }

    #[test]
    fn bold_only() {
        // Text "Hello" + CR = 6 chars
        let text_len = 6;
        let mut data = Vec::new();

        // Paragraph run: 6 chars, no properties
        data.extend_from_slice(&para_run_plain(text_len as u32));

        // Character run: 6 chars, bit 0 set (flags present), flags = 0x0001 (bold)
        let flags_bytes = 0x0001u16.to_le_bytes();
        data.extend_from_slice(&char_run(text_len as u32, 0x0000_0001, &flags_bytes));

        let result = parse_style_text_prop(&data, text_len, &null_diag()).unwrap();
        assert_eq!(result.char_runs.len(), 1);
        assert_eq!(result.char_runs[0].char_count, 6);
        assert_eq!(result.char_runs[0].bold, Some(true));
        assert_eq!(result.char_runs[0].italic, Some(false));
        assert_eq!(result.char_runs[0].font_size_pt, None);
        assert_eq!(result.char_runs[0].font_index, None);
    }

    #[test]
    fn italic_only() {
        let text_len = 6;
        let mut data = Vec::new();

        // Paragraph run
        data.extend_from_slice(&para_run_plain(text_len as u32));

        // Character run: bit 1 set (flags present), flags = 0x0002 (italic)
        let flags_bytes = 0x0002u16.to_le_bytes();
        data.extend_from_slice(&char_run(text_len as u32, 0x0000_0002, &flags_bytes));

        let result = parse_style_text_prop(&data, text_len, &null_diag()).unwrap();
        assert_eq!(result.char_runs.len(), 1);
        assert_eq!(result.char_runs[0].bold, Some(false));
        assert_eq!(result.char_runs[0].italic, Some(true));
    }

    #[test]
    fn bold_italic_and_font_size() {
        let text_len = 6;
        let mut data = Vec::new();

        // Paragraph run
        data.extend_from_slice(&para_run_plain(text_len as u32));

        // Character run: bits 0 (flags) + bit 8 (fontSize)
        // cf_mask = 0x0000_0101
        let cf_mask: u32 = 0x0001 | (1 << 8);
        let mut props = Vec::new();
        props.extend_from_slice(&0x0003u16.to_le_bytes()); // flags: bold + italic
        props.extend_from_slice(&48u16.to_le_bytes()); // fontSize: 48 half-points = 24pt

        data.extend_from_slice(&char_run(text_len as u32, cf_mask, &props));

        let result = parse_style_text_prop(&data, text_len, &null_diag()).unwrap();
        assert_eq!(result.char_runs.len(), 1);
        let run = &result.char_runs[0];
        assert_eq!(run.bold, Some(true));
        assert_eq!(run.italic, Some(true));
        assert_eq!(run.font_size_pt, Some(24.0));
    }

    #[test]
    fn multiple_character_runs() {
        // "Hello World" + CR = 12 chars. First 6 bold, next 6 italic.
        let text_len = 12;
        let mut data = Vec::new();

        // Paragraph run covering all 12 chars
        data.extend_from_slice(&para_run_plain(text_len as u32));

        // Char run 1: 6 chars, bold
        let bold_flags = 0x0001u16.to_le_bytes();
        data.extend_from_slice(&char_run(6, 0x0000_0001, &bold_flags));

        // Char run 2: 6 chars, italic
        let italic_flags = 0x0002u16.to_le_bytes();
        data.extend_from_slice(&char_run(6, 0x0000_0002, &italic_flags));

        let result = parse_style_text_prop(&data, text_len, &null_diag()).unwrap();
        assert_eq!(result.char_runs.len(), 2);

        assert_eq!(result.char_runs[0].char_count, 6);
        assert_eq!(result.char_runs[0].bold, Some(true));
        assert_eq!(result.char_runs[0].italic, Some(false));

        assert_eq!(result.char_runs[1].char_count, 6);
        assert_eq!(result.char_runs[1].bold, Some(false));
        assert_eq!(result.char_runs[1].italic, Some(true));
    }

    #[test]
    fn char_count_exceeds_text_length_warns() {
        // text_length=6 but char run claims 20 chars
        let text_len = 6;
        let mut data = Vec::new();

        // Paragraph run: claim 6 chars (correct)
        data.extend_from_slice(&para_run_plain(text_len as u32));

        // Character run: claim 20 chars (too many)
        let flags_bytes = 0x0001u16.to_le_bytes();
        data.extend_from_slice(&char_run(20, 0x0000_0001, &flags_bytes));

        let collecting = Arc::new(CollectingDiagnostics::new());
        let diag: Arc<dyn DiagnosticsSink> = collecting.clone();
        let result = parse_style_text_prop(&data, text_len, &diag).unwrap();

        // Should still return the run, just with a warning
        assert_eq!(result.char_runs.len(), 1);
        assert_eq!(result.char_runs[0].char_count, 20);

        let warnings = collecting.warnings();
        assert!(
            warnings.iter().any(|w| w.kind == "CharRunCountMismatch"),
            "expected CharRunCountMismatch warning, got: {warnings:?}"
        );
    }

    #[test]
    fn font_index_extracted() {
        let text_len = 6;
        let mut data = Vec::new();

        // Paragraph run
        data.extend_from_slice(&para_run_plain(text_len as u32));

        // Character run: bit 4 (fontRef)
        let cf_mask: u32 = 1 << 4;
        let font_ref = 42u16.to_le_bytes();
        data.extend_from_slice(&char_run(text_len as u32, cf_mask, &font_ref));

        let result = parse_style_text_prop(&data, text_len, &null_diag()).unwrap();
        assert_eq!(result.char_runs.len(), 1);
        assert_eq!(result.char_runs[0].font_index, Some(42));
        assert_eq!(result.char_runs[0].bold, None);
        assert_eq!(result.char_runs[0].italic, None);
    }

    #[test]
    fn all_character_properties() {
        // Exercise all known character mask bits to make sure we skip correctly
        let text_len = 6;
        let mut data = Vec::new();

        // Paragraph run
        data.extend_from_slice(&para_run_plain(text_len as u32));

        // Character run: all bits 0-10 set
        let cf_mask: u32 = (1 << 11) - 1; // bits 0-10
        let mut props = Vec::new();
        props.extend_from_slice(&0x0003u16.to_le_bytes()); // flags (bits 0-3): bold+italic
        props.extend_from_slice(&7u16.to_le_bytes()); // bit 4: fontRef = 7
        props.extend_from_slice(&0u16.to_le_bytes()); // bit 5: oldFontRef (skip)
        props.extend_from_slice(&0u16.to_le_bytes()); // bit 6: ansiFont (skip)
        props.extend_from_slice(&0u16.to_le_bytes()); // bit 7: fontSymbol (skip)
        props.extend_from_slice(&36u16.to_le_bytes()); // bit 8: fontSize = 36 half-pts = 18pt
        props.extend_from_slice(&0xFFu32.to_le_bytes()); // bit 9: fontColor (skip, u32)
        props.extend_from_slice(&0u16.to_le_bytes()); // bit 10: position (skip)

        data.extend_from_slice(&char_run(text_len as u32, cf_mask, &props));

        let result = parse_style_text_prop(&data, text_len, &null_diag()).unwrap();
        assert_eq!(result.char_runs.len(), 1);
        let run = &result.char_runs[0];
        assert_eq!(run.bold, Some(true));
        assert_eq!(run.italic, Some(true));
        assert_eq!(run.font_index, Some(7));
        assert_eq!(run.font_size_pt, Some(18.0));
    }

    #[test]
    fn paragraph_properties_skipped_correctly() {
        // Paragraph with alignment (bit 8) set, followed by a bold character run.
        // Verifies that paragraph property skipping advances the cursor correctly
        // so the character run is parsed at the right offset.
        let text_len = 6;
        let mut data = Vec::new();

        // Paragraph run: 6 chars, pfMask with bit 8 (alignment)
        data.extend_from_slice(&(text_len as u32).to_le_bytes()); // charCount
        data.extend_from_slice(&0u16.to_le_bytes()); // indentLevel
        let pf_mask: u32 = 1 << 8; // alignment
        data.extend_from_slice(&pf_mask.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes()); // alignment value (center=1)

        // Character run: bold
        let flags_bytes = 0x0001u16.to_le_bytes();
        data.extend_from_slice(&char_run(text_len as u32, 0x0000_0001, &flags_bytes));

        let result = parse_style_text_prop(&data, text_len, &null_diag()).unwrap();
        assert_eq!(result.char_runs.len(), 1);
        assert_eq!(result.char_runs[0].bold, Some(true));
    }

    #[test]
    fn paragraph_with_bullet_properties() {
        // Paragraph with flags (bits 0-3), bulletChar (bit 4), bulletColor (bit 7)
        let text_len = 6;
        let mut data = Vec::new();

        // Paragraph run
        data.extend_from_slice(&(text_len as u32).to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes()); // indentLevel
        let pf_mask: u32 = 0x0001 | (1 << 4) | (1 << 7); // flags + bulletChar + bulletColor
        data.extend_from_slice(&pf_mask.to_le_bytes());
        data.extend_from_slice(&0x000Fu16.to_le_bytes()); // flags value
        data.extend_from_slice(&0x2022u16.to_le_bytes()); // bulletChar (bullet)
        data.extend_from_slice(&0x00FF0000u32.to_le_bytes()); // bulletColor (red)

        // Character run: no formatting
        data.extend_from_slice(&char_run(text_len as u32, 0, &[]));

        let result = parse_style_text_prop(&data, text_len, &null_diag()).unwrap();
        assert_eq!(result.char_runs.len(), 1);
        assert_eq!(result.char_runs[0].char_count, 6);
        assert_eq!(result.char_runs[0].bold, None);
    }

    #[test]
    fn truncated_data_returns_partial_result() {
        // Build data that cuts off mid-character-run
        let text_len = 12;
        let mut data = Vec::new();

        // Paragraph run
        data.extend_from_slice(&para_run_plain(text_len as u32));

        // First char run: complete (bold, 6 chars)
        let flags_bytes = 0x0001u16.to_le_bytes();
        data.extend_from_slice(&char_run(6, 0x0000_0001, &flags_bytes));

        // Second char run: truncated -- only write charCount, no mask
        data.extend_from_slice(&6u32.to_le_bytes());
        // Missing cf_mask and properties

        let collecting = Arc::new(CollectingDiagnostics::new());
        let diag: Arc<dyn DiagnosticsSink> = collecting.clone();
        let result = parse_style_text_prop(&data, text_len, &diag).unwrap();

        // Should have the first complete run
        assert_eq!(result.char_runs.len(), 1);
        assert_eq!(result.char_runs[0].bold, Some(true));

        let warnings = collecting.warnings();
        assert!(
            warnings.iter().any(|w| w.kind == "TruncatedCharacterRuns"),
            "expected TruncatedCharacterRuns warning, got: {warnings:?}"
        );
    }

    #[test]
    fn no_style_runs_for_zero_length_text() {
        // text_length=0 should immediately return empty
        let data = Vec::new();
        let result = parse_style_text_prop(&data, 0, &null_diag()).unwrap();
        assert!(result.char_runs.is_empty());
    }

    #[test]
    fn split_text_by_runs_basic() {
        let runs = vec![
            CharStyleRun {
                char_count: 5,
                bold: Some(true),
                ..Default::default()
            },
            CharStyleRun {
                char_count: 6,
                italic: Some(true),
                ..Default::default()
            },
        ];
        let (chunks, remainder) = split_text_by_runs("Hello World", &runs);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].0, "Hello");
        assert!(chunks[0].1.bold.unwrap());
        assert_eq!(chunks[1].0, " World");
        assert!(chunks[1].1.italic.unwrap());
        assert!(remainder.is_empty());
    }

    #[test]
    fn split_text_by_runs_with_remainder() {
        let runs = vec![CharStyleRun {
            char_count: 3,
            ..Default::default()
        }];
        let (chunks, remainder) = split_text_by_runs("Hello", &runs);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].0, "Hel");
        assert_eq!(remainder, "lo");
    }

    #[test]
    fn split_text_by_runs_utf16_surrogates() {
        // U+1F600 (grinning face) = 2 UTF-16 code units
        let text = "A\u{1F600}B";
        let runs = vec![
            CharStyleRun {
                char_count: 3, // 'A' (1) + emoji (2) = 3 UTF-16 code units
                bold: Some(true),
                ..Default::default()
            },
            CharStyleRun {
                char_count: 1, // 'B' (1)
                ..Default::default()
            },
        ];
        let (chunks, remainder) = split_text_by_runs(text, &runs);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].0, "A\u{1F600}");
        assert_eq!(chunks[1].0, "B");
        assert!(remainder.is_empty());
    }

    #[test]
    fn split_text_by_runs_empty() {
        let (chunks, remainder) = split_text_by_runs("", &[]);
        assert!(chunks.is_empty());
        assert!(remainder.is_empty());
    }
}
