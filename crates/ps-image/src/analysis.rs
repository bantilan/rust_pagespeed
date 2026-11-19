//! Image content analysis: gradients, histograms, and photo detection.
//!
//! Ports `SobelGradient`, `Histogram`, `WidestPeakWidth`, `PhotoMetric`, and
//! `IsPhoto` from `image_analysis.cc`. The floating-point arithmetic follows
//! the C++ operation order exactly (f32 throughout, same roundings) so photo
//! metrics match bit-for-bit.

use crate::frame::{Frame, PixelFormat};

/// Histogram bins (`kNumColorHistogramBins`).
pub const HISTOGRAM_BINS: usize = 256;
/// Histogram significance threshold (`kHistogramThreshold`).
pub const HISTOGRAM_THRESHOLD: f32 = 0.01;
/// Photo metric cut-off (`kPhotoMetricThreshold`).
pub const PHOTO_METRIC_THRESHOLD: f32 = 16.0;

/// Computes the Sobel gradient of a frame's luminance into a fresh buffer.
///
/// Border pixels are zero; luminance is the R+G+B average. Returns `None`
/// for images smaller than 3x3 or unsupported formats (the C++ returns false
/// and `PhotoMetric` then reports 0).
pub fn sobel_gradient(frame: &Frame) -> Option<Vec<u8>> {
    let width = frame.width() as usize;
    let height = frame.height() as usize;
    if width < 3 || height < 3 {
        return None;
    }
    let mut gradient = vec![0u8; width * height];
    match frame.format() {
        PixelFormat::Gray8 => {
            gradient_from_luminance_u8(
                frame.pixels(),
                width,
                height,
                frame.bytes_per_row(),
                1.0,
                &mut gradient,
            );
        }
        PixelFormat::Rgb888 | PixelFormat::Rgba8888 => {
            let channels = frame.format().channels();
            let mut luminance = vec![0i32; width * height];
            for (y, row) in frame
                .pixels()
                .chunks_exact(frame.bytes_per_row())
                .enumerate()
                .take(height)
            {
                for x in 0..width {
                    let base = x * channels;
                    // The C++ stores the channel SUM here; the /3 averaging
                    // happens through norm_factor in the gradient pass.
                    luminance[y * width + x] =
                        i32::from(row[base]) + i32::from(row[base + 1]) + i32::from(row[base + 2]);
                }
            }
            gradient_from_luminance_i32(&luminance, width, height, width, 1.0 / 3.0, &mut gradient);
        }
        PixelFormat::Unsupported => return None,
    }
    Some(gradient)
}

/// Sobel kernels (x and y): `[1 2 1; 0 0 0; -1 -2 -1]` and its transpose.
/// The 0.25 factor removes the Sobel magnification; results are rounded with
/// +0.5 and clamped to 255, exactly like `ComputeGradientFromLuminance`.
fn gradient_from_luminance_u8(
    luminance: &[u8],
    width: usize,
    height: usize,
    elements_per_line: usize,
    norm_factor: f32,
    gradient: &mut [u8],
) {
    let norm_factor = norm_factor * 0.25;
    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let at = |row: usize, col: usize| -> i32 {
                i32::from(luminance[row * elements_per_line + col])
            };
            let dif_y = at(y - 1, x - 1) + (at(y - 1, x) << 1) + at(y - 1, x + 1)
                - at(y + 1, x - 1)
                - (at(y + 1, x) << 1)
                - at(y + 1, x + 1);
            let dif_x = at(y - 1, x - 1) + (at(y, x - 1) << 1) + at(y + 1, x - 1)
                - at(y - 1, x + 1)
                - (at(y, x + 1) << 1)
                - at(y + 1, x + 1);
            let dif2 = (dif_x * dif_x + dif_y * dif_y) as f32;
            let dif = dif2.sqrt() * norm_factor + 0.5;
            gradient[y * width + x] = dif.min(255.0) as u8;
        }
    }
}

fn gradient_from_luminance_i32(
    luminance: &[i32],
    width: usize,
    height: usize,
    elements_per_line: usize,
    norm_factor: f32,
    gradient: &mut [u8],
) {
    let norm_factor = norm_factor * 0.25;
    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let at = |row: usize, col: usize| -> i32 { luminance[row * elements_per_line + col] };
            let dif_y = at(y - 1, x - 1) + (at(y - 1, x) << 1) + at(y - 1, x + 1)
                - at(y + 1, x - 1)
                - (at(y + 1, x) << 1)
                - at(y + 1, x + 1);
            let dif_x = at(y - 1, x - 1) + (at(y, x - 1) << 1) + at(y + 1, x - 1)
                - at(y - 1, x + 1)
                - (at(y, x + 1) << 1)
                - at(y + 1, x + 1);
            let dif2 = (dif_x * dif_x + dif_y * dif_y) as f32;
            let dif = dif2.sqrt() * norm_factor + 0.5;
            gradient[y * width + x] = dif.min(255.0) as u8;
        }
    }
}

