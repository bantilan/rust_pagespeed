//! Single-format optimizers with audited option structs.
//!
//! Ports `OptimizeJpeg`/`OptimizeJpegWithOptions` (+ `JpegCompressionOptions`),
//! `PngOptimizer::OptimizePng`/`OptimizePngBestCompression`
//! (+ `PngCompressParams`, the 4-trial matrix, and the optipng reduction
//! subset), `OptimizeWebp`/`ReduceWebpImageQuality` from the rewriter, and
//! `ShouldConvertToProgressive` with its byte-ratio estimator.
//!
//! PNG reduction implements the `OPNG_REDUCE_ALL` subset that applies to
//! 8-bit frames: opaque-alpha strip, RGB-to-gray, RGB(A)-to-palette at 256
//! colors or fewer (with tRNS), and 8-to-4/2/1-bit packing for gray and
//! indexed output. The gAMA chunk passes through; all other ancillaries are
//! dropped, like `CopyPngStructs`.

use std::collections::HashMap;

use crate::error::{Error, ErrorSource, Result};
use crate::format::{sniff, ImageType};
use crate::frame::{Frame, PixelFormat};
use crate::png::{PngCompression, PngEncodeOptions, PngFilter};

pub use crate::jpeg::ColorSampling;

/// Lossy JPEG options (`JpegLossyOptions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JpegLossyOptions {
    /// Quality 1..=100 (85 is the web-images default).
    pub quality: i32,
    /// Progressive scans to emit; <= 0 keeps the default script.
    pub num_scans: i32,
    /// Chroma subsampling.
    pub color_sampling: ColorSampling,
}

impl Default for JpegLossyOptions {
    fn default() -> Self {
        JpegLossyOptions {
            quality: 85,
            num_scans: -1,
            color_sampling: ColorSampling::Yuv420,
        }
    }
}

/// JPEG compression options (`JpegCompressionOptions`).
#[derive(Debug, Clone, Default)]
pub struct JpegCompressionOptions {
    /// Emit progressive JPEG (YCbCr only; ignored for grayscale).
    pub progressive: bool,
    /// Copy APP2 (ICC profile) markers verbatim.
    pub retain_color_profile: bool,
    /// Copy APP1 (EXIF) markers verbatim.
    pub retain_exif_data: bool,
    /// Re-encode lossily; false selects the lossless path.
    pub lossy: bool,
    /// Lossy settings; only read when `lossy` is true.
    pub lossy_options: JpegLossyOptions,
}

/// Lossless JPEG optimization: pixel-identical output with metadata removed.
///
/// The C++ additionally re-optimizes Huffman tables at the coefficient
/// level; that half is queued as a follow-up (see `jpeg::strip_metadata`),
/// so inputs without metadata round-trip byte-identical here. Corrupt
/// entropy fails, like the C++ coefficient read.
pub fn optimize_jpeg_lossless(bytes: &[u8]) -> Result<Vec<u8>> {
    // A full decode validates the entropy data the marker walk cannot see.
    let _ = crate::jpeg::decode(bytes)?;
    crate::jpeg::strip_metadata(bytes)
}

/// JPEG optimization with explicit options (`OptimizeJpegWithOptions`).
///
/// The lossless path ignores `progressive` (baseline-to-progressive
/// conversion needs the deferred coefficient re-encode); everything else
/// maps one-for-one.
pub fn optimize_jpeg_with_options(
    bytes: &[u8],
    options: &JpegCompressionOptions,
) -> Result<Vec<u8>> {
    if !options.lossy {
        return optimize_jpeg_lossless(bytes);
    }
    let frame = crate::jpeg::decode(bytes)?;
    let app_segments = crate::jpeg::retained_app_segments(
        bytes,
        options.retain_exif_data,
        options.retain_color_profile,
    )?;
    let encode_options = crate::jpeg::JpegEncodeOptions {
        quality: options.lossy_options.quality.clamp(1, 100) as u8,
        progressive: options.progressive,
        num_scans: options.lossy_options.num_scans,
        sampling: options.lossy_options.color_sampling,
        app_segments,
        optimized_huffman: true,
    };
    if options.lossy_options.color_sampling == ColorSampling::Retain {
        crate::jpeg::encode_retaining_sampling(&frame, bytes, &encode_options)
    } else {
        crate::jpeg::encode(&frame, &encode_options)
    }
}

