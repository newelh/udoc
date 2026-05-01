use criterion::{black_box, criterion_group, criterion_main, Criterion};
use udoc_pdf::parse::Lexer;

fn bench_lex_integers(c: &mut Criterion) {
    let data = b"123 456 789 -1 -999 0 42 1234567890";
    c.bench_function("lex_integers", |b| {
        b.iter(|| {
            let mut lexer = Lexer::new(black_box(data));
            while !matches!(lexer.next_token(), udoc_pdf::parse::Token::Eof) {}
        });
    });
}

fn bench_lex_strings(c: &mut Criterion) {
    let nested = b"(outer (middle (inner)))";
    c.bench_function("lex_nested_string", |b| {
        b.iter(|| {
            let mut lexer = Lexer::new(black_box(nested));
            lexer.next_token();
        });
    });
}

criterion_group!(benches, bench_lex_integers, bench_lex_strings);
criterion_main!(benches);
