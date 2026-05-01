//! W1-MARKDOWN: `Document.to_markdown()` -- thin wrapper around the Rust
//! `udoc::output::markdown` module shipped (T1b-MARKDOWN-OUT).
//!
//! No new pyclasses live here -- the markdown rendering / dict / JSON
//! helpers are module-level functions called by `PyDocument` methods
//! (W1-METHODS-DOCUMENT). `register()` stays empty by design.

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

/// Convert a udoc Document into LLM-friendly markdown.
///
/// Wraps `udoc::output::markdown::markdown_with_anchors()` ( T1b).
/// `with_anchors=true` preserves citation anchors as HTML comments
/// (`<!-- udoc:page=N bbox=... node=node:1234 -->`); `false` strips
/// them for human consumption.
pub fn document_to_markdown(doc: &udoc_facade::Document, with_anchors: bool) -> String {
    if with_anchors {
        udoc_facade::output::markdown::markdown_with_anchors(doc)
    } else {
        udoc_facade::output::markdown::markdown(doc)
    }
}

/// Convert a udoc Document into a Python dict.
///
///the long-term path is a direct visitor that walks the
/// document tree and builds PyObjects in place. For the  alpha we
/// take the json.loads roundtrip: it's correct, exercises the same
/// serde shape we ship via `to_json`, and stays out of the way of the
/// W1-CONVERT visitor (which targets the typed pyclasses, not raw
/// dicts).
///
/// TODO(post-alpha): replace with a direct serde_json::Value -> PyAny
/// visitor to skip the string roundtrip.
pub fn document_to_dict(py: Python<'_>, doc: &udoc_facade::Document) -> PyResult<Py<PyAny>> {
    let json = serde_json::to_string(doc)
        .map_err(|e| PyRuntimeError::new_err(format!("serialize: {e}")))?;
    let json_module = py.import("json")?;
    let value = json_module.call_method1("loads", (json,))?;
    Ok(value.unbind())
}

