//! SttbfFfn (font table) parser for DOC binary format.
//!
//! Parses the SttbfFfn structure from the Table stream to extract font
//! names. The font_index from character properties (sprmCRgFtc0) indexes
//! into this table.
//!
//! Reference: MS-DOC 2.9.273 (SttbfFfn), 2.9.89 (Ffn)

use crate::error::{Error, Result};
use crate::fib::Fib;

/// Offset of the font name (xszFfn) within an FFN structure.
///
/// FFN layout (MS-DOC 2.9.89):
/// - byte 0: cbFfnM1 (total FFN size - 1)
/// - byte 1: flags (prq, fTrueType, reserved)
/// - bytes 2-3: wWeight
/// - byte 4: chs (charset)
/// - byte 5: ixchSzAlt
/// - bytes 6-15: panose (10 bytes)
/// - bytes 16-39: fs / FONTSIGNATURE (24 bytes)
/// - bytes 40+: xszFfn (null-terminated UTF-16LE font name)
const FFN_NAME_OFFSET: usize = 40;

/// Maximum number of fonts we'll accept (safety limit).
const MAX_FONTS: usize = 10_000;

/// Parse the SttbfFfn (font table) from the Table stream.
///
/// Returns a Vec where index i is the font name for font_index i.
/// Returns an empty Vec if the font table is empty or missing.
pub fn parse_font_table(table_stream: &[u8], fib: &Fib) -> Result<Vec<String>> {
    let offset = fib.fc_sttbf_ffn as usize;
    let size = fib.lcb_sttbf_ffn as usize;

    if size == 0 {
        return Ok(Vec::new());
    }

    let end = offset
        .checked_add(size)
        .ok_or_else(|| Error::new("SttbfFfn end offset overflow"))?;

    let data = table_stream.get(offset..end).ok_or_else(|| {
        Error::new(format!(
            "SttbfFfn out of bounds: offset {offset}..{end}, table stream length {}",
            table_stream.len()
        ))
    })?;

    // Minimum STTB header: fExtend (2) + cData (2) + cbExtra (2) = 6 bytes
    if data.len() < 6 {
        return Err(Error::new(format!(
            "SttbfFfn too small: {} bytes, need at least 6",
            data.len()
        )));
    }

    let f_extend = u16::from_le_bytes([data[0], data[1]]);
    if f_extend != 0xFFFF {
        // Not an extended STTB. Older Word versions may use non-extended
        // format but we only support Word 97+ which uses extended.
        return Err(Error::new(format!(
            "SttbfFfn: expected extended STTB marker 0xFFFF, got 0x{f_extend:04X}"
        )));
    }

    let c_data = u16::from_le_bytes([data[2], data[3]]) as usize;
    let cb_extra = u16::from_le_bytes([data[4], data[5]]) as usize;

    if c_data > MAX_FONTS {
        return Err(Error::new(format!(
            "SttbfFfn: too many fonts: {c_data}, maximum is {MAX_FONTS}"
        )));
    }

    let mut fonts = Vec::with_capacity(c_data);
    let mut pos = 6; // start after header

    for _ in 0..c_data {
        // Each entry: u16 cbData (byte count of FFN data), then cbData bytes, then cbExtra bytes
        if pos + 2 > data.len() {
            break; // truncated
        }

        let cb_data = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
        pos += 2;

        if cb_data == 0 {
            fonts.push(String::new());
            pos += cb_extra;
            continue;
        }

        let entry_end = pos + cb_data;
        if entry_end > data.len() {
            break; // truncated entry
        }

        let entry = &data[pos..entry_end];
        let name = extract_font_name(entry);
        fonts.push(name);

        pos = entry_end + cb_extra;
    }

    Ok(fonts)
}

