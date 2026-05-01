//! JBIG2 segment header parser (ISO 14492 §7.2) and globals-resolver scaffolding.
//!
//! A JBIG2 stream is a sequence of *segments*; each segment starts with a
//! fixed-format header (§7.2) followed by variable-length per-segment data.
//! This module owns the header parser: it decodes the header into a
//! [`SegmentHeader`] and leaves per-segment-data interpretation to the
//! region decoders (arith / generic / symbol / text / halftone /
//! refinement).
//!
//! # Byte order
//!
//! ISO 14492-2 specifies all multi-byte integers as network byte order
//! (big-endian). We honour that here despite PDF's common little-endian
//! convention in surrounding metadata; the segment wire format is not
//! PDF-specific.
//!
//! # Page association width
//!
//! Segment headers carry the page-association field as either 1 byte or
//! 4 bytes (§7.2.6). Bit 6 of the segment-type flags byte selects the
//! width per segment. PDF-embedded JBIG2 conventionally uses 1-byte PA
//! (sequential-segments mode per ISO 32000-2 §8.9.5.4); standalone .jb2
//! streams prefixed with a file header can use either.
//!
//! # Globals scaffolding
//!
//! [`parse_globals`] parses a `/JBIG2Globals` stream (a sequence of
//! segments with `page_association = 0`) into a vector of
//! [`SegmentHeader`]s. The actual stream-to-bytes resolution is left to
//! the caller (T3-TEXT will wire the PDF object resolver).
//!
//! # Reference
//!
//! pdfium's `third_party/jbig2/JBig2_SegmentHeader.cpp` is the cleanest
//! public port target. libjbig2dec's retain-bits handling has known
//! endianness quirks on exotic segment types; avoid.

use std::fmt;

/// ISO 14492 §7.3 segment types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SegmentType {
    /// Type 0 -- symbol dictionary (§7.4.2).
    SymbolDictionary,
    /// Type 4 -- intermediate text region (§7.4.3).
    IntermediateTextRegion,
    /// Type 6 -- immediate text region (§7.4.3).
    ImmediateTextRegion,
    /// Type 7 -- immediate lossless text region (§7.4.3).
    ImmediateLosslessTextRegion,
    /// Type 16 -- pattern dictionary (§7.4.4).
    PatternDictionary,
    /// Type 20 -- intermediate halftone region (§7.4.5).
    IntermediateHalftoneRegion,
    /// Type 22 -- immediate halftone region (§7.4.5).
    ImmediateHalftoneRegion,
    /// Type 23 -- immediate lossless halftone region (§7.4.5).
    ImmediateLosslessHalftoneRegion,
    /// Type 36 -- intermediate generic region (§7.4.6).
    IntermediateGenericRegion,
    /// Type 38 -- immediate generic region (§7.4.6).
    ImmediateGenericRegion,
    /// Type 39 -- immediate lossless generic region (§7.4.6).
    ImmediateLosslessGenericRegion,
    /// Type 40 -- intermediate generic refinement region (§7.4.7).
    IntermediateGenericRefinementRegion,
    /// Type 42 -- immediate generic refinement region (§7.4.7).
    ImmediateGenericRefinementRegion,
    /// Type 43 -- immediate lossless generic refinement region (§7.4.7).
    ImmediateLosslessGenericRefinementRegion,
    /// Type 48 -- page information (§7.4.8).
    PageInformation,
    /// Type 49 -- end of page (§7.4.9).
    EndOfPage,
    /// Type 50 -- end of stripe (§7.4.10).
    EndOfStripe,
    /// Type 51 -- end of file (§7.4.11).
    EndOfFile,
    /// Type 52 -- profiles (§7.4.12).
    Profiles,
    /// Type 53 -- tables (§7.4.13).
    Tables,
    /// Reserved or unknown segment type. The raw type code is preserved
    /// so the caller can log a diagnostic and skip the segment's data.
    Reserved(u8),
}

