//! Whole-image analysis and the `ImageOptimizer` driver.
//!
//! Ports `AnalyzeImage` (dimensions, progressive, animated, transparency,
//! photo, and JPEG-quality detection with the JPEG-photo shortcut) and the
//! `ImageOptimizer::Optimize` pipeline: desired-format selection, integer
//! resize-dimension math, quality/progressive selection, and single-frame
//! (resize + strip + rewrite) and animated (GIF → animated WebP) execution,
//! all behind the `must_reduce_bytes` gate.

use std::time::Instant;

use crate::error::{Error, ErrorSource, Result};
use crate::format::{sniff, ImageFormat, ImageType};
use crate::frame::Frame;

/// Whole-image characteristics, port of the `AnalyzeImage` out-params.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageAnalysis {
    pub width: u32,
    pub height: u32,
    /// Progressive/interlaced encoding (always false for animations).
    pub progressive: bool,
    pub animated: bool,
    pub transparency: bool,
    pub photo: bool,
    /// Estimated JPEG quality; only populated for JPEG input.
    pub quality: Option<u8>,
}

/// Analyzes image bytes: type-specific decode plus the shared photo and
/// transparency rules (`AnalyzeImage`).
///
/// JPEG input always reports `photo` without computing the metric; animated
/// GIFs report dimensions only (progressive/transparency/photo stay false);
/// WebP input analyzes as single-frame stills.
pub fn analyze(bytes: &[u8]) -> Result<ImageAnalysis> {
    match sniff(bytes) {
        ImageType::Png => {
            let frame = crate::png::decode(bytes)?;
            let info = crate::png::scan_info(bytes)?;
            let stripped = frame.strip_opaque_alpha();
            Ok(ImageAnalysis {
                width: frame.width(),
                height: frame.height(),
                progressive: info.interlaced,
                animated: false,
                transparency: frame.has_transparency(),
                photo: crate::analysis::is_photo(&stripped),
                quality: None,
            })
        }
        ImageType::Jpeg => {
            let frame = crate::jpeg::decode(bytes)?;
            let info = crate::jpeg::scan_info(bytes)?;
            Ok(ImageAnalysis {
                width: frame.width(),
                height: frame.height(),
                progressive: info.progressive,
                animated: false,
                // JPEG has no alpha channel; all JPEGs count as photos to
                // save the gradient computation.
                transparency: false,
                photo: true,
                quality: crate::jpeg::estimate_quality(bytes),
            })
        }
        ImageType::Gif => {
            let header = crate::gif::probe(bytes)?;
            if crate::gif::is_animated(bytes)? {
                // Multi-frame GIFs only yield dimensions here.
                return Ok(ImageAnalysis {
                    width: header.width,
                    height: header.height,
                    progressive: false,
                    animated: true,
                    transparency: false,
                    photo: false,
                    quality: None,
                });
            }
            let frame = crate::gif::first_frame(bytes)?;
            let stripped = frame.strip_opaque_alpha();
            Ok(ImageAnalysis {
                width: frame.width(),
                height: frame.height(),
                progressive: false,
                animated: false,
                transparency: frame.has_transparency(),
                photo: crate::analysis::is_photo(&stripped),
                quality: None,
            })
        }
        ImageType::Webp | ImageType::WebpLosslessOrAlpha => {
            let frame = crate::webp::decode(bytes)?;
            let stripped = frame.strip_opaque_alpha();
            Ok(ImageAnalysis {
                width: frame.width(),
                height: frame.height(),
                progressive: false,
                animated: false,
                transparency: frame.has_transparency(),
                photo: crate::analysis::is_photo(&stripped),
                quality: None,
            })
        }
        ImageType::WebpAnimated | ImageType::Avif | ImageType::Unknown => Err(
            Error::unsupported_format("image analysis needs PNG, JPEG, GIF, or still WebP"),
        ),
    }
}

/// Optimizer allowances and ceilings (`ImageOptions` proto, with its defaults).
#[derive(Debug, Clone)]
pub struct ImageOptions {
    pub allow_png: bool,
    pub allow_jpeg: bool,
    pub allow_webp_lossy: bool,
    pub allow_webp_lossless_or_alpha: bool,
    pub allow_webp_animated: bool,
    pub allow_convert_lossless_to_lossy: bool,
    pub must_reduce_bytes: bool,
    pub max_jpeg_quality: i32,
    pub max_webp_quality: i32,
    pub max_webp_animated_quality: i32,
    pub max_timeout_ms: i64,
    pub try_best_compression_for_png: bool,
}

impl Default for ImageOptions {
    fn default() -> Self {
        ImageOptions {
            allow_png: true,
            allow_jpeg: true,
            allow_webp_lossy: true,
            allow_webp_lossless_or_alpha: false,
            allow_webp_animated: false,
            allow_convert_lossless_to_lossy: false,
            must_reduce_bytes: true,
            max_jpeg_quality: 85,
            max_webp_quality: 85,
            max_webp_animated_quality: 85,
            max_timeout_ms: -1,
            try_best_compression_for_png: true,
        }
    }
}

