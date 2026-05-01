//! Shared test helpers for udoc-containers tests.
//!
//! Available in unit tests unconditionally and in integration tests via the
//! `test-internals` Cargo feature.

/// Build a 128-byte raw CFB directory entry for use in hand-built test containers.
///
/// Fields: name (UTF-16LE), entry type (1=storage, 2=stream, 5=root), child/left/right
/// sibling IDs, start sector, and stream size. Matches the on-disk 128-byte directory
/// entry format from the MS-CFB spec.
pub fn build_cfb_dir_entry(
    name: &str,
    entry_type: u8,
    child: u32,
    left: u32,
    right: u32,
    start_sector: u32,
    size: u64,
) -> [u8; 128] {
    let mut buf = [0u8; 128];
    let utf16: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let name_bytes = utf16.len() * 2;
    for (i, &ch) in utf16.iter().enumerate() {
        buf[i * 2..i * 2 + 2].copy_from_slice(&ch.to_le_bytes());
    }
    buf[0x40..0x42].copy_from_slice(&(name_bytes as u16).to_le_bytes());
    buf[0x42] = entry_type;
    buf[0x44..0x48].copy_from_slice(&left.to_le_bytes());
    buf[0x48..0x4C].copy_from_slice(&right.to_le_bytes());
    buf[0x4C..0x50].copy_from_slice(&child.to_le_bytes());
    buf[0x74..0x78].copy_from_slice(&start_sector.to_le_bytes());
    buf[0x78..0x80].copy_from_slice(&size.to_le_bytes());
    buf
}

// ---------------------------------------------------------------------------
// Shared OPC/OOXML XML fixture constants for integration tests.
// Backends should import these rather than copy-pasting the same XML.
// ---------------------------------------------------------------------------

/// Minimal `[Content_Types].xml` for a DOCX with only `word/document.xml`.
pub const DOCX_CONTENT_TYPES: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

/// Package-level `.rels` pointing to `word/document.xml` only.
pub const DOCX_PACKAGE_RELS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
        Target="word/document.xml"/>
</Relationships>"#;

/// Package-level `.rels` pointing to both `word/document.xml` and `docProps/core.xml`.
pub const DOCX_PACKAGE_RELS_WITH_CORE: &[u8] =
    br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
        Target="word/document.xml"/>
    <Relationship Id="rId2"
        Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties"
        Target="docProps/core.xml"/>
</Relationships>"#;

// ---------------------------------------------------------------------------
// Shared XLSX XML fixture constants for integration tests.
// ---------------------------------------------------------------------------

/// Minimal `[Content_Types].xml` for an XLSX with only `xl/workbook.xml`.
pub const XLSX_CONTENT_TYPES: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
    <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
    <Default Extension="xml" ContentType="application/xml"/>
    <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
</Types>"#;

/// Package-level `.rels` pointing to `xl/workbook.xml` only.
pub const XLSX_PACKAGE_RELS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
        Target="xl/workbook.xml"/>
</Relationships>"#;

/// Package-level `.rels` pointing to both `xl/workbook.xml` and `docProps/core.xml`.
pub const XLSX_PACKAGE_RELS_WITH_CORE: &[u8] =
    br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
        Target="xl/workbook.xml"/>
    <Relationship Id="rId2"
        Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties"
        Target="docProps/core.xml"/>
</Relationships>"#;

/// Single-sheet workbook.xml for XLSX test fixtures.
pub const XLSX_WORKBOOK_1SHEET: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
    xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
    <sheets>
        <sheet name="Sheet1" sheetId="1" r:id="rId1"/>
    </sheets>
</workbook>"#;

/// Single-sheet workbook relationship pointing to `worksheets/sheet1.xml`.
pub const XLSX_WB_RELS_1SHEET: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    <Relationship Id="rId1"
        Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"
        Target="worksheets/sheet1.xml"/>
</Relationships>"#;

// ---------------------------------------------------------------------------
// Shared PPTX XML fixture constants for integration tests.
// ---------------------------------------------------------------------------

/// `[Content_Types].xml` for a PPTX with a single slide.
pub const PPTX_CONTENT_TYPES_1SLIDE: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
  <Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
</Types>"#;

/// `[Content_Types].xml` for a PPTX with two slides.
pub const PPTX_CONTENT_TYPES_2SLIDES: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
  <Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
  <Override PartName="/ppt/slides/slide2.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
</Types>"#;

/// `[Content_Types].xml` for a PPTX with one slide and a notes slide.
pub const PPTX_CONTENT_TYPES_NOTES: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
  <Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
  <Override PartName="/ppt/notesSlides/notesSlide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.notesSlide+xml"/>
</Types>"#;

/// Package-level `.rels` pointing to `ppt/presentation.xml`.
pub const PPTX_PACKAGE_RELS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
</Relationships>"#;

/// Presentation XML referencing a single slide via rId2.
pub const PPTX_PRESENTATION_1SLIDE: &[u8] =
    br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
                xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <p:sldIdLst>
    <p:sldId id="256" r:id="rId2"/>
  </p:sldIdLst>
</p:presentation>"#;