/// DEFLATE strategy knob (`PngCompressParams::compression_strategy`).
///
/// Accepted for option parity; the `png` crate exposes no strategy selector,
/// so trials vary the compression level instead (documented on
/// [`optimize_png_best`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PngStrategy {
    #[default]
    Default,
    Filtered,
    HuffmanOnly,
    Rle,
    Fixed,
}

/// PNG compression parameters (`PngCompressParams`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PngCompressParams {
    pub filter: PngFilter,
    pub strategy: PngStrategy,
    /// Run the multi-trial best-compression search.
    pub try_best_compression: bool,
    /// Emit interlaced output. Every C++ call site passes false, and the
    /// encoder refuses it; kept for option parity.
    pub progressive: bool,
}

impl PngCompressParams {
    #[must_use]
    pub fn new(
        filter: PngFilter,
        strategy: PngStrategy,
        try_best_compression: bool,
        progressive: bool,
    ) -> Self {
        PngCompressParams {
            filter,
            strategy,
            try_best_compression,
            progressive,
        }
    }
}

/// Fast PNG optimization: one encode pass (filter none, default level).
pub fn optimize_png(bytes: &[u8]) -> Result<Vec<u8>> {
    optimize_png_impl(bytes, false)
}

/// Best PNG optimization: the 4-trial smallest-wins search. The C++ varies
/// filter × zlib-strategy at best level; the available knobs here are
/// filter × deflate-level, so trials cover (adaptive, none) × (9, 6).
pub fn optimize_png_best(bytes: &[u8]) -> Result<Vec<u8>> {
    optimize_png_impl(bytes, true)
}

fn optimize_png_impl(bytes: &[u8], best: bool) -> Result<Vec<u8>> {
    let frame = match sniff(bytes) {
        ImageType::Png => crate::png::decode(bytes)?,
        // The C++ runs the same optimizer over GifReader output.
        ImageType::Gif => crate::gif::first_frame(bytes)?,
        _ => {
            return Err(Error::unsupported_format(
                "PNG optimizer needs PNG or GIF input",
            ));
        }
    };
    let gamma = if sniff(bytes) == ImageType::Png {
        scan_gama(bytes)
    } else {
        None
    };
    optimize_png_frame(&frame, gamma, best)
}

/// Frame-level PNG optimization for already-decoded (possibly resized)
/// pixels. `gamma` carries the original gAMA value through, if any.
pub fn optimize_png_frame(frame: &Frame, gamma: Option<u32>, best: bool) -> Result<Vec<u8>> {
    let reduced = reduce_frame(frame)?;
    let trials: &[(PngFilter, PngCompression)] = if best {
        &[
            (PngFilter::MinEntropy, PngCompression::Best),
            (PngFilter::Adaptive, PngCompression::Best),
            (PngFilter::None, PngCompression::Best),
            (PngFilter::MinEntropy, PngCompression::Default),
            (PngFilter::Adaptive, PngCompression::Default),
            (PngFilter::None, PngCompression::Default),
        ]
    } else {
        &[(PngFilter::None, PngCompression::Default)]
    };
    let mut winner: Option<Vec<u8>> = None;
    for (filter, compression) in trials {
        let options = PngEncodeOptions {
            filter: *filter,
            compression: *compression,
            interlaced: false,
        };
        if let Ok(out) = encode_reduced(&reduced, gamma, &options) {
            let take = winner
                .as_ref()
                .is_none_or(|best: &Vec<u8>| out.len() < best.len());
            if take {
                winner = Some(out);
            }
        }
    }
    winner.ok_or_else(|| Error::internal(ErrorSource::PngWriter, "all PNG encode trials failed"))
}

