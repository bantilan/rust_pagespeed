//! PNG parsing, decoding, and encoding.
//!
//! Ports the `PngScanlineReaderRaw` transform chain (16-to-8-bit strip,
//! sub-8-bit expansion, palette/tRNS expansion, gray-alpha to RGBA), the
//! `PngScanlineWriter` encode side, and the `HasTransparencyForPng` chunk
//! probe from `net/instaweb/rewriter/image.cc`.
//!
//! Inflate/deflate and Adam7 deinterlacing delegate to the pure-Rust `png`
//! crate; pixel expansion follows the libpng transform sequence above so
//! decoded frames match the C++ reader pixel-for-pixel.

use std::io::Cursor;

use crate::error::{Error, ErrorSource, Result};
use crate::frame::{Frame, PixelFormat, ALPHA_OPAQUE, ALPHA_TRANSPARENT};
use crate::limits;

/// PNG filter selection for encoding (`PngCompressParams::filter_level`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PngFilter {
    None,
    Sub,
    Up,
    Average,
    Paeth,
    /// Try all filters per row (libpng `PNG_ALL_FILTERS` heuristic).
    #[default]
    Adaptive,
    /// Brute-force minimum-entropy filter per row (closest to
    /// libpng `PNG_ALL_FILTERS` exhaustive search).
    MinEntropy,
}

impl PngFilter {
    pub(crate) fn to_png_crate(self) -> png::Filter {
        match self {
            PngFilter::None => png::Filter::NoFilter,
            PngFilter::Sub => png::Filter::Sub,
            PngFilter::Up => png::Filter::Up,
            PngFilter::Average => png::Filter::Avg,
            PngFilter::Paeth => png::Filter::Paeth,
            PngFilter::Adaptive => png::Filter::Adaptive,
            PngFilter::MinEntropy => png::Filter::MinEntropy,
        }
    }
}

/// PNG compression level for encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PngCompression {
    /// Stored uncompressed (`Z_NO_COMPRESSION`); also forces filter none.
    None,
    Fast,
    #[default]
    Default,
    Best,
}

impl PngCompression {
    pub(crate) fn to_png_crate(self) -> png::Compression {
        match self {
            PngCompression::None => png::Compression::NoCompression,
            PngCompression::Fast => png::Compression::Fast,
            PngCompression::Default => png::Compression::default(),
            PngCompression::Best => png::Compression::High,
        }
    }
}

/// PNG encode options, mirroring `PngCompressParams` (minus the
/// best-compression trial flag, which lives in [`crate::optimize`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PngEncodeOptions {
    pub filter: PngFilter,
    pub compression: PngCompression,
    /// Emit Adam7 interlaced output (`is_progressive`). The C++ passes false
    /// at every call site ("never use progressive format"), and the `png`
    /// crate cannot emit interlaced files, so requesting it fails cleanly.
    pub interlaced: bool,
}

/// Header metadata from the IHDR chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PngInfo {
    pub width: u32,
    pub height: u32,
    /// Adam7 interlacing (`IsProgressive` in the C++ reader).
    pub interlaced: bool,
    /// Raw IHDR color type byte (0, 2, 3, 4, or 6).
    pub color_type: u8,
    /// Raw IHDR bit depth.
    pub bit_depth: u8,
}

/// Reads the IHDR chunk directly: 8-byte magic, length, "IHDR", then
/// width/height/color metadata. Used where full decode is overkill.
pub fn scan_info(bytes: &[u8]) -> Result<PngInfo> {
    if bytes.len() < 33 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return Err(Error::parse(ErrorSource::PngReader, "bad PNG header"));
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    let bit_depth = bytes[24];
    let color_type = bytes[25];
    let interlaced = bytes[28] == 1;
    limits::check_dimensions(ErrorSource::PngReader, width, height)?;
    Ok(PngInfo {
        width,
        height,
        interlaced,
        color_type,
        bit_depth,
    })
}

