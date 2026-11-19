//! Fixture-driven codec tests over `tests/data` (copied read-only from the
//! C++ `pagespeed/kernel/image/testdata` corpus).

use std::fs;
use std::path::{Path, PathBuf};

use ps_image::format::{sniff, webp_features, ImageType};
use ps_image::frame::{Frame, PixelFormat};
use ps_image::{analysis, gif, jpeg, png, resizer, webp};

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

fn read_fixture(name: &str) -> Vec<u8> {
    fs::read(data_dir().join(name)).unwrap_or_else(|_| panic!("missing fixture {name}"))
}

fn fixtures_with_extension(dir: &str, extension: &str) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(data_dir().join(dir))
        .unwrap_or_else(|_| panic!("missing fixture dir {dir}"))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == extension))
        .collect();
    files.sort();
    files
}

#[test]
fn sniff_classifies_corpus_by_magic() {
    // JPEG dir: magic rules, so misnamed and corrupt files still classify.
    let cases = [
        ("jpeg/sjpeg1.jpg", ImageType::Jpeg),
        ("jpeg/progressive.jpg", ImageType::Jpeg),
        ("jpeg/corrupt.jpg", ImageType::Jpeg),
        ("jpeg/notajpeg.png", ImageType::Png),
        ("jpeg/notajpeg.gif", ImageType::Gif),
        ("jpeg/emptyfile.jpg", ImageType::Unknown),
        ("gif/animated.gif", ImageType::Gif),
        // bad.gif is 7 bytes: under the C++ 8-byte sniff gate, like the C++.
        ("gif/bad.gif", ImageType::Unknown),
        ("gif/zero_size_animation.gif", ImageType::Gif),
        ("png/pagespeed-33x34.png", ImageType::Png),
        ("webp/opaque_32x20.webp", ImageType::Webp),
        ("webp/alpha_32x32.webp", ImageType::WebpLosslessOrAlpha),
        ("webp/animated.webp", ImageType::WebpAnimated),
        ("webp/gray_saved_as_rgb.webp", ImageType::Webp),
        // corrupt_header leads with a bogus VP9X chunk: libwebp rejects it.
        ("webp/corrupt_header.webp", ImageType::Unknown),
        // corrupt_body has a valid header (VP8X + alpha + VP8): the C++
        // sniffs by header only, so this classifies and fails at decode.
        ("webp/corrupt_body.webp", ImageType::WebpLosslessOrAlpha),
        ("webp/multiple_frame_opaque.gif", ImageType::Gif),
    ];
    for (name, expected) in cases {
        let bytes = read_fixture(name);
        assert_eq!(sniff(&bytes), expected, "{name}");
    }
    // Whole-directory sweeps. The known-invalid set sniffs by its (broken)
    // magic, exactly like the C++, so it is covered separately below.
    for path in fixtures_with_extension("pngsuite", "png") {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if [
            "emptyfile.png",
            "x00n0g01.png",
            "xcrn0g04.png",
            "xlfn0g04.png",
        ]
        .contains(&name.as_str())
        {
            continue;
        }
        let bytes = fs::read(&path).unwrap();
        assert_eq!(sniff(&bytes), ImageType::Png, "{}", path.display());
    }
    // Known-invalid magics: empty and corrupted headers sniff Unknown (the
    // C++ magic gate agrees), while the zero-size file keeps valid magic.
    assert_eq!(
        sniff(&read_fixture("pngsuite/emptyfile.png")),
        ImageType::Unknown
    );
    assert_eq!(
        sniff(&read_fixture("pngsuite/xcrn0g04.png")),
        ImageType::Unknown
    );
    assert_eq!(
        sniff(&read_fixture("pngsuite/xlfn0g04.png")),
        ImageType::Unknown
    );
    assert_eq!(
        sniff(&read_fixture("pngsuite/x00n0g01.png")),
        ImageType::Png
    );
    for path in fixtures_with_extension("resized", "png") {
        let bytes = fs::read(&path).unwrap();
        assert_eq!(sniff(&bytes), ImageType::Png, "{}", path.display());
    }
    for path in fixtures_with_extension("png", "png") {
        let bytes = fs::read(&path).unwrap();
        assert_eq!(sniff(&bytes), ImageType::Png, "{}", path.display());
    }
}

#[test]
fn jpeg_scan_matches_decode() {
    let names = [
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
        "app_segments.jpg",
    ];
    for name in names {
        let bytes = read_fixture(&format!("jpeg/{name}"));
        let info = jpeg::scan_info(&bytes).unwrap_or_else(|_| panic!("scan {name}"));
        let frame = jpeg::decode(&bytes).unwrap_or_else(|_| panic!("decode {name}"));
        assert_eq!(
            (frame.width(), frame.height()),
            (info.width, info.height),
            "{name}"
        );
        assert!(
            frame.width() > 0 && frame.height() > 0,
            "{name} has empty dims"
        );
    }
    let progressive = read_fixture("jpeg/progressive.jpg");
    assert!(jpeg::scan_info(&progressive).unwrap().progressive);
    // The C++ test documents test444.jpg as baseline (single-scan).
    let baseline = read_fixture("jpeg/test444.jpg");
    assert!(!jpeg::scan_info(&baseline).unwrap().progressive);
}

