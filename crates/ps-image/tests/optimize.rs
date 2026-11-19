//! Single-format optimizer tests: JPEG/PNG/WebP option matrices and the
//! lossless pixel-identity property over the PNGSuite corpus.

use std::fs;
use std::path::{Path, PathBuf};

use ps_image::format::{sniff, ImageType};
use ps_image::frame::PixelFormat;
use ps_image::{converter, gif, jpeg, optimize, png};

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

fn read_fixture(name: &str) -> Vec<u8> {
    fs::read(data_dir().join(name)).unwrap_or_else(|_| panic!("missing fixture {name}"))
}

fn valid_suite_pngs() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(data_dir().join("pngsuite"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "png"))
        .filter(|path| {
            ![
                "emptyfile.png",
                "x00n0g01.png",
                "xcrn0g04.png",
                "xlfn0g04.png",
            ]
            .contains(&path.file_name().unwrap().to_string_lossy().as_ref())
        })
        .collect();
    files.sort();
    files
}

const VALID_JPEGS: [&str; 13] = [
    "sjpeg1.jpg",
    "sjpeg2.jpg",
    "sjpeg3.jpg",
    "sjpeg4.jpg",
    "sjpeg6.jpg",
    "test411.jpg",
    "test420.jpg",
    "test422.jpg",
    "test444.jpg",
    "testgray.jpg",
    "progressive.jpg",
    "quality100.jpg",
    "already_optimized.jpg",
];

#[test]
fn jpeg_lossless_never_grows_and_stays_identical() {
    for name in VALID_JPEGS {
        let bytes = read_fixture(&format!("jpeg/{name}"));
        let optimized =
            optimize::optimize_jpeg_lossless(&bytes).unwrap_or_else(|_| panic!("lossless {name}"));
        assert!(optimized.len() <= bytes.len(), "{name} grew");
        assert_eq!(sniff(&optimized), ImageType::Jpeg, "{name}");
        // Same pixels (same DQTs, same scans, same dims).
        let before = jpeg::scan_info(&bytes).unwrap();
        let after = jpeg::scan_info(&optimized).unwrap();
        assert_eq!((after.width, after.height), (before.width, before.height));
        assert_eq!(
            jpeg::estimate_quality(&optimized),
            jpeg::estimate_quality(&bytes),
            "{name} quality changed"
        );
    }
    // Metadata-bearing input strictly shrinks.
    let bytes = read_fixture("jpeg/app_segments.jpg");
    let optimized = optimize::optimize_jpeg_lossless(&bytes).unwrap();
    assert!(optimized.len() < bytes.len());
    // Corrupt entropy fails, like the C++ coefficient read.
    assert!(optimize::optimize_jpeg_lossless(&read_fixture("jpeg/corrupt.jpg")).is_err());
    assert!(optimize::optimize_jpeg_lossless(&read_fixture("jpeg/emptyfile.jpg")).is_err());
}