/// A frame reduced to its cheapest lossless PNG representation.
enum ReducedImage {
    Gray {
        width: u32,
        height: u32,
        depth: png::BitDepth,
        pixels: Vec<u8>,
    },
    Rgb {
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    },
    Rgba {
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    },
    Indexed {
        width: u32,
        height: u32,
        depth: png::BitDepth,
        palette: Vec<u8>,
        trns: Option<Vec<u8>>,
        pixels: Vec<u8>,
    },
}

/// Applies the optipng reduction subset: opaque-alpha strip, RGB-to-gray,
/// RGB(A)-to-palette at 256 colors or fewer, and sub-8-bit packing.
fn reduce_frame(frame: &Frame) -> Result<ReducedImage> {
    let opaque = frame.strip_opaque_alpha();
    let gray_candidate = match opaque.format() {
        PixelFormat::Gray8 => Some(opaque.pixels().to_vec()),
        PixelFormat::Rgb888 if is_gray(opaque.pixels()) => {
            Some(opaque.pixels().iter().step_by(3).copied().collect())
        }
        _ => None,
    };
    if let Some(gray) = gray_candidate {
        let (depth, packed) = pack_gray(&gray, opaque.width() as usize, opaque.height() as usize);
        return Ok(ReducedImage::Gray {
            width: opaque.width(),
            height: opaque.height(),
            depth,
            pixels: packed,
        });
    }
    if let Some(indexed) = try_index(&opaque) {
        return Ok(indexed);
    }
    match opaque.format() {
        PixelFormat::Rgb888 => Ok(ReducedImage::Rgb {
            width: opaque.width(),
            height: opaque.height(),
            pixels: opaque.pixels().to_vec(),
        }),
        PixelFormat::Rgba8888 => Ok(ReducedImage::Rgba {
            width: opaque.width(),
            height: opaque.height(),
            pixels: opaque.pixels().to_vec(),
        }),
        PixelFormat::Gray8 | PixelFormat::Unsupported => Err(Error::internal(
            ErrorSource::Util,
            "unreachable pixel format in PNG reduction",
        )),
    }
}

fn is_gray(rgb: &[u8]) -> bool {
    rgb.as_chunks::<3>()
        .0
        .iter()
        .all(|px| px[0] == px[1] && px[1] == px[2])
}