/// Conservative transparency probe, port of `HasTransparencyForPng`.
///
/// Returns true when the color type carries alpha, when a nonempty tRNS chunk
/// appears before the first IDAT, or when the chunk walk cannot prove
/// otherwise (truncation, unknown layout). Only a clean bill — no alpha bit
/// and no tRNS before IDAT — returns false.
pub fn has_transparency_probe(bytes: &[u8]) -> bool {
    if bytes.len() <= 25 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return true;
    }
    // No alpha bit (0x4) in the IHDR color type: tRNS is the only remaining
    // transparency source, and it must precede the first IDAT.
    if bytes[25] & 0x04 != 0 {
        return true;
    }
    let mut section_start = 8;
    while section_start + 8 < bytes.len() {
        let Some(len_bytes) = bytes.get(section_start..section_start + 4) else {
            return true;
        };
        let section_size =
            u32::from_be_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]) as usize;
        let Some(id) = bytes.get(section_start + 4..section_start + 8) else {
            return true;
        };
        if id == b"IDAT" {
            return false;
        }
        if id == b"tRNS" && section_size > 0 {
            return true;
        }
        // Length + id + payload + CRC.
        section_start = match section_start
            .checked_add(section_size)
            .and_then(|end| end.checked_add(12))
        {
            Some(next) => next,
            None => return true,
        };
    }
    true
}

/// Decodes a PNG into a frame, applying the C++ raw-reader transform chain.
pub fn decode(bytes: &[u8]) -> Result<Frame> {
    let info = scan_info(bytes)?;
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    // Match `png_set_user_limits`: refuse huge IDAT allocations early.
    decoder.set_limits(png::Limits {
        bytes: (limits::MAX_IMAGE_PIXELS * 4).min(usize::MAX as u64) as usize,
    });
    let mut reader = decoder
        .read_info()
        .map_err(|err| Error::parse(ErrorSource::PngReader, format!("png decode failed: {err}")))?;
    let buffer_size = reader
        .output_buffer_size()
        .ok_or_else(|| Error::parse(ErrorSource::PngReader, "cannot size PNG output buffer"))?;
    let mut raw = vec![0u8; buffer_size];
    let output = reader
        .next_frame(&mut raw)
        .map_err(|err| Error::parse(ErrorSource::PngReader, format!("png decode failed: {err}")))?;
    raw.truncate(output.buffer_size());
    let png_info = reader.info();
    expand_pixels(&raw, info.width, info.height, png_info)
}

