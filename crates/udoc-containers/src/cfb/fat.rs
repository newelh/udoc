//! FAT and MiniFAT chain walking for CFB containers.
//!
//! The File Allocation Table maps each sector to its successor in a chain,
//! similar to a linked list stored as a flat array. The MiniFAT does the
//! same for mini-sectors within the mini-stream.

use std::collections::HashSet;
use std::sync::Arc;

use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use super::header::{read_u32_le, CfbHeader};
use crate::error::{Error, Result, ResultExt};

// CFB special sector values.
pub(super) const FREESECT: u32 = 0xFFFF_FFFF;
pub(super) const ENDOFCHAIN: u32 = 0xFFFF_FFFE;
pub(super) const FATSECT: u32 = 0xFFFF_FFFD;
pub(super) const DIFSECT: u32 = 0xFFFF_FFFC;
/// Maximum number of DIFAT sectors to follow before giving up. Prevents
/// unbounded loops when `difat_sector_count` is set to a huge value.
const MAX_DIFAT_SECTORS: u32 = 1_000_000;

/// The File Allocation Table: a flat array mapping sector ID -> next sector ID.
#[derive(Debug)]
pub(super) struct Fat {
    entries: Vec<u32>,
}

impl Fat {
    /// Build the FAT by assembling the DIFAT chain and reading FAT sectors.
    pub fn build(data: &[u8], header: &CfbHeader, diag: &Arc<dyn DiagnosticsSink>) -> Result<Self> {
        let fat_sector_ids = collect_difat(data, header, diag).context("assembling DIFAT")?;

        if fat_sector_ids.len() != header.fat_sector_count as usize {
            diag.warning(Warning::new(
                "CfbFatSectorCountMismatch",
                format!(
                    "DIFAT yielded {} FAT sectors but header declares {}",
                    fat_sector_ids.len(),
                    header.fat_sector_count,
                ),
            ));
        }

        let entries_per_sector = (header.sector_size / 4) as usize;
        let mut entries = Vec::with_capacity(fat_sector_ids.len() * entries_per_sector);

        for &fat_sector_id in &fat_sector_ids {
            let offset = match header.sector_offset(fat_sector_id) {
                Some(off) => off as usize,
                None => {
                    diag.warning(Warning::new(
                        "CfbFatSectorTruncated",
                        format!(
                            "FAT sector {} offset overflows, filling with FREESECT",
                            fat_sector_id,
                        ),
                    ));
                    entries.resize(entries.len() + entries_per_sector, FREESECT);
                    continue;
                }
            };
            let end = offset + header.sector_size as usize;

            if end > data.len() {
                diag.warning(Warning::new(
                    "CfbFatSectorTruncated",
                    format!(
                        "FAT sector {} at offset {} extends beyond file (len {}), \
                         filling with FREESECT",
                        fat_sector_id,
                        offset,
                        data.len(),
                    ),
                ));
                entries.resize(entries.len() + entries_per_sector, FREESECT);
                continue;
            }

            for i in 0..entries_per_sector {
                let val = read_u32_le(data, offset + i * 4).unwrap_or(FREESECT);
                entries.push(val);
            }
        }

        Ok(Fat { entries })
    }

    /// Follow a FAT chain starting at `start`, returning the list of sector IDs.
    ///
    /// Returns an empty vec if `start` is ENDOFCHAIN or FREESECT.
    pub fn follow_chain(&self, start: u32, diag: &Arc<dyn DiagnosticsSink>) -> Result<Vec<u32>> {
        follow_chain_impl(&self.entries, start, diag, "FAT")
    }

    /// Look up a single FAT entry by index.
    #[cfg(test)]
    pub fn get(&self, index: u32) -> Option<u32> {
        self.entries.get(index as usize).copied()
    }
}

/// The Mini File Allocation Table: chain walking for mini-sectors.
#[derive(Debug)]
pub(super) struct MiniFat {
    entries: Vec<u32>,
}

