//! Driver tests: analysis, `ImageOptimizer::Optimize`, the transcode matrix,
//! resizing, and the external AVIF pipeline with a fake encoder.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use ps_image::format::{sniff, ImageFormat, ImageType};
use ps_image::image::{
    analyze, optimize, resize_image, transcode, CompressionOptions, ConversionBudget, ImageOptions,
    PreferredWebp, RequestedDimensions, TranscodeContext,
};
use ps_image::stats::{NullRecorder, TestRecorder};

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

fn read_fixture(name: &str) -> Vec<u8> {
    fs::read(data_dir().join(name)).unwrap_or_else(|_| panic!("missing fixture {name}"))
}

fn default_ctx() -> TranscodeContext {
    TranscodeContext {
        resized: false,
        resized_dims: None,
        original_type: None,
    }
}

#[test]
fn analysis_reports_per_type() {
    let jpeg = analyze(&read_fixture("jpeg/test420.jpg")).unwrap();
    assert_eq!((jpeg.width, jpeg.height), (130, 97));
    assert!(!jpeg.progressive && !jpeg.animated);
    assert!(jpeg.photo && !jpeg.transparency);
    assert!(jpeg.quality.is_some());

    let progressive = analyze(&read_fixture("jpeg/progressive.jpg")).unwrap();
    assert!(progressive.progressive);

    let png = analyze(&read_fixture("pngsuite/basn6a08.png")).unwrap();
    assert!(png.transparency);
    assert_eq!(png.quality, None);

    let gif = analyze(&read_fixture("gif/animated.gif")).unwrap();
    assert!(gif.animated && !gif.progressive);

    let single_gif = analyze(&read_fixture("gif/transparent.gif")).unwrap();
    assert!(!single_gif.animated && single_gif.transparency);

    assert!(analyze(&read_fixture("webp/animated.webp")).is_err());
    assert!(analyze(b"definitely not an image....").is_err());
}

#[test]
fn optimizer_matrix_defaults() {
    let options = ImageOptions::default();
    let no_resize = RequestedDimensions::default();
    // Photo JPEG with defaults: lossy WebP, smaller than the input.
    for name in ["jpeg/sjpeg2.jpg", "jpeg/sjpeg4.jpg"] {
        let bytes = read_fixture(name);
        let out = optimize(&bytes, &options, no_resize, None)
            .unwrap_or_else(|_| panic!("optimize {name}"));
        assert_eq!(out.format, ImageFormat::Webp, "{name}");
        assert!(out.uses_lossy);
        assert!(out.bytes.len() < bytes.len(), "{name} did not shrink");
        assert_eq!(sniff(&out.bytes), ImageType::Webp);
    }
    // Graphic PNG with defaults: optimized PNG.
    let bytes = read_fixture("pngsuite/basn3p08.png");
    let out = optimize(&bytes, &options, no_resize, None).unwrap();
    assert_eq!(out.format, ImageFormat::Png);
    assert!(!out.uses_lossy);
    assert!(out.bytes.len() <= bytes.len());
    // Transparent PNG with defaults: stays PNG (lossless WebP not allowed).
    let bytes = read_fixture("pngsuite/basn6a08.png");
    let out = optimize(&bytes, &options, no_resize, None).unwrap();
    assert_eq!(out.format, ImageFormat::Png);
}

#[test]
fn optimizer_refusals_and_gates() {
    let options = ImageOptions::default();
    let no_resize = RequestedDimensions::default();
    // WebP/AVIF/unknown inputs are refused.
    assert!(optimize(
        &read_fixture("webp/opaque_32x20.webp"),
        &options,
        no_resize,
        None
    )
    .is_err());
    assert!(optimize(b"definitely not an image....", &options, no_resize, None).is_err());
    // Animated GIF without animated WebP allowed: refused.
    assert!(optimize(&read_fixture("gif/animated.gif"), &options, no_resize, None).is_err());
    // Nothing allowed: refused.
    let none = ImageOptions {
        allow_png: false,
        allow_jpeg: false,
        allow_webp_lossy: false,
        ..Default::default()
    };
    assert!(optimize(&read_fixture("jpeg/sjpeg2.jpg"), &none, no_resize, None).is_err());
    // The byte gate bites: a 1x1 GIF (43 bytes) cannot beat PNG
    // container overhead, so must_reduce_bytes refuses it.
    let bytes = b"\x47\x49\x46\x38\x37\x61\x01\x00\x01\x00\x81\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x08\x04\x00\x01\x04\x04\x00\x3b".to_vec();
    assert!(optimize(&bytes, &options, no_resize, None).is_err());
    let no_gate = ImageOptions {
        must_reduce_bytes: false,
        ..Default::default()
    };
    let out = optimize(&bytes, &no_gate, no_resize, None).unwrap();
    assert_eq!(out.format, ImageFormat::Png);
}

