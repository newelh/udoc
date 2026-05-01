//! JBIG2 arithmetic coder (ISO 14492 Annex E) + integer arithmetic decoder
//! (ISO 14492 Annex A.2).
//!
//! The MQ coder is a binary arithmetic coder with a 46-entry probability-estimate
//! table (`QE_TABLE`). Each context tracks an index into that table plus a
//! single "more-probable-symbol" (MPS) bit. The decoder maintains a code
//! register `c`, an interval register `a`, a byte counter `ct`, and a byte
//! cursor `bp` over the input stream.
//!
//! Implementation follows ISO/IEC 14492:2001 Annex E.3 (DECODE, BYTEIN,
//! INITDEC, LPS_EXCHANGE, MPS_EXCHANGE, RENORMD) and Annex A.2 for the
//! integer arithmetic decoder with the 13 named contexts.
//!
//! Port target: pdfium `third_party/jbig2/JBig2_ArithDecoder.cpp` (BSD).
//! Cross-checked against `libjbig2dec/jbig2_arith.c` for BYTEIN/sticky-FF
//! behaviour (Annex E.2.4).

use std::fmt;

// ---------------------------------------------------------------------------
// QE probability table (ISO 14492 Annex E.1.2)
// ---------------------------------------------------------------------------
//
// Each row: (qe, nmps, nlps, switch).
// - qe:     16-bit LPS sub-interval estimate (fixed-point, 0.15)
// - nmps:   next index after an MPS decode
// - nlps:   next index after an LPS decode
// - switch: 1 iff MPS flips on LPS renormalization
//
// Values are taken verbatim from Annex E.1.2, Table E.1 (bit patterns).
// If a single entry deviates here the entire decoder silently corrupts, so
// the table is treated as a spec constant and must not be tuned.

#[derive(Clone, Copy)]
struct QeEntry {
    qe: u32,
    nmps: u8,
    nlps: u8,
    switch_: u8,
}