impl MiniFat {
    /// Build the MiniFAT by following its sector chain in the regular FAT.
    pub fn build(
        data: &[u8],
        header: &CfbHeader,
        fat: &Fat,
        diag: &Arc<dyn DiagnosticsSink>,
    ) -> Result<Self> {
        if header.first_mini_fat_sector == ENDOFCHAIN {
            if header.mini_fat_sector_count != 0 {
                diag.warning(Warning::new(
                    "CfbMiniFatSectorCountMismatch",
                    format!(
                        "no mini-FAT chain but header declares {} mini-FAT sectors",
                        header.mini_fat_sector_count,
                    ),
                ));
            }
            return Ok(MiniFat {
                entries: Vec::new(),
            });
        }

        let chain = fat
            .follow_chain(header.first_mini_fat_sector, diag)
            .context("following mini-FAT sector chain")?;

        if chain.len() != header.mini_fat_sector_count as usize {
            diag.warning(Warning::new(
                "CfbMiniFatSectorCountMismatch",
                format!(
                    "mini-FAT chain has {} sectors but header declares {}",
                    chain.len(),
                    header.mini_fat_sector_count,
                ),
            ));
        }

        let entries_per_sector = (header.sector_size / 4) as usize;
        let mut entries = Vec::with_capacity(chain.len() * entries_per_sector);

        for &sector_id in &chain {
            let offset = match header.sector_offset(sector_id) {
                Some(off) => off as usize,
                None => {
                    diag.warning(Warning::new(
                        "CfbMiniFatSectorTruncated",
                        format!("mini-FAT sector {} offset overflows, stopping", sector_id,),
                    ));
                    break;
                }
            };
            let end = offset + header.sector_size as usize;

            if end > data.len() {
                diag.warning(Warning::new(
                    "CfbMiniFatSectorTruncated",
                    format!(
                        "mini-FAT sector {} at offset {} extends beyond file, \
                         stopping",
                        sector_id, offset,
                    ),
                ));
                break;
            }

            for i in 0..entries_per_sector {
                let val = read_u32_le(data, offset + i * 4).unwrap_or(FREESECT);
                entries.push(val);
            }
        }

        Ok(MiniFat { entries })
    }

    /// Follow a mini-FAT chain starting at `start`.
    pub fn follow_chain(&self, start: u32, diag: &Arc<dyn DiagnosticsSink>) -> Result<Vec<u32>> {
        follow_chain_impl(&self.entries, start, diag, "MiniFAT")
    }
}

// -- Private helpers --

/// Collect all FAT sector IDs from the DIFAT (header entries + DIFAT chain).
fn collect_difat(
    data: &[u8],
    header: &CfbHeader,
    diag: &Arc<dyn DiagnosticsSink>,
) -> Result<Vec<u32>> {
    // Header already filters out FREESECT from difat_entries.
    let mut fat_sector_ids: Vec<u32> = header.difat_entries.clone();

    if header.first_difat_sector != ENDOFCHAIN && header.difat_sector_count > 0 {
        let ids_per_difat_sector = (header.sector_size / 4 - 1) as usize;
        let mut current = header.first_difat_sector;
        let mut iterations: u32 = 0;
        let max_iterations = header.difat_sector_count.min(MAX_DIFAT_SECTORS);
        let mut visited_difat = HashSet::new();

        while current != ENDOFCHAIN && current != FREESECT {
            if iterations >= max_iterations {
                diag.warning(Warning::new(
                    "CfbDifatChainTooLong",
                    format!("DIFAT chain exceeded limit of {max_iterations}, stopping",),
                ));
                break;
            }

            if !visited_difat.insert(current) {
                diag.warning(Warning::new(
                    "CfbDifatCycle",
                    format!("DIFAT chain has cycle at sector {current}"),
                ));
                break;
            }

            let offset = match header.sector_offset(current) {
                Some(off) => off as usize,
                None => {
                    return Err(Error::cfb(format!(
                        "DIFAT sector {} offset overflows",
                        current,
                    )));
                }
            };
            let end = offset + header.sector_size as usize;

            if end > data.len() {
                return Err(Error::cfb(format!(
                    "DIFAT sector {} at offset {} extends beyond file (len {})",
                    current,
                    offset,
                    data.len(),
                )));
            }

            for i in 0..ids_per_difat_sector {
                let id = read_u32_le(data, offset + i * 4).unwrap_or(FREESECT);
                if id != FREESECT {
                    fat_sector_ids.push(id);
                }
            }

            // Last 4 bytes of the DIFAT sector are the next-pointer.
            let next_offset = offset + ids_per_difat_sector * 4;
            current = read_u32_le(data, next_offset).unwrap_or(ENDOFCHAIN);
            iterations += 1;
        }
    }

    Ok(fat_sector_ids)
}

