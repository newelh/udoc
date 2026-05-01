//! JBIG2 globals resolver (ISO 14492 §7.4.2.1.3, issue #158).
//!
//! `/JBIG2Globals` is an indirect stream reference in the PDF Image
//! dict whose bytes carry a sequence of segments with
//! `page_association = 0`. The segments are prepended to each per-page
//! segment list at decode time so they're available as referred
//! segments for symbol-dict / pattern-dict / etc. resolution.
//!
//! This module owns the thin wiring between a caller that has already
//! resolved the globals bytes (the PDF layer does that via the PDF
//! object resolver) and the segment-level dispatcher. The heavy
//! lifting -- parse_header, parse_globals, merge_globals_and_page --
//! lives in [`super::segments`]. This module exposes a single
//! ergonomic entry point [`build_segment_list`].

use super::segments::{parse_header, ParseError, SegmentHeader};

/// One "view" over a byte buffer plus a parsed segment header.
///
/// The byte buffer is the original stream (globals or per-page) that
/// owns the segment's data. `data_range` is `(offset_into_buffer,
/// length)` pointing at the segment's payload slice. Callers use the
/// pair to slice the right buffer without copying; the dispatcher
/// then re-parses type-specific per-segment data from that slice.
#[derive(Debug, Clone)]
pub struct SegmentView<'a> {
    /// Parsed header for the segment (already-renumbered if this
    /// segment was a per-page one that collided with a globals
    /// segment number).
    pub header: SegmentHeader,
    /// Byte buffer containing the segment's data. Always one of
    /// `globals_bytes` or `page_bytes` from the call that produced
    /// this view. Kept as a slice reference (no lifetime extension).
    pub source: &'a [u8],
}

impl<'a> SegmentView<'a> {
    /// The segment's data slice within [`source`](Self::source).
    pub fn data(&self) -> &'a [u8] {
        let start = self.header.data_offset;
        // Clamp on malformed length (u32::MAX unknown-length sentinel).
        let declared = self.header.data_length as usize;
        let end = start
            .checked_add(declared)
            .unwrap_or(self.source.len())
            .min(self.source.len());
        &self.source[start..end]
    }
}

/// Errors surfaced while resolving a global + page segment list.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GlobalsError {
    /// A header in the globals stream failed to parse.
    GlobalsParse(ParseError),
    /// A header in the per-page stream failed to parse.
    PageParse(ParseError),
}

impl std::fmt::Display for GlobalsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GlobalsParse(e) => write!(f, "globals stream: {e}"),
            Self::PageParse(e) => write!(f, "page stream: {e}"),
        }
    }
}

impl std::error::Error for GlobalsError {}

/// Walk a byte buffer and return one [`SegmentView`] per parsed
/// segment. Stops at the first `ParseError` (returned via `Err`) or
/// when the buffer is exhausted.
fn parse_all_from<'a>(
    source: &'a [u8],
    on_err: impl Fn(ParseError) -> GlobalsError,
) -> Result<Vec<SegmentView<'a>>, GlobalsError> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset < source.len() {
        let start = offset;
        let header = parse_header(source, &mut offset).map_err(&on_err)?;
        // Advance past the segment's data. If the declared length
        // exceeds the remaining buffer (truncated stream or
        // unknown-length sentinel), stop and surface what we have.
        let declared = header.data_length;
        let remain = source.len().saturating_sub(header.data_offset);
        let advance = match usize::try_from(declared) {
            Ok(n) if n <= remain => n,
            // Unknown-length sentinel (u32::MAX) on
            // immediate-generic / refinement regions: scan forward
            // until end-of-buffer. Higher-level dispatcher is
            // responsible for the embedded row-count marker.
            Ok(_) => remain,
            Err(_) => remain,
        };
        let _ = start;
        out.push(SegmentView { header, source });
        offset = offset.saturating_add(advance).min(source.len());
    }
    Ok(out)
}