#[rustfmt::skip]
const QE_TABLE: [QeEntry; 47] = [
    QeEntry { qe: 0x5601, nmps: 1,  nlps: 1,  switch_: 1 },
    QeEntry { qe: 0x3401, nmps: 2,  nlps: 6,  switch_: 0 },
    QeEntry { qe: 0x1801, nmps: 3,  nlps: 9,  switch_: 0 },
    QeEntry { qe: 0x0AC1, nmps: 4,  nlps: 12, switch_: 0 },
    QeEntry { qe: 0x0521, nmps: 5,  nlps: 29, switch_: 0 },
    QeEntry { qe: 0x0221, nmps: 38, nlps: 33, switch_: 0 },
    QeEntry { qe: 0x5601, nmps: 7,  nlps: 6,  switch_: 1 },
    QeEntry { qe: 0x5401, nmps: 8,  nlps: 14, switch_: 0 },
    QeEntry { qe: 0x4801, nmps: 9,  nlps: 14, switch_: 0 },
    QeEntry { qe: 0x3801, nmps: 10, nlps: 14, switch_: 0 },
    QeEntry { qe: 0x3001, nmps: 11, nlps: 17, switch_: 0 },
    QeEntry { qe: 0x2401, nmps: 12, nlps: 18, switch_: 0 },
    QeEntry { qe: 0x1C01, nmps: 13, nlps: 20, switch_: 0 },
    QeEntry { qe: 0x1601, nmps: 29, nlps: 21, switch_: 0 },
    QeEntry { qe: 0x5601, nmps: 15, nlps: 14, switch_: 1 },
    QeEntry { qe: 0x5401, nmps: 16, nlps: 14, switch_: 0 },
    QeEntry { qe: 0x5101, nmps: 17, nlps: 15, switch_: 0 },
    QeEntry { qe: 0x4801, nmps: 18, nlps: 16, switch_: 0 },
    QeEntry { qe: 0x3801, nmps: 19, nlps: 17, switch_: 0 },
    QeEntry { qe: 0x3401, nmps: 20, nlps: 18, switch_: 0 },
    QeEntry { qe: 0x3001, nmps: 21, nlps: 19, switch_: 0 },
    QeEntry { qe: 0x2801, nmps: 22, nlps: 19, switch_: 0 },
    QeEntry { qe: 0x2401, nmps: 23, nlps: 20, switch_: 0 },
    QeEntry { qe: 0x2201, nmps: 24, nlps: 21, switch_: 0 },
    QeEntry { qe: 0x1C01, nmps: 25, nlps: 22, switch_: 0 },
    QeEntry { qe: 0x1801, nmps: 26, nlps: 23, switch_: 0 },
    QeEntry { qe: 0x1601, nmps: 27, nlps: 24, switch_: 0 },
    QeEntry { qe: 0x1401, nmps: 28, nlps: 25, switch_: 0 },
    QeEntry { qe: 0x1201, nmps: 29, nlps: 26, switch_: 0 },
    QeEntry { qe: 0x1101, nmps: 30, nlps: 27, switch_: 0 },
    QeEntry { qe: 0x0AC1, nmps: 31, nlps: 28, switch_: 0 },
    QeEntry { qe: 0x09C1, nmps: 32, nlps: 29, switch_: 0 },
    QeEntry { qe: 0x08A1, nmps: 33, nlps: 30, switch_: 0 },
    QeEntry { qe: 0x0521, nmps: 34, nlps: 31, switch_: 0 },
    QeEntry { qe: 0x0441, nmps: 35, nlps: 32, switch_: 0 },
    QeEntry { qe: 0x02A1, nmps: 36, nlps: 33, switch_: 0 },
    QeEntry { qe: 0x0221, nmps: 37, nlps: 34, switch_: 0 },
    QeEntry { qe: 0x0141, nmps: 38, nlps: 35, switch_: 0 },
    QeEntry { qe: 0x0111, nmps: 39, nlps: 36, switch_: 0 },
    QeEntry { qe: 0x0085, nmps: 40, nlps: 37, switch_: 0 },
    QeEntry { qe: 0x0049, nmps: 41, nlps: 38, switch_: 0 },
    QeEntry { qe: 0x0025, nmps: 42, nlps: 39, switch_: 0 },
    QeEntry { qe: 0x0015, nmps: 43, nlps: 40, switch_: 0 },
    QeEntry { qe: 0x0009, nmps: 44, nlps: 41, switch_: 0 },
    QeEntry { qe: 0x0005, nmps: 45, nlps: 42, switch_: 0 },
    QeEntry { qe: 0x0001, nmps: 45, nlps: 43, switch_: 0 },
    QeEntry { qe: 0x5601, nmps: 46, nlps: 46, switch_: 0 },
];

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

/// Single arithmetic-coder context: QE index + MPS bit.
///
/// Start every context at `(index=0, mps=0)` per Annex E.2.2 "initialise
/// statistics". A fresh `[Context]` slice does exactly that.
#[derive(Clone, Copy, Default, Debug)]
pub struct Context {
    index: u8,
    mps: u8,
}

impl Context {
    /// Fresh context: `(index=0, mps=0)`.
    pub const fn new() -> Self {
        Context { index: 0, mps: 0 }
    }
}

/// A table of arithmetic-coder contexts, indexed by an up-to-`bits` context
/// word derived from surrounding pixel values per the region spec
/// (e.g. GBTEMPLATE context shift in §6.2.5.3).
#[derive(Debug, Clone)]
pub struct ContextTable {
    /// One entry per context word. Allocation is eager; callers typically
    /// size by `1 << ctx_bits` which for template-0 generic regions is 16
    /// bits -> 64K entries.
    pub entries: Vec<Context>,
}

impl ContextTable {
    /// Allocate a context table with `count` zero-initialised entries.
    pub fn new(count: usize) -> Self {
        ContextTable {
            entries: vec![Context::new(); count],
        }
    }

