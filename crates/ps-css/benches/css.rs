//! Benchmarks for parsing, minifying, and scanning (Step 2 gate).
use criterion::{criterion_group, criterion_main, Criterion};
use ps_css::minify::Minifier;
use ps_css::parser::Parser;
use ps_css::scanner::{transform_urls, TransformStatus, Transformer};
use std::hint::black_box;

const SAMPLE: &[u8] = br#"
@import url(base.css) screen;
@font-face {
  font-family: MainText;
  src: url(gentium.woff) format('woff');
  font-weight: 400 800;
}
.background_red { background-color: red; }
ul.nav > li.item:first-child {
  background: #ffffff url(/images/sprite.png) no-repeat 0px 0px;
  font: italic small-caps bold 16px/1.5 Georgia, serif;
  margin: 0.25em 0px 10%;
}
@media screen and (max-width: 600px) {
  .sidebar { display: none; width: calc(100% - 20px); }
}
"#;

struct CountTransformer {
    count: usize,
}

impl Transformer for CountTransformer {
    fn transform(&mut self, url: &mut Vec<u8>) -> TransformStatus {
        self.count += 1;
        let _ = url;
        TransformStatus::NoChange
    }
}

fn bench_parse(criterion: &mut Criterion) {
    criterion.bench_function("parse_raw_stylesheet", |bencher| {
        bencher.iter(|| {
            let mut parser = Parser::new(black_box(SAMPLE));
            parser.set_preservation_mode(true);
            parser.set_quirks_mode(false);
            let _ = parser.parse_raw_stylesheet();
        });
    });
}

fn bench_minify(criterion: &mut Criterion) {
    criterion.bench_function("minify_stylesheet", |bencher| {
        bencher.iter(|| {
            let mut minifier = Minifier::new(false);
            let _ = minifier.parse_stylesheet(black_box(SAMPLE));
        });
    });
}

fn bench_scan(criterion: &mut Criterion) {
    criterion.bench_function("scanner_transform", |bencher| {
        bencher.iter(|| {
            let mut transformer = CountTransformer { count: 0 };
            let _ = transform_urls(black_box(SAMPLE), &mut transformer);
        });
    });
}

criterion_group!(benches, bench_parse, bench_minify, bench_scan);
criterion_main!(benches);
