//! Stream decoding: Filter chains, predictors, decompression bomb detection.
//!
//! Pure function: takes raw bytes + dictionary, returns decoded bytes.
//! No knowledge of file layout or object resolution.

use flate2::read::DeflateDecoder;
use std::cell::RefCell;
use std::io::Read;

use crate::diagnostics::{DiagnosticsSink, Warning, WarningKind};
use crate::error::{Error, Limit, ResultExt};
use crate::object::types::{PdfDictionary, PdfObject};
use crate::Result;

// ---------------------------------------------------------------------------
// Thread-local buffer pool.
//
// Hot allocation path: every FlateDecode / LZW / predictor output buffer was
// a fresh Vec::new() that grew incrementally. At >128 KB, glibc malloc routes
// through mmap, which takes the per-process mmap_lock. P06 load test showed
// throughput peaking at jobs=16 then collapsing as 32+ threads serialised on
// that lock. Tests with MALLOC_MMAP_MAX_=0 made it worse (sys time doubled,
// page faults doubled), confirming the kernel mm_struct lock -- not just
// mmap_sem -- is the bottleneck. mimalloc didn't help (it was 24% slower at
// jobs=64 than glibc).
//
// The fix: hand each thread a small pool of reusable Vecs. After the first
// allocation, subsequent stream decodes reuse the same memory region without
// hitting malloc / brk / mmap at all. Pool capacity is capped so a single
// pathological large stream doesn't bloat the per-thread footprint forever.
// ---------------------------------------------------------------------------

/// Maximum number of buffers retained per thread.
const POOL_MAX_BUFFERS: usize = 4;
/// Above this size we don't return a buffer to the pool -- one outlier
/// shouldn't pin per-thread memory.
const POOL_MAX_BUFFER_BYTES: usize = 32 * 1024 * 1024;

thread_local! {
    /// Per-thread pool of decode-output buffers. Filled by `take_pooled_buffer`
    /// returns and drained by `take_pooled_buffer`. Bounded by
    /// [`POOL_MAX_BUFFERS`] entries.
    static DECODE_POOL: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
}

/// Rent a buffer from the thread-local decode pool, or allocate a fresh one
/// if the pool is empty. The returned buffer is `clear()`ed (capacity kept).
fn take_pooled_buffer() -> Vec<u8> {
    DECODE_POOL
        .with_borrow_mut(|pool| {
            pool.pop().map(|mut v| {
                v.clear();
                v
            })
        })
        .unwrap_or_default()
}

/// Return a buffer to the thread-local decode pool for later reuse, unless
/// the pool is full or the buffer's capacity is too large to retain.
fn return_pooled_buffer(buf: Vec<u8>) {
    if buf.capacity() == 0 || buf.capacity() > POOL_MAX_BUFFER_BYTES {
        return;
    }
    DECODE_POOL.with_borrow_mut(|pool| {
        if pool.len() < POOL_MAX_BUFFERS {
            pool.push(buf);
        }
    });
}

/// Limits applied during stream decompression.
#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
pub struct DecodeLimits {
    /// Maximum decompressed output size in bytes (default 250 MB).
    pub max_decompressed_size: u64,
    /// Maximum decompression ratio (decompressed/compressed). Streams exceeding
    /// this ratio AND exceeding `ratio_floor_size` are rejected as potential
    /// decompression bombs. Default: 100.
    pub max_decompression_ratio: u64,
    /// Minimum decompressed size before ratio limit applies. Small streams
    /// naturally have high ratios and should not be rejected. Default: 10 MB.
    pub ratio_floor_size: u64,
}

impl DecodeLimits {
    /// Create new decode limits with the given parameters.
    pub fn new(
        max_decompressed_size: u64,
        max_decompression_ratio: u64,
        ratio_floor_size: u64,
    ) -> Self {
        Self {
            max_decompressed_size,
            max_decompression_ratio,
            ratio_floor_size,
        }
    }
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_decompressed_size: 250 * 1024 * 1024,
            max_decompression_ratio: 100,
            ratio_floor_size: 10 * 1024 * 1024,
        }
    }
}

/// Parameters extracted from a /DecodeParms entry.
struct DecodeParams {
    predictor: i64,
    colors: i64,
    bits_per_component: i64,
    columns: i64,
    early_change: i64,
    // CCITT-specific params
    k: i64,           // <0 = Group 4, 0 = Group 3 1D, >0 = Group 3 2D
    rows: i64,        // image height (0 = unknown)
    black_is_1: bool, // bit polarity
    // JBIG2-specific: resolved global segment data
    jbig2_globals: Option<Vec<u8>>,
}

impl Default for DecodeParams {
    fn default() -> Self {
        Self {
            predictor: 1,
            colors: 1,
            bits_per_component: 8,
            columns: 1,
            early_change: 1,
            k: 0,
            rows: 0,
            black_is_1: false,
            jbig2_globals: None,
        }
    }
}

impl DecodeParams {
    fn from_dict(dict: &PdfDictionary) -> Self {
        let mut params = Self::default();
        if let Some(v) = dict.get_i64(b"Predictor") {
            params.predictor = v;
        }
        if let Some(v) = dict.get_i64(b"Colors") {
            params.colors = v;
        }
        if let Some(v) = dict.get_i64(b"BitsPerComponent") {
            params.bits_per_component = v;
        }
        if let Some(v) = dict.get_i64(b"Columns") {
            params.columns = v;
        }
        if let Some(v) = dict.get_i64(b"EarlyChange") {
            params.early_change = v;
        }
        if let Some(v) = dict.get_i64(b"K") {
            params.k = v;
        }
        if let Some(v) = dict.get_i64(b"Rows") {
            params.rows = v;
        }
        if let Some(v) = dict.get_bool(b"BlackIs1") {
            params.black_is_1 = v;
        }
        params
    }
}

/// Maximum number of filters in a chain before truncation.
const MAX_FILTER_CHAIN_DEPTH: usize = 16;

/// Decode a PDF stream given its raw bytes and dictionary.
///
/// Applies the filter chain specified by `/Filter` and `/DecodeParms`,
/// with decompression bomb detection enforced by `limits`.
pub fn decode_stream(
    raw_data: &[u8],
    dict: &PdfDictionary,
    limits: &DecodeLimits,
    diagnostics: &dyn DiagnosticsSink,
    data_offset: u64,
) -> Result<Vec<u8>> {
    decode_stream_with_globals(raw_data, dict, limits, diagnostics, data_offset, None)
}

/// Decode a PDF stream with optional JBIG2 global segment data.
pub fn decode_stream_with_globals(
    raw_data: &[u8],
    dict: &PdfDictionary,
    limits: &DecodeLimits,
    diagnostics: &dyn DiagnosticsSink,
    data_offset: u64,
    jbig2_globals: Option<Vec<u8>>,
) -> Result<Vec<u8>> {
    let filters = extract_filters(dict);
    if filters.is_empty() {
        return Ok(raw_data.to_vec());
    }

    if filters.len() > MAX_FILTER_CHAIN_DEPTH {
        diagnostics.warning(Warning::new(
            Some(data_offset),
            WarningKind::InvalidState,
            format!(
                "filter chain depth {} exceeds limit {}, truncating",
                filters.len(),
                MAX_FILTER_CHAIN_DEPTH
            ),
        ));
    }

    let effective_len = filters.len().min(MAX_FILTER_CHAIN_DEPTH);
    let mut params_list = extract_decode_params(dict, filters.len());

    // Inject resolved JBIG2 globals into the params for the JBIG2 filter.
    if let Some(globals) = jbig2_globals {
        for params in &mut params_list {
            params.jbig2_globals = Some(globals.clone());
        }
    }

    let compressed_len = raw_data.len() as u64;
    // Initial copy uses to_vec() (exact-size single alloc) since the pool
    // would only save us if it had a buffer >= raw_data.len(). The pool's
    // win is on the intermediate filter outputs, where decode_flate /
    // apply_predictor / decode_lzw routinely allocate large buffers that
    // were previously freed-then-reallocated on the next stream.
    let mut data = raw_data.to_vec();
    for (i, filter) in filters[..effective_len].iter().enumerate() {
        let params = &params_list[i];
        let new_data =
            apply_filter(filter, &data, params, limits, diagnostics, data_offset).context(
                format!("applying filter /{}", String::from_utf8_lossy(filter)),
            )?;
        // Return the previous filter's output buffer to the pool so the
        // next filter (or the next stream's first filter) can pick it up
        // without going through malloc / mmap.
        let old_data = std::mem::replace(&mut data, new_data);
        return_pooled_buffer(old_data);
    }

    // Ratio-based decompression bomb guard: only kicks in for large outputs
    // (small streams naturally have high ratios and are harmless).
    let decompressed_len = data.len() as u64;
    if decompressed_len > limits.ratio_floor_size {
        let ratio = decompressed_len / compressed_len.max(1);
        if ratio > limits.max_decompression_ratio {
            return Err(Error::resource_limit(Limit::DecompressionRatio {
                ratio,
                limit: limits.max_decompression_ratio,
            }));
        }
    }

    Ok(data)
}

/// Extract the list of filter names from the stream dictionary.
/// Handles both single-name and array forms of /Filter.
fn extract_filters(dict: &PdfDictionary) -> Vec<&[u8]> {
    match dict.get(b"Filter") {
        Some(PdfObject::Name(name)) => vec![name.as_slice()],
        Some(PdfObject::Array(arr)) => arr.iter().filter_map(|obj| obj.as_name()).collect(),
        _ => vec![],
    }
}