/// Requested resize dimensions: each side optional (aspect preserved).
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestedDimensions {
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// Successful optimization output.
#[derive(Debug, Clone)]
pub struct OptimizedImage {
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
    /// False when the output is losslessly encoded.
    pub uses_lossy: bool,
    /// True when a WebP deadline fired (output still returned on success).
    pub timed_out: bool,
}

/// Minimum input bytes for progressive JPEG consideration (10 KiB, matching
/// both the optimizer-local constant and the rewrite default).
pub const MIN_PROGRESSIVE_JPEG_BYTES: i64 = 10240;

/// Applies all optimizations to one image (`ImageOptimizer::Optimize`).
///
/// WebP and AVIF inputs are refused (like the C++ `Run` gate); on success
/// the output format, dimensions, and bytes are returned. `webp_deadline`
/// bounds still and animated WebP encoding; other formats ignore it.
pub fn optimize(
    bytes: &[u8],
    options: &ImageOptions,
    requested: RequestedDimensions,
    webp_deadline: Option<Instant>,
) -> Result<OptimizedImage> {
    let input_type = sniff(bytes);
    let original_format = input_type.format();
    match original_format {
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif => {}
        _ => {
            return Err(Error::unsupported_format(
                "optimizer handles PNG, JPEG, and GIF input",
            ));
        }
    }
    let analysis = analyze(bytes)?;
    if analysis.width < 1 || analysis.height < 1 {
        return Err(Error::invocation(
            ErrorSource::Util,
            "cannot optimize an empty image",
        ));
    }

    let (optimized_format, desired_lossless) = desired_format(
        options,
        original_format,
        analysis.animated,
        analysis.transparency,
        analysis.photo,
    )
    .ok_or_else(|| {
        Error::unsupported_feature(ErrorSource::Util, "no allowed output format applies")
    })?;

    let (optimized_width, optimized_height) = resized_dimensions(&analysis, requested)
        .ok_or_else(|| Error::invocation(ErrorSource::Util, "invalid requested dimensions"))?;

    let (desired_quality, desired_progressive) = desired_quality_progressive(
        options,
        optimized_format,
        analysis.animated,
        analysis.quality,
        bytes.len(),
        optimized_width,
        optimized_height,
    )
    .ok_or_else(|| Error::internal(ErrorSource::Util, "unusable desired quality"))?;

    let (output, timed_out) = if analysis.animated {
        let output = rewrite_animated(bytes, webp_deadline)?;
        (output, false)
    } else {
        let job = SingleFrameJob {
            bytes,
            original_format,
            transparent: analysis.transparency,
            optimized_format,
            desired_lossless,
            optimized_width,
            optimized_height,
            desired_quality,
            desired_progressive,
            try_best_png: options.try_best_compression_for_png,
            webp_deadline,
        };
        rewrite_single_frame(&job)?
    };

    if options.must_reduce_bytes && output.len() > bytes.len() {
        return Err(Error::unsupported_feature(
            ErrorSource::Util,
            "optimized image is larger than the original",
        ));
    }

    Ok(OptimizedImage {
        format: optimized_format,
        width: optimized_width,
        height: optimized_height,
        bytes: output,
        uses_lossy: !desired_lossless,
        timed_out,
    })
}

/// Output-format selection (`ComputeDesiredFormat`).
fn desired_format(
    options: &ImageOptions,
    original: ImageFormat,
    animated: bool,
    transparent: bool,
    photo: bool,
) -> Option<(ImageFormat, bool)> {
    if animated {
        if options.allow_webp_animated {
            return Some((ImageFormat::Webp, true));
        }
    } else if transparent {
        if options.allow_webp_lossless_or_alpha {
            return Some((ImageFormat::Webp, true));
        } else if options.allow_png {
            return Some((ImageFormat::Png, true));
        }
    } else if photo && (original == ImageFormat::Jpeg || options.allow_convert_lossless_to_lossy) {
        if options.allow_webp_lossy {
            return Some((ImageFormat::Webp, false));
        } else if options.allow_jpeg {
            return Some((ImageFormat::Jpeg, false));
        }
    } else if options.allow_webp_lossless_or_alpha {
        return Some((ImageFormat::Webp, true));
    } else if options.allow_png {
        return Some((ImageFormat::Png, true));
    }
    None
}

/// Resize-dimension math (`ComputeResizedDimension`), in integer arithmetic
/// with the C++ round-half-up bias. Over-size requests and animations keep
/// the original dimensions without failing.
fn resized_dimensions(
    analysis: &ImageAnalysis,
    requested: RequestedDimensions,
) -> Option<(u32, u32)> {
    let (original_width, original_height) = (i64::from(analysis.width), i64::from(analysis.height));
    if let Some(width) = requested.width {
        if width < 1 {
            return None;
        }
    }
    if let Some(height) = requested.height {
        if height < 1 {
            return None;
        }
    }
    if analysis.animated
        || requested
            .width
            .is_some_and(|width| i64::from(width) > original_width)
        || requested
            .height
            .is_some_and(|height| i64::from(height) > original_height)
    {
        return Some((analysis.width, analysis.height));
    }
    match (requested.width, requested.height) {
        (None, None) => Some((analysis.width, analysis.height)),
        (None, Some(height)) => {
            let height = i64::from(height);
            let width = (height * original_width + original_height / 2) / original_height;
            Some((width as u32, height as u32))
        }
        (Some(width), None) => {
            let width = i64::from(width);
            let height = (width * original_height + original_width / 2) / original_width;
            Some((width as u32, height as u32))
        }
        (Some(width), Some(height)) => Some((width, height)),
    }
}

/// Quality and progressive selection (`ComputeDesiredQualityProgressive`).
fn desired_quality_progressive(
    options: &ImageOptions,
    optimized_format: ImageFormat,
    animated: bool,
    original_quality: Option<u8>,
    original_len: usize,
    optimized_width: u32,
    optimized_height: u32,
) -> Option<(i32, bool)> {
    let mut quality = original_quality.map_or(100, i32::from);
    let mut progressive = false;
    if optimized_format == ImageFormat::Jpeg {
        quality = quality.min(options.max_jpeg_quality);
        progressive = crate::optimize::should_convert_to_progressive(
            quality as i64,
            MIN_PROGRESSIVE_JPEG_BYTES,
            original_len,
            optimized_width,
            optimized_height,
        );
    } else if animated {
        quality = quality.min(options.max_webp_animated_quality);
    } else {
        quality = quality.min(options.max_webp_quality);
    }
    if (0..=100).contains(&quality) {
        Some((quality, progressive))
    } else {
        None
    }
}

/// Decodes the optimizer's accepted inputs to a frame.
fn decode_input(bytes: &[u8], format: ImageFormat) -> Result<Frame> {
    match format {
        ImageFormat::Png => crate::png::decode(bytes),
        ImageFormat::Jpeg => crate::jpeg::decode(bytes),
        ImageFormat::Gif => crate::gif::first_frame(bytes),
        _ => Err(Error::unsupported_format(
            "optimizer handles PNG, JPEG, and GIF input",
        )),
    }
}

/// Bundled inputs for a single-frame rewrite (mirrors the C++ member state).
struct SingleFrameJob<'a> {
    bytes: &'a [u8],
    original_format: ImageFormat,
    transparent: bool,
    optimized_format: ImageFormat,
    desired_lossless: bool,
    optimized_width: u32,
    optimized_height: u32,
    desired_quality: i32,
    desired_progressive: bool,
    try_best_png: bool,
    webp_deadline: Option<Instant>,
}