/// Shared chain-following logic for both FAT and MiniFAT.
fn follow_chain_impl(
    entries: &[u32],
    start: u32,
    diag: &Arc<dyn DiagnosticsSink>,
    label: &str,
) -> Result<Vec<u32>> {
    if start == ENDOFCHAIN || start == FREESECT {
        return Ok(Vec::new());
    }

    let max_len = entries.len();
    let mut chain = Vec::new();
    let mut visited = HashSet::new();
    let mut current = start;

    loop {
        if current as usize >= entries.len() {
            diag.warning(Warning::new(
                format!("Cfb{label}OutOfBounds"),
                format!(
                    "{label} chain references sector {} but table has only \
                     {} entries",
                    current,
                    entries.len(),
                ),
            ));
            break;
        }

        if !visited.insert(current) {
            diag.warning(Warning::new(
                format!("Cfb{label}Cycle"),
                format!("{label} chain has cycle at sector {current}"),
            ));
            break;
        }

        if chain.len() >= max_len {
            diag.warning(Warning::new(
                format!("Cfb{label}ChainTooLong"),
                format!(
                    "{label} chain exceeds maximum length of {max_len}, \
                     stopping",
                ),
            ));
            break;
        }

        chain.push(current);

        let next = entries[current as usize];
        match next {
            ENDOFCHAIN => break,
            FREESECT | FATSECT | DIFSECT => {
                diag.warning(Warning::new(
                    format!("Cfb{label}BadEntry"),
                    format!(
                        "{label} chain hit special value {next:#010X} at \
                         sector {current}",
                    ),
                ));
                break;
            }
            _ => current = next,
        }
    }

    Ok(chain)
}

#[cfg(test)]
impl Fat {
    /// Construct a Fat from raw entries (for sibling module tests).
    pub fn from_entries_for_test(entries: Vec<u32>) -> Self {
        Self { entries }
    }
}

#[cfg(test)]
impl MiniFat {
    /// Construct a MiniFat from raw entries (for sibling module tests).
    pub fn from_entries_for_test(entries: Vec<u32>) -> Self {
        Self { entries }
    }
}

#[cfg(test)]
mod tests {
    use super::super::header::CfbVersion;
    use super::*;
    use udoc_core::diagnostics::{CollectingDiagnostics, NullDiagnostics};

    fn null_diag() -> Arc<dyn DiagnosticsSink> {
        Arc::new(NullDiagnostics)
    }

    fn collecting_diag() -> Arc<CollectingDiagnostics> {
        Arc::new(CollectingDiagnostics::new())
    }

    /// Build a test header pointing at FAT sector 0 (immediately after header).
    fn test_header() -> CfbHeader {
        CfbHeader {
            version: CfbVersion::V3,
            sector_size: 512,
            mini_sector_size: 64,
            mini_stream_cutoff: 4096,
            fat_sector_count: 1,
            first_dir_sector: ENDOFCHAIN,
            first_mini_fat_sector: ENDOFCHAIN,
            mini_fat_sector_count: 0,
            first_difat_sector: ENDOFCHAIN,
            difat_sector_count: 0,
            difat_entries: vec![0],
        }
    }

