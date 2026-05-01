use criterion::{criterion_group, criterion_main, Criterion};
use udoc_pdf::Document;

const CORPUS_DIR: &str = "tests/corpus/minimal";

fn bench_end_to_end(c: &mut Criterion) {
    // Collect corpus PDFs that exist
    let corpus_files: Vec<_> = std::fs::read_dir(CORPUS_DIR)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "pdf"))
        .map(|e| e.path())
        .collect();

    if corpus_files.is_empty() {
        return;
    }

    let mut group = c.benchmark_group("end_to_end");

    for path in &corpus_files[..corpus_files.len().min(10)] {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let data = std::fs::read(path).unwrap();

        group.bench_function(&name, |b| {
            b.iter(|| {
                let mut doc = Document::from_bytes(data.clone()).unwrap();
                for i in 0..doc.page_count() {
                    let mut page = doc.page(i).unwrap();
                    let _ = page.text();
                }
            });
        });
    }
    group.finish();
}

fn bench_encrypted_end_to_end(c: &mut Criterion) {
    let encrypted_dir = "tests/corpus/encrypted";

    // (filename, password)
    let cases: &[(&str, &[u8])] = &[
        ("rc4_40_empty_password.pdf", b""),
        ("rc4_128_user_password.pdf", b"test123"),
        ("rc4_128_objstm.pdf", b""),
        ("aes128_empty_password.pdf", b""),
        ("aes128_user_password.pdf", b"aespass"),
        ("aes128_both_passwords.pdf", b"user_aes"),
    ];

    let mut group = c.benchmark_group("encrypted_end_to_end");

    for (filename, password) in cases {
        let path = format!("{}/{}", encrypted_dir, filename);
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(_) => continue,
        };

        let name = filename.trim_end_matches(".pdf");
        let pw = password.to_vec();
        group.bench_function(name, |b| {
            // clone cost is included; from_bytes_with_password takes ownership
            b.iter(|| {
                let mut doc = Document::from_bytes_with_password(data.clone(), pw.clone()).unwrap();
                for i in 0..doc.page_count() {
                    let mut page = doc.page(i).unwrap();
                    let _ = page.text();
                }
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_end_to_end, bench_encrypted_end_to_end);
criterion_main!(benches);
