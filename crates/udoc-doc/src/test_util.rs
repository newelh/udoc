//! Shared test helpers for building synthetic DOC binary structures.
//!
//! Available when the `test-internals` feature is enabled (dev-dependencies
//! enable it by default). Used by unit tests, golden file tests, malformed
//! recovery tests, and facade integration tests.

use crate::fib;

/// Build a minimal valid FIB binary blob.
///
/// Creates a FIB with the specified parameters. Other ccp fields default to 0.
/// The table stream bit controls whether "0Table" or "1Table" is selected.
pub fn build_fib(ccp_text: u32, fc_clx: u32, lcb_clx: u32, table_stream_bit: bool) -> Vec<u8> {
    let mut buf = Vec::new();

    // FibBase (32 bytes at offset 0x00)
    buf.extend_from_slice(&0xA5ECu16.to_le_bytes()); // 0x00: wIdent
    buf.extend_from_slice(&0x00C1u16.to_le_bytes()); // 0x02: nFib (Word 97)
    buf.extend_from_slice(&0u16.to_le_bytes()); // 0x04: unused
    buf.extend_from_slice(&0u16.to_le_bytes()); // 0x06: unused
    buf.extend_from_slice(&0u16.to_le_bytes()); // 0x08: unused

    // 0x0A: flags -- bit 9 = fWhichTblStm
    let flags: u16 = if table_stream_bit { 1 << 9 } else { 0 };
    buf.extend_from_slice(&flags.to_le_bytes());

    // Pad rest of FibBase to 32 bytes
    buf.resize(fib::FIB_BASE_SIZE, 0);

    // csw (u16) at offset 0x20: 7 words in FibRgW97
    buf.extend_from_slice(&7u16.to_le_bytes());

    // FibRgW97: 7 * 2 = 14 bytes of zeros
    buf.extend_from_slice(&[0u8; 14]);

    // cslw (u16): 22 dwords in FibRgLw97
    buf.extend_from_slice(&22u16.to_le_bytes());

    // FibRgLw97: 22 * 4 = 88 bytes
    let mut rglw = [0u8; 88];
    rglw[3 * 4..3 * 4 + 4].copy_from_slice(&ccp_text.to_le_bytes());
    buf.extend_from_slice(&rglw);

    // cbRgFcLcb (u16): 74 pairs
    buf.extend_from_slice(&74u16.to_le_bytes());

    // FibRgFcLcb: 74 * 8 = 592 bytes
    let mut fc_lcb = vec![0u8; 74 * 8];
    // Pair 66: Clx
    fc_lcb[66 * 8..66 * 8 + 4].copy_from_slice(&fc_clx.to_le_bytes());
    fc_lcb[66 * 8 + 4..66 * 8 + 8].copy_from_slice(&lcb_clx.to_le_bytes());
    buf.extend_from_slice(&fc_lcb);

    buf
}

/// Build CLX data from piece descriptions.
///
/// Each tuple is `(cp_start, cp_end, text_data, is_compressed)`.
/// The text_data is the raw bytes in the WordDocument stream that this piece
/// references. The byte_offset for each piece is computed assuming pieces
/// are stored sequentially starting at offset 0 in the WordDocument stream.
pub fn build_clx(pieces: &[(u32, u32, &[u8], bool)]) -> Vec<u8> {
    // Compute sequential byte offsets
    let mut offset = 0u32;
    let mut entries: Vec<(u32, u32, u32, bool)> = Vec::new();
    for &(cp_start, cp_end, data, compressed) in pieces {
        entries.push((cp_start, cp_end, offset, compressed));
        offset += data.len() as u32;
    }
    build_clx_with_offsets(&entries)
}