#[test]
fn jpeg_input_sampling_factors() {
    // True SOF sampling factors, verified by an independent marker-walk
    // script. test411's filename is stale: it carries a COM segment with a
    // decoy SOF, and its real luma factors are (1,1), not (4,1).
    let cases = [
        ("jpeg/test411.jpg", (1, 1)),
        ("jpeg/test420.jpg", (2, 2)),
        ("jpeg/test422.jpg", (2, 1)),
        ("jpeg/test444.jpg", (1, 1)),
    ];
    for (name, expected) in cases {
        let bytes = read_fixture(name);
        let info = jpeg::scan_info(&bytes).unwrap();
        assert_eq!(info.sampling_factors[0], expected, "{name}");
    }
    let gray = read_fixture("jpeg/testgray.jpg");
    assert_eq!(jpeg::scan_info(&gray).unwrap().num_components, 1);
}

#[test]
fn jpeg_quality_estimates_are_sane() {
    let quality100 = read_fixture("jpeg/quality100.jpg");
    let estimated = jpeg::estimate_quality(&quality100).expect("quality100 estimates");
    assert!(estimated >= 95, "quality100 estimated as {estimated}");
    for name in ["sjpeg1.jpg", "test420.jpg", "testgray.jpg", "sjpeg3.jpg"] {
        let bytes = read_fixture(&format!("jpeg/{name}"));
        let estimated = jpeg::estimate_quality(&bytes).expect("estimate");
        assert!((1..=100).contains(&estimated), "{name}: {estimated}");
    }
}

#[test]
fn jpeg_rejects_invalid_inputs() {
    for name in [
        "jpeg/emptyfile.jpg",
        "jpeg/notajpeg.png",
        "jpeg/notajpeg.gif",
    ] {
        let bytes = read_fixture(name);
        assert!(jpeg::decode(&bytes).is_err(), "{name} decodes");
        assert!(jpeg::scan_info(&bytes).is_err(), "{name} scans");
    }
    // corrupt.jpg carries an invalid Huffman code in the scan data: the C++
    // optimizer refuses it, and so must decode or the header scan.
    let corrupt = read_fixture("jpeg/corrupt.jpg");
    let scan_ok = jpeg::scan_info(&corrupt).is_ok();
    let decode_ok = jpeg::decode(&corrupt).is_ok();
    assert!(!(scan_ok && decode_ok), "corrupt.jpg fully decodes");
}

#[test]
fn jpeg_encode_round_trip_preserves_shape() {
    for name in ["jpeg/test420.jpg", "jpeg/testgray.jpg", "jpeg/sjpeg1.jpg"] {
        let bytes = read_fixture(name);
        let frame = jpeg::decode(&bytes).unwrap();
        let options = jpeg::JpegEncodeOptions::default();
        let encoded = jpeg::encode(&frame, &options).unwrap();
        assert_eq!(sniff(&encoded), ImageType::Jpeg, "{name}");
        let info = jpeg::scan_info(&encoded).unwrap();
        assert_eq!((info.width, info.height), (frame.width(), frame.height()));
        let decoded = jpeg::decode(&encoded).unwrap();
        assert_eq!(decoded.format(), frame.format(), "{name}");
    }
}

#[test]
fn jpeg_encode_sampling_matrix() {
    // Port of ValidJpegLossyAndColorSampling, minus libjpeg's exact byte
    // counts: sampling factors and the 420 < 422 < 444 size order transfer.
    use jpeg::ColorSampling;
    let bytes = read_fixture("jpeg/test422.jpg");
    let frame = jpeg::decode(&bytes).unwrap();
    let mut sizes = [0usize; 3];
    let cases = [
        (ColorSampling::Yuv420, (2, 2)),
        (ColorSampling::Yuv422, (2, 1)),
        (ColorSampling::Yuv444, (1, 1)),
    ];
    for (i, (sampling, expected)) in cases.iter().enumerate() {
        let options = jpeg::JpegEncodeOptions {
            sampling: *sampling,
            ..Default::default()
        };
        let encoded = jpeg::encode(&frame, &options).unwrap();
        let info = jpeg::scan_info(&encoded).unwrap();
        assert_eq!(info.sampling_factors[0], *expected, "{sampling:?}");
        sizes[i] = encoded.len();
    }
    assert!(sizes[0] < sizes[1], "420 vs 422: {sizes:?}");
    assert!(sizes[1] < sizes[2], "422 vs 444: {sizes:?}");

    // RETAIN keeps the input's 422 factors.
    let options = jpeg::JpegEncodeOptions::default();
    let retained = jpeg::encode_retaining_sampling(&frame, &bytes, &options).unwrap();
    let info = jpeg::scan_info(&retained).unwrap();
    assert_eq!(info.sampling_factors[0], (2, 1));
}