#[test]
fn jpeg_lossy_options_matrix() {
    use optimize::{ColorSampling, JpegCompressionOptions};
    let bytes = read_fixture("jpeg/test422.jpg");
    // Quality orders sizes.
    let mut last = usize::MAX;
    for quality in [95, 75, 50] {
        let options = JpegCompressionOptions {
            lossy: true,
            lossy_options: optimize::JpegLossyOptions {
                quality,
                num_scans: -1,
                color_sampling: ColorSampling::Yuv420,
            },
            ..Default::default()
        };
        let optimized = optimize::optimize_jpeg_with_options(&bytes, &options).unwrap();
        assert!(optimized.len() < last, "q{quality} not smaller");
        last = optimized.len();
        let frame = jpeg::decode(&optimized).unwrap();
        assert_eq!((frame.width(), frame.height()), (130, 97));
    }
    // Progressive flag produces progressive output.
    let options = JpegCompressionOptions {
        lossy: true,
        progressive: true,
        ..Default::default()
    };
    let optimized = optimize::optimize_jpeg_with_options(&bytes, &options).unwrap();
    assert!(jpeg::scan_info(&optimized).unwrap().progressive);
    // RETAIN keeps the input's 422 factors.
    let options = JpegCompressionOptions {
        lossy: true,
        lossy_options: optimize::JpegLossyOptions {
            color_sampling: ColorSampling::Retain,
            ..Default::default()
        },
        ..Default::default()
    };
    let optimized = optimize::optimize_jpeg_with_options(&bytes, &options).unwrap();
    assert_eq!(
        jpeg::scan_info(&optimized).unwrap().sampling_factors[0],
        (2, 1)
    );
    // Retention flags copy markers through the options struct.
    let bytes = read_fixture("jpeg/app_segments.jpg");
    let options = JpegCompressionOptions {
        lossy: true,
        retain_color_profile: true,
        retain_exif_data: true,
        ..Default::default()
    };
    let optimized = optimize::optimize_jpeg_with_options(&bytes, &options).unwrap();
    let kept = jpeg::retained_app_segments(&bytes, true, true).unwrap();
    let kept_again = jpeg::retained_app_segments(&optimized, true, true).unwrap();
    assert_eq!(kept_again, kept);
}

#[test]
fn png_optimize_is_pixel_identical_suite_wide() {
    for path in valid_suite_pngs() {
        let bytes = fs::read(&path).unwrap();
        for (optimized, label) in [
            (
                optimize::optimize_png(&bytes)
                    .unwrap_or_else(|_| panic!("fast {}", path.display())),
                "fast",
            ),
            (
                optimize::optimize_png_best(&bytes)
                    .unwrap_or_else(|_| panic!("best {}", path.display())),
                "best",
            ),
        ] {
            assert_eq!(
                sniff(&optimized),
                ImageType::Png,
                "{label} {}",
                path.display()
            );
            let expected = png::decode(&bytes).unwrap();
            let actual = png::decode(&optimized).unwrap();
            assert_eq!(
                (actual.width(), actual.height()),
                (expected.width(), expected.height()),
                "{label} {}",
                path.display()
            );
            // Formats may narrow (RGBA->RGB, RGB->gray, ...); pixels must not.
            let expected_rgba = expected.expand(PixelFormat::Rgba8888).unwrap();
            let actual_rgba = actual.expand(PixelFormat::Rgba8888).unwrap();
            assert_eq!(
                actual_rgba.pixels(),
                expected_rgba.pixels(),
                "{label} {}",
                path.display()
            );
        }
    }
}

#[test]
fn png_best_never_loses_to_fast() {
    // The best trial set includes the fast configuration, so best <= fast
    // holds by construction on every input.
    for path in valid_suite_pngs() {
        let bytes = fs::read(&path).unwrap();
        let fast = optimize::optimize_png(&bytes).unwrap();
        let best = optimize::optimize_png_best(&bytes).unwrap();
        assert!(best.len() <= fast.len(), "{}", path.display());
    }
    for name in [
        "png/pagespeed-33x34.png",
        "png/large.png",
        "gif/transparent.gif",
    ] {
        let bytes = read_fixture(name);
        let fast = optimize::optimize_png(&bytes).unwrap();
        let best = optimize::optimize_png_best(&bytes).unwrap();
        assert!(best.len() <= fast.len(), "{name}");
    }
}

