//! Existing srcset parse/serialize microbaseline; no fetches, image work or filters.
//! Run: cargo run --release -p ps-html --example srcset_baseline
use ps_html::srcset::{parse_srcset, serialize_srcset};
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
    println!("{{\"scope\":\"existing parser only; not HTML or network latency\",\"batches\":9,\"iterations_per_batch\":5000,\"cases\":[");
    for (index, (name, count, density)) in [
        ("wordpress_widths", 3, false),
        ("density_candidates", 3, true),
        ("widths_24", 24, false),
        ("widths_256", 256, false),
    ]
    .iter()
    .enumerate()
    {
        let input = (0..*count)
            .map(|i| {
                let descriptor = if *density {
                    format!("{}x", i + 1)
                } else {
                    format!(
                        "{}w",
                        [300, 768, 800].get(i).copied().unwrap_or((i + 1) * 100)
                    )
                };
                format!("https://example.test/wp-content/uploads/2026/11/hero-{i}.jpg {descriptor}")
            })
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(parse_srcset(input.as_bytes()).len(), *count);
        let parse = median_ns(|| {
            black_box(parse_srcset(black_box(input.as_bytes())));
        });
        let roundtrip = median_ns(|| {
            let parsed = parse_srcset(black_box(input.as_bytes()));
            black_box(serialize_srcset(&parsed));
        });
        println!(
            "{}{{\"name\":\"{}\",\"candidates\":{},\"input_bytes\":{},\"parse_ns\":{:.3},\"parse_serialize_ns\":{:.3}}}",
            if index == 0 { "" } else { "," }, name, count, input.len(), parse, roundtrip
        );
    }
    println!("]}}");
}