/// Convert a udoc Document to a JSON string.
///
/// `pretty=true` returns multi-line indented JSON (`serde_json::to_string_pretty`);
/// `pretty=false` returns the compact single-line form (`serde_json::to_string`).
pub fn document_to_json(doc: &udoc_facade::Document, pretty: bool) -> PyResult<String> {
    if pretty {
        serde_json::to_string_pretty(doc)
            .map_err(|e| PyRuntimeError::new_err(format!("serialize: {e}")))
    } else {
        serde_json::to_string(doc).map_err(|e| PyRuntimeError::new_err(format!("serialize: {e}")))
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // No pyclasses, no top-level Python functions: PyDocument's methods
    // (W1-METHODS-DOCUMENT) call the module-level helpers above directly.
    let _ = m;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
//
// These are pure-Rust tests: no `Python::attach`, no PyO3 GIL acquisition,
// and no calls into our `PyResult`-returning helpers (`document_to_dict`,
// `document_to_json`). `udoc-py` builds with `pyo3/extension-module`, which
// suppresses linking against libpython, so any code path that constructs a
// `PyErr` (via `PyRuntimeError::new_err`, `?` from `PyResult`, etc.) drags
// `_Py_IncRef` / `Py_None` / `PyImport_Import` symbols into the `cargo test`
// binary at link time, where they have no resolver. The pyo3-touching paths
// are exercised end-to-end by the Python pytest suite under `python/tests/`
// (W3-PYTEST), which runs inside a real interpreter via `maturin develop`.
//
// What we cover here without a Python runtime:
//   - `document_to_markdown(...)`: pure `&Document -> String`. Exercised
//     directly.
//   - `document_to_json(...)` / `document_to_dict(...)`: invariants are
//     tested via `serde_json` directly on the `Document`. The wrapper
//     bodies are 3 lines each (delegate + map_err); the value is in the
//     serialization shape. The Python-side roundtrip lives in W3-PYTEST.

#[cfg(all(test, not(feature = "extension-module")))]
mod tests {
    use super::document_to_markdown;

    /// Workspace-relative path to the minimal PDF fixture, resolved at
    /// compile time so the test binary doesn't depend on cwd.
    const HELLO_PDF: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/corpus/minimal/hello.pdf"
    );

    fn load_hello() -> udoc_facade::Document {
        let bytes = std::fs::read(HELLO_PDF).expect("read hello.pdf");
        udoc_facade::extract_bytes(&bytes).expect("extract hello.pdf")
    }

    /// Build a tiny synthetic Document with one H1 + paragraph. Used by
    /// tests that need a known-shape doc independent of PDF heuristics.
    fn synth_doc() -> udoc_facade::Document {
        use udoc_facade::{Block, Document, Inline, SpanStyle};

        let mut doc = Document::new();
        let h_text_id = doc.alloc_node_id();
        let h_id = doc.alloc_node_id();
        doc.content.push(Block::Heading {
            id: h_id,
            level: 1,
            content: vec![Inline::Text {
                id: h_text_id,
                text: "Greetings".into(),
                style: SpanStyle::default(),
            }],
        });
        let p_text_id = doc.alloc_node_id();
        let p_id = doc.alloc_node_id();
        doc.content.push(Block::Paragraph {
            id: p_id,
            content: vec![Inline::Text {
                id: p_text_id,
                text: "world".into(),
                style: SpanStyle::default(),
            }],
        });
        doc
    }

    #[test]
    fn test_document_to_markdown_with_anchors_includes_anchor_comment() {
        let doc = load_hello();
        let md = document_to_markdown(&doc, true);
        assert!(
            md.contains("<!-- udoc:"),
            "with_anchors=true must emit `<!-- udoc:` comments, got:\n{md}"
        );
    }

    #[test]
    fn test_document_to_markdown_without_anchors_strips() {
        let doc = load_hello();
        let md = document_to_markdown(&doc, false);
        assert!(
            !md.contains("<!-- udoc:"),
            "with_anchors=false must strip `<!-- udoc:` comments, got:\n{md}"
        );
    }

    #[test]
    fn test_document_to_markdown_includes_headings() {
        // Synthetic doc, so we exercise the H1 emit path independently of
        // PDF font-size inference.
        let doc = synth_doc();
        let md = document_to_markdown(&doc, false);
        assert!(
            md.contains("# Greetings"),
            "expected `# Greetings` heading in:\n{md}"
        );
    }

    #[test]
    fn test_document_to_dict_returns_dict() {
        // No Python runtime in the test binary, so we assert the JSON
        // serialization is a JSON object. `document_to_dict` is
        // `json.loads(json)` and json.loads on a JSON object returns a
        // dict by Python contract -- the Python-side roundtrip is
        // covered by `python/tests/` under W3-PYTEST.
        let doc = load_hello();
        let json = serde_json::to_string(&doc).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert!(
            v.is_object(),
            "Document must serialize as a JSON object so json.loads -> dict, got: {v:?}"
        );
    }

    #[test]
    fn test_document_to_dict_has_content_key() {
        // The dict shape comes from serde::Serialize for Document. If
        // `content` is missing in the JSON, `document_to_dict` won't
        // have it either.
        let doc = load_hello();
        let json = serde_json::to_string(&doc).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert!(
            v.get("content").is_some(),
            "to_dict result must include a `content` key, got: {v}"
        );
    }

    #[test]
    fn test_document_to_dict_has_metadata_key() {
        let doc = load_hello();
        let json = serde_json::to_string(&doc).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert!(
            v.get("metadata").is_some(),
            "to_dict result must include a `metadata` key, got: {v}"
        );
    }

    #[test]
    fn test_document_to_json_pretty_is_indented() {
        // Mirrors the `pretty=true` path of `document_to_json`, which is
        // a thin wrapper over `serde_json::to_string_pretty` (the only
        // logic in the wrapper is mapping serde errors to PyRuntimeError,
        // which we cannot exercise without a Python runtime).
        let doc = load_hello();
        let pretty = serde_json::to_string_pretty(&doc).expect("to_string_pretty");
        assert!(
            pretty.contains('\n'),
            "pretty JSON must contain newlines, got: {pretty}"
        );
        assert!(
            pretty.contains("  "),
            "pretty JSON must use indentation, got: {pretty}"
        );
    }

    #[test]
    fn test_document_to_json_compact_is_single_line() {
        // Mirrors the `pretty=false` path of `document_to_json`.
        let doc = load_hello();
        let compact = serde_json::to_string(&doc).expect("to_string");
        assert!(
            !compact.contains('\n'),
            "compact JSON must be single-line, got: {compact}"
        );
        // Sanity: it parses back as JSON.
        let _: serde_json::Value =
            serde_json::from_str(&compact).expect("compact JSON must round-trip");
    }
}