/// Extract decode parameters for each filter in the chain.
/// Handles single dict, array of dicts, and missing /DecodeParms.
fn extract_decode_params(dict: &PdfDictionary, count: usize) -> Vec<DecodeParams> {
    match dict.get(b"DecodeParms") {
        Some(PdfObject::Dictionary(d)) => {
            let mut result = vec![DecodeParams::from_dict(d)];
            result.resize_with(count, DecodeParams::default);
            result
        }
        Some(PdfObject::Array(arr)) => {
            let mut result: Vec<DecodeParams> = arr
                .iter()
                .map(|obj| match obj.as_dict() {
                    Some(d) => DecodeParams::from_dict(d),
                    None => DecodeParams::default(),
                })
                .collect();
            result.resize_with(count, DecodeParams::default);
            result
        }
        _ => (0..count).map(|_| DecodeParams::default()).collect(),
    }
}

/// Dispatch to the appropriate filter implementation.
fn apply_filter(
    name: &[u8],
    input: &[u8],
    params: &DecodeParams,
    limits: &DecodeLimits,
    diagnostics: &dyn DiagnosticsSink,
    data_offset: u64,
) -> Result<Vec<u8>> {
    match name {
        b"FlateDecode" | b"Fl" => decode_flate(input, params, limits, diagnostics, data_offset),
        b"ASCIIHexDecode" | b"AHx" => Ok(decode_ascii_hex(input, diagnostics, data_offset)),
        b"ASCII85Decode" | b"A85" => decode_ascii85(input, diagnostics, data_offset),
        b"LZWDecode" | b"LZW" => decode_lzw(input, params, limits, diagnostics, data_offset),
        b"RunLengthDecode" | b"RL" => decode_run_length(input, limits, diagnostics, data_offset),
        b"CCITTFaxDecode" | b"CCF" => {
            // Clamp width/height at the boundary: /Columns and /Rows are attacker
            // controlled. decode_ccitt also clamps via MAX_IMAGE_DIMENSION, but
            // clamping here keeps the inferred height (when /Rows is absent) from
            // ballooning into a 64-bit bomb via tiny /Columns + huge input.
            let max_dim = udoc_image::MAX_IMAGE_DIMENSION as usize;
            let width = (params.columns.max(1) as usize).min(max_dim);
            let height = if params.rows > 0 {
                (params.rows as usize).min(max_dim)
            } else {
                ((input.len() * 8 / width.max(1)).max(1)).min(max_dim)
            };
            match udoc_image::decode_ccitt(
                input,
                udoc_image::CcittParams {
                    width,
                    height,
                    k: params.k,
                    black_is_1: params.black_is_1,
                },
            ) {
                Some(decoded) => Ok(decoded.pixels),
                None => {
                    diagnostics.warning(Warning::new(
                        Some(data_offset),
                        WarningKind::UnsupportedFilter,
                        String::from("CCITTFaxDecode: decoding failed, passing through raw data"),
                    ));
                    Ok(input.to_vec())
                }
            }
        }
        b"JBIG2Decode" => {
            #[cfg(feature = "jbig2")]
            {
                let globals = params.jbig2_globals.as_deref();
                if std::env::var_os("UDOC_JBIG2_DEBUG").is_some() {
                    eprintln!(
                        "JBIG2Decode filter called: input={} bytes, globals={:?}",
                        input.len(),
                        globals.map(|g| g.len())
                    );
                }
                match udoc_image::decode_jbig2(input, udoc_image::Jbig2Params { globals }) {
                    Some(decoded) => Ok(decoded.pixels),
                    None => {
                        diagnostics.warning(Warning::new(
                            Some(data_offset),
                            WarningKind::UnsupportedFilter,
                            String::from("JBIG2Decode: decoding failed, passing through raw data"),
                        ));
                        Ok(input.to_vec())
                    }
                }
            }
            #[cfg(not(feature = "jbig2"))]
            {
                // The jbig2 feature is disabled; skip decode entirely.
                // Downstream consumers see the raw JBIG2 bytes and can detect the
                // UnsupportedFilter warning.
                let _ = params.jbig2_globals.as_deref();
                diagnostics.warning(Warning::new(
                    Some(data_offset),
                    WarningKind::UnsupportedFilter,
                    String::from(
                        "JBIG2Decode: udoc built without the `jbig2` feature, passing through raw data",
                    ),
                ));
                Ok(input.to_vec())
            }
        }
        // Image filters: irrelevant for text extraction, pass through raw bytes.
        b"DCTDecode" | b"DCT" | b"JPXDecode" => {
            diagnostics.warning(Warning::new(
                Some(data_offset),
                WarningKind::UnsupportedFilter,
                format!(
                    "image filter {} not decoded, passing through raw data",
                    String::from_utf8_lossy(name)
                ),
            ));
            Ok(input.to_vec())
        }
        _ => Err(Error::structure(format!(
            "unsupported stream filter: {}",
            String::from_utf8_lossy(name)
        ))),
    }
}

// ---------------------------------------------------------------------------
// ASCIIHexDecode
// ---------------------------------------------------------------------------

/// Decode ASCIIHexDecode data. Hex pairs, whitespace tolerant, `>` terminator.
fn decode_ascii_hex(input: &[u8], diagnostics: &dyn DiagnosticsSink, data_offset: u64) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len() / 2);
    let mut high_nibble: Option<u8> = None;

    for &b in input {
        if b == b'>' {
            break;
        }
        if b.is_ascii_whitespace() {
            continue;
        }
        match hex_nibble(b) {
            Some(nibble) => match high_nibble.take() {
                None => high_nibble = Some(nibble),
                Some(high) => output.push((high << 4) | nibble),
            },
            None => {
                diagnostics.warning(Warning::new(
                    Some(data_offset),
                    WarningKind::DecodeError,
                    format!("ASCIIHexDecode: invalid hex byte 0x{:02X}", b),
                ));
            }
        }
    }

    // Odd trailing nibble: pad with 0 (per spec)
    if let Some(high) = high_nibble {
        output.push(high << 4);
    }

    output
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// ASCII85Decode
// ---------------------------------------------------------------------------

/// Decode ASCII85Decode data. Base-85 encoding, `~>` terminator, `z` shorthand.
fn decode_ascii85(
    input: &[u8],
    diagnostics: &dyn DiagnosticsSink,
    data_offset: u64,
) -> Result<Vec<u8>> {
    let mut output = Vec::with_capacity(input.len() * 4 / 5);
    let mut group: [u8; 5] = [0; 5];
    let mut group_len: usize = 0;

    for &b in input {
        // End-of-data marker
        if b == b'~' {
            // Expect '>'
            break;
        }

        if b.is_ascii_whitespace() {
            continue;
        }

        if b == b'z' {
            if group_len != 0 {
                diagnostics.warning(Warning::new(
                    Some(data_offset),
                    WarningKind::DecodeError,
                    "'z' in middle of ASCII85 group",
                ));
                // Flush partial group before processing 'z'
                decode_ascii85_group(&group, group_len, &mut output, diagnostics, data_offset);
                group_len = 0;
            }
            output.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }

        if !(b'!'..=b'u').contains(&b) {
            diagnostics.warning(Warning::new(
                Some(data_offset),
                WarningKind::DecodeError,
                format!("ASCII85Decode: invalid byte 0x{:02X}", b),
            ));
            continue;
        }

        group[group_len] = b - b'!';
        group_len += 1;

        if group_len == 5 {
            decode_ascii85_group(&group, 5, &mut output, diagnostics, data_offset);
            group_len = 0;
        }
    }

    // Handle final partial group (2-4 chars produce 1-3 bytes)
    if group_len > 1 {
        decode_ascii85_group(&group, group_len, &mut output, diagnostics, data_offset);
    } else if group_len == 1 {
        diagnostics.warning(Warning::new(
            Some(data_offset),
            WarningKind::DecodeError,
            "ASCII85Decode: single trailing character (invalid)",
        ));
    }

    Ok(output)
}

/// Decode one ASCII85 group of 2-5 characters into output bytes.
fn decode_ascii85_group(
    group: &[u8; 5],
    len: usize,
    output: &mut Vec<u8>,
    diagnostics: &dyn DiagnosticsSink,
    data_offset: u64,
) {
    // Pad missing chars with 'u' (84) per spec
    let mut padded = [84u8; 5];
    padded[..len].copy_from_slice(&group[..len]);

    let mut value: u64 = 0;
    for &digit in &padded {
        value = value * 85 + u64::from(digit);
    }

    if value > u64::from(u32::MAX) {
        diagnostics.warning(Warning::new(
            Some(data_offset),
            WarningKind::DecodeError,
            "ASCII85Decode: group overflow",
        ));
        return;
    }

    let bytes = (value as u32).to_be_bytes();
    let out_len = len - 1; // 5 chars -> 4 bytes, 4 -> 3, 3 -> 2, 2 -> 1
    output.extend_from_slice(&bytes[..out_len]);
}

// ---------------------------------------------------------------------------
// FlateDecode
// ---------------------------------------------------------------------------

