//! ZIP archive reader.
//!
//! Parses ZIP archives from byte slices with lenient handling of real-world
//! quirks: trailing garbage, CRC mismatches, data descriptors, ZIP64.
//! Decompression via flate2 with configurable size limits.

mod decompress;
mod parse;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use crate::error::{Error, Result, ResultExt};

/// ZIP local file header magic bytes (`PK\x03\x04`).
///
/// Used for format detection in the facade. Also defined as `LOCAL_HEADER_SIG`
/// in the parse module for internal validation.
pub const ZIP_MAGIC: [u8; 4] = [0x50, 0x4B, 0x03, 0x04];

/// Default maximum decompressed size PER ENTRY: 250 MB.
const DEFAULT_MAX_DECOMPRESSED: u64 = 250 * 1024 * 1024;

/// Default cumulative decompressed-bytes budget for one archive: 500 MB
/// (SEC-ALLOC-CLAMP, #62, OOXML-F1).
///
/// A well-formed OOXML archive rarely tops ~50 MB decompressed total; 500 MB
/// leaves 10x headroom while refusing the cumulative-zip-bomb attack
/// (e.g. 1000-sheet XLSX at 250 MB each = 250 GB, which the per-entry
/// cap alone doesn't prevent because each entry is individually valid).
const DEFAULT_MAX_ARCHIVE_DECOMPRESSED: u64 = 500 * 1024 * 1024;

/// Default maximum compression ratio (decompressed / compressed).
/// Ratios above this threshold are suspicious (potential zip bomb).
/// 100:1 is generous; legitimate OOXML entries rarely exceed 20:1.
const DEFAULT_MAX_COMPRESSION_RATIO: u64 = 100;

/// Compression methods we understand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionMethod {
    /// Method 0: data is stored uncompressed.
    Stored,
    /// Method 8: DEFLATE.
    Deflated,
    /// Unknown/unsupported method.
    Unknown(u16),
}

impl CompressionMethod {
    fn from_u16(v: u16) -> Self {
        match v {
            0 => CompressionMethod::Stored,
            8 => CompressionMethod::Deflated,
            other => CompressionMethod::Unknown(other),
        }
    }
}

/// A single entry in the ZIP central directory.
#[derive(Debug, Clone)]
pub struct ZipEntry {
    /// Entry name (path within the archive), sanitized.
    pub name: String,
    /// Compressed size in bytes.
    pub compressed_size: u64,
    /// Uncompressed size in bytes.
    pub uncompressed_size: u64,
    /// CRC-32 checksum of uncompressed data.
    pub crc32: u32,
    /// Compression method.
    pub method: CompressionMethod,
    /// Offset of the local file header in the archive.
    pub(crate) local_header_offset: u64,
    /// Whether general purpose flag bit 3 (data descriptor) is set.
    #[allow(dead_code)] // parsed from ZIP header; reserved for streaming support
    pub(crate) has_data_descriptor: bool,
    /// Whether general purpose flag bit 11 (UTF-8 names) is set.
    #[allow(dead_code)] // parsed from ZIP central directory; reserved for encoding validation
    pub(crate) is_utf8: bool,
}

/// Configuration for the ZIP reader.
#[derive(Debug, Clone)]
pub struct ZipConfig {
    /// Maximum decompressed size per individual entry, in bytes.
    pub max_decompressed_size: u64,
    /// Cumulative cap on decompressed bytes across all entries read
    /// from a single archive (SEC-ALLOC-CLAMP). The per-entry limit
    /// alone doesn't stop an attacker sending an archive with 1000
    /// entries each at the per-entry limit; this is the aggregate.
    /// Set to `u64::MAX` to disable.
    pub max_archive_decompressed: u64,
    /// Maximum allowed compression ratio (decompressed / compressed).
    /// Entries exceeding this ratio are rejected as potential zip bombs.
    /// Set to 0 to disable the check.
    pub max_compression_ratio: u64,
}

impl Default for ZipConfig {
    fn default() -> Self {
        Self {
            max_decompressed_size: DEFAULT_MAX_DECOMPRESSED,
            max_archive_decompressed: DEFAULT_MAX_ARCHIVE_DECOMPRESSED,
            max_compression_ratio: DEFAULT_MAX_COMPRESSION_RATIO,
        }
    }
}

