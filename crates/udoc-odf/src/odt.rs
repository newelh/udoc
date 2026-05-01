//! ODT (office:text) body parser.
//!
//! Walks office:body > office:text and extracts paragraphs, headings, lists,
//! tables, and text boxes. Inline formatting is resolved from text:span
//! style references.

use udoc_containers::xml::namespace::ns;
use udoc_containers::xml::{attr_value, XmlEvent, XmlReader};
use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use crate::error::{Result, ResultExt};
use crate::styles::{OdfStyleMap, ResolvedSpanFlags};

/// Maximum number of body elements to prevent DoS.
const MAX_BODY_ELEMENTS: usize = 1_000_000;

/// Maximum text length per paragraph (10 MB).
const MAX_TEXT_LENGTH: usize = udoc_core::limits::DEFAULT_MAX_TEXT_LENGTH;

/// Maximum number of rows per table to prevent DoS.
const MAX_TABLE_ROWS: usize = 100_000;

/// Maximum number of cells per table row to prevent DoS.
const MAX_CELLS_PER_ROW: usize = 10_000;

/// Maximum number of items per list to prevent DoS.
const MAX_LIST_ITEMS: usize = 100_000;

/// A run of inline text with formatting info.
#[derive(Debug, Clone)]
pub(crate) struct OdtRun {
    pub text: String,
    pub style_name: Option<String>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    /// Underline resolved from style.
    pub underline: Option<bool>,
    /// Strikethrough resolved from style.
    pub strikethrough: Option<bool>,
    /// Hyperlink URL if this run is inside a text:a element.
    pub link_url: Option<String>,
}

/// A paragraph in the ODT body.
#[derive(Debug, Clone)]
pub(crate) struct OdtParagraph {
    pub runs: Vec<OdtRun>,
    pub style_name: Option<String>,
    /// 0 = not a heading, 1-6 = heading level.
    pub heading_level: u8,
}

impl OdtParagraph {
    fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }

    fn is_empty(&self) -> bool {
        self.runs.iter().all(|r| r.text.is_empty())
    }
}

/// A cell in an ODT table.
#[derive(Debug, Clone)]
pub(crate) struct OdtTableCell {
    pub text: String,
    pub col_span: usize,
    pub row_span: usize,
}

/// A row in an ODT table.
#[derive(Debug, Clone)]
pub(crate) struct OdtTableRow {
    pub cells: Vec<OdtTableCell>,
}

/// A table in the ODT body.
#[derive(Debug, Clone)]
pub(crate) struct OdtTable {
    pub rows: Vec<OdtTableRow>,
}

/// A list item containing paragraphs.
#[derive(Debug, Clone)]
pub(crate) struct OdtListItem {
    pub paragraphs: Vec<OdtParagraph>,
}

/// A list in the ODT body.
#[derive(Debug, Clone)]
pub(crate) struct OdtList {
    pub items: Vec<OdtListItem>,
}

/// A body element (paragraph, heading, table, or list) in document order.
#[derive(Debug, Clone)]
pub(crate) enum OdtElement {
    Paragraph(OdtParagraph),
    Table(OdtTable),
    List(OdtList),
}

/// An image reference found in a draw:frame/draw:image element.
#[derive(Debug, Clone)]
pub(crate) struct OdtImageRef {
    /// Path within the ODF ZIP (e.g., "Pictures/image1.png").
    pub href: String,
}

/// Parsed ODT body.
#[derive(Debug)]
pub(crate) struct OdtBody {
    pub elements: Vec<OdtElement>,
    /// Footnote paragraphs collected from text:note elements.
    pub footnotes: Vec<OdtParagraph>,
    /// Endnote paragraphs collected from text:note elements.
    pub endnotes: Vec<OdtParagraph>,
    pub warnings: Vec<String>,
    /// Image references collected from draw:frame/draw:image elements.
    pub image_refs: Vec<OdtImageRef>,
}

/// Maximum number of footnotes/endnotes to prevent DoS.
const MAX_NOTES: usize = 50_000;

/// Maximum number of image references to collect.
const MAX_IMAGES: usize = 10_000;

/// Collectors for footnotes and endnotes encountered inline.
/// Passed through the parsing stack so text:note elements anywhere
/// in the body tree can push their content.
struct NoteCollector {
    footnotes: Vec<OdtParagraph>,
    endnotes: Vec<OdtParagraph>,
}

