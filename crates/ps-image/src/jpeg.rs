//! JPEG parsing, decoding, and encoding.
//!
//! This module covers the `JpegScanlineReader` header side (dimensions,
//! progressive detection, per-component sampling factors), `JpegUtils`
//! quality estimation, and the `JpegScanlineWriter` encode side (quality,
//! progressive scans, chroma sampling, marker retention).
//!
//! Decoding and pixel encoding delegate to the pure-Rust `jpeg-decoder` and
//! `jpeg-encoder` crates. Marker walking (SOF/DQT/APP/SOS) is implemented
//! here so the pipeline never depends on decoder internals for metadata.

use std::io::Cursor;

use crate::error::{Error, ErrorSource, Result};
use crate::frame::{Frame, PixelFormat};
use crate::limits;

/// Chroma subsampling for JPEG encoding (`ColorSampling`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorSampling {
    /// Keep the input image's sampling factors.
    Retain,
    /// 4:2:0 (the default, matching `JpegLossyOptions`).
    #[default]
    Yuv420,
    /// 4:2:2.
    Yuv422,
    /// 4:4:4 (no subsampling).
    Yuv444,
}

/// JPEG encode options, mirroring `JpegCompressionOptions` + `JpegLossyOptions`.
#[derive(Debug, Clone)]
pub struct JpegEncodeOptions {
    /// Quality in 1..=100. Values outside are clamped, like `jpeg_set_quality`.
    pub quality: u8,
    /// Emit a progressive JPEG (`jpeg_simple_progression` equivalent).
    pub progressive: bool,
    /// Truncate the progressive scan script to this many scans. Values <= 0
    /// keep the default script (`num_scans == -1` in the C++).
    pub num_scans: i32,
    /// Chroma subsampling.
    pub sampling: ColorSampling,
    /// Verbatim APP segments to copy (marker retention): `(app_number, payload)`
    /// with `app_number` in 1..=15. Payload excludes the `FF En` tag and length.
    pub app_segments: Vec<(u8, Vec<u8>)>,
    /// Optimize Huffman tables (`optimize_coding = TRUE` in the C++).
    pub optimized_huffman: bool,
}

impl Default for JpegEncodeOptions {
    fn default() -> Self {
        JpegEncodeOptions {
            quality: 85,
            progressive: false,
            num_scans: -1,
            sampling: ColorSampling::Yuv420,
            app_segments: Vec::new(),
            optimized_huffman: true,
        }
    }
}

/// Header metadata from a JPEG marker walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JpegInfo {
    pub width: u32,
    pub height: u32,
    /// True when the first SOF marker is SOF2 (progressive).
    pub progressive: bool,
    /// Component count from SOF (1, 3, or 4).
    pub num_components: u8,
    /// Per-component (horizontal, vertical) sampling factors from SOF.
    pub sampling_factors: Vec<(u8, u8)>,
    /// Adobe APP14 color transform byte, when an Adobe marker is present.
    pub adobe_transform: Option<u8>,
}

/// Scans JPEG markers for dimensions, progressive mode, sampling factors,
/// DQT tables, and APP segments in a single pass.
#[derive(Debug, Default)]
struct MarkerScan {
    info: Option<JpegInfo>,
    /// Quantization tables by id (0..=3), values in natural (row-major) order.
    quant_tables: [Option<[u16; 64]>; 4],
    /// Raw APP segment payloads: `(app_number, payload)`.
    app_segments: Vec<(u8, Vec<u8>)>,
    adobe_transform: Option<u8>,
}

/// Zigzag scan order (JPEG Annex A, Figure A.6): stream position `k` holds
/// the coefficient with natural index `ZIGZAG[k]`.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Standard luminance quantization table (JPEG spec section K.1).
const STD_LUMINANCE_QUANT: [u32; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113,
    92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];

/// Standard chrominance quantization table (JPEG spec section K.1).
const STD_CHROMINANCE_QUANT: [u32; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];

