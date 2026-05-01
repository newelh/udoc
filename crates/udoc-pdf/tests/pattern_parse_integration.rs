//! Integration tests for Type 1 coloured tiling pattern parsing
//! (ISO 32000-2 §8.7.3).
//!
//! These tests parse real PDF pattern resource dicts from the corpus
//! (not synthetic ones; the synthetic round-trip lives in the
//! in-crate `pattern::tests` module).
//!
//! The Sprint-50 `residual-gaps.md` doc hypothesised PMC1079898's
//! pink title banner might use a Pattern colorspace; in practice the
//! banner fill is `/Cs6 cs 1 1 1 scn`, a device colorspace. The real
//! Type-1 coloured tiling patterns live in the arxiv-physics subset
//! (`2602.14347.pdf` and friends, all PaintType 1), which we use
//! here as the on-the-ground acceptance target. Wave 3
//! (T3-PATTERN-RENDER) wires the parser to the emit-on-paint code
//! path; Wave 1 proves the parser works against real data.

use std::sync::Arc;

use udoc_pdf::object::resolver::ObjectResolver;
use udoc_pdf::object::{PdfDictionary, PdfObject};
use udoc_pdf::parse::document_parser::DocumentParser;
use udoc_pdf::{
    parse_tiling_pattern, CollectingDiagnostics, ParseOutcome, TilingPattern, WarningKind,
};

fn corpus_path(rel: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/downloaded")
        .join(rel)
}

/// Walk all resolved indirect-object dicts in the PDF and return the
/// first one whose `/Type /Pattern` + `/PatternType 1` + `/PaintType 1`
/// matches. Traversing by xref is faster and simpler than walking
/// `/Resources /Pattern` through every page, and it is sufficient for
/// a smoke test.
fn find_first_coloured_tiling_ref(
    data: &[u8],
) -> Option<(udoc_pdf::object::ObjRef, udoc_pdf::object::PdfObject)> {
    let diag = Arc::new(CollectingDiagnostics::new());
    let doc = DocumentParser::with_diagnostics(data, diag.clone())
        .parse()
        .ok()?;
    let xref_entries: Vec<u32> = doc.xref.iter().map(|(k, _)| k).collect();
    let mut resolver = ObjectResolver::from_document_with_diagnostics(data, doc, diag);
    for obj_num in xref_entries {
        let obj_ref = udoc_pdf::object::ObjRef::new(obj_num, 0);
        let Ok(resolved) = resolver.resolve(obj_ref) else {
            continue;
        };
        let dict: Option<&PdfDictionary> = match &resolved {
            PdfObject::Stream(s) => Some(&s.dict),
            PdfObject::Dictionary(d) => Some(d),
            _ => None,
        };
        let Some(dict) = dict else { continue };
        if dict.get_i64(b"PatternType") == Some(1) && dict.get_i64(b"PaintType") == Some(1) {
            return Some((obj_ref, resolved));
        }
    }
    None
}

