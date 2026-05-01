//! Integration tests for the ZIP archive reader.
//!
//! These tests build real ZIP archives in memory and exercise the full
//! parse + decompress stack.

use std::sync::Arc;

use udoc_containers::zip::{CompressionMethod, ZipArchive, ZipConfig};
use udoc_core::diagnostics::{CollectingDiagnostics, NullDiagnostics};

// --------------------------------------------------------------------------
// Shared ZIP builder
// --------------------------------------------------------------------------

/// CRC-32 via crc32fast (same hasher the production code uses).
fn crc32(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

/// A single entry description for the builder.
struct Entry<'a> {
    name: &'a str,
    content: &'a [u8],
    deflate: bool,
}

impl<'a> Entry<'a> {
    fn stored(name: &'a str, content: &'a [u8]) -> Self {
        Self {
            name,
            content,
            deflate: false,
        }
    }

    fn deflated(name: &'a str, content: &'a [u8]) -> Self {
        Self {
            name,
            content,
            deflate: true,
        }
    }
}

/// Build a valid ZIP archive from a list of entries.
///
/// Supports both Stored (method 0) and Deflated (method 8) entries in the
/// same archive. The central directory and EOCD are written correctly so
/// `ZipArchive::new` can parse it without warnings.
fn build_zip(entries: &[Entry<'_>]) -> Vec<u8> {
    use flate2::write::DeflateEncoder;
    use flate2::Compression;
    use std::io::Write;

    let mut buf = Vec::<u8>::new();
    let mut local_offsets = Vec::<u32>::new();
    // Store compressed payloads so we can compute CD fields.
    let mut compressed_payloads = Vec::<Vec<u8>>::new();

    for entry in entries {
        local_offsets.push(buf.len() as u32);

        let payload: Vec<u8> = if entry.deflate {
            let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
            enc.write_all(entry.content).expect("deflate encode");
            enc.finish().expect("deflate finish")
        } else {
            entry.content.to_vec()
        };

        let method: u16 = if entry.deflate { 8 } else { 0 };
        let crc = crc32(entry.content);
        let name_bytes = entry.name.as_bytes();

        // Local file header
        buf.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04]); // signature
        buf.extend_from_slice(&20u16.to_le_bytes()); // version needed
        buf.extend_from_slice(&0u16.to_le_bytes()); // flags
        buf.extend_from_slice(&method.to_le_bytes()); // compression method
        buf.extend_from_slice(&0u16.to_le_bytes()); // mod time
        buf.extend_from_slice(&0u16.to_le_bytes()); // mod date
        buf.extend_from_slice(&crc.to_le_bytes());
        buf.extend_from_slice(&(payload.len() as u32).to_le_bytes()); // compressed size
        buf.extend_from_slice(&(entry.content.len() as u32).to_le_bytes()); // uncompressed size
        buf.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // extra field length
        buf.extend_from_slice(name_bytes);
        buf.extend_from_slice(&payload);

        compressed_payloads.push(payload);
    }

    // Central directory
    let cd_start = buf.len() as u32;
    for (i, entry) in entries.iter().enumerate() {
        let method: u16 = if entry.deflate { 8 } else { 0 };
        let crc = crc32(entry.content);
        let name_bytes = entry.name.as_bytes();
        let payload = &compressed_payloads[i];

        buf.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02]); // signature
        buf.extend_from_slice(&20u16.to_le_bytes()); // version made by
        buf.extend_from_slice(&20u16.to_le_bytes()); // version needed
        buf.extend_from_slice(&0u16.to_le_bytes()); // flags
        buf.extend_from_slice(&method.to_le_bytes()); // compression method
        buf.extend_from_slice(&0u16.to_le_bytes()); // mod time
        buf.extend_from_slice(&0u16.to_le_bytes()); // mod date
        buf.extend_from_slice(&crc.to_le_bytes());
        buf.extend_from_slice(&(payload.len() as u32).to_le_bytes()); // compressed size
        buf.extend_from_slice(&(entry.content.len() as u32).to_le_bytes()); // uncompressed size
        buf.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // extra field length
        buf.extend_from_slice(&0u16.to_le_bytes()); // file comment length
        buf.extend_from_slice(&0u16.to_le_bytes()); // disk number start
        buf.extend_from_slice(&0u16.to_le_bytes()); // internal file attrs
        buf.extend_from_slice(&0u32.to_le_bytes()); // external file attrs
        buf.extend_from_slice(&local_offsets[i].to_le_bytes()); // local header offset
        buf.extend_from_slice(name_bytes);
    }

    let cd_size = buf.len() as u32 - cd_start;
    let count = entries.len() as u16;

    // End of central directory record
    buf.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06]); // signature
    buf.extend_from_slice(&0u16.to_le_bytes()); // disk number
    buf.extend_from_slice(&0u16.to_le_bytes()); // disk with start of CD
    buf.extend_from_slice(&count.to_le_bytes()); // entries on this disk
    buf.extend_from_slice(&count.to_le_bytes()); // total entries
    buf.extend_from_slice(&cd_size.to_le_bytes()); // CD size
    buf.extend_from_slice(&cd_start.to_le_bytes()); // CD offset
    buf.extend_from_slice(&0u16.to_le_bytes()); // comment length

    buf
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------

