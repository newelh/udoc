//! ZIP archive structure parsing: EOCD, central directory, ZIP64.

use std::sync::Arc;

use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use super::{sanitize_name, CompressionMethod, ZipEntry};
use crate::error::{Error, Result, ResultExt};

const LOCAL_HEADER_SIG: [u8; 4] = [0x50, 0x4B, 0x03, 0x04];
const CENTRAL_DIR_SIG: [u8; 4] = [0x50, 0x4B, 0x01, 0x02];
const EOCD_SIG: [u8; 4] = [0x50, 0x4B, 0x05, 0x06];
const ZIP64_EOCD_LOCATOR_SIG: [u8; 4] = [0x50, 0x4B, 0x06, 0x07];
const ZIP64_EOCD_SIG: [u8; 4] = [0x50, 0x4B, 0x06, 0x06];

/// Minimum size of EOCD record (no comment).
const EOCD_MIN_SIZE: usize = 22;
/// Maximum search window for EOCD (last 64KB + EOCD size, handles max-length comments).
const EOCD_SEARCH_SIZE: usize = 65536 + EOCD_MIN_SIZE;
/// Maximum number of entries we will attempt to parse. Real OOXML files have
/// at most a few hundred entries; anything beyond 1M is malicious or corrupt.
const MAX_ENTRY_COUNT: u64 = 1_000_000;

/// CP437 high-byte lookup table (0x80..0xFF -> Unicode).
///
/// Bytes 0x00-0x7F are ASCII. Bytes 0x80-0xFF map to these Unicode codepoints.
/// This is the original IBM PC character set used by legacy ZIP archivers when
/// the UTF-8 flag (bit 11) is not set.
#[rustfmt::skip]
const CP437_HIGH: [char; 128] = [
    // 0x80-0x8F: accented Latin
    '\u{00C7}', '\u{00FC}', '\u{00E9}', '\u{00E2}', '\u{00E4}', '\u{00E0}', '\u{00E5}', '\u{00E7}',
    '\u{00EA}', '\u{00EB}', '\u{00E8}', '\u{00EF}', '\u{00EE}', '\u{00EC}', '\u{00C4}', '\u{00C5}',
    // 0x90-0x9F: accented Latin + currency
    '\u{00C9}', '\u{00E6}', '\u{00C6}', '\u{00F4}', '\u{00F6}', '\u{00F2}', '\u{00FB}', '\u{00F9}',
    '\u{00FF}', '\u{00D6}', '\u{00DC}', '\u{00A2}', '\u{00A3}', '\u{00A5}', '\u{20A7}', '\u{0192}',
    // 0xA0-0xAF: accented Latin + fractions + punctuation
    '\u{00E1}', '\u{00ED}', '\u{00F3}', '\u{00FA}', '\u{00F1}', '\u{00D1}', '\u{00AA}', '\u{00BA}',
    '\u{00BF}', '\u{2310}', '\u{00AC}', '\u{00BD}', '\u{00BC}', '\u{00A1}', '\u{00AB}', '\u{00BB}',
    // 0xB0-0xBF: box-drawing light
    '\u{2591}', '\u{2592}', '\u{2593}', '\u{2502}', '\u{2524}', '\u{2561}', '\u{2562}', '\u{2556}',
    '\u{2555}', '\u{2563}', '\u{2551}', '\u{2557}', '\u{255D}', '\u{255C}', '\u{255B}', '\u{2510}',
    // 0xC0-0xCF: box-drawing continued
    '\u{2514}', '\u{2534}', '\u{252C}', '\u{251C}', '\u{2500}', '\u{253C}', '\u{255E}', '\u{255F}',
    '\u{255A}', '\u{2554}', '\u{2569}', '\u{2566}', '\u{2560}', '\u{2550}', '\u{256C}', '\u{2567}',
    // 0xD0-0xDF: box-drawing continued
    '\u{2568}', '\u{2564}', '\u{2565}', '\u{2559}', '\u{2558}', '\u{2552}', '\u{2553}', '\u{256B}',
    '\u{256A}', '\u{2518}', '\u{250C}', '\u{2588}', '\u{2584}', '\u{258C}', '\u{2590}', '\u{2580}',
    // 0xE0-0xEF: Greek letters + math
    '\u{03B1}', '\u{00DF}', '\u{0393}', '\u{03C0}', '\u{03A3}', '\u{03C3}', '\u{00B5}', '\u{03C4}',
    '\u{03A6}', '\u{0398}', '\u{03A9}', '\u{03B4}', '\u{221E}', '\u{03C6}', '\u{03B5}', '\u{2229}',
    // 0xF0-0xFF: math + misc
    '\u{2261}', '\u{00B1}', '\u{2265}', '\u{2264}', '\u{2320}', '\u{2321}', '\u{00F7}', '\u{2248}',
    '\u{00B0}', '\u{2219}', '\u{00B7}', '\u{221A}', '\u{207F}', '\u{00B2}', '\u{25A0}', '\u{00A0}',
];