/// Build CLX data from piece descriptions with explicit byte offsets.
///
/// Each tuple is `(cp_start, cp_end, byte_offset, is_compressed)`.
pub fn build_clx_with_offsets(pieces: &[(u32, u32, u32, bool)]) -> Vec<u8> {
    let n = pieces.len();

    // PlcPcd: (n+1) CPs + n PCDs
    let plc_pcd_size = (n + 1) * 4 + n * 8;

    let mut plc_pcd = Vec::with_capacity(plc_pcd_size);

    // Write CPs
    if n == 0 {
        // Single CP boundary at 0
        plc_pcd.extend_from_slice(&0u32.to_le_bytes());
    } else {
        for (i, &(cp_start, cp_end, _, _)) in pieces.iter().enumerate() {
            if i == 0 {
                plc_pcd.extend_from_slice(&cp_start.to_le_bytes());
            }
            plc_pcd.extend_from_slice(&cp_end.to_le_bytes());
        }
    }

    // Write PCDs (8 bytes each)
    for &(_, _, byte_offset, is_compressed) in pieces {
        // Bytes 0-1: igrpprl (unused)
        plc_pcd.extend_from_slice(&0u16.to_le_bytes());

        // Bytes 2-5: fc with encoding flag
        let fc = if is_compressed {
            // Compressed: bit 30 set, store byte_offset * 2
            (byte_offset * 2) | (1 << 30)
        } else {
            // Uncompressed: bit 30 clear, store byte_offset directly
            byte_offset
        };
        plc_pcd.extend_from_slice(&fc.to_le_bytes());

        // Bytes 6-7: prm (unused)
        plc_pcd.extend_from_slice(&0u16.to_le_bytes());
    }

    // Build CLX: Pcdt marker (0x02) + lcbPlcPcd (u32) + PlcPcd data
    let mut clx = Vec::new();
    clx.push(0x02); // Pcdt marker
    clx.extend_from_slice(&(plc_pcd.len() as u32).to_le_bytes());
    clx.extend_from_slice(&plc_pcd);

    clx
}

/// Build a minimal FIB with footnote and endnote character counts set.
///
/// Extends `build_fib` by also populating ccp_ftn and ccp_edn in FibRgLw97.
pub fn build_fib_with_stories(
    ccp_text: u32,
    ccp_ftn: u32,
    ccp_edn: u32,
    fc_clx: u32,
    lcb_clx: u32,
) -> Vec<u8> {
    build_fib_with_all_stories(ccp_text, ccp_ftn, 0, ccp_edn, fc_clx, lcb_clx)
}

/// Build a minimal FIB with footnote, header/footer, and endnote character counts.
///
/// Extends `build_fib` by populating ccp_ftn, ccp_hdd, and ccp_edn in FibRgLw97.
pub fn build_fib_with_all_stories(
    ccp_text: u32,
    ccp_ftn: u32,
    ccp_hdd: u32,
    ccp_edn: u32,
    fc_clx: u32,
    lcb_clx: u32,
) -> Vec<u8> {
    let mut buf = build_fib(ccp_text, fc_clx, lcb_clx, false);

    // FibRgLw97 base = 0x20 (csw) + 2 + 7*2 (FibRgW97) + 2 (cslw) = 0x32
    let rglw_base = 0x22 + 14 + 2;

    // ccpFtn = index 4, ccpHdd = index 5, ccpEdn = index 11
    buf[rglw_base + 4 * 4..rglw_base + 4 * 4 + 4].copy_from_slice(&ccp_ftn.to_le_bytes());
    buf[rglw_base + 5 * 4..rglw_base + 5 * 4 + 4].copy_from_slice(&ccp_hdd.to_le_bytes());
    buf[rglw_base + 11 * 4..rglw_base + 11 * 4 + 4].copy_from_slice(&ccp_edn.to_le_bytes());

    buf
}

/// Build a complete minimal DOC file with body text, footnotes, and endnotes.
///
/// All three text regions are stored as a single contiguous compressed piece
/// covering the full CP space [0, ccp_text + ccp_ftn + ccp_edn).
pub fn build_minimal_doc_with_notes(body: &str, footnotes: &str, endnotes: &str) -> Vec<u8> {
    build_minimal_doc_with_all_stories(body, footnotes, "", endnotes)
}