/// Expands raw PNG rows to [`PixelFormat`] pixels following the libpng
/// transform order: strip 16, expand sub-8-bit/tRNS, gray-to-RGB, palette.
fn expand_pixels(raw: &[u8], width: u32, height: u32, info: &png::Info) -> Result<Frame> {
    use png::{BitDepth as Depth, ColorType as CType};
    let depth_bits = match info.bit_depth {
        Depth::One => 1,
        Depth::Two => 2,
        Depth::Four => 4,
        Depth::Eight => 8,
        Depth::Sixteen => 16,
    };
    let channels: usize = match info.color_type {
        CType::Grayscale => 1,
        CType::GrayscaleAlpha => 2,
        CType::Rgb => 3,
        CType::Rgba => 4,
        CType::Indexed => 1,
    };
    // Bits per row, padded up to whole bytes (sub-8-bit samples pack).
    let bits_per_row = width as usize * channels * depth_bits;
    let bytes_per_row = bits_per_row.div_ceil(8);
    if raw.len() < bytes_per_row * height as usize {
        return Err(Error::parse(
            ErrorSource::PngReader,
            "truncated PNG pixel rows",
        ));
    }

    // 16-bit: strip to the high byte, like `png_set_strip_16`.
    let row_bytes = bytes_per_row;
    let eight_bit: Vec<u8>;
    let rows: &[u8] = if depth_bits == 16 {
        eight_bit = raw
            .chunks_exact(row_bytes)
            .take(height as usize)
            .flat_map(|row| row.as_chunks::<2>().0.iter().map(|pair| pair[0]))
            .collect();
        &eight_bit
    } else {
        raw
    };
    let stride = if depth_bits == 16 {
        width as usize * channels
    } else {
        row_bytes
    };

    let has_trns = info.trns.as_ref().is_some_and(|trns| !trns.is_empty());
    let (format, pixels) = match (info.color_type, depth_bits) {
        (CType::Grayscale, 8 | 16) if !has_trns => {
            let mut pixels = Vec::with_capacity(width as usize * height as usize);
            for row in rows.chunks_exact(stride).take(height as usize) {
                pixels.extend_from_slice(&row[..width as usize]);
            }
            (PixelFormat::Gray8, pixels)
        }
        (CType::Grayscale, 1 | 2 | 4) if !has_trns => {
            // `png_set_expand` scales sub-8-bit gray to full range.
            let scale_up = |v: u8| -> u8 {
                match depth_bits {
                    1 => v * 255,
                    2 => v * 85,
                    _ => v * 17,
                }
            };
            let mut pixels = Vec::with_capacity(width as usize * height as usize);
            for row in rows.chunks_exact(stride).take(height as usize) {
                let mut bit_pos = 0;
                for _ in 0..width {
                    let byte = row[bit_pos / 8];
                    let shift = 8 - depth_bits - (bit_pos % 8);
                    let mask = (1u8 << depth_bits) - 1;
                    pixels.push(scale_up((byte >> shift) & mask));
                    bit_pos += depth_bits;
                }
            }
            (PixelFormat::Gray8, pixels)
        }
        (CType::Grayscale, _) if has_trns => {
            // Gray + tRNS becomes gray-alpha, then RGBA via gray-to-RGB.
            // The tRNS sample holds a full-depth value (0..2^bitdepth - 1),
            // so it compares against the raw sample, not the expanded one.
            let trns = info.trns.as_deref().unwrap_or(&[]);
            let transparent: u16 = if trns.len() >= 2 {
                u16::from_be_bytes([trns[0], trns[1]])
            } else if trns.len() == 1 {
                u16::from(trns[0])
            } else {
                u16::MAX
            };
            let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
            for row in rows.chunks_exact(stride).take(height as usize) {
                for x in 0..width as usize {
                    let sample = gray_sample_raw(row, x, depth_bits);
                    // 16-bit pixels were stripped to their high byte, so the
                    // transparent sample compares high byte to high byte.
                    let is_transparent = if depth_bits == 16 {
                        sample == (transparent >> 8) as u8
                    } else {
                        u16::from(sample) == transparent
                    };
                    let alpha = if is_transparent {
                        ALPHA_TRANSPARENT
                    } else {
                        ALPHA_OPAQUE
                    };
                    let g = gray_sample(row, x, depth_bits);
                    pixels.extend_from_slice(&[g, g, g, alpha]);
                }
            }
            (PixelFormat::Rgba8888, pixels)
        }
        (CType::GrayscaleAlpha, _) => {
            // Native gray-alpha expands straight to RGBA.
            let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
            for row in rows.chunks_exact(stride).take(height as usize) {
                for pair in row.as_chunks::<2>().0.iter().take(width as usize) {
                    pixels.extend_from_slice(&[pair[0], pair[0], pair[0], pair[1]]);
                }
            }
            (PixelFormat::Rgba8888, pixels)
        }
        (CType::Rgb, _) if !has_trns => {
            let mut pixels = Vec::with_capacity(width as usize * height as usize * 3);
            for row in rows.chunks_exact(stride).take(height as usize) {
                pixels.extend_from_slice(&row[..width as usize * 3]);
            }
            (PixelFormat::Rgb888, pixels)
        }
        (CType::Rgb, _) => {
            // RGB + tRNS: the transparent triple becomes alpha 0. tRNS
            // samples are full-depth big-endian values; 8-bit images store
            // them in the low bytes, 16-bit pixels were stripped to the
            // high bytes, so each depth compares against its own half.
            let trns = info.trns.as_deref().unwrap_or(&[]);
            let key = if trns.len() >= 6 {
                if depth_bits == 16 {
                    [trns[0], trns[2], trns[4]]
                } else {
                    [trns[1], trns[3], trns[5]]
                }
            } else {
                [0, 0, 0]
            };
            let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
            for row in rows.chunks_exact(stride).take(height as usize) {
                for triple in row.as_chunks::<3>().0.iter().take(width as usize) {
                    let alpha = if *triple == key {
                        ALPHA_TRANSPARENT
                    } else {
                        ALPHA_OPAQUE
                    };
                    pixels.extend_from_slice(&[triple[0], triple[1], triple[2], alpha]);
                }
            }
            (PixelFormat::Rgba8888, pixels)
        }
        (CType::Rgba, _) => {
            let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
            for row in rows.chunks_exact(stride).take(height as usize) {
                pixels.extend_from_slice(&row[..width as usize * 4]);
            }
            (PixelFormat::Rgba8888, pixels)
        }
        (CType::Indexed, _) => {
            let palette = info.palette.as_deref().unwrap_or(&[]);
            let trns = info.trns.as_deref().unwrap_or(&[]);
            let with_alpha = has_trns;
            let mut pixels = Vec::with_capacity(
                width as usize * height as usize * if with_alpha { 4 } else { 3 },
            );
            for row in rows.chunks_exact(stride).take(height as usize) {
                for x in 0..width as usize {
                    let index = if depth_bits < 8 {
                        let bit_pos = x * depth_bits;
                        let byte = row[bit_pos / 8];
                        let shift = 8 - depth_bits - (bit_pos % 8);
                        ((byte >> shift) & ((1u8 << depth_bits) - 1)) as usize
                    } else {
                        row[x] as usize
                    };
                    let base = index * 3;
                    let (r, g, b) = if base + 2 < palette.len() {
                        (palette[base], palette[base + 1], palette[base + 2])
                    } else {
                        (0, 0, 0)
                    };
                    if with_alpha {
                        let alpha = trns.get(index).copied().unwrap_or(ALPHA_OPAQUE);
                        pixels.extend_from_slice(&[r, g, b, alpha]);
                    } else {
                        pixels.extend_from_slice(&[r, g, b]);
                    }
                }
            }
            (
                if with_alpha {
                    PixelFormat::Rgba8888
                } else {
                    PixelFormat::Rgb888
                },
                pixels,
            )
        }
        _ => {
            return Err(Error::internal(
                ErrorSource::PngReader,
                format!(
                    "unreachable color type/depth: {:?}/{depth_bits}",
                    info.color_type
                ),
            ));
        }
    };
    Frame::from_parts(width, height, format, pixels)
}