impl SegmentType {
    /// Decode a 6-bit segment type code (`0..=63`).
    pub fn from_code(code: u8) -> Self {
        match code {
            0 => Self::SymbolDictionary,
            4 => Self::IntermediateTextRegion,
            6 => Self::ImmediateTextRegion,
            7 => Self::ImmediateLosslessTextRegion,
            16 => Self::PatternDictionary,
            20 => Self::IntermediateHalftoneRegion,
            22 => Self::ImmediateHalftoneRegion,
            23 => Self::ImmediateLosslessHalftoneRegion,
            36 => Self::IntermediateGenericRegion,
            38 => Self::ImmediateGenericRegion,
            39 => Self::ImmediateLosslessGenericRegion,
            40 => Self::IntermediateGenericRefinementRegion,
            42 => Self::ImmediateGenericRefinementRegion,
            43 => Self::ImmediateLosslessGenericRefinementRegion,
            48 => Self::PageInformation,
            49 => Self::EndOfPage,
            50 => Self::EndOfStripe,
            51 => Self::EndOfFile,
            52 => Self::Profiles,
            53 => Self::Tables,
            other => Self::Reserved(other),
        }
    }

    /// The raw 6-bit type code.
    pub fn code(self) -> u8 {
        match self {
            Self::SymbolDictionary => 0,
            Self::IntermediateTextRegion => 4,
            Self::ImmediateTextRegion => 6,
            Self::ImmediateLosslessTextRegion => 7,
            Self::PatternDictionary => 16,
            Self::IntermediateHalftoneRegion => 20,
            Self::ImmediateHalftoneRegion => 22,
            Self::ImmediateLosslessHalftoneRegion => 23,
            Self::IntermediateGenericRegion => 36,
            Self::ImmediateGenericRegion => 38,
            Self::ImmediateLosslessGenericRegion => 39,
            Self::IntermediateGenericRefinementRegion => 40,
            Self::ImmediateGenericRefinementRegion => 42,
            Self::ImmediateLosslessGenericRefinementRegion => 43,
            Self::PageInformation => 48,
            Self::EndOfPage => 49,
            Self::EndOfStripe => 50,
            Self::EndOfFile => 51,
            Self::Profiles => 52,
            Self::Tables => 53,
            Self::Reserved(code) => code,
        }
    }

    /// True when this type has an "unknown data length" sentinel
    /// allowed per §7.2.7 (generic/refinement region at the end of a
    /// page). The parser does not currently scan for the
    /// immediate-generic-region unknown-length end marker; callers
    /// should treat `data_length == u32::MAX` as a diagnostic.
    pub fn allows_unknown_length(self) -> bool {
        matches!(
            self,
            Self::ImmediateGenericRegion
                | Self::ImmediateLosslessGenericRegion
                | Self::ImmediateGenericRefinementRegion
                | Self::ImmediateLosslessGenericRefinementRegion
        )
    }
}

/// A parsed JBIG2 segment header.
///
/// Produced by [`parse_header`]; consumed by the region decoders via the
/// `data_offset + data_length` slice into the originating byte buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentHeader {
    /// §7.2.2 segment number, 4-byte big-endian.
    pub segment_number: u32,
    /// §7.2.3 decoded segment type (bits 0-5 of the flags byte).
    pub segment_type: SegmentType,
    /// §7.2.3 bit 7 -- deferred non-retain flag.
    pub deferred_non_retain: bool,
    /// §7.2.6 page association (1-byte or 4-byte variant, normalised to `u32`).
    /// Value `0` means "applies globally" (e.g. /JBIG2Globals stream).
    pub page_association: u32,
    /// §7.2.4 retain bits flags. Low 5 bits carry per-referred-segment
    /// retention flags (up to 4 refs; long form extends via
    /// [`SegmentHeader::extended_retain_flags`]).
    pub retain_flags: u8,
    /// §7.2.4 extended retention flags for long-form referred counts.
    /// Empty for the common short-form (count <= 4).
    pub extended_retain_flags: Vec<u8>,
    /// §7.2.5 referred-segment numbers.
    pub referred_segments: Vec<u32>,
    /// §7.2.7 segment data length, as declared in the header. `u64` so
    /// unknown-length sentinel (`u32::MAX` per spec) round-trips without
    /// widening loss and the long-form parse path is natural.
    pub data_length: u64,
    /// Byte offset of the *data* portion within the input slice passed
    /// to [`parse_header`]. Callers decode from
    /// `input[data_offset.data_offset + data_length as usize]` (where
    /// the length is known; see [`SegmentType::allows_unknown_length`]).
    pub data_offset: usize,
    /// Byte length of the header itself within the input slice.
    pub header_length: usize,
    /// Width (in bytes) of referred-segment numbers for this segment, per
    /// §7.2.5. Preserved so a re-emitter can round-trip byte-exact.
    pub referred_segment_field_width: ReferredFieldWidth,
}

