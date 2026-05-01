//! Integration tests against real PDF corpus files.
//!
//! Four test levels, applied systematically to all corpus files:
//! 1. Structure: DocumentParser::parse() succeeds, xref non-empty, trailer has /Root
//! 2. Resolution: Resolve /Root -> /Catalog, walk /Pages tree, resolve all page dicts
//! 3. Stream decode: For each page, resolve /Contents stream(s) and decode them
//! 4. Font dict: For each page, extract /Resources /Font dict entries
//!
//! The manifest (tests/corpus/manifest.toml) tags each file with expected behavior.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use udoc_pdf::object::resolver::ObjectResolver;
use udoc_pdf::object::{ObjRef, PdfObject};
use udoc_pdf::parse::DocumentParser;
use udoc_pdf::CollectingDiagnostics;

const CORPUS_DIR: &str = "tests/corpus/minimal";
const MANIFEST_PATH: &str = "tests/corpus/manifest.toml";

// ---- Manifest types ----

#[derive(Deserialize)]
struct Manifest {
    files: BTreeMap<String, FileEntry>,
}

#[allow(dead_code)] // fields deserialized from manifest TOML but accessed structurally
#[derive(Deserialize)]
struct FileEntry {
    source: String,
    license: String,
    version: String,
    features: Vec<String>,
    expect: String,
}

fn load_manifest() -> Manifest {
    let content = std::fs::read_to_string(MANIFEST_PATH)
        .unwrap_or_else(|e| panic!("failed to read manifest: {e}"));
    toml::from_str(&content).unwrap_or_else(|e| panic!("failed to parse manifest: {e}"))
}

fn read_corpus(filename: &str) -> Vec<u8> {
    let path = format!("{}/{}", CORPUS_DIR, filename);
    std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {}: {}", path, e))
}

// ---- Level 1: Structure test ----

#[test]
fn corpus_level1_structure_all_files() {
    let manifest = load_manifest();
    let mut failures = Vec::new();

    for (filename, entry) in &manifest.files {
        let data = read_corpus(filename);
        let diag = Arc::new(CollectingDiagnostics::new());
        let result = DocumentParser::with_diagnostics(&data, diag.clone()).parse();

        match (&entry.expect[..], result) {
            ("ok" | "ok-with-warnings", Ok(doc)) => {
                if doc.xref.is_empty() {
                    failures.push(format!("{filename}: xref table is empty"));
                }
                if doc.trailer.get(b"Root").is_none() {
                    failures.push(format!("{filename}: trailer missing /Root"));
                }
            }
            ("parse-error", Err(_)) => {
                // Expected failure
            }
            ("parse-error", Ok(_)) => {
                failures.push(format!("{filename}: expected parse error, but succeeded"));
            }
            (_, Err(e)) => {
                failures.push(format!("{filename}: parse failed: {e}"));
            }
            (expect, Ok(_)) => {
                failures.push(format!(
                    "{filename}: unknown expect value '{expect}', parsed ok"
                ));
            }
        }
    }

    if !failures.is_empty() {
        panic!("Level 1 (structure) failures:\n  {}", failures.join("\n  "));
    }
}

// ---- Level 2: Resolution test ----

/// Walk the /Pages tree and collect all page dictionaries.
///
/// Safety against circular references: this relies on the resolver's cycle
/// detection (visited set) to break infinite loops. Production page-tree
/// walking in Phase 3 will add its own explicit depth limit.
fn collect_pages(
    resolver: &mut ObjectResolver,
    pages_ref: ObjRef,
) -> Result<Vec<udoc_pdf::object::PdfDictionary>, String> {
    let pages_dict = resolver
        .resolve_dict(pages_ref)
        .map_err(|e| format!("resolving /Pages: {e}"))?;

    let type_name = pages_dict.get_name(b"Type");

    match type_name {
        Some(b"Pages") => {
            let kids = pages_dict
                .get_array(b"Kids")
                .ok_or("missing /Kids in /Pages")?;
            let mut all_pages = Vec::new();
            for kid in kids {
                match kid {
                    PdfObject::Reference(r) => {
                        let mut sub = collect_pages(resolver, *r)?;
                        all_pages.append(&mut sub);
                    }
                    _ => return Err(format!("non-reference in /Kids: {kid}")),
                }
            }
            Ok(all_pages)
        }
        Some(b"Page") => Ok(vec![pages_dict]),
        Some(other) => Err(format!(
            "unexpected /Type: /{}",
            String::from_utf8_lossy(other)
        )),
        None => {
            // Some generators omit /Type on leaf pages
            if pages_dict.get(b"Kids").is_some() {
                // It's a Pages node
                let kids = pages_dict
                    .get_array(b"Kids")
                    .ok_or("missing /Kids in Pages node")?;
                let mut all_pages = Vec::new();
                for kid in kids {
                    match kid {
                        PdfObject::Reference(r) => {
                            let mut sub = collect_pages(resolver, *r)?;
                            all_pages.append(&mut sub);
                        }
                        _ => return Err(format!("non-reference in /Kids: {kid}")),
                    }
                }
                Ok(all_pages)
            } else {
                // Treat as a leaf Page
                Ok(vec![pages_dict])
            }
        }
    }
}