/// Presentation XML referencing two slides via rId2 and rId3.
pub const PPTX_PRESENTATION_2SLIDES: &[u8] =
    br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
                xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <p:sldIdLst>
    <p:sldId id="256" r:id="rId2"/>
    <p:sldId id="257" r:id="rId3"/>
  </p:sldIdLst>
</p:presentation>"#;

/// Presentation rels for a single slide (rId2 -> slides/slide1.xml).
pub const PPTX_PRES_RELS_1SLIDE: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
</Relationships>"#;

/// Presentation rels for two slides (rId2 + rId3).
pub const PPTX_PRES_RELS_2SLIDES: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/>
</Relationships>"#;

/// Empty slide rels (no notes or other per-slide relationships).
pub const PPTX_SLIDE_RELS_EMPTY: &[u8] =
    br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;

/// Slide rels with a notesSlide relationship.
pub const PPTX_SLIDE_RELS_WITH_NOTES: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide" Target="../notesSlides/notesSlide1.xml"/>
</Relationships>"#;

/// Build a stored-only ZIP archive from name/content pairs.
///
/// Each entry uses compression method 0 (stored). Central directory and EOCD
/// are written correctly. Sufficient for OPC and XML parsing tests.
pub fn build_stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    debug_assert!(
        entries.len() <= u16::MAX as usize,
        "EOCD entry count is u16, max 65535 entries"
    );
    let mut buf = Vec::<u8>::new();
    let mut local_offsets = Vec::<u32>::new();

    for (name, content) in entries {
        let crc = crc32fast::hash(content);
        let name_bytes = name.as_bytes();
        local_offsets.push(buf.len() as u32);

        // Local file header
        buf.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04]);
        buf.extend_from_slice(&20u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // flags
        buf.extend_from_slice(&0u16.to_le_bytes()); // method: stored
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&crc.to_le_bytes());
        buf.extend_from_slice(&(content.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(content.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(name_bytes);
        buf.extend_from_slice(content);
    }

    // Central directory
    let cd_start = buf.len() as u32;
    for (i, (name, content)) in entries.iter().enumerate() {
        let crc = crc32fast::hash(content);
        let name_bytes = name.as_bytes();

        buf.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02]);
        buf.extend_from_slice(&20u16.to_le_bytes());
        buf.extend_from_slice(&20u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // flags
        buf.extend_from_slice(&0u16.to_le_bytes()); // method: stored
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&crc.to_le_bytes());
        buf.extend_from_slice(&(content.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(content.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&local_offsets[i].to_le_bytes());
        buf.extend_from_slice(name_bytes);
    }

    let cd_size = buf.len() as u32 - cd_start;
    let count = entries.len() as u16;

    // EOCD
    buf.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06]);
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&count.to_le_bytes());
    buf.extend_from_slice(&count.to_le_bytes());
    buf.extend_from_slice(&cd_size.to_le_bytes());
    buf.extend_from_slice(&cd_start.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());

    buf
}

