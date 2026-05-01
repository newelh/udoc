//! CFB stream reader.
//!
//! Reads stream data for directory entries, routing between regular FAT
//! sectors and the mini-stream based on entry size and type.

use std::sync::Arc;

use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use super::directory::{DirEntry, EntryType};
use super::fat::{Fat, MiniFat};
use super::header::CfbHeader;
use crate::error::{Error, Result, ResultExt};

/// Groups the archive-level state needed for stream reads.
pub(super) struct StreamContext<'a> {
    pub data: &'a [u8],
    pub header: &'a CfbHeader,
    pub fat: &'a Fat,
    pub mini_fat: &'a MiniFat,
    pub mini_stream: &'a [u8],
    pub max_stream_size: u64,
    pub diag: &'a Arc<dyn DiagnosticsSink>,
}

/// Read the stream data for a directory entry.
///
/// Routes to regular FAT sectors or mini-stream based on entry type and size.
/// Root entries always read from FAT (the root entry's stream is the
/// mini-stream container itself).
pub(super) fn read_stream(ctx: &StreamContext, entry: &DirEntry) -> Result<Vec<u8>> {
    if entry.size == 0 {
        return Ok(Vec::new());
    }

    // Check configured limit first (policy), then try_into checks platform
    // addressability (e.g. u64 stream size on 32-bit where usize is u32).
    if entry.size > ctx.max_stream_size {
        return Err(Error::resource_limit(format!(
            "stream '{}' size {} exceeds limit of {} bytes",
            entry.name, entry.size, ctx.max_stream_size
        )));
    }

    // Root entry's stream is the mini-stream container; always use regular FAT.
    if entry.entry_type == EntryType::RootEntry {
        return read_regular_stream(ctx.data, ctx.header, ctx.fat, entry, ctx.diag)
            .context(format!("reading root entry stream '{}'", entry.name));
    }

    if entry.size < ctx.header.mini_stream_cutoff as u64 {
        read_mini_stream(ctx.header, ctx.mini_fat, ctx.mini_stream, entry, ctx.diag)
            .context(format!("reading mini-stream '{}'", entry.name))
    } else {
        read_regular_stream(ctx.data, ctx.header, ctx.fat, entry, ctx.diag)
            .context(format!("reading regular stream '{}'", entry.name))
    }
}

/// Read the root entry's stream (the mini-stream container).
///
/// Called during CfbArchive construction to materialize the mini-stream
/// backing store. Same logic as `read_regular_stream`.
pub(super) fn read_root_stream(
    data: &[u8],
    header: &CfbHeader,
    fat: &Fat,
    root_entry: &DirEntry,
    max_stream_size: u64,
    diag: &Arc<dyn DiagnosticsSink>,
) -> Result<Vec<u8>> {
    if root_entry.size == 0 {
        return Ok(Vec::new());
    }

    if root_entry.size > max_stream_size {
        return Err(Error::resource_limit(format!(
            "root stream size {} exceeds limit of {} bytes",
            root_entry.size, max_stream_size
        )));
    }

    read_regular_stream(data, header, fat, root_entry, diag)
        .context("reading root entry stream (mini-stream container)")
}

/// Convert entry size to usize with platform-safety check.
fn checked_target_size(entry: &DirEntry) -> Result<usize> {
    entry.size.try_into().map_err(|_| {
        Error::resource_limit(format!(
            "stream size {} exceeds addressable limit on this platform",
            entry.size,
        ))
    })
}

/// Read stream data from regular FAT sectors.
fn read_regular_stream(
    data: &[u8],
    header: &CfbHeader,
    fat: &Fat,
    entry: &DirEntry,
    diag: &Arc<dyn DiagnosticsSink>,
) -> Result<Vec<u8>> {
    let chain = fat
        .follow_chain(entry.start_sector, diag)
        .context("following FAT chain")?;
    let target_size = checked_target_size(entry)?;
    let params = GatherParams {
        source: data,
        chain: &chain,
        sector_size: header.sector_size as u64,
        target_size,
        label: "Stream",
        entry_name: &entry.name,
        diag,
    };
    Ok(gather_sectors(&params, |id| header.sector_offset(id)))
}

