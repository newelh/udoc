//! File Information Block (FIB) parser for DOC binary format.
//!
//! The FIB is the first structure in the WordDocument stream and contains
//! magic number, version, flags, and offsets/lengths for all sub-streams
//! (CLX, PlcfBtePapx, PlcfBteChpx, SttbfFfn, etc.) stored in the Table
//! stream.
//!
//! Reference: MS-DOC 2.5.1 - Fib

use crate::error::{Error, Result, ResultExt};

/// Magic number identifying a Word 97+ binary document.
const WORD_MAGIC: u16 = 0xA5EC;

/// Minimum nFib for Word 97 format (0x00C1 = 193).
const MIN_NFIB: u16 = 0x00C1;

/// Minimum size of FibBase (bytes 0x00..0x1F inclusive = 32 bytes).
pub const FIB_BASE_SIZE: usize = 0x20;

/// Parsed File Information Block from the WordDocument stream.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Structural completeness: all FIB fields parsed for spec fidelity
pub struct Fib {
    /// Document version (0x00C1 for Word 97, higher for later versions).
    pub n_fib: u16,
    /// Name of the table stream in the CFB container ("0Table" or "1Table").
    pub table_stream_name: &'static str,
    /// Character count of the main document text story.
    pub ccp_text: u32,
    /// Character count of the footnote text.
    pub ccp_ftn: u32,
    /// Character count of the header/footer text.
    pub ccp_hdd: u32,
    /// Character count of the annotation (comment) text.
    pub ccp_atn: u32,
    /// Character count of the endnote text.
    pub ccp_edn: u32,
    /// Character count of the textbox text.
    pub ccp_txbx: u32,
    /// Character count of the header textbox text.
    pub ccp_hdr_txbx: u32,
    /// Offset in the Table stream of the CLX (piece table) data.
    pub fc_clx: u32,
    /// Size in bytes of the CLX data.
    pub lcb_clx: u32,
    /// Offset in the Table stream of the PlcfBtePapx (paragraph properties).
    pub fc_plcf_bte_papx: u32,
    /// Size in bytes of the PlcfBtePapx.
    pub lcb_plcf_bte_papx: u32,
    /// Offset in the Table stream of the PlcfBteChpx (character properties).
    pub fc_plcf_bte_chpx: u32,
    /// Size in bytes of the PlcfBteChpx.
    pub lcb_plcf_bte_chpx: u32,
    /// Offset in the Table stream of the SttbfFfn (font table).
    pub fc_sttbf_ffn: u32,
    /// Size in bytes of the SttbfFfn.
    pub lcb_sttbf_ffn: u32,
    /// Byte offset in the WordDocument stream where document text begins.
    ///
    /// For normal files this is determined by the piece table. For fast-save
    /// files with an empty CLX, this is the fallback starting offset: the
    /// total size of the FIB (i.e., the byte right after all FIB sections).
    pub fib_size: u32,
}

/// Read a little-endian u16 from `data` at `offset`, with context on failure.
fn read_u16(data: &[u8], offset: usize, field: &str) -> Result<u16> {
    let end = offset
        .checked_add(2)
        .ok_or_else(|| Error::new(format!("offset overflow reading {field} at 0x{offset:X}")))?;
    let bytes = data.get(offset..end).ok_or_else(|| {
        Error::new(format!(
            "truncated FIB: need 2 bytes for {field} at offset 0x{offset:X}, have {} total",
            data.len()
        ))
    })?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

/// Read a little-endian u32 from `data` at `offset`, with context on failure.
fn read_u32(data: &[u8], offset: usize, field: &str) -> Result<u32> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| Error::new(format!("offset overflow reading {field} at 0x{offset:X}")))?;
    let bytes = data.get(offset..end).ok_or_else(|| {
        Error::new(format!(
            "truncated FIB: need 4 bytes for {field} at offset 0x{offset:X}, have {} total",
            data.len()
        ))
    })?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Parse the File Information Block from the WordDocument stream.