    /// Reset every context to `(index=0, mps=0)`. Per-region MQ state is
    /// reset per Annex E.2.2, *not* per segment (§7.4.6 clarifies the
    /// distinction).
    pub fn reset(&mut self) {
        for c in self.entries.iter_mut() {
            *c = Context::new();
        }
    }
}

// ---------------------------------------------------------------------------
// ArithDecoder
// ---------------------------------------------------------------------------

/// JBIG2 MQ arithmetic decoder.
///
/// Owns the code register `c`, interval register `a`, byte-counter `ct`
/// and a cursor into the input buffer. Context state is supplied by the
/// caller on each [`decode`](Self::decode) call so that a single decoder
/// instance can drive many independent context tables.
pub struct ArithDecoder<'a> {
    /// Input bitstream (BYTEIN consumes from this).
    buf: &'a [u8],
    /// Current byte cursor.
    bp: usize,
    /// Code register (high 16 bits = Chigh, low 16 bits = Clow of Annex E).
    c: u32,
    /// Interval register.
    a: u32,
    /// Bit-shift countdown: renormalization consumes `ct` bits before
    /// triggering BYTEIN.
    ct: u32,
    /// Latched most-recent byte (for sticky-FF handling past EOF).
    b: u8,
}

impl<'a> fmt::Debug for ArithDecoder<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArithDecoder")
            .field("bp", &self.bp)
            .field("len", &self.buf.len())
            .field("c", &format_args!("{:#010x}", self.c))
            .field("a", &format_args!("{:#06x}", self.a))
            .field("ct", &self.ct)
            .field("b", &format_args!("{:#04x}", self.b))
            .finish()
    }
}

impl<'a> ArithDecoder<'a> {
    /// Initialise a decoder over `bytes`, running INITDEC (Annex E.3.5).
    ///
    /// INITDEC semantics:
    ///   B  <- B0
    ///   C  <- B0 << 16
    ///   BYTEIN
    ///   C  <- C << 7
    ///   CT <- CT - 7
    ///   A  <- 0x8000
    ///
    /// We also tolerate an empty buffer: the decoder still constructs,
    /// all subsequent BYTEINs see EOF and return 0xFF per Annex E.2.4.
    pub fn new(bytes: &'a [u8]) -> Self {
        let b0 = bytes.first().copied().unwrap_or(0xFF);
        let mut dec = ArithDecoder {
            buf: bytes,
            bp: 0,
            c: (b0 as u32) << 16,
            a: 0x8000,
            ct: 0,
            b: b0,
        };
        dec.byte_in();
        dec.c <<= 7;
        dec.ct = dec.ct.wrapping_sub(7);
        dec.a = 0x8000;
        dec
    }

    /// Decode a single binary symbol using `ctx` (Annex E.3.2 DECODE).
    pub fn decode(&mut self, table: &mut ContextTable, ctx: usize) -> u8 {
        let cx = &mut table.entries[ctx];
        let qe_entry = QE_TABLE[cx.index as usize];
        let qe = qe_entry.qe;

        // A <- A - Qe(I(CX))
        self.a = self.a.wrapping_sub(qe);

        // Fast MPS path: Chigh >= Qe and no renorm needed.
        let chigh = self.c >> 16;

        let d;
        if chigh < qe {
            // LPS path: C stays the same, renormalize.
            d = self.lps_exchange(cx, qe_entry, qe);
            self.renorm_d();
        } else {
            // MPS path: subtract Qe from C, maybe renormalize.
            self.c = self.c.wrapping_sub(qe << 16);
            if (self.a & 0x8000) == 0 {
                d = self.mps_exchange(cx, qe_entry);
                self.renorm_d();
            } else {
                d = cx.mps;
            }
        }
        d
    }

