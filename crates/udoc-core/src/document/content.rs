//! Content spine types: Block, Inline, and supporting types.
//!
//! The content spine is the text-first layer of the document model.
//! It carries text, logical structure, core visual markup, and table
//! structure. Extended styling lives in the presentation overlay.

use super::assets::{AssetRef, ImageAsset};
use super::table::TableData;
use super::NodeId;

/// A block-level content element.
#[derive(Debug, Clone)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(tag = "type", rename_all = "snake_case")
)]
#[non_exhaustive]
pub enum Block {
    /// Section heading with level (1-6). Values outside 1-6 may appear
    /// from hooks or manual construction; consumers should clamp.
    Heading {
        id: NodeId,
        level: u8,
        content: Vec<Inline>,
    },
    /// A paragraph of inline content.
    Paragraph { id: NodeId, content: Vec<Inline> },
    /// A table.
    Table { id: NodeId, table: TableData },
    /// An ordered or unordered list.
    List {
        id: NodeId,
        items: Vec<ListItem>,
        kind: ListKind,
        start: u64,
    },
    /// A block of preformatted code.
    CodeBlock {
        id: NodeId,
        text: String,
        language: Option<String>,
    },
    /// A block-level image.
    Image {
        id: NodeId,
        image_ref: ImageRef,
        alt_text: Option<String>,
    },
    /// A page, slide, or sheet boundary.
    PageBreak { id: NodeId },
    /// A thematic/horizontal rule break.
    ThematicBreak { id: NodeId },
    /// A semantic container (HTML section/article/nav/aside, PDF tagged
    /// structure, DOCX section). Contains child blocks.
    Section {
        id: NodeId,
        role: Option<SectionRole>,
        children: Vec<Block>,
    },
    /// A non-textual visual element (PPTX shape, SVG primitive, Figma
    /// frame). Visual properties in the presentation overlay. Contains
    /// child blocks (shapes can nest and contain text).
    Shape {
        id: NodeId,
        kind: ShapeKind,
        children: Vec<Block>,
        alt_text: Option<String>,
    },
}

impl Block {
    /// Get this block's NodeId.
    pub fn id(&self) -> NodeId {
        match self {
            Self::Heading { id, .. }
            | Self::Paragraph { id, .. }
            | Self::Table { id, .. }
            | Self::List { id, .. }
            | Self::CodeBlock { id, .. }
            | Self::Image { id, .. }
            | Self::PageBreak { id }
            | Self::ThematicBreak { id }
            | Self::Section { id, .. }
            | Self::Shape { id, .. } => *id,
        }
    }

    /// Collect all text content recursively as a plain string.
    ///
    /// Image and shape alt_text is not included. Use pattern matching
    /// on `Block::Image` or `Block::Shape` to access alt_text.
    pub fn text(&self) -> String {
        let mut out = String::with_capacity(128);
        collect_block_text(self, &mut out);
        out
    }

    /// Direct child blocks (for manual recursive traversal).
    ///
    /// Returns the child block slice for container blocks (Section, Shape).
    /// Returns an empty slice for leaf blocks. List and Table children are
    /// nested inside items/rows/cells, not directly accessible as a flat
    /// slice. Use `Document::walk()` for full recursive traversal, or match
    /// on the variant directly.
    pub fn children(&self) -> &[Block] {
        match self {
            Self::Section { children, .. } | Self::Shape { children, .. } => children,
            _ => &[],
        }
    }
}

/// Maximum recursion depth for text collection and tree walking.
/// Prevents stack overflow from adversarial deeply-nested documents.
/// Shared across content.rs, mod.rs (walk/walk_mut, deserialization scan).
pub(crate) const MAX_RECURSION_DEPTH: usize = 256;

/// Recursively collect all text from a block into a string.
pub(crate) fn collect_block_text(block: &Block, out: &mut String) {
    collect_block_text_inner(block, out, 0);
}

