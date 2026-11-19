//! Decode and conversion limits.
//!
//! Ports the scanline caps from `scanline_utils.h`: images beyond these
//! dimensions are refused before any pixel buffer is allocated.

use crate::error::{Error, ErrorSource, Result};

/// Per-side pixel cap (`kScanlineMaxDimension`).
pub const MAX_DIMENSION: u32 = 16384;
/// Total pixel cap (`kScanlineMaxImagePixels`, 100 Mi pixels).
pub const MAX_IMAGE_PIXELS: u64 = 100 << 20;

/// Refuses zero-sized images and images beyond the scanline caps.
pub fn check_dimensions(source: ErrorSource, width: u32, height: u32) -> Result<()> {
    if width == 0 || height == 0 {
        return Err(Error::invocation(
            source,
            "image dimensions must be positive",
        ));
    }
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(Error::parse(
            source,
            format!("image dimensions {width}x{height} exceed the supported maximum"),
        ));
    }
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return Err(Error::parse(
            source,
            "image pixel count exceeds the supported maximum".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_match_cpp() {
        assert_eq!(MAX_DIMENSION, 16384);
        assert_eq!(MAX_IMAGE_PIXELS, 100 * 1024 * 1024);
        assert!(check_dimensions(ErrorSource::Util, 100, 100).is_ok());
        assert!(check_dimensions(ErrorSource::Util, 0, 100).is_err());
        assert!(check_dimensions(ErrorSource::Util, 16385, 1).is_err());
        assert!(check_dimensions(ErrorSource::Util, 16384, 16384).is_err());
    }
}