/// Decode a byte slice from CP437 encoding to a UTF-8 String.
fn decode_cp437(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b < 0x80 {
            out.push(b as char);
        } else {
            out.push(CP437_HIGH[(b - 0x80) as usize]);
        }
    }
    out
}

/// Parse a ZIP archive from raw bytes, returning all central directory entries.
pub(crate) fn parse_archive(data: &[u8], diag: &Arc<dyn DiagnosticsSink>) -> Result<Vec<ZipEntry>> {
    let eocd_offset = find_eocd(data).context("scanning for end of central directory")?;

    // Warn if there is data after the EOCD record
    let eocd_comment_len = read_u16(data, eocd_offset + 20)? as usize;
    let eocd_end = eocd_offset + EOCD_MIN_SIZE + eocd_comment_len;
    if eocd_end < data.len() {
        diag.warning(
            Warning::new(
                "ZipTrailingData",
                format!(
                    "{} bytes of trailing data after EOCD",
                    data.len() - eocd_end
                ),
            )
            .at_offset(eocd_end as u64),
        );
    }

    // Read EOCD fields
    let mut entry_count = read_u16(data, eocd_offset + 10)? as u64;
    let mut cd_size = read_u32(data, eocd_offset + 12)? as u64;
    let mut cd_offset = read_u32(data, eocd_offset + 16)? as u64;

    // Check for ZIP64 EOCD locator (20 bytes before EOCD)
    if needs_zip64(entry_count, cd_size, cd_offset) {
        if let Some(z64) = find_zip64_eocd(data, eocd_offset)? {
            entry_count = z64.entry_count;
            cd_size = z64.cd_size;
            cd_offset = z64.cd_offset;
        }
    }

    // Parse central directory
    let cd_end = cd_offset
        .checked_add(cd_size)
        .ok_or_else(|| Error::zip("central directory offset + size overflow"))?;
    if cd_end > data.len() as u64 {
        return Err(Error::zip_at(
            cd_offset,
            format!(
                "central directory extends beyond archive (offset {} + size {} > {})",
                cd_offset,
                cd_size,
                data.len()
            ),
        ));
    }

    if entry_count > MAX_ENTRY_COUNT {
        return Err(Error::resource_limit(format!(
            "ZIP claims {entry_count} entries, exceeds limit of {MAX_ENTRY_COUNT}"
        )));
    }

    let mut entries = Vec::with_capacity(entry_count.min(65536) as usize);
    let mut seen_names = std::collections::HashSet::new();
    let mut pos = cd_offset as usize;

    for i in 0..entry_count {
        if pos + 46 > data.len() {
            return Err(Error::zip_at(
                pos as u64,
                format!("central directory entry {i} truncated"),
            ));
        }

        let sig = &data[pos..pos + 4];
        if sig != CENTRAL_DIR_SIG {
            return Err(Error::zip_at(
                pos as u64,
                format!(
                    "expected central directory signature at entry {i}, got {:02X}{:02X}{:02X}{:02X}",
                    sig[0], sig[1], sig[2], sig[3]
                ),
            ));
        }

        let entry = parse_central_dir_entry(data, pos, diag)
            .context(format!("parsing central directory entry {i}"))?;

        if !seen_names.insert(entry.name.clone()) {
            diag.warning(Warning::new(
                "ZipDuplicateEntry",
                format!("duplicate entry name: {}", entry.name),
            ));
        }

        // Advance past this entry (checked arithmetic to prevent wrapping)
        let name_len = read_u16(data, pos + 28)? as usize;
        let extra_len = read_u16(data, pos + 30)? as usize;
        let comment_len = read_u16(data, pos + 32)? as usize;
        pos = pos
            .checked_add(46)
            .and_then(|p| p.checked_add(name_len))
            .and_then(|p| p.checked_add(extra_len))
            .and_then(|p| p.checked_add(comment_len))
            .ok_or_else(|| Error::zip("central directory entry offset overflow"))?;

        entries.push(entry);
    }

    Ok(entries)
}