#[test]
fn optimizer_animated_and_resize() {
    let options = ImageOptions {
        allow_webp_animated: true,
        ..Default::default()
    };
    let out = optimize(
        &read_fixture("gif/animated.gif"),
        &options,
        RequestedDimensions::default(),
        None,
    )
    .unwrap();
    assert_eq!(out.format, ImageFormat::Webp);
    assert!(!out.uses_lossy, "optimizer animation is lossless");
    assert_eq!(sniff(&out.bytes), ImageType::WebpAnimated);

    // Resize requests flow to the output dimensions.
    let options = ImageOptions::default();
    let out = optimize(
        &read_fixture("jpeg/sjpeg4.jpg"),
        &options,
        RequestedDimensions {
            width: Some(256),
            height: Some(192),
        },
        None,
    )
    .unwrap();
    assert_eq!((out.width, out.height), (256, 192));
    // Oversize requests keep the original dimensions without failing.
    let out = optimize(
        &read_fixture("jpeg/sjpeg4.jpg"),
        &options,
        RequestedDimensions {
            width: Some(4096),
            height: None,
        },
        None,
    )
    .unwrap();
    assert_eq!((out.width, out.height), (512, 384));
}

#[test]
fn transcode_jpeg_paths() {
    let recorder = NullRecorder;
    let mut budget = ConversionBudget::new();
    // JPEG -> WebP with negotiation for lossy.
    let options = CompressionOptions {
        convert_jpeg_to_webp: true,
        preferred_webp: PreferredWebp::Lossy,
        ..Default::default()
    };
    let out = transcode(
        &read_fixture("jpeg/sjpeg2.jpg"),
        Some((130, 97)),
        default_ctx(),
        &options,
        &mut budget,
        false,
        &recorder,
    )
    .unwrap();
    assert_eq!(out.image_type, ImageType::Webp);
    assert_eq!(sniff(&out.bytes), ImageType::Webp);
    // JPEG recompress only: rewrites. Budget is consumed per
    // attempt (options first), as the C++ filter and image tests
    // require; the snapshot's MayConvert-first order would dead-end
    // this gate.
    let mut budget = ConversionBudget::new();
    let options = CompressionOptions {
        recompress_jpeg: true,
        ..Default::default()
    };
    let input = read_fixture("jpeg/app_segments.jpg");
    let out = transcode(
        &input,
        None,
        default_ctx(),
        &options,
        &mut budget,
        false,
        &recorder,
    )
    .unwrap();
    assert_eq!(out.image_type, ImageType::Jpeg);
    assert_eq!(sniff(&out.bytes), ImageType::Jpeg);
    assert!(out.bytes.len() < input.len());
    // Nothing enabled: no rewrite.
    let mut budget = ConversionBudget::new();
    assert!(transcode(
        &read_fixture("jpeg/sjpeg2.jpg"),
        None,
        default_ctx(),
        &CompressionOptions::default(),
        &mut budget,
        false,
        &recorder,
    )
    .is_err());
}

