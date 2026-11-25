//! Isolated eligibility cost; not HTML latency or image transfer savings.
use ps_html::srcset::{supports_width_sizes, MAX_SIZES_SRCSET_BYTES};
use std::hint::black_box;
use std::time::Instant;

fn median_ns(mut work: impl FnMut()) -> f64 {
    for _ in 0..100 {
        work();
    }
    let mut batches = Vec::new();
    for _ in 0..9 {
        let start = Instant::now();
        for _ in 0..5000 {
            work();
        }
        batches.push(start.elapsed().as_nanos() as f64 / 5000.0);
    }
    batches.sort_by(f64::total_cmp);
    batches[4]
}

fn main() {
    let widths = |count: usize| {
        (1..=count)
            .map(|w| format!("https://example.test/hero-{w}.jpg {}w", w * 100))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let cases = [
        ("widths_3", widths(3), true),
        ("widths_24", widths(24), true),
        ("widths_64", widths(64), true),
        ("over_candidates_65", widths(65), false),
        ("density", "a.jpg 1x, b.jpg 2x".into(), false),
        ("over_bytes", "x".repeat(MAX_SIZES_SRCSET_BYTES + 1), false),
    ];
    println!("{{\"scope\":\"allocation-free eligibility only; not HTML latency\",\"batches\":9,\"iterations_per_batch\":5000,\"cases\":[");
    for (index, (name, input, expected)) in cases.iter().enumerate() {
        assert_eq!(supports_width_sizes(input.as_bytes()), *expected);
        let cost = median_ns(|| {
            black_box(supports_width_sizes(black_box(input.as_bytes())));
        });
        println!(
            "{}{{\"name\":\"{}\",\"bytes\":{},\"eligible\":{},\"validation_ns\":{:.3}}}",
            if index == 0 { "" } else { "," },
            name,
            input.len(),
            expected,
            cost
        );
    }
    println!("]}}");
}
