//! Property test: CodepageDecoder symmetry.
//!
//! Hosted in udoc-rtf (this crate is the codepage decoder's primary user)
//! but exercises the canonical implementation in `udoc_core::codepage`.
//!
//! Property: for any codepage in the supported set, decoding via
//! `CodepageDecoder` is a *left-inverse* of `encoding_rs::Encoding::encode`,
//! i.e., encode -> decode -> encode produces the same bytes as the first
//! encode. (Plain "encode -> decode == identity" is invalid for some
//! codepages: SHIFT_JIS maps both U+00A5 and U+005C to 0x5C, so the round
//! trip collapses them. See the  for the failure trace.)
//!
//! `encoding_rs::Encoding::encode` returns a `Cow<[u8]>` plus the encoding
//! it actually used (some encodings switch to UTF-8 fallback for
//! unrepresentable input) plus a "had_unmappable_characters" flag. We
//! discard cases where any of those signal an inexact encoding so the
//! property remains well-defined. We also filter inputs starting with
//! U+FEFF because `encoding_rs` BOM-strips on decode.
//!
//! Budget: 256 cases per property, capped via `ProptestConfig`.

use encoding_rs::Encoding;
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

use udoc_core::codepage::{encoding_for_codepage, CodepageDecoder};

/// Codepages we test. Mirrors the supported set in `encoding_for_codepage`.
/// CP437 / CP850 are excluded because they are documented approximations
/// (`is_approximate_codepage`) and the symmetry property does not hold.
const CODEPAGES: &[u16] = &[
    874,   // WINDOWS_874 (Thai)
    932,   // SHIFT_JIS
    936,   // GBK
    949,   // EUC_KR
    950,   // BIG5
    1250,  // WINDOWS_1250 (Central European)
    1251,  // WINDOWS_1251 (Cyrillic)
    1252,  // WINDOWS_1252 (Western)
    1253,  // WINDOWS_1253 (Greek)
    1254,  // WINDOWS_1254 (Turkish)
    1255,  // WINDOWS_1255 (Hebrew)
    1256,  // WINDOWS_1256 (Arabic)
    1257,  // WINDOWS_1257 (Baltic)
    1258,  // WINDOWS_1258 (Vietnamese)
    10000, // MACINTOSH
    65001, // UTF-8
];

/// Strategy for picking one of the supported codepages.
fn codepage_strategy() -> impl Strategy<Value = u16> {
    proptest::sample::select(CODEPAGES.to_vec())
}

/// Strategy for arbitrary Unicode strings of length 0..32. We use the full
/// `any::<char>()` strategy so the test exercises BMP, supplementary planes,
/// and the surrogate-free subset proptest produces.
fn text_strategy() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..32).prop_map(|v| v.into_iter().collect())
}

/// Encode `text` via the given encoding. Returns `None` if the encoder used
/// substitution / fallback (i.e., the input is not exactly representable).
fn try_encode(text: &str, enc: &'static Encoding) -> Option<Vec<u8>> {
    // encoding_rs's `Encoding::decode` performs BOM-sniffing: a leading
    // U+FEFF in the byte stream gets stripped during decode. That breaks
    // the encode -> decode identity for any text starting with U+FEFF.
    // Filter such inputs at the strategy boundary so the property remains
    // well-defined.
    if text.starts_with('\u{feff}') {
        return None;
    }
    let (bytes, used, had_unmappable) = enc.encode(text);
    // `used` may differ from `enc` if encoding_rs picked a different encoding
    // (e.g., it returns UTF-8 for ISO-2022-JP fallback). We require an exact
    // match so the test invariant holds.
    if used != enc || had_unmappable {
        None
    } else {
        Some(bytes.into_owned())
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        max_shrink_iters: 4096,
        .. ProptestConfig::default()
    })]

    /// Decode is a left-inverse of encode: re-encoding the decoded string
    /// reproduces the original bytes.
    ///
    /// We deliberately do NOT assert `text == decode(encode(text))` because
    /// many legacy codepages collapse multiple Unicode codepoints onto the
    /// same byte (SHIFT_JIS U+00A5 and U+005C both -> 0x5C). The looser
    /// "encode -> decode -> encode is idempotent" property is what
    /// downstream RTF / DOC / XLS round-tripping actually relies on.
    #[test]
    fn encode_decode_encode_is_idempotent(
        cpg in codepage_strategy(),
        text in text_strategy(),
    ) {
        let enc = encoding_for_codepage(cpg);
        let Some(bytes) = try_encode(&text, enc) else {
            // Input is not exactly representable in this codepage; the
            // property is not defined here. Skip.
            return Ok(());
        };

        let mut dec = CodepageDecoder::new(enc);
        for b in &bytes {
            dec.push_byte(*b);
        }
        let decoded = dec.flush();

        // Re-encode and compare bytes.
        let Some(reencoded) = try_encode(&decoded, enc) else {
            // The decoded string lost the "exactly representable" guarantee
            // (e.g., it now contains a BOM the encoder won't reproduce).
            // Skip - the original property would be malformed here.
            return Ok(());
        };

        prop_assert_eq!(bytes, reencoded);
    }

    /// Encoding lookup is total: every codepage in the supported set returns
    /// a non-null `Encoding` and the same value on repeated calls. (Cheap
    /// invariant but it's how RTF / DOC / XLS rely on the function.)
    #[test]
    fn encoding_lookup_is_stable(cpg in codepage_strategy()) {
        let a = encoding_for_codepage(cpg);
        let b = encoding_for_codepage(cpg);
        prop_assert!(std::ptr::eq(a, b));
    }

    /// `flush()` after decoding is empty: the decoder fully consumes its
    /// internal buffer on every flush. (Catches "leftover trail byte"
    /// regressions in CJK paths.)
    #[test]
    fn flush_clears_buffer(
        cpg in codepage_strategy(),
        bytes in prop::collection::vec(any::<u8>(), 0..32),
    ) {
        let enc = encoding_for_codepage(cpg);
        let mut dec = CodepageDecoder::new(enc);
        for b in &bytes {
            dec.push_byte(*b);
        }
        let _ = dec.flush();
        prop_assert!(dec.is_empty());
        // A second flush on an empty buffer must produce an empty string.
        prop_assert_eq!(dec.flush(), "");
    }
}