fn collect_block_text_inner(block: &Block, out: &mut String, depth: usize) {
    if depth > MAX_RECURSION_DEPTH {
        return;
    }
    match block {
        Block::Heading { content, .. } | Block::Paragraph { content, .. } => {
            for inline in content {
                collect_inline_text_inner(inline, out, depth + 1);
            }
        }
        Block::CodeBlock { text, .. } => out.push_str(text),
        Block::List { items, .. } => {
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                for (j, block) in item.content.iter().enumerate() {
                    if j > 0 {
                        out.push('\n');
                    }
                    collect_block_text_inner(block, out, depth + 1);
                }
            }
        }
        Block::Table { table, .. } => {
            for (i, row) in table.rows.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                for (j, cell) in row.cells.iter().enumerate() {
                    if j > 0 {
                        out.push('\t');
                    }
                    for (k, block) in cell.content.iter().enumerate() {
                        if k > 0 {
                            out.push(' ');
                        }
                        collect_block_text_inner(block, out, depth + 1);
                    }
                }
            }
        }
        Block::Section { children, .. } | Block::Shape { children, .. } => {
            for (i, child) in children.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                collect_block_text_inner(child, out, depth + 1);
            }
        }
        _ => {}
    }
}

fn collect_inline_text_inner(inline: &Inline, out: &mut String, depth: usize) {
    if depth > MAX_RECURSION_DEPTH {
        return;
    }
    match inline {
        Inline::Text { text, .. } | Inline::Code { text, .. } => out.push_str(text),
        Inline::Link { content, .. } => {
            for child in content {
                collect_inline_text_inner(child, out, depth + 1);
            }
        }
        Inline::SoftBreak { .. } => out.push(' '),
        Inline::LineBreak { .. } => out.push('\n'),
        _ => {}
    }
}

/// An inline content element (styled text within a block).
#[derive(Debug, Clone)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(tag = "type", rename_all = "snake_case")
)]
#[non_exhaustive]
pub enum Inline {
    /// Text with core visual styling.
    Text {
        id: NodeId,
        text: String,
        style: SpanStyle,
    },
    /// Inline code.
    Code { id: NodeId, text: String },
    /// A hyperlink. URL is content (it is part of what the document says).
    Link {
        id: NodeId,
        url: String,
        content: Vec<Inline>,
    },
    /// A footnote reference marker. Definition in relationships overlay.
    FootnoteRef { id: NodeId, label: String },
    /// An inline image.
    InlineImage {
        id: NodeId,
        image_ref: ImageRef,
        alt_text: Option<String>,
    },
    /// A soft line break (may be reflowed).
    SoftBreak { id: NodeId },
    /// A hard line break.
    LineBreak { id: NodeId },
}

impl Inline {
    /// Get this inline's NodeId.
    pub fn id(&self) -> NodeId {
        match self {
            Self::Text { id, .. }
            | Self::Code { id, .. }
            | Self::Link { id, .. }
            | Self::FootnoteRef { id, .. }
            | Self::InlineImage { id, .. }
            | Self::SoftBreak { id }
            | Self::LineBreak { id } => *id,
        }
    }

    /// Get the text content of this inline (not recursive).
    pub fn text(&self) -> &str {
        match self {
            Self::Text { text, .. } | Self::Code { text, .. } => text,
            _ => "",
        }
    }
}

/// Core visual styling on a text span.
///
/// These properties carry semantic weight (bold = emphasis, italic = citation).
/// Extended styling (font name, size, color) lives in the presentation overlay.
///
/// Custom serde: only serializes fields that are true (sparse style).
/// Empty SpanStyle serializes as `{}`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SpanStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub superscript: bool,
    pub subscript: bool,
}

impl SpanStyle {
    /// Whether all style flags are false (plain/unstyled text).
    pub fn is_plain(&self) -> bool {
        !self.bold
            && !self.italic
            && !self.underline
            && !self.strikethrough
            && !self.superscript
            && !self.subscript
    }

    /// Builder: set bold flag.
    pub fn with_bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// Builder: set italic flag.
    pub fn with_italic(mut self) -> Self {
        self.italic = true;
        self
    }
}

impl ListItem {
    /// Create a new list item with the given id and content blocks.
    pub fn new(id: NodeId, content: Vec<Block>) -> Self {
        Self { id, content }
    }
}

