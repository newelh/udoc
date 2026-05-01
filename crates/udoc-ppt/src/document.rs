//! PptDocument and page types implementing FormatBackend/PageExtractor.
//!
//! `PptDocument` opens a .ppt file via CFB, parses the CurrentUser and
//! PowerPoint Document streams, builds the persist directory, and extracts
//! slide text. Each slide is a "page" in the FormatBackend sense.

use std::sync::Arc;

use udoc_containers::cfb::CfbArchive;
use udoc_core::backend::{DocumentMetadata, FormatBackend, PageExtractor};
use udoc_core::diagnostics::DiagnosticsSink;
use udoc_core::image::PageImage;
use udoc_core::table::Table;
use udoc_core::text::{TextLine, TextSpan};

use crate::error::{Error, Result, ResultExt};
use crate::persist;
use crate::slides::{self, SlideContent, TextType};
use crate::MAX_FILE_SIZE;

/// A parsed PPT document ready for content extraction.
///
/// Implements `FormatBackend` where each "page" is a slide.
#[derive(Debug)]
pub struct PptDocument {
    slides: Vec<SlideContent>,
    metadata: DocumentMetadata,
}

/// A page (slide) view for content extraction.
pub struct PptPage<'a> {
    slide: &'a SlideContent,
}

impl PptDocument {
    udoc_core::define_backend_constructors!(MAX_FILE_SIZE, "PPT");

    /// Access a slide's content by index (for in-crate conversion).
    pub(crate) fn slide_content(&self, index: usize) -> Option<&SlideContent> {
        self.slides.get(index)
    }

    /// Find the offset of the latest UserEditAtom. Tries the "Current User"
    /// stream first; if it's missing or invalid, scans the PowerPoint Document
    /// stream backwards for a UserEditAtom record.
    fn find_current_edit_offset(
        archive: &CfbArchive,
        ppt_stream: &[u8],
        diag: &Arc<dyn DiagnosticsSink>,
    ) -> Result<u64> {
        // Try Current User stream first.
        if let Some(cu_entry) = archive.find("Current User") {
            if let Ok(cu_data) = archive.read(cu_entry) {
                match persist::parse_current_user(&cu_data) {
                    Ok(offset) => return Ok(offset),
                    Err(e) => {
                        diag.warning(udoc_core::diagnostics::Warning::new(
                            "CurrentUserParseError",
                            format!(
                                "Current User stream invalid ({e}), falling back to stream scan"
                            ),
                        ));
                    }
                }
            }
        }

        // Fallback: scan backward through the PPT stream for the last UserEditAtom.
        Self::scan_for_user_edit_atom(ppt_stream)
    }

    /// Scan the PPT stream for the last UserEditAtom (recType 0x0FF5).
    ///
    /// Per MS-PPT, when the Current User stream is missing or corrupt, the last
    /// UserEditAtom in the stream is the most recent edit. We do a forward scan
    /// tracking the last match, which gives us the final (most recent) edit.
    fn scan_for_user_edit_atom(ppt_stream: &[u8]) -> Result<u64> {
        use crate::records::{rt, HEADER_SIZE};

        if ppt_stream.len() < HEADER_SIZE {
            return Err(Error::new(
                "PowerPoint Document stream too short for any records",
            ));
        }

        let mut last_ue_offset: Option<usize> = None;

        let mut offset = 0;
        while offset + HEADER_SIZE <= ppt_stream.len() {
            if let Ok(hdr) = crate::records::read_record_header(ppt_stream, offset) {
                if hdr.rec_type == rt::USER_EDIT_ATOM && hdr.rec_ver == 0 {
                    last_ue_offset = Some(offset);
                }
                // Advance past this record
                let next = offset
                    .checked_add(HEADER_SIZE)
                    .and_then(|o| o.checked_add(hdr.rec_len as usize));
                match next {
                    Some(n) if n > offset => offset = n,
                    _ => break,
                }
            } else {
                break;
            }
        }

        last_ue_offset.map(|o| o as u64).ok_or_else(|| {
            Error::new(
                "no UserEditAtom found in PowerPoint Document stream \
                 (Current User stream missing/invalid and stream scan failed)",
            )
        })
    }

