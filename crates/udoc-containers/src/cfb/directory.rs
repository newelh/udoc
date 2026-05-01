//! CFB directory entry parsing.
//!
//! Parses 128-byte directory entry slots from the directory sector chain,
//! filters out unallocated entries, and reconstructs paths by walking
//! child/sibling IDs. Red-black tree invariants are ignored.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use super::fat::Fat;
use super::header::{read_u16_le, read_u32_le, read_u64_le, CfbHeader, CfbVersion};
use crate::error::{Result, ResultExt};
use crate::Error;

/// Sentinel value indicating no sibling or child.
const NOSTREAM: u32 = 0xFFFF_FFFF;

/// Maximum tree depth for path building (prevents stack-like blowup).
const MAX_TREE_DEPTH: usize = 256;

/// Type of a directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    /// Storage (folder-like container).
    Storage,
    /// Stream (readable data).
    Stream,
    /// Root entry (contains mini-stream).
    RootEntry,
}

/// A parsed directory entry.
#[derive(Debug, Clone)]
pub struct DirEntry {
    /// Entry name decoded from UTF-16LE.
    pub name: String,
    /// Full path from root (e.g. "Workbook", "ObjectPool/obj1").
    pub path: String,
    /// Entry type.
    pub entry_type: EntryType,
    /// Stream size in bytes.
    pub size: u64,
    /// Starting sector (FAT sector for large streams, mini-FAT sector for small).
    pub(super) start_sector: u32,
}

/// Internal representation of a raw 128-byte directory entry slot.
struct RawDirEntry {
    name: String,
    entry_type: u8,
    left_sibling: u32,
    right_sibling: u32,
    child_id: u32,
    start_sector: u32,
    size: u64,
    /// Original slot index from the directory sector data. Used by
    /// `build_entries` to build the `raw_by_index` lookup table, since
    /// type-0 (unallocated) entries are filtered out and vec positions
    /// no longer match slot positions.
    index: u32,
}

/// Parse raw directory entries from concatenated directory sector bytes.
///
/// Reads 128-byte slots, decodes UTF-16LE names, and filters out type-0
/// (unknown/unallocated) entries. For v3 files, the high 32 bits of the
/// stream size are masked to zero.
fn parse_raw_entries(
    dir_data: &[u8],
    version: CfbVersion,
    max_dir_entries: usize,
    diag: &Arc<dyn DiagnosticsSink>,
) -> Vec<RawDirEntry> {
    let slot_count = dir_data.len() / 128;
    let cap = slot_count.min(max_dir_entries);
    let mut entries = Vec::with_capacity(cap);

    for i in 0..slot_count {
        if entries.len() >= max_dir_entries {
            diag.warning(Warning::new(
                "CfbDirEntryLimit",
                format!("directory entry count exceeds limit {max_dir_entries}, truncating"),
            ));
            break;
        }

        let base = i * 128;
        let slot = &dir_data[base..base + 128];

        // Object type at offset 0x42.
        let obj_type = slot[0x42];
        if obj_type == 0 {
            continue;
        }

        // Name length at offset 0x40 (byte count including null terminator).
        let name_len_bytes = match read_u16_le(slot, 0x40) {
            Ok(v) => v as usize,
            Err(_) => continue,
        };
        if name_len_bytes == 0 || name_len_bytes > 64 {
            diag.warning(Warning::new(
                "CfbDirEntryBadNameLen",
                format!("directory entry {i} has invalid name length {name_len_bytes}, skipping"),
            ));
            continue;
        }
        if name_len_bytes % 2 != 0 {
            diag.warning(Warning::new(
                "CfbDirEntryOddNameLen",
                format!(
                    "directory entry {i} has odd name byte count {name_len_bytes}, \
                     truncating to even"
                ),
            ));
        }

        // Decode UTF-16LE name. Char count = byte_count / 2 - 1 (subtract null).
        let char_count = name_len_bytes / 2;
        let char_count = if char_count > 0 { char_count - 1 } else { 0 };
        let mut u16_buf = Vec::with_capacity(char_count);
        for j in 0..char_count {
            let lo = slot[j * 2] as u16;
            let hi = slot[j * 2 + 1] as u16;
            u16_buf.push(lo | (hi << 8));
        }
        let name = String::from_utf16_lossy(&u16_buf);

        let left_sibling = read_u32_le(slot, 0x44).unwrap_or(NOSTREAM);
        let right_sibling = read_u32_le(slot, 0x48).unwrap_or(NOSTREAM);
        let child_id = read_u32_le(slot, 0x4C).unwrap_or(NOSTREAM);
        let start_sector = read_u32_le(slot, 0x74).unwrap_or(0);

        // Stream size: 8 bytes at offset 0x78 (u64 LE).
        let mut size = read_u64_le(slot, 0x78).unwrap_or(0);

        // For v3: mask high 32 bits to zero (legacy writers leave garbage).
        if version == CfbVersion::V3 {
            size &= 0xFFFF_FFFF;
        }

        entries.push(RawDirEntry {
            name,
            entry_type: obj_type,
            left_sibling,
            right_sibling,
            child_id,
            start_sector,
            size,
            index: i as u32,
        });
    }

    entries
}

