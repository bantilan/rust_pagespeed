//! WebP decoding and encoding through a vendored libwebp.
//!
//! Ports `WebpScanlineReader` (decode), `WebpFrameWriter` (still and animated
//! encode), and the `WebpConfiguration` option struct. This is the crate's
//! only FFI: `unsafe` is confined to this module, every raw-pointer use
//! carries a `SAFETY` comment, and the public API is panic-free across the
//! boundary (the progress hook performs no allocation and cannot panic).
//!
//! Animated WebP decoding is unsupported, exactly like the C++ reader:
//! [`decode`] refuses animated inputs, and animation frames are produced
//! from GIF iteration ([`crate::gif`]) instead.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::os::raw::{c_float, c_int};
use std::time::Instant;

use libwebp_sys::{
    WebPAnimEncoderAdd, WebPAnimEncoderAssemble, WebPAnimEncoderDelete, WebPAnimEncoderNewInternal,
    WebPAnimEncoderOptions, WebPAnimEncoderOptionsInitInternal, WebPConfig, WebPConfigPreset,
    WebPData, WebPDataClear, WebPDataInit, WebPDecodeRGBAInto, WebPEncode, WebPPicture,
    WebPPictureFree, WebPPictureImportRGB, WebPPictureImportRGBA, WebPPictureInit,
    WebPValidateConfig, WEBP_MUX_ABI_VERSION, WEBP_PRESET_DEFAULT,
};

use crate::error::{Error, ErrorSource, Result};
use crate::frame::{Frame, PixelFormat};
use crate::limits;

/// Still-image encode configuration, mirroring `WebpConfiguration`.
#[derive(Debug, Clone)]
pub struct WebpEncodeConfig {
    /// Lossless (VP8L) instead of lossy (VP8) encoding.
    pub lossless: bool,
    /// Quality 0..=100.
    pub quality: f32,
    /// Quality/speed trade-off 0..=6 (PageSpeed uses 3).
    pub method: i32,
    /// Desired output size in bytes; 0 disables.
    pub target_size: u32,
    /// Alpha-plane compression: 0 = none, 1 = lossless WebP.
    pub alpha_compression: i32,
    /// Alpha-plane predictive filtering: 0 = none, 1 = fast, 2 = best.
    pub alpha_filtering: i32,
    /// Alpha quality 0..=100. Like the C++, a zero value with transparent
    /// pixels fails the encode (opaque-only fast path).
    pub alpha_quality: i32,
}

impl Default for WebpEncodeConfig {
    fn default() -> Self {
        WebpEncodeConfig {
            lossless: true,
            quality: 75.0,
            method: 3,
            target_size: 0,
            alpha_compression: 1,
            alpha_filtering: 1,
            alpha_quality: 100,
        }
    }
}

/// Animated-image encode configuration (see `ConvertAnimatedGifToWebp`).
#[derive(Debug, Clone)]
pub struct WebpAnimConfig {
    /// Lossless frames instead of lossy ones.
    pub lossless: bool,
    /// Quality 0..=100.
    pub quality: f32,
    /// Minimum keyframe interval (0 disables keyframes).
    pub kmin: i32,
    /// Maximum keyframe interval.
    pub kmax: i32,
    /// Total play count for the ANIM chunk (0 = infinite).
    pub loop_count: u32,
}

impl Default for WebpAnimConfig {
    fn default() -> Self {
        WebpAnimConfig {
            lossless: false,
            quality: 75.0,
            kmin: 3,
            kmax: 5,
            loop_count: 0,
        }
    }
}

/// libwebp features for `bytes`, used to differentially test the pure-Rust
/// [`crate::format::webp_features`] parser against the real `WebPGetFeatures`.
#[cfg(test)]
pub(crate) fn libwebp_features(bytes: &[u8]) -> Option<crate::format::WebpFeatures> {
    use crate::format::WebpFeatures;
    use libwebp_sys::VP8_STATUS_OK;
    let mut raw: libwebp_sys::WebPBitstreamFeatures = unsafe { std::mem::zeroed() };
    // SAFETY: input pointer/len are valid; `raw` is a valid out-param.
    let status = unsafe {
        libwebp_sys::WebPGetFeaturesInternal(
            bytes.as_ptr(),
            bytes.len(),
            &mut raw,
            libwebp_sys::WEBP_DECODER_ABI_VERSION,
        )
    };
    if status != VP8_STATUS_OK {
        return None;
    }
    if raw.width <= 0 || raw.height <= 0 {
        return None;
    }
    Some(WebpFeatures {
        has_animation: raw.has_animation != 0,
        has_alpha: raw.has_alpha != 0,
        is_lossless: raw.format == 2,
        width: raw.width as u32,
        height: raw.height as u32,
    })
}

