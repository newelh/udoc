//! SlideListWithText parser and text extraction for PPT binary files.
//!
//! Extracts text from the SlideListWithTextContainer records inside the
//! DocumentContainer. These containers group text atoms by slide, driven
//! by SlidePersistAtom markers. Two instances matter:
//! - recInstance 0: slide body text
//! - recInstance 2: notes text
//!
//! Instance 1 (master slides) is skipped.

use std::sync::Arc;

use udoc_core::codepage::encoding_for_codepage;
use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use crate::error::{Error, Result, ResultExt};
use crate::persist::PersistDirectory;
use crate::records::{self, rt, RecordIter, HEADER_SIZE};
use crate::styles;
use crate::{MAX_SLIDES, MAX_TEXT_LENGTH};

/// Text type from TextHeaderAtom, drives heading inference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextType {
    /// Title text (textType = 0).
    Title,
    /// Body text (textType = 1).
    Body,
    /// Notes text (textType = 2).
    Notes,
    /// Other text (textType = 4).
    Other,
    /// Center title text (textType = 5).
    CenterTitle,
    /// Subtitle text (textType = 6).
    Subtitle,
    /// Half body text (textType = 7).
    HalfBody,
}

impl TextType {
    fn from_u32(value: u32) -> Self {
        match value {
            0 => TextType::Title,
            1 => TextType::Body,
            2 => TextType::Notes,
            4 => TextType::Other,
            5 => TextType::CenterTitle,
            6 => TextType::Subtitle,
            7 => TextType::HalfBody,
            _ => TextType::Other,
        }
    }
}

/// A block of text from a single TextCharsAtom or TextBytesAtom.
#[derive(Debug, Clone)]
pub struct TextBlock {
    /// The decoded text content.
    pub text: String,
    /// The type of text (title, body, notes, etc.).
    pub text_type: TextType,
    /// Character-level style runs from a following StyleTextPropAtom, if any.
    pub styles: Option<styles::TextStyle>,
}

/// All text content for a single slide.
#[derive(Debug, Clone)]
pub struct SlideContent {
    /// Persist ID from SlidePersistAtom (psrReference).
    pub persist_id: u32,
    /// Slide identifier from SlidePersistAtom (slideIdentifier, bytes 12-15).
    /// Determines presentation order: slides are sorted ascending by this value.
    pub slide_id: u32,
    /// Text blocks from the slide body SLWT (recInstance 0).
    pub text_blocks: Vec<TextBlock>,
    /// Text blocks from the notes SLWT (recInstance 2).
    pub notes_text: Vec<TextBlock>,
}

/// Decode a TextCharsAtom (UTF-16LE encoded).
fn decode_text_chars(data: &[u8], diag: &Arc<dyn DiagnosticsSink>) -> String {
    let usable = if !data.len().is_multiple_of(2) {
        diag.warning(Warning::new(
            "OddTextCharsLength",
            format!(
                "TextCharsAtom has odd byte count ({}), truncating last byte",
                data.len()
            ),
        ));
        &data[..data.len() - 1]
    } else {
        data
    };

    if usable.len() > MAX_TEXT_LENGTH {
        diag.warning(Warning::new(
            "TextTooLong",
            format!(
                "TextCharsAtom exceeds max length ({} > {}), truncating",
                usable.len(),
                MAX_TEXT_LENGTH
            ),
        ));
        let truncated = &usable[..MAX_TEXT_LENGTH & !1]; // align to u16 boundary
        let (decoded, _, _) = encoding_rs::UTF_16LE.decode(truncated);
        return decoded.into_owned();
    }

    let (decoded, _, had_errors) = encoding_rs::UTF_16LE.decode(usable);
    if had_errors {
        diag.warning(Warning::new(
            "TextCharsDecodeError",
            "TextCharsAtom contained invalid UTF-16LE sequences",
        ));
    }
    decoded.into_owned()
}

/// Decode a TextBytesAtom (CP1252 single-byte encoded).
fn decode_text_bytes(data: &[u8], diag: &Arc<dyn DiagnosticsSink>) -> String {
    let encoding = encoding_for_codepage(1252);

    if data.len() > MAX_TEXT_LENGTH {
        diag.warning(Warning::new(
            "TextTooLong",
            format!(
                "TextBytesAtom exceeds max length ({} > {}), truncating",
                data.len(),
                MAX_TEXT_LENGTH
            ),
        ));
        let (decoded, _, _) = encoding.decode(&data[..MAX_TEXT_LENGTH]);
        return decoded.into_owned();
    }

    let (decoded, _, had_errors) = encoding.decode(data);
    if had_errors {
        diag.warning(Warning::new(
            "TextBytesDecodeError",
            "TextBytesAtom contained invalid CP1252 sequences",
        ));
    }
    decoded.into_owned()
}

/// Parsed fields from a SlidePersistAtom.
struct SlidePersistInfo {
    /// psrReference (persist ID) from bytes 0-3.
    persist_id: u32,
    /// slideIdentifier from bytes 12-15. Determines presentation order.
    /// Zero if the atom is too short to contain this field.
    slide_id: u32,
}

/// Parse a SlidePersistAtom's data region. Extracts psrReference (persist ID)
/// from bytes 0-3 and slideIdentifier from bytes 12-15.
fn parse_slide_persist_atom(data: &[u8]) -> Result<SlidePersistInfo> {
    if data.len() < 4 {
        return Err(Error::new(format!(
            "SlidePersistAtom too short: {} bytes, need at least 4",
            data.len()
        )));
    }
    let persist_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let slide_id = if data.len() >= 16 {
        u32::from_le_bytes([data[12], data[13], data[14], data[15]])
    } else {
        0
    };
    Ok(SlidePersistInfo {
        persist_id,
        slide_id,
    })
}

/// Persist ID + slide identifier pair for a slide.
type SlideKey = (u32, u32);