/// Map raw entry type byte to public EntryType. Returns None for unknown types.
fn map_entry_type(raw: u8) -> Option<EntryType> {
    match raw {
        1 => Some(EntryType::Storage),
        2 => Some(EntryType::Stream),
        5 => Some(EntryType::RootEntry),
        _ => None,
    }
}

/// Shared immutable context for directory tree traversal.
struct TraversalContext<'a> {
    raw_entries: &'a [RawDirEntry],
    raw_by_index: &'a HashMap<u32, usize>,
    diag: &'a Arc<dyn DiagnosticsSink>,
}

/// Walk the sibling tree rooted at `root_node_id` (in-order traversal) and
/// collect DirEntry values with computed paths.
///
/// Sibling traversal is iterative (explicit stack) to prevent stack overflow
/// on degenerate trees (e.g., 100K entries in a right-only chain). Only
/// parent->child transitions use recursion, bounded by MAX_TREE_DEPTH.
fn collect_children(
    ctx: &TraversalContext<'_>,
    parent_path: &str,
    root_node_id: u32,
    result: &mut Vec<DirEntry>,
    visited: &mut HashSet<u32>,
    depth: usize,
) {
    if root_node_id == NOSTREAM {
        return;
    }
    if depth > MAX_TREE_DEPTH {
        ctx.diag.warning(Warning::new(
            "CfbDirTreeTooDeep",
            format!("directory tree depth exceeds {MAX_TREE_DEPTH}, stopping traversal"),
        ));
        return;
    }

    // Iterative in-order traversal of the sibling BST.
    let mut stack: Vec<u32> = Vec::new();
    let mut current = root_node_id;

    loop {
        // Descend left, pushing nodes onto the stack.
        while current != NOSTREAM {
            match ctx.raw_by_index.get(&current) {
                Some(&pos) => {
                    if !visited.insert(current) {
                        ctx.diag.warning(Warning::new(
                            "CfbDirSiblingCycle",
                            format!(
                                "directory entry {current} already visited \
                                 (cycle or duplicate reference)"
                            ),
                        ));
                        // Breaks the inner while only. Nodes already on
                        // the stack are safe to process: they're marked
                        // visited so they won't be re-pushed.
                        break;
                    }
                    stack.push(current);
                    current = ctx.raw_entries[pos].left_sibling;
                }
                None => {
                    ctx.diag.warning(Warning::new(
                        "CfbDirEntryOutOfBounds",
                        format!("directory entry references invalid index {current}"),
                    ));
                    break;
                }
            }
        }

        // Pop the next node to process.
        let node_id = match stack.pop() {
            Some(id) => id,
            None => break,
        };

        let pos = match ctx.raw_by_index.get(&node_id) {
            Some(&p) => p,
            None => continue,
        };
        let entry = &ctx.raw_entries[pos];

        // Process this node.
        if let Some(etype) = map_entry_type(entry.entry_type) {
            let path = if parent_path.is_empty() {
                entry.name.clone()
            } else {
                format!("{parent_path}/{}", entry.name)
            };

            result.push(DirEntry {
                name: entry.name.clone(),
                path: path.clone(),
                entry_type: etype,
                size: entry.size,
                start_sector: entry.start_sector,
            });

            // Recurse into child subtree (depth-guarded, max 256 levels).
            if etype == EntryType::Storage {
                collect_children(ctx, &path, entry.child_id, result, visited, depth + 1);
            } else if entry.child_id != NOSTREAM {
                ctx.diag.warning(Warning::new(
                    "CfbDirStreamHasChild",
                    format!(
                        "stream '{}' has child_id {} but streams cannot have children",
                        entry.name, entry.child_id,
                    ),
                ));
            }
        } else {
            ctx.diag.warning(Warning::new(
                "CfbDirUnknownType",
                format!(
                    "directory entry '{}' has unknown type {}, skipping",
                    entry.name, entry.entry_type,
                ),
            ));
        }

        // Continue with right sibling.
        current = entry.right_sibling;
    }
}