/// Decodes a still WebP image to RGB or RGBA, matching the C++ reader's
/// format selection (RGBA only with transparency).
pub fn decode(bytes: &[u8]) -> Result<Frame> {
    let features = crate::format::webp_features(bytes)
        .ok_or_else(|| Error::parse(ErrorSource::WebpReader, "invalid WebP bitstream"))?;
    if features.has_animation {
        return Err(Error::unsupported_feature(
            ErrorSource::WebpReader,
            "animated WebP decoding is not supported",
        ));
    }
    limits::check_dimensions(ErrorSource::WebpReader, features.width, features.height)?;
    let stride = features.width as usize * 4;
    let mut rgba = vec![0u8; stride * features.height as usize];
    // SAFETY: input and output pointers/lengths are valid; stride matches.
    let out = unsafe {
        WebPDecodeRGBAInto(
            bytes.as_ptr(),
            bytes.len(),
            rgba.as_mut_ptr(),
            rgba.len(),
            stride as c_int,
        )
    };
    if out.is_null() {
        return Err(Error::parse(
            ErrorSource::WebpReader,
            "WebP pixel decode failed",
        ));
    }
    let frame = Frame::from_parts(features.width, features.height, PixelFormat::Rgba8888, rgba)?;
    // The C++ reader reports RGB_888 when the image has no alpha channel.
    Ok(frame.strip_opaque_alpha())
}

/// Encodes a frame as a still WebP image.
pub fn encode(frame: &Frame, config: &WebpEncodeConfig) -> Result<Vec<u8>> {
    encode_with_deadline(frame, config, None).map(|(bytes, _)| bytes)
}