/// Decode FlateDecode (zlib/deflate) data with bomb detection.
/// Tries zlib wrapper first, falls back to raw deflate on failure.
fn decode_flate(
    input: &[u8],
    params: &DecodeParams,
    limits: &DecodeLimits,
    diagnostics: &dyn DiagnosticsSink,
    data_offset: u64,
) -> Result<Vec<u8>> {
    // Rent a pooled buffer. After this function returns, the resulting
    // Vec is owned by the caller; whether the caller can return it to
    // the pool is up to higher layers. The win here is that on the first
    // call per thread the buffer allocates fresh, and on subsequent
    // calls (after a `return_pooled_buffer` somewhere up the stack)
    // we reuse the same memory region -- skipping mmap/brk entirely
    // ( root-cause).
    let mut raw = take_pooled_buffer();

    if let Err(e) = inflate_into(input, limits, true, &mut raw) {
        match e {
            Error::ResourceLimit(_) => {
                return_pooled_buffer(raw);
                return Err(e);
            }
            _ => {
                diagnostics.warning(Warning::new(
                    Some(data_offset),
                    WarningKind::DecodeError,
                    "FlateDecode: zlib header failed, retrying raw deflate",
                ));
                raw.clear();
                if let Err(e2) = inflate_into(input, limits, false, &mut raw) {
                    return_pooled_buffer(raw);
                    return Err(e2).context("decompressing FlateDecode data");
                }
            }
        }
    }

    if params.predictor > 1 {
        let result = apply_predictor(&raw, params).context("applying predictor");
        return_pooled_buffer(raw);
        result
    } else {
        Ok(raw)
    }
}

/// Inflate `input` into `output` (which is `clear()`ed first) with a size limit,
/// checking during decompression (not after).
///
/// `zlib_header`: true to expect zlib wrapper, false for raw deflate.
///
/// Output is written into the caller's `Vec<u8>` so callers can pool the buffer
/// across many stream decodes.
fn inflate_into(
    input: &[u8],
    limits: &DecodeLimits,
    zlib_header: bool,
    output: &mut Vec<u8>,
) -> Result<()> {
    output.clear();
    let mut buf = [0u8; 65536];

    if zlib_header {
        let mut decoder = flate2::read::ZlibDecoder::new(input);
        loop {
            let n = decoder
                .read(&mut buf)
                .map_err(|e| Error::structure(format!("zlib decompression error: {e}")))?;
            if n == 0 {
                break;
            }
            output.extend_from_slice(&buf[..n]);
            if output.len() as u64 > limits.max_decompressed_size {
                return Err(Error::resource_limit(Limit::DecompressedSize(
                    limits.max_decompressed_size,
                )));
            }
        }
    } else {
        let mut decoder = DeflateDecoder::new(input);
        loop {
            let n = decoder
                .read(&mut buf)
                .map_err(|e| Error::structure(format!("deflate decompression error: {e}")))?;
            if n == 0 {
                break;
            }
            output.extend_from_slice(&buf[..n]);
            if output.len() as u64 > limits.max_decompressed_size {
                return Err(Error::resource_limit(Limit::DecompressedSize(
                    limits.max_decompressed_size,
                )));
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// LZWDecode
// ---------------------------------------------------------------------------

/// LZW clear-table code.
const LZW_CLEAR: u16 = 256;
/// LZW end-of-data code.
const LZW_EOD: u16 = 257;

/// Decode LZWDecode data. Variable-width codes, MSB-first, /EarlyChange.
fn decode_lzw(
    input: &[u8],
    params: &DecodeParams,
    limits: &DecodeLimits,
    diagnostics: &dyn DiagnosticsSink,
    data_offset: u64,
) -> Result<Vec<u8>> {
    let early_change = params.early_change != 0;
    let mut reader = BitReader::new(input);
    let mut output = Vec::new();

    // Index-based LZW table: entries are (offset, len) into `string_pool`.
    // Avoids per-code Vec<u8> cloning (fixes #14).
    let mut string_pool: Vec<u8> = (0..=255u8).collect();
    // Initialize table: entries 0-255 are single bytes, 256/257 are clear/EOD (empty).
    let mut table: Vec<(usize, usize)> = (0..258)
        .map(|i| {
            if i < 256 {
                (i, 1)
            } else {
                (0, 0) // clear/EOD markers, never read
            }
        })
        .collect();
    let mut code_size: u8 = 9;
    // Track previous entry as (offset, len) in string_pool.
    let mut prev_entry: Option<(usize, usize)> = None;

    while let Some(code) = reader.read_bits(code_size) {
        if code == LZW_CLEAR {
            table.truncate(258);
            string_pool.truncate(256);
            code_size = 9;
            prev_entry = None;
            continue;
        }

        if code == LZW_EOD {
            break;
        }

        let (entry_off, entry_len) = if (code as usize) < table.len() {
            table[code as usize]
        } else if code as usize == table.len() {
            // KwKwK case: code == next code to be added.
            match prev_entry {
                Some((off, len)) => {
                    let first_byte = string_pool[off];
                    let new_off = string_pool.len();
                    string_pool.extend_from_within(off..off + len);
                    string_pool.push(first_byte);
                    (new_off, len + 1)
                }
                None => {
                    diagnostics.warning(Warning::new(
                        Some(data_offset),
                        WarningKind::DecodeError,
                        "LZWDecode: unexpected code with no previous entry",
                    ));
                    break;
                }
            }
        } else {
            diagnostics.warning(Warning::new(
                Some(data_offset),
                WarningKind::DecodeError,
                format!(
                    "LZWDecode: code {} out of range (table size {})",
                    code,
                    table.len()
                ),
            ));
            break;
        };

        output.extend_from_slice(&string_pool[entry_off..entry_off + entry_len]);
        if output.len() as u64 > limits.max_decompressed_size {
            return Err(Error::resource_limit(Limit::DecompressedSize(
                limits.max_decompressed_size,
            )));
        }

        // Add new table entry: prev_entry + first byte of current entry
        if let Some((prev_off, prev_len)) = prev_entry {
            if table.len() < 4096 {
                let first_byte = string_pool[entry_off];
                let new_off = string_pool.len();
                string_pool.extend_from_within(prev_off..prev_off + prev_len);
                string_pool.push(first_byte);
                table.push((new_off, prev_len + 1));
            }
        }

        // Bump code size when table reaches the threshold
        let threshold = if early_change {
            (1u32 << code_size) - 1
        } else {
            1u32 << code_size
        };
        if table.len() as u32 >= threshold && code_size < 12 {
            code_size += 1;
        }

        prev_entry = Some((entry_off, entry_len));
    }

    if params.predictor > 1 {
        apply_predictor(&output, params).context("applying predictor after LZW")
    } else {
        Ok(output)
    }
}

/// MSB-first bit reader for LZW.
struct BitReader<'a> {
    data: &'a [u8],
    byte_pos: usize,
    bit_pos: u8, // 0..8, bits consumed in current byte (from MSB)
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_pos: 0,
        }
    }

    fn read_bits(&mut self, count: u8) -> Option<u16> {
        let mut result: u16 = 0;
        for _ in 0..count {
            if self.byte_pos >= self.data.len() {
                return None;
            }
            let bit = (self.data[self.byte_pos] >> (7 - self.bit_pos)) & 1;
            result = (result << 1) | u16::from(bit);
            self.bit_pos += 1;
            if self.bit_pos == 8 {
                self.bit_pos = 0;
                self.byte_pos += 1;
            }
        }
        Some(result)
    }
}

// ---------------------------------------------------------------------------
// RunLengthDecode
// ---------------------------------------------------------------------------

/// Decode RunLengthDecode data (PDF spec, Table 3.40).
///
/// - Length byte 0-127: copy next `n+1` bytes literally
/// - Length byte 129-255: repeat the next byte `257-n` times
/// - Length byte 128: end-of-data
fn decode_run_length(
    input: &[u8],
    limits: &DecodeLimits,
    diagnostics: &dyn DiagnosticsSink,
    data_offset: u64,
) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut i = 0;

    while i < input.len() {
        let n = input[i];
        i += 1;

        if n == 128 {
            // EOD
            break;
        } else if n <= 127 {
            // Copy next n+1 bytes literally
            let count = n as usize + 1;
            let avail = input.len().saturating_sub(i);
            if avail < count {
                diagnostics.warning(Warning::new(
                    Some(data_offset),
                    WarningKind::DecodeError,
                    format!(
                        "RunLengthDecode: truncated literal run (need {} bytes, have {})",
                        count, avail
                    ),
                ));
                output.extend_from_slice(&input[i..i + avail]);
                break;
            }
            output.extend_from_slice(&input[i..i + count]);
            i += count;
        } else {
            // Repeat next byte 257-n times
            let count = 257 - n as usize;
            if i >= input.len() {
                diagnostics.warning(Warning::new(
                    Some(data_offset),
                    WarningKind::DecodeError,
                    "RunLengthDecode: truncated repeat run (missing data byte)",
                ));
                break;
            }
            let byte = input[i];
            i += 1;
            for _ in 0..count {
                output.push(byte);
            }
        }

        if output.len() as u64 > limits.max_decompressed_size {
            return Err(Error::resource_limit(Limit::DecompressedSize(
                limits.max_decompressed_size,
            )));
        }
    }

    Ok(output)
}

// ---------------------------------------------------------------------------
// Predictor support
// ---------------------------------------------------------------------------

/// Apply predictor post-processing to decompressed data.
fn apply_predictor(data: &[u8], params: &DecodeParams) -> Result<Vec<u8>> {
    match params.predictor {
        1 => Ok(data.to_vec()),
        2 => apply_tiff_predictor(data, params),
        10..=15 => apply_png_predictor(data, params),
        other => Err(Error::structure(format!("unsupported predictor: {other}"))),
    }
}