#[test]
fn png_reduction_narrows_formats() {
    // Palette input stays palettized.
    let optimized = optimize::optimize_png_best(&read_fixture("pngsuite/basn3p08.png")).unwrap();
    assert_eq!(png::scan_info(&optimized).unwrap().color_type, 3);
    // 16-bit gray strips to 8-bit.
    let optimized = optimize::optimize_png_best(&read_fixture("pngsuite/basn0g16.png")).unwrap();
    let info = png::scan_info(&optimized).unwrap();
    assert_eq!((info.color_type, info.bit_depth), (0, 8));
    // Few-color RGB collapses to palette.
    let optimized = optimize::optimize_png_best(&read_fixture("pngsuite/basn3p01.png")).unwrap();
    assert_eq!(png::scan_info(&optimized).unwrap().color_type, 3);
    // Opaque RGBA strips its alpha channel.
    let opaque_rgba = png::decode(&read_fixture("pngsuite/basn6a08.png")).unwrap();
    let has_transparency = opaque_rgba.has_transparency();
    let optimized = optimize::optimize_png_best(&read_fixture("pngsuite/basn6a08.png")).unwrap();
    let color_type = png::scan_info(&optimized).unwrap().color_type;
    if has_transparency {
        assert_eq!(color_type, 6);
    } else {
        assert_ne!(color_type, 6);
        assert_ne!(color_type, 4);
    }
    // GIF input optimizes through the same path (first frame).
    let optimized = optimize::optimize_png_best(&read_fixture("gif/transparent.gif")).unwrap();
    assert_eq!(sniff(&optimized), ImageType::Png);
    let expected = gif::first_frame(&read_fixture("gif/transparent.gif")).unwrap();
    let actual = png::decode(&optimized).unwrap();
    assert_eq!(
        actual.expand(PixelFormat::Rgba8888).unwrap().pixels(),
        expected.expand(PixelFormat::Rgba8888).unwrap().pixels()
    );
}

#[test]
fn webp_from_jpeg_follows_quality_rule() {
    // Output quality = min(input estimate, configured): sizes order with the
    // configured quality on both sides of the input estimate.
    let bytes = read_fixture("jpeg/sjpeg2.jpg");
    let input_quality = jpeg::estimate_quality(&bytes).expect("estimate") as i32;
    assert!((1..100).contains(&input_quality));
    let mut last = 0;
    for configured in [10, 50, 95] {
        let optimized = optimize::optimize_webp_from_jpeg(&bytes, configured).unwrap();
        assert!(
            matches!(
                sniff(&optimized),
                ImageType::Webp | ImageType::WebpLosslessOrAlpha
            ),
            "q{configured}"
        );
        let frame = ps_image::webp::decode(&optimized).unwrap();
        assert_eq!((frame.width(), frame.height()), (130, 97));
        assert!(optimized.len() >= last, "q{configured} not monotonic");
        last = optimized.len();
    }
    // Unknown configured quality falls back to 75.
    let optimized = optimize::optimize_webp_from_jpeg(&bytes, optimize::NO_QUALITY_GIVEN).unwrap();
    assert_eq!(sniff(&optimized), ImageType::Webp);
}

#[test]
fn webp_quality_reduction_contract() {
    let bytes = read_fixture("webp/opaque_32x20.webp");
    // Below 1 returns the original bytes untouched.
    assert_eq!(optimize::reduce_webp_quality(&bytes, 0).unwrap(), bytes);
    assert_eq!(optimize::reduce_webp_quality(&bytes, -3).unwrap(), bytes);
    // Otherwise a valid, smaller-or-equal WebP at the requested quality.
    let reduced = optimize::reduce_webp_quality(&bytes, 50).unwrap();
    assert_eq!(sniff(&reduced), ImageType::Webp);
    let frame = ps_image::webp::decode(&reduced).unwrap();
    assert_eq!((frame.width(), frame.height()), (32, 20));
    // Animated input fails (the still decoder refuses it, like WebPDecode).
    assert!(optimize::reduce_webp_quality(&read_fixture("webp/animated.webp"), 50).is_err());
}

// The C++ kValidGifImages file list (opaque suite GIFs; exact byte counts
// are encoder-specific and intentionally not ported).
const OPAQUE_SUITE_GIFS: [&str; 16] = [
    "basi0g01", "basi0g02", "basi0g04", "basi0g08", "basn0g01", "basn0g02", "basn0g04", "basn0g08",
    "basi3p01", "basi3p02", "basi3p04", "basi3p08", "basn3p01", "basn3p02", "basn3p04", "basn3p08",
];