#[test]
fn corpus_level2_resolution_all_files() {
    let manifest = load_manifest();
    let mut failures = Vec::new();

    for (filename, entry) in &manifest.files {
        if entry.expect == "parse-error" {
            continue;
        }

        let data = read_corpus(filename);
        let diag = Arc::new(CollectingDiagnostics::new());
        let doc = match DocumentParser::with_diagnostics(&data, diag.clone()).parse() {
            Ok(d) => d,
            Err(_) => continue, // Level 1 already covers parse failures
        };

        let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag.clone());

        // Resolve /Root -> /Catalog
        let trailer = resolver.trailer().expect("trailer").clone();
        let root_ref = match trailer.get_ref(b"Root") {
            Some(r) => r,
            None => {
                failures.push(format!("{filename}: trailer has no /Root ref"));
                continue;
            }
        };

        let catalog = match resolver.resolve_dict(root_ref) {
            Ok(c) => c,
            Err(e) => {
                failures.push(format!("{filename}: failed to resolve /Root: {e}"));
                continue;
            }
        };

        // Resolve /Pages
        let pages_ref = match catalog.get_ref(b"Pages") {
            Some(r) => r,
            None => {
                failures.push(format!("{filename}: catalog has no /Pages ref"));
                continue;
            }
        };

        let pages = match collect_pages(&mut resolver, pages_ref) {
            Ok(p) => p,
            Err(e) => {
                // Malformed files may have unresolvable page references
                if entry.expect == "ok-with-warnings"
                    && entry.features.contains(&"malformed-xref".to_string())
                {
                    continue;
                }
                failures.push(format!("{filename}: failed to walk /Pages: {e}"));
                continue;
            }
        };

        if pages.is_empty() {
            failures.push(format!("{filename}: /Pages tree has 0 pages"));
        }
    }

    if !failures.is_empty() {
        panic!(
            "Level 2 (resolution) failures:\n  {}",
            failures.join("\n  ")
        );
    }
}

// ---- Level 3: Stream decode test ----

#[test]
fn corpus_level3_stream_decode() {
    let manifest = load_manifest();
    let mut failures = Vec::new();

    for (filename, entry) in &manifest.files {
        if entry.expect == "parse-error" {
            continue;
        }
        // Skip files with known malformed streams (wrong_length is tested separately)
        if entry.features.contains(&"wrong-stream-length".to_string()) {
            continue;
        }

        let data = read_corpus(filename);
        let diag = Arc::new(CollectingDiagnostics::new());
        let doc = match DocumentParser::with_diagnostics(&data, diag.clone()).parse() {
            Ok(d) => d,
            Err(_) => continue,
        };

        let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag.clone());

        let trailer = resolver.trailer().expect("trailer").clone();
        let root_ref = match trailer.get_ref(b"Root") {
            Some(r) => r,
            None => continue,
        };

        let catalog = match resolver.resolve_dict(root_ref) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let pages_ref = match catalog.get_ref(b"Pages") {
            Some(r) => r,
            None => continue,
        };

        let pages = match collect_pages(&mut resolver, pages_ref) {
            Ok(p) => p,
            Err(_) => continue,
        };

        for (i, page) in pages.iter().enumerate() {
            let contents = page.get(b"Contents");
            if contents.is_none() {
                continue; // empty page, no content stream
            }

            let contents = contents.unwrap();
            let stream_refs: Vec<ObjRef> = match contents {
                PdfObject::Reference(r) => vec![*r],
                PdfObject::Array(arr) => arr
                    .iter()
                    .filter_map(|o| {
                        if let PdfObject::Reference(r) = o {
                            Some(*r)
                        } else {
                            None
                        }
                    })
                    .collect(),
                _ => continue,
            };

            for sref in stream_refs {
                let stream = match resolver.resolve_stream(sref) {
                    Ok(s) => s,
                    Err(e) => {
                        failures.push(format!(
                            "{filename} page {i}: failed to resolve content stream {sref}: {e}"
                        ));
                        continue;
                    }
                };

                if let Err(e) = resolver.decode_stream_data(&stream, Some(sref)) {
                    failures.push(format!(
                        "{filename} page {i}: failed to decode content stream {sref}: {e}"
                    ));
                }
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "Level 3 (stream decode) failures:\n  {}",
            failures.join("\n  ")
        );
    }
}

