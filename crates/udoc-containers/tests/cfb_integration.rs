//! Integration tests for the CFB/OLE2 compound document reader.
//!
//! These tests build synthetic CFB containers with `build_cfb` and exercise
//! the full parse stack, then verify against real XLS files from the corpus.

use std::sync::Arc;

use udoc_containers::cfb::{CfbArchive, CfbConfig, EntryType};
use udoc_containers::test_util::{build_cfb, build_cfb_dir_entry};
use udoc_core::diagnostics::NullDiagnostics;

fn diag() -> Arc<dyn udoc_core::diagnostics::DiagnosticsSink> {
    Arc::new(NullDiagnostics)
}

// -------------------------------------------------------------------------
// Synthetic tests
// -------------------------------------------------------------------------

#[test]
fn test_synthetic_single_stream() {
    let data = build_cfb(&[("TestStream", b"Hello")]);
    let archive = CfbArchive::new(&data, diag()).unwrap();
    let entry = archive.find("TestStream").expect("stream not found");
    let content = archive.read(entry).unwrap();
    assert_eq!(content, b"Hello");
}

#[test]
fn test_synthetic_multiple_streams() {
    let data = build_cfb(&[("Alpha", b"aaa"), ("Beta", b"bbb"), ("Gamma", b"ccc")]);
    let archive = CfbArchive::new(&data, diag()).unwrap();

    let alpha = archive.find("Alpha").expect("Alpha not found");
    assert_eq!(archive.read(alpha).unwrap(), b"aaa");

    let beta = archive.find("Beta").expect("Beta not found");
    assert_eq!(archive.read(beta).unwrap(), b"bbb");

    let gamma = archive.find("Gamma").expect("Gamma not found");
    assert_eq!(archive.read(gamma).unwrap(), b"ccc");
}

#[test]
fn test_synthetic_empty_stream() {
    let data = build_cfb(&[("Empty", b"")]);
    let archive = CfbArchive::new(&data, diag()).unwrap();
    let entry = archive.find("Empty").expect("Empty not found");
    let content = archive.read(entry).unwrap();
    assert!(
        content.is_empty(),
        "expected empty vec, got {} bytes",
        content.len()
    );
}

#[test]
fn test_find_case_insensitive() {
    let data = build_cfb(&[("TestStream", b"data")]);
    let archive = CfbArchive::new(&data, diag()).unwrap();

    // find() is case-insensitive per the CfbArchive implementation.
    assert!(
        archive.find("TESTSTREAM").is_some(),
        "uppercase lookup failed"
    );
    assert!(
        archive.find("teststream").is_some(),
        "lowercase lookup failed"
    );
    assert!(
        archive.find("TestStream").is_some(),
        "exact case lookup failed"
    );
    assert!(
        archive.find("Teststream").is_some(),
        "mixed case lookup failed"
    );
}

#[test]
fn test_entries_lists_all() {
    let data = build_cfb(&[("StreamA", b"a"), ("StreamB", b"b")]);
    let archive = CfbArchive::new(&data, diag()).unwrap();

    // entries() includes root + 2 streams = 3
    let entries = archive.entries();
    assert_eq!(entries.len(), 3, "expected root + 2 streams");

    let has_root = entries.iter().any(|e| e.entry_type == EntryType::RootEntry);
    assert!(has_root, "root entry missing from entries()");

    let has_a = entries.iter().any(|e| e.name == "StreamA");
    let has_b = entries.iter().any(|e| e.name == "StreamB");
    assert!(has_a, "StreamA missing");
    assert!(has_b, "StreamB missing");
}

