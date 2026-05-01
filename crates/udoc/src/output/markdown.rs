//! Markdown output (T1b-MARKDOWN-OUT).
//!
//! Emits LLM-friendly markdown from the [`Document`] model. Two
//! public functions:
//!
//! - [`markdown_with_anchors`] -- preserves citation anchors as HTML
//!   comments (`<!-- udoc:page=N bbox=... node=node:1234 -->`) so
//!   downstream chunkers can recover provenance.
//! - [`markdown`] -- anchors stripped for human consumption.
//!
//! See  §4.2.5 +  Domain Expert spec. Foundation
//!'s `Document.to_markdown()` Python method.
//!
//! ## Style nesting
//!
//! Bold-inside-italic emits as `_**x**_` (asymmetric, CommonMark-stable).
//! Strike (`~~x~~`) is GFM-only. Underline / superscript / subscript
//! fall back to HTML (`<u>`, `<sup>`, `<sub>`) because there is no
//! portable markdown for them.
//!
//! ## Tables
//!
//! Pipe-syntax for simple cells. When any cell contains a literal `|`,
//! a newline, or multiple block children, the whole table renders as
//! `<table>` HTML instead -- markdown table syntax cannot escape
//! either character or split a cell across lines.
//!
//! ## Headings
//!
//! Backends that already encode an explicit heading level (DOCX `<w:pStyle
//! w:val="Heading1"/>`) come through as `Block::Heading { level, .. }`.
//! That level is honored 1:1. The font-size-rank fallback for
//! presentation-only documents (PDF, where heading rank is inferred from
//! relative font sizes) is handled at convert time, not here -- the
//! emitter trusts whatever level the Block carries.

use std::collections::HashMap;
use std::fmt::Write as _;

use udoc_core::document::{
    AssetStore, Block, Document, ImageRef, Inline, ListItem, ListKind, NodeId, SectionRole,
    SpanStyle, TableData,
};
use udoc_core::image::ImageFilter;

/// Emit LLM-friendly markdown with citation anchors as HTML comments.
///
/// Each block is preceded by a comment of the form
/// `<!-- udoc:page=N bbox=x_min,y_min,x_max,y_max node=node:1234 -->`.
/// Missing page or bbox attributes are simply omitted. Footnote
/// definitions are emitted in a trailing `[^label]: ...` section when
/// the [`udoc_core::document::Relationships`] overlay carries them.
pub fn markdown_with_anchors(doc: &Document) -> String {
    Emitter::new(doc, true).emit()
}

/// Emit human-readable markdown with citation anchors stripped.
///
/// Equivalent to [`markdown_with_anchors`] without the `<!-- udoc:... -->`
/// comments. Callers who need provenance should use the with-anchors
/// variant; this one is for previewing or terminal display.
pub fn markdown(doc: &Document) -> String {
    Emitter::new(doc, false).emit()
}

// ---------------------------------------------------------------------------
// Emitter
// ---------------------------------------------------------------------------

struct Emitter<'a> {
    doc: &'a Document,
    with_anchors: bool,
    out: String,
    /// Footnote labels referenced from the body, in encounter order.
    /// Used to emit a trailing `[^label]: definition` section.
    footnote_refs: Vec<String>,
}

impl<'a> Emitter<'a> {
    fn new(doc: &'a Document, with_anchors: bool) -> Self {
        Self {
            doc,
            with_anchors,
            out: String::with_capacity(1024),
            footnote_refs: Vec::new(),
        }
    }

    fn emit(mut self) -> String {
        let blocks = &self.doc.content;
        for (i, block) in blocks.iter().enumerate() {
            if i > 0 {
                self.out.push('\n');
            }
            self.emit_block(block);
        }
        self.emit_footnote_definitions();
        self.out
    }

    fn emit_anchor(&mut self, id: NodeId) {
        if !self.with_anchors {
            return;
        }
        let page = self
            .doc
            .presentation
            .as_ref()
            .and_then(|p| p.page_assignments.get(id))
            .copied();
        let bbox = self
            .doc
            .presentation
            .as_ref()
            .and_then(|p| p.geometry.get(id))
            .copied();
        // Suppress the comment entirely if there is no provenance to
        // carry. The node= attribute alone is debug noise.
        if page.is_none() && bbox.is_none() {
            return;
        }
        self.out.push_str("<!-- udoc:");
        let mut wrote = false;
        if let Some(p) = page {
            let _ = write!(self.out, "page={}", p);
            wrote = true;
        }
        if let Some(b) = bbox {
            if wrote {
                self.out.push(' ');
            }
            let _ = write!(
                self.out,
                "bbox={:.2},{:.2},{:.2},{:.2}",
                b.x_min, b.y_min, b.x_max, b.y_max
            );
            wrote = true;
        }
        if wrote {
            self.out.push(' ');
        }
        let _ = writeln!(self.out, "node={} -->", id);
    }

