use criterion::{criterion_group, criterion_main, Criterion};
use udoc_containers::test_util::{build_stored_zip, DOCX_CONTENT_TYPES, DOCX_PACKAGE_RELS};
use udoc_core::backend::{FormatBackend, PageExtractor};
use udoc_docx::DocxDocument;

/// Build a synthetic DOCX with multiple paragraphs for benchmarking.
///
/// More content than the minimal unit test fixture so the benchmark has
/// something meaningful to chew on.
fn make_bench_docx() -> Vec<u8> {
    // Generate a body with 100 paragraphs, each containing two runs (one styled).
    let mut body = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:body>
"#,
    );

    for i in 0..100 {
        body.push_str(&format!(
            r#"        <w:p>
            <w:r>
                <w:rPr><w:b/></w:rPr>
                <w:t xml:space="preserve">Paragraph {i} bold run. </w:t>
            </w:r>
            <w:r>
                <w:t>The quick brown fox jumps over the lazy dog.</w:t>
            </w:r>
        </w:p>
"#
        ));
    }

    body.push_str(
        r#"    </w:body>
</w:document>"#,
    );

    let body_bytes = body.into_bytes();

    build_stored_zip(&[
        ("[Content_Types].xml", DOCX_CONTENT_TYPES),
        ("_rels/.rels", DOCX_PACKAGE_RELS),
        ("word/document.xml", &body_bytes),
    ])
}

fn bench_docx_page_text_library_api(c: &mut Criterion) {
    let data = make_bench_docx();

    c.bench_function("docx_page_text_library_api", |b| {
        b.iter(|| {
            let mut doc = DocxDocument::from_bytes(&data).expect("from_bytes");
            let mut page = doc.page(0).expect("page 0");
            let _ = page.text().expect("text()");
        });
    });
}

fn bench_docx_from_bytes_only(c: &mut Criterion) {
    let data = make_bench_docx();

    c.bench_function("docx_from_bytes", |b| {
        b.iter(|| {
            let _ = DocxDocument::from_bytes(&data).expect("from_bytes");
        });
    });
}

fn bench_docx_page_text_only(c: &mut Criterion) {
    let data = make_bench_docx();

    c.bench_function("docx_page_text_only", |b| {
        b.iter_batched(
            || DocxDocument::from_bytes(&data).expect("bench setup: from_bytes"),
            |mut doc| {
                let mut page = doc.page(0).expect("bench: page 0");
                criterion::black_box(page.text().expect("bench: text()"))
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

criterion_group!(
    benches,
    bench_docx_page_text_library_api,
    bench_docx_from_bytes_only,
    bench_docx_page_text_only
);
criterion_main!(benches);