#[test]
fn convert_opaque_gif_to_png() {
    for name in OPAQUE_SUITE_GIFS {
        let bytes = read_fixture(&format!("pngsuite/gif/{name}.gif"));
        let optimized =
            optimize::optimize_png_best(&bytes).unwrap_or_else(|_| panic!("gif->png {name}"));
        assert_eq!(sniff(&optimized), ImageType::Png, "{name}");
        let expected = gif::first_frame(&bytes).unwrap();
        let actual = png::decode(&optimized).unwrap();
        assert_eq!(
            actual.expand(PixelFormat::Rgba8888).unwrap().pixels(),
            expected.expand(PixelFormat::Rgba8888).unwrap().pixels(),
            "{name}"
        );
    }
}

#[test]
fn convert_opaque_gif_to_jpeg() {
    use optimize::{ColorSampling, JpegCompressionOptions, JpegLossyOptions};
    for name in OPAQUE_SUITE_GIFS {
        let bytes = read_fixture(&format!("pngsuite/gif/{name}.gif"));
        let options = JpegCompressionOptions {
            lossy: true,
            progressive: false,
            lossy_options: JpegLossyOptions {
                quality: 100,
                num_scans: -1,
                color_sampling: ColorSampling::Yuv420,
            },
            ..Default::default()
        };
        let converted = converter::convert_png_to_jpeg(&bytes, &options)
            .unwrap_or_else(|_| panic!("gif->jpeg {name}"));
        assert_eq!(sniff(&converted), ImageType::Jpeg, "{name}");
        let frame = jpeg::decode(&converted).unwrap();
        assert_eq!((frame.width(), frame.height()), (32, 32), "{name}");
    }
    // Transparency refuses the conversion instead of flattening it.
    let options = JpegCompressionOptions {
        lossy: true,
        ..Default::default()
    };
    assert!(
        converter::convert_png_to_jpeg(&read_fixture("gif/transparent.gif"), &options).is_err()
    );
    assert!(
        converter::convert_png_to_jpeg(&read_fixture("pngsuite/basn6a08.png"), &options).is_err()
    );
}

#[test]
fn convert_opaque_gif_to_webp() {
    // Byte counts are libwebp-version-specific (C++ pins 0.6.1; this port
    // vendors a modern libwebp), so the portable assertions are success,
    // the opaque flag, and lossless pixel identity — not exact sizes.
    for name in OPAQUE_SUITE_GIFS {
        let bytes = read_fixture(&format!("pngsuite/gif/{name}.gif"));
        let config = ps_image::webp::WebpEncodeConfig::default();
        let (converted, is_opaque) = converter::convert_png_to_webp(&bytes, &config)
            .unwrap_or_else(|_| panic!("gif->webp {name}"));
        assert!(is_opaque, "{name}");
        let expected = gif::first_frame(&bytes).unwrap();
        let actual = ps_image::webp::decode(&converted).unwrap();
        assert_eq!(
            actual.expand(PixelFormat::Rgba8888).unwrap().pixels(),
            expected.expand(PixelFormat::Rgba8888).unwrap().pixels(),
            "{name}"
        );
    }
    // The alpha gate refuses transparent input when alpha is disabled.
    let config = ps_image::webp::WebpEncodeConfig {
        lossless: false,
        quality: 80.0,
        alpha_quality: 0,
        alpha_compression: 0,
        ..Default::default()
    };
    assert!(converter::convert_png_to_webp(&read_fixture("gif/transparent.gif"), &config).is_err());
    let (converted, is_opaque) = converter::convert_png_to_webp(
        &read_fixture("gif/transparent.gif"),
        &ps_image::webp::WebpEncodeConfig::default(),
    )
    .unwrap();
    assert!(!is_opaque);
    assert_eq!(sniff(&converted), ImageType::WebpLosslessOrAlpha);
}