impl SegmentHeader {
    /// Total byte length occupied by this segment (header + data),
    /// assuming a known data length. Returns `None` for the unknown
    /// sentinel.
    pub fn total_length(&self) -> Option<usize> {
        if self.data_length == u32::MAX as u64 && self.segment_type.allows_unknown_length() {
            return None;
        }
        // Guard against overflow on pathological inputs.
        let dl = usize::try_from(self.data_length).ok()?;
        self.header_length.checked_add(dl)
    }
}

/// Width of each referred-segment number field per §7.2.5.
///
/// The encoder picks the smallest width that fits the highest segment
/// number *seen so far in the stream*. A decoder that parses headers
/// independently must be told the width, which for PDF-embedded JBIG2
/// we derive from the current segment's own number as a
/// monotonically-non-decreasing upper bound (§7.2.5 note).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferredFieldWidth {
    /// 1 byte per referred segment number (max segnum so far <= 255).
    One,
    /// 2 bytes per referred segment number (max segnum so far <= 65535).
    Two,
    /// 4 bytes per referred segment number (max segnum so far > 65535).
    Four,
}

impl ReferredFieldWidth {
    /// Pick the width based on the largest segment number seen so far.
    /// 5 the encoder uses the smallest width sufficient for the
    /// highest segment number in the entire stream (not just those
    /// referred to). Since segment numbers are monotonically increasing,
    /// deriving width from the parsing segment's own number is a safe
    /// lower bound for headers that reference only lower-numbered
    /// segments.
    pub fn for_max_segnum(max: u32) -> Self {
        if max <= 0xFF {
            Self::One
        } else if max <= 0xFFFF {
            Self::Two
        } else {
            Self::Four
        }
    }

    /// Byte width of the field.
    pub fn bytes(self) -> usize {
        match self {
            Self::One => 1,
            Self::Two => 2,
            Self::Four => 4,
        }
    }
}

