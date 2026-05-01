//! DOCX-to-Document model conversion.
//!
//! Converts the parsed DOCX AST (body elements, styles, numbering,
//! headers/footers, footnotes/endnotes) into the unified Document model.
//! This keeps DOCX internals inside the DOCX crate; the facade calls
//! `docx_to_document` without reaching into parser types.

use std::collections::HashSet;

use udoc_core::backend::FormatBackend;
use udoc_core::convert::{
    alloc_id, build_table_data, propagate_warnings, push_run_inline, register_hyperlink,
    set_block_layout, text_inline, RunData,
};
use udoc_core::diagnostics::DiagnosticsSink;
use udoc_core::document::*;
use udoc_core::error::{Result, ResultExt};

use crate::document::DocxDocument;
use crate::numbering::ListKind as DocxListKind;
use crate::parser::{BodyElement, Endnote, Footnote, Paragraph};
use crate::styles::StyleMap;
use crate::table::{convert_table, DocxTable, VMergeState};

/// Map a DOCX highlight color name to RGB.
/// See ECMA-376 ST_HighlightColor (Section 17.18.40).
fn highlight_to_color(name: &str) -> Option<Color> {
    match name {
        "yellow" => Some(Color::rgb(255, 255, 0)),
        "green" => Some(Color::rgb(0, 255, 0)),
        "cyan" => Some(Color::rgb(0, 255, 255)),
        "magenta" => Some(Color::rgb(255, 0, 255)),
        "blue" => Some(Color::rgb(0, 0, 255)),
        "red" => Some(Color::rgb(255, 0, 0)),
        "darkBlue" => Some(Color::rgb(0, 0, 139)),
        "darkCyan" => Some(Color::rgb(0, 139, 139)),
        "darkGreen" => Some(Color::rgb(0, 100, 0)),
        "darkMagenta" => Some(Color::rgb(139, 0, 139)),
        "darkRed" => Some(Color::rgb(139, 0, 0)),
        "darkYellow" => Some(Color::rgb(128, 128, 0)),
        "darkGray" => Some(Color::rgb(169, 169, 169)),
        "lightGray" => Some(Color::rgb(211, 211, 211)),
        "black" => Some(Color::rgb(0, 0, 0)),
        "white" => Some(Color::rgb(255, 255, 255)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// DOCX-specific conversion logic
// ---------------------------------------------------------------------------

/// Convert a DOCX backend into the unified Document model.
///
/// Iterates body elements directly (not through PageExtractor) to preserve
/// document order and wire heading detection from styles.xml.
/// Includes ancillary content (headers, footers, footnotes, endnotes).
///
/// The `diagnostics` parameter receives parse warnings. This function does
/// not handle page range filtering; the caller is responsible for skipping
/// the call entirely when page 0 is out of range.
pub fn docx_to_document(
    docx: &mut DocxDocument,
    diagnostics: &dyn DiagnosticsSink,
    _max_pages: usize, // DOCX is always 1 logical page; kept for macro signature uniformity
) -> Result<Document> {
    let mut doc = Document::new();
    doc.metadata = FormatBackend::metadata(docx);

    propagate_warnings(docx.warnings(), diagnostics, "DocxParse");

    let styles = docx.styles();
    let numbering = docx.numbering();

    // Dedup set for hyperlink URLs collected during conversion (#142).
    // Replaces the facade's post-hoc tree walk over `Inline::Link` nodes.
    let mut hyperlink_seen: HashSet<String> = HashSet::new();

    // Headers: emit as a named section at the top.
    let header_blocks =
        ancillary_paragraphs_to_blocks(&mut doc, docx.headers(), Some(styles), &mut hyperlink_seen)
            .context("converting headers")?;
    if !header_blocks.is_empty() {
        let section_id = alloc_id(&doc).context("allocating header section id")?;
        doc.content.push(Block::Section {
            id: section_id,
            role: Some(SectionRole::Header),
            children: header_blocks,
        });
    }

    // Convert body elements in document order (paragraphs and tables interleaved).
    // List accumulator: consecutive list paragraphs with the same numId are
    // grouped into a single Block::List.
    let mut pending_list: Option<PendingList> = None;

    for elem in docx.body() {
        match elem {
            BodyElement::Paragraph(para) => {
                // Check if this paragraph is a list item.
                if let Some((ref num_id, ilvl)) = para.num_props {
                    let list_kind = numbering
                        .resolve_list_kind(num_id, ilvl)
                        .unwrap_or(DocxListKind::Unordered);
                    let is_ordered = list_kind == DocxListKind::Ordered;

                    let inlines =
                        paragraph_to_inlines(&mut doc, para, Some(styles), &mut hyperlink_seen)
                            .context("converting list item paragraph")?;
                    if inlines.is_empty() {
                        continue;
                    }
                    let item_id = alloc_id(&doc).context("allocating list item id")?;
                    let para_id = alloc_id(&doc).context("allocating list para id")?;
                    apply_paragraph_layout(&mut doc, para_id, para);
                    let item = ListItem::new(
                        item_id,
                        vec![Block::Paragraph {
                            id: para_id,
                            content: inlines,
                        }],
                    );

                    // Group consecutive list items with the same numId.
                    match pending_list {
                        Some(ref mut pl) if pl.num_id == *num_id => {
                            pl.items.push(item);
                        }
                        _ => {
                            // Flush any previous list.
                            if let Some(pl) = pending_list.take() {
                                flush_pending_list(&mut doc, pl)?;
                            }
                            let start = numbering.resolve_start(num_id, ilvl);
                            pending_list = Some(PendingList {
                                num_id: num_id.clone(),
                                items: vec![item],
                                ordered: is_ordered,
                                start,
                            });
                        }
                    }
                    continue;
                }

                // Not a list item: flush any pending list first.
                if let Some(pl) = pending_list.take() {
                    flush_pending_list(&mut doc, pl)?;
                }

                let inlines =
                    paragraph_to_inlines(&mut doc, para, Some(styles), &mut hyperlink_seen)
                        .context("converting body paragraph")?;
                if inlines.is_empty() {
                    continue;
                }

                let block_id = alloc_id(&doc).context("allocating body block id")?;

                // Heading detection: check direct outlineLvl first, then resolve
                // from style definitions via basedOn chain.
                let heading_level = if let Some(level) = para.outline_level {
                    (level + 1).min(6)
                } else if let Some(ref style_id) = para.style_id {
                    styles.resolve_heading_level(style_id)
                } else {
                    0
                };

                apply_paragraph_layout(&mut doc, block_id, para);

                if heading_level > 0 {
                    doc.content.push(Block::Heading {
                        id: block_id,
                        level: heading_level,
                        content: inlines,
                    });
                } else {
                    doc.content.push(Block::Paragraph {
                        id: block_id,
                        content: inlines,
                    });
                }
            }
            BodyElement::Table(tbl) => {
                // Flush any pending list before the table.
                if let Some(pl) = pending_list.take() {
                    flush_pending_list(&mut doc, pl)?;
                }

                // Collect bookmarks from table cells before converting.
                let cell_bookmarks: Vec<String> = tbl
                    .rows
                    .iter()
                    .flat_map(|r| &r.cells)
                    .flat_map(|c| &c.bookmarks)
                    .cloned()
                    .collect();
                if !cell_bookmarks.is_empty() {
                    let rels = doc.relationships.get_or_insert_with(Relationships::default);
                    for name in cell_bookmarks {
                        rels.add_bookmark(name, BookmarkTarget::Positional);
                    }
                }

                let table_id = alloc_id(&doc).context("allocating table id")?;
                let core_table = convert_table(tbl);
                let rows = convert_docx_table_rows(&mut doc, tbl, &core_table, &mut hyperlink_seen)
                    .context("converting table rows")?;
                let td = build_table_data(rows, &core_table);
                doc.content.push(Block::Table {
                    id: table_id,
                    table: td,
                });
            }
        }
    }

    // Flush any trailing list.
    if let Some(pl) = pending_list.take() {
        flush_pending_list(&mut doc, pl)?;
    }

    // Footnotes are present in both the content tree (as a Section block for
    // readable text output) and the relationships overlay (as FootnoteDef entries
    // for structured access). Convert once and wire both in a single pass.
    convert_and_wire_notes(
        &mut doc,
        docx.footnotes(),
        Some(styles),
        "footnotes",
        "fn",
        &mut hyperlink_seen,
    )?;
    convert_and_wire_notes(
        &mut doc,
        docx.endnotes(),
        Some(styles),
        "endnotes",
        "en",
        &mut hyperlink_seen,
    )?;

    // Footers: emit as a named section at the bottom.
    let footer_blocks =
        ancillary_paragraphs_to_blocks(&mut doc, docx.footers(), Some(styles), &mut hyperlink_seen)
            .context("converting footers")?;
    if !footer_blocks.is_empty() {
        let section_id = alloc_id(&doc).context("allocating footer section id")?;
        doc.content.push(Block::Section {
            id: section_id,
            role: Some(SectionRole::Footer),
            children: footer_blocks,
        });
    }

    // Wire bookmarks into the relationships overlay.
    // DOCX bookmarks mark positions between elements, not specific nodes in our
    // tree model. The value is None meaning "bookmark exists but target node is
    // unresolved."
    if !docx.bookmarks().is_empty() {
        let rels = doc.relationships.get_or_insert_with(Relationships::default);
        for name in docx.bookmarks() {
            rels.add_bookmark(name.clone(), BookmarkTarget::Positional);
        }
    }

    Ok(doc)
}

/// Convert notes (footnotes or endnotes) into both a content Section block
/// and FootnoteDef entries in the relationships overlay in a single pass.
///
/// The `section_role` names the Section ("footnotes" or "endnotes").
/// The `prefix` ("fn" or "en") is prepended to the numeric note ID to avoid
/// collisions when both footnotes and endnotes use the same IDs.
fn convert_and_wire_notes<T: Note>(
    doc: &mut Document,
    notes: &[T],
    styles: Option<&StyleMap>,
    section_role: &str,
    prefix: &str,
    seen_urls: &mut HashSet<String>,
) -> Result<()> {
    let mut section_blocks = Vec::new();

    for note in notes {
        let paras = note.paragraphs();
        if paras.is_empty() {
            continue;
        }
        let label = format!("{}:{}", prefix, note.id());
        let mut note_blocks = Vec::new();
        // Track which source paragraph index produced each block, since
        // paragraphs with empty inlines are skipped and the indices don't
        // align 1:1 with the blocks.
        let mut block_para_indices: Vec<usize> = Vec::new();
        for (i, para) in paras.iter().enumerate() {
            let inlines = paragraph_to_inlines(doc, para, styles, seen_urls)
                .context("converting footnote paragraph")?;
            if !inlines.is_empty() {
                let block_id = alloc_id(doc).context("allocating footnote block id")?;
                apply_paragraph_layout(doc, block_id, para);
                note_blocks.push(Block::Paragraph {
                    id: block_id,
                    content: inlines,
                });
                block_para_indices.push(i);
            }
        }
        if !note_blocks.is_empty() {
            // Structural sharing (#145): drop the deep-clone-with-fresh-IDs
            // copy that used to feed the content spine. Both the FootnoteDef
            // version (inside rels) and the Section version (inside the
            // content spine) share the same NodeIds via Block::clone, so the
            // presentation overlay populated during paragraph_to_inlines
            // applies to both traversal paths without a propagate pass.
            //
            // NodeId uniqueness is a soft invariant: overlays are lookup
            // tables keyed by NodeId, so duplicate IDs across the spine +
            // rels map to the same style without conflict. This saves
            // ~N alloc_ids, ~N overlay propagate walks, and the O(depth)
            // walk inside clone_block_fresh_ids for every footnote block.
            let _ = block_para_indices; // drop no-longer-needed indices
            for block in &note_blocks {
                section_blocks.push(block.clone());
            }
            let rels = doc.relationships.get_or_insert_with(Relationships::default);
            rels.add_footnote(label.clone(), FootnoteDef::new(label, note_blocks));
        }
    }

    if !section_blocks.is_empty() {
        let section_id = alloc_id(doc).context("allocating notes section id")?;
        doc.content.push(Block::Section {
            id: section_id,
            role: Some(SectionRole::from_name(section_role)),
            children: section_blocks,
        });
    }

    Ok(())
}

/// Accumulator for grouping consecutive list paragraphs.
struct PendingList {
    num_id: String,
    items: Vec<ListItem>,
    ordered: bool,
    start: u64,
}

/// Flush a pending list accumulator into the document content.
fn flush_pending_list(doc: &mut Document, pl: PendingList) -> Result<()> {
    let list_id = alloc_id(doc).context("allocating list id")?;
    let kind = if pl.ordered {
        ListKind::Ordered
    } else {
        ListKind::Unordered
    };
    doc.content.push(Block::List {
        id: list_id,
        items: pl.items,
        kind,
        start: pl.start,
    });
    Ok(())
}

/// Convert ancillary paragraph groups (headers, footers) to Block elements.
fn ancillary_paragraphs_to_blocks(
    doc: &mut Document,
    part_groups: &[Vec<Paragraph>],
    styles: Option<&StyleMap>,
    seen_urls: &mut HashSet<String>,
) -> Result<Vec<Block>> {
    let mut blocks = Vec::new();
    for group in part_groups {
        for para in group {
            let inlines = paragraph_to_inlines(doc, para, styles, seen_urls)
                .context("converting ancillary paragraph")?;
            if !inlines.is_empty() {
                let block_id = alloc_id(doc).context("allocating ancillary block id")?;
                apply_paragraph_layout(doc, block_id, para);
                blocks.push(Block::Paragraph {
                    id: block_id,
                    content: inlines,
                });
            }
        }
    }
    Ok(blocks)
}

/// Trait for abstracting over Footnote and Endnote.
trait Note {
    fn id(&self) -> &str;
    fn paragraphs(&self) -> &[Paragraph];
}

impl Note for Footnote {
    fn id(&self) -> &str {
        &self.id
    }
    fn paragraphs(&self) -> &[Paragraph] {
        &self.paragraphs
    }
}

impl Note for Endnote {
    fn id(&self) -> &str {
        &self.id
    }
    fn paragraphs(&self) -> &[Paragraph] {
        &self.paragraphs
    }
}

/// Apply paragraph-level layout (alignment, spacing, indentation) to
/// the presentation overlay if any formatting is set.
fn apply_paragraph_layout(doc: &mut Document, block_id: NodeId, para: &Paragraph) {
    set_block_layout(
        doc,
        block_id,
        BlockLayout::new()
            .alignment(
                para.alignment
                    .as_deref()
                    .and_then(Alignment::from_format_str),
            )
            .indent_left(para.indent_left)
            .indent_right(para.indent_right)
            .space_before(para.space_before)
            .space_after(para.space_after),
    );
}

/// Convert DOCX table rows into document model TableRows.
///
/// Like `convert_table_rows` in udoc-core but hyperlink-aware: when a DOCX
/// table cell carries hyperlinks, the cell content includes `Inline::Link`
/// nodes instead of plain text. The hyperlinks are also wired into the
/// relationships overlay.
fn convert_docx_table_rows(
    doc: &mut Document,
    docx_table: &DocxTable,
    core_table: &udoc_core::table::Table,
    seen_urls: &mut HashSet<String>,
) -> Result<Vec<TableRow>> {
    // Walk DOCX rows and core rows in parallel. They have the same row count
    // but core rows have vMerge Continue cells filtered out.
    let mut result = Vec::with_capacity(core_table.rows.len());

    for (docx_row, core_row) in docx_table.rows.iter().zip(core_table.rows.iter()) {
        let row_id = alloc_id(doc).context("allocating table row id")?;
        let mut cells = Vec::with_capacity(core_row.cells.len());

        // Filter DOCX cells the same way convert_table does: skip vMerge Continue.
        let docx_cells_iter = docx_row
            .cells
            .iter()
            .filter(|c| c.v_merge != VMergeState::Continue);

        for (docx_cell, core_cell) in docx_cells_iter.zip(core_row.cells.iter()) {
            let cell_id = alloc_id(doc).context("allocating table cell id")?;

            let content = if docx_cell.hyperlinks.is_empty() {
                // No hyperlinks: single plain-text paragraph (same as before).
                let para_id = alloc_id(doc).context("allocating table cell paragraph id")?;
                let inline = text_inline(doc, core_cell.text.clone())
                    .context("creating table cell text inline")?;
                vec![Block::Paragraph {
                    id: para_id,
                    content: vec![inline],
                }]
            } else {
                // Has hyperlinks: build inlines with Inline::Link nodes.
                let para_id = alloc_id(doc).context("allocating table cell paragraph id")?;
                let inlines =
                    build_cell_inlines_with_links(doc, &core_cell.text, &docx_cell.hyperlinks)
                        .context("building table cell inlines with hyperlinks")?;

                // Dedup-wire hyperlinks into the relationships overlay (#142).
                for (_, url) in &docx_cell.hyperlinks {
                    register_hyperlink(doc, seen_urls, url);
                }

                vec![Block::Paragraph {
                    id: para_id,
                    content: inlines,
                }]
            };

            let mut tc = TableCell::new(cell_id, content);
            tc.col_span = core_cell.col_span;
            tc.row_span = core_cell.row_span;
            cells.push(tc);
        }

        let mut tr = TableRow::new(row_id, cells);
        tr.is_header = core_row.is_header;
        result.push(tr);
    }

    Ok(result)
}

/// Build inline content for a table cell that contains hyperlinks.
///
/// Splits the cell text into segments: plain text segments and hyperlink
/// segments. Each hyperlink becomes an `Inline::Link` wrapping `Inline::Text`.
/// Remaining text (not part of any hyperlink) becomes plain `Inline::Text`.
fn build_cell_inlines_with_links(
    doc: &Document,
    full_text: &str,
    hyperlinks: &[(String, String)],
) -> Result<Vec<Inline>> {
    let mut inlines = Vec::new();
    let mut remaining = full_text;

    for (display_text, url) in hyperlinks {
        if let Some(pos) = remaining.find(display_text.as_str()) {
            // Emit plain text before the link.
            let before = &remaining[..pos];
            if !before.is_empty() {
                inlines.push(text_inline(doc, before.to_string())?);
            }
            // Emit the link.
            let link_id = alloc_id(doc).context("allocating table cell hyperlink id")?;
            let text_id = alloc_id(doc).context("allocating table cell link text id")?;
            inlines.push(Inline::Link {
                id: link_id,
                url: url.clone(),
                content: vec![Inline::Text {
                    id: text_id,
                    text: display_text.clone(),
                    style: SpanStyle::default(),
                }],
            });
            remaining = &remaining[pos + display_text.len()..];
        }
        // If display_text not found in remaining text, skip this hyperlink.
        // This is a safety fallback for edge cases (e.g. duplicated text).
    }

    // Emit any trailing text after the last link.
    if !remaining.is_empty() {
        inlines.push(text_inline(doc, remaining.to_string())?);
    }

    Ok(inlines)
}

/// Convert a DOCX paragraph's visible runs into document model Inline elements.
/// Applies style inheritance: if a run doesn't specify bold/italic directly,
/// the paragraph style's definition is used as fallback.
///
/// Wires presentation data (font, color) and hyperlinks (Inline::Link) when
/// present on the parsed Run. Hyperlink URLs are registered with
/// `Relationships` via `seen_urls` to avoid a post-hoc tree walk (#142).
fn paragraph_to_inlines(
    doc: &mut Document,
    para: &Paragraph,
    styles: Option<&StyleMap>,
    seen_urls: &mut HashSet<String>,
) -> Result<Vec<Inline>> {
    // Resolve style-level bold/italic once for the paragraph.
    let (style_bold, style_italic) = match (styles, para.style_id.as_deref()) {
        (Some(sm), Some(sid)) => (
            sm.resolve_bold(sid).unwrap_or(false),
            sm.resolve_italic(sid).unwrap_or(false),
        ),
        _ => (false, false),
    };

    let mut inlines = Vec::new();

    for run in para.runs.iter().filter(|r| !r.invisible) {
        let mut style = SpanStyle::default();
        style.bold = run.bold.unwrap_or(style_bold);
        style.italic = run.italic.unwrap_or(style_italic);
        style.underline = run.underline;
        style.strikethrough = run.strikethrough;

        let extended = ExtendedTextStyle::new()
            .font_name(run.font_name.clone())
            .font_size(run.font_size_pts)
            .color(run.color.map(Color::from))
            .background_color(run.highlight.as_deref().and_then(highlight_to_color));

        // Emit footnote/endnote reference marker before the run text so the
        // marker precedes the glyphs it refers to.
        if let Some(ref label) = run.note_ref {
            let ref_id = alloc_id(doc).context("allocating footnote ref id")?;
            inlines.push(Inline::FootnoteRef {
                id: ref_id,
                label: label.clone(),
            });
        }

        push_run_inline(
            doc,
            seen_urls,
            &mut inlines,
            RunData {
                text: &run.text,
                style,
                extended,
                hyperlink_url: run.hyperlink_url.as_deref(),
            },
        )
        .context("emitting DOCX run")?;
    }

    Ok(inlines)
}

/// Maximum recursion depth for walking nested inline content (e.g. Link
/// inside Link). Matches the old MAX_CLONE_DEPTH constant.
#[cfg(test)]
const MAX_CLONE_DEPTH: usize = 64;

/// Clone a block with freshly allocated NodeIds so that the copy can live
/// in the content spine without sharing IDs with the relationships overlay.
///
/// No longer on the hot path after #145 (footnote tree is structurally
/// shared with the rels overlay). Retained for the max-depth regression
/// test and for any future call site that needs NodeId-fresh copies.
///
/// Drop-and-rebuild pattern (#151): `Block::clone()` does the deep
/// structural copy; `reassign_ids_in_block` then walks the copy and
/// replaces every NodeId via `alloc_id`. This is one traversal per clone
/// and zero per-variant match arms, versus the old rebuild-every-variant
/// approach which had ~150 LOC of boilerplate and one error-prone
/// "unhandled variant" arm that had to be kept in sync with every new
/// Block/Inline variant.
///
/// Depth bound (MAX_CLONE_DEPTH) guards against stack overflow on
/// adversarial nested inputs; beyond the limit we stop reassigning IDs
/// (the original IDs stay, which is a conservative choice since depth
/// limits are already a "recover and warn" case).
#[cfg(test)]
fn clone_block_fresh_ids(doc: &Document, block: &Block) -> Result<Block> {
    let mut cloned = block.clone();
    reassign_ids_in_block(doc, &mut cloned, 0)?;
    Ok(cloned)
}

#[cfg(test)]
fn reassign_ids_in_block(doc: &Document, block: &mut Block, depth: usize) -> Result<()> {
    if depth >= MAX_CLONE_DEPTH {
        // Bail out without further recursion. The sub-tree keeps its
        // original NodeIds past the depth limit; this is the same
        // conservative behaviour as the old version, which truncated to
        // an empty Paragraph.
        return Ok(());
    }
    match block {
        Block::Heading { id, content, .. } | Block::Paragraph { id, content, .. } => {
            *id = alloc_id(doc).context("reassigning block id")?;
            for inline in content.iter_mut() {
                reassign_ids_in_inline(doc, inline, depth + 1)?;
            }
        }
        Block::Table { id, table } => {
            *id = alloc_id(doc).context("reassigning block id")?;
            for row in table.rows.iter_mut() {
                row.id = alloc_id(doc).context("reassigning row id")?;
                for cell in row.cells.iter_mut() {
                    cell.id = alloc_id(doc).context("reassigning cell id")?;
                    for child in cell.content.iter_mut() {
                        reassign_ids_in_block(doc, child, depth + 1)?;
                    }
                }
            }
        }
        Block::List { id, items, .. } => {
            *id = alloc_id(doc).context("reassigning block id")?;
            for item in items.iter_mut() {
                item.id = alloc_id(doc).context("reassigning list item id")?;
                for child in item.content.iter_mut() {
                    reassign_ids_in_block(doc, child, depth + 1)?;
                }
            }
        }
        Block::Section { id, children, .. } | Block::Shape { id, children, .. } => {
            *id = alloc_id(doc).context("reassigning block id")?;
            for child in children.iter_mut() {
                reassign_ids_in_block(doc, child, depth + 1)?;
            }
        }
        Block::CodeBlock { id, .. }
        | Block::Image { id, .. }
        | Block::PageBreak { id }
        | Block::ThematicBreak { id } => {
            *id = alloc_id(doc).context("reassigning block id")?;
        }
        // Forward-compat catch-all. Block is #[non_exhaustive]; if a new
        // variant is added without a match arm above, this path leaves
        // the NodeId untouched (duplicate with the source). Flag it.
        #[allow(unreachable_patterns)]
        _ => {
            return Err(udoc_core::error::Error::new(
                "unhandled Block variant in reassign_ids_in_block",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
fn reassign_ids_in_inline(doc: &Document, inline: &mut Inline, depth: usize) -> Result<()> {
    if depth >= MAX_CLONE_DEPTH {
        return Ok(());
    }
    // Every Inline variant has `id: NodeId` as its first field with the
    // same name; grab a mutable reference and bump it.
    match inline {
        Inline::Text { id, .. }
        | Inline::Code { id, .. }
        | Inline::FootnoteRef { id, .. }
        | Inline::InlineImage { id, .. }
        | Inline::SoftBreak { id }
        | Inline::LineBreak { id } => {
            *id = alloc_id(doc).context("reassigning inline id")?;
        }
        Inline::Link { id, content, .. } => {
            *id = alloc_id(doc).context("reassigning inline id")?;
            for nested in content.iter_mut() {
                reassign_ids_in_inline(doc, nested, depth + 1)?;
            }
        }
        // Inline is #[non_exhaustive]; catch future variants.
        #[allow(unreachable_patterns)]
        _ => {
            return Err(udoc_core::error::Error::new(
                "unhandled Inline variant in reassign_ids_in_inline",
            ));
        }
    }
    Ok(())
}

// `propagate_inline_styling` was removed by #145: structural-sharing of
// the footnote block tree means the FootnoteDef + Section copies share
// NodeIds, so the text_styling overlay applies to both traversal paths
// without a separate copy pass.

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_containers::test_util::{build_stored_zip, DOCX_CONTENT_TYPES, DOCX_PACKAGE_RELS};
    use udoc_core::diagnostics::NullDiagnostics;

    #[test]
    fn basic_conversion() {
        let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Hello World</w:t></w:r></w:p>
        <w:p>
            <w:r>
                <w:rPr><w:b/></w:rPr>
                <w:t>Bold text</w:t>
            </w:r>
        </w:p>
    </w:body>
</w:document>"#;

        let data = build_stored_zip(&[
            ("[Content_Types].xml", DOCX_CONTENT_TYPES),
            ("_rels/.rels", DOCX_PACKAGE_RELS),
            ("word/document.xml", document_xml),
        ]);

        let mut docx = DocxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = docx_to_document(&mut docx, &diag, usize::MAX).unwrap();

        // Should have two paragraphs.
        assert_eq!(result.content.len(), 2);
        match &result.content[0] {
            Block::Paragraph { content, .. } => {
                assert_eq!(content.len(), 1);
                match &content[0] {
                    Inline::Text { text, .. } => assert_eq!(text, "Hello World"),
                    other => panic!("expected Text, got {:?}", other),
                }
            }
            other => panic!("expected Paragraph, got {:?}", other),
        }
    }

    #[test]
    fn conversion_with_table() {
        let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Before table</w:t></w:r></w:p>
        <w:tbl>
            <w:tr>
                <w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc>
                <w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc>
            </w:tr>
        </w:tbl>
        <w:p><w:r><w:t>After table</w:t></w:r></w:p>
    </w:body>
</w:document>"#;

        let data = build_stored_zip(&[
            ("[Content_Types].xml", DOCX_CONTENT_TYPES),
            ("_rels/.rels", DOCX_PACKAGE_RELS),
            ("word/document.xml", document_xml),
        ]);

        let mut docx = DocxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = docx_to_document(&mut docx, &diag, usize::MAX).unwrap();

        // paragraph + table + paragraph
        assert_eq!(result.content.len(), 3);
        assert!(matches!(&result.content[0], Block::Paragraph { .. }));
        assert!(matches!(&result.content[1], Block::Table { .. }));
        assert!(matches!(&result.content[2], Block::Paragraph { .. }));
    }

    #[test]
    fn conversion_with_list() {
        let doc_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering"
        Target="numbering.xml"/>
</Relationships>"#;

        let numbering_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:abstractNum w:abstractNumId="0">
        <w:lvl w:ilvl="0">
            <w:start w:val="1"/>
            <w:numFmt w:val="decimal"/>
            <w:lvlText w:val="%1."/>
        </w:lvl>
    </w:abstractNum>
    <w:num w:numId="1">
        <w:abstractNumId w:val="0"/>
    </w:num>
</w:numbering>"#;

        let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p>
            <w:pPr>
                <w:numPr><w:numId w:val="1"/><w:ilvl w:val="0"/></w:numPr>
            </w:pPr>
            <w:r><w:t>First item</w:t></w:r>
        </w:p>
        <w:p>
            <w:pPr>
                <w:numPr><w:numId w:val="1"/><w:ilvl w:val="0"/></w:numPr>
            </w:pPr>
            <w:r><w:t>Second item</w:t></w:r>
        </w:p>
        <w:p><w:r><w:t>After list</w:t></w:r></w:p>
    </w:body>
</w:document>"#;

        let data = build_stored_zip(&[
            ("[Content_Types].xml", DOCX_CONTENT_TYPES),
            ("_rels/.rels", DOCX_PACKAGE_RELS),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml),
            ("word/numbering.xml", numbering_xml),
        ]);

        let mut docx = DocxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = docx_to_document(&mut docx, &diag, usize::MAX).unwrap();

        // Should have a list block + a trailing paragraph.
        assert_eq!(result.content.len(), 2);
        match &result.content[0] {
            Block::List {
                items, kind, start, ..
            } => {
                assert_eq!(*kind, ListKind::Ordered);
                assert_eq!(*start, 1);
                assert_eq!(items.len(), 2);
            }
            other => panic!("expected List, got {:?}", other),
        }
        assert!(matches!(&result.content[1], Block::Paragraph { .. }));
    }

    /// Helper to build a DOCX zip with document.xml and optional document.xml.rels.
    fn make_docx_with_rels(document_xml: &[u8], doc_rels: Option<&[u8]>) -> Vec<u8> {
        let mut entries: Vec<(&str, &[u8])> = vec![
            ("[Content_Types].xml", DOCX_CONTENT_TYPES),
            ("_rels/.rels", DOCX_PACKAGE_RELS),
            ("word/document.xml", document_xml),
        ];
        if let Some(rels) = doc_rels {
            entries.push(("word/_rels/document.xml.rels", rels));
        }
        build_stored_zip(&entries)
    }

    #[test]
    fn test_docx_text_color() {
        let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p>
            <w:r>
                <w:rPr><w:color w:val="FF0000"/></w:rPr>
                <w:t>Red text</w:t>
            </w:r>
        </w:p>
    </w:body>
</w:document>"#;

        let data = make_docx_with_rels(document_xml, None);
        let mut docx = DocxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = docx_to_document(&mut docx, &diag, usize::MAX).unwrap();

        assert_eq!(result.content.len(), 1);
        match &result.content[0] {
            Block::Paragraph { content, .. } => {
                let text_id = content[0].id();
                let pres = result.presentation.as_ref().expect("presentation layer");
                let ext = pres.text_styling.get(text_id).expect("text styling");
                assert_eq!(ext.color, Some(Color::rgb(255, 0, 0)));
            }
            other => panic!("expected Paragraph, got {:?}", other),
        }
    }

    #[test]
    fn test_docx_hyperlink_external() {
        let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
            xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
    <w:body>
        <w:p>
            <w:hyperlink r:id="rId1">
                <w:r><w:t>Click here</w:t></w:r>
            </w:hyperlink>
        </w:p>
    </w:body>
</w:document>"#;

        let doc_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink"
        Target="https://example.com"
        TargetMode="External"/>
</Relationships>"#;

        let data = make_docx_with_rels(document_xml, Some(doc_rels));
        let mut docx = DocxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = docx_to_document(&mut docx, &diag, usize::MAX).unwrap();

        assert_eq!(result.content.len(), 1);
        match &result.content[0] {
            Block::Paragraph { content, .. } => {
                assert_eq!(content.len(), 1);
                match &content[0] {
                    Inline::Link {
                        url,
                        content: children,
                        ..
                    } => {
                        assert_eq!(url, "https://example.com");
                        assert_eq!(children.len(), 1);
                        assert_eq!(children[0].text(), "Click here");
                    }
                    other => panic!("expected Link, got {:?}", other),
                }
            }
            other => panic!("expected Paragraph, got {:?}", other),
        }
    }

    #[test]
    fn test_docx_hyperlink_anchor() {
        let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p>
            <w:hyperlink w:anchor="section1">
                <w:r><w:t>Go to section</w:t></w:r>
            </w:hyperlink>
        </w:p>
    </w:body>
</w:document>"#;

        let data = make_docx_with_rels(document_xml, None);
        let mut docx = DocxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = docx_to_document(&mut docx, &diag, usize::MAX).unwrap();

        match &result.content[0] {
            Block::Paragraph { content, .. } => match &content[0] {
                Inline::Link { url, .. } => {
                    assert_eq!(url, "#section1");
                }
                other => panic!("expected Link, got {:?}", other),
            },
            other => panic!("expected Paragraph, got {:?}", other),
        }
    }

    #[test]
    fn test_docx_strikethrough() {
        let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p>
            <w:r>
                <w:rPr><w:strike/></w:rPr>
                <w:t>Struck text</w:t>
            </w:r>
        </w:p>
    </w:body>
</w:document>"#;

        let data = make_docx_with_rels(document_xml, None);
        let mut docx = DocxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = docx_to_document(&mut docx, &diag, usize::MAX).unwrap();

        match &result.content[0] {
            Block::Paragraph { content, .. } => match &content[0] {
                Inline::Text { style, .. } => {
                    assert!(style.strikethrough);
                }
                other => panic!("expected Text, got {:?}", other),
            },
            other => panic!("expected Paragraph, got {:?}", other),
        }
    }

    #[test]
    fn test_docx_underline() {
        let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p>
            <w:r>
                <w:rPr><w:u w:val="single"/></w:rPr>
                <w:t>Underlined text</w:t>
            </w:r>
        </w:p>
    </w:body>
</w:document>"#;

        let data = make_docx_with_rels(document_xml, None);
        let mut docx = DocxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = docx_to_document(&mut docx, &diag, usize::MAX).unwrap();

        match &result.content[0] {
            Block::Paragraph { content, .. } => match &content[0] {
                Inline::Text { style, .. } => {
                    assert!(style.underline);
                }
                other => panic!("expected Text, got {:?}", other),
            },
            other => panic!("expected Paragraph, got {:?}", other),
        }
    }

    #[test]
    fn test_docx_paragraph_alignment() {
        let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p>
            <w:pPr><w:jc w:val="center"/></w:pPr>
            <w:r><w:t>Centered text</w:t></w:r>
        </w:p>
    </w:body>
</w:document>"#;

        let data = make_docx_with_rels(document_xml, None);
        let mut docx = DocxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = docx_to_document(&mut docx, &diag, usize::MAX).unwrap();

        let block_id = result.content[0].id();
        let pres = result.presentation.as_ref().expect("presentation layer");
        let layout = pres.block_layout.get(block_id).expect("block layout");
        assert_eq!(layout.alignment, Some(Alignment::Center));
    }

    #[test]
    fn test_docx_font_forwarding() {
        let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p>
            <w:r>
                <w:rPr>
                    <w:rFonts w:ascii="Arial"/>
                    <w:sz w:val="28"/>
                </w:rPr>
                <w:t>Styled text</w:t>
            </w:r>
        </w:p>
    </w:body>
</w:document>"#;

        let data = make_docx_with_rels(document_xml, None);
        let mut docx = DocxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = docx_to_document(&mut docx, &diag, usize::MAX).unwrap();

        match &result.content[0] {
            Block::Paragraph { content, .. } => {
                let text_id = content[0].id();
                let pres = result.presentation.as_ref().expect("presentation layer");
                let ext = pres.text_styling.get(text_id).expect("text styling");
                assert_eq!(ext.font_name.as_deref(), Some("Arial"));
                assert_eq!(ext.font_size, Some(14.0)); // 28 half-points = 14 pts
            }
            other => panic!("expected Paragraph, got {:?}", other),
        }
    }

    #[test]
    fn test_docx_footnote() {
        let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p><w:r><w:t>Body text</w:t></w:r></w:p>
    </w:body>
</w:document>"#;

        let doc_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes"
        Target="footnotes.xml"/>
</Relationships>"#;

        let footnotes_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:footnote w:id="0"><w:p><w:r><w:t>separator</w:t></w:r></w:p></w:footnote>
    <w:footnote w:id="1"><w:p><w:r><w:t>continuation</w:t></w:r></w:p></w:footnote>
    <w:footnote w:id="2">
        <w:p><w:r><w:t>This is a footnote.</w:t></w:r></w:p>
    </w:footnote>
</w:footnotes>"#;

        let data = build_stored_zip(&[
            ("[Content_Types].xml", DOCX_CONTENT_TYPES),
            ("_rels/.rels", DOCX_PACKAGE_RELS),
            ("word/document.xml", document_xml),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/footnotes.xml", footnotes_xml),
        ]);

        let mut docx = DocxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = docx_to_document(&mut docx, &diag, usize::MAX).unwrap();

        // Check that the footnote is in the relationships overlay.
        let rels = result.relationships.as_ref().expect("relationships layer");
        assert!(
            rels.footnotes().contains_key("fn:2"),
            "footnote fn:2 should be present"
        );
        let fn_def = &rels.footnotes()["fn:2"];
        assert_eq!(fn_def.label, "fn:2");
        assert!(!fn_def.content.is_empty());
        // Check the footnote content text.
        assert_eq!(fn_def.content[0].text(), "This is a footnote.");
    }

    #[test]
    fn test_docx_footnote_ref_in_body() {
        let document_xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
        <w:p>
            <w:r><w:t>See note</w:t></w:r>
            <w:r>
                <w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr>
                <w:footnoteReference w:id="2"/>
            </w:r>
        </w:p>
    </w:body>
</w:document>"#;

        let data = make_docx_with_rels(document_xml, None);
        let mut docx = DocxDocument::from_bytes(&data).unwrap();
        let diag = NullDiagnostics;
        let result = docx_to_document(&mut docx, &diag, usize::MAX).unwrap();

        assert_eq!(result.content.len(), 1);
        match &result.content[0] {
            Block::Paragraph { content, .. } => {
                // First inline: the "See note" text.
                assert!(
                    matches!(&content[0], Inline::Text { text, .. } if text == "See note"),
                    "expected Text 'See note', got {:?}",
                    content[0]
                );
                // The footnoteReference run produces both an empty Text and a FootnoteRef.
                // Find the FootnoteRef inline.
                let foot_ref = content
                    .iter()
                    .find(|i| matches!(i, Inline::FootnoteRef { .. }));
                match foot_ref {
                    Some(Inline::FootnoteRef { label, .. }) => {
                        assert_eq!(label, "fn:2");
                    }
                    other => panic!("expected FootnoteRef with label 'fn:2', got {:?}", other),
                }
            }
            other => panic!("expected Paragraph, got {:?}", other),
        }
    }

    #[test]
    fn clone_block_fresh_ids_at_max_depth() {
        // Build a deeply nested list structure that exceeds MAX_CLONE_DEPTH.
        let doc = Document::new();
        let mut block = Block::Paragraph {
            id: alloc_id(&doc).unwrap(),
            content: vec![Inline::Text {
                id: alloc_id(&doc).unwrap(),
                text: "leaf".into(),
                style: Default::default(),
            }],
        };
        // Wrap in MAX_CLONE_DEPTH + 1 layers of Section to exceed the limit.
        for _ in 0..=MAX_CLONE_DEPTH {
            block = Block::Section {
                id: alloc_id(&doc).unwrap(),
                role: None,
                children: vec![block],
            };
        }
        let result = clone_block_fresh_ids(&doc, &block).unwrap();
        // Should succeed. After #151, reassignment stops at
        // MAX_CLONE_DEPTH without rewriting deeper NodeIds; the outer
        // structure is preserved and the function returns normally.
        assert!(matches!(result, Block::Section { .. }));
    }
}