#[test]
fn jpeg_progressive_encode_and_scans() {
    let bytes = read_fixture("jpeg/test420.jpg");
    let frame = jpeg::decode(&bytes).unwrap();
    let options = jpeg::JpegEncodeOptions {
        progressive: true,
        ..Default::default()
    };
    let encoded = jpeg::encode(&frame, &options).unwrap();
    assert!(jpeg::scan_info(&encoded).unwrap().progressive);
    // The re-encoded progressive image still decodes to the same shape.
    let decoded = jpeg::decode(&encoded).unwrap();
    assert_eq!(
        (decoded.width(), decoded.height()),
        (frame.width(), frame.height())
    );

    // Truncated scan scripts stay valid and progressive.
    let options = jpeg::JpegEncodeOptions {
        progressive: true,
        num_scans: 2,
        ..Default::default()
    };
    let encoded = jpeg::encode(&frame, &options).unwrap();
    assert!(jpeg::scan_info(&encoded).unwrap().progressive);
}

#[test]
fn jpeg_marker_retention_round_trip() {
    // app_segments.jpg carries APP/COM segments; retaining them copies the
    // raw marker payloads verbatim, like jpeg_write_marker in the C++.
    let bytes = read_fixture("jpeg/app_segments.jpg");
    let kept = jpeg::retained_app_segments(&bytes, true, true).unwrap();
    assert!(!kept.is_empty(), "app_segments.jpg has no APP1/APP2");
    let frame = jpeg::decode(&bytes).unwrap();

    let options = jpeg::JpegEncodeOptions {
        app_segments: kept.clone(),
        ..Default::default()
    };
    let encoded = jpeg::encode(&frame, &options).unwrap();
    let kept_again = jpeg::retained_app_segments(&encoded, true, true).unwrap();
    assert_eq!(kept_again, kept);

    // Without retention the encoder emits no APP segments of its own.
    let plain = jpeg::encode(&frame, &jpeg::JpegEncodeOptions::default()).unwrap();
    let none = jpeg::retained_app_segments(&plain, true, true).unwrap();
    assert!(none.is_empty());
}

#[test]
fn jpeg_strip_metadata_is_lossless_and_smaller() {
    let bytes = read_fixture("jpeg/app_segments.jpg");
    let stripped = jpeg::strip_metadata(&bytes).unwrap();
    assert!(stripped.len() < bytes.len());
    // Pixel-identical: same dims and same quality estimate (same DQTs).
    let before = jpeg::scan_info(&bytes).unwrap();
    let after = jpeg::scan_info(&stripped).unwrap();
    assert_eq!((after.width, after.height), (before.width, before.height));
    assert_eq!(
        jpeg::estimate_quality(&stripped),
        jpeg::estimate_quality(&bytes)
    );
    // No APPn or COM markers survive.
    let mut pos = 2;
    while pos + 1 < stripped.len() {
        if stripped[pos] != 0xff {
            pos += 1;
            continue;
        }
        let mut end = pos + 1;
        while end < stripped.len() && stripped[end] == 0xff {
            end += 1;
        }
        if end >= stripped.len() {
            break;
        }
        let marker = stripped[end];
        assert!(
            !((0xe0..=0xef).contains(&marker) || marker == 0xfe),
            "marker 0xFF{marker:02X} survived the strip"
        );
        pos = end + 1;
    }
    // Stripping is idempotent: a second pass changes nothing.
    let stripped_again = jpeg::strip_metadata(&stripped).unwrap();
    assert_eq!(stripped_again, stripped);
}

#[test]
fn png_decodes_whole_suite() {
    // The C++ kInvalidFiles set: these must fail, never decode.
    for name in [
        "emptyfile.png",
        "x00n0g01.png",
        "xcrn0g04.png",
        "xlfn0g04.png",
    ] {
        let bytes = read_fixture(&format!("pngsuite/{name}"));
        assert!(png::decode(&bytes).is_err(), "{name} decodes");
    }
    let mut count = 0;
    for dir in ["png", "pngsuite", "resized"] {
        for path in fixtures_with_extension(dir, "png") {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if [
                "emptyfile.png",
                "x00n0g01.png",
                "xcrn0g04.png",
                "xlfn0g04.png",
            ]
            .contains(&name.as_str())
            {
                continue;
            }
            let bytes = fs::read(&path).unwrap();
            let info = png::scan_info(&bytes).unwrap_or_else(|_| panic!("scan {}", path.display()));
            let frame = png::decode(&bytes).unwrap_or_else(|_| panic!("decode {}", path.display()));
            assert_eq!((frame.width(), frame.height()), (info.width, info.height));
            assert!(
                matches!(
                    frame.format(),
                    PixelFormat::Gray8 | PixelFormat::Rgb888 | PixelFormat::Rgba8888
                ),
                "{}",
                path.display()
            );
            count += 1;
        }
    }
    assert!(count > 200, "expected the full PNGSuite, got {count}");
}