/// State machine for collecting text blocks within a SLWT container.
struct SlwtCollector {
    /// Completed slides: (persist_id, slide_id, text_blocks).
    slides: Vec<(SlideKey, Vec<TextBlock>)>,
    /// Current slide's (persist_id, slide_id) (None before first SlidePersistAtom).
    current_key: Option<SlideKey>,
    /// Text blocks accumulated for the current slide.
    current_blocks: Vec<TextBlock>,
    /// Most recent TextHeaderAtom type (None if not yet seen or consumed).
    pending_text_type: Option<TextType>,
    /// Character count of the last text atom (for StyleTextPropAtom parsing).
    last_text_char_count: usize,
    /// Orphan blocks seen before the first SlidePersistAtom.
    orphan_blocks: Vec<TextBlock>,
}

impl SlwtCollector {
    fn new() -> Self {
        Self {
            slides: Vec::new(),
            current_key: None,
            current_blocks: Vec::new(),
            pending_text_type: None,
            last_text_char_count: 0,
            orphan_blocks: Vec::new(),
        }
    }

    /// A new SlidePersistAtom was encountered. Flush the current slide
    /// (if any) and start a new one.
    fn start_slide(&mut self, persist_id: u32, slide_id: u32) {
        self.flush_current();
        self.current_key = Some((persist_id, slide_id));
        self.pending_text_type = None;
        self.last_text_char_count = 0;
    }

    /// A TextHeaderAtom was encountered with the given text type.
    fn set_text_type(&mut self, text_type: TextType) {
        self.pending_text_type = Some(text_type);
    }

    /// A text atom (TextCharsAtom or TextBytesAtom) was encountered.
    fn push_text(&mut self, text: String) {
        self.last_text_char_count = text.encode_utf16().count();
        let text_type = self.pending_text_type.take().unwrap_or(TextType::Body);
        let block = TextBlock {
            text,
            text_type,
            styles: None,
        };

        if self.current_key.is_some() {
            self.current_blocks.push(block);
        } else {
            self.orphan_blocks.push(block);
        }
    }

    /// Attach a parsed TextStyle to the most recently pushed text block.
    fn attach_style(&mut self, style: styles::TextStyle) {
        let blocks = if self.current_key.is_some() {
            &mut self.current_blocks
        } else {
            &mut self.orphan_blocks
        };
        if let Some(last) = blocks.last_mut() {
            last.styles = Some(style);
        }
    }

    /// Flush the current slide's blocks into the completed list.
    fn flush_current(&mut self) {
        if let Some(key) = self.current_key.take() {
            let blocks = std::mem::take(&mut self.current_blocks);
            self.slides.push((key, blocks));
        }
    }

    /// Finalize and return all collected slides. Prepends orphan blocks
    /// to the first slide if any exist.
    fn finish(mut self, diag: &Arc<dyn DiagnosticsSink>) -> Vec<(SlideKey, Vec<TextBlock>)> {
        self.flush_current();

        if !self.orphan_blocks.is_empty() {
            if self.slides.is_empty() {
                diag.warning(Warning::new(
                    "OrphanTextBlocks",
                    format!(
                        "found {} text blocks before any SlidePersistAtom with no slides to attach to",
                        self.orphan_blocks.len()
                    ),
                ));
            } else {
                diag.warning(Warning::new(
                    "OrphanTextBlocks",
                    format!(
                        "found {} text blocks before first SlidePersistAtom, prepending to first slide",
                        self.orphan_blocks.len()
                    ),
                ));
                let orphans = std::mem::take(&mut self.orphan_blocks);
                let first = &mut self.slides[0].1;
                let mut merged = orphans;
                merged.append(first);
                *first = merged;
            }
        }

        self.slides
    }
}

/// Process the children of a single SlideListWithTextContainer, collecting
/// text blocks grouped by SlidePersistAtom.
fn process_slwt_children(
    slwt_data: &[u8],
    diag: &Arc<dyn DiagnosticsSink>,
) -> Result<Vec<(SlideKey, Vec<TextBlock>)>> {
    let mut collector = SlwtCollector::new();
    let mut slide_count = 0usize;

    let iter = RecordIter::new(slwt_data);
    for item in iter {
        let (rel_offset, hdr) = item.context("iterating SLWT children")?;

        let data_start = rel_offset
            .checked_add(HEADER_SIZE)
            .ok_or_else(|| Error::new("SLWT child data start overflow"))?;
        let data_end = data_start
            .checked_add(hdr.rec_len as usize)
            .ok_or_else(|| Error::new("SLWT child data end overflow"))?
            .min(slwt_data.len());

        let atom_data = slwt_data.get(data_start..data_end).unwrap_or(&[]);

        match hdr.rec_type {
            rt::SLIDE_PERSIST_ATOM => {
                slide_count += 1;
                if slide_count > MAX_SLIDES {
                    diag.warning(Warning::new(
                        "TooManySlides",
                        format!("exceeded {MAX_SLIDES} slides, stopping SLWT parse"),
                    ));
                    break;
                }

                let info =
                    parse_slide_persist_atom(atom_data).context("parsing SlidePersistAtom")?;
                collector.start_slide(info.persist_id, info.slide_id);
            }

            rt::TEXT_HEADER_ATOM => {
                if atom_data.len() < 4 {
                    diag.warning(Warning::new(
                        "TruncatedTextHeader",
                        format!(
                            "TextHeaderAtom at offset {} has only {} bytes, need 4",
                            rel_offset,
                            atom_data.len()
                        ),
                    ));
                    continue;
                }
                let text_type_val =
                    u32::from_le_bytes([atom_data[0], atom_data[1], atom_data[2], atom_data[3]]);
                collector.set_text_type(TextType::from_u32(text_type_val));
            }

            rt::TEXT_CHARS_ATOM => {
                let text = decode_text_chars(atom_data, diag);
                collector.push_text(text);
            }

            rt::TEXT_BYTES_ATOM => {
                let text = decode_text_bytes(atom_data, diag);
                collector.push_text(text);
            }

            rt::STYLE_TEXT_PROP_ATOM if collector.last_text_char_count > 0 => {
                // PPT implicitly appends a CR to text, so style runs
                // cover text_length + 1 characters.
                let style_text_len = collector.last_text_char_count + 1;
                match styles::parse_style_text_prop(atom_data, style_text_len, diag) {
                    Ok(style) => collector.attach_style(style),
                    Err(e) => {
                        diag.warning(Warning::new(
                            "StyleTextPropParseError",
                            format!("failed to parse StyleTextPropAtom: {e}"),
                        ));
                    }
                }
            }

            _ => {}
        }
    }

    Ok(collector.finish(diag))
}

