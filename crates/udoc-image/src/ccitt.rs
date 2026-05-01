//! CCITT fax decoder (Group 3 1D / Group 4).
//!
//! Implements ITU-T T.4 (Group 3 1D, Modified Huffman) and T.6 (Group 4, MMR)
//! run-length / 2D coding. Tables from T.4 Tables 2/3 (terminating + makeup
//! codes including extended makeup 1792-2560) and T.6 codeword set.
//!
//! Output convention: `0x00` = black pixel, `0xFF` = white pixel. The CCITT
//! internal bit convention (0 = white run, 1 = black run) is mapped to
//! grayscale at the boundary. `/BlackIs1` is honored by inverting at the end.

/// Decode CCITT fax data to raw 1-byte-per-pixel grayscale.
/// Returns `None` on decode failure. Output: `0x00` = black, `0xFF` = white.
pub(crate) fn decode_ccitt_fax(
    data: &[u8],
    width: usize,
    height: usize,
    k: i64,
    black_is_1: bool,
) -> Option<Vec<u8>> {
    if width == 0 || height == 0 {
        return None;
    }

    // Internal bit convention: 0 = white pixel, 1 = black pixel (matches T.4/T.6).
    let bits = if k < 0 {
        ccitt_decode_group4(data, width, height)?
    } else {
        ccitt_decode_group3_1d(data, width, height)?
    };

    // Map internal bits to grayscale.
    // Default (BlackIs1 = false): CCITT bit 0 = sample 1 = white, bit 1 = sample 0 = black.
    // BlackIs1 = true: CCITT bit 0 = sample 0 = black, bit 1 = sample 1 = white.
    // Either way we collapse to 0xFF for white pixels and 0x00 for black pixels.
    // The bit semantics from the codec are already (0 = white, 1 = black); BlackIs1
    // would require inverting that interpretation when consuming the decoded image
    // downstream, but udoc's filter contract is "raw pixel bytes", so we honor
    // BlackIs1 here by inverting the mapping.
    let mut pixels = Vec::with_capacity(width * height);
    if black_is_1 {
        for &bit in &bits {
            pixels.push(if bit == 0 { 0u8 } else { 255u8 });
        }
    } else {
        for &bit in &bits {
            pixels.push(if bit == 0 { 255u8 } else { 0u8 });
        }
    }
    Some(pixels)
}

/// MSB-first bit reader with a refill register.
///
/// Holds up to 32 bits in `buf`. Top `bits_in_buf` bits are valid; the topmost
/// is bit 31. Consumers `peek` (left-aligned) and `consume` exact bit counts.
struct CcittBitReader<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u32,
    bits_in_buf: u32,
}

impl<'a> CcittBitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            buf: 0,
            bits_in_buf: 0,
        }
    }

    /// Refill `buf` from `data` until at least `min_bits` are available or input is exhausted.
    fn refill(&mut self, min_bits: u32) {
        while self.bits_in_buf < min_bits && self.pos < self.data.len() {
            self.buf |= (self.data[self.pos] as u32) << (24 - self.bits_in_buf);
            self.bits_in_buf += 8;
            self.pos += 1;
        }
    }

    /// Peek up to 16 bits left-aligned (high bit of return value = next stream bit).
    fn peek16(&mut self) -> u16 {
        if self.bits_in_buf < 16 {
            self.refill(16);
        }
        (self.buf >> 16) as u16
    }

    /// Consume `n` bits (n <= 16). Caller must ensure bits are available via peek.
    fn consume(&mut self, n: u32) {
        debug_assert!(n <= 16);
        if self.bits_in_buf < n {
            self.refill(n);
        }
        if self.bits_in_buf < n {
            // Treat as exhausted.
            self.buf = 0;
            self.bits_in_buf = 0;
            return;
        }
        self.buf <<= n;
        self.bits_in_buf -= n;
    }

    /// Skip to the next byte boundary (used after EOL in Group 3 with EncodedByteAlign,
    /// not currently used by Group 4 but kept for completeness).
    #[allow(dead_code)]
    fn align_to_byte(&mut self) {
        let extra = self.bits_in_buf % 8;
        if extra > 0 {
            self.buf <<= extra;
            self.bits_in_buf -= extra;
        }
    }
}