/// Build a complete minimal DOC file with body, footnotes, headers/footers, and endnotes.
///
/// All text regions are stored as a single contiguous compressed piece
/// covering the full CP space. Story layout in the piece table follows
/// MS-DOC ordering: body, footnotes, headers/footers, (annotations skipped), endnotes.
pub fn build_minimal_doc_with_all_stories(
    body: &str,
    footnotes: &str,
    headers_footers: &str,
    endnotes: &str,
) -> Vec<u8> {
    let body_bytes = body.as_bytes();
    let ftn_bytes = footnotes.as_bytes();
    let hdd_bytes = headers_footers.as_bytes();
    let edn_bytes = endnotes.as_bytes();

    let ccp_text = body_bytes.len() as u32;
    let ccp_ftn = ftn_bytes.len() as u32;
    let ccp_hdd = hdd_bytes.len() as u32;
    let ccp_edn = edn_bytes.len() as u32;
    let total_cps = ccp_text + ccp_ftn + ccp_hdd + ccp_edn;

    // Build a placeholder FIB to measure its size
    let placeholder_fib = build_fib_with_all_stories(ccp_text, ccp_ftn, ccp_hdd, ccp_edn, 0, 0);
    let fib_size = placeholder_fib.len();
    let text_offset = fib_size as u32;

    // Build the CLX: single piece covering all CPs
    let clx = build_clx_with_offsets(&[(0, total_cps, text_offset, true)]);
    let fc_clx = 0u32;
    let lcb_clx = clx.len() as u32;

    let real_fib = build_fib_with_all_stories(ccp_text, ccp_ftn, ccp_hdd, ccp_edn, fc_clx, lcb_clx);

    // Assemble WordDocument stream: FIB + all text concatenated in story order.
    // MS-DOC story order: body, footnotes, headers/footers, annotations, endnotes.
    let mut word_doc = real_fib;
    word_doc.extend_from_slice(body_bytes);
    word_doc.extend_from_slice(ftn_bytes);
    word_doc.extend_from_slice(hdd_bytes);
    word_doc.extend_from_slice(edn_bytes);

    let table_stream = clx;

    udoc_containers::test_util::build_cfb(&[("WordDocument", &word_doc), ("0Table", &table_stream)])
}

/// Build a complete minimal DOC file (CFB container) with the given text.
///
/// The text is stored as a single compressed (CP1252) piece. The resulting
/// bytes can be parsed by `CfbArchive::new()` and contain:
/// - "WordDocument" stream with FIB + text data
/// - "0Table" (or "1Table") stream with CLX
pub fn build_minimal_doc(text: &str) -> Vec<u8> {
    let text_bytes = text.as_bytes();
    let ccp_text = text_bytes.len() as u32;

    // WordDocument stream layout:
    // [FIB header.] [text bytes at some offset]
    // We need to build the FIB first to know its size, then append text.

    // Build a placeholder FIB to measure its size
    let placeholder_fib = build_fib(ccp_text, 0, 0, false);
    let fib_size = placeholder_fib.len();

    // Text goes right after the FIB in the WordDocument stream
    let text_offset = fib_size as u32;

    // Build the CLX (piece table) for this text
    let clx = build_clx_with_offsets(&[(0, ccp_text, text_offset, true)]);
    let fc_clx = 0u32; // CLX at start of table stream
    let lcb_clx = clx.len() as u32;

    // Now build the real FIB with correct CLX offset/size
    let real_fib = build_fib(ccp_text, fc_clx, lcb_clx, false);

    // Assemble WordDocument stream
    let mut word_doc = real_fib;
    word_doc.extend_from_slice(text_bytes);

    // Table stream is just the CLX
    let table_stream = clx;

    // Wrap in CFB
    udoc_containers::test_util::build_cfb(&[("WordDocument", &word_doc), ("0Table", &table_stream)])
}

/// Build a minimal fast-save DOC file where the CLX is deliberately empty.
///
/// The text is stored as UTF-16LE directly after the FIB in the WordDocument
/// stream. The Table stream contains no CLX data (lcbClx = 0), so the
/// fast-save fallback path in `PieceTable::parse_with_fast_save_fallback`
/// is exercised.
pub fn build_minimal_fast_save_doc(text: &str) -> Vec<u8> {
    let ccp_text = text.len() as u32;

    // Encode text as UTF-16LE (fast-save uses UTF-16LE, not compressed CP1252).
    let utf16_bytes: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();

    // Build the FIB with lcbClx = 0 (no CLX data) and fc_clx = 0.
    let fib = build_fib(ccp_text, 0, 0, false);

    // WordDocument stream: FIB + UTF-16LE text.
    let mut word_doc = fib;
    word_doc.extend_from_slice(&utf16_bytes);

    // Table stream is empty (no CLX).
    let table_stream: Vec<u8> = Vec::new();

    udoc_containers::test_util::build_cfb(&[("WordDocument", &word_doc), ("0Table", &table_stream)])
}