/// Build DirEntry list and case-insensitive lookup index from raw entries.
///
/// Entry 0 is the root. Its child_id is the root of the top-level sibling tree.
/// The root entry itself gets path "" and is included in the result.
fn build_entries(
    raw: &[RawDirEntry],
    diag: &Arc<dyn DiagnosticsSink>,
) -> (Vec<DirEntry>, HashMap<String, usize>) {
    let mut result = Vec::new();

    if raw.is_empty() {
        return (result, HashMap::new());
    }

    // Build index from slot position -> vec position for O(1) lookup.
    let mut raw_by_index: HashMap<u32, usize> = HashMap::with_capacity(raw.len());
    for (i, entry) in raw.iter().enumerate() {
        raw_by_index.insert(entry.index, i);
    }

    // Find the root entry (slot index 0).
    let root_pos = match raw_by_index.get(&0) {
        Some(&p) => p,
        None => {
            diag.warning(Warning::new(
                "CfbDirNoRoot",
                "no root entry (index 0) found in directory",
            ));
            return (result, HashMap::new());
        }
    };

    let root = &raw[root_pos];

    // Add root entry with empty path.
    if let Some(etype) = map_entry_type(root.entry_type) {
        result.push(DirEntry {
            name: root.name.clone(),
            path: String::new(),
            entry_type: etype,
            size: root.size,
            start_sector: root.start_sector,
        });
    }

    // Walk children of root.
    let ctx = TraversalContext {
        raw_entries: raw,
        raw_by_index: &raw_by_index,
        diag,
    };
    let mut visited = HashSet::new();
    visited.insert(0u32);
    collect_children(&ctx, "", root.child_id, &mut result, &mut visited, 1);

    // Build case-insensitive index (uppercase path -> position in result vec).
    // Skip the root entry (empty path) since it is not a named stream/storage.
    let mut index = HashMap::with_capacity(result.len());
    for (i, entry) in result.iter().enumerate() {
        if !entry.path.is_empty() {
            index.insert(entry.path.to_ascii_uppercase(), i);
        }
    }

    (result, index)
}