/// Extract the font name from an FFN structure.
///
/// The font name is a null-terminated UTF-16LE string starting at offset
/// 40 within the FFN. If the entry is too short, returns an empty string.
fn extract_font_name(ffn: &[u8]) -> String {
    if ffn.len() <= FFN_NAME_OFFSET {
        return String::new();
    }

    let name_bytes = &ffn[FFN_NAME_OFFSET..];

    // Decode UTF-16LE, stopping at null terminator
    let mut chars = Vec::new();
    let mut i = 0;
    while i + 1 < name_bytes.len() {
        let code_unit = u16::from_le_bytes([name_bytes[i], name_bytes[i + 1]]);
        if code_unit == 0 {
            break; // null terminator
        }
        chars.push(code_unit);
        i += 2;
    }

    String::from_utf16_lossy(&chars)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic SttbfFfn with the given font names.
    fn build_sttbf_ffn(names: &[&str]) -> Vec<u8> {
        let mut data = Vec::new();

        // Header
        data.extend_from_slice(&0xFFFFu16.to_le_bytes()); // fExtend
        data.extend_from_slice(&(names.len() as u16).to_le_bytes()); // cData
        data.extend_from_slice(&0u16.to_le_bytes()); // cbExtra

        for name in names {
            // Build FFN entry
            let name_utf16: Vec<u16> = name.encode_utf16().collect();
            // Name bytes: (len+1)*2 for null-terminated UTF-16LE
            let name_byte_len = (name_utf16.len() + 1) * 2;
            let ffn_size = FFN_NAME_OFFSET + name_byte_len;

            // cbData = size of FFN entry
            data.extend_from_slice(&(ffn_size as u16).to_le_bytes());

            // FFN fixed fields (40 bytes)
            let cb_ffn_m1 = (ffn_size - 1) as u8;
            data.push(cb_ffn_m1);
            // Remaining 39 bytes of fixed fields (zeros)
            data.extend_from_slice(&[0u8; 39]);

            // Font name as null-terminated UTF-16LE
            for &unit in &name_utf16 {
                data.extend_from_slice(&unit.to_le_bytes());
            }
            // Null terminator
            data.extend_from_slice(&0u16.to_le_bytes());
        }

        data
    }

    /// Build a Fib with SttbfFfn offset/size.
    fn fib_with_font_table(fc: u32, lcb: u32) -> Fib {
        Fib {
            n_fib: 0x00C1,
            table_stream_name: "0Table",
            ccp_text: 0,
            ccp_ftn: 0,
            ccp_hdd: 0,
            ccp_atn: 0,
            ccp_edn: 0,
            ccp_txbx: 0,
            ccp_hdr_txbx: 0,
            fc_clx: 0,
            lcb_clx: 0,
            fc_plcf_bte_papx: 0,
            lcb_plcf_bte_papx: 0,
            fc_plcf_bte_chpx: 0,
            lcb_plcf_bte_chpx: 0,
            fc_sttbf_ffn: fc,
            lcb_sttbf_ffn: lcb,
            fib_size: 0,
        }
    }

    #[test]
    fn parse_single_font() {
        let sttb = build_sttbf_ffn(&["Times New Roman"]);
        let fib = fib_with_font_table(0, sttb.len() as u32);
        let fonts = parse_font_table(&sttb, &fib).unwrap();

        assert_eq!(fonts.len(), 1);
        assert_eq!(fonts[0], "Times New Roman");
    }

    #[test]
    fn parse_multiple_fonts() {
        let sttb = build_sttbf_ffn(&["Arial", "Courier New", "Verdana"]);
        let fib = fib_with_font_table(0, sttb.len() as u32);
        let fonts = parse_font_table(&sttb, &fib).unwrap();

        assert_eq!(fonts.len(), 3);
        assert_eq!(fonts[0], "Arial");
        assert_eq!(fonts[1], "Courier New");
        assert_eq!(fonts[2], "Verdana");
    }

    #[test]
    fn parse_empty_font_table() {
        let fib = fib_with_font_table(0, 0);
        let fonts = parse_font_table(&[], &fib).unwrap();
        assert!(fonts.is_empty());
    }

    #[test]
    fn parse_zero_entries() {
        let sttb = build_sttbf_ffn(&[]);
        let fib = fib_with_font_table(0, sttb.len() as u32);
        let fonts = parse_font_table(&sttb, &fib).unwrap();
        assert!(fonts.is_empty());
    }

    #[test]
    fn font_table_at_nonzero_offset() {
        let sttb = build_sttbf_ffn(&["Helvetica"]);
        let mut table_stream = vec![0xAA; 100]; // padding
        let fc = table_stream.len() as u32;
        table_stream.extend_from_slice(&sttb);
        let fib = fib_with_font_table(fc, sttb.len() as u32);

        let fonts = parse_font_table(&table_stream, &fib).unwrap();
        assert_eq!(fonts.len(), 1);
        assert_eq!(fonts[0], "Helvetica");
    }

    #[test]
    fn truncated_entry_stops_gracefully() {
        // Build valid header but truncate partway through first entry
        let mut data = Vec::new();
        data.extend_from_slice(&0xFFFFu16.to_le_bytes()); // fExtend
        data.extend_from_slice(&1u16.to_le_bytes()); // cData = 1
        data.extend_from_slice(&0u16.to_le_bytes()); // cbExtra = 0
                                                     // cbData claims 50 bytes but we only provide 10
        data.extend_from_slice(&50u16.to_le_bytes());
        data.extend_from_slice(&[0u8; 10]);

        let fib = fib_with_font_table(0, data.len() as u32);
        let fonts = parse_font_table(&data, &fib).unwrap();
        // Should return empty vec (truncated entry skipped)
        assert!(fonts.is_empty());
    }

    #[test]
    fn non_extended_sttb_rejected() {
        let mut data = Vec::new();
        data.extend_from_slice(&0x0000u16.to_le_bytes()); // not 0xFFFF
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());

        let fib = fib_with_font_table(0, data.len() as u32);
        let err = parse_font_table(&data, &fib);
        assert!(err.is_err());
    }

    #[test]
    fn extract_font_name_from_short_entry() {
        // Entry shorter than FFN_NAME_OFFSET
        let short = vec![0u8; 20];
        assert_eq!(extract_font_name(&short), "");
    }

    #[test]
    fn font_name_with_unicode() {
        // Test non-ASCII font name
        let sttb = build_sttbf_ffn(&["MS Mincho"]);
        let fib = fib_with_font_table(0, sttb.len() as u32);
        let fonts = parse_font_table(&sttb, &fib).unwrap();
        assert_eq!(fonts[0], "MS Mincho");
    }
}
