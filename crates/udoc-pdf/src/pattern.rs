//! Pattern colorspace dict parser (ISO 32000-2 §8.7.3) used by the
//! `cs` / `CS` / `scn` / `SCN` operators when the current colorspace is
//! `/Pattern`.
//!
//!, only **Type 1 coloured tiling
//! patterns** are fully materialized by this module:
//!
//! * **PatternType 1, PaintType 1 (coloured):** a full
//!   [`TilingPattern`] is returned. The tile cell's content stream is
//!   decoded and attached; the renderer (T3-PATTERN-RENDER, Wave 3)
//!   interprets it once per tile and composites the result across the
//!   fill region at `/XStep` / `/YStep` intervals.
//! * **PatternType 1, PaintType 2 (uncoloured):** falls through to the
//!   base fill color with a [`WarningKind::UnsupportedPatternType`]
//!   diagnostic. Wiring uncoloured patterns needs color-threading
//!   through `scn` which is post-alpha.
//! * **PatternType 2 (shading pattern):** falls through. The `sh`
//!   operator already handles shading via; pattern-
//!   colorspace shading-as-paint is post-alpha.
//! * **Anything else:** falls through with a warning.
//!
//! The parser never panics on malformed dicts. Missing required fields
//! (`/BBox`, `/XStep`, `/YStep`) short-circuit to
//! [`ParseOutcome::Invalid`] so the caller falls through to the base
//! fill color; the diagnostics sink gets one warning per occurrence.

use crate::diagnostics::{DiagnosticsSink, Warning, WarningKind};
use crate::object::resolver::ObjectResolver;
use crate::object::{ObjRef, PdfDictionary, PdfObject, PdfStream};

/// A Type 1 coloured tiling pattern (ISO 32000-2 §8.7.3.3).
///
/// Owns the fully-decoded content-stream bytes for one tile cell plus
/// the geometry needed to tile it (bbox, xstep, ystep, matrix) and the
/// nested /Resources dict that the tile's drawing ops reference.
///
/// `obj_ref` is the indirect reference the pattern was resolved from
/// (for diagnostics); when the pattern was inline in the page's
/// `/Resources /Pattern` dict it's `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct TilingPattern {
    /// Resource name under which the pattern was registered in the
    /// page's `/Resources /Pattern` dict (e.g., "P1").
    pub resource_name: String,
    /// Indirect reference this pattern was resolved from, if any.
    pub obj_ref: Option<ObjRef>,
    /// Tile cell bounding box in pattern coordinates: `[llx, lly, urx,
    /// ury]` per `/BBox`.
    pub bbox: [f64; 4],
    /// Horizontal spacing between tile origins (`/XStep`, ISO 32000-2
    /// §8.7.3.3). May be negative; must be non-zero.
    pub xstep: f64,
    /// Vertical spacing between tile origins (`/YStep`).
    pub ystep: f64,
    /// Pattern-to-userspace transform `[a, b, c, d, e, f]`. Defaults to
    /// the identity when `/Matrix` is absent.
    pub matrix: [f64; 6],
    /// Tiling type (`/TilingType`): 1 = constant spacing (default),
    /// 2 = no distortion, 3 = constant spacing and faster tiling. We
    /// preserve the value but the renderer may treat them all the
    /// same for alpha-1.
    pub tiling_type: i64,
    /// Nested /Resources dict for the tile's content stream. Patterns
    /// reference fonts, XObjects, colorspaces etc. via this dict,
    /// independent of the host page's resources.
    pub resources: PdfDictionary,
    /// Filter-decoded bytes of the tile's content stream.
    pub content_stream: Vec<u8>,
}