#[test]
fn test_streams_filters() {
    let data = build_cfb(&[("OnlyStream", b"payload")]);
    let archive = CfbArchive::new(&data, diag()).unwrap();

    let stream_entries: Vec<_> = archive.streams().collect();
    assert_eq!(
        stream_entries.len(),
        1,
        "streams() should return only stream entries"
    );
    assert_eq!(stream_entries[0].name, "OnlyStream");
    assert_eq!(stream_entries[0].entry_type, EntryType::Stream);

    // Confirm root is excluded.
    let has_root = stream_entries
        .iter()
        .any(|e| e.entry_type == EntryType::RootEntry);
    assert!(!has_root, "streams() should not include root entry");
}

#[test]
fn test_with_config_max_stream_size() {
    let data = build_cfb(&[("BigStream", &[0xAA; 600])]);
    // Set max_stream_size below the stream's 600 bytes.
    let config = CfbConfig {
        max_stream_size: 100,
        ..CfbConfig::default()
    };
    let archive = CfbArchive::with_config(&data, diag(), config).unwrap();
    let entry = archive.find("BigStream").expect("stream not found");
    let result = archive.read(entry);
    assert!(result.is_err(), "expected resource limit error");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("resource limit") || err.contains("exceeds limit"),
        "unexpected error: {err}",
    );
}

#[test]
fn test_with_config_max_directory_entries() {
    let data = build_cfb(&[("A", b"a"), ("B", b"b"), ("C", b"c")]);
    // Limit to 2 directory entries (root + 1 stream, should truncate).
    let config = CfbConfig {
        max_directory_entries: 2,
        ..CfbConfig::default()
    };
    let archive = CfbArchive::with_config(&data, diag(), config).unwrap();
    // Only 2 raw entries parsed (root + one stream), so fewer than the full 4.
    assert!(
        archive.entries().len() < 4,
        "expected fewer entries with low max_directory_entries, got {}",
        archive.entries().len(),
    );
}

#[test]
fn test_synthetic_five_streams_multi_sector_directory() {
    // 5 streams + root = 6 directory entries, requiring 2 directory sectors
    // (4 entries per 512-byte sector). This exercises the multi-sector
    // directory chain path in build_cfb.
    let data = build_cfb(&[
        ("Alpha", b"aaa"),
        ("Beta", b"bbb"),
        ("Gamma", b"ccc"),
        ("Delta", b"ddd"),
        ("Epsilon", b"eee"),
    ]);
    let archive = CfbArchive::new(&data, diag()).unwrap();

    // Root + 5 streams = 6 entries.
    assert_eq!(archive.entries().len(), 6);

    // Verify each stream can be found and read back.
    let expected: &[(&str, &[u8])] = &[
        ("Alpha", b"aaa"),
        ("Beta", b"bbb"),
        ("Gamma", b"ccc"),
        ("Delta", b"ddd"),
        ("Epsilon", b"eee"),
    ];
    for (name, content) in expected {
        let entry = archive
            .find(name)
            .unwrap_or_else(|| panic!("{name} not found"));
        let read = archive.read(entry).unwrap();
        assert_eq!(read, *content, "content mismatch for {name}");
    }
}

#[test]
fn test_synthetic_eight_streams_three_dir_sectors() {
    // 8 streams + root = 9 directory entries = 3 directory sectors.
    // Also uses varying payload sizes to exercise multi-sector data chains.
    let streams: Vec<(&str, Vec<u8>)> = (0..8)
        .map(|i| {
            let name: &str = match i {
                0 => "S0",
                1 => "S1",
                2 => "S2",
                3 => "S3",
                4 => "S4",
                5 => "S5",
                6 => "S6",
                7 => "S7",
                _ => unreachable!(),
            };
            // Varying sizes: some single-sector, some multi-sector.
            let size = 100 + i * 150;
            let payload: Vec<u8> = (0..size).map(|b| (b & 0xFF) as u8).collect();
            (name, payload)
        })
        .collect();

    let stream_refs: Vec<(&str, &[u8])> = streams.iter().map(|(n, d)| (*n, d.as_slice())).collect();
    let data = build_cfb(&stream_refs);
    let archive = CfbArchive::new(&data, diag()).unwrap();

    // Root + 8 = 9.
    assert_eq!(archive.entries().len(), 9);
    assert_eq!(archive.streams().count(), 8);

    for (name, payload) in &streams {
        let entry = archive
            .find(name)
            .unwrap_or_else(|| panic!("{name} not found"));
        let read = archive.read(entry).unwrap();
        assert_eq!(read.len(), payload.len(), "size mismatch for {name}");
        assert_eq!(read, *payload, "content mismatch for {name}");
    }
}

