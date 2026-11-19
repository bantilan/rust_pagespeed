//! Benchmarks for filter/option lookups and parsing (Step 7 gate).
use criterion::{criterion_group, criterion_main, Criterion};
use ps_config::filter::{Filter, FilterState};
use ps_config::options::RewriteOptions;
use ps_config::property::{lookup_by_id, lookup_by_name};
use ps_config::scope::Scope;
use std::hint::black_box;

fn bench_filter_lookups(criterion: &mut Criterion) {
    criterion.bench_function("filter_from_name", |bencher| {
        bencher.iter(|| black_box(Filter::from_name(black_box("rewrite_javascript"))));
    });
    criterion.bench_function("filter_from_id", |bencher| {
        bencher.iter(|| black_box(Filter::from_id(black_box("jm"))));
    });
    criterion.bench_function("option_lookup_by_name", |bencher| {
        bencher.iter(|| black_box(lookup_by_name(black_box("CssInlineMaxBytes"))));
    });
    criterion.bench_function("option_lookup_by_id", |bencher| {
        bencher.iter(|| black_box(lookup_by_id(black_box("ci"))));
    });
}

fn bench_filter_adjust(criterion: &mut Criterion) {
    criterion.bench_function("adjust_incremental", |bencher| {
        bencher.iter(|| {
            let mut state = FilterState::new();
            state.adjust_by_comma_list(black_box("+rewrite_css,-combine_css"));
            black_box(state.enabled(Filter::RewriteCss))
        });
    });
    criterion.bench_function("adjust_reset", |bencher| {
        bencher.iter(|| {
            let mut state = FilterState::new();
            state.adjust_by_comma_list(black_box("rewrite_css,rewrite_images"));
            black_box(state.enabled(Filter::RewriteCss))
        });
    });
    criterion.bench_function("enabled_core", |bencher| {
        let mut state = FilterState::new();
        state.set_level(ps_config::filter::RewriteLevel::CoreFilters);
        bencher.iter(|| black_box(state.enabled(black_box(Filter::RewriteCss))));
    });
}

fn bench_options(criterion: &mut Criterion) {
    criterion.bench_function("set_option", |bencher| {
        bencher.iter(|| {
            let mut options = RewriteOptions::new();
            options
                .set_option(
                    black_box("CssInlineMaxBytes"),
                    black_box("4096"),
                    Scope::ProcessStrict,
                )
                .unwrap();
            black_box(options)
        });
    });
    criterion.bench_function("merge", |bencher| {
        let mut child = RewriteOptions::new();
        child
            .set_option("CssInlineMaxBytes", "4096", Scope::ProcessStrict)
            .unwrap();
        child.filters_mut().enable(Filter::SpriteImages);
        bencher.iter(|| {
            let mut base = RewriteOptions::new();
            base.merge(black_box(&child));
            black_box(base)
        });
    });
}

criterion_group!(
    benches,
    bench_filter_lookups,
    bench_filter_adjust,
    bench_options
);
criterion_main!(benches);