    /// Create a PPT document from in-memory bytes with diagnostics.
    pub fn from_bytes_with_diag(data: &[u8], diag: Arc<dyn DiagnosticsSink>) -> Result<Self> {
        if data.len() as u64 > MAX_FILE_SIZE {
            return Err(Error::new(format!(
                "PPT file too large ({} bytes, limit is {} bytes)",
                data.len(),
                MAX_FILE_SIZE
            )));
        }

        let archive =
            CfbArchive::new(data, Arc::clone(&diag)).context("opening PPT CFB archive")?;

        // Read the "PowerPoint Document" stream (required).
        let ppt_entry = archive
            .find("PowerPoint Document")
            .ok_or_else(|| Error::new("PowerPoint Document stream not found"))?;
        let ppt_stream = archive
            .read(ppt_entry)
            .context("reading PowerPoint Document stream")?;

        // Find the latest UserEditAtom offset. Try "Current User" stream first,
        // fall back to scanning the PPT stream if it's missing or invalid.
        let current_edit_offset = Self::find_current_edit_offset(&archive, &ppt_stream, &diag)?;

        // Build the merged persist directory by walking the UserEditAtom chain.
        let persist_dir =
            persist::build_persist_directory(&ppt_stream, current_edit_offset, diag.as_ref())
                .context("building persist directory")?;

        // Extract slides from the SlideListWithText containers.
        let slide_contents = slides::extract_slides(&ppt_stream, &persist_dir, &diag)
            .context("extracting slides")?;

        // Build metadata: title from first title text block.
        let title = slide_contents.iter().find_map(|sc| {
            sc.text_blocks.iter().find_map(|tb| {
                if matches!(tb.text_type, TextType::Title | TextType::CenterTitle) {
                    let trimmed = tb.text.trim();
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    }
                }
                None
            })
        });

        let mut metadata = DocumentMetadata::with_page_count(slide_contents.len());
        metadata.title = title;

        Ok(Self {
            slides: slide_contents,
            metadata,
        })
    }
}

impl FormatBackend for PptDocument {
    type Page<'a> = PptPage<'a>;

    fn page_count(&self) -> usize {
        self.slides.len()
    }

    fn page(&mut self, index: usize) -> Result<PptPage<'_>> {
        if index >= self.slides.len() {
            return Err(Error::new(format!(
                "slide index {index} out of range (document has {} slides)",
                self.slides.len()
            )));
        }
        Ok(PptPage {
            slide: &self.slides[index],
        })
    }

    fn metadata(&self) -> DocumentMetadata {
        self.metadata.clone()
    }
}

/// Determine bold/italic for a text block, preferring char-level style runs
/// over TextType inference. When a single style run covers the entire text,
/// use its bold/italic flags. When multiple runs exist, falls back to
/// TextType inference for the whole block (splitting into per-run spans
/// happens in text_lines/raw_spans).
fn text_block_is_bold(tb: &crate::slides::TextBlock) -> bool {
    if let Some(ref style) = tb.styles {
        if style.char_runs.len() == 1 {
            if let Some(bold) = style.char_runs[0].bold {
                return bold;
            }
        }
    }
    // Fallback: infer from text type
    matches!(
        tb.text_type,
        TextType::Title | TextType::CenterTitle | TextType::Subtitle
    )
}

/// Build spans from a text block, splitting by char-level style runs when available.
fn spans_from_text_block(tb: &crate::slides::TextBlock, y: f64) -> Vec<TextSpan> {
    if let Some(ref style) = tb.styles {
        if style.char_runs.len() > 1 {
            let (chunks, remainder) = crate::styles::split_text_by_runs(&tb.text, &style.char_runs);
            let mut spans: Vec<TextSpan> = chunks
                .into_iter()
                .map(|(text, run)| {
                    let mut span = TextSpan::new(text, 0.0, y, 0.0, 0.0);
                    span.is_bold = run.bold.unwrap_or(false);
                    span.is_italic = run.italic.unwrap_or(false);
                    span
                })
                .collect();
            if !remainder.is_empty() {
                spans.push(TextSpan::new(remainder, 0.0, y, 0.0, 0.0));
            }
            if !spans.is_empty() {
                return spans;
            }
        }
    }
    // Single run or no styles: one span for the whole block
    let is_bold = text_block_is_bold(tb);
    let is_italic = tb
        .styles
        .as_ref()
        .and_then(|s| s.char_runs.first())
        .and_then(|r| r.italic)
        .unwrap_or(false);
    let mut span = TextSpan::new(tb.text.clone(), 0.0, y, 0.0, 0.0);
    span.is_bold = is_bold;
    span.is_italic = is_italic;
    vec![span]
}

