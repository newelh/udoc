//! PDF document structure parser.
//!
//! Parses the structural skeleton of a PDF: header, startxref, xref tables,
//! and trailer dictionaries. Builds an object index mapping (obj_num, gen_num)
//! to file offsets for the object resolver to use.
//!
//! Handles:
//! - Traditional xref tables (PDF 1.0+)
//! - Incremental updates (chained via /Prev)
//! - PDF header version detection
//!
//! Xref streams (PDF 1.5+) are fully supported, including FlateDecode
//! decompression and binary entry parsing.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

#[cfg(any(test, feature = "test-internals"))]
use crate::diagnostics::NullDiagnostics;
use crate::diagnostics::{DiagnosticsSink, Warning, WarningKind};
use crate::error::{Error, ResultExt};
use crate::object::stream::{decode_stream, DecodeLimits};
use crate::object::{PdfDictionary, PdfObject};
use crate::parse::object_parser::ObjectParser;
use crate::Result;

use super::lexer::Token;

/// Maximum number of incremental update sections to follow.
/// Prevents infinite loops on circular /Prev chains.
const MAX_XREF_CHAIN_DEPTH: usize = 64;

/// Maximum /Size value accepted in an xref stream.
/// 10M objects is well beyond any legitimate PDF (ISO 32000 imposes no limit,
/// but the largest real-world files have ~1M objects). This prevents absurd
/// allocations from malicious /Size values while staying above any real need.
const MAX_XREF_STREAM_SIZE: i64 = 10_000_000;

/// How far back from EOF to scan for startxref.
/// PDF spec (ISO 32000-1:2008, Annex H) says 1024, but real-world PDFs
/// often have extra whitespace, comments, or trailing data after %%EOF.
/// 4096 bytes provides tolerance for common malformations while remaining
/// bounded (prevents scanning entire file for malicious inputs).
const STARTXREF_SCAN_SIZE: usize = 4096;

/// Maximum number of objects the repair scanner will collect.
/// Prevents runaway allocation on adversarial inputs.
const MAX_REPAIR_OBJECTS: usize = 500_000;

/// Maximum file size for repair mode scanning.
/// Files larger than this skip repair entirely to avoid DoS.
const MAX_REPAIR_SCAN_SIZE: usize = 256 * 1024 * 1024; // 256 MiB

/// PDF version extracted from the header.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PdfVersion {
    pub major: u8,
    pub minor: u8,
}

impl std::fmt::Display for PdfVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Where a particular object lives in the file.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum XrefEntry {
    /// Object is at this byte offset in the file (type 1 / "n" entry).
    Uncompressed { offset: u64, gen: u16 },
    /// Object is free (type 0 / "f" entry).
    Free { next_free: u32, gen: u16 },
    /// Object is in a compressed object stream (type 2, xref streams only).
    Compressed { stream_obj: u32, index: u32 },
}

/// Complete cross-reference index for a PDF document.
///
/// Maps object numbers to their location in the file. Built by walking
/// the xref chain from the most recent section backwards through /Prev.
#[derive(Debug, Clone)]
pub struct XrefTable {
    /// Mapping of object number to its entry.
    entries: HashMap<u32, XrefEntry>,
}

impl Default for XrefTable {
    fn default() -> Self {
        Self::new()
    }
}

impl XrefTable {
    /// Create an empty xref table.
    pub(crate) fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Look up where an object lives.
    pub fn get(&self, obj_num: u32) -> Option<&XrefEntry> {
        self.entries.get(&obj_num)
    }

    /// Number of entries in the table.
    #[cfg(any(test, feature = "test-internals"))]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table is empty.
    #[cfg(any(test, feature = "test-internals"))]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over all entries.
    #[cfg(any(test, feature = "test-internals"))]
    pub fn iter(&self) -> impl Iterator<Item = (u32, &XrefEntry)> {
        self.entries.iter().map(|(&k, v)| (k, v))
    }

    /// Insert an entry. Does NOT overwrite existing entries, because
    /// newer xref sections are processed first in the chain walk.
    pub(crate) fn insert_if_absent(&mut self, obj_num: u32, entry: XrefEntry) {
        self.entries.entry(obj_num).or_insert(entry);
    }
}

/// Result of parsing the document structure.
#[derive(Debug)]
#[non_exhaustive]
pub struct DocumentStructure {
    /// PDF version from the header.
    pub version: PdfVersion,
    /// Complete cross-reference table.
    pub xref: XrefTable,
    /// The final merged trailer dictionary.
    pub trailer: PdfDictionary,
}

impl DocumentStructure {
    /// Create a new document structure result.
    #[cfg(any(test, feature = "test-internals"))]
    pub fn new(version: PdfVersion, xref: XrefTable, trailer: PdfDictionary) -> Self {
        Self {
            version,
            xref,
            trailer,
        }
    }
}

/// Parses document structure from raw PDF bytes.
pub struct DocumentParser<'a> {
    data: &'a [u8],
    diagnostics: Arc<dyn DiagnosticsSink>,
}

impl fmt::Debug for DocumentParser<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DocumentParser")
            .field("data_len", &self.data.len())
            .finish()
    }
}