/// Parse directory entries from the CFB file.
///
/// Follows the directory sector chain, parses 128-byte entry slots, filters
/// out unallocated entries, and builds paths by walking sibling/child IDs.
///
/// Returns (entry list, case-insensitive path index).
pub(super) fn parse_directory(
    data: &[u8],
    header: &CfbHeader,
    fat: &Fat,
    max_dir_entries: usize,
    diag: &Arc<dyn DiagnosticsSink>,
) -> Result<(Vec<DirEntry>, HashMap<String, usize>)> {
    let chain = fat
        .follow_chain(header.first_dir_sector, diag)
        .context("following directory sector chain")?;

    if chain.is_empty() {
        return Err(Error::cfb("directory sector chain is empty"));
    }

    // Read directory sector bytes.
    let sector_size = header.sector_size as usize;
    let mut dir_data = Vec::with_capacity(chain.len() * sector_size);
    for &sector_id in &chain {
        let offset = header
            .sector_offset(sector_id)
            .ok_or_else(|| Error::cfb(format!("sector offset overflow for sector {sector_id}")))?
            as usize;
        let end = offset + sector_size;
        if end > data.len() {
            diag.warning(Warning::new(
                "CfbDirSectorTruncated",
                format!(
                    "directory sector {sector_id} at offset {offset} extends beyond file end {}",
                    data.len()
                ),
            ));
            let available = if offset < data.len() {
                data.len() - offset
            } else {
                0
            };
            if available > 0 {
                dir_data.extend_from_slice(&data[offset..offset + available]);
            }
            dir_data.resize(dir_data.len() + sector_size - available, 0);
        } else {
            dir_data.extend_from_slice(&data[offset..end]);
        }
    }

    let raw = parse_raw_entries(&dir_data, header.version, max_dir_entries, diag);
    let (entries, index) = build_entries(&raw, diag);

    Ok((entries, index))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::build_cfb_dir_entry as build_dir_entry;
    use udoc_core::diagnostics::{CollectingDiagnostics, NullDiagnostics};

    const NF: u32 = NOSTREAM;

    fn root_entry(child: u32) -> [u8; 128] {
        build_dir_entry("Root Entry", 5, child, NF, NF, 0, 0)
    }

    fn null_diag() -> Arc<dyn DiagnosticsSink> {
        Arc::new(NullDiagnostics)
    }

    fn collecting_diag() -> Arc<CollectingDiagnostics> {
        Arc::new(CollectingDiagnostics::new())
    }

    #[test]
    fn parse_single_root() {
        let mut dir_data = Vec::new();
        dir_data.extend_from_slice(&root_entry(NF));
        let raw = parse_raw_entries(&dir_data, CfbVersion::V3, 100_000, &null_diag());
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].name, "Root Entry");
        assert_eq!(raw[0].entry_type, 5);

        let (entries, index) = build_entries(&raw, &null_diag());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Root Entry");
        assert_eq!(entries[0].entry_type, EntryType::RootEntry);
        assert_eq!(entries[0].path, "");
        // Root entry is not indexed (empty path); use entries() to access it.
        assert!(!index.contains_key(""));
    }

    #[test]
    fn parse_root_with_streams() {
        // Root (index 0) -> child_id = 2 (middle sibling).
        // Entry 1: "Alpha", left=NOSTREAM, right=NOSTREAM.
        // Entry 2: "Beta", left=1, right=3.
        // Entry 3: "Gamma", left=NOSTREAM, right=NOSTREAM.
        let mut dir_data = Vec::new();
        dir_data.extend_from_slice(&root_entry(2));
        dir_data.extend_from_slice(&build_dir_entry("Alpha", 2, NF, NF, NF, 10, 100));
        dir_data.extend_from_slice(&build_dir_entry("Beta", 2, NF, 1, 3, 20, 200));
        dir_data.extend_from_slice(&build_dir_entry("Gamma", 2, NF, NF, NF, 30, 300));

        let raw = parse_raw_entries(&dir_data, CfbVersion::V3, 100_000, &null_diag());
        assert_eq!(raw.len(), 4);

        let (entries, _index) = build_entries(&raw, &null_diag());
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].entry_type, EntryType::RootEntry);

        // In-order: Alpha, Beta, Gamma.
        let stream_names: Vec<&str> = entries[1..].iter().map(|e| e.name.as_str()).collect();
        assert_eq!(stream_names, vec!["Alpha", "Beta", "Gamma"]);
        assert_eq!(entries[1].path, "Alpha");
        assert_eq!(entries[2].path, "Beta");
        assert_eq!(entries[3].path, "Gamma");
    }

    #[test]
    fn parse_utf16_name() {
        let mut dir_data = Vec::new();
        dir_data.extend_from_slice(&build_dir_entry(
            "Workbook", 2, NOSTREAM, NOSTREAM, NOSTREAM, 5, 4096,
        ));
        let raw = parse_raw_entries(&dir_data, CfbVersion::V3, 100_000, &null_diag());
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].name, "Workbook");
    }

    #[test]
    fn parse_v3_stream_size_mask() {
        // V3: high 32 bits should be masked to zero.
        let size: u64 = 0xDEAD_BEEF_0000_1000;
        let mut dir_data = Vec::new();
        dir_data.extend_from_slice(&build_dir_entry("Root Entry", 5, NF, NF, NF, 0, size));

        let raw = parse_raw_entries(&dir_data, CfbVersion::V3, 100_000, &null_diag());
        assert_eq!(raw.len(), 1);
        assert_eq!(
            raw[0].size, 0x0000_1000,
            "high 32 bits should be masked off for v3"
        );

        // V4 should preserve the full 64-bit size.
        let raw_v4 = parse_raw_entries(&dir_data, CfbVersion::V4, 100_000, &null_diag());
        assert_eq!(raw_v4[0].size, size);
    }

    #[test]
    fn parse_type_zero_filtered() {
        let mut dir_data = Vec::new();
        dir_data.extend_from_slice(&root_entry(2));
        dir_data.extend_from_slice(&build_dir_entry("Empty", 0, NF, NF, NF, 0, 0)); // filtered
        dir_data.extend_from_slice(&build_dir_entry("Data", 2, NF, NF, NF, 10, 512));

        let raw = parse_raw_entries(&dir_data, CfbVersion::V3, 100_000, &null_diag());
        assert_eq!(raw.len(), 2);
        assert_eq!(raw[0].name, "Root Entry");
        assert_eq!(raw[1].name, "Data");
    }

    #[test]
    fn parse_nested_storage() {
        // Root -> Storage1 -> Stream1.
        let mut dir_data = Vec::new();
        dir_data.extend_from_slice(&root_entry(1));
        dir_data.extend_from_slice(&build_dir_entry("Storage1", 1, 2, NF, NF, 0, 0));
        dir_data.extend_from_slice(&build_dir_entry("Stream1", 2, NF, NF, NF, 5, 100));

        let raw = parse_raw_entries(&dir_data, CfbVersion::V3, 100_000, &null_diag());
        let (entries, index) = build_entries(&raw, &null_diag());

        assert_eq!(entries.len(), 3);
        let stream = entries.iter().find(|e| e.name == "Stream1").unwrap();
        assert_eq!(stream.path, "Storage1/Stream1");
        assert!(index.contains_key("STORAGE1/STREAM1"));
    }

    #[test]
    fn parse_circular_sibling() {
        // A.right=2, B.right=1 -- cycle.
        let mut dir_data = Vec::new();
        dir_data.extend_from_slice(&root_entry(1));
        dir_data.extend_from_slice(&build_dir_entry("A", 2, NF, NF, 2, 10, 100));
        dir_data.extend_from_slice(&build_dir_entry("B", 2, NF, NF, 1, 20, 200));

        let diag = collecting_diag();
        let sink: Arc<dyn DiagnosticsSink> = diag.clone();
        let raw = parse_raw_entries(&dir_data, CfbVersion::V3, 100_000, &sink);
        let (entries, _) = build_entries(&raw, &sink);

        // Should get root + A + B (cycle detected when revisiting A from B).
        assert!(entries.len() >= 2, "should have at least root + A");

        let warnings = diag.warnings();
        let has_cycle_warn = warnings.iter().any(|w| w.kind == "CfbDirSiblingCycle");
        assert!(
            has_cycle_warn,
            "expected DirSiblingCycle warning, got: {warnings:?}"
        );
    }

    #[test]
    fn parse_nostream_children() {
        let mut dir_data = Vec::new();
        dir_data.extend_from_slice(&root_entry(NF));

        let raw = parse_raw_entries(&dir_data, CfbVersion::V3, 100_000, &null_diag());
        let (entries, _) = build_entries(&raw, &null_diag());

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry_type, EntryType::RootEntry);
    }

    #[test]
    fn parse_case_insensitive_index() {
        let mut dir_data = Vec::new();
        dir_data.extend_from_slice(&root_entry(1));
        dir_data.extend_from_slice(&build_dir_entry("Workbook", 2, NF, NF, NF, 5, 4096));

        let raw = parse_raw_entries(&dir_data, CfbVersion::V3, 100_000, &null_diag());
        let (entries, index) = build_entries(&raw, &null_diag());

        assert_eq!(entries.len(), 2);
        assert!(index.contains_key("WORKBOOK"));
        let idx = index["WORKBOOK"];
        assert_eq!(entries[idx].name, "Workbook");
    }

    #[test]
    fn parse_deep_nesting_limit() {
        // Build a chain deeper than MAX_TREE_DEPTH.
        let depth = MAX_TREE_DEPTH + 10;
        let total = depth + 1; // root + depth entries
        let mut dir_data = Vec::with_capacity(total * 128);

        dir_data.extend_from_slice(&root_entry(1));
        for i in 1..=depth {
            let child = if i < depth { (i + 1) as u32 } else { NF };
            let name = format!("S{i}");
            dir_data.extend_from_slice(&build_dir_entry(&name, 1, child, NF, NF, 0, 0));
        }

        let diag = collecting_diag();
        let sink: Arc<dyn DiagnosticsSink> = diag.clone();
        let raw = parse_raw_entries(&dir_data, CfbVersion::V3, 100_000, &sink);
        let (entries, _) = build_entries(&raw, &sink);

        assert!(
            entries.len() < total,
            "should not have parsed all {total} entries"
        );

        let warnings = diag.warnings();
        let has_depth_warn = warnings.iter().any(|w| w.kind == "CfbDirTreeTooDeep");
        assert!(
            has_depth_warn,
            "expected DirTreeTooDeep warning, got: {warnings:?}"
        );
    }

    #[test]
    fn parse_out_of_bounds_sibling() {
        // Entry 1 right sibling = 999 (out of bounds).
        let mut dir_data = Vec::new();
        dir_data.extend_from_slice(&root_entry(1));
        dir_data.extend_from_slice(&build_dir_entry("Stream1", 2, NF, NF, 999, 5, 100));

        let diag = collecting_diag();
        let sink: Arc<dyn DiagnosticsSink> = diag.clone();
        let raw = parse_raw_entries(&dir_data, CfbVersion::V3, 100_000, &sink);
        let (entries, _) = build_entries(&raw, &sink);

        assert_eq!(entries.len(), 2);

        let warnings = diag.warnings();
        let has_oob_warn = warnings.iter().any(|w| w.kind == "CfbDirEntryOutOfBounds");
        assert!(
            has_oob_warn,
            "expected DirEntryOutOfBounds warning, got: {warnings:?}"
        );
    }

    /// Build minimal file data with a header block, directory sector, and FAT sector.
    fn build_test_file(dir_entries: &[[u8; 128]]) -> (Vec<u8>, CfbHeader) {
        use super::super::fat::{ENDOFCHAIN, FREESECT};
        let ss = 512usize;
        let header = CfbHeader {
            version: CfbVersion::V3,
            sector_size: ss as u32,
            mini_sector_size: 64,
            mini_stream_cutoff: 4096,
            fat_sector_count: 1,
            first_dir_sector: 0,
            first_mini_fat_sector: ENDOFCHAIN,
            mini_fat_sector_count: 0,
            first_difat_sector: ENDOFCHAIN,
            difat_sector_count: 0,
            difat_entries: vec![1],
        };
        let mut data = vec![0u8; ss]; // header placeholder
        let mut dir_sector = Vec::new();
        for e in dir_entries {
            dir_sector.extend_from_slice(e);
        }
        dir_sector.resize(ss, 0);
        data.extend_from_slice(&dir_sector);
        // FAT sector: entry[0]=ENDOFCHAIN, entry[1]=FATSECT, rest FREESECT.
        let mut fat = vec![0u8; ss];
        fat[0..4].copy_from_slice(&ENDOFCHAIN.to_le_bytes());
        fat[4..8].copy_from_slice(&0xFFFF_FFFDu32.to_le_bytes());
        for i in 2..ss / 4 {
            fat[i * 4..i * 4 + 4].copy_from_slice(&FREESECT.to_le_bytes());
        }
        data.extend_from_slice(&fat);
        (data, header)
    }

    #[test]
    fn parse_directory_full_pipeline() {
        let root = root_entry(1);
        let wb = build_dir_entry("Workbook", 2, NF, NF, NF, 5, 8192);
        let (file_data, header) = build_test_file(&[root, wb]);
        let diag = null_diag();
        let fat = Fat::build(&file_data, &header, &diag).unwrap();
        let (entries, index) = parse_directory(&file_data, &header, &fat, 100_000, &diag).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].entry_type, EntryType::RootEntry);
        assert_eq!(entries[1].name, "Workbook");
        assert_eq!(entries[1].size, 8192);
        assert!(index.contains_key("WORKBOOK"));
    }

    #[test]
    fn parse_multiple_storages_with_siblings() {
        // Root -> child 1 (ObjectPool, storage), right sibling 3 (Workbook).
        // ObjectPool child 2 (obj1, stream).
        let mut dir_data = Vec::new();
        dir_data.extend_from_slice(&root_entry(1));
        dir_data.extend_from_slice(&build_dir_entry("ObjectPool", 1, 2, NF, 3, 0, 0));
        dir_data.extend_from_slice(&build_dir_entry("obj1", 2, NF, NF, NF, 10, 50));
        dir_data.extend_from_slice(&build_dir_entry("Workbook", 2, NF, NF, NF, 20, 8192));
        let raw = parse_raw_entries(&dir_data, CfbVersion::V3, 100_000, &null_diag());
        let (entries, index) = build_entries(&raw, &null_diag());
        assert_eq!(entries.len(), 4);
        let obj1 = entries.iter().find(|e| e.name == "obj1").unwrap();
        assert_eq!(obj1.path, "ObjectPool/obj1");
        let wb = entries.iter().find(|e| e.name == "Workbook").unwrap();
        assert_eq!(wb.path, "Workbook");
        assert!(index.contains_key("OBJECTPOOL/OBJ1"));
        assert!(index.contains_key("WORKBOOK"));
    }

    #[test]
    fn parse_odd_name_len_warns() {
        // Build a dir entry with an odd name_len_bytes value.
        let mut slot = build_dir_entry("Test", 2, NF, NF, NF, 0, 0);
        // "Test" = 4 chars + null = 5 u16 = 10 bytes. Set to 9 (odd).
        slot[0x40..0x42].copy_from_slice(&9u16.to_le_bytes());

        let diag = collecting_diag();
        let sink: Arc<dyn DiagnosticsSink> = diag.clone();
        let raw = parse_raw_entries(&slot, CfbVersion::V3, 100_000, &sink);
        // Should still parse (truncating to 4 chars), but warn.
        assert_eq!(raw.len(), 1);

        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.kind == "CfbDirEntryOddNameLen"),
            "expected CfbDirEntryOddNameLen warning, got: {warnings:?}"
        );
    }

    #[test]
    fn parse_stream_with_child_warns() {
        // Stream entry with a non-NOSTREAM child_id.
        let mut dir_data = Vec::new();
        dir_data.extend_from_slice(&root_entry(1));
        // child_id = 2 on a stream entry (type 2) is invalid per spec.
        dir_data.extend_from_slice(&build_dir_entry("BadStream", 2, 2, NF, NF, 10, 100));
        dir_data.extend_from_slice(&build_dir_entry("Orphan", 2, NF, NF, NF, 20, 50));

        let diag = collecting_diag();
        let sink: Arc<dyn DiagnosticsSink> = diag.clone();
        let raw = parse_raw_entries(&dir_data, CfbVersion::V3, 100_000, &sink);
        let (_entries, _) = build_entries(&raw, &sink);

        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.kind == "CfbDirStreamHasChild"),
            "expected CfbDirStreamHasChild warning, got: {warnings:?}"
        );
    }

    #[test]
    fn parse_unknown_entry_type_warns() {
        // Entry with type 3 (reserved/unknown) should be skipped with a warning.
        let mut dir_data = Vec::new();
        dir_data.extend_from_slice(&root_entry(1));
        dir_data.extend_from_slice(&build_dir_entry("Weird", 3, NF, NF, NF, 10, 100));

        let diag = collecting_diag();
        let sink: Arc<dyn DiagnosticsSink> = diag.clone();
        let raw = parse_raw_entries(&dir_data, CfbVersion::V3, 100_000, &sink);
        let (entries, _) = build_entries(&raw, &sink);

        // Root is present but the type-3 entry should be skipped.
        assert_eq!(
            entries.len(),
            1,
            "type-3 entry should not appear in results"
        );

        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.kind == "CfbDirUnknownType"),
            "expected CfbDirUnknownType warning, got: {warnings:?}"
        );
    }
}