/// Extract text blocks from a SlideContainer by recursively walking its shape tree.
///
/// Real-world PPT files often store text inside per-slide ShapeContainers
/// (SlideContainer -> PPDrawing -> ShapeGroup -> Shape -> ClientTextbox)
/// rather than in the centralized SlideListWithText. This function handles
/// that case by scanning the slide record for TextHeaderAtom + TextBytesAtom/
/// TextCharsAtom pairs anywhere in the tree.
fn extract_text_from_slide_container(
    ppt_stream: &[u8],
    slide_offset: usize,
    diag: &Arc<dyn DiagnosticsSink>,
) -> Vec<TextBlock> {
    let mut blocks = Vec::new();
    let mut pending_text_type: Option<TextType> = None;

    // Read the container header at slide_offset
    let hdr = match records::read_record_header(ppt_stream, slide_offset) {
        Ok(h) => h,
        Err(_) => return blocks,
    };

    if !hdr.is_container() {
        return blocks;
    }

    let data_start = match slide_offset.checked_add(HEADER_SIZE) {
        Some(s) => s,
        None => return blocks,
    };
    let data_end = data_start
        .checked_add(hdr.rec_len as usize)
        .unwrap_or(ppt_stream.len())
        .min(ppt_stream.len());
    let container_data = match ppt_stream.get(data_start..data_end) {
        Some(d) => d,
        None => return blocks,
    };

    // Recursive scan for text atoms within any nested containers.
    extract_text_recursive(container_data, diag, &mut blocks, &mut pending_text_type, 0);

    blocks
}

/// Recursively scan a byte region for TextHeaderAtom + TextBytesAtom/TextCharsAtom pairs.
fn extract_text_recursive(
    data: &[u8],
    diag: &Arc<dyn DiagnosticsSink>,
    blocks: &mut Vec<TextBlock>,
    pending_text_type: &mut Option<TextType>,
    depth: usize,
) {
    if depth > crate::MAX_RECORD_DEPTH {
        return;
    }

    let iter = RecordIter::new(data);
    for item in iter {
        let (rel_offset, hdr) = match item {
            Ok(v) => v,
            Err(e) => {
                diag.warning(Warning::new(
                    "ShapeTreeRecordError",
                    format!("error iterating shape tree at depth {depth}: {e}"),
                ));
                break;
            }
        };

        let atom_start = match rel_offset.checked_add(HEADER_SIZE) {
            Some(s) => s,
            None => break,
        };
        let atom_end = atom_start
            .checked_add(hdr.rec_len as usize)
            .unwrap_or(data.len())
            .min(data.len());
        let atom_data = data.get(atom_start..atom_end).unwrap_or(&[]);

        match hdr.rec_type {
            rt::TEXT_HEADER_ATOM => {
                if atom_data.len() >= 4 {
                    let tt = u32::from_le_bytes([
                        atom_data[0],
                        atom_data[1],
                        atom_data[2],
                        atom_data[3],
                    ]);
                    *pending_text_type = Some(TextType::from_u32(tt));
                }
            }
            rt::TEXT_CHARS_ATOM => {
                let text = decode_text_chars(atom_data, diag);
                let text_type = pending_text_type.take().unwrap_or(TextType::Body);
                blocks.push(TextBlock {
                    text,
                    text_type,
                    styles: None,
                });
            }
            rt::TEXT_BYTES_ATOM => {
                let text = decode_text_bytes(atom_data, diag);
                let text_type = pending_text_type.take().unwrap_or(TextType::Body);
                blocks.push(TextBlock {
                    text,
                    text_type,
                    styles: None,
                });
            }
            _ => {
                if hdr.is_container() {
                    extract_text_recursive(atom_data, diag, blocks, pending_text_type, depth + 1);
                }
            }
        }
    }
}

/// Find the DocumentContainer offset using the doc_persist_id from UserEditAtom.
fn find_document_container_offset(ppt_stream: &[u8], persist: &PersistDirectory) -> Result<usize> {
    let doc_id = persist.doc_persist_id;
    let offset = persist.get(doc_id).ok_or_else(|| {
        Error::new(format!(
            "persist ID {doc_id} (DocumentContainer) not found in persist directory"
        ))
    })?;
    let offset = offset as usize;
    if offset >= ppt_stream.len() {
        return Err(Error::new(format!(
            "DocumentContainer offset {} exceeds stream length {}",
            offset,
            ppt_stream.len()
        )));
    }
    Ok(offset)
}