/// Maximum columns in predictor (guards against overflow/OOM).
const MAX_PREDICTOR_COLUMNS: i64 = 100_000;
/// Maximum color channels in predictor.
const MAX_PREDICTOR_COLORS: i64 = 256;
/// Maximum bits per component in predictor.
const MAX_PREDICTOR_BPC: i64 = 16;

/// Apply PNG predictor (types 10-15). Each row has a filter-type byte prefix.
fn apply_png_predictor(data: &[u8], params: &DecodeParams) -> Result<Vec<u8>> {
    if params.columns < 0 || params.colors < 0 || params.bits_per_component < 0 {
        return Err(Error::structure(format!(
            "negative predictor params: columns={}, colors={}, bpc={}",
            params.columns, params.colors, params.bits_per_component
        )));
    }
    if params.columns > MAX_PREDICTOR_COLUMNS
        || params.colors > MAX_PREDICTOR_COLORS
        || params.bits_per_component > MAX_PREDICTOR_BPC
    {
        return Err(Error::structure(format!(
            "predictor params out of range: columns={}, colors={}, bpc={}",
            params.columns, params.colors, params.bits_per_component
        )));
    }

    let colors = params.colors.max(1) as usize;
    let bpc = params.bits_per_component.max(1) as usize;
    let columns = params.columns.max(1) as usize;

    // Bytes per pixel (rounded up for sub-byte components)
    let bpp = (colors * bpc).div_ceil(8);
    // Bytes per row of actual data (no filter byte)
    let bytes_per_row = (colors * bpc * columns).div_ceil(8);
    // Each row in input: 1 filter byte + bytes_per_row data bytes
    let input_row_len = 1 + bytes_per_row;

    if bytes_per_row == 0 {
        return Ok(Vec::new());
    }

    let row_count = data.len() / input_row_len;
    let mut output = Vec::with_capacity(row_count * bytes_per_row);
    // Reuse two row buffers across iterations to avoid per-row allocation.
    let mut prev_row = vec![0u8; bytes_per_row];
    let mut current_row = vec![0u8; bytes_per_row];

    for row_idx in 0..row_count {
        let row_start = row_idx * input_row_len;
        if row_start + input_row_len > data.len() {
            break;
        }
        let filter_type = data[row_start];
        let row_data = &data[row_start + 1..row_start + input_row_len];

        for i in 0..bytes_per_row {
            let raw = row_data[i];
            let a = if i >= bpp { current_row[i - bpp] } else { 0 };
            let b = prev_row[i];
            let c = if i >= bpp { prev_row[i - bpp] } else { 0 };

            current_row[i] = match filter_type {
                0 => raw,                                                         // None
                1 => raw.wrapping_add(a),                                         // Sub
                2 => raw.wrapping_add(b),                                         // Up
                3 => raw.wrapping_add(((u16::from(a) + u16::from(b)) / 2) as u8), // Average
                4 => raw.wrapping_add(paeth_predictor(a, b, c)),                  // Paeth
                _ => raw, // Unknown filter type: treat as None
            };
        }

        output.extend_from_slice(&current_row);
        std::mem::swap(&mut prev_row, &mut current_row);
    }

    Ok(output)
}