/// Single-frame rewrite: strip alpha, resize when smaller, encode.
fn rewrite_single_frame(job: &SingleFrameJob) -> Result<(Vec<u8>, bool)> {
    let frame = decode_input(job.bytes, job.original_format)?;
    let frame = frame.strip_opaque_alpha();
    let frame = if job.optimized_width < frame.width() || job.optimized_height < frame.height() {
        crate::resizer::resize(&frame, job.optimized_width, job.optimized_height)?
    } else {
        frame
    };
    match job.optimized_format {
        ImageFormat::Png => {
            let gamma = if job.original_format == ImageFormat::Png {
                crate::optimize::scan_gama(job.bytes)
            } else {
                None
            };
            Ok((
                crate::optimize::optimize_png_frame(&frame, gamma, job.try_best_png)?,
                false,
            ))
        }
        ImageFormat::Jpeg => {
            let options = crate::jpeg::JpegEncodeOptions {
                quality: job.desired_quality.clamp(0, 100) as u8,
                progressive: job.desired_progressive,
                num_scans: -1,
                sampling: crate::jpeg::ColorSampling::Yuv420,
                app_segments: Vec::new(),
                optimized_huffman: true,
            };
            Ok((crate::jpeg::encode(&frame, &options)?, false))
        }
        ImageFormat::Webp => {
            let config = crate::webp::WebpEncodeConfig {
                lossless: job.desired_lossless,
                // Lossless quality is pinned for consistent output.
                quality: if job.desired_lossless {
                    100.0
                } else {
                    job.desired_quality as f32
                },
                method: 3,
                alpha_quality: if job.transparent { 100 } else { 0 },
                alpha_compression: if job.transparent { 1 } else { 0 },
                ..Default::default()
            };
            Ok(crate::webp::encode_with_deadline(
                &frame,
                &config,
                job.webp_deadline,
            )?)
        }
        _ => Err(Error::internal(
            ErrorSource::Util,
            "optimizer cannot write this format",
        )),
    }
}

/// Animated rewrite: GIF frames to animated WebP (no resizing, like the C++).
fn rewrite_animated(bytes: &[u8], webp_deadline: Option<Instant>) -> Result<Vec<u8>> {
    let mut iterator = crate::gif::Frames::open(bytes)?;
    let loop_plays = iterator.header().loop_plays;
    let mut frames = Vec::new();
    while let Some(frame) = iterator.next_frame()? {
        frames.push((frame.canvas, frame.delay_ms));
    }
    if frames.is_empty() {
        return Err(Error::parse(ErrorSource::GifReader, "GIF has no frames"));
    }
    // The optimizer always writes lossless animation with pinned quality.
    let config = crate::webp::WebpAnimConfig {
        lossless: true,
        quality: 100.0,
        kmin: 3,
        kmax: 5,
        loop_count: loop_plays,
    };
    crate::webp::encode_animated(&frames, &config, webp_deadline).map(|(bytes, _)| bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_defaults_match_proto() {
        let options = ImageOptions::default();
        assert!(options.allow_png && options.allow_jpeg && options.allow_webp_lossy);
        assert!(!options.allow_webp_lossless_or_alpha && !options.allow_webp_animated);
        assert!(!options.allow_convert_lossless_to_lossy);
        assert!(options.must_reduce_bytes);
        assert_eq!(options.max_jpeg_quality, 85);
        assert_eq!(options.max_webp_quality, 85);
        assert_eq!(options.max_webp_animated_quality, 85);
        assert_eq!(options.max_timeout_ms, -1);
        assert!(options.try_best_compression_for_png);
    }

    #[test]
    fn format_selection_matrix() {
        let options = ImageOptions::default();
        // Photo JPEG with defaults: lossy WebP.
        assert_eq!(
            desired_format(&options, ImageFormat::Jpeg, false, false, true),
            Some((ImageFormat::Webp, false))
        );
        // Transparent PNG with defaults: PNG (lossless WebP not allowed).
        assert_eq!(
            desired_format(&options, ImageFormat::Png, false, true, false),
            Some((ImageFormat::Png, true))
        );
        // Animated GIF with defaults: refused (animated WebP not allowed).
        assert_eq!(
            desired_format(&options, ImageFormat::Gif, true, false, false),
            None
        );
        // Opaque graphic PNG: PNG (no lossy conversion allowed).
        assert_eq!(
            desired_format(&options, ImageFormat::Png, false, false, false),
            Some((ImageFormat::Png, true))
        );
    }
}

/// Preferred WebP level negotiated from the client (`PreferredLibwebpLevel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PreferredWebp {
    #[default]
    None,
    Lossy,
    Lossless,
    Animated,
}

