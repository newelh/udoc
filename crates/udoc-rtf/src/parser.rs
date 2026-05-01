//! RTF parser that interprets lexer tokens into document content.
//!
//! Maintains a group stack with `ParserState` and dispatches control
//! words to build text, tables, and images.

use std::collections::HashMap;
use std::sync::Arc;

use udoc_core::convert::twips_to_points;

use crate::codepage::{
    encoding_for_ansicpg, encoding_for_charset, is_approximate_codepage, CodepageDecoder,
};
use crate::error::Result;
use crate::hex_val;
use crate::lexer::{Lexer, Token};
use crate::state::{Alignment, Destination, FontEntry, FontFamily, InfoField, ParserState};

/// Maximum nesting depth before we bail out.
const MAX_DEPTH: usize = udoc_core::MAX_NESTING_DEPTH;
/// Maximum hex bytes in an image before we stop accumulating (100 MB decoded).
const MAX_IMAGE_HEX_BYTES: usize = 200_000_000;
/// Maximum number of images per document to prevent memory exhaustion.
const MAX_IMAGES: usize = 1000;
/// Stop accumulating warnings after this many to bound memory usage.
const MAX_WARNINGS: usize = 1000;
/// Maximum color table entries to prevent unbounded allocation from malicious RTF.
const MAX_COLOR_TABLE_ENTRIES: usize = 10_000;
/// Maximum URL length from HYPERLINK field instructions to prevent memory exhaustion.
const MAX_URL_LENGTH: usize = 65_536;

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// A text run with formatting info.
#[derive(Debug, Clone)]
pub struct TextRun {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub superscript: bool,
    pub subscript: bool,
    pub invisible: bool,
    /// Font name, shared via Arc to avoid per-run cloning during parsing.
    pub font_name: Option<Arc<str>>,
    pub font_size_pts: f64,
    /// Foreground color from color table, None means auto/default.
    pub color: Option<[u8; 3]>,
    /// Background color (\highlight takes precedence over \cb), None means auto/default.
    pub bg_color: Option<[u8; 3]>,
    /// Hyperlink URL from a HYPERLINK field instruction, if this run is inside
    /// a `\field{\*\fldinst HYPERLINK "..."}{\fldrslt ...}` group.
    pub hyperlink_url: Option<String>,
}

/// Parsed content from an RTF document.
#[derive(Debug)]
pub struct ParsedDocument {
    pub paragraphs: Vec<Paragraph>,
    pub tables: Vec<ParsedTable>,
    pub images: Vec<ParsedImage>,
    pub metadata: DocumentInfo,
    #[allow(dead_code)]
    // Font names already wired per-run via font_name_cache; Vec used in tests only
    pub fonts: Vec<FontEntry>,
    /// Color table parsed from the document. Index 0 is auto (None).
    pub color_table: Vec<Option<[u8; 3]>>,
    /// Warnings accumulated during parsing (malformed input, skipped data, etc.).
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Paragraph {
    pub runs: Vec<TextRun>,
    /// Paragraph alignment.
    pub alignment: Option<Alignment>,
    /// Space before paragraph in points.
    pub space_before: Option<f64>,
    /// Space after paragraph in points.
    pub space_after: Option<f64>,
    /// Left indent in points.
    pub indent_left: Option<f64>,
    /// Right indent in points.
    pub indent_right: Option<f64>,
    /// First line indent in points.
    /// Parsed from \fi but not forwarded to the Document model because
    /// BlockLayout has no first_line_indent field. Add when the model
    /// supports it.
    pub first_line_indent: Option<f64>,
}

#[derive(Debug)]
pub struct ParsedTable {
    pub rows: Vec<ParsedTableRow>,
}

#[derive(Debug)]
pub struct ParsedTableRow {
    pub cells: Vec<ParsedTableCell>,
    #[allow(dead_code)] // Twip column positions; RTF tables have no bbox in the core model
    pub cell_boundaries: Vec<u32>,
    pub merge_flags: Vec<CellMerge>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CellMerge {
    None,
    First,
    Continue,
}

#[derive(Debug)]
pub struct ParsedTableCell {
    pub runs: Vec<TextRun>,
}

#[derive(Debug)]
pub struct ParsedImage {
    pub format: ImageFormat,
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Display-intent width in twips (\picwgoal). Used as last-resort
    /// fallback for pixel dimensions when \picw and image headers are absent.
    pub goal_width: u32,
    /// Display-intent height in twips (\pichgoal). Used as last-resort
    /// fallback for pixel dimensions when \pich and image headers are absent.
    pub goal_height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Emf,
    Wmf,
    Unknown,
}

#[derive(Debug, Default)]
pub struct DocumentInfo {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
}

// ---------------------------------------------------------------------------
// Image builder (accumulates hex data inside \pict groups)
// ---------------------------------------------------------------------------

struct ImageBuilder {
    format: ImageFormat,
    width: u32,
    height: u32,
    goal_width: u32,
    goal_height: u32,
    hex_buf: Vec<u8>,
}

impl ImageBuilder {
    fn new() -> Self {
        Self {
            format: ImageFormat::Unknown,
            width: 0,
            height: 0,
            goal_width: 0,
            goal_height: 0,
            hex_buf: Vec::new(),
        }
    }

    fn push_hex(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if b.is_ascii_hexdigit() && self.hex_buf.len() < MAX_IMAGE_HEX_BYTES {
                self.hex_buf.push(b);
            }
        }
    }