/// Paeth predictor function (PNG spec).
fn paeth_predictor(a: u8, b: u8, c: u8) -> u8 {
    let a = i16::from(a);
    let b = i16::from(b);
    let c = i16::from(c);
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

/// TIFF predictor 2: horizontal differencing.
fn apply_tiff_predictor(data: &[u8], params: &DecodeParams) -> Result<Vec<u8>> {
    if params.columns < 0 || params.colors < 0 || params.bits_per_component < 0 {
        return Err(Error::structure(format!(
            "negative predictor params: columns={}, colors={}, bpc={}",
            params.columns, params.colors, params.bits_per_component
        )));
    }
    if params.columns > MAX_PREDICTOR_COLUMNS
        || params.colors > MAX_PREDICTOR_COLORS
        || params.bits_per_component > MAX_PREDICTOR_BPC
    {
        return Err(Error::structure(format!(
            "predictor params out of range: columns={}, colors={}, bpc={}",
            params.columns, params.colors, params.bits_per_component
        )));
    }

    let colors = params.colors.max(1) as usize;
    let bpc = params.bits_per_component as usize;
    let columns = params.columns.max(1) as usize;

    // Only handle 8-bit components for now (covers vast majority of PDFs)
    if bpc != 8 {
        return Err(Error::structure(format!(
            "TIFF predictor with {bpc} bits/component not supported (only 8)"
        )));
    }

    let bytes_per_row = colors * columns;
    if bytes_per_row == 0 {
        return Ok(Vec::new());
    }

    let mut output = data.to_vec();
    let row_count = output.len() / bytes_per_row;

    for row_idx in 0..row_count {
        let row_start = row_idx * bytes_per_row;
        // Accumulate from the second pixel onward
        for i in colors..bytes_per_row {
            let pos = row_start + i;
            if pos >= output.len() {
                break;
            }
            output[pos] = output[pos].wrapping_add(output[pos - colors]);
        }
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{CollectingDiagnostics, NullDiagnostics};
    use crate::error::ResourceLimitError;

    fn null_diag() -> &'static dyn DiagnosticsSink {
        &NullDiagnostics
    }

    fn default_limits() -> DecodeLimits {
        DecodeLimits::default()
    }

    // -- extract_filters --

    #[test]
    fn test_extract_filters_none() {
        let dict = PdfDictionary::new();
        assert!(extract_filters(&dict).is_empty());
    }

    #[test]
    fn test_extract_filters_single_name() {
        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"FlateDecode".to_vec()));
        let filters = extract_filters(&dict);
        assert_eq!(filters, vec![b"FlateDecode".as_slice()]);
    }

    #[test]
    fn test_extract_filters_array() {
        let mut dict = PdfDictionary::new();
        dict.insert(
            b"Filter".to_vec(),
            PdfObject::Array(vec![
                PdfObject::Name(b"ASCIIHexDecode".to_vec()),
                PdfObject::Name(b"FlateDecode".to_vec()),
            ]),
        );
        let filters = extract_filters(&dict);
        assert_eq!(filters.len(), 2);
        assert_eq!(filters[0], b"ASCIIHexDecode");
        assert_eq!(filters[1], b"FlateDecode");
    }

    // -- extract_decode_params --

    #[test]
    fn test_extract_decode_params_missing() {
        let dict = PdfDictionary::new();
        let params = extract_decode_params(&dict, 2);
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].predictor, 1);
    }

    #[test]
    fn test_extract_decode_params_single_dict() {
        let mut dp = PdfDictionary::new();
        dp.insert(b"Predictor".to_vec(), PdfObject::Integer(12));
        dp.insert(b"Columns".to_vec(), PdfObject::Integer(4));

        let mut dict = PdfDictionary::new();
        dict.insert(b"DecodeParms".to_vec(), PdfObject::Dictionary(dp));

        let params = extract_decode_params(&dict, 1);
        assert_eq!(params[0].predictor, 12);
        assert_eq!(params[0].columns, 4);
    }

    #[test]
    fn test_ascii_hex_basic() {
        let input = b"48656C6C6F>";
        let result = decode_ascii_hex(input, null_diag(), 0);
        assert_eq!(result, b"Hello");
    }

    #[test]
    fn test_ascii_hex_lowercase() {
        let input = b"48656c6c6f>";
        let result = decode_ascii_hex(input, null_diag(), 0);
        assert_eq!(result, b"Hello");
    }

    #[test]
    fn test_ascii_hex_whitespace() {
        let input = b"48 65 6C 6C 6F>";
        let result = decode_ascii_hex(input, null_diag(), 0);
        assert_eq!(result, b"Hello");
    }

    #[test]
    fn test_ascii_hex_odd_nibble() {
        let input = b"486>";
        let result = decode_ascii_hex(input, null_diag(), 0);
        assert_eq!(result, &[0x48, 0x60]);
    }

    #[test]
    fn test_ascii_hex_empty() {
        let input = b">";
        let result = decode_ascii_hex(input, null_diag(), 0);
        assert!(result.is_empty());
    }

    #[test]
    fn test_ascii_hex_no_terminator() {
        let input = b"4865";
        let result = decode_ascii_hex(input, null_diag(), 0);
        assert_eq!(result, b"He");
    }

    #[test]
    fn test_ascii_hex_invalid_char_warns() {
        let diag = CollectingDiagnostics::new();
        let input = b"48ZZ65>";
        let result = decode_ascii_hex(input, &diag, 0);
        assert_eq!(result, b"He");
        assert_eq!(diag.warnings().len(), 2);
    }

    #[test]
    fn test_ascii85_basic() {
        // "Hello" = 87 cURD] in ASCII85
        let input = b"87cURD]~>";
        let result = decode_ascii85(input, null_diag(), 0).unwrap();
        assert_eq!(result, b"Hello");
    }

    #[test]
    fn test_ascii85_z_shorthand() {
        let input = b"z~>";
        let result = decode_ascii85(input, null_diag(), 0).unwrap();
        assert_eq!(result, &[0, 0, 0, 0]);
    }

    #[test]
    fn test_ascii85_empty() {
        let input = b"~>";
        let result = decode_ascii85(input, null_diag(), 0).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_ascii85_partial_group() {
        // Two chars -> 1 byte
        let input = b"/c~>";
        let result = decode_ascii85(input, null_diag(), 0).unwrap();
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_ascii85_whitespace_ignored() {
        let input = b"87 cU RD ]~>";
        let result = decode_ascii85(input, null_diag(), 0).unwrap();
        assert_eq!(result, b"Hello");
    }

    #[test]
    fn test_flate_basic() {
        // Compress some data, then decompress
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let original = b"Hello, PDF world! This is a test of FlateDecode.";
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(original).unwrap();
        let compressed = encoder.finish().unwrap();

        let params = DecodeParams::default();
        let result = decode_flate(&compressed, &params, &default_limits(), null_diag(), 0).unwrap();
        assert_eq!(result, original);
    }

    #[test]
    fn test_flate_raw_deflate_fallback() {
        // Compress with raw deflate (no zlib header) to test fallback
        use flate2::write::DeflateEncoder;
        use flate2::Compression;
        use std::io::Write;

        let original = b"raw deflate test data";
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(original).unwrap();
        let compressed = encoder.finish().unwrap();

        let diag = CollectingDiagnostics::new();
        let params = DecodeParams::default();
        let result = decode_flate(&compressed, &params, &default_limits(), &diag, 0).unwrap();
        assert_eq!(result, original);
        // Should have warned about zlib failure
        assert!(diag
            .warnings()
            .iter()
            .any(|w| w.message.contains("zlib header failed")));
    }

    #[test]
    fn test_flate_bomb_detection() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        // Create data that decompresses to more than our limit
        let original = vec![0u8; 1024];
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&original).unwrap();
        let compressed = encoder.finish().unwrap();

        let tiny_limit = DecodeLimits {
            max_decompressed_size: 100,
            ..DecodeLimits::default()
        };
        let params = DecodeParams::default();
        let err = decode_flate(&compressed, &params, &tiny_limit, null_diag(), 0).unwrap_err();
        assert!(matches!(
            err,
            Error::ResourceLimit(ResourceLimitError {
                limit: Limit::DecompressedSize(100),
                ..
            })
        ));
    }

    // -- PNG predictor --

    #[test]
    fn test_png_predictor_none() {
        // Filter type 0 (None): data passes through
        let data = vec![0, 10, 20, 30, 0, 40, 50, 60];
        let params = DecodeParams {
            predictor: 10,
            columns: 3,
            colors: 1,
            bits_per_component: 8,
            ..DecodeParams::default()
        };
        let result = apply_png_predictor(&data, &params).unwrap();
        assert_eq!(result, &[10, 20, 30, 40, 50, 60]);
    }

    #[test]
    fn test_png_predictor_sub() {
        // Filter type 1 (Sub): each byte = raw + left neighbor
        // Row: filter=1, raw=[10, 5, 3]
        // Decoded: [10, 10+5=15, 15+3=18]
        let data = vec![1, 10, 5, 3];
        let params = DecodeParams {
            predictor: 11,
            columns: 3,
            colors: 1,
            bits_per_component: 8,
            ..DecodeParams::default()
        };
        let result = apply_png_predictor(&data, &params).unwrap();
        assert_eq!(result, &[10, 15, 18]);
    }

    #[test]
    fn test_png_predictor_up() {
        // Filter type 2 (Up): each byte = raw + byte above
        let data = vec![
            0, 10, 20, 30, // row 0: None
            2, 5, 5, 5, // row 1: Up
        ];
        let params = DecodeParams {
            predictor: 12,
            columns: 3,
            colors: 1,
            bits_per_component: 8,
            ..DecodeParams::default()
        };
        let result = apply_png_predictor(&data, &params).unwrap();
        assert_eq!(result, &[10, 20, 30, 15, 25, 35]);
    }

    #[test]
    fn test_png_predictor_average() {
        // Filter type 3 (Average): raw + floor((left + above) / 2)
        let data = vec![
            0, 10, 20, 30, // row 0: None
            3, 5, 5, 5, // row 1: Average
        ];
        let params = DecodeParams {
            predictor: 13,
            columns: 3,
            colors: 1,
            bits_per_component: 8,
            ..DecodeParams::default()
        };
        let result = apply_png_predictor(&data, &params).unwrap();
        // Row 0: [10, 20, 30]
        // Row 1, i=0: 5 + floor((0 + 10)/2) = 5 + 5 = 10
        // Row 1, i=1: 5 + floor((10 + 20)/2) = 5 + 15 = 20
        // Row 1, i=2: 5 + floor((20 + 30)/2) = 5 + 25 = 30
        assert_eq!(result, &[10, 20, 30, 10, 20, 30]);
    }

    #[test]
    fn test_png_predictor_paeth() {
        // Filter type 4 (Paeth): first row, no above or upper-left
        let data = vec![4, 10, 5, 3];
        let params = DecodeParams {
            predictor: 14,
            columns: 3,
            colors: 1,
            bits_per_component: 8,
            ..DecodeParams::default()
        };
        let result = apply_png_predictor(&data, &params).unwrap();
        // First row: a=0, b=0, c=0, paeth(0,0,0)=0
        // i=0: 10+paeth(0,0,0) = 10
        // i=1: 5+paeth(10,0,0) = 5+10 = 15
        // i=2: 3+paeth(15,0,0) = 3+15 = 18
        assert_eq!(result, &[10, 15, 18]);
    }

    #[test]
    fn test_png_predictor_mixed_filter_types() {
        // Different filter per row (predictor 15 = optimum, per-row selection)
        let data = vec![
            0, 10, 20, 30, // row 0: None
            1, 5, 5, 5, // row 1: Sub
        ];
        let params = DecodeParams {
            predictor: 15,
            columns: 3,
            colors: 1,
            bits_per_component: 8,
            ..DecodeParams::default()
        };
        let result = apply_png_predictor(&data, &params).unwrap();
        assert_eq!(result, &[10, 20, 30, 5, 10, 15]);
    }

    // -- TIFF predictor --

    #[test]
    fn test_tiff_predictor_basic() {
        // Single color channel, 4 columns
        // Input (differenced): [10, 5, 3, 2]
        // Output: [10, 15, 18, 20]
        let data = vec![10, 5, 3, 2];
        let params = DecodeParams {
            predictor: 2,
            columns: 4,
            colors: 1,
            bits_per_component: 8,
            ..DecodeParams::default()
        };
        let result = apply_tiff_predictor(&data, &params).unwrap();
        assert_eq!(result, &[10, 15, 18, 20]);
    }

    #[test]
    fn test_tiff_predictor_multi_color() {
        // 2 color channels, 2 columns -> bytes_per_row = 4
        // Input: [R0, G0, dR1, dG1] = [10, 20, 5, 3]
        // Output: [10, 20, 15, 23]
        let data = vec![10, 20, 5, 3];
        let params = DecodeParams {
            predictor: 2,
            columns: 2,
            colors: 2,
            bits_per_component: 8,
            ..DecodeParams::default()
        };
        let result = apply_tiff_predictor(&data, &params).unwrap();
        assert_eq!(result, &[10, 20, 15, 23]);
    }

    #[test]
    fn test_tiff_predictor_multi_row() {
        // Each row is independent
        let data = vec![10, 5, 3, 20, 2, 1];
        let params = DecodeParams {
            predictor: 2,
            columns: 3,
            colors: 1,
            bits_per_component: 8,
            ..DecodeParams::default()
        };
        let result = apply_tiff_predictor(&data, &params).unwrap();
        assert_eq!(result, &[10, 15, 18, 20, 22, 23]);
    }

    #[test]
    fn test_lzw_basic() {
        // Manually construct a simple LZW stream
        // Clear(256) + literal bytes + EOD(257)
        let mut writer = BitWriter::new();
        writer.write_bits(LZW_CLEAR, 9); // clear
        writer.write_bits(b'A' as u16, 9); // literal 'A'
        writer.write_bits(b'B' as u16, 9); // literal 'B'
        writer.write_bits(b'A' as u16, 9); // literal 'A' (adds "AB" as 258)
        writer.write_bits(258, 9); // code 258 = "AB"
        writer.write_bits(LZW_EOD, 9); // end

        let input = writer.finish();
        let params = DecodeParams::default();
        let result = decode_lzw(&input, &params, &default_limits(), null_diag(), 0).unwrap();
        assert_eq!(result, b"ABAAB");
    }

    #[test]
    fn test_lzw_clear_resets_table() {
        let mut writer = BitWriter::new();
        writer.write_bits(LZW_CLEAR, 9);
        writer.write_bits(b'X' as u16, 9);
        writer.write_bits(LZW_CLEAR, 9); // reset
        writer.write_bits(b'Y' as u16, 9);
        writer.write_bits(LZW_EOD, 9);

        let input = writer.finish();
        let params = DecodeParams::default();
        let result = decode_lzw(&input, &params, &default_limits(), null_diag(), 0).unwrap();
        assert_eq!(result, b"XY");
    }

    #[test]
    fn test_lzw_bomb_detection() {
        // Build an LZW stream that decodes to lots of data
        let mut writer = BitWriter::new();
        writer.write_bits(LZW_CLEAR, 9);
        for _ in 0..200 {
            writer.write_bits(b'A' as u16, 9);
        }
        writer.write_bits(LZW_EOD, 9);

        let input = writer.finish();
        let tiny_limit = DecodeLimits {
            max_decompressed_size: 50,
            ..DecodeLimits::default()
        };
        let params = DecodeParams::default();
        let err = decode_lzw(&input, &params, &tiny_limit, null_diag(), 0).unwrap_err();
        assert!(matches!(
            err,
            Error::ResourceLimit(ResourceLimitError {
                limit: Limit::DecompressedSize(50),
                ..
            })
        ));
    }

    // -- decode_stream integration --

    #[test]
    fn test_decode_stream_no_filter() {
        let dict = PdfDictionary::new();
        let result = decode_stream(b"raw data", &dict, &default_limits(), null_diag(), 0).unwrap();
        assert_eq!(result, b"raw data");
    }

    #[test]
    fn test_decode_stream_ascii_hex() {
        let mut dict = PdfDictionary::new();
        dict.insert(
            b"Filter".to_vec(),
            PdfObject::Name(b"ASCIIHexDecode".to_vec()),
        );
        let result =
            decode_stream(b"48656C6C6F>", &dict, &default_limits(), null_diag(), 0).unwrap();
        assert_eq!(result, b"Hello");
    }

    #[test]
    fn test_decode_stream_flate() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let original = b"test flate stream decoding";
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(original).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"FlateDecode".to_vec()));
        let result = decode_stream(&compressed, &dict, &default_limits(), null_diag(), 0).unwrap();
        assert_eq!(result, original);
    }

    #[test]
    fn test_decode_stream_filter_chain() {
        // ASCIIHexDecode wrapping FlateDecode
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let original = b"chained filter test";
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(original).unwrap();
        let compressed = encoder.finish().unwrap();

        // Hex-encode the compressed data
        let hex_encoded: String = compressed.iter().map(|b| format!("{:02X}", b)).collect();
        let hex_with_eod = format!("{}>", hex_encoded);

        let mut dict = PdfDictionary::new();
        dict.insert(
            b"Filter".to_vec(),
            PdfObject::Array(vec![
                PdfObject::Name(b"ASCIIHexDecode".to_vec()),
                PdfObject::Name(b"FlateDecode".to_vec()),
            ]),
        );
        let result = decode_stream(
            hex_with_eod.as_bytes(),
            &dict,
            &default_limits(),
            null_diag(),
            0,
        )
        .unwrap();
        assert_eq!(result, original);
    }

    #[test]
    fn test_decode_stream_unsupported_filter() {
        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Crypt".to_vec()));
        let err = decode_stream(b"data", &dict, &default_limits(), null_diag(), 0).unwrap_err();
        assert!(err.to_string().contains("unsupported stream filter"));
    }

    #[test]
    fn test_decode_stream_flate_with_predictor() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        // Build PNG-predicted data: 2 rows, 3 columns, filter=None(0)
        let predicted = vec![
            0, 10, 20, 30, // row 0: None
            0, 40, 50, 60, // row 1: None
        ];

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&predicted).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut dp = PdfDictionary::new();
        dp.insert(b"Predictor".to_vec(), PdfObject::Integer(12));
        dp.insert(b"Columns".to_vec(), PdfObject::Integer(3));

        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"FlateDecode".to_vec()));
        dict.insert(b"DecodeParms".to_vec(), PdfObject::Dictionary(dp));

        let result = decode_stream(&compressed, &dict, &default_limits(), null_diag(), 0).unwrap();
        assert_eq!(result, &[10, 20, 30, 40, 50, 60]);
    }

    #[test]
    fn test_filter_abbreviations() {
        // /Fl is abbreviation for FlateDecode
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let original = b"abbreviation test";
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(original).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"Fl".to_vec()));
        let result = decode_stream(&compressed, &dict, &default_limits(), null_diag(), 0).unwrap();
        assert_eq!(result, original);
    }

    #[test]
    fn test_ascii_hex_abbreviation() {
        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"AHx".to_vec()));
        let result =
            decode_stream(b"48656C6C6F>", &dict, &default_limits(), null_diag(), 0).unwrap();
        assert_eq!(result, b"Hello");
    }

    // -- Paeth predictor unit --

    #[test]
    fn test_paeth_predictor_fn() {
        assert_eq!(paeth_predictor(0, 0, 0), 0);
        assert_eq!(paeth_predictor(10, 20, 10), 20); // p=20, pa=10, pb=0, pc=10 -> b
        assert_eq!(paeth_predictor(10, 0, 0), 10); // p=10, pa=0 -> a
    }

    #[test]
    fn test_run_length_literal() {
        // Length byte 4 = copy next 5 bytes
        let input = [4, b'H', b'e', b'l', b'l', b'o', 128];
        let result = decode_run_length(&input, &default_limits(), null_diag(), 0).unwrap();
        assert_eq!(result, b"Hello");
    }

    #[test]
    fn test_run_length_repeat() {
        // Length byte 254 = repeat next byte 257-254=3 times
        let input = [254, b'A', 128];
        let result = decode_run_length(&input, &default_limits(), null_diag(), 0).unwrap();
        assert_eq!(result, b"AAA");
    }

    #[test]
    fn test_run_length_mixed() {
        // Literal "Hi" (length=1, copy 2 bytes) + repeat 'X' 4 times (253)
        let input = [1, b'H', b'i', 253, b'X', 128];
        let result = decode_run_length(&input, &default_limits(), null_diag(), 0).unwrap();
        assert_eq!(result, b"HiXXXX");
    }

    #[test]
    fn test_run_length_eod() {
        // EOD marker stops decoding, trailing data ignored
        let input = [0, b'A', 128, 0, b'B'];
        let result = decode_run_length(&input, &default_limits(), null_diag(), 0).unwrap();
        assert_eq!(result, b"A");
    }

    #[test]
    fn test_run_length_empty() {
        let input = [128];
        let result = decode_run_length(&input, &default_limits(), null_diag(), 0).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_run_length_truncated_literal() {
        let diag = CollectingDiagnostics::new();
        // Says copy 5 bytes but only 2 available
        let input = [4, b'A', b'B'];
        let result = decode_run_length(&input, &default_limits(), &diag, 0).unwrap();
        assert_eq!(result, b"AB");
        assert!(diag
            .warnings()
            .iter()
            .any(|w| w.message.contains("truncated literal")));
    }

    #[test]
    fn test_run_length_truncated_repeat() {
        let diag = CollectingDiagnostics::new();
        // Repeat run but no data byte follows
        let input = [254];
        let result = decode_run_length(&input, &default_limits(), &diag, 0).unwrap();
        assert!(result.is_empty());
        assert!(diag
            .warnings()
            .iter()
            .any(|w| w.message.contains("truncated repeat")));
    }

    #[test]
    fn test_run_length_bomb_detection() {
        // Max repeat: length=129 means 257-129=128 copies per code
        // 10 such codes = 1280 bytes output
        let mut input = Vec::new();
        for _ in 0..10 {
            input.push(129); // repeat 128 times
            input.push(b'Z');
        }
        input.push(128); // EOD

        let tiny_limit = DecodeLimits {
            max_decompressed_size: 100,
            ..DecodeLimits::default()
        };
        let err = decode_run_length(&input, &tiny_limit, null_diag(), 0).unwrap_err();
        assert!(matches!(
            err,
            Error::ResourceLimit(ResourceLimitError {
                limit: Limit::DecompressedSize(100),
                ..
            })
        ));
    }

    // -- Image filter passthrough --

    #[test]
    fn test_image_filter_passthrough() {
        let diag = CollectingDiagnostics::new();
        let raw = b"fake jpeg data";
        let params = DecodeParams::default();

        for filter in &[
            b"DCTDecode".as_slice(),
            b"DCT",
            b"JPXDecode",
            b"JBIG2Decode",
        ] {
            let result = apply_filter(filter, raw, &params, &default_limits(), &diag, 0).unwrap();
            assert_eq!(
                result,
                raw,
                "passthrough failed for {}",
                String::from_utf8_lossy(filter)
            );
        }

        // Should have warned for each filter (CCITTFaxDecode is now decoded, not passed through)
        assert_eq!(diag.warnings().len(), 4);
        assert!(diag.warnings()[0].message.contains("not decoded"));
    }

    #[test]
    fn test_decode_stream_image_filter_passthrough() {
        let diag = CollectingDiagnostics::new();
        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"DCTDecode".to_vec()));
        let raw = b"jpeg content";
        let result = decode_stream(raw, &dict, &default_limits(), &diag, 0).unwrap();
        assert_eq!(result, raw);
        assert!(!diag.warnings().is_empty());
    }

    // -- data_offset propagation --

    #[test]
    fn test_warnings_carry_data_offset() {
        let diag = CollectingDiagnostics::new();
        let offset: u64 = 42_000;

        // ASCIIHexDecode with invalid byte
        decode_ascii_hex(b"ZZ>", &diag, offset);
        assert_eq!(diag.warnings()[0].offset, Some(offset));

        // Image filter passthrough through decode_stream
        let diag2 = CollectingDiagnostics::new();
        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"DCTDecode".to_vec()));
        decode_stream(b"img", &dict, &default_limits(), &diag2, offset).unwrap();
        assert_eq!(diag2.warnings()[0].offset, Some(offset));

        // FlateDecode raw deflate fallback (zlib header fails)
        let diag3 = CollectingDiagnostics::new();
        {
            use flate2::write::DeflateEncoder;
            use flate2::Compression;
            use std::io::Write;

            let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
            enc.write_all(b"x").unwrap();
            let raw_deflate = enc.finish().unwrap();
            let params = DecodeParams::default();
            decode_flate(&raw_deflate, &params, &default_limits(), &diag3, offset).unwrap();
            assert_eq!(diag3.warnings()[0].offset, Some(offset));
        }

        // RunLengthDecode truncated
        let diag4 = CollectingDiagnostics::new();
        decode_run_length(&[4, b'A'], &default_limits(), &diag4, offset).unwrap();
        assert_eq!(diag4.warnings()[0].offset, Some(offset));
    }

    // -- Security limit tests --

    #[test]
    fn test_filter_chain_depth_limit() {
        use std::sync::Arc;
        let diag = Arc::new(CollectingDiagnostics::new());
        // Build a dict with 20 ASCIIHexDecode filters (exceeds MAX_FILTER_CHAIN_DEPTH=16)
        let mut filters = Vec::new();
        for _ in 0..20 {
            filters.push(PdfObject::Name(b"ASCIIHexDecode".to_vec()));
        }
        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Array(filters));

        // Hex-encode 16 times deep (each pass wraps in hex)
        let mut data = b"41>".to_vec(); // "A" in hex
        for _ in 1..16 {
            // Re-encode each byte as two hex digits + ">"
            let hex: String = data.iter().map(|b| format!("{:02X}", b)).collect();
            data = format!("{}>", hex).into_bytes();
        }

        let result = decode_stream(&data, &dict, &default_limits(), &*diag, 0);
        // Should warn about truncation
        assert!(diag
            .warnings()
            .iter()
            .any(|w| w.message.contains("filter chain depth")));
        // Should still produce some result (truncated chain, not error)
        assert!(result.is_ok());
    }

    #[test]
    fn test_predictor_columns_too_large() {
        let params = DecodeParams {
            predictor: 10,
            columns: 200_000,
            colors: 1,
            bits_per_component: 8,
            early_change: 1,
            k: 0,
            rows: 0,
            black_is_1: false,
            jbig2_globals: None,
        };
        let err = apply_png_predictor(b"", &params).unwrap_err();
        assert!(format!("{}", err).contains("out of range"));
    }

    #[test]
    fn test_predictor_colors_too_large() {
        let params = DecodeParams {
            predictor: 10,
            columns: 1,
            colors: 500,
            bits_per_component: 8,
            early_change: 1,
            k: 0,
            rows: 0,
            black_is_1: false,
            jbig2_globals: None,
        };
        let err = apply_png_predictor(b"", &params).unwrap_err();
        assert!(format!("{}", err).contains("out of range"));
    }

    #[test]
    fn test_predictor_negative_params() {
        let params = DecodeParams {
            predictor: 10,
            columns: -1,
            colors: 1,
            bits_per_component: 8,
            early_change: 1,
            k: 0,
            rows: 0,
            black_is_1: false,
            jbig2_globals: None,
        };
        let err = apply_png_predictor(b"", &params).unwrap_err();
        assert!(format!("{}", err).contains("negative"));
    }

    #[test]
    fn test_tiff_predictor_columns_too_large() {
        let params = DecodeParams {
            predictor: 2,
            columns: 200_000,
            colors: 1,
            bits_per_component: 8,
            early_change: 1,
            k: 0,
            rows: 0,
            black_is_1: false,
            jbig2_globals: None,
        };
        let err = apply_tiff_predictor(b"", &params).unwrap_err();
        assert!(format!("{}", err).contains("out of range"));
    }

    // -- DecodeParams::from_dict --

    #[test]
    fn test_decode_params_from_dict_defaults() {
        let dict = PdfDictionary::new();
        let params = DecodeParams::from_dict(&dict);
        assert_eq!(params.predictor, 1);
        assert_eq!(params.colors, 1);
        assert_eq!(params.bits_per_component, 8);
        assert_eq!(params.columns, 1);
        assert_eq!(params.early_change, 1);
    }

    #[test]
    fn test_decode_params_from_dict_with_values() {
        let mut dict = PdfDictionary::new();
        dict.insert(b"Colors".to_vec(), PdfObject::Integer(3));
        dict.insert(b"BitsPerComponent".to_vec(), PdfObject::Integer(4));
        dict.insert(b"EarlyChange".to_vec(), PdfObject::Integer(0));
        let params = DecodeParams::from_dict(&dict);
        assert_eq!(params.colors, 3);
        assert_eq!(params.bits_per_component, 4);
        assert_eq!(params.early_change, 0);
    }

    // -- extract_decode_params array --

    #[test]
    fn test_extract_decode_params_array() {
        let mut dp1 = PdfDictionary::new();
        dp1.insert(b"Predictor".to_vec(), PdfObject::Integer(12));
        let mut dp2 = PdfDictionary::new();
        dp2.insert(b"Columns".to_vec(), PdfObject::Integer(8));

        let mut dict = PdfDictionary::new();
        dict.insert(
            b"DecodeParms".to_vec(),
            PdfObject::Array(vec![PdfObject::Dictionary(dp1), PdfObject::Dictionary(dp2)]),
        );

        let params = extract_decode_params(&dict, 2);
        assert_eq!(params[0].predictor, 12);
        assert_eq!(params[1].columns, 8);
    }

    #[test]
    fn test_extract_decode_params_array_with_null() {
        let mut dp = PdfDictionary::new();
        dp.insert(b"Predictor".to_vec(), PdfObject::Integer(12));

        let mut dict = PdfDictionary::new();
        dict.insert(
            b"DecodeParms".to_vec(),
            PdfObject::Array(vec![PdfObject::Dictionary(dp), PdfObject::Null]),
        );

        let params = extract_decode_params(&dict, 2);
        assert_eq!(params[0].predictor, 12);
        assert_eq!(params[1].predictor, 1); // default
    }

    // -- LZW edge cases --

    #[test]
    fn test_lzw_kwkwk_case() {
        // Trigger KwKwK: emit code == table.len() with a prev entry.
        // After clear: table has 258 entries (0-255 + clear + EOD).
        // Emit 'A' (65): prev=A
        // Emit 'B' (66): output B, add table[258]="AB", prev=B
        // Emit 258: output "AB", add table[259]="BA", prev="AB"
        // Emit 260 == table.len(): KwKwK case. prev="AB", first='A', so entry="ABA"
        let mut w = BitWriter::new();
        w.write_bits(LZW_CLEAR, 9);
        w.write_bits(b'A' as u16, 9);
        w.write_bits(b'B' as u16, 9);
        w.write_bits(258, 9);
        w.write_bits(260, 9); // KwKwK: prev="AB", emit "ABA"
        w.write_bits(LZW_EOD, 9);
        let data = w.finish();

        let params = DecodeParams::default();
        let result = decode_lzw(&data, &params, &default_limits(), null_diag(), 0).unwrap();
        // A + B + AB + ABA = "ABABABA"
        assert_eq!(result, b"ABABABA");
    }

    #[test]
    fn test_lzw_out_of_range_code() {
        // Emit a code that is beyond the table
        let mut w = BitWriter::new();
        w.write_bits(LZW_CLEAR, 9);
        w.write_bits(b'X' as u16, 9);
        w.write_bits(500, 9); // way beyond table size
        let data = w.finish();

        let diag = CollectingDiagnostics::new();
        let params = DecodeParams::default();
        let result = decode_lzw(&data, &params, &default_limits(), &diag, 0).unwrap();
        assert_eq!(result, b"X");
        assert!(diag.warnings()[0].message.contains("out of range"));
    }

    #[test]
    fn test_lzw_kwkwk_no_prev() {
        // KwKwK case with no previous entry
        let mut w = BitWriter::new();
        w.write_bits(LZW_CLEAR, 9);
        w.write_bits(258, 9); // code == table.len() but no prev
        let data = w.finish();

        let diag = CollectingDiagnostics::new();
        let params = DecodeParams::default();
        let _ = decode_lzw(&data, &params, &default_limits(), &diag, 0).unwrap();
        assert!(diag.warnings()[0].message.contains("no previous entry"));
    }

    #[test]
    fn test_lzw_no_early_change() {
        let mut w = BitWriter::new();
        w.write_bits(LZW_CLEAR, 9);
        w.write_bits(b'A' as u16, 9);
        w.write_bits(b'B' as u16, 9);
        w.write_bits(LZW_EOD, 9);
        let data = w.finish();

        let params = DecodeParams {
            early_change: 0,
            k: 0,
            rows: 0,
            black_is_1: false,
            jbig2_globals: None,
            ..DecodeParams::default()
        };
        let result = decode_lzw(&data, &params, &default_limits(), null_diag(), 0).unwrap();
        assert_eq!(result, b"AB");
    }

    #[test]
    fn test_lzw_with_predictor() {
        // LZW output + predictor=10 (PNG None): each row has filter byte 0
        let mut w = BitWriter::new();
        w.write_bits(LZW_CLEAR, 9);
        // Row: filter=0, data=10, 20
        w.write_bits(0, 9); // filter byte
        w.write_bits(10, 9);
        w.write_bits(20, 9);
        w.write_bits(LZW_EOD, 9);
        let data = w.finish();

        let params = DecodeParams {
            predictor: 10,
            columns: 2,
            colors: 1,
            bits_per_component: 8,
            early_change: 1,
            k: 0,
            rows: 0,
            black_is_1: false,
            jbig2_globals: None,
        };
        let result = decode_lzw(&data, &params, &default_limits(), null_diag(), 0).unwrap();
        assert_eq!(result, &[10, 20]);
    }

    #[test]
    fn test_lzw_size_limit() {
        // Emit many bytes, then check limit
        let mut w = BitWriter::new();
        w.write_bits(LZW_CLEAR, 9);
        for _ in 0..20 {
            w.write_bits(b'A' as u16, 9);
        }
        w.write_bits(LZW_EOD, 9);
        let data = w.finish();

        let tiny_limit = DecodeLimits {
            max_decompressed_size: 5,
            ..DecodeLimits::default()
        };
        let params = DecodeParams::default();
        let result = decode_lzw(&data, &params, &tiny_limit, null_diag(), 0);
        assert!(result.is_err());
    }

    // -- ASCII85 edge cases --

    #[test]
    fn test_ascii85_invalid_byte() {
        let diag = CollectingDiagnostics::new();
        let input = b"\x01~>"; // invalid byte 0x01
        let result = decode_ascii85(input, &diag, 0).unwrap();
        assert!(result.is_empty());
        assert!(diag.warnings()[0].message.contains("invalid byte"));
    }

    #[test]
    fn test_ascii85_z_in_middle_of_group() {
        let diag = CollectingDiagnostics::new();
        let input = b"!!z~>"; // two chars then z
        let result = decode_ascii85(input, &diag, 0).unwrap();
        // Should flush partial group, then emit 4 zeros
        assert!(!result.is_empty());
        assert!(!diag.warnings().is_empty());
    }

    #[test]
    fn test_ascii85_single_trailing_char() {
        let diag = CollectingDiagnostics::new();
        let input = b"!~>"; // single trailing char (invalid)
        let result = decode_ascii85(input, &diag, 0).unwrap();
        assert!(result.is_empty());
        assert!(diag.warnings()[0].message.contains("single trailing"));
    }

    #[test]
    fn test_ascii85_group_overflow() {
        let diag = CollectingDiagnostics::new();
        // Construct a group that overflows u32 (all max-value digits)
        // 's' is the max (84). "sssss" = 84 * (85^4 + 85^3 + 85^2 + 85 + 1) > u32::MAX
        let input = b"sssss~>";
        let result = decode_ascii85(input, &diag, 0).unwrap();
        // Overflows u32, so group should be skipped with warning
        assert!(result.is_empty());
        assert!(diag.warnings()[0].message.contains("overflow"));
    }

    // -- Predictor edge cases --

    #[test]
    fn test_unsupported_predictor() {
        let params = DecodeParams {
            predictor: 99,
            ..DecodeParams::default()
        };
        let result = apply_predictor(b"data", &params);
        assert!(result.is_err());
    }

    #[test]
    fn test_png_predictor_unknown_filter_type() {
        // Unknown filter type 99 treated as None
        let data = vec![99, 10, 20, 30];
        let params = DecodeParams {
            predictor: 10,
            columns: 3,
            colors: 1,
            bits_per_component: 8,
            ..DecodeParams::default()
        };
        let result = apply_png_predictor(&data, &params).unwrap();
        assert_eq!(result, &[10, 20, 30]);
    }

    #[test]
    fn test_tiff_predictor_non_8bpc() {
        let params = DecodeParams {
            predictor: 2,
            columns: 4,
            colors: 1,
            bits_per_component: 16,
            ..DecodeParams::default()
        };
        let result = apply_tiff_predictor(b"data", &params);
        assert!(result.is_err());
        assert!(format!("{}", result.unwrap_err()).contains("16 bits"));
    }

    #[test]
    fn test_tiff_predictor_negative_params() {
        let params = DecodeParams {
            predictor: 2,
            columns: -1,
            colors: 1,
            bits_per_component: 8,
            ..DecodeParams::default()
        };
        let err = apply_tiff_predictor(b"data", &params).unwrap_err();
        assert!(format!("{}", err).contains("negative"));
    }

    #[test]
    fn test_tiff_predictor_empty_row() {
        let params = DecodeParams {
            predictor: 2,
            columns: 0,
            colors: 1,
            bits_per_component: 8,
            ..DecodeParams::default()
        };
        let result = apply_tiff_predictor(b"", &params).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_png_predictor_empty_row() {
        let params = DecodeParams {
            predictor: 10,
            columns: 0,
            colors: 1,
            bits_per_component: 8,
            ..DecodeParams::default()
        };
        let result = apply_png_predictor(b"", &params).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_png_predictor_negative_params() {
        let params = DecodeParams {
            predictor: 10,
            columns: -1,
            colors: 1,
            bits_per_component: 8,
            ..DecodeParams::default()
        };
        let result = apply_png_predictor(b"data", &params);
        assert!(result.is_err());
    }

    // -- decode_stream integration --

    #[test]
    fn test_filter_chain_depth_limit_truncation_warns() {
        let diag = CollectingDiagnostics::new();
        // 17 filters = over the limit of 16
        let filters: Vec<PdfObject> = (0..17)
            .map(|_| PdfObject::Name(b"ASCIIHexDecode".to_vec()))
            .collect();
        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Array(filters));

        // It will fail because chaining 16 ASCIIHex on non-hex data is garbage,
        // but the truncation warning should still fire.
        let _ = decode_stream(b"48>", &dict, &default_limits(), &diag, 0);
        assert!(diag
            .warnings()
            .iter()
            .any(|w| w.message.contains("truncating")));
    }

    // -- BitWriter helper for LZW tests --

    struct BitWriter {
        data: Vec<u8>,
        current_byte: u8,
        bit_pos: u8,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                data: Vec::new(),
                current_byte: 0,
                bit_pos: 0,
            }
        }

        fn write_bits(&mut self, value: u16, count: u8) {
            for i in (0..count).rev() {
                let bit = (value >> i) & 1;
                self.current_byte |= (bit as u8) << (7 - self.bit_pos);
                self.bit_pos += 1;
                if self.bit_pos == 8 {
                    self.data.push(self.current_byte);
                    self.current_byte = 0;
                    self.bit_pos = 0;
                }
            }
        }

        fn finish(mut self) -> Vec<u8> {
            if self.bit_pos > 0 {
                self.data.push(self.current_byte);
            }
            self.data
        }
    }

    // -- DecodeLimits defaults --

    #[test]
    fn test_decode_limits_default_values() {
        let limits = DecodeLimits::default();
        assert_eq!(limits.max_decompressed_size, 250 * 1024 * 1024);
        assert_eq!(limits.max_decompression_ratio, 100);
        assert_eq!(limits.ratio_floor_size, 10 * 1024 * 1024);
    }

    // -- Decompression limit: 200MB succeeds (above old 100MB, below new 250MB) --

    #[test]
    fn test_200mb_decompressed_succeeds() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        // We can't actually allocate 200MB in a unit test. Instead, use
        // limits that mirror the same relationship: old=10, new=25,
        // decompressed output=20 (above old, below new).
        let original = vec![0u8; 20];
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&original).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"FlateDecode".to_vec()));

        // Old limit would have been 10, new is 25. Output is 20 => should succeed.
        let limits = DecodeLimits {
            max_decompressed_size: 25,
            max_decompression_ratio: 100,
            ratio_floor_size: 10 * 1024 * 1024, // high floor so ratio check doesn't fire
        };
        let result = decode_stream(&compressed, &dict, &limits, null_diag(), 0);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 20);
    }

    // -- Decompression limit: exceeding absolute limit fails --

    #[test]
    fn test_exceeding_absolute_limit_fails() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let original = vec![0u8; 300];
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&original).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"FlateDecode".to_vec()));

        let limits = DecodeLimits {
            max_decompressed_size: 250,
            max_decompression_ratio: 1000, // high so ratio doesn't fire
            ratio_floor_size: 10 * 1024 * 1024,
        };
        let err = decode_stream(&compressed, &dict, &limits, null_diag(), 0).unwrap_err();
        assert!(matches!(
            err,
            Error::ResourceLimit(ResourceLimitError {
                limit: Limit::DecompressedSize(250),
                ..
            })
        ));
    }

    // -- Ratio guard: high ratio over floor triggers error --

    #[test]
    fn test_ratio_guard_rejects_high_ratio_over_floor() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        // Create data with a high compression ratio (zeros compress extremely well).
        let original = vec![0u8; 2000];
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&original).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"FlateDecode".to_vec()));

        // Set floor low enough that 2000 bytes exceeds it, and ratio limit
        // low enough that the ~100:1 ratio trips it.
        let limits = DecodeLimits {
            max_decompressed_size: 1_000_000, // absolute limit won't fire
            max_decompression_ratio: 10,      // 10:1 max
            ratio_floor_size: 100,            // floor at 100 bytes
        };
        let err = decode_stream(&compressed, &dict, &limits, null_diag(), 0).unwrap_err();
        assert!(
            matches!(
                err,
                Error::ResourceLimit(ResourceLimitError {
                    limit: Limit::DecompressionRatio { .. },
                    ..
                })
            ),
            "expected DecompressionRatio, got: {err}"
        );
    }

    // -- Ratio guard: small stream with high ratio passes (below floor) --

    #[test]
    fn test_small_stream_high_ratio_passes_below_floor() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        // 1KB of zeros compresses to a few bytes => very high ratio.
        let original = vec![0u8; 1024];
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&original).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut dict = PdfDictionary::new();
        dict.insert(b"Filter".to_vec(), PdfObject::Name(b"FlateDecode".to_vec()));

        // Floor is 10MB, so 1KB output won't trigger ratio check even though
        // the ratio is ~200:1 which exceeds the 10:1 limit.
        let limits = DecodeLimits {
            max_decompressed_size: 1_000_000,
            max_decompression_ratio: 10,
            ratio_floor_size: 10 * 1024 * 1024, // 10MB floor
        };
        let result = decode_stream(&compressed, &dict, &limits, null_diag(), 0)
            .expect("small stream should pass below ratio floor");
        assert_eq!(result.len(), 1024);
    }
}