/// A parsed ZIP archive backed by a byte slice.
///
/// Construct with [`ZipArchive::new`] or [`ZipArchive::with_config`], then
/// iterate entries or read individual files.
pub struct ZipArchive<'a> {
    data: &'a [u8],
    entries: Vec<ZipEntry>,
    /// Exact entry name -> index for O(1) lookup.
    names_exact: HashMap<String, usize>,
    /// Lowercase entry name -> index for O(1) case-insensitive lookup.
    names_lower: HashMap<String, usize>,
    diag: Arc<dyn DiagnosticsSink>,
    config: ZipConfig,
    /// Cumulative decompressed bytes returned from `read`. Enforced against
    /// `config.max_archive_decompressed` on every call (SEC-ALLOC-CLAMP #62
    /// OOXML-F1). Atomic so `&self` read methods can still mutate.
    archive_decompressed: AtomicU64,
}

impl<'a> ZipArchive<'a> {
    /// Open a ZIP archive from raw bytes with default configuration.
    pub fn new(data: &'a [u8], diag: Arc<dyn DiagnosticsSink>) -> Result<Self> {
        Self::with_config(data, diag, ZipConfig::default())
    }

    /// Open a ZIP archive with custom configuration.
    pub fn with_config(
        data: &'a [u8],
        diag: Arc<dyn DiagnosticsSink>,
        config: ZipConfig,
    ) -> Result<Self> {
        let entries = parse::parse_archive(data, &diag).context("opening ZIP archive")?;
        let mut names_exact = HashMap::with_capacity(entries.len());
        let mut names_lower = HashMap::with_capacity(entries.len());
        for (i, entry) in entries.iter().enumerate() {
            names_exact.entry(entry.name.clone()).or_insert(i);
            names_lower
                .entry(entry.name.to_ascii_lowercase())
                .or_insert(i);
        }
        Ok(Self {
            data,
            entries,
            names_exact,
            names_lower,
            diag,
            config,
            archive_decompressed: AtomicU64::new(0),
        })
    }

    /// All entries in the central directory.
    pub fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    /// Find an entry by exact name.
    pub fn find(&self, name: &str) -> Option<&ZipEntry> {
        self.names_exact.get(name).map(|&i| &self.entries[i])
    }

    /// Find an entry by case-insensitive name (for OPC part lookup).
    pub fn find_ci(&self, name: &str) -> Option<&ZipEntry> {
        let lower = name.to_ascii_lowercase();
        self.names_lower.get(&lower).map(|&i| &self.entries[i])
    }

    /// Read and decompress an entry's data.
    ///
    /// The `entry` must come from *this* archive (obtained via [`entries`](Self::entries),
    /// [`find`](Self::find), or [`find_ci`](Self::find_ci)). Passing an entry from a
    /// different archive will read at incorrect offsets.
    ///
    /// Enforces both the per-entry cap (`max_decompressed_size`) via the
    /// decompressor and the cumulative per-archive cap
    /// (`max_archive_decompressed`) via a running `AtomicU64` counter;
    /// calls past the aggregate budget return an error.
    pub fn read(&self, entry: &ZipEntry) -> Result<Vec<u8>> {
        let bytes = decompress::read_entry(self.data, entry, &self.diag, &self.config)
            .context(format!("reading ZIP entry '{}'", entry.name))?;
        // SEC-ALLOC-CLAMP #62 ( OOXML-F1): running tally. We
        // `fetch_add` after a successful read (so failed reads don't
        // eat from the budget) and check against the limit. This is
        // lenient: a single entry over the cap still succeeds if it
        // was the first entry (so realistic 300 MB sheets still work);
        // subsequent entries are refused.
        let total = self
            .archive_decompressed
            .fetch_add(bytes.len() as u64, Ordering::Relaxed)
            .saturating_add(bytes.len() as u64);
        if total > self.config.max_archive_decompressed {
            return Err(Error::zip(format!(
                "archive decompressed total {} exceeds limit {} \
                 (triggered by entry '{}')",
                total, self.config.max_archive_decompressed, entry.name
            )));
        }
        Ok(bytes)
    }

    /// Read an entry as a UTF-8 string.
    pub fn read_string(&self, entry: &ZipEntry) -> Result<String> {
        let bytes = self.read(entry)?;
        String::from_utf8(bytes)
            .map_err(|e| Error::zip(format!("entry '{}' is not valid UTF-8: {e}", entry.name)))
    }
}