/// Rewrite-level conversion settings (`Image::CompressionOptions`).
///
/// Statistics sinks are not fields here: callers pass a
/// [`crate::stats::Recorder`] to [`transcode`] instead of the raw
/// `ConversionVariables`/`AvifConversionVariables` pointers.
#[derive(Debug, Clone)]
pub struct CompressionOptions {
    pub preferred_webp: PreferredWebp,
    pub allow_webp_alpha: bool,
    pub allow_webp_animated: bool,
    pub webp_quality: i64,
    pub webp_animated_quality: i64,
    pub jpeg_quality: i64,
    pub progressive_jpeg_min_bytes: i64,
    pub progressive_jpeg: bool,
    pub convert_gif_to_png: bool,
    pub convert_png_to_jpeg: bool,
    pub convert_jpeg_to_webp: bool,
    pub convert_to_avif: bool,
    pub recompress_jpeg: bool,
    pub recompress_png: bool,
    pub recompress_webp: bool,
    pub retain_color_profile: bool,
    pub retain_color_sampling: bool,
    pub retain_exif_data: bool,
    pub use_transparent_for_blank_image: bool,
    pub jpeg_num_progressive_scans: i64,
    pub webp_conversion_timeout_ms: i64,
    pub avif_conversion_timeout_ms: i64,
    pub avif_max_concurrent_conversions: i32,
    pub preserve_lossless: bool,
}

impl Default for CompressionOptions {
    fn default() -> Self {
        CompressionOptions {
            preferred_webp: PreferredWebp::None,
            allow_webp_alpha: false,
            allow_webp_animated: false,
            webp_quality: 85,
            webp_animated_quality: 85,
            jpeg_quality: 85,
            progressive_jpeg_min_bytes: MIN_PROGRESSIVE_JPEG_BYTES,
            progressive_jpeg: false,
            convert_gif_to_png: false,
            convert_png_to_jpeg: false,
            convert_jpeg_to_webp: false,
            convert_to_avif: false,
            recompress_jpeg: false,
            recompress_png: false,
            recompress_webp: false,
            retain_color_profile: false,
            retain_color_sampling: false,
            retain_exif_data: false,
            use_transparent_for_blank_image: false,
            jpeg_num_progressive_scans: -1,
            webp_conversion_timeout_ms: -1,
            avif_conversion_timeout_ms: -1,
            avif_max_concurrent_conversions: 0,
            preserve_lossless: false,
        }
    }
}

impl CompressionOptions {
    /// Low-resolution transform (`SetTransformToLowRes`): forces quality 10
    /// across the board and downgrades lossless WebP to lossy.
    pub fn transform_to_low_res(&mut self) {
        if self.preferred_webp != PreferredWebp::None {
            self.preferred_webp = PreferredWebp::Lossy;
        }
        self.webp_quality = 10;
        self.webp_animated_quality = 10;
        self.jpeg_quality = 10;
    }
}

/// Conversion-attempt budget (`MayConvert`, capped at 2 per image).
#[derive(Debug, Default)]
pub struct ConversionBudget {
    attempted: u32,
}

impl ConversionBudget {
    #[must_use]
    pub fn new() -> Self {
        ConversionBudget::default()
    }

    fn may_convert(&mut self) -> bool {
        if self.attempted < 2 {
            self.attempted += 1;
            true
        } else {
            false
        }
    }
}

/// Context for a transcode call: whether the bytes were resized (and to
/// what), mirroring the `ImageImpl` resized state.
#[derive(Debug, Clone, Copy, Default)]
pub struct TranscodeContext {
    /// True when `bytes` are resized output rather than the original.
    pub resized: bool,
    /// Dimensions of the resized bytes, for the progressive gate.
    pub resized_dims: Option<(u32, u32)>,
    /// Original input type for dispatch (`ComputeOutputContents`
    /// switches on the original type, so a resized GIF still takes
    /// the GIF branch with `FROM_GIF` stats). `None` sniffs `bytes`.
    pub original_type: Option<crate::format::ImageType>,
}

/// Transcode result: winning type plus bytes.
#[derive(Debug, Clone)]
pub struct TranscodedImage {
    pub image_type: ImageType,
    pub bytes: Vec<u8>,
}

/// Quality ceiling shared by the JPEG paths (`kMaxJpegQuality`).
const MAX_JPEG_QUALITY: i64 = 100;
/// Fallback quality when no estimate exists
/// (`kQualityForJpegWithUnkownQuality`).
const QUALITY_FOR_UNKNOWN_JPEG: i64 = 85;