// ---- Level 4: Font dict test ----

#[test]
fn corpus_level4_font_dicts() {
    let manifest = load_manifest();
    let mut failures = Vec::new();

    for (filename, entry) in &manifest.files {
        if entry.expect == "parse-error" {
            continue;
        }

        let data = read_corpus(filename);
        let diag = Arc::new(CollectingDiagnostics::new());
        let doc = match DocumentParser::with_diagnostics(&data, diag.clone()).parse() {
            Ok(d) => d,
            Err(_) => continue,
        };

        let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag.clone());

        let trailer = resolver.trailer().expect("trailer").clone();
        let root_ref = match trailer.get_ref(b"Root") {
            Some(r) => r,
            None => continue,
        };

        let catalog = match resolver.resolve_dict(root_ref) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let pages_ref = match catalog.get_ref(b"Pages") {
            Some(r) => r,
            None => continue,
        };

        let pages = match collect_pages(&mut resolver, pages_ref) {
            Ok(p) => p,
            Err(_) => continue,
        };

        for (i, page) in pages.iter().enumerate() {
            // Get /Resources (may be inherited, but for our corpus it's on the page)
            let resources = match resolver.get_resolved_dict(page, b"Resources") {
                Ok(Some(r)) => r,
                Ok(None) => continue, // no resources
                Err(e) => {
                    failures.push(format!(
                        "{filename} page {i}: failed to resolve /Resources: {e}"
                    ));
                    continue;
                }
            };

            // Get /Font dict from resources
            let font_dict = match resolver.get_resolved_dict(&resources, b"Font") {
                Ok(Some(f)) => f,
                Ok(None) => continue, // no fonts
                Err(e) => {
                    failures.push(format!(
                        "{filename} page {i}: failed to resolve /Font dict: {e}"
                    ));
                    continue;
                }
            };

            // Resolve each font entry
            for (name, value) in font_dict.iter() {
                if let PdfObject::Reference(r) = value {
                    if let Err(e) = resolver.resolve_dict(*r) {
                        failures.push(format!(
                            "{filename} page {i}: failed to resolve font /{}: {e}",
                            String::from_utf8_lossy(name)
                        ));
                    }
                }
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "Level 4 (font dicts) failures:\n  {}",
            failures.join("\n  ")
        );
    }
}

// ---- Specific feature validation tests ----

/// Verify xref stream files parse and can resolve /Root from ObjStm.
#[test]
fn corpus_xref_stream_files_resolve_root() {
    let manifest = load_manifest();
    let xref_stream_files: Vec<&str> = manifest
        .files
        .iter()
        .filter(|(_, e)| e.features.contains(&"xref-stream".to_string()))
        .map(|(name, _)| name.as_str())
        .collect();

    assert!(
        xref_stream_files.len() >= 4,
        "expected at least 4 xref-stream files, found {}",
        xref_stream_files.len()
    );

    for filename in xref_stream_files {
        let data = read_corpus(filename);
        let diag = Arc::new(CollectingDiagnostics::new());
        let doc = DocumentParser::with_diagnostics(&data, diag.clone())
            .parse()
            .unwrap_or_else(|e| panic!("{filename}: {e}"));

        let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag);
        let trailer = resolver.trailer().expect("trailer").clone();
        let root_ref = trailer
            .get_ref(b"Root")
            .unwrap_or_else(|| panic!("{filename}: no /Root"));
        let catalog = resolver
            .resolve_dict(root_ref)
            .unwrap_or_else(|e| panic!("{filename}: resolve /Root: {e}"));
        assert_eq!(
            catalog.get_name(b"Type"),
            Some(b"Catalog".as_slice()),
            "{filename}: /Root is not /Catalog"
        );
    }
}