/// Encodes with an optional monotonic deadline, port of the
/// `ConversionTimeoutHandler` progress-hook wiring: when the deadline passes,
/// encoding aborts unless output bytes are already flowing (progress is being
/// made). Returns the bytes and whether the deadline fired.
pub fn encode_with_deadline(
    frame: &Frame,
    config: &WebpEncodeConfig,
    deadline: Option<Instant>,
) -> Result<(Vec<u8>, bool)> {
    limits::check_dimensions(ErrorSource::WebpWriter, frame.width(), frame.height())?;
    if config.alpha_quality == 0 && frame.has_transparency() {
        // Matches ConvertPngToWebp: the opaque fast path (alpha disabled)
        // fails on transparent pixels instead of dropping the alpha.
        return Err(Error::unsupported_feature(
            ErrorSource::WebpWriter,
            "alpha disabled but the image has transparent pixels",
        ));
    }

    let mut lib_config: WebPConfig = unsafe { std::mem::zeroed() };
    // SAFETY: `lib_config` is a valid out-param.
    if unsafe {
        WebPConfigPreset(
            &mut lib_config,
            WEBP_PRESET_DEFAULT,
            config.quality as c_float,
        )
    } == 0
    {
        return Err(Error::internal(
            ErrorSource::WebpWriter,
            "WebPConfigPreset failed",
        ));
    }
    lib_config.lossless = if config.lossless { 1 } else { 0 };
    lib_config.method = config.method;
    lib_config.target_size = config.target_size as c_int;
    lib_config.alpha_compression = config.alpha_compression;
    lib_config.alpha_filtering = config.alpha_filtering;
    lib_config.alpha_quality = config.alpha_quality;
    // SAFETY: reads a fully initialized config.
    if unsafe { WebPValidateConfig(&lib_config) } == 0 {
        return Err(Error::invocation(
            ErrorSource::WebpWriter,
            "invalid WebP configuration",
        ));
    }

    // WebP has no native grayscale: replicate luminance to RGB, like the C++
    // `should_expand_gray_to_rgb_` path. Fully opaque RGBA is stripped to
    // RGB first (the C++ `PixelFormatOptimizer` step): claiming an RGB
    // stride over RGBA bytes would shift every row.
    let expanded;
    let (pixels, use_alpha) = match frame.format() {
        PixelFormat::Gray8 => {
            expanded = frame.expand(PixelFormat::Rgb888)?;
            (expanded.pixels(), false)
        }
        PixelFormat::Rgb888 => (frame.pixels(), false),
        PixelFormat::Rgba8888 => {
            if frame.has_transparency() {
                (frame.pixels(), true)
            } else {
                expanded = frame.strip_opaque_alpha();
                (expanded.pixels(), false)
            }
        }
        PixelFormat::Unsupported => {
            return Err(Error::invocation(
                ErrorSource::WebpWriter,
                "cannot encode an unsupported pixel format",
            ));
        }
    };

    let mut picture: WebPPicture = unsafe { std::mem::zeroed() };
    // SAFETY: `picture` is a valid out-param.
    if unsafe { WebPPictureInit(&mut picture) } == 0 {
        return Err(Error::internal(
            ErrorSource::WebpWriter,
            "WebPPictureInit failed",
        ));
    }
    let _picture_guard = PictureGuard(&mut picture);

    let mut hook_data = ProgressHookData {
        deadline,
        timed_out: false,
    };
    // Output accumulator owned here; the writer callback appends through
    // `custom_ptr`, like the C++ GoogleStringWebpWriter.
    let mut output: Vec<u8> = Vec::new();
    picture.writer = Some(vec_webp_writer);
    picture.custom_ptr = &mut output as *mut Vec<u8> as *mut c_void;
    picture.width = frame.width() as c_int;
    picture.height = frame.height() as c_int;
    // Always keep ARGB (like the C++ writer): lossless encodes it directly,
    // and lossy converts from it — importing to YUV first would lossy-round
    // even "lossless" output.
    picture.use_argb = 1;
    if deadline.is_some() {
        picture.progress_hook = Some(progress_hook);
        picture.user_data = &mut hook_data as *mut ProgressHookData as *mut c_void;
    }

    let stride = frame.width() as c_int * if use_alpha { 4 } else { 3 };
    // SAFETY: pixels/stride/dimensions describe the frame buffer exactly.
    let imported = unsafe {
        if use_alpha {
            WebPPictureImportRGBA(&mut picture, pixels.as_ptr(), stride)
        } else {
            WebPPictureImportRGB(&mut picture, pixels.as_ptr(), stride)
        }
    };
    if imported == 0 {
        return Err(Error::new(
            crate::error::ErrorKind::MemoryError,
            ErrorSource::WebpWriter,
            "WebPPictureImport failed",
        ));
    }
    // SAFETY: config and picture are initialized and borrowed correctly.
    let ok = unsafe { WebPEncode(&lib_config, &mut picture) };
    if ok == 0 {
        if hook_data.timed_out {
            return Err(Error::new(
                crate::error::ErrorKind::Timeout,
                ErrorSource::WebpWriter,
                "WebP encode exceeded its deadline",
            ));
        }
        return Err(Error::parse(ErrorSource::WebpWriter, "WebPEncode failed"));
    }
    Ok((output, hook_data.timed_out))
}

/// libwebp writer callback appending to our output `Vec`, like the C++
/// `GoogleStringWebpWriter`. Returns nonzero on success.
extern "C" fn vec_webp_writer(
    data: *const u8,
    data_size: usize,
    picture: *const WebPPicture,
) -> c_int {
    // SAFETY: libwebp passes valid chunk bytes and the picture we configured;
    // custom_ptr points at our live output Vec.
    unsafe {
        if data.is_null() || picture.is_null() {
            return 0;
        }
        let output = (*picture).custom_ptr as *mut Vec<u8>;
        if output.is_null() {
            return 0;
        }
        let chunk = std::slice::from_raw_parts(data, data_size);
        (*output).extend_from_slice(chunk);
        1
    }
}

/// Progress-hook state, equivalent to `ConversionTimeoutHandler`.
struct ProgressHookData {
    deadline: Option<Instant>,
    timed_out: bool,
}

/// libwebp progress hook. Returns nonzero to continue, zero to abort.
/// Never panics or allocates; only reads hook state and the writer size.
extern "C" fn progress_hook(_percent: c_int, picture: *const WebPPicture) -> c_int {
    // SAFETY: libwebp passes the picture we configured; user_data points at
    // our live hook state and custom_ptr at our live memory writer.
    unsafe {
        if picture.is_null() {
            return 0;
        }
        let data = (*picture).user_data as *mut ProgressHookData;
        if data.is_null() {
            return 0;
        }
        let expired = (*data)
            .deadline
            .is_some_and(|deadline| Instant::now() > deadline);
        if !expired {
            return 1;
        }
        // Past the deadline: keep going only while output bytes are flowing
        // (progress is being made), exactly like Continue().
        let output = (*picture).custom_ptr as *const Vec<u8>;
        let flowing = !output.is_null() && !(*output).is_empty();
        if flowing {
            return 1;
        }
        (*data).timed_out = true;
        0
    }
}