#[test]
fn png_or_jpeg_picks_by_savings_ratio() {
    use optimize::{ColorSampling, JpegCompressionOptions, JpegLossyOptions};
    let options = JpegCompressionOptions {
        lossy: true,
        lossy_options: JpegLossyOptions {
            quality: 85,
            num_scans: -1,
            color_sampling: ColorSampling::Yuv420,
        },
        ..Default::default()
    };
    // Photographic PNG: JPEG wins by a wide margin.
    let (out, is_png) = converter::optimize_png_or_convert_to_jpeg(
        &read_fixture("pngsuite/f04n2c08.png"),
        &options,
    )
    .unwrap();
    assert!(!is_png);
    assert_eq!(sniff(&out), ImageType::Jpeg);
    // Few-color graphic: PNG wins (the C++ table has JPEG at 1036 vs PNG
    // at 166 for this image — far above the 0.8 JPEG threshold).
    let (out, is_png) = converter::optimize_png_or_convert_to_jpeg(
        &read_fixture("pngsuite/basi0g01.png"),
        &options,
    )
    .unwrap();
    assert!(is_png);
    assert_eq!(sniff(&out), ImageType::Png);
}

#[test]
fn smallest_of_png_jpeg_webp_matrix() {
    use converter::SmallestType;
    use optimize::JpegCompressionOptions;
    let jpeg_options = JpegCompressionOptions {
        lossy: true,
        ..Default::default()
    };
    let webp_lossy = ps_image::webp::WebpEncodeConfig {
        lossless: false,
        quality: 75.0,
        ..Default::default()
    };
    // Photo: a lossy format wins.
    let (kind, out) = converter::get_smallest_of_png_jpeg_webp(
        &read_fixture("pngsuite/f04n2c08.png"),
        Some(&jpeg_options),
        Some(&webp_lossy),
    );
    assert!(
        matches!(kind, SmallestType::Jpeg | SmallestType::Webp),
        "photo picked {kind:?}"
    );
    assert!(!out.is_empty());
    // Graphic with no lossy options: lossless or original wins, never lossy.
    let (kind, out) = converter::get_smallest_of_png_jpeg_webp(
        &read_fixture("pngsuite/basn3p08.png"),
        None,
        None,
    );
    assert!(
        matches!(
            kind,
            SmallestType::Png | SmallestType::Webp | SmallestType::None
        ),
        "graphic picked {kind:?}"
    );
    assert!(!out.is_empty());
    // Corrupt input: original bytes back with None.
    let bytes = read_fixture("pngsuite/xcrn0g04.png");
    let (kind, out) =
        converter::get_smallest_of_png_jpeg_webp(&bytes, Some(&jpeg_options), Some(&webp_lossy));
    assert_eq!(kind, SmallestType::None);
    assert_eq!(out, bytes);
}

#[test]
fn progressive_jpeg_thresholds() {
    // Ported from `ImageRewriteTest.ProgressiveJpegThresholds`: the
    // progressive gate over Puzzle.jpg (1023x766).
    let image_data = read_fixture("jpeg/Puzzle.jpg");
    let num_bytes = image_data.len();
    let threshold = ps_image::image::MIN_PROGRESSIVE_JPEG_BYTES;

    // Since we haven't established a size, resizing won't happen.
    assert!(optimize::should_convert_to_progressive(
        -1, threshold, num_bytes, 1023, 766
    ));

    // Now provide a context, resizing the image to 10x10.  Of course
    // we should not convert that to progressive, because post-resizing
    // the image will be tiny.
    assert!(!optimize::should_convert_to_progressive(
        -1, threshold, num_bytes, 10, 10
    ));

    // At 256x192, we are close to the tipping point, and whether we should
    // convert to progressive or not is dependent on the compression
    // level.
    assert!(optimize::should_convert_to_progressive(
        -1, threshold, num_bytes, 256, 192
    ));

    // Setting compression to 90.  The quality level is high, and our model
    // says we'll wind up with an image >10204 bytes, which is still
    // large enough to convert to progressive.
    assert!(optimize::should_convert_to_progressive(
        90, threshold, num_bytes, 256, 192
    ));

    // Now set the compression to 75, which shrinks our image to <10k so
    // we should stop converting to progressive.
    assert!(!optimize::should_convert_to_progressive(
        75, threshold, num_bytes, 256, 192
    ));
}