#[test]
fn transcode_png_and_gif_paths() {
    let recorder = TestRecorder::new();
    // Photo PNG -> JPEG when conversion is on.
    let options = CompressionOptions {
        convert_png_to_jpeg: true,
        ..Default::default()
    };
    let mut budget = ConversionBudget::new();
    let out = transcode(
        &read_fixture("pngsuite/f04n2c08.png"),
        None,
        default_ctx(),
        &options,
        &mut budget,
        false,
        &recorder,
    )
    .unwrap();
    assert_eq!(out.image_type, ImageType::Jpeg);
    // Graphic PNG -> lossless WebP when preferred.
    let options = CompressionOptions {
        preferred_webp: PreferredWebp::Lossless,
        ..Default::default()
    };
    let mut budget = ConversionBudget::new();
    let out = transcode(
        &read_fixture("pngsuite/basn3p08.png"),
        None,
        default_ctx(),
        &options,
        &mut budget,
        false,
        &recorder,
    )
    .unwrap();
    assert_eq!(out.image_type, ImageType::WebpLosslessOrAlpha);
    // GIF -> PNG when enabled.
    let options = CompressionOptions {
        convert_gif_to_png: true,
        ..Default::default()
    };
    let mut budget = ConversionBudget::new();
    let out = transcode(
        &read_fixture("gif/transparent.gif"),
        None,
        default_ctx(),
        &options,
        &mut budget,
        false,
        &recorder,
    )
    .unwrap();
    assert_eq!(out.image_type, ImageType::Png);
    // Animated GIF -> animated WebP when negotiated.
    let options = CompressionOptions {
        preferred_webp: PreferredWebp::Animated,
        allow_webp_animated: true,
        ..Default::default()
    };
    let mut budget = ConversionBudget::new();
    let out = transcode(
        &read_fixture("gif/animated.gif"),
        None,
        default_ctx(),
        &options,
        &mut budget,
        false,
        &recorder,
    )
    .unwrap();
    assert_eq!(out.image_type, ImageType::WebpAnimated);
    // WebP conversions were recorded (source + opacity entries each).
    assert!(!recorder.webp_events.lock().unwrap().is_empty());
}

#[test]
fn transcode_low_quality_shrinks_output() {
    let recorder = NullRecorder;
    let options = CompressionOptions {
        convert_jpeg_to_webp: true,
        preferred_webp: PreferredWebp::Lossy,
        ..Default::default()
    };
    let bytes = read_fixture("jpeg/sjpeg4.jpg");
    let mut budget = ConversionBudget::new();
    let normal = transcode(
        &bytes,
        None,
        default_ctx(),
        &options,
        &mut budget,
        false,
        &recorder,
    )
    .unwrap()
    .bytes
    .len();
    let mut budget = ConversionBudget::new();
    let low = transcode(
        &bytes,
        None,
        default_ctx(),
        &options,
        &mut budget,
        true,
        &recorder,
    )
    .unwrap()
    .bytes
    .len();
    assert!(low < normal, "low-quality {low} vs normal {normal}");
}

#[test]
fn transcode_budget_caps_attempts() {
    // AVIF is enabled but the encoder fails: the AVIF attempt fails (1),
    // WebP succeeds (2) — the matrix still lands on WebP. The failing
    // encoder is faked: the real `avifenc` exists on some hosts (and
    // sibling tests re-point PATH at fakes mid-run), so an unfaked spawn
    // flips this test with the environment and thread timing.
    with_fake_avifenc("fail", || {
        let recorder = TestRecorder::new();
        let options = CompressionOptions {
            convert_to_avif: true,
            convert_jpeg_to_webp: true,
            preferred_webp: PreferredWebp::Lossy,
            avif_conversion_timeout_ms: -1,
            ..Default::default()
        };
        let mut budget = ConversionBudget::new();
        let out = transcode(
            &read_fixture("jpeg/sjpeg2.jpg"),
            None,
            default_ctx(),
            &options,
            &mut budget,
            false,
            &recorder,
        )
        .unwrap();
        assert_eq!(out.image_type, ImageType::Webp);
        // The failed AVIF run was recorded.
        assert_eq!(recorder.avif_events.lock().unwrap().len(), 1);
    });
}

