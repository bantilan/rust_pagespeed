//! Unique full bodies: compare original one-shot compression with the new path.
use std::{hint::black_box, time::Instant};
fn main() {
    let html =
        std::fs::read("/var/www/test/moto-business-html5-responsive-web-template/index.html")
            .unwrap();
    let css =
        b".item { color: #123456; background: url(hero.jpg); margin: 0 12px; }\n".repeat(1024);
    println!("{{\"scope\":\"unique-body cache misses; same level3; first8 bytes varied; median of9 batches of200\",\"cases\":[");
    for (i, (name, source)) in [("html", html), ("css", css)].into_iter().enumerate() {
        let mut baseline = Vec::new();
        let mut optimized = Vec::new();
        for round in 0..9 {
            for fast in if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let mut data = source.clone();
                let start = Instant::now();
                for n in 0..200 {
                    let id = (round * 200 + n) as u64;
                    data[..8].copy_from_slice(&id.to_le_bytes());
                    let output = if fast {
                        ps_http::zstd::encode_zstd(&data).unwrap()
                    } else {
                        zstd::bulk::compress(&data, 3).unwrap()
                    };
                    black_box(output);
                }
                let time = start.elapsed().as_nanos() as f64 / 200.0;
                if fast {
                    optimized.push(time)
                } else {
                    baseline.push(time)
                }
            }
        }
        let median = |v: &[f64]| {
            let mut sorted = v.to_vec();
            sorted.sort_by(f64::total_cmp);
            sorted[4]
        };
        println!("{}{{\"name\":\"{name}\",\"original_ns\":{},\"refined_ns\":{},\"original_samples_ns\":{:?},\"refined_samples_ns\":{:?}}}",if i==0 {""} else {","},median(&baseline),median(&optimized),baseline,optimized);
    }
    println!("]}}");
}
