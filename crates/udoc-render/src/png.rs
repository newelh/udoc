//! Minimal PNG encoder using flate2 for DEFLATE compression.
//!
//! Produces valid PNG files with no external dependencies beyond flate2
//! (already in the workspace). Supports 8-bit RGB and grayscale output.

use flate2::write::DeflateEncoder;
use flate2::Compression;
use std::io::Write;

/// Encode an RGB pixel buffer as a PNG file.
///
/// `pixels` must be `width * height * 3` bytes (RGB, row-major, top-to-bottom).
/// Returns the complete PNG file as a byte vector.
pub fn encode_rgb_png(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let expected = width as usize * height as usize * 3;
    assert!(
        pixels.len() >= expected,
        "pixel buffer too small: {} < {}",
        pixels.len(),
        expected
    );

    let mut out = Vec::with_capacity(expected / 2); // rough estimate

    // PNG signature (8 bytes).
    out.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);

    // IHDR chunk: width, height, bit_depth=8, color_type=2 (RGB).
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(2); // color type: RGB
    ihdr.push(0); // compression method
    ihdr.push(0); // filter method
    ihdr.push(0); // interlace method
    write_chunk(&mut out, b"IHDR", &ihdr);

    // IDAT chunk(s): filtered + DEFLATE-compressed pixel data.
    // Write rows directly to the compressor to avoid a full-frame copy.
    let row_bytes = width as usize * 3;

    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::new(1));
    // zlib header: CMF=0x78 (deflate, 32K window), FLG=0x01 (check bits)
    let _ = encoder.get_mut().write_all(&[0x78, 0x01]);
    let filter_byte = [0u8]; // None filter
    let mut adler_state = Adler32State::new();
    for y in 0..height as usize {
        let _ = encoder.write_all(&filter_byte);
        adler_state.update(&filter_byte);
        let row_start = y * row_bytes;
        let row = &pixels[row_start..row_start + row_bytes];
        let _ = encoder.write_all(row);
        adler_state.update(row);
    }
    let compressed = encoder.finish().unwrap_or_default();

    let adler = adler_state.finish();
    let mut idat_data = compressed;
    idat_data.extend_from_slice(&adler.to_be_bytes());

    write_chunk(&mut out, b"IDAT", &idat_data);

    // IEND chunk (empty).
    write_chunk(&mut out, b"IEND", &[]);

    out
}

/// Write a PNG chunk: length(4) + type(4) + data + CRC(4).
fn write_chunk(out: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(chunk_type);
    out.extend_from_slice(data);
    // CRC-32 over type + data.
    let crc = crc32(&out[out.len() - data.len() - 4..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Compute CRC-32 (ISO 3309 / PNG) for a byte slice.
fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::LazyLock<[u32; 256]> = std::sync::LazyLock::new(|| {
        let mut table = [0u32; 256];
        for i in 0..256u32 {
            let mut c = i;
            for _ in 0..8 {
                if c & 1 != 0 {
                    c = 0xEDB88320 ^ (c >> 1);
                } else {
                    c >>= 1;
                }
            }
            table[i as usize] = c;
        }
        table
    });

    let mut crc = 0xFFFFFFFFu32;
    for &byte in data {
        crc = TABLE[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFFFFFF
}

/// Incremental Adler-32 state for streaming computation.
struct Adler32State {
    a: u32,
    b: u32,
}

impl Adler32State {
    fn new() -> Self {
        Self { a: 1, b: 0 }
    }

    fn update(&mut self, data: &[u8]) {
        for &byte in data {
            self.a = (self.a + byte as u32) % 65521;
            self.b = (self.b + self.a) % 65521;
        }
    }

    fn finish(&self) -> u32 {
        (self.b << 16) | self.a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_1x1_white() {
        let pixels = [255u8, 255, 255]; // 1x1 white pixel
        let png = encode_rgb_png(&pixels, 1, 1);
        // Check PNG signature.
        assert_eq!(&png[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
        // Check IHDR chunk type.
        assert_eq!(&png[12..16], b"IHDR");
        // Check width/height in IHDR.
        assert_eq!(u32::from_be_bytes([png[16], png[17], png[18], png[19]]), 1);
        assert_eq!(u32::from_be_bytes([png[20], png[21], png[22], png[23]]), 1);
    }

    #[test]
    fn encode_10x10_black() {
        let pixels = vec![0u8; 10 * 10 * 3];
        let png = encode_rgb_png(&pixels, 10, 10);
        assert_eq!(&png[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
        // Width = 10.
        assert_eq!(u32::from_be_bytes([png[16], png[17], png[18], png[19]]), 10);
    }

    #[test]
    fn black_and_white_differ() {
        let white = encode_rgb_png(&[255; 3], 1, 1);
        let black = encode_rgb_png(&[0; 3], 1, 1);
        assert_ne!(white, black);
    }

    #[test]
    fn encode_100x100() {
        let pixels = vec![128u8; 100 * 100 * 3];
        let png = encode_rgb_png(&pixels, 100, 100);
        assert_eq!(&png[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
        assert_eq!(
            u32::from_be_bytes([png[16], png[17], png[18], png[19]]),
            100
        );
        assert_eq!(
            u32::from_be_bytes([png[20], png[21], png[22], png[23]]),
            100
        );
        // IEND should be at the end.
        let len = png.len();
        assert_eq!(&png[len - 12..len - 8], &0u32.to_be_bytes()); // IEND length = 0
        assert_eq!(&png[len - 8..len - 4], b"IEND");
    }

    #[test]
    fn crc32_known_value() {
        // CRC-32 of "IEND" should be a known value.
        let crc = crc32(b"IEND");
        assert_eq!(crc, 0xAE426082);
    }

    #[test]
    fn adler32_known_value() {
        let mut state = Adler32State::new();
        state.update(b"Wikipedia");
        assert_eq!(state.finish(), 0x11E60398);
    }
}