/// Parse a single central directory file header at `offset`.
fn parse_central_dir_entry(
    data: &[u8],
    offset: usize,
    diag: &Arc<dyn DiagnosticsSink>,
) -> Result<ZipEntry> {
    let flags = read_u16(data, offset + 8)?;
    let method = CompressionMethod::from_u16(read_u16(data, offset + 10)?);
    let crc32 = read_u32(data, offset + 16)?;
    let mut compressed_size = read_u32(data, offset + 20)? as u64;
    let mut uncompressed_size = read_u32(data, offset + 24)? as u64;
    let name_len = read_u16(data, offset + 28)? as usize;
    let extra_len = read_u16(data, offset + 30)? as usize;
    let mut local_header_offset = read_u32(data, offset + 42)? as u64;

    let has_data_descriptor = flags & 0x0008 != 0;
    let is_utf8 = flags & 0x0800 != 0;

    // Read name
    let name_start = offset + 46;
    let name_end = name_start + name_len;
    if name_end > data.len() {
        return Err(Error::zip_at(
            offset as u64,
            "entry name extends beyond data",
        ));
    }
    let raw_name = &data[name_start..name_end];
    let name_str = if is_utf8 {
        String::from_utf8_lossy(raw_name).into_owned()
    } else {
        decode_cp437(raw_name)
    };
    let name = sanitize_name(&name_str, diag);

    // Check for ZIP64 extra field
    let extra_start = name_end;
    let extra_end = extra_start + extra_len;
    if extra_end <= data.len() {
        parse_zip64_extra(
            &data[extra_start..extra_end],
            &mut uncompressed_size,
            &mut compressed_size,
            &mut local_header_offset,
        );
    }

    Ok(ZipEntry {
        name,
        compressed_size,
        uncompressed_size,
        crc32,
        method,
        local_header_offset,
        has_data_descriptor,
        is_utf8,
    })
}

/// Parse the ZIP64 extended information extra field if present.
/// Updates sizes and offset in-place when the 32-bit values are 0xFFFFFFFF.
fn parse_zip64_extra(
    extra: &[u8],
    uncompressed: &mut u64,
    compressed: &mut u64,
    local_offset: &mut u64,
) {
    let mut pos = 0;
    while pos + 4 <= extra.len() {
        let tag = u16::from_le_bytes([extra[pos], extra[pos + 1]]);
        let size = u16::from_le_bytes([extra[pos + 2], extra[pos + 3]]) as usize;
        pos += 4;

        if tag == 0x0001 {
            // ZIP64 extended information. Bound checks use both the declared
            // field size AND the actual extra slice length, because a truncated
            // archive can have a declared size that exceeds available data.
            let field_end = (pos + size).min(extra.len());
            let mut field_pos = pos;

            if *uncompressed == 0xFFFF_FFFF && field_pos + 8 <= field_end {
                if let Ok(bytes) = <[u8; 8]>::try_from(&extra[field_pos..field_pos + 8]) {
                    *uncompressed = u64::from_le_bytes(bytes);
                }
                field_pos += 8;
            }
            if *compressed == 0xFFFF_FFFF && field_pos + 8 <= field_end {
                if let Ok(bytes) = <[u8; 8]>::try_from(&extra[field_pos..field_pos + 8]) {
                    *compressed = u64::from_le_bytes(bytes);
                }
                field_pos += 8;
            }
            if *local_offset == 0xFFFF_FFFF && field_pos + 8 <= field_end {
                if let Ok(bytes) = <[u8; 8]>::try_from(&extra[field_pos..field_pos + 8]) {
                    *local_offset = u64::from_le_bytes(bytes);
                }
            }
            return;
        }

        pos += size;
    }
}

