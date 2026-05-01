//! Extracts fuzz seeds from corpus PDFs.
//!
//! Run with: cargo test --test extract_fuzz_seeds -- --ignored
//! Only needs to run when adding/modifying corpus PDFs. Output is committed.

use std::path::Path;

use udoc_pdf::content::interpreter::get_page_content;
use udoc_pdf::object::resolver::ObjectResolver;
use udoc_pdf::object::{ObjRef, PdfObject};
use udoc_pdf::parse::DocumentParser;

const CORPUS_DIR: &str = "tests/corpus/minimal";
const CONTENT_SEEDS_DIR: &str = "fuzz/seeds/fuzz_content";
const PUBLIC_API_SEEDS_DIR: &str = "fuzz/seeds/fuzz_public_api";

fn read_corpus(filename: &str) -> Vec<u8> {
    let path = format!("{CORPUS_DIR}/{filename}");
    std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
}

fn write_if_changed(path: &Path, data: &[u8]) {
    if path.exists() {
        if let Ok(existing) = std::fs::read(path) {
            if existing == data {
                return;
            }
        }
    }
    std::fs::write(path, data)
        .unwrap_or_else(|e| panic!("failed to write {}: {}", path.display(), e));
    eprintln!("  wrote {} ({} bytes)", path.display(), data.len());
}

/// Extract decoded content stream bytes from each page of a PDF.
fn extract_content_streams(pdf_data: &[u8]) -> Vec<Vec<u8>> {
    let parser = DocumentParser::new(pdf_data);
    let structure = match parser.parse() {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let xref = structure.xref.clone();
    let mut resolver = ObjectResolver::new(pdf_data, xref);

    // Walk the page tree
    let root_ref = match structure.trailer.get(b"Root") {
        Some(PdfObject::Reference(r)) => *r,
        _ => return Vec::new(),
    };
    let root = match resolver.resolve(root_ref) {
        Ok(obj) => obj,
        Err(_) => return Vec::new(),
    };
    let pages_ref = match root.as_dict().and_then(|d| d.get(b"Pages")) {
        Some(PdfObject::Reference(r)) => *r,
        _ => return Vec::new(),
    };

    let page_refs = collect_page_refs(&mut resolver, pages_ref);
    let mut streams = Vec::new();

    for (i, page_ref) in page_refs.iter().enumerate() {
        let page_dict = match resolver.resolve(*page_ref) {
            Ok(obj) => obj,
            Err(_) => continue,
        };
        let dict = match page_dict.as_dict() {
            Some(d) => d,
            None => continue,
        };
        match get_page_content(&mut resolver, dict, Some(i)) {
            Ok(data) if !data.is_empty() => streams.push(data),
            _ => {}
        }
    }

    streams
}

fn collect_page_refs(resolver: &mut ObjectResolver<'_>, node_ref: ObjRef) -> Vec<ObjRef> {
    let node = match resolver.resolve(node_ref) {
        Ok(obj) => obj,
        Err(_) => return Vec::new(),
    };
    let dict = match node.as_dict() {
        Some(d) => d,
        None => return Vec::new(),
    };

    let type_name = dict.get(b"Type").and_then(|o| o.as_name()).unwrap_or(b"");

    if type_name == b"Page" {
        return vec![node_ref];
    }

    // Pages node: recurse into Kids
    let kids = match dict.get(b"Kids") {
        Some(PdfObject::Array(arr)) => arr.clone(),
        _ => return Vec::new(),
    };

    let mut pages = Vec::new();
    for kid in &kids {
        if let Some(r) = kid.as_reference() {
            pages.extend(collect_page_refs(resolver, r));
        }
    }
    pages
}

#[test]
#[ignore] // Only run manually: cargo test --test extract_fuzz_seeds -- --ignored
fn extract_all_fuzz_seeds() {
    let content_dir = Path::new(CONTENT_SEEDS_DIR);
    let api_dir = Path::new(PUBLIC_API_SEEDS_DIR);
    assert!(content_dir.exists(), "create {CONTENT_SEEDS_DIR} first");
    assert!(api_dir.exists(), "create {PUBLIC_API_SEEDS_DIR} first");

    let mut entries: Vec<_> = std::fs::read_dir(CORPUS_DIR)
        .expect("failed to read corpus dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "pdf"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let mut content_count = 0;
    let mut api_count = 0;

    for entry in &entries {
        let filename = entry.file_name();
        let name = filename.to_string_lossy();
        let stem = name.trim_end_matches(".pdf");
        let pdf_data = read_corpus(&name);

        // Seed for fuzz_public_api: whole PDF file
        let api_path = api_dir.join(&*name);
        write_if_changed(&api_path, &pdf_data);
        api_count += 1;

        // Seeds for fuzz_content: extracted content streams
        let streams = extract_content_streams(&pdf_data);
        for (i, stream) in streams.iter().enumerate() {
            let seed_name = if streams.len() == 1 {
                stem.to_string()
            } else {
                format!("{stem}_p{i}")
            };
            let seed_path = content_dir.join(&seed_name);
            write_if_changed(&seed_path, stream);
            content_count += 1;
        }
    }

    eprintln!("Done: {content_count} content seeds, {api_count} public API seeds.");
}