    fn finalize(self) -> ParsedImage {
        let data = decode_hex(&self.hex_buf);
        ParsedImage {
            format: self.format,
            data,
            width: self.width,
            height: self.height,
            goal_width: self.goal_width,
            goal_height: self.goal_height,
        }
    }
}

fn decode_hex(hex: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(hex.len() / 2 + 1);
    let mut i = 0;
    while i + 1 < hex.len() {
        if let (Some(hi), Some(lo)) = (hex_val(hex[i]), hex_val(hex[i + 1])) {
            out.push((hi << 4) | lo);
        }
        i += 2;
    }
    // Trailing odd nibble: pad with zero (e.g. "A" -> 0xA0).
    if hex.len() % 2 == 1 {
        if let Some(hi) = hex_val(hex[hex.len() - 1]) {
            out.push(hi << 4);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// RTF highlight palette (fixed 16-color mapping for \highlightN)
// ---------------------------------------------------------------------------

/// Resolve a `\highlightN` index to an RGB triple using the fixed RTF palette.
///
/// Unlike `\cb` which indexes the document's color table, `\highlight` uses a
/// hardcoded 16-color palette defined by the RTF spec. Index 0 means "no
/// highlight" (auto/default), and unknown indices also return None.
fn resolve_highlight(index: u16) -> Option<[u8; 3]> {
    match index {
        0 => None,                   // auto / no highlight
        1 => Some([0, 0, 0]),        // black
        2 => Some([0, 0, 255]),      // blue
        3 => Some([0, 255, 255]),    // cyan
        4 => Some([0, 255, 0]),      // green
        5 => Some([255, 0, 255]),    // magenta
        6 => Some([255, 0, 0]),      // red
        7 => Some([255, 255, 0]),    // yellow
        8 => Some([255, 255, 255]),  // white (unused slot)
        9 => Some([0, 0, 128]),      // dark blue
        10 => Some([0, 128, 128]),   // dark cyan
        11 => Some([0, 128, 0]),     // dark green
        12 => Some([128, 0, 128]),   // dark magenta
        13 => Some([128, 0, 0]),     // dark red
        14 => Some([128, 128, 0]),   // dark yellow
        15 => Some([128, 128, 128]), // dark gray
        16 => Some([192, 192, 192]), // light gray
        _ => None,                   // unknown
    }
}

// ---------------------------------------------------------------------------
// Table builder (accumulates rows/cells while in table mode)
// ---------------------------------------------------------------------------

struct TableRowBuilder {
    cell_boundaries: Vec<u32>,
    merge_flags: Vec<CellMerge>,
    cells: Vec<ParsedTableCell>,
    current_cell_runs: Vec<TextRun>,
    current_merge: CellMerge,
}

impl TableRowBuilder {
    fn new() -> Self {
        Self {
            cell_boundaries: Vec::new(),
            merge_flags: Vec::new(),
            cells: Vec::new(),
            current_cell_runs: Vec::new(),
            current_merge: CellMerge::None,
        }
    }

    fn add_cellx(&mut self, twips: u32) {
        self.cell_boundaries.push(twips);
    }

    fn finalize_cell(&mut self) {
        let runs = std::mem::take(&mut self.current_cell_runs);
        self.cells.push(ParsedTableCell { runs });
        self.merge_flags.push(self.current_merge);
        self.current_merge = CellMerge::None;
    }

    fn finalize_row(mut self) -> ParsedTableRow {
        // If there are pending runs that weren't closed by \cell, flush them.
        if !self.current_cell_runs.is_empty() {
            self.finalize_cell();
        }
        ParsedTableRow {
            cells: self.cells,
            cell_boundaries: self.cell_boundaries,
            merge_flags: self.merge_flags,
        }
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

pub struct Parser<'a> {
    lexer: Lexer<'a>,
    state_stack: Vec<ParserState>,
    state: ParserState,
    decoder: CodepageDecoder,
    fonts: Vec<FontEntry>,
    // O(1) lookup: font index -> position in fonts Vec
    font_index_map: HashMap<usize, usize>,
    // Cached Arc<str> font names for cheap cloning into TextRuns
    font_name_cache: HashMap<usize, Arc<str>>,
    // Font table parsing state
    current_font_entry: Option<FontEntry>,
    // Color table
    color_table: Vec<Option<[u8; 3]>>,
    // Color table parsing state (pending RGB components)
    pending_red: Option<u8>,
    pending_green: Option<u8>,
    pending_blue: Option<u8>,
    // Ignorable destination tracking
    ignore_depth: usize,
    star_pending: bool,
    skip_after_unicode: usize,
    // Count of group opens beyond MAX_DEPTH (not saved on state_stack)
    over_limit_depth: usize,
    // Document-level encoding
    default_encoding: &'static encoding_rs::Encoding,
    // Default font index
    default_font: usize,
    // Nesting depth limit
    depth: usize,
    // Accumulated content
    paragraphs: Vec<Paragraph>,
    current_runs: Vec<TextRun>,
    // Paragraph-level formatting (NOT group-scoped, persists until \pard).
    // RTF paragraph properties apply to the paragraph being built, not to
    // the enclosing group. They survive group push/pop.
    para_alignment: Option<Alignment>,
    para_space_before: Option<f64>,
    para_space_after: Option<f64>,
    para_indent_left: Option<f64>,
    para_indent_right: Option<f64>,
    para_first_line_indent: Option<f64>,
    // Table state
    tables: Vec<ParsedTable>,
    current_table_rows: Vec<ParsedTableRow>,
    row_builder: Option<TableRowBuilder>,
    // Image state
    images: Vec<ParsedImage>,
    image_builder: Option<ImageBuilder>,
    // Metadata
    metadata: DocumentInfo,
    info_text_buf: String,
    // Hyperlink field state
    // Buffer for accumulating text inside \fldinst groups
    field_inst_buf: String,
    // URL extracted from the most recent HYPERLINK field instruction.
    // Set when \fldinst group closes, cleared when the outer \field group closes.
    pending_hyperlink: Option<String>,
    // Depth at which the outermost \field group was entered (0 = not in a field).
    field_depth: usize,
    // Warnings accumulated during parsing
    warnings: Vec<String>,
}

impl<'a> Parser<'a> {
    /// Parse RTF data and return the extracted document.
    pub fn parse(data: &[u8]) -> Result<ParsedDocument> {
        let mut parser = Parser {
            lexer: Lexer::new(data),
            state_stack: Vec::new(),
            state: ParserState::default(),
            decoder: CodepageDecoder::new(encoding_rs::WINDOWS_1252),
            fonts: Vec::new(),
            font_index_map: HashMap::new(),
            font_name_cache: HashMap::new(),
            current_font_entry: None,
            color_table: Vec::new(),
            pending_red: None,
            pending_green: None,
            pending_blue: None,
            ignore_depth: 0,
            star_pending: false,
            skip_after_unicode: 0,
            over_limit_depth: 0,
            default_encoding: encoding_rs::WINDOWS_1252,
            default_font: 0,
            depth: 0,
            paragraphs: Vec::new(),
            current_runs: Vec::new(),
            para_alignment: None,
            para_space_before: None,
            para_space_after: None,
            para_indent_left: None,
            para_indent_right: None,
            para_first_line_indent: None,
            tables: Vec::new(),
            current_table_rows: Vec::new(),
            row_builder: None,
            images: Vec::new(),
            image_builder: None,
            metadata: DocumentInfo::default(),
            info_text_buf: String::new(),
            field_inst_buf: String::new(),
            pending_hyperlink: None,
            field_depth: 0,
            warnings: Vec::new(),
        };

        parser.run()?;

        // Warn about unclosed groups at EOF.
        if parser.depth > 0 {
            parser.warn(format!(
                "{} unclosed group(s) at end of document",
                parser.depth
            ));
        }

        // Flush any remaining text into a final paragraph.
        parser.flush_paragraph();

        // If there are accumulated table rows without a closing \row, flush.
        parser.flush_pending_table();

        Ok(ParsedDocument {
            paragraphs: parser.paragraphs,
            tables: parser.tables,
            images: parser.images,
            metadata: parser.metadata,
            fonts: parser.fonts,
            color_table: parser.color_table,
            warnings: parser.warnings,
        })
    }

    fn run(&mut self) -> Result<()> {
        loop {
            let token = match self.lexer.next_token() {
                Ok(Some(t)) => t,
                Ok(None) => break,
                Err(e) => {
                    // Tolerate lexer errors: log a warning and keep going.
                    // The lexer already advanced past the problematic input.
                    self.warn(format!("lexer error at offset {}: {e}", self.lexer.pos()));
                    continue;
                }
            };

            match token {
                Token::GroupOpen => self.handle_group_open(),
                Token::GroupClose => self.handle_group_close(),
                Token::ControlSymbol(ch) => self.handle_control_symbol(ch),
                Token::ControlWord { name, param } => self.handle_control_word(name, param)?,
                Token::HexEscape(byte) => self.handle_hex_escape(byte),
                Token::Text(bytes) => self.handle_text(bytes),
                Token::BinaryData(_) => {
                    // Binary data counts as 1 character for unicode skip.
                    if self.skip_after_unicode > 0 {
                        self.skip_after_unicode -= 1;
                    }
                }
            }
        }

        // Flush any remaining bytes in the decoder at EOF.
        self.flush_decoder();

        Ok(())
    }

    // -- Group handling -----------------------------------------------------

    fn handle_group_open(&mut self) {
        // Flush accumulated text before entering child group, so it's
        // emitted with the parent group's formatting and destination.
        if self.ignore_depth == 0 {
            self.flush_decoder();
        }

        self.depth += 1;

        if self.depth > MAX_DEPTH {
            // Beyond the limit: track separately, don't push state.
            self.over_limit_depth += 1;
            if self.ignore_depth == 0 {
                self.ignore_depth = 1;
            } else {
                self.ignore_depth += 1;
            }
            return;
        }

        // Save skip_after_unicode on the stack by resetting it here.
        // The RTF spec says \ucN skip counts must not leak across groups.
        self.state_stack.push(self.state.clone());

        if self.ignore_depth > 0 {
            self.ignore_depth += 1;
        }
    }

    fn handle_group_close(&mut self) {
        // Flush any accumulated text before restoring parent state,
        // so the text is emitted with the current group's formatting.
        if self.ignore_depth == 0 {
            self.flush_decoder();
        }

        // ignore_depth tracks how many nested groups we're skipping.
        // It's set to 1 when we enter an unknown \* destination or exceed
        // MAX_DEPTH, then incremented on each nested GroupOpen within the
        // ignored region. Decrementing here is safe: we only increment
        // ignore_depth when it's already > 0 (in handle_group_open), so
        // it can't underflow past the group where ignoring started.
        if self.ignore_depth > 0 {
            self.ignore_depth -= 1;
        }

        self.depth = self.depth.saturating_sub(1);

        // If we're closing an over-limit group, just decrement the counter
        // and return. We never pushed state for these, so don't pop.
        if self.over_limit_depth > 0 {
            self.over_limit_depth -= 1;
            return;
        }

        let popping_dest = self.state.destination;

        // Flush any pending color entry when leaving the ColorTable group.
        // Handles malformed RTF where the last entry lacks a trailing ';'.
        if popping_dest == Destination::ColorTable
            && (self.pending_red.is_some()
                || self.pending_green.is_some()
                || self.pending_blue.is_some())
        {
            self.push_color_entry();
        }

        // Finalize font entry when leaving FontTable group.
        if popping_dest == Destination::FontTable {
            self.finalize_font_entry();
        }

        // Finalize image when leaving Pict group.
        if popping_dest == Destination::Pict {
            self.finalize_image();
        }

        // Finalize metadata field when leaving InfoField group.
        if let Destination::InfoField(field) = popping_dest {
            self.finalize_info_field(field);
        }

        // Only pop if we actually pushed (depth was within limit).
        // Reset skip_after_unicode since the skip count from a \uN in the
        // child group must not leak to the parent.
        if let Some(prev) = self.state_stack.pop() {
            self.state = prev;
        }
        self.skip_after_unicode = 0;

        // Parse HYPERLINK URL from accumulated field instruction text.
        // Only finalize when we're leaving FieldInst entirely (the parent
        // destination is NOT FieldInst), so nested groups within \fldinst
        // don't trigger premature parsing.
        if popping_dest == Destination::FieldInst
            && self.state.destination != Destination::FieldInst
        {
            self.finalize_field_inst();
        }

        // Clear pending hyperlink when leaving the outer \field group.
        // After decrement, self.depth < self.field_depth means we've
        // exited the group where \field was encountered.
        if self.field_depth > 0 && self.depth < self.field_depth {
            self.pending_hyperlink = None;
            self.field_depth = 0;
        }
    }

    // -- Control symbol handling --------------------------------------------

    fn handle_control_symbol(&mut self, ch: u8) {
        if self.ignore_depth > 0 && ch != b'*' {
            return;
        }

        // Per RTF spec, control symbols count as one "character" for
        // the \ucN skip count (except \*).
        if ch != b'*' && self.skip_after_unicode > 0 {
            self.skip_after_unicode -= 1;
            return;
        }

        match ch {
            b'*' => {
                self.star_pending = true;
            }
            b'~' => {
                // Non-breaking space
                self.flush_decoder();
                self.emit_text("\u{00A0}");
            }
            b'-' => {
                // Soft hyphen (discard)
            }
            b'_' => {
                // Non-breaking hyphen
                self.flush_decoder();
                self.emit_text("\u{2011}");
            }
            b'{' => {
                self.flush_decoder();
                self.emit_text("{");
            }
            b'}' => {
                self.flush_decoder();
                self.emit_text("}");
            }
            b'\\' => {
                self.flush_decoder();
                self.emit_text("\\");
            }
            // \<CR> and \<LF> are paragraph breaks (equivalent to \par).
            // Common in macOS TextEdit RTF output.
            0x0A | 0x0D => {
                self.flush_decoder();
                self.finish_paragraph();
            }
            _ => {}
        }
    }

    // -- Control word dispatch ----------------------------------------------

    fn handle_control_word(&mut self, name: &str, param: Option<i32>) -> Result<()> {
        // Handle \* marking: if star was pending and we don't recognize the
        // destination, mark it as Unknown.
        let was_star = self.star_pending;
        self.star_pending = false;

        if self.ignore_depth > 0 {
            return Ok(());
        }

        // Per RTF spec, control words count as one "character" for the
        // \ucN skip count after \uN. The \u word itself is exempt (it
        // sets the counter), as is \uc (which adjusts the skip count).
        if self.skip_after_unicode > 0 && name != "u" && name != "uc" {
            self.skip_after_unicode -= 1;
            return Ok(());
        }

        match name {
            // Document header
            "rtf" => {}
            "ansi" => {
                self.default_encoding = encoding_rs::WINDOWS_1252;
                self.decoder.set_encoding(self.default_encoding);
            }
            "mac" => {
                self.default_encoding = encoding_rs::MACINTOSH;
                self.decoder.set_encoding(self.default_encoding);
            }
            "pc" => {
                // CP437 (OEM US). encoding_rs doesn't have CP437; WINDOWS_1252 is
                // the closest approximation for the printable ASCII range.
                self.default_encoding = encoding_rs::WINDOWS_1252;
                self.decoder.set_encoding(self.default_encoding);
            }
            "pca" => {
                // RTF spec says PC-850, but encoding_rs has no CP850.
                // WINDOWS_1252 is the closest available approximation.
                self.warn(
                    "\\pca encoding (CP-850) not available, using Windows-1252 approximation"
                        .into(),
                );
                self.default_encoding = encoding_rs::WINDOWS_1252;
                self.decoder.set_encoding(self.default_encoding);
            }
            "ansicpg" => {
                if let Some(cpg) = param {
                    if cpg > 0 && cpg <= u16::MAX as i32 {
                        // Flush accumulated bytes before switching encoding so they
                        // are decoded with the previous encoding, not the new one.
                        self.flush_decoder();
                        let cpg = cpg as u16;
                        if is_approximate_codepage(cpg) {
                            self.warn(format!(
                                "\\ansicpg{cpg} not available, using Windows-1252 approximation"
                            ));
                        }
                        let enc = encoding_for_ansicpg(cpg);
                        self.default_encoding = enc;
                        self.decoder.set_encoding(enc);
                    }
                }
            }
            "deff" => {
                if let Some(idx) = param {
                    let idx = idx.max(0) as usize;
                    self.default_font = idx;
                    self.state.font_index = idx;
                }
            }

            // Font table
            "fonttbl" => {
                self.state.destination = Destination::FontTable;
            }
            "f" => {
                let idx = param.unwrap_or(0).max(0) as usize;
                if self.state.destination == Destination::FontTable {
                    // Finalize previous entry if any, then start new.
                    self.finalize_font_entry();
                    self.current_font_entry = Some(FontEntry {
                        index: idx,
                        name: String::new(),
                        charset: 0,
                        family: FontFamily::Nil,
                    });
                } else {
                    // Body: switch current font.
                    self.flush_decoder();
                    self.state.font_index = idx;
                    self.update_decoder_for_font(idx);
                }
            }
            "fcharset" => {
                if let Some(cs) = param {
                    if let Some(ref mut entry) = self.current_font_entry {
                        entry.charset = cs.clamp(0, 255) as u8;
                    }
                }
            }
            "froman" => self.set_font_family(FontFamily::Roman),
            "fswiss" => self.set_font_family(FontFamily::Swiss),
            "fmodern" => self.set_font_family(FontFamily::Modern),
            "fscript" => self.set_font_family(FontFamily::Script),
            "fdecor" => self.set_font_family(FontFamily::Decor),
            "ftech" => self.set_font_family(FontFamily::Tech),
            "fnil" => self.set_font_family(FontFamily::Nil),
            "fbidi" => self.set_font_family(FontFamily::Bidi),

            // Color table
            "colortbl" => {
                self.state.destination = Destination::ColorTable;
            }
            "red" => {
                if self.state.destination == Destination::ColorTable {
                    self.pending_red = Some(param.unwrap_or(0).clamp(0, 255) as u8);
                }
            }
            "green" => {
                if self.state.destination == Destination::ColorTable {
                    self.pending_green = Some(param.unwrap_or(0).clamp(0, 255) as u8);
                }
            }
            "blue" => {
                if self.state.destination == Destination::ColorTable {
                    self.pending_blue = Some(param.unwrap_or(0).clamp(0, 255) as u8);
                }
            }
            // Foreground color index
            "cf" => {
                self.flush_decoder();
                self.state.color_index = Some(param.unwrap_or(0).max(0) as usize);
            }
            // Background color index
            "cb" => {
                self.flush_decoder();
                self.state.bg_color_index = Some(param.unwrap_or(0).max(0) as usize);
            }
            "highlight" => {
                self.flush_decoder();
                self.state.highlight_color = resolve_highlight(param.unwrap_or(0).max(0) as u16);
            }

            // Info block
            "info" => {
                self.state.destination = Destination::Info;
            }
            "title" => {
                self.state.destination = Destination::InfoField(InfoField::Title);
                self.info_text_buf.clear();
            }
            "author" => {
                self.state.destination = Destination::InfoField(InfoField::Author);
                self.info_text_buf.clear();
            }
            "subject" => {
                self.state.destination = Destination::InfoField(InfoField::Subject);
                self.info_text_buf.clear();
            }

            // Image
            "pict" => {
                self.state.destination = Destination::Pict;
                if self.images.len() < MAX_IMAGES {
                    self.image_builder = Some(ImageBuilder::new());
                }
            }
            "pngblip" => {
                if let Some(ref mut ib) = self.image_builder {
                    ib.format = ImageFormat::Png;
                }
            }
            "jpegblip" => {
                if let Some(ref mut ib) = self.image_builder {
                    ib.format = ImageFormat::Jpeg;
                }
            }
            "emfblip" => {
                if let Some(ref mut ib) = self.image_builder {
                    ib.format = ImageFormat::Emf;
                }
            }
            "wmetafile" => {
                if let Some(ref mut ib) = self.image_builder {
                    ib.format = ImageFormat::Wmf;
                }
            }
            "picw" => {
                if let Some(ref mut ib) = self.image_builder {
                    ib.width = param.unwrap_or(0).max(0) as u32;
                }
            }
            "pich" => {
                if let Some(ref mut ib) = self.image_builder {
                    ib.height = param.unwrap_or(0).max(0) as u32;
                }
            }
            "picwgoal" => {
                if let Some(ref mut ib) = self.image_builder {
                    ib.goal_width = param.unwrap_or(0).max(0) as u32;
                }
            }
            "pichgoal" => {
                if let Some(ref mut ib) = self.image_builder {
                    ib.goal_height = param.unwrap_or(0).max(0) as u32;
                }
            }

            // Skip destinations: use ignore_depth to reliably suppress all
            // content within these groups, even if nested groups reset the
            // destination to Body (e.g. \pard inside \footnote).
            "stylesheet" => {
                self.state.destination = Destination::Stylesheet;
                self.ignore_depth = 1;
            }
            "header" | "headerl" | "headerr" | "headerf" => {
                self.state.destination = Destination::Header;
                self.ignore_depth = 1;
            }
            "footer" | "footerl" | "footerr" | "footerf" => {
                self.state.destination = Destination::Footer;
                self.ignore_depth = 1;
            }
            "footnote" => {
                self.state.destination = Destination::Footnote;
                self.ignore_depth = 1;
            }
            "field" => {
                // Track that we're inside a \field group so we can clear
                // pending_hyperlink when the group closes. We record the
                // current depth (already incremented by handle_group_open).
                if self.field_depth == 0 {
                    self.field_depth = self.depth;
                }
            }
            "fldinst" => {
                // Accumulate instruction text instead of ignoring the group,
                // so we can extract HYPERLINK URLs.
                self.state.destination = Destination::FieldInst;
                self.field_inst_buf.clear();
            }

            // Character formatting
            "b" => {
                self.flush_decoder();
                self.state.bold = param != Some(0);
            }
            "i" => {
                self.flush_decoder();
                self.state.italic = param != Some(0);
            }
            "ul" => {
                self.flush_decoder();
                self.state.underline = true;
            }
            "ulnone" => {
                self.flush_decoder();
                self.state.underline = false;
            }
            // \striked is non-standard but seen in some Word-generated files
            "strike" | "striked" => {
                self.flush_decoder();
                self.state.strikethrough = param != Some(0);
            }
            "super" => {
                self.flush_decoder();
                self.state.superscript = true;
                self.state.subscript = false;
            }
            "sub" => {
                self.flush_decoder();
                self.state.subscript = true;
                self.state.superscript = false;
            }
            "nosupersub" => {
                self.flush_decoder();
                self.state.superscript = false;
                self.state.subscript = false;
            }
            "v" => {
                self.flush_decoder();
                self.state.invisible = param != Some(0);
            }
            "fs" => {
                self.flush_decoder();
                self.state.font_size_half_pts = param.unwrap_or(24);
            }
            "plain" => {
                self.flush_decoder();
                self.state.bold = false;
                self.state.italic = false;
                self.state.underline = false;
                self.state.strikethrough = false;
                self.state.superscript = false;
                self.state.subscript = false;
                self.state.invisible = false;
                self.state.color_index = None;
                self.state.bg_color_index = None;
                self.state.highlight_color = None;
                self.state.font_size_half_pts = 24;
                self.state.font_index = self.default_font;
                self.update_decoder_for_font(self.default_font);
            }
            "pard" => {
                // Reset paragraph formatting. Keep character formatting.
                // Finalize any in-progress row but defer table flush.
                // Some producers emit \pard between table rows as a
                // formatting reset; flushing here would split one logical
                // table into multiple ParsedTables. Instead we keep
                // accumulated rows and flush them when non-table content
                // is emitted (in finish_paragraph/flush_paragraph) or EOF.
                if self.state.in_table {
                    if let Some(rb) = self.row_builder.take() {
                        let row = rb.finalize_row();
                        self.current_table_rows.push(row);
                    }
                }
                self.state.in_table = false;
                // Reset paragraph-level properties.
                self.para_alignment = None;
                self.para_space_before = None;
                self.para_space_after = None;
                self.para_indent_left = None;
                self.para_indent_right = None;
                self.para_first_line_indent = None;
            }

            // Paragraph alignment (NOT group-scoped, stored on Parser)
            "ql" => {
                self.para_alignment = Some(Alignment::Left);
            }
            "qc" => {
                self.para_alignment = Some(Alignment::Center);
            }
            "qr" => {
                self.para_alignment = Some(Alignment::Right);
            }
            "qj" => {
                self.para_alignment = Some(Alignment::Justify);
            }

            // Paragraph spacing (twips -> points: divide by 20).
            // Negative values are uncommon but valid in RTF; preserve them
            // for consistency with \li/\ri which also allow negatives.
            "sb" => {
                if let Some(twips) = param {
                    self.para_space_before = Some(twips_to_points(twips as f64));
                }
            }
            "sa" => {
                if let Some(twips) = param {
                    self.para_space_after = Some(twips_to_points(twips as f64));
                }
            }

            // Paragraph indentation (twips -> points: divide by 20).
            // Negative values are valid (hanging indentation effects).
            "li" => {
                if let Some(twips) = param {
                    self.para_indent_left = Some(twips_to_points(twips as f64));
                }
            }
            "ri" => {
                if let Some(twips) = param {
                    self.para_indent_right = Some(twips_to_points(twips as f64));
                }
            }
            "fi" => {
                if let Some(twips) = param {
                    self.para_first_line_indent = Some(twips_to_points(twips as f64));
                }
            }

            // Paragraph breaks
            "par" => {
                self.flush_decoder();
                self.finish_paragraph();
            }
            // Line break within current paragraph (not a paragraph break).
            "line" => {
                self.flush_decoder();
                self.emit_text("\n");
            }
            "page" | "sect" => {
                self.flush_decoder();
                self.finish_paragraph();
            }
            "tab" => {
                self.flush_decoder();
                self.emit_text("\t");
            }

            // Unicode
            "u" => {
                self.flush_decoder();
                if let Some(code) = param {
                    let codepoint = if code < 0 {
                        (code as i64 + 65536) as u32
                    } else {
                        code as u32
                    };
                    if let Some(ch) = char::from_u32(codepoint) {
                        self.emit_text(&ch.to_string());
                    }
                    self.skip_after_unicode = self.state.uc_skip;
                }
            }
            "uc" => {
                // SEC #62 ( round-2 audit, CVSS 5.3): cap the
                // \uc skip count. Adversarial RTF can claim
                // 쐩4967295 to force the parser to skip ~4
                // billion characters per Unicode escape. Real-world
                // RTF rarely uses \uc > 8; cap at 1024 as a safe
                // ceiling.
                const MAX_UC_SKIP: i32 = 1024;
                let raw = param.unwrap_or(1).max(0);
                self.state.uc_skip = raw.min(MAX_UC_SKIP) as usize;
            }

            // Table control words
            "trowd" => {
                self.flush_decoder();
                self.state.in_table = true;
                self.row_builder = Some(TableRowBuilder::new());
            }
            "cellx" => {
                if let Some(ref mut rb) = self.row_builder {
                    rb.add_cellx(param.unwrap_or(0).max(0) as u32);
                }
            }
            "cell" => {
                self.flush_decoder();
                self.finalize_table_cell();
            }
            "row" => {
                self.flush_decoder();
                self.finalize_table_row();
            }
            "intbl" => {
                self.state.in_table = true;
            }
            // Horizontal cell merge.
            "clmgf" => {
                if let Some(ref mut rb) = self.row_builder {
                    rb.current_merge = CellMerge::First;
                }
            }
            "clmrg" => {
                if let Some(ref mut rb) = self.row_builder {
                    rb.current_merge = CellMerge::Continue;
                }
            }
            // Vertical cell merge (not yet implemented, row_span stays 1).
            "clvmgf" | "clvmrg" => {
                self.warn(format!(
                    "\\{name} vertical cell merge not supported, row_span will be 1"
                ));
            }

            // Special characters
            "lquote" => {
                self.flush_decoder();
                self.emit_text("\u{2018}");
            }
            "rquote" => {
                self.flush_decoder();
                self.emit_text("\u{2019}");
            }
            "ldblquote" => {
                self.flush_decoder();
                self.emit_text("\u{201C}");
            }
            "rdblquote" => {
                self.flush_decoder();
                self.emit_text("\u{201D}");
            }
            "bullet" => {
                self.flush_decoder();
                self.emit_text("\u{2022}");
            }
            "emdash" => {
                self.flush_decoder();
                self.emit_text("\u{2014}");
            }
            "endash" => {
                self.flush_decoder();
                self.emit_text("\u{2013}");
            }
            "emspace" => {
                self.flush_decoder();
                self.emit_text("\u{2003}");
            }
            "enspace" => {
                self.flush_decoder();
                self.emit_text("\u{2002}");
            }

            // Unknown control word
            _ => {
                if was_star {
                    self.warn(format!("skipping unknown \\* destination: \\{name}"));
                    self.state.destination = Destination::Unknown;
                    self.ignore_depth = 1;
                }
            }
        }

        Ok(())
    }

    // -- Text / hex handling ------------------------------------------------

    fn handle_hex_escape(&mut self, byte: u8) {
        if self.ignore_depth > 0 {
            return;
        }
        if self.skip_after_unicode > 0 {
            self.skip_after_unicode -= 1;
            return;
        }

        match self.state.destination {
            Destination::Body => {
                self.decoder.push_byte(byte);
            }
            Destination::FontTable => {
                self.decoder.push_byte(byte);
            }
            Destination::Pict => {
                // Hex escapes in pict are unusual but treat as hex chars.
                if let Some(ref mut ib) = self.image_builder {
                    let hi = b"0123456789abcdef"[(byte >> 4) as usize];
                    let lo = b"0123456789abcdef"[(byte & 0x0f) as usize];
                    ib.push_hex(&[hi, lo]);
                }
            }
            Destination::InfoField(_) => {
                self.decoder.push_byte(byte);
            }
            Destination::FieldInst => {
                self.decoder.push_byte(byte);
            }
            _ => {}
        }
    }

    fn handle_text(&mut self, bytes: &[u8]) {
        if self.ignore_depth > 0 {
            return;
        }

        match self.state.destination {
            Destination::Body => {
                for &b in bytes {
                    if self.skip_after_unicode > 0 {
                        self.skip_after_unicode -= 1;
                        continue;
                    }
                    self.decoder.push_byte(b);
                }
            }
            Destination::FontTable => {
                // Font name text: may end with ';'
                for &b in bytes {
                    if self.skip_after_unicode > 0 {
                        self.skip_after_unicode -= 1;
                        continue;
                    }
                    self.decoder.push_byte(b);
                }
                // Check if the decoded text contains ';' (font name terminator).
                let text = self.decoder.flush();
                if !text.is_empty() {
                    let font_name = text.trim_end_matches(';').to_string();
                    if let Some(ref mut entry) = self.current_font_entry {
                        entry.name.push_str(&font_name);
                    }
                }
            }
            Destination::Pict => {
                if let Some(ref mut ib) = self.image_builder {
                    ib.push_hex(bytes);
                }
            }
            Destination::InfoField(_) => {
                for &b in bytes {
                    if self.skip_after_unicode > 0 {
                        self.skip_after_unicode -= 1;
                        continue;
                    }
                    self.decoder.push_byte(b);
                }
                let text = self.decoder.flush();
                self.info_text_buf.push_str(&text);
            }
            Destination::FieldInst => {
                // Accumulate field instruction text (e.g. HYPERLINK "url").
                // Use the decoder for proper codepage handling.
                for &b in bytes {
                    if self.skip_after_unicode > 0 {
                        self.skip_after_unicode -= 1;
                        continue;
                    }
                    self.decoder.push_byte(b);
                }
                let text = self.decoder.flush();
                if self.field_inst_buf.len() + text.len() <= MAX_URL_LENGTH {
                    self.field_inst_buf.push_str(&text);
                }
            }
            Destination::ColorTable => {
                // Semicolons delimit color entries. Each ';' pushes the
                // accumulated RGB values (or None for auto) into the table.
                for &b in bytes {
                    if b == b';' {
                        self.push_color_entry();
                    }
                }
            }
            _ => {
                // Skip text in Stylesheet, Header, Footer, etc.
            }
        }
    }

    fn warn(&mut self, msg: String) {
        // Reserve one slot for the sentinel message, so total capacity is
        // MAX_WARNINGS entries (MAX_WARNINGS - 1 real + 1 sentinel).
        if self.warnings.len() < MAX_WARNINGS.saturating_sub(1) {
            self.warnings.push(msg);
        } else if self.warnings.len() == MAX_WARNINGS.saturating_sub(1) {
            self.warnings
                .push("further warnings suppressed".to_string());
        }
    }

    fn set_font_family(&mut self, family: FontFamily) {
        if let Some(ref mut entry) = self.current_font_entry {
            entry.family = family;
        }
    }

    /// Push a color entry from the accumulated pending_red/green/blue values.
    /// Called on each ';' in the colortbl destination.
    fn push_color_entry(&mut self) {
        if self.color_table.len() >= MAX_COLOR_TABLE_ENTRIES {
            self.warn(format!(
                "color table exceeded {MAX_COLOR_TABLE_ENTRIES} entries, ignoring excess"
            ));
            return;
        }
        let entry = match (self.pending_red, self.pending_green, self.pending_blue) {
            (Some(r), Some(g), Some(b)) => Some([r, g, b]),
            (None, None, None) => None, // Auto/default color
            _ => {
                // Partial color spec: treat as auto, warn.
                self.warn("incomplete color table entry, treating as auto".into());
                None
            }
        };
        self.color_table.push(entry);
        self.pending_red = None;
        self.pending_green = None;
        self.pending_blue = None;
    }

    /// Resolve a color index to an RGB triple. Returns None for index 0
    /// (auto) or out-of-range indices.
    fn resolve_color(&self, index: Option<usize>) -> Option<[u8; 3]> {
        match index {
            Some(0) | None => None, // Index 0 is always auto
            Some(idx) => {
                if idx < self.color_table.len() {
                    self.color_table[idx]
                } else {
                    // Out of range: treat as auto. Malformed RTF documents may
                    // reference color indices beyond the table size.
                    None
                }
            }
        }
    }

    fn flush_decoder(&mut self) {
        let text = self.decoder.flush();
        if !text.is_empty() {
            match self.state.destination {
                Destination::Body => {
                    self.emit_text(&text);
                }
                Destination::FontTable => {
                    let font_name = text.trim_end_matches(';').to_string();
                    if let Some(ref mut entry) = self.current_font_entry {
                        entry.name.push_str(&font_name);
                    }
                }
                Destination::InfoField(_) => {
                    self.info_text_buf.push_str(&text);
                }
                Destination::FieldInst
                    if self.field_inst_buf.len() + text.len() <= MAX_URL_LENGTH =>
                {
                    self.field_inst_buf.push_str(&text);
                }
                _ => {}
            }
        }
    }

    fn emit_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        let run = self.make_text_run(text);

        // If we're in a table cell, push to the row builder's current cell.
        if self.state.in_table {
            if let Some(ref mut rb) = self.row_builder {
                rb.current_cell_runs.push(run);
                return;
            }
        }

        self.current_runs.push(run);
    }

    fn make_text_run(&self, text: &str) -> TextRun {
        let font_name = self.font_name_for_index(self.state.font_index);
        TextRun {
            text: text.to_string(),
            bold: self.state.bold,
            italic: self.state.italic,
            underline: self.state.underline,
            strikethrough: self.state.strikethrough,
            superscript: self.state.superscript,
            subscript: self.state.subscript,
            invisible: self.state.invisible,
            font_name,
            font_size_pts: self.state.font_size_pts(),
            color: self.resolve_color(self.state.color_index),
            bg_color: self
                .state
                .highlight_color
                .or_else(|| self.resolve_color(self.state.bg_color_index)),
            hyperlink_url: self.pending_hyperlink.clone(),
        }
    }

    fn font_name_for_index(&self, idx: usize) -> Option<Arc<str>> {
        self.font_name_cache.get(&idx).cloned()
    }

    fn update_decoder_for_font(&mut self, idx: usize) {
        if let Some(&pos) = self.font_index_map.get(&idx) {
            let enc = encoding_for_charset(self.fonts[pos].charset);
            self.decoder.set_encoding(enc);
        } else {
            self.decoder.set_encoding(self.default_encoding);
        }
    }

    fn finish_paragraph(&mut self) {
        if self.state.in_table {
            let has_active_row = self.row_builder.as_ref().is_some();
            if has_active_row {
                // Active row builder: \par is a line break inside cell text.
                let has_cell_runs = self
                    .row_builder
                    .as_ref()
                    .is_some_and(|rb| !rb.current_cell_runs.is_empty());
                if has_cell_runs {
                    let run = self.make_text_run("\n");
                    if let Some(ref mut rb) = self.row_builder {
                        rb.current_cell_runs.push(run);
                    }
                }
                return;
            }
            // No active row builder: we're past the last \row but \pard
            // hasn't appeared yet. Flush accumulated table rows and reset
            // table state so subsequent text becomes normal paragraphs.
            self.flush_pending_table();
            self.state.in_table = false;
        }

        // If we have accumulated table rows from a previous table context
        // (deferred by \pard), flush them now before emitting body text.
        self.flush_pending_table();

        let runs = std::mem::take(&mut self.current_runs);
        if !runs.is_empty() {
            self.paragraphs.push(self.make_paragraph(runs));
        }
    }

    fn flush_paragraph(&mut self) {
        let runs = std::mem::take(&mut self.current_runs);
        if !runs.is_empty() {
            self.paragraphs.push(self.make_paragraph(runs));
        }
    }

    fn make_paragraph(&self, runs: Vec<TextRun>) -> Paragraph {
        Paragraph {
            runs,
            alignment: self.para_alignment,
            space_before: self.para_space_before,
            space_after: self.para_space_after,
            indent_left: self.para_indent_left,
            indent_right: self.para_indent_right,
            first_line_indent: self.para_first_line_indent,
        }
    }

    fn finalize_font_entry(&mut self) {
        if let Some(entry) = self.current_font_entry.take() {
            // Flush any remaining decoder bytes as the font name.
            let leftover = self.decoder.flush();
            let mut final_entry = entry;
            if !leftover.is_empty() {
                let name_part = leftover.trim_end_matches(';');
                final_entry.name.push_str(name_part);
            }
            // Trim trailing whitespace from font name.
            let trimmed = final_entry.name.trim().to_string();
            final_entry.name = trimmed;
            let idx = final_entry.index;
            let pos = self.fonts.len();
            let name_arc: Arc<str> = Arc::from(final_entry.name.as_str());
            self.font_index_map.insert(idx, pos);
            self.font_name_cache.insert(idx, name_arc);
            self.fonts.push(final_entry);
        }
    }

    fn finalize_image(&mut self) {
        if let Some(ib) = self.image_builder.take() {
            match ib.format {
                ImageFormat::Emf => {
                    self.warn("skipping EMF image (not supported)".into());
                    return;
                }
                ImageFormat::Wmf => {
                    self.warn("skipping WMF image (not supported)".into());
                    return;
                }
                _ => {}
            }
            let image = ib.finalize();
            self.images.push(image);
        }
    }

    fn finalize_info_field(&mut self, field: InfoField) {
        // Flush any remaining decoder bytes.
        let leftover = self.decoder.flush();
        self.info_text_buf.push_str(&leftover);

        let value = std::mem::take(&mut self.info_text_buf);
        if value.is_empty() {
            return;
        }

        match field {
            InfoField::Title => self.metadata.title = Some(value),
            InfoField::Author => self.metadata.author = Some(value),
            InfoField::Subject => self.metadata.subject = Some(value),
            InfoField::Keywords | InfoField::Other => {}
        }
    }

    /// Parse the accumulated `\fldinst` text for a `HYPERLINK` instruction
    /// and store the extracted URL in `pending_hyperlink`.
    fn finalize_field_inst(&mut self) {
        // Flush any remaining decoder bytes into the instruction buffer.
        let leftover = self.decoder.flush();
        if self.field_inst_buf.len() + leftover.len() <= MAX_URL_LENGTH {
            self.field_inst_buf.push_str(&leftover);
        }

        let inst = std::mem::take(&mut self.field_inst_buf);
        let trimmed = inst.trim();

        // Only handle HYPERLINK instructions. Other field types (PAGE,
        // DATE, TOC, etc.) are intentionally ignored.
        if let Some(rest) = trimmed.strip_prefix("HYPERLINK") {
            let rest = rest.trim_start();
            let url = if let Some(after_quote) = rest.strip_prefix('"') {
                // Quoted URL: HYPERLINK "http://example.com"
                // Find the closing quote.
                if let Some(end) = after_quote.find('"') {
                    &after_quote[..end]
                } else {
                    // No closing quote: take everything after the opening quote.
                    after_quote
                }
            } else {
                // Unquoted URL: take until first whitespace or end.
                rest.split_whitespace().next().unwrap_or("")
            };

            if !url.is_empty() {
                if url.len() > MAX_URL_LENGTH {
                    self.warn(format!(
                        "HYPERLINK URL truncated from {} to {} bytes",
                        url.len(),
                        MAX_URL_LENGTH
                    ));
                    self.pending_hyperlink = Some(url[..MAX_URL_LENGTH].to_string());
                } else {
                    self.pending_hyperlink = Some(url.to_string());
                }
            }
        }
    }

    fn finalize_table_cell(&mut self) {
        // Some producers emit \cell without a preceding \trowd. Create
        // a row builder on demand so we don't silently drop cell content.
        if self.row_builder.is_none() {
            self.row_builder = Some(TableRowBuilder::new());
        }
        if let Some(ref mut rb) = self.row_builder {
            let runs = std::mem::take(&mut self.current_runs);
            // Also grab any runs already pushed to the row builder.
            let mut cell_runs = std::mem::take(&mut rb.current_cell_runs);
            cell_runs.extend(runs);
            rb.current_cell_runs = cell_runs;
            rb.finalize_cell();
        }
    }

    fn finalize_table_row(&mut self) {
        if let Some(rb) = self.row_builder.take() {
            let row = rb.finalize_row();
            self.current_table_rows.push(row);
        }
        // Don't reset in_table here; it persists until \pard.
    }

    fn flush_pending_table(&mut self) {
        if !self.current_table_rows.is_empty() {
            let rows = std::mem::take(&mut self.current_table_rows);
            self.tables.push(ParsedTable { rows });
        }
    }
}

#[cfg(test)]
#[path = "parser_tests.rs"]
mod tests;