    /// Construct minimal file data: header block + one FAT sector with the
    /// given entries (padded with FREESECT).
    fn build_fat_test_data(fat_entries: &[u32], header: &CfbHeader) -> Vec<u8> {
        let sector_size = header.sector_size as usize;
        let entries_per_sector = sector_size / 4;
        let mut data = vec![0u8; sector_size + sector_size];
        for (i, &entry) in fat_entries.iter().enumerate() {
            let offset = sector_size + i * 4;
            if offset + 4 <= data.len() {
                data[offset..offset + 4].copy_from_slice(&entry.to_le_bytes());
            }
        }
        for i in fat_entries.len()..entries_per_sector {
            let offset = sector_size + i * 4;
            if offset + 4 <= data.len() {
                data[offset..offset + 4].copy_from_slice(&FREESECT.to_le_bytes());
            }
        }
        data
    }

    #[test]
    fn follow_chain_simple() {
        // Chain: 0 -> 1 -> 2 -> ENDOFCHAIN
        let header = test_header();
        let data = build_fat_test_data(&[1, 2, ENDOFCHAIN], &header);
        let diag = null_diag();
        let fat = Fat::build(&data, &header, &diag).unwrap();
        let chain = fat.follow_chain(0, &diag).unwrap();
        assert_eq!(chain, vec![0, 1, 2]);
    }

    #[test]
    fn follow_chain_single() {
        // Chain: 0 -> ENDOFCHAIN
        let header = test_header();
        let data = build_fat_test_data(&[ENDOFCHAIN], &header);
        let diag = null_diag();
        let fat = Fat::build(&data, &header, &diag).unwrap();
        let chain = fat.follow_chain(0, &diag).unwrap();
        assert_eq!(chain, vec![0]);
    }

    #[test]
    fn follow_chain_empty() {
        let header = test_header();
        let data = build_fat_test_data(&[FREESECT], &header);
        let diag = null_diag();
        let fat = Fat::build(&data, &header, &diag).unwrap();
        let chain = fat.follow_chain(ENDOFCHAIN, &diag).unwrap();
        assert!(chain.is_empty());
    }