impl<'a> DocumentParser<'a> {
    /// Create a new document parser over the given byte slice.
    #[cfg(any(test, feature = "test-internals"))]
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            diagnostics: Arc::new(NullDiagnostics),
        }
    }

    /// Create a new document parser with a custom diagnostics sink.
    pub fn with_diagnostics(data: &'a [u8], diagnostics: Arc<dyn DiagnosticsSink>) -> Self {
        Self { data, diagnostics }
    }

    fn warn(&self, offset: u64, kind: WarningKind, message: impl Into<String>) {
        self.diagnostics
            .warning(Warning::new(Some(offset), kind, message));
    }

    fn emit_info(&self, kind: WarningKind, message: impl Into<String>) {
        self.diagnostics.info(Warning::info(kind, message));
    }

    /// Parse the complete document structure: header, xref, trailer.
    ///
    /// Tries three strategies in order:
    /// 1. Primary startxref offset (the last `startxref` keyword near EOF)
    /// 2. Alternate startxref offsets (earlier `startxref` keywords)
    /// 3. Repair mode (linear scan for object headers, synthetic xref)
    pub fn parse(&self) -> Result<DocumentStructure> {
        let version = self.parse_header().context("parsing PDF header")?;

        // Collect all startxref candidate offsets (last occurrence first)
        let candidates = self.find_all_startxref_offsets();

        // Try each candidate
        let mut last_err = None;
        for (i, &startxref_offset) in candidates.iter().enumerate() {
            match self.load_xref_chain(startxref_offset) {
                Ok((xref, trailer)) => {
                    if i > 0 {
                        self.emit_info(
                            WarningKind::MalformedXref,
                            format!(
                                "primary startxref failed, recovered using alternate \
                                 startxref at offset {startxref_offset}"
                            ),
                        );
                    }
                    return Ok(DocumentStructure {
                        version,
                        xref,
                        trailer,
                    });
                }
                Err(e) => {
                    last_err = Some(e);
                }
            }
        }

        // All startxref candidates failed. Try repair mode.
        match self.repair_xref() {
            Ok((entries, trailer)) => {
                let mut xref = XrefTable::new();
                for (obj_num, entry) in entries {
                    xref.insert_if_absent(obj_num, entry);
                }
                self.diagnostics.warning(Warning::new(
                    None,
                    WarningKind::MalformedXref,
                    "all startxref offsets failed, rebuilt xref via repair scan",
                ));
                return Ok(DocumentStructure {
                    version,
                    xref,
                    trailer,
                });
            }
            Err(_repair_err) => {
                // Return the original xref chain error, not the repair error,
                // since the original is more informative for well-formed files.
            }
        }

        Err(last_err.unwrap_or_else(|| {
            Error::structure("no startxref keyword found and repair scan failed")
        }))
    }

    /// Parse the PDF header to extract version. (F-307)
    ///
    /// Looks for `%PDF-X.Y` within the first 1024 bytes. The spec says
    /// byte 0, but some files have leading garbage (e.g., from email
    /// transport encoding).
    fn parse_header(&self) -> Result<PdfVersion> {
        let search_limit = self.data.len().min(1024);
        let search_region = &self.data[..search_limit];

        // Look for %PDF- marker
        let marker = b"%PDF-";
        let pos = find_bytes(search_region, marker).ok_or_else(|| {
            Error::structure("PDF header (%PDF-X.Y) not found in first 1024 bytes")
        })?;

        if pos > 0 {
            self.warn(
                0,
                WarningKind::GarbageBytes,
                format!("PDF header found at offset {pos}, expected offset 0"),
            );
        }

        // Parse version digits: expect "X.Y" after %PDF-
        let version_start = pos + marker.len();
        if version_start + 3 > self.data.len() {
            return Err(Error::structure(
                "PDF header truncated: version number missing",
            ));
        }

        let major = self.data[version_start];
        let dot = self.data[version_start + 1];
        let minor = self.data[version_start + 2];

        if !major.is_ascii_digit() || dot != b'.' || !minor.is_ascii_digit() {
            return Err(Error::parse(
                version_start as u64,
                "version number (e.g., 1.7)",
                format!("{}{}{}", major as char, dot as char, minor as char),
            ));
        }

        Ok(PdfVersion {
            major: major - b'0',
            minor: minor - b'0',
        })
    }

    /// Locate the startxref offset from near the end of the file. (F-308)
    ///
    /// Scans backwards from EOF for the `startxref` keyword, then reads
    /// the integer offset that follows it.
    ///
    /// Not called by `parse()` directly (which uses `find_all_startxref_offsets`
    /// for multi-candidate fallback), but kept for tests.
    #[cfg(test)]
    fn find_startxref(&self) -> Result<u64> {
        let scan_start = self.data.len().saturating_sub(STARTXREF_SCAN_SIZE);
        let tail = &self.data[scan_start..];

        // Find the last occurrence of "startxref" in the tail
        let keyword = b"startxref";
        let pos = rfind_bytes(tail, keyword)
            .ok_or_else(|| Error::structure("startxref keyword not found near end of file"))?;

        // Parse the integer after "startxref" + whitespace
        let after_keyword = scan_start + pos + keyword.len();
        let offset_str = self
            .read_integer_after_whitespace(after_keyword)
            .context("parsing startxref offset")?;

        let offset: u64 = offset_str.parse().map_err(|_| {
            Error::parse(
                after_keyword as u64,
                "integer offset",
                format!("'{offset_str}'"),
            )
        })?;

        if offset as usize >= self.data.len() {
            return Err(Error::structure_at(
                offset,
                format!(
                    "startxref offset {offset} is beyond end of file ({})",
                    self.data.len()
                ),
            ));
        }

        Ok(offset)
    }

    /// Find all `startxref` keyword occurrences near EOF and return their
    /// parsed byte offsets, ordered from last (most likely correct) to first.
    ///
    /// Returns an empty Vec only if no `startxref` keyword is found at all.
    /// Invalid occurrences (non-numeric value, offset past EOF) are silently
    /// skipped rather than included.
    fn find_all_startxref_offsets(&self) -> Vec<u64> {
        let scan_start = self.data.len().saturating_sub(STARTXREF_SCAN_SIZE);
        let tail = &self.data[scan_start..];
        let keyword = b"startxref";

        // Collect all occurrences of "startxref" in the tail region
        let mut positions = Vec::new();
        let mut search_start = 0;
        while search_start + keyword.len() <= tail.len() {
            if let Some(pos) = find_bytes(&tail[search_start..], keyword) {
                positions.push(search_start + pos);
                search_start += pos + keyword.len();
            } else {
                break;
            }
        }

        // Parse each occurrence into an offset, filtering out invalid ones.
        // Reverse so the last occurrence (most likely correct) is first.
        let mut offsets = Vec::new();
        for &pos in positions.iter().rev() {
            let after_keyword = scan_start + pos + keyword.len();
            if let Ok(offset_str) = self.read_integer_after_whitespace(after_keyword) {
                if let Ok(offset) = offset_str.parse::<u64>() {
                    if (offset as usize) < self.data.len() {
                        offsets.push(offset);
                    }
                }
            }
        }

        offsets
    }

    /// Read a sequence of ASCII digits after skipping whitespace.
    fn read_integer_after_whitespace(&self, start: usize) -> Result<String> {
        let mut pos = start;

        // Skip whitespace
        while pos < self.data.len() && is_pdf_whitespace(self.data[pos]) {
            pos += 1;
        }

        if pos >= self.data.len() {
            return Err(Error::parse(start as u64, "integer", "end of file"));
        }

        // Read digits
        let digit_start = pos;
        while pos < self.data.len() && self.data[pos].is_ascii_digit() {
            pos += 1;
        }

        if pos == digit_start {
            return Err(Error::parse(
                digit_start as u64,
                "digit",
                format!("byte 0x{:02X}", self.data[digit_start]),
            ));
        }

        // All bytes are ASCII digits (verified above), so UTF-8 conversion is infallible.
        // Use from_utf8_lossy for defense-in-depth (no panic on impossible failure).
        Ok(String::from_utf8_lossy(&self.data[digit_start..pos]).into_owned())
    }

    /// Load the full xref chain starting at the given offset. (F-301, F-302, F-303)
    ///
    /// Walks the chain of xref sections linked by /Prev pointers in the
    /// trailer dictionaries. The most recent section is parsed first, and
    /// its entries take priority (insert_if_absent).
    fn load_xref_chain(&self, initial_offset: u64) -> Result<(XrefTable, PdfDictionary)> {
        let mut xref = XrefTable::new();
        let mut final_trailer: Option<PdfDictionary> = None;
        let mut current_offset = Some(initial_offset);
        let mut depth = 0;
        let mut visited_offsets: Vec<u64> = Vec::new();

        while let Some(offset) = current_offset {
            if depth >= MAX_XREF_CHAIN_DEPTH {
                self.warn(
                    offset,
                    WarningKind::MalformedXref,
                    format!(
                        "xref chain depth limit ({MAX_XREF_CHAIN_DEPTH}) reached, \
                         stopping chain walk"
                    ),
                );
                break;
            }

            // Circular reference detection
            if visited_offsets.contains(&offset) {
                self.warn(
                    offset,
                    WarningKind::MalformedXref,
                    format!("circular /Prev reference at offset {offset}, stopping chain walk"),
                );
                break;
            }
            visited_offsets.push(offset);

            // Detect whether this is a traditional xref table or an xref stream
            let section_result = if self.is_xref_table_at(offset) {
                self.parse_xref_section(offset)
                    .context(format!("parsing xref section at offset {offset}"))
            } else {
                self.parse_xref_stream(offset)
                    .context(format!("parsing xref stream at offset {offset}"))
            };

            let (section_entries, trailer) = match section_result {
                Ok(result) => result,
                Err(e) => {
                    if depth == 0 {
                        // First section failed (can't recover)
                        return Err(e);
                    }
                    // Later section failed (we already have some data, warn and stop)
                    self.warn(
                        offset,
                        WarningKind::MalformedXref,
                        format!(
                            "failed to parse xref section at offset {offset}, stopping chain walk"
                        ),
                    );
                    break;
                }
            };

            // Merge entries (first seen wins, so most recent section's entries stick)
            for (obj_num, entry) in &section_entries {
                xref.insert_if_absent(*obj_num, *entry);
            }

            // /XRefStm support (ISO 32000-1, Section 7.5.8.4):
            // Traditional xref tables may include a /XRefStm key in the trailer
            // pointing to an xref stream that provides entries for compressed
            // objects. The stream entries fill in gaps only (don't override
            // entries already present from the traditional table).
            if self.is_xref_table_at(offset) {
                if let Some(xref_stm_offset) = trailer.get_i64(b"XRefStm") {
                    if xref_stm_offset >= 0 && (xref_stm_offset as usize) < self.data.len() {
                        let stm_offset = xref_stm_offset as u64;
                        if !visited_offsets.contains(&stm_offset) {
                            match self.parse_xref_stream(stm_offset) {
                                Ok((stm_entries, _stm_dict)) => {
                                    visited_offsets.push(stm_offset);
                                    // Only fill gaps: insert_if_absent means
                                    // traditional table entries take priority
                                    for (obj_num, entry) in stm_entries {
                                        xref.insert_if_absent(obj_num, entry);
                                    }
                                }
                                Err(e) => {
                                    self.warn(
                                        stm_offset,
                                        WarningKind::MalformedXref,
                                        format!(
                                            "failed to parse /XRefStm at offset {stm_offset}: {e}"
                                        ),
                                    );
                                }
                            }
                        }
                    } else {
                        self.warn(
                            offset,
                            WarningKind::MalformedXref,
                            format!("/XRefStm offset {} is invalid", xref_stm_offset),
                        );
                    }
                }
            }

            // Keep the first (most recent) trailer as the final one
            if final_trailer.is_none() {
                final_trailer = Some(trailer.clone());
            }

            // Follow /Prev to the next xref section
            current_offset = trailer.get(b"Prev").and_then(|obj| match obj.as_i64() {
                Some(prev) if prev >= 0 => Some(prev as u64),
                Some(prev) => {
                    self.warn(
                        offset,
                        WarningKind::MalformedXref,
                        format!("/Prev value is negative ({prev})"),
                    );
                    None
                }
                None => {
                    self.warn(
                        offset,
                        WarningKind::MalformedXref,
                        "/Prev is not an integer",
                    );
                    None
                }
            });

            depth += 1;
        }

        let trailer =
            final_trailer.ok_or_else(|| Error::structure("no trailer dictionary found"))?;

        Ok((xref, trailer))
    }

    /// Check if the bytes at the given offset look like "xref" (traditional table).
    fn is_xref_table_at(&self, offset: u64) -> bool {
        let Ok(offset) = usize::try_from(offset) else {
            return false;
        };
        if offset + 4 > self.data.len() {
            return false;
        }
        &self.data[offset..offset + 4] == b"xref"
    }

    /// Parse a single xref section (traditional table format) and its trailer. (F-301, F-302)
    ///
    /// Returns the entries from this section and the trailer dictionary.
    fn parse_xref_section(&self, offset: u64) -> Result<(Vec<(u32, XrefEntry)>, PdfDictionary)> {
        let mut pos = usize::try_from(offset).map_err(|_| {
            Error::structure_at(offset, "xref section offset exceeds addressable range")
        })?;

        // Skip "xref" keyword
        if pos + 4 > self.data.len() || &self.data[pos..pos + 4] != b"xref" {
            let found = if pos + 4 <= self.data.len() {
                format!("'{}'", String::from_utf8_lossy(&self.data[pos..pos + 4]))
            } else {
                "end of file".into()
            };
            return Err(Error::parse(offset, "'xref' keyword", found));
        }
        pos += 4;
        pos = skip_whitespace(self.data, pos);

        let mut entries = Vec::new();

        // Parse subsections until we hit "trailer"
        loop {
            pos = skip_whitespace(self.data, pos);

            if pos >= self.data.len() {
                return Err(Error::parse(pos as u64, "'trailer' keyword", "end of file"));
            }

            // Check for "trailer" keyword
            if self.data[pos..].starts_with(b"trailer") {
                pos += 7; // skip "trailer"
                break;
            }

            // Parse subsection header: <first_obj_num> <count>
            let (first_obj, new_pos) = self
                .parse_ascii_integer(pos)
                .context("parsing xref subsection first object number")?;
            pos = skip_whitespace(self.data, new_pos);

            let (count, new_pos) = self
                .parse_ascii_integer(pos)
                .context("parsing xref subsection count")?;
            pos = skip_whitespace(self.data, new_pos);

            // Validate and convert first_obj to u32
            let first_obj = u32::try_from(first_obj).map_err(|_| {
                Error::parse(
                    pos as u64,
                    "valid first object number (0..2^32)",
                    format!("{first_obj}"),
                )
            })?;

            // Validate and convert count to u32
            let count = u32::try_from(count).map_err(|_| {
                Error::parse(pos as u64, "valid count (0..2^32)", format!("{count}"))
            })?;

            // Validate that first_obj + count won't overflow u32
            if let Some(_last_obj) = first_obj.checked_add(count.saturating_sub(1)) {
                // Also sanity check that count doesn't exceed available data
                // Each entry is 20 bytes minimum
                if count as u64 * 20 > self.data.len().saturating_sub(pos) as u64 {
                    self.warn(
                        pos as u64,
                        WarningKind::MalformedXref,
                        format!(
                            "xref subsection count {count} * 20 bytes exceeds remaining data, \
                             clamping to available space"
                        ),
                    );
                }
            } else {
                return Err(Error::parse(
                    pos as u64,
                    "valid object number range",
                    format!("first_obj {first_obj} + count {count} would overflow u32"),
                ));
            }

            // Parse entries: each is exactly 20 bytes "OOOOOOOOOO GGGGG X \r\n"
            // But be lenient about the line ending (could be \r\n, \n, or \r).
            for i in 0..count {
                let obj_num = first_obj.checked_add(i).ok_or_else(|| {
                    Error::structure_at(pos as u64, "object number overflow in xref subsection")
                })?;
                let entry_result = self.parse_xref_entry(pos, obj_num);
                match entry_result {
                    Ok((entry, new_pos)) => {
                        entries.push((obj_num, entry));
                        pos = new_pos;
                    }
                    Err(e) => {
                        self.warn(
                            pos as u64,
                            WarningKind::MalformedXref,
                            format!("malformed xref entry for object {}: {e}", obj_num),
                        );
                        // Try to skip 20 bytes and continue
                        pos += 20;
                        if pos > self.data.len() {
                            return Err(e)
                                .context(format!("parsing xref entry for object {}", obj_num));
                        }
                    }
                }
            }
        }

        // Parse trailer dictionary
        pos = skip_whitespace(self.data, pos);
        let trailer = self
            .parse_dictionary_at(pos)
            .context("parsing trailer dictionary")?;

        Ok((entries, trailer))
    }

    /// Parse a single 20-byte xref entry.
    ///
    /// Format: "OOOOOOOOOO GGGGG n \r\n" or "OOOOOOOOOO GGGGG f \r\n"
    /// where O = offset (10 digits), G = generation (5 digits), n/f = in-use/free.
    fn parse_xref_entry(&self, pos: usize, obj_num: u32) -> Result<(XrefEntry, usize)> {
        // Need at least 18 bytes for "OOOOOOOOOO GGGGG X" plus line ending
        if pos + 18 > self.data.len() {
            return Err(Error::parse(
                pos as u64,
                "20-byte xref entry",
                "end of file",
            ));
        }

        // Parse offset (10 digits)
        let offset_slice = &self.data[pos..pos + 10];
        let offset_str = std::str::from_utf8(offset_slice)
            .map_err(|_| Error::parse(pos as u64, "10-digit offset", "non-ASCII bytes"))?;
        let offset: u64 = offset_str
            .trim()
            .parse()
            .map_err(|_| Error::parse(pos as u64, "10-digit offset", format!("'{offset_str}'")))?;

        // Expect space at position 10
        if self.data[pos + 10] != b' ' {
            self.warn(
                (pos + 10) as u64,
                WarningKind::MalformedXref,
                format!(
                    "expected space after offset in xref entry for object {obj_num}, \
                     got 0x{:02X}",
                    self.data[pos + 10]
                ),
            );
        }

        // Parse generation (5 digits)
        let gen_slice = &self.data[pos + 11..pos + 16];
        let gen_str = std::str::from_utf8(gen_slice).map_err(|_| {
            Error::parse((pos + 11) as u64, "5-digit generation", "non-ASCII bytes")
        })?;
        let gen: u16 = gen_str.trim().parse().map_err(|_| {
            Error::parse(
                (pos + 11) as u64,
                "5-digit generation",
                format!("'{gen_str}'"),
            )
        })?;

        // Expect space at position 16
        if self.data[pos + 16] != b' ' {
            self.warn(
                (pos + 16) as u64,
                WarningKind::MalformedXref,
                format!(
                    "expected space before type marker in xref entry for object {obj_num}, \
                     got 0x{:02X}",
                    self.data[pos + 16]
                ),
            );
        }

        // Type marker at position 17: 'n' (in-use) or 'f' (free)
        let type_marker = self.data[pos + 17];

        let entry = match type_marker {
            b'n' => XrefEntry::Uncompressed { offset, gen },
            b'f' => {
                let next_free = u32::try_from(offset).unwrap_or_else(|_| {
                    self.warn(
                        pos as u64,
                        WarningKind::MalformedXref,
                        format!("free entry next_free {offset} exceeds u32, clamping to 0"),
                    );
                    0
                });
                XrefEntry::Free { next_free, gen }
            }
            _ => {
                return Err(Error::parse(
                    (pos + 17) as u64,
                    "'n' or 'f'",
                    format!("0x{type_marker:02X}"),
                ));
            }
        };

        // Skip past the entry. Standard is 20 bytes (ending with \r\n),
        // but handle \n alone, \r alone, or \r\n.
        let mut end = pos + 18;
        // Skip optional space after type marker
        if end < self.data.len() && self.data[end] == b' ' {
            end += 1;
        }
        // Skip line ending
        if end < self.data.len() && self.data[end] == b'\r' {
            end += 1;
        }
        if end < self.data.len() && self.data[end] == b'\n' {
            end += 1;
        }

        Ok((entry, end))
    }

    /// Parse an ASCII integer at the given position.
    /// Returns (value, position_after_integer).
    fn parse_ascii_integer(&self, start: usize) -> Result<(i64, usize)> {
        let mut pos = start;

        if pos >= self.data.len() {
            return Err(Error::parse(start as u64, "integer", "end of file"));
        }

        // Handle optional sign
        let negative = self.data[pos] == b'-';
        if self.data[pos] == b'-' || self.data[pos] == b'+' {
            pos += 1;
        }

        let digit_start = pos;
        while pos < self.data.len() && self.data[pos].is_ascii_digit() {
            pos += 1;
        }

        if pos == digit_start {
            return Err(Error::parse(
                start as u64,
                "digit",
                if pos < self.data.len() {
                    format!("byte 0x{:02X}", self.data[pos])
                } else {
                    "end of file".into()
                },
            ));
        }

        let digits = std::str::from_utf8(&self.data[digit_start..pos])
            .map_err(|_| Error::parse(start as u64, "ASCII digits", "non-ASCII bytes"))?;

        let value: i64 = digits
            .parse()
            .map_err(|_| Error::parse(start as u64, "integer value", format!("'{digits}'")))?;

        Ok((if negative { -value } else { value }, pos))
    }

    /// Parse a dictionary at the given byte position using the ObjectParser.
    fn parse_dictionary_at(&self, pos: usize) -> Result<PdfDictionary> {
        if pos >= self.data.len() {
            return Err(Error::parse(pos as u64, "dictionary", "end of file"));
        }

        let mut parser =
            ObjectParser::with_diagnostics(&self.data[pos..], self.diagnostics.clone());

        let obj = parser.parse_object().context("parsing dictionary object")?;

        match obj {
            PdfObject::Dictionary(d) => Ok(d),
            other => Err(Error::parse(pos as u64, "dictionary", format!("{other}"))),
        }
    }

    /// Parse an xref stream (PDF 1.5+) at the given byte offset.
    ///
    /// Xref streams are indirect objects whose body is a stream containing
    /// binary-encoded cross-reference entries. The stream dictionary doubles
    /// as the trailer dictionary.
    fn parse_xref_stream(&self, offset: u64) -> Result<(Vec<(u32, XrefEntry)>, PdfDictionary)> {
        let start = usize::try_from(offset).map_err(|_| {
            Error::structure_at(offset, "xref stream offset exceeds addressable range")
        })?;
        if start >= self.data.len() {
            return Err(Error::structure_at(
                offset,
                format!("xref stream offset {offset} is beyond end of file"),
            ));
        }

        // Parse the indirect object: N G obj <stream> endobj
        let mut parser =
            ObjectParser::with_diagnostics(&self.data[start..], self.diagnostics.clone());

        let lexer = parser.lexer_mut();
        let obj_num_token = lexer.next_token();
        if !matches!(obj_num_token, Token::Integer(_)) {
            return Err(Error::parse(
                offset,
                "object number",
                format!("{obj_num_token:?}"),
            ));
        }
        let gen_token = lexer.next_token();
        if !matches!(gen_token, Token::Integer(_)) {
            return Err(Error::parse(
                offset,
                "generation number",
                format!("{gen_token:?}"),
            ));
        }
        let obj_keyword = lexer.next_token();
        if obj_keyword != Token::Obj {
            return Err(Error::parse(
                offset,
                "'obj' keyword",
                format!("{obj_keyword:?}"),
            ));
        }

        let stream_obj = parser
            .parse_object()
            .context("parsing xref stream object")?;

        let stream = match stream_obj {
            PdfObject::Stream(s) => s,
            other => {
                return Err(Error::parse(
                    offset,
                    "stream object",
                    other.type_name().to_string(),
                ));
            }
        };

        let dict = stream.dict;

        // -- Validate the stream dictionary --

        // /Type must be /XRef
        if let Some(type_name) = dict.get_name(b"Type") {
            if type_name != b"XRef" {
                return Err(Error::structure_at(
                    offset,
                    format!(
                        "xref stream /Type is /{}, expected /XRef",
                        String::from_utf8_lossy(type_name)
                    ),
                ));
            }
        } else {
            self.warn(
                offset,
                WarningKind::MalformedXref,
                "xref stream missing /Type, assuming /XRef",
            );
        }

        // /Size (required)
        let size = dict
            .get_i64(b"Size")
            .ok_or_else(|| Error::structure_at(offset, "xref stream missing required /Size"))?;
        if size < 0 {
            return Err(Error::structure_at(
                offset,
                format!("xref stream /Size is negative ({size})"),
            ));
        }
        if size > MAX_XREF_STREAM_SIZE {
            return Err(Error::structure_at(
                offset,
                format!(
                    "xref stream /Size ({size}) exceeds maximum allowed ({MAX_XREF_STREAM_SIZE})"
                ),
            ));
        }

        // /W (required): array of exactly 3 non-negative integers, each 0-4
        let w_array = dict
            .get_array(b"W")
            .ok_or_else(|| Error::structure_at(offset, "xref stream missing required /W array"))?;
        if w_array.len() != 3 {
            return Err(Error::structure_at(
                offset,
                format!("xref stream /W has {} elements, expected 3", w_array.len()),
            ));
        }
        let mut w = [0usize; 3];
        for (i, obj) in w_array.iter().enumerate() {
            let val = obj.as_i64().ok_or_else(|| {
                Error::structure_at(offset, format!("xref stream /W[{i}] is not an integer"))
            })?;
            if !(0..=4).contains(&val) {
                return Err(Error::structure_at(
                    offset,
                    format!("xref stream /W[{i}] = {val}, must be 0-4"),
                ));
            }
            w[i] = val as usize;
        }
        let row_width = w[0] + w[1] + w[2];
        if row_width == 0 {
            return Err(Error::structure_at(
                offset,
                "xref stream /W total width is 0",
            ));
        }

        // /Index (optional): pairs of [first count ...], defaults to [0 Size]
        let index_pairs = if let Some(index_array) = dict.get_array(b"Index") {
            if index_array.len() % 2 != 0 {
                return Err(Error::structure_at(
                    offset,
                    format!("xref stream /Index has odd length ({})", index_array.len()),
                ));
            }
            let mut pairs = Vec::new();
            for chunk in index_array.chunks(2) {
                let first = chunk[0].as_i64().ok_or_else(|| {
                    Error::structure_at(offset, "xref stream /Index element is not an integer")
                })?;
                let count = chunk[1].as_i64().ok_or_else(|| {
                    Error::structure_at(offset, "xref stream /Index element is not an integer")
                })?;
                if first < 0 || count < 0 {
                    return Err(Error::structure_at(
                        offset,
                        format!("xref stream /Index has negative value ({first}, {count})"),
                    ));
                }
                pairs.push((first as u32, count as u64));
            }
            pairs
        } else {
            vec![(0u32, size as u64)]
        };

        // -- Decode the stream body --
        // data_offset is relative to the sub-slice (&self.data[start.]) and
        // already points past the post-"stream" EOL (ObjectParser calls
        // skip_stream_eol() before recording data_offset).
        let abs_data_start = start
            .checked_add(stream.data_offset as usize)
            .ok_or_else(|| Error::structure_at(offset, "xref stream data offset overflow"))?;

        let data_length = stream.data_length as usize;
        let abs_data_end = abs_data_start
            .checked_add(data_length)
            .ok_or_else(|| Error::structure_at(offset, "xref stream data end overflow"))?;
        if abs_data_end > self.data.len() {
            return Err(Error::structure_at(
                offset,
                format!(
                    "xref stream data extends beyond file (offset {abs_data_start}, length {data_length})"
                ),
            ));
        }
        let raw_data = &self.data[abs_data_start..abs_data_end];

        let decoded = decode_stream(
            raw_data,
            &dict,
            &DecodeLimits::default(),
            &*self.diagnostics,
            abs_data_start as u64,
        )
        .context("decoding xref stream")?;

        // -- Parse binary entries from decoded data --
        let total_entries: u64 = index_pairs
            .iter()
            .map(|(_, count)| *count)
            .try_fold(0u64, |acc, c| acc.checked_add(c))
            .ok_or_else(|| {
                Error::structure_at(offset, "xref stream /Index total count overflow")
            })?;

        let expected_bytes = total_entries
            .checked_mul(row_width as u64)
            .ok_or_else(|| Error::structure_at(offset, "xref stream size overflow"))?;

        let actual_entries = if expected_bytes > decoded.len() as u64 {
            self.warn(
                offset,
                WarningKind::MalformedXref,
                format!(
                    "xref stream declares {} entries ({} bytes) but decoded data \
                     is only {} bytes, clamping",
                    total_entries,
                    expected_bytes,
                    decoded.len()
                ),
            );
            decoded.len() as u64 / row_width as u64
        } else {
            total_entries
        };

        let mut entries = Vec::new();
        let mut data_pos = 0usize;
        let mut entries_remaining = actual_entries;

        for &(first, count) in &index_pairs {
            for i in 0..count {
                if entries_remaining == 0 {
                    break;
                }
                if data_pos + row_width > decoded.len() {
                    break;
                }

                let obj_num = match first.checked_add(i as u32) {
                    Some(n) => n,
                    None => {
                        self.warn(
                            offset,
                            WarningKind::MalformedXref,
                            format!("xref stream object number overflow at {first} + {i}"),
                        );
                        data_pos += row_width;
                        entries_remaining -= 1;
                        continue;
                    }
                };

                let field0 = read_field_be(&decoded[data_pos..], w[0]);
                let field1 = read_field_be(&decoded[data_pos + w[0]..], w[1]);
                let field2 = read_field_be(&decoded[data_pos + w[0] + w[1]..], w[2]);

                // Default type is 1 (uncompressed) if w[0] is 0
                let entry_type = if w[0] == 0 { 1 } else { field0 };

                let entry = match entry_type {
                    0 => {
                        let next_free = u32::try_from(field1).unwrap_or_else(|_| {
                            self.warn(
                                offset,
                                WarningKind::MalformedXref,
                                format!("free entry next_free {field1} exceeds u32, clamping to 0"),
                            );
                            0
                        });
                        let gen = u16::try_from(field2).unwrap_or_else(|_| {
                            self.warn(
                                offset,
                                WarningKind::MalformedXref,
                                format!("free entry gen {field2} exceeds u16, clamping to 0"),
                            );
                            0
                        });
                        XrefEntry::Free { next_free, gen }
                    }
                    1 => {
                        let entry_offset = field1;
                        let gen = u16::try_from(field2).unwrap_or_else(|_| {
                            self.warn(
                                offset,
                                WarningKind::MalformedXref,
                                format!(
                                    "uncompressed entry gen {field2} exceeds u16, clamping to 0"
                                ),
                            );
                            0
                        });
                        XrefEntry::Uncompressed {
                            offset: entry_offset,
                            gen,
                        }
                    }
                    2 => {
                        let stream_obj = u32::try_from(field1).unwrap_or_else(|_| {
                            self.warn(
                                offset,
                                WarningKind::MalformedXref,
                                format!(
                                    "compressed entry stream obj {field1} exceeds u32, \
                                     clamping to 0"
                                ),
                            );
                            0
                        });
                        let index = u32::try_from(field2).unwrap_or_else(|_| {
                            self.warn(
                                offset,
                                WarningKind::MalformedXref,
                                format!(
                                    "compressed entry index {field2} exceeds u32, clamping to 0"
                                ),
                            );
                            0
                        });
                        XrefEntry::Compressed { stream_obj, index }
                    }
                    _ => {
                        self.warn(
                            offset,
                            WarningKind::MalformedXref,
                            format!(
                                "unknown xref entry type {entry_type} for object {obj_num}, \
                                 skipping"
                            ),
                        );
                        data_pos += row_width;
                        entries_remaining -= 1;
                        continue;
                    }
                };

                entries.push((obj_num, entry));
                data_pos += row_width;
                entries_remaining -= 1;
            }
        }

        // The xref stream dictionary IS the trailer
        Ok((entries, dict))
    }

    /// Repair mode: scan the entire file for `N G obj` patterns and build
    /// a synthetic xref table. Also attempts to locate a trailer dictionary.
    ///
    /// This is a last-resort recovery strategy similar to MuPDF's pdf-repair.c
    /// and Poppler's XRef::constructXRef(). Used when all startxref candidates
    /// fail to produce a valid xref chain.
    fn repair_xref(&self) -> Result<(Vec<(u32, XrefEntry)>, PdfDictionary)> {
        if self.data.len() > MAX_REPAIR_SCAN_SIZE {
            return Err(Error::structure(format!(
                "file too large for repair scan ({} bytes, limit {})",
                self.data.len(),
                MAX_REPAIR_SCAN_SIZE
            )));
        }

        let mut entries: Vec<(u32, XrefEntry)> = Vec::new();
        let mut last_trailer: Option<PdfDictionary> = None;
        let mut catalog_ref: Option<u32> = None;

        // Scan the file using the lexer to find "N G obj" sequences.
        // We look for Integer Integer Obj patterns.
        use super::lexer::Lexer;

        let mut lexer = Lexer::with_diagnostics(self.data, self.diagnostics.clone());
        let mut prev_prev: Option<(i64, u64)> = None; // (value, offset)
        let mut prev: Option<(i64, u64)> = None; // (value, offset)

        loop {
            if entries.len() >= MAX_REPAIR_OBJECTS {
                self.warn(
                    lexer.position(),
                    WarningKind::ResourceLimit,
                    format!("repair scan hit object limit ({MAX_REPAIR_OBJECTS}), stopping"),
                );
                break;
            }

            let token_offset = lexer.position();
            let token = lexer.next_token();

            match token {
                Token::Eof => break,
                Token::Integer(val) => {
                    prev_prev = prev;
                    prev = Some((val, token_offset));
                }
                Token::Obj => {
                    // Check if we have "N G obj" pattern
                    if let (Some((obj_num, obj_offset)), Some((gen, _))) = (prev_prev, prev) {
                        if let (Ok(obj_num), Ok(gen)) = (u32::try_from(obj_num), u16::try_from(gen))
                        {
                            entries.push((
                                obj_num,
                                XrefEntry::Uncompressed {
                                    offset: obj_offset,
                                    gen,
                                },
                            ));

                            // While we're here, try to sniff if this object is
                            // a Catalog (helps us build a trailer if none found)
                            if catalog_ref.is_none() {
                                // Peek ahead for << /Type /Catalog
                                let obj_body_start = lexer.position() as usize;
                                if obj_body_start + 30 < self.data.len() {
                                    let snippet = &self.data[obj_body_start
                                        ..obj_body_start.saturating_add(200).min(self.data.len())];
                                    if find_bytes(snippet, b"/Type").is_some()
                                        && find_bytes(snippet, b"/Catalog").is_some()
                                    {
                                        catalog_ref = Some(obj_num);
                                    }
                                }
                            }
                        }
                    }
                    prev_prev = None;
                    prev = None;
                }
                Token::Trailer => {
                    // Try to parse the trailer dictionary that follows
                    let trailer_pos = lexer.position() as usize;
                    if let Ok(dict) = self.parse_dictionary_at(trailer_pos) {
                        last_trailer = Some(dict);
                    }
                    prev_prev = None;
                    prev = None;
                }
                _ => {
                    prev_prev = None;
                    prev = None;
                }
            }
        }

        if entries.is_empty() {
            return Err(Error::structure("repair scan found no objects in file"));
        }

        // Build or use trailer dictionary
        let trailer = if let Some(t) = last_trailer {
            t
        } else if let Some(cat_num) = catalog_ref {
            // Synthesize a minimal trailer with /Root pointing to the catalog
            let mut dict = PdfDictionary::new();
            let max_obj = entries.iter().map(|(n, _)| *n).max().unwrap_or(0);
            dict.insert(b"Size".to_vec(), PdfObject::Integer((max_obj + 1) as i64));
            dict.insert(
                b"Root".to_vec(),
                PdfObject::Reference(crate::object::ObjRef::new(cat_num, 0)),
            );
            dict
        } else {
            return Err(Error::structure(
                "repair scan found no trailer dictionary and no /Catalog object",
            ));
        };

        Ok((entries, trailer))
    }
}