/// Verify the malformed xref entry file parses with warnings.
#[test]
fn corpus_bad_xref_entry_warns() {
    let data = read_corpus("bad_xref_entry.pdf");
    let diag = Arc::new(CollectingDiagnostics::new());
    let doc = DocumentParser::with_diagnostics(&data, diag.clone())
        .parse()
        .expect("bad_xref_entry.pdf should parse (with warnings)");

    let warnings = diag.warnings();
    assert!(
        !warnings.is_empty(),
        "bad_xref_entry.pdf should emit at least one warning"
    );
    assert!(
        warnings.iter().any(|w| w.message.contains("malformed")),
        "expected a 'malformed' warning, got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );

    // Catalog should still be resolvable
    let mut resolver = ObjectResolver::from_document(&data, doc);
    let trailer = resolver.trailer().expect("trailer").clone();
    let root_ref = trailer.get_ref(b"Root").expect("/Root");
    resolver
        .resolve_dict(root_ref)
        .expect("should resolve catalog despite bad xref entry");
}

/// Verify incremental update PDF (xelatex-drawboard, 3 revisions).
#[test]
fn corpus_incremental_update_resolves() {
    let data = read_corpus("xelatex-drawboard.pdf");
    let diag = Arc::new(CollectingDiagnostics::new());
    let doc = DocumentParser::with_diagnostics(&data, diag.clone())
        .parse()
        .expect("xelatex-drawboard.pdf should parse");

    // Should have entries from multiple xref sections
    assert!(
        doc.xref.len() > 5,
        "expected many xref entries from incremental updates, got {}",
        doc.xref.len()
    );

    let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag);
    let trailer = resolver.trailer().expect("trailer").clone();
    let root_ref = trailer.get_ref(b"Root").expect("/Root");
    let catalog = resolver
        .resolve_dict(root_ref)
        .expect("should resolve /Root");
    assert_eq!(catalog.get_name(b"Type"), Some(b"Catalog".as_slice()));
}

/// Verify FlateDecode content streams decode successfully.
#[test]
fn corpus_flate_streams_decode() {
    for filename in &["flate_content.pdf", "two_flate_streams.pdf"] {
        let data = read_corpus(filename);
        let diag = Arc::new(CollectingDiagnostics::new());
        let doc = DocumentParser::with_diagnostics(&data, diag.clone())
            .parse()
            .unwrap_or_else(|e| panic!("{filename}: {e}"));

        let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag);
        let trailer = resolver.trailer().expect("trailer").clone();
        let root_ref = trailer.get_ref(b"Root").expect("/Root");
        let catalog = resolver.resolve_dict(root_ref).expect("catalog");
        let pages_ref = catalog.get_ref(b"Pages").expect("/Pages");
        let pages =
            collect_pages(&mut resolver, pages_ref).unwrap_or_else(|e| panic!("{filename}: {e}"));

        for (i, page) in pages.iter().enumerate() {
            if let Some(PdfObject::Reference(r)) = page.get(b"Contents") {
                let stream = resolver
                    .resolve_stream(*r)
                    .unwrap_or_else(|e| panic!("{filename} page {i}: {e}"));
                let decoded = resolver
                    .decode_stream_data(&stream, Some(*r))
                    .unwrap_or_else(|e| panic!("{filename} page {i} decode: {e}"));
                assert!(
                    !decoded.is_empty(),
                    "{filename} page {i}: decoded content is empty"
                );
            }
        }
    }
}

/// Verify multi-page PDF has the right page count.
#[test]
fn corpus_multipage_count() {
    let data = read_corpus("multipage.pdf");
    let doc = DocumentParser::new(&data).parse().expect("should parse");
    let mut resolver = ObjectResolver::from_document(&data, doc);
    let trailer = resolver.trailer().expect("trailer").clone();
    let root_ref = trailer.get_ref(b"Root").expect("/Root");
    let catalog = resolver.resolve_dict(root_ref).expect("catalog");
    let pages_ref = catalog.get_ref(b"Pages").expect("/Pages");
    let pages = collect_pages(&mut resolver, pages_ref).expect("pages tree");
    assert_eq!(pages.len(), 5, "multipage.pdf should have 5 pages");
}