fn u16be(buf: &[u8], pos: usize) -> Option<u16> {
    buf.get(pos..pos + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
}

/// Walks JPEG markers, collecting SOF/DQT/APP data. Returns the scan plus the
/// byte ranges to keep for metadata stripping: callers that only need headers
/// ignore the ranges.
fn scan_markers(bytes: &[u8]) -> Result<MarkerScan> {
    if bytes.len() < 2 || bytes[0] != 0xff || bytes[1] != 0xd8 {
        return Err(Error::parse(ErrorSource::JpegReader, "missing SOI marker"));
    }
    let mut scan = MarkerScan::default();
    let mut pos = 2;
    while pos < bytes.len() {
        if bytes[pos] != 0xff {
            return Err(Error::parse(
                ErrorSource::JpegReader,
                format!("expected marker at offset {pos}"),
            ));
        }
        // Skip fill bytes; 0xFF00 inside entropy data never reaches us because
        // scan data is skipped wholesale below.
        while pos < bytes.len() && bytes[pos] == 0xff {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }
        let marker = bytes[pos];
        pos += 1;
        // Standalone markers without length: RST0-7, SOI, EOI, TEM.
        if marker == 0xd9 {
            break;
        }
        if marker == 0x01 || (0xd0..=0xd8).contains(&marker) {
            continue;
        }
        let len = u16be(bytes, pos)
            .ok_or_else(|| Error::parse(ErrorSource::JpegReader, "truncated marker length"))?
            as usize;
        if len < 2 || pos + len > bytes.len() {
            return Err(Error::parse(
                ErrorSource::JpegReader,
                format!("marker 0xFF{marker:02X} overruns input"),
            ));
        }
        let payload = &bytes[pos + 2..pos + len];
        match marker {
            // SOF0/1/2/3 (baseline, extended, progressive, lossless).
            0xc0..=0xc3 => {
                if payload.len() < 6 {
                    return Err(Error::parse(
                        ErrorSource::JpegReader,
                        "truncated SOF segment",
                    ));
                }
                let height = u16::from_be_bytes([payload[1], payload[2]]) as u32;
                let width = u16::from_be_bytes([payload[3], payload[4]]) as u32;
                let components = payload[5] as usize;
                if payload.len() < 6 + components * 3 {
                    return Err(Error::parse(
                        ErrorSource::JpegReader,
                        "truncated SOF components",
                    ));
                }
                let mut sampling = Vec::with_capacity(components);
                for c in 0..components {
                    let packed = payload[6 + c * 3 + 1];
                    sampling.push((packed >> 4, packed & 0x0f));
                }
                limits::check_dimensions(ErrorSource::JpegReader, width, height)?;
                scan.info = Some(JpegInfo {
                    width,
                    height,
                    progressive: marker == 0xc2,
                    num_components: components as u8,
                    sampling_factors: sampling,
                    adobe_transform: scan.adobe_transform,
                });
            }
            // DQT: one or more tables, values in zigzag order.
            0xdb => {
                let mut table_pos = 0;
                while table_pos < payload.len() {
                    let spec = payload[table_pos];
                    let precision_16 = spec & 0xf0 != 0;
                    let table_id = (spec & 0x0f) as usize;
                    table_pos += 1;
                    let step = if precision_16 { 2 } else { 1 };
                    if table_id >= 4 || table_pos + 64 * step > payload.len() {
                        break;
                    }
                    let mut table = [0u16; 64];
                    for k in 0..64 {
                        let value = if precision_16 {
                            u16::from_be_bytes([
                                payload[table_pos + k * 2],
                                payload[table_pos + k * 2 + 1],
                            ])
                        } else {
                            u16::from(payload[table_pos + k])
                        };
                        table[ZIGZAG[k]] = value;
                    }
                    scan.quant_tables[table_id] = Some(table);
                    table_pos += 64 * step;
                }
            }
            // APPn: keep raw payloads for marker retention.
            0xe0..=0xef => {
                let app = marker - 0xe0;
                if app == 14 && payload.len() >= 12 && &payload[..6] == b"Adobe\0" {
                    scan.adobe_transform = Some(payload[11]);
                    if let Some(info) = scan.info.as_mut() {
                        info.adobe_transform = Some(payload[11]);
                    }
                }
                scan.app_segments.push((app, payload.to_vec()));
            }
            // SOS: skip the header, then the entropy-coded scan that follows.
            0xda => {
                pos += len;
                pos = skip_scan_data(bytes, pos)?;
                continue;
            }
            _ => {}
        }
        pos += len;
    }
    Ok(scan)
}

/// Advances past entropy-coded scan data to the next marker. `FF 00` is a
/// stuffed byte, `FF D0..D7` are restarts without payload; anything else
/// starting with `FF` begins a new marker.
fn skip_scan_data(bytes: &[u8], mut pos: usize) -> Result<usize> {
    while pos + 1 < bytes.len() {
        if bytes[pos] != 0xff {
            pos += 1;
            continue;
        }
        let next = bytes[pos + 1];
        if next == 0x00 || (0xd0..=0xd7).contains(&next) {
            pos += 2;
            continue;
        }
        // Skip padding FFs; the marker starts at the last FF.
        let mut marker_pos = pos;
        while marker_pos + 1 < bytes.len() && bytes[marker_pos + 1] == 0xff {
            marker_pos += 1;
        }
        return Ok(marker_pos);
    }
    Err(Error::parse(
        ErrorSource::JpegReader,
        "scan data runs past end of input",
    ))
}

/// Reads JPEG header metadata (dimensions, progressive mode, sampling).
pub fn scan_info(bytes: &[u8]) -> Result<JpegInfo> {
    let scan = scan_markers(bytes)?;
    scan.info
        .ok_or_else(|| Error::parse(ErrorSource::JpegReader, "no SOF marker found"))
}

/// Collects verbatim APP segment payloads for marker retention.
///
/// Mirrors the C++ `jpeg_save_markers` calls: every APP1 segment is kept when
/// `retain_exif` is set, every APP2 segment when `retain_icc` is set,
/// regardless of the payload's inner magic.
pub fn retained_app_segments(
    bytes: &[u8],
    retain_exif: bool,
    retain_icc: bool,
) -> Result<Vec<(u8, Vec<u8>)>> {
    if !retain_exif && !retain_icc {
        return Ok(Vec::new());
    }
    let scan = scan_markers(bytes)?;
    Ok(scan
        .app_segments
        .into_iter()
        .filter(|(app, _)| (*app == 1 && retain_exif) || (*app == 2 && retain_icc))
        .collect())
}

/// Estimates the quality (1..=100) the input was encoded with, port of
/// `JpegUtils::GetImageQualityFromImage`. Returns `None` when no usable
/// quantization table is present (the C++ returns -1).
pub fn estimate_quality(bytes: &[u8]) -> Option<u8> {
    let scan = scan_markers(bytes).ok()?;
    let mut sum = 0.0f64;
    let mut count = 0.0f64;
    if let Some(table) = &scan.quant_tables[0] {
        sum += quality_entries_sum(table, &STD_LUMINANCE_QUANT);
        count += 64.0;
    }
    if let Some(table) = &scan.quant_tables[1] {
        sum += quality_entries_sum(table, &STD_CHROMINANCE_QUANT);
        count += 64.0;
    }
    if count > 0.0 {
        // Fractional quality scaled to 1..=100, rounded like the C++.
        let quality = sum * 100.0 / count + 0.5;
        Some(quality.clamp(1.0, 100.0) as u8)
    } else {
        None
    }
}

/// Port of `ComputeQualityEntriesSum`: inverts the libjpeg quality scaling
/// (`Q = 2 - 2*q*S` above q=0.5, `Q = (0.5/q)*S` below) against the standard
/// table to recover the fractional quality each entry implies.
fn quality_entries_sum(table: &[u16; 64], std_table: &[u32; 64]) -> f64 {
    let mut sum = 0.0;
    for i in 0..64 {
        if table[i] == 1 {
            // Minimum denominator implies quality 100.
            sum += 1.0;
        } else {
            let scale = f64::from(table[i]) / f64::from(std_table[i]);
            sum += if scale > 1.0 {
                0.5 / scale
            } else {
                (2.0 - scale) / 2.0
            };
        }
    }
    sum
}

/// Decodes a JPEG into a frame.
///
/// Grayscale decodes to [`PixelFormat::Gray8`], everything else to
/// [`PixelFormat::Rgb888`], matching the C++ reader's `out_color_space`
/// selection (grayscale preserved, all else converted to RGB). Plain CMYK is
/// converted with the naive inversion the C++ documents via libjpeg; Adobe
/// YCCK is refused — see the module docs for the follow-up.
pub fn decode(bytes: &[u8]) -> Result<Frame> {
    use jpeg_decoder::{Decoder, PixelFormat as JpegPixelFormat};
    let mut decoder = Decoder::new(Cursor::new(bytes));
    let pixels = decoder.decode().map_err(|err| {
        Error::parse(
            ErrorSource::JpegReader,
            format!("jpeg decode failed: {err}"),
        )
    })?;
    let info = decoder
        .info()
        .ok_or_else(|| Error::internal(ErrorSource::JpegReader, "decoder produced no info"))?;
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    limits::check_dimensions(ErrorSource::JpegReader, width, height)?;
    match info.pixel_format {
        JpegPixelFormat::L8 => Frame::from_parts(width, height, PixelFormat::Gray8, pixels),
        // 12-bit JPEGs fail in the C++ too (8-bit libjpeg-turbo build).
        JpegPixelFormat::L16 => Err(Error::unsupported_feature(
            ErrorSource::JpegReader,
            "12-bit JPEG samples are not supported",
        )),
        JpegPixelFormat::RGB24 => Frame::from_parts(width, height, PixelFormat::Rgb888, pixels),
        JpegPixelFormat::CMYK32 => {
            // jpeg-decoder hands out inverted components (255 - C, ...); the
            // naive CMYK -> RGB conversion is R = max(0, c + k - 255), matching
            // libjpeg's non-Adobe path up to rounding.
            if is_ycck(bytes) {
                return Err(Error::unsupported_feature(
                    ErrorSource::JpegReader,
                    "Adobe YCCK JPEGs are not supported",
                ));
            }
            let mut rgb = Vec::with_capacity(pixels.len() * 3 / 4);
            for cmyk in pixels.as_chunks::<4>().0.iter() {
                let k = u16::from(cmyk[3]);
                rgb.push(cmyk_add(cmyk[0], k));
                rgb.push(cmyk_add(cmyk[1], k));
                rgb.push(cmyk_add(cmyk[2], k));
            }
            Frame::from_parts(width, height, PixelFormat::Rgb888, rgb)
        }
    }
}

fn cmyk_add(channel_inverted: u8, k_inverted: u16) -> u8 {
    u16::from(channel_inverted)
        .saturating_add(k_inverted)
        .saturating_sub(255)
        .min(255) as u8
}

/// True when an Adobe APP14 marker declares the YCCK transform.
fn is_ycck(bytes: &[u8]) -> bool {
    scan_markers(bytes)
        .map(|scan| scan.adobe_transform == Some(1))
        .unwrap_or(false)
}

/// Encodes a frame as JPEG.
///
/// RGBA input is flattened onto white first: JPEG has no alpha channel, and
/// the C++ pipeline only ever feeds opaque or expanded pixels to its writer.
/// Grayscale stays grayscale; RGB encodes as YCbCr with the requested
/// subsampling.
pub fn encode(frame: &Frame, options: &JpegEncodeOptions) -> Result<Vec<u8>> {
    encode_impl(frame, options)
}

/// Encodes with the input's own chroma sampling preserved (`RETAIN`).
///
/// The sampling factors come from a header scan of the original bytes; unknown
/// or exotic factors fall back to 4:2:0, matching the C++ default.
pub fn encode_retaining_sampling(
    frame: &Frame,
    original: &[u8],
    options: &JpegEncodeOptions,
) -> Result<Vec<u8>> {
    let mut resolved = options.clone();
    resolved.sampling = ColorSampling::Yuv420;
    if let Ok(info) = scan_info(original) {
        if let Some(&first) = info.sampling_factors.first() {
            resolved.sampling = match first {
                (1, 1) => ColorSampling::Yuv444,
                (2, 1) => ColorSampling::Yuv422,
                (2, 2) => ColorSampling::Yuv420,
                _ => ColorSampling::Yuv420,
            };
        }
    }
    encode_impl(frame, &resolved)
}

fn encode_impl(frame: &Frame, options: &JpegEncodeOptions) -> Result<Vec<u8>> {
    use jpeg_encoder::{ColorType as JpegColorType, Encoder, SamplingFactor};
    limits::check_dimensions(ErrorSource::JpegWriter, frame.width(), frame.height())?;
    let quality = options.quality.clamp(1, 100);
    // Borrow the output buffer: `encode` consumes the encoder, so passing
    // `&mut out` (which also implements the writer trait) keeps ownership here.
    let mut out = Vec::new();
    let mut encoder = Encoder::new(&mut out, quality);
    encoder.set_optimized_huffman_tables(options.optimized_huffman);
    if options.progressive {
        if options.num_scans > 0 {
            encoder.set_progressive_scans(options.num_scans.clamp(2, 64) as u8);
        } else {
            encoder.set_progressive(true);
        }
    }
    encoder.set_sampling_factor(match options.sampling {
        ColorSampling::Yuv420 | ColorSampling::Retain => SamplingFactor::F_2_2,
        ColorSampling::Yuv422 => SamplingFactor::F_2_1,
        ColorSampling::Yuv444 => SamplingFactor::F_1_1,
    });
    for (app, payload) in &options.app_segments {
        encoder
            .add_app_segment(*app, payload.clone())
            .map_err(|err| {
                Error::parse(ErrorSource::JpegWriter, format!("bad APP segment: {err}"))
            })?;
    }
    let owned;
    let (pixels, color): (&[u8], JpegColorType) = match frame.format() {
        PixelFormat::Gray8 => (frame.pixels(), JpegColorType::Luma),
        PixelFormat::Rgb888 => (frame.pixels(), JpegColorType::Rgb),
        PixelFormat::Rgba8888 => {
            owned = flatten_rgba_onto_white(frame.pixels());
            (&owned, JpegColorType::Rgb)
        }
        PixelFormat::Unsupported => {
            return Err(Error::invocation(
                ErrorSource::JpegWriter,
                "cannot encode an unsupported pixel format",
            ));
        }
    };
    encoder
        .encode(pixels, frame.width() as u16, frame.height() as u16, color)
        .map_err(|err| {
            Error::new(
                crate::error::ErrorKind::MemoryError,
                ErrorSource::JpegWriter,
                format!("jpeg encode failed: {err}"),
            )
        })?;
    Ok(out)
}

fn flatten_rgba_onto_white(rgba: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(rgba.len() * 3 / 4);
    for px in rgba.as_chunks::<4>().0.iter() {
        let alpha = u16::from(px[3]);
        // out = src * a/255 + 255 * (1 - a/255), rounded.
        for &channel in &px[..3] {
            let blended = (u16::from(channel) * alpha + 255 * (255 - alpha) + 127) / 255;
            rgb.push(blended.min(255) as u8);
        }
    }
    rgb
}

/// Lossless metadata strip: rebuilds the JPEG byte-identical except that all
/// APPn and COM segments are dropped.
///
/// This is the pixel-identical half of `OptimizeLossless` (coefficient copy).
/// Huffman-table re-optimization — the other half — needs entropy
/// re-encoding, which is queued as a follow-up (see `docs/PLAN.md` Step 3);
/// inputs without metadata round-trip byte-identical here.
pub fn strip_metadata(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() < 2 || bytes[0] != 0xff || bytes[1] != 0xd8 {
        return Err(Error::parse(ErrorSource::JpegReader, "missing SOI marker"));
    }
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&bytes[..2]);
    let mut pos = 2;
    while pos < bytes.len() {
        if bytes[pos] != 0xff {
            return Err(Error::parse(
                ErrorSource::JpegReader,
                format!("expected marker at offset {pos}"),
            ));
        }
        let marker_start = pos;
        while pos < bytes.len() && bytes[pos] == 0xff {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }
        let marker = bytes[pos];
        pos += 1;
        if marker == 0xd9 {
            out.extend_from_slice(&bytes[marker_start..pos]);
            break;
        }
        if marker == 0x01 || (0xd0..=0xd8).contains(&marker) {
            out.extend_from_slice(&bytes[marker_start..pos]);
            continue;
        }
        let len = u16be(bytes, pos)
            .ok_or_else(|| Error::parse(ErrorSource::JpegReader, "truncated marker length"))?
            as usize;
        if len < 2 || pos + len > bytes.len() {
            return Err(Error::parse(
                ErrorSource::JpegReader,
                format!("marker 0xFF{marker:02X} overruns input"),
            ));
        }
        let keep = !((0xe0..=0xef).contains(&marker) || marker == 0xfe);
        if keep {
            out.extend_from_slice(&bytes[marker_start..pos + len]);
        }
        pos += len;
        // SOS is followed by scan data, which is always kept verbatim.
        if marker == 0xda {
            let scan_end = skip_scan_data(bytes, pos)?;
            out.extend_from_slice(&bytes[pos..scan_end]);
            pos = scan_end;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zigzag_table_is_a_permutation() {
        let mut seen = [false; 64];
        for &index in &ZIGZAG {
            assert!(!seen[index], "duplicate zigzag index {index}");
            seen[index] = true;
        }
    }

    #[test]
    fn cmyk_naive_conversion() {
        // White in inverted CMYK (all zeros after 255-x of 255s)... check the
        // math directly: full ink (c=0,k=0 inverted) -> black.
        assert_eq!(cmyk_add(0, 0), 0);
        // No ink (c=255,k=255 inverted) -> white.
        assert_eq!(cmyk_add(255, 255), 255);
    }

    #[test]
    fn flatten_handles_edges() {
        assert_eq!(flatten_rgba_onto_white(&[10, 20, 30, 255]), &[10, 20, 30]);
        assert_eq!(flatten_rgba_onto_white(&[10, 20, 30, 0]), &[255, 255, 255]);
    }
}