#[test]
fn png_known_fixtures() {
    let info = png::scan_info(&read_fixture("png/pagespeed-33x34.png")).unwrap();
    assert_eq!((info.width, info.height), (33, 34));

    // Interlaced suite member: flag set, still decodes.
    let bytes = read_fixture("pngsuite/s37i3p04.png");
    let info = png::scan_info(&bytes).unwrap();
    assert!(info.interlaced);
    let frame = png::decode(&bytes).unwrap();
    assert_eq!((frame.width(), frame.height()), (info.width, info.height));

    // 1-bit gray and 1-bit palette expand to Gray8 and RGB888.
    let gray1 = png::decode(&read_fixture("pngsuite/basn0g01.png")).unwrap();
    assert_eq!(gray1.format(), PixelFormat::Gray8);
    let pal1 = png::decode(&read_fixture("pngsuite/basn3p01.png")).unwrap();
    assert_eq!(pal1.format(), PixelFormat::Rgb888);
    // 16-bit gray strips to Gray8.
    let gray16 = png::decode(&read_fixture("pngsuite/basn0g16.png")).unwrap();
    assert_eq!(gray16.format(), PixelFormat::Gray8);
    // Transparent grayscale goes RGBA, like png_set_gray_to_rgb.
    let tg = png::decode(&read_fixture("pngsuite/tbbn1g04.png")).unwrap();
    assert_eq!(tg.format(), PixelFormat::Rgba8888);
}

#[test]
fn png_round_trip_is_pixel_identical() {
    for name in [
        "png/pagespeed-33x34.png",
        "pngsuite/basn0g01.png",
        "pngsuite/basn2c08.png",
        "pngsuite/basn6a08.png",
        "pngsuite/basn3p08.png",
        "pngsuite/tbbn1g04.png",
    ] {
        let bytes = read_fixture(name);
        let frame = png::decode(&bytes).unwrap();
        let encoded = png::encode(&frame, &png::PngEncodeOptions::default()).unwrap();
        assert_eq!(sniff(&encoded), ImageType::Png, "{name}");
        let decoded = png::decode(&encoded).unwrap();
        assert_eq!(decoded, frame, "{name}");
    }
}

#[test]
fn png_transparency_probe_matches_decode() {
    // Opaque fixtures probe false; alpha fixtures probe true. The probe is
    // conservative: pagespeed-33x34 carries an alpha channel, so it probes
    // true even if every pixel turns out opaque.
    assert!(!png::has_transparency_probe(&read_fixture(
        "pngsuite/basn0g08.png"
    )));
    assert!(!png::has_transparency_probe(&read_fixture(
        "pngsuite/basn2c08.png"
    )));
    assert!(png::has_transparency_probe(&read_fixture(
        "pngsuite/basn6a08.png"
    )));
    assert!(png::has_transparency_probe(&read_fixture(
        "pngsuite/tbbn1g04.png"
    )));
    assert!(png::has_transparency_probe(&read_fixture(
        "png/pagespeed-33x34.png"
    )));
    // Cross-check the probe against decoded pixels for the whole suite: real
    // transparency must always probe true (the reverse need not hold).
    for path in fixtures_with_extension("pngsuite", "png") {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if [
            "emptyfile.png",
            "x00n0g01.png",
            "xcrn0g04.png",
            "xlfn0g04.png",
        ]
        .contains(&name.as_str())
        {
            continue;
        }
        let bytes = fs::read(&path).unwrap();
        let frame = png::decode(&bytes).unwrap();
        if frame.has_transparency() {
            assert!(
                png::has_transparency_probe(&bytes),
                "{}: pixels transparent but probe says opaque",
                path.display()
            );
        }
    }
}

#[test]
fn gif_first_frames_match_rgba_ground_truth() {
    // Every pngsuite gif with a .rgba twin must render byte-identical to the
    // C++-era reference composite.
    let mut count = 0;
    for path in fixtures_with_extension("pngsuite/gif", "gif") {
        let rgba_path = path.with_extension("gif.rgba");
        if !rgba_path.exists() {
            continue;
        }
        let bytes = fs::read(&path).unwrap();
        let expected = fs::read(&rgba_path).unwrap();
        let frame =
            gif::first_frame(&bytes).unwrap_or_else(|_| panic!("first frame {}", path.display()));
        assert_eq!(frame.format(), PixelFormat::Rgba8888, "{}", path.display());
        assert_eq!(
            expected.len(),
            frame.width() as usize * frame.height() as usize * 4,
            "{}",
            path.display()
        );
        assert_eq!(frame.pixels(), expected.as_slice(), "{}", path.display());
        count += 1;
    }
    // Every .rgba file in the corpus must have been exercised.
    let twins = fs::read_dir(data_dir().join("pngsuite/gif"))
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .is_ok_and(|entry| entry.path().extension().is_some_and(|ext| ext == "rgba"))
        })
        .count();
    assert_eq!(count, twins);
    assert!(count > 0, "no .rgba twins found");
}