/// Read stream data from the mini-stream.
fn read_mini_stream(
    header: &CfbHeader,
    mini_fat: &MiniFat,
    mini_stream: &[u8],
    entry: &DirEntry,
    diag: &Arc<dyn DiagnosticsSink>,
) -> Result<Vec<u8>> {
    let chain = mini_fat
        .follow_chain(entry.start_sector, diag)
        .context("following mini-FAT chain")?;
    let target_size = checked_target_size(entry)?;
    let mini_sector_size = header.mini_sector_size as u64;
    let params = GatherParams {
        source: mini_stream,
        chain: &chain,
        sector_size: mini_sector_size,
        target_size,
        label: "MiniStream",
        entry_name: &entry.name,
        diag,
    };
    Ok(gather_sectors(&params, |id| {
        Some(id as u64 * mini_sector_size)
    }))
}

/// Parameters for `gather_sectors` that describe the read geometry.
struct GatherParams<'a> {
    source: &'a [u8],
    chain: &'a [u32],
    sector_size: u64,
    target_size: usize,
    /// Warning label prefix, e.g. "Stream" or "MiniStream".
    label: &'a str,
    entry_name: &'a str,
    diag: &'a Arc<dyn DiagnosticsSink>,
}

/// Gather data from a chain of sectors into a single buffer.
///
/// Shared by regular stream reads (sectors in the file) and mini-stream reads
/// (mini-sectors in the materialized mini-stream). The `offset_fn` maps a
/// sector ID to its byte offset in `source`, returning `None` on overflow.
fn gather_sectors(params: &GatherParams<'_>, offset_fn: impl Fn(u32) -> Option<u64>) -> Vec<u8> {
    let GatherParams {
        source,
        chain,
        sector_size,
        target_size,
        label,
        entry_name,
        diag,
    } = params;
    let target_size = *target_size;
    let sector_size = *sector_size;

    let mut output = Vec::with_capacity(target_size);
    let source_len = source.len() as u64;

    for &sector_id in *chain {
        let offset = match offset_fn(sector_id) {
            Some(off) => off,
            None => {
                diag.warning(Warning::new(
                    format!("Cfb{label}Overflow"),
                    format!("sector offset overflow for sector {sector_id}, stopping chain"),
                ));
                break;
            }
        };

        if offset >= source_len {
            diag.warning(Warning::new(
                format!("Cfb{label}OutOfBounds"),
                format!(
                    "sector {sector_id} at offset {offset} beyond end ({source_len}), \
                     stopping chain"
                ),
            ));
            break;
        }

        let end = offset + sector_size;
        let available_end = end.min(source_len) as usize;
        let offset_usize = offset as usize;

        if end > source_len {
            diag.warning(Warning::new(
                format!("Cfb{label}Truncated"),
                format!(
                    "sector {sector_id} truncated: expected {sector_size} bytes, got {} bytes",
                    available_end - offset_usize
                ),
            ));
        }

        let remaining = target_size - output.len();
        let chunk = &source[offset_usize..available_end];
        output.extend_from_slice(&chunk[..chunk.len().min(remaining)]);

        if output.len() >= target_size {
            break;
        }
    }

    if output.len() < target_size {
        diag.warning(Warning::new(
            format!("Cfb{label}Short"),
            format!(
                "'{}' short: expected {} bytes, got {}",
                entry_name,
                target_size,
                output.len()
            ),
        ));
    }

    output
}

#[cfg(test)]
mod tests {
    use super::super::fat::ENDOFCHAIN;
    use super::super::header::CfbVersion;
    use super::*;
    use udoc_core::diagnostics::NullDiagnostics;

    const DEFAULT_MAX: u64 = 250 * 1024 * 1024;

    fn diag() -> Arc<dyn DiagnosticsSink> {
        Arc::new(NullDiagnostics)
    }

    fn make_header(sector_size: u32) -> CfbHeader {
        CfbHeader {
            version: CfbVersion::V3,
            sector_size,
            mini_sector_size: 64,
            mini_stream_cutoff: 4096,
            fat_sector_count: 1,
            first_dir_sector: 0,
            first_mini_fat_sector: ENDOFCHAIN,
            mini_fat_sector_count: 0,
            first_difat_sector: ENDOFCHAIN,
            difat_sector_count: 0,
            difat_entries: vec![],
        }
    }