impl PageExtractor for PptPage<'_> {
    fn text(&mut self) -> Result<String> {
        let mut parts = Vec::new();

        for tb in &self.slide.text_blocks {
            if !tb.text.is_empty() {
                parts.push(tb.text.as_str());
            }
        }

        // Append notes if present.
        let mut has_notes = false;
        for tb in &self.slide.notes_text {
            if !tb.text.is_empty() {
                if !has_notes {
                    parts.push("\n[Notes]");
                    has_notes = true;
                }
                parts.push(tb.text.as_str());
            }
        }

        Ok(parts.join("\n"))
    }

    fn text_lines(&mut self) -> Result<Vec<TextLine>> {
        let mut lines = Vec::new();
        let mut line_idx: usize = 0;

        for tb in &self.slide.text_blocks {
            if tb.text.is_empty() {
                continue;
            }
            let y = line_idx as f64;
            let spans = spans_from_text_block(tb, y);
            lines.push(TextLine::new(spans, y, false));
            line_idx += 1;
        }

        for tb in &self.slide.notes_text {
            if tb.text.is_empty() {
                continue;
            }
            let y = line_idx as f64;
            let span = TextSpan::new(tb.text.clone(), 0.0, y, 0.0, 0.0);
            lines.push(TextLine::new(vec![span], y, false));
            line_idx += 1;
        }

        Ok(lines)
    }

    fn raw_spans(&mut self) -> Result<Vec<TextSpan>> {
        let mut spans = Vec::new();
        let mut idx: usize = 0;

        for tb in &self.slide.text_blocks {
            if tb.text.is_empty() {
                continue;
            }
            let y = idx as f64;
            let block_spans = spans_from_text_block(tb, y);
            idx += block_spans.len();
            spans.extend(block_spans);
        }

        for tb in &self.slide.notes_text {
            if tb.text.is_empty() {
                continue;
            }
            let y = idx as f64;
            let span = TextSpan::new(tb.text.clone(), 0.0, y, 0.0, 0.0);
            spans.push(span);
            idx += 1;
        }

        Ok(spans)
    }

    fn tables(&mut self) -> Result<Vec<Table>> {
        // PPT table extraction deferred.
        Ok(Vec::new())
    }

    fn images(&mut self) -> Result<Vec<PageImage>> {
        // PPT image extraction deferred.
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use udoc_containers::test_util::build_cfb;
    use udoc_core::backend::{FormatBackend, PageExtractor};
    use udoc_core::diagnostics::{CollectingDiagnostics, DiagnosticsSink};

    use crate::records::rt;
    use crate::test_util::*;

    use super::*;

    // ---------------------------------------------------------------
    // Test 1: PptDocument::from_bytes with a minimal valid PPT
    // ---------------------------------------------------------------
    #[test]
    fn from_bytes_minimal_single_slide() {
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_header_atom(0)); // Title
        slwt.extend_from_slice(&build_text_chars_atom("Hello PPT"));

        let cfb_data = build_ppt_cfb(&slwt, &[]);
        let mut doc = PptDocument::from_bytes(&cfb_data).expect("from_bytes should succeed");
        assert_eq!(doc.page_count(), 1);

        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");
        assert_eq!(text, "Hello PPT");
    }

    // ---------------------------------------------------------------
    // Test 2: page_count returns correct value
    // ---------------------------------------------------------------
    #[test]
    fn page_count_multiple_slides() {
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom("Slide 1"));
        slwt.extend_from_slice(&build_slide_persist_atom(2));
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom("Slide 2"));
        slwt.extend_from_slice(&build_slide_persist_atom(3));
        slwt.extend_from_slice(&build_text_header_atom(1));
        slwt.extend_from_slice(&build_text_chars_atom("Slide 3 body"));

        let cfb_data = build_ppt_cfb(&slwt, &[]);
        let doc = PptDocument::from_bytes(&cfb_data).expect("from_bytes");
        assert_eq!(doc.page_count(), 3);
    }

    // ---------------------------------------------------------------
    // Test 3: text() returns slide text with notes
    // ---------------------------------------------------------------
    #[test]
    fn text_with_notes() {
        let mut slide_slwt = Vec::new();
        slide_slwt.extend_from_slice(&build_slide_persist_atom(1));
        slide_slwt.extend_from_slice(&build_text_header_atom(0)); // Title
        slide_slwt.extend_from_slice(&build_text_chars_atom("My Title"));
        slide_slwt.extend_from_slice(&build_text_header_atom(1)); // Body
        slide_slwt.extend_from_slice(&build_text_chars_atom("Body text"));

        let mut notes_slwt = Vec::new();
        notes_slwt.extend_from_slice(&build_slide_persist_atom(100));
        notes_slwt.extend_from_slice(&build_text_header_atom(2));
        notes_slwt.extend_from_slice(&build_text_chars_atom("Speaker notes"));

        let cfb_data = build_ppt_cfb(&slide_slwt, &notes_slwt);
        let mut doc = PptDocument::from_bytes(&cfb_data).expect("from_bytes");

        let mut page = doc.page(0).expect("page 0");
        let text = page.text().expect("text()");

        assert!(text.contains("My Title"), "missing title in: {text}");
        assert!(text.contains("Body text"), "missing body in: {text}");
        assert!(text.contains("[Notes]"), "missing notes marker in: {text}");
        assert!(
            text.contains("Speaker notes"),
            "missing notes text in: {text}"
        );
    }

    // ---------------------------------------------------------------
    // Test 4: Metadata title extraction
    // ---------------------------------------------------------------
    #[test]
    fn metadata_title_from_first_title_block() {
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_header_atom(0)); // Title
        slwt.extend_from_slice(&build_text_chars_atom("Presentation Title"));
        slwt.extend_from_slice(&build_text_header_atom(1)); // Body
        slwt.extend_from_slice(&build_text_chars_atom("Body content"));

        let cfb_data = build_ppt_cfb(&slwt, &[]);
        let doc = PptDocument::from_bytes(&cfb_data).expect("from_bytes");
        let meta = doc.metadata();

        assert_eq!(meta.title.as_deref(), Some("Presentation Title"));
        assert_eq!(meta.page_count, 1);
    }

    // ---------------------------------------------------------------
    // Test 5: Missing "PowerPoint Document" stream returns error
    // ---------------------------------------------------------------
    #[test]
    fn missing_powerpoint_document_stream() {
        // CFB with only "Current User", no "PowerPoint Document"
        let current_user = build_current_user(0);
        let cfb_data = build_cfb(&[("Current User", &current_user)]);
        let result = PptDocument::from_bytes(&cfb_data);

        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("PowerPoint Document"),
            "error should mention missing stream: {err_msg}"
        );
    }

    // ---------------------------------------------------------------
    // Test 6: Missing "Current User" stream returns error
    // ---------------------------------------------------------------
    #[test]
    fn missing_current_user_stream() {
        let ppt_stream = build_container(rt::DOCUMENT, &[]);
        let cfb_data = build_cfb(&[("PowerPoint Document", &ppt_stream)]);
        let result = PptDocument::from_bytes(&cfb_data);

        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("Current User"),
            "error should mention missing stream: {err_msg}"
        );
    }

    // ---------------------------------------------------------------
    // Test 7: Page out of range returns error
    // ---------------------------------------------------------------
    #[test]
    fn page_out_of_range() {
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom("Only slide"));

        let cfb_data = build_ppt_cfb(&slwt, &[]);
        let mut doc = PptDocument::from_bytes(&cfb_data).expect("from_bytes");

        assert!(doc.page(0).is_ok());
        assert!(doc.page(1).is_err());
    }

    // ---------------------------------------------------------------
    // Test 8: text_lines marks titles as bold
    // ---------------------------------------------------------------
    #[test]
    fn text_lines_title_is_bold() {
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_header_atom(0)); // Title
        slwt.extend_from_slice(&build_text_chars_atom("Title Text"));
        slwt.extend_from_slice(&build_text_header_atom(1)); // Body
        slwt.extend_from_slice(&build_text_chars_atom("Body Text"));

        let cfb_data = build_ppt_cfb(&slwt, &[]);
        let mut doc = PptDocument::from_bytes(&cfb_data).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let lines = page.text_lines().expect("text_lines()");

        assert_eq!(lines.len(), 2);
        // Title should be bold
        assert!(lines[0].spans[0].is_bold, "title should be bold");
        assert_eq!(lines[0].spans[0].text, "Title Text");
        // Body should not be bold
        assert!(!lines[1].spans[0].is_bold, "body should not be bold");
        assert_eq!(lines[1].spans[0].text, "Body Text");
    }

    // ---------------------------------------------------------------
    // Test 9: raw_spans returns spans for all text blocks
    // ---------------------------------------------------------------
    #[test]
    fn raw_spans_includes_all_blocks() {
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_header_atom(5)); // CenterTitle
        slwt.extend_from_slice(&build_text_chars_atom("Center"));
        slwt.extend_from_slice(&build_text_header_atom(6)); // Subtitle
        slwt.extend_from_slice(&build_text_chars_atom("Sub"));
        slwt.extend_from_slice(&build_text_header_atom(1)); // Body
        slwt.extend_from_slice(&build_text_chars_atom("Body"));

        let cfb_data = build_ppt_cfb(&slwt, &[]);
        let mut doc = PptDocument::from_bytes(&cfb_data).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");
        let spans = page.raw_spans().expect("raw_spans()");

        assert_eq!(spans.len(), 3);
        assert!(spans[0].is_bold, "CenterTitle should be bold");
        assert_eq!(spans[0].text, "Center");
        assert!(spans[1].is_bold, "Subtitle should be bold");
        assert_eq!(spans[1].text, "Sub");
        assert!(!spans[2].is_bold, "Body should not be bold");
        assert_eq!(spans[2].text, "Body");
    }

    // ---------------------------------------------------------------
    // Test 10: tables and images return empty vecs
    // ---------------------------------------------------------------
    #[test]
    fn tables_and_images_empty() {
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom("Some text"));

        let cfb_data = build_ppt_cfb(&slwt, &[]);
        let mut doc = PptDocument::from_bytes(&cfb_data).expect("from_bytes");
        let mut page = doc.page(0).expect("page 0");

        assert!(page.tables().expect("tables()").is_empty());
        assert!(page.images().expect("images()").is_empty());
    }

    // ---------------------------------------------------------------
    // Test 11: Non-CFB data returns error
    // ---------------------------------------------------------------
    #[test]
    fn from_bytes_rejects_non_cfb() {
        let result = PptDocument::from_bytes(b"not a CFB file");
        assert!(result.is_err());
    }

    // ---------------------------------------------------------------
    // Test 12: Metadata title from CenterTitle
    // ---------------------------------------------------------------
    #[test]
    fn metadata_title_from_center_title() {
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_header_atom(5)); // CenterTitle
        slwt.extend_from_slice(&build_text_chars_atom("Centered Title"));

        let cfb_data = build_ppt_cfb(&slwt, &[]);
        let doc = PptDocument::from_bytes(&cfb_data).expect("from_bytes");
        assert_eq!(doc.metadata().title.as_deref(), Some("Centered Title"));
    }

    // ---------------------------------------------------------------
    // Test 13: No title block means metadata title is None
    // ---------------------------------------------------------------
    #[test]
    fn metadata_title_none_when_no_title_block() {
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_header_atom(1)); // Body only
        slwt.extend_from_slice(&build_text_chars_atom("Just body text"));

        let cfb_data = build_ppt_cfb(&slwt, &[]);
        let doc = PptDocument::from_bytes(&cfb_data).expect("from_bytes");
        assert!(doc.metadata().title.is_none());
    }

    // ---------------------------------------------------------------
    // Test 14: Missing Current User triggers successful stream scan fallback
    // ---------------------------------------------------------------
    #[test]
    fn missing_current_user_scan_fallback_succeeds() {
        // Build a valid PPT structure but omit the "Current User" stream.
        // The fallback scan should find the UserEditAtom in the PPT stream.
        let mut slwt = Vec::new();
        slwt.extend_from_slice(&build_slide_persist_atom(1));
        slwt.extend_from_slice(&build_text_header_atom(0));
        slwt.extend_from_slice(&build_text_chars_atom("Scan Fallback"));

        let doc_container = build_ppt_stream_with_slwts(&slwt, &[]);
        let (ppt_stream, _current_user) = build_ppt_stream_with_persist(&doc_container);

        // Build CFB with only "PowerPoint Document", no "Current User".
        let cfb_data = build_cfb(&[("PowerPoint Document", &ppt_stream)]);

        let diag = Arc::new(CollectingDiagnostics::new());
        let mut doc =
            PptDocument::from_bytes_with_diag(&cfb_data, diag.clone() as Arc<dyn DiagnosticsSink>)
                .expect("scan fallback should find UserEditAtom and succeed");

        assert_eq!(doc.page_count(), 1);
        let mut page = doc.page(0).expect("page 0");
        assert_eq!(page.text().expect("text()"), "Scan Fallback");
    }
}
