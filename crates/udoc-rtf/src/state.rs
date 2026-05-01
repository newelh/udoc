//! Parser state that gets pushed/popped with RTF groups.

/// Formatting and parsing state tracked per RTF group level.
#[derive(Debug, Clone)]
pub struct ParserState {
    /// Current font index (\fN).
    pub font_index: usize,
    /// Font size in half-points (\fsN). Default 24 (12pt).
    pub font_size_half_pts: i32,
    /// Bold flag (\b / \b0).
    pub bold: bool,
    /// Italic flag (\i / \i0).
    pub italic: bool,
    /// Underline flag (\ul / \ulnone).
    pub underline: bool,
    /// Strikethrough flag (\strike / \strike0).
    pub strikethrough: bool,
    /// Superscript flag (\super).
    pub superscript: bool,
    /// Subscript flag (\sub).
    pub subscript: bool,
    /// Hidden/invisible text (\v / \v0).
    pub invisible: bool,
    /// Foreground color index (\cfN) into the color table.
    pub color_index: Option<usize>,
    /// Background color index (\cbN) into the color table.
    pub bg_color_index: Option<usize>,
    /// Highlight color resolved from the fixed 16-color RTF palette (\highlightN).
    /// Takes precedence over bg_color_index when set.
    pub highlight_color: Option<[u8; 3]>,
    /// Unicode skip count (\ucN). Default 1.
    pub uc_skip: usize,
    /// Whether we're inside a table (\intbl).
    pub in_table: bool,
    /// Current destination context.
    pub destination: Destination,
}

impl Default for ParserState {
    fn default() -> Self {
        Self {
            font_index: 0,          // \deff0
            font_size_half_pts: 24, // \fs24 = 12pt
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            superscript: false,
            subscript: false,
            invisible: false,
            color_index: None,
            bg_color_index: None,
            highlight_color: None,
            uc_skip: 1, // \uc1
            in_table: false,
            destination: Destination::Body,
        }
    }
}

impl ParserState {
    /// Converts the font size from half-points to points.
    pub fn font_size_pts(&self) -> f64 {
        self.font_size_half_pts as f64 / 2.0
    }
}

/// Paragraph text alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    Left,
    Center,
    Right,
    Justify,
}

/// RTF destination contexts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    /// Normal body text.
    Body,
    /// Font table (\fonttbl).
    FontTable,
    /// Color table (\colortbl).
    ColorTable,
    /// Document info (\info).
    Info,
    /// Info sub-destinations (title, author, etc).
    InfoField(InfoField),
    /// Picture data (\pict).
    Pict,
    /// Header (\header, \headerl, etc.) -- skip for text extraction.
    Header,
    /// Footer (\footer, etc.) -- skip for text extraction.
    Footer,
    /// Footnote (\footnote).
    Footnote,
    /// Field instruction (\fldinst) -- skip.
    FieldInst,
    /// Stylesheet (\stylesheet) -- skip for now.
    Stylesheet,
    /// Unknown destination prefixed with \* -- skip text.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Keywords/Other variants exist for match completeness but are never constructed
pub enum InfoField {
    Title,
    Author,
    Subject,
    Keywords,
    Other,
}

/// Font entry from the font table.
#[derive(Debug, Clone)]
pub struct FontEntry {
    /// Font index (\fN).
    pub index: usize,
    /// Font name.
    pub name: String,
    /// Font charset (\fcharsetN).
    pub charset: u8,
    /// Font family (\froman, \fswiss, etc.).
    pub family: FontFamily,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FontFamily {
    Roman,
    Swiss,
    Modern,
    Script,
    Decor,
    Tech,
    Bidi,
    #[default]
    Nil,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state() {
        let state = ParserState::default();
        assert_eq!(state.font_index, 0);
        assert_eq!(state.font_size_half_pts, 24);
        assert!(!state.bold);
        assert!(!state.italic);
        assert!(!state.underline);
        assert!(!state.strikethrough);
        assert!(!state.superscript);
        assert!(!state.subscript);
        assert!(!state.invisible);
        assert!(state.color_index.is_none());
        assert!(state.bg_color_index.is_none());
        assert!(state.highlight_color.is_none());
        assert_eq!(state.uc_skip, 1);
        assert!(!state.in_table);
        assert_eq!(state.destination, Destination::Body);
    }

    #[test]
    fn clone_preserves_state() {
        let state = ParserState {
            bold: true,
            font_size_half_pts: 48,
            destination: Destination::FontTable,
            ..Default::default()
        };

        let cloned = state.clone();
        assert!(cloned.bold);
        assert_eq!(cloned.font_size_half_pts, 48);
        assert_eq!(cloned.destination, Destination::FontTable);
        // Mutating the clone doesn't affect the original.
        assert!(state.bold);
    }

    #[test]
    fn font_size_pts_conversion() {
        let mut state = ParserState::default();
        // Default: 24 half-points = 12pt
        assert!((state.font_size_pts() - 12.0).abs() < f64::EPSILON);

        state.font_size_half_pts = 48;
        assert!((state.font_size_pts() - 24.0).abs() < f64::EPSILON);

        // Odd half-point value
        state.font_size_half_pts = 25;
        assert!((state.font_size_pts() - 12.5).abs() < f64::EPSILON);
    }
}