#[test]
fn gif_animation_detection_and_iteration() {
    assert!(gif::is_animated(&read_fixture("gif/animated.gif")).unwrap());
    assert!(gif::is_animated(&read_fixture("gif/full2loop.gif")).unwrap());
    assert!(gif::is_animated(&read_fixture("gif/square2loop.gif")).unwrap());
    assert!(!gif::is_animated(&read_fixture("gif/transparent.gif")).unwrap());
    assert!(!gif::is_animated(&read_fixture("gif/interlaced.gif")).unwrap());

    // Full iteration terminates with sane delays and canvas sizes.
    let bytes = read_fixture("gif/animated.gif");
    let mut frames = gif::Frames::open(&bytes).unwrap();
    let header = frames.header();
    assert!(header.width > 0 && header.height > 0);
    let mut count = 0;
    let mut saw_delay = false;
    while let Some(frame) = frames.next_frame().unwrap() {
        assert_eq!(
            (frame.canvas.width(), frame.canvas.height()),
            (header.width, header.height)
        );
        assert_eq!(frame.canvas.format(), PixelFormat::Rgba8888);
        saw_delay |= frame.delay_ms > 0;
        count += 1;
        assert!(count < 1000, "animation runaway");
    }
    assert!(count >= 2, "animated.gif yielded {count} frames");
    assert!(saw_delay, "no frame carried a delay");
    // Second pass after exhaustion stays exhausted.
    assert!(frames.next_frame().unwrap().is_none());
}

#[test]
fn gif_loop_counts_follow_chrome_quirk() {
    // loop_plays = Netscape repeats + 1; 1 when no extension is present.
    let looped = gif::probe(&read_fixture("gif/full2loop.gif")).unwrap();
    assert!(looped.loop_plays >= 2, "full2loop: {}", looped.loop_plays);
    let single = gif::probe(&read_fixture("gif/transparent.gif")).unwrap();
    assert_eq!(single.loop_plays, 1);
}

#[test]
fn gif_handles_edge_cases() {
    // Zero-size canvas grows to the first frame (Chrome quirk).
    let zero = gif::first_frame(&read_fixture("gif/zero_size_animation.gif")).unwrap();
    assert!(zero.width() > 0 && zero.height() > 0);
    // Empty screen and fully transparent inputs render without failing.
    assert!(gif::first_frame(&read_fixture("gif/red_empty_screen.gif")).is_ok());
    let clear = gif::first_frame(&read_fixture("gif/completely_transparent.gif")).unwrap();
    assert!(clear.pixels().iter().skip(3).step_by(4).all(|&a| a == 0));
    // Frame smaller than the screen composites onto transparency.
    let small = gif::first_frame(&read_fixture("gif/frame_smaller_than_screen.gif")).unwrap();
    assert!(small.width() > 0 && small.height() > 0);
    // Out-of-range palette indices decode without failing (transparent in
    // the frame path, like the C++ InvalidPixels tests).
    assert!(gif::first_frame(&read_fixture("gif/bad_pixel_global_palette.gif")).is_ok());
    assert!(gif::first_frame(&read_fixture("gif/bad_pixel_local_palette.gif")).is_ok());
    // Truncated input fails instead of rendering garbage.
    assert!(gif::first_frame(&read_fixture("gif/bad.gif")).is_err());
    assert!(gif::is_animated(&read_fixture("gif/bad.gif")).is_err());
}

#[test]
fn gif_interlaced_matches_png_render() {
    // interlaced.png is the expected render of interlaced.gif.
    let gif_frame = gif::first_frame(&read_fixture("gif/interlaced.gif")).unwrap();
    let png_frame = png::decode(&read_fixture("gif/interlaced.png")).unwrap();
    assert_eq!(
        (png_frame.width(), png_frame.height()),
        (gif_frame.width(), gif_frame.height())
    );
    let gif_rgb = gif_frame.strip_opaque_alpha();
    match png_frame.format() {
        PixelFormat::Rgb888 => assert_eq!(png_frame.pixels(), gif_rgb.pixels()),
        PixelFormat::Rgba8888 => assert_eq!(png_frame.pixels(), gif_frame.pixels()),
        PixelFormat::Gray8 => {
            let gray = gif_rgb
                .pixels()
                .iter()
                .step_by(3)
                .copied()
                .collect::<Vec<_>>();
            assert_eq!(png_frame.pixels(), gray.as_slice());
        }
        PixelFormat::Unsupported => panic!("unexpected PNG format"),
    }
}

/// PSNR in dB between two same-shape RGB(A) frames, ignoring RGB where the
/// reference is transparent (the C++ `ignore_transparent_rgb` comparison).
fn psnr_db(reference: &Frame, actual: &Frame) -> f64 {
    assert_eq!(
        (actual.width(), actual.height()),
        (reference.width(), reference.height())
    );
    let ref_rgba = reference.expand(PixelFormat::Rgba8888).unwrap();
    let act_rgba = actual.expand(PixelFormat::Rgba8888).unwrap();
    let mut sum_sq = 0u64;
    let mut count = 0u64;
    for (expected, actual) in ref_rgba
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(act_rgba.pixels().as_chunks::<4>().0.iter())
    {
        // Alpha always compares; RGB only where the reference is visible.
        let diff = i32::from(expected[3]) - i32::from(actual[3]);
        sum_sq += (diff * diff) as u64;
        count += 1;
        if expected[3] != 0 {
            for channel in 0..3 {
                let diff = i32::from(expected[channel]) - i32::from(actual[channel]);
                sum_sq += (diff * diff) as u64;
                count += 1;
            }
        }
    }
    if sum_sq == 0 {
        return f64::INFINITY;
    }
    let mse = sum_sq as f64 / count as f64;
    20.0 * (255.0 / mse.sqrt()).log10()
}

