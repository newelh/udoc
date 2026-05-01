use criterion::{criterion_group, criterion_main, Criterion};
use udoc_pdf::Document;

fn bench_simple_table(c: &mut Criterion) {
    let path = "tests/corpus/minimal/table_layout.pdf";
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => {
            eprintln!("skipping bench_simple_table: {path} not found");
            return;
        }
    };

    c.bench_function("table_simple", |b| {
        b.iter(|| {
            let mut doc = Document::from_bytes(data.clone()).unwrap();
            let mut page = doc.page(0).unwrap();
            let _ = page.tables();
        });
    });
}

fn bench_complex_table(c: &mut Criterion) {
    let path = "tests/corpus/realworld/irs_1040.pdf";
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => {
            eprintln!("skipping bench_complex_table: {path} not found");
            return;
        }
    };

    // IRS 1040 is a multi-page form with many ruled lines and grid structures.
    // Benchmark page 0 which has dense tabular layout.
    c.bench_function("table_complex", |b| {
        b.iter(|| {
            let mut doc = Document::from_bytes(data.clone()).unwrap();
            let mut page = doc.page(0).unwrap();
            let _ = page.tables();
        });
    });
}

fn bench_no_tables(c: &mut Criterion) {
    let path = "tests/corpus/minimal/xelatex.pdf";
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => {
            eprintln!("skipping bench_no_tables: {path} not found");
            return;
        }
    };

    // Text-only page: measures overhead of table detection when no tables exist.
    c.bench_function("table_none", |b| {
        b.iter(|| {
            let mut doc = Document::from_bytes(data.clone()).unwrap();
            let mut page = doc.page(0).unwrap();
            let _ = page.tables();
        });
    });
}

criterion_group!(
    benches,
    bench_simple_table,
    bench_complex_table,
    bench_no_tables
);
criterion_main!(benches);