// Custom serde for SpanStyle: omit false bools (sparse style).
#[cfg(feature = "serde")]
impl serde::Serialize for SpanStyle {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut count = 0;
        if self.bold {
            count += 1;
        }
        if self.italic {
            count += 1;
        }
        if self.underline {
            count += 1;
        }
        if self.strikethrough {
            count += 1;
        }
        if self.superscript {
            count += 1;
        }
        if self.subscript {
            count += 1;
        }
        let mut map = serializer.serialize_map(Some(count))?;
        if self.bold {
            map.serialize_entry("bold", &true)?;
        }
        if self.italic {
            map.serialize_entry("italic", &true)?;
        }
        if self.underline {
            map.serialize_entry("underline", &true)?;
        }
        if self.strikethrough {
            map.serialize_entry("strikethrough", &true)?;
        }
        if self.superscript {
            map.serialize_entry("superscript", &true)?;
        }
        if self.subscript {
            map.serialize_entry("subscript", &true)?;
        }
        map.end()
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for SpanStyle {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{MapAccess, Visitor};
        use std::fmt;

        struct SpanStyleVisitor;

        impl<'de> Visitor<'de> for SpanStyleVisitor {
            type Value = SpanStyle;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a map of style flags")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut access: M) -> Result<Self::Value, M::Error> {
                let mut style = SpanStyle::default();
                while let Some(key) = access.next_key::<String>()? {
                    match key.as_str() {
                        "bold" => style.bold = access.next_value()?,
                        "italic" => style.italic = access.next_value()?,
                        "underline" => style.underline = access.next_value()?,
                        "strikethrough" => style.strikethrough = access.next_value()?,
                        "superscript" => style.superscript = access.next_value()?,
                        "subscript" => style.subscript = access.next_value()?,
                        _ => {
                            let _ = access.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }
                Ok(style)
            }
        }

        deserializer.deserialize_map(SpanStyleVisitor)
    }
}

/// Semantic role for Section blocks.
///
/// Backend converters used to reach for `SectionRole::Named("footnotes")`,
/// `Named("headers-footers")`, etc., which required downstream consumers
/// to match on raw strings. The common roles are now first-class variants;
/// [`SectionRole::Named`] is retained for format-specific custom roles
/// that don't correspond to a semantic category (DOC custom sections,
/// PDF tagged structure `/Part`, HTML custom elements, etc.) but new
/// converters should prefer the typed variants whenever possible (#146).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
#[non_exhaustive]
pub enum SectionRole {
    /// Generic container (div-like).
    Generic,
    /// Article content.
    Article,
    /// Navigation section.
    Navigation,
    /// Complementary / aside content.
    Complementary,
    /// Semantic page header (emitted at document top).
    Header,
    /// Semantic page footer (emitted at document bottom).
    Footer,
    /// Main content region.
    Main,
    /// Grouped headers and footers (DOC "headers-footers" section bucket).
    HeadersFooters,
    /// Footnote definitions collected as a trailing section (DOCX, DOC, ODT).
    Footnotes,
    /// Endnote definitions collected as a trailing section (DOCX, DOC, ODT).
    Endnotes,
    /// Slide speaker notes (PPTX, PPT, ODP) attached to a slide.
    Notes,
    /// Markdown blockquote or equivalent quoted section.
    Blockquote,
    /// Fallback for format-specific roles that don't map to a typed
    /// variant. Prefer a typed variant where one exists.
    Named(String),
}

impl SectionRole {
    /// Map a role name string to its typed variant when one exists. Unknown
    /// names fall back to [`SectionRole::Named`]. This keeps backend code
    /// like `push_named_section(doc, "footnotes", ..)` working with typed
    /// output, and gives a single point of truth for the string mapping.
    pub fn from_name(name: &str) -> Self {
        match name {
            "footnotes" => SectionRole::Footnotes,
            "endnotes" => SectionRole::Endnotes,
            "headers-footers" => SectionRole::HeadersFooters,
            "notes" => SectionRole::Notes,
            "blockquote" => SectionRole::Blockquote,
            other => SectionRole::Named(other.to_string()),
        }
    }
}

/// Shape kind for Shape blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
#[non_exhaustive]
pub enum ShapeKind {
    Rectangle,
    Ellipse,
    Polygon,
    Path,
    Line,
    Group,
    Frame,
    Canvas,
    Custom(String),
}

/// List kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
#[non_exhaustive]
pub enum ListKind {
    Unordered,
    Ordered,
}

/// A list item containing blocks.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct ListItem {
    pub id: NodeId,
    pub content: Vec<Block>,
}