/// Encodes composited full-canvas frames as an animated WebP image.
///
/// `frames` yields `(canvas, delay_ms)` pairs; timestamps accumulate from the
/// delays, matching `ConvertAnimatedGifToWebp`. All canvases must share the
/// first frame's dimensions. The optional deadline aborts through each
/// frame's progress hook, like the C++ animated path.
pub fn encode_animated(
    frames: &[(Frame, u32)],
    config: &WebpAnimConfig,
    deadline: Option<Instant>,
) -> Result<(Vec<u8>, bool)> {
    let Some((first, _)) = frames.first() else {
        return Err(Error::invocation(
            ErrorSource::WebpWriter,
            "animated WebP needs at least one frame",
        ));
    };
    limits::check_dimensions(ErrorSource::WebpWriter, first.width(), first.height())?;
    for (frame, _) in frames {
        if frame.width() != first.width() || frame.height() != first.height() {
            return Err(Error::invocation(
                ErrorSource::WebpWriter,
                "animated WebP frames must share dimensions",
            ));
        }
    }

    let mut options: WebPAnimEncoderOptions = unsafe { std::mem::zeroed() };
    // SAFETY: `options` is a valid out-param.
    if unsafe { WebPAnimEncoderOptionsInitInternal(&mut options, WEBP_MUX_ABI_VERSION) } == 0 {
        return Err(Error::internal(
            ErrorSource::WebpWriter,
            "WebPAnimEncoderOptionsInit failed",
        ));
    }
    options.anim_params.loop_count = config.loop_count as c_int;
    options.minimize_size = 0;
    options.kmin = config.kmin;
    options.kmax = config.kmax;
    options.allow_mixed = 0;
    options.verbose = 0;

    // SAFETY: dimensions are positive; options is initialized.
    let encoder = unsafe {
        WebPAnimEncoderNewInternal(
            first.width() as c_int,
            first.height() as c_int,
            &options,
            WEBP_MUX_ABI_VERSION,
        )
    };
    if encoder.is_null() {
        return Err(Error::new(
            crate::error::ErrorKind::MemoryError,
            ErrorSource::WebpWriter,
            "WebPAnimEncoderNew failed",
        ));
    }
    struct EncoderGuard(*mut libwebp_sys::WebPAnimEncoder);
    impl Drop for EncoderGuard {
        fn drop(&mut self) {
            // SAFETY: guard owns a live encoder.
            unsafe {
                WebPAnimEncoderDelete(self.0);
            }
        }
    }
    let _encoder_guard = EncoderGuard(encoder);

    let mut lib_config: WebPConfig = unsafe { std::mem::zeroed() };
    // SAFETY: `lib_config` is a valid out-param.
    if unsafe {
        WebPConfigPreset(
            &mut lib_config,
            WEBP_PRESET_DEFAULT,
            config.quality as c_float,
        )
    } == 0
    {
        return Err(Error::internal(
            ErrorSource::WebpWriter,
            "WebPConfigPreset failed",
        ));
    }
    lib_config.method = 3;
    lib_config.lossless = if config.lossless { 1 } else { 0 };
    lib_config.alpha_quality = 100;
    lib_config.alpha_compression = 1;

    let mut hook_data = ProgressHookData {
        deadline,
        timed_out: false,
    };
    let mut timestamp_ms = 0;
    for (frame, delay_ms) in frames {
        let mut picture: WebPPicture = unsafe { std::mem::zeroed() };
        // SAFETY: `picture` is a valid out-param.
        if unsafe { WebPPictureInit(&mut picture) } == 0 {
            return Err(Error::internal(
                ErrorSource::WebpWriter,
                "WebPPictureInit failed",
            ));
        }
        let _picture_guard = PictureGuard(&mut picture);
        picture.width = frame.width() as c_int;
        picture.height = frame.height() as c_int;
        picture.use_argb = 1;
        if deadline.is_some() {
            picture.progress_hook = Some(progress_hook);
            picture.user_data = &mut hook_data as *mut ProgressHookData as *mut c_void;
        }

        let expanded;
        let (pixels, use_alpha) = match frame.format() {
            PixelFormat::Gray8 => {
                expanded = frame.expand(PixelFormat::Rgb888)?;
                (expanded.pixels(), false)
            }
            PixelFormat::Rgb888 => (frame.pixels(), false),
            PixelFormat::Rgba8888 => (frame.pixels(), true),
            PixelFormat::Unsupported => {
                return Err(Error::invocation(
                    ErrorSource::WebpWriter,
                    "cannot encode an unsupported pixel format",
                ));
            }
        };
        let stride = frame.width() as c_int * if use_alpha { 4 } else { 3 };
        // SAFETY: pixels/stride/dimensions describe the frame buffer exactly.
        let imported = unsafe {
            if use_alpha {
                WebPPictureImportRGBA(&mut picture, pixels.as_ptr(), stride)
            } else {
                WebPPictureImportRGB(&mut picture, pixels.as_ptr(), stride)
            }
        };
        if imported == 0 {
            return Err(Error::new(
                crate::error::ErrorKind::MemoryError,
                ErrorSource::WebpWriter,
                "WebPPictureImport failed",
            ));
        }
        // SAFETY: encoder, picture, and config are all live and initialized.
        if unsafe { WebPAnimEncoderAdd(encoder, &mut picture, timestamp_ms, &lib_config) } == 0 {
            if hook_data.timed_out {
                return Err(Error::new(
                    crate::error::ErrorKind::Timeout,
                    ErrorSource::WebpWriter,
                    "animated WebP encode exceeded its deadline",
                ));
            }
            return Err(Error::parse(
                ErrorSource::WebpWriter,
                "WebPAnimEncoderAdd failed",
            ));
        }
        timestamp_ms += *delay_ms as c_int;
    }
    // One final add with a NULL frame flushes the encoder (duration extends
    // past the last frame by its delay, like the C++ timestamp accumulation).
    // SAFETY: encoder and config live; NULL picture is the documented flush.
    if unsafe { WebPAnimEncoderAdd(encoder, std::ptr::null_mut(), timestamp_ms, &lib_config) } == 0
    {
        return Err(Error::parse(
            ErrorSource::WebpWriter,
            "WebPAnimEncoder flush failed",
        ));
    }

    let mut output: WebPData = unsafe { std::mem::zeroed() };
    // SAFETY: `output` is a valid out-param.
    unsafe {
        WebPDataInit(&mut output);
    }
    // SAFETY: encoder and output are live.
    if unsafe { WebPAnimEncoderAssemble(encoder, &mut output) } == 0 {
        // SAFETY: output was initialized; clearing a failed assemble is safe.
        unsafe {
            WebPDataClear(&mut output);
        }
        return Err(Error::parse(
            ErrorSource::WebpWriter,
            "WebPAnimEncoderAssemble failed",
        ));
    }
    // SAFETY: assemble filled `output` with `size` valid bytes.
    let bytes = unsafe { std::slice::from_raw_parts(output.bytes, output.size).to_vec() };
    // SAFETY: output owns its buffer until cleared here.
    unsafe {
        WebPDataClear(&mut output);
    }
    Ok((bytes, hook_data.timed_out))
}