/// Histogram of a rectangular region: raw per-value counts as f32.
///
/// Despite the C++ comment claiming normalization, the code stores plain
/// counts — and only relative comparisons use them, so this ports the code.
pub fn histogram(
    image: &[u8],
    bytes_per_line: usize,
    x0: usize,
    y0: usize,
    width: usize,
    height: usize,
) -> [f32; HISTOGRAM_BINS] {
    let mut hist = [0u32; HISTOGRAM_BINS];
    for y in y0..y0 + height {
        let row_start = y * bytes_per_line + x0;
        for x in 0..width {
            hist[image[row_start + x] as usize] += 1;
        }
    }
    let mut out = [0.0f32; HISTOGRAM_BINS];
    for (slot, count) in out.iter_mut().zip(hist.iter()) {
        *slot = *count as f32;
    }
    out
}

/// Width of the widest histogram peak: a maximal run of bins at or above
/// `max * threshold`, port of `WidestPeakWidth`.
pub fn widest_peak_width(hist: &[f32; HISTOGRAM_BINS], threshold: f32) -> f32 {
    let max_hist = hist.iter().fold(0.0f32, |max, &bin| max.max(bin));
    let threshold_hist = threshold * max_hist;
    let mut widest_peak: i32 = 0;
    let mut i = 0;
    while i < HISTOGRAM_BINS {
        while i < HISTOGRAM_BINS && hist[i] < threshold_hist {
            i += 1;
        }
        let first_significant_bin = i;
        while i < HISTOGRAM_BINS && hist[i] >= threshold_hist {
            i += 1;
        }
        let width = (i - first_significant_bin) as f32;
        if (widest_peak as f32) < width {
            widest_peak = width as i32;
        }
    }
    widest_peak as f32
}

/// Photo-likeness metric in 0..=256: photos score high (wide gradient
/// histogram), flat graphics score low. Returns 0 when no gradient can be
/// computed, conservatively reporting "not a photo".
pub fn photo_metric(frame: &Frame) -> f32 {
    let Some(gradient) = sobel_gradient(frame) else {
        return 0.0;
    };
    let width = frame.width() as usize;
    let height = frame.height() as usize;
    // The border holds no gradient, so it is excluded from the histogram.
    let hist = histogram(&gradient, width, 1, 1, width - 2, height - 2);
    widest_peak_width(&hist, HISTOGRAM_THRESHOLD)
}

/// True when the frame looks photographic. Images with transparency always
/// report false (over 99% of transparent images are graphics, per the C++),
/// as do empty or unsupported frames.
pub fn is_photo(frame: &Frame) -> bool {
    if frame.format() == PixelFormat::Unsupported
        || frame.format() == PixelFormat::Rgba8888
        || frame.width() == 0
        || frame.height() == 0
    {
        return false;
    }
    photo_metric(frame) >= PHOTO_METRIC_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widest_peak_width_matches_cpp() {
        // Single peak 3 wide above the 1% line.
        let mut hist = [0.0f32; HISTOGRAM_BINS];
        hist[10] = 100.0;
        hist[11] = 50.0;
        hist[12] = 2.0;
        hist[200] = 1.0;
        assert_eq!(widest_peak_width(&hist, 0.01), 3.0);
        // All bins equal: one peak spanning everything.
        let hist = [5.0f32; HISTOGRAM_BINS];
        assert_eq!(widest_peak_width(&hist, 0.01), 256.0);
    }

    #[test]
    fn gradient_needs_3x3() {
        let tiny = Frame::solid(2, 2, PixelFormat::Gray8, &[128]).unwrap();
        assert!(sobel_gradient(&tiny).is_none());
        assert_eq!(photo_metric(&tiny), 0.0);
        assert!(!is_photo(&tiny));
    }

    #[test]
    fn flat_images_are_not_photos() {
        let flat = Frame::solid(32, 32, PixelFormat::Rgb888, &[200, 100, 50]).unwrap();
        assert_eq!(photo_metric(&flat), 1.0);
        assert!(!is_photo(&flat));
    }

    #[test]
    fn transparent_images_are_not_photos() {
        let frame = Frame::solid(32, 32, PixelFormat::Rgba8888, &[200, 100, 50, 255]).unwrap();
        assert!(!is_photo(&frame));
    }
}
