//! Property test: PDF lexer + object parser round-trip.
//!
//! Generates well-formed `PdfObject` trees, serialises them with a minimal
//! canonical writer, parses the bytes through `Lexer` + `ObjectParser`, then
//! re-serialises the parsed object. The two byte strings must be identical.
//!
//! "Modulo whitespace normalisation" is achieved by always emitting via the
//! same canonical writer: any extra whitespace the lexer would tolerate gets
//! collapsed when we re-emit, so the second pass is byte-equal to the first.
//!
//! Streams and indirect references are excluded from the strategy: the
//! parser resolves streams against the surrounding document body and
//! references are not stand-alone parseable objects (they require an xref).
//! Both have dedicated unit/integration coverage elsewhere; this property
//! exists to flag round-trip regressions in scalar / array / dict handling.
//!
//! Budget: 256 cases per property, capped via `ProptestConfig`.

use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

use udoc_pdf::object::{PdfDictionary, PdfObject, PdfString};
use udoc_pdf::parse::object_parser::ObjectParser;

// ---------------------------------------------------------------------------
// Canonical writer
// ---------------------------------------------------------------------------

/// Serialise a `PdfObject` to canonical PDF syntax that the lexer + parser
/// will reconstruct losslessly. Strings always go out as hex (sidesteps
/// literal-string escaping bugs); names always use `#XX` for any byte that
/// is not "name-safe"; reals use a fixed `{:.6}` format the parser accepts.
fn write_object(buf: &mut Vec<u8>, obj: &PdfObject) {
    match obj {
        PdfObject::Null => buf.extend_from_slice(b"null"),
        PdfObject::Boolean(true) => buf.extend_from_slice(b"true"),
        PdfObject::Boolean(false) => buf.extend_from_slice(b"false"),
        PdfObject::Integer(n) => buf.extend_from_slice(n.to_string().as_bytes()),
        PdfObject::Real(n) => {
            // Match the canonical print used elsewhere in this codebase: a
            // fixed-precision decimal that the lexer always parses back as a
            // Real (not an Integer) and that round-trips bit-exactly through
            // the strategy's quantised inputs.
            buf.extend_from_slice(format!("{:.6}", n).as_bytes());
        }
        PdfObject::Name(bytes) => {
            buf.push(b'/');
            for &b in bytes {
                if is_name_safe(b) {
                    buf.push(b);
                } else {
                    buf.extend_from_slice(format!("#{:02X}", b).as_bytes());
                }
            }
        }
        PdfObject::String(s) => {
            // Hex encoding only: trivially round-trips. Literal strings
            // need escape handling that's tested elsewhere.
            buf.push(b'<');
            for b in s.as_bytes() {
                buf.extend_from_slice(format!("{:02X}", b).as_bytes());
            }
            buf.push(b'>');
        }
        PdfObject::Array(arr) => {
            buf.push(b'[');
            for (i, item) in arr.iter().enumerate() {
                if i > 0 {
                    buf.push(b' ');
                }
                write_object(buf, item);
            }
            buf.push(b']');
        }
        PdfObject::Dictionary(dict) => {
            buf.extend_from_slice(b"<<");
            for (k, v) in dict.iter() {
                buf.push(b' ');
                // Key is a name
                buf.push(b'/');
                for &b in k {
                    if is_name_safe(b) {
                        buf.push(b);
                    } else {
                        buf.extend_from_slice(format!("#{:02X}", b).as_bytes());
                    }
                }
                buf.push(b' ');
                write_object(buf, v);
            }
            buf.extend_from_slice(b" >>");
        }
        // Streams and references are excluded from the strategy. The
        // wildcard covers both those variants and any future
        // #[non_exhaustive] additions.
        _ => unreachable!("strategy never emits Stream / Reference / future variants"),
    }
}

/// Bytes that can appear unescaped inside a name token.
fn is_name_safe(b: u8) -> bool {
    // Per PDF spec a name body excludes whitespace, delimiters, the `#`
    // escape introducer itself, and any non-printable byte.
    if !(0x21..=0x7E).contains(&b) {
        return false;
    }
    // Delimiters and whitespace: ()<>[]{}/%# (and SP/TAB/CR/LF caught above)
    !matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%' | b'#'
    )
}

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