#[test]
fn parses_coloured_tiling_from_real_corpus_doc() {
    // arxiv-physics/2602.14347.pdf ships pattern objects with
    // /PatternType 1 /PaintType 1 (coloured tiling). See
    // `grep -l /PatternType 1 /PaintType 1` across the corpus for
    // the full list; this doc is one of ~15.
    let path = corpus_path("arxiv-physics/2602.14347.pdf");
    if !path.exists() {
        eprintln!(
            "skipping: corpus doc not present at {} (run from repo root \
             with `tests/corpus/downloaded/` populated)",
            path.display()
        );
        return;
    }
    let data = std::fs::read(&path).expect("corpus doc readable");

    let Some((obj_ref, obj)) = find_first_coloured_tiling_ref(&data) else {
        panic!(
            "no /PatternType 1 /PaintType 1 found in {}, corpus may have drifted",
            path.display()
        );
    };

    let diag = Arc::new(CollectingDiagnostics::new());
    let doc = DocumentParser::with_diagnostics(&data, diag.clone())
        .parse()
        .expect("parse");
    let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag.clone());

    // parse_tiling_pattern takes the *value* slot as it would appear
    // in /Resources /Pattern (a Reference in the common case).
    let slot = PdfObject::Reference(obj_ref);
    let outcome = parse_tiling_pattern("P1", &slot, &mut resolver, &*diag);

    let tp: TilingPattern = match outcome {
        ParseOutcome::ColouredTiling(tp) => tp,
        other => panic!("expected ColouredTiling, got {other:?} for {obj_ref:?}"),
    };
    // Silence "unused" for the fallback branch above.
    let _ = obj;

    // Core invariants:
    assert_eq!(tp.resource_name, "P1");
    assert_eq!(tp.obj_ref, Some(obj_ref));
    // BBox must be a well-formed rectangle (urx > llx, ury > lly).
    assert!(
        tp.bbox[2] > tp.bbox[0] && tp.bbox[3] > tp.bbox[1],
        "degenerate /BBox: {:?}",
        tp.bbox
    );
    // /XStep and /YStep must be non-zero and finite.
    assert!(tp.xstep.is_finite() && tp.xstep != 0.0);
    assert!(tp.ystep.is_finite() && tp.ystep != 0.0);
    // /Matrix is either the default or a full 6-element transform.
    // All components finite.
    for v in tp.matrix {
        assert!(
            v.is_finite(),
            "non-finite matrix value {v} in {:?}",
            tp.matrix
        );
    }
    // The tile must have some drawing ops: a Type 1 tiling is
    // useless without them.
    assert!(
        !tp.content_stream.is_empty(),
        "empty tile content stream on {obj_ref:?}"
    );

    // Debug-printable for the task-report "paste the struct" line.
    // (Don't assert on the Debug text; it carries potentially long
    // /Resources + raw bytes.) Just confirm it renders without
    // panicking. Print when run with --nocapture so the task report
    // can quote the real output.
    eprintln!(
        "TilingPattern {{\n  resource_name: {:?},\n  obj_ref: {:?},\n  bbox: {:?},\n  xstep: {},\n  ystep: {},\n  matrix: {:?},\n  tiling_type: {},\n  resources: <{} keys>,\n  content_stream: <{} bytes, first 16 = {:?}>\n}}",
        tp.resource_name,
        tp.obj_ref,
        tp.bbox,
        tp.xstep,
        tp.ystep,
        tp.matrix,
        tp.tiling_type,
        tp.resources.iter().count(),
        tp.content_stream.len(),
        &tp.content_stream[..tp.content_stream.len().min(16)]
    );
}

#[test]
fn parse_outcome_reports_unsupported_for_type2_shading_pattern() {
    // arxiv-physics has plenty of PatternType 2 (shading pattern)
    // dicts too. Find one and assert we emit the right warning kind
    // and fall through.
    let path = corpus_path("arxiv-physics/2602.17074.pdf");
    if !path.exists() {
        eprintln!("skipping: {} not present", path.display());
        return;
    }
    let data = std::fs::read(&path).expect("corpus doc readable");

    let diag = Arc::new(CollectingDiagnostics::new());
    let doc = DocumentParser::with_diagnostics(&data, diag.clone())
        .parse()
        .expect("parse");
    let xref_entries: Vec<u32> = doc.xref.iter().map(|(k, _)| k).collect();
    let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag.clone());

    let mut found_type2: Option<udoc_pdf::object::ObjRef> = None;
    for obj_num in xref_entries {
        let obj_ref = udoc_pdf::object::ObjRef::new(obj_num, 0);
        let Ok(resolved) = resolver.resolve(obj_ref) else {
            continue;
        };
        let dict: Option<&PdfDictionary> = match &resolved {
            PdfObject::Stream(s) => Some(&s.dict),
            PdfObject::Dictionary(d) => Some(d),
            _ => None,
        };
        let Some(dict) = dict else { continue };
        if dict.get_i64(b"PatternType") == Some(2) {
            found_type2 = Some(obj_ref);
            break;
        }
    }
    let Some(obj_ref) = found_type2 else {
        eprintln!(
            "no PatternType 2 found in {}, corpus may have drifted",
            path.display()
        );
        return;
    };

    let slot = PdfObject::Reference(obj_ref);
    let outcome = parse_tiling_pattern("P-shading", &slot, &mut resolver, &*diag);
    match outcome {
        ParseOutcome::Unsupported { pattern_type, .. } => assert_eq!(pattern_type, 2),
        other => panic!("expected Unsupported for PatternType 2, got {other:?}"),
    }
    let warnings = diag.warnings();
    assert!(
        warnings
            .iter()
            .any(|w| w.kind == WarningKind::UnsupportedPatternType),
        "expected UnsupportedPatternType warning, got {:?}",
        warnings.iter().map(|w| w.kind).collect::<Vec<_>>()
    );
}
