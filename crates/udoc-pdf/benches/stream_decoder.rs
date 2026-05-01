use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::Write;
use udoc_pdf::object::{decode_stream, DecodeLimits, PdfDictionary, PdfObject};
use udoc_pdf::NullDiagnostics;

fn make_flate_stream(size: usize) -> (Vec<u8>, PdfDictionary) {
    let input: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&input).unwrap();
    let compressed = encoder.finish().unwrap();

    let mut dict = PdfDictionary::new();
    dict.insert(b"Filter".to_vec(), PdfObject::Name(b"FlateDecode".to_vec()));
    (compressed, dict)
}

fn bench_flate_decode(c: &mut Criterion) {
    let limits = DecodeLimits::default();
    let diag = NullDiagnostics;

    let mut group = c.benchmark_group("flate_decode");
    for size in [1024, 100_000, 1_000_000] {
        let (compressed, dict) = make_flate_stream(size);
        group.bench_with_input(BenchmarkId::new("bytes", size), &size, |b, _| {
            b.iter(|| {
                decode_stream(black_box(&compressed), &dict, &limits, &diag, 0).unwrap();
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_flate_decode);
criterion_main!(benches);