/// Atom strategies: every leaf the round-trip must handle losslessly.
fn atom_strategy() -> impl Strategy<Value = PdfObject> {
    prop_oneof![
        Just(PdfObject::Null),
        any::<bool>().prop_map(PdfObject::Boolean),
        any::<i32>().prop_map(|n| PdfObject::Integer(n as i64)),
        // Quantise reals to 6 decimal digits so the canonical writer's
        // {:.6} format is bit-exact on the round-trip. Excludes NaN/Inf
        // because PDF can't represent them.
        (-1_000_000i64..1_000_000i64).prop_map(|n| PdfObject::Real(n as f64 / 1_000.0)),
        // Names: 0..16 bytes from full 0x01..=0xFF range. Bytes that can't
        // appear unescaped in a name are emitted via `#XX` by the writer.
        prop::collection::vec(1u8..=0xFFu8, 0..16).prop_map(PdfObject::Name),
        // Strings: arbitrary 0..32 bytes including NUL, hex-emitted.
        prop::collection::vec(any::<u8>(), 0..32)
            .prop_map(|v| PdfObject::String(PdfString::new(v))),
    ]
}

/// Recursive object strategy: 4 levels deep, up to 6 children per container.
fn object_strategy() -> impl Strategy<Value = PdfObject> {
    let leaf = atom_strategy();
    leaf.prop_recursive(4, 64, 6, |inner| {
        prop_oneof![
            // Arrays
            prop::collection::vec(inner.clone(), 0..6).prop_map(PdfObject::Array),
            // Dictionaries: keys are non-empty name-safe ASCII so the dict
            // parser doesn't have to deal with two adjacent `/` tokens.
            prop::collection::vec(
                (
                    "[A-Za-z][A-Za-z0-9_]{0,8}".prop_map(|s| s.into_bytes()),
                    inner,
                ),
                0..6,
            )
            .prop_map(|kvs| {
                let mut d = PdfDictionary::new();
                for (k, v) in kvs {
                    d.insert(k, v);
                }
                PdfObject::Dictionary(d)
            }),
        ]
    })
}

// ---------------------------------------------------------------------------
// The properties
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        max_shrink_iters: 4096,
        .. ProptestConfig::default()
    })]

    /// Lex -> parse -> re-emit produces a byte string identical to the
    /// canonical first emission.
    #[test]
    fn lex_parse_roundtrip_atoms(obj in atom_strategy()) {
        let mut emitted = Vec::new();
        write_object(&mut emitted, &obj);

        let mut parser = ObjectParser::new(&emitted);
        let parsed = parser.parse_object().expect("parse");

        let mut reemitted = Vec::new();
        write_object(&mut reemitted, &parsed);

        prop_assert_eq!(&emitted, &reemitted);
    }

    /// Same property over the recursive object strategy: arrays and
    /// dictionaries with up to 4 levels of nesting.
    #[test]
    fn lex_parse_roundtrip_nested(obj in object_strategy()) {
        let mut emitted = Vec::new();
        write_object(&mut emitted, &obj);

        let mut parser = ObjectParser::new(&emitted);
        let parsed = parser.parse_object().expect("parse");

        let mut reemitted = Vec::new();
        write_object(&mut reemitted, &parsed);

        prop_assert_eq!(&emitted, &reemitted);
    }

    /// Whitespace insensitivity: inserting extra spaces between tokens
    /// must not change the parsed value.
    #[test]
    fn extra_whitespace_is_tolerated(obj in object_strategy()) {
        let mut canonical = Vec::new();
        write_object(&mut canonical, &obj);

        // Naively double every space. PDF whitespace handling is permissive
        // enough that this is always a valid input.
        let padded: Vec<u8> = canonical
            .iter()
            .flat_map(|&b| if b == b' ' { vec![b' ', b' '] } else { vec![b] })
            .collect();

        let mut parser = ObjectParser::new(&padded);
        let parsed = parser.parse_object().expect("parse");

        let mut reemitted = Vec::new();
        write_object(&mut reemitted, &parsed);

        prop_assert_eq!(&canonical, &reemitted);
    }
}