/// Build a merged segment list from the globals stream (optional) and
/// the per-page stream. Segments in the returned list are in wire
/// order: globals first, then per-page segments, with per-page
/// segment numbers renumbered if they collide with globals segment
/// numbers.
///
/// Both byte buffers remain owned by the caller; the returned
/// [`SegmentView`]s borrow from them.
pub fn build_segment_list<'a>(
    globals_bytes: Option<&'a [u8]>,
    page_bytes: &'a [u8],
) -> Result<Vec<SegmentView<'a>>, GlobalsError> {
    let mut merged: Vec<SegmentView<'a>> = Vec::new();

    // Parse globals first, if any.
    if let Some(g) = globals_bytes {
        let globals = parse_all_from(g, GlobalsError::GlobalsParse)?;
        merged.extend(globals);
    }

    // Parse page.
    let page = parse_all_from(page_bytes, GlobalsError::PageParse)?;

    // Collision-resolve page segment numbers against globals.
    use std::collections::HashSet;
    let global_numbers: HashSet<u32> = merged.iter().map(|v| v.header.segment_number).collect();
    let max_global = merged
        .iter()
        .map(|v| v.header.segment_number)
        .max()
        .unwrap_or(0);
    let mut next_free = max_global.saturating_add(1);
    for mut v in page {
        if global_numbers.contains(&v.header.segment_number) {
            v.header.segment_number = next_free;
            next_free = next_free.saturating_add(1);
        }
        merged.push(v);
    }

    Ok(merged)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::segments::SegmentType;
    use super::*;

    /// Minimal 11-byte segment header (segnum, type, no refs, 1-byte PA, data_len).
    fn hdr(segnum: u32, type_code: u8, pa: u8, data_len: u32) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&segnum.to_be_bytes());
        v.push(type_code & 0x3F);
        v.push(0x00); // rc=0, retain=0
        v.push(pa);
        v.extend_from_slice(&data_len.to_be_bytes());
        v
    }

    #[test]
    fn builds_empty_list_on_empty_page_and_no_globals() {
        let page: &[u8] = &[];
        let segs = build_segment_list(None, page).unwrap();
        assert!(segs.is_empty());
    }

    #[test]
    fn build_segment_list_single_page_segment_no_globals() {
        // one 11-byte header + zero-length data.
        let page = hdr(7, 48, 1, 0);
        let segs = build_segment_list(None, &page).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].header.segment_number, 7);
        assert_eq!(segs[0].header.segment_type, SegmentType::PageInformation);
    }

    #[test]
    fn build_segment_list_globals_prepended() {
        // globals: seg 0 (symbol dict), seg 1 (pattern dict), page_assoc = 0.
        let mut globals = hdr(0, 0, 0, 0);
        globals.extend_from_slice(&hdr(1, 16, 0, 0));

        // page: seg 2 (page info), seg 3 (generic region).
        let mut page = hdr(2, 48, 1, 0);
        page.extend_from_slice(&hdr(3, 38, 1, 0));

        let segs = build_segment_list(Some(&globals), &page).unwrap();
        assert_eq!(segs.len(), 4);
        assert_eq!(segs[0].header.segment_number, 0);
        assert_eq!(segs[1].header.segment_number, 1);
        assert_eq!(segs[2].header.segment_number, 2);
        assert_eq!(segs[3].header.segment_number, 3);
        assert_eq!(segs[0].header.page_association, 0); // globals
        assert_eq!(segs[2].header.page_association, 1); // page
    }

    #[test]
    fn build_segment_list_renumbers_collisions() {
        // globals: seg 0 (dict) and seg 1.
        let mut globals = hdr(0, 0, 0, 0);
        globals.extend_from_slice(&hdr(1, 16, 0, 0));

        // page: seg 0 (page info) -- collides with globals' seg 0.
        let page = hdr(0, 48, 1, 0);

        let segs = build_segment_list(Some(&globals), &page).unwrap();
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].header.segment_number, 0);
        assert_eq!(segs[1].header.segment_number, 1);
        // Collision: page's segment 0 renumbered to 2 (max_global + 1).
        assert_eq!(segs[2].header.segment_number, 2);
    }

    #[test]
    fn segment_view_data_slice_matches_declared_length() {
        // hdr with data_length = 5, followed by 5 bytes of payload.
        let mut bytes = hdr(9, 38, 1, 5);
        bytes.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD, 0xEE]);
        let segs = build_segment_list(None, &bytes).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].data(), &[0xAA, 0xBB, 0xCC, 0xDD, 0xEE]);
    }

    #[test]
    fn segment_view_data_clamps_to_buffer_end_on_truncation() {
        // hdr with data_length = 20, but only 3 bytes of actual payload.
        let mut bytes = hdr(9, 38, 1, 20);
        bytes.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        let segs = build_segment_list(None, &bytes).unwrap();
        assert_eq!(segs.len(), 1);
        // Data slice is clamped to what's actually there.
        assert_eq!(segs[0].data(), &[0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn build_segment_list_returns_err_on_malformed_page_header() {
        // 5-byte truncated page header.
        let page = vec![0u8; 5];
        let err = build_segment_list(None, &page).unwrap_err();
        assert!(matches!(err, GlobalsError::PageParse(_)));
    }

    #[test]
    fn build_segment_list_returns_err_on_malformed_globals() {
        let globals = vec![0u8; 5]; // truncated
        let page: Vec<u8> = Vec::new();
        let err = build_segment_list(Some(&globals), &page).unwrap_err();
        assert!(matches!(err, GlobalsError::GlobalsParse(_)));
    }
}