/// Parse the ODT body from content.xml.
pub(crate) fn parse_odt_body(
    data: &[u8],
    styles: &OdfStyleMap,
    diag: &dyn DiagnosticsSink,
) -> Result<OdtBody> {
    let mut reader = XmlReader::new(data).context("initializing XML parser for ODT body")?;

    let mut elements = Vec::new();
    let mut warnings = Vec::new();
    let mut notes = NoteCollector {
        footnotes: Vec::new(),
        endnotes: Vec::new(),
    };
    let mut image_refs: Vec<OdtImageRef> = Vec::new();
    let mut in_body = false;
    let mut element_count: usize = 0;

    loop {
        let event = reader.next_element().context("parsing ODT body")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                let name = local_name.as_ref();

                if ns_str == ns::OFFICE && name == "text" {
                    in_body = true;
                    continue;
                }

                if !in_body {
                    continue;
                }

                if element_count >= MAX_BODY_ELEMENTS {
                    diag.warning(Warning::new(
                        "OdtMaxElements",
                        format!("body element limit ({MAX_BODY_ELEMENTS}) exceeded, truncating"),
                    ));
                    break;
                }

                match (ns_str, name) {
                    (ns_text, "p") if ns_text == ns::TEXT => {
                        let style_name =
                            attr_value(&attributes, "style-name").map(|s| s.to_string());
                        let para = parse_paragraph(
                            &mut reader,
                            style_name,
                            0,
                            styles,
                            diag,
                            &mut notes,
                            &mut image_refs,
                        )?;
                        if !para.is_empty() {
                            elements.push(OdtElement::Paragraph(para));
                            element_count += 1;
                        }
                    }
                    (ns_text, "h") if ns_text == ns::TEXT => {
                        let style_name =
                            attr_value(&attributes, "style-name").map(|s| s.to_string());
                        let level = attr_value(&attributes, "outline-level")
                            .and_then(|s| s.parse::<u8>().ok())
                            .unwrap_or(1)
                            .clamp(1, 6);
                        let para = parse_paragraph(
                            &mut reader,
                            style_name,
                            level,
                            styles,
                            diag,
                            &mut notes,
                            &mut image_refs,
                        )?;
                        if !para.is_empty() {
                            elements.push(OdtElement::Paragraph(para));
                            element_count += 1;
                        }
                    }
                    (ns_text, "list") if ns_text == ns::TEXT => {
                        let list =
                            parse_list(&mut reader, styles, diag, &mut notes, &mut image_refs)?;
                        if !list.items.is_empty() {
                            elements.push(OdtElement::List(list));
                            element_count += 1;
                        }
                    }
                    (ns_table, "table") if ns_table == ns::TABLE => {
                        let table = parse_table(&mut reader)?;
                        if !table.rows.is_empty() {
                            elements.push(OdtElement::Table(table));
                            element_count += 1;
                        }
                    }
                    (ns_draw, "frame") if ns_draw == ns::DRAW => {
                        // Look for draw:image child to extract image href
                        collect_frame_images(&mut reader, &mut image_refs)?;
                    }
                    (ns_text, "table-of-content") if ns_text == ns::TEXT => {
                        warnings.push("skipping text:table-of-content".to_string());
                        diag.warning(Warning::new(
                            "OdtSkippedToc",
                            "skipping text:table-of-content element",
                        ));
                        crate::styles::skip_element(&mut reader)?;
                    }
                    _ => {}
                }
            }
            XmlEvent::EndElement {
                local_name,
                namespace_uri,
                ..
            } => {
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                if ns_str == ns::OFFICE && local_name.as_ref() == "text" {
                    in_body = false;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(OdtBody {
        elements,
        footnotes: notes.footnotes,
        endnotes: notes.endnotes,
        warnings,
        image_refs,
    })
}

/// Flush accumulated plain text into a run, attaching the active link URL if any.
fn flush_text_buf(text_buf: &mut String, runs: &mut Vec<OdtRun>, link_url: Option<&str>) {
    if !text_buf.is_empty() {
        runs.push(OdtRun {
            text: std::mem::take(text_buf),
            style_name: None,
            bold: None,
            italic: None,
            underline: None,
            strikethrough: None,
            link_url: link_url.map(|s| s.to_string()),
        });
    }
}

/// Parse a text:p or text:h element into an OdtParagraph.
fn parse_paragraph(
    reader: &mut XmlReader<'_>,
    style_name: Option<String>,
    heading_level: u8,
    styles: &OdfStyleMap,
    diag: &dyn DiagnosticsSink,
    notes: &mut NoteCollector,
    image_refs: &mut Vec<OdtImageRef>,
) -> Result<OdtParagraph> {
    let mut runs = Vec::new();
    let mut depth: usize = 1;
    let mut text_buf = String::new();
    let mut text_truncated = false;
    let mut active_link: Option<String> = None;

    loop {
        let event = reader.next_event().context("parsing paragraph")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                depth += 1;
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                let name = local_name.as_ref();

                match (ns_str, name) {
                    (ns_text, "span") if ns_text == ns::TEXT => {
                        // Flush any accumulated text before the span.
                        flush_text_buf(&mut text_buf, &mut runs, active_link.as_deref());

                        let span_style =
                            attr_value(&attributes, "style-name").map(|s| s.to_string());
                        let resolved = resolve_run_style(&span_style, styles);
                        let span_runs = collect_span_runs(
                            reader,
                            &mut depth,
                            &span_style,
                            &resolved,
                            active_link.as_deref(),
                            styles,
                            0,
                        )?;
                        runs.extend(span_runs);
                    }
                    (ns_text, "a") if ns_text == ns::TEXT => {
                        // Flush text before the hyperlink.
                        flush_text_buf(&mut text_buf, &mut runs, active_link.as_deref());
                        // ODF hyperlinks use xlink:href. We match on local name "href"
                        // which is unambiguous on text:a elements (no other href attrs).
                        active_link = attr_value(&attributes, "href").map(|s| s.to_string());
                    }
                    (ns_text, "tab") if ns_text == ns::TEXT && text_buf.len() < MAX_TEXT_LENGTH => {
                        text_buf.push('\t');
                    }
                    (ns_text, "line-break")
                        if ns_text == ns::TEXT && text_buf.len() < MAX_TEXT_LENGTH =>
                    {
                        text_buf.push('\n');
                    }
                    (ns_text, "s") if ns_text == ns::TEXT => {
                        // text:s = space; c attribute = repeat count.
                        let count = attr_value(&attributes, "c")
                            .and_then(|s| s.parse::<usize>().ok())
                            .unwrap_or(1);
                        let remaining = MAX_TEXT_LENGTH.saturating_sub(text_buf.len());
                        let effective = count.min(100).min(remaining);
                        for _ in 0..effective {
                            text_buf.push(' ');
                        }
                        if effective < count.min(100) && !text_truncated {
                            diag.warning(Warning::new(
                                "OdtTextTruncated",
                                format!(
                                    "paragraph text length limit ({MAX_TEXT_LENGTH}) exceeded, \
                                     truncating"
                                ),
                            ));
                            text_truncated = true;
                        }
                    }
                    (ns_text, "note") if ns_text == ns::TEXT => {
                        let note_class =
                            attr_value(&attributes, "note-class").unwrap_or("footnote");
                        parse_note(
                            reader, &mut depth, note_class, styles, diag, notes, image_refs,
                        )?;
                    }
                    (ns_draw, "frame") if ns_draw == ns::DRAW => {
                        // draw:frame can contain draw:text-box (text) and/or
                        // draw:image (image). Collect both.
                        let frame_text =
                            collect_text_box(reader, &mut depth, styles, diag, notes, image_refs)?;
                        if !frame_text.is_empty() && text_buf.len() < MAX_TEXT_LENGTH {
                            text_buf.push_str(&frame_text);
                        }
                    }
                    _ => {}
                }
            }
            XmlEvent::Text(text) | XmlEvent::CData(text) => {
                if text_buf.len() < MAX_TEXT_LENGTH {
                    text_buf.push_str(text.as_ref());
                    if text_buf.len() > MAX_TEXT_LENGTH && !text_truncated {
                        diag.warning(Warning::new(
                            "OdtTextTruncated",
                            format!(
                                "paragraph text length limit ({MAX_TEXT_LENGTH}) exceeded, \
                                 truncating"
                            ),
                        ));
                        text_truncated = true;
                        text_buf.truncate(MAX_TEXT_LENGTH);
                    }
                }
            }
            XmlEvent::EndElement {
                local_name,
                namespace_uri,
                ..
            } => {
                // Check if we are closing a text:a element.
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                if ns_str == ns::TEXT && local_name.as_ref() == "a" {
                    // Flush link text, then clear active_link.
                    flush_text_buf(&mut text_buf, &mut runs, active_link.as_deref());
                    active_link = None;
                }

                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            XmlEvent::Eof => break,
        }
    }

    // Flush any remaining text.
    flush_text_buf(&mut text_buf, &mut runs, active_link.as_deref());

    Ok(OdtParagraph {
        runs,
        style_name,
        heading_level,
    })
}

/// Parse a text:note element and collect its paragraphs into the NoteCollector.
///
/// ODF text:note structure:
///   <text:note text:note-class="footnote|endnote">
///     <text:note-citation>1</text:note-citation>
///     <text:note-body>
///       <text:p>Footnote text here</text:p>
///     </text:note-body>
///   </text:note>
fn parse_note(
    reader: &mut XmlReader<'_>,
    depth: &mut usize,
    note_class: &str,
    styles: &OdfStyleMap,
    diag: &dyn DiagnosticsSink,
    notes: &mut NoteCollector,
    image_refs: &mut Vec<OdtImageRef>,
) -> Result<()> {
    let is_endnote = note_class == "endnote";

    let total_notes = notes.footnotes.len() + notes.endnotes.len();
    if total_notes >= MAX_NOTES {
        diag.warning(Warning::new(
            "OdtNoteLimitReached",
            format!("stopped collecting notes at {MAX_NOTES} limit"),
        ));
        crate::styles::skip_element(reader)?;
        *depth = depth.saturating_sub(1);
        return Ok(());
    }

    // Collect paragraphs into a local vec to avoid borrow conflicts,
    // then append to the appropriate notes vec at the end.
    let mut collected = Vec::new();
    let start_depth = *depth;

    loop {
        let event = reader.next_event().context("parsing text:note")?;
        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                ..
            } => {
                *depth += 1;
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                let name = local_name.as_ref();

                if ns_str == ns::TEXT && name == "note-body" {
                    // Parse paragraphs inside text:note-body.
                    parse_note_body(
                        reader,
                        depth,
                        styles,
                        diag,
                        &mut collected,
                        image_refs,
                        notes,
                    )?;
                }
                // text:note-citation and other children are consumed by the
                // depth tracking; we just ignore their content.
            }
            XmlEvent::EndElement { .. } => {
                *depth = depth.saturating_sub(1);
                if *depth < start_depth {
                    break;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    if is_endnote {
        notes.endnotes.extend(collected);
    } else {
        notes.footnotes.extend(collected);
    }

    Ok(())
}

/// Parse the content of a text:note-body element, extracting paragraphs.
fn parse_note_body(
    reader: &mut XmlReader<'_>,
    depth: &mut usize,
    styles: &OdfStyleMap,
    diag: &dyn DiagnosticsSink,
    collected: &mut Vec<OdtParagraph>,
    image_refs: &mut Vec<OdtImageRef>,
    notes: &mut NoteCollector,
) -> Result<()> {
    let start_depth = *depth;

    loop {
        let event = reader.next_event().context("parsing text:note-body")?;
        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                *depth += 1;
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                let name = local_name.as_ref();

                if ns_str == ns::TEXT && (name == "p" || name == "h") {
                    let style_name = attr_value(&attributes, "style-name").map(|s| s.to_string());
                    let level = if name == "h" {
                        attr_value(&attributes, "outline-level")
                            .and_then(|s| s.parse::<u8>().ok())
                            .unwrap_or(1)
                            .clamp(1, 6)
                    } else {
                        0
                    };
                    let para = parse_paragraph(
                        reader, style_name, level, styles, diag, notes, image_refs,
                    )?;
                    if !para.is_empty() {
                        collected.push(para);
                    }
                    *depth = depth.saturating_sub(1); // parse_paragraph consumed end element.
                }
            }
            XmlEvent::EndElement { .. } => {
                *depth = depth.saturating_sub(1);
                if *depth < start_depth {
                    break;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(())
}

/// Resolve bold/italic/underline/strikethrough from a style name reference.
/// Uses a single parent-chain walk via `resolve_span_flags`.
fn resolve_run_style(style_name: &Option<String>, styles: &OdfStyleMap) -> ResolvedSpanFlags {
    match style_name {
        Some(name) => styles.resolve_span_flags(name),
        None => ResolvedSpanFlags::default(),
    }
}

/// Collect all text content until the current element ends.
/// Returns the accumulated text and updates the depth counter.
fn collect_text(reader: &mut XmlReader<'_>, depth: &mut usize) -> Result<String> {
    let start_depth = *depth;
    let mut text = String::new();

    loop {
        let event = reader.next_event().context("collecting text")?;
        match event {
            XmlEvent::Text(t) | XmlEvent::CData(t) => {
                if text.len() < MAX_TEXT_LENGTH {
                    text.push_str(t.as_ref());
                    if text.len() > MAX_TEXT_LENGTH {
                        text.truncate(MAX_TEXT_LENGTH);
                    }
                }
            }
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                *depth += 1;
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                let name = local_name.as_ref();
                if ns_str == ns::TEXT && text.len() < MAX_TEXT_LENGTH {
                    match name {
                        "tab" => text.push('\t'),
                        "line-break" => text.push('\n'),
                        "s" => {
                            let count = attr_value(&attributes, "c")
                                .and_then(|s| s.parse::<usize>().ok())
                                .unwrap_or(1);
                            let remaining = MAX_TEXT_LENGTH.saturating_sub(text.len());
                            for _ in 0..count.min(100).min(remaining) {
                                text.push(' ');
                            }
                        }
                        _ => {}
                    }
                }
            }
            XmlEvent::EndElement { .. } => {
                *depth = depth.saturating_sub(1);
                if *depth < start_depth {
                    break;
                }
            }
            XmlEvent::Eof => break,
        }
    }

    Ok(text)
}

/// Collect inline runs from inside a text:span element, recognizing nested
/// text:a hyperlinks and nested text:span elements. A single span like
/// `<text:span><text:a href="...">link</text:a> more</text:span>` produces
/// multiple OdtRun entries: one for the hyperlink (with link_url set) and one
/// for the trailing text. All runs inherit the span's style and resolved flags.
/// Nested text:span elements override the outer span's style when present.
fn collect_span_runs(
    reader: &mut XmlReader<'_>,
    depth: &mut usize,
    span_style: &Option<String>,
    resolved: &ResolvedSpanFlags,
    outer_link: Option<&str>,
    styles: &OdfStyleMap,
    nesting: usize,
) -> Result<Vec<OdtRun>> {
    let start_depth = *depth;
    let mut runs = Vec::new();
    let mut text_buf = String::new();

    /// Flush accumulated text into a run with the span style.
    fn flush_span_buf(
        text_buf: &mut String,
        runs: &mut Vec<OdtRun>,
        span_style: &Option<String>,
        resolved: &ResolvedSpanFlags,
        link_url: Option<&str>,
    ) {
        if !text_buf.is_empty() {
            runs.push(OdtRun {
                text: std::mem::take(text_buf),
                style_name: span_style.clone(),
                bold: resolved.bold,
                italic: resolved.italic,
                underline: resolved.underline,
                strikethrough: resolved.strikethrough,
                link_url: link_url.map(|s| s.to_string()),
            });
        }
    }

    loop {
        let event = reader.next_event().context("collecting span content")?;
        match event {
            XmlEvent::Text(t) | XmlEvent::CData(t) => {
                if text_buf.len() < MAX_TEXT_LENGTH {
                    text_buf.push_str(t.as_ref());
                    if text_buf.len() > MAX_TEXT_LENGTH {
                        text_buf.truncate(MAX_TEXT_LENGTH);
                    }
                }
            }
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                *depth += 1;
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                let name = local_name.as_ref();

                if ns_str == ns::TEXT && name == "a" {
                    // Flush any text accumulated before this hyperlink.
                    flush_span_buf(&mut text_buf, &mut runs, span_style, resolved, outer_link);
                    // Collect the hyperlink text and create a run with the link URL.
                    let href = attr_value(&attributes, "href").map(|s| s.to_string());
                    let link_text = collect_text(reader, depth)?;
                    if !link_text.is_empty() {
                        runs.push(OdtRun {
                            text: link_text,
                            style_name: span_style.clone(),
                            bold: resolved.bold,
                            italic: resolved.italic,
                            underline: resolved.underline,
                            strikethrough: resolved.strikethrough,
                            link_url: href,
                        });
                    }
                } else if ns_str == ns::TEXT
                    && name == "span"
                    && nesting < udoc_core::MAX_NESTING_DEPTH
                {
                    // Nested text:span: flush accumulated text, resolve the
                    // inner span's style, and recurse.
                    flush_span_buf(&mut text_buf, &mut runs, span_style, resolved, outer_link);
                    let inner_style = attr_value(&attributes, "style-name").map(|s| s.to_string());
                    let inner_resolved = resolve_run_style(&inner_style, styles);
                    let inner_runs = collect_span_runs(
                        reader,
                        depth,
                        &inner_style,
                        &inner_resolved,
                        outer_link,
                        styles,
                        nesting + 1,
                    )?;
                    runs.extend(inner_runs);
                } else if ns_str == ns::TEXT && text_buf.len() < MAX_TEXT_LENGTH {
                    match name {
                        "tab" => text_buf.push('\t'),
                        "line-break" => text_buf.push('\n'),
                        "s" => {
                            let count = attr_value(&attributes, "c")
                                .and_then(|s| s.parse::<usize>().ok())
                                .unwrap_or(1);
                            let remaining = MAX_TEXT_LENGTH.saturating_sub(text_buf.len());
                            for _ in 0..count.min(100).min(remaining) {
                                text_buf.push(' ');
                            }
                        }
                        _ => {}
                    }
                }
            }
            XmlEvent::EndElement { .. } => {
                *depth = depth.saturating_sub(1);
                if *depth < start_depth {
                    break;
                }
            }
            XmlEvent::Eof => break,
        }
    }

    // Flush any remaining text after the last child element.
    flush_span_buf(&mut text_buf, &mut runs, span_style, resolved, outer_link);

    Ok(runs)
}

/// Collect text from a draw:frame, looking for draw:text-box children.
fn collect_text_box(
    reader: &mut XmlReader<'_>,
    depth: &mut usize,
    styles: &OdfStyleMap,
    diag: &dyn DiagnosticsSink,
    notes: &mut NoteCollector,
    image_refs: &mut Vec<OdtImageRef>,
) -> Result<String> {
    let start_depth = *depth;
    let mut parts = Vec::new();
    let mut total_len: usize = 0;

    loop {
        let event = reader.next_event().context("collecting text-box")?;
        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                *depth += 1;
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                let name = local_name.as_ref();

                if ns_str == ns::DRAW && name == "image" {
                    // Collect draw:image refs from inside frames.
                    if let Some(href) = attr_value(&attributes, "href") {
                        if !href.is_empty() && image_refs.len() < MAX_IMAGES {
                            image_refs.push(OdtImageRef {
                                href: href.to_string(),
                            });
                        }
                    }
                } else if ns_str == ns::TEXT
                    && (name == "p" || name == "h")
                    && total_len < MAX_TEXT_LENGTH
                {
                    let style_name = attr_value(&attributes, "style-name").map(|s| s.to_string());
                    let level = if name == "h" {
                        attr_value(&attributes, "outline-level")
                            .and_then(|s| s.parse::<u8>().ok())
                            .unwrap_or(1)
                    } else {
                        0
                    };
                    let para = parse_paragraph(
                        reader, style_name, level, styles, diag, notes, image_refs,
                    )?;
                    let text = para.text();
                    if !text.is_empty() {
                        total_len = total_len.saturating_add(text.len());
                        parts.push(text);
                    }
                    // parse_paragraph consumed its end element.
                    *depth = depth.saturating_sub(1);
                }
            }
            XmlEvent::EndElement { .. } => {
                *depth = depth.saturating_sub(1);
                if *depth < start_depth {
                    break;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(parts.join(" "))
}

/// Parse a text:list into an OdtList.
fn parse_list(
    reader: &mut XmlReader<'_>,
    styles: &OdfStyleMap,
    diag: &dyn DiagnosticsSink,
    notes: &mut NoteCollector,
    image_refs: &mut Vec<OdtImageRef>,
) -> Result<OdtList> {
    let mut items = Vec::new();
    let mut depth: usize = 1;

    loop {
        let event = reader.next_element().context("parsing list")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                depth += 1;
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                if ns_str == ns::TEXT && local_name.as_ref() == "list-item" {
                    if items.len() < MAX_LIST_ITEMS {
                        let item = parse_list_item(reader, styles, diag, notes, image_refs)?;
                        items.push(item);
                    } else {
                        crate::styles::skip_element(reader)?;
                    }
                    depth = depth.saturating_sub(1); // consumed end element.
                } else if ns_str == ns::TEXT && local_name.as_ref() == "p" {
                    let style_name = attr_value(&attributes, "style-name").map(|s| s.to_string());
                    let para =
                        parse_paragraph(reader, style_name, 0, styles, diag, notes, image_refs)?;
                    // Paragraph directly in list (rare but valid).
                    if !para.is_empty() {
                        items.push(OdtListItem {
                            paragraphs: vec![para],
                        });
                    }
                    depth = depth.saturating_sub(1);
                }
            }
            XmlEvent::EndElement { .. } => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(OdtList { items })
}

/// Parse a text:list-item.
fn parse_list_item(
    reader: &mut XmlReader<'_>,
    styles: &OdfStyleMap,
    diag: &dyn DiagnosticsSink,
    notes: &mut NoteCollector,
    image_refs: &mut Vec<OdtImageRef>,
) -> Result<OdtListItem> {
    let mut paragraphs = Vec::new();
    let mut depth: usize = 1;

    loop {
        let event = reader.next_element().context("parsing list-item")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                depth += 1;
                let ns_str = namespace_uri.as_deref().unwrap_or("");

                if ns_str == ns::TEXT && local_name.as_ref() == "p" {
                    let style_name = attr_value(&attributes, "style-name").map(|s| s.to_string());
                    let para =
                        parse_paragraph(reader, style_name, 0, styles, diag, notes, image_refs)?;
                    if !para.is_empty() {
                        paragraphs.push(para);
                    }
                    depth = depth.saturating_sub(1); // parse_paragraph consumed end element.
                }
            }
            XmlEvent::EndElement { .. } => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(OdtListItem { paragraphs })
}

/// Walk a draw:frame element and collect any draw:image hrefs.
/// Consumes XML until the matching end element for draw:frame.
fn collect_frame_images(
    reader: &mut XmlReader<'_>,
    image_refs: &mut Vec<OdtImageRef>,
) -> Result<()> {
    let mut depth: usize = 1;
    loop {
        match reader
            .next_element()
            .context("parsing draw:frame for images")?
        {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                depth += 1;
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                if ns_str == ns::DRAW && local_name.as_ref() == "image" {
                    if let Some(href) = attr_value(&attributes, "href") {
                        if !href.is_empty() && image_refs.len() < MAX_IMAGES {
                            image_refs.push(OdtImageRef {
                                href: href.to_string(),
                            });
                        }
                    }
                }
            }
            XmlEvent::EndElement { .. } => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Ok(());
                }
            }
            XmlEvent::Eof => return Ok(()),
            _ => {}
        }
    }
}

/// Parse a table:table element.
// Cell text is extracted as plain text without inline formatting.
// Style-based bold/italic in table cells is a known limitation for ODF v1.
fn parse_table(reader: &mut XmlReader<'_>) -> Result<OdtTable> {
    let mut rows = Vec::new();
    let mut depth: usize = 1;

    loop {
        let event = reader.next_element().context("parsing table")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                ..
            } => {
                depth += 1;
                let ns_str = namespace_uri.as_deref().unwrap_or("");

                if ns_str == ns::TABLE && local_name.as_ref() == "table-row" {
                    if rows.len() < MAX_TABLE_ROWS {
                        let row = parse_table_row(reader)?;
                        rows.push(row);
                    } else {
                        crate::styles::skip_element(reader)?;
                    }
                    depth = depth.saturating_sub(1); // consumed end element.
                }
            }
            XmlEvent::EndElement { .. } => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(OdtTable { rows })
}

/// Parse a table:table-row element.
fn parse_table_row(reader: &mut XmlReader<'_>) -> Result<OdtTableRow> {
    let mut cells = Vec::new();
    let mut depth: usize = 1;

    loop {
        let event = reader.next_element().context("parsing table-row")?;

        match event {
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                attributes,
                ..
            } => {
                depth += 1;
                let ns_str = namespace_uri.as_deref().unwrap_or("");

                if ns_str == ns::TABLE && local_name.as_ref() == "table-cell" {
                    if cells.len() < MAX_CELLS_PER_ROW {
                        let col_span = attr_value(&attributes, "number-columns-spanned")
                            .and_then(|s| s.parse::<usize>().ok())
                            .unwrap_or(1);
                        let row_span = attr_value(&attributes, "number-rows-spanned")
                            .and_then(|s| s.parse::<usize>().ok())
                            .unwrap_or(1);

                        let text = collect_cell_text(reader)?;
                        cells.push(OdtTableCell {
                            text,
                            col_span,
                            row_span,
                        });
                    } else {
                        crate::styles::skip_element(reader)?;
                    }
                    depth = depth.saturating_sub(1); // consumed end element.
                } else if ns_str == ns::TABLE && local_name.as_ref() == "covered-table-cell" {
                    // Spanned-over cell, skip.
                    crate::styles::skip_element(reader)?;
                    depth = depth.saturating_sub(1);
                }
            }
            XmlEvent::EndElement { .. } => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    Ok(OdtTableRow { cells })
}

/// Collect text content from a table:table-cell, joining paragraphs with newlines.
fn collect_cell_text(reader: &mut XmlReader<'_>) -> Result<String> {
    let mut parts = Vec::new();
    let mut depth: usize = 1;
    let mut text_buf = String::new();
    let mut total_len: usize = 0;

    loop {
        let event = reader.next_event().context("collecting cell text")?;

        match event {
            XmlEvent::Text(text) | XmlEvent::CData(text) => {
                if total_len < MAX_TEXT_LENGTH {
                    text_buf.push_str(text.as_ref());
                    total_len = total_len.saturating_add(text.as_ref().len());
                }
            }
            XmlEvent::StartElement {
                local_name,
                namespace_uri,
                ..
            } => {
                depth += 1;
                let ns_str = namespace_uri.as_deref().unwrap_or("");
                if ns_str == ns::TEXT && local_name.as_ref() == "p" && !text_buf.is_empty() {
                    // New paragraph in the cell; store previous text.
                    parts.push(std::mem::take(&mut text_buf));
                }
            }
            XmlEvent::EndElement { .. } => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            XmlEvent::Eof => break,
        }
    }

    if !text_buf.is_empty() {
        parts.push(text_buf);
    }

    Ok(parts.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::diagnostics::NullDiagnostics;

    fn empty_styles() -> OdfStyleMap {
        OdfStyleMap::default()
    }

    #[test]
    fn parse_paragraphs() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:p>Hello World</text:p>
      <text:p>Second paragraph</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let body = parse_odt_body(xml, &empty_styles(), &NullDiagnostics).unwrap();
        assert_eq!(body.elements.len(), 2);
        match &body.elements[0] {
            OdtElement::Paragraph(p) => assert_eq!(p.text(), "Hello World"),
            other => panic!("expected Paragraph, got {:?}", other),
        }
    }

    #[test]
    fn parse_headings() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:h text:outline-level="1">Title</text:h>
      <text:h text:outline-level="2">Subtitle</text:h>
      <text:p>Body</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let body = parse_odt_body(xml, &empty_styles(), &NullDiagnostics).unwrap();
        assert_eq!(body.elements.len(), 3);
        match &body.elements[0] {
            OdtElement::Paragraph(p) => {
                assert_eq!(p.heading_level, 1);
                assert_eq!(p.text(), "Title");
            }
            other => panic!("expected Paragraph heading, got {:?}", other),
        }
        match &body.elements[1] {
            OdtElement::Paragraph(p) => assert_eq!(p.heading_level, 2),
            other => panic!("expected Paragraph heading, got {:?}", other),
        }
    }

    #[test]
    fn parse_table() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
    xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0">
  <office:body>
    <office:text>
      <table:table>
        <table:table-row>
          <table:table-cell><text:p>A1</text:p></table:table-cell>
          <table:table-cell><text:p>B1</text:p></table:table-cell>
        </table:table-row>
        <table:table-row>
          <table:table-cell><text:p>A2</text:p></table:table-cell>
          <table:table-cell><text:p>B2</text:p></table:table-cell>
        </table:table-row>
      </table:table>
    </office:text>
  </office:body>
</office:document-content>"#;

        let body = parse_odt_body(xml, &empty_styles(), &NullDiagnostics).unwrap();
        assert_eq!(body.elements.len(), 1);
        match &body.elements[0] {
            OdtElement::Table(t) => {
                assert_eq!(t.rows.len(), 2);
                assert_eq!(t.rows[0].cells[0].text, "A1");
                assert_eq!(t.rows[0].cells[1].text, "B1");
            }
            other => panic!("expected Table, got {:?}", other),
        }
    }

    #[test]
    fn parse_list() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:list>
        <text:list-item><text:p>Item 1</text:p></text:list-item>
        <text:list-item><text:p>Item 2</text:p></text:list-item>
      </text:list>
    </office:text>
  </office:body>
</office:document-content>"#;

        let body = parse_odt_body(xml, &empty_styles(), &NullDiagnostics).unwrap();
        assert_eq!(body.elements.len(), 1);
        match &body.elements[0] {
            OdtElement::List(l) => {
                assert_eq!(l.items.len(), 2);
                assert_eq!(l.items[0].paragraphs[0].text(), "Item 1");
            }
            other => panic!("expected List, got {:?}", other),
        }
    }

    #[test]
    fn parse_footnote_inline() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:p>Body text<text:note text:note-class="footnote">
        <text:note-citation>1</text:note-citation>
        <text:note-body><text:p>This is a footnote.</text:p></text:note-body>
      </text:note> continues.</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let body = parse_odt_body(xml, &empty_styles(), &NullDiagnostics).unwrap();
        assert_eq!(body.elements.len(), 1);
        match &body.elements[0] {
            OdtElement::Paragraph(p) => {
                let text = p.text();
                assert!(text.contains("Body text"), "got: {text}");
                assert!(text.contains("continues."), "got: {text}");
                assert!(
                    !text.contains("footnote."),
                    "note body leaked into paragraph: {text}"
                );
            }
            other => panic!("expected Paragraph, got {:?}", other),
        }
        assert_eq!(body.footnotes.len(), 1);
        assert_eq!(body.footnotes[0].text(), "This is a footnote.");
        assert!(body.endnotes.is_empty());
    }

    #[test]
    fn parse_endnote_inline() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:p>Body text<text:note text:note-class="endnote">
        <text:note-citation>i</text:note-citation>
        <text:note-body><text:p>This is an endnote.</text:p></text:note-body>
      </text:note></text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let body = parse_odt_body(xml, &empty_styles(), &NullDiagnostics).unwrap();
        assert!(body.footnotes.is_empty());
        assert_eq!(body.endnotes.len(), 1);
        assert_eq!(body.endnotes[0].text(), "This is an endnote.");
    }

    #[test]
    fn parse_footnote_and_endnote_together() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:p>First<text:note text:note-class="footnote">
        <text:note-citation>1</text:note-citation>
        <text:note-body><text:p>Fn1</text:p></text:note-body>
      </text:note></text:p>
      <text:p>Second<text:note text:note-class="endnote">
        <text:note-citation>i</text:note-citation>
        <text:note-body><text:p>En1</text:p></text:note-body>
      </text:note></text:p>
      <text:p>Third<text:note text:note-class="footnote">
        <text:note-citation>2</text:note-citation>
        <text:note-body><text:p>Fn2</text:p></text:note-body>
      </text:note></text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let body = parse_odt_body(xml, &empty_styles(), &NullDiagnostics).unwrap();
        assert_eq!(body.elements.len(), 3);
        assert_eq!(body.footnotes.len(), 2);
        assert_eq!(body.footnotes[0].text(), "Fn1");
        assert_eq!(body.footnotes[1].text(), "Fn2");
        assert_eq!(body.endnotes.len(), 1);
        assert_eq!(body.endnotes[0].text(), "En1");
    }

    #[test]
    fn parse_note_with_multiple_paragraphs() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:p>Body<text:note text:note-class="footnote">
        <text:note-citation>1</text:note-citation>
        <text:note-body>
          <text:p>Footnote para 1</text:p>
          <text:p>Footnote para 2</text:p>
        </text:note-body>
      </text:note></text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let body = parse_odt_body(xml, &empty_styles(), &NullDiagnostics).unwrap();
        assert_eq!(body.footnotes.len(), 2);
        assert_eq!(body.footnotes[0].text(), "Footnote para 1");
        assert_eq!(body.footnotes[1].text(), "Footnote para 2");
    }

    #[test]
    fn parse_no_notes() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:p>No notes here</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let body = parse_odt_body(xml, &empty_styles(), &NullDiagnostics).unwrap();
        assert!(body.footnotes.is_empty());
        assert!(body.endnotes.is_empty());
    }

    #[test]
    fn parse_note_default_class_is_footnote() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:p>Body<text:note>
        <text:note-body><text:p>Default class note</text:p></text:note-body>
      </text:note></text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let body = parse_odt_body(xml, &empty_styles(), &NullDiagnostics).unwrap();
        assert_eq!(body.footnotes.len(), 1);
        assert_eq!(body.footnotes[0].text(), "Default class note");
        assert!(body.endnotes.is_empty());
    }

    #[test]
    fn parse_hyperlink() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
    xmlns:xlink="http://www.w3.org/1999/xlink">
  <office:body>
    <office:text>
      <text:p>Before <text:a xlink:href="https://example.com">link text</text:a> after</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let body = parse_odt_body(xml, &empty_styles(), &NullDiagnostics).unwrap();
        assert_eq!(body.elements.len(), 1);
        match &body.elements[0] {
            OdtElement::Paragraph(p) => {
                // Should have 3 runs: "Before ", "link text" (with URL), " after"
                assert_eq!(p.runs.len(), 3);
                assert_eq!(p.runs[0].text, "Before ");
                assert!(p.runs[0].link_url.is_none());
                assert_eq!(p.runs[1].text, "link text");
                assert_eq!(p.runs[1].link_url.as_deref(), Some("https://example.com"));
                assert_eq!(p.runs[2].text, " after");
                assert!(p.runs[2].link_url.is_none());
            }
            other => panic!("expected Paragraph, got {:?}", other),
        }
    }

    #[test]
    fn parse_hyperlink_inside_span() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
    xmlns:xlink="http://www.w3.org/1999/xlink">
  <office:body>
    <office:text>
      <text:p>Before <text:span text:style-name="T1">styled <text:a xlink:href="https://example.com">link</text:a> after link</text:span> end</text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let body = parse_odt_body(xml, &empty_styles(), &NullDiagnostics).unwrap();
        assert_eq!(body.elements.len(), 1);
        match &body.elements[0] {
            OdtElement::Paragraph(p) => {
                // Runs: "Before " (unstyled), "styled " (span), "link" (span+url),
                // " after link" (span), " end" (unstyled)
                assert_eq!(p.runs.len(), 5, "runs: {:?}", p.runs);
                assert_eq!(p.runs[0].text, "Before ");
                assert!(p.runs[0].link_url.is_none());
                assert!(p.runs[0].style_name.is_none());

                assert_eq!(p.runs[1].text, "styled ");
                assert!(p.runs[1].link_url.is_none());
                assert_eq!(p.runs[1].style_name.as_deref(), Some("T1"));

                assert_eq!(p.runs[2].text, "link");
                assert_eq!(p.runs[2].link_url.as_deref(), Some("https://example.com"));
                assert_eq!(p.runs[2].style_name.as_deref(), Some("T1"));

                assert_eq!(p.runs[3].text, " after link");
                assert!(p.runs[3].link_url.is_none());
                assert_eq!(p.runs[3].style_name.as_deref(), Some("T1"));

                assert_eq!(p.runs[4].text, " end");
                assert!(p.runs[4].link_url.is_none());
                assert!(p.runs[4].style_name.is_none());
            }
            other => panic!("expected Paragraph, got {:?}", other),
        }
    }

    #[test]
    fn parse_span_only_hyperlink() {
        // Span contains nothing but a hyperlink (no surrounding text).
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
    xmlns:xlink="http://www.w3.org/1999/xlink">
  <office:body>
    <office:text>
      <text:p><text:span text:style-name="T1"><text:a xlink:href="https://rust-lang.org">Rust</text:a></text:span></text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let body = parse_odt_body(xml, &empty_styles(), &NullDiagnostics).unwrap();
        match &body.elements[0] {
            OdtElement::Paragraph(p) => {
                assert_eq!(p.runs.len(), 1, "runs: {:?}", p.runs);
                assert_eq!(p.runs[0].text, "Rust");
                assert_eq!(p.runs[0].link_url.as_deref(), Some("https://rust-lang.org"));
                assert_eq!(p.runs[0].style_name.as_deref(), Some("T1"));
            }
            other => panic!("expected Paragraph, got {:?}", other),
        }
    }

    #[test]
    fn parse_span_multiple_hyperlinks() {
        // Span with two hyperlinks and text between them.
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
    xmlns:xlink="http://www.w3.org/1999/xlink">
  <office:body>
    <office:text>
      <text:p><text:span text:style-name="T1"><text:a xlink:href="http://a.com">A</text:a> and <text:a xlink:href="http://b.com">B</text:a></text:span></text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let body = parse_odt_body(xml, &empty_styles(), &NullDiagnostics).unwrap();
        match &body.elements[0] {
            OdtElement::Paragraph(p) => {
                assert_eq!(p.runs.len(), 3, "runs: {:?}", p.runs);
                assert_eq!(p.runs[0].text, "A");
                assert_eq!(p.runs[0].link_url.as_deref(), Some("http://a.com"));
                assert_eq!(p.runs[1].text, " and ");
                assert!(p.runs[1].link_url.is_none());
                assert_eq!(p.runs[2].text, "B");
                assert_eq!(p.runs[2].link_url.as_deref(), Some("http://b.com"));
            }
            other => panic!("expected Paragraph, got {:?}", other),
        }
    }
}
