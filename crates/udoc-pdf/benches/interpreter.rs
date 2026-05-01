use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::sync::Arc;
use udoc_pdf::content::interpreter::ContentInterpreter;
use udoc_pdf::object::{ObjectResolver, PdfDictionary};
use udoc_pdf::parse::XrefTable;
use udoc_pdf::NullDiagnostics;

/// Build a content stream with N text-showing operations.
fn make_text_content(n: usize) -> Vec<u8> {
    let mut content = Vec::with_capacity(n * 10 + 30);
    content.extend_from_slice(b"BT /F1 12 Tf ");
    for _ in 0..n {
        content.extend_from_slice(b"(Hello) Tj ");
    }
    content.extend_from_slice(b"ET");
    content
}

/// Build a content stream with N inline images interspersed with text.
fn make_mixed_content(n_text: usize, n_images: usize) -> Vec<u8> {
    let mut content = Vec::with_capacity(n_text * 10 + n_images * 50 + 30);
    content.extend_from_slice(b"BT /F1 12 Tf ");
    for i in 0..(n_text + n_images) {
        if i % 5 == 0 && n_images > 0 {
            content.extend_from_slice(b"ET BI /W 1 /H 1 /CS /G /BPC 8 ID \x00 EI BT /F1 12 Tf ");
        } else {
            content.extend_from_slice(b"(Hello) Tj ");
        }
    }
    content.extend_from_slice(b"ET");
    content
}

fn bench_text_only(c: &mut Criterion) {
    let content = make_text_content(1000);
    let resources = PdfDictionary::new();
    let pdf_data = b"%PDF-1.4\n";
    let xref = XrefTable::default();

    c.bench_function("interpret_text_only_1000", |b| {
        b.iter(|| {
            let mut resolver = ObjectResolver::new(pdf_data.as_slice(), xref.clone());
            let diag = Arc::new(NullDiagnostics);
            let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
            interp.set_extract_images(false);
            let _ = interp.interpret(black_box(&content));
        });
    });
}

fn bench_with_images(c: &mut Criterion) {
    let content = make_mixed_content(800, 200);
    let resources = PdfDictionary::new();
    let pdf_data = b"%PDF-1.4\n";
    let xref = XrefTable::default();

    c.bench_function("interpret_with_images_1000", |b| {
        b.iter(|| {
            let mut resolver = ObjectResolver::new(pdf_data.as_slice(), xref.clone());
            let diag = Arc::new(NullDiagnostics);
            let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
            let _ = interp.interpret(black_box(&content));
        });
    });
}

fn bench_text_only_no_images(c: &mut Criterion) {
    // Same mixed content but with extract_images=false to measure the skip path
    let content = make_mixed_content(800, 200);
    let resources = PdfDictionary::new();
    let pdf_data = b"%PDF-1.4\n";
    let xref = XrefTable::default();

    c.bench_function("interpret_mixed_skip_images_1000", |b| {
        b.iter(|| {
            let mut resolver = ObjectResolver::new(pdf_data.as_slice(), xref.clone());
            let diag = Arc::new(NullDiagnostics);
            let mut interp = ContentInterpreter::new(&resources, &mut resolver, diag, None);
            interp.set_extract_images(false);
            let _ = interp.interpret(black_box(&content));
        });
    });
}

criterion_group!(
    benches,
    bench_text_only,
    bench_with_images,
    bench_text_only_no_images
);
criterion_main!(benches);