    fn emit_block(&mut self, block: &Block) {
        match block {
            Block::Heading { id, level, content } => {
                self.emit_anchor(*id);
                let lvl = (*level).clamp(1, 6) as usize;
                for _ in 0..lvl {
                    self.out.push('#');
                }
                self.out.push(' ');
                self.emit_inlines(content);
                self.out.push('\n');
            }
            Block::Paragraph { id, content } => {
                self.emit_anchor(*id);
                self.emit_inlines(content);
                self.out.push('\n');
            }
            Block::List {
                id,
                items,
                kind,
                start,
            } => {
                self.emit_anchor(*id);
                self.emit_list(items, *kind, *start);
            }
            Block::Table { id, table } => {
                self.emit_anchor(*id);
                self.emit_table(table);
            }
            Block::CodeBlock { id, text, language } => {
                self.emit_anchor(*id);
                self.out.push_str("```");
                if let Some(lang) = language {
                    self.out.push_str(lang);
                }
                self.out.push('\n');
                self.out.push_str(text);
                if !text.ends_with('\n') {
                    self.out.push('\n');
                }
                self.out.push_str("```\n");
            }
            Block::Image {
                id,
                image_ref,
                alt_text,
            } => {
                self.emit_anchor(*id);
                self.emit_image_block(*image_ref, alt_text.as_deref());
            }
            Block::PageBreak { id } => {
                self.emit_anchor(*id);
                // Markdown horizontal rule doubles as a page-break marker:
                // it survives most downstream renderers and is unambiguous.
                self.out.push_str("---\n");
            }
            Block::ThematicBreak { id } => {
                self.emit_anchor(*id);
                self.out.push_str("---\n");
            }
            Block::Section { id, role, children } => {
                // Drop the trailing footnotes / endnotes section: those
                // definitions are pulled from the relationships overlay
                // and emitted in the footnote definition tail. Including
                // them inline would produce duplicate text.
                if matches!(role, Some(SectionRole::Footnotes | SectionRole::Endnotes)) {
                    return;
                }
                self.emit_anchor(*id);
                for (i, child) in children.iter().enumerate() {
                    if i > 0 {
                        self.out.push('\n');
                    }
                    self.emit_block(child);
                }
            }
            Block::Shape {
                id,
                children,
                alt_text,
                ..
            } => {
                self.emit_anchor(*id);
                if let Some(alt) = alt_text {
                    let _ = writeln!(self.out, "_(shape: {})_", alt);
                }
                for (i, child) in children.iter().enumerate() {
                    if i > 0 {
                        self.out.push('\n');
                    }
                    self.emit_block(child);
                }
            }
            // Block is non_exhaustive; future variants are silently
            // dropped from markdown output rather than panicking.
            _ => {}
        }
    }

    fn emit_list(&mut self, items: &[ListItem], kind: ListKind, start: u64) {
        for (i, item) in items.iter().enumerate() {
            match kind {
                ListKind::Unordered => self.out.push_str("- "),
                ListKind::Ordered => {
                    let _ = write!(self.out, "{}. ", start.saturating_add(i as u64));
                }
                // ListKind is non_exhaustive; future variants degrade
                // to unordered rather than panicking.
                _ => self.out.push_str("- "),
            }
            // List items hold blocks. Render them inline-first
            // (paragraph -> inlines without a trailing blank), then
            // anything richer (nested list, code block) on its own line.
            for (j, child) in item.content.iter().enumerate() {
                if j > 0 {
                    self.out.push('\n');
                    self.out.push_str("  ");
                }
                self.emit_list_child(child);
            }
            self.out.push('\n');
        }
    }

    fn emit_list_child(&mut self, block: &Block) {
        match block {
            Block::Paragraph { content, .. } | Block::Heading { content, .. } => {
                self.emit_inlines(content);
            }
            _ => {
                // For nested lists / code blocks etc., fall back to the
                // generic emitter. The leading two-space indent above
                // covers the first line; subsequent lines won't be
                // re-indented but most renderers are forgiving.
                self.emit_block(block);
            }
        }
    }

    fn emit_table(&mut self, table: &TableData) {
        if table.rows.is_empty() {
            return;
        }
        // Markdown pipe tables can't escape `|` or `\n` inside a cell,
        // and can't represent multiple blocks per cell. Drop to HTML
        // when any cell needs richer structure.
        let needs_html = table_needs_html(table);
        if needs_html {
            self.emit_table_html(table);
        } else {
            self.emit_table_markdown(table);
        }
    }

    fn emit_table_markdown(&mut self, table: &TableData) {
        let cols = table.num_columns.max(1);
        for (row_idx, row) in table.rows.iter().enumerate() {
            self.out.push('|');
            for col_idx in 0..cols {
                let cell_text = row
                    .cells
                    .get(col_idx)
                    .map(inline_cell_text)
                    .unwrap_or_default();
                self.out.push(' ');
                self.out.push_str(&cell_text);
                self.out.push(' ');
                self.out.push('|');
            }
            self.out.push('\n');
            // After the first header row, emit the alignment separator.
            // Tables without a header row still need one to be valid GFM,
            // so synthesize a separator after the first row.
            if row_idx == 0 {
                self.out.push('|');
                for _ in 0..cols {
                    self.out.push_str(" --- |");
                }
                self.out.push('\n');
            }
        }
    }

    fn emit_table_html(&mut self, table: &TableData) {
        self.out.push_str("<table>\n");
        for row in &table.rows {
            self.out.push_str("<tr>");
            let tag = if row.is_header { "th" } else { "td" };
            for cell in &row.cells {
                let _ = write!(self.out, "<{tag}>");
                let cell_text = inline_cell_text(cell);
                push_html_escaped(&mut self.out, &cell_text);
                let _ = write!(self.out, "</{tag}>");
            }
            self.out.push_str("</tr>\n");
        }
        self.out.push_str("</table>\n");
    }