/// Parse errors surfaced by [`parse_header`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseError {
    /// Input slice ended mid-header.
    UnexpectedEof {
        /// Offset (within the input) at which the parser ran out of bytes.
        at: usize,
        /// Number of bytes required beyond that offset.
        needed: usize,
    },
    /// Referred-segment count encoding is reserved / malformed (§7.2.4
    /// values `5` and `6` are explicitly reserved).
    ReservedReferredCount {
        /// Raw byte value of the retain-flags field.
        flags_byte: u8,
    },
    /// Long-form referred-segment count exceeded a sanity bound. ISO
    /// 14492 does not impose a hard upper limit; we cap at 2^24 to
    /// prevent a pathological input from allocating tens of GB.
    ExcessiveReferredCount {
        /// Parsed count value.
        count: u32,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEof { at, needed } => {
                write!(
                    f,
                    "unexpected end-of-input at offset {at} (needed {needed} more bytes)"
                )
            }
            Self::ReservedReferredCount { flags_byte } => {
                write!(
                    f,
                    "reserved referred-segment count in retain-flags byte 0x{flags_byte:02x}"
                )
            }
            Self::ExcessiveReferredCount { count } => {
                write!(f, "excessive referred-segment count {count} (cap 2^24)")
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// Sanity cap on the long-form referred-segment count. Real streams
/// have counts in the tens at most; 2^24 allows headroom without
/// permitting multi-GB allocations.
const MAX_REFERRED_COUNT: u32 = 1 << 24;

/// Parse a single JBIG2 segment header starting at `*offset` within
/// `data`. Advances `*offset` to the start of the segment *data* on
/// success (equivalent to the returned [`SegmentHeader::data_offset`]).
///
/// On failure `*offset` is left at the byte the parser could not
/// consume; the caller can log a diagnostic and scan for the next
/// plausible segment start.
pub fn parse_header(data: &[u8], offset: &mut usize) -> Result<SegmentHeader, ParseError> {
    let start = *offset;

    // §7.2.2 segment number: 4 bytes big-endian.
    let segment_number = read_u32_be(data, offset)?;

    // §7.2.3 segment header flags.
    let flags = read_u8(data, offset)?;
    let segment_type = SegmentType::from_code(flags & 0x3F);
    let page_assoc_is_4_bytes = (flags & 0x40) != 0;
    let deferred_non_retain = (flags & 0x80) != 0;

    // §7.2.4 referred-segment count + retention flags.
    let rc_byte = read_u8(data, offset)?;
    let short_count = (rc_byte >> 5) & 0x07;
    let (referred_count, retain_flags, extended_retain_flags) = match short_count {
        0..=4 => (u32::from(short_count), rc_byte & 0x1F, Vec::new()),
        7 => {
            // Long form: top 3 bits == 0b111, then next 4 bytes carry
            // the actual 29-bit count (§7.2.4 allows up to 2^29 - 1).
            // The first byte already consumed contributes its low 5
            // bits as the top 5 bits of the count; the following
            // 3 bytes contribute the remaining 24 bits.
            let top_bits = u32::from(rc_byte & 0x1F);
            let b1 = u32::from(read_u8(data, offset)?);
            let b2 = u32::from(read_u8(data, offset)?);
            let b3 = u32::from(read_u8(data, offset)?);
            let count = (top_bits << 24) | (b1 << 16) | (b2 << 8) | b3;
            if count > MAX_REFERRED_COUNT {
                return Err(ParseError::ExcessiveReferredCount { count });
            }
            let flags_bytes_needed = ((count + 8) / 8) as usize;
            let flags_slice = read_slice(data, offset, flags_bytes_needed)?;
            (count, 0, flags_slice.to_vec())
        }
        // Values 5 and 6 are reserved per §7.2.4.
        _ => {
            return Err(ParseError::ReservedReferredCount {
                flags_byte: rc_byte,
            })
        }
    };

    // §7.2.5 referred-segment numbers. Width depends on the highest
    // segment number encountered so far in the stream; we derive a
    // lower bound from this segment's own number (segment numbers are
    // monotonically non-decreasing in a well-formed stream).
    let field_width = ReferredFieldWidth::for_max_segnum(segment_number);
    let mut referred_segments = Vec::with_capacity(referred_count as usize);
    for _ in 0..referred_count {
        let num = match field_width {
            ReferredFieldWidth::One => u32::from(read_u8(data, offset)?),
            ReferredFieldWidth::Two => u32::from(read_u16_be(data, offset)?),
            ReferredFieldWidth::Four => read_u32_be(data, offset)?,
        };
        referred_segments.push(num);
    }

    // §7.2.6 segment page association.
    let page_association = if page_assoc_is_4_bytes {
        read_u32_be(data, offset)?
    } else {
        u32::from(read_u8(data, offset)?)
    };

    // §7.2.7 segment data length: always 4-byte big-endian. The
    // "unknown length" sentinel `u32::MAX` is legal only for the
    // immediate-generic / refinement region types at end-of-page.
    let raw_length = read_u32_be(data, offset)?;
    let data_length = u64::from(raw_length);

    let header_length = *offset - start;
    let data_offset = *offset;

    Ok(SegmentHeader {
        segment_number,
        segment_type,
        deferred_non_retain,
        page_association,
        retain_flags,
        extended_retain_flags,
        referred_segments,
        data_length,
        data_offset,
        header_length,
        referred_segment_field_width: field_width,
    })
}

/// Parse every segment header in a `/JBIG2Globals` stream.
///
/// Per ISO 32000-2 §8.9.5.4, the globals stream is a concatenation of
/// segments that all carry `page_association = 0` (global scope). They
/// are prepended to each per-page segment list at decode time.
///
/// This is currently scaffolding: it parses the headers but returns
/// them unresolved. Actual resolution of the PDF indirect stream
/// reference (walking a `PdfObject` to bytes) is wired in.
pub fn parse_globals(data: &[u8]) -> Result<Vec<SegmentHeader>, ParseError> {
    let mut out = Vec::new();
    let mut offset = 0;
    while offset < data.len() {
        let header = parse_header(data, &mut offset)?;
        // Advance past the segment's data on success. Unknown-length
        // end-of-page markers have no place in a globals stream, so
        // we treat them as terminal.
        match usize::try_from(header.data_length) {
            Ok(n) if offset.checked_add(n).is_some_and(|end| end <= data.len()) => {
                offset += n;
                out.push(header);
            }
            _ => {
                // Truncated or unknown-length; stop and return what we have.
                out.push(header);
                break;
            }
        }
    }
    Ok(out)
}

/// Merge a globals segment list with a per-page segment list,
/// renumbering the page-stream segments if their numbers collide with
/// globals.
///
/// Returns the merged segment list (globals first, then per-page) along
/// with a map of old -> new segment numbers for the renumbered page
/// segments (empty if no collisions).
///
/// This is scaffolding for. The actual text-region decoder
/// will consume the merged list.
pub fn merge_globals_and_page(
    globals: &[SegmentHeader],
    page: &[SegmentHeader],
) -> (Vec<SegmentHeader>, Vec<(u32, u32)>) {
    use std::collections::HashSet;
    let global_numbers: HashSet<u32> = globals.iter().map(|s| s.segment_number).collect();
    let mut renumbered = Vec::new();
    let mut merged: Vec<SegmentHeader> = globals.to_vec();

    // Compute the next free number above both sets for collision resolution.
    let max_global = globals.iter().map(|s| s.segment_number).max().unwrap_or(0);
    let mut next_free = max_global.saturating_add(1);

    for seg in page {
        if global_numbers.contains(&seg.segment_number) {
            let new_num = next_free;
            next_free = next_free.saturating_add(1);
            renumbered.push((seg.segment_number, new_num));
            let mut cloned = seg.clone();
            cloned.segment_number = new_num;
            merged.push(cloned);
        } else {
            merged.push(seg.clone());
        }
    }

    (merged, renumbered)
}

// ---------------------------------------------------------------------------
// byte-level readers
// ---------------------------------------------------------------------------

fn read_u8(data: &[u8], offset: &mut usize) -> Result<u8, ParseError> {
    if *offset >= data.len() {
        return Err(ParseError::UnexpectedEof {
            at: *offset,
            needed: 1,
        });
    }
    let b = data[*offset];
    *offset += 1;
    Ok(b)
}

fn read_u16_be(data: &[u8], offset: &mut usize) -> Result<u16, ParseError> {
    let slice = read_slice(data, offset, 2)?;
    Ok(u16::from_be_bytes([slice[0], slice[1]]))
}

fn read_u32_be(data: &[u8], offset: &mut usize) -> Result<u32, ParseError> {
    let slice = read_slice(data, offset, 4)?;
    Ok(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_slice<'a>(data: &'a [u8], offset: &mut usize, n: usize) -> Result<&'a [u8], ParseError> {
    if data.len() < *offset + n {
        return Err(ParseError::UnexpectedEof {
            at: *offset,
            needed: n - (data.len() - *offset),
        });
    }
    let out = &data[*offset..*offset + n];
    *offset += n;
    Ok(out)
}

// ===========================================================================
// tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic minimal header: segnum N, type T, no refs, PA=1 byte, datalen D.
    /// 11 bytes total, matches the common PDF-embedded case.
    fn minimal_header_bytes(segnum: u32, type_code: u8, page_assoc: u8, datalen: u32) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&segnum.to_be_bytes());
        v.push(type_code & 0x3F); // no PA4 bit, no deferred bit
        v.push(0x00); // rc=0, retain=0
        v.push(page_assoc);
        v.extend_from_slice(&datalen.to_be_bytes());
        v
    }

    /// Assert the round-trip of a minimal header parses to the expected type.
    fn assert_type_roundtrip(type_code: u8, expected: SegmentType) {
        let bytes = minimal_header_bytes(7, type_code, 1, 42);
        let mut off = 0;
        let hdr = parse_header(&bytes, &mut off).expect("parse minimal header");
        assert_eq!(hdr.segment_number, 7);
        assert_eq!(hdr.segment_type, expected);
        assert_eq!(hdr.page_association, 1);
        assert_eq!(hdr.data_length, 42);
        assert_eq!(hdr.header_length, 11);
        assert_eq!(hdr.data_offset, 11);
        // round-trip of the type code
        assert_eq!(hdr.segment_type.code(), type_code);
        assert_eq!(off, 11);
    }

    // --- per-type unit tests ---

    #[test]
    fn parses_symbol_dictionary() {
        assert_type_roundtrip(0, SegmentType::SymbolDictionary);
    }

    #[test]
    fn parses_intermediate_text_region() {
        assert_type_roundtrip(4, SegmentType::IntermediateTextRegion);
    }

    #[test]
    fn parses_immediate_text_region() {
        assert_type_roundtrip(6, SegmentType::ImmediateTextRegion);
    }

    #[test]
    fn parses_immediate_lossless_text_region() {
        assert_type_roundtrip(7, SegmentType::ImmediateLosslessTextRegion);
    }

    #[test]
    fn parses_pattern_dictionary() {
        assert_type_roundtrip(16, SegmentType::PatternDictionary);
    }

    #[test]
    fn parses_intermediate_halftone_region() {
        assert_type_roundtrip(20, SegmentType::IntermediateHalftoneRegion);
    }

    #[test]
    fn parses_immediate_halftone_region() {
        assert_type_roundtrip(22, SegmentType::ImmediateHalftoneRegion);
    }

    #[test]
    fn parses_immediate_lossless_halftone_region() {
        assert_type_roundtrip(23, SegmentType::ImmediateLosslessHalftoneRegion);
    }

    #[test]
    fn parses_intermediate_generic_region() {
        assert_type_roundtrip(36, SegmentType::IntermediateGenericRegion);
    }

    #[test]
    fn parses_immediate_generic_region() {
        assert_type_roundtrip(38, SegmentType::ImmediateGenericRegion);
    }

    #[test]
    fn parses_immediate_lossless_generic_region() {
        assert_type_roundtrip(39, SegmentType::ImmediateLosslessGenericRegion);
    }

    #[test]
    fn parses_intermediate_generic_refinement_region() {
        assert_type_roundtrip(40, SegmentType::IntermediateGenericRefinementRegion);
    }

    #[test]
    fn parses_immediate_generic_refinement_region() {
        assert_type_roundtrip(42, SegmentType::ImmediateGenericRefinementRegion);
    }

    #[test]
    fn parses_immediate_lossless_generic_refinement_region() {
        assert_type_roundtrip(43, SegmentType::ImmediateLosslessGenericRefinementRegion);
    }

    #[test]
    fn parses_page_information() {
        assert_type_roundtrip(48, SegmentType::PageInformation);
    }

    #[test]
    fn parses_end_of_page() {
        assert_type_roundtrip(49, SegmentType::EndOfPage);
    }

    #[test]
    fn parses_end_of_stripe() {
        assert_type_roundtrip(50, SegmentType::EndOfStripe);
    }

    #[test]
    fn parses_end_of_file() {
        assert_type_roundtrip(51, SegmentType::EndOfFile);
    }

    #[test]
    fn parses_profiles() {
        assert_type_roundtrip(52, SegmentType::Profiles);
    }

    #[test]
    fn parses_tables() {
        assert_type_roundtrip(53, SegmentType::Tables);
    }

    #[test]
    fn reserved_type_code_preserved() {
        // Type 1 is reserved per ISO 14492 §7.3; parser must surface
        // the raw code without crashing.
        let bytes = minimal_header_bytes(0, 1, 1, 0);
        let mut off = 0;
        let hdr = parse_header(&bytes, &mut off).expect("reserved type parses");
        assert_eq!(hdr.segment_type, SegmentType::Reserved(1));
        assert_eq!(hdr.segment_type.code(), 1);
    }

    // --- structural tests ---

    #[test]
    fn page_association_4_byte_form() {
        // flags byte: type=48 (page_info) | PA4 bit (0x40) = 0x70
        let mut v = Vec::new();
        v.extend_from_slice(&1u32.to_be_bytes()); // segnum
        v.push(0x70); // type 48 + PA is 4 bytes
        v.push(0x00); // rc=0
        v.extend_from_slice(&0x0000_2A7Bu32.to_be_bytes()); // PA
        v.extend_from_slice(&0u32.to_be_bytes()); // datalen
        let mut off = 0;
        let hdr = parse_header(&v, &mut off).expect("4-byte PA parses");
        assert_eq!(hdr.page_association, 0x2A7B);
        assert_eq!(hdr.header_length, 14);
    }

    #[test]
    fn deferred_non_retain_flag_propagates() {
        // flags byte: type=48 | deferred (0x80) = 0xB0
        let mut v = Vec::new();
        v.extend_from_slice(&0u32.to_be_bytes());
        v.push(0xB0);
        v.push(0x00);
        v.push(1);
        v.extend_from_slice(&0u32.to_be_bytes());
        let mut off = 0;
        let hdr = parse_header(&v, &mut off).expect("deferred flag parses");
        assert!(hdr.deferred_non_retain);
        assert_eq!(hdr.segment_type, SegmentType::PageInformation);
    }

    #[test]
    fn u64_data_length_via_large_u32() {
        // §7.2.7 data_length field is always 4 bytes. The `data_length`
        // on SegmentHeader is `u64` to accommodate the unknown-length
        // sentinel naturally. Verify a max-u32 value round-trips.
        let bytes = minimal_header_bytes(0, 38, 1, u32::MAX);
        let mut off = 0;
        let hdr = parse_header(&bytes, &mut off).expect("max-u32 datalen parses");
        assert_eq!(hdr.data_length, u64::from(u32::MAX));
        // Immediate generic region permits the unknown-length sentinel.
        assert!(hdr.segment_type.allows_unknown_length());
        assert!(hdr.total_length().is_none());
    }

    #[test]
    fn multi_referred_segments_1byte_width() {
        // segnum=10 (<=255), 3 refs: widths of 1 byte each.
        let mut v = Vec::new();
        v.extend_from_slice(&10u32.to_be_bytes());
        v.push(0); // type=0, PA=1byte, deferred=0
        v.push((3 << 5) | 0b00111); // count=3, retain flags=0b00111
        v.push(2);
        v.push(5);
        v.push(7);
        v.push(1); // PA
        v.extend_from_slice(&0u32.to_be_bytes()); // datalen
        let mut off = 0;
        let hdr = parse_header(&v, &mut off).expect("3-ref header parses");
        assert_eq!(hdr.referred_segments, vec![2, 5, 7]);
        assert_eq!(hdr.retain_flags, 0b00111);
        assert_eq!(hdr.referred_segment_field_width, ReferredFieldWidth::One);
    }

    #[test]
    fn multi_referred_segments_2byte_width() {
        // segnum=0x01FF (>255), 2 refs: widths of 2 bytes each.
        let mut v = Vec::new();
        v.extend_from_slice(&0x01FFu32.to_be_bytes());
        v.push(0);
        v.push((2 << 5) | 0b00011);
        v.extend_from_slice(&1u16.to_be_bytes());
        v.extend_from_slice(&0x0100u16.to_be_bytes());
        v.push(1);
        v.extend_from_slice(&0u32.to_be_bytes());
        let mut off = 0;
        let hdr = parse_header(&v, &mut off).expect("2byte-ref header parses");
        assert_eq!(hdr.referred_segments, vec![1, 0x0100]);
        assert_eq!(hdr.referred_segment_field_width, ReferredFieldWidth::Two);
    }

    #[test]
    fn long_form_referred_count() {
        // short_count=7 triggers long form; actual count = 5, so we need
        // ceil((5+1)/8) = 1 byte of retention flags, then 5 * 1-byte refs.
        let mut v = Vec::new();
        v.extend_from_slice(&3u32.to_be_bytes());
        v.push(0); // type=0, 1-byte PA
                   // rc byte: top 3 bits = 111, low 5 bits = top 5 of 29-bit count
                   // actual count = 5 means low 5 bits of the byte hold 0 (count fits
                   // entirely in the next 3 bytes).
        v.push(0b11100000);
        // next 3 bytes of count
        v.push(0); // b1
        v.push(0); // b2
        v.push(5); // b3: count = 5
        v.push(0xFF); // retain flags (1 byte for count=5)
                      // 5 referred segment numbers (1 byte each since segnum=3 < 256)
        v.extend_from_slice(&[1, 2, 3, 4, 5]);
        v.push(1); // PA
        v.extend_from_slice(&0u32.to_be_bytes()); // datalen
        let mut off = 0;
        let hdr = parse_header(&v, &mut off).expect("long-form count parses");
        assert_eq!(hdr.referred_segments, vec![1, 2, 3, 4, 5]);
        assert_eq!(hdr.extended_retain_flags, vec![0xFF]);
    }

    #[test]
    fn reserved_short_count_5_rejected() {
        let mut v = Vec::new();
        v.extend_from_slice(&0u32.to_be_bytes());
        v.push(0);
        v.push(5 << 5); // reserved short count
        v.push(1);
        v.extend_from_slice(&0u32.to_be_bytes());
        let mut off = 0;
        let err = parse_header(&v, &mut off).expect_err("reserved count rejected");
        assert!(matches!(err, ParseError::ReservedReferredCount { .. }));
    }

    #[test]
    fn reserved_short_count_6_rejected() {
        let mut v = Vec::new();
        v.extend_from_slice(&0u32.to_be_bytes());
        v.push(0);
        v.push(6 << 5); // also reserved
        v.push(1);
        v.extend_from_slice(&0u32.to_be_bytes());
        let mut off = 0;
        let err = parse_header(&v, &mut off).expect_err("reserved count rejected");
        assert!(matches!(err, ParseError::ReservedReferredCount { .. }));
    }

    #[test]
    fn unexpected_eof_on_truncated_header() {
        let short = vec![0u8; 5]; // truncated mid-flags
        let mut off = 0;
        let err = parse_header(&short, &mut off).expect_err("truncated header rejected");
        assert!(matches!(err, ParseError::UnexpectedEof { .. }));
    }

    #[test]
    fn page_assoc_zero_means_global() {
        // Globals stream convention: page_association = 0 => applies to all pages.
        let bytes = minimal_header_bytes(0, 0, 0, 7);
        let mut off = 0;
        let hdr = parse_header(&bytes, &mut off).expect("global seg parses");
        assert_eq!(hdr.page_association, 0);
    }

    // --- globals-resolver scaffolding tests ---

    #[test]
    fn parse_globals_walks_multiple_segments() {
        // Two back-to-back minimal segments, both with data_length=0.
        let mut s = minimal_header_bytes(0, 0, 0, 0);
        s.extend_from_slice(&minimal_header_bytes(1, 16, 0, 0));
        let segs = parse_globals(&s).expect("globals parses");
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].segment_type, SegmentType::SymbolDictionary);
        assert_eq!(segs[1].segment_type, SegmentType::PatternDictionary);
        assert!(segs.iter().all(|s| s.page_association == 0));
    }

    #[test]
    fn merge_globals_renumbers_on_collision() {
        // Globals: seg 0 (SymbolDictionary).
        // Page:    seg 0 (PatternDictionary) -- collides.
        let g0 = parse_header(&minimal_header_bytes(0, 0, 0, 0), &mut 0).unwrap();
        let p0 = parse_header(&minimal_header_bytes(0, 16, 1, 0), &mut 0).unwrap();
        let (merged, renumbered) = merge_globals_and_page(&[g0], &[p0]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].segment_number, 0);
        assert_ne!(merged[1].segment_number, 0);
        assert_eq!(renumbered.len(), 1);
        assert_eq!(renumbered[0].0, 0); // old
        assert_eq!(renumbered[0].1, merged[1].segment_number);
    }

    #[test]
    fn merge_globals_passes_through_when_disjoint() {
        // Disjoint numbers: no renumbering.
        let g0 = parse_header(&minimal_header_bytes(0, 0, 0, 0), &mut 0).unwrap();
        let p1 = parse_header(&minimal_header_bytes(1, 16, 1, 0), &mut 0).unwrap();
        let (merged, renumbered) = merge_globals_and_page(&[g0], &[p1]);
        assert_eq!(merged.len(), 2);
        assert!(renumbered.is_empty());
        // Actual resolution of the PDF indirect-stream reference to the
        // globals byte slice is deferred to; merge_globals_and_page
        // operates on already-parsed headers. See module docs.
    }
}
