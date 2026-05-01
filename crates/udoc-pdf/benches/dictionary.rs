use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use udoc_pdf::object::{PdfDictionary, PdfObject};

fn create_dict_with_n_entries(n: usize) -> PdfDictionary {
    let mut dict = PdfDictionary::new();
    for i in 0..n {
        dict.insert(
            format!("Key{}", i).into_bytes(),
            PdfObject::Integer(i as i64),
        );
    }
    dict
}

fn bench_dict_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("dict_lookup");
    for size in [5, 10, 20, 50, 100] {
        let dict = create_dict_with_n_entries(size);
        let key = format!("Key{}", size / 2).into_bytes();
        group.bench_with_input(BenchmarkId::from_parameter(size), &dict, |b, dict| {
            b.iter(|| dict.get(black_box(&key)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_dict_lookup);
criterion_main!(benches);
