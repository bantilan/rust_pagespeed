//! Codec work only; original level and chunk/flush policy, exact roundtrips.
use ps_http::zstd::{encode_zstd, ZstdStreamEncoder};
use std::{hint::black_box, time::Instant};
fn main() {
    let html =
        std::fs::read("/var/www/test/moto-business-html5-responsive-web-template/index.html")
            .unwrap();
    let css =
        b".item { color: #123456; background: url(hero.jpg); margin: 0 12px; }\n".repeat(1024);
    println!(
        "{{\"level\":{},\"samples\":9,\"iterations\":200,\"cases\":[",
        ps_http::zstd::DEFAULT_ZSTD_LEVEL
    );
    for (index, (name, data, streaming)) in [
        ("html_bulk", &html, false),
        ("html_stream", &html, true),
        ("css_bulk", &css, false),
        ("css_stream", &css, true),
    ]
    .into_iter()
    .enumerate()
    {
        let encode = || {
            if streaming {
                let mut encoder = ZstdStreamEncoder::new().unwrap();
                let mut out = Vec::new();
                for chunk in data.chunks(4096) {
                    out.extend(encoder.write_chunk(chunk).unwrap());
                }
                out.extend(encoder.finish().unwrap());
                out
            } else {
                encode_zstd(data).unwrap()
            }
        };
        let cold = Instant::now();
        let frame = encode();
        let cold_ns = cold.elapsed().as_nanos();
        assert_eq!(zstd::bulk::decompress(&frame, data.len()).unwrap(), *data);
        let mut samples = Vec::new();
        for _ in 0..9 {
            let start = Instant::now();
            for _ in 0..200 {
                black_box(encode());
            }
            samples.push(start.elapsed().as_nanos() as f64 / 200.0);
        }
        let mut sorted = samples.clone();
        sorted.sort_by(f64::total_cmp);
        println!("{}{{\"name\":\"{name}\",\"input_bytes\":{},\"encoded_bytes\":{},\"cold_ns\":{cold_ns},\"median_ns\":{},\"samples_ns\":{:?}}}",if index==0 {""} else {","},data.len(),frame.len(),sorted[4],samples);
    }
    println!("]}}");
}