/// Estimates the quality for a resized JPEG (`EstimateQualityForResizedJpeg`):
/// the input estimate capped by the configuration, falling back through
/// configuration to the 85 default.
fn estimate_quality_for_resized_jpeg(original: &[u8], jpeg_quality: i64) -> i64 {
    let input_quality = crate::jpeg::estimate_quality(original).map_or(-1, i64::from);
    let output_quality = MAX_JPEG_QUALITY.min(jpeg_quality);
    if input_quality > 0 && output_quality > 0 {
        input_quality.min(output_quality)
    } else if input_quality > 0 {
        input_quality
    } else if output_quality > 0 {
        output_quality
    } else {
        QUALITY_FOR_UNKNOWN_JPEG
    }
}

/// Builds lossy JPEG options from rewrite settings (`ConvertToJpegOptions`).
fn convert_to_jpeg_options(
    original: &[u8],
    input_type: ImageType,
    natural_dims: Option<(u32, u32)>,
    resized_dims: Option<(u32, u32)>,
    options: &CompressionOptions,
) -> crate::optimize::JpegCompressionOptions {
    let mut jpeg_options = crate::optimize::JpegCompressionOptions {
        retain_color_profile: options.retain_color_profile,
        retain_exif_data: options.retain_exif_data,
        ..Default::default()
    };
    let input_quality = crate::jpeg::estimate_quality(original).map_or(-1, i64::from);
    let output_quality = estimate_quality_for_resized_jpeg(original, options.jpeg_quality);
    if options.jpeg_quality > 0
        && (input_type != ImageType::Jpeg
            || options.jpeg_num_progressive_scans > 0
            || input_quality > output_quality)
    {
        jpeg_options.lossy = true;
        jpeg_options.lossy_options.quality = output_quality as i32;
        if options.progressive_jpeg {
            jpeg_options.lossy_options.num_scans = options.jpeg_num_progressive_scans as i32;
        }
        if options.retain_color_sampling {
            jpeg_options.lossy_options.color_sampling = crate::optimize::ColorSampling::Retain;
        }
    }
    // Progressive gate over the expected (possibly resized) dimensions.
    let expected = resized_dims.or(natural_dims);
    jpeg_options.progressive = options.progressive_jpeg && {
        match expected {
            Some((width, height)) => crate::optimize::should_convert_to_progressive(
                output_quality,
                options.progressive_jpeg_min_bytes,
                original.len(),
                width,
                height,
            ),
            None => (original.len() as i64) >= options.progressive_jpeg_min_bytes,
        }
    };
    jpeg_options
}

/// Rewrites image bytes to the best allowed output (`ComputeOutputContents`).
///
/// The decision order ports the C++ exactly: AVIF first, then WebP, then
/// same-format recompression, returning on the first success without
/// comparing alternative conversions. `low_quality` applies the low-res
/// transform to a copy of the options.
#[allow(clippy::too_many_arguments)]
pub fn transcode(
    bytes: &[u8],
    natural_dims: Option<(u32, u32)>,
    ctx: TranscodeContext,
    options: &CompressionOptions,
    budget: &mut ConversionBudget,
    low_quality: bool,
    recorder: &dyn crate::stats::Recorder,
) -> Result<TranscodedImage> {
    let mut options = options.clone();
    if low_quality {
        options.transform_to_low_res();
    }
    let input_type = ctx.original_type.unwrap_or_else(|| sniff(bytes));
    match input_type {
        ImageType::Unknown | ImageType::Avif | ImageType::WebpAnimated => Err(
            Error::unsupported_feature(ErrorSource::Util, "image type is not rewritable"),
        ),
        ImageType::Webp | ImageType::WebpLosslessOrAlpha => {
            if (ctx.resized || options.recompress_webp) && budget.may_convert() {
                let out = crate::optimize::reduce_webp_quality(bytes, options.webp_quality as i32)?;
                Ok(TranscodedImage {
                    image_type: input_type,
                    bytes: out,
                })
            } else {
                Err(Error::unsupported_feature(
                    ErrorSource::Util,
                    "webp recompression not enabled",
                ))
            }
        }
        ImageType::Jpeg => transcode_jpeg(bytes, natural_dims, ctx, &options, budget, recorder),
        ImageType::Png => {
            transcode_gif_or_png(bytes, natural_dims, ctx, &options, budget, recorder, false)
        }
        ImageType::Gif => {
            transcode_gif_or_png(bytes, natural_dims, ctx, &options, budget, recorder, true)
        }
    }
}

