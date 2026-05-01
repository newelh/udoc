//! CFB header parsing (sector sizes, FAT pointers, DIFAT array).

use std::sync::Arc;

use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use crate::error::{Error, Result, ResultExt};

/// CFB magic signature: `\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1`
pub(crate) const CFB_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// Free sector marker in DIFAT/FAT.
const FREESECT: u32 = 0xFFFF_FFFF;

/// Number of DIFAT entries stored directly in the header.
const HEADER_DIFAT_COUNT: usize = 109;

/// Minimum header size. Both v3 and v4 header fields live in the first 512
/// bytes; v4 pads the rest of the first 4096-byte sector with zeros.
const MIN_HEADER_SIZE: usize = 512;

/// CFB file format version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CfbVersion {
    /// Version 3: 512-byte sectors.
    V3,
    /// Version 4: 4096-byte sectors.
    V4,
}

/// Parsed CFB header.
#[derive(Debug, Clone)]
pub(super) struct CfbHeader {
    pub version: CfbVersion,
    /// Bytes per sector: 512 (v3) or 4096 (v4).
    pub sector_size: u32,
    /// Bytes per mini-sector (typically 64).
    pub mini_sector_size: u32,
    /// Streams smaller than this go in the mini-stream.
    pub mini_stream_cutoff: u32,
    pub fat_sector_count: u32,
    pub first_dir_sector: u32,
    pub first_mini_fat_sector: u32,
    pub mini_fat_sector_count: u32,
    pub first_difat_sector: u32,
    pub difat_sector_count: u32,
    /// DIFAT entries from the header (valid sector IDs only, FREESECT filtered out).
    pub difat_entries: Vec<u32>,
}

impl CfbHeader {
    /// Parse a CFB header from raw file bytes.
    pub fn parse(data: &[u8], diag: &Arc<dyn DiagnosticsSink>) -> Result<Self> {
        if data.len() < MIN_HEADER_SIZE {
            return Err(Error::cfb(format!(
                "file too small for CFB header ({} bytes, need {})",
                data.len(),
                MIN_HEADER_SIZE
            )));
        }

        if data[0..8] != CFB_MAGIC {
            return Err(Error::cfb_at(0, "invalid CFB magic signature"));
        }

        let byte_order = read_u16_le(data, 0x1C).context("reading byte order")?;
        if byte_order != 0xFFFE {
            return Err(Error::cfb_at(
                0x1C,
                format!("unsupported byte order mark 0x{byte_order:04X}, expected 0xFFFE"),
            ));
        }

        let major_version = read_u16_le(data, 0x1A).context("reading major version")?;
        let version = match major_version {
            3 => CfbVersion::V3,
            4 => CfbVersion::V4,
            other => {
                diag.warning(
                    Warning::new(
                        "CfbUnknownVersion",
                        format!("unknown CFB major version {other}, defaulting to v3"),
                    )
                    .at_offset(0x1A),
                );
                CfbVersion::V3
            }
        };

        // Sector shift determines sector size. If it disagrees with the version,
        // trust the shift value (real files sometimes lie about version).
        let sector_shift = read_u16_le(data, 0x1E).context("reading sector shift")?;
        let expected_shift = match version {
            CfbVersion::V3 => 9,
            CfbVersion::V4 => 12,
        };
        if sector_shift != expected_shift {
            diag.warning(
                Warning::new(
                    "CfbSectorShiftMismatch",
                    format!(
                        "sector shift {} does not match {:?} (expected {}), using shift {}",
                        sector_shift, version, expected_shift, sector_shift
                    ),
                )
                .at_offset(0x1E),
            );
        }
        // Clamp to a sane range to prevent panic/hang on malicious input.
        // Valid values: 9 (512 bytes, v3) and 12 (4096 bytes, v4).
        // Accept anything in 7..=16 with a warning, reject outside that.
        // Below 7: sector_size < 128, too small for a directory entry (128 bytes)
        //          and causes u32 underflow in DIFAT sector entry count calculation.
        if !(7..=16).contains(&sector_shift) {
            return Err(Error::cfb_at(
                0x1E,
                format!("sector shift {sector_shift} is outside valid range (7..=16)"),
            ));
        }
        let sector_size = 1u32 << sector_shift;

        let mini_sector_shift = read_u16_le(data, 0x20).context("reading mini sector shift")?;
        let mini_sector_size = if mini_sector_shift != 6 {
            diag.warning(
                Warning::new(
                    "CfbMiniSectorShift",
                    format!("mini sector shift is {mini_sector_shift}, expected 6, using 6"),
                )
                .at_offset(0x20),
            );
            64
        } else {
            1u32 << mini_sector_shift
        };

        // v3 spec requires directory sector count to be 0
        let dir_sector_count = read_u32_le(data, 0x28).context("reading directory sector count")?;
        if version == CfbVersion::V3 && dir_sector_count != 0 {
            diag.warning(
                Warning::new(
                    "CfbV3DirSectors",
                    format!("v3 header has nonzero directory sector count ({dir_sector_count})"),
                )
                .at_offset(0x28),
            );
        }

        let fat_sector_count = read_u32_le(data, 0x2C).context("reading FAT sector count")?;
        let first_dir_sector = read_u32_le(data, 0x30).context("reading first directory sector")?;

        let mini_stream_cutoff = read_u32_le(data, 0x38).context("reading mini-stream cutoff")?;
        let mini_stream_cutoff = if mini_stream_cutoff == 0 {
            diag.warning(
                Warning::new(
                    "CfbMiniCutoffZero",
                    "mini-stream cutoff is 0, treating as 4096",
                )
                .at_offset(0x38),
            );
            4096
        } else {
            mini_stream_cutoff
        };

        let first_mini_fat_sector =
            read_u32_le(data, 0x3C).context("reading first mini FAT sector")?;
        let mini_fat_sector_count =
            read_u32_le(data, 0x40).context("reading mini FAT sector count")?;
        let first_difat_sector = read_u32_le(data, 0x44).context("reading first DIFAT sector")?;
        let difat_sector_count = read_u32_le(data, 0x48).context("reading DIFAT sector count")?;

        // 109 DIFAT entries at offset 0x4C, each a u32 sector ID
        let mut difat_entries = Vec::new();
        for i in 0..HEADER_DIFAT_COUNT {
            let offset = 0x4C + i * 4;
            let entry = read_u32_le(data, offset).context("reading DIFAT entry")?;
            if entry != FREESECT {
                difat_entries.push(entry);
            }
        }

        Ok(CfbHeader {
            version,
            sector_size,
            mini_sector_size,
            mini_stream_cutoff,
            fat_sector_count,
            first_dir_sector,
            first_mini_fat_sector,
            mini_fat_sector_count,
            first_difat_sector,
            difat_sector_count,
            difat_entries,
        })
    }