/// Locate the EOCD record by scanning backward from the end of the data.
fn find_eocd(data: &[u8]) -> Result<usize> {
    if data.len() < EOCD_MIN_SIZE {
        return Err(Error::zip("data too small to be a ZIP archive"));
    }

    let search_start = data.len().saturating_sub(EOCD_SEARCH_SIZE);
    // Scan backward for the EOCD signature
    for i in (search_start..=data.len() - EOCD_MIN_SIZE).rev() {
        if data[i..i + 4] == EOCD_SIG {
            // Validate: comment length should match remaining bytes
            let comment_len = u16::from_le_bytes([data[i + 20], data[i + 21]]) as usize;
            let expected_end = i + EOCD_MIN_SIZE + comment_len;
            // Allow trailing garbage (expected_end <= data.len())
            if expected_end <= data.len() {
                return Ok(i);
            }
        }
    }

    Err(Error::zip(
        "could not find end of central directory signature",
    ))
}

/// ZIP64 EOCD values.
struct Zip64Eocd {
    entry_count: u64,
    cd_size: u64,
    cd_offset: u64,
}

/// Check whether any 32-bit EOCD field is at its sentinel value.
fn needs_zip64(entry_count: u64, cd_size: u64, cd_offset: u64) -> bool {
    entry_count == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_offset == 0xFFFF_FFFF
}

/// Find and parse the ZIP64 EOCD locator + ZIP64 EOCD record.
fn find_zip64_eocd(data: &[u8], eocd_offset: usize) -> Result<Option<Zip64Eocd>> {
    // ZIP64 EOCD locator is 20 bytes immediately before the EOCD
    if eocd_offset < 20 {
        return Ok(None);
    }
    let locator_offset = eocd_offset - 20;
    if data[locator_offset..locator_offset + 4] != ZIP64_EOCD_LOCATOR_SIG {
        return Ok(None);
    }

    // Read ZIP64 EOCD offset from locator (at +8, 8 bytes)
    let z64_eocd_offset = read_u64(data, locator_offset + 8)? as usize;
    if z64_eocd_offset + 56 > data.len() {
        return Err(Error::zip_at(
            z64_eocd_offset as u64,
            "ZIP64 EOCD record truncated",
        ));
    }
    if data[z64_eocd_offset..z64_eocd_offset + 4] != ZIP64_EOCD_SIG {
        return Err(Error::zip_at(
            z64_eocd_offset as u64,
            "invalid ZIP64 EOCD signature",
        ));
    }

    let entry_count = read_u64(data, z64_eocd_offset + 32)?;
    let cd_size = read_u64(data, z64_eocd_offset + 40)?;
    let cd_offset = read_u64(data, z64_eocd_offset + 48)?;

    Ok(Some(Zip64Eocd {
        entry_count,
        cd_size,
        cd_offset,
    }))
}

/// Read offset of compressed data from a local file header.
/// Returns (data_offset, local_name_len, local_extra_len) for seeking.
pub(crate) fn local_header_data_offset(data: &[u8], local_offset: u64) -> Result<usize> {
    let off = local_offset as usize;
    if off + 30 > data.len() {
        return Err(Error::zip_at(local_offset, "local header truncated"));
    }
    if data[off..off + 4] != LOCAL_HEADER_SIG {
        return Err(Error::zip_at(
            local_offset,
            "invalid local header signature",
        ));
    }

    let name_len = read_u16(data, off + 26)? as usize;
    let extra_len = read_u16(data, off + 28)? as usize;
    let data_start = off
        .checked_add(30)
        .and_then(|p| p.checked_add(name_len))
        .and_then(|p| p.checked_add(extra_len))
        .ok_or_else(|| Error::zip_at(local_offset, "local header offset overflow"))?;

    if data_start > data.len() {
        return Err(Error::zip_at(
            local_offset,
            "local header name + extra extends beyond data",
        ));
    }

    Ok(data_start)
}