#[test]
fn webp_decode_fixtures() {
    let opaque = webp::decode(&read_fixture("webp/opaque_32x20.webp")).unwrap();
    assert_eq!((opaque.width(), opaque.height()), (32, 20));
    assert_eq!(opaque.format(), PixelFormat::Rgb888);

    let alpha = webp::decode(&read_fixture("webp/alpha_32x32.webp")).unwrap();
    assert_eq!((alpha.width(), alpha.height()), (32, 32));
    assert_eq!(alpha.format(), PixelFormat::Rgba8888);
    assert!(alpha.has_transparency());

    let gray = webp::decode(&read_fixture("webp/gray_saved_as_rgb.webp")).unwrap();
    assert_eq!(gray.format(), PixelFormat::Rgb888);

    // Corrupt inputs fail; animated input is refused like the C++ reader.
    assert!(webp::decode(&read_fixture("webp/corrupt_body.webp")).is_err());
    assert!(webp::decode(&read_fixture("webp/corrupt_header.webp")).is_err());
    assert!(webp::decode(&read_fixture("webp/animated.webp")).is_err());
}

#[test]
fn webp_lossless_round_trip_is_identical() {
    for name in [
        "webp/opaque_32x20.png",
        "webp/alpha_32x32.png",
        "webp/pagespeed_32x32_gray.png",
        "png/pagespeed-33x34.png",
    ] {
        let source = png::decode(&read_fixture(name)).unwrap();
        let config = webp::WebpEncodeConfig {
            lossless: true,
            ..Default::default()
        };
        let encoded = webp::encode(&source, &config).unwrap();
        let features = webp_features(&encoded).expect("features");
        assert!(features.is_lossless, "{name}");
        let decoded = webp::decode(&encoded).unwrap();
        assert_eq!(
            (decoded.width(), decoded.height()),
            (source.width(), source.height()),
            "{name}"
        );
        // Grayscale sources come back as RGB (WebP has no gray mode).
        // Transparent RGB is ignored: libwebp does not preserve color under
        // fully transparent pixels, which is why the C++ comparisons pass
        // ignore_transparent_rgb=true.
        let source_rgba = source.expand(PixelFormat::Rgba8888).unwrap();
        let decoded_rgba = decoded.expand(PixelFormat::Rgba8888).unwrap();
        assert_eq!(
            source_rgba.pixels().len(),
            decoded_rgba.pixels().len(),
            "{name}"
        );
        for (index, (expected, actual)) in source_rgba
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .zip(decoded_rgba.pixels().as_chunks::<4>().0.iter())
            .enumerate()
        {
            assert_eq!(actual[3], expected[3], "{name} pixel {index} alpha");
            if expected[3] != 0 {
                assert_eq!(actual[..3], expected[..3], "{name} pixel {index} color");
            }
        }
    }
}

#[test]
fn webp_lossy_round_trip_meets_psnr_floor() {
    // Port of the kMinPSNR = 33 dB lossy gate over photographic content. The
    // RGBA source encodes to lossy-with-alpha (LosslessOrAlpha); the opaque
    // one to plain lossy.
    for (name, expected) in [
        ("webp/opaque_32x20.png", ImageType::Webp),
        (
            "webp/partially_opaque_32x20.png",
            ImageType::WebpLosslessOrAlpha,
        ),
    ] {
        let source = png::decode(&read_fixture(name)).unwrap();
        let config = webp::WebpEncodeConfig {
            lossless: false,
            quality: 75.0,
            ..Default::default()
        };
        let encoded = webp::encode(&source, &config).unwrap();
        assert_eq!(sniff(&encoded), expected, "{name}");
        let decoded = webp::decode(&encoded).unwrap();
        let psnr = psnr_db(&source, &decoded);
        assert!(psnr >= 33.0, "{name}: PSNR {psnr:.1} dB");
    }
    // Synthetic graphics (sharp edges) score lower at q75; quality must
    // still improve monotonically with the quality setting.
    let logo = png::decode(&read_fixture("png/pagespeed-33x34.png")).unwrap();
    let mut last = 0.0;
    for quality in [50.0, 75.0, 95.0] {
        let config = webp::WebpEncodeConfig {
            lossless: false,
            quality,
            ..Default::default()
        };
        let encoded = webp::encode(&logo, &config).unwrap();
        let decoded = webp::decode(&encoded).unwrap();
        let psnr = psnr_db(&logo, &decoded);
        assert!(psnr > last, "q{quality}: PSNR {psnr:.1} dB, last {last:.1}");
        last = psnr;
    }
}