/// Read one full white run (sum of zero-or-more makeup codes plus a terminating code).
/// Returns the total run length in pixels, or None on decode failure.
fn ccitt_white_run(r: &mut CcittBitReader) -> Option<i32> {
    let mut total: i32 = 0;
    loop {
        let bits = r.peek16();
        let (run, len) = ccitt_match_white(bits)?;
        r.consume(len as u32);
        if run < 0 {
            // EOL inside a run: treat as decode failure for this run.
            return Some(-1);
        }
        total += run;
        if run < 64 {
            return Some(total);
        }
        // Makeup code (>= 64): keep reading until we hit a terminating code.
    }
}

/// Read one full black run.
fn ccitt_black_run(r: &mut CcittBitReader) -> Option<i32> {
    let mut total: i32 = 0;
    loop {
        let bits = r.peek16();
        let (run, len) = ccitt_match_black(bits)?;
        r.consume(len as u32);
        if run < 0 {
            return Some(-1);
        }
        total += run;
        if run < 64 {
            return Some(total);
        }
    }
}

// ITU-T T.4 Table 2 white terminating + Table 3 makeup, plus T.6 extended makeup.
// `bits` is a 16-bit window MSB-first (high bit = next stream bit).
// Returns (run_length, code_length_bits). run_length < 0 means EOL.
fn ccitt_match_white(bits: u16) -> Option<(i32, u8)> {
    if let Some((run, len)) = lookup_white(bits) {
        return Some((run, len));
    }
    // EOL: 11 zeros + 1 -> 12-bit code 0x001 (000000000001).
    if bits >> 4 == 0b000000000001 {
        return Some((-1, 12));
    }
    None
}

fn ccitt_match_black(bits: u16) -> Option<(i32, u8)> {
    if let Some((run, len)) = lookup_black(bits) {
        return Some((run, len));
    }
    if bits >> 4 == 0b000000000001 {
        return Some((-1, 12));
    }
    None
}

/// Complete T.4 white code table (Table 2 terminating + Table 3 makeup) merged with
/// T.6 extended makeup (Table 14, common to both colors).
fn lookup_white(bits: u16) -> Option<(i32, u8)> {
    // Each entry is (code << (16 - bit_length), bit_length, run_length).
    // Tables ordered roughly shortest -> longest within each length bucket.
    // Using a static table with mask comparison.
    for &(code, len, run) in WHITE_CODES {
        let shift = 16 - len;
        if (bits >> shift) == code {
            return Some((run, len as u8));
        }
    }
    // Extended makeup (1792-2560) - shared with black.
    for &(code, len, run) in EXTENDED_MAKEUP {
        let shift = 16 - len;
        if (bits >> shift) == code {
            return Some((run, len as u8));
        }
    }
    None
}

fn lookup_black(bits: u16) -> Option<(i32, u8)> {
    for &(code, len, run) in BLACK_CODES {
        let shift = 16 - len;
        if (bits >> shift) == code {
            return Some((run, len as u8));
        }
    }
    for &(code, len, run) in EXTENDED_MAKEUP {
        let shift = 16 - len;
        if (bits >> shift) == code {
            return Some((run, len as u8));
        }
    }
    None
}