/// Sanitize an entry name: reject zip-slip, strip leading slashes, resolve `.`/`..` segments.
fn sanitize_name(raw: &str, diag: &Arc<dyn DiagnosticsSink>) -> String {
    let name = raw.replace('\\', "/");
    let name = name.trim_start_matches('/');

    if name.split('/').any(|seg| seg == "..") {
        diag.warning(Warning::new(
            "ZipSlipPath",
            format!("path traversal in entry name: {raw}"),
        ));
    }

    // Resolve `.` and `..` segments against the path structure, same algorithm
    // as OPC normalize_path but without leading `/`.
    let mut segments: Vec<&str> = Vec::new();
    for seg in name.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            s => segments.push(s),
        }
    }

    let result = segments.join("/");
    if result.is_empty() {
        "_".to_string()
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use udoc_core::diagnostics::{CollectingDiagnostics, NullDiagnostics};

    use super::*;

    /// Helper: build a minimal valid ZIP with one Stored entry.
    fn make_stored_zip(name: &str, content: &[u8]) -> Vec<u8> {
        make_multi_entry_zip(&[(name, content)])
    }

    /// Helper: build a ZIP with one DEFLATE-compressed entry.
    fn make_deflated_zip(name: &str, content: &[u8]) -> Vec<u8> {
        use flate2::write::DeflateEncoder;
        use flate2::Compression;
        use std::io::Write;

        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(content).unwrap();
        let compressed = encoder.finish().unwrap();

        let crc = crc32(content);
        let name_bytes = name.as_bytes();
        let mut buf = Vec::new();

        // Local file header
        let local_offset = buf.len() as u32;
        buf.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04]);
        buf.extend_from_slice(&20u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // flags
        buf.extend_from_slice(&8u16.to_le_bytes()); // method: deflated
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&crc.to_le_bytes());
        buf.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(content.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(name_bytes);
        buf.extend_from_slice(&compressed);

        // Central directory
        let cd_offset = buf.len() as u32;
        buf.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02]);
        buf.extend_from_slice(&20u16.to_le_bytes());
        buf.extend_from_slice(&20u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // flags
        buf.extend_from_slice(&8u16.to_le_bytes()); // method: deflated
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&crc.to_le_bytes());
        buf.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(content.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&local_offset.to_le_bytes());
        buf.extend_from_slice(name_bytes);

        let cd_size = (buf.len() as u32) - cd_offset;

        // EOCD
        buf.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06]);
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&cd_size.to_le_bytes());
        buf.extend_from_slice(&cd_offset.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());

        buf
    }

    use crate::test_util::build_stored_zip as make_multi_entry_zip;

    /// Simple CRC-32 for tests.
    fn crc32(data: &[u8]) -> u32 {
        crc32fast::hash(data)
    }

    #[test]
    fn read_stored_entry() {
        let zip = make_stored_zip("hello.txt", b"Hello, world!");
        let archive = ZipArchive::new(&zip, Arc::new(NullDiagnostics)).unwrap();
        assert_eq!(archive.entries().len(), 1);
        assert_eq!(archive.entries()[0].name, "hello.txt");
        let content = archive.read(&archive.entries()[0]).unwrap();
        assert_eq!(content, b"Hello, world!");
    }

    #[test]
    fn read_deflated_entry() {
        let content = b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let zip = make_deflated_zip("compressed.txt", content);
        let archive = ZipArchive::new(&zip, Arc::new(NullDiagnostics)).unwrap();
        assert_eq!(archive.entries().len(), 1);
        assert_eq!(archive.entries()[0].method, CompressionMethod::Deflated);
        let result = archive.read(&archive.entries()[0]).unwrap();
        assert_eq!(result, content);
    }

    #[test]
    fn read_string_entry() {
        let zip = make_stored_zip("doc.xml", b"<root/>");
        let archive = ZipArchive::new(&zip, Arc::new(NullDiagnostics)).unwrap();
        let s = archive.read_string(&archive.entries()[0]).unwrap();
        assert_eq!(s, "<root/>");
    }

    #[test]
    fn find_exact() {
        let zip = make_multi_entry_zip(&[("a.txt", b"aaa"), ("dir/b.txt", b"bbb")]);
        let archive = ZipArchive::new(&zip, Arc::new(NullDiagnostics)).unwrap();
        assert!(archive.find("a.txt").is_some());
        assert!(archive.find("dir/b.txt").is_some());
        assert!(archive.find("A.TXT").is_none());
        assert!(archive.find("c.txt").is_none());
    }

    #[test]
    fn find_case_insensitive() {
        let zip = make_multi_entry_zip(&[("[Content_Types].xml", b"<Types/>")]);
        let archive = ZipArchive::new(&zip, Arc::new(NullDiagnostics)).unwrap();
        assert!(archive.find_ci("[content_types].xml").is_some());
        assert!(archive.find_ci("[CONTENT_TYPES].XML").is_some());
    }

    #[test]
    fn multi_entry_zip() {
        let zip = make_multi_entry_zip(&[
            ("word/document.xml", b"<doc/>"),
            ("word/_rels/document.xml.rels", b"<rels/>"),
            ("_rels/.rels", b"<root-rels/>"),
            ("[Content_Types].xml", b"<types/>"),
        ]);
        let archive = ZipArchive::new(&zip, Arc::new(NullDiagnostics)).unwrap();
        assert_eq!(archive.entries().len(), 4);
        for entry in archive.entries() {
            let data = archive.read(entry).unwrap();
            assert!(!data.is_empty());
        }
    }

    #[test]
    fn trailing_garbage() {
        let mut zip = make_stored_zip("test.txt", b"data");
        // Append 1KB of garbage
        zip.extend_from_slice(&[0xDE; 1024]);

        let diag = Arc::new(CollectingDiagnostics::new());
        let archive = ZipArchive::new(&zip, diag.clone()).unwrap();
        assert_eq!(archive.entries().len(), 1);
        let content = archive.read(&archive.entries()[0]).unwrap();
        assert_eq!(content, b"data");

        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.kind == "ZipTrailingData"),
            "expected trailing data warning, got: {warnings:?}"
        );
    }

    #[test]
    fn crc_mismatch_warns() {
        let mut zip = make_stored_zip("test.txt", b"correct");
        // Corrupt the CRC in central directory: find PK\x01\x02 and modify CRC field
        // CRC is at offset 16 from start of central directory entry
        let cd_sig = [0x50, 0x4B, 0x01, 0x02];
        let cd_pos = zip.windows(4).rposition(|w| w == cd_sig).unwrap();
        // CRC field is at offset +16 from CD entry start
        zip[cd_pos + 16] ^= 0xFF;

        let diag = Arc::new(CollectingDiagnostics::new());
        let archive = ZipArchive::new(&zip, diag.clone()).unwrap();
        let content = archive.read(&archive.entries()[0]).unwrap();
        // Data is still returned despite CRC mismatch
        assert_eq!(content, b"correct");

        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.kind == "ZipCrcMismatch"),
            "expected CRC mismatch warning, got: {warnings:?}"
        );
    }

    #[test]
    fn empty_archive() {
        let mut buf = Vec::new();
        // EOCD with 0 entries
        buf.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06]);
        buf.extend_from_slice(&0u16.to_le_bytes()); // disk number
        buf.extend_from_slice(&0u16.to_le_bytes()); // cd disk
        buf.extend_from_slice(&0u16.to_le_bytes()); // entries on disk
        buf.extend_from_slice(&0u16.to_le_bytes()); // total entries
        buf.extend_from_slice(&0u32.to_le_bytes()); // cd size
        buf.extend_from_slice(&0u32.to_le_bytes()); // cd offset (points to EOCD itself, but 0 entries)
        buf.extend_from_slice(&0u16.to_le_bytes()); // comment len

        let archive = ZipArchive::new(&buf, Arc::new(NullDiagnostics)).unwrap();
        assert_eq!(archive.entries().len(), 0);
    }

    #[test]
    fn zip_slip_path_sanitized() {
        let collecting = Arc::new(CollectingDiagnostics::new());
        let diag: Arc<dyn DiagnosticsSink> = collecting.clone();
        let name = sanitize_name("../../etc/passwd", &diag);
        assert!(
            !name.contains(".."),
            "sanitized name should not contain ..: {name}"
        );
        assert_eq!(name, "etc/passwd");

        let warnings = collecting.warnings();
        assert!(warnings.iter().any(|w| w.kind == "ZipSlipPath"));
    }

    #[test]
    fn zip_slip_all_traversal_gets_safe_name() {
        let collecting = Arc::new(CollectingDiagnostics::new());
        let diag: Arc<dyn DiagnosticsSink> = collecting.clone();
        let name = sanitize_name("../../../", &diag);
        assert_eq!(name, "_", "all-traversal path should map to '_': {name}");
    }

    #[test]
    fn dot_segments_resolved() {
        let diag: Arc<dyn DiagnosticsSink> = Arc::new(NullDiagnostics);
        let name = sanitize_name("a/./b/../c/file.txt", &diag);
        assert_eq!(name, "a/c/file.txt");
    }

    #[test]
    fn lone_dot_segment() {
        let diag: Arc<dyn DiagnosticsSink> = Arc::new(NullDiagnostics);
        let name = sanitize_name("./file.txt", &diag);
        assert_eq!(name, "file.txt");
    }

    #[test]
    fn leading_slash_stripped() {
        let diag: Arc<dyn DiagnosticsSink> = Arc::new(NullDiagnostics);
        let name = sanitize_name("/absolute/path.txt", &diag);
        assert_eq!(name, "absolute/path.txt");
    }

    #[test]
    fn backslash_normalized() {
        let diag: Arc<dyn DiagnosticsSink> = Arc::new(NullDiagnostics);
        let name = sanitize_name("dir\\subdir\\file.txt", &diag);
        assert_eq!(name, "dir/subdir/file.txt");
    }

    #[test]
    fn no_eocd_is_error() {
        let data = b"this is not a zip file at all";
        let result = ZipArchive::new(data, Arc::new(NullDiagnostics));
        let err = result.err().expect("should be an error");
        let msg = format!("{err}");
        assert!(msg.contains("end of central directory"), "got: {msg}");
    }

    #[test]
    fn too_small_is_error() {
        let result = ZipArchive::new(b"PK", Arc::new(NullDiagnostics));
        assert!(result.is_err());
    }

    #[test]
    fn data_descriptor_entry() {
        // Build a ZIP where the local header has flag bit 3 set (data descriptor)
        // and sizes=0 in local header (central directory has the real sizes).
        let content = b"descriptor test";
        let crc = crc32(content);
        let name = b"desc.txt";
        let mut buf = Vec::new();

        // Local file header with bit 3 set, sizes=0
        let local_offset = buf.len() as u32;
        buf.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04]);
        buf.extend_from_slice(&20u16.to_le_bytes());
        buf.extend_from_slice(&0x0008u16.to_le_bytes()); // flags: bit 3 set
        buf.extend_from_slice(&0u16.to_le_bytes()); // stored
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // CRC = 0 (in descriptor)
        buf.extend_from_slice(&0u32.to_le_bytes()); // compressed = 0
        buf.extend_from_slice(&0u32.to_le_bytes()); // uncompressed = 0
        buf.extend_from_slice(&(name.len() as u16).to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(name);
        buf.extend_from_slice(content);

        // Data descriptor (with signature)
        buf.extend_from_slice(&[0x50, 0x4B, 0x07, 0x08]);
        buf.extend_from_slice(&crc.to_le_bytes());
        buf.extend_from_slice(&(content.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(content.len() as u32).to_le_bytes());

        // Central directory (has the real sizes)
        let cd_offset = buf.len() as u32;
        buf.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02]);
        buf.extend_from_slice(&20u16.to_le_bytes());
        buf.extend_from_slice(&20u16.to_le_bytes());
        buf.extend_from_slice(&0x0008u16.to_le_bytes()); // flags: bit 3 set
        buf.extend_from_slice(&0u16.to_le_bytes()); // stored
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&crc.to_le_bytes());
        buf.extend_from_slice(&(content.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(content.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(name.len() as u16).to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&local_offset.to_le_bytes());
        buf.extend_from_slice(name);

        let cd_size = (buf.len() as u32) - cd_offset;

        // EOCD
        buf.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06]);
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&cd_size.to_le_bytes());
        buf.extend_from_slice(&cd_offset.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());

        let archive = ZipArchive::new(&buf, Arc::new(NullDiagnostics)).unwrap();
        assert_eq!(archive.entries().len(), 1);
        assert!(archive.entries()[0].has_data_descriptor);
        let result = archive.read(&archive.entries()[0]).unwrap();
        assert_eq!(result, content);
    }

    #[test]
    fn duplicate_entry_names_warn() {
        let zip = make_multi_entry_zip(&[("dup.txt", b"first"), ("dup.txt", b"second")]);
        let diag = Arc::new(CollectingDiagnostics::new());
        let archive = ZipArchive::new(&zip, diag.clone()).unwrap();
        // Both entries are kept (first one wins for find() via HashMap::or_insert)
        assert_eq!(archive.entries().len(), 2);

        let warnings = diag.warnings();
        assert!(
            warnings.iter().any(|w| w.kind == "ZipDuplicateEntry"),
            "expected duplicate entry warning, got: {warnings:?}"
        );
    }

    #[test]
    fn subdirectory_paths() {
        let zip =
            make_multi_entry_zip(&[("a/b/c/deep.txt", b"deep"), ("a/shallow.txt", b"shallow")]);
        let archive = ZipArchive::new(&zip, Arc::new(NullDiagnostics)).unwrap();
        assert!(archive.find("a/b/c/deep.txt").is_some());
        assert!(archive.find("a/shallow.txt").is_some());
    }

    #[test]
    fn unsupported_compression_method_errors() {
        // Build a ZIP with compression method 99 (WinZip AES) which we don't support.
        let name = b"test.txt";
        let content = b"data";
        let crc = crc32(content);
        let mut buf = Vec::new();

        // Local file header
        let local_offset = buf.len() as u32;
        buf.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04]);
        buf.extend_from_slice(&20u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // flags
        buf.extend_from_slice(&99u16.to_le_bytes()); // unsupported method
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&crc.to_le_bytes());
        buf.extend_from_slice(&(content.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(content.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(name.len() as u16).to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(name);
        buf.extend_from_slice(content);

        // Central directory
        let cd_offset = buf.len() as u32;
        buf.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02]);
        buf.extend_from_slice(&20u16.to_le_bytes());
        buf.extend_from_slice(&20u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // flags
        buf.extend_from_slice(&99u16.to_le_bytes()); // unsupported method
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&crc.to_le_bytes());
        buf.extend_from_slice(&(content.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(content.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(name.len() as u16).to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&local_offset.to_le_bytes());
        buf.extend_from_slice(name);

        let cd_size = (buf.len() as u32) - cd_offset;

        // EOCD
        buf.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06]);
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&cd_size.to_le_bytes());
        buf.extend_from_slice(&cd_offset.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());

        let archive = ZipArchive::new(&buf, Arc::new(NullDiagnostics)).unwrap();
        let result = archive.read(&archive.entries()[0]);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("unsupported compression method 99"),
            "got: {err}"
        );
    }

    #[test]
    fn resource_limit_on_decompression() {
        let content = b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let zip = make_deflated_zip("big.txt", content);
        let config = ZipConfig {
            max_decompressed_size: 10, // very small limit
            ..ZipConfig::default()
        };
        let archive = ZipArchive::with_config(&zip, Arc::new(NullDiagnostics), config).unwrap();
        let result = archive.read(&archive.entries()[0]);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("resource limit") || err.contains("size"),
            "got: {err}"
        );
    }

    #[test]
    fn compression_ratio_limit() {
        // Build a DEFLATE entry where the compressed size is tiny relative to
        // the uncompressed size (high ratio). Use a content that compresses
        // extremely well (all zeros).
        let content = vec![0u8; 10_000];
        let zip = make_deflated_zip("bomb.txt", &content);
        let config = ZipConfig {
            max_compression_ratio: 2, // very strict: reject anything above 2:1
            ..ZipConfig::default()
        };
        let archive = ZipArchive::with_config(&zip, Arc::new(NullDiagnostics), config).unwrap();
        let result = archive.read(&archive.entries()[0]);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("compression ratio"),
            "expected ratio error, got: {err}"
        );
    }

    #[test]
    fn resource_limit_on_stored_entry() {
        let content = b"Hello, stored!";
        let zip = make_stored_zip("stored.txt", content);
        let config = ZipConfig {
            max_decompressed_size: 5, // smaller than content
            ..ZipConfig::default()
        };
        let archive = ZipArchive::with_config(&zip, Arc::new(NullDiagnostics), config).unwrap();
        let result = archive.read(&archive.entries()[0]);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("size") && err.contains("exceeds limit"),
            "got: {err}"
        );
    }
}
