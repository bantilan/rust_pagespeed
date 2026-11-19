//! Benchmarks for tokenizing and minifying (Step 7 gate).
use criterion::{criterion_group, criterion_main, Criterion};
use ps_js::minify::{legacy_minify_js, minify_utf8};
use ps_js::tokenizer::{Tokenizer, TokenizerPatterns};
use std::hint::black_box;

const SAMPLE: &[u8] = br#"
(function (global) {
  // A comment worth removing.
  var greeting = 'hello, world';
  function add(a, b) { return a + b; }
  var re = /ab+c/gi;
  for (var i = 0; i < 100; i++) {
    global.total += add(i, parseFloat('1.5'));
  }
})(this);
"#;

fn bench_tokenize(criterion: &mut Criterion) {
    criterion.bench_function("tokenize", |bencher| {
        let patterns = TokenizerPatterns::new();
        bencher.iter(|| {
            let mut tokenizer = Tokenizer::new(&patterns, black_box(SAMPLE));
            let mut count = 0;
            loop {
                let (kind, _) = tokenizer.next_token();
                if matches!(
                    kind,
                    ps_js::keywords::TokenType::EndOfInput | ps_js::keywords::TokenType::Error
                ) {
                    break;
                }
                count += 1;
            }
            black_box(count)
        });
    });
}

fn bench_minify(criterion: &mut Criterion) {
    criterion.bench_function("minify_utf8", |bencher| {
        let patterns = TokenizerPatterns::new();
        bencher.iter(|| black_box(minify_utf8(&patterns, black_box(SAMPLE))));
    });
    criterion.bench_function("legacy_minify", |bencher| {
        bencher.iter(|| black_box(legacy_minify_js(black_box(SAMPLE))));
    });
}

criterion_group!(benches, bench_tokenize, bench_minify);
criterion_main!(benches);