#[test]
fn resize_image_paths() {
    let options = CompressionOptions::default();
    let (out, format) = resize_image(&read_fixture("jpeg/sjpeg4.jpg"), 256, 192, &options).unwrap();
    assert_eq!(format, ImageFormat::Jpeg);
    let frame = ps_image::jpeg::decode(&out).unwrap();
    assert_eq!((frame.width(), frame.height()), (256, 192));
    let (out, format) =
        resize_image(&read_fixture("png/pagespeed-33x34.png"), 16, 16, &options).unwrap();
    assert_eq!(format, ImageFormat::Png);
    let frame = ps_image::png::decode(&out).unwrap();
    assert_eq!((frame.width(), frame.height()), (16, 16));
    // GIF input converts to PNG through the resize.
    let (out, format) =
        resize_image(&read_fixture("gif/transparent.gif"), 160, 160, &options).unwrap();
    assert_eq!(format, ImageFormat::Png);
    assert_eq!(sniff(&out), ImageType::Png);
    // WebP input is refused.
    assert!(resize_image(&read_fixture("webp/opaque_32x20.webp"), 16, 10, &options).is_err());
    assert!(resize_image(&read_fixture("jpeg/sjpeg4.jpg"), 0, 192, &options).is_err());
}

// ---------------------------------------------------------------------------
// AVIF pipeline tests with a fake encoder.
// ---------------------------------------------------------------------------

/// Serializes the AVIF tests: they share process-global PATH, the encoder
/// slot counter, and /tmp staging names.
fn avif_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn fake_avifenc_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ps_fake_avifenc_{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let script = dir.join("avifenc");
    fs::write(
        &script,
        r#"#!/bin/sh
# Fake avifenc: last two args are input and output paths.
mode="${FAKE_AVIFENC_MODE:-ok}"
out=""
prev=""
for arg in "$@"; do
  if [ "$prev" != "" ]; then out="$arg"; fi
  prev="$arg"
done
# Record argv for the CQ-mapping test.
if [ -n "$FAKE_AVIFENC_ARGV" ]; then echo "$@" > "$FAKE_AVIFENC_ARGV"; fi
case "$mode" in
  fail) exit 1;;
  sleep) sleep 30;;
esac
# Minimal plausible AVIF: 64 bytes with ftypavif at the box header.
python3 -c "import sys; b = bytearray(64); b[4:8] = b'ftyp'; b[8:12] = b'avif'; open(sys.argv[1], 'wb').write(b)" "$out"
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

/// Runs `f` with PATH prefixed by the fake encoder dir. Holds the AVIF test
/// lock for the whole call; restores PATH afterwards.
fn with_fake_avifenc<R>(mode: &str, f: impl FnOnce() -> R) -> R {
    let _guard = avif_test_lock().lock().unwrap();
    let dir = fake_avifenc_dir();
    let old_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{old_path}", dir.to_string_lossy()));
    std::env::set_var("FAKE_AVIFENC_MODE", mode);
    let result = f();
    std::env::set_var("PATH", old_path);
    std::env::remove_var("FAKE_AVIFENC_MODE");
    result
}

#[test]
fn avif_success_path() {
    with_fake_avifenc("ok", || {
        let recorder = TestRecorder::new();
        let request = ps_image::avif::AvifRequest {
            input_suffix: ".jpg",
            quality: 85,
            timeout_ms: -1,
            max_concurrent: 0,
        };
        let out =
            ps_image::avif::convert_to_avif(&read_fixture("jpeg/sjpeg2.jpg"), &request, &recorder)
                .unwrap();
        assert_eq!(sniff(&out), ImageType::Avif);
        let events = recorder.avif_events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].0, events[0].1), (true, false));
    });
}