/// FKP page size in bytes (always 512).
const FKP_PAGE_SIZE: usize = 512;

/// Build a minimal DOC file with body text where runs have bold/italic properties.
///
/// Constructs a complete CFB with a WordDocument stream containing:
/// - FIB with PlcfBteChpx pointing to a ChpxFkp page
/// - Text data
/// - ChpxFkp page at a 512-byte aligned offset
///
/// The `runs` slice describes character property spans: each entry is
/// `(run_char_len, bold, italic)`. The run_char_len values must sum to
/// the character count of `text`.
///
/// For example, `text = "Bold\rPlain"` with `runs = &[(4, true, false), (6, false, false)]`
/// encodes "Bold" as bold and "\rPlain" as plain.
///
/// Note: the `text` must use '\r' as the paragraph separator, and the
/// resulting DOC's body paragraphs will split on those paragraph marks.
pub fn build_minimal_doc_with_bold_italic(text: &str, runs: &[(u32, bool, bool)]) -> Vec<u8> {
    // Build a placeholder FIB to get its size.
    let ccp_text = text.len() as u32;
    let placeholder_fib = build_fib(ccp_text, 0, 0, false);
    let fib_size = placeholder_fib.len();

    // Text starts right after FIB in WordDocument stream.
    let text_offset = fib_size as u32;
    let text_bytes = text.as_bytes();

    // The ChpxFkp page must sit at a 512-byte-aligned offset in WordDocument.
    // Choose the first 512-aligned offset at or after (fib_size + text.len()).
    let after_text = fib_size + text_bytes.len();
    let fkp_page_offset = after_text.next_multiple_of(FKP_PAGE_SIZE);
    let fkp_page_number = (fkp_page_offset / FKP_PAGE_SIZE) as u32;

    // Build the ChpxFkp page.
    // For a compressed piece at text_offset, the FCs in the FKP are:
    //   fc = (byte_offset * 2) | 0x4000_0000
    let mut fkp_page = [0u8; FKP_PAGE_SIZE];
    let num_runs = runs.len();

    fkp_page[FKP_PAGE_SIZE - 1] = num_runs as u8; // crun at byte 511

    // Write FC boundaries: (num_runs + 1) u32 values.
    let mut cp_cursor = 0u32;
    for (i, &(run_len, _, _)) in runs.iter().enumerate() {
        let byte_off = text_offset + cp_cursor;
        let fc = (byte_off * 2) | 0x4000_0000;
        let off = i * 4;
        fkp_page[off..off + 4].copy_from_slice(&fc.to_le_bytes());
        cp_cursor += run_len;
    }
    // Final FC boundary
    {
        let byte_off = text_offset + cp_cursor;
        let fc = (byte_off * 2) | 0x4000_0000;
        let off = num_runs * 4;
        fkp_page[off..off + 4].copy_from_slice(&fc.to_le_bytes());
    }

    // BX entries for CHPX are 1 byte each.
    let bx_base = (num_runs + 1) * 4;
    let mut chpx_pos: usize = FKP_PAGE_SIZE - 2; // start just before crun byte

    for (i, &(_, bold, italic)) in runs.iter().enumerate() {
        // Build CHPX sprm bytes.
        let mut sprms: Vec<u8> = Vec::new();
        if bold {
            sprms.extend_from_slice(&0x0835u16.to_le_bytes()); // sprmCFBold
            sprms.push(0x01);
        }
        if italic {
            sprms.extend_from_slice(&0x0836u16.to_le_bytes()); // sprmCFItalic
            sprms.push(0x01);
        }

        // CHPX: cb (1 byte count of sprms) + sprms.
        let chpx_total = 1 + sprms.len();
        chpx_pos = chpx_pos.saturating_sub(chpx_total);
        // Align to even offset.
        if !chpx_pos.is_multiple_of(2) {
            chpx_pos = chpx_pos.saturating_sub(1);
        }

        fkp_page[chpx_pos] = sprms.len() as u8;
        if !sprms.is_empty() {
            fkp_page[chpx_pos + 1..chpx_pos + 1 + sprms.len()].copy_from_slice(&sprms);
        }

        // BX entry: offset / 2.
        fkp_page[bx_base + i] = (chpx_pos / 2) as u8;
    }

    // Build CLX: single compressed piece covering all CPs.
    let clx = build_clx_with_offsets(&[(0, ccp_text, text_offset, true)]);
    let fc_clx = 0u32;
    let lcb_clx = clx.len() as u32;

    // Build PlcfBteChpx: a PLC with a single BTE entry pointing to fkp_page_number.
    // Format: (n+1) FCs (u32) + n BTEs (u32 each).  n=1 here.
    let mut plcf_chpx: Vec<u8> = Vec::new();
    plcf_chpx.extend_from_slice(&0u32.to_le_bytes()); // FC[0]
    plcf_chpx.extend_from_slice(&1u32.to_le_bytes()); // FC[1] (dummy end)
    plcf_chpx.extend_from_slice(&fkp_page_number.to_le_bytes()); // BTE[0]

    // Table stream layout: CLX first, then PlcfBteChpx.
    let fc_chpx = lcb_clx; // starts right after CLX
    let lcb_chpx = plcf_chpx.len() as u32;

    // Build the real FIB with correct CLX and CHPX offsets.
    let mut fib = build_fib(ccp_text, fc_clx, lcb_clx, false);
    // Patch PlcfBteChpx into pair 73 of FibRgFcLcb (8 bytes per pair, 74 pairs total).
    let fc_lcb_base = fib.len() - 74 * 8;
    fib[fc_lcb_base + 73 * 8..fc_lcb_base + 73 * 8 + 4].copy_from_slice(&fc_chpx.to_le_bytes());
    fib[fc_lcb_base + 73 * 8 + 4..fc_lcb_base + 73 * 8 + 8]
        .copy_from_slice(&lcb_chpx.to_le_bytes());

    // Assemble WordDocument stream: FIB + text + padding + ChpxFkp page.
    let mut word_doc = fib;
    word_doc.extend_from_slice(text_bytes);
    // Pad to fkp_page_offset.
    word_doc.resize(fkp_page_offset, 0u8);
    word_doc.extend_from_slice(&fkp_page);

    // Table stream: CLX + PlcfBteChpx.
    let mut table_stream = clx;
    table_stream.extend_from_slice(&plcf_chpx);

    udoc_containers::test_util::build_cfb(&[("WordDocument", &word_doc), ("0Table", &table_stream)])
}