// ITU-T T.4 Table 2: White terminating codes (run lengths 0-63).
// ITU-T T.4 Table 3a: White makeup codes (64-1728).
// (code, code_length, run_length)
#[rustfmt::skip]
static WHITE_CODES: &[(u16, u16, i32)] = &[
    // Terminating (0-63)
    (0b00110101, 8, 0),
    (0b000111,   6, 1),
    (0b0111,     4, 2),
    (0b1000,     4, 3),
    (0b1011,     4, 4),
    (0b1100,     4, 5),
    (0b1110,     4, 6),
    (0b1111,     4, 7),
    (0b10011,    5, 8),
    (0b10100,    5, 9),
    (0b00111,    5, 10),
    (0b01000,    5, 11),
    (0b001000,   6, 12),
    (0b000011,   6, 13),
    (0b110100,   6, 14),
    (0b110101,   6, 15),
    (0b101010,   6, 16),
    (0b101011,   6, 17),
    (0b0100111,  7, 18),
    (0b0001100,  7, 19),
    (0b0001000,  7, 20),
    (0b0010111,  7, 21),
    (0b0000011,  7, 22),
    (0b0000100,  7, 23),
    (0b0101000,  7, 24),
    (0b0101011,  7, 25),
    (0b0010011,  7, 26),
    (0b0100100,  7, 27),
    (0b0011000,  7, 28),
    (0b00000010, 8, 29),
    (0b00000011, 8, 30),
    (0b00011010, 8, 31),
    (0b00011011, 8, 32),
    (0b00010010, 8, 33),
    (0b00010011, 8, 34),
    (0b00010100, 8, 35),
    (0b00010101, 8, 36),
    (0b00010110, 8, 37),
    (0b00010111, 8, 38),
    (0b00101000, 8, 39),
    (0b00101001, 8, 40),
    (0b00101010, 8, 41),
    (0b00101011, 8, 42),
    (0b00101100, 8, 43),
    (0b00101101, 8, 44),
    (0b00000100, 8, 45),
    (0b00000101, 8, 46),
    (0b00001010, 8, 47),
    (0b00001011, 8, 48),
    (0b01010010, 8, 49),
    (0b01010011, 8, 50),
    (0b01010100, 8, 51),
    (0b01010101, 8, 52),
    (0b00100100, 8, 53),
    (0b00100101, 8, 54),
    (0b01011000, 8, 55),
    (0b01011001, 8, 56),
    (0b01011010, 8, 57),
    (0b01011011, 8, 58),
    (0b01001010, 8, 59),
    (0b01001011, 8, 60),
    (0b00110010, 8, 61),
    (0b00110011, 8, 62),
    (0b00110100, 8, 63),
    // Makeup (64-1728): T.4 Table 3a
    (0b11011,        5, 64),
    (0b10010,        5, 128),
    (0b010111,       6, 192),
    (0b0110111,      7, 256),
    (0b00110110,     8, 320),
    (0b00110111,     8, 384),
    (0b01100100,     8, 448),
    (0b01100101,     8, 512),
    (0b01101000,     8, 576),
    (0b01100111,     8, 640),
    (0b011001100,    9, 704),
    (0b011001101,    9, 768),
    (0b011010010,    9, 832),
    (0b011010011,    9, 896),
    (0b011010100,    9, 960),
    (0b011010101,    9, 1024),
    (0b011010110,    9, 1088),
    (0b011010111,    9, 1152),
    (0b011011000,    9, 1216),
    (0b011011001,    9, 1280),
    (0b011011010,    9, 1344),
    (0b011011011,    9, 1408),
    (0b010011000,    9, 1472),
    (0b010011001,    9, 1536),
    (0b010011010,    9, 1600),
    (0b011000,       6, 1664),
    (0b010011011,    9, 1728),
];

