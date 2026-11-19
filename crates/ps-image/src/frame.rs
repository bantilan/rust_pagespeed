//! Decoded pixel buffers and pixel-format conversion.
//!
//! Ports `PixelFormat` and the helpers in `scanline_utils.h/.cc`
//! (`GetNumChannelsFromPixelFormat`, `ExpandPixelFormat`), the opaque-alpha
//! stripping of `PixelFormatOptimizer`, and `GenerateBlankImage`
//! (`blank_image.cc`).
//!
//! Where the C++ hands out one scanline at a time, this module hands out one
//! frame at a time: every decoder below produces a single owned [`Frame`],
//! which keeps the pipeline free of per-scanline virtual calls while staying
//! within the configured byte limits (see [`crate::limits`]).

use crate::error::{Error, ErrorSource, Result};

/// Pixel layout of a decoded frame (`PixelFormat` in `image_util.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    Unsupported,
    /// RGB triplets, 24 bits per pixel.
    Rgb888,
    /// RGB triplet plus alpha channel, 32 bits per pixel.
    Rgba8888,
    /// Grayscale, 8 bits per pixel.
    Gray8,
}

impl PixelFormat {
    /// Port of `GetNumChannelsFromPixelFormat`; 0 for unsupported formats.
    #[must_use]
    pub fn channels(self) -> usize {
        match self {
            PixelFormat::Gray8 => 1,
            PixelFormat::Rgb888 => 3,
            PixelFormat::Rgba8888 => 4,
            PixelFormat::Unsupported => 0,
        }
    }
}

/// Fully opaque alpha value (`kAlphaOpaque`).
pub const ALPHA_OPAQUE: u8 = 255;
/// Fully transparent alpha value (`kAlphaTransparent`).
pub const ALPHA_TRANSPARENT: u8 = 0;

/// One decoded image frame: dimensions, pixel format, and tightly packed rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    width: u32,
    height: u32,
    format: PixelFormat,
    /// Row-major pixels, `width * height * format.channels()` bytes.
    pixels: Vec<u8>,
}

impl Frame {
    /// Builds a frame from validated parts.
    pub fn from_parts(
        width: u32,
        height: u32,
        format: PixelFormat,
        pixels: Vec<u8>,
    ) -> Result<Self> {
        let channels = format.channels();
        if channels == 0 {
            return Err(Error::invocation(
                ErrorSource::Util,
                "cannot build a frame with an unsupported pixel format",
            ));
        }
        let want = width as usize * height as usize * channels;
        if pixels.len() != want {
            return Err(Error::internal(
                ErrorSource::Util,
                format!(
                    "pixel buffer has {} bytes, need {want} for {width}x{height} {format:?}",
                    pixels.len(),
                ),
            ));
        }
        Ok(Frame {
            width,
            height,
            format,
            pixels,
        })
    }

    /// Builds a solid-color frame.
    pub fn solid(width: u32, height: u32, format: PixelFormat, pixel: &[u8]) -> Result<Self> {
        let channels = format.channels();
        if channels == 0 || pixel.len() != channels {
            return Err(Error::invocation(
                ErrorSource::Util,
                "solid pixel must match the frame pixel format",
            ));
        }
        let mut pixels = Vec::new();
        let count = width as usize * height as usize;
        pixels.try_reserve(count * channels).map_err(|_| {
            Error::new(
                crate::error::ErrorKind::MemoryError,
                ErrorSource::Util,
                "solid frame allocation failed",
            )
        })?;
        for _ in 0..count {
            pixels.extend_from_slice(pixel);
        }
        Frame::from_parts(width, height, format, pixels)
    }

    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    #[must_use]
    pub fn format(&self) -> PixelFormat {
        self.format
    }

    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    #[must_use]
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    #[must_use]
    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }

    /// Bytes per row without padding.
    #[must_use]
    pub fn bytes_per_row(&self) -> usize {
        self.width as usize * self.format.channels()
    }

    /// Row `y` as a byte slice.
    #[must_use]
    pub fn row(&self, y: u32) -> &[u8] {
        let stride = self.bytes_per_row();
        let start = y as usize * stride;
        &self.pixels[start..start + stride]
    }

    /// True when every alpha sample is opaque. Non-alpha formats report true,
    /// matching the `PixelFormatOptimizer` convention that opaque alpha can be
    /// stripped; callers use this to decide RGBA -> RGB narrowing.
    #[must_use]
    pub fn is_opaque(&self) -> bool {
        if self.format != PixelFormat::Rgba8888 {
            return true;
        }
        self.pixels
            .iter()
            .skip(3)
            .step_by(4)
            .all(|&a| a == ALPHA_OPAQUE)
    }

    /// True when any alpha sample is neither opaque nor fully transparent, or
    /// (for formats without alpha) always false. Used by the WebP-alpha
    /// decision: fully transparent pixels still count as transparency.
    #[must_use]
    pub fn has_transparency(&self) -> bool {
        if self.format != PixelFormat::Rgba8888 {
            return false;
        }
        self.pixels
            .iter()
            .skip(3)
            .step_by(4)
            .any(|&a| a != ALPHA_OPAQUE)
    }

    /// Converts this frame to `dst` format, port of `ExpandPixelFormat`.
    ///
    /// Supported: identity copies, gray -> RGB/RGBA (replicate), RGB -> RGBA
    /// (opaque alpha). Anything else fails, exactly like the C++ `DFATAL`
    /// paths, but as a recoverable error instead of a crash.
    pub fn expand(&self, dst: PixelFormat) -> Result<Frame> {
        if self.format == dst {
            return Ok(self.clone());
        }
        let converted = match (self.format, dst) {
            (PixelFormat::Gray8, PixelFormat::Rgb888) => {
                let mut out = Vec::with_capacity(self.pixels.len() * 3);
                for &g in &self.pixels {
                    out.extend_from_slice(&[g, g, g]);
                }
                out
            }
            (PixelFormat::Gray8, PixelFormat::Rgba8888) => {
                let mut out = Vec::with_capacity(self.pixels.len() * 4);
                for &g in &self.pixels {
                    out.extend_from_slice(&[g, g, g, ALPHA_OPAQUE]);
                }
                out
            }
            (PixelFormat::Rgb888, PixelFormat::Rgba8888) => {
                let mut out = Vec::with_capacity(self.pixels.len() * 4 / 3);
                for rgb in self.pixels.as_chunks::<3>().0.iter() {
                    out.extend_from_slice(&[rgb[0], rgb[1], rgb[2], ALPHA_OPAQUE]);
                }
                out
            }
            _ => {
                return Err(Error::unsupported_feature(
                    ErrorSource::Util,
                    format!(
                        "unsupported pixel format conversion: {:?} -> {dst:?}",
                        self.format
                    ),
                ));
            }
        };
        Frame::from_parts(self.width, self.height, dst, converted)
    }

    /// Strips a fully opaque alpha channel (RGBA -> RGB), the one narrowing
    /// `PixelFormatOptimizer` performs. Returns a clone when there is nothing
    /// to strip.
    #[must_use]
    pub fn strip_opaque_alpha(&self) -> Frame {
        if self.format != PixelFormat::Rgba8888 || self.has_transparency() {
            return self.clone();
        }
        let mut out = Vec::with_capacity(self.pixels.len() * 3 / 4);
        for rgba in self.pixels.as_chunks::<4>().0.iter() {
            out.extend_from_slice(&rgba[..3]);
        }
        // Length is exact by construction; only a bug could trip this.
        Frame::from_parts(self.width, self.height, PixelFormat::Rgb888, out)
            .expect("stripped buffer has exact length")
    }
}