    fn make_entry(name: &str, entry_type: EntryType, start_sector: u32, size: u64) -> DirEntry {
        DirEntry {
            name: name.to_string(),
            path: name.to_string(),
            entry_type,
            size,
            start_sector,
        }
    }

    fn make_fat(entries: &[u32]) -> Fat {
        Fat::from_entries_for_test(entries.to_vec())
    }

    fn make_mini_fat(entries: &[u32]) -> MiniFat {
        MiniFat::from_entries_for_test(entries.to_vec())
    }

    /// Build file data: 1 header-sized block + N data sectors.
    /// Each sector is padded to sector_size with zeros.
    fn build_file_data(sector_size: usize, sector_data: &[&[u8]]) -> Vec<u8> {
        let mut data = vec![0u8; sector_size]; // header block
        for &sector in sector_data {
            let mut padded = sector.to_vec();
            padded.resize(sector_size, 0);
            data.extend_from_slice(&padded);
        }
        data
    }

    #[test]
    fn read_regular_stream_simple() {
        let header = make_header(512);
        let d = diag();

        let payload: Vec<u8> = (0..100).collect();
        let file_data = build_file_data(512, &[&payload]);

        let fat = make_fat(&[ENDOFCHAIN]);
        let entry = make_entry("Test", EntryType::Stream, 0, 100);

        let result = read_regular_stream(&file_data, &header, &fat, &entry, &d).unwrap();
        assert_eq!(result.len(), 100);
        assert_eq!(result, payload);
    }

    #[test]
    fn read_regular_stream_multi_sector() {
        let header = make_header(512);
        let d = diag();

        let s0: Vec<u8> = vec![0xAA; 512];
        let s1: Vec<u8> = vec![0xBB; 512];
        let s2: Vec<u8> = vec![0xCC; 512];
        let file_data = build_file_data(512, &[&s0, &s1, &s2]);

        // FAT chain: 0 -> 1 -> 2 -> ENDOFCHAIN
        let fat = make_fat(&[1, 2, ENDOFCHAIN]);
        let entry = make_entry("Multi", EntryType::Stream, 0, 1200);

        let result = read_regular_stream(&file_data, &header, &fat, &entry, &d).unwrap();
        assert_eq!(result.len(), 1200);
        assert_eq!(&result[..512], &[0xAA; 512]);
        assert_eq!(&result[512..1024], &[0xBB; 512]);
        assert_eq!(&result[1024..1200], &[0xCC; 176]);
    }