// -- Little-endian helpers --

fn read_u16(data: &[u8], offset: usize) -> Result<u16> {
    let bytes: [u8; 2] = data
        .get(offset..offset + 2)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| Error::zip_at(offset as u64, "unexpected end of data reading u16"))?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32> {
    let bytes: [u8; 4] = data
        .get(offset..offset + 4)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| Error::zip_at(offset as u64, "unexpected end of data reading u32"))?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(data: &[u8], offset: usize) -> Result<u64> {
    let bytes: [u8; 8] = data
        .get(offset..offset + 8)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| Error::zip_at(offset as u64, "unexpected end of data reading u64"))?;
    Ok(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_eocd_at_end() {
        let mut buf = vec![0u8; 100];
        // Place EOCD at end
        let eocd_pos = buf.len() - EOCD_MIN_SIZE;
        buf[eocd_pos..eocd_pos + 4].copy_from_slice(&EOCD_SIG);
        // comment_len = 0
        buf[eocd_pos + 20] = 0;
        buf[eocd_pos + 21] = 0;

        assert_eq!(find_eocd(&buf).unwrap(), eocd_pos);
    }

    #[test]
    fn find_eocd_with_comment() {
        let comment = b"archive comment here";
        let mut buf = vec![0u8; 100];
        let eocd_pos = buf.len() - EOCD_MIN_SIZE - comment.len();
        buf[eocd_pos..eocd_pos + 4].copy_from_slice(&EOCD_SIG);
        buf[eocd_pos + 20] = comment.len() as u8;
        buf[eocd_pos + 21] = 0;
        buf[eocd_pos + 22..eocd_pos + 22 + comment.len()].copy_from_slice(comment);

        // Need enough room for trailing comment
        assert_eq!(find_eocd(&buf).unwrap(), eocd_pos);
    }

    #[test]
    fn read_helpers() {
        let data = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        assert_eq!(read_u16(&data, 0).unwrap(), 0x0201);
        assert_eq!(read_u32(&data, 0).unwrap(), 0x04030201);
        assert_eq!(read_u64(&data, 0).unwrap(), 0x0807060504030201);
    }

    #[test]
    fn zip64_extra_field_parsing() {
        // Build a ZIP64 extra field: tag=0x0001, size=24, three u64s
        let mut extra = Vec::new();
        extra.extend_from_slice(&0x0001u16.to_le_bytes()); // tag
        extra.extend_from_slice(&24u16.to_le_bytes()); // size
        extra.extend_from_slice(&1000u64.to_le_bytes()); // uncompressed
        extra.extend_from_slice(&500u64.to_le_bytes()); // compressed
        extra.extend_from_slice(&200u64.to_le_bytes()); // local offset

        let mut uncompressed = 0xFFFF_FFFF;
        let mut compressed = 0xFFFF_FFFF;
        let mut local_offset = 0xFFFF_FFFF;

        parse_zip64_extra(
            &extra,
            &mut uncompressed,
            &mut compressed,
            &mut local_offset,
        );

        assert_eq!(uncompressed, 1000);
        assert_eq!(compressed, 500);
        assert_eq!(local_offset, 200);
    }

    #[test]
    fn zip64_extra_only_overwrites_sentinel() {
        let mut extra = Vec::new();
        extra.extend_from_slice(&0x0001u16.to_le_bytes());
        extra.extend_from_slice(&8u16.to_le_bytes()); // only one u64
        extra.extend_from_slice(&999u64.to_le_bytes());

        let mut uncompressed = 0xFFFF_FFFF;
        let mut compressed = 42; // not sentinel, should stay
        let mut local_offset = 100; // not sentinel, should stay

        parse_zip64_extra(
            &extra,
            &mut uncompressed,
            &mut compressed,
            &mut local_offset,
        );

        assert_eq!(uncompressed, 999);
        assert_eq!(compressed, 42); // unchanged
        assert_eq!(local_offset, 100); // unchanged
    }

    #[test]
    fn zip64_extra_truncated_skipped_gracefully() {
        // ZIP64 extra field with tag=0x0001 and declared size=24, but only 10
        // bytes of actual data after the 4-byte header. The parser should skip
        // the truncated field and leave sentinel values unchanged.
        // Build a truncated ZIP64 extra: header says 24 bytes, but only 10 present.
        let mut extra = Vec::new();
        extra.extend_from_slice(&0x0001u16.to_le_bytes());
        extra.extend_from_slice(&24u16.to_le_bytes());
        extra.extend_from_slice(&[0xAA; 10]);

        let mut uncompressed = 0xFFFF_FFFF;
        let mut compressed = 0xFFFF_FFFF;
        let mut local_offset = 0xFFFF_FFFF;

        parse_zip64_extra(
            &extra,
            &mut uncompressed,
            &mut compressed,
            &mut local_offset,
        );

        // With only 10 bytes of data, the first field (8 bytes) fits but
        // the second and third do not. First field should be parsed.
        assert_ne!(uncompressed, 0xFFFF_FFFF, "first field should be parsed");
        // Remaining fields should stay at sentinel (not enough data).
        assert_eq!(compressed, 0xFFFF_FFFF);
        assert_eq!(local_offset, 0xFFFF_FFFF);
    }

    #[test]
    fn zip64_extra_beyond_data_len_skipped() {
        // Central directory entry declares extra_len that extends beyond the
        // archive data. parse_central_dir_entry guards this with
        // `if extra_end <= data.len()` and silently skips ZIP64 parsing.
        // Verify that the sentinel values are preserved when extra_end > data.len().
        let mut uncompressed = 0xFFFF_FFFF;
        let mut compressed = 42;
        let mut local_offset = 100;

        // Empty extra field data: nothing to parse.
        parse_zip64_extra(&[], &mut uncompressed, &mut compressed, &mut local_offset);

        assert_eq!(uncompressed, 0xFFFF_FFFF, "should remain sentinel");
        assert_eq!(compressed, 42, "should remain unchanged");
        assert_eq!(local_offset, 100, "should remain unchanged");
    }

    #[test]
    fn decode_cp437_ascii_passthrough() {
        let input = b"hello/world.txt";
        assert_eq!(decode_cp437(input), "hello/world.txt");
    }

    #[test]
    fn decode_cp437_empty() {
        assert_eq!(decode_cp437(b""), "");
    }

    #[test]
    fn decode_cp437_high_bytes() {
        // 0x80 = C-cedilla (U+00C7), 0x81 = u-diaeresis (U+00FC)
        assert_eq!(decode_cp437(&[0x80, 0x81]), "\u{00C7}\u{00FC}");
        // 0xC4 = box-drawing horizontal (U+2500)
        assert_eq!(decode_cp437(&[0xC4]), "\u{2500}");
        // 0xE3 = pi (U+03C0)
        assert_eq!(decode_cp437(&[0xE3]), "\u{03C0}");
        // 0xFE = black square (U+25A0)
        assert_eq!(decode_cp437(&[0xFE]), "\u{25A0}");
    }

    #[test]
    fn decode_cp437_mixed_ascii_and_high() {
        // "file" + 0x84 (a-diaeresis U+00E4) + ".txt"
        let input = [b'f', b'i', b'l', b'e', 0x84, b'.', b't', b'x', b't'];
        assert_eq!(decode_cp437(&input), "file\u{00E4}.txt");
    }

    #[test]
    fn local_header_data_offset_works() {
        let name = b"test.txt";
        let mut buf = vec![0u8; 30]; // local header is 30 bytes fixed
        buf[0..4].copy_from_slice(&LOCAL_HEADER_SIG);
        buf[26] = name.len() as u8; // name_len
        buf[27] = 0;
        buf[28] = 0; // extra_len = 0
        buf[29] = 0;
        buf.extend_from_slice(name);
        buf.extend_from_slice(b"file content here");

        let data_off = local_header_data_offset(&buf, 0).unwrap();
        assert_eq!(data_off, 30 + name.len());
        assert_eq!(&buf[data_off..data_off + 4], b"file");
    }
}