/// Builds an indexed image when 256 colors or fewer are used, with tRNS for
/// any transparency. Returns `None` otherwise.
fn try_index(frame: &Frame) -> Option<ReducedImage> {
    let (pixels, with_alpha) = match frame.format() {
        PixelFormat::Rgb888 => (frame.pixels(), false),
        PixelFormat::Rgba8888 => (frame.pixels(), true),
        PixelFormat::Gray8 | PixelFormat::Unsupported => return None,
    };
    let mut index_of = HashMap::new();
    let mut palette = Vec::new();
    let mut trns = Vec::new();
    let mut indices = Vec::with_capacity(frame.width() as usize * frame.height() as usize);
    // Walk pixels in RGB or RGBA stride.
    let stride_pixels: Vec<&[u8]> = if with_alpha {
        pixels.as_chunks::<4>().0.iter().map(|px| &px[..]).collect()
    } else {
        pixels.as_chunks::<3>().0.iter().map(|px| &px[..]).collect()
    };
    for px in stride_pixels {
        let key: (u8, u8, u8, u8) = if with_alpha {
            (px[0], px[1], px[2], px[3])
        } else {
            (px[0], px[1], px[2], 255)
        };
        let next = index_of.len();
        let index = *index_of.entry(key).or_insert(next);
        if index >= 256 {
            return None;
        }
        if palette.len() / 3 <= index {
            palette.extend_from_slice(&[key.0, key.1, key.2]);
            if with_alpha {
                trns.push(key.3);
            }
        }
        indices.push(index as u8);
    }
    // Sort the palette by luminance so indices vary smoothly and
    // filter-compress well (photos gain ~15% over scan order).
    let colors = palette.len() / 3;
    let mut order: Vec<usize> = (0..colors).collect();
    order.sort_by_key(|&i| {
        let (r, g, b) = (
            palette[3 * i] as u32,
            palette[3 * i + 1] as u32,
            palette[3 * i + 2] as u32,
        );
        (
            299 * r + 587 * g + 114 * b,
            (r, g, b),
        )
    });
    let mut remap = vec![0u8; colors];
    for (new_index, &old_index) in order.iter().enumerate() {
        remap[old_index] = new_index as u8;
    }
    let mut sorted_palette = Vec::with_capacity(palette.len());
    for &old_index in &order {
        sorted_palette.extend_from_slice(&palette[3 * old_index..3 * old_index + 3]);
    }
    let sorted_trns: Vec<u8> = if with_alpha {
        order.iter().map(|&old_index| trns[old_index]).collect()
    } else {
        Vec::new()
    };
    let indices: Vec<u8> = indices.iter().map(|&index| remap[index as usize]).collect();
    // Trim trailing opaque tRNS entries (tRNS implies the rest are opaque).
    let trns = if with_alpha {
        let mut sorted_trns = sorted_trns;
        while sorted_trns.last() == Some(&255) {
            sorted_trns.pop();
        }
        if sorted_trns.is_empty() {
            None
        } else {
            Some(sorted_trns)
        }
    } else {
        None
    };
    let (depth, packed) = pack_indices(
        &indices,
        frame.width() as usize,
        frame.height() as usize,
        sorted_palette.len() / 3,
    );
    Some(ReducedImage::Indexed {
        width: frame.width(),
        height: frame.height(),
        depth,
        palette: sorted_palette,
        trns,
        pixels: packed,
    })
}

/// Packs gray samples to the narrowest depth that fits (8/4/2/1).
fn pack_gray(gray: &[u8], width: usize, height: usize) -> (png::BitDepth, Vec<u8>) {
    debug_assert_eq!(gray.len(), width * height);
    // Only exact power-step values pack down: 1-bit {0,255}, 2-bit {0,85,
    // 170,255}, 4-bit multiples of 17. Anything else stays 8-bit.
    let fits = |step: u8| gray.iter().all(|&v| v % step == 0);
    let depth_bits = if fits(255) {
        1
    } else if fits(85) {
        2
    } else if fits(17) {
        4
    } else {
        8
    };
    if depth_bits == 8 {
        return (png::BitDepth::Eight, gray.to_vec());
    }
    let packed = pack_bits(gray, width, height, depth_bits, |v| match depth_bits {
        1 => v / 255,
        2 => v / 85,
        _ => v / 17,
    });
    let depth = match depth_bits {
        1 => png::BitDepth::One,
        2 => png::BitDepth::Two,
        _ => png::BitDepth::Four,
    };
    (depth, packed)
}

/// Packs palette indices to the narrowest depth fitting the palette size.
fn pack_indices(
    indices: &[u8],
    width: usize,
    height: usize,
    palette_len: usize,
) -> (png::BitDepth, Vec<u8>) {
    let depth_bits = if palette_len <= 2 {
        1
    } else if palette_len <= 4 {
        2
    } else if palette_len <= 16 {
        4
    } else {
        8
    };
    if depth_bits == 8 {
        return (png::BitDepth::Eight, indices.to_vec());
    }
    let packed = pack_bits(indices, width, height, depth_bits, |v| v);
    let depth = match depth_bits {
        1 => png::BitDepth::One,
        2 => png::BitDepth::Two,
        _ => png::BitDepth::Four,
    };
    (depth, packed)
}