#[test]
fn test_synthetic_four_streams_boundary() {
    // 4 streams + root = 5 entries = exactly fills sector 1 + 1 entry in sector 2.
    // This is the boundary case: the first stream count that needs 2 dir sectors.
    let data = build_cfb(&[
        ("W", b"w_data"),
        ("X", b"x_data"),
        ("Y", b"y_data"),
        ("Z", b"z_data"),
    ]);
    let archive = CfbArchive::new(&data, diag()).unwrap();
    assert_eq!(archive.entries().len(), 5);

    for name in &["W", "X", "Y", "Z"] {
        let entry = archive
            .find(name)
            .unwrap_or_else(|| panic!("{name} not found"));
        let expected = format!("{}_data", name.to_ascii_lowercase());
        let content = archive.read(entry).unwrap();
        assert_eq!(content, expected.as_bytes(), "content mismatch for {name}");
    }
}

#[test]
fn test_mini_stream_routing() {
    // Hand-built CFB with mini-stream cutoff = 4096. A 12-byte stream routes
    // through the mini-stream path (mini-FAT chain -> root backing store).
    //
    // Layout: header | sector 0 (FAT) | sector 1 (dir) | sector 2 (mini-FAT)
    //         | sector 3 (root backing / mini-stream container)
    const SS: usize = 512;
    const FATSECT: u32 = 0xFFFF_FFFD;
    const ENDOFCHAIN: u32 = 0xFFFF_FFFE;
    const FREESECT: u32 = 0xFFFF_FFFF;
    const NOSTREAM: u32 = 0xFFFF_FFFF;

    let payload = b"Hello, mini!";

    let mut buf = vec![0u8; SS];
    buf[0..8].copy_from_slice(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
    buf[0x18..0x1A].copy_from_slice(&0x003Eu16.to_le_bytes());
    buf[0x1A..0x1C].copy_from_slice(&3u16.to_le_bytes());
    buf[0x1C..0x1E].copy_from_slice(&0xFFFEu16.to_le_bytes());
    buf[0x1E..0x20].copy_from_slice(&9u16.to_le_bytes());
    buf[0x20..0x22].copy_from_slice(&6u16.to_le_bytes());
    buf[0x2C..0x30].copy_from_slice(&1u32.to_le_bytes()); // FAT sector count
    buf[0x30..0x34].copy_from_slice(&1u32.to_le_bytes()); // first dir sector
    buf[0x38..0x3C].copy_from_slice(&4096u32.to_le_bytes()); // mini-stream cutoff
    buf[0x3C..0x40].copy_from_slice(&2u32.to_le_bytes()); // first mini-FAT sector
    buf[0x40..0x44].copy_from_slice(&1u32.to_le_bytes()); // mini-FAT sector count
    buf[0x44..0x48].copy_from_slice(&ENDOFCHAIN.to_le_bytes());
    buf[0x4C..0x50].copy_from_slice(&0u32.to_le_bytes()); // DIFAT[0] = 0
    for i in 1..109 {
        let off = 0x4C + i * 4;
        buf[off..off + 4].copy_from_slice(&FREESECT.to_le_bytes());
    }

    // -- Sector 0: FAT --
    let mut fat = vec![0u8; SS];
    fat[0..4].copy_from_slice(&FATSECT.to_le_bytes());
    fat[4..8].copy_from_slice(&ENDOFCHAIN.to_le_bytes()); // dir
    fat[8..12].copy_from_slice(&ENDOFCHAIN.to_le_bytes()); // mini-FAT
    fat[12..16].copy_from_slice(&ENDOFCHAIN.to_le_bytes()); // root backing
    for i in 4..(SS / 4) {
        let off = i * 4;
        fat[off..off + 4].copy_from_slice(&FREESECT.to_le_bytes());
    }
    buf.extend_from_slice(&fat);

    // -- Sector 1: Directory --
    let mut dir = vec![0u8; SS];
    // Root: child=1, start_sector=3 (backing stream), size=64 (one mini-sector)
    dir[0..128].copy_from_slice(&build_cfb_dir_entry(
        "Root Entry",
        5,
        1,
        NOSTREAM,
        NOSTREAM,
        3,
        64,
    ));
    // SmallStream: mini-sector 0, 12 bytes (< 4096 cutoff -> mini-stream path)
    dir[128..256].copy_from_slice(&build_cfb_dir_entry(
        "SmallStream",
        2,
        NOSTREAM,
        NOSTREAM,
        NOSTREAM,
        0,
        payload.len() as u64,
    ));
    buf.extend_from_slice(&dir);

    // -- Sector 2: Mini-FAT --
    let mut mini_fat = vec![0u8; SS];
    mini_fat[0..4].copy_from_slice(&ENDOFCHAIN.to_le_bytes());
    for i in 1..(SS / 4) {
        let off = i * 4;
        mini_fat[off..off + 4].copy_from_slice(&FREESECT.to_le_bytes());
    }
    buf.extend_from_slice(&mini_fat);

    // -- Sector 3: Root backing stream (mini-stream container) --
    let mut backing = vec![0u8; SS];
    backing[..payload.len()].copy_from_slice(payload);
    buf.extend_from_slice(&backing);

    // -- Verify mini-stream routing --
    let archive = CfbArchive::new(&buf, diag()).unwrap();
    let entry = archive.find("SmallStream").expect("SmallStream not found");
    assert_eq!(entry.size, payload.len() as u64);
    let content = archive.read(entry).unwrap();
    assert_eq!(content, payload, "mini-stream data mismatch");
}

/// Q-006: Root entry with size > 0 but start_sector = ENDOFCHAIN.
///
/// This is a malformed file: the root declares non-zero size for the
/// mini-stream container but points to ENDOFCHAIN instead of a real sector
/// chain. The parser should recover gracefully (empty mini-stream, no panic).
#[test]
fn test_root_entry_size_nonzero_start_endofchain() {
    const SS: usize = 512;
    const FATSECT: u32 = 0xFFFF_FFFD;
    const ENDOFCHAIN: u32 = 0xFFFF_FFFE;
    const FREESECT: u32 = 0xFFFF_FFFF;
    const NOSTREAM: u32 = 0xFFFF_FFFF;

    let mut buf = vec![0u8; SS];
    buf[0..8].copy_from_slice(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
    buf[0x18..0x1A].copy_from_slice(&0x003Eu16.to_le_bytes());
    buf[0x1A..0x1C].copy_from_slice(&3u16.to_le_bytes());
    buf[0x1C..0x1E].copy_from_slice(&0xFFFEu16.to_le_bytes());
    buf[0x1E..0x20].copy_from_slice(&9u16.to_le_bytes());
    buf[0x20..0x22].copy_from_slice(&6u16.to_le_bytes());
    buf[0x2C..0x30].copy_from_slice(&1u32.to_le_bytes()); // FAT sector count
    buf[0x30..0x34].copy_from_slice(&1u32.to_le_bytes()); // first dir sector
    buf[0x38..0x3C].copy_from_slice(&4096u32.to_le_bytes()); // mini-stream cutoff
    buf[0x3C..0x40].copy_from_slice(&ENDOFCHAIN.to_le_bytes()); // no mini-FAT
    buf[0x44..0x48].copy_from_slice(&ENDOFCHAIN.to_le_bytes()); // no DIFAT chain
    buf[0x4C..0x50].copy_from_slice(&0u32.to_le_bytes()); // DIFAT[0] = sector 0
    for i in 1..109 {
        let off = 0x4C + i * 4;
        buf[off..off + 4].copy_from_slice(&FREESECT.to_le_bytes());
    }

    // -- Sector 0: FAT --
    let mut fat = vec![0u8; SS];
    fat[0..4].copy_from_slice(&FATSECT.to_le_bytes()); // sector 0 is FAT
    fat[4..8].copy_from_slice(&ENDOFCHAIN.to_le_bytes()); // sector 1 is dir (one sector)
    for i in 2..(SS / 4) {
        let off = i * 4;
        fat[off..off + 4].copy_from_slice(&FREESECT.to_le_bytes());
    }
    buf.extend_from_slice(&fat);

    // -- Sector 1: Directory --
    let mut dir = vec![0u8; SS];
    // Root entry: size = 1024 (claims non-empty mini-stream), but
    // start_sector = ENDOFCHAIN (no actual chain). This is the malformed case.
    dir[0..128].copy_from_slice(&build_cfb_dir_entry(
        "Root Entry",
        5,
        NOSTREAM,
        NOSTREAM,
        NOSTREAM,
        ENDOFCHAIN,
        1024,
    ));
    buf.extend_from_slice(&dir);

    // Opening should not panic. The parser should recover gracefully.
    let result = CfbArchive::new(&buf, diag());
    assert!(
        result.is_ok(),
        "expected archive to open despite malformed root entry, got: {}",
        result.unwrap_err(),
    );

    let archive = result.unwrap();
    assert_eq!(archive.entries().len(), 1, "should have root entry only");
}

/// Q-006: Stream entry with size > 0 but start_sector = ENDOFCHAIN.
///
/// Similar to the root entry case but for a regular stream. Reading the stream
/// should not panic; it should return truncated/empty data or a clean error.
#[test]
fn test_stream_entry_size_nonzero_start_endofchain() {
    const SS: usize = 512;
    const FATSECT: u32 = 0xFFFF_FFFD;
    const ENDOFCHAIN: u32 = 0xFFFF_FFFE;
    const FREESECT: u32 = 0xFFFF_FFFF;
    const NOSTREAM: u32 = 0xFFFF_FFFF;

    let mut buf = vec![0u8; SS];
    buf[0..8].copy_from_slice(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
    buf[0x18..0x1A].copy_from_slice(&0x003Eu16.to_le_bytes());
    buf[0x1A..0x1C].copy_from_slice(&3u16.to_le_bytes());
    buf[0x1C..0x1E].copy_from_slice(&0xFFFEu16.to_le_bytes());
    buf[0x1E..0x20].copy_from_slice(&9u16.to_le_bytes());
    buf[0x20..0x22].copy_from_slice(&6u16.to_le_bytes());
    buf[0x2C..0x30].copy_from_slice(&1u32.to_le_bytes()); // FAT sector count
    buf[0x30..0x34].copy_from_slice(&1u32.to_le_bytes()); // first dir sector
    buf[0x38..0x3C].copy_from_slice(&1u32.to_le_bytes()); // mini-stream cutoff = 1
    buf[0x3C..0x40].copy_from_slice(&ENDOFCHAIN.to_le_bytes()); // no mini-FAT
    buf[0x44..0x48].copy_from_slice(&ENDOFCHAIN.to_le_bytes()); // no DIFAT chain
    buf[0x4C..0x50].copy_from_slice(&0u32.to_le_bytes()); // DIFAT[0] = sector 0
    for i in 1..109 {
        let off = 0x4C + i * 4;
        buf[off..off + 4].copy_from_slice(&FREESECT.to_le_bytes());
    }

    // -- Sector 0: FAT --
    let mut fat = vec![0u8; SS];
    fat[0..4].copy_from_slice(&FATSECT.to_le_bytes()); // sector 0 is FAT
    fat[4..8].copy_from_slice(&ENDOFCHAIN.to_le_bytes()); // sector 1 is dir
    for i in 2..(SS / 4) {
        let off = i * 4;
        fat[off..off + 4].copy_from_slice(&FREESECT.to_le_bytes());
    }
    buf.extend_from_slice(&fat);

    // -- Sector 1: Directory --
    let mut dir = vec![0u8; SS];
    // Root entry: no mini-stream (size 0).
    dir[0..128].copy_from_slice(&build_cfb_dir_entry(
        "Root Entry",
        5,
        1,
        NOSTREAM,
        NOSTREAM,
        0,
        0,
    ));
    // Stream entry: claims 500 bytes but start_sector = ENDOFCHAIN.
    // With mini_stream_cutoff = 1, this 500-byte stream routes through
    // regular FAT, where follow_chain(ENDOFCHAIN) returns an empty chain.
    dir[128..256].copy_from_slice(&build_cfb_dir_entry(
        "BadStream",
        2,
        NOSTREAM,
        NOSTREAM,
        NOSTREAM,
        ENDOFCHAIN,
        500,
    ));
    buf.extend_from_slice(&dir);

    // Opening should succeed (the root itself is fine).
    let archive =
        CfbArchive::new(&buf, diag()).expect("archive should open despite malformed stream entry");

    let entry = archive
        .find("BadStream")
        .expect("BadStream should be listed");
    assert_eq!(entry.size, 500);

    // Reading the stream should not panic. It may return empty/short data
    // (the chain is empty so no sectors are gathered).
    let content = archive
        .read(entry)
        .expect("read should succeed with empty/short data");
    assert!(
        content.is_empty(),
        "expected empty data from ENDOFCHAIN start, got {} bytes",
        content.len(),
    );
}

#[test]
fn test_real_xls_opens() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let xls_path = std::path::Path::new(manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/table-gt/icdar2013/eu-dataset/eu-001-fnc.xls");

    if !xls_path.exists() {
        eprintln!("skipping test_real_xls_opens: {:?} not found", xls_path);
        return;
    }

    let data = std::fs::read(&xls_path).unwrap();
    let archive = CfbArchive::new(&data, diag())
        .unwrap_or_else(|e| panic!("failed to open {:?}: {e}", xls_path));

    // Real XLS files contain either "Workbook" (BIFF8) or "Book" (BIFF5).
    let has_workbook = archive.find("Workbook").is_some() || archive.find("Book").is_some();
    assert!(
        has_workbook,
        "expected 'Workbook' or 'Book' stream in {:?}, found entries: {:?}",
        xls_path,
        archive
            .entries()
            .iter()
            .map(|e| &e.name)
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_real_xls_corpus() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let corpus_dir = std::path::Path::new(manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/table-gt/icdar2013/eu-dataset");

    if !corpus_dir.exists() {
        eprintln!("skipping test_real_xls_corpus: {:?} not found", corpus_dir);
        return;
    }

    let mut xls_count = 0;
    for dir_entry in std::fs::read_dir(&corpus_dir).unwrap() {
        let dir_entry = dir_entry.unwrap();
        let path = dir_entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("xls") {
            continue;
        }

        let data = std::fs::read(&path).unwrap();
        let archive = CfbArchive::new(&data, diag())
            .unwrap_or_else(|e| panic!("failed to open {:?}: {e}", path));

        let stream_count = archive.streams().count();
        assert!(
            stream_count >= 1,
            "{:?} has no streams (entries: {:?})",
            path,
            archive
                .entries()
                .iter()
                .map(|e| &e.name)
                .collect::<Vec<_>>()
        );
        xls_count += 1;
    }

    assert!(xls_count > 0, "no .xls files found in {:?}", corpus_dir);
}