/// Five entries mixing stored and deflated; read each back and check content.
#[test]
fn mixed_stored_and_deflated_five_entries() {
    let entries = [
        Entry::stored("a.txt", b"stored content one"),
        Entry::deflated(
            "b.txt",
            b"deflated content two -- repetition repetition repetition",
        ),
        Entry::stored("c.txt", b"stored three"),
        Entry::deflated("d.txt", b"deflated four -- AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        Entry::stored("e.txt", b"stored five"),
    ];

    let zip_data = build_zip(&entries);
    let archive = ZipArchive::new(&zip_data, Arc::new(NullDiagnostics)).unwrap();

    assert_eq!(archive.entries().len(), 5);

    // Verify each entry name and content.
    let expected: &[(&str, &[u8])] = &[
        ("a.txt", b"stored content one"),
        (
            "b.txt",
            b"deflated content two -- repetition repetition repetition",
        ),
        ("c.txt", b"stored three"),
        ("d.txt", b"deflated four -- AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        ("e.txt", b"stored five"),
    ];

    for (entry, (expected_name, expected_content)) in archive.entries().iter().zip(expected.iter())
    {
        assert_eq!(
            &entry.name, expected_name,
            "entry name mismatch at {}",
            expected_name
        );
        let data = archive.read(entry).unwrap();
        assert_eq!(
            data, *expected_content,
            "content mismatch for {}",
            expected_name
        );
    }

    // Verify compression methods recorded correctly.
    assert_eq!(archive.entries()[0].method, CompressionMethod::Stored);
    assert_eq!(archive.entries()[1].method, CompressionMethod::Deflated);
    assert_eq!(archive.entries()[2].method, CompressionMethod::Stored);
    assert_eq!(archive.entries()[3].method, CompressionMethod::Deflated);
    assert_eq!(archive.entries()[4].method, CompressionMethod::Stored);
}

/// OOXML-style subdirectory paths parse and are readable.
#[test]
fn ooxml_subdirectory_paths() {
    let document_xml = b"<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body/></w:document>";
    let document_rels = b"<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/></Relationships>";
    let content_types = b"<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"xml\" ContentType=\"application/xml\"/></Types>";
    let package_rels = b"<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>";

    let entries = [
        Entry::stored("[Content_Types].xml", content_types),
        Entry::stored("_rels/.rels", package_rels),
        Entry::stored("word/document.xml", document_xml),
        Entry::stored("word/_rels/document.xml.rels", document_rels),
    ];

    let zip_data = build_zip(&entries);
    let archive = ZipArchive::new(&zip_data, Arc::new(NullDiagnostics)).unwrap();

    assert_eq!(archive.entries().len(), 4);

    // Case-sensitive find for each OOXML path.
    assert!(
        archive.find("[Content_Types].xml").is_some(),
        "missing [Content_Types].xml"
    );
    assert!(archive.find("_rels/.rels").is_some(), "missing _rels/.rels");
    assert!(
        archive.find("word/document.xml").is_some(),
        "missing word/document.xml"
    );
    assert!(
        archive.find("word/_rels/document.xml.rels").is_some(),
        "missing word/_rels/document.xml.rels"
    );

    // Read and verify content roundtrips correctly.
    let doc_entry = archive.find("word/document.xml").unwrap();
    let doc_content = archive.read(doc_entry).unwrap();
    assert_eq!(doc_content, document_xml);

    let rels_entry = archive.find("word/_rels/document.xml.rels").unwrap();
    let rels_content = archive.read(rels_entry).unwrap();
    assert_eq!(rels_content, document_rels);

    // Case-insensitive lookup for the content types file (OPC requirement).
    assert!(
        archive.find_ci("[content_types].xml").is_some(),
        "case-insensitive find failed"
    );
}

/// ZIP with 1KB of trailing garbage after EOCD succeeds and emits a warning.
#[test]
fn trailing_garbage_succeeds_with_warning() {
    let entries = [
        Entry::stored("hello.txt", b"Hello, world!"),
        Entry::deflated("data.bin", &[0xAB; 512]),
    ];

    let mut zip_data = build_zip(&entries);

    // Append 1 KB of garbage after the valid archive.
    zip_data.extend(std::iter::repeat_n(0xDE_u8, 1024));

    let diag = Arc::new(CollectingDiagnostics::new());
    let archive = ZipArchive::new(&zip_data, diag.clone()).unwrap();

    // Archive must still be fully readable.
    assert_eq!(archive.entries().len(), 2);

    let hello = archive.find("hello.txt").unwrap();
    assert_eq!(archive.read(hello).unwrap(), b"Hello, world!");

    let data = archive.find("data.bin").unwrap();
    assert_eq!(archive.read(data).unwrap(), vec![0xAB_u8; 512]);

    // A trailing-data warning must have been emitted.
    let warnings = diag.warnings();
    assert!(
        warnings.iter().any(|w| w.kind == "ZipTrailingData"),
        "expected ZipTrailingData warning; got: {warnings:?}"
    );
}

/// `read_string` returns correct UTF-8 text for stored and deflated entries.
#[test]
fn read_string_stored_and_deflated() {
    let xml = b"<?xml version=\"1.0\"?><root>hello</root>";
    let big_text: Vec<u8> = b"The quick brown fox jumped over the lazy dog. "
        .iter()
        .cycle()
        .take(512)
        .copied()
        .collect();

    let entries = [
        Entry::stored("small.xml", xml),
        Entry::deflated("big.txt", &big_text),
    ];

    let zip_data = build_zip(&entries);
    let archive = ZipArchive::new(&zip_data, Arc::new(NullDiagnostics)).unwrap();

    let small = archive.find("small.xml").unwrap();
    assert_eq!(
        archive.read_string(small).unwrap(),
        std::str::from_utf8(xml).unwrap()
    );

    let big = archive.find("big.txt").unwrap();
    assert_eq!(
        archive.read_string(big).unwrap(),
        std::str::from_utf8(&big_text).unwrap()
    );
}

/// Resource limit is enforced on deflated entries that expand too large.
#[test]
fn resource_limit_on_deflated_entry() {
    // 200 bytes of highly compressible data.
    let content = vec![b'A'; 200];
    let entries = [Entry::deflated("big.txt", &content)];

    let zip_data = build_zip(&entries);

    let config = ZipConfig {
        max_decompressed_size: 10, // tiny limit
        ..ZipConfig::default()
    };
    let archive = ZipArchive::with_config(&zip_data, Arc::new(NullDiagnostics), config).unwrap();
    let result = archive.read(archive.entries().first().unwrap());

    assert!(result.is_err(), "expected resource limit error");
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("resource limit") || msg.contains("decompressed size"),
        "unexpected error message: {msg}"
    );
}

/// `find` returns None for unknown names; `find_ci` matches case-insensitively.
#[test]
fn find_and_find_ci_semantics() {
    let entries = [
        Entry::stored("[Content_Types].xml", b"<T/>"),
        Entry::stored("Word/Document.XML", b"<d/>"),
    ];

    let zip_data = build_zip(&entries);
    let archive = ZipArchive::new(&zip_data, Arc::new(NullDiagnostics)).unwrap();

    // Exact match works.
    assert!(archive.find("[Content_Types].xml").is_some());

    // Wrong case fails exact match.
    assert!(archive.find("[content_types].xml").is_none());

    // Case-insensitive match works.
    assert!(archive.find_ci("[content_types].xml").is_some());
    assert!(archive.find_ci("[CONTENT_TYPES].XML").is_some());
    assert!(archive.find_ci("word/document.xml").is_some());

    // Completely missing entry returns None from both.
    assert!(archive.find("missing.txt").is_none());
    assert!(archive.find_ci("missing.txt").is_none());
}

/// ZIP64 archive: standard EOCD has sentinel values, real metadata in ZIP64 EOCD.
#[test]
fn zip64_archive_roundtrip() {
    let content = b"zip64 content here";
    let name = b"data.txt";
    let crc = crc32(content);
    let mut buf = Vec::new();

    // Local file header
    let local_offset = buf.len() as u32;
    buf.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04]);
    buf.extend_from_slice(&45u16.to_le_bytes()); // version needed (4.5 for ZIP64)
    buf.extend_from_slice(&0u16.to_le_bytes()); // flags
    buf.extend_from_slice(&0u16.to_le_bytes()); // method: stored
    buf.extend_from_slice(&0u16.to_le_bytes()); // mod time
    buf.extend_from_slice(&0u16.to_le_bytes()); // mod date
    buf.extend_from_slice(&crc.to_le_bytes());
    buf.extend_from_slice(&(content.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(content.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(name.len() as u16).to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes()); // extra len
    buf.extend_from_slice(name);
    buf.extend_from_slice(content);

    // Central directory
    let cd_offset = buf.len() as u64;
    buf.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02]);
    buf.extend_from_slice(&45u16.to_le_bytes()); // version made by
    buf.extend_from_slice(&45u16.to_le_bytes()); // version needed
    buf.extend_from_slice(&0u16.to_le_bytes()); // flags
    buf.extend_from_slice(&0u16.to_le_bytes()); // method
    buf.extend_from_slice(&0u16.to_le_bytes()); // mod time
    buf.extend_from_slice(&0u16.to_le_bytes()); // mod date
    buf.extend_from_slice(&crc.to_le_bytes());
    buf.extend_from_slice(&(content.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(content.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(name.len() as u16).to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes()); // extra len
    buf.extend_from_slice(&0u16.to_le_bytes()); // comment len
    buf.extend_from_slice(&0u16.to_le_bytes()); // disk start
    buf.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
    buf.extend_from_slice(&0u32.to_le_bytes()); // external attrs
    buf.extend_from_slice(&local_offset.to_le_bytes());
    buf.extend_from_slice(name);

    let cd_size = (buf.len() as u64) - cd_offset;

    // ZIP64 EOCD record
    let z64_eocd_offset = buf.len() as u64;
    buf.extend_from_slice(&[0x50, 0x4B, 0x06, 0x06]); // signature
    buf.extend_from_slice(&44u64.to_le_bytes()); // size of remaining ZIP64 EOCD
    buf.extend_from_slice(&45u16.to_le_bytes()); // version made by
    buf.extend_from_slice(&45u16.to_le_bytes()); // version needed
    buf.extend_from_slice(&0u32.to_le_bytes()); // disk number
    buf.extend_from_slice(&0u32.to_le_bytes()); // disk with CD start
    buf.extend_from_slice(&1u64.to_le_bytes()); // entries on this disk
    buf.extend_from_slice(&1u64.to_le_bytes()); // total entries
    buf.extend_from_slice(&cd_size.to_le_bytes()); // CD size
    buf.extend_from_slice(&cd_offset.to_le_bytes()); // CD offset

    // ZIP64 EOCD locator
    buf.extend_from_slice(&[0x50, 0x4B, 0x06, 0x07]); // signature
    buf.extend_from_slice(&0u32.to_le_bytes()); // disk with ZIP64 EOCD
    buf.extend_from_slice(&z64_eocd_offset.to_le_bytes()); // ZIP64 EOCD offset
    buf.extend_from_slice(&1u32.to_le_bytes()); // total disks

    // Standard EOCD with sentinel values forcing ZIP64 path
    buf.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06]); // signature
    buf.extend_from_slice(&0u16.to_le_bytes()); // disk number
    buf.extend_from_slice(&0u16.to_le_bytes()); // disk with CD start
    buf.extend_from_slice(&0xFFFFu16.to_le_bytes()); // entries on disk (sentinel)
    buf.extend_from_slice(&0xFFFFu16.to_le_bytes()); // total entries (sentinel)
    buf.extend_from_slice(&0xFFFFFFFFu32.to_le_bytes()); // CD size (sentinel)
    buf.extend_from_slice(&0xFFFFFFFFu32.to_le_bytes()); // CD offset (sentinel)
    buf.extend_from_slice(&0u16.to_le_bytes()); // comment len

    let archive = ZipArchive::new(&buf, Arc::new(NullDiagnostics)).unwrap();
    assert_eq!(archive.entries().len(), 1);
    assert_eq!(archive.entries()[0].name, "data.txt");

    let data = archive.read(&archive.entries()[0]).unwrap();
    assert_eq!(data, content);
}
