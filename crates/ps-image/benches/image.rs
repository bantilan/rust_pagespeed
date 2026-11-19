//! Benchmarks for sniffing, decoding, encoding, and optimizing (Step 3 gate).
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use std::hint::black_box;
use std::path::PathBuf;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(data_dir().join(name)).unwrap()
}

fn bench_sniff(criterion: &mut Criterion) {
    let jpeg = fixture("jpeg/sjpeg4.jpg");
    let png = fixture("png/pagespeed-33x34.png");
    let webp = fixture("webp/opaque_32x20.webp");
    criterion.bench_function("sniff_jpeg", |bencher| {
        bencher.iter(|| ps_image::format::sniff(black_box(&jpeg)))
    });
    criterion.bench_function("sniff_png", |bencher| {
        bencher.iter(|| ps_image::format::sniff(black_box(&png)))
    });
    criterion.bench_function("sniff_webp_features", |bencher| {
        bencher.iter(|| ps_image::format::webp_features(black_box(&webp)))
    });
}

fn bench_decode(criterion: &mut Criterion) {
    let jpeg = fixture("jpeg/sjpeg4.jpg");
    let png = fixture("png/pagespeed-128.png");
    let gif = fixture("gif/animated.gif");
    let webp = fixture("webp/opaque_32x20.webp");
    criterion.bench_function("decode_jpeg_512x384", |bencher| {
        bencher.iter(|| ps_image::jpeg::decode(black_box(&jpeg)))
    });
    criterion.bench_function("decode_png_128", |bencher| {
        bencher.iter(|| ps_image::png::decode(black_box(&png)))
    });
    criterion.bench_function("decode_gif_first_frame", |bencher| {
        bencher.iter(|| ps_image::gif::first_frame(black_box(&gif)))
    });
    criterion.bench_function("decode_webp_32x20", |bencher| {
        bencher.iter(|| ps_image::webp::decode(black_box(&webp)))
    });
}

fn bench_encode(criterion: &mut Criterion) {
    let jpeg_bytes = fixture("jpeg/sjpeg4.jpg");
    let frame = ps_image::jpeg::decode(&jpeg_bytes).unwrap();
    criterion.bench_function("encode_jpeg_q85_420", |bencher| {
        bencher.iter(|| {
            ps_image::jpeg::encode(
                black_box(&frame),
                &ps_image::jpeg::JpegEncodeOptions::default(),
            )
        })
    });
    let png_bytes = fixture("png/pagespeed-128.png");
    let frame = ps_image::png::decode(&png_bytes).unwrap();
    criterion.bench_function("encode_png_default", |bencher| {
        bencher.iter(|| {
            ps_image::png::encode(
                black_box(&frame),
                &ps_image::png::PngEncodeOptions::default(),
            )
        })
    });
    criterion.bench_function("encode_webp_lossless", |bencher| {
        bencher.iter(|| {
            ps_image::webp::encode(
                black_box(&frame),
                &ps_image::webp::WebpEncodeConfig::default(),
            )
        })
    });
}

fn bench_optimize(criterion: &mut Criterion) {
    let jpeg = fixture("jpeg/sjpeg4.jpg");
    criterion.bench_function("optimize_jpeg_lossless", |bencher| {
        bencher.iter(|| ps_image::optimize::optimize_jpeg_lossless(black_box(&jpeg)))
    });
    let png = fixture("png/pagespeed-128.png");
    criterion.bench_function("optimize_png_best", |bencher| {
        bencher.iter(|| ps_image::optimize::optimize_png_best(black_box(&png)))
    });
    criterion.bench_function("jpeg_to_webp_q75", |bencher| {
        bencher.iter(|| ps_image::optimize::optimize_webp_from_jpeg(black_box(&jpeg), 75))
    });
}

fn bench_transforms(criterion: &mut Criterion) {
    let png_bytes = fixture("png/pagespeed-128.png");
    let frame = ps_image::png::decode(&png_bytes).unwrap();
    criterion.bench_function("resize_128_to_32", |bencher| {
        bencher.iter(|| ps_image::resizer::resize(black_box(&frame), 32, 32))
    });
    criterion.bench_function("photo_metric_128", |bencher| {
        bencher.iter(|| ps_image::analysis::photo_metric(black_box(&frame)))
    });
}

fn bench_driver(criterion: &mut Criterion) {
    use ps_image::image::{optimize, ImageOptions, RequestedDimensions};
    use ps_image::stats::NullRecorder;
    let jpeg = fixture("jpeg/sjpeg4.jpg");
    let options = ImageOptions::default();
    criterion.bench_function("optimizer_jpeg_to_webp", |bencher| {
        bencher.iter(|| {
            optimize(
                black_box(&jpeg),
                &options,
                RequestedDimensions::default(),
                None,
            )
        })
    });
    let gif = fixture("gif/animated.gif");
    let animated = ImageOptions {
        allow_webp_animated: true,
        ..Default::default()
    };
    criterion.bench_function("optimizer_gif_to_animated_webp", |bencher| {
        bencher.iter_batched(
            || gif.clone(),
            |bytes| optimize(&bytes, &animated, RequestedDimensions::default(), None),
            BatchSize::SmallInput,
        )
    });
    let _ = NullRecorder;
}

criterion_group!(
    benches,
    bench_sniff,
    bench_decode,
    bench_encode,
    bench_optimize,
    bench_transforms,
    bench_driver
);
criterion_main!(benches);
