//! Hook protocol types, handshake parsing, and metadata enforcement.
//!
//! Defines the Phase/Capability/Need/Provide enums, the handshake
//! parsing logic for the udoc-hook-v1 protocol, and the metadata
//! mutation functions that enforce protocol constraints (key validation,
//! property limits).

use serde_json::Value;

use udoc_core::document::Document;

// ---------------------------------------------------------------------------
// Protocol enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Phase {
    Ocr,
    Layout,
    Annotate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Capability {
    Ocr,
    Layout,
    Annotate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Need {
    Image,
    Spans,
    Blocks,
    Text,
    /// Hook wants the whole document sent in one request instead of per-page.
    /// Request shape: `{"document_path": "...", "page_count": N, "format": "pdf"}`.
    /// Response shape: `{"pages": [{"page_index": 0, "spans": [...]}, ...]}`.
    Document,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Provide {
    Spans,
    Regions,
    Tables,
    Blocks,
    Overlays,
    Entities,
    Labels,
}

// ---------------------------------------------------------------------------
// Parsing helpers for capability/need/provide strings
// ---------------------------------------------------------------------------

pub(crate) fn parse_capability(s: &str) -> Option<Capability> {
    match s {
        "ocr" => Some(Capability::Ocr),
        "layout" => Some(Capability::Layout),
        "annotate" => Some(Capability::Annotate),
        _ => None,
    }
}

pub(crate) fn parse_need(s: &str) -> Option<Need> {
    match s {
        "image" => Some(Need::Image),
        "spans" => Some(Need::Spans),
        "blocks" => Some(Need::Blocks),
        "text" => Some(Need::Text),
        "document" => Some(Need::Document),
        _ => None,
    }
}

pub(crate) fn parse_provide(s: &str) -> Option<Provide> {
    match s {
        "spans" => Some(Provide::Spans),
        "regions" => Some(Provide::Regions),
        "tables" => Some(Provide::Tables),
        "blocks" => Some(Provide::Blocks),
        "overlays" => Some(Provide::Overlays),
        "entities" => Some(Provide::Entities),
        "labels" => Some(Provide::Labels),
        _ => None,
    }
}

/// Determine the phase from capabilities. The earliest capability wins.
pub(crate) fn phase_from_capabilities(caps: &[Capability]) -> Phase {
    if caps.contains(&Capability::Ocr) {
        Phase::Ocr
    } else if caps.contains(&Capability::Layout) {
        Phase::Layout
    } else {
        Phase::Annotate
    }
}

/// Wire-format protocol identifier this build accepts.
pub(crate) const HOOK_PROTOCOL_ID: &str = "udoc-hook-v1";

/// Outcome of parsing a hook's first output line as a handshake.
///
/// Tri-state. The earlier shape was `Option<...>`, which conflated "not a
/// handshake" with "wrong protocol id" and silently demoted handshake
/// errors into a default OCR shape; the typed three-way outcome lets the
/// caller surface a clear error when the protocol id does not match.
#[derive(Debug)]
pub(crate) enum HandshakeOutcome {
    /// Recognised handshake on the current protocol identifier.
    Valid {
        capabilities: Vec<Capability>,
        needs: Vec<Need>,
        provides: Vec<Provide>,
    },
    /// Not a handshake at all (no `protocol` field, malformed JSON,
    /// missing required arrays, or empty capabilities). Caller treats
    /// the line as the hook's first response line and proceeds with
    /// the default OCR hook shape.
    NotHandshake,
    /// Handshake JSON had a `protocol` field but the value did not
    /// equal [`HOOK_PROTOCOL_ID`]. Carries the observed value so the
    /// caller can name both expected and observed in the error.
    WrongProtocol { observed: String },
}

/// Parse a line as a hook protocol handshake. See [`HandshakeOutcome`]
/// for the three observable states.
pub(crate) fn parse_handshake(line: &str) -> HandshakeOutcome {
    let obj: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return HandshakeOutcome::NotHandshake,
    };

    // Distinguish "no protocol field at all" from "protocol field
    // present but wrong value". The first is "not a handshake" and we
    // fall back to the default OCR shape; the second is a clear-error
    // path (T1a-HOOKS-PROTOCOL-FIX).
    let protocol_field = match obj.get("protocol") {
        Some(v) => v,
        None => return HandshakeOutcome::NotHandshake,
    };
    let Some(protocol) = protocol_field.as_str() else {
        return HandshakeOutcome::NotHandshake;
    };

    if protocol != HOOK_PROTOCOL_ID {
        return HandshakeOutcome::WrongProtocol {
            observed: protocol.to_string(),
        };
    }

    let Some(caps_arr) = obj.get("capabilities").and_then(|v| v.as_array()) else {
        return HandshakeOutcome::NotHandshake;
    };
    let Some(needs_arr) = obj.get("needs").and_then(|v| v.as_array()) else {
        return HandshakeOutcome::NotHandshake;
    };
    let Some(provides_arr) = obj.get("provides").and_then(|v| v.as_array()) else {
        return HandshakeOutcome::NotHandshake;
    };

    let caps: Vec<Capability> = caps_arr
        .iter()
        .filter_map(|v| v.as_str().and_then(parse_capability))
        .collect();

    let needs: Vec<Need> = needs_arr
        .iter()
        .filter_map(|v| v.as_str().and_then(parse_need))
        .collect();

    let provides: Vec<Provide> = provides_arr
        .iter()
        .filter_map(|v| v.as_str().and_then(parse_provide))
        .collect();

    if caps.is_empty() {
        return HandshakeOutcome::NotHandshake;
    }

    HandshakeOutcome::Valid {
        capabilities: caps,
        needs,
        provides,
    }
}

/// Maximum length for a hook metadata key component (overlay name, label key).
const MAX_HOOK_KEY_LEN: usize = 256;

/// Validate a key component from hook output.
/// Only allows ASCII alphanumeric, underscore, dot, and hyphen.
pub(crate) fn is_valid_hook_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_HOOK_KEY_LEN
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
}