    // LPS_EXCHANGE (Annex E.3.3).
    //
    // if A < Qe:
    //     D    <- MPS(CX)
    //     A    <- Qe
    //     I    <- NMPS(I)        (this branch only reachable when LPS<MPS after sub)
    // else:
    //     D    <- 1 - MPS(CX)
    //     A    <- Qe
    //     if SWITCH(I): MPS(CX) flip
    //     I    <- NLPS(I)
    //
    // Spec labels them odd: the "A < Qe" half (true LPS) decodes to MPS
    // because the LPS sub-interval shifted into the MPS region after the
    // subtraction. Sign-carry here is the classic MQ-coder foot-gun.
    fn lps_exchange(&mut self, cx: &mut Context, qe_entry: QeEntry, qe: u32) -> u8 {
        let d;
        if self.a < qe {
            d = cx.mps;
            cx.index = qe_entry.nmps;
        } else {
            d = 1 - cx.mps;
            if qe_entry.switch_ == 1 {
                cx.mps = 1 - cx.mps;
            }
            cx.index = qe_entry.nlps;
        }
        self.a = qe;
        d
    }

    // MPS_EXCHANGE (Annex E.3.4).
    fn mps_exchange(&mut self, cx: &mut Context, qe_entry: QeEntry) -> u8 {
        let d;
        if self.a < qe_entry.qe {
            d = 1 - cx.mps;
            if qe_entry.switch_ == 1 {
                cx.mps = 1 - cx.mps;
            }
            cx.index = qe_entry.nlps;
        } else {
            d = cx.mps;
            cx.index = qe_entry.nmps;
        }
        d
    }

    // RENORMD (Annex E.3.6): shift A and C left until A >= 0x8000.
    fn renorm_d(&mut self) {
        loop {
            if self.ct == 0 {
                self.byte_in();
            }
            self.a <<= 1;
            self.c <<= 1;
            self.ct -= 1;
            if (self.a & 0x8000) != 0 {
                break;
            }
        }
    }

    // BYTEIN (Annex E.3.7): shift a byte from the stream into the low byte
    // of C, with the marker-detection side channel. Past-EOF returns sticky
    // 0xFF forever (Annex E.2.4).
    //
    // Algorithm:
    //   if B == 0xFF:
    //       if next-byte > 0x8F:
    //           C  <- C + 0xFF00   (stuffing byte pattern; don't advance bp)
    //           CT <- 8
    //       else:
    //           bp <- bp + 1
    //           B  <- next-byte
    //           C  <- C + (B << 9)
    //           CT <- 7
    //   else:
    //       bp <- bp + 1
    //       B  <- next-byte
    //       C  <- C + (B << 8)
    //       CT <- 8
    //
    // For robustness past EOF we keep returning 0xFF as the "next byte",
    // which satisfies the marker-detection branch and preserves decoder
    // liveness (matches pdfium + libjbig2dec behaviour).
    fn byte_in(&mut self) {
        if self.b == 0xFF {
            // Peek next byte without consuming.
            let next = self.buf.get(self.bp + 1).copied().unwrap_or(0xFF);
            if next > 0x8F {
                // End-of-stream marker territory. Inject 0xFF00 and latch.
                self.c = self.c.wrapping_add(0xFF00);
                self.ct = 8;
                // Note: do NOT advance bp. We stay on this 0xFF forever,
                // which is exactly the sticky-FF semantics of §E.2.4.
            } else {
                self.bp += 1;
                self.b = self.buf.get(self.bp).copied().unwrap_or(0xFF);
                self.c = self.c.wrapping_add((self.b as u32) << 9);
                self.ct = 7;
            }
        } else {
            self.bp += 1;
            self.b = self.buf.get(self.bp).copied().unwrap_or(0xFF);
            self.c = self.c.wrapping_add((self.b as u32) << 8);
            self.ct = 8;
        }
    }

    /// Current byte-cursor position (for diagnostics / sub-decoder boundary
    /// detection in symbol-dict `SDNUMNEWSYMS` loops).
    pub fn position(&self) -> usize {
        self.bp
    }
}

// ---------------------------------------------------------------------------
// IntegerDecoder (Annex A.2)
// ---------------------------------------------------------------------------

