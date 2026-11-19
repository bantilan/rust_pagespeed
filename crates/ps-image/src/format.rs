//! Image formats, rewrite-level image types, and content sniffing.
//!
//! Ports `ImageFormat` (`image_util.h`), `ImageType`
//! (`kernel/http/image_types.proto`), `ImageTypeToImageFormat` and
//! `ComputeImageType` (`image_util.cc`), plus the WebP bitstream feature
//! probe that `ComputeImageType` delegates to `WebPGetFeatures`.
//!
//! The C++ sniffing order is preserved exactly: AVIF brand first (needs 12
//! bytes), then a first-byte switch needing only 8 bytes. In particular a
//! 2-byte `FF D8` prefix inside an 8-byte buffer already sniffs as JPEG.

/// Kernel-level image format (`ImageFormat` in `image_util.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    Unknown,
    Jpeg,
    Png,
    Gif,
    Webp,
    Avif,
}

/// Rewrite-level image type (`ImageType` in `image_types.proto`).
///
/// Discriminants match the proto field values so persisted stats and logs
/// stay comparable with the C++ implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ImageType {
    Unknown = 0,
    Jpeg = 1,
    Png = 2,
    Gif = 3,
    Webp = 4,
    /// WebP that is lossless or carries transparency.
    WebpLosslessOrAlpha = 5,
    WebpAnimated = 6,
    Avif = 7,
}

impl ImageType {
    /// Port of `ImageTypeToImageFormat` in `net/instaweb/rewriter/image.cc`.
    #[must_use]
    pub fn format(self) -> ImageFormat {
        match self {
            ImageType::Unknown => ImageFormat::Unknown,
            ImageType::Jpeg => ImageFormat::Jpeg,
            ImageType::Png => ImageFormat::Png,
            ImageType::Gif => ImageFormat::Gif,
            ImageType::Webp | ImageType::WebpLosslessOrAlpha | ImageType::WebpAnimated => {
                ImageFormat::Webp
            }
            ImageType::Avif => ImageFormat::Avif,
        }
    }

    /// Port of `Image::TypeToContentType`: the MIME type served for this type.
    #[must_use]
    pub fn mime_type(self) -> Option<&'static str> {
        match self {
            ImageType::Unknown => None,
            ImageType::Jpeg => Some("image/jpeg"),
            ImageType::Png => Some("image/png"),
            ImageType::Gif => Some("image/gif"),
            ImageType::Webp | ImageType::WebpLosslessOrAlpha | ImageType::WebpAnimated => {
                Some("image/webp")
            }
            ImageType::Avif => Some("image/avif"),
        }
    }

    /// Canonical lowercase file extension used for rewritten variants.
    #[must_use]
    pub fn extension(self) -> Option<&'static str> {
        match self {
            ImageType::Unknown => None,
            ImageType::Jpeg => Some("jpg"),
            ImageType::Png => Some("png"),
            ImageType::Gif => Some("gif"),
            ImageType::Webp | ImageType::WebpLosslessOrAlpha | ImageType::WebpAnimated => {
                Some("webp")
            }
            ImageType::Avif => Some("avif"),
        }
    }
}

/// Features of a WebP bitstream, mirroring the `WebPBitstreamFeatures` fields
/// that `ComputeImageType` consults (`has_animation`, `has_alpha`, `format`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebpFeatures {
    /// VP8X animation flag with at least one animation frame chunk.
    pub has_animation: bool,
    /// Any alpha signal: VP8L alpha bit or VP8X alpha flag.
    pub has_alpha: bool,
    /// True for VP8L payloads (libwebp `format == 2`).
    pub is_lossless: bool,
    /// Canvas dimensions in pixels.
    pub width: u32,
    /// Canvas dimensions in pixels.
    pub height: u32,
}

/// Minimum bytes `ComputeImageType` looks at before classifying.
pub const SNIFF_MIN_BYTES: usize = 8;
const AVIF_BRAND_MIN_BYTES: usize = 12;
const PNG_MAGIC: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const GIF_MAGIC: &[u8; 4] = b"GIF8";

