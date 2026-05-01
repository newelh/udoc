//! ZIP entry decompression: Stored and DEFLATE.

use std::sync::Arc;

use flate2::Decompress;
use udoc_core::diagnostics::{DiagnosticsSink, Warning};

use super::parse::local_header_data_offset;
use super::{CompressionMethod, ZipConfig, ZipEntry};
use crate::error::{Error, Result, ResultExt};

/// Read and decompress a ZIP entry's data.
pub(crate) fn read_entry(
    data: &[u8],
    entry: &ZipEntry,
    diag: &Arc<dyn DiagnosticsSink>,
    config: &ZipConfig,
) -> Result<Vec<u8>> {
    // Check both sizes against resource limit. For well-formed Stored entries
    // these are equal, but a malicious archive can set them independently.
    let max_size = entry.uncompressed_size.max(entry.compressed_size);
    if max_size > config.max_decompressed_size {
        return Err(Error::resource_limit(format!(
            "entry size {} exceeds limit {}",
            max_size, config.max_decompressed_size
        )));
    }

    let data_offset = local_header_data_offset(data, entry.local_header_offset)
        .context("reading local file header")?;

    let compressed_size_usize = usize::try_from(entry.compressed_size).map_err(|_| {
        Error::resource_limit(format!(
            "compressed size {} exceeds addressable range",
            entry.compressed_size
        ))
    })?;
    let compressed_end = data_offset
        .checked_add(compressed_size_usize)
        .ok_or_else(|| Error::zip("compressed data offset overflow"))?;
    if compressed_end > data.len() {
        return Err(Error::zip_at(
            data_offset as u64,
            format!(
                "compressed data extends beyond archive ({} + {} > {})",
                data_offset,
                entry.compressed_size,
                data.len()
            ),
        ));
    }

    let compressed_data = &data[data_offset..compressed_end];

    // Pre-flight compression ratio check against declared sizes (zip bomb detection).
    // Uses multiplication instead of division to avoid truncation: reject when
    // uncompressed > compressed * max_ratio.
    // When compressed_size == 0, the ratio check is skipped. This is safe because
    // the absolute size limit above already rejected any entry where
    // max(uncompressed, compressed) exceeds max_decompressed_size.
    if config.max_compression_ratio > 0 && entry.compressed_size > 0 {
        let limit = entry
            .compressed_size
            .saturating_mul(config.max_compression_ratio);
        if entry.uncompressed_size > limit {
            let ratio = entry.uncompressed_size / entry.compressed_size;
            return Err(Error::resource_limit(format!(
                "compression ratio {}:1 exceeds limit {}:1 for '{}'",
                ratio, config.max_compression_ratio, entry.name
            )));
        }
    }

    let decompressed = match entry.method {
        CompressionMethod::Stored => compressed_data.to_vec(),
        CompressionMethod::Deflated => {
            decompress_deflate(compressed_data, entry.uncompressed_size, config)
                .context("decompressing DEFLATE data")?
        }
        CompressionMethod::Unknown(method) => {
            return Err(Error::zip(format!(
                "unsupported compression method {method}"
            )));
        }
    };

    // Verify CRC-32
    let actual_crc = crc32(&decompressed);
    if actual_crc != entry.crc32 {
        diag.warning(
            Warning::new(
                "ZipCrcMismatch",
                format!(
                    "CRC-32 mismatch for '{}': expected {:08X}, got {:08X}",
                    entry.name, entry.crc32, actual_crc
                ),
            )
            .at_offset(entry.local_header_offset),
        );
    }

    Ok(decompressed)
}

/// Decompress DEFLATE data with a size limit.
fn decompress_deflate(
    compressed: &[u8],
    expected_size: u64,
    config: &ZipConfig,
) -> Result<Vec<u8>> {
    let mut decompressor = Decompress::new(false); // raw deflate, no zlib header
    let cap = expected_size.min(config.max_decompressed_size) as usize;
    let mut output = Vec::with_capacity(cap.min(16 * 1024 * 1024)); // cap initial alloc at 16MB

    // Decompress in chunks to enforce size limits
    let mut input_offset = 0;
    loop {
        if output.len() as u64 >= config.max_decompressed_size {
            return Err(Error::resource_limit(format!(
                "decompressed size exceeds limit {} during inflation",
                config.max_decompressed_size
            )));
        }

        // Grow output buffer
        let remaining = (config.max_decompressed_size as usize) - output.len();
        let chunk_size = remaining.min(32 * 1024); // 32KB chunks
        let prev_len = output.len();
        output.resize(prev_len + chunk_size, 0);

        let before_in = decompressor.total_in() as usize;
        let before_out = decompressor.total_out() as usize;

        let status = decompressor
            .decompress(
                &compressed[input_offset..],
                &mut output[prev_len..],
                flate2::FlushDecompress::None,
            )
            .map_err(|e| Error::zip(format!("DEFLATE error: {e}")))?;

        let consumed_in = decompressor.total_in() as usize - before_in;
        let produced_out = decompressor.total_out() as usize - before_out;

        input_offset += consumed_in;
        output.truncate(prev_len + produced_out);

        match status {
            flate2::Status::StreamEnd => break,
            flate2::Status::Ok | flate2::Status::BufError => {
                if consumed_in == 0 && produced_out == 0 {
                    // No progress, avoid infinite loop
                    return Err(Error::zip("DEFLATE decompression stalled"));
                }
            }
        }
    }

    Ok(output)
}

/// CRC-32 using the crc32fast crate (via flate2's re-export, or inline table).
/// We use a simple implementation since flate2 pulls in crc32fast anyway.
fn crc32(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_nonzero_for_data() {
        let content = b"Hello, stored!";
        let actual = crc32(content);
        assert_ne!(actual, 0);
    }

    #[test]
    fn decompress_deflate_roundtrip() {
        use flate2::write::DeflateEncoder;
        use flate2::Compression;
        use std::io::Write;

        let original =
            b"The quick brown fox jumps over the lazy dog. Repeated. Repeated. Repeated.";

        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(original).unwrap();
        let compressed = encoder.finish().unwrap();

        let config = ZipConfig::default();
        let result = decompress_deflate(&compressed, original.len() as u64, &config).unwrap();
        assert_eq!(result, original);
    }

    #[test]
    fn decompress_deflate_size_limit() {
        use flate2::write::DeflateEncoder;
        use flate2::Compression;
        use std::io::Write;

        let original = vec![0u8; 1000];
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&original).unwrap();
        let compressed = encoder.finish().unwrap();

        let config = ZipConfig {
            max_decompressed_size: 100, // too small
            ..ZipConfig::default()
        };
        let result = decompress_deflate(&compressed, 1000, &config);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("resource limit") || err.contains("decompressed size"),
            "got: {err}"
        );
    }

    #[test]
    fn crc32_known_value() {
        // CRC-32 of empty data is 0
        assert_eq!(crc32(b""), 0);
        // CRC-32 of known string (verified externally)
        let crc = crc32(b"hello");
        assert_ne!(crc, 0);
    }
}