    #[test]
    fn read_empty_stream() {
        let header = make_header(512);
        let d = diag();
        let fat = make_fat(&[ENDOFCHAIN]);
        let mini_fat = make_mini_fat(&[]);
        let entry = make_entry("Empty", EntryType::Stream, 0, 0);

        let ctx = StreamContext {
            data: &[],
            header: &header,
            fat: &fat,
            mini_fat: &mini_fat,
            mini_stream: &[],
            max_stream_size: DEFAULT_MAX,
            diag: &d,
        };
        let result = read_stream(&ctx, &entry).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn read_mini_stream_simple() {
        let header = make_header(512);
        let d = diag();

        let payload: Vec<u8> = (0..50).collect();
        let mut mini_stream_data = payload.clone();
        mini_stream_data.resize(64, 0);

        let mini_fat = make_mini_fat(&[ENDOFCHAIN]);
        let entry = make_entry("Small", EntryType::Stream, 0, 50);

        let result = read_mini_stream(&header, &mini_fat, &mini_stream_data, &entry, &d).unwrap();
        assert_eq!(result.len(), 50);
        assert_eq!(result, payload);
    }

    #[test]
    fn read_mini_stream_multi_sector() {
        let header = make_header(512);
        let d = diag();

        let mut mini_stream_data = Vec::new();
        mini_stream_data.extend_from_slice(&[0x11; 64]);
        mini_stream_data.extend_from_slice(&[0x22; 64]);
        mini_stream_data.extend_from_slice(&[0x33; 64]);

        // Mini-FAT chain: 0 -> 1 -> 2 -> ENDOFCHAIN
        let mini_fat = make_mini_fat(&[1, 2, ENDOFCHAIN]);
        let entry = make_entry("MiniMulti", EntryType::Stream, 0, 150);

        let result = read_mini_stream(&header, &mini_fat, &mini_stream_data, &entry, &d).unwrap();
        assert_eq!(result.len(), 150);
        assert_eq!(&result[..64], &[0x11; 64]);
        assert_eq!(&result[64..128], &[0x22; 64]);
        assert_eq!(&result[128..150], &[0x33; 22]);
    }

    #[test]
    fn read_stream_at_cutoff_uses_regular_fat() {
        let header = make_header(512);
        let d = diag();

        // Stream exactly 4096 bytes (== cutoff), should use regular FAT.
        let sectors: Vec<Vec<u8>> = (0..8).map(|i| vec![i as u8; 512]).collect();
        let sector_refs: Vec<&[u8]> = sectors.iter().map(|s| s.as_slice()).collect();
        let file_data = build_file_data(512, &sector_refs);

        let fat = make_fat(&[1, 2, 3, 4, 5, 6, 7, ENDOFCHAIN]);
        let mini_fat = make_mini_fat(&[]);
        let entry = make_entry("AtCutoff", EntryType::Stream, 0, 4096);

        // size == cutoff, NOT < cutoff, so it goes through regular FAT.
        let ctx = StreamContext {
            data: &file_data,
            header: &header,
            fat: &fat,
            mini_fat: &mini_fat,
            mini_stream: &[],
            max_stream_size: DEFAULT_MAX,
            diag: &d,
        };
        let result = read_stream(&ctx, &entry).unwrap();
        assert_eq!(result.len(), 4096);
        assert_eq!(&result[..512], &[0x00; 512]);
        assert_eq!(&result[3584..4096], &[0x07; 512]);
    }

    #[test]
    fn read_truncated_sector() {
        let header = make_header(512);
        let d = diag();

        // File: header (512 bytes) + partial sector (only 256 bytes).
        let mut file_data = vec![0u8; 512];
        file_data.extend_from_slice(&vec![0xFF; 256]);

        let fat = make_fat(&[ENDOFCHAIN]);
        let entry = make_entry("Trunc", EntryType::Stream, 0, 400);

        let result = read_regular_stream(&file_data, &header, &fat, &entry, &d).unwrap();
        // Only 256 bytes available from partial sector; entry declares 400.
        assert_eq!(result.len(), 256);
    }

    #[test]
    fn read_stream_size_limit() {
        let header = make_header(512);
        let d = diag();
        let fat = make_fat(&[ENDOFCHAIN]);
        let mini_fat = make_mini_fat(&[]);
        let entry = make_entry("Huge", EntryType::Stream, 0, DEFAULT_MAX + 1);

        let ctx = StreamContext {
            data: &[],
            header: &header,
            fat: &fat,
            mini_fat: &mini_fat,
            mini_stream: &[],
            max_stream_size: DEFAULT_MAX,
            diag: &d,
        };
        let result = read_stream(&ctx, &entry);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("resource limit"), "got: {err}");
    }

    #[test]
    fn read_root_entry_via_fat() {
        let header = make_header(512);
        let d = diag();

        // Root entry with size < cutoff still uses regular FAT (not mini-stream).
        let payload: Vec<u8> = vec![0xDE; 100];
        let file_data = build_file_data(512, &[&payload]);

        let fat = make_fat(&[ENDOFCHAIN]);
        let mini_fat = make_mini_fat(&[]);
        let entry = make_entry("Root Entry", EntryType::RootEntry, 0, 100);

        let ctx = StreamContext {
            data: &file_data,
            header: &header,
            fat: &fat,
            mini_fat: &mini_fat,
            mini_stream: &[],
            max_stream_size: DEFAULT_MAX,
            diag: &d,
        };
        let result = read_stream(&ctx, &entry).unwrap();
        assert_eq!(result.len(), 100);
        assert_eq!(result, vec![0xDE; 100]);
    }
}