    #[test]
    fn follow_chain_cycle_detected() {
        // Chain: 0 -> 1 -> 0 (cycle)
        let header = test_header();
        let data = build_fat_test_data(&[1, 0], &header);
        let diag = collecting_diag();
        let sink: Arc<dyn DiagnosticsSink> = diag.clone();
        let fat = Fat::build(&data, &header, &sink).unwrap();
        let chain = fat.follow_chain(0, &sink).unwrap();
        assert_eq!(chain, vec![0, 1]);
        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.kind == "CfbFATCycle"),
            "expected cycle warning, got: {warnings:?}",
        );
    }

    #[test]
    fn follow_chain_out_of_bounds() {
        // Entry 0 points to sector 999 which is beyond the FAT.
        let header = test_header();
        let data = build_fat_test_data(&[999], &header);
        let diag = collecting_diag();
        let sink: Arc<dyn DiagnosticsSink> = diag.clone();
        let fat = Fat::build(&data, &header, &sink).unwrap();
        let chain = fat.follow_chain(0, &sink).unwrap();
        assert_eq!(chain, vec![0]);
        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.kind == "CfbFATOutOfBounds"),
            "expected out-of-bounds warning, got: {warnings:?}",
        );
    }

    #[test]
    fn follow_chain_freesect_start() {
        let header = test_header();
        let data = build_fat_test_data(&[FREESECT], &header);
        let diag = null_diag();
        let fat = Fat::build(&data, &header, &diag).unwrap();
        let chain = fat.follow_chain(FREESECT, &diag).unwrap();
        assert!(chain.is_empty());
    }

    #[test]
    fn follow_chain_fatsect_mid_chain() {
        // Chain: 0 -> 1, entry[1] = FATSECT (should warn and break)
        let header = test_header();
        let data = build_fat_test_data(&[1, FATSECT], &header);
        let diag = collecting_diag();
        let sink: Arc<dyn DiagnosticsSink> = diag.clone();
        let fat = Fat::build(&data, &header, &sink).unwrap();
        let chain = fat.follow_chain(0, &sink).unwrap();
        assert_eq!(chain, vec![0, 1]);
        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.kind == "CfbFATBadEntry"),
            "expected bad-entry warning, got: {warnings:?}",
        );
    }

    #[test]
    fn build_fat_single_sector() {
        let header = test_header();
        let data = build_fat_test_data(&[1, ENDOFCHAIN, FREESECT], &header);
        let diag = null_diag();
        let fat = Fat::build(&data, &header, &diag).unwrap();
        assert_eq!(fat.get(0), Some(1));
        assert_eq!(fat.get(1), Some(ENDOFCHAIN));
        assert_eq!(fat.get(2), Some(FREESECT));
        // 512 / 4 = 128 entries per sector.
        assert_eq!(fat.entries.len(), 128);
    }

    #[test]
    fn build_fat_beyond_file() {
        let mut header = test_header();
        header.difat_entries = vec![9999]; // sector 9999 is way out
        let data = vec![0u8; header.sector_size as usize];
        let diag = collecting_diag();
        let sink: Arc<dyn DiagnosticsSink> = diag.clone();
        let fat = Fat::build(&data, &header, &sink).unwrap();
        assert_eq!(fat.entries.len(), 128);
        assert!(fat.entries.iter().all(|&e| e == FREESECT));
        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.kind == "CfbFatSectorTruncated"),
            "expected truncated warning, got: {warnings:?}",
        );
    }

    #[test]
    fn mini_fat_build_and_chain() {
        let mut header = test_header();
        header.first_mini_fat_sector = 1;
        header.mini_fat_sector_count = 1;

        let sector_size = header.sector_size as usize;
        let entries_per_sector = sector_size / 4;

        // header block + sector 0 (FAT) + sector 1 (mini-FAT data)
        let mut data = vec![0u8; sector_size * 3];

        // FAT in sector 0: entry 0 = FATSECT, entry 1 = ENDOFCHAIN, rest free.
        let fat_off = sector_size;
        data[fat_off..fat_off + 4].copy_from_slice(&FATSECT.to_le_bytes());
        data[fat_off + 4..fat_off + 8].copy_from_slice(&ENDOFCHAIN.to_le_bytes());
        for i in 2..entries_per_sector {
            let off = fat_off + i * 4;
            data[off..off + 4].copy_from_slice(&FREESECT.to_le_bytes());
        }

        // Mini-FAT in sector 1: chain 0 -> 1 -> 2 -> ENDOFCHAIN, rest free.
        let mf_off = sector_size * 2;
        data[mf_off..mf_off + 4].copy_from_slice(&1u32.to_le_bytes());
        data[mf_off + 4..mf_off + 8].copy_from_slice(&2u32.to_le_bytes());
        data[mf_off + 8..mf_off + 12].copy_from_slice(&ENDOFCHAIN.to_le_bytes());
        for i in 3..entries_per_sector {
            let off = mf_off + i * 4;
            data[off..off + 4].copy_from_slice(&FREESECT.to_le_bytes());
        }

        let diag = null_diag();
        let fat = Fat::build(&data, &header, &diag).unwrap();
        let mini_fat = MiniFat::build(&data, &header, &fat, &diag).unwrap();
        let chain = mini_fat.follow_chain(0, &diag).unwrap();
        assert_eq!(chain, vec![0, 1, 2]);
    }

    #[test]
    fn mini_fat_empty() {
        let header = test_header();
        let data = build_fat_test_data(&[FREESECT], &header);
        let diag = null_diag();
        let fat = Fat::build(&data, &header, &diag).unwrap();
        let mini_fat = MiniFat::build(&data, &header, &fat, &diag).unwrap();
        let chain = mini_fat.follow_chain(ENDOFCHAIN, &diag).unwrap();
        assert!(chain.is_empty());
    }

    // -- DIFAT continuation chain tests --

    /// Build file data with a DIFAT continuation chain.
    ///
    /// Layout: header (512 bytes) + 110 FAT sectors (each 512 bytes) + 1 DIFAT
    /// continuation sector (512 bytes). The header holds 109 DIFAT entries
    /// pointing to FAT sectors 0..109. The DIFAT continuation sector (sector
    /// 110) holds one entry pointing to FAT sector 109 (using 110 as the
    /// sector that stores it) plus ENDOFCHAIN as the next-pointer.
    fn build_difat_chain_file() -> (Vec<u8>, CfbHeader) {
        let ss = 512usize;
        let entries_per_sector = ss / 4; // 128
        let fat_sector_count: u32 = 110; // more than 109 header slots
        let difat_continuation_sector: u32 = 110; // sector after the 110 FAT sectors

        // Header DIFAT: sectors 0..109 (109 entries, fills the header).
        let difat_entries: Vec<u32> = (0..109).collect();

        let header = CfbHeader {
            version: CfbVersion::V3,
            sector_size: ss as u32,
            mini_sector_size: 64,
            mini_stream_cutoff: 4096,
            fat_sector_count,
            first_dir_sector: ENDOFCHAIN,
            first_mini_fat_sector: ENDOFCHAIN,
            mini_fat_sector_count: 0,
            first_difat_sector: difat_continuation_sector,
            difat_sector_count: 1,
            difat_entries,
        };

        let total_sectors = 111usize; // 110 FAT + 1 DIFAT continuation
        let mut data = vec![0u8; ss + total_sectors * ss]; // header + sectors

        // Write 110 FAT sectors (all FREESECT for simplicity).
        for sector_id in 0..110u32 {
            let offset = (sector_id as usize + 1) * ss;
            for i in 0..entries_per_sector {
                let off = offset + i * 4;
                data[off..off + 4].copy_from_slice(&FREESECT.to_le_bytes());
            }
        }

        // Write DIFAT continuation sector (sector 110).
        // It holds (entries_per_sector - 1) DIFAT entries + 1 next-pointer.
        let difat_off = (difat_continuation_sector as usize + 1) * ss;
        // First entry: FAT sector 109 (the 110th FAT sector).
        data[difat_off..difat_off + 4].copy_from_slice(&109u32.to_le_bytes());
        // Remaining entries: FREESECT.
        for i in 1..(entries_per_sector - 1) {
            let off = difat_off + i * 4;
            data[off..off + 4].copy_from_slice(&FREESECT.to_le_bytes());
        }
        // Last 4 bytes: next DIFAT sector pointer = ENDOFCHAIN.
        let next_off = difat_off + (entries_per_sector - 1) * 4;
        data[next_off..next_off + 4].copy_from_slice(&ENDOFCHAIN.to_le_bytes());

        (data, header)
    }

    #[test]
    fn difat_continuation_chain() {
        let (data, header) = build_difat_chain_file();
        let diag = null_diag();
        let fat = Fat::build(&data, &header, &diag).unwrap();
        // 109 from header + 1 from continuation = 110 FAT sectors.
        // Each FAT sector has 128 entries -> 110 * 128 = 14080 total entries.
        assert_eq!(fat.entries.len(), 110 * 128);
    }

    #[test]
    fn difat_continuation_cycle_detected() {
        let ss = 512usize;
        let entries_per_sector = ss / 4;

        // DIFAT continuation sector points back to itself.
        let difat_sector: u32 = 109;
        let header = CfbHeader {
            version: CfbVersion::V3,
            sector_size: ss as u32,
            mini_sector_size: 64,
            mini_stream_cutoff: 4096,
            fat_sector_count: 110,
            first_dir_sector: ENDOFCHAIN,
            first_mini_fat_sector: ENDOFCHAIN,
            mini_fat_sector_count: 0,
            first_difat_sector: difat_sector,
            difat_sector_count: 2, // claims 2, but cycle should stop it
            difat_entries: (0..109).collect(),
        };

        let total_sectors = 110usize;
        let mut data = vec![0u8; ss + total_sectors * ss];

        // Write FAT sectors (FREESECT).
        for sector_id in 0..109u32 {
            let offset = (sector_id as usize + 1) * ss;
            for i in 0..entries_per_sector {
                let off = offset + i * 4;
                data[off..off + 4].copy_from_slice(&FREESECT.to_le_bytes());
            }
        }

        // DIFAT continuation sector (sector 109): one entry + self-referencing next-pointer.
        let difat_off = (difat_sector as usize + 1) * ss;
        data[difat_off..difat_off + 4].copy_from_slice(&109u32.to_le_bytes()); // entry
        for i in 1..(entries_per_sector - 1) {
            let off = difat_off + i * 4;
            data[off..off + 4].copy_from_slice(&FREESECT.to_le_bytes());
        }
        // Next pointer = same sector (cycle).
        let next_off = difat_off + (entries_per_sector - 1) * 4;
        data[next_off..next_off + 4].copy_from_slice(&difat_sector.to_le_bytes());

        let diag = collecting_diag();
        let sink: Arc<dyn DiagnosticsSink> = diag.clone();
        let fat = Fat::build(&data, &header, &sink).unwrap();
        // Should have gotten the FAT sectors from header (109) + one from
        // the first visit of the DIFAT sector before cycle detection kicks in.
        assert!(fat.entries.len() >= 109 * 128);

        let warnings = diag.warnings();
        let has_cycle = warnings.iter().any(|w| w.kind == "CfbDifatCycle");
        assert!(
            has_cycle,
            "expected CfbDifatCycle warning, got: {warnings:?}",
        );
    }

    #[test]
    fn difat_chain_too_long() {
        let ss = 512usize;
        let entries_per_sector = ss / 4;

        // Set difat_sector_count to 2 so we cap iteration at 2.
        let header = CfbHeader {
            version: CfbVersion::V3,
            sector_size: ss as u32,
            mini_sector_size: 64,
            mini_stream_cutoff: 4096,
            fat_sector_count: 111,
            first_dir_sector: ENDOFCHAIN,
            first_mini_fat_sector: ENDOFCHAIN,
            mini_fat_sector_count: 0,
            first_difat_sector: 109,
            difat_sector_count: 2,
            difat_entries: (0..109).collect(),
        };

        let total_sectors = 112usize;
        let mut data = vec![0u8; ss + total_sectors * ss];

        for sector_id in 0..109u32 {
            let offset = (sector_id as usize + 1) * ss;
            for i in 0..entries_per_sector {
                let off = offset + i * 4;
                data[off..off + 4].copy_from_slice(&FREESECT.to_le_bytes());
            }
        }

        // DIFAT sector 109 -> next = 110.
        let difat_off_0 = (109 + 1) * ss;
        data[difat_off_0..difat_off_0 + 4].copy_from_slice(&109u32.to_le_bytes());
        for i in 1..(entries_per_sector - 1) {
            let off = difat_off_0 + i * 4;
            data[off..off + 4].copy_from_slice(&FREESECT.to_le_bytes());
        }
        let next_off = difat_off_0 + (entries_per_sector - 1) * 4;
        data[next_off..next_off + 4].copy_from_slice(&110u32.to_le_bytes());

        // DIFAT sector 110 -> next = 111 (extends beyond limit of 2).
        let difat_off_1 = (110 + 1) * ss;
        data[difat_off_1..difat_off_1 + 4].copy_from_slice(&110u32.to_le_bytes());
        for i in 1..(entries_per_sector - 1) {
            let off = difat_off_1 + i * 4;
            data[off..off + 4].copy_from_slice(&FREESECT.to_le_bytes());
        }
        let next_off_1 = difat_off_1 + (entries_per_sector - 1) * 4;
        data[next_off_1..next_off_1 + 4].copy_from_slice(&111u32.to_le_bytes());

        let diag = collecting_diag();
        let sink: Arc<dyn DiagnosticsSink> = diag.clone();
        let fat = Fat::build(&data, &header, &sink).unwrap();
        // Only 2 DIFAT sectors processed (the limit).
        assert!(!fat.entries.is_empty());

        let warnings = diag.warnings();
        let has_too_long = warnings.iter().any(|w| w.kind == "CfbDifatChainTooLong");
        assert!(
            has_too_long,
            "expected CfbDifatChainTooLong warning, got: {warnings:?}",
        );
    }
}