// ITU-T T.4 Table 2: Black terminating codes (0-63).
// ITU-T T.4 Table 3b: Black makeup codes (64-1728).
#[rustfmt::skip]
static BLACK_CODES: &[(u16, u16, i32)] = &[
    // Terminating (0-63)
    (0b0000110111,    10, 0),
    (0b010,           3,  1),
    (0b11,            2,  2),
    (0b10,            2,  3),
    (0b011,           3,  4),
    (0b0011,          4,  5),
    (0b0010,          4,  6),
    (0b00011,         5,  7),
    (0b000101,        6,  8),
    (0b000100,        6,  9),
    (0b0000100,       7,  10),
    (0b0000101,       7,  11),
    (0b0000111,       7,  12),
    (0b00000100,      8,  13),
    (0b00000111,      8,  14),
    (0b000011000,     9,  15),
    (0b0000010111,    10, 16),
    (0b0000011000,    10, 17),
    (0b0000001000,    10, 18),
    (0b00001100111,   11, 19),
    (0b00001101000,   11, 20),
    (0b00001101100,   11, 21),
    (0b00000110111,   11, 22),
    (0b00000101000,   11, 23),
    (0b00000010111,   11, 24),
    (0b00000011000,   11, 25),
    (0b000011001010,  12, 26),
    (0b000011001011,  12, 27),
    (0b000011001100,  12, 28),
    (0b000011001101,  12, 29),
    (0b000001101000,  12, 30),
    (0b000001101001,  12, 31),
    (0b000001101010,  12, 32),
    (0b000001101011,  12, 33),
    (0b000011010010,  12, 34),
    (0b000011010011,  12, 35),
    (0b000011010100,  12, 36),
    (0b000011010101,  12, 37),
    (0b000011010110,  12, 38),
    (0b000011010111,  12, 39),
    (0b000001101100,  12, 40),
    (0b000001101101,  12, 41),
    (0b000011011010,  12, 42),
    (0b000011011011,  12, 43),
    (0b000001010100,  12, 44),
    (0b000001010101,  12, 45),
    (0b000001010110,  12, 46),
    (0b000001010111,  12, 47),
    (0b000001100100,  12, 48),
    (0b000001100101,  12, 49),
    (0b000001010010,  12, 50),
    (0b000001010011,  12, 51),
    (0b000000100100,  12, 52),
    (0b000000110111,  12, 53),
    (0b000000111000,  12, 54),
    (0b000000100111,  12, 55),
    (0b000000101000,  12, 56),
    (0b000001011000,  12, 57),
    (0b000001011001,  12, 58),
    (0b000000101011,  12, 59),
    (0b000000101100,  12, 60),
    (0b000001011010,  12, 61),
    (0b000001100110,  12, 62),
    (0b000001100111,  12, 63),
    // Makeup (64-1728): T.4 Table 3b
    (0b0000001111,    10, 64),
    (0b000011001000,  12, 128),
    (0b000011001001,  12, 192),
    (0b000001011011,  12, 256),
    (0b000000110011,  12, 320),
    (0b000000110100,  12, 384),
    (0b000000110101,  12, 448),
    (0b0000001101100, 13, 512),
    (0b0000001101101, 13, 576),
    (0b0000001001010, 13, 640),
    (0b0000001001011, 13, 704),
    (0b0000001001100, 13, 768),
    (0b0000001001101, 13, 832),
    (0b0000001110010, 13, 896),
    (0b0000001110011, 13, 960),
    (0b0000001110100, 13, 1024),
    (0b0000001110101, 13, 1088),
    (0b0000001110110, 13, 1152),
    (0b0000001110111, 13, 1216),
    (0b0000001010010, 13, 1280),
    (0b0000001010011, 13, 1344),
    (0b0000001010100, 13, 1408),
    (0b0000001010101, 13, 1472),
    (0b0000001011010, 13, 1536),
    (0b0000001011011, 13, 1600),
    (0b0000001100100, 13, 1664),
    (0b0000001100101, 13, 1728),
];

// ITU-T T.6 Table 14: Extended makeup codes (1792-2560), shared by white and black.
#[rustfmt::skip]
static EXTENDED_MAKEUP: &[(u16, u16, i32)] = &[
    (0b00000001000,    11, 1792),
    (0b00000001100,    11, 1856),
    (0b00000001101,    11, 1920),
    (0b000000010010,   12, 1984),
    (0b000000010011,   12, 2048),
    (0b000000010100,   12, 2112),
    (0b000000010101,   12, 2176),
    (0b000000010110,   12, 2240),
    (0b000000010111,   12, 2304),
    (0b000000011100,   12, 2368),
    (0b000000011101,   12, 2432),
    (0b000000011110,   12, 2496),
    (0b000000011111,   12, 2560),
];

