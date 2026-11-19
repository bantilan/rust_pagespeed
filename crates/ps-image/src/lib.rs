//! Image decoding, optimization, and format conversion.
//!
//! Rust port of the `pagespeed/kernel/image` scanline engine plus the
//! `net/instaweb/rewriter/image.cc` optimization driver. The pipeline works
//! frame-at-a-time instead of scanline-at-a-time: every decoder produces one
//! owned [`frame::Frame`], transforms consume and produce frames, and every
//! encoder takes a frame. Byte limits from [`limits`] bound every allocation.
//!
//! Codec backends are pure Rust (`png`, `jpeg-decoder`, `jpeg-encoder`,
//! `gif`) except WebP, which links a vendored libwebp through the `webp`
//! module's narrow FFI: only libwebp implements the lossy, lossless, and
//! animated encoders PageSpeed relies on. That module is the crate's only
//! `unsafe` code.
#![deny(unsafe_code)]

pub mod analysis;
pub mod avif;
pub mod converter;
pub mod error;
pub mod format;
pub mod frame;
pub mod gif;
pub mod image;
pub mod jpeg;
pub mod limits;
pub mod negotiate;
pub mod optimize;
pub mod png;
pub mod resizer;
pub mod spriter;
pub mod stats;
pub mod variant;
pub mod webp;

/// Returns true for image extensions the optimizer can handle.
#[must_use]
pub fn extension_is_supported(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "avif"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_extensions() {
        assert!(extension_is_supported("webp"));
        assert!(extension_is_supported("JPG"));
        assert!(!extension_is_supported("bmp"));
    }
}