/// Build a SummaryInformation binary stream with the given properties.
///
/// Each property is (property_id, string_value). Use PIDSI constants from
/// `summary_info` (0x0002 = title, 0x0003 = subject, 0x0004 = author).
pub fn build_summary_info_stream(props: &[(u32, &str)]) -> Vec<u8> {
    const VT_LPSTR: u32 = 0x001E;

    let mut buf = Vec::new();

    // Property Set Header (28 bytes)
    buf.extend_from_slice(&0xFFFEu16.to_le_bytes()); // byte_order
    buf.extend_from_slice(&0x0000u16.to_le_bytes()); // version
    buf.extend_from_slice(&0u32.to_le_bytes()); // os_version
    buf.extend_from_slice(&[0u8; 16]); // class_id
    buf.extend_from_slice(&1u32.to_le_bytes()); // num_property_sets

    // Property Set Entry (20 bytes): FMTID + offset
    buf.extend_from_slice(&[0u8; 16]); // FMTID
    let set_offset = 48u32;
    buf.extend_from_slice(&set_offset.to_le_bytes());

    let num_props = props.len() as u32;
    let pairs_size = (props.len() * 8) as u32;
    let values_base = 8 + pairs_size;

    let mut values = Vec::new();
    let mut prop_offsets: Vec<u32> = Vec::new();
    for &(_, text) in props {
        prop_offsets.push(values_base + values.len() as u32);
        values.extend_from_slice(&VT_LPSTR.to_le_bytes());
        let byte_count = (text.len() + 1) as u32;
        values.extend_from_slice(&byte_count.to_le_bytes());
        values.extend_from_slice(text.as_bytes());
        values.push(0);
        while values.len() % 4 != 0 {
            values.push(0);
        }
    }

    let set_size = 8 + pairs_size as usize + values.len();

    buf.extend_from_slice(&(set_size as u32).to_le_bytes());
    buf.extend_from_slice(&num_props.to_le_bytes());

    for (i, &(id, _)) in props.iter().enumerate() {
        buf.extend_from_slice(&id.to_le_bytes());
        buf.extend_from_slice(&prop_offsets[i].to_le_bytes());
    }

    buf.extend_from_slice(&values);

    buf
}