/// Classifies bytes by magic, port of `ComputeImageType` (`image_util.cc`).
#[must_use]
pub fn sniff(buf: &[u8]) -> ImageType {
    if buf.len() < SNIFF_MIN_BYTES {
        return ImageType::Unknown;
    }
    // AVIF brand check comes first and needs 12 bytes: bytes 4..12 must be
    // "ftypavif" or "ftypavis" (the "ftyp" box tag is part of the brand).
    if buf.len() >= AVIF_BRAND_MIN_BYTES
        && buf[4] == b'f'
        && buf[5] == b't'
        && buf[6] == b'y'
        && buf[7] == b'p'
        && (buf[8] == b'a'
            && buf[9] == b'v'
            && buf[10] == b'i'
            && (buf[11] == b'f' || buf[11] == b's'))
    {
        return ImageType::Avif;
    }
    match buf[0] {
        // Either JPEG or JPEG-2000 (the latter is unhandled, like the C++).
        0xff => {
            if buf[1] == 0xd8 {
                ImageType::Jpeg
            } else {
                ImageType::Unknown
            }
        }
        0x89 => {
            if buf.starts_with(PNG_MAGIC) {
                ImageType::Png
            } else {
                ImageType::Unknown
            }
        }
        b'G' => {
            if buf.starts_with(GIF_MAGIC) && (buf[4] == b'7' || buf[4] == b'9') && buf[5] == b'a' {
                ImageType::Gif
            } else {
                ImageType::Unknown
            }
        }
        // Possible WebP: full bitstream-feature probe, like WebPGetFeatures.
        b'R' => match webp_features(buf) {
            Some(features) if features.has_animation => ImageType::WebpAnimated,
            Some(features) if features.is_lossless || features.has_alpha => {
                ImageType::WebpLosslessOrAlpha
            }
            Some(_) => ImageType::Webp,
            None => ImageType::Unknown,
        },
        _ => ImageType::Unknown,
    }
}

fn read_u24le(buf: &[u8]) -> u32 {
    u32::from(buf[0]) | (u32::from(buf[1]) << 8) | (u32::from(buf[2]) << 16)
}

/// Parses just enough of a WebP container to answer the three questions
/// `ComputeImageType` asks libwebp: animated? alpha? lossless?
///
/// Handles the simple (`VP8 `, `VP8L`) and extended (`VP8X`) containers.
/// Returns `None` for truncated or structurally invalid bitstreams, matching
/// the `VP8_STATUS_OK` gate in the C++.
pub fn webp_features(buf: &[u8]) -> Option<WebpFeatures> {
    if buf.len() < 12 || &buf[0..4] != b"RIFF" || &buf[8..12] != b"WEBP" {
        return None;
    }
    let mut pos = 12;
    // libwebp requires the first chunk to carry image data: VP8X, VP8, or
    // VP8L. Anything else (e.g. the corrupt_header.webp VP9X decoy) fails
    // WebPGetFeatures outright.
    if pos + 8 > buf.len() {
        return None;
    }
    let first = &buf[pos..pos + 4];
    if first == b"VP8 " || first == b"VP8L" {
        // Simple container: the first chunk is the whole story (no animation
        // possible). Only the frame header must be present; the rest of the
        // payload may be truncated, like WebPGetFeatures allows.
        let size = u32::from_le_bytes([buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]]);
        let payload_start = pos + 8;
        let payload_end = payload_start.checked_add(size as usize)?;
        let payload = if payload_end > buf.len() {
            &buf[payload_start..]
        } else {
            &buf[payload_start..payload_end]
        };
        if first == b"VP8 " {
            let (width, height) = vp8_lossy_dims(payload)?;
            return Some(WebpFeatures {
                has_animation: false,
                has_alpha: false,
                is_lossless: false,
                width,
                height,
            });
        }
        let (width, height, has_alpha) = vp8_lossless_dims(payload)?;
        return Some(WebpFeatures {
            has_animation: false,
            has_alpha,
            is_lossless: true,
            width,
            height,
        });
    }
    if first != b"VP8X" {
        return None;
    }
    // Extended-format flags, when a VP8X chunk has been seen.
    let mut vp8x_flags: Option<u8> = None;
    let mut vp8x_size: Option<(u32, u32)> = None;
    let mut image_size: Option<(u32, u32)> = None;
    let mut saw_image_chunk = false;
    let mut lossless = false;
    let mut alpha = false;
    let mut animated = false;

    while pos + 8 <= buf.len() {
        let fourcc = &buf[pos..pos + 4];
        let size = u32::from_le_bytes([buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]]);
        let payload_start = pos + 8;
        // Chunk sizes are u32; guard against overflow past the buffer end.
        let payload_end = payload_start.checked_add(size as usize)?;
        // libwebp validates only the frame header at GetFeatures time: a
        // truncated image payload (like corrupt_body.webp) still yields
        // features, and fails later at decode. Structural chunks must be
        // fully present.
        let payload = if payload_end > buf.len() {
            if fourcc != b"VP8 " && fourcc != b"VP8L" {
                return None;
            }
            &buf[payload_start..]
        } else {
            &buf[payload_start..payload_end]
        };
        match fourcc {
            b"VP8X" => {
                if payload.len() < 10 {
                    return None;
                }
                vp8x_flags = Some(payload[0]);
                let width = read_u24le(&payload[4..7]) + 1;
                let height = read_u24le(&payload[7..10]) + 1;
                if width == 0 || height == 0 {
                    return None;
                }
                vp8x_size = Some((width, height));
            }
            b"VP8 " => {
                let (width, height) = vp8_lossy_dims(payload)?;
                saw_image_chunk = true;
                image_size = Some((width, height));
            }
            b"VP8L" => {
                let (width, height, has_alpha) = vp8_lossless_dims(payload)?;
                saw_image_chunk = true;
                lossless = true;
                alpha |= has_alpha;
                image_size = Some((width, height));
            }
            b"ANIM" | b"ANMF" => {
                animated = true;
            }
            b"ALPH" => {
                alpha = true;
            }
            _ => {}
        }
        // Chunks are padded to even sizes: advance to the next even offset.
        pos = (payload_end + 1) & !1;
    }

    if let Some(flags) = vp8x_flags {
        // VP8X feature flags: 0x10 alpha, 0x02 animation.
        alpha |= flags & 0x10 != 0;
        // libwebp sets has_animation from the flag; animation frame chunks
        // corroborate it. The flag alone short-circuits validation, like the
        // early return in ParseHeadersInternal.
        animated |= flags & 0x02 != 0;
    }
    let (width, height) = vp8x_size?;
    if !saw_image_chunk && !animated {
        // Metadata-only containers have no decodable image; libwebp reports
        // VP8_STATUS_BITSTREAM_ERROR for these. Animated containers are the
        // exception: their VP8/VP8L payloads live inside ANMF chunks, so a
        // top-level image chunk is not required.
        return None;
    }
    // Image-size coherency: a non-animated VP8X canvas must match the
    // bitstream dimensions, or libwebp reports BITSTREAM_ERROR.
    if let Some(image_size) = image_size {
        if !animated && image_size != (width, height) {
            return None;
        }
    }
    Some(WebpFeatures {
        has_animation: animated,
        has_alpha: alpha,
        is_lossless: lossless,
        width,
        height,
    })
}