/// One 8-bit gray sample, scaling sub-8-bit values up like `png_set_expand`.
fn gray_sample(row: &[u8], x: usize, depth_bits: usize) -> u8 {
    let raw = gray_sample_raw(row, x, depth_bits);
    match depth_bits {
        1 => raw * 255,
        2 => raw * 85,
        4 => raw * 17,
        _ => raw,
    }
}

fn gray_sample_raw(row: &[u8], x: usize, depth_bits: usize) -> u8 {
    if depth_bits >= 8 {
        return row[x];
    }
    let bit_pos = x * depth_bits;
    let byte = row[bit_pos / 8];
    let shift = 8 - depth_bits - (bit_pos % 8);
    (byte >> shift) & ((1u8 << depth_bits) - 1)
}

/// Encodes a frame as PNG.
pub fn encode(frame: &Frame, options: &PngEncodeOptions) -> Result<Vec<u8>> {
    limits::check_dimensions(ErrorSource::PngWriter, frame.width(), frame.height())?;
    if options.interlaced {
        return Err(Error::unsupported_feature(
            ErrorSource::PngWriter,
            "interlaced PNG output is not supported",
        ));
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, frame.width(), frame.height());
    let color = match frame.format() {
        PixelFormat::Gray8 => png::ColorType::Grayscale,
        PixelFormat::Rgb888 => png::ColorType::Rgb,
        PixelFormat::Rgba8888 => png::ColorType::Rgba,
        PixelFormat::Unsupported => {
            return Err(Error::invocation(
                ErrorSource::PngWriter,
                "cannot encode an unsupported pixel format",
            ));
        }
    };
    encoder.set_color(color);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_filter(options.filter.to_png_crate());
    encoder.set_compression(options.compression.to_png_crate());
    let mut writer = encoder.write_header().map_err(|err| {
        Error::new(
            crate::error::ErrorKind::MemoryError,
            ErrorSource::PngWriter,
            format!("png header write failed: {err}"),
        )
    })?;
    writer.write_image_data(frame.pixels()).map_err(|err| {
        Error::new(
            crate::error::ErrorKind::MemoryError,
            ErrorSource::PngWriter,
            format!("png encode failed: {err}"),
        )
    })?;
    drop(writer);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_defaults_to_transparent() {
        assert!(has_transparency_probe(&[]));
        assert!(has_transparency_probe(b"not a png at all.............."));
    }

    #[test]
    fn probe_reads_color_type_and_trns() {
        // Minimal IHDR (color type 2 = RGB) with no tRNS before IDAT.
        let mut png = vec![];
        png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1u32.to_be_bytes());
        png.extend_from_slice(&1u32.to_be_bytes());
        png.extend_from_slice(&[8, 2, 0, 0, 0]);
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(b"IDAT");
        png.extend_from_slice(&0u32.to_be_bytes());
        assert!(!has_transparency_probe(&png));

        // Same, but color type 6 (RGBA): alpha bit set.
        png[25] = 6;
        assert!(has_transparency_probe(&png));

        // Color type 3 (palette) with a nonempty tRNS before IDAT.
        png[25] = 3;
        let mut trns = vec![];
        trns.extend_from_slice(&png[..33]);
        trns.extend_from_slice(&1u32.to_be_bytes());
        trns.extend_from_slice(b"tRNS");
        trns.push(0);
        trns.extend_from_slice(&0u32.to_be_bytes());
        trns.extend_from_slice(&png[33..]);
        assert!(has_transparency_probe(&trns));
    }
}