/// Build a minimal DOC file with body text and SummaryInformation metadata.
///
/// The `metadata` slice contains (property_id, value) pairs for the
/// SummaryInformation stream.
pub fn build_minimal_doc_with_metadata(text: &str, metadata: &[(u32, &str)]) -> Vec<u8> {
    let text_bytes = text.as_bytes();
    let ccp_text = text_bytes.len() as u32;

    let placeholder_fib = build_fib(ccp_text, 0, 0, false);
    let fib_size = placeholder_fib.len();
    let text_offset = fib_size as u32;

    let clx = build_clx_with_offsets(&[(0, ccp_text, text_offset, true)]);
    let fc_clx = 0u32;
    let lcb_clx = clx.len() as u32;

    let real_fib = build_fib(ccp_text, fc_clx, lcb_clx, false);

    let mut word_doc = real_fib;
    word_doc.extend_from_slice(text_bytes);

    let table_stream = clx;
    let si_stream = build_summary_info_stream(metadata);

    udoc_containers::test_util::build_cfb(&[
        ("WordDocument", &word_doc),
        ("0Table", &table_stream),
        ("\x05SummaryInformation", &si_stream),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_fib_roundtrips() {
        let data = build_fib(42, 0x100, 0x200, true);
        let parsed = fib::parse_fib(&data).unwrap();
        assert_eq!(parsed.ccp_text, 42);
        assert_eq!(parsed.fc_clx, 0x100);
        assert_eq!(parsed.lcb_clx, 0x200);
        assert_eq!(parsed.table_stream_name, "1Table");
    }

    #[test]
    fn build_clx_single_piece() {
        let text = b"Test";
        let clx = build_clx(&[(0, 4, text, true)]);
        // Should be parseable
        let pt = crate::piece_table::PieceTable::parse(&clx, 0, clx.len() as u32).unwrap();
        let result = pt.assemble_text(text, 0, 4).unwrap();
        assert_eq!(result, "Test");
    }

    #[test]
    fn build_minimal_doc_valid_cfb() {
        use std::sync::Arc;
        use udoc_containers::cfb::CfbArchive;
        use udoc_core::diagnostics::NullDiagnostics;

        let doc_bytes = build_minimal_doc("Hello DOC");
        let diag = Arc::new(NullDiagnostics);
        let archive = CfbArchive::new(&doc_bytes, diag).unwrap();

        // Should find both streams
        assert!(archive.find("WordDocument").is_some());
        assert!(archive.find("0Table").is_some());

        // Read WordDocument and parse FIB
        let wd_entry = archive.find("WordDocument").unwrap();
        let wd_data = archive.read(wd_entry).unwrap();
        let parsed_fib = fib::parse_fib(&wd_data).unwrap();
        assert_eq!(parsed_fib.ccp_text, 9); // "Hello DOC" = 9 chars

        // Read table stream and parse piece table, then extract text
        let tbl_entry = archive.find("0Table").unwrap();
        let tbl_data = archive.read(tbl_entry).unwrap();
        let pt =
            crate::piece_table::PieceTable::parse(&tbl_data, parsed_fib.fc_clx, parsed_fib.lcb_clx)
                .unwrap();
        let text = pt.assemble_text(&wd_data, 0, parsed_fib.ccp_text).unwrap();
        assert_eq!(text, "Hello DOC");
    }
}
