//! CFB/OLE2 compound document reader.
//!
//! Parses Compound File Binary format containers used by legacy Office
//! formats (DOC, XLS, PPT) and OOXML encryption.

mod directory;
mod fat;
mod header;
mod stream;
pub mod summary_info;

use std::collections::HashMap;
use std::sync::Arc;

use udoc_core::diagnostics::DiagnosticsSink;

use crate::error::{Result, ResultExt};

pub use directory::{DirEntry, EntryType};
#[cfg(any(test, feature = "test-internals"))]
pub(crate) use header::CFB_MAGIC;
pub use summary_info::{parse_summary_information, SummaryInfo};

/// Configuration for the CFB reader.
#[derive(Debug, Clone)]
pub struct CfbConfig {
    /// Maximum stream size in bytes (default 250 MB).
    pub max_stream_size: u64,
    /// Maximum number of directory entries to parse (default 100,000).
    pub max_directory_entries: usize,
}

impl Default for CfbConfig {
    fn default() -> Self {
        Self {
            max_stream_size: 250 * 1024 * 1024,
            max_directory_entries: 100_000,
        }
    }
}

/// A parsed CFB/OLE2 compound document backed by a byte slice.
pub struct CfbArchive<'a> {
    data: &'a [u8],
    header: header::CfbHeader,
    entries: Vec<DirEntry>,
    names_index: HashMap<String, usize>,
    fat: fat::Fat,
    mini_fat: fat::MiniFat,
    mini_stream: Vec<u8>,
    diag: Arc<dyn DiagnosticsSink>,
    config: CfbConfig,
}

impl std::fmt::Debug for CfbArchive<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CfbArchive")
            .field("version", &self.header.version)
            .field("sector_size", &self.header.sector_size)
            .field("entry_count", &self.entries.len())
            .finish()
    }
}

impl<'a> CfbArchive<'a> {
    /// Open a CFB archive from raw bytes with default configuration.
    pub fn new(data: &'a [u8], diag: Arc<dyn DiagnosticsSink>) -> Result<Self> {
        Self::with_config(data, diag, CfbConfig::default())
    }