/// Resolve a notes SLWT entry's persist ID to the slide persist ID it belongs to.
///
/// Looks up the NotesContainer at the notes persist ID's stream offset, finds
/// the NotesAtom child record, and reads its slideIdRef field (bytes 0-3).
/// Falls back to None if the chain can't be resolved.
fn resolve_notes_slide_id(
    ppt_stream: &[u8],
    persist: &PersistDirectory,
    notes_persist_id: u32,
    diag: &Arc<dyn DiagnosticsSink>,
) -> Option<u32> {
    let offset = persist.get(notes_persist_id)? as usize;
    if offset >= ppt_stream.len() {
        diag.warning(Warning::new(
            "NotesResolveOutOfBounds",
            format!(
                "notes persist ID {notes_persist_id} offset {offset} exceeds stream length {}",
                ppt_stream.len()
            ),
        ));
        return None;
    }

    let hdr = match records::read_record_header(ppt_stream, offset) {
        Ok(h) => h,
        Err(_) => {
            diag.warning(Warning::new(
                "NotesResolveHeaderError",
                format!(
                    "failed to read record header at offset {offset} for notes persist ID {notes_persist_id}"
                ),
            ));
            return None;
        }
    };
    if hdr.rec_type != rt::NOTES || !hdr.is_container() {
        diag.warning(Warning::new(
            "NotesResolveTypeMismatch",
            format!(
                "notes persist ID {notes_persist_id} at offset {offset} is not a NotesContainer \
                 (recType=0x{:04X}, expected 0x{:04X})",
                hdr.rec_type,
                rt::NOTES
            ),
        ));
        return None;
    }

    // Scan the NotesContainer children for NotesAtom.
    let data_start = offset.checked_add(HEADER_SIZE)?;
    let data_end = data_start
        .checked_add(hdr.rec_len as usize)?
        .min(ppt_stream.len());
    let notes_data = ppt_stream.get(data_start..data_end)?;

    for item in RecordIter::new(notes_data) {
        let (rel, child_hdr) = match item {
            Ok(v) => v,
            Err(e) => {
                diag.warning(Warning::new(
                    "NotesResolveRecordError",
                    format!(
                        "error reading NotesContainer child for persist ID {notes_persist_id}: {e}"
                    ),
                ));
                return None;
            }
        };
        if child_hdr.rec_type == rt::NOTES_ATOM {
            let atom_start = rel.checked_add(HEADER_SIZE)?;
            let atom_data = notes_data.get(atom_start..atom_start.checked_add(4)?)?;
            let slide_id =
                u32::from_le_bytes([atom_data[0], atom_data[1], atom_data[2], atom_data[3]]);
            return Some(slide_id);
        }
    }

    None
}