/// Generates a blank PNG canvas, port of `GenerateBlankImage`.
///
/// The C++ fills the canvas with zero bytes (black, or transparent black)
/// and encodes with filter none and no compression; this port matches both.
pub fn generate_blank_image(width: u32, height: u32, transparent: bool) -> Result<Vec<u8>> {
    if width == 0 || height == 0 {
        return Err(Error::invocation(
            ErrorSource::Util,
            "blank image dimensions must be positive",
        ));
    }
    let (format, pixel): (PixelFormat, &[u8]) = if transparent {
        (PixelFormat::Rgba8888, &[0, 0, 0, ALPHA_TRANSPARENT])
    } else {
        (PixelFormat::Rgb888, &[0, 0, 0])
    };
    let frame = Frame::solid(width, height, format, pixel)?;
    let options = crate::png::PngEncodeOptions {
        filter: crate::png::PngFilter::None,
        compression: crate::png::PngCompression::None,
        interlaced: false,
    };
    crate::png::encode(&frame, &options)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba_frame() -> Frame {
        Frame::from_parts(
            2,
            1,
            PixelFormat::Rgba8888,
            vec![10, 20, 30, 255, 40, 50, 60, 128],
        )
        .unwrap()
    }

    #[test]
    fn channels_match_cpp() {
        assert_eq!(PixelFormat::Gray8.channels(), 1);
        assert_eq!(PixelFormat::Rgb888.channels(), 3);
        assert_eq!(PixelFormat::Rgba8888.channels(), 4);
        assert_eq!(PixelFormat::Unsupported.channels(), 0);
    }

    #[test]
    fn from_parts_validates_length() {
        assert!(Frame::from_parts(2, 2, PixelFormat::Rgb888, vec![0; 12]).is_ok());
        assert!(Frame::from_parts(2, 2, PixelFormat::Rgb888, vec![0; 11]).is_err());
        assert!(Frame::from_parts(1, 1, PixelFormat::Unsupported, vec![0]).is_err());
    }

    #[test]
    fn expand_replicates_gray() {
        let gray = Frame::from_parts(2, 1, PixelFormat::Gray8, vec![7, 200]).unwrap();
        let rgb = gray.expand(PixelFormat::Rgb888).unwrap();
        assert_eq!(rgb.pixels(), &[7, 7, 7, 200, 200, 200]);
        let rgba = gray.expand(PixelFormat::Rgba8888).unwrap();
        assert_eq!(rgba.pixels(), &[7, 7, 7, 255, 200, 200, 200, 255]);
    }

    #[test]
    fn expand_rejects_narrowing() {
        let frame = rgba_frame();
        assert!(frame.expand(PixelFormat::Rgb888).is_err());
        assert!(frame.expand(PixelFormat::Gray8).is_err());
    }

    #[test]
    fn transparency_and_opacity() {
        let frame = rgba_frame();
        assert!(!frame.is_opaque());
        assert!(frame.has_transparency());
        // Stripping must refuse: one pixel is semi-transparent.
        assert_eq!(frame.strip_opaque_alpha().format(), PixelFormat::Rgba8888);

        let opaque = Frame::from_parts(
            2,
            1,
            PixelFormat::Rgba8888,
            vec![10, 20, 30, 255, 40, 50, 60, 255],
        )
        .unwrap();
        assert!(opaque.is_opaque());
        assert!(!opaque.has_transparency());
        let stripped = opaque.strip_opaque_alpha();
        assert_eq!(stripped.format(), PixelFormat::Rgb888);
        assert_eq!(stripped.pixels(), &[10, 20, 30, 40, 50, 60]);
    }
}