fn transcode_jpeg(
    bytes: &[u8],
    natural_dims: Option<(u32, u32)>,
    ctx: TranscodeContext,
    options: &CompressionOptions,
    budget: &mut ConversionBudget,
    recorder: &dyn crate::stats::Recorder,
) -> Result<TranscodedImage> {
    // NOTE: options are checked BEFORE consuming budget. The C++
    // in this snapshot calls MayConvert() first, which burns both
    // attempts on disabled conversions and dead-ends recompress-only
    // configs — but its own tests (JpegTest, ImgTag) require
    // recompress to work, so the tests define the behavior here.
    if options.convert_to_avif && options.webp_quality > 0 && budget.may_convert() {
        let request = crate::avif::AvifRequest {
            input_suffix: ".jpg",
            quality: options.webp_quality,
            timeout_ms: options.avif_conversion_timeout_ms,
            max_concurrent: options.avif_max_concurrent_conversions,
        };
        match crate::avif::convert_to_avif(bytes, &request, recorder) {
            Ok(out) => {
                return Ok(TranscodedImage {
                    image_type: ImageType::Avif,
                    bytes: out,
                });
            }
            Err(_) => {
                // Fall through to WebP/JPEG like the C++.
            }
        }
    }
    if options.convert_jpeg_to_webp
        && options.preferred_webp != PreferredWebp::None
        && budget.may_convert()
    {
        if webp_pre_timed_out(options) {
            recorder.webp_conversion(crate::stats::WebpVar::FromJpeg, false, true, 0);
            recorder.webp_conversion(crate::stats::WebpVar::Opaque, false, true, 0);
        } else {
            let started = Instant::now();
            let out =
                crate::optimize::optimize_webp_from_jpeg(bytes, options.webp_quality as i32);
            let elapsed_ms = started.elapsed().as_millis() as i64;
            match out {
                Ok(out) => {
                    // The C++ records timeouts via the hook; without a deadline
                    // the conversion cannot time out.
                    recorder.webp_conversion(
                        crate::stats::WebpVar::FromJpeg,
                        true,
                        false,
                        elapsed_ms,
                    );
                    recorder.webp_conversion(
                        crate::stats::WebpVar::Opaque,
                        true,
                        false,
                        elapsed_ms,
                    );
                    return Ok(TranscodedImage {
                        image_type: ImageType::Webp,
                        bytes: out,
                    });
                }
                Err(_) => {
                    recorder.webp_conversion(
                        crate::stats::WebpVar::FromJpeg,
                        false,
                        false,
                        elapsed_ms,
                    );
                    recorder.webp_conversion(
                        crate::stats::WebpVar::Opaque,
                        false,
                        false,
                        elapsed_ms,
                    );
                }
            }
        }
    }
    if (ctx.resized || options.recompress_jpeg) && budget.may_convert() {
        let jpeg_options = convert_to_jpeg_options(
            bytes,
            ImageType::Jpeg,
            natural_dims,
            ctx.resized_dims,
            options,
        );
        let out = crate::optimize::optimize_jpeg_with_options(bytes, &jpeg_options)?;
        return Ok(TranscodedImage {
            image_type: ImageType::Jpeg,
            bytes: out,
        });
    }
    Err(Error::unsupported_feature(
        ErrorSource::Util,
        "no JPEG rewrite applies",
    ))
}