    /// Byte offset of a sector in the file.
    /// Sector 0 starts right after the header (at byte `sector_size`).
    pub(super) fn sector_offset(&self, sector_id: u32) -> Option<u64> {
        (sector_id as u64 + 1).checked_mul(self.sector_size as u64)
    }
}

// -- Bounds-checked little-endian readers --

pub(super) fn read_u16_le(data: &[u8], offset: usize) -> Result<u16> {
    let bytes: [u8; 2] = data
        .get(offset..offset + 2)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| Error::cfb_at(offset as u64, "unexpected end of data"))?;
    Ok(u16::from_le_bytes(bytes))
}

pub(super) fn read_u32_le(data: &[u8], offset: usize) -> Result<u32> {
    let bytes: [u8; 4] = data
        .get(offset..offset + 4)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| Error::cfb_at(offset as u64, "unexpected end of data"))?;
    Ok(u32::from_le_bytes(bytes))
}

pub(super) fn read_u64_le(data: &[u8], offset: usize) -> Result<u64> {
    let bytes: [u8; 8] = data
        .get(offset..offset + 8)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| Error::cfb_at(offset as u64, "unexpected end of data"))?;
    Ok(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::diagnostics::{CollectingDiagnostics, NullDiagnostics};

    fn null_diag() -> Arc<dyn DiagnosticsSink> {
        Arc::new(NullDiagnostics)
    }

    fn collecting_diag() -> Arc<CollectingDiagnostics> {
        Arc::new(CollectingDiagnostics::new())
    }

    /// Build a minimal valid v3 CFB header (512 bytes).
    fn build_v3_header() -> Vec<u8> {
        let mut buf = vec![0u8; 512];
        buf[0..8].copy_from_slice(&CFB_MAGIC);
        // minor version
        buf[0x18..0x1A].copy_from_slice(&0x003Eu16.to_le_bytes());
        // major version = 3
        buf[0x1A..0x1C].copy_from_slice(&0x0003u16.to_le_bytes());
        // byte order = little-endian
        buf[0x1C..0x1E].copy_from_slice(&0xFFFEu16.to_le_bytes());
        // sector shift = 9 (512 bytes)
        buf[0x1E..0x20].copy_from_slice(&9u16.to_le_bytes());
        // mini sector shift = 6 (64 bytes)
        buf[0x20..0x22].copy_from_slice(&6u16.to_le_bytes());
        // mini-stream cutoff = 4096
        buf[0x38..0x3C].copy_from_slice(&4096u32.to_le_bytes());
        // first_dir_sector = ENDOFCHAIN
        buf[0x30..0x34].copy_from_slice(&0xFFFFFFFEu32.to_le_bytes());
        // first_mini_fat_sector = ENDOFCHAIN
        buf[0x3C..0x40].copy_from_slice(&0xFFFFFFFEu32.to_le_bytes());
        // first_difat_sector = ENDOFCHAIN
        buf[0x44..0x48].copy_from_slice(&0xFFFFFFFEu32.to_le_bytes());
        // fill DIFAT array with FREESECT
        for i in 0..HEADER_DIFAT_COUNT {
            let offset = 0x4C + i * 4;
            buf[offset..offset + 4].copy_from_slice(&FREESECT.to_le_bytes());
        }
        buf
    }

    #[test]
    fn parse_valid_v3() {
        let mut buf = build_v3_header();
        buf[0x2C..0x30].copy_from_slice(&1u32.to_le_bytes());
        buf[0x30..0x34].copy_from_slice(&0u32.to_le_bytes());
        buf[0x4C..0x50].copy_from_slice(&5u32.to_le_bytes());

        let hdr = CfbHeader::parse(&buf, &null_diag()).unwrap();
        assert_eq!(hdr.version, CfbVersion::V3);
        assert_eq!(hdr.sector_size, 512);
        assert_eq!(hdr.mini_sector_size, 64);
        assert_eq!(hdr.mini_stream_cutoff, 4096);
        assert_eq!(hdr.fat_sector_count, 1);
        assert_eq!(hdr.first_dir_sector, 0);
        assert_eq!(hdr.first_mini_fat_sector, 0xFFFFFFFE);
        assert_eq!(hdr.first_difat_sector, 0xFFFFFFFE);
        assert_eq!(hdr.difat_sector_count, 0);
        assert_eq!(hdr.difat_entries, vec![5]);
    }

    #[test]
    fn parse_valid_v4() {
        let mut buf = build_v3_header();
        buf[0x1A..0x1C].copy_from_slice(&0x0004u16.to_le_bytes());
        buf[0x1E..0x20].copy_from_slice(&12u16.to_le_bytes());

        let hdr = CfbHeader::parse(&buf, &null_diag()).unwrap();
        assert_eq!(hdr.version, CfbVersion::V4);
        assert_eq!(hdr.sector_size, 4096);
    }

    #[test]
    fn parse_wrong_magic() {
        let mut buf = build_v3_header();
        buf[0] = 0x00;
        let err = CfbHeader::parse(&buf, &null_diag()).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("magic signature"), "got: {msg}");
    }

    #[test]
    fn parse_wrong_byte_order() {
        let mut buf = build_v3_header();
        buf[0x1C..0x1E].copy_from_slice(&0xFEFFu16.to_le_bytes());
        let err = CfbHeader::parse(&buf, &null_diag()).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("byte order"), "got: {msg}");
    }

    #[test]
    fn parse_truncated() {
        let buf = vec![0u8; 100];
        let err = CfbHeader::parse(&buf, &null_diag()).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("too small"), "got: {msg}");
    }

    #[test]
    fn parse_unknown_version() {
        let mut buf = build_v3_header();
        buf[0x1A..0x1C].copy_from_slice(&5u16.to_le_bytes());
        let diag = collecting_diag();
        let hdr = CfbHeader::parse(&buf, &(diag.clone() as Arc<dyn DiagnosticsSink>)).unwrap();
        assert_eq!(hdr.version, CfbVersion::V3);
        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.kind == "CfbUnknownVersion"),
            "expected CfbUnknownVersion warning"
        );
    }

    #[test]
    fn parse_sector_shift_mismatch() {
        let mut buf = build_v3_header();
        buf[0x1E..0x20].copy_from_slice(&12u16.to_le_bytes());
        let diag = collecting_diag();
        let hdr = CfbHeader::parse(&buf, &(diag.clone() as Arc<dyn DiagnosticsSink>)).unwrap();
        assert_eq!(hdr.sector_size, 4096);
        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.kind == "CfbSectorShiftMismatch"),
            "expected CfbSectorShiftMismatch warning"
        );
    }

    #[test]
    fn parse_mini_cutoff_zero() {
        let mut buf = build_v3_header();
        buf[0x38..0x3C].copy_from_slice(&0u32.to_le_bytes());
        let diag = collecting_diag();
        let hdr = CfbHeader::parse(&buf, &(diag.clone() as Arc<dyn DiagnosticsSink>)).unwrap();
        assert_eq!(hdr.mini_stream_cutoff, 4096);
        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.kind == "CfbMiniCutoffZero"),
            "expected CfbMiniCutoffZero warning"
        );
    }

    #[test]
    fn parse_difat_entries_filtered() {
        let mut buf = build_v3_header();
        buf[0x4C..0x50].copy_from_slice(&10u32.to_le_bytes());
        buf[0x50..0x54].copy_from_slice(&20u32.to_le_bytes());
        // 0x54 stays FREESECT
        buf[0x58..0x5C].copy_from_slice(&30u32.to_le_bytes());

        let hdr = CfbHeader::parse(&buf, &null_diag()).unwrap();
        assert_eq!(hdr.difat_entries, vec![10, 20, 30]);
    }

    #[test]
    fn parse_all_freesect_difat() {
        let buf = build_v3_header();
        let hdr = CfbHeader::parse(&buf, &null_diag()).unwrap();
        assert!(hdr.difat_entries.is_empty());
    }

    #[test]
    fn sector_offset_calculation() {
        let buf = build_v3_header();
        let hdr = CfbHeader::parse(&buf, &null_diag()).unwrap();
        assert_eq!(hdr.sector_offset(0), Some(512));
        assert_eq!(hdr.sector_offset(1), Some(1024));
    }

    #[test]
    fn sector_offset_v4() {
        let mut buf = build_v3_header();
        buf[0x1A..0x1C].copy_from_slice(&0x0004u16.to_le_bytes());
        buf[0x1E..0x20].copy_from_slice(&12u16.to_le_bytes());
        let hdr = CfbHeader::parse(&buf, &null_diag()).unwrap();
        assert_eq!(hdr.sector_offset(0), Some(4096));
        assert_eq!(hdr.sector_offset(1), Some(8192));
    }

    #[test]
    fn read_helpers_bounds_check() {
        let data = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        assert_eq!(read_u16_le(&data, 0).unwrap(), 0x0201);
        assert_eq!(read_u32_le(&data, 0).unwrap(), 0x04030201);
        assert_eq!(read_u64_le(&data, 0).unwrap(), 0x0807060504030201);

        assert!(read_u16_le(&data, 7).is_err());
        assert!(read_u32_le(&data, 5).is_err());
        assert!(read_u64_le(&data, 1).is_err());
    }

    #[test]
    fn parse_v3_nonzero_dir_sectors_warns() {
        let mut buf = build_v3_header();
        buf[0x28..0x2C].copy_from_slice(&3u32.to_le_bytes());
        let diag = collecting_diag();
        let _hdr = CfbHeader::parse(&buf, &(diag.clone() as Arc<dyn DiagnosticsSink>)).unwrap();
        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.kind == "CfbV3DirSectors"),
            "expected CfbV3DirSectors warning"
        );
    }

    #[test]
    fn parse_sector_shift_too_large() {
        let mut buf = build_v3_header();
        buf[0x1E..0x20].copy_from_slice(&17u16.to_le_bytes());
        let err = CfbHeader::parse(&buf, &null_diag()).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("sector shift"), "got: {msg}");
    }

    #[test]
    fn parse_sector_shift_too_small() {
        let mut buf = build_v3_header();
        buf[0x1E..0x20].copy_from_slice(&6u16.to_le_bytes());
        let err = CfbHeader::parse(&buf, &null_diag()).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("sector shift"), "got: {msg}");
    }

    #[test]
    fn parse_sector_shift_zero_rejected() {
        // Shift 0 would cause underflow in DIFAT entry count calculation.
        let mut buf = build_v3_header();
        buf[0x1E..0x20].copy_from_slice(&0u16.to_le_bytes());
        let err = CfbHeader::parse(&buf, &null_diag()).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("sector shift"), "got: {msg}");
    }

    #[test]
    fn parse_sector_shift_min_valid() {
        // Shift 7 (128-byte sectors) is the minimum accepted.
        let mut buf = build_v3_header();
        buf[0x1E..0x20].copy_from_slice(&7u16.to_le_bytes());
        let hdr = CfbHeader::parse(&buf, &null_diag()).unwrap();
        assert_eq!(hdr.sector_size, 128);
    }

    #[test]
    fn parse_sector_shift_max_valid() {
        // Shift 16 (65536-byte sectors) should be accepted.
        let mut buf = build_v3_header();
        buf[0x1E..0x20].copy_from_slice(&16u16.to_le_bytes());
        let hdr = CfbHeader::parse(&buf, &null_diag()).unwrap();
        assert_eq!(hdr.sector_size, 65536);
    }
}