#[test]
fn webp_alpha_gate_matches_cpp() {
    let opaque = png::decode(&read_fixture("webp/opaque_32x20.png")).unwrap();
    let alpha = png::decode(&read_fixture("webp/alpha_32x32.png")).unwrap();
    assert!(alpha.has_transparency());
    // alpha_quality == 0 disables alpha: opaque input encodes, transparent
    // input fails instead of silently dropping the channel.
    let config = webp::WebpEncodeConfig {
        lossless: false,
        quality: 80.0,
        alpha_quality: 0,
        alpha_compression: 0,
        ..Default::default()
    };
    assert!(webp::encode(&opaque, &config).is_ok());
    assert!(webp::encode(&alpha, &config).is_err());
}

#[test]
fn webp_animated_encode_from_gif() {
    // Collect composited canvases from an animated GIF, like
    // ConvertAnimatedGifToWebp, and encode them as animated WebP.
    let bytes = read_fixture("gif/animated.gif");
    let mut iterator = gif::Frames::open(&bytes).unwrap();
    let mut frames = Vec::new();
    while let Some(frame) = iterator.next_frame().unwrap() {
        frames.push((frame.canvas, frame.delay_ms.max(10)));
    }
    assert!(frames.len() >= 2);
    let (encoded, timed_out) =
        webp::encode_animated(&frames, &webp::WebpAnimConfig::default(), None).unwrap();
    assert!(!timed_out);
    assert_eq!(sniff(&encoded), ImageType::WebpAnimated);
    // Structure: VP8X with the animation flag, one ANIM chunk, and at most
    // one ANMF chunk per input frame: libwebp merges pixel-identical
    // consecutive frames (animated.gif ends with a duplicate), so the
    // duration sum — not the chunk count — must be preserved.
    let mut anim_chunks = 0;
    let mut anmf_chunks = 0;
    let mut anmf_duration_ms = 0u32;
    let mut pos = 12;
    assert_eq!(&encoded[..4], b"RIFF");
    assert_eq!(&encoded[8..12], b"WEBP");
    assert_eq!(&encoded[12..16], b"VP8X");
    assert_ne!(encoded[20] & 0x02, 0, "animation flag missing");
    while pos + 8 <= encoded.len() {
        let id = &encoded[pos..pos + 4];
        let size = u32::from_le_bytes([
            encoded[pos + 4],
            encoded[pos + 5],
            encoded[pos + 6],
            encoded[pos + 7],
        ]) as usize;
        if id == b"ANIM" {
            anim_chunks += 1;
        } else if id == b"ANMF" {
            anmf_chunks += 1;
            // ANMF frame header: 12 bytes of rect/dims, then 24-bit duration.
            let duration = u32::from(encoded[pos + 8 + 12])
                | (u32::from(encoded[pos + 8 + 13]) << 8)
                | (u32::from(encoded[pos + 8 + 14]) << 16);
            anmf_duration_ms += duration;
        }
        // Chunks pad to even sizes when the payload is odd.
        pos = pos + 8 + size + (size & 1);
        if pos > encoded.len() {
            break;
        }
    }
    assert_eq!(anim_chunks, 1);
    assert!(anmf_chunks <= frames.len());
    assert!(anmf_chunks >= 2);
    let input_duration_ms: u32 = frames.iter().map(|(_, delay)| delay).sum();
    assert_eq!(anmf_duration_ms, input_duration_ms);
}

#[test]
fn webp_deadline_is_honored() {
    use std::time::{Duration, Instant};
    let source = png::decode(&read_fixture("png/large.png")).unwrap();
    let config = webp::WebpEncodeConfig {
        lossless: false,
        quality: 75.0,
        ..Default::default()
    };
    // A generous deadline never fires.
    let (bytes, timed_out) = webp::encode_with_deadline(
        &source,
        &config,
        Some(Instant::now() + Duration::from_secs(60)),
    )
    .unwrap();
    assert!(!timed_out);
    assert!(!bytes.is_empty());
    // An expired deadline either aborts the encode or (when the progress
    // hook never fires on a fast encode) succeeds without hanging.
    let _ = webp::encode_with_deadline(&source, &config, Some(Instant::now()));
}

#[test]
fn resizer_matches_goldens_bit_exact() {
    // Port of ScanlineResizerTest.Accuracy/LargeImage via ResizeAndValidateImage:
    // the kOutputSize request table, with golden names formed by substituting
    // a preserved (0) side with the other side — note the named dims are not
    // the actual dims for non-square inputs (large_w16_h16.png is 16x8).
    let inputs = [
        ("pngsuite/basi0g04.png", "basi0g04"),
        ("pngsuite/basi3p02.png", "basi3p02"),
        ("pngsuite/basn6a16.png", "basn6a16"),
        ("png/pagespeed-33x34.png", "pagespeed-33x34"),
        ("png/large.png", "large"),
    ];
    let requests = [
        (16, 0),
        (0, 8),
        (3, 3),
        (16, 25),
        (32, 5),
        (3, 32),
        (31, 31),
        (32, 32),
    ];
    for (input_name, base) in inputs {
        let input = png::decode(&read_fixture(input_name)).unwrap();
        for (request_width, request_height) in requests {
            let name_width = if request_width == 0 {
                request_height
            } else {
                request_width
            };
            let name_height = if request_height == 0 {
                request_width
            } else {
                request_height
            };
            let golden_name = format!("resized/{base}_w{name_width}_h{name_height}.png");
            let golden = png::decode(&read_fixture(&golden_name)).unwrap();
            let resized =
                resizer::resize(&input, request_width, request_height).unwrap_or_else(|_| {
                    panic!("resize {input_name} to {request_width}x{request_height}")
                });
            assert_eq!(
                (resized.width(), resized.height()),
                (golden.width(), golden.height()),
                "{golden_name} dims"
            );
            assert_eq!(resized.format(), golden.format(), "{golden_name} format");
            assert_eq!(resized.pixels(), golden.pixels(), "{golden_name} pixels");
        }
    }
}