fn pack_bits(
    samples: &[u8],
    width: usize,
    height: usize,
    depth_bits: usize,
    map: impl Fn(u8) -> u8,
) -> Vec<u8> {
    let row_bytes = width.div_ceil(8 / depth_bits);
    let mut packed = vec![0u8; row_bytes * height];
    for y in 0..height {
        for x in 0..width {
            let value = map(samples[y * width + x]);
            let bit_pos = x * depth_bits;
            packed[y * row_bytes + bit_pos / 8] |= value << (8 - depth_bits - bit_pos % 8);
        }
    }
    packed
}

fn encode_reduced(
    reduced: &ReducedImage,
    gamma: Option<u32>,
    options: &PngEncodeOptions,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let (width, height) = match reduced {
        ReducedImage::Gray { width, height, .. }
        | ReducedImage::Rgb { width, height, .. }
        | ReducedImage::Rgba { width, height, .. }
        | ReducedImage::Indexed { width, height, .. } => (*width, *height),
    };
    crate::limits::check_dimensions(ErrorSource::PngWriter, width, height)?;
    let mut encoder = png::Encoder::new(&mut out, width, height);
    let pixels: &[u8] = match reduced {
        ReducedImage::Gray { depth, pixels, .. } => {
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(*depth);
            pixels
        }
        ReducedImage::Rgb { pixels, .. } => {
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            pixels
        }
        ReducedImage::Rgba { pixels, .. } => {
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            pixels
        }
        ReducedImage::Indexed {
            depth,
            palette,
            trns,
            pixels,
            ..
        } => {
            encoder.set_color(png::ColorType::Indexed);
            encoder.set_depth(*depth);
            encoder.set_palette(palette.clone());
            if let Some(trns) = trns {
                encoder.set_trns(trns.clone());
            }
            pixels
        }
    };
    // NOTE: compression first — the png crate's set_compression
    // overwrites the filter with its own default.
    encoder.set_compression(options.compression.to_png_crate());
    encoder.set_filter(options.filter.to_png_crate());
    if let Some(gamma) = gamma {
        encoder.set_source_gamma(png::ScaledFloat::from_scaled(gamma));
    }
    let mut writer = encoder.write_header().map_err(|err| {
        Error::new(
            crate::error::ErrorKind::MemoryError,
            ErrorSource::PngWriter,
            format!("png header write failed: {err}"),
        )
    })?;
    writer.write_image_data(pixels).map_err(|err| {
        Error::new(
            crate::error::ErrorKind::MemoryError,
            ErrorSource::PngWriter,
            format!("png encode failed: {err}"),
        )
    })?;
    drop(writer);
    Ok(out)
}

/// Reads the gAMA chunk value (gamma × 100000), if present and well-formed.
pub(crate) fn scan_gama(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 8 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut pos = 8;
    while pos + 8 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        let id = &bytes[pos + 4..pos + 8];
        let end = pos.checked_add(12)?.checked_add(len)?;
        if end > bytes.len() {
            return None;
        }
        if id == b"gAMA" && len == 4 {
            return Some(u32::from_be_bytes([
                bytes[pos + 8],
                bytes[pos + 9],
                bytes[pos + 10],
                bytes[pos + 11],
            ]));
        }
        if id == b"IDAT" {
            return None;
        }
        pos = end;
    }
    None
}

/// Sentinel for an unknown quality (`kNoQualityGiven`).
pub const NO_QUALITY_GIVEN: i32 = -1;

/// Converts a JPEG to lossy WebP (`OptimizeWebp`): the output quality is the
/// input estimate capped by the configured quality, on the DEFAULT preset
/// with method 3.
pub fn optimize_webp_from_jpeg(bytes: &[u8], configured_quality: i32) -> Result<Vec<u8>> {
    let configured = if configured_quality == NO_QUALITY_GIVEN {
        75
    } else {
        configured_quality
    };
    let input_quality = crate::jpeg::estimate_quality(bytes).map_or(NO_QUALITY_GIVEN, i32::from);
    let output_quality = if input_quality != NO_QUALITY_GIVEN && input_quality < configured {
        input_quality
    } else {
        configured
    };
    let frame = crate::jpeg::decode(bytes)?;
    let config = crate::webp::WebpEncodeConfig {
        lossless: false,
        quality: output_quality.clamp(1, 100) as f32,
        method: 3,
        ..Default::default()
    };
    crate::webp::encode(&frame, &config)
}