/// Dimensions of a lossy `VP8 ` payload. Only keyframes carry dimensions;
/// interframe-only payloads fail, matching `WebPGetFeatures`.
fn vp8_lossy_dims(payload: &[u8]) -> Option<(u32, u32)> {
    if payload.len() < 10 {
        return None;
    }
    // Frame tag: bit 0 clear means keyframe.
    if payload[0] & 0x01 != 0 {
        return None;
    }
    // Keyframe start code 0x9D 0x01 0x2A.
    if payload[3] != 0x9d || payload[4] != 0x01 || payload[5] != 0x2a {
        return None;
    }
    let width = u32::from(payload[6]) | (u32::from(payload[7]) << 8);
    let height = u32::from(payload[8]) | (u32::from(payload[9]) << 8);
    let width = width & 0x3fff;
    let height = height & 0x3fff;
    if width == 0 || height == 0 {
        return None;
    }
    Some((width, height))
}

/// Dimensions and alpha bit of a lossless `VP8L` payload.
fn vp8_lossless_dims(payload: &[u8]) -> Option<(u32, u32, bool)> {
    if payload.len() < 5 {
        return None;
    }
    // VP8L signature byte.
    if payload[0] != 0x2f {
        return None;
    }
    let bits = u32::from_le_bytes([payload[1], payload[2], payload[3], payload[4]]);
    let width = (bits & 0x3fff) + 1;
    let height = ((bits >> 14) & 0x3fff) + 1;
    let has_alpha = (bits >> 28) & 0x01 != 0;
    let version = (bits >> 29) & 0x07;
    if version != 0 {
        return None;
    }
    Some((width, height, has_alpha))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_maps_to_format() {
        assert_eq!(ImageType::Jpeg.format(), ImageFormat::Jpeg);
        assert_eq!(ImageType::WebpAnimated.format(), ImageFormat::Webp);
        assert_eq!(ImageType::WebpLosslessOrAlpha.format(), ImageFormat::Webp);
        assert_eq!(ImageType::Unknown.format(), ImageFormat::Unknown);
    }

    #[test]
    fn mime_types_match_cpp() {
        assert_eq!(ImageType::Jpeg.mime_type(), Some("image/jpeg"));
        assert_eq!(ImageType::Png.mime_type(), Some("image/png"));
        assert_eq!(ImageType::Gif.mime_type(), Some("image/gif"));
        assert_eq!(ImageType::Webp.mime_type(), Some("image/webp"));
        assert_eq!(
            ImageType::WebpLosslessOrAlpha.mime_type(),
            Some("image/webp")
        );
        assert_eq!(ImageType::WebpAnimated.mime_type(), Some("image/webp"));
        assert_eq!(ImageType::Avif.mime_type(), Some("image/avif"));
        assert_eq!(ImageType::Unknown.mime_type(), None);
    }

    #[test]
    fn sniff_needs_eight_bytes() {
        assert_eq!(sniff(&[]), ImageType::Unknown);
        assert_eq!(sniff(&[0xff, 0xd8]), ImageType::Unknown);
        let mut buf = [0u8; 8];
        buf[0] = 0xff;
        buf[1] = 0xd8;
        assert_eq!(sniff(&buf), ImageType::Jpeg);
    }

    #[test]
    fn sniff_png_and_gif() {
        let mut png = [0u8; 8];
        png.copy_from_slice(PNG_MAGIC);
        assert_eq!(sniff(&png), ImageType::Png);
        assert_eq!(sniff(b"GIF89a\x01\x02"), ImageType::Gif);
        assert_eq!(sniff(b"GIF87a\x01\x02"), ImageType::Gif);
        assert_eq!(sniff(b"GIF86a\x01\x02"), ImageType::Unknown);
    }

    #[test]
    fn sniff_avif_brands() {
        let mut avif = [0u8; 12];
        avif[4..12].copy_from_slice(b"ftypavif");
        assert_eq!(sniff(&avif), ImageType::Avif);
        avif[8..12].copy_from_slice(b"avis");
        assert_eq!(sniff(&avif), ImageType::Avif);
        // Too short for the brand check: falls through to first-byte switch.
        assert_eq!(sniff(&avif[..11]), ImageType::Unknown);
    }

    #[test]
    fn sniff_webp_lossy_and_lossless() {
        // Minimal VP8 keyframe: tag + start code + 1x1 dims.
        let mut vp8 = vec![];
        vp8.extend_from_slice(b"RIFF");
        vp8.extend_from_slice(&22u32.to_le_bytes());
        vp8.extend_from_slice(b"WEBP");
        vp8.extend_from_slice(b"VP8 ");
        vp8.extend_from_slice(&10u32.to_le_bytes());
        vp8.extend_from_slice(&[0x00, 0x00, 0x00, 0x9d, 0x01, 0x2a, 1, 0, 1, 0]);
        assert_eq!(sniff(&vp8), ImageType::Webp);
        let features = webp_features(&vp8).expect("features");
        assert_eq!((features.width, features.height), (1, 1));
        assert!(!features.has_alpha && !features.is_lossless);

        // Minimal VP8L: signature + 1x1, alpha bit set.
        let mut vp8l = vec![];
        vp8l.extend_from_slice(b"RIFF");
        vp8l.extend_from_slice(&18u32.to_le_bytes());
        vp8l.extend_from_slice(b"WEBP");
        vp8l.extend_from_slice(b"VP8L");
        vp8l.extend_from_slice(&5u32.to_le_bytes());
        vp8l.extend_from_slice(&[0x2f, 0x00, 0x00, 0x00, 0x10]);
        assert_eq!(sniff(&vp8l), ImageType::WebpLosslessOrAlpha);
    }

    #[test]
    fn sniff_webp_animated_and_alpha() {
        // VP8X with animation + alpha flags, VP8 payload, one ANMF chunk.
        let mut vp8x = vec![];
        vp8x.extend_from_slice(b"RIFF");
        vp8x.extend_from_slice(&100u32.to_le_bytes());
        vp8x.extend_from_slice(b"WEBP");
        vp8x.extend_from_slice(b"VP8X");
        vp8x.extend_from_slice(&10u32.to_le_bytes());
        vp8x.extend_from_slice(&[0x12, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        vp8x.extend_from_slice(b"VP8 ");
        vp8x.extend_from_slice(&10u32.to_le_bytes());
        vp8x.extend_from_slice(&[0x00, 0x00, 0x00, 0x9d, 0x01, 0x2a, 1, 0, 1, 0]);
        vp8x.extend_from_slice(b"ANMF");
        vp8x.extend_from_slice(&0u32.to_le_bytes());
        let features = webp_features(&vp8x).expect("features");
        assert!(features.has_animation && features.has_alpha);
        assert_eq!(sniff(&vp8x), ImageType::WebpAnimated);
    }

    #[test]
    fn sniff_rejects_broken_webp() {
        assert_eq!(sniff(b"RIFF\xff\xff\xff\xff"), ImageType::Unknown);
        // RIFF/WEBP tags but truncated chunk header.
        assert_eq!(sniff(b"RIFF\x10\x00\x00\x00WEBP"), ImageType::Unknown);
        // No image chunk: metadata-only container.
        let mut meta = vec![];
        meta.extend_from_slice(b"RIFF");
        meta.extend_from_slice(&20u32.to_le_bytes());
        meta.extend_from_slice(b"WEBP");
        meta.extend_from_slice(b"VP8X");
        meta.extend_from_slice(&10u32.to_le_bytes());
        meta.extend_from_slice(&[0u8; 10]);
        assert_eq!(sniff(&meta), ImageType::Unknown);
    }
}