    fn emit_image_block(&mut self, image_ref: ImageRef, alt_text: Option<&str>) {
        let alt = alt_text.unwrap_or("");
        if let Some(filename) = image_filename(&self.doc.assets, image_ref) {
            let _ = writeln!(self.out, "![{alt}]({filename})");
        } else {
            // No asset present -- emit an italic fallback rather than a
            // broken markdown image link. Downstream LLM consumers have
            // a clear signal that the image was referenced but its bytes
            // weren't extracted (e.g. layers config stripped images).
            self.out.push_str("_(image)_\n");
        }
    }

    fn emit_inlines(&mut self, inlines: &[Inline]) {
        // Two-pass walker: pass 1 merges adjacent Text nodes that share
        // an identical SpanStyle into one logical run; pass 2 emits via
        // a style-state machine that opens/closes markers only on
        // boundary transitions. This produces the minimal-nesting form
        // (e.g. `_**a b**_` instead of `_**a**_ _**b**_`).
        let runs = merge_runs(inlines);
        let mut state = SpanStyle::default();
        for run in &runs {
            match run {
                Run::Text { text, style } => {
                    let close = close_diff(state, *style);
                    let open = open_diff(state, *style);
                    self.out.push_str(close);
                    self.out.push_str(open);
                    self.out.push_str(text);
                    state = *style;
                }
                Run::Code { text } => {
                    let close = close_diff(state, SpanStyle::default());
                    self.out.push_str(close);
                    state = SpanStyle::default();
                    self.out.push('`');
                    self.out.push_str(text);
                    self.out.push('`');
                }
                Run::Link { url, content } => {
                    let close = close_diff(state, SpanStyle::default());
                    self.out.push_str(close);
                    state = SpanStyle::default();
                    self.out.push('[');
                    self.emit_inlines(content);
                    let _ = write!(self.out, "]({url})");
                }
                Run::Footnote { label } => {
                    let close = close_diff(state, SpanStyle::default());
                    self.out.push_str(close);
                    state = SpanStyle::default();
                    let _ = write!(self.out, "[^{label}]");
                    if !self.footnote_refs.iter().any(|l| l == label) {
                        self.footnote_refs.push(label.clone());
                    }
                }
                Run::InlineImage { image_ref, alt } => {
                    let close = close_diff(state, SpanStyle::default());
                    self.out.push_str(close);
                    state = SpanStyle::default();
                    let alt_str = alt.as_deref().unwrap_or("");
                    if let Some(filename) = image_filename(&self.doc.assets, *image_ref) {
                        let _ = write!(self.out, "![{alt_str}]({filename})");
                    } else {
                        self.out.push_str("_(image)_");
                    }
                }
                Run::SoftBreak => {
                    let close = close_diff(state, SpanStyle::default());
                    self.out.push_str(close);
                    state = SpanStyle::default();
                    self.out.push(' ');
                }
                Run::LineBreak => {
                    let close = close_diff(state, SpanStyle::default());
                    self.out.push_str(close);
                    state = SpanStyle::default();
                    // GFM-style hard break: two trailing spaces + newline.
                    self.out.push_str("  \n");
                }
            }
        }
        // Close any markers left open at the end of the run.
        self.out.push_str(close_diff(state, SpanStyle::default()));
    }

    fn emit_footnote_definitions(&mut self) {
        if self.footnote_refs.is_empty() {
            return;
        }
        let Some(rels) = self.doc.relationships.as_ref() else {
            return;
        };
        let footnotes = rels.footnotes();
        let mut wrote_header = false;
        // Dedupe references on the way out (insertion order preserved).
        let mut seen: HashMap<&str, ()> = HashMap::new();
        for label in &self.footnote_refs {
            if seen.insert(label.as_str(), ()).is_some() {
                continue;
            }
            let Some(def) = footnotes.get(label) else {
                continue;
            };
            if !wrote_header {
                self.out.push('\n');
                wrote_header = true;
            }
            // Render the definition body as plain text (footnotes are
            // typically a paragraph or two; rich block trees are rare
            // and would need indentation rules we don't bother with).
            let body = footnote_body_text(&def.content);
            let _ = writeln!(self.out, "[^{}]: {}", label, body);
        }
    }
}

// ---------------------------------------------------------------------------
// Style nesting
// ---------------------------------------------------------------------------