#[test]
fn resizer_preserve_ratio_matches_cpp() {
    // Port of the kOutputSize preserve-ratio rows plus the Initialize tests:
    // (16,0) and (0,8) on 32x32 inputs hit the w16_h16/w8_h8 goldens.
    for (input_name, base) in [
        ("pngsuite/basi0g04.png", "basi0g04"),
        ("pngsuite/basn6a16.png", "basn6a16"),
    ] {
        let input = png::decode(&read_fixture(input_name)).unwrap();
        for (request, golden_dims) in [((16, 0), (16, 16)), ((0, 8), (8, 8))] {
            let resized = resizer::resize(&input, request.0, request.1).unwrap();
            assert_eq!(
                (resized.width(), resized.height()),
                golden_dims,
                "{input_name} {request:?}"
            );
            let golden = png::decode(&read_fixture(&format!(
                "resized/{base}_w{}_h{}.png",
                golden_dims.0, golden_dims.1
            )))
            .unwrap();
            assert_eq!(
                resized.pixels(),
                golden.pixels(),
                "{input_name} {request:?}"
            );
        }
    }
    // 33x34 input: (16,0) rounds to 16x16, (0,8) rounds to 8x8.
    let input = png::decode(&read_fixture("png/pagespeed-33x34.png")).unwrap();
    assert_eq!(resizer::resize(&input, 16, 0).unwrap().width(), 16);
    assert_eq!(resizer::resize(&input, 16, 0).unwrap().height(), 16);
    assert_eq!((resizer::resize(&input, 0, 8).unwrap().width()), 8);
    assert_eq!((resizer::resize(&input, 0, 8).unwrap().height()), 8);
    // Empty and degenerate requests fail instead of panicking.
    assert!(resizer::resize(&input, 0, 0).is_err());
}

#[test]
fn photo_detection_separates_photos_from_graphics() {
    // Photographic JPEG content detects as photo; flat graphics do not.
    // (JPEG inputs always count as photos in AnalyzeImage; here the metric
    // itself is exercised on decoded pixels.)
    let photo = jpeg::decode(&read_fixture("jpeg/sjpeg3.jpg")).unwrap();
    assert!(
        analysis::is_photo(&photo),
        "sjpeg3 metric {}",
        analysis::photo_metric(&photo)
    );
    let flat = Frame::solid(64, 64, PixelFormat::Rgb888, &[128, 128, 128]).unwrap();
    assert!(!analysis::is_photo(&flat));
    // Grayscale photo content also detects.
    let gray_photo = jpeg::decode(&read_fixture("jpeg/testgray.jpg")).unwrap();
    assert_eq!(gray_photo.format(), PixelFormat::Gray8);
    let metric = analysis::photo_metric(&gray_photo);
    assert!(
        metric >= analysis::PHOTO_METRIC_THRESHOLD,
        "testgray metric {metric}"
    );
    // The PNGSuite photographic image detects; palette graphics do not.
    let suite_photo = png::decode(&read_fixture("pngsuite/f04n2c08.png")).unwrap();
    assert!(analysis::is_photo(&suite_photo));
    let graphic = png::decode(&read_fixture("png/pagespeed-128.png")).unwrap();
    assert!(!analysis::is_photo(&graphic.strip_opaque_alpha()));
    // RGBA input never counts as photo, whatever the metric says.
    let rgba = png::decode(&read_fixture("png/pagespeed-33x34.png")).unwrap();
    assert_eq!(rgba.format(), PixelFormat::Rgba8888);
    assert!(!analysis::is_photo(&rgba));
}

#[test]
fn blank_images_generate_valid_png() {
    // Blanks are zero-filled (black / transparent black), uncompressed.
    let black = ps_image::frame::generate_blank_image(8, 8, false).unwrap();
    let frame = png::decode(&black).unwrap();
    assert_eq!(frame.format(), PixelFormat::Rgb888);
    assert!(frame.pixels().iter().all(|&b| b == 0));

    let transparent = ps_image::frame::generate_blank_image(4, 5, true).unwrap();
    let frame = png::decode(&transparent).unwrap();
    assert_eq!(frame.format(), PixelFormat::Rgba8888);
    assert_eq!((frame.width(), frame.height()), (4, 5));
    assert!(frame.pixels().iter().all(|&b| b == 0));
}