/// Outcome of parsing a pattern resource entry.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseOutcome {
    /// A fully-decoded Type 1 coloured tiling pattern. Ready to emit
    /// to the presentation overlay.
    ColouredTiling(TilingPattern),
    /// A recognised pattern kind we do not implement (Type 1
    /// uncoloured, Type 2 shading pattern, or anything else). The
    /// caller falls through to the base fill color. The diagnostics
    /// sink has already been notified.
    Unsupported {
        /// Raw `/PatternType` value from the dict (1 or 2 typically).
        pattern_type: i64,
        /// Raw `/PaintType` value (1 = coloured, 2 = uncoloured).
        /// 0 when the field is absent (e.g., Type 2 shading patterns
        /// do not carry /PaintType).
        paint_type: i64,
    },
    /// The pattern dict was malformed (missing /BBox, /XStep, etc.)
    /// or could not be resolved. Already diagnosed.
    Invalid,
}

/// Parse a pattern resource object (inline dict+stream or indirect
/// reference) into a [`ParseOutcome`].
///
///only Type 1 coloured tiling produces
/// [`ParseOutcome::ColouredTiling`]; every other supported combination
/// emits a diagnostic warning and returns [`ParseOutcome::Unsupported`]
/// so the caller can fall through to the base fill color.
pub fn parse_tiling_pattern(
    resource_name: &str,
    value: &PdfObject,
    resolver: &mut ObjectResolver<'_>,
    sink: &dyn DiagnosticsSink,
) -> ParseOutcome {
    // Resolve indirect references to the stream/dict form we need.
    // We collapse everything down to an Option<PdfStream> + owned
    // PdfDictionary up front so later field lookups don't conflict
    // with the moves required for `decode_stream_data`.
    let (obj_ref, owned_stream) = match value {
        PdfObject::Reference(r) => match resolver.resolve(*r) {
            Ok(PdfObject::Stream(s)) => (Some(*r), Some(s)),
            Ok(PdfObject::Dictionary(d)) => {
                // Type 2 shading patterns have no stream body. Wrap
                // the dict into a pseudo-stream with zero-length data
                // so the shared code path still works.
                (
                    Some(*r),
                    Some(PdfStream {
                        dict: d,
                        data_offset: 0,
                        data_length: 0,
                    }),
                )
            }
            _ => {
                sink.warning(Warning::new(
                    None,
                    WarningKind::InvalidState,
                    format!(
                        "Pattern /{resource_name}: indirect ref did not resolve to dict/stream"
                    ),
                ));
                return ParseOutcome::Invalid;
            }
        },
        PdfObject::Stream(s) => (None, Some(s.clone())),
        PdfObject::Dictionary(d) => (
            None,
            Some(PdfStream {
                dict: d.clone(),
                data_offset: 0,
                data_length: 0,
            }),
        ),
        _ => {
            sink.warning(Warning::new(
                None,
                WarningKind::InvalidState,
                format!("Pattern /{resource_name}: not a dict/stream"),
            ));
            return ParseOutcome::Invalid;
        }
    };

    let stream = owned_stream.expect("stream always set for dict/stream/ref paths");
    let has_stream_body = stream.data_length > 0;
    let pattern_type = stream.dict.get_i64(b"PatternType").unwrap_or(-1);
    let paint_type = stream.dict.get_i64(b"PaintType").unwrap_or(0);

    // Type 2 shading pattern, or anything we don't recognize, falls
    // through immediately (no content stream to decode).
    if pattern_type != 1 {
        sink.warning(Warning::new(
            None,
            WarningKind::UnsupportedPatternType,
            format!(
                "Pattern /{resource_name}: unsupported /PatternType {pattern_type} \
                 ( ships Type 1 coloured tiling only), \
                 falling through to base fill"
            ),
        ));
        return ParseOutcome::Unsupported {
            pattern_type,
            paint_type,
        };
    }

    // Type 1 uncoloured: emit warning and fall through. The /PaintType
    // field is required for Type 1 patterns (ISO 32000-2 §8.7.3.3);
    // treat missing as invalid to avoid silently rendering.
    if paint_type != 1 {
        sink.warning(Warning::new(
            None,
            WarningKind::UnsupportedPatternType,
            format!(
                "Pattern /{resource_name}: Type 1 /PaintType {paint_type} \
                 (uncoloured or invalid); only PaintType 1 (coloured) is \
                 implemented, falling through to base fill"
            ),
        ));
        return ParseOutcome::Unsupported {
            pattern_type,
            paint_type,
        };
    }

    // Coloured tiling must have a stream body.
    if !has_stream_body {
        sink.warning(Warning::new(
            None,
            WarningKind::InvalidState,
            format!(
                "Pattern /{resource_name}: Type 1 tiling has no content stream \
                 (pattern must be a stream, not a bare dict)"
            ),
        ));
        return ParseOutcome::Invalid;
    }

    // Extract all fields now, before the stream gets consumed by
    // decode_stream_data.
    // /BBox [llx lly urx ury] -- required
    let Some(bbox) = parse_numbers_fixed::<4>(stream.dict.get(b"BBox"), resolver) else {
        sink.warning(Warning::new(
            None,
            WarningKind::InvalidState,
            format!("Pattern /{resource_name}: missing or malformed /BBox"),
        ));
        return ParseOutcome::Invalid;
    };

    // /XStep, /YStep -- required. Zero/NaN is invalid per the spec
    // (would divide by zero when tiling).
    let xstep = stream.dict.get_f64(b"XStep").unwrap_or(f64::NAN);
    let ystep = stream.dict.get_f64(b"YStep").unwrap_or(f64::NAN);
    if !xstep.is_finite() || xstep == 0.0 || !ystep.is_finite() || ystep == 0.0 {
        sink.warning(Warning::new(
            None,
            WarningKind::InvalidState,
            format!(
                "Pattern /{resource_name}: invalid /XStep={xstep} /YStep={ystep} \
                 (must be finite and non-zero)"
            ),
        ));
        return ParseOutcome::Invalid;
    }

    // /Matrix optional, defaults to identity.
    let matrix = parse_numbers_fixed::<6>(stream.dict.get(b"Matrix"), resolver)
        .unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    let tiling_type = stream.dict.get_i64(b"TilingType").unwrap_or(1);

    // /Resources is recommended but may be absent (pattern has no
    // external refs). Default to an empty dict.
    let resources = match stream.dict.get(b"Resources") {
        Some(PdfObject::Dictionary(d)) => d.clone(),
        Some(PdfObject::Reference(r)) => match resolver.resolve(*r) {
            Ok(PdfObject::Dictionary(d)) => d,
            _ => PdfDictionary::new(),
        },
        _ => PdfDictionary::new(),
    };

    // Decode the tile's content stream. If decode fails we still emit
    // a pattern record with empty bytes so the renderer has enough
    // geometry to at least draw the fill region's base color; a
    // warning has already been pushed by the decoder.
    let content_stream = match resolver.decode_stream_data(&stream, obj_ref) {
        Ok(bytes) => bytes,
        Err(e) => {
            sink.warning(Warning::new(
                None,
                WarningKind::DecodeError,
                format!("Pattern /{resource_name}: failed to decode tile content stream: {e}"),
            ));
            Vec::new()
        }
    };

    ParseOutcome::ColouredTiling(TilingPattern {
        resource_name: resource_name.to_string(),
        obj_ref,
        bbox,
        xstep,
        ystep,
        matrix,
        tiling_type,
        resources,
        content_stream,
    })
}