/// Reference to an image in the [`AssetStore`](super::assets::AssetStore).
///
/// Type alias for `AssetRef<ImageAsset>`. Use `ImageRef::new(index)` to
/// create, and `ref.index()` to read the index.
pub type ImageRef = AssetRef<ImageAsset>;

/// Backward-compat alias: `ImageData` is now [`ImageAsset`] in the asset store.
pub type ImageData = ImageAsset;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::table::{TableCell, TableRow};

    #[test]
    fn block_id_accessor() {
        let id = NodeId::new(42);
        let block = Block::Paragraph {
            id,
            content: vec![],
        };
        assert_eq!(block.id(), id);
    }

    #[test]
    fn block_id_all_variants() {
        let id = NodeId::new(1);
        let variants: Vec<Block> = vec![
            Block::Heading {
                id,
                level: 1,
                content: vec![],
            },
            Block::Paragraph {
                id,
                content: vec![],
            },
            Block::Table {
                id,
                table: TableData {
                    rows: vec![],
                    num_columns: 0,
                    header_row_count: 0,
                    may_continue_from_previous: false,
                    may_continue_to_next: false,
                },
            },
            Block::List {
                id,
                items: vec![],
                kind: ListKind::Unordered,
                start: 1,
            },
            Block::CodeBlock {
                id,
                text: String::new(),
                language: None,
            },
            Block::Image {
                id,
                image_ref: ImageRef::new(0),
                alt_text: None,
            },
            Block::PageBreak { id },
            Block::ThematicBreak { id },
            Block::Section {
                id,
                role: None,
                children: vec![],
            },
            Block::Shape {
                id,
                kind: ShapeKind::Rectangle,
                children: vec![],
                alt_text: None,
            },
        ];
        for block in &variants {
            assert_eq!(block.id(), id);
        }
    }

    #[test]
    fn inline_id_accessor() {
        let id = NodeId::new(7);
        let inline = Inline::Text {
            id,
            text: "hi".into(),
            style: SpanStyle::default(),
        };
        assert_eq!(inline.id(), id);
    }

    #[test]
    fn inline_text_accessor() {
        let id = NodeId::new(0);
        assert_eq!(
            Inline::Text {
                id,
                text: "hello".into(),
                style: SpanStyle::default(),
            }
            .text(),
            "hello"
        );
        assert_eq!(
            Inline::Code {
                id,
                text: "fn main()".into(),
            }
            .text(),
            "fn main()"
        );
        assert_eq!(
            Inline::Link {
                id,
                url: "https://example.com".into(),
                content: vec![],
            }
            .text(),
            ""
        );
        assert_eq!(Inline::SoftBreak { id }.text(), "");
        assert_eq!(Inline::LineBreak { id }.text(), "");
    }

    #[test]
    fn span_style_is_plain() {
        assert!(SpanStyle::default().is_plain());
        assert!(!SpanStyle {
            bold: true,
            ..Default::default()
        }
        .is_plain());
        assert!(!SpanStyle {
            italic: true,
            ..Default::default()
        }
        .is_plain());
        assert!(!SpanStyle {
            underline: true,
            ..Default::default()
        }
        .is_plain());
        assert!(!SpanStyle {
            strikethrough: true,
            ..Default::default()
        }
        .is_plain());
        assert!(!SpanStyle {
            superscript: true,
            ..Default::default()
        }
        .is_plain());
        assert!(!SpanStyle {
            subscript: true,
            ..Default::default()
        }
        .is_plain());
    }

    #[test]
    fn block_text_paragraph() {
        let block = Block::Paragraph {
            id: NodeId::new(0),
            content: vec![
                Inline::Text {
                    id: NodeId::new(1),
                    text: "hello ".into(),
                    style: SpanStyle::default(),
                },
                Inline::Text {
                    id: NodeId::new(2),
                    text: "world".into(),
                    style: SpanStyle {
                        bold: true,
                        ..Default::default()
                    },
                },
            ],
        };
        assert_eq!(block.text(), "hello world");
    }

    #[test]
    fn block_text_heading() {
        let block = Block::Heading {
            id: NodeId::new(0),
            level: 1,
            content: vec![Inline::Text {
                id: NodeId::new(1),
                text: "Title".into(),
                style: SpanStyle::default(),
            }],
        };
        assert_eq!(block.text(), "Title");
    }

    #[test]
    fn block_text_code_block() {
        let block = Block::CodeBlock {
            id: NodeId::new(0),
            text: "fn main() {}".into(),
            language: Some("rust".into()),
        };
        assert_eq!(block.text(), "fn main() {}");
    }

    #[test]
    fn block_text_list() {
        let block = Block::List {
            id: NodeId::new(0),
            items: vec![
                ListItem {
                    id: NodeId::new(1),
                    content: vec![Block::Paragraph {
                        id: NodeId::new(2),
                        content: vec![Inline::Text {
                            id: NodeId::new(3),
                            text: "item one".into(),
                            style: SpanStyle::default(),
                        }],
                    }],
                },
                ListItem {
                    id: NodeId::new(4),
                    content: vec![Block::Paragraph {
                        id: NodeId::new(5),
                        content: vec![Inline::Text {
                            id: NodeId::new(6),
                            text: "item two".into(),
                            style: SpanStyle::default(),
                        }],
                    }],
                },
            ],
            kind: ListKind::Unordered,
            start: 1,
        };
        assert_eq!(block.text(), "item one\nitem two");
    }

    #[test]
    fn block_text_table() {
        let block = Block::Table {
            id: NodeId::new(0),
            table: TableData {
                rows: vec![
                    TableRow {
                        id: NodeId::new(1),
                        cells: vec![
                            TableCell {
                                id: NodeId::new(2),
                                content: vec![Block::Paragraph {
                                    id: NodeId::new(3),
                                    content: vec![Inline::Text {
                                        id: NodeId::new(4),
                                        text: "A".into(),
                                        style: SpanStyle::default(),
                                    }],
                                }],
                                col_span: 1,
                                row_span: 1,
                                value: None,
                            },
                            TableCell {
                                id: NodeId::new(5),
                                content: vec![Block::Paragraph {
                                    id: NodeId::new(6),
                                    content: vec![Inline::Text {
                                        id: NodeId::new(7),
                                        text: "B".into(),
                                        style: SpanStyle::default(),
                                    }],
                                }],
                                col_span: 1,
                                row_span: 1,
                                value: None,
                            },
                        ],
                        is_header: true,
                    },
                    TableRow {
                        id: NodeId::new(8),
                        cells: vec![
                            TableCell {
                                id: NodeId::new(9),
                                content: vec![Block::Paragraph {
                                    id: NodeId::new(10),
                                    content: vec![Inline::Text {
                                        id: NodeId::new(11),
                                        text: "1".into(),
                                        style: SpanStyle::default(),
                                    }],
                                }],
                                col_span: 1,
                                row_span: 1,
                                value: None,
                            },
                            TableCell {
                                id: NodeId::new(12),
                                content: vec![Block::Paragraph {
                                    id: NodeId::new(13),
                                    content: vec![Inline::Text {
                                        id: NodeId::new(14),
                                        text: "2".into(),
                                        style: SpanStyle::default(),
                                    }],
                                }],
                                col_span: 1,
                                row_span: 1,
                                value: None,
                            },
                        ],
                        is_header: false,
                    },
                ],
                num_columns: 2,
                header_row_count: 1,
                may_continue_from_previous: false,
                may_continue_to_next: false,
            },
        };
        assert_eq!(block.text(), "A\tB\n1\t2");
    }

    #[test]
    fn block_text_section() {
        let block = Block::Section {
            id: NodeId::new(0),
            role: Some(SectionRole::Article),
            children: vec![
                Block::Heading {
                    id: NodeId::new(1),
                    level: 1,
                    content: vec![Inline::Text {
                        id: NodeId::new(2),
                        text: "Title".into(),
                        style: SpanStyle::default(),
                    }],
                },
                Block::Paragraph {
                    id: NodeId::new(3),
                    content: vec![Inline::Text {
                        id: NodeId::new(4),
                        text: "Body".into(),
                        style: SpanStyle::default(),
                    }],
                },
            ],
        };
        assert_eq!(block.text(), "Title\nBody");
    }

    #[test]
    fn block_text_with_inline_breaks() {
        let block = Block::Paragraph {
            id: NodeId::new(0),
            content: vec![
                Inline::Text {
                    id: NodeId::new(1),
                    text: "line one".into(),
                    style: SpanStyle::default(),
                },
                Inline::LineBreak { id: NodeId::new(2) },
                Inline::Text {
                    id: NodeId::new(3),
                    text: "line two".into(),
                    style: SpanStyle::default(),
                },
            ],
        };
        assert_eq!(block.text(), "line one\nline two");
    }

    #[test]
    fn block_text_with_soft_break() {
        let block = Block::Paragraph {
            id: NodeId::new(0),
            content: vec![
                Inline::Text {
                    id: NodeId::new(1),
                    text: "word".into(),
                    style: SpanStyle::default(),
                },
                Inline::SoftBreak { id: NodeId::new(2) },
                Inline::Text {
                    id: NodeId::new(3),
                    text: "next".into(),
                    style: SpanStyle::default(),
                },
            ],
        };
        assert_eq!(block.text(), "word next");
    }

    #[test]
    fn block_text_with_link() {
        let block = Block::Paragraph {
            id: NodeId::new(0),
            content: vec![Inline::Link {
                id: NodeId::new(1),
                url: "https://example.com".into(),
                content: vec![Inline::Text {
                    id: NodeId::new(2),
                    text: "click here".into(),
                    style: SpanStyle::default(),
                }],
            }],
        };
        assert_eq!(block.text(), "click here");
    }

    #[test]
    fn block_children() {
        let section = Block::Section {
            id: NodeId::new(0),
            role: None,
            children: vec![Block::Paragraph {
                id: NodeId::new(1),
                content: vec![],
            }],
        };
        assert_eq!(section.children().len(), 1);

        let paragraph = Block::Paragraph {
            id: NodeId::new(2),
            content: vec![],
        };
        assert!(paragraph.children().is_empty());
    }

    #[test]
    fn block_text_leaf_variants() {
        assert_eq!(Block::PageBreak { id: NodeId::new(0) }.text(), "");
        assert_eq!(Block::ThematicBreak { id: NodeId::new(0) }.text(), "");
        assert_eq!(
            Block::Image {
                id: NodeId::new(0),
                image_ref: ImageRef::new(0),
                alt_text: Some("photo".into()),
            }
            .text(),
            ""
        );
    }

    #[test]
    fn table_cell_text_method() {
        let cell = TableCell {
            id: NodeId::new(0),
            content: vec![
                Block::Paragraph {
                    id: NodeId::new(1),
                    content: vec![Inline::Text {
                        id: NodeId::new(2),
                        text: "line 1".into(),
                        style: SpanStyle::default(),
                    }],
                },
                Block::Paragraph {
                    id: NodeId::new(3),
                    content: vec![Inline::Text {
                        id: NodeId::new(4),
                        text: "line 2".into(),
                        style: SpanStyle::default(),
                    }],
                },
            ],
            col_span: 1,
            row_span: 1,
            value: None,
        };
        assert_eq!(cell.text(), "line 1\nline 2");
    }

    #[test]
    fn image_ref_is_asset_ref() {
        // ImageRef is now a type alias for AssetRef<ImageAsset>.
        let r = ImageRef::new(42);
        assert_eq!(r.index(), 42);
    }
}