/// Group 3 1D decoder (Modified Huffman, no 2D).
fn ccitt_decode_group3_1d(data: &[u8], w: usize, max_h: usize) -> Option<Vec<u8>> {
    let mut r = CcittBitReader::new(data);
    let mut rows: Vec<u8> = Vec::with_capacity(w * max_h);
    for _ in 0..max_h {
        // Optional EOL prefix (T.4): 11 zeros + 1. If present, consume it.
        let bits = r.peek16();
        if bits >> 4 == 0b000000000001 {
            r.consume(12);
        }
        let mut row = vec![0u8; w];
        let mut x = 0usize;
        let mut is_white = true;
        while x < w {
            let run = if is_white {
                ccitt_white_run(&mut r)?
            } else {
                ccitt_black_run(&mut r)?
            };
            if run < 0 {
                break;
            }
            let count = (run as usize).min(w - x);
            if !is_white {
                for px in &mut row[x..x + count] {
                    *px = 1;
                }
            }
            x += count;
            is_white = !is_white;
        }
        rows.extend_from_slice(&row);
    }
    if rows.is_empty() {
        None
    } else {
        Some(rows)
    }
}

/// ITU-T T.6 Group 4 (MMR) 2D decoder.
///
/// State: a0 (changing element on coding line; -1 represents the imaginary
/// element to the left of column 0). is_white tracks the color of a0.
/// b1, b2 are reference-line changing elements per T.6.
fn ccitt_decode_group4(data: &[u8], w: usize, max_h: usize) -> Option<Vec<u8>> {
    let mut r = CcittBitReader::new(data);
    // Reference line starts as imaginary all-white line.
    let mut ref_line = vec![0u8; w];
    let mut rows: Vec<u8> = Vec::with_capacity(w * max_h);

    for _ in 0..max_h {
        let mut cur = vec![0u8; w];
        // a0 is signed: -1 = imaginary left-of-column-0, otherwise the column index
        // of the most recent changing element on the coding line.
        let mut a0: i32 = -1;
        // is_white = color of pixels to the right of a0 on the coding line up to
        // the next changing element. Per T.6, the imaginary element a0 starts white.
        let mut is_white = true;

        loop {
            // Stop when a0 has reached or passed the right edge.
            if a0 >= w as i32 {
                break;
            }

            // Find b1: first changing element on reference line strictly to the
            // right of a0 whose color is opposite to that of a0. Color of a0
            // equals !is_white pre-toggle (a0 sits at a transition). We use the
            // convention that "color of a0" = the color CHANGING TO at a0, i.e.
            // !is_white; equivalently we look for b1 of color is_white's opposite.
            //
            // Simpler restatement (from T.6 spec): we want b1 where the
            // reference line transitions from "same color as the run right of a0"
            // back to itself's opposite. Standard formulation: a0's color is the
            // color we just left = !current_run_color. b1 must have opposite
            // color to a0's color = same as current_run_color = is_white's color.
            //
            // We use convention: target of b1 = "color of pixels to the right of
            // a0 on the coding line" = our `is_white` flag (true => target white,
            // false => target black).
            let a0_search = if a0 < 0 { 0 } else { a0 as usize };
            // b1 must be a changing element of OPPOSITE color to a0.
            // a0's color = current `is_white` (color of run starting at a0 on coding line).
            // Therefore target_color = !is_white's color.
            let target_color: u8 = if is_white { 1 } else { 0 };
            let b1 = find_b1(&ref_line, a0_search, a0, target_color, w);
            let b2 = find_next_change(&ref_line, b1, w);

            match read_g4_mode(&mut r)? {
                G4Mode::Pass => {
                    // No color change; fill from a0 to b2 with current color.
                    fill(&mut cur, a0, b2 as i32, is_white);
                    a0 = b2 as i32;
                }
                G4Mode::Horizontal => {
                    // Two run lengths: first matches current color, second matches opposite.
                    let r1 = if is_white {
                        ccitt_white_run(&mut r)?
                    } else {
                        ccitt_black_run(&mut r)?
                    };
                    let r2 = if is_white {
                        ccitt_black_run(&mut r)?
                    } else {
                        ccitt_white_run(&mut r)?
                    };
                    if r1 < 0 || r2 < 0 {
                        break;
                    }
                    let start = if a0 < 0 { 0 } else { a0 as usize };
                    let end1 = (start + r1 as usize).min(w);
                    let end2 = (end1 + r2 as usize).min(w);
                    // Fill r1 in current color
                    if !is_white {
                        for px in &mut cur[start..end1] {
                            *px = 1;
                        }
                    }
                    // Fill r2 in opposite color
                    if is_white {
                        for px in &mut cur[end1..end2] {
                            *px = 1;
                        }
                    }
                    a0 = end2 as i32;
                    // Two color flips => is_white unchanged
                }
                G4Mode::Vertical(delta) => {
                    let a1 = ((b1 as i32) + delta).clamp(0, w as i32);
                    fill(&mut cur, a0, a1, is_white);
                    a0 = a1;
                    is_white = !is_white;
                }
                G4Mode::Eofb => {
                    // End-of-fax-block: stop decoding entirely. Pad the current row
                    // with current color and return what we have.
                    let start = if a0 < 0 { 0 } else { a0 as usize };
                    if !is_white {
                        for px in &mut cur[start..] {
                            *px = 1;
                        }
                    }
                    rows.extend_from_slice(&cur);
                    if rows.is_empty() {
                        return None;
                    }
                    return Some(rows);
                }
            }
        }
        rows.extend_from_slice(&cur);
        ref_line = cur;
    }
    if rows.is_empty() {
        None
    } else {
        Some(rows)
    }
}