// ---------------------------------------------------------------------------
// Metadata mutation (enforces protocol constraints)
// ---------------------------------------------------------------------------

/// Maximum number of properties hooks may add to document metadata.
const MAX_HOOK_PROPERTIES: usize = 10_000;

/// Apply overlay data from a hook response.
/// Simplified for v1: stores as named properties in metadata.
pub(crate) fn apply_overlays(doc: &mut Document, overlays: &serde_json::Map<String, Value>) {
    for (overlay_name, overlay_data) in overlays {
        if !is_valid_hook_key(overlay_name) {
            eprintln!(
                "hook: invalid overlay name {:?}, skipping",
                &overlay_name[..overlay_name.len().min(50)]
            );
            continue;
        }
        if let Some(entries) = overlay_data.as_object() {
            for (node_id_str, value) in entries {
                if !is_valid_hook_key(node_id_str) {
                    continue;
                }
                if doc.metadata.properties.len() >= MAX_HOOK_PROPERTIES {
                    eprintln!(
                        "hook: metadata property limit ({}) reached, skipping remaining",
                        MAX_HOOK_PROPERTIES
                    );
                    return;
                }
                let key = format!("hook.overlay.{}.{}", overlay_name, node_id_str);
                let val = match value {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                doc.metadata.properties.insert(key, val);
            }
        }
    }
}

/// Apply entity data from a hook response.
/// Simplified for v1: stores as a JSON array in metadata.
pub(crate) fn apply_entities(doc: &mut Document, entities: &[Value], page_idx: usize) {
    if doc.metadata.properties.len() >= MAX_HOOK_PROPERTIES {
        eprintln!(
            "udoc: hook entity limit reached ({MAX_HOOK_PROPERTIES}), skipping page {page_idx} entities"
        );
        return;
    }
    let key = format!("hook.entities.page.{}", page_idx);
    let json = serde_json::to_string(entities).unwrap_or_else(|_| "[]".to_string());
    doc.metadata.properties.insert(key, json);
}

/// Apply label data from a hook response.
/// Merges into DocumentMetadata.properties with `hook.label.` prefix
/// to prevent hooks from overwriting core metadata keys.
pub(crate) fn apply_labels(doc: &mut Document, labels: &serde_json::Map<String, Value>) {
    for (key, value) in labels {
        if !is_valid_hook_key(key) {
            eprintln!(
                "hook: invalid label key {:?}, skipping",
                &key[..key.len().min(50)]
            );
            continue;
        }
        if doc.metadata.properties.len() >= MAX_HOOK_PROPERTIES {
            eprintln!(
                "hook: metadata property limit ({}) reached, skipping remaining",
                MAX_HOOK_PROPERTIES
            );
            return;
        }
        let val = match value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let namespaced_key = format!("hook.label.{}", key);
        doc.metadata.properties.insert(namespaced_key, val);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_handshake_valid() {
        let line = r#"{"protocol":"udoc-hook-v1","capabilities":["ocr","layout"],"needs":["image","spans"],"provides":["spans","regions"]}"#;
        let result = parse_handshake(line);
        match result {
            HandshakeOutcome::Valid {
                capabilities,
                needs,
                provides,
            } => {
                assert_eq!(capabilities, vec![Capability::Ocr, Capability::Layout]);
                assert_eq!(needs, vec![Need::Image, Need::Spans]);
                assert_eq!(provides, vec![Provide::Spans, Provide::Regions]);
            }
            other => panic!("expected Valid, got {other:?}"),
        }
    }

    #[test]
    fn parse_handshake_wrong_protocol_v2() {
        // A future v2 handshake should be flagged as WrongProtocol so
        // operators get a clear error to bump tooling, not silently
        // demoted to OCR.
        let line = r#"{"protocol":"udoc-hook-v2","capabilities":["ocr"],"needs":["image"],"provides":["spans"]}"#;
        match parse_handshake(line) {
            HandshakeOutcome::WrongProtocol { observed } => {
                assert_eq!(observed, "udoc-hook-v2");
            }
            other => panic!("expected WrongProtocol, got {other:?}"),
        }
    }

    #[test]
    fn parse_handshake_missing_protocol() {
        let line = r#"{"capabilities":["ocr"],"needs":["image"],"provides":["spans"]}"#;
        assert!(matches!(
            parse_handshake(line),
            HandshakeOutcome::NotHandshake
        ));
    }

    #[test]
    fn parse_handshake_not_json() {
        assert!(matches!(
            parse_handshake("not json at all"),
            HandshakeOutcome::NotHandshake
        ));
    }

    #[test]
    fn parse_handshake_empty_capabilities() {
        let line = r#"{"protocol":"udoc-hook-v1","capabilities":[],"needs":["image"],"provides":["spans"]}"#;
        assert!(matches!(
            parse_handshake(line),
            HandshakeOutcome::NotHandshake
        ));
    }

    #[test]
    fn parse_handshake_unknown_capabilities_filtered() {
        let line = r#"{"protocol":"udoc-hook-v1","capabilities":["ocr","quantum"],"needs":["image"],"provides":["spans"]}"#;
        match parse_handshake(line) {
            HandshakeOutcome::Valid { capabilities, .. } => {
                assert_eq!(capabilities, vec![Capability::Ocr]);
            }
            other => panic!("expected Valid, got {other:?}"),
        }
    }

    #[test]
    fn phase_ordering() {
        assert!(Phase::Ocr < Phase::Layout);
        assert!(Phase::Layout < Phase::Annotate);
    }

    #[test]
    fn phase_from_caps_ocr() {
        assert_eq!(phase_from_capabilities(&[Capability::Ocr]), Phase::Ocr);
    }

    #[test]
    fn phase_from_caps_ocr_and_layout() {
        // Multi-capability: runs in earliest phase.
        assert_eq!(
            phase_from_capabilities(&[Capability::Ocr, Capability::Layout]),
            Phase::Ocr
        );
    }

    #[test]
    fn phase_from_caps_layout() {
        assert_eq!(
            phase_from_capabilities(&[Capability::Layout]),
            Phase::Layout
        );
    }

    #[test]
    fn phase_from_caps_annotate() {
        assert_eq!(
            phase_from_capabilities(&[Capability::Annotate]),
            Phase::Annotate
        );
    }

    // -- Key validation tests --

    #[test]
    fn is_valid_hook_key_valid() {
        assert!(is_valid_hook_key("confidence"));
        assert!(is_valid_hook_key("my-overlay"));
        assert!(is_valid_hook_key("node.123"));
        assert!(is_valid_hook_key("a_b_c"));
        assert!(is_valid_hook_key("123"));
    }

    #[test]
    fn is_valid_hook_key_empty() {
        assert!(!is_valid_hook_key(""));
    }

    #[test]
    fn is_valid_hook_key_special_chars() {
        assert!(!is_valid_hook_key("foo bar"));
        assert!(!is_valid_hook_key("foo\nbar"));
        assert!(!is_valid_hook_key("foo/bar"));
        assert!(!is_valid_hook_key("<script>"));
        assert!(!is_valid_hook_key("../../etc"));
    }

    #[test]
    fn is_valid_hook_key_too_long() {
        let long_key = "a".repeat(MAX_HOOK_KEY_LEN + 1);
        assert!(!is_valid_hook_key(&long_key));
        let max_key = "a".repeat(MAX_HOOK_KEY_LEN);
        assert!(is_valid_hook_key(&max_key));
    }

    #[test]
    fn apply_labels_to_doc() {
        let mut doc = Document::new();
        let mut labels = serde_json::Map::new();
        labels.insert("document_type".into(), Value::String("invoice".into()));
        labels.insert("language".into(), Value::String("en".into()));
        apply_labels(&mut doc, &labels);
        assert_eq!(
            doc.metadata.properties.get("hook.label.document_type"),
            Some(&"invoice".to_string())
        );
        assert_eq!(
            doc.metadata.properties.get("hook.label.language"),
            Some(&"en".to_string())
        );
    }

    #[test]
    fn apply_entities_to_doc() {
        let mut doc = Document::new();
        let entities = vec![serde_json::json!({
            "text": "John Smith",
            "label": "PERSON",
            "start": 0,
            "end": 10,
            "block_id": 5
        })];
        apply_entities(&mut doc, &entities, 0);
        let key = "hook.entities.page.0";
        assert!(doc.metadata.properties.contains_key(key));
        let stored: Vec<Value> =
            serde_json::from_str(doc.metadata.properties.get(key).unwrap()).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0]["text"], "John Smith");
    }

    #[test]
    fn apply_overlays_to_doc() {
        let mut doc = Document::new();
        let mut overlays = serde_json::Map::new();
        let mut confidence = serde_json::Map::new();
        confidence.insert("5".into(), serde_json::json!(0.99));
        confidence.insert("6".into(), serde_json::json!(0.87));
        overlays.insert("confidence".into(), Value::Object(confidence));
        apply_overlays(&mut doc, &overlays);
        assert_eq!(
            doc.metadata.properties.get("hook.overlay.confidence.5"),
            Some(&"0.99".to_string())
        );
        assert_eq!(
            doc.metadata.properties.get("hook.overlay.confidence.6"),
            Some(&"0.87".to_string())
        );
    }

    #[test]
    fn apply_overlays_rejects_bad_keys() {
        let mut doc = Document::new();
        let mut overlays = serde_json::Map::new();
        // Valid overlay with invalid node key
        let mut entries = serde_json::Map::new();
        entries.insert("valid_key".into(), serde_json::json!(0.99));
        entries.insert("<injected>".into(), serde_json::json!(0.5));
        overlays.insert("confidence".into(), Value::Object(entries));
        // Invalid overlay name
        let mut entries2 = serde_json::Map::new();
        entries2.insert("5".into(), serde_json::json!(1.0));
        overlays.insert("bad/name".into(), Value::Object(entries2));
        apply_overlays(&mut doc, &overlays);
        // Only the valid combination should be stored
        assert_eq!(
            doc.metadata
                .properties
                .get("hook.overlay.confidence.valid_key"),
            Some(&"0.99".to_string())
        );
        assert!(!doc
            .metadata
            .properties
            .contains_key("hook.overlay.confidence.<injected>"));
        assert!(!doc
            .metadata
            .properties
            .keys()
            .any(|k| k.contains("bad/name")));
    }

    #[test]
    fn apply_labels_rejects_bad_keys() {
        let mut doc = Document::new();
        let mut labels = serde_json::Map::new();
        labels.insert("valid_label".into(), Value::String("ok".into()));
        labels.insert("bad key".into(), Value::String("rejected".into()));
        labels.insert("".into(), Value::String("also rejected".into()));
        apply_labels(&mut doc, &labels);
        assert_eq!(
            doc.metadata.properties.get("hook.label.valid_label"),
            Some(&"ok".to_string())
        );
        assert!(!doc.metadata.properties.contains_key("hook.label.bad key"));
        assert_eq!(doc.metadata.properties.len(), 1);
    }
}