/// Shared GIF/PNG output computation (`ComputeOutputContentsFromGifOrPng`).
#[allow(clippy::too_many_arguments)]
fn transcode_gif_or_png(
    bytes: &[u8],
    natural_dims: Option<(u32, u32)>,
    ctx: TranscodeContext,
    options: &CompressionOptions,
    budget: &mut ConversionBudget,
    recorder: &dyn crate::stats::Recorder,
    is_gif: bool,
) -> Result<TranscodedImage> {
    let (input_type, fall_back_to_png, var) = if is_gif {
        if ctx.resized {
            // Resized GIF bytes are already PNG (the resize path converts).
            (ImageType::Png, false, crate::stats::WebpVar::FromGif)
        } else if options.convert_gif_to_png
            || options.preferred_webp == PreferredWebp::Animated
            || options.allow_webp_animated
        {
            (
                ImageType::Gif,
                options.convert_gif_to_png,
                crate::stats::WebpVar::FromGif,
            )
        } else {
            return Err(Error::unsupported_feature(
                ErrorSource::Util,
                "GIF conversion not enabled",
            ));
        }
    } else {
        (
            ImageType::Png,
            ctx.resized || options.recompress_png,
            crate::stats::WebpVar::FromPng,
        )
    };

    let analysis = analyze(bytes).unwrap_or(ImageAnalysis {
        width: 0,
        height: 0,
        progressive: false,
        animated: false,
        transparency: false,
        photo: false,
        quality: None,
    });
    if analysis.width == 0 || analysis.height == 0 {
        return Err(Error::parse(
            ErrorSource::Util,
            "cannot optimize an empty image",
        ));
    }
    let (is_animated, has_transparency, is_photo) =
        (analysis.animated, analysis.transparency, analysis.photo);

    // Target selection, in C++ order.
    enum Target {
        WebpAnimated,
        Avif,
        WebpLossy,
        WebpLosslessOrAlpha,
        Jpeg,
        PngFallback,
        None,
    }
    let mut target = Target::None;
    let mut compress_color_losslessly = false;
    if is_animated {
        if options.preferred_webp == PreferredWebp::Animated && options.webp_animated_quality > 0 {
            target = Target::WebpAnimated;
        }
    } else if is_photo
        && options.convert_png_to_jpeg
        && (input_type == ImageType::Png
            || (input_type == ImageType::Gif && options.convert_gif_to_png))
    {
        if !has_transparency {
            if options.convert_to_avif && options.webp_quality > 0 {
                target = Target::Avif;
            } else if options.preferred_webp != PreferredWebp::None
                && options.convert_jpeg_to_webp
                && options.webp_quality > 0
            {
                target = Target::WebpLossy;
            } else if options.jpeg_quality > 0 {
                target = Target::Jpeg;
            }
        } else if options.convert_to_avif && options.webp_quality > 0 {
            target = Target::Avif;
        } else if options.allow_webp_alpha
            && options.convert_jpeg_to_webp
            && options.webp_quality > 0
        {
            target = Target::WebpLosslessOrAlpha;
        }
    } else if options.convert_to_avif && options.webp_quality > 0 && input_type != ImageType::Gif {
        target = Target::Avif;
    } else if options.preferred_webp == PreferredWebp::Animated
        || options.preferred_webp == PreferredWebp::Lossless
    {
        compress_color_losslessly = true;
        target = Target::WebpLosslessOrAlpha;
    }

    // Animated input only converts to animated webp; every other
    // target (including the PNG fallback) refuses, like the C++
    // selector that leaves output_type unknown.
    if is_animated && !matches!(target, Target::WebpAnimated) {
        return Err(Error::unsupported_feature(
            ErrorSource::Util,
            "animated input without animated webp target",
        ));
    }
    if matches!(target, Target::WebpAnimated) {
        if !budget.may_convert() {
            return Err(Error::unsupported_feature(
                ErrorSource::Util,
                "conversion budget exhausted",
            ));
        }
        let started = Instant::now();
        let out = convert_animated_gif_to_webp(bytes, options);
        let elapsed_ms = started.elapsed().as_millis() as i64;
        return match out {
            Ok(out) => {
                recorder.webp_conversion(
                    crate::stats::WebpVar::FromGifAnimated,
                    true,
                    false,
                    elapsed_ms,
                );
                record_opacity(recorder, has_transparency, true, false, elapsed_ms);
                Ok(TranscodedImage {
                    image_type: ImageType::WebpAnimated,
                    bytes: out,
                })
            }
            Err(err) => {
                recorder.webp_conversion(
                    crate::stats::WebpVar::FromGifAnimated,
                    false,
                    false,
                    elapsed_ms,
                );
                record_opacity(recorder, has_transparency, false, false, elapsed_ms);
                Err(err)
            }
        };
    }

    // AVIF attempt with JPEG/PNG fallback, in C++ order.
    let mut fall_back_to_png = fall_back_to_png;
    if matches!(target, Target::Avif) {
        if budget.may_convert() {
            let request = crate::avif::AvifRequest {
                input_suffix: if input_type == ImageType::Png {
                    ".png"
                } else {
                    ".jpg"
                },
                quality: options.webp_quality,
                timeout_ms: options.avif_conversion_timeout_ms,
                max_concurrent: options.avif_max_concurrent_conversions,
            };
            match crate::avif::convert_to_avif(bytes, &request, recorder) {
                Ok(out) => {
                    return Ok(TranscodedImage {
                        image_type: ImageType::Avif,
                        bytes: out,
                    });
                }
                Err(_) => {
                    if !has_transparency && options.jpeg_quality > 0 {
                        target = Target::Jpeg;
                    } else {
                        target = Target::PngFallback;
                        fall_back_to_png = true;
                    }
                }
            }
        } else {
            target = Target::PngFallback;
            fall_back_to_png = true;
        }
    }

    if matches!(target, Target::WebpLossy | Target::WebpLosslessOrAlpha) {
        if budget.may_convert() {
            if webp_pre_timed_out(options) {
                recorder.webp_conversion(var, false, true, 0);
                record_opacity(recorder, has_transparency, false, true, 0);
                if matches!(target, Target::WebpLossy) {
                    target = Target::Jpeg;
                } else {
                    target = Target::PngFallback;
                    fall_back_to_png = true;
                }
            } else {
                let started = Instant::now();
                let out = convert_png_to_webp_with_options(
                    bytes,
                    compress_color_losslessly,
                    has_transparency,
                    options,
                );
                let elapsed_ms = started.elapsed().as_millis() as i64;
                match out {
                    Ok((out, output_type)) => {
                        recorder.webp_conversion(var, true, false, elapsed_ms);
                        record_opacity(recorder, has_transparency, true, false, elapsed_ms);
                        return Ok(TranscodedImage {
                            image_type: output_type,
                            bytes: out,
                        });
                    }
                    Err(_) => {
                        recorder.webp_conversion(var, false, false, elapsed_ms);
                        record_opacity(recorder, has_transparency, false, false, elapsed_ms);
                        if matches!(target, Target::WebpLossy) {
                            target = Target::Jpeg;
                        } else {
                            target = Target::PngFallback;
                            fall_back_to_png = true;
                        }
                    }
                }
            }
        } else {
            target = Target::PngFallback;
            fall_back_to_png = true;
        }
    }

    if matches!(target, Target::Jpeg) && budget.may_convert() {
        // Non-JPEG input always enables lossy here (jpeg_quality > 0 is
        // part of the Jpeg target gate), matching ConvertToJpegOptions.
        let jpeg_options =
            convert_to_jpeg_options(bytes, input_type, natural_dims, ctx.resized_dims, options);
        let out = crate::converter::convert_png_to_jpeg(bytes, &jpeg_options)?;
        return Ok(TranscodedImage {
            image_type: ImageType::Jpeg,
            bytes: out,
        });
    }

    if matches!(target, Target::PngFallback | Target::None)
        && fall_back_to_png
        && budget.may_convert()
    {
        let out = crate::optimize::optimize_png_best(bytes)?;
        return Ok(TranscodedImage {
            image_type: ImageType::Png,
            bytes: out,
        });
    }

    Err(Error::unsupported_feature(
        ErrorSource::Util,
        "no GIF/PNG rewrite applies",
    ))
}

/// Whether a webp conversion is pre-timed-out: `timeout_ms == 0`
/// means already expired (the C++ watchdog hook fires immediately),
/// negative disables the deadline.
fn webp_pre_timed_out(options: &CompressionOptions) -> bool {
    options.webp_conversion_timeout_ms == 0
}