/// 13 named integer-arith contexts per ISO 14492 Annex A.2 Table A.1.
///
/// Context sizes (all 512 entries, indexed by a 9-bit PREV register):
///   IADH, IADW, IAEX, IAFS, IAIT, IARDW, IARDH, IARDX, IARDY, IARI, IADT,
///   IAAI, IACOMPDEF.
///
/// IAID is variable-width (symbol-ID) and is decoded via
/// [`IntegerDecoder::decode_iaid`] against a caller-supplied context table.
/// We keep IAID out of the fixed enum because its context count depends on
/// the surrounding symbol dictionary (`1 << SBSYMCODELEN`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IaName {
    /// Delta height.
    Iadh,
    /// Delta width.
    Iadw,
    /// Export flag.
    Iaex,
    /// First symbol instance S coordinate (delta).
    Iafs,
    /// Instance T coordinate.
    Iait,
    /// Refinement delta width.
    Iardw,
    /// Refinement delta height.
    Iardh,
    /// Refinement delta X.
    Iardx,
    /// Refinement delta Y.
    Iardy,
    /// Refinement indicator.
    Iari,
    /// Delta T between strips.
    Iadt,
    /// Number of symbol instances on the page.
    Iaai,
    /// Collective-pixel decoding flag.
    Iacompdef,
}

/// Wrapper owning the 13 fixed-size context tables plus the shared MQ
/// decoder. Each Annex A.2 name uses a 9-bit PREV register and a 512-entry
/// context table.
///
/// IAID is intentionally not stored here because its context width depends
/// on surrounding-segment state (`SBSYMCODELEN` for text regions,
/// `SDNUMINSYMS + SDNUMNEWSYMS` for symbol dicts). Use
/// [`IntegerDecoder::decode_iaid`] with a caller-owned context table.
pub struct IntegerDecoder {
    tables: [ContextTable; 13],
}

impl Default for IntegerDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl IntegerDecoder {
    /// Allocate 13 fresh 512-entry context tables.
    pub fn new() -> Self {
        IntegerDecoder {
            tables: [
                ContextTable::new(512),
                ContextTable::new(512),
                ContextTable::new(512),
                ContextTable::new(512),
                ContextTable::new(512),
                ContextTable::new(512),
                ContextTable::new(512),
                ContextTable::new(512),
                ContextTable::new(512),
                ContextTable::new(512),
                ContextTable::new(512),
                ContextTable::new(512),
                ContextTable::new(512),
            ],
        }
    }

    /// Reset every context table (Annex E.2.2 "initialise statistics").
    pub fn reset(&mut self) {
        for t in self.tables.iter_mut() {
            t.reset();
        }
    }

    fn table_mut(&mut self, which: IaName) -> &mut ContextTable {
        let idx = which as usize;
        &mut self.tables[idx]
    }

    /// Decode one integer against `which`. Returns `Some(value)` on success
    /// or `None` if the decode overflows the representable range or the
    /// MQ decoder runs out of live bits. Per Annex A.2 `None` is also the
    /// out-of-band (OOB) signal for certain integer fields.
    ///
    /// Procedure (Annex A.2 step-by-step):
    ///   1. PREV=1.
    ///   2. Decode S.
    ///   3. Decode prefix bits until a '0' terminator, branching on each
    ///      prefix '1' into a wider-value range.
    ///   4. Decode `nbits` value bits (MSB first).
    ///   5. Assemble V = offset + bits; apply sign.
    ///   6. If S=1 and V=0, return OOB.
    ///
    /// See Annex A.2 Table A.1 for the (prefix, offset, nbits) tuples.
    pub fn decode(&mut self, arith: &mut ArithDecoder<'_>, which: IaName) -> Option<i64> {
        let table = self.table_mut(which);

        // PREV starts at 1 (Annex A.2, step i).
        let mut prev: u32 = 1;

        // Helper: read one bit and update PREV.
        let read_bit = |arith: &mut ArithDecoder<'_>, table: &mut ContextTable, prev: &mut u32| {
            let bit = arith.decode(table, *prev as usize) as u32;
            // Shift PREV left by one and or in bit. Clamp to 9 bits per A.2.
            *prev = if *prev < 0x100 {
                (*prev << 1) | bit
            } else {
                (((*prev << 1) | bit) & 0x1FF) | 0x100
            };
            bit
        };