/// What close markers are needed when transitioning from `from` to `to`.
///
/// Returns a static string slice covering every flag that was on in `from`
/// but is off in `to`. Order is the inverse of [`open_diff`] so nesting
/// stays balanced. `code` is not represented here because [`Inline::Code`]
/// is a separate variant that the dispatcher handles by closing all style
/// before emission.
fn close_diff(from: SpanStyle, to: SpanStyle) -> &'static str {
    let bold_off = from.bold && !to.bold;
    let italic_off = from.italic && !to.italic;
    let strike_off = from.strikethrough && !to.strikethrough;
    let under_off = from.underline && !to.underline;
    let sup_off = from.superscript && !to.superscript;
    let sub_off = from.subscript && !to.subscript;

    // Order: sub, sup, under, strike, bold, italic (inverse of open order).
    // Each combination produces the minimal closing sequence in markdown,
    // with HTML closers preceding markdown ones (HTML opened first).
    match (
        sub_off, sup_off, under_off, strike_off, bold_off, italic_off,
    ) {
        (false, false, false, false, false, false) => "",
        // Single-flag toggles -- the dominant case.
        (false, false, false, false, true, false) => "**",
        (false, false, false, false, false, true) => "_",
        (false, false, false, true, false, false) => "~~",
        (false, false, true, false, false, false) => "</u>",
        (false, true, false, false, false, false) => "</sup>",
        (true, false, false, false, false, false) => "</sub>",
        // Bold + italic together -- emitted as `_**...**_` so close in
        // the inverse order: bold first, then italic.
        (false, false, false, false, true, true) => "**_",
        // Bold + strike: `~~**...**~~`, close bold then strike.
        (false, false, false, true, true, false) => "**~~",
        // Italic + strike: `~~_..._~~`, close italic then strike.
        (false, false, false, true, false, true) => "_~~",
        // Bold + italic + strike: `~~_**...**_~~`.
        (false, false, false, true, true, true) => "**_~~",
        // Anything else: kitchen-sink unwind. Renderer-tolerant; the
        // table above covers the dominant cases. Inline HTML wins
        // ties for unusual nesting scenarios.
        _ => "</u></sup></sub>~~**_",
    }
}

fn open_diff(from: SpanStyle, to: SpanStyle) -> &'static str {
    let bold_on = !from.bold && to.bold;
    let italic_on = !from.italic && to.italic;
    let strike_on = !from.strikethrough && to.strikethrough;
    let under_on = !from.underline && to.underline;
    let sup_on = !from.superscript && to.superscript;
    let sub_on = !from.subscript && to.subscript;

    match (italic_on, bold_on, strike_on, under_on, sup_on, sub_on) {
        (false, false, false, false, false, false) => "",
        // Single-flag toggles.
        (false, true, false, false, false, false) => "**",
        (true, false, false, false, false, false) => "_",
        (false, false, true, false, false, false) => "~~",
        (false, false, false, true, false, false) => "<u>",
        (false, false, false, false, true, false) => "<sup>",
        (false, false, false, false, false, true) => "<sub>",
        // Bold + italic: `_**...**_` (italic outer, bold inner).
        (true, true, false, false, false, false) => "_**",
        // Bold + strike: `~~**...**~~`.
        (false, true, true, false, false, false) => "~~**",
        // Italic + strike.
        (true, false, true, false, false, false) => "~~_",
        // Bold + italic + strike.
        (true, true, true, false, false, false) => "~~_**",
        // Compound edge cases: kitchen-sink open. Mirrors the close
        // counterpart so balance holds even when minimal-nesting fails.
        _ => "<sub><sup><u>~~_**",
    }
}

// ---------------------------------------------------------------------------
// Run merging + helpers
// ---------------------------------------------------------------------------

/// A logical inline run after merging adjacent same-style text nodes.
enum Run<'a> {
    Text {
        text: String,
        style: SpanStyle,
    },
    Code {
        text: &'a str,
    },
    Link {
        url: &'a str,
        content: &'a [Inline],
    },
    Footnote {
        label: String,
    },
    InlineImage {
        image_ref: ImageRef,
        alt: Option<String>,
    },
    SoftBreak,
    LineBreak,
}

fn merge_runs(inlines: &[Inline]) -> Vec<Run<'_>> {
    let mut out: Vec<Run<'_>> = Vec::with_capacity(inlines.len());
    for inline in inlines {
        match inline {
            Inline::Text { text, style, .. } => {
                if let Some(Run::Text {
                    text: prev_text,
                    style: prev_style,
                }) = out.last_mut()
                {
                    if prev_style == style {
                        prev_text.push_str(text);
                        continue;
                    }
                }
                out.push(Run::Text {
                    text: text.clone(),
                    style: *style,
                });
            }
            Inline::Code { text, .. } => out.push(Run::Code {
                text: text.as_str(),
            }),
            Inline::Link { url, content, .. } => out.push(Run::Link {
                url: url.as_str(),
                content,
            }),
            Inline::FootnoteRef { label, .. } => out.push(Run::Footnote {
                label: label.clone(),
            }),
            Inline::InlineImage {
                image_ref,
                alt_text,
                ..
            } => out.push(Run::InlineImage {
                image_ref: *image_ref,
                alt: alt_text.clone(),
            }),
            Inline::SoftBreak { .. } => out.push(Run::SoftBreak),
            Inline::LineBreak { .. } => out.push(Run::LineBreak),
            // Inline is non_exhaustive; new variants are silently
            // dropped rather than panicking. Add a typed Run when a
            // real backend produces one.
            _ => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Table cell rendering
// ---------------------------------------------------------------------------

/// Render a cell's blocks as a single-line plain-text string.
///
/// Block separators collapse to spaces. Pipe-table emission relies on
/// the absence of `\n` and `|`, which `table_needs_html` checks first.
fn inline_cell_text(cell: &udoc_core::document::TableCell) -> String {
    let mut out = String::new();
    for (i, block) in cell.content.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&block.text());
    }
    // Collapse newlines / tabs to spaces so the markdown row stays valid.
    // The HTML fallback path runs before reaching here, so this only
    // catches stragglers (e.g. tabs that the markdown row doesn't mind).
    out.replace(['\n', '\t'], " ")
}