/// Build a minimal valid v3 CFB file from name/content pairs.
///
/// Supports any number of streams by allocating multiple directory sectors
/// when needed (each 512-byte sector holds 4 x 128-byte directory entries).
/// Streams are stored in regular FAT sectors with `mini_stream_cutoff = 1`
/// so all non-empty streams use the regular path.
///
/// Note: streams are linked as a right-only sibling chain (entry 1 is root's
/// child, entry 2 is 1's right sibling, etc.). For left-sibling coverage,
/// build containers manually with `build_cfb_dir_entry`.
pub fn build_cfb(streams: &[(&str, &[u8])]) -> Vec<u8> {
    const SECTOR_SIZE: usize = 512;
    const ENTRIES_PER_SECTOR: usize = SECTOR_SIZE / 128;
    const FATSECT: u32 = 0xFFFF_FFFD;
    const ENDOFCHAIN: u32 = 0xFFFF_FFFE;
    const FREESECT: u32 = 0xFFFF_FFFF;
    const NOSTREAM: u32 = 0xFFFF_FFFF;

    // Directory entry count = root + streams. Sectors needed = ceil(count / 4).
    let dir_entry_count = 1 + streams.len();
    let dir_sector_count = dir_entry_count.div_ceil(ENTRIES_PER_SECTOR);

    // Layout: header | sector 0 (FAT) | sectors 1..dir_sector_count (directory)
    //         | data sectors
    let first_data_sector = 1 + dir_sector_count; // sector 0 is FAT

    let mut data_sectors: Vec<usize> = Vec::new(); // start sector per stream
    let mut next_sector = first_data_sector;
    for (_, content) in streams {
        let count = if content.is_empty() {
            0
        } else {
            content.len().div_ceil(SECTOR_SIZE)
        };
        data_sectors.push(next_sector);
        next_sector += count;
    }
    let total_sectors = next_sector; // includes FAT + dir + data sectors

    // Sanity check: single FAT sector holds 128 entries, enough for 128 sectors.
    assert!(
        total_sectors <= SECTOR_SIZE / 4,
        "build_cfb: total sectors ({}) exceeds single FAT sector capacity ({})",
        total_sectors,
        SECTOR_SIZE / 4,
    );

    // -- Header (512 bytes) --
    let mut buf = vec![0u8; SECTOR_SIZE];
    buf[0..8].copy_from_slice(&crate::cfb::CFB_MAGIC);
    buf[0x18..0x1A].copy_from_slice(&0x003Eu16.to_le_bytes()); // minor version
    buf[0x1A..0x1C].copy_from_slice(&3u16.to_le_bytes()); // major version (v3)
    buf[0x1C..0x1E].copy_from_slice(&0xFFFEu16.to_le_bytes()); // byte order
    buf[0x1E..0x20].copy_from_slice(&9u16.to_le_bytes()); // sector shift (512)
    buf[0x20..0x22].copy_from_slice(&6u16.to_le_bytes()); // mini sector shift (64)
    buf[0x2C..0x30].copy_from_slice(&1u32.to_le_bytes()); // FAT sector count
    buf[0x30..0x34].copy_from_slice(&1u32.to_le_bytes()); // first directory sector
    buf[0x38..0x3C].copy_from_slice(&1u32.to_le_bytes()); // mini-stream cutoff (1 = all non-empty streams use regular FAT)
    buf[0x3C..0x40].copy_from_slice(&ENDOFCHAIN.to_le_bytes()); // first mini-FAT sector
    buf[0x44..0x48].copy_from_slice(&ENDOFCHAIN.to_le_bytes()); // first DIFAT sector
                                                                // DIFAT[0] = 0 (FAT is sector 0)
    buf[0x4C..0x50].copy_from_slice(&0u32.to_le_bytes());
    // DIFAT[1..109] = FREESECT
    for i in 1..109 {
        let off = 0x4C + i * 4;
        buf[off..off + 4].copy_from_slice(&FREESECT.to_le_bytes());
    }

    // -- Sector 0: FAT --
    let mut fat = vec![0u8; SECTOR_SIZE];
    fat[0..4].copy_from_slice(&FATSECT.to_le_bytes()); // FAT[0] = FATSECT

    // Chain directory sectors: 1 -> 2 -> ... -> dir_sector_count, last = ENDOFCHAIN.
    for d in 0..dir_sector_count {
        let sector_id = 1 + d;
        let val = if d + 1 < dir_sector_count {
            (sector_id + 1) as u32
        } else {
            ENDOFCHAIN
        };
        let off = sector_id * 4;
        fat[off..off + 4].copy_from_slice(&val.to_le_bytes());
    }

    // Chain data sectors for each stream.
    for (i, (_, content)) in streams.iter().enumerate() {
        if content.is_empty() {
            continue;
        }
        let start = data_sectors[i];
        let count = content.len().div_ceil(SECTOR_SIZE);
        for j in 0..count {
            let sector_id = start + j;
            let val = if j + 1 < count {
                (sector_id + 1) as u32
            } else {
                ENDOFCHAIN
            };
            let off = sector_id * 4;
            fat[off..off + 4].copy_from_slice(&val.to_le_bytes());
        }
    }
    // Fill remaining FAT entries with FREESECT.
    for i in total_sectors..(SECTOR_SIZE / 4) {
        let off = i * 4;
        fat[off..off + 4].copy_from_slice(&FREESECT.to_le_bytes());
    }
    buf.extend_from_slice(&fat);

    // -- Directory sectors --
    // Build all directory entries as a flat byte vec, then split into sectors.
    let dir_bytes_total = dir_sector_count * SECTOR_SIZE;
    let mut dir = vec![0u8; dir_bytes_total];

    // Entry 0: Root Entry. child_id = 1 if streams exist, else NOSTREAM.
    let root_child = if streams.is_empty() { NOSTREAM } else { 1 };
    dir[0..128].copy_from_slice(&build_cfb_dir_entry(
        "Root Entry",
        5,
        root_child,
        NOSTREAM,
        NOSTREAM,
        0,
        0,
    ));

    // Entries 1+: streams. Entry 1 is root's child; entries 2+ are right siblings.
    for (i, (name, content)) in streams.iter().enumerate() {
        let slot_start = (i + 1) * 128;
        let right = if i + 1 < streams.len() {
            (i + 2) as u32
        } else {
            NOSTREAM
        };
        let start = if content.is_empty() {
            0
        } else {
            data_sectors[i] as u32
        };
        dir[slot_start..slot_start + 128].copy_from_slice(&build_cfb_dir_entry(
            name,
            2,
            NOSTREAM,
            NOSTREAM,
            right,
            start,
            content.len() as u64,
        ));
    }
    buf.extend_from_slice(&dir);

    // -- Data sectors --
    for (_, content) in streams {
        if content.is_empty() {
            continue;
        }
        let count = content.len().div_ceil(SECTOR_SIZE);
        for j in 0..count {
            let chunk_start = j * SECTOR_SIZE;
            let chunk_end = (chunk_start + SECTOR_SIZE).min(content.len());
            let mut sector = vec![0u8; SECTOR_SIZE];
            sector[..chunk_end - chunk_start].copy_from_slice(&content[chunk_start..chunk_end]);
            buf.extend_from_slice(&sector);
        }
    }

    buf
}