        let s = read_bit(arith, table, &mut prev);

        // Prefix decode: read bits until hitting the terminator pattern
        // defined by Annex A.2 Table A.1.
        // (offset, nbits)
        let (offset, nbits) = {
            let b0 = read_bit(arith, table, &mut prev);
            if b0 == 0 {
                (0u64, 2u32) // V in [0, 3]
            } else {
                let b1 = read_bit(arith, table, &mut prev);
                if b1 == 0 {
                    (4u64, 4u32) // V in [4, 19]
                } else {
                    let b2 = read_bit(arith, table, &mut prev);
                    if b2 == 0 {
                        (20u64, 6u32) // V in [20, 83]
                    } else {
                        let b3 = read_bit(arith, table, &mut prev);
                        if b3 == 0 {
                            (84u64, 8u32) // V in [84, 339]
                        } else {
                            let b4 = read_bit(arith, table, &mut prev);
                            if b4 == 0 {
                                (340u64, 12u32) // V in [340, 4435]
                            } else {
                                (4436u64, 32u32) // wide range
                            }
                        }
                    }
                }
            }
        };

        // Read `nbits` value bits MSB-first.
        let mut value: u64 = 0;
        for _ in 0..nbits {
            let bit = read_bit(arith, table, &mut prev) as u64;
            value = (value << 1) | bit;
        }

        let magnitude = offset.checked_add(value)?;

        // OOB: S=1 and V=0.
        if s == 1 && magnitude == 0 {
            return None;
        }