///
/// Validates the magic number and version, extracts key offsets and sizes
/// needed for piece table parsing, paragraph/character properties, and
/// font tables.
pub fn parse_fib(data: &[u8]) -> Result<Fib> {
    // FibBase: minimum 32 bytes
    if data.len() < FIB_BASE_SIZE {
        return Err(Error::new(format!(
            "WordDocument stream too short for FIB: {} bytes, need at least {FIB_BASE_SIZE}",
            data.len()
        )));
    }

    // Offset 0x00: wIdent (magic)
    let w_ident = read_u16(data, 0x00, "wIdent")?;
    if w_ident != WORD_MAGIC {
        return Err(Error::new(format!(
            "not a Word document: magic 0x{w_ident:04X}, expected 0x{WORD_MAGIC:04X}"
        )));
    }

    // Offset 0x02: nFib (version)
    let n_fib = read_u16(data, 0x02, "nFib")?;
    if n_fib < MIN_NFIB {
        return Err(Error::new(format!(
            "Word 95 format not supported: nFib=0x{n_fib:04X}, minimum is 0x{MIN_NFIB:04X}"
        )));
    }

    // Offset 0x0A: flags
    let flags = read_u16(data, 0x0A, "flags")?;
    let f_encrypted = (flags >> 8) & 1 != 0;
    if f_encrypted {
        return Err(Error::new("encrypted DOC files are not supported"));
    }
    let f_which_tbl_stm = (flags >> 9) & 1 != 0;
    let table_stream_name = if f_which_tbl_stm { "1Table" } else { "0Table" };

    // After FibBase (32 bytes), we have:
    //   csw (u16) at offset 0x20: count of u16s in FibRgW97
    let csw = read_u16(data, 0x20, "csw")? as usize;

    // FibRgW97 starts at offset 0x22, length = csw * 2 bytes
    let fib_rgw_end = 0x22usize
        .checked_add(
            csw.checked_mul(2)
                .ok_or_else(|| Error::new("csw overflow"))?,
        )
        .ok_or_else(|| Error::new("FibRgW97 end offset overflow"))?;

    // cslw (u16) at fib_rgw_end: count of u32s in FibRgLw97
    let cslw = read_u16(data, fib_rgw_end, "cslw")? as usize;

    // FibRgLw97 starts at fib_rgw_end + 2
    let fib_rglw_base = fib_rgw_end
        .checked_add(2)
        .ok_or_else(|| Error::new("FibRgLw97 base offset overflow"))?;

    // Extract ccp* fields from FibRgLw97 (each is a u32 at index * 4)
    // ccpText = index 3, ccpFtn = index 4, ccpHdd = index 5,
    // ccpAtn = index 6, ccpEdn = index 11, ccpTxbx = index 12, ccpHdrTxbx = index 13
    let read_lw = |index: usize, name: &str| -> Result<u32> {
        let off = fib_rglw_base
            .checked_add(
                index
                    .checked_mul(4)
                    .ok_or_else(|| Error::new("lw index overflow"))?,
            )
            .ok_or_else(|| Error::new(format!("FibRgLw97 offset overflow for {name}")))?;
        read_u32(data, off, name)
    };

    let ccp_text = read_lw(3, "ccpText").context("reading ccpText from FibRgLw97")?;
    let ccp_ftn = read_lw(4, "ccpFtn").context("reading ccpFtn from FibRgLw97")?;
    let ccp_hdd = read_lw(5, "ccpHdd").context("reading ccpHdd from FibRgLw97")?;
    let ccp_atn = read_lw(6, "ccpAtn").context("reading ccpAtn from FibRgLw97")?;
    let ccp_edn = read_lw(11, "ccpEdn").context("reading ccpEdn from FibRgLw97")?;
    let ccp_txbx = read_lw(12, "ccpTxbx").context("reading ccpTxbx from FibRgLw97")?;
    let ccp_hdr_txbx = read_lw(13, "ccpHdrTxbx").context("reading ccpHdrTxbx from FibRgLw97")?;

    // After FibRgLw97: cbRgFcLcb (u16) = count of fc/lcb pairs
    let fib_rglw_end = fib_rglw_base
        .checked_add(
            cslw.checked_mul(4)
                .ok_or_else(|| Error::new("cslw size overflow"))?,
        )
        .ok_or_else(|| Error::new("FibRgLw97 end offset overflow"))?;

    let cb_rg_fc_lcb = read_u16(data, fib_rglw_end, "cbRgFcLcb")? as usize;

    // FibRgFcLcb starts at fib_rglw_end + 2, each pair is 8 bytes (fc u32 + lcb u32)
    let fc_lcb_base = fib_rglw_end
        .checked_add(2)
        .ok_or_else(|| Error::new("FibRgFcLcb base offset overflow"))?;

    let read_fc_lcb = |pair_index: usize, name: &str| -> Result<(u32, u32)> {
        if pair_index >= cb_rg_fc_lcb {
            return Err(Error::new(format!(
                "FibRgFcLcb pair index {pair_index} out of range (count={cb_rg_fc_lcb}) for {name}"
            )));
        }
        let pair_off = fc_lcb_base
            .checked_add(
                pair_index
                    .checked_mul(8)
                    .ok_or_else(|| Error::new("fc/lcb pair offset overflow"))?,
            )
            .ok_or_else(|| Error::new(format!("fc/lcb offset overflow for {name}")))?;
        let fc = read_u32(data, pair_off, &format!("fc_{name}"))?;
        let lcb = read_u32(data, pair_off + 4, &format!("lcb_{name}"))?;
        Ok((fc, lcb))
    };

    let (fc_sttbf_ffn, lcb_sttbf_ffn) =
        read_fc_lcb(43, "SttbfFfn").context("reading SttbfFfn fc/lcb")?;
    let (fc_clx, lcb_clx) = read_fc_lcb(66, "Clx").context("reading Clx fc/lcb")?;
    let (fc_plcf_bte_papx, lcb_plcf_bte_papx) =
        read_fc_lcb(72, "PlcfBtePapx").context("reading PlcfBtePapx fc/lcb")?;
    let (fc_plcf_bte_chpx, lcb_plcf_bte_chpx) =
        read_fc_lcb(73, "PlcfBteChpx").context("reading PlcfBteChpx fc/lcb")?;

    // The byte offset where document text begins in the WordDocument stream.
    // For normal files the piece table determines this. For fast-save fallback,
    // text follows immediately after the FIB, so record the FIB's total byte size.
    let fib_size = fc_lcb_base
        .checked_add(
            cb_rg_fc_lcb
                .checked_mul(8)
                .ok_or_else(|| Error::new("FibRgFcLcb size overflow"))?,
        )
        .ok_or_else(|| Error::new("fib_size overflow"))? as u32;

    Ok(Fib {
        n_fib,
        table_stream_name,
        ccp_text,
        ccp_ftn,
        ccp_hdd,
        ccp_atn,
        ccp_edn,
        ccp_txbx,
        ccp_hdr_txbx,
        fc_clx,
        lcb_clx,
        fc_plcf_bte_papx,
        lcb_plcf_bte_papx,
        fc_plcf_bte_chpx,
        lcb_plcf_bte_chpx,
        fc_sttbf_ffn,
        lcb_sttbf_ffn,
        fib_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid FIB binary blob.
    ///
    /// The layout follows the MS-DOC FIB structure:
    /// - FibBase (32 bytes)
    /// - csw + FibRgW97 (csw=7, 14 bytes of zeros)
    /// - cslw + FibRgLw97 (cslw=22, 88 bytes with ccp* values)
    /// - cbRgFcLcb + FibRgFcLcb (74 pairs, 592 bytes)
    fn build_fib(ccp_text: u32, fc_clx: u32, lcb_clx: u32, table_stream_bit: bool) -> Vec<u8> {
        let mut buf = Vec::new();

        // FibBase (32 bytes at offset 0x00)
        buf.extend_from_slice(&WORD_MAGIC.to_le_bytes()); // 0x00: wIdent
        buf.extend_from_slice(&0x00C1u16.to_le_bytes()); // 0x02: nFib (Word 97)
        buf.extend_from_slice(&0u16.to_le_bytes()); // 0x04: unused
        buf.extend_from_slice(&0u16.to_le_bytes()); // 0x06: unused
        buf.extend_from_slice(&0u16.to_le_bytes()); // 0x08: unused

        // 0x0A: flags -- bit 9 = fWhichTblStm
        let flags: u16 = if table_stream_bit { 1 << 9 } else { 0 };
        buf.extend_from_slice(&flags.to_le_bytes());

        // Pad rest of FibBase to 32 bytes
        buf.resize(FIB_BASE_SIZE, 0);

        // csw (u16) at offset 0x20: 7 words in FibRgW97
        let csw: u16 = 7;
        buf.extend_from_slice(&csw.to_le_bytes());

        // FibRgW97: 7 * 2 = 14 bytes of zeros
        buf.extend_from_slice(&[0u8; 14]);

        // cslw (u16): 22 dwords in FibRgLw97
        let cslw: u16 = 22;
        buf.extend_from_slice(&cslw.to_le_bytes());

        // FibRgLw97: 22 * 4 = 88 bytes
        // Indices: 3=ccpText, 4=ccpFtn, 5=ccpHdd, 6=ccpAtn,
        //          11=ccpEdn, 12=ccpTxbx, 13=ccpHdrTxbx
        let mut rglw = [0u8; 88];
        rglw[3 * 4..3 * 4 + 4].copy_from_slice(&ccp_text.to_le_bytes());
        // Other ccp fields default to 0
        buf.extend_from_slice(&rglw);

        // cbRgFcLcb (u16): 74 pairs (enough for index 73)
        let cb_rg: u16 = 74;
        buf.extend_from_slice(&cb_rg.to_le_bytes());

        // FibRgFcLcb: 74 * 8 = 592 bytes
        let mut fc_lcb = vec![0u8; 74 * 8];
        // Pair 43: SttbfFfn (fc=0, lcb=0 by default)
        // Pair 66: Clx
        fc_lcb[66 * 8..66 * 8 + 4].copy_from_slice(&fc_clx.to_le_bytes());
        fc_lcb[66 * 8 + 4..66 * 8 + 8].copy_from_slice(&lcb_clx.to_le_bytes());
        // Pairs 72-73: PlcfBtePapx, PlcfBteChpx (zeros by default)
        buf.extend_from_slice(&fc_lcb);

        buf
    }

    #[test]
    fn parse_valid_fib() {
        let data = build_fib(100, 0x1000, 0x200, false);
        let fib = parse_fib(&data).unwrap();
        assert_eq!(fib.n_fib, 0x00C1);
        assert_eq!(fib.table_stream_name, "0Table");
        assert_eq!(fib.ccp_text, 100);
        assert_eq!(fib.fc_clx, 0x1000);
        assert_eq!(fib.lcb_clx, 0x200);
        assert_eq!(fib.ccp_ftn, 0);
        assert_eq!(fib.ccp_hdd, 0);
        assert_eq!(fib.ccp_atn, 0);
    }

    #[test]
    fn table_stream_1table() {
        let data = build_fib(0, 0, 0, true);
        let fib = parse_fib(&data).unwrap();
        assert_eq!(fib.table_stream_name, "1Table");
    }

    #[test]
    fn table_stream_0table() {
        let data = build_fib(0, 0, 0, false);
        let fib = parse_fib(&data).unwrap();
        assert_eq!(fib.table_stream_name, "0Table");
    }

    #[test]
    fn wrong_magic_rejected() {
        let mut data = build_fib(0, 0, 0, false);
        data[0] = 0xFF;
        data[1] = 0xFF;
        let err = parse_fib(&data).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("not a Word document"), "got: {msg}");
    }

    #[test]
    fn word95_rejected() {
        let mut data = build_fib(0, 0, 0, false);
        // Set nFib to 0x0065 (Word 6/95)
        data[2..4].copy_from_slice(&0x0065u16.to_le_bytes());
        let err = parse_fib(&data).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("Word 95"), "got: {msg}");
    }

    #[test]
    fn encrypted_rejected() {
        let mut data = build_fib(0, 0, 0, false);
        // Set bit 8 (fEncrypted) in flags at offset 0x0A
        let flags = u16::from_le_bytes([data[0x0A], data[0x0B]]);
        let new_flags = flags | (1 << 8);
        data[0x0A..0x0C].copy_from_slice(&new_flags.to_le_bytes());
        let err = parse_fib(&data).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("encrypted"), "got: {msg}");
    }

    #[test]
    fn truncated_data_too_short() {
        let data = vec![0u8; 10];
        let err = parse_fib(&data).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("too short"), "got: {msg}");
    }

    #[test]
    fn truncated_at_fc_lcb() {
        // Build valid FIB but truncate before FibRgFcLcb data
        let mut data = build_fib(0, 0, 0, false);
        // Truncate partway through FibRgFcLcb (keep enough for header but not pair 66)
        let fc_lcb_base = data.len() - 74 * 8; // where FibRgFcLcb starts
        data.truncate(fc_lcb_base + 66 * 8); // truncate right at pair 66
        let err = parse_fib(&data).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("truncated") || msg.contains("need"),
            "got: {msg}"
        );
    }

    #[test]
    fn ccp_fields_parsed_correctly() {
        let mut data = build_fib(500, 0, 0, false);

        // Compute FibRgLw97 base offset:
        // 0x20 (csw) + 2 + 7*2 (FibRgW97) + 2 (cslw) = 0x22 + 14 + 2 = 0x32
        let rglw_base = 0x22 + 14 + 2;

        // Write specific ccp values
        data[rglw_base + 4 * 4..rglw_base + 4 * 4 + 4].copy_from_slice(&10u32.to_le_bytes()); // ccpFtn
        data[rglw_base + 5 * 4..rglw_base + 5 * 4 + 4].copy_from_slice(&20u32.to_le_bytes()); // ccpHdd
        data[rglw_base + 6 * 4..rglw_base + 6 * 4 + 4].copy_from_slice(&30u32.to_le_bytes()); // ccpAtn
        data[rglw_base + 11 * 4..rglw_base + 11 * 4 + 4].copy_from_slice(&40u32.to_le_bytes()); // ccpEdn
        data[rglw_base + 12 * 4..rglw_base + 12 * 4 + 4].copy_from_slice(&50u32.to_le_bytes()); // ccpTxbx
        data[rglw_base + 13 * 4..rglw_base + 13 * 4 + 4].copy_from_slice(&60u32.to_le_bytes()); // ccpHdrTxbx

        let fib = parse_fib(&data).unwrap();
        assert_eq!(fib.ccp_text, 500);
        assert_eq!(fib.ccp_ftn, 10);
        assert_eq!(fib.ccp_hdd, 20);
        assert_eq!(fib.ccp_atn, 30);
        assert_eq!(fib.ccp_edn, 40);
        assert_eq!(fib.ccp_txbx, 50);
        assert_eq!(fib.ccp_hdr_txbx, 60);
    }

    #[test]
    fn fc_lcb_pairs_parsed_correctly() {
        let mut data = build_fib(0, 0x100, 0x200, false);

        // Compute FibRgFcLcb base offset
        let fc_lcb_base = data.len() - 74 * 8;

        // Set SttbfFfn (pair 43)
        data[fc_lcb_base + 43 * 8..fc_lcb_base + 43 * 8 + 4]
            .copy_from_slice(&0x300u32.to_le_bytes());
        data[fc_lcb_base + 43 * 8 + 4..fc_lcb_base + 43 * 8 + 8]
            .copy_from_slice(&0x400u32.to_le_bytes());

        // Set PlcfBtePapx (pair 72)
        data[fc_lcb_base + 72 * 8..fc_lcb_base + 72 * 8 + 4]
            .copy_from_slice(&0x500u32.to_le_bytes());
        data[fc_lcb_base + 72 * 8 + 4..fc_lcb_base + 72 * 8 + 8]
            .copy_from_slice(&0x600u32.to_le_bytes());

        // Set PlcfBteChpx (pair 73)
        data[fc_lcb_base + 73 * 8..fc_lcb_base + 73 * 8 + 4]
            .copy_from_slice(&0x700u32.to_le_bytes());
        data[fc_lcb_base + 73 * 8 + 4..fc_lcb_base + 73 * 8 + 8]
            .copy_from_slice(&0x800u32.to_le_bytes());

        let fib = parse_fib(&data).unwrap();
        assert_eq!(fib.fc_clx, 0x100);
        assert_eq!(fib.lcb_clx, 0x200);
        assert_eq!(fib.fc_sttbf_ffn, 0x300);
        assert_eq!(fib.lcb_sttbf_ffn, 0x400);
        assert_eq!(fib.fc_plcf_bte_papx, 0x500);
        assert_eq!(fib.lcb_plcf_bte_papx, 0x600);
        assert_eq!(fib.fc_plcf_bte_chpx, 0x700);
        assert_eq!(fib.lcb_plcf_bte_chpx, 0x800);
    }

    #[test]
    fn higher_nfib_accepted() {
        let mut data = build_fib(0, 0, 0, false);
        // Word 2003 nFib = 0x0112
        data[2..4].copy_from_slice(&0x0112u16.to_le_bytes());
        let fib = parse_fib(&data).unwrap();
        assert_eq!(fib.n_fib, 0x0112);
    }
}
