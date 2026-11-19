//! Cross-format conversion and smallest-of-N selection.
//!
//! Ports `ImageConverter::ConvertPngToJpeg`, `ConvertPngToWebp`,
//! `OptimizePngOrConvertToJpeg`, and `GetSmallestOfPngJpegWebp`, including
//! the 0.8 lossy-savings ratios and the strict-smaller selection rule.
//! Inputs may be PNG or GIF bytes (the C++ `PngReaderInterface` covers
//! both; GIF converts from its first frame).

use crate::error::{Error, ErrorSource, Result};
use crate::format::{sniff, ImageType};
use crate::frame::Frame;
use crate::optimize::JpegCompressionOptions;
use crate::webp::WebpEncodeConfig;

/// Minimum byte-savings ratio for choosing lossy JPEG over lossless
/// (`kMinJpegSavingsRatio`): JPEG must be under 80% of the PNG size.
pub const MIN_JPEG_SAVINGS_RATIO: f64 = 0.8;
/// Minimum byte-savings ratio for choosing lossy WebP (`kMinWebpSavingsRatio`).
pub const MIN_WEBP_SAVINGS_RATIO: f64 = 0.8;

/// Winning output of [`get_smallest_of_png_jpeg_webp`]
/// (`ImageConverter::ImageType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmallestType {
    /// No conversion won; the original bytes are returned.
    None,
    Png,
    Jpeg,
    Webp,
}

/// Decodes PNG bytes, or the first frame of GIF bytes, like the C++
/// `PngReaderInterface` specializations behind the converter.
fn decode_png_or_gif(bytes: &[u8]) -> Result<Frame> {
    match sniff(bytes) {
        ImageType::Png => crate::png::decode(bytes),
        ImageType::Gif => crate::gif::first_frame(bytes),
        _ => Err(Error::unsupported_format(
            "converter needs PNG or GIF input",
        )),
    }
}

/// Converts PNG/GIF bytes to JPEG (`ConvertPngToJpeg`).
///
/// Fails on transparency (JPEG cannot carry alpha; the C++ reader's
/// `require_opaque` gate), on empty images, and on encode errors.
pub fn convert_png_to_jpeg(bytes: &[u8], options: &JpegCompressionOptions) -> Result<Vec<u8>> {
    let frame = decode_png_or_gif(bytes)?;
    if frame.has_transparency() {
        return Err(Error::unsupported_feature(
            ErrorSource::JpegWriter,
            "cannot convert transparent pixels to JPEG",
        ));
    }
    if frame.width() == 0 || frame.height() == 0 {
        return Err(Error::invocation(
            ErrorSource::JpegWriter,
            "cannot convert an empty image",
        ));
    }
    // RETAIN with non-JPEG input keeps the encoder default (420), matching
    // the C++ fall-through when no decompress struct exists.
    let encode_options = crate::jpeg::JpegEncodeOptions {
        quality: options.lossy_options.quality.clamp(1, 100) as u8,
        progressive: options.progressive,
        num_scans: options.lossy_options.num_scans,
        sampling: options.lossy_options.color_sampling,
        app_segments: Vec::new(),
        optimized_huffman: true,
    };
    crate::jpeg::encode(&frame, &encode_options)
}

/// Converts PNG/GIF bytes to WebP (`ConvertPngToWebp`).
///
/// Returns the bytes and whether the input was opaque. Grayscale expands to
/// RGB inside the encoder; an `alpha_quality` of 0 refuses transparency.
pub fn convert_png_to_webp(bytes: &[u8], config: &WebpEncodeConfig) -> Result<(Vec<u8>, bool)> {
    let frame = decode_png_or_gif(bytes)?;
    if frame.width() == 0 || frame.height() == 0 {
        return Err(Error::invocation(
            ErrorSource::WebpWriter,
            "cannot convert an empty image",
        ));
    }
    let is_opaque = !frame.has_transparency();
    let encoded = crate::webp::encode(&frame, config)?;
    Ok((encoded, is_opaque))
}