struct PictureGuard(*mut WebPPicture);
impl Drop for PictureGuard {
    fn drop(&mut self) {
        // SAFETY: guard owns a successfully initialized picture.
        unsafe {
            WebPPictureFree(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn default_config_matches_cpp() {
        let config = WebpEncodeConfig::default();
        assert!(config.lossless);
        assert_eq!(config.quality, 75.0);
        assert_eq!(config.method, 3);
        assert_eq!(config.alpha_quality, 100);
    }

    #[test]
    fn pure_parser_agrees_with_libwebp() {
        // The hand-written `webp_features` parser must agree with the real
        // WebPGetFeatures on every corpus file, byte for byte.
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/webp");
        let mut count = 0;
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            if path
                .extension()
                .is_some_and(|ext| ext != "webp" && ext != "webpla")
            {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let mine = crate::format::webp_features(&bytes);
            let theirs = libwebp_features(&bytes);
            assert_eq!(mine, theirs, "{}", path.display());
            count += 1;
        }
        assert!(count > 5, "no webp fixtures found");

        // ... and on adversarial truncations of a valid file.
        let bytes = std::fs::read(dir.join("opaque_32x20.webp")).unwrap();
        for len in [0, 1, 11, 12, 13, 20, 21, 30, 31, 40] {
            let prefix = &bytes[..len.min(bytes.len())];
            assert_eq!(
                crate::format::webp_features(prefix),
                libwebp_features(prefix),
                "truncation at {len}"
            );
        }
    }
}