fn record_opacity(
    recorder: &dyn crate::stats::Recorder,
    has_transparency: bool,
    ok: bool,
    timed_out: bool,
    elapsed_ms: i64,
) {
    recorder.webp_conversion(
        if has_transparency {
            crate::stats::WebpVar::NonOpaque
        } else {
            crate::stats::WebpVar::Opaque
        },
        ok,
        timed_out,
        elapsed_ms,
    );
}

/// PNG/GIF to WebP with rewrite-driven settings (`ImageImpl::ConvertPngToWebp`).
fn convert_png_to_webp_with_options(
    bytes: &[u8],
    compress_color_losslessly: bool,
    has_transparency: bool,
    options: &CompressionOptions,
) -> Result<(Vec<u8>, ImageType)> {
    let mut config = crate::webp::WebpEncodeConfig {
        method: 3,
        quality: options.webp_quality as f32,
        ..Default::default()
    };
    let output_type;
    if compress_color_losslessly {
        config.lossless = true;
        output_type = ImageType::WebpLosslessOrAlpha;
    } else {
        config.lossless = false;
        if has_transparency {
            config.alpha_quality = 100;
            config.alpha_compression = 1;
            output_type = ImageType::WebpLosslessOrAlpha;
        } else {
            config.alpha_quality = 0;
            config.alpha_compression = 0;
            output_type = ImageType::Webp;
        }
    }
    // The C++ wires the conversion timeout hook here; without a deadline the
    // encode runs to completion (deadline plumbing arrives with async).
    let (out, _) = crate::converter::convert_png_to_webp(bytes, &config)?;
    Ok((out, output_type))
}

/// Animated GIF to animated WebP (`ConvertAnimatedGifToWebp`).
fn convert_animated_gif_to_webp(bytes: &[u8], options: &CompressionOptions) -> Result<Vec<u8>> {
    let mut iterator = crate::gif::Frames::open(bytes)?;
    let loop_plays = iterator.header().loop_plays;
    let mut frames = Vec::new();
    while let Some(frame) = iterator.next_frame()? {
        frames.push((frame.canvas, frame.delay_ms));
    }
    if frames.is_empty() {
        return Err(Error::parse(ErrorSource::GifReader, "GIF has no frames"));
    }
    let config = crate::webp::WebpAnimConfig {
        lossless: false,
        quality: options.webp_animated_quality as f32,
        kmin: 3,
        kmax: 5,
        loop_count: loop_plays,
    };
    crate::webp::encode_animated(&frames, &config, None).map(|(bytes, _)| bytes)
}

/// Resizes image bytes for the rewrite path (`ImageImpl::ResizeTo`).
///
/// WebP input is refused; GIF input converts through its first frame (the
/// caller treats resized GIF bytes as PNG). JPEG output uses the resized
/// quality estimate; PNG output uses the fast configuration.
pub fn resize_image(
    bytes: &[u8],
    new_width: u32,
    new_height: u32,
    options: &CompressionOptions,
) -> Result<(Vec<u8>, ImageFormat)> {
    if new_width == 0 || new_height == 0 {
        return Err(Error::invocation(
            ErrorSource::Resizer,
            "resize dimensions must be positive",
        ));
    }
    let format = sniff(bytes).format();
    if format == ImageFormat::Webp {
        return Err(Error::unsupported_feature(
            ErrorSource::Resizer,
            "WebP resizing is not supported",
        ));
    }
    let frame = decode_input(bytes, format)?;
    let frame = crate::resizer::resize(&frame, new_width, new_height)?;
    match format {
        ImageFormat::Jpeg => {
            let quality = estimate_quality_for_resized_jpeg(bytes, options.jpeg_quality);
            let encode_options = crate::jpeg::JpegEncodeOptions {
                quality: quality.clamp(1, 100) as u8,
                progressive: false,
                num_scans: -1,
                sampling: crate::jpeg::ColorSampling::Yuv420,
                app_segments: Vec::new(),
                optimized_huffman: true,
            };
            Ok((
                crate::jpeg::encode(&frame, &encode_options)?,
                ImageFormat::Jpeg,
            ))
        }
        _ => {
            let gamma = if format == ImageFormat::Png {
                crate::optimize::scan_gama(bytes)
            } else {
                None
            };
            // Fast configuration (filter none, default strategy/level).
            Ok((
                crate::optimize::optimize_png_frame(&frame, gamma, false)?,
                ImageFormat::Png,
            ))
        }
    }
}

#[cfg(test)]
mod transcode_tests {
    use super::*;

    #[test]
    fn resize_dimension_math() {
        let analysis = ImageAnalysis {
            width: 33,
            height: 34,
            progressive: false,
            animated: false,
            transparency: false,
            photo: false,
            quality: None,
        };
        // (16,0): width = 16, height = (16*34 + 33/2)/33 = (544+16)/33 = 16.
        assert_eq!(
            resized_dimensions(
                &analysis,
                RequestedDimensions {
                    width: Some(16),
                    height: None
                }
            ),
            Some((16, 16))
        );
        // Oversize requests keep the original.
        assert_eq!(
            resized_dimensions(
                &analysis,
                RequestedDimensions {
                    width: Some(64),
                    height: None
                }
            ),
            Some((33, 34))
        );
        // Zero sides are invalid.
        assert_eq!(
            resized_dimensions(
                &analysis,
                RequestedDimensions {
                    width: Some(0),
                    height: None
                }
            ),
            None
        );
    }
}