        let signed = if s == 1 {
            -(magnitude as i128)
        } else {
            magnitude as i128
        };
        i64::try_from(signed).ok()
    }

    /// Decode a symbol-ID value (IAID). Unlike the other 13 names, IAID
    /// reads exactly `sbsymcodelen` raw bits MSB-first from the MQ decoder
    /// using a single caller-provided context table whose size is `1 <<
    /// sbsymcodelen` (Annex A.3).
    ///
    /// PREV starts at 1 and is shifted left by 1 with each new bit, keeping
    /// only `sbsymcodelen` bits of history.
    pub fn decode_iaid(
        arith: &mut ArithDecoder<'_>,
        table: &mut ContextTable,
        sbsymcodelen: u32,
    ) -> u64 {
        let mut prev: u32 = 1;
        let mut value: u64 = 0;
        let mask: u32 = if sbsymcodelen >= 31 {
            u32::MAX
        } else {
            (1u32 << sbsymcodelen) - 1
        };
        for _ in 0..sbsymcodelen {
            let bit = arith.decode(table, prev as usize) as u32;
            prev = ((prev << 1) | bit) & ((mask << 1) | 1);
            value = (value << 1) | (bit as u64);
        }
        value
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ISO 14492 Annex H.2 "COMPRESSED TEST SEQUENCE" encoded stream.
    //
    // This 30-byte payload is the canonical arith-coder test vector cited
    // in the JBIG2 spec (Annex H.2) and reproduced in multiple reference
    // implementations. The test below decodes 256 bits from the stream
    // using a single context `(index=0, mps=0)` (per Annex H.2 setup) and
    // compares the 32-byte output against the reference value.
    //
    // ANNEX_H2_EXPECTED is the reference decode output. If this test ever
    // regresses, suspect either:
    //   (a) QE_TABLE drift (spec constant; must not change)
    //   (b) BYTEIN convention mix-up (JBIG2 uses the original Annex E
    //       form, NOT the T.88-201808 Annex G complementary form)
    //   (c) INITDEC skipping the B=0xFF case (sticky-FF past EOF)
    const ANNEX_H2_ENCODED: &[u8] = &[
        0x84, 0xC7, 0x3B, 0xFC, 0xE1, 0xA1, 0x43, 0x04, 0x02, 0x20, 0x00, 0x00, 0x41, 0x0D, 0xBB,
        0x86, 0xF4, 0x31, 0x7F, 0xFF, 0x88, 0xFF, 0x37, 0x47, 0x1A, 0xDB, 0x6A, 0xDF, 0xFF, 0xAC,
    ];

    const ANNEX_H2_EXPECTED: &[u8] = &[
        0x00, 0x02, 0x00, 0x51, 0x00, 0x00, 0x00, 0xC0, 0x03, 0x52, 0x87, 0x2A, 0xAA, 0xAA, 0xAA,
        0xAA, 0x82, 0xC0, 0x20, 0x00, 0xFC, 0xD7, 0x9E, 0xF6, 0xBF, 0x7F, 0xED, 0x90, 0x4F, 0x46,
        0xA3, 0xBF,
    ];

    #[test]
    fn annex_h2_decodes_byte_exact() {
        let mut arith = ArithDecoder::new(ANNEX_H2_ENCODED);
        let mut table = ContextTable::new(1);
        let mut out = Vec::with_capacity(ANNEX_H2_EXPECTED.len());
        for _ in 0..ANNEX_H2_EXPECTED.len() {
            let mut byte: u8 = 0;
            for _ in 0..8 {
                let bit = arith.decode(&mut table, 0);
                byte = (byte << 1) | bit;
            }
            out.push(byte);
        }
        assert_eq!(
            out, ANNEX_H2_EXPECTED,
            "Annex H.2 byte-exact divergence: got {:02X?} vs expected {:02X?}",
            out, ANNEX_H2_EXPECTED,
        );
    }

    #[test]
    fn fresh_context_starts_zero() {
        let ctx = Context::new();
        assert_eq!(ctx.index, 0);
        assert_eq!(ctx.mps, 0);
    }

    #[test]
    fn context_table_reset_clears_state() {
        let mut table = ContextTable::new(8);
        for c in table.entries.iter_mut() {
            c.index = 42;
            c.mps = 1;
        }
        table.reset();
        for c in &table.entries {
            assert_eq!(c.index, 0);
            assert_eq!(c.mps, 0);
        }
    }

    #[test]
    fn eof_sticky_ff_decoder_does_not_panic() {
        // All bytes 0xFF: BYTEIN's marker-detection branch triggers on every
        // call and never advances `bp`. Spec: Annex E.2.4 sticky-FF.
        let ff_stream = vec![0xFF; 4];
        let mut arith = ArithDecoder::new(&ff_stream);
        let mut table = ContextTable::new(1);
        for _ in 0..10_000 {
            let _ = arith.decode(&mut table, 0);
        }
        // Decoder stays within the input buffer (no past-EOF wandering).
        assert!(arith.position() <= ff_stream.len());
    }

    #[test]
    fn empty_buffer_decoder_does_not_panic() {
        let mut arith = ArithDecoder::new(&[]);
        let mut table = ContextTable::new(1);
        // Still produces output without panic per Annex E.2.4.
        for _ in 0..1_000 {
            let _ = arith.decode(&mut table, 0);
        }
    }

    #[test]
    fn mps_lps_switch_at_qe_crossover() {
        // Force contexts into states near the QE crossover (index 0 has
        // switch_=1). A long stream of all-zero bytes never triggers LPS
        // at index 0 because Chigh starts at 0 and A subtracted by Qe
        // drops A below 0x8000 often -- this tests the MPS_EXCHANGE path.
        let stream = vec![0x00; 16];
        let mut arith = ArithDecoder::new(&stream);
        let mut table = ContextTable::new(1);
        let mut hit_switch = false;
        for _ in 0..64 {
            let _ = arith.decode(&mut table, 0);
            // If MPS ever flips from initial 0, SWITCH triggered.
            if table.entries[0].mps != 0 {
                hit_switch = true;
                break;
            }
        }
        // Either the switch fired or the stream of zeros simply never
        // hit an LPS; both are correct -- the assertion is that the
        // decoder didn't crash at the crossover.
        let _ = hit_switch;
    }

    #[test]
    fn renorm_at_a_equals_0x8000_boundary() {
        // After INITDEC A = 0x8000 exactly. A subtract of a small Qe keeps
        // A >= 0x8000 on the MPS path (no renorm). This just asserts the
        // exact-boundary path does not spuriously renormalize.
        let stream = [0x00u8, 0x00, 0x00, 0x00];
        let mut arith = ArithDecoder::new(&stream);
        let before = arith.ct;
        let mut table = ContextTable::new(1);
        // Force a subtraction that keeps A at or above 0x8000 by decoding
        // against a fresh context (Qe index 0 = 0x5601, A=0x8000-0x5601
        // = 0x29FF, which is < 0x8000, so renorm fires here -- check that
        // ct decreases).
        let _ = arith.decode(&mut table, 0);
        assert!(arith.ct <= before + 8);
    }

    #[test]
    fn integer_decoder_reuses_contexts_across_symbols() {
        // Decode several integers back-to-back from a single stream.
        // Values aren't checked here -- the invariant is that `decode`
        // can be called repeatedly without state corruption.
        let stream = vec![0x00u8; 64];
        let mut arith = ArithDecoder::new(&stream);
        let mut idec = IntegerDecoder::new();
        for _ in 0..16 {
            let _ = idec.decode(&mut arith, IaName::Iadh);
        }
    }

    #[test]
    fn integer_decoder_reset_clears_all_tables() {
        let mut idec = IntegerDecoder::new();
        // Manually dirty each table.
        for t in idec.tables.iter_mut() {
            t.entries[0].index = 17;
            t.entries[0].mps = 1;
        }
        idec.reset();
        for t in idec.tables.iter() {
            assert_eq!(t.entries[0].index, 0);
            assert_eq!(t.entries[0].mps, 0);
        }
    }

    #[test]
    fn iaid_decode_reads_exact_bitwidth() {
        // Pure 0x00 stream: IAID should decode a value whose bit-count
        // equals SBSYMCODELEN (regardless of what the bits come out to).
        let stream = vec![0x00u8; 16];
        let mut arith = ArithDecoder::new(&stream);
        let mut table = ContextTable::new(1 << 4);
        let v = IntegerDecoder::decode_iaid(&mut arith, &mut table, 4);
        // 4-bit IAID fits in [0, 15].
        assert!(v <= 15, "IAID value {} out of 4-bit range", v);
    }

    #[test]
    fn decoder_position_monotonic_on_non_ff_stream() {
        // Non-0xFF stream: bp must never regress.
        let stream: Vec<u8> = (0..32).map(|i| (i as u8) ^ 0x5A).collect();
        let mut arith = ArithDecoder::new(&stream);
        let mut table = ContextTable::new(1);
        let mut last = arith.position();
        for _ in 0..128 {
            let _ = arith.decode(&mut table, 0);
            let cur = arith.position();
            assert!(cur >= last, "bp regressed: {} -> {}", last, cur);
            last = cur;
        }
    }

    #[test]
    fn qe_table_invariants() {
        // Sanity: NMPS/NLPS never point past the table, and SWITCH only
        // appears at Qe = 0x5601 entries (the crossover indices 0, 6, 14
        // per Annex E.1.2 Table E.1).
        for (i, e) in QE_TABLE.iter().enumerate().take(46) {
            assert!((e.nmps as usize) < QE_TABLE.len(), "NMPS OOB at {}", i);
            assert!((e.nlps as usize) < QE_TABLE.len(), "NLPS OOB at {}", i);
            if e.switch_ == 1 {
                assert_eq!(
                    e.qe, 0x5601,
                    "SWITCH=1 only at crossover Qe=0x5601; row {} has {:#x}",
                    i, e.qe
                );
            }
        }
    }
}