/// Fill `cur[a0..a1]` with current color (is_white). Handles a0 = -1.
#[inline]
fn fill(cur: &mut [u8], a0: i32, a1: i32, is_white: bool) {
    if a0 >= a1 {
        return;
    }
    let start = if a0 < 0 { 0 } else { a0 as usize };
    let end = (a1 as usize).min(cur.len());
    if start >= end {
        return;
    }
    if !is_white {
        for px in &mut cur[start..end] {
            *px = 1;
        }
    }
    // is_white = true: cur is already 0; nothing to do.
}

/// Find b1 on the reference line.
///
/// b1 is the first changing element on the reference line strictly to the right
/// of a0 whose color matches `target_color`. A "changing element" is where the
/// color differs from the previous pixel on the same line (or the imaginary
/// white pixel to the left of column 0).
#[inline]
fn find_b1(ref_line: &[u8], search_start: usize, a0: i32, target_color: u8, w: usize) -> usize {
    let mut i = search_start;
    // Skip pixels of the same color as a0 (i.e., color != target_color), starting
    // at the position right after a0. b1 is the first transition to target_color.
    // First, advance i so that ref_line[i] is at-or-past a0. Then walk.
    while i < w {
        let prev = if i == 0 { 0u8 } else { ref_line[i - 1] };
        let cur = ref_line[i];
        if cur != prev && cur == target_color && (i as i32) > a0 {
            return i;
        }
        i += 1;
    }
    w
}

/// Find the next changing element on the reference line starting after `pos`.
#[inline]
fn find_next_change(ref_line: &[u8], pos: usize, w: usize) -> usize {
    if pos >= w {
        return w;
    }
    let color = ref_line[pos];
    let mut i = pos + 1;
    while i < w {
        if ref_line[i] != color {
            return i;
        }
        i += 1;
    }
    w
}

#[derive(Clone, Copy, Debug)]
enum G4Mode {
    Pass,
    Horizontal,
    Vertical(i32),
    /// End-of-Fax-Block: two consecutive EOL codes (24 zero bits + two `1` bits).
    Eofb,
}

