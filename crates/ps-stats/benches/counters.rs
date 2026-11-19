//! Benchmarks for counters, histograms, and snapshots (Step 7 gate).
use criterion::{criterion_group, criterion_main, Criterion};
use ps_stats::core::{Histogram, Statistics, TimedLevel, UpDownCounter, Variable};
use ps_stats::names::init_all;
use std::hint::black_box;
use std::sync::Arc;

fn bench_counters(criterion: &mut Criterion) {
    criterion.bench_function("variable_add", |bencher| {
        let var = Variable::new("v");
        bencher.iter(|| black_box(var.add(black_box(1))));
    });
    criterion.bench_function("updown_add", |bencher| {
        let counter = UpDownCounter::new("u");
        bencher.iter(|| black_box(counter.add(black_box(1))));
    });
    criterion.bench_function("variable_add_4_threads", |bencher| {
        bencher.iter(|| {
            let var = Arc::new(Variable::new("v"));
            std::thread::scope(|scope| {
                for _ in 0..4 {
                    let var = var.clone();
                    scope.spawn(move || {
                        for _ in 0..1000 {
                            var.add(1);
                        }
                    });
                }
            });
            black_box(var.get())
        });
    });
}

fn bench_histogram(criterion: &mut Criterion) {
    criterion.bench_function("histogram_add", |bencher| {
        let hist = Histogram::new("h");
        let mut value = 0.0;
        bencher.iter(|| {
            value += 1.0;
            hist.add(black_box(value % 1000.0));
        });
    });
    criterion.bench_function("histogram_percentile", |bencher| {
        let hist = Histogram::new("h");
        for i in 0..1000 {
            hist.add(i as f64);
        }
        bencher.iter(|| black_box(hist.percentile(black_box(99.0))));
    });
}

fn bench_registry(criterion: &mut Criterion) {
    criterion.bench_function("snapshot_counters", |bencher| {
        let stats = Statistics::new();
        init_all(&stats);
        stats.get_variable("cache_hits").unwrap().add(1);
        bencher.iter(|| black_box(stats.snapshot_counters()));
    });
    criterion.bench_function("timed_inc_get", |bencher| {
        let stats = Statistics::new();
        let timed = stats.add_timed_variable("t", "default");
        bencher.iter(|| {
            timed.inc_by(black_box(1));
            black_box(timed.get(TimedLevel::Start))
        });
    });
}

criterion_group!(benches, bench_counters, bench_histogram, bench_registry);
criterion_main!(benches);