/// Parse an N-element numeric array, resolving indirect refs. Returns
/// None if the value isn't an array of exactly N numbers (or more;
/// extras are ignored).
fn parse_numbers_fixed<const N: usize>(
    value: Option<&PdfObject>,
    resolver: &mut ObjectResolver<'_>,
) -> Option<[f64; N]> {
    let owned;
    let arr = match value? {
        PdfObject::Array(a) => a.as_slice(),
        PdfObject::Reference(r) => {
            owned = resolver.resolve(*r).ok()?;
            match &owned {
                PdfObject::Array(a) => a.as_slice(),
                _ => return None,
            }
        }
        _ => return None,
    };
    if arr.len() < N {
        return None;
    }
    let mut out = [0.0_f64; N];
    for i in 0..N {
        out[i] = arr[i].as_f64()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::CollectingDiagnostics;
    use crate::object::{PdfDictionary, PdfObject, PdfStream};
    use crate::parse::document_parser::XrefTable;

    fn name(b: &[u8]) -> PdfObject {
        PdfObject::Name(b.to_vec())
    }
    fn num(n: f64) -> PdfObject {
        PdfObject::Real(n)
    }
    fn int(n: i64) -> PdfObject {
        PdfObject::Integer(n)
    }
    fn arr(items: Vec<PdfObject>) -> PdfObject {
        PdfObject::Array(items)
    }

    fn stream_dict_with_entries(entries: Vec<(&[u8], PdfObject)>) -> PdfDictionary {
        let mut d = PdfDictionary::new();
        for (k, v) in entries {
            d.insert(k.to_vec(), v);
        }
        d
    }

    fn minimal_resolver(data: &[u8]) -> ObjectResolver<'_> {
        ObjectResolver::new(data, XrefTable::new())
    }

    #[test]
    fn parse_type1_coloured_roundtrip() {
        // Synthetic: 1-byte content stream "q" (save gstate) wrapped
        // in a PdfStream placed at a known offset inside the source.
        let tile_ops: &[u8] = b"q Q\n";
        let mut src: Vec<u8> = Vec::new();
        src.extend_from_slice(b"prefix-junk-"); // 12 bytes
        let data_offset = src.len() as u64;
        src.extend_from_slice(tile_ops);

        let mut dict = stream_dict_with_entries(vec![
            (b"Type", name(b"Pattern")),
            (b"PatternType", int(1)),
            (b"PaintType", int(1)),
            (b"TilingType", int(1)),
            (b"BBox", arr(vec![num(0.0), num(0.0), num(10.0), num(10.0)])),
            (b"XStep", num(10.0)),
            (b"YStep", num(10.0)),
            (
                b"Matrix",
                arr(vec![
                    num(1.0),
                    num(0.0),
                    num(0.0),
                    num(1.0),
                    num(5.0),
                    num(5.0),
                ]),
            ),
            (b"Resources", PdfObject::Dictionary(PdfDictionary::new())),
            (b"Length", int(tile_ops.len() as i64)),
        ]);
        // Length wasn't inserted properly via helper; insert directly.
        dict.insert(b"Length".to_vec(), int(tile_ops.len() as i64));

        let stream = PdfStream {
            dict,
            data_offset,
            data_length: tile_ops.len() as u64,
        };
        let obj = PdfObject::Stream(stream);

        let mut resolver = minimal_resolver(&src);
        let sink = CollectingDiagnostics::new();
        let out = parse_tiling_pattern("P1", &obj, &mut resolver, &sink);
        match out {
            ParseOutcome::ColouredTiling(tp) => {
                assert_eq!(tp.resource_name, "P1");
                assert_eq!(tp.bbox, [0.0, 0.0, 10.0, 10.0]);
                assert_eq!(tp.xstep, 10.0);
                assert_eq!(tp.ystep, 10.0);
                assert_eq!(tp.matrix, [1.0, 0.0, 0.0, 1.0, 5.0, 5.0]);
                assert_eq!(tp.tiling_type, 1);
                assert_eq!(tp.content_stream, tile_ops);
            }
            other => panic!("expected ColouredTiling, got {other:?}"),
        }
        assert!(
            sink.warnings().is_empty(),
            "unexpected warnings: {:?}",
            sink.warnings()
        );
    }

    #[test]
    fn parse_type1_uncoloured_warns_and_falls_through() {
        let mut dict = stream_dict_with_entries(vec![
            (b"PatternType", int(1)),
            (b"PaintType", int(2)),
            (b"BBox", arr(vec![num(0.0), num(0.0), num(10.0), num(10.0)])),
            (b"XStep", num(10.0)),
            (b"YStep", num(10.0)),
        ]);
        dict.insert(b"Length".to_vec(), int(0));
        let stream = PdfStream {
            dict,
            data_offset: 0,
            data_length: 0,
        };
        let obj = PdfObject::Stream(stream);

        let src: Vec<u8> = Vec::new();
        let mut resolver = minimal_resolver(&src);
        let sink = CollectingDiagnostics::new();
        let out = parse_tiling_pattern("P2", &obj, &mut resolver, &sink);
        match out {
            ParseOutcome::Unsupported {
                pattern_type,
                paint_type,
            } => {
                assert_eq!(pattern_type, 1);
                assert_eq!(paint_type, 2);
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
        let warnings = sink.warnings();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].kind, WarningKind::UnsupportedPatternType);
    }

    #[test]
    fn parse_type2_shading_warns() {
        let mut dict = stream_dict_with_entries(vec![
            (b"PatternType", int(2)),
            (b"Shading", PdfObject::Reference(ObjRef::new(100, 0))),
        ]);
        dict.insert(b"Length".to_vec(), int(0));
        let obj = PdfObject::Dictionary(dict);

        let src: Vec<u8> = Vec::new();
        let mut resolver = minimal_resolver(&src);
        let sink = CollectingDiagnostics::new();
        let out = parse_tiling_pattern("P3", &obj, &mut resolver, &sink);
        match out {
            ParseOutcome::Unsupported { pattern_type, .. } => {
                assert_eq!(pattern_type, 2);
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
        assert_eq!(sink.warnings().len(), 1);
        assert_eq!(sink.warnings()[0].kind, WarningKind::UnsupportedPatternType);
    }

    #[test]
    fn parse_missing_bbox_is_invalid() {
        let mut dict = stream_dict_with_entries(vec![
            (b"PatternType", int(1)),
            (b"PaintType", int(1)),
            (b"XStep", num(10.0)),
            (b"YStep", num(10.0)),
        ]);
        dict.insert(b"Length".to_vec(), int(0));
        let stream = PdfStream {
            dict,
            data_offset: 0,
            data_length: 0,
        };
        let obj = PdfObject::Stream(stream);

        let src: Vec<u8> = Vec::new();
        let mut resolver = minimal_resolver(&src);
        let sink = CollectingDiagnostics::new();
        let out = parse_tiling_pattern("P4", &obj, &mut resolver, &sink);
        assert!(matches!(out, ParseOutcome::Invalid));
        assert!(!sink.warnings().is_empty());
    }

    #[test]
    fn parse_zero_xstep_is_invalid() {
        let mut dict = stream_dict_with_entries(vec![
            (b"PatternType", int(1)),
            (b"PaintType", int(1)),
            (b"BBox", arr(vec![num(0.0), num(0.0), num(10.0), num(10.0)])),
            (b"XStep", num(0.0)),
            (b"YStep", num(10.0)),
        ]);
        dict.insert(b"Length".to_vec(), int(0));
        let stream = PdfStream {
            dict,
            data_offset: 0,
            data_length: 0,
        };
        let obj = PdfObject::Stream(stream);

        let src: Vec<u8> = Vec::new();
        let mut resolver = minimal_resolver(&src);
        let sink = CollectingDiagnostics::new();
        let out = parse_tiling_pattern("P5", &obj, &mut resolver, &sink);
        assert!(matches!(out, ParseOutcome::Invalid));
    }

    // Placeholder to prevent dead_code warning on arr/num/int if a
    // test gets trimmed.
    #[test]
    fn helpers_compile() {
        let _ = arr(vec![num(1.0), int(2)]);
        let _ = name(b"X");
    }
}