/// Optimizes to PNG and converts to JPEG, keeping the substantially smaller
/// result (`OptimizePngOrConvertToJpeg`). JPEG wins only under 80% of the
/// optimized PNG size; the flag reports which format won.
pub fn optimize_png_or_convert_to_jpeg(
    bytes: &[u8],
    options: &JpegCompressionOptions,
) -> Result<(Vec<u8>, bool)> {
    let jpeg_out = convert_png_to_jpeg(bytes, options).ok();
    let png_out = crate::optimize::optimize_png_best(bytes).ok();
    match (jpeg_out, png_out) {
        (Some(jpeg), Some(png)) => {
            if jpeg.len() as f64 > MIN_JPEG_SAVINGS_RATIO * png.len() as f64 {
                Ok((png, true))
            } else {
                Ok((jpeg, false))
            }
        }
        (Some(jpeg), None) => Ok((jpeg, false)),
        (None, Some(png)) => Ok((png, true)),
        (None, None) => Err(Error::parse(
            ErrorSource::Util,
            "PNG optimization and JPEG conversion both failed",
        )),
    }
}

/// Picks the smallest of optimized PNG, lossless WebP, custom JPEG, and
/// custom WebP (`GetSmallestOfPngJpegWebp`).
///
/// Lossy candidates must beat the lossless best by the 0.8 savings ratio to
/// compensate for quality loss. When nothing wins, the original bytes are
/// returned with [`SmallestType::None`].
pub fn get_smallest_of_png_jpeg_webp(
    bytes: &[u8],
    jpeg_options: Option<&JpegCompressionOptions>,
    webp_config: Option<&WebpEncodeConfig>,
) -> (SmallestType, Vec<u8>) {
    // Lossless WebP with the default (lossless) configuration. A failed
    // attempt leaves no output, and the custom-quality attempt below can
    // only run when decoding succeeded.
    let (webp_lossless_out, decode_ok, is_opaque) = match decode_png_or_gif(bytes) {
        Ok(frame) => {
            let is_opaque = !frame.has_transparency();
            let out = crate::webp::encode(&frame, &WebpEncodeConfig::default()).ok();
            (out, true, is_opaque)
        }
        Err(_) => (None, false, false),
    };

    // Custom-quality WebP re-encodes the same decoded pixels.
    let webp_lossy_out = if webp_config.is_some() && decode_ok {
        let frame = decode_png_or_gif(bytes).ok();
        match (frame, webp_config) {
            (Some(frame), Some(config)) => crate::webp::encode(&frame, config).ok(),
            _ => None,
        }
    } else {
        None
    };

    let png_out = crate::optimize::optimize_png_best(bytes).ok();

    // JPEG is attempted unless a lossy WebP already exists for a
    // transparent image (where JPEG conversion is doomed anyway).
    let jpeg_out = match jpeg_options {
        Some(options) if webp_lossy_out.is_none() || is_opaque => {
            convert_png_to_jpeg(bytes, options).ok()
        }
        _ => None,
    };

    // Lossless round, seeded with the original (ratio 1, strict-smaller).
    let mut best_lossless: (SmallestType, &[u8]) = (SmallestType::None, bytes);
    for (kind, candidate) in [
        (SmallestType::Webp, webp_lossless_out.as_deref()),
        (SmallestType::Png, png_out.as_deref()),
    ] {
        if let Some(out) = candidate {
            if !out.is_empty() && out.len() < best_lossless.1.len() {
                best_lossless = (kind, out);
            }
        }
    }

    // Lossy round: first nonempty candidate seeds, then strict-smaller wins.
    let mut best_lossy: Option<(SmallestType, &[u8])> = None;
    for (kind, candidate) in [
        (SmallestType::Webp, webp_lossy_out.as_deref()),
        (SmallestType::Jpeg, jpeg_out.as_deref()),
    ] {
        if let Some(out) = candidate {
            if out.is_empty() {
                continue;
            }
            let take = match best_lossy {
                None => true,
                Some((_, best)) => out.len() < best.len(),
            };
            if take {
                best_lossy = Some((kind, out));
            }
        }
    }

    // The lossy winner must substantially beat the lossless winner — with
    // one quirk straight from `SelectSmallerImage`: when the ORIGINAL is
    // the lossless best, any nonempty lossy candidate wins unconditionally
    // (the `best == NONE takes all` arm fires before the ratio check).
    let mut best = best_lossless;
    if let Some((kind, out)) = best_lossy {
        if best.0 == SmallestType::None {
            best = (kind, out);
        } else {
            let ratio = if kind == SmallestType::Webp {
                MIN_WEBP_SAVINGS_RATIO
            } else {
                MIN_JPEG_SAVINGS_RATIO
            };
            if (out.len() as f64) < (best.1.len() as f64) * ratio {
                best = (kind, out);
            }
        }
    }

    (best.0, best.1.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn savings_ratios_match_cpp() {
        assert_eq!(MIN_JPEG_SAVINGS_RATIO, 0.8);
        assert_eq!(MIN_WEBP_SAVINGS_RATIO, 0.8);
    }
}