    /// Open a CFB archive with custom configuration.
    pub fn with_config(
        data: &'a [u8],
        diag: Arc<dyn DiagnosticsSink>,
        config: CfbConfig,
    ) -> Result<Self> {
        let hdr = header::CfbHeader::parse(data, &diag).context("parsing CFB header")?;
        let fat = fat::Fat::build(data, &hdr, &diag).context("building FAT")?;
        let mini_fat = fat::MiniFat::build(data, &hdr, &fat, &diag).context("building mini-FAT")?;
        let (entries, names_index) =
            directory::parse_directory(data, &hdr, &fat, config.max_directory_entries, &diag)
                .context("parsing directory")?;

        let mini_stream = if let Some(root) = entries
            .iter()
            .find(|e| e.entry_type == EntryType::RootEntry)
        {
            if root.size > 0 {
                stream::read_root_stream(data, &hdr, &fat, root, config.max_stream_size, &diag)
                    .context("reading mini-stream container")?
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        Ok(Self {
            data,
            header: hdr,
            entries,
            names_index,
            fat,
            mini_fat,
            mini_stream,
            diag,
            config,
        })
    }

    /// All directory entries (streams, storages, and root).
    pub fn entries(&self) -> &[DirEntry] {
        &self.entries
    }

    /// Iterator over stream entries only (excludes storages and root).
    pub fn streams(&self) -> impl Iterator<Item = &DirEntry> {
        self.entries
            .iter()
            .filter(|e| e.entry_type == EntryType::Stream)
    }

    /// Find a stream or storage entry by case-insensitive path.
    ///
    /// Comparison uses ASCII uppercasing only. MS-CFB specifies Unicode case
    /// tables, but ASCII covers all known Office stream names in practice
    /// (e.g. "Workbook", "PowerPoint Document", "WordDocument"). Non-ASCII
    /// entry names will match only if the caller provides the exact casing.
    ///
    /// Returns `None` for the root entry (use `entries()` to access it).
    /// Nested entries use `/` separators, e.g. `"ObjectPool/obj1"`.
    pub fn find(&self, path: &str) -> Option<&DirEntry> {
        let upper = path.to_ascii_uppercase();
        self.names_index.get(&upper).map(|&i| &self.entries[i])
    }

    /// Read a stream entry's data.
    pub fn read(&self, entry: &DirEntry) -> Result<Vec<u8>> {
        let ctx = stream::StreamContext {
            data: self.data,
            header: &self.header,
            fat: &self.fat,
            mini_fat: &self.mini_fat,
            mini_stream: &self.mini_stream,
            max_stream_size: self.config.max_stream_size,
            diag: &self.diag,
        };
        stream::read_stream(&ctx, entry).context(format!("reading stream '{}'", entry.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use udoc_core::diagnostics::NullDiagnostics;

    use super::fat::{ENDOFCHAIN, FATSECT, FREESECT};
    use super::CFB_MAGIC;
    use crate::test_util::build_cfb_dir_entry as build_dir_entry;
    const NOSTREAM: u32 = 0xFFFF_FFFF;

    fn null_diag() -> Arc<dyn DiagnosticsSink> {
        Arc::new(NullDiagnostics)
    }

    /// Build a minimal valid v3 CFB file with:
    /// - 512-byte header
    /// - Sector 0: FAT sector
    /// - Sector 1: Directory sector (root entry + one "TestStream" entry)
    /// - Sector 2: Data sector for TestStream containing b"Hello, CFB!"
    fn build_minimal_cfb() -> Vec<u8> {
        let sector_size = 512usize;
        let entries_per_fat_sector = sector_size / 4;

        // -- Header (512 bytes) --
        let mut buf = vec![0u8; sector_size];
        buf[0..8].copy_from_slice(&CFB_MAGIC);
        // minor version
        buf[0x18..0x1A].copy_from_slice(&0x003Eu16.to_le_bytes());
        // major version = 3
        buf[0x1A..0x1C].copy_from_slice(&3u16.to_le_bytes());
        // byte order = little-endian
        buf[0x1C..0x1E].copy_from_slice(&0xFFFEu16.to_le_bytes());
        // sector shift = 9 (512 bytes)
        buf[0x1E..0x20].copy_from_slice(&9u16.to_le_bytes());
        // mini sector shift = 6 (64 bytes)
        buf[0x20..0x22].copy_from_slice(&6u16.to_le_bytes());
        // FAT sector count = 1
        buf[0x2C..0x30].copy_from_slice(&1u32.to_le_bytes());
        // first directory sector = 1
        buf[0x30..0x34].copy_from_slice(&1u32.to_le_bytes());
        // mini-stream cutoff = 8 (streams >= 8 bytes use regular FAT,
        // so our 11-byte TestStream goes through the regular path)
        buf[0x38..0x3C].copy_from_slice(&8u32.to_le_bytes());
        // first mini-FAT sector = ENDOFCHAIN (no mini-FAT)
        buf[0x3C..0x40].copy_from_slice(&ENDOFCHAIN.to_le_bytes());
        // first DIFAT sector = ENDOFCHAIN
        buf[0x44..0x48].copy_from_slice(&ENDOFCHAIN.to_le_bytes());
        // DIFAT[0] = 0 (FAT is in sector 0)
        buf[0x4C..0x50].copy_from_slice(&0u32.to_le_bytes());
        // Fill remaining 108 DIFAT entries with FREESECT
        for i in 1..109 {
            let offset = 0x4C + i * 4;
            buf[offset..offset + 4].copy_from_slice(&FREESECT.to_le_bytes());
        }

        // -- Sector 0: FAT sector --
        let mut fat_sector = vec![0u8; sector_size];
        // FAT[0] = FATSECT (sector 0 is the FAT itself)
        fat_sector[0..4].copy_from_slice(&FATSECT.to_le_bytes());
        // FAT[1] = ENDOFCHAIN (directory is one sector)
        fat_sector[4..8].copy_from_slice(&ENDOFCHAIN.to_le_bytes());
        // FAT[2] = ENDOFCHAIN (stream data is one sector)
        fat_sector[8..12].copy_from_slice(&ENDOFCHAIN.to_le_bytes());
        // Rest = FREESECT
        for i in 3..entries_per_fat_sector {
            let off = i * 4;
            fat_sector[off..off + 4].copy_from_slice(&FREESECT.to_le_bytes());
        }
        buf.extend_from_slice(&fat_sector);

        // -- Sector 1: Directory sector --
        let mut dir_sector = vec![0u8; sector_size];
        // Entry 0: Root Entry (type 5), child_id = 1
        let root = build_dir_entry("Root Entry", 5, 1, NOSTREAM, NOSTREAM, 0, 0);
        dir_sector[0..128].copy_from_slice(&root);
        // Entry 1: TestStream (type 2), start_sector = 2, size = 11
        let stream_entry = build_dir_entry("TestStream", 2, NOSTREAM, NOSTREAM, NOSTREAM, 2, 11);
        dir_sector[128..256].copy_from_slice(&stream_entry);
        buf.extend_from_slice(&dir_sector);

        // -- Sector 2: Data sector --
        let mut data_sector = vec![0u8; sector_size];
        let payload = b"Hello, CFB!";
        data_sector[..payload.len()].copy_from_slice(payload);
        buf.extend_from_slice(&data_sector);

        buf
    }

    #[test]
    fn test_open_minimal() {
        let data = build_minimal_cfb();
        let archive = CfbArchive::new(&data, null_diag()).unwrap();
        // Root + TestStream = 2 entries
        assert_eq!(archive.entries().len(), 2);
    }

    #[test]
    fn test_find_stream() {
        let data = build_minimal_cfb();
        let archive = CfbArchive::new(&data, null_diag()).unwrap();
        // Exact case
        assert!(archive.find("TestStream").is_some());
        // All-uppercase (case-insensitive)
        assert!(archive.find("TESTSTREAM").is_some());
        // Mixed case
        assert!(archive.find("teststream").is_some());
    }

    #[test]
    fn test_read_stream() {
        let data = build_minimal_cfb();
        let archive = CfbArchive::new(&data, null_diag()).unwrap();
        let entry = archive.find("TestStream").unwrap();
        let bytes = archive.read(entry).unwrap();
        assert_eq!(bytes, b"Hello, CFB!");
    }

    #[test]
    fn test_streams_iterator() {
        let data = build_minimal_cfb();
        let archive = CfbArchive::new(&data, null_diag()).unwrap();
        let stream_entries: Vec<&DirEntry> = archive.streams().collect();
        // Only TestStream, not root
        assert_eq!(stream_entries.len(), 1);
        assert_eq!(stream_entries[0].name, "TestStream");
        assert_eq!(stream_entries[0].entry_type, EntryType::Stream);
    }

    #[test]
    fn test_find_nonexistent() {
        let data = build_minimal_cfb();
        let archive = CfbArchive::new(&data, null_diag()).unwrap();
        assert!(archive.find("Nope").is_none());
        assert!(archive.find("TestStream/child").is_none());
        assert!(archive.find("NoSuchStream").is_none());
    }
}