/// Verify nested Pages tree works.
#[test]
fn corpus_nested_pages_tree() {
    let data = read_corpus("nested_pages.pdf");
    let doc = DocumentParser::new(&data).parse().expect("should parse");
    let mut resolver = ObjectResolver::from_document(&data, doc);
    let trailer = resolver.trailer().expect("trailer").clone();
    let root_ref = trailer.get_ref(b"Root").expect("/Root");
    let catalog = resolver.resolve_dict(root_ref).expect("catalog");
    let pages_ref = catalog.get_ref(b"Pages").expect("/Pages");
    let pages = collect_pages(&mut resolver, pages_ref).expect("pages tree");
    assert_eq!(pages.len(), 2, "nested_pages.pdf should have 2 pages");
}

/// Verify content array (multiple content streams per page).
#[test]
fn corpus_content_array() {
    let data = read_corpus("content_array.pdf");
    let doc = DocumentParser::new(&data).parse().expect("should parse");
    let mut resolver = ObjectResolver::from_document(&data, doc);
    let trailer = resolver.trailer().expect("trailer").clone();
    let root_ref = trailer.get_ref(b"Root").expect("/Root");
    let catalog = resolver.resolve_dict(root_ref).expect("catalog");
    let pages_ref = catalog.get_ref(b"Pages").expect("/Pages");
    let pages = collect_pages(&mut resolver, pages_ref).expect("pages tree");
    assert_eq!(pages.len(), 1);

    // /Contents should be an array
    let contents = pages[0].get(b"Contents").expect("/Contents");
    assert!(
        contents.as_array().is_some(),
        "expected /Contents to be an array"
    );
}

// ---- Level 5: Text extraction test ----

/// Extract text from corpus PDFs using the content interpreter.
///
/// This exercises the full pipeline: parse -> resolve -> decode stream ->
/// load fonts -> interpret content stream -> produce TextSpans.
#[test]
fn corpus_level5_text_extraction() {
    use udoc_pdf::content::interpreter::{get_page_content, ContentInterpreter};

    let manifest = load_manifest();
    let mut total_spans = 0;
    let mut files_with_text = 0;
    let mut failures = Vec::new();

    for (filename, entry) in &manifest.files {
        if entry.expect == "parse-error" {
            continue;
        }

        let data = read_corpus(filename);
        let diag = Arc::new(CollectingDiagnostics::new());
        let doc = match DocumentParser::with_diagnostics(&data, diag.clone()).parse() {
            Ok(d) => d,
            Err(_) => continue,
        };

        let mut resolver = ObjectResolver::from_document_with_diagnostics(&data, doc, diag.clone());

        let trailer = resolver.trailer().expect("trailer").clone();
        let root_ref = match trailer.get_ref(b"Root") {
            Some(r) => r,
            None => continue,
        };

        let catalog = match resolver.resolve_dict(root_ref) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let pages_ref = match catalog.get_ref(b"Pages") {
            Some(r) => r,
            None => continue,
        };

        let pages = match collect_pages(&mut resolver, pages_ref) {
            Ok(p) => p,
            Err(_) => continue,
        };

        let mut file_spans = 0;

        for (i, page) in pages.iter().enumerate() {
            // Resolve resources and content, then interpret, all in one scope
            let resources = match resolver.get_resolved_dict(page, b"Resources") {
                Ok(Some(r)) => r,
                _ => udoc_pdf::object::PdfDictionary::new(),
            };

            let content = match get_page_content(&mut resolver, page, Some(i)) {
                Ok(c) => c,
                Err(e) => {
                    if !entry.features.contains(&"wrong-stream-length".to_string()) {
                        failures.push(format!("{filename} page {i}: get_page_content failed: {e}"));
                    }
                    continue;
                }
            };

            if content.is_empty() {
                continue;
            }

            let mut interp =
                ContentInterpreter::new(&resources, &mut resolver, diag.clone(), Some(i));
            match interp.interpret(&content) {
                Ok(spans) => {
                    file_spans += spans.len();
                    total_spans += spans.len();
                }
                Err(e) => {
                    failures.push(format!("{filename} page {i}: interpret failed: {e}"));
                }
            }
        }

        if file_spans > 0 {
            files_with_text += 1;
        }
    }

    if !failures.is_empty() {
        panic!(
            "Level 5 (text extraction) failures:\n  {}",
            failures.join("\n  ")
        );
    }

    // At least 3 corpus PDFs should produce text (acceptance criterion)
    assert!(
        files_with_text >= 3,
        "expected at least 3 corpus PDFs with extracted text, got {files_with_text}"
    );
    assert!(
        total_spans > 0,
        "expected non-zero total text spans across corpus"
    );
}