/// Extract text from all slides in a PPT stream.
///
/// Navigates to the DocumentContainer via persist ID 0, finds
/// SlideListWithTextContainer records, and extracts text grouped by slide.
///
/// Returns slides ordered by their appearance in the SLWT.
pub fn extract_slides(
    ppt_stream: &[u8],
    persist: &PersistDirectory,
    diag: &Arc<dyn DiagnosticsSink>,
) -> Result<Vec<SlideContent>> {
    let doc_offset = find_document_container_offset(ppt_stream, persist)
        .context("locating DocumentContainer")?;

    let doc_header = records::read_record_header(ppt_stream, doc_offset)
        .context("reading DocumentContainer header")?;

    if doc_header.rec_type != rt::DOCUMENT {
        return Err(Error::new(format!(
            "expected DocumentContainer (0x{:04X}) at offset {}, found 0x{:04X}",
            rt::DOCUMENT,
            doc_offset,
            doc_header.rec_type,
        )));
    }

    if !doc_header.is_container() {
        return Err(Error::new(format!(
            "DocumentContainer at offset {} is not a container record (rec_ver={})",
            doc_offset, doc_header.rec_ver,
        )));
    }

    // Walk children of the DocumentContainer looking for SLWT records.
    let doc_children = records::children(ppt_stream, doc_offset, &doc_header)
        .context("iterating DocumentContainer children")?;

    let doc_data_start = doc_offset
        .checked_add(HEADER_SIZE)
        .ok_or_else(|| Error::new("DocumentContainer data start overflow"))?;
    let doc_data_end = doc_data_start
        .checked_add(doc_header.rec_len as usize)
        .ok_or_else(|| Error::new("DocumentContainer data end overflow"))?
        .min(ppt_stream.len());
    let doc_data = ppt_stream.get(doc_data_start..doc_data_end).unwrap_or(&[]);

    let mut slide_text_map: Vec<(SlideKey, Vec<TextBlock>)> = Vec::new();
    let mut notes_text_map: Vec<(SlideKey, Vec<TextBlock>)> = Vec::new();

    for item in doc_children {
        let (rel_offset, hdr) = item.context("iterating DocumentContainer children")?;

        if hdr.rec_type != rt::SLIDE_LIST_WITH_TEXT {
            continue;
        }

        let slwt_data_start = rel_offset
            .checked_add(HEADER_SIZE)
            .ok_or_else(|| Error::new("SLWT data start overflow"))?;
        let slwt_data_end = slwt_data_start
            .checked_add(hdr.rec_len as usize)
            .ok_or_else(|| Error::new("SLWT data end overflow"))?
            .min(doc_data.len());

        let slwt_child_data = doc_data.get(slwt_data_start..slwt_data_end).unwrap_or(&[]);

        match hdr.rec_instance {
            0 => {
                let slides = process_slwt_children(slwt_child_data, diag)
                    .context("processing slide SLWT")?;
                slide_text_map.extend(slides);
            }
            2 => {
                let notes = process_slwt_children(slwt_child_data, diag)
                    .context("processing notes SLWT")?;
                notes_text_map.extend(notes);
            }
            _ => {
                // Instance 1 = master slides, skip.
            }
        }
    }

    // Build SlideContent entries from slide text.
    let mut result: Vec<SlideContent> = slide_text_map
        .into_iter()
        .map(|((persist_id, slide_id), text_blocks)| SlideContent {
            persist_id,
            slide_id,
            text_blocks,
            notes_text: Vec::new(),
        })
        .collect();

    // Fallback for slides with no text in the SLWT: extract text from
    // the individual SlideContainer records via the persist directory.
    // Real-world PPT files often store text in per-slide shape containers
    // rather than the centralized SLWT.
    for slide in &mut result {
        if slide.text_blocks.is_empty() {
            if let Some(offset) = persist.get(slide.persist_id) {
                let blocks = extract_text_from_slide_container(ppt_stream, offset as usize, diag);
                if !blocks.is_empty() {
                    slide.text_blocks = blocks;
                }
            }
        }
    }

    // Reorder slides by slideIdentifier to match presentation order.
    // The SLWT stores slides in creation order, but slideIdentifier
    // reflects the actual presentation sequence. Only reorder when all
    // slide_id values are non-zero and distinct (well-formed atoms).
    if result.len() > 1 {
        let all_valid = result.iter().all(|s| s.slide_id != 0);
        let all_distinct = if all_valid {
            let mut seen = std::collections::HashSet::with_capacity(result.len());
            result.iter().all(|s| seen.insert(s.slide_id))
        } else {
            false
        };
        if all_valid && all_distinct {
            result.sort_by_key(|s| s.slide_id);
        }
    }

    // Attach notes to slides when present.
    if !notes_text_map.is_empty() {
        // Build maps for notes matching. NotesAtom.slideIdRef references
        // the slide's slideIdentifier (slide_id), not persist_id. Try
        // slide_id first, fall back to persist_id, then positional.
        let slide_idx_by_id: std::collections::HashMap<u32, usize> = result
            .iter()
            .enumerate()
            .filter(|(_, sc)| sc.slide_id != 0)
            .map(|(i, sc)| (sc.slide_id, i))
            .collect();
        let slide_idx_by_persist: std::collections::HashMap<u32, usize> = result
            .iter()
            .enumerate()
            .map(|(i, sc)| (sc.persist_id, i))
            .collect();

        // Try slide_id-based matching first (NotesContainer -> NotesAtom
        // -> slideIdRef matches slideIdentifier), then persist_id, then positional.
        for (i, ((notes_persist_id, _), notes_blocks)) in notes_text_map.into_iter().enumerate() {
            if let Some(slide_ref) =
                resolve_notes_slide_id(ppt_stream, persist, notes_persist_id, diag)
            {
                if let Some(&idx) = slide_idx_by_id
                    .get(&slide_ref)
                    .or_else(|| slide_idx_by_persist.get(&slide_ref))
                {
                    result[idx].notes_text = notes_blocks;
                    continue;
                }
            }

            // Fallback: positional matching (notes entry i -> slide i).
            if i < result.len() {
                result[i].notes_text = notes_blocks;
            } else {
                diag.warning(Warning::new(
                    "UnmatchedNotesText",
                    format!(
                        "notes SLWT has more entries ({}) than slide SLWT ({})",
                        i + 1,
                        result.len()
                    ),
                ));
                break;
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use udoc_core::diagnostics::{CollectingDiagnostics, DiagnosticsSink, NullDiagnostics};

    use super::*;
    use crate::test_util::*;

    fn null_diag() -> Arc<dyn DiagnosticsSink> {
        Arc::new(NullDiagnostics)
    }

    /// Build a PersistDirectory mapping persist ID 0 -> given offset.
    fn build_persist_at_offset(offset: u32) -> PersistDirectory {
        let mut pd_data = Vec::new();
        let header_word: u32 = 1 << 20; // start=0, count=1
        pd_data.extend_from_slice(&header_word.to_le_bytes());
        pd_data.extend_from_slice(&offset.to_le_bytes());

        let mut pd_record = Vec::new();
        let ver_inst: u16 = 0;
        pd_record.extend_from_slice(&ver_inst.to_le_bytes());
        pd_record.extend_from_slice(&rt::PERSIST_DIRECTORY_ATOM.to_le_bytes());
        pd_record.extend_from_slice(&(pd_data.len() as u32).to_le_bytes());
        pd_record.extend_from_slice(&pd_data);

        let mut ue_data = Vec::new();
        ue_data.extend_from_slice(&0u32.to_le_bytes());
        ue_data.extend_from_slice(&0u16.to_le_bytes());
        ue_data.extend_from_slice(&3u16.to_le_bytes());
        ue_data.extend_from_slice(&0u32.to_le_bytes());
        ue_data.extend_from_slice(&0u32.to_le_bytes());
        ue_data.resize(28, 0);

        let ue_offset = pd_record.len();
        let mut ue_record = Vec::new();
        ue_record.extend_from_slice(&0u16.to_le_bytes());
        ue_record.extend_from_slice(&rt::USER_EDIT_ATOM.to_le_bytes());
        ue_record.extend_from_slice(&(ue_data.len() as u32).to_le_bytes());
        ue_record.extend_from_slice(&ue_data);

        let mut persist_stream = Vec::new();
        persist_stream.extend_from_slice(&pd_record);
        persist_stream.extend_from_slice(&ue_record);

        let diag: Arc<dyn DiagnosticsSink> = Arc::new(NullDiagnostics);
        crate::persist::build_persist_directory(&persist_stream, ue_offset as u64, diag.as_ref())
            .expect("failed to build test persist directory")
    }

    #[test]
    fn single_slide_title_text_chars() {
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom("Hello World"));

        let stream = build_ppt_stream_with_slwts(&slwt, &[]);
        let persist = build_persist_at_offset(0);
        let diag = null_diag();

        let slides = extract_slides(&stream, &persist, &diag).unwrap();
        assert_eq!(slides.len(), 1);
        assert_eq!(slides[0].persist_id, 1);
        assert_eq!(slides[0].text_blocks.len(), 1);
        assert_eq!(slides[0].text_blocks[0].text, "Hello World");
        assert_eq!(slides[0].text_blocks[0].text_type, TextType::Title);
        assert!(slides[0].notes_text.is_empty());
    }

    #[test]
    fn single_slide_body_text_bytes_cp1252() {
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_header_atom(1));
        let cp1252_bytes = b"caf\xe9 \xfcber";
        slwt.extend_from_slice(&build_text_bytes_atom(cp1252_bytes));

        let stream = build_ppt_stream_with_slwts(&slwt, &[]);
        let persist = build_persist_at_offset(0);
        let diag = null_diag();

        let slides = extract_slides(&stream, &persist, &diag).unwrap();
        assert_eq!(slides[0].text_blocks[0].text, "caf\u{e9} \u{fc}ber");
        assert_eq!(slides[0].text_blocks[0].text_type, TextType::Body);
    }

    #[test]
    fn two_slides_ordering() {
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(10));
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom("Slide One Title"));
        slwt.extend_from_slice(&build_text_header_atom(1));
        slwt.extend_from_slice(&build_text_chars_atom("Slide One Body"));
        slwt.extend_from_slice(&build_slide_persist_atom(20));
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom("Slide Two Title"));

        let stream = build_ppt_stream_with_slwts(&slwt, &[]);
        let persist = build_persist_at_offset(0);
        let diag = null_diag();

        let slides = extract_slides(&stream, &persist, &diag).unwrap();
        assert_eq!(slides.len(), 2);
        assert_eq!(slides[0].text_blocks[0].text, "Slide One Title");
        assert_eq!(slides[0].text_blocks[1].text, "Slide One Body");
        assert_eq!(slides[1].text_blocks[0].text, "Slide Two Title");
    }

    #[test]
    fn text_header_type_mapping() {
        assert_eq!(TextType::from_u32(0), TextType::Title);
        assert_eq!(TextType::from_u32(1), TextType::Body);
        assert_eq!(TextType::from_u32(2), TextType::Notes);
        assert_eq!(TextType::from_u32(4), TextType::Other);
        assert_eq!(TextType::from_u32(5), TextType::CenterTitle);
        assert_eq!(TextType::from_u32(6), TextType::Subtitle);
        assert_eq!(TextType::from_u32(7), TextType::HalfBody);
        assert_eq!(TextType::from_u32(3), TextType::Other);
        assert_eq!(TextType::from_u32(99), TextType::Other);
    }

    #[test]
    fn empty_slwt_returns_no_slides() {
        let stream = build_ppt_stream_with_slwts(&[], &[]);
        let persist = build_persist_at_offset(0);
        let diag = null_diag();
        let slides = extract_slides(&stream, &persist, &diag).unwrap();
        assert!(slides.is_empty());
    }

    #[test]
    fn text_chars_odd_byte_count() {
        let odd_data: &[u8] = &[0x41, 0x00, 0x42, 0x00, 0xFF];
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_header_atom(1));
        slwt.extend_from_slice(&build_atom(rt::TEXT_CHARS_ATOM, odd_data));

        let stream = build_ppt_stream_with_slwts(&slwt, &[]);
        let persist = build_persist_at_offset(0);
        let diag = Arc::new(CollectingDiagnostics::new());

        let slides = extract_slides(
            &stream,
            &persist,
            &(diag.clone() as Arc<dyn DiagnosticsSink>),
        )
        .unwrap();

        assert_eq!(slides[0].text_blocks[0].text, "AB");
        let warnings = diag.warnings();
        assert!(warnings.iter().any(|w| w.kind == "OddTextCharsLength"));
    }

    #[test]
    fn notes_text_extraction() {
        let mut slide_slwt = Vec::new();
        slide_slwt.extend_from_slice(&build_slide_persist_atom(1));
        slide_slwt.extend_from_slice(&build_text_header_atom(0));
        slide_slwt.extend_from_slice(&build_text_chars_atom("Slide Title"));

        let mut notes_slwt = Vec::new();
        notes_slwt.extend_from_slice(&build_slide_persist_atom(100));
        notes_slwt.extend_from_slice(&build_text_header_atom(2));
        notes_slwt.extend_from_slice(&build_text_chars_atom("Speaker notes here"));

        let stream = build_ppt_stream_with_slwts(&slide_slwt, &notes_slwt);
        let persist = build_persist_at_offset(0);
        let diag = null_diag();

        let slides = extract_slides(&stream, &persist, &diag).unwrap();
        assert_eq!(slides[0].notes_text.len(), 1);
        assert_eq!(slides[0].notes_text[0].text, "Speaker notes here");
    }

    #[test]
    fn text_without_header_defaults_to_body() {
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_chars_atom("orphan text"));

        let stream = build_ppt_stream_with_slwts(&slwt, &[]);
        let persist = build_persist_at_offset(0);
        let diag = null_diag();

        let slides = extract_slides(&stream, &persist, &diag).unwrap();
        assert_eq!(slides[0].text_blocks[0].text_type, TextType::Body);
    }

    #[test]
    fn empty_text_atom_included() {
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom(""));

        let stream = build_ppt_stream_with_slwts(&slwt, &[]);
        let persist = build_persist_at_offset(0);
        let diag = null_diag();

        let slides = extract_slides(&stream, &persist, &diag).unwrap();
        assert_eq!(slides[0].text_blocks[0].text, "");
    }

    #[test]
    fn no_slwt_returns_empty() {
        let doc_atom = build_atom(rt::DOCUMENT_ATOM, &[0; 4]);
        let stream = build_container(rt::DOCUMENT, &doc_atom);
        let persist = build_persist_at_offset(0);
        let diag = null_diag();

        let slides = extract_slides(&stream, &persist, &diag).unwrap();
        assert!(slides.is_empty());
    }

    #[test]
    fn orphan_text_prepended_to_first_slide() {
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom("orphan"));
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_header_atom(1));
        slwt.extend_from_slice(&build_text_chars_atom("body text"));

        let stream = build_ppt_stream_with_slwts(&slwt, &[]);
        let persist = build_persist_at_offset(0);
        let diag = Arc::new(CollectingDiagnostics::new());

        let slides = extract_slides(
            &stream,
            &persist,
            &(diag.clone() as Arc<dyn DiagnosticsSink>),
        )
        .unwrap();

        assert_eq!(slides[0].text_blocks.len(), 2);
        assert_eq!(slides[0].text_blocks[0].text, "orphan");
        assert_eq!(slides[0].text_blocks[1].text, "body text");
        assert!(diag.warnings().iter().any(|w| w.kind == "OrphanTextBlocks"));
    }

    #[test]
    fn style_text_prop_attached_to_text_block() {
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_header_atom(1)); // Body
        slwt.extend_from_slice(&build_text_chars_atom("Hello")); // 5 chars

        // Build a StyleTextPropAtom: 1 para run (6 chars, no props) + 1 char run (6 chars, bold)
        let mut style_data = Vec::new();
        // Paragraph run: 6 chars (5 + implicit CR), no properties
        style_data.extend_from_slice(&6u32.to_le_bytes());
        style_data.extend_from_slice(&0u16.to_le_bytes()); // indentLevel
        style_data.extend_from_slice(&0u32.to_le_bytes()); // pfMask = 0
                                                           // Character run: 6 chars, bold
        style_data.extend_from_slice(&6u32.to_le_bytes());
        style_data.extend_from_slice(&0x0001u32.to_le_bytes()); // cfMask bit 0
        style_data.extend_from_slice(&0x0001u16.to_le_bytes()); // flags: bold
        slwt.extend_from_slice(&build_atom(rt::STYLE_TEXT_PROP_ATOM, &style_data));

        let stream = build_ppt_stream_with_slwts(&slwt, &[]);
        let persist = build_persist_at_offset(0);
        let diag = null_diag();

        let slides = extract_slides(&stream, &persist, &diag).unwrap();
        assert_eq!(slides[0].text_blocks.len(), 1);
        let styles = slides[0].text_blocks[0].styles.as_ref();
        assert!(styles.is_some(), "styles should be attached");
        let runs = &styles.unwrap().char_runs;
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].bold, Some(true));
    }

    #[test]
    fn slides_reordered_by_slide_identifier() {
        // SLWT has slides in creation order (persist 10 first, then 20),
        // but slideIdentifier values are reversed (200, 100). The output
        // should be sorted by slideIdentifier, so slide 20 (id=100) comes
        // first and slide 10 (id=200) comes second.
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom_with_id(10, 200));
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom("Created First"));
        slwt.extend_from_slice(&build_slide_persist_atom_with_id(20, 100));
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom("Created Second"));

        let stream = build_ppt_stream_with_slwts(&slwt, &[]);
        let persist = build_persist_at_offset(0);
        let diag = null_diag();

        let slides = extract_slides(&stream, &persist, &diag).unwrap();
        assert_eq!(slides.len(), 2);
        // Slide with slide_id=100 should come first (presentation order).
        assert_eq!(slides[0].slide_id, 100);
        assert_eq!(slides[0].persist_id, 20);
        assert_eq!(slides[0].text_blocks[0].text, "Created Second");
        // Slide with slide_id=200 should come second.
        assert_eq!(slides[1].slide_id, 200);
        assert_eq!(slides[1].persist_id, 10);
        assert_eq!(slides[1].text_blocks[0].text, "Created First");
    }

    #[test]
    fn slides_not_reordered_when_slide_id_zero() {
        // When slideIdentifier is 0 (truncated atom), preserve SLWT order.
        let mut slwt = Vec::new();
        // Build atoms with only 4 bytes (persist_id only, no slide_id).
        slwt.extend_from_slice(&build_atom(rt::SLIDE_PERSIST_ATOM, &10u32.to_le_bytes()));
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom("First in SLWT"));
        slwt.extend_from_slice(&build_atom(rt::SLIDE_PERSIST_ATOM, &20u32.to_le_bytes()));
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom("Second in SLWT"));

        let stream = build_ppt_stream_with_slwts(&slwt, &[]);
        let persist = build_persist_at_offset(0);
        let diag = null_diag();

        let slides = extract_slides(&stream, &persist, &diag).unwrap();
        assert_eq!(slides.len(), 2);
        // SLWT order preserved (no reordering because slide_id is 0).
        assert_eq!(slides[0].persist_id, 10);
        assert_eq!(slides[0].slide_id, 0);
        assert_eq!(slides[0].text_blocks[0].text, "First in SLWT");
        assert_eq!(slides[1].persist_id, 20);
        assert_eq!(slides[1].slide_id, 0);
        assert_eq!(slides[1].text_blocks[0].text, "Second in SLWT");
    }

    #[test]
    fn slides_not_reordered_when_slide_ids_duplicate() {
        // When slideIdentifier values are not distinct, preserve SLWT order.
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom_with_id(10, 42));
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom("First"));
        slwt.extend_from_slice(&build_slide_persist_atom_with_id(20, 42));
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom("Second"));

        let stream = build_ppt_stream_with_slwts(&slwt, &[]);
        let persist = build_persist_at_offset(0);
        let diag = null_diag();

        let slides = extract_slides(&stream, &persist, &diag).unwrap();
        assert_eq!(slides.len(), 2);
        // Duplicate slide_ids: SLWT order preserved.
        assert_eq!(slides[0].persist_id, 10);
        assert_eq!(slides[0].text_blocks[0].text, "First");
        assert_eq!(slides[1].persist_id, 20);
        assert_eq!(slides[1].text_blocks[0].text, "Second");
    }

    #[test]
    fn three_slides_reordered_by_slide_identifier() {
        // Three slides in SLWT order: A(id=300), B(id=100), C(id=200).
        // Expected presentation order: B(100), C(200), A(300).
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom_with_id(1, 300));
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom("Slide A"));
        slwt.extend_from_slice(&build_slide_persist_atom_with_id(2, 100));
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom("Slide B"));
        slwt.extend_from_slice(&build_slide_persist_atom_with_id(3, 200));
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom("Slide C"));

        let stream = build_ppt_stream_with_slwts(&slwt, &[]);
        let persist = build_persist_at_offset(0);
        let diag = null_diag();

        let slides = extract_slides(&stream, &persist, &diag).unwrap();
        assert_eq!(slides.len(), 3);
        assert_eq!(slides[0].text_blocks[0].text, "Slide B");
        assert_eq!(slides[0].slide_id, 100);
        assert_eq!(slides[1].text_blocks[0].text, "Slide C");
        assert_eq!(slides[1].slide_id, 200);
        assert_eq!(slides[2].text_blocks[0].text, "Slide A");
        assert_eq!(slides[2].slide_id, 300);
    }

    /// Build a PersistDirectory mapping multiple persist IDs to offsets.
    /// Entries: `&[(persist_id, offset)]`. Persist ID 0 is always the
    /// doc_persist_id used by `find_document_container_offset`.
    fn build_persist_multi(entries: &[(u32, u32)]) -> PersistDirectory {
        // Group consecutive persist IDs into runs for the persist directory atom.
        // For simplicity, emit one (start, &[offset]) per entry.
        let persist_entries: Vec<(u32, Vec<u32>)> =
            entries.iter().map(|&(pid, off)| (pid, vec![off])).collect();
        let refs: Vec<(u32, &[u32])> = persist_entries
            .iter()
            .map(|(pid, offs)| (*pid, offs.as_slice()))
            .collect();

        let persist_atom = build_persist_directory_atom(&refs);
        let ue_offset = persist_atom.len() as u32;
        let user_edit = build_user_edit_atom(0, 0);

        let mut persist_stream = Vec::new();
        persist_stream.extend_from_slice(&persist_atom);
        persist_stream.extend_from_slice(&user_edit);

        let diag: Arc<dyn DiagnosticsSink> = Arc::new(NullDiagnostics);
        crate::persist::build_persist_directory(&persist_stream, ue_offset as u64, diag.as_ref())
            .expect("failed to build test persist directory")
    }

    #[test]
    fn slide_without_slwt_text_not_lost() {
        // A slide registered in the SLWT via SlidePersistAtom but with no
        // following text atoms should still appear in results (with empty
        // text_blocks), not silently dropped.
        let mut slwt = Vec::new();
        // Slide 1 has text.
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom("Slide One"));
        // Slide 2: SlidePersistAtom only, no text atoms follow.
        slwt.extend_from_slice(&build_slide_persist_atom(2));
        // Slide 3 has text (proves slide 2 is flushed when slide 3 starts).
        slwt.extend_from_slice(&build_slide_persist_atom(3));
        slwt.extend_from_slice(&build_text_header_atom(1));
        slwt.extend_from_slice(&build_text_chars_atom("Slide Three"));

        let stream = build_ppt_stream_with_slwts(&slwt, &[]);
        let persist = build_persist_at_offset(0);
        let diag = null_diag();

        let slides = extract_slides(&stream, &persist, &diag).unwrap();
        assert_eq!(slides.len(), 3);
        // Slide 1: has text.
        assert_eq!(slides[0].persist_id, 1);
        assert_eq!(slides[0].text_blocks.len(), 1);
        assert_eq!(slides[0].text_blocks[0].text, "Slide One");
        // Slide 2: registered but no SLWT text, persist ID not in persist
        // directory so fallback finds nothing. Should still be present.
        assert_eq!(slides[1].persist_id, 2);
        assert!(slides[1].text_blocks.is_empty());
        // Slide 3: has text.
        assert_eq!(slides[2].persist_id, 3);
        assert_eq!(slides[2].text_blocks.len(), 1);
        assert_eq!(slides[2].text_blocks[0].text, "Slide Three");
    }

    #[test]
    fn slide_without_slwt_text_falls_back_to_slide_container() {
        // When a slide has no text in the SLWT, the fallback path looks up
        // the slide's persist ID in the persist directory, reads the
        // SlideContainer at that offset, and extracts text from its shape
        // tree. This test exercises that full fallback.

        // Build a SlideContainer (recType = SLIDE, container) with text
        // atoms nested inside it (simulating a shape tree with text).
        let mut shape_children = Vec::new();
        shape_children.extend_from_slice(&build_text_header_atom(0)); // Title
        shape_children.extend_from_slice(&build_text_chars_atom("Fallback Title"));
        shape_children.extend_from_slice(&build_text_header_atom(1)); // Body
        shape_children.extend_from_slice(&build_text_chars_atom("Fallback Body"));
        let slide_container = build_container(rt::SLIDE, &shape_children);

        // Build the SLWT: one slide with persist_id=5, no text atoms.
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(5));
        // No text atoms follow -- the SLWT has the slide registered but
        // with no text content.

        // Build the DocumentContainer with the SLWT.
        let doc_container = build_ppt_stream_with_slwts(&slwt, &[]);

        // Append the SlideContainer after the DocumentContainer.
        let slide_offset = doc_container.len() as u32;
        let mut stream = Vec::new();
        stream.extend_from_slice(&doc_container);
        stream.extend_from_slice(&slide_container);

        // Build persist directory: ID 0 -> DocumentContainer (offset 0),
        // ID 5 -> SlideContainer (at slide_offset).
        let persist = build_persist_multi(&[(0, 0), (5, slide_offset)]);
        let diag = null_diag();

        let slides = extract_slides(&stream, &persist, &diag).unwrap();
        assert_eq!(slides.len(), 1);
        assert_eq!(slides[0].persist_id, 5);
        // The fallback should have extracted text from the SlideContainer.
        assert_eq!(slides[0].text_blocks.len(), 2);
        assert_eq!(slides[0].text_blocks[0].text, "Fallback Title");
        assert_eq!(slides[0].text_blocks[0].text_type, TextType::Title);
        assert_eq!(slides[0].text_blocks[1].text, "Fallback Body");
        assert_eq!(slides[0].text_blocks[1].text_type, TextType::Body);
    }

    #[test]
    fn utf16_char_count_with_non_bmp() {
        // Non-BMP characters (emoji, supplementary CJK) are 2 UTF-16 code
        // units but 1 Unicode scalar. StyleTextPropAtom char counts use
        // UTF-16 code units, so last_text_char_count must match.
        let mut collector = SlwtCollector::new();
        collector.start_slide(1, 1);

        // U+1F600 (grinning face) = 2 UTF-16 code units, 1 char
        // "A\u{1F600}B" = 3 chars, 4 UTF-16 code units
        let text = "A\u{1F600}B".to_string();
        assert_eq!(text.chars().count(), 3);
        assert_eq!(text.encode_utf16().count(), 4);

        collector.push_text(text);
        // Should count UTF-16 code units, not Unicode scalars
        assert_eq!(collector.last_text_char_count, 4);
    }
}