/// Recompresses a WebP image at a lower quality (`ReduceWebpImageQuality`).
///
/// Qualities below 1 return the original bytes; above 100 clamp. The C++
/// round-trips through YUV(A) space directly; this port goes through RGB(A),
/// one extra colorspace hop for a simpler pipeline.
pub fn reduce_webp_quality(bytes: &[u8], quality: i32) -> Result<Vec<u8>> {
    if quality < 1 {
        return Ok(bytes.to_vec());
    }
    let frame = crate::webp::decode(bytes)?;
    // Note: no method override here — the C++ keeps the libwebp default (4).
    let config = crate::webp::WebpEncodeConfig {
        lossless: false,
        quality: quality.clamp(1, 100) as f32,
        method: 4,
        ..Default::default()
    };
    crate::webp::encode(&frame, &config)
}

/// Estimated bytes-per-pixel for a JPEG quality (`JpegPixelToByteRatio`).
///
/// Least-squares fit (slope 4.92865674, intercept 0.04177743) over measured
/// corpora; qualities above 95 clamp to 95, negatives to 95 as well.
pub fn jpeg_pixel_to_byte_ratio(quality: i64) -> f64 {
    let level = if !(0..=95).contains(&quality) {
        95
    } else {
        quality
    };
    4.92865674 / (110.0 - level as f64) + 0.04177743
}

/// Progressive-JPEG gate (`ShouldConvertToProgressive`): convert when the
/// input reaches `threshold_bytes` unless the resized output would estimate
/// below it.
pub fn should_convert_to_progressive(
    quality: i64,
    threshold_bytes: i64,
    num_bytes: usize,
    desired_width: u32,
    desired_height: u32,
) -> bool {
    if (num_bytes as i64) < threshold_bytes {
        return false;
    }
    let num_pixels = desired_width as f64 * desired_height as f64;
    let estimated_bytes = (num_pixels * jpeg_pixel_to_byte_ratio(quality)) as i64;
    estimated_bytes >= threshold_bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_option_defaults_match_cpp() {
        let options = JpegCompressionOptions::default();
        assert!(!options.progressive && !options.lossy);
        assert!(!options.retain_color_profile && !options.retain_exif_data);
        assert_eq!(options.lossy_options.quality, 85);
        assert_eq!(options.lossy_options.num_scans, -1);
        assert_eq!(options.lossy_options.color_sampling, ColorSampling::Yuv420);
    }

    #[test]
    fn progressive_gate_matches_cpp() {
        // Below threshold: never.
        assert!(!should_convert_to_progressive(85, 10240, 10239, 800, 600));
        // Above threshold with a large-enough estimate: yes.
        assert!(should_convert_to_progressive(85, 10240, 20000, 800, 600));
        // Above threshold but tiny output estimate: no.
        assert!(!should_convert_to_progressive(85, 10240, 20000, 10, 10));
    }

    #[test]
    fn byte_ratio_clamps_like_cpp() {
        assert_eq!(jpeg_pixel_to_byte_ratio(100), jpeg_pixel_to_byte_ratio(95));
        assert_eq!(jpeg_pixel_to_byte_ratio(-5), jpeg_pixel_to_byte_ratio(95));
        assert!(jpeg_pixel_to_byte_ratio(85) > jpeg_pixel_to_byte_ratio(50));
    }

    #[test]
    fn gama_scan_finds_gamma() {
        let mut png = vec![];
        png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&[0u8; 13]);
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(&4u32.to_be_bytes());
        png.extend_from_slice(b"gAMA");
        png.extend_from_slice(&45455u32.to_be_bytes());
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(b"IDAT");
        assert_eq!(scan_gama(&png), Some(45455));
        assert_eq!(scan_gama(b"junk"), None);
    }
}