// ---- Sprint 12: Feature-specific corpus tests ----
//
// These use the public API (Document/Page) rather than internal types, so
// they are resilient to internal refactoring.

/// Helper: open a single-page corpus PDF via the public API.
fn open_corpus_page(filename: &str) -> udoc_pdf::Document {
    let path = format!("{}/{}", CORPUS_DIR, filename);
    udoc_pdf::Document::open(&path).unwrap_or_else(|e| panic!("{filename} should open: {e}"))
}

/// Verify inline image PDF extracts text before and after the image.
#[test]
fn corpus_inline_image_text_extraction() {
    let mut doc = open_corpus_page("inline_image.pdf");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text");
    assert!(
        text.contains("Hello"),
        "inline image PDF text should contain 'Hello', got: {text:?}"
    );
    assert!(
        text.contains("World"),
        "inline image PDF text should contain 'World', got: {text:?}"
    );
}

/// Verify image XObject PDF extracts text alongside the image.
#[test]
fn corpus_image_xobject_text_extraction() {
    let mut doc = open_corpus_page("image_xobject.pdf");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text");
    assert!(
        !text.is_empty(),
        "image xobject PDF should produce non-empty text"
    );
}

/// Verify the multi-column corpus PDF produces multiple text spans.
#[test]
fn corpus_two_column_text_extraction() {
    let mut doc = open_corpus_page("two_column.pdf");
    let mut page = doc.page(0).expect("page 0");
    let spans = page.raw_spans().expect("raw_spans");
    assert!(
        spans.len() >= 4,
        "two_column.pdf should produce multiple text spans, got {}",
        spans.len()
    );
}

/// Verify the rotated text corpus PDF produces rotated spans.
#[test]
fn corpus_rotated_text_extraction() {
    let mut doc = open_corpus_page("rotated_text.pdf");
    let mut page = doc.page(0).expect("page 0");
    let spans = page.raw_spans().expect("raw_spans");
    assert!(
        !spans.is_empty(),
        "rotated_text.pdf should produce text spans"
    );
    let has_rotated = spans.iter().any(|s| s.rotation.abs() > 45.0);
    assert!(
        has_rotated,
        "rotated_text.pdf should have at least one rotated span"
    );
}

/// Verify the table layout corpus PDF reads as row-first order.
#[test]
fn corpus_table_layout_extraction() {
    let mut doc = open_corpus_page("table_layout.pdf");
    let mut page = doc.page(0).expect("page 0");
    let text = page.text().expect("text");
    assert!(
        !text.is_empty(),
        "table_layout.pdf should produce non-empty text"
    );
    // Table rows should stay together (not split into columns).
    // Check that "Name" and "Score" are on the same line.
    let has_header_row = text
        .lines()
        .any(|line| line.contains("Name") && line.contains("Score"));
    assert!(
        has_header_row,
        "table_layout.pdf should have 'Name' and 'Score' on the same line, got:\n{text}"
    );
}

/// Verify all Sprint 12 manifest-tagged features parse at structure level.
#[test]
fn corpus_sprint12_features_structure() {
    let manifest = load_manifest();
    let sprint12_features = [
        "inline-image",
        "image-xobject",
        "multi-column",
        "rotated-text",
        "table-layout",
    ];
    let mut tested = 0;

    for (filename, entry) in &manifest.files {
        if !entry
            .features
            .iter()
            .any(|f| sprint12_features.contains(&f.as_str()))
        {
            continue;
        }

        let data = read_corpus(filename);
        let diag = Arc::new(CollectingDiagnostics::new());
        let result = DocumentParser::with_diagnostics(&data, diag.clone()).parse();

        match entry.expect.as_str() {
            "ok" | "ok-with-warnings" => {
                let doc = result.unwrap_or_else(|e| panic!("{filename}: {e}"));
                assert!(!doc.xref.is_empty(), "{filename}: xref should not be empty");
                assert!(
                    doc.trailer.get(b"Root").is_some(),
                    "{filename}: trailer should have /Root"
                );
            }
            "parse-error" => {
                assert!(result.is_err(), "{filename}: expected parse error");
            }
            other => {
                panic!("{filename}: unknown expect value '{other}'");
            }
        }
        tested += 1;
    }

    assert!(
        tested >= 6,
        "expected at least 6 Sprint 12 feature PDFs, tested {tested}"
    );
}