/// Read a big-endian unsigned integer of 0-4 bytes from a byte slice.
///
/// Width 0 returns 0 (caller applies default). Width 1-4 reads big-endian.
/// Width >4 should be rejected during validation (before calling this).
/// Caller must ensure `data.len() >= width`.
#[inline]
fn read_field_be(data: &[u8], width: usize) -> u64 {
    debug_assert!(
        data.len() >= width,
        "read_field_be: data too short ({} < {width})",
        data.len()
    );
    let mut value: u64 = 0;
    for &byte in &data[..width] {
        value = (value << 8) | u64::from(byte);
    }
    value
}

/// Find a byte sequence in a slice. Returns the offset of the first match.
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Find the last occurrence of a byte sequence in a slice.
#[cfg(test)]
fn rfind_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

/// Skip PDF whitespace characters (space, tab, CR, LF, FF, NUL).
fn skip_whitespace(data: &[u8], start: usize) -> usize {
    let mut pos = start;
    while pos < data.len() && is_pdf_whitespace(data[pos]) {
        pos += 1;
    }
    pos
}

/// Is this byte PDF whitespace? (Table 1 in PDF spec)
fn is_pdf_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0C | 0x00)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_simple_pdf(body: &[u8], xref_and_trailer: &[u8]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");
        data.extend_from_slice(body);
        let xref_offset = data.len();
        data.extend_from_slice(xref_and_trailer);
        data.extend_from_slice(b"\nstartxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");
        data
    }

    fn minimal_pdf() -> Vec<u8> {
        let body = b"1 0 obj\n<< /Type /Catalog >>\nendobj\n";
        let body_offset = 9; // after "%PDF-1.4\n"

        let xref = format!(
            "xref\n\
             0 2\n\
             0000000000 65535 f \r\n\
             {body_offset:010} 00000 n \r\n\
             trailer\n\
             << /Size 2 /Root 1 0 R >>\n"
        );

        make_simple_pdf(body, xref.as_bytes())
    }

    // -- Header tests (F-307) --

    #[test]
    fn test_parse_header_normal() {
        let data = b"%PDF-1.7\nsome content";
        let parser = DocumentParser::new(data);
        let version = parser.parse_header().unwrap();
        assert_eq!(version.major, 1);
        assert_eq!(version.minor, 7);
    }

    #[test]
    fn test_parse_header_version_2() {
        let data = b"%PDF-2.0\nsome content";
        let parser = DocumentParser::new(data);
        let version = parser.parse_header().unwrap();
        assert_eq!(version.major, 2);
        assert_eq!(version.minor, 0);
    }

    #[test]
    fn test_parse_header_with_leading_garbage() {
        // Some PDFs have garbage before the header (e.g., email transport)
        let mut data = Vec::new();
        data.extend_from_slice(b"\r\n\r\n");
        data.extend_from_slice(b"%PDF-1.4\n");
        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let version = parser.parse_header().unwrap();
        assert_eq!(version.major, 1);
        assert_eq!(version.minor, 4);
        // Should have warned about non-zero offset
        assert!(!diag.warnings().is_empty());
    }

    #[test]
    fn test_parse_header_missing() {
        let data = b"not a pdf file at all";
        let parser = DocumentParser::new(data);
        assert!(parser.parse_header().is_err());
    }

    #[test]
    fn test_parse_header_truncated() {
        let data = b"%PDF-";
        let parser = DocumentParser::new(data);
        assert!(parser.parse_header().is_err());
    }

    #[test]
    fn test_parse_header_bad_version() {
        let data = b"%PDF-X.Y\n";
        let parser = DocumentParser::new(data);
        assert!(parser.parse_header().is_err());
    }

    // -- Startxref tests (F-308) --

    #[test]
    fn test_find_startxref_normal() {
        let data = b"%PDF-1.4\nxref\n0 0\ntrailer\n<< >>\nstartxref\n9\n%%EOF\n";
        let parser = DocumentParser::new(data);
        let offset = parser.find_startxref().unwrap();
        assert_eq!(offset, 9);
    }

    #[test]
    fn test_find_startxref_missing() {
        let data = b"%PDF-1.4\nsome stuff\n%%EOF\n";
        let parser = DocumentParser::new(data);
        assert!(parser.find_startxref().is_err());
    }

    #[test]
    fn test_find_startxref_beyond_eof() {
        let data = b"%PDF-1.4\nstartxref\n99999\n%%EOF\n";
        let parser = DocumentParser::new(data);
        assert!(parser.find_startxref().is_err());
    }

    // -- Xref table parsing tests (F-301, F-302) --

    #[test]
    fn test_parse_xref_simple() {
        let data = minimal_pdf();
        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();

        assert_eq!(result.version, PdfVersion { major: 1, minor: 4 });

        // Should have 2 entries (obj 0 = free, obj 1 = in-use)
        assert_eq!(result.xref.len(), 2);

        // Object 0 should be free
        match result.xref.get(0) {
            Some(XrefEntry::Free { gen, .. }) => assert_eq!(*gen, 65535),
            other => panic!("expected free entry for obj 0, got {other:?}"),
        }

        // Object 1 should be uncompressed
        match result.xref.get(1) {
            Some(XrefEntry::Uncompressed { gen, .. }) => assert_eq!(*gen, 0),
            other => panic!("expected uncompressed entry for obj 1, got {other:?}"),
        }

        // Trailer should have /Size and /Root
        assert_eq!(result.trailer.get_i64(b"Size"), Some(2));
        assert!(result.trailer.get(b"Root").is_some());
    }

    #[test]
    fn test_parse_xref_multiple_subsections() {
        // Two subsections: objects 0-1 and objects 5-6
        let body = b"";
        let xref = b"xref\n\
            0 2\n\
            0000000000 65535 f \r\n\
            0000000100 00000 n \r\n\
            5 2\n\
            0000000200 00000 n \r\n\
            0000000300 00001 n \r\n\
            trailer\n\
            << /Size 7 >>\n";

        let data = make_simple_pdf(body, xref);
        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();

        assert_eq!(result.xref.len(), 4);
        assert!(result.xref.get(0).is_some());
        assert!(result.xref.get(1).is_some());
        assert!(result.xref.get(5).is_some());
        assert!(result.xref.get(6).is_some());
        assert!(result.xref.get(2).is_none());

        match result.xref.get(6) {
            Some(XrefEntry::Uncompressed { offset: _, gen }) => assert_eq!(*gen, 1),
            other => panic!("expected uncompressed entry for obj 6, got {other:?}"),
        }
    }

    // -- Incremental update tests (F-303) --

    #[test]
    fn test_parse_incremental_update() {
        // Build a PDF with two xref sections (simulating an incremental update).
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");

        // First object
        let obj1_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        // First xref section
        let xref1_offset = data.len();
        let xref1 = format!(
            "xref\n\
             0 2\n\
             0000000000 65535 f \r\n\
             {obj1_offset:010} 00000 n \r\n\
             trailer\n\
             << /Size 2 /Root 1 0 R >>\n"
        );
        data.extend_from_slice(xref1.as_bytes());

        // Second object (added in incremental update)
        let obj2_offset = data.len();
        data.extend_from_slice(b"2 0 obj\n<< /Type /Page >>\nendobj\n");

        // Second xref section (incremental update), with /Prev pointing to first
        let xref2_offset = data.len();
        let xref2 = format!(
            "xref\n\
             2 1\n\
             {obj2_offset:010} 00000 n \r\n\
             trailer\n\
             << /Size 3 /Root 1 0 R /Prev {xref1_offset} >>\n"
        );
        data.extend_from_slice(xref2.as_bytes());

        // startxref points to second (most recent) xref section
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref2_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();

        // Should have all 3 objects
        assert_eq!(result.xref.len(), 3);
        assert!(result.xref.get(0).is_some());
        assert!(result.xref.get(1).is_some());
        assert!(result.xref.get(2).is_some());

        // Trailer should be from the most recent section (/Size 3)
        assert_eq!(result.trailer.get_i64(b"Size"), Some(3));
    }

    #[test]
    fn test_circular_prev_detected() {
        // Build a PDF where /Prev points back to itself
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");
        let xref_offset = data.len();

        let xref = format!(
            "xref\n\
             0 1\n\
             0000000000 65535 f \r\n\
             trailer\n\
             << /Size 1 /Prev {xref_offset} >>\n"
        );
        data.extend_from_slice(xref.as_bytes());
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        // Should succeed (circular ref detected and warned, not fatal)
        let result = parser.parse().unwrap();
        assert_eq!(result.xref.len(), 1);

        let warnings = diag.warnings();
        assert!(warnings.iter().any(|w| w.message.contains("circular")));
    }

    // -- Xref entry parsing edge cases --

    #[test]
    fn test_xref_entry_with_lf_only() {
        // Some generators use \n instead of \r\n
        let body = b"";
        let xref = b"xref\n\
            0 2\n\
            0000000000 65535 f \n\
            0000000100 00000 n \n\
            trailer\n\
            << /Size 2 >>\n";

        let data = make_simple_pdf(body, xref);
        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();
        assert_eq!(result.xref.len(), 2);
    }

    // -- Full document parse --

    #[test]
    fn test_parse_complete_document() {
        let data = minimal_pdf();
        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();

        assert_eq!(result.version, PdfVersion { major: 1, minor: 4 });
        assert_eq!(result.xref.len(), 2);
        assert!(result.trailer.get(b"Root").is_some());
    }

    #[test]
    fn test_version_display() {
        let v = PdfVersion { major: 1, minor: 7 };
        assert_eq!(format!("{v}"), "1.7");
    }

    #[test]
    fn test_xref_table_insert_if_absent() {
        let mut xref = XrefTable::new();
        let entry1 = XrefEntry::Uncompressed {
            offset: 100,
            gen: 0,
        };
        let entry2 = XrefEntry::Uncompressed {
            offset: 200,
            gen: 0,
        };

        xref.insert_if_absent(5, entry1);
        xref.insert_if_absent(5, entry2); // Should NOT overwrite

        match xref.get(5) {
            Some(XrefEntry::Uncompressed { offset, .. }) => assert_eq!(*offset, 100),
            other => panic!("expected uncompressed at offset 100, got {other:?}"),
        }
    }

    #[test]
    fn test_empty_xref_table() {
        let xref = XrefTable::new();
        assert!(xref.is_empty());
        assert_eq!(xref.len(), 0);
        assert!(xref.get(0).is_none());
    }

    // -- Xref error recovery tests --

    #[test]
    fn test_parse_xref_malformed_entry_recovery() {
        // Build PDF with corrupt xref entry in middle of subsection.
        // Parser should warn and skip the bad entry, but parse the valid ones.
        let body = b"2 0 obj\n<< /Type /Page >>\nendobj\n";
        let body_offset = 9; // after "%PDF-1.4\n"

        // Use \n line endings (20 bytes total per entry including line ending)
        // Corrupt entry has non-digit characters 'XXXX' in offset field
        let xref = format!(
            "xref\n\
             0 3\n\
             0000000000 65535 f \n\
             000000XXXX 00000 n \n\
             {body_offset:010} 00000 n \n\
             trailer\n\
             << /Size 3 >>\n"
        );

        let data = make_simple_pdf(body, xref.as_bytes());
        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();

        // Valid entries (0 and 2) should still be present
        assert!(result.xref.get(0).is_some());
        assert!(result.xref.get(2).is_some());

        // Should have warned about malformed entry for object 1
        let warnings = diag.warnings();
        assert!(warnings
            .iter()
            .any(|w| w.message.contains("malformed") && w.message.contains("object 1")));
    }

    // -- Xref stream tests (F-304) --

    /// Build a minimal PDF with an xref stream (no FlateDecode, raw binary entries).
    fn make_xref_stream_pdf(objects: &[(u32, &[u8])]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        let mut xref_entries: Vec<(u32, XrefEntry)> = Vec::new();

        // Object 0 is always free
        xref_entries.push((
            0,
            XrefEntry::Free {
                next_free: 0,
                gen: 65535,
            },
        ));

        // Write objects
        for &(num, body) in objects {
            let offset = data.len();
            data.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            data.extend_from_slice(body);
            data.extend_from_slice(b"\nendobj\n");
            xref_entries.push((
                num,
                XrefEntry::Uncompressed {
                    offset: offset as u64,
                    gen: 0,
                },
            ));
        }

        // Sort entries by object number for the xref stream
        xref_entries.sort_by_key(|(num, _)| *num);

        let xref_obj_num = xref_entries.iter().map(|(n, _)| n).max().unwrap_or(&0) + 1;
        let size = xref_obj_num + 1;

        // Build the binary xref stream data using /W [1 3 2]
        // (1 byte type, 3 bytes offset/stream_obj, 2 bytes gen/index)
        let mut stream_data = Vec::new();
        for (_, entry) in &xref_entries {
            match entry {
                XrefEntry::Free { next_free, gen } => {
                    stream_data.push(0); // type 0
                    let nf_bytes = next_free.to_be_bytes();
                    stream_data.extend_from_slice(&nf_bytes[1..]); // 3 bytes
                    stream_data.extend_from_slice(&gen.to_be_bytes());
                }
                XrefEntry::Uncompressed { offset, gen } => {
                    stream_data.push(1); // type 1
                    let off_bytes = (*offset as u32).to_be_bytes();
                    stream_data.extend_from_slice(&off_bytes[1..]); // 3 bytes
                    stream_data.extend_from_slice(&gen.to_be_bytes());
                }
                XrefEntry::Compressed { stream_obj, index } => {
                    stream_data.push(2); // type 2
                    let so_bytes = stream_obj.to_be_bytes();
                    stream_data.extend_from_slice(&so_bytes[1..]); // 3 bytes
                    stream_data.extend_from_slice(&(*index as u16).to_be_bytes());
                }
            }
        }
        // Add entry for the xref stream object itself
        let xref_stream_offset = data.len();
        stream_data.push(1); // type 1
        let xso_bytes = (xref_stream_offset as u32).to_be_bytes();
        stream_data.extend_from_slice(&xso_bytes[1..]);
        stream_data.extend_from_slice(&0u16.to_be_bytes()); // gen 0

        // Build /Index array
        let total_objects = xref_entries.len() + 1; // +1 for xref stream obj itself
        let index_str = format!("0 {total_objects}");

        // Build xref stream object
        let dict_str = format!(
            "<< /Type /XRef /Size {size} /W [1 3 2] /Index [{index_str}] /Length {} /Root 1 0 R >>",
            stream_data.len()
        );

        data.extend_from_slice(format!("{xref_obj_num} 0 obj\n").as_bytes());
        data.extend_from_slice(dict_str.as_bytes());
        data.extend_from_slice(b"\nstream\n");
        data.extend_from_slice(&stream_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_stream_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        data
    }

    #[test]
    fn test_xref_stream_basic() {
        let data = make_xref_stream_pdf(&[(1, b"<< /Type /Catalog >>")]);
        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();

        assert_eq!(result.version, PdfVersion { major: 1, minor: 5 });

        // Should have 3 entries: obj 0 (free), obj 1 (catalog), obj 2 (xref stream)
        assert_eq!(result.xref.len(), 3);

        match result.xref.get(0) {
            Some(XrefEntry::Free { gen, .. }) => assert_eq!(*gen, 65535),
            other => panic!("expected free entry for obj 0, got {other:?}"),
        }

        match result.xref.get(1) {
            Some(XrefEntry::Uncompressed { gen, .. }) => assert_eq!(*gen, 0),
            other => panic!("expected uncompressed entry for obj 1, got {other:?}"),
        }

        // Trailer (from xref stream dict) should have /Root
        assert!(result.trailer.get(b"Root").is_some());
        assert!(result.trailer.get_i64(b"Size").is_some());
    }

    #[test]
    fn test_xref_stream_missing_w_fails() {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let xref_offset = data.len();
        data.extend_from_slice(
            b"5 0 obj\n<< /Type /XRef /Size 1 /Length 0 >>\nstream\n\nendstream\nendobj\n",
        );
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let err = parser.parse().unwrap_err();
        assert!(err.to_string().contains("/W"));
    }

    #[test]
    fn test_xref_stream_missing_type_warns() {
        // Xref stream without /Type should warn but still parse
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        let obj_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_offset = data.len();
        // /W [1 2 1], 2 entries: free obj 0 + uncompressed obj 1
        // Entry 0: type=0, next=0x0000, gen=0xFF
        // Entry 1: type=1, offset, gen=0x00
        let obj_off_hi = (obj_offset >> 8) as u8;
        let obj_off_lo = (obj_offset & 0xFF) as u8;
        let stream_data: Vec<u8> = vec![
            0, 0x00, 0x00, 0xFF, // obj 0: free
            1, obj_off_hi, obj_off_lo, 0x00, // obj 1: uncompressed
        ];
        let dict_str = format!(
            "<< /Size 2 /W [1 2 1] /Length {} /Root 1 0 R >>",
            stream_data.len()
        );
        data.extend_from_slice(format!("2 0 obj\n{dict_str}\nstream\n").as_bytes());
        data.extend_from_slice(&stream_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();
        assert!(result.xref.get(0).is_some());
        assert!(result.xref.get(1).is_some());

        let warnings = diag.warnings();
        assert!(warnings.iter().any(|w| w.message.contains("missing /Type")));
    }

    #[test]
    fn test_xref_stream_compressed_entries() {
        // Build a PDF with compressed xref entries (type 2)
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        let obj_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_offset = data.len();
        // /W [1 2 2]: type=1, field1=2, field2=2
        // 4 entries: free, uncompressed catalog, compressed obj 3, xref stream obj
        let obj_off = obj_offset as u16;
        let stream_data: Vec<u8> = vec![
            0,
            0x00,
            0x00,
            0xFF,
            0xFF, // obj 0: free
            1,
            (obj_off >> 8) as u8,
            obj_off as u8,
            0x00,
            0x00, // obj 1: uncompressed
            2,
            0x00,
            0x0A,
            0x00,
            0x03, // obj 2: compressed in stream 10, index 3
        ];
        let dict_str = format!(
            "<< /Type /XRef /Size 4 /W [1 2 2] /Index [0 3] /Length {} /Root 1 0 R >>",
            stream_data.len()
        );
        data.extend_from_slice(format!("3 0 obj\n{dict_str}\nstream\n").as_bytes());
        data.extend_from_slice(&stream_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();

        match result.xref.get(2) {
            Some(XrefEntry::Compressed { stream_obj, index }) => {
                assert_eq!(*stream_obj, 10);
                assert_eq!(*index, 3);
            }
            other => panic!("expected compressed entry for obj 2, got {other:?}"),
        }
    }

    #[test]
    fn test_xref_stream_w0_zero_defaults_to_type1() {
        // When w[0] is 0, the type field defaults to 1 (uncompressed)
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        let obj_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_offset = data.len();
        // /W [0 2 1]: no type field, default to type 1
        let obj_off = obj_offset as u16;
        let stream_data: Vec<u8> = vec![
            0x00,
            0x00,
            0xFF, // obj 0: offset=0, gen=255 (treated as type 1)
            (obj_off >> 8) as u8,
            obj_off as u8,
            0x00, // obj 1: offset, gen=0
        ];
        let dict_str = format!(
            "<< /Type /XRef /Size 2 /W [0 2 1] /Length {} /Root 1 0 R >>",
            stream_data.len()
        );
        data.extend_from_slice(format!("2 0 obj\n{dict_str}\nstream\n").as_bytes());
        data.extend_from_slice(&stream_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();

        // All entries should be uncompressed (type 1 default)
        match result.xref.get(0) {
            Some(XrefEntry::Uncompressed { .. }) => {}
            other => panic!("expected uncompressed entry for obj 0, got {other:?}"),
        }
        match result.xref.get(1) {
            Some(XrefEntry::Uncompressed { gen, .. }) => assert_eq!(*gen, 0),
            other => panic!("expected uncompressed entry for obj 1, got {other:?}"),
        }
    }

    #[test]
    fn test_xref_stream_unknown_type_skipped() {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        let obj_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_offset = data.len();
        let obj_off = obj_offset as u16;
        // Entry with unknown type 5
        let stream_data: Vec<u8> = vec![
            0,
            0x00,
            0x00,
            0xFF, // obj 0: free
            1,
            (obj_off >> 8) as u8,
            obj_off as u8,
            0x00, // obj 1: uncompressed
            5,
            0x00,
            0x00,
            0x00, // obj 2: unknown type 5
        ];
        let dict_str = format!(
            "<< /Type /XRef /Size 4 /W [1 2 1] /Index [0 3] /Length {} /Root 1 0 R >>",
            stream_data.len()
        );
        data.extend_from_slice(format!("3 0 obj\n{dict_str}\nstream\n").as_bytes());
        data.extend_from_slice(&stream_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();

        // Obj 0 and 1 should be present, obj 2 (unknown type) should be skipped
        assert!(result.xref.get(0).is_some());
        assert!(result.xref.get(1).is_some());
        assert!(result.xref.get(2).is_none());

        let warnings = diag.warnings();
        assert!(warnings
            .iter()
            .any(|w| w.message.contains("unknown xref entry type")));
    }

    #[test]
    fn test_xref_stream_data_starting_with_0a() {
        // Regression: the parser must not skip a leading 0x0A (LF) byte in
        // the decoded xref stream data. An earlier version double-skipped
        // the post-"stream" EOL, which would corrupt entries when the first
        // binary byte happened to be CR or LF.
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        // Object at offset 0x0A01 would need a large file; instead use
        // /W [0 2 1] (no type field, default type 1) so field1 is the
        // first byte of each row. Set obj 0's offset high byte to 0x0A.
        let obj_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_offset = data.len();
        let obj_off = obj_offset as u16;
        // /W [0 2 1]: 3 bytes per entry, no type field (defaults to type 1)
        // First entry: obj 0 at offset 0x0A00, gen 0 -- first byte is 0x0A
        let stream_data: Vec<u8> = vec![
            0x0A,
            0x00,
            0x00, // obj 0: offset=0x0A00, gen=0
            (obj_off >> 8) as u8,
            obj_off as u8,
            0x00, // obj 1: offset=obj_offset, gen=0
        ];
        let dict_str = format!(
            "<< /Type /XRef /Size 2 /W [0 2 1] /Length {} /Root 1 0 R >>",
            stream_data.len()
        );
        data.extend_from_slice(format!("2 0 obj\n{dict_str}\nstream\n").as_bytes());
        data.extend_from_slice(&stream_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();

        // Obj 0 should have offset 0x0A00 = 2560, NOT a corrupted value
        // from skipping the leading 0x0A byte.
        match result.xref.get(0) {
            Some(XrefEntry::Uncompressed { offset, gen }) => {
                assert_eq!(*offset, 0x0A00, "obj 0 offset corrupted by EOL skip");
                assert_eq!(*gen, 0);
            }
            other => panic!("expected uncompressed entry for obj 0, got {other:?}"),
        }

        match result.xref.get(1) {
            Some(XrefEntry::Uncompressed { offset, gen }) => {
                assert_eq!(*offset, obj_offset as u64);
                assert_eq!(*gen, 0);
            }
            other => panic!("expected uncompressed entry for obj 1, got {other:?}"),
        }
    }

    // -- read_field_be tests --

    #[test]
    fn test_read_field_be_width_0() {
        assert_eq!(read_field_be(&[0xFF, 0xFF], 0), 0);
    }

    #[test]
    fn test_read_field_be_width_1() {
        assert_eq!(read_field_be(&[0x42], 1), 0x42);
    }

    #[test]
    fn test_read_field_be_width_2() {
        assert_eq!(read_field_be(&[0x01, 0x00], 2), 256);
    }

    #[test]
    fn test_read_field_be_width_3() {
        assert_eq!(read_field_be(&[0x01, 0x00, 0x00], 3), 65536);
    }

    #[test]
    fn test_read_field_be_width_4() {
        assert_eq!(read_field_be(&[0xFF, 0xFF, 0xFF, 0xFF], 4), 0xFFFFFFFF);
    }

    #[test]
    fn test_read_field_be_max_values() {
        assert_eq!(read_field_be(&[0xFF], 1), 255);
        assert_eq!(read_field_be(&[0xFF, 0xFF], 2), 65535);
    }

    // -- Utility function tests --

    #[test]
    fn test_find_bytes() {
        assert_eq!(find_bytes(b"hello world", b"world"), Some(6));
        assert_eq!(find_bytes(b"hello world", b"hello"), Some(0));
        assert_eq!(find_bytes(b"hello world", b"xyz"), None);
        assert_eq!(find_bytes(b"hello", b""), None);
        assert_eq!(find_bytes(b"", b"hello"), None);
    }

    #[test]
    fn test_rfind_bytes() {
        assert_eq!(rfind_bytes(b"abcabc", b"abc"), Some(3));
        assert_eq!(rfind_bytes(b"abc", b"abc"), Some(0));
        assert_eq!(rfind_bytes(b"abc", b"xyz"), None);
    }

    #[test]
    fn test_is_pdf_whitespace() {
        assert!(is_pdf_whitespace(b' '));
        assert!(is_pdf_whitespace(b'\t'));
        assert!(is_pdf_whitespace(b'\r'));
        assert!(is_pdf_whitespace(b'\n'));
        assert!(is_pdf_whitespace(0x0C)); // form feed
        assert!(is_pdf_whitespace(0x00)); // null
        assert!(!is_pdf_whitespace(b'a'));
        assert!(!is_pdf_whitespace(b'0'));
    }

    use crate::CollectingDiagnostics;

    // -- Malformed header tests --

    #[test]
    fn test_parse_header_truncated_after_dash() {
        // "%PDF-1" with no dot or minor digit
        let data = b"%PDF-1";
        let parser = DocumentParser::new(data);
        assert!(parser.parse_header().is_err());
    }

    #[test]
    fn test_parse_header_truncated_after_dot() {
        // "%PDF-1." with no minor digit
        let data = b"%PDF-1.";
        let parser = DocumentParser::new(data);
        assert!(parser.parse_header().is_err());
    }

    #[test]
    fn test_parse_header_invalid_major_digit() {
        let data = b"%PDF-X.4\ncontent";
        let parser = DocumentParser::new(data);
        let err = parser.parse_header().unwrap_err();
        assert!(err.to_string().contains("version"));
    }

    #[test]
    fn test_parse_header_invalid_minor_digit() {
        let data = b"%PDF-1.Z\ncontent";
        let parser = DocumentParser::new(data);
        let err = parser.parse_header().unwrap_err();
        assert!(err.to_string().contains("version"));
    }

    #[test]
    fn test_parse_header_missing_dot() {
        let data = b"%PDF-17\ncontent";
        let parser = DocumentParser::new(data);
        assert!(parser.parse_header().is_err());
    }

    #[test]
    fn test_parse_header_only_marker_at_end_of_buffer() {
        // %PDF- at end, no room for version digits
        let data = b"%PDF-";
        let parser = DocumentParser::new(data);
        let err = parser.parse_header().unwrap_err();
        assert!(err.to_string().contains("truncated"));
    }

    #[test]
    fn test_parse_header_empty_input() {
        let data = b"";
        let parser = DocumentParser::new(data);
        assert!(parser.parse_header().is_err());
    }

    // -- Startxref edge cases --

    #[test]
    fn test_find_startxref_non_numeric_value() {
        let data = b"%PDF-1.4\nstartxref\nabc\n%%EOF\n";
        let parser = DocumentParser::new(data);
        let err = parser.find_startxref().unwrap_err();
        // Should fail parsing "abc" as integer
        assert!(err.to_string().contains("digit"));
    }

    #[test]
    fn test_find_startxref_offset_exactly_at_file_length() {
        // startxref pointing exactly to data.len() (one past the end)
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\nstartxref\n");
        // The data will be this long, so offset == data.len() is past EOF
        let total_len = data.len() + b"9999\n%%EOF\n".len();
        let offset_val = total_len.to_string();
        data.extend_from_slice(offset_val.as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let err = parser.find_startxref().unwrap_err();
        assert!(err.to_string().contains("beyond end of file"));
    }

    #[test]
    fn test_find_startxref_whitespace_only_after_keyword() {
        // "startxref" followed only by whitespace, no digits
        let data = b"%PDF-1.4\nstartxref\n   \n%%EOF\n";
        let parser = DocumentParser::new(data);
        assert!(parser.find_startxref().is_err());
    }

    #[test]
    fn test_find_startxref_empty_file() {
        let data = b"";
        let parser = DocumentParser::new(data);
        assert!(parser.find_startxref().is_err());
    }

    #[test]
    fn test_find_startxref_uses_last_occurrence() {
        // Two "startxref" keywords; parser should use the last one
        let body = b"1 0 obj\n<< /Type /Catalog >>\nendobj\n";
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");
        data.extend_from_slice(body);
        let xref_offset = data.len();
        let xref = "xref\n\
             0 2\n\
             0000000000 65535 f \r\n\
             0000000009 00000 n \r\n\
             trailer\n\
             << /Size 2 /Root 1 0 R >>\n";
        data.extend_from_slice(xref.as_bytes());
        // First (fake) startxref pointing to 0 (bogus)
        data.extend_from_slice(b"startxref\n0\n%%EOF\n");
        // Real startxref
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let offset = parser.find_startxref().unwrap();
        assert_eq!(offset, xref_offset as u64);
    }

    // -- Xref chain circular reference detection --

    #[test]
    fn test_circular_prev_two_section_loop() {
        // Two xref sections that point to each other via /Prev
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");

        let xref1_offset = data.len();
        // We'll patch /Prev after we know xref2_offset
        let xref1_template = "xref\n\
             0 1\n\
             0000000000 65535 f \r\n\
             trailer\n\
             << /Size 1 /Prev PLACEHOLDER >>\n";
        let xref1_placeholder_start = data.len();
        data.extend_from_slice(xref1_template.as_bytes());

        let xref2_offset = data.len();
        let xref2 = format!(
            "xref\n\
             0 1\n\
             0000000000 65535 f \r\n\
             trailer\n\
             << /Size 1 /Prev {xref1_offset} >>\n"
        );
        data.extend_from_slice(xref2.as_bytes());

        // Patch xref1's /Prev to point to xref2
        let replacement = format!("{:<11}", xref2_offset);
        let xref1_bytes = xref1_template.replace("PLACEHOLDER", &replacement);
        data[xref1_placeholder_start..xref1_placeholder_start + xref1_bytes.len()]
            .copy_from_slice(xref1_bytes.as_bytes());

        // startxref -> xref2
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref2_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();
        assert_eq!(result.xref.len(), 1);

        let warnings = diag.warnings();
        assert!(warnings.iter().any(|w| w.message.contains("circular")));
    }

    // -- Xref chain depth limit --

    #[test]
    fn test_xref_chain_depth_limit() {
        // Build a chain of MAX_XREF_CHAIN_DEPTH + 1 xref sections.
        // Since that's 65 sections, this tests that the parser stops at 64.
        let chain_len = MAX_XREF_CHAIN_DEPTH + 1;
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");

        let mut offsets = Vec::new();
        // Write all xref sections first with placeholder /Prev values
        for i in 0..chain_len {
            offsets.push(data.len());
            let obj_num_start = i as u32;
            let xref = format!(
                "xref\n\
                 {obj_num_start} 1\n\
                 0000000000 65535 f \r\n\
                 trailer\n\
                 << /Size {} /Prev 000000000 >>\n",
                obj_num_start + 1
            );
            data.extend_from_slice(xref.as_bytes());
        }

        // Now rewrite each section with correct /Prev. Section i's /Prev -> section i+1.
        // The last section has no valid /Prev (it points nowhere useful, but the
        // depth limit should kick in before we get there).
        for i in 0..chain_len {
            let prev_offset = if i + 1 < chain_len {
                offsets[i + 1]
            } else {
                0 // doesn't matter, won't be reached
            };
            // Find "/Prev " in this section and write the offset
            let section_start = offsets[i];
            let section_bytes = if i + 1 < chain_len {
                &data[section_start..offsets[i + 1]]
            } else {
                &data[section_start..]
            };
            if let Some(prev_pos) = find_bytes(section_bytes, b"/Prev ") {
                let write_start = section_start + prev_pos + 6;
                let prev_str = format!("{:09}", prev_offset);
                data[write_start..write_start + 9].copy_from_slice(prev_str.as_bytes());
            }
        }

        // startxref -> first (most recent) section
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(offsets[0].to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let _result = parser.parse().unwrap();

        // Should have stopped at the depth limit
        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.message.contains("depth limit")),
            "expected depth limit warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    // -- Xref entries with non-standard spacing --

    #[test]
    fn test_xref_entry_with_cr_only() {
        // Some generators use \r instead of \r\n
        let body = b"";
        let mut xref = Vec::new();
        xref.extend_from_slice(b"xref\n0 2\n");
        xref.extend_from_slice(b"0000000000 65535 f \r");
        xref.extend_from_slice(b"0000000100 00000 n \r");
        xref.extend_from_slice(b"trailer\n<< /Size 2 >>\n");

        let data = make_simple_pdf(body, &xref);
        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();
        assert_eq!(result.xref.len(), 2);
    }

    #[test]
    fn test_xref_entry_without_trailing_space() {
        // Entry with type marker immediately followed by line ending (no space)
        // "0000000000 65535 f\r\n" (18 bytes + \r\n = 20)
        let body = b"";
        let mut xref = Vec::new();
        xref.extend_from_slice(b"xref\n0 2\n");
        xref.extend_from_slice(b"0000000000 65535 f\r\n");
        xref.extend_from_slice(b"0000000100 00000 n\r\n");
        xref.extend_from_slice(b"trailer\n<< /Size 2 >>\n");

        let data = make_simple_pdf(body, &xref);
        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();
        assert_eq!(result.xref.len(), 2);
    }

    #[test]
    fn test_xref_entry_non_space_separator_warns() {
        // Tab instead of space between offset and gen. The parser warns but
        // the entry still has valid digits so should parse via trim().
        // Actually the parser checks data[pos+10] == b' ' and warns if not,
        // but still continues parsing. Let's verify the warning fires.
        let body = b"";
        // Build entry with tab at position 10 instead of space
        let mut xref = Vec::new();
        xref.extend_from_slice(b"xref\n0 1\n");
        // "0000000000\t65535 f \r\n" -- tab at offset 10
        xref.extend_from_slice(b"0000000000\t65535 f \r\n");
        xref.extend_from_slice(b"trailer\n<< /Size 1 >>\n");

        let data = make_simple_pdf(body, &xref);
        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        // The gen field won't parse correctly because the tab shifts bytes,
        // but the parser should at least warn about the non-space separator.
        // Whether the overall parse succeeds depends on exact byte alignment.
        let _ = parser.parse();
        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("expected space")),
            "expected a 'space' warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    // -- Missing xref keyword / malformed xref tables --

    #[test]
    fn test_missing_xref_keyword_falls_through_to_xref_stream() {
        // If startxref points to something that's not "xref", the parser
        // tries to parse it as an xref stream. With garbage, that should fail.
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");
        let offset = data.len();
        data.extend_from_slice(b"garbage data here\n");
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        assert!(parser.parse().is_err());
    }

    #[test]
    fn test_xref_table_missing_trailer_keyword() {
        // xref table with entries but no "trailer" keyword before EOF
        let body = b"";
        let xref = b"xref\n0 1\n0000000000 65535 f \r\nNOT_TRAILER\n<< /Size 1 >>\n";

        let data = make_simple_pdf(body, xref);
        let parser = DocumentParser::new(&data);
        // The parser loops looking for "trailer"; "NOT_TRAILER" doesn't match,
        // so it tries to parse it as a subsection header, which should fail.
        assert!(parser.parse().is_err());
    }

    #[test]
    fn test_xref_table_truncated_before_trailer() {
        // xref table that ends abruptly
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");
        let xref_offset = data.len();
        data.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \r\n");
        // No trailer, just startxref
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        // "startxref" will be encountered where "trailer" is expected.
        // It doesn't start with "trailer", so the parser tries to parse it
        // as a subsection header, which will fail.
        assert!(parser.parse().is_err());
    }

    // -- Trailer parsing edge cases --

    #[test]
    fn test_trailer_not_a_dictionary() {
        // trailer followed by a non-dictionary object
        let body = b"";
        let xref = b"xref\n0 1\n0000000000 65535 f \r\ntrailer\n42\n";

        let data = make_simple_pdf(body, xref);
        let parser = DocumentParser::new(&data);
        let err = parser.parse().unwrap_err();
        assert!(err.to_string().contains("dictionary"));
    }

    #[test]
    fn test_trailer_empty_dictionary() {
        // Trailer with empty dict (no /Size, no /Root)
        // Should parse OK structurally; the document_parser doesn't validate
        // trailer contents beyond what load_xref_chain needs.
        let body = b"";
        let xref = b"xref\n0 1\n0000000000 65535 f \r\ntrailer\n<< >>\n";

        let data = make_simple_pdf(body, xref);
        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();
        assert!(result.trailer.get(b"Size").is_none());
    }

    // -- Incremental update chains with /Prev --

    #[test]
    fn test_incremental_update_newer_entry_wins() {
        // Two xref sections both define object 1. The newer (first-parsed)
        // section's entry should win due to insert_if_absent.
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");

        // Object 1 at some offset
        let obj1_v1_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Version 1 >>\nendobj\n");

        // First (older) xref section
        let xref1_offset = data.len();
        let xref1 = format!(
            "xref\n\
             0 2\n\
             0000000000 65535 f \r\n\
             {obj1_v1_offset:010} 00000 n \r\n\
             trailer\n\
             << /Size 2 /Root 1 0 R >>\n"
        );
        data.extend_from_slice(xref1.as_bytes());

        // Updated object 1 at a new offset
        let obj1_v2_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Version 2 >>\nendobj\n");

        // Second (newer) xref section with /Prev pointing to first
        let xref2_offset = data.len();
        let xref2 = format!(
            "xref\n\
             1 1\n\
             {obj1_v2_offset:010} 00000 n \r\n\
             trailer\n\
             << /Size 2 /Root 1 0 R /Prev {xref1_offset} >>\n"
        );
        data.extend_from_slice(xref2.as_bytes());

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref2_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();

        // Object 1 should point to the v2 offset (newer section wins)
        match result.xref.get(1) {
            Some(XrefEntry::Uncompressed { offset, .. }) => {
                assert_eq!(
                    *offset, obj1_v2_offset as u64,
                    "newer xref entry should take priority"
                );
            }
            other => panic!("expected uncompressed entry for obj 1, got {other:?}"),
        }
    }

    #[test]
    fn test_incremental_update_negative_prev_warns() {
        // /Prev with a negative value should warn and stop the chain
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");

        let xref_offset = data.len();
        let xref = "xref\n\
             0 1\n\
             0000000000 65535 f \r\n\
             trailer\n\
             << /Size 1 /Prev -1 >>\n";
        data.extend_from_slice(xref.as_bytes());

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();
        assert_eq!(result.xref.len(), 1);

        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.message.contains("negative")),
            "expected negative /Prev warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_incremental_update_prev_not_integer_warns() {
        // /Prev with a non-integer value (e.g., a name)
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");

        let xref_offset = data.len();
        let xref = "xref\n\
             0 1\n\
             0000000000 65535 f \r\n\
             trailer\n\
             << /Size 1 /Prev /NotAnInteger >>\n";
        data.extend_from_slice(xref.as_bytes());

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();
        assert_eq!(result.xref.len(), 1);

        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("not an integer")),
            "expected non-integer /Prev warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_incremental_update_later_section_parse_failure() {
        // First xref section parses fine, but /Prev points to garbage.
        // Parser should warn and return what it has from the first section.
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");

        // Garbage at offset 9 (after header)
        let garbage_offset = data.len();
        data.extend_from_slice(b"THIS IS NOT AN XREF TABLE\n");

        let xref_offset = data.len();
        let xref = format!(
            "xref\n\
             0 1\n\
             0000000000 65535 f \r\n\
             trailer\n\
             << /Size 1 /Prev {garbage_offset} >>\n"
        );
        data.extend_from_slice(xref.as_bytes());

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();
        // Should still have the entry from the first section
        assert_eq!(result.xref.len(), 1);

        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("failed to parse")),
            "expected parse failure warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    // -- Xref stream: /Type validation --

    #[test]
    fn test_xref_stream_wrong_type_value() {
        // /Type is /Foo instead of /XRef -- should error
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let xref_offset = data.len();
        let stream_data: Vec<u8> = vec![0, 0x00, 0x00, 0xFF, 0xFF];
        let dict_str = format!(
            "<< /Type /Foo /Size 1 /W [1 2 2] /Length {} >>",
            stream_data.len()
        );
        data.extend_from_slice(format!("1 0 obj\n{dict_str}\nstream\n").as_bytes());
        data.extend_from_slice(&stream_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let err = parser.parse().unwrap_err();
        assert!(err.to_string().contains("/Foo"));
    }

    // -- Xref stream: /W validation --

    #[test]
    fn test_xref_stream_w_wrong_element_count() {
        // /W with 2 elements instead of 3
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let xref_offset = data.len();
        data.extend_from_slice(
            b"1 0 obj\n<< /Type /XRef /Size 1 /W [1 2] /Length 0 >>\nstream\n\nendstream\nendobj\n",
        );
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let err = parser.parse().unwrap_err();
        assert!(err.to_string().contains("/W"));
    }

    #[test]
    fn test_xref_stream_w_value_too_large() {
        // /W element > 4 should error
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let xref_offset = data.len();
        data.extend_from_slice(
            b"1 0 obj\n<< /Type /XRef /Size 1 /W [1 5 2] /Length 0 >>\nstream\n\nendstream\nendobj\n",
        );
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let err = parser.parse().unwrap_err();
        assert!(err.to_string().contains("/W"));
    }

    #[test]
    fn test_xref_stream_w_negative_value() {
        // /W element < 0
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let xref_offset = data.len();
        data.extend_from_slice(
            b"1 0 obj\n<< /Type /XRef /Size 1 /W [1 -1 2] /Length 0 >>\nstream\n\nendstream\nendobj\n",
        );
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let err = parser.parse().unwrap_err();
        assert!(err.to_string().contains("/W"));
    }

    #[test]
    fn test_xref_stream_w_all_zero() {
        // /W [0 0 0] means total width is 0, which should error
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let xref_offset = data.len();
        data.extend_from_slice(
            b"1 0 obj\n<< /Type /XRef /Size 1 /W [0 0 0] /Length 0 >>\nstream\n\nendstream\nendobj\n",
        );
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let err = parser.parse().unwrap_err();
        assert!(err.to_string().contains("width is 0"));
    }

    // -- Xref stream: /Size validation --

    #[test]
    fn test_xref_stream_missing_size() {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let xref_offset = data.len();
        data.extend_from_slice(
            b"1 0 obj\n<< /Type /XRef /W [1 2 1] /Length 0 >>\nstream\n\nendstream\nendobj\n",
        );
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let err = parser.parse().unwrap_err();
        assert!(err.to_string().contains("/Size"));
    }

    #[test]
    fn test_xref_stream_negative_size() {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let xref_offset = data.len();
        data.extend_from_slice(
            b"1 0 obj\n<< /Type /XRef /Size -5 /W [1 2 1] /Length 0 >>\nstream\n\nendstream\nendobj\n",
        );
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let err = parser.parse().unwrap_err();
        assert!(err.to_string().contains("negative"));
    }

    #[test]
    fn test_xref_stream_size_exceeds_max() {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let xref_offset = data.len();
        let huge_size = MAX_XREF_STREAM_SIZE + 1;
        let dict = format!("<< /Type /XRef /Size {huge_size} /W [1 2 1] /Length 0 >>");
        data.extend_from_slice(
            format!("1 0 obj\n{dict}\nstream\n\nendstream\nendobj\n").as_bytes(),
        );
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let err = parser.parse().unwrap_err();
        assert!(err.to_string().contains("exceeds maximum allowed"));
    }

    // -- Xref stream: /Index validation --

    #[test]
    fn test_xref_stream_index_odd_length() {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let xref_offset = data.len();
        data.extend_from_slice(
            b"1 0 obj\n<< /Type /XRef /Size 1 /W [1 2 1] /Index [0 1 2] /Length 4 >>\nstream\n\x01\x00\x00\x00\nendstream\nendobj\n",
        );
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let err = parser.parse().unwrap_err();
        assert!(err.to_string().contains("odd length"));
    }

    #[test]
    fn test_xref_stream_index_negative_values() {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let xref_offset = data.len();
        data.extend_from_slice(
            b"1 0 obj\n<< /Type /XRef /Size 1 /W [1 2 1] /Index [-1 1] /Length 4 >>\nstream\n\x01\x00\x00\x00\nendstream\nendobj\n",
        );
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let err = parser.parse().unwrap_err();
        assert!(err.to_string().contains("negative"));
    }

    // -- Xref stream field overflow --

    #[test]
    fn test_xref_stream_gen_overflow_clamps() {
        // gen field value > u16::MAX should warn and clamp
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        let obj_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_offset = data.len();
        // /W [1 3 4]: type=1, offset=3bytes, gen=4bytes
        // For obj 0: type 1, offset=obj_offset, gen=0x00010000 (exceeds u16)
        let obj_off_bytes = (obj_offset as u32).to_be_bytes();
        let stream_data: Vec<u8> = vec![
            1,
            obj_off_bytes[1],
            obj_off_bytes[2],
            obj_off_bytes[3],
            0x00,
            0x01,
            0x00,
            0x00, // gen = 65536, exceeds u16
        ];
        let dict_str = format!(
            "<< /Type /XRef /Size 2 /W [1 3 4] /Index [0 1] /Length {} /Root 1 0 R >>",
            stream_data.len()
        );
        data.extend_from_slice(format!("2 0 obj\n{dict_str}\nstream\n").as_bytes());
        data.extend_from_slice(&stream_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();

        // gen should be clamped to 0
        match result.xref.get(0) {
            Some(XrefEntry::Uncompressed { gen, .. }) => {
                assert_eq!(*gen, 0, "overflowing gen should be clamped to 0");
            }
            other => panic!("expected uncompressed entry, got {other:?}"),
        }

        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.message.contains("exceeds u16")),
            "expected u16 overflow warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_xref_stream_free_entry_next_free_overflow() {
        // free entry with next_free > u32::MAX via 4-byte field
        // Actually u64 values from read_field_be with width 4 can't exceed u32::MAX (0xFFFFFFFF)
        // but we can test the gen overflow for free entries
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_offset = data.len();
        // /W [1 2 4]: type=1byte, field1=2bytes, field2=4bytes
        // Free entry: type=0, next_free=0, gen=0x00010000 (exceeds u16)
        let stream_data: Vec<u8> = vec![
            0, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, // obj 0: free, next=0, gen=65536
        ];
        let dict_str = format!(
            "<< /Type /XRef /Size 2 /W [1 2 4] /Index [0 1] /Length {} /Root 1 0 R >>",
            stream_data.len()
        );
        data.extend_from_slice(format!("2 0 obj\n{dict_str}\nstream\n").as_bytes());
        data.extend_from_slice(&stream_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();

        match result.xref.get(0) {
            Some(XrefEntry::Free { gen, .. }) => {
                assert_eq!(*gen, 0, "overflowing gen should be clamped to 0");
            }
            other => panic!("expected free entry, got {other:?}"),
        }

        let warnings = diag.warnings();
        assert!(warnings.iter().any(|w| w.message.contains("exceeds u16")));
    }

    #[test]
    fn test_xref_stream_compressed_entry_stream_obj_overflow() {
        // Compressed entry with stream_obj that fits in u64 but not u32
        // is not possible with max width 4, but we can test index overflow
        // with /W [1 2 4] where index uses 4 bytes
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_offset = data.len();
        // /W [1 4 4]: type, stream_obj (4 bytes), index (4 bytes)
        // compressed entry: type=2, stream_obj=10, index=0x1_0000_0000 can't fit in 4 bytes
        // Instead test that a value fitting in u32 works, and rely on gen overflow
        // tests above for the clamping path.
        // Let's use /W [1 2 2] with a valid compressed entry for coverage of type 2
        let stream_data: Vec<u8> = vec![
            0, 0x00, 0x00, 0xFF, 0xFF, // obj 0: free
            2, 0x00, 0x0A, 0x00, 0x07, // obj 1: compressed in stream 10, index 7
        ];
        let dict_str = format!(
            "<< /Type /XRef /Size 3 /W [1 2 2] /Index [0 2] /Length {} /Root 1 0 R >>",
            stream_data.len()
        );
        data.extend_from_slice(format!("2 0 obj\n{dict_str}\nstream\n").as_bytes());
        data.extend_from_slice(&stream_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();

        match result.xref.get(1) {
            Some(XrefEntry::Compressed { stream_obj, index }) => {
                assert_eq!(*stream_obj, 10);
                assert_eq!(*index, 7);
            }
            other => panic!("expected compressed entry for obj 1, got {other:?}"),
        }
    }

    // -- read_field_be edge cases --

    #[test]
    fn test_read_field_be_width_0_empty_slice() {
        // Width 0 should return 0 regardless of slice content (even empty)
        assert_eq!(read_field_be(&[], 0), 0);
    }

    #[test]
    fn test_read_field_be_width_1_single_zero() {
        assert_eq!(read_field_be(&[0x00], 1), 0);
    }

    #[test]
    fn test_read_field_be_width_4_all_zeros() {
        assert_eq!(read_field_be(&[0x00, 0x00, 0x00, 0x00], 4), 0);
    }

    #[test]
    fn test_read_field_be_width_3_mixed() {
        // 0xAB_CD_EF
        assert_eq!(read_field_be(&[0xAB, 0xCD, 0xEF], 3), 0xABCDEF);
    }

    #[test]
    fn test_read_field_be_extra_bytes_ignored() {
        // Slice is longer than width; extra bytes should be ignored
        assert_eq!(read_field_be(&[0x01, 0x02, 0xFF, 0xFF], 2), 0x0102);
    }

    // -- rfind_bytes edge cases --

    #[test]
    fn test_rfind_bytes_empty_needle() {
        assert_eq!(rfind_bytes(b"hello", b""), None);
    }

    #[test]
    fn test_rfind_bytes_empty_haystack() {
        assert_eq!(rfind_bytes(b"", b"x"), None);
    }

    #[test]
    fn test_rfind_bytes_needle_longer_than_haystack() {
        assert_eq!(rfind_bytes(b"ab", b"abcdef"), None);
    }

    #[test]
    fn test_rfind_bytes_single_byte() {
        assert_eq!(rfind_bytes(b"abcba", b"a"), Some(4));
    }

    #[test]
    fn test_rfind_bytes_full_match() {
        assert_eq!(rfind_bytes(b"abc", b"abc"), Some(0));
    }

    #[test]
    fn test_find_bytes_needle_longer_than_haystack() {
        assert_eq!(find_bytes(b"ab", b"abcdef"), None);
    }

    #[test]
    fn test_find_bytes_single_byte() {
        assert_eq!(find_bytes(b"abcba", b"a"), Some(0));
    }

    // -- DocumentParser construction and parse() failure modes --

    #[test]
    fn test_document_parser_new_empty_data() {
        let parser = DocumentParser::new(b"");
        assert!(parser.parse().is_err());
    }

    #[test]
    fn test_document_parser_with_diagnostics() {
        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(b"", diag.clone());
        assert!(parser.parse().is_err());
        // No warnings expected for empty file (just errors)
    }

    #[test]
    fn test_parse_fails_on_valid_header_but_no_startxref() {
        let data = b"%PDF-1.4\nsome body content here\n%%EOF\n";
        let parser = DocumentParser::new(data);
        let err = parser.parse().unwrap_err();
        assert!(err.to_string().contains("startxref"));
    }

    #[test]
    fn test_parse_fails_on_valid_header_and_startxref_but_bad_xref() {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");
        let xref_offset = data.len();
        // Not a valid xref table or xref stream
        data.extend_from_slice(b"[1 2 3]\n");
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        assert!(parser.parse().is_err());
    }

    #[test]
    fn test_is_xref_table_at_offset_past_end() {
        let data = b"%PDF-1.4\n";
        let parser = DocumentParser::new(data);
        assert!(!parser.is_xref_table_at(9999));
    }

    #[test]
    fn test_is_xref_table_at_too_close_to_end() {
        // "xre" is only 3 bytes, needs 4
        let data = b"xre";
        let parser = DocumentParser::new(data);
        assert!(!parser.is_xref_table_at(0));
    }

    #[test]
    fn test_is_xref_table_at_valid() {
        let data = b"xref\n0 1\n";
        let parser = DocumentParser::new(data);
        assert!(parser.is_xref_table_at(0));
    }

    #[test]
    fn test_parse_dictionary_at_end_of_file() {
        let data = b"%PDF-1.4\n";
        let parser = DocumentParser::new(data);
        assert!(parser.parse_dictionary_at(data.len()).is_err());
    }

    #[test]
    fn test_parse_ascii_integer_at_end_of_file() {
        let data = b"%PDF-1.4\n";
        let parser = DocumentParser::new(data);
        assert!(parser.parse_ascii_integer(data.len()).is_err());
    }

    #[test]
    fn test_parse_ascii_integer_no_digits() {
        let data = b"abc";
        let parser = DocumentParser::new(data);
        let err = parser.parse_ascii_integer(0).unwrap_err();
        assert!(err.to_string().contains("digit"));
    }

    #[test]
    fn test_parse_ascii_integer_negative() {
        let data = b"-42rest";
        let parser = DocumentParser::new(data);
        let (value, pos) = parser.parse_ascii_integer(0).unwrap();
        assert_eq!(value, -42);
        assert_eq!(pos, 3);
    }

    #[test]
    fn test_parse_ascii_integer_positive_sign() {
        let data = b"+99end";
        let parser = DocumentParser::new(data);
        let (value, pos) = parser.parse_ascii_integer(0).unwrap();
        assert_eq!(value, 99);
        assert_eq!(pos, 3);
    }

    #[test]
    fn test_parse_ascii_integer_sign_only() {
        let data = b"-abc";
        let parser = DocumentParser::new(data);
        assert!(parser.parse_ascii_integer(0).is_err());
    }

    #[test]
    fn test_read_integer_after_whitespace_end_of_file() {
        let data = b"   ";
        let parser = DocumentParser::new(data);
        assert!(parser.read_integer_after_whitespace(0).is_err());
    }

    #[test]
    fn test_read_integer_after_whitespace_no_digits() {
        let data = b"   abc";
        let parser = DocumentParser::new(data);
        let err = parser.read_integer_after_whitespace(0).unwrap_err();
        assert!(err.to_string().contains("digit"));
    }

    #[test]
    fn test_read_integer_after_whitespace_normal() {
        let data = b"  123end";
        let parser = DocumentParser::new(data);
        let result = parser.read_integer_after_whitespace(0).unwrap();
        assert_eq!(result, "123");
    }

    // -- Xref stream: data shorter than expected (clamping) --

    #[test]
    fn test_xref_stream_data_shorter_than_declared() {
        // Declare more entries than the stream data contains.
        // Parser should warn and clamp to available entries.
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        let obj_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_offset = data.len();
        let obj_off = obj_offset as u16;
        // /W [1 2 1] = 4 bytes per entry. Only provide 2 entries (8 bytes)
        // but declare /Size 10 (would need 10 entries = 40 bytes).
        let stream_data: Vec<u8> = vec![
            0,
            0x00,
            0x00,
            0xFF, // obj 0: free
            1,
            (obj_off >> 8) as u8,
            obj_off as u8,
            0x00, // obj 1: uncompressed
        ];
        let dict_str = format!(
            "<< /Type /XRef /Size 10 /W [1 2 1] /Length {} /Root 1 0 R >>",
            stream_data.len()
        );
        data.extend_from_slice(format!("2 0 obj\n{dict_str}\nstream\n").as_bytes());
        data.extend_from_slice(&stream_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();

        // Should only have the 2 entries that actually fit
        assert!(result.xref.get(0).is_some());
        assert!(result.xref.get(1).is_some());

        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.message.contains("clamping")),
            "expected clamping warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    // -- Xref stream: not a stream object --

    #[test]
    fn test_xref_stream_not_a_stream_object() {
        // startxref points to an object that is a dictionary, not a stream
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let xref_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /XRef /Size 1 /W [1 2 1] >>\nendobj\n");
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let err = parser.parse().unwrap_err();
        // Should fail because it's a dict, not a stream
        assert!(
            err.to_string().contains("stream") || err.to_string().contains("dictionary"),
            "expected stream-related error, got: {}",
            err
        );
    }

    // -- Xref entry: entry truncated before 18 bytes --

    #[test]
    fn test_xref_entry_truncated() {
        // xref entry is cut short
        let body = b"";
        let mut xref = Vec::new();
        xref.extend_from_slice(b"xref\n0 1\n");
        xref.extend_from_slice(b"0000000000 6553"); // only 15 bytes, need 18+
                                                    // no trailer

        let data = make_simple_pdf(body, &xref);
        let parser = DocumentParser::new(&data);
        // Should error due to truncated entry or missing trailer
        assert!(parser.parse().is_err());
    }

    // -- Xref entry: invalid type marker --

    #[test]
    fn test_xref_entry_invalid_type_marker() {
        // Entry with 'x' instead of 'n' or 'f'
        let body = b"";
        let mut xref = Vec::new();
        xref.extend_from_slice(b"xref\n0 1\n");
        xref.extend_from_slice(b"0000000000 65535 x \r\n");
        xref.extend_from_slice(b"trailer\n<< /Size 1 >>\n");

        let data = make_simple_pdf(body, &xref);
        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        // The malformed entry is warned and skipped; parse should still succeed
        let result = parser.parse().unwrap();

        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.message.contains("malformed")),
            "expected malformed entry warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
        // The entry should not be in the table since it was skipped
        assert!(result.xref.get(0).is_none());
    }

    // -- XrefTable iteration --

    #[test]
    fn test_xref_table_iter() {
        let mut xref = XrefTable::new();
        xref.insert_if_absent(
            0,
            XrefEntry::Free {
                next_free: 0,
                gen: 65535,
            },
        );
        xref.insert_if_absent(
            5,
            XrefEntry::Uncompressed {
                offset: 100,
                gen: 0,
            },
        );

        let entries: Vec<(u32, &XrefEntry)> = xref.iter().collect();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|(num, _)| *num == 0));
        assert!(entries.iter().any(|(num, _)| *num == 5));
    }

    // -- Xref subsection count exceeds remaining data --

    #[test]
    fn test_xref_subsection_count_exceeds_data_warns() {
        // Subsection header claims more entries than bytes available,
        // but we have enough to parse at least 1 entry before hitting EOF
        let body = b"";
        let mut xref = Vec::new();
        xref.extend_from_slice(b"xref\n0 999\n");
        xref.extend_from_slice(b"0000000000 65535 f \r\n");
        xref.extend_from_slice(b"trailer\n<< /Size 999 >>\n");

        let data = make_simple_pdf(body, &xref);
        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        // This will fail because after 1 entry, the parser tries to read
        // 998 more and hits "trailer" or runs out of data
        let _ = parser.parse();
        // Just verify warnings were generated about the count
        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("exceeds") || w.message.contains("malformed")),
            "expected xref count warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    // -- Skip whitespace edge case --

    #[test]
    fn test_skip_whitespace_all_types() {
        let data = b" \t\r\n\x0C\x00hello";
        let pos = skip_whitespace(data, 0);
        assert_eq!(pos, 6);
        assert_eq!(data[pos], b'h');
    }

    #[test]
    fn test_skip_whitespace_no_whitespace() {
        let data = b"hello";
        let pos = skip_whitespace(data, 0);
        assert_eq!(pos, 0);
    }

    #[test]
    fn test_skip_whitespace_all_whitespace() {
        let data = b"   \t\n";
        let pos = skip_whitespace(data, 0);
        assert_eq!(pos, data.len());
    }

    #[test]
    fn test_skip_whitespace_from_middle() {
        let data = b"abc  def";
        let pos = skip_whitespace(data, 3);
        assert_eq!(pos, 5);
    }

    #[test]
    fn test_skip_whitespace_past_end() {
        let data = b"abc";
        let pos = skip_whitespace(data, 10);
        assert_eq!(pos, 10);
    }

    // -- Xref stream offset beyond file --

    #[test]
    fn test_xref_stream_offset_beyond_file() {
        // Construct a PDF where the startxref points to an offset that is
        // past the actual data (parse_xref_stream checks start >= data.len()).
        // We manually build bytes with startxref pointing to a huge offset.
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        // Write a valid-looking xref stream object for the parser to find
        data.extend_from_slice(
            b"1 0 obj\n<< /Type /XRef /Size 1 /W [1 2 1] /Length 4 >>\nstream\n",
        );
        data.extend_from_slice(&[0, 0x00, 0x00, 0xFF]);
        data.extend_from_slice(b"\nendstream\nendobj\n");
        // Put startxref pointing past EOF (the find_startxref check catches this)
        let past_eof = data.len() + 100;
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(past_eof.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        assert!(parser.parse().is_err());
    }

    // -- PdfVersion Display --

    #[test]
    fn test_pdf_version_display_various() {
        assert_eq!(format!("{}", PdfVersion { major: 2, minor: 0 }), "2.0");
        assert_eq!(format!("{}", PdfVersion { major: 1, minor: 0 }), "1.0");
        assert_eq!(format!("{}", PdfVersion { major: 1, minor: 9 }), "1.9");
    }

    // -- Xref stream: unknown entry type multiple --

    #[test]
    fn test_xref_stream_multiple_unknown_types_skipped() {
        // Multiple entries with unknown types (3, 4, 5) should all be skipped
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        let obj_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_offset = data.len();
        let obj_off = obj_offset as u16;
        let stream_data: Vec<u8> = vec![
            0,
            0x00,
            0x00,
            0xFF, // obj 0: free
            1,
            (obj_off >> 8) as u8,
            obj_off as u8,
            0x00, // obj 1: uncompressed
            3,
            0x00,
            0x00,
            0x00, // obj 2: unknown type 3
            4,
            0x00,
            0x00,
            0x00, // obj 3: unknown type 4
        ];
        let dict_str = format!(
            "<< /Type /XRef /Size 5 /W [1 2 1] /Index [0 4] /Length {} /Root 1 0 R >>",
            stream_data.len()
        );
        data.extend_from_slice(format!("4 0 obj\n{dict_str}\nstream\n").as_bytes());
        data.extend_from_slice(&stream_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();

        assert!(result.xref.get(0).is_some());
        assert!(result.xref.get(1).is_some());
        assert!(result.xref.get(2).is_none()); // skipped
        assert!(result.xref.get(3).is_none()); // skipped

        let warnings = diag.warnings();
        let unknown_warnings: Vec<_> = warnings
            .iter()
            .filter(|w| w.message.contains("unknown xref entry type"))
            .collect();
        assert_eq!(
            unknown_warnings.len(),
            2,
            "expected 2 unknown type warnings, got {}",
            unknown_warnings.len()
        );
    }

    // -- Xref stream: W element not an integer --

    #[test]
    fn test_xref_stream_w_element_not_integer() {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let xref_offset = data.len();
        data.extend_from_slice(
            b"1 0 obj\n<< /Type /XRef /Size 1 /W [1 /Name 2] /Length 0 >>\nstream\n\nendstream\nendobj\n",
        );
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let err = parser.parse().unwrap_err();
        assert!(err.to_string().contains("/W"));
    }

    // -- Xref stream: parse_xref_stream offset beyond file --

    #[test]
    fn test_xref_stream_start_offset_beyond_file() {
        // Directly test that parse_xref_stream errors when offset is past file end.
        // We can't easily call parse_xref_stream directly since it requires
        // the full file setup, but we can set startxref to point past end
        // which triggers find_startxref's check.
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\nstartxref\n99999\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        assert!(parser.parse().is_err());
    }

    // -- parse_xref_entry: non-ASCII in offset or gen field --

    #[test]
    fn test_xref_entry_non_ascii_in_gen_field() {
        let body = b"";
        let mut xref = Vec::new();
        xref.extend_from_slice(b"xref\n0 1\n");
        // Non-ASCII byte 0x80 in the gen field
        xref.extend_from_slice(b"0000000000 \x80\x80\x80\x80\x80 f \r\n");
        xref.extend_from_slice(b"trailer\n<< /Size 1 >>\n");

        let data = make_simple_pdf(body, &xref);
        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let _ = parser.parse();

        // Should have warned about malformed entry
        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.message.contains("malformed")),
            "expected malformed warning for non-ASCII gen field"
        );
    }

    // -- Xref: free entry with offset exceeding u32 --

    #[test]
    fn test_xref_free_entry_large_offset_in_table() {
        // Free entry with offset (next_free) = 5000000000 (exceeds u32)
        let body = b"";
        let mut xref = Vec::new();
        xref.extend_from_slice(b"xref\n0 1\n");
        xref.extend_from_slice(b"5000000000 65535 f \r\n");
        xref.extend_from_slice(b"trailer\n<< /Size 1 >>\n");

        let data = make_simple_pdf(body, &xref);
        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();

        // Should clamp to 0 and warn
        match result.xref.get(0) {
            Some(XrefEntry::Free { next_free, .. }) => {
                assert_eq!(*next_free, 0, "large next_free should be clamped to 0");
            }
            other => panic!("expected free entry, got {other:?}"),
        }

        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.message.contains("exceeds u32")),
            "expected u32 overflow warning for next_free"
        );
    }

    // -- XrefTable::default() --

    #[test]
    fn test_xref_table_default() {
        let xref = XrefTable::default();
        assert!(xref.is_empty());
        assert_eq!(xref.len(), 0);
    }

    // -- parse_xref_stream: bad object number token --

    #[test]
    fn test_xref_stream_bad_obj_num_token() {
        // startxref points to something that starts with a non-integer token
        // (e.g., a name) where the object number should be
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let xref_offset = data.len();
        // Write "/Name 0 obj" instead of "N 0 obj"
        data.extend_from_slice(b"/Name 0 obj\n<< /Type /XRef /Size 1 /W [1 2 1] /Length 0 >>\nstream\n\nendstream\nendobj\n");
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let err = parser.parse().unwrap_err();
        assert!(
            err.to_string().contains("object number"),
            "expected object number error, got: {err}"
        );
    }

    // -- parse_xref_stream: bad generation number token --

    #[test]
    fn test_xref_stream_bad_gen_token() {
        // Object number is fine, but generation is a name instead of integer
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let xref_offset = data.len();
        data.extend_from_slice(b"1 /Name obj\n<< /Type /XRef /Size 1 /W [1 2 1] /Length 0 >>\nstream\n\nendstream\nendobj\n");
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let err = parser.parse().unwrap_err();
        assert!(
            err.to_string().contains("generation number"),
            "expected generation number error, got: {err}"
        );
    }

    // -- parse_xref_stream: missing obj keyword --

    #[test]
    fn test_xref_stream_missing_obj_keyword() {
        // Object number and gen are fine, but third token is not "obj"
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let xref_offset = data.len();
        data.extend_from_slice(b"1 0 stream\n<< /Type /XRef /Size 1 /W [1 2 1] /Length 0 >>\nstream\n\nendstream\nendobj\n");
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let err = parser.parse().unwrap_err();
        assert!(
            err.to_string().contains("obj"),
            "expected 'obj' keyword error, got: {err}"
        );
    }

    // -- parse_xref_stream: offset beyond file (via direct is_xref_table_at false path) --

    #[test]
    fn test_xref_stream_start_beyond_eof_direct() {
        // Build a file where the startxref offset is valid (within the file)
        // but points to non-xref data. This exercises the is_xref_table_at -> false
        // -> parse_xref_stream path with a bad token at the start.
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let bad_offset = data.len();
        // Write a string literal where an object should be
        data.extend_from_slice(b"(not an object at all)\n");
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(bad_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        assert!(parser.parse().is_err());
    }

    // -- parse_xref_section: negative first_obj and negative count --

    #[test]
    fn test_xref_subsection_negative_first_obj() {
        let body = b"";
        let xref = b"xref\n-1 1\n0000000000 65535 f \r\ntrailer\n<< /Size 1 >>\n";
        let data = make_simple_pdf(body, xref);
        let parser = DocumentParser::new(&data);
        let err = parser.parse().unwrap_err();
        assert!(
            err.to_string().contains("first object number"),
            "expected first object number error, got: {err}"
        );
    }

    #[test]
    fn test_xref_subsection_negative_count() {
        let body = b"";
        let xref = b"xref\n0 -1\n0000000000 65535 f \r\ntrailer\n<< /Size 1 >>\n";
        let data = make_simple_pdf(body, xref);
        let parser = DocumentParser::new(&data);
        let err = parser.parse().unwrap_err();
        assert!(
            err.to_string().contains("valid count"),
            "expected valid count error, got: {err}"
        );
    }

    // -- parse_xref_section: first_obj + count overflow --

    #[test]
    fn test_xref_subsection_obj_num_overflow() {
        // first_obj near u32::MAX, count > 1 would overflow
        let body = b"";
        let xref = b"xref\n4294967295 2\n0000000000 65535 f \r\n0000000000 65535 f \r\ntrailer\n<< /Size 1 >>\n";
        let data = make_simple_pdf(body, xref);
        let parser = DocumentParser::new(&data);
        let err = parser.parse().unwrap_err();
        assert!(
            err.to_string().contains("overflow"),
            "expected overflow error, got: {err}"
        );
    }

    // -- parse_ascii_integer: sign followed by end of file --

    #[test]
    fn test_parse_ascii_integer_sign_then_eof() {
        let data = b"-";
        let parser = DocumentParser::new(data);
        let err = parser.parse_ascii_integer(0).unwrap_err();
        assert!(
            err.to_string().contains("end of file"),
            "expected end of file error, got: {err}"
        );
    }

    // -- parse_xref_entry: non-ASCII in offset field --

    #[test]
    fn test_xref_entry_non_ascii_in_offset_field() {
        let body = b"";
        let mut xref = Vec::new();
        xref.extend_from_slice(b"xref\n0 1\n");
        // Non-ASCII bytes 0x80 in the offset field
        xref.extend_from_slice(b"\x80\x80\x80\x80\x80\x80\x80\x80\x80\x80 65535 f \r\n");
        xref.extend_from_slice(b"trailer\n<< /Size 1 >>\n");

        let data = make_simple_pdf(body, &xref);
        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let _ = parser.parse();

        // Should have warned about malformed entry (non-ASCII bytes in offset)
        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.message.contains("malformed")),
            "expected malformed warning for non-ASCII offset field"
        );
    }

    // -- parse_xref_entry: space at position 16 not a space --

    #[test]
    fn test_xref_entry_non_space_before_type_marker_warns() {
        // Tab instead of space at position 16 (between gen and type marker)
        let body = b"";
        let mut xref = Vec::new();
        xref.extend_from_slice(b"xref\n0 1\n");
        // "0000000000 65535\tf \r\n" -- tab at position 16
        xref.extend_from_slice(b"0000000000 65535\tf \r\n");
        xref.extend_from_slice(b"trailer\n<< /Size 1 >>\n");

        let data = make_simple_pdf(body, &xref);
        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let _ = parser.parse();
        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("expected space before type marker")),
            "expected 'space before type marker' warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    // -- Xref stream: /Index element not an integer --

    #[test]
    fn test_xref_stream_index_element_not_integer() {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let xref_offset = data.len();
        data.extend_from_slice(
            b"1 0 obj\n<< /Type /XRef /Size 1 /W [1 2 1] /Index [/Name 1] /Length 4 >>\nstream\n\x01\x00\x00\x00\nendstream\nendobj\n",
        );
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let err = parser.parse().unwrap_err();
        assert!(
            err.to_string().contains("integer"),
            "expected integer error for /Index element, got: {err}"
        );
    }

    // -- Xref stream: data extends beyond file --

    #[test]
    fn test_xref_stream_data_extends_beyond_file() {
        // Build an xref stream where /Length claims more data than the file contains.
        // No endstream marker, so the parser can't recover via scanning.
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let xref_offset = data.len();
        // /Length 9999 with no endstream. The object parser will use
        // /Length as-is since there's no endstream to recover with.
        data.extend_from_slice(
            b"1 0 obj\n<< /Type /XRef /Size 1 /W [1 2 1] /Length 9999 >>\nstream\n\x00\x00\x00\x00",
        );
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        // Should fail: stream data extends past end of file, and no endstream
        // to recover with. Whether it errors at object parsing or at
        // data range validation doesn't matter; it should not succeed.
        assert!(parser.parse().is_err());
    }

    // -- Xref stream: multiple /Index subsections --

    #[test]
    fn test_xref_stream_multiple_index_subsections() {
        // Test /Index with multiple pairs: [0 2 10 1]
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        let obj_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_offset = data.len();
        let obj_off = obj_offset as u16;
        // /W [1 2 1] = 4 bytes per entry
        // Index [0 2 10 1]: objects 0,1 then object 10
        let stream_data: Vec<u8> = vec![
            0,
            0x00,
            0x00,
            0xFF, // obj 0: free
            1,
            (obj_off >> 8) as u8,
            obj_off as u8,
            0x00, // obj 1: uncompressed
            1,
            0x01,
            0x00,
            0x00, // obj 10: uncompressed at offset 0x0100
        ];
        let dict_str = format!(
            "<< /Type /XRef /Size 11 /W [1 2 1] /Index [0 2 10 1] /Length {} /Root 1 0 R >>",
            stream_data.len()
        );
        data.extend_from_slice(format!("2 0 obj\n{dict_str}\nstream\n").as_bytes());
        data.extend_from_slice(&stream_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();

        assert!(result.xref.get(0).is_some());
        assert!(result.xref.get(1).is_some());
        assert!(result.xref.get(10).is_some());
        // Objects 2-9 should not exist
        assert!(result.xref.get(2).is_none());
        assert!(result.xref.get(9).is_none());

        match result.xref.get(10) {
            Some(XrefEntry::Uncompressed { offset, .. }) => {
                assert_eq!(*offset, 0x0100);
            }
            other => panic!("expected uncompressed entry for obj 10, got {other:?}"),
        }
    }

    // -- Xref stream: no /Index defaults to [0 Size] --

    #[test]
    fn test_xref_stream_no_index_defaults_to_zero_size() {
        // Build xref stream without /Index, should default to [0 Size]
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        let obj_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_offset = data.len();
        let obj_off = obj_offset as u16;
        // /W [1 2 1] = 4 bytes per entry, /Size 3 = 3 entries (0, 1, 2)
        let stream_data: Vec<u8> = vec![
            0,
            0x00,
            0x00,
            0xFF, // obj 0: free
            1,
            (obj_off >> 8) as u8,
            obj_off as u8,
            0x00, // obj 1: uncompressed
            1,
            0x00,
            0x50,
            0x00, // obj 2: uncompressed at 0x50
        ];
        let dict_str = format!(
            "<< /Type /XRef /Size 3 /W [1 2 1] /Length {} /Root 1 0 R >>",
            stream_data.len()
        );
        data.extend_from_slice(format!("3 0 obj\n{dict_str}\nstream\n").as_bytes());
        data.extend_from_slice(&stream_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();

        assert!(result.xref.get(0).is_some());
        assert!(result.xref.get(1).is_some());
        assert!(result.xref.get(2).is_some());
    }

    // -- Xref stream: obj_num overflow in index iteration --

    #[test]
    fn test_xref_stream_obj_num_overflow_in_iteration() {
        // /Index [4294967295 2]: first obj is u32::MAX, so first+1 overflows
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_offset = data.len();
        // /W [1 2 1] = 4 bytes per entry, 2 entries
        let stream_data: Vec<u8> = vec![
            1, 0x00, 0x09, 0x00, // first entry
            1, 0x00, 0x09, 0x00, // second entry (would need obj_num u32::MAX + 1)
        ];
        let dict_str = format!(
            "<< /Type /XRef /Size 100 /W [1 2 1] /Index [4294967295 2] /Length {} /Root 1 0 R >>",
            stream_data.len()
        );
        data.extend_from_slice(format!("2 0 obj\n{dict_str}\nstream\n").as_bytes());
        data.extend_from_slice(&stream_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();

        // First entry (obj 4294967295) should be present
        assert!(result.xref.get(u32::MAX).is_some());

        // Second entry should have been skipped due to overflow
        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.message.contains("overflow")),
            "expected overflow warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    // -- Xref stream: compressed entry with overflowing stream_obj and index --

    #[test]
    fn test_xref_stream_compressed_stream_obj_overflow() {
        // Use /W [1 4 4] so field values can exceed u32 range
        // But actually width 4 max value is 0xFFFFFFFF which is u32::MAX.
        // We need field values that require u64 to overflow u32, which
        // isn't possible with width 4. Instead test with valid compressed entry.
        // This test verifies the compressed entry parsing path with W [1 4 2].
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_offset = data.len();
        // /W [1 4 2]: type=1byte, field1=4bytes, field2=2bytes
        let stream_data: Vec<u8> = vec![
            0, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, // obj 0: free, next=0, gen=65535
            2, 0x00, 0x00, 0x00, 0x0A, 0x00, 0x05, // obj 1: compressed in stream 10, index 5
        ];
        let dict_str = format!(
            "<< /Type /XRef /Size 3 /W [1 4 2] /Index [0 2] /Length {} /Root 1 0 R >>",
            stream_data.len()
        );
        data.extend_from_slice(format!("2 0 obj\n{dict_str}\nstream\n").as_bytes());
        data.extend_from_slice(&stream_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();

        match result.xref.get(1) {
            Some(XrefEntry::Compressed { stream_obj, index }) => {
                assert_eq!(*stream_obj, 10);
                assert_eq!(*index, 5);
            }
            other => panic!("expected compressed entry, got {other:?}"),
        }
    }

    // -- read_integer_after_whitespace: no leading whitespace --

    #[test]
    fn test_read_integer_after_whitespace_no_leading_ws() {
        let data = b"42rest";
        let parser = DocumentParser::new(data);
        let result = parser.read_integer_after_whitespace(0).unwrap();
        assert_eq!(result, "42");
    }

    // -- parse_dictionary_at: non-dictionary object --

    #[test]
    fn test_parse_dictionary_at_not_a_dict() {
        let data = b"42";
        let parser = DocumentParser::new(data);
        let err = parser.parse_dictionary_at(0).unwrap_err();
        assert!(
            err.to_string().contains("dictionary"),
            "expected dictionary error, got: {err}"
        );
    }

    // -- XrefTable: insert_if_absent with various entry types --

    #[test]
    fn test_xref_table_insert_compressed_and_free() {
        let mut xref = XrefTable::new();
        xref.insert_if_absent(
            10,
            XrefEntry::Compressed {
                stream_obj: 5,
                index: 3,
            },
        );
        xref.insert_if_absent(
            20,
            XrefEntry::Free {
                next_free: 0,
                gen: 0,
            },
        );

        assert_eq!(xref.len(), 2);
        assert!(!xref.is_empty());

        match xref.get(10) {
            Some(XrefEntry::Compressed { stream_obj, index }) => {
                assert_eq!(*stream_obj, 5);
                assert_eq!(*index, 3);
            }
            other => panic!("expected compressed, got {other:?}"),
        }
        match xref.get(20) {
            Some(XrefEntry::Free { next_free, gen }) => {
                assert_eq!(*next_free, 0);
                assert_eq!(*gen, 0);
            }
            other => panic!("expected free, got {other:?}"),
        }
    }

    // ========================================================================
    // Additional coverage: parse_header edge cases (lines 190, 199, 209-210)
    // ========================================================================

    #[test]
    fn test_parse_header_at_nonzero_offset_warns() {
        // PDF header at offset > 0 should warn about garbage bytes
        let mut data = Vec::new();
        data.extend_from_slice(b"JUNKJUNK");
        data.extend_from_slice(b"%PDF-1.7\nrest");
        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let version = parser.parse_header().unwrap();
        assert_eq!(version.major, 1);
        assert_eq!(version.minor, 7);
        let warnings = diag.warnings();
        assert!(warnings.iter().any(|w| w.message.contains("offset 8")));
    }

    #[test]
    fn test_parse_header_version_truncated_after_major() {
        // "%PDF-1" with exactly 1 byte after marker (need 3)
        let data = b"%PDF-1";
        let parser = DocumentParser::new(data);
        let err = parser.parse_header().unwrap_err();
        assert!(err.to_string().contains("truncated"));
    }

    #[test]
    fn test_parse_header_non_digit_minor() {
        // "%PDF-1.!" has non-digit minor
        let data = b"%PDF-1.!\nrest";
        let parser = DocumentParser::new(data);
        let err = parser.parse_header().unwrap_err();
        assert!(err.to_string().contains("version"));
    }

    // ========================================================================
    // Additional coverage: find_startxref with unparseable offset (lines 241-244)
    // ========================================================================

    #[test]
    fn test_find_startxref_offset_unparseable() {
        // startxref followed by something that starts with digits but
        // overflows (extremely large number)
        let data = b"%PDF-1.4\nstartxref\n999999999999999999999999999999\n%%EOF\n";
        let parser = DocumentParser::new(data);
        let err = parser.find_startxref().unwrap_err();
        // Should fail parsing as u64
        assert!(err.to_string().contains("offset") || err.to_string().contains("integer"));
    }

    // ========================================================================
    // Additional coverage: xref chain depth limit (lines 307-308, 310-311, 314)
    // These are already tested but let me add a focused minimal test.
    // ========================================================================

    // Already well-tested in test_xref_chain_depth_limit above.

    // ========================================================================
    // Additional coverage: circular /Prev (lines 320-321, 324)
    // Already tested, but let me ensure the warning message path is hit.
    // ========================================================================

    // Already well-tested in test_circular_prev_detected above.

    // ========================================================================
    // Additional coverage: later xref section failure (lines 346-347, 349, 352)
    // ========================================================================

    #[test]
    fn test_later_section_parse_failure_with_xref_stream() {
        // First xref section (stream) parses OK, /Prev points to
        // an offset that has garbage. Should warn and continue.
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        let garbage_offset = data.len();
        data.extend_from_slice(b"GARBAGE GARBAGE GARBAGE\n");

        let obj_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_offset = data.len();
        let obj_off = obj_offset as u16;
        // Xref stream with /Prev pointing to garbage
        let stream_data: Vec<u8> = vec![
            0,
            0x00,
            0x00,
            0xFF, // obj 0: free
            1,
            (obj_off >> 8) as u8,
            obj_off as u8,
            0x00, // obj 1: uncompressed
        ];
        let dict_str = format!(
            "<< /Type /XRef /Size 3 /W [1 2 1] /Index [0 2] /Length {} /Root 1 0 R /Prev {garbage_offset} >>",
            stream_data.len()
        );
        data.extend_from_slice(format!("2 0 obj\n{dict_str}\nstream\n").as_bytes());
        data.extend_from_slice(&stream_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();
        // First section should have succeeded
        assert!(result.xref.get(0).is_some());
        assert!(result.xref.get(1).is_some());

        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("failed to parse")),
            "expected parse failure warning"
        );
    }

    // ========================================================================
    // Additional coverage: /Prev is non-integer (lines 377, 380-381)
    // ========================================================================

    #[test]
    fn test_prev_is_real_number_warns() {
        // /Prev is a real number (3.14), not an integer. Should warn.
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");
        let xref_offset = data.len();
        let xref = "xref\n\
             0 1\n\
             0000000000 65535 f \r\n\
             trailer\n\
             << /Size 1 /Prev 3.14 >>\n";
        data.extend_from_slice(xref.as_bytes());
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();
        assert_eq!(result.xref.len(), 1);
        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("not an integer")),
            "expected non-integer /Prev warning"
        );
    }

    // ========================================================================
    // Additional coverage: no trailer dict found (line 391)
    // ========================================================================

    // This is hard to trigger because the while loop always adds at least
    // one trailer if the first section parses. The only path is if
    // current_offset starts as None, which it never does. But if the first
    // parse succeeds, final_trailer is Some. If it fails, we return Err.
    // So this line is unreachable in normal operation.

    // ========================================================================
    // Additional coverage: xref table missing trailer at EOF (lines 421, 425)
    // ========================================================================

    #[test]
    fn test_xref_table_eof_before_trailer() {
        // xref with entries but data ends before "trailer" keyword
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");
        let xref_offset = data.len();
        data.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \r\n");
        // EOF right after entries, no trailer
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        assert!(parser.parse().is_err());
    }

    // ========================================================================
    // Additional coverage: xref entry type 'f' with normal offset (line 593)
    // ========================================================================

    #[test]
    fn test_xref_free_entry_normal() {
        let body = b"";
        let xref = b"xref\n\
            0 2\n\
            0000000000 65535 f \r\n\
            0000000001 00000 f \r\n\
            trailer\n\
            << /Size 2 >>\n";
        let data = make_simple_pdf(body, xref);
        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();
        assert_eq!(result.xref.len(), 2);
        match result.xref.get(0) {
            Some(XrefEntry::Free { next_free, gen }) => {
                assert_eq!(*next_free, 0);
                assert_eq!(*gen, 65535);
            }
            other => panic!("expected free entry for obj 0, got {other:?}"),
        }
        match result.xref.get(1) {
            Some(XrefEntry::Free { next_free, gen }) => {
                assert_eq!(*next_free, 1);
                assert_eq!(*gen, 0);
            }
            other => panic!("expected free entry for obj 1, got {other:?}"),
        }
    }

    // ========================================================================
    // Additional coverage: xref entry invalid type marker (line 604, 607)
    // ========================================================================

    #[test]
    fn test_xref_entry_type_marker_z() {
        // 'z' instead of 'n' or 'f' triggers the error branch
        let body = b"";
        let mut xref = Vec::new();
        xref.extend_from_slice(b"xref\n0 2\n");
        xref.extend_from_slice(b"0000000000 65535 f \r\n");
        xref.extend_from_slice(b"0000000100 00000 z \r\n");
        xref.extend_from_slice(b"trailer\n<< /Size 2 >>\n");

        let data = make_simple_pdf(body, &xref);
        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();
        // Obj 0 should be present, obj 1 should be skipped (malformed)
        assert!(result.xref.get(0).is_some());
        assert!(result.xref.get(1).is_none());
        let warnings = diag.warnings();
        assert!(warnings.iter().any(|w| w.message.contains("malformed")));
    }

    // ========================================================================
    // Additional coverage: xref stream bad tokens (lines 706, 712-713, 720-721, 728-729)
    // ========================================================================

    #[test]
    fn test_xref_stream_eof_for_obj_num() {
        // startxref points to empty data
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        // Put startxref right at the spot where the file has just whitespace
        let offset = data.len();
        data.extend_from_slice(b"   "); // whitespace only, lexer returns Eof
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        assert!(parser.parse().is_err());
    }

    // ========================================================================
    // Additional coverage: xref stream object is not a stream (line 742-743)
    // ========================================================================

    // Already tested in test_xref_stream_not_a_stream_object above.

    // ========================================================================
    // Additional coverage: xref stream /Type wrong value (line 757, 759)
    // ========================================================================

    // Already tested in test_xref_stream_wrong_type_value above.

    // ========================================================================
    // Additional coverage: xref stream /Size negative and too large
    // (lines 778, 784, 786)
    // ========================================================================

    // Already tested above.

    // ========================================================================
    // Additional coverage: xref stream /W element bounds (lines 808, 817-818)
    // ========================================================================

    // Already tested above.

    // ========================================================================
    // Additional coverage: xref stream /Index negative values (line 840)
    // ========================================================================

    // Already tested above.

    // ========================================================================
    // Additional coverage: xref stream data extends beyond file (lines 861, 863)
    // ========================================================================

    // Already tested in test_xref_stream_data_extends_beyond_file above.

    // ========================================================================
    // Additional coverage: xref stream free entry gen overflow in stream
    // (lines 955, 958, and compressed stream_obj/index overflow)
    // ========================================================================

    #[test]
    fn test_xref_stream_free_entry_gen_overflow_in_stream() {
        // Free entry where gen field exceeds u16 (via /W [1 2 4])
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_offset = data.len();
        // /W [1 2 4]: type=1, next_free=2bytes, gen=4bytes
        // Free entry with gen=0x00010000 (exceeds u16::MAX)
        let stream_data: Vec<u8> = vec![
            0, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, // type 0, next_free=0, gen=65536
        ];
        let dict_str = format!(
            "<< /Type /XRef /Size 2 /W [1 2 4] /Index [0 1] /Length {} /Root 1 0 R >>",
            stream_data.len()
        );
        data.extend_from_slice(format!("2 0 obj\n{dict_str}\nstream\n").as_bytes());
        data.extend_from_slice(&stream_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();
        match result.xref.get(0) {
            Some(XrefEntry::Free { gen, .. }) => {
                assert_eq!(*gen, 0, "gen should be clamped to 0");
            }
            other => panic!("expected free entry, got {other:?}"),
        }
        let warnings = diag.warnings();
        assert!(warnings.iter().any(|w| w.message.contains("exceeds u16")));
    }

    #[test]
    fn test_xref_stream_compressed_index_overflow() {
        // Compressed entry with index field exceeding u32 via /W [1 2 4]
        // Width 4 max is 0xFFFFFFFF which fits u32, but let's test the path
        // with /W [1 4 4] where we set stream_obj to a value that fits u32.
        // To actually trigger the overflow we'd need field > u32::MAX,
        // which can't happen with width <= 4. This test just exercises the
        // compressed entry parsing path with larger field widths.
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_offset = data.len();
        // /W [1 4 4]: type=1byte, field1=4bytes, field2=4bytes = 9 bytes/entry
        let stream_data: Vec<u8> = vec![
            0, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF,
            0xFF, // obj 0: free, next=0, gen=u32::MAX
            2, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00,
            0x03, // obj 1: compressed, stream=5, idx=3
        ];
        let dict_str = format!(
            "<< /Type /XRef /Size 3 /W [1 4 4] /Index [0 2] /Length {} /Root 1 0 R >>",
            stream_data.len()
        );
        data.extend_from_slice(format!("2 0 obj\n{dict_str}\nstream\n").as_bytes());
        data.extend_from_slice(&stream_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();

        match result.xref.get(0) {
            Some(XrefEntry::Free { gen, .. }) => {
                // gen = u32::MAX = 4294967295 exceeds u16::MAX, should clamp to 0
                assert_eq!(*gen, 0);
            }
            other => panic!("expected free entry, got {other:?}"),
        }
        match result.xref.get(1) {
            Some(XrefEntry::Compressed { stream_obj, index }) => {
                assert_eq!(*stream_obj, 5);
                assert_eq!(*index, 3);
            }
            other => panic!("expected compressed entry, got {other:?}"),
        }
    }

    // ========================================================================
    // Additional coverage: xref entry line ending variations (lines 615-626)
    // ========================================================================

    #[test]
    fn test_xref_entry_no_trailing_space_lf_only() {
        // Entry with type marker immediately followed by LF (no space, no CR)
        let body = b"";
        let mut xref = Vec::new();
        xref.extend_from_slice(b"xref\n0 2\n");
        xref.extend_from_slice(b"0000000000 65535 f\n"); // 19 bytes (no space after f)
        xref.extend_from_slice(b"0000000100 00000 n\n"); // 19 bytes
        xref.extend_from_slice(b"trailer\n<< /Size 2 >>\n");

        let data = make_simple_pdf(body, &xref);
        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();
        assert_eq!(result.xref.len(), 2);
    }

    // ========================================================================
    // Additional coverage: xref stream with clamped data (lines 893-894, 896-899)
    // ========================================================================

    // Already tested in test_xref_stream_data_shorter_than_declared above.

    // ========================================================================
    // Additional coverage: parse_xref_entry gen field non-parseable (lines 568-571)
    // ========================================================================

    #[test]
    fn test_xref_entry_non_numeric_gen() {
        // Gen field contains non-numeric characters that are still ASCII
        let body = b"";
        let mut xref = Vec::new();
        xref.extend_from_slice(b"xref\n0 1\n");
        xref.extend_from_slice(b"0000000000 XXXXX f \r\n");
        xref.extend_from_slice(b"trailer\n<< /Size 1 >>\n");

        let data = make_simple_pdf(body, &xref);
        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let _ = parser.parse();
        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.message.contains("malformed")),
            "expected malformed warning for non-numeric gen"
        );
    }

    // ========================================================================
    // Additional coverage: parse_ascii_integer with positive sign then EOF
    // ========================================================================

    #[test]
    fn test_parse_ascii_integer_plus_sign_then_eof() {
        let data = b"+";
        let parser = DocumentParser::new(data);
        let err = parser.parse_ascii_integer(0).unwrap_err();
        assert!(
            err.to_string().contains("end of file"),
            "expected end of file error, got: {err}"
        );
    }

    // ========================================================================
    // Additional coverage: xref stream entry_remaining loop (lines 914-918)
    // ========================================================================

    #[test]
    fn test_xref_stream_entries_remaining_exhausted() {
        // Two /Index subsections where total entries < sum of counts.
        // The clamping should exhaust entries_remaining mid-subsection.
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        let obj_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_offset = data.len();
        let obj_off = obj_offset as u16;
        // /W [1 2 1] = 4 bytes per entry
        // /Index [0 3 10 3]: wants 6 entries total
        // But only provide 4 entries worth of data (16 bytes)
        let stream_data: Vec<u8> = vec![
            0,
            0x00,
            0x00,
            0xFF, // obj 0: free
            1,
            (obj_off >> 8) as u8,
            obj_off as u8,
            0x00, // obj 1: uncompressed
            1,
            0x00,
            0x50,
            0x00, // obj 2: uncompressed
            1,
            0x01,
            0x00,
            0x00, // obj 10: uncompressed
        ];
        let dict_str = format!(
            "<< /Type /XRef /Size 20 /W [1 2 1] /Index [0 3 10 3] /Length {} /Root 1 0 R >>",
            stream_data.len()
        );
        data.extend_from_slice(format!("2 0 obj\n{dict_str}\nstream\n").as_bytes());
        data.extend_from_slice(&stream_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();

        // Should have entries for obj 0, 1, 2, 10 (only 4 from the data)
        assert!(result.xref.get(0).is_some());
        assert!(result.xref.get(1).is_some());
        assert!(result.xref.get(2).is_some());
        assert!(result.xref.get(10).is_some());
        // obj 11 and 12 should not exist (not enough data)
        assert!(result.xref.get(11).is_none());
        assert!(result.xref.get(12).is_none());

        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.message.contains("clamping")),
            "expected clamping warning"
        );
    }

    // ========================================================================
    // Additional coverage: xref stream free entry next_free overflow
    // via large field value (lines 943-950)
    // ========================================================================

    // The free entry next_free overflow path requires field1 > u32::MAX,
    // which is impossible with max width 4. But we can verify the gen
    // overflow path for free entries (already tested above).

    // ========================================================================
    // Additional coverage: xref entry offset field parse error (line 547)
    // ========================================================================

    #[test]
    fn test_xref_entry_non_numeric_offset() {
        let body = b"";
        let mut xref = Vec::new();
        xref.extend_from_slice(b"xref\n0 1\n");
        // Offset field contains "abcdefghij" (ASCII but non-digits)
        xref.extend_from_slice(b"abcdefghij 65535 f \r\n");
        xref.extend_from_slice(b"trailer\n<< /Size 1 >>\n");

        let data = make_simple_pdf(body, &xref);
        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let _ = parser.parse();
        let warnings = diag.warnings();
        assert!(warnings.iter().any(|w| w.message.contains("malformed")));
    }

    // ========================================================================
    // Additional coverage: xref subsection count of zero
    // ========================================================================

    #[test]
    fn test_xref_subsection_count_zero() {
        // Subsection with count 0 means no entries to parse
        let body = b"";
        let xref = b"xref\n0 0\ntrailer\n<< /Size 0 >>\n";
        let data = make_simple_pdf(body, xref);
        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();
        assert_eq!(result.xref.len(), 0);
    }

    // ========================================================================
    // Additional coverage: parse full document with xref stream that has
    // multiple types (covers type 0, 1, 2 entries in one stream)
    // ========================================================================

    #[test]
    fn test_xref_stream_all_entry_types() {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        let obj_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_offset = data.len();
        let obj_off = obj_offset as u16;
        // /W [1 2 2]: type=1, field1=2, field2=2 = 5 bytes per entry
        let stream_data: Vec<u8> = vec![
            0,
            0x00,
            0x00,
            0xFF,
            0xFF, // obj 0: free, next=0, gen=65535
            1,
            (obj_off >> 8) as u8,
            obj_off as u8,
            0x00,
            0x00, // obj 1: uncompressed
            2,
            0x00,
            0x01,
            0x00,
            0x02, // obj 2: compressed, stream=1, index=2
        ];
        let dict_str = format!(
            "<< /Type /XRef /Size 4 /W [1 2 2] /Index [0 3] /Length {} /Root 1 0 R >>",
            stream_data.len()
        );
        data.extend_from_slice(format!("3 0 obj\n{dict_str}\nstream\n").as_bytes());
        data.extend_from_slice(&stream_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();

        match result.xref.get(0) {
            Some(XrefEntry::Free { next_free, gen }) => {
                assert_eq!(*next_free, 0);
                assert_eq!(*gen, 65535);
            }
            other => panic!("expected free entry for obj 0, got {other:?}"),
        }
        match result.xref.get(1) {
            Some(XrefEntry::Uncompressed { gen, .. }) => {
                assert_eq!(*gen, 0);
            }
            other => panic!("expected uncompressed for obj 1, got {other:?}"),
        }
        match result.xref.get(2) {
            Some(XrefEntry::Compressed { stream_obj, index }) => {
                assert_eq!(*stream_obj, 1);
                assert_eq!(*index, 2);
            }
            other => panic!("expected compressed for obj 2, got {other:?}"),
        }
    }

    // ========================================================================
    // Additional coverage: parse_dictionary_at with parse error (line 686)
    // ========================================================================

    #[test]
    fn test_parse_dictionary_at_array_instead() {
        // Passing an array where a dictionary is expected
        let data = b"[1 2 3]";
        let parser = DocumentParser::new(data);
        let err = parser.parse_dictionary_at(0).unwrap_err();
        assert!(err.to_string().contains("dictionary"));
    }

    // ========================================================================
    // Additional coverage: xref stream /W row_width (line 814-818)
    // ========================================================================

    // Already tested by test_xref_stream_w_all_zero

    // ========================================================================
    // Additional coverage: xref stream /Index count as second element (line 836)
    // ========================================================================

    #[test]
    fn test_xref_stream_index_count_not_integer() {
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");
        let xref_offset = data.len();
        data.extend_from_slice(
            b"1 0 obj\n<< /Type /XRef /Size 1 /W [1 2 1] /Index [0 /Name] /Length 4 >>\nstream\n\x01\x00\x00\x00\nendstream\nendobj\n",
        );
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let err = parser.parse().unwrap_err();
        assert!(
            err.to_string().contains("integer"),
            "expected integer error for /Index count, got: {err}"
        );
    }

    // ========================================================================
    // Multi-startxref fallback tests (T1-XREF)
    // ========================================================================

    #[test]
    fn test_multi_startxref_fallback_corrupt_primary() {
        // Build a PDF where the LAST startxref (primary) points to garbage,
        // but an earlier startxref points to a valid xref section.
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");

        let obj1_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_offset = data.len();
        let xref = format!(
            "xref\n\
             0 2\n\
             0000000000 65535 f \r\n\
             {obj1_offset:010} 00000 n \r\n\
             trailer\n\
             << /Size 2 /Root 1 0 R >>\n"
        );
        data.extend_from_slice(xref.as_bytes());

        // First (valid) startxref
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        // Append a second (corrupt) startxref that points to garbage
        let garbage_offset = 5; // points into "%PDF-1.4\n"
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(garbage_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();

        // Should have recovered using the first (alternate) startxref
        assert_eq!(result.xref.len(), 2);
        assert!(result.trailer.get(b"Root").is_some());

        // Should have emitted an info diagnostic about fallback
        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("alternate startxref")),
            "expected alternate startxref info, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_find_all_startxref_offsets_multiple() {
        // Build data with three startxref keywords
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");

        let offset_a = data.len();
        data.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \r\ntrailer\n<< /Size 1 >>\n");

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(offset_a.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        // Second startxref pointing to offset 5 (invalid xref but valid offset)
        data.extend_from_slice(b"startxref\n5\n%%EOF\n");

        // Third startxref pointing to offset_a (valid)
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(offset_a.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let offsets = parser.find_all_startxref_offsets();

        // Should have at least 2 candidates (last first)
        assert!(
            offsets.len() >= 2,
            "expected at least 2 candidates, got: {offsets:?}"
        );
        // Last occurrence should be first in the returned vec
        assert_eq!(offsets[0], offset_a as u64);
    }

    // ========================================================================
    // Repair mode tests (T1-XREF)
    // ========================================================================

    #[test]
    fn test_repair_mode_no_valid_startxref() {
        // Build a PDF with objects but completely broken startxref.
        // The repair scanner should find the objects and build a synthetic xref.
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");

        let obj1_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let obj2_offset = data.len();
        data.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");

        // startxref pointing to garbage (beyond file)
        data.extend_from_slice(b"startxref\n99\n%%EOF\n");

        let _ = obj1_offset;
        let _ = obj2_offset;

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();

        // Repair mode should have found both objects
        assert!(
            result.xref.len() >= 2,
            "expected at least 2 entries from repair, got {}",
            result.xref.len()
        );

        // Object 1 should be in the xref
        assert!(
            result.xref.get(1).is_some(),
            "repair mode should find object 1"
        );
        assert!(
            result.xref.get(2).is_some(),
            "repair mode should find object 2"
        );

        // The trailer should have /Root pointing to the Catalog
        assert!(
            result.trailer.get(b"Root").is_some(),
            "repair mode should find /Root in trailer"
        );

        // Should have emitted a warning about repair mode
        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.message.contains("repair scan")),
            "expected repair scan warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_repair_mode_with_trailer() {
        // A file with broken xref but a valid trailer keyword + dictionary.
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");

        let obj1_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let _ = obj1_offset;

        // Traditional xref/trailer that won't be reachable via startxref
        data.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \r\n");
        data.extend_from_slice(b"trailer\n<< /Size 2 /Root 1 0 R /Info 3 0 R >>\n");

        // Broken startxref
        data.extend_from_slice(b"startxref\n99\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();

        // Should use the trailer dictionary found during repair scan
        assert!(result.trailer.get(b"Root").is_some());
        // The /Info key should be present (proves we used the actual trailer,
        // not a synthesized one)
        assert!(
            result.trailer.get(b"Info").is_some(),
            "expected /Info from scanned trailer"
        );
    }

    #[test]
    fn test_repair_mode_diagnostics() {
        // Verify repair mode emits Warning level (not just Info)
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        data.extend_from_slice(b"startxref\n99\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let _result = parser.parse().unwrap();

        let warnings = diag.warnings();
        let repair_warning = warnings.iter().find(|w| w.message.contains("repair scan"));
        assert!(repair_warning.is_some(), "expected repair scan warning");
        assert_eq!(
            repair_warning.map(|w| w.level),
            Some(crate::diagnostics::WarningLevel::Warning),
            "repair mode should emit Warning level diagnostic"
        );
    }

    // ========================================================================
    // /XRefStm tests (T1-XREF)
    // ========================================================================

    #[test]
    fn test_xref_stm_in_trailer() {
        // Build a PDF with a traditional xref table whose trailer has /XRefStm
        // pointing to an xref stream with additional compressed entries.
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        // Object 1: Catalog
        let obj1_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        // Object 3: xref stream containing a compressed entry for object 2
        // (object 2 is "in" object stream 10 at index 5)
        let xref_stm_offset = data.len();

        // /W [1 2 2], 1 entry (obj 2: compressed in stream 10, index 5)
        let stm_data: Vec<u8> = vec![
            2, 0x00, 0x0A, 0x00, 0x05, // obj 2: compressed, stream=10, index=5
        ];
        let stm_dict = format!(
            "<< /Type /XRef /Size 3 /W [1 2 2] /Index [2 1] /Length {} >>",
            stm_data.len()
        );
        data.extend_from_slice(format!("3 0 obj\n{stm_dict}\nstream\n").as_bytes());
        data.extend_from_slice(&stm_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        // Traditional xref table with /XRefStm pointing to the stream
        let xref_offset = data.len();
        let xref = format!(
            "xref\n\
             0 2\n\
             0000000000 65535 f \r\n\
             {obj1_offset:010} 00000 n \r\n\
             trailer\n\
             << /Size 4 /Root 1 0 R /XRefStm {xref_stm_offset} >>\n"
        );
        data.extend_from_slice(xref.as_bytes());

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();

        // Traditional table: obj 0 (free) + obj 1 (uncompressed)
        assert!(result.xref.get(0).is_some(), "obj 0 should be present");
        assert!(result.xref.get(1).is_some(), "obj 1 should be present");

        // From /XRefStm: obj 2 should be compressed
        match result.xref.get(2) {
            Some(XrefEntry::Compressed { stream_obj, index }) => {
                assert_eq!(*stream_obj, 10);
                assert_eq!(*index, 5);
            }
            other => panic!("expected compressed entry for obj 2, got {other:?}"),
        }
    }

    #[test]
    fn test_xref_stm_does_not_override_traditional_entries() {
        // The traditional table defines obj 1 at offset X.
        // The xref stream also defines obj 1 at a different offset.
        // Per ISO 32000-1 7.5.8.4, traditional entries take priority.
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        let obj1_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        // Xref stream that also claims obj 1 is at offset 9999
        let xref_stm_offset = data.len();
        let obj1_off_hi = (9999u16 >> 8) as u8;
        let obj1_off_lo = (9999u16 & 0xFF) as u8;
        let stm_data: Vec<u8> = vec![
            1,
            obj1_off_hi,
            obj1_off_lo,
            0x00,
            0x00, // obj 1: uncompressed at 9999
        ];
        let stm_dict = format!(
            "<< /Type /XRef /Size 2 /W [1 2 2] /Index [1 1] /Length {} >>",
            stm_data.len()
        );
        data.extend_from_slice(format!("2 0 obj\n{stm_dict}\nstream\n").as_bytes());
        data.extend_from_slice(&stm_data);
        data.extend_from_slice(b"\nendstream\nendobj\n");

        // Traditional xref table with obj 1 at the real offset
        let xref_offset = data.len();
        let xref = format!(
            "xref\n\
             0 2\n\
             0000000000 65535 f \r\n\
             {obj1_offset:010} 00000 n \r\n\
             trailer\n\
             << /Size 3 /Root 1 0 R /XRefStm {xref_stm_offset} >>\n"
        );
        data.extend_from_slice(xref.as_bytes());

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let parser = DocumentParser::new(&data);
        let result = parser.parse().unwrap();

        // Obj 1 should have the offset from the traditional table, NOT 9999
        match result.xref.get(1) {
            Some(XrefEntry::Uncompressed { offset, .. }) => {
                assert_eq!(
                    *offset, obj1_offset as u64,
                    "traditional table entry should take priority over /XRefStm"
                );
            }
            other => panic!("expected uncompressed entry for obj 1, got {other:?}"),
        }
    }

    #[test]
    fn test_xref_stm_invalid_offset_warns() {
        // /XRefStm points to an invalid offset. Should warn, not fail.
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        let obj1_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_offset = data.len();
        let xref = format!(
            "xref\n\
             0 2\n\
             0000000000 65535 f \r\n\
             {obj1_offset:010} 00000 n \r\n\
             trailer\n\
             << /Size 2 /Root 1 0 R /XRefStm -5 >>\n"
        );
        data.extend_from_slice(xref.as_bytes());

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();

        // Should still parse successfully (traditional table works fine)
        assert_eq!(result.xref.len(), 2);

        // Should have warned about invalid /XRefStm
        let warnings = diag.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("/XRefStm") && w.message.contains("invalid")),
            "expected /XRefStm invalid warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_xref_stm_parse_failure_warns() {
        // /XRefStm points to a valid offset but the content there isn't
        // a valid xref stream. Should warn, not fail.
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.5\n");

        let obj1_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        // Put a non-xref-stream object where /XRefStm points
        let stm_offset = data.len();
        data.extend_from_slice(b"2 0 obj\n<< /Type /Page >>\nendobj\n");

        let xref_offset = data.len();
        let xref = format!(
            "xref\n\
             0 2\n\
             0000000000 65535 f \r\n\
             {obj1_offset:010} 00000 n \r\n\
             trailer\n\
             << /Size 3 /Root 1 0 R /XRefStm {stm_offset} >>\n"
        );
        data.extend_from_slice(xref.as_bytes());

        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();

        // Traditional table entries should still be present
        assert_eq!(result.xref.len(), 2);

        // Should have warned about /XRefStm failure
        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.message.contains("/XRefStm")),
            "expected /XRefStm warning, got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_multi_startxref_info_diagnostic_level() {
        // When fallback to an alternate startxref succeeds, should emit Info
        let mut data = Vec::new();
        data.extend_from_slice(b"%PDF-1.4\n");

        let obj1_offset = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let xref_offset = data.len();
        let xref = format!(
            "xref\n\
             0 2\n\
             0000000000 65535 f \r\n\
             {obj1_offset:010} 00000 n \r\n\
             trailer\n\
             << /Size 2 /Root 1 0 R >>\n"
        );
        data.extend_from_slice(xref.as_bytes());

        // Valid startxref
        data.extend_from_slice(b"startxref\n");
        data.extend_from_slice(xref_offset.to_string().as_bytes());
        data.extend_from_slice(b"\n%%EOF\n");

        // Corrupt startxref (last, will be tried first)
        data.extend_from_slice(b"startxref\n5\n%%EOF\n");

        let diag = Arc::new(CollectingDiagnostics::new());
        let parser = DocumentParser::with_diagnostics(&data, diag.clone());
        let result = parser.parse().unwrap();
        assert!(result.xref.len() >= 2);

        // The fallback info should be at Info level
        let warnings = diag.warnings();
        let fallback_info = warnings
            .iter()
            .find(|w| w.message.contains("alternate startxref"));
        assert!(fallback_info.is_some(), "expected alternate startxref info");
        assert_eq!(
            fallback_info.map(|w| w.level),
            Some(crate::diagnostics::WarningLevel::Info),
            "multi-startxref fallback should emit Info level"
        );
    }
}