#[test]
fn avif_failure_and_timeout_paths() {
    // Encoder exits nonzero: error + failure stats.
    with_fake_avifenc("fail", || {
        let recorder = TestRecorder::new();
        let request = ps_image::avif::AvifRequest {
            input_suffix: ".jpg",
            quality: 85,
            timeout_ms: -1,
            max_concurrent: 0,
        };
        assert!(ps_image::avif::convert_to_avif(
            &read_fixture("jpeg/sjpeg2.jpg"),
            &request,
            &recorder
        )
        .is_err());
        let events = recorder.avif_events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].0, events[0].1), (false, false));
    });
    // Hanging encoder with a short deadline: killed + timeout stats.
    with_fake_avifenc("sleep", || {
        let recorder = TestRecorder::new();
        let request = ps_image::avif::AvifRequest {
            input_suffix: ".jpg",
            quality: 85,
            timeout_ms: 100,
            max_concurrent: 0,
        };
        let err =
            ps_image::avif::convert_to_avif(&read_fixture("jpeg/sjpeg2.jpg"), &request, &recorder)
                .unwrap_err();
        assert_eq!(err.kind(), ps_image::error::ErrorKind::Timeout);
        let events = recorder.avif_events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].0, events[0].1), (false, true));
    });
    // Zero deadline: immediate timeout without spawning.
    with_fake_avifenc("ok", || {
        let recorder = TestRecorder::new();
        let request = ps_image::avif::AvifRequest {
            input_suffix: ".jpg",
            quality: 85,
            timeout_ms: 0,
            max_concurrent: 0,
        };
        assert!(ps_image::avif::convert_to_avif(
            &read_fixture("jpeg/sjpeg2.jpg"),
            &request,
            &recorder
        )
        .is_err());
        let events = recorder.avif_events.lock().unwrap();
        assert_eq!((events[0].0, events[0].1), (false, true));
    });
}

#[test]
fn avif_shed_under_cap() {
    // A slow conversion holds the single slot; a second conversion sheds.
    with_fake_avifenc("sleep", || {
        let request = ps_image::avif::AvifRequest {
            input_suffix: ".jpg",
            quality: 85,
            timeout_ms: 5000,
            max_concurrent: 1,
        };
        let input = read_fixture("jpeg/sjpeg2.jpg");
        let holder = std::thread::spawn(move || {
            let recorder = TestRecorder::new();
            // Times out long after the shed below; outcome irrelevant.
            let _ = ps_image::avif::convert_to_avif(&input, &request, &recorder);
        });
        // Wait until the holder plausibly holds the slot.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let recorder = TestRecorder::new();
        let request = ps_image::avif::AvifRequest {
            input_suffix: ".jpg",
            quality: 85,
            timeout_ms: 1000,
            max_concurrent: 1,
        };
        assert!(ps_image::avif::convert_to_avif(
            &read_fixture("jpeg/sjpeg2.jpg"),
            &request,
            &recorder
        )
        .is_err());
        assert_eq!(
            recorder
                .avif_sheds
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        holder.join().unwrap();
    });
}

#[test]
fn avif_quality_maps_to_cq_argv() {
    with_fake_avifenc("ok", || {
        let argv_path = std::env::temp_dir().join(format!(
            "ps_fake_argv_{}_{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("FAKE_AVIFENC_ARGV", &argv_path);
        let recorder = TestRecorder::new();
        let request = ps_image::avif::AvifRequest {
            input_suffix: ".png",
            quality: 85,
            timeout_ms: -1,
            max_concurrent: 0,
        };
        ps_image::avif::convert_to_avif(
            &read_fixture("pngsuite/basn2c08.png"),
            &request,
            &recorder,
        )
        .unwrap();
        let argv = fs::read_to_string(&argv_path).unwrap();
        // q85 -> CQ 10, passed as --min/--max with --jobs 1.
        assert!(argv.contains("--jobs 1 --min 10 --max 10"), "{argv}");
        assert!(argv.contains(".png"), "{argv}");
        let _ = fs::remove_file(&argv_path);
        std::env::remove_var("FAKE_AVIFENC_ARGV");
    });
}

#[test]
fn avif_transcode_end_to_end() {
    // The full transcode matrix reaches AVIF when a working encoder exists.
    with_fake_avifenc("ok", || {
        let recorder = TestRecorder::new();
        let options = CompressionOptions {
            convert_to_avif: true,
            ..Default::default()
        };
        let mut budget = ConversionBudget::new();
        let out = transcode(
            &read_fixture("jpeg/sjpeg2.jpg"),
            None,
            default_ctx(),
            &options,
            &mut budget,
            false,
            &recorder,
        )
        .unwrap();
        assert_eq!(out.image_type, ImageType::Avif);
        assert_eq!(sniff(&out.bytes), ImageType::Avif);
    });
}
