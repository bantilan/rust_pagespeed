//! End-to-end rewrite benchmarks (Step 7 gate): latency and
//! throughput by page size and filter level.
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use ps_config::filter::RewriteLevel;
use ps_config::options::RewriteOptions;
use ps_html::chain::ChainContext;
use ps_html::driver::Driver;
use std::hint::black_box;

fn fragment() -> Vec<u8> {
    br#"<!DOCTYPE html><html><head><title>Bench page</title><style>body { color: red; } /* tint */</style><script>function f() { return 1 + 2; }</script></head><body><div class="a" id="x"><p>Hello <b>world</b> <!-- note --> <a href="/p?q=1">link</a></p><img src="i.png" width="10" height="10"></div></body></html>"#
        .to_vec()
}

fn corpus(repeats: usize) -> Vec<u8> {
    let piece = fragment();
    let mut out = Vec::with_capacity(piece.len() * repeats);
    for _ in 0..repeats {
        out.extend_from_slice(&piece);
    }
    out
}

fn rewrite_once(options: &RewriteOptions, input: &[u8]) {
    let ctx = ChainContext::new(true, true);
    let mut driver = Driver::new(options, &ctx, Vec::new(), None);
    driver.start_parse(b"http://bench.test/page.html", false);
    driver.write(black_box(input));
    driver.finish();
}

fn bench_rewrite(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("rewrite");
    for (level_name, level) in [
        ("passthrough", RewriteLevel::PassThrough),
        ("core", RewriteLevel::CoreFilters),
        ("all", RewriteLevel::AllFilters),
    ] {
        let mut options = RewriteOptions::new();
        options.filters_mut().set_level(level);
        for (size_name, repeats) in [("small", 2), ("medium", 200), ("large", 2000)] {
            let input = corpus(repeats);
            group.throughput(Throughput::Bytes(input.len() as u64));
            group.bench_with_input(
                BenchmarkId::new(level_name, size_name),
                &input,
                |bencher, input| {
                    bencher.iter(|| rewrite_once(&options, black_box(input)));
                },
            );
        }
    }
    group.finish();
}

criterion_group!(benches, bench_rewrite);
criterion_main!(benches);