fn table_needs_html(table: &TableData) -> bool {
    for row in &table.rows {
        for cell in &row.cells {
            // Multiple paragraphs / blocks per cell can't be expressed
            // in a markdown pipe row -- the row syntax is single-line.
            if cell.content.len() > 1 {
                return true;
            }
            for block in &cell.content {
                let txt = block.text();
                if txt.contains('\n') || txt.contains('|') {
                    return true;
                }
                // A nested list, table, or code block in a cell also
                // can't fit in one pipe-syntax line.
                if matches!(
                    block,
                    Block::List { .. } | Block::Table { .. } | Block::CodeBlock { .. }
                ) {
                    return true;
                }
            }
        }
    }
    false
}

fn push_html_escaped(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}

// ---------------------------------------------------------------------------
// Footnote body extraction
// ---------------------------------------------------------------------------

fn footnote_body_text(blocks: &[Block]) -> String {
    let mut out = String::new();
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&block.text());
    }
    // Single-line definition; collapse whitespace runs from inline
    // breaks so the `[^label]: ...` line stays valid.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------
// Image filename helpers
// ---------------------------------------------------------------------------

/// Compute a plausible markdown link target for an image asset, or None
/// when the asset doesn't exist in the store. The filename pattern
/// `image-N.{ext}` matches what the CLI's `--out images` mode writes
/// to disk; downstream tooling that processes the markdown alongside an
/// images directory can resolve the link without further mapping.
fn image_filename(assets: &AssetStore, image_ref: ImageRef) -> Option<String> {
    let asset = assets.image(image_ref)?;
    let ext = match asset.filter {
        ImageFilter::Jpeg | ImageFilter::Jpeg2000 => "jpg",
        ImageFilter::Png => "png",
        ImageFilter::Gif => "gif",
        ImageFilter::Tiff | ImageFilter::Ccitt => "tiff",
        ImageFilter::Bmp => "bmp",
        ImageFilter::Jbig2 => "jb2",
        ImageFilter::Emf => "emf",
        ImageFilter::Wmf => "wmf",
        ImageFilter::Raw => "bin",
        // ImageFilter is non_exhaustive.
        _ => "bin",
    };
    Some(format!("image-{}.{}", image_ref.index(), ext))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::document::{
        Block, Document, FootnoteDef, ImageAsset, ImageRef, Inline, ListItem, ListKind, NodeId,
        Presentation, Relationships, SectionRole, ShapeKind, SpanStyle, TableCell, TableData,
        TableRow,
    };
    use udoc_core::geometry::BoundingBox;
    use udoc_core::image::ImageFilter;

    fn doc_with(blocks: Vec<Block>) -> Document {
        let mut d = Document::new();
        d.content = blocks;
        d
    }

    fn text_inline(id: u64, s: &str) -> Inline {
        Inline::Text {
            id: NodeId::new(id),
            text: s.into(),
            style: SpanStyle::default(),
        }
    }

    fn styled_inline(id: u64, s: &str, style: SpanStyle) -> Inline {
        Inline::Text {
            id: NodeId::new(id),
            text: s.into(),
            style,
        }
    }

    fn span_style_with(
        bold: bool,
        italic: bool,
        underline: bool,
        strikethrough: bool,
        superscript: bool,
        subscript: bool,
    ) -> SpanStyle {
        // SpanStyle is non_exhaustive; mutate field-by-field on the
        // default rather than struct-literal init.
        let mut s = SpanStyle::default();
        s.bold = bold;
        s.italic = italic;
        s.underline = underline;
        s.strikethrough = strikethrough;
        s.superscript = superscript;
        s.subscript = subscript;
        s
    }

    // ---- Block variant emission ----

    #[test]
    fn empty_doc_emits_empty_string() {
        let doc = Document::new();
        assert_eq!(markdown(&doc), "");
        assert_eq!(markdown_with_anchors(&doc), "");
    }

    #[test]
    fn paragraph_block() {
        let doc = doc_with(vec![Block::Paragraph {
            id: NodeId::new(0),
            content: vec![text_inline(1, "Hello world")],
        }]);
        assert_eq!(markdown(&doc), "Hello world\n");
    }

    #[test]
    fn heading_block_levels() {
        for lvl in 1u8..=6 {
            let doc = doc_with(vec![Block::Heading {
                id: NodeId::new(0),
                level: lvl,
                content: vec![text_inline(1, "Title")],
            }]);
            let expected = format!("{} Title\n", "#".repeat(lvl as usize));
            assert_eq!(markdown(&doc), expected, "level {}", lvl);
        }
    }

    #[test]
    fn heading_clamps_out_of_range_level() {
        let doc = doc_with(vec![Block::Heading {
            id: NodeId::new(0),
            level: 9,
            content: vec![text_inline(1, "Big")],
        }]);
        assert_eq!(markdown(&doc), "###### Big\n");
    }

    #[test]
    fn unordered_list() {
        let doc = doc_with(vec![Block::List {
            id: NodeId::new(0),
            kind: ListKind::Unordered,
            start: 1,
            items: vec![
                ListItem::new(
                    NodeId::new(1),
                    vec![Block::Paragraph {
                        id: NodeId::new(2),
                        content: vec![text_inline(3, "alpha")],
                    }],
                ),
                ListItem::new(
                    NodeId::new(4),
                    vec![Block::Paragraph {
                        id: NodeId::new(5),
                        content: vec![text_inline(6, "beta")],
                    }],
                ),
            ],
        }]);
        assert_eq!(markdown(&doc), "- alpha\n- beta\n");
    }

    #[test]
    fn ordered_list_respects_start() {
        let doc = doc_with(vec![Block::List {
            id: NodeId::new(0),
            kind: ListKind::Ordered,
            start: 7,
            items: vec![
                ListItem::new(
                    NodeId::new(1),
                    vec![Block::Paragraph {
                        id: NodeId::new(2),
                        content: vec![text_inline(3, "seven")],
                    }],
                ),
                ListItem::new(
                    NodeId::new(4),
                    vec![Block::Paragraph {
                        id: NodeId::new(5),
                        content: vec![text_inline(6, "eight")],
                    }],
                ),
            ],
        }]);
        assert_eq!(markdown(&doc), "7. seven\n8. eight\n");
    }

    fn simple_cell(id: u64, txt_id: u64, s: &str) -> TableCell {
        TableCell::new(
            NodeId::new(id),
            vec![Block::Paragraph {
                id: NodeId::new(txt_id),
                content: vec![text_inline(txt_id + 100, s)],
            }],
        )
    }

    #[test]
    fn table_pipe_syntax() {
        let table = TableData::new(vec![
            TableRow::new(
                NodeId::new(1),
                vec![simple_cell(2, 3, "A"), simple_cell(4, 5, "B")],
            )
            .with_header(),
            TableRow::new(
                NodeId::new(8),
                vec![simple_cell(9, 10, "1"), simple_cell(11, 12, "2")],
            ),
        ]);
        let doc = doc_with(vec![Block::Table {
            id: NodeId::new(0),
            table,
        }]);
        let md = markdown(&doc);
        assert!(md.starts_with("| A | B |\n| --- | --- |\n"), "got: {md}");
        assert!(md.contains("| 1 | 2 |\n"), "got: {md}");
    }

    #[test]
    fn table_html_fallback_for_pipe_in_cell() {
        let cell_with_pipe = TableCell::new(
            NodeId::new(2),
            vec![Block::Paragraph {
                id: NodeId::new(3),
                content: vec![text_inline(4, "x|y")],
            }],
        );
        let table = TableData::new(vec![TableRow::new(
            NodeId::new(1),
            vec![cell_with_pipe, simple_cell(5, 6, "ok")],
        )
        .with_header()]);
        let doc = doc_with(vec![Block::Table {
            id: NodeId::new(0),
            table,
        }]);
        let md = markdown(&doc);
        assert!(md.contains("<table>"), "got: {md}");
        assert!(md.contains("<th>x|y</th>"), "got: {md}");
        assert!(md.contains("<th>ok</th>"), "got: {md}");
        assert!(md.contains("</table>"), "got: {md}");
    }

    #[test]
    fn table_html_fallback_for_newline_in_cell() {
        let cell_multiline = TableCell::new(
            NodeId::new(2),
            vec![
                Block::Paragraph {
                    id: NodeId::new(3),
                    content: vec![text_inline(4, "line one")],
                },
                Block::Paragraph {
                    id: NodeId::new(5),
                    content: vec![text_inline(6, "line two")],
                },
            ],
        );
        let table = TableData::new(vec![TableRow::new(NodeId::new(1), vec![cell_multiline])]);
        let doc = doc_with(vec![Block::Table {
            id: NodeId::new(0),
            table,
        }]);
        let md = markdown(&doc);
        assert!(md.contains("<table>"), "got: {md}");
    }

    #[test]
    fn code_block_with_language() {
        let doc = doc_with(vec![Block::CodeBlock {
            id: NodeId::new(0),
            text: "fn main() {}".into(),
            language: Some("rust".into()),
        }]);
        assert_eq!(markdown(&doc), "```rust\nfn main() {}\n```\n");
    }

    #[test]
    fn code_block_without_language() {
        let doc = doc_with(vec![Block::CodeBlock {
            id: NodeId::new(0),
            text: "raw".into(),
            language: None,
        }]);
        assert_eq!(markdown(&doc), "```\nraw\n```\n");
    }

    #[test]
    fn page_break_emits_horizontal_rule() {
        let doc = doc_with(vec![
            Block::Paragraph {
                id: NodeId::new(0),
                content: vec![text_inline(1, "before")],
            },
            Block::PageBreak { id: NodeId::new(2) },
            Block::Paragraph {
                id: NodeId::new(3),
                content: vec![text_inline(4, "after")],
            },
        ]);
        let md = markdown(&doc);
        assert!(md.contains("before\n"), "got: {md}");
        assert!(md.contains("---\n"), "got: {md}");
        assert!(md.contains("after\n"), "got: {md}");
    }

    #[test]
    fn thematic_break() {
        let doc = doc_with(vec![Block::ThematicBreak { id: NodeId::new(0) }]);
        assert_eq!(markdown(&doc), "---\n");
    }

    #[test]
    fn section_emits_children() {
        let doc = doc_with(vec![Block::Section {
            id: NodeId::new(0),
            role: Some(SectionRole::Article),
            children: vec![
                Block::Heading {
                    id: NodeId::new(1),
                    level: 1,
                    content: vec![text_inline(2, "Hello")],
                },
                Block::Paragraph {
                    id: NodeId::new(3),
                    content: vec![text_inline(4, "Body")],
                },
            ],
        }]);
        let md = markdown(&doc);
        assert!(md.contains("# Hello\n"), "got: {md}");
        assert!(md.contains("Body\n"), "got: {md}");
    }

    #[test]
    fn shape_block_with_alt_text() {
        let doc = doc_with(vec![Block::Shape {
            id: NodeId::new(0),
            kind: ShapeKind::Rectangle,
            children: vec![Block::Paragraph {
                id: NodeId::new(1),
                content: vec![text_inline(2, "label")],
            }],
            alt_text: Some("logo".into()),
        }]);
        let md = markdown(&doc);
        assert!(md.contains("_(shape: logo)_"), "got: {md}");
        assert!(md.contains("label\n"), "got: {md}");
    }

    // ---- Inline style flags ----

    #[test]
    fn inline_bold() {
        let doc = doc_with(vec![Block::Paragraph {
            id: NodeId::new(0),
            content: vec![styled_inline(1, "x", SpanStyle::default().with_bold())],
        }]);
        assert_eq!(markdown(&doc), "**x**\n");
    }

    #[test]
    fn inline_italic() {
        let doc = doc_with(vec![Block::Paragraph {
            id: NodeId::new(0),
            content: vec![styled_inline(1, "x", SpanStyle::default().with_italic())],
        }]);
        assert_eq!(markdown(&doc), "_x_\n");
    }

    #[test]
    fn inline_bold_italic_uses_asymmetric_nesting() {
        let style = SpanStyle::default().with_bold().with_italic();
        let doc = doc_with(vec![Block::Paragraph {
            id: NodeId::new(0),
            content: vec![styled_inline(1, "x", style)],
        }]);
        // Asymmetric `_**x**_` per Domain Expert spec (CommonMark-stable).
        assert_eq!(markdown(&doc), "_**x**_\n");
    }

    #[test]
    fn inline_strike() {
        let style = span_style_with(false, false, false, true, false, false);
        let doc = doc_with(vec![Block::Paragraph {
            id: NodeId::new(0),
            content: vec![styled_inline(1, "x", style)],
        }]);
        assert_eq!(markdown(&doc), "~~x~~\n");
    }

    #[test]
    fn inline_underline_uses_html() {
        let style = span_style_with(false, false, true, false, false, false);
        let doc = doc_with(vec![Block::Paragraph {
            id: NodeId::new(0),
            content: vec![styled_inline(1, "x", style)],
        }]);
        assert_eq!(markdown(&doc), "<u>x</u>\n");
    }

    #[test]
    fn inline_super_sub_use_html() {
        let sup = span_style_with(false, false, false, false, true, false);
        let sub = span_style_with(false, false, false, false, false, true);
        let doc = doc_with(vec![Block::Paragraph {
            id: NodeId::new(0),
            content: vec![
                styled_inline(1, "a", sup),
                text_inline(2, "/"),
                styled_inline(3, "b", sub),
            ],
        }]);
        let md = markdown(&doc);
        assert!(md.contains("<sup>a</sup>"), "got: {md}");
        assert!(md.contains("<sub>b</sub>"), "got: {md}");
    }

    #[test]
    fn inline_code_uses_backticks() {
        let doc = doc_with(vec![Block::Paragraph {
            id: NodeId::new(0),
            content: vec![Inline::Code {
                id: NodeId::new(1),
                text: "foo()".into(),
            }],
        }]);
        assert_eq!(markdown(&doc), "`foo()`\n");
    }

    #[test]
    fn inline_link() {
        let doc = doc_with(vec![Block::Paragraph {
            id: NodeId::new(0),
            content: vec![Inline::Link {
                id: NodeId::new(1),
                url: "https://example.com".into(),
                content: vec![text_inline(2, "click")],
            }],
        }]);
        assert_eq!(markdown(&doc), "[click](https://example.com)\n");
    }

    #[test]
    fn adjacent_same_style_runs_merge() {
        // Two adjacent bold runs should emit as a single `**ab**` not
        // `**a****b**`.
        let bold = SpanStyle::default().with_bold();
        let doc = doc_with(vec![Block::Paragraph {
            id: NodeId::new(0),
            content: vec![styled_inline(1, "a", bold), styled_inline(2, "b", bold)],
        }]);
        assert_eq!(markdown(&doc), "**ab**\n");
    }

    #[test]
    fn mixed_styles_minimal_nesting() {
        // "plain **bold** _italic_ plain"
        let bold = SpanStyle::default().with_bold();
        let italic = SpanStyle::default().with_italic();
        let doc = doc_with(vec![Block::Paragraph {
            id: NodeId::new(0),
            content: vec![
                text_inline(1, "plain "),
                styled_inline(2, "bold", bold),
                text_inline(3, " "),
                styled_inline(4, "italic", italic),
                text_inline(5, " plain"),
            ],
        }]);
        assert_eq!(markdown(&doc), "plain **bold** _italic_ plain\n");
    }

    // ---- Citation anchors ----

    #[test]
    fn anchor_emits_page_and_bbox_when_present() {
        let mut doc = Document::new();
        let id = NodeId::new(42);
        doc.content.push(Block::Paragraph {
            id,
            content: vec![text_inline(43, "hello")],
        });
        let mut pres = Presentation::default();
        pres.page_assignments.set(id, 3);
        pres.geometry
            .set(id, BoundingBox::new(10.0, 20.0, 110.0, 70.0));
        doc.presentation = Some(pres);
        let md = markdown_with_anchors(&doc);
        assert!(
            md.contains("<!-- udoc:page=3 bbox=10.00,20.00,110.00,70.00 node=node:42 -->"),
            "got: {md}",
        );
    }

    #[test]
    fn anchor_omitted_when_no_provenance() {
        let doc = doc_with(vec![Block::Paragraph {
            id: NodeId::new(0),
            content: vec![text_inline(1, "x")],
        }]);
        let md = markdown_with_anchors(&doc);
        assert!(!md.contains("<!--"), "got: {md}");
    }

    #[test]
    fn markdown_strips_anchors_relative_to_with_anchors() {
        let mut doc = Document::new();
        let id = NodeId::new(0);
        doc.content.push(Block::Paragraph {
            id,
            content: vec![text_inline(1, "hello")],
        });
        let mut pres = Presentation::default();
        pres.page_assignments.set(id, 1);
        doc.presentation = Some(pres);
        let with = markdown_with_anchors(&doc);
        let without = markdown(&doc);
        assert!(with.contains("<!-- udoc:"));
        assert!(!without.contains("<!--"));
        // Both should still contain the body.
        assert!(with.contains("hello\n"));
        assert!(without.contains("hello\n"));
    }

    #[test]
    fn image_with_asset_present() {
        let mut doc = Document::new();
        let asset = ImageAsset::new(vec![0xFF, 0xD8], ImageFilter::Jpeg, 100, 50, 8);
        let r = doc.assets.add_image(asset);
        doc.content.push(Block::Image {
            id: NodeId::new(0),
            image_ref: r,
            alt_text: Some("photo".into()),
        });
        let md = markdown(&doc);
        assert!(md.contains("![photo](image-0.jpg)"), "got: {md}");
    }

    #[test]
    fn image_without_asset_falls_back_to_italic() {
        let doc = doc_with(vec![Block::Image {
            id: NodeId::new(0),
            image_ref: ImageRef::new(99),
            alt_text: None,
        }]);
        assert_eq!(markdown(&doc), "_(image)_\n");
    }

    #[test]
    fn inline_image_with_asset() {
        let mut doc = Document::new();
        let asset = ImageAsset::new(vec![0x89, 0x50], ImageFilter::Png, 32, 32, 8);
        let r = doc.assets.add_image(asset);
        doc.content.push(Block::Paragraph {
            id: NodeId::new(0),
            content: vec![Inline::InlineImage {
                id: NodeId::new(1),
                image_ref: r,
                alt_text: Some("icon".into()),
            }],
        });
        let md = markdown(&doc);
        assert!(md.contains("![icon](image-0.png)"), "got: {md}");
    }

    #[test]
    fn footnote_ref_with_definition() {
        let mut doc = Document::new();
        doc.content.push(Block::Paragraph {
            id: NodeId::new(0),
            content: vec![
                text_inline(1, "see "),
                Inline::FootnoteRef {
                    id: NodeId::new(2),
                    label: "1".into(),
                },
                text_inline(3, " below"),
            ],
        });
        let mut rels = Relationships::default();
        rels.add_footnote(
            "1".into(),
            FootnoteDef::new(
                "1".into(),
                vec![Block::Paragraph {
                    id: NodeId::new(10),
                    content: vec![text_inline(11, "the citation")],
                }],
            ),
        );
        doc.relationships = Some(rels);
        let md = markdown(&doc);
        assert!(md.contains("see [^1] below\n"), "got: {md}");
        assert!(md.contains("[^1]: the citation"), "got: {md}");
    }

    #[test]
    fn footnote_ref_without_definition_drops_definition_only() {
        let doc = doc_with(vec![Block::Paragraph {
            id: NodeId::new(0),
            content: vec![Inline::FootnoteRef {
                id: NodeId::new(1),
                label: "missing".into(),
            }],
        }]);
        let md = markdown(&doc);
        // The marker still emits, but no definition tail follows.
        assert!(md.contains("[^missing]"), "got: {md}");
        assert!(
            !md.contains("[^missing]:"),
            "should not fabricate a definition body, got: {md}"
        );
    }

    #[test]
    fn footnotes_section_block_is_dropped() {
        // Backends can attach a Section { role: Footnotes } at the end
        // of doc.content. The emitter pulls definitions from the
        // Relationships overlay; the inline section would duplicate.
        let doc = doc_with(vec![
            Block::Paragraph {
                id: NodeId::new(0),
                content: vec![text_inline(1, "body")],
            },
            Block::Section {
                id: NodeId::new(2),
                role: Some(SectionRole::Footnotes),
                children: vec![Block::Paragraph {
                    id: NodeId::new(3),
                    content: vec![text_inline(4, "DUPLICATE")],
                }],
            },
        ]);
        let md = markdown(&doc);
        assert!(md.contains("body\n"), "got: {md}");
        assert!(!md.contains("DUPLICATE"), "got: {md}");
    }
}