/// Read a G4 mode codeword. Codes per T.6 Table 5:
///   1            -> V(0)
///   011          -> VR(1)
///   010          -> VL(1)
///   000011       -> VR(2)
///   000010       -> VL(2)
///   0000011      -> VR(3)
///   0000010      -> VL(3)
///   001          -> H (Horizontal)
///   0001         -> P (Pass)
///   000000000001 -> EOL (used in pairs for EOFB)
fn read_g4_mode(r: &mut CcittBitReader) -> Option<G4Mode> {
    let bits = r.peek16();

    // V(0): 1 bit code
    if bits >> 15 == 0b1 {
        r.consume(1);
        return Some(G4Mode::Vertical(0));
    }
    // 3-bit codes: VR(1) = 011, VL(1) = 010
    let top3 = bits >> 13;
    if top3 == 0b011 {
        r.consume(3);
        return Some(G4Mode::Vertical(1));
    }
    if top3 == 0b010 {
        r.consume(3);
        return Some(G4Mode::Vertical(-1));
    }
    // H (3-bit): 001
    if top3 == 0b001 {
        r.consume(3);
        return Some(G4Mode::Horizontal);
    }
    // P (4-bit): 0001
    let top4 = bits >> 12;
    if top4 == 0b0001 {
        r.consume(4);
        return Some(G4Mode::Pass);
    }
    // 6-bit codes: VR(2) = 000011, VL(2) = 000010
    let top6 = bits >> 10;
    if top6 == 0b000011 {
        r.consume(6);
        return Some(G4Mode::Vertical(2));
    }
    if top6 == 0b000010 {
        r.consume(6);
        return Some(G4Mode::Vertical(-2));
    }
    // 7-bit codes: VR(3) = 0000011, VL(3) = 0000010
    let top7 = bits >> 9;
    if top7 == 0b0000011 {
        r.consume(7);
        return Some(G4Mode::Vertical(3));
    }
    if top7 == 0b0000010 {
        r.consume(7);
        return Some(G4Mode::Vertical(-3));
    }
    // EOL prefix: 12-bit 0x001 (000000000001). For EOFB we need a SECOND EOL
    // immediately following. Probe for that.
    if bits >> 4 == 0b000000000001 {
        r.consume(12);
        // Look for second EOL
        let next = r.peek16();
        if next >> 4 == 0b000000000001 {
            r.consume(12);
        }
        return Some(G4Mode::Eofb);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ccitt_group4_all_white() {
        // Group 4, 8-pixel wide, 2 rows, all white.
        // Each row: V(0) = bit 1, fills entire row white from all-white reference.
        // Two V(0) codes = bits 11, padded to byte 0xC0.
        let data = [0xC0];
        let result = decode_ccitt_fax(&data, 8, 2, -1, false);
        assert!(result.is_some(), "decode_ccitt_fax returned None");
        let pixels = result.unwrap();
        assert_eq!(pixels.len(), 16);
        for (i, &px) in pixels.iter().enumerate() {
            assert_eq!(px, 255, "pixel {i} should be white (255), got {px}");
        }
    }

    #[test]
    fn test_ccitt_group4_all_white_black_is_1() {
        // Same all-white encoding, but BlackIs1 = true => output should be all BLACK
        // (since CCITT bit 0 maps to sample 0 = black under BlackIs1).
        let data = [0xC0];
        let result = decode_ccitt_fax(&data, 8, 2, -1, true);
        assert!(result.is_some());
        let pixels = result.unwrap();
        assert_eq!(pixels.len(), 16);
        for &px in &pixels {
            assert_eq!(px, 0);
        }
    }

    #[test]
    fn test_ccitt_group4_horizontal_mode_first_row() {
        // 8 px wide, 1 row: 4 black + 4 white via H mode from all-white reference.
        // Encoding: H(001) + white_run(0)=8 bits 00110101 + black_run(4)=3 bits 011 + white_run(4)=4 bits 1011
        // = 001 00110101 011 1011 = 19 bits, padded to 24: 0010 0110 1010 1101 1101 1000 = 0x26 0xAD 0xD8
        let data = [0x26, 0xAD, 0xD8];
        let result = decode_ccitt_fax(&data, 8, 1, -1, false);
        assert!(result.is_some(), "decode failed");
        let pixels = result.unwrap();
        assert_eq!(pixels.len(), 8);
        // First 4 = black (0), next 4 = white (255)
        assert_eq!(&pixels[..], &[0, 0, 0, 0, 255, 255, 255, 255]);
    }

    #[test]
    fn test_ccitt_group4_vertical_mode_repeat_row() {
        // Row 1 H mode: 4 black + 4 white (18 bits): 001 00110101 011 1011
        // Row 2 reproduces row 1 verbatim using three V(0) codes:
        //   - V(0) at b1=0 (transition to BLACK on ref line): a0:-1->0, is_white:true->false (no pixels)
        //   - V(0) at b1=4 (transition to WHITE on ref line): fill cur[0..4]=BLACK, a0=4, is_white=true
        //   - V(0) at b1=8 (end-of-line, no further transition): fill cur[4..8]=WHITE, a0=8
        // Row 2 bits: 1 1 1 = 3 bits.
        // Combined: 001 00110101 011 1011 111 = 21 bits, padded to 24 with 3 zeros.
        // Bytes: 0010 0110 | 1010 1101 | 1111 1000 = 0x26 0xAD 0xF8
        let data = [0x26, 0xAD, 0xF8];
        let result = decode_ccitt_fax(&data, 8, 2, -1, false).expect("decode");
        assert_eq!(result.len(), 16);
        let expected = [0u8, 0, 0, 0, 255, 255, 255, 255];
        assert_eq!(&result[0..8], &expected[..]);
        assert_eq!(&result[8..16], &expected[..]);
    }

    #[test]
    fn test_ccitt_group4_wide_white_row_uses_extended_makeup() {
        // 2560 px wide, 1 row, all white. From all-white reference, V(0) suffices.
        // Single V(0) = bit 1, padded to 0x80.
        // This exercises the row layout but not extended makeup; nevertheless verify.
        let data = [0x80];
        let result = decode_ccitt_fax(&data, 2560, 1, -1, false).expect("decode");
        assert_eq!(result.len(), 2560);
        for &px in &result {
            assert_eq!(px, 255);
        }
    }

    #[test]
    fn test_ccitt_group4_extended_makeup_white_run() {
        // 2560 px wide, 1 row. We force a Horizontal mode that issues a 2304-pixel
        // white terminator, exercising EXTENDED_MAKEUP (T.6 Table 14, code for 2304
        // white = 12-bit 000000010111). Encoding:
        //   H(001) + white_run(2304)=12 bits 000000010111 + white_run(0)=8 bits 00110101
        //   followed by another H to fill remaining 256 pixels with black+white.
        // Simpler test: verify the lookup table directly returns 2304 for that prefix.
        // 0b000000010111 left-aligned in 16-bit: 0b000000010111_0000 = 0x0170
        let bits: u16 = 0b000000010111 << 4;
        let m = lookup_white(bits);
        assert_eq!(m, Some((2304, 12)));
        // And black side (extended makeup is shared):
        let m_b = lookup_black(bits);
        assert_eq!(m_b, Some((2304, 12)));
    }

    #[test]
    fn test_ccitt_white_makeup_64_then_terminator() {
        // White makeup 64 = 5 bits 11011, then terminator white(8) = 5 bits 10011.
        // Combined: 11011 10011 = 10 bits. ccitt_white_run should sum to 72.
        // Padded to 16 bits MSB-first: 1101110011_000000 = 0xDCC0
        let data = [0xDC, 0xC0];
        let mut r = CcittBitReader::new(&data);
        let run = ccitt_white_run(&mut r).expect("decode");
        assert_eq!(run, 72);
    }

    #[test]
    fn test_ccitt_white_extended_makeup_2560_then_terminator() {
        // White run of 2563 pixels: extended makeup 2560 (12 bits 000000011111)
        // + white terminator 3 (4 bits 1000).
        // Combined bits: 000000011111 1000 = 16 bits = 0x01F8
        let data = [0x01, 0xF8];
        let mut r = CcittBitReader::new(&data);
        let run = ccitt_white_run(&mut r).expect("decode");
        assert_eq!(run, 2563);
    }

    #[test]
    fn test_ccitt_black_makeup_chain() {
        // Black run of 1500: 1408 (13 bits 0000001010100) + 64 (10 bits 0000001111)
        //                    + terminator black(28) = 12 bits 000011001100
        // Bit stream (35 bits): 0000001010100 0000001111 000011001100
        // Padded to 40 bits with 5 zeros:
        //   00000010 10100000 00011110 00011001 10000000
        //   = 0x02 0xA0 0x1E 0x19 0x80
        let data = [0x02, 0xA0, 0x1E, 0x19, 0x80];
        let mut r = CcittBitReader::new(&data);
        let run = ccitt_black_run(&mut r).expect("decode");
        assert_eq!(run, 1500);
    }
}
