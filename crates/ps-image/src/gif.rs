//! GIF parsing, disposal compositing, and animation iteration.
//!
//! Ports `GifFrameReader` (with the default `QUIRKS_CHROME` behavior) and the
//! single-frame rendering behind `PngReaderInterface GifReader`. Pixel
//! decoding (LZW, deinterlacing, palette expansion) delegates to the `gif`
//! crate with RGBA color output; disposal, canvas management, and the Chrome
//! canvas-resize quirk are implemented here.
//!
//! The animation API is a pull iterator ([`Frames`]) yielding owned
//! full-canvas composites one frame at a time, so animated GIFs never hold
//! every frame in memory at once — the frame-at-a-time analogue of the C++
//! scanline streaming.

use std::io::Cursor;

use crate::error::{Error, ErrorSource, Result};
use crate::frame::{Frame, PixelFormat, ALPHA_TRANSPARENT};
use crate::limits;

/// GIF disposal method (`FrameSpec::DisposalMethod`, GIF values 0..=3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Disposal {
    /// No disposal specified; behaves like [`Disposal::Keep`].
    #[default]
    Unknown,
    /// Leave the canvas as-is.
    Keep,
    /// Clear the frame rect to transparent before the next frame.
    Background,
    /// Restore the canvas snapshot from before this frame.
    Restore,
}

impl Disposal {
    fn from_gif(method: gif::DisposalMethod) -> Self {
        match method {
            gif::DisposalMethod::Any => Disposal::Unknown,
            gif::DisposalMethod::Keep => Disposal::Keep,
            gif::DisposalMethod::Background => Disposal::Background,
            gif::DisposalMethod::Previous => Disposal::Restore,
        }
    }
}

/// One decoded GIF frame: full-canvas composite plus timing.
#[derive(Debug, Clone)]
pub struct AnimatedFrame {
    /// Full-canvas RGBA composite with this frame applied.
    pub canvas: Frame,
    /// Frame delay in milliseconds (centiseconds × 10).
    pub delay_ms: u32,
    /// Disposal to apply before rendering the NEXT frame.
    pub disposal: Disposal,
}

/// Animation header: canvas size and total play count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GifHeader {
    pub width: u32,
    pub height: u32,
    /// Total number of plays (`ImageSpec::loop_count`): 1 when no Netscape
    /// extension is present, otherwise the repeat value + 1 (the Chrome
    /// quirk; note a repeat-0 "infinite" GIF reports 1, like the C++).
    pub loop_plays: u32,
}

/// Pull iterator over an animated GIF's composited frames.
pub struct Frames<'a> {
    decoder: gif::Decoder<Cursor<&'a [u8]>>,
    width: u32,
    height: u32,
    loop_plays: u32,
    /// Current canvas, RGBA. Grows under the Chrome resize quirk.
    canvas: Vec<u8>,
    /// Snapshot for [`Disposal::Restore`], taken before the frame that
    /// requests it.
    restore_snapshot: Option<Vec<u8>>,
    /// Disposal left behind by the previous frame.
    pending_disposal: Disposal,
    /// Rect of the previous frame, for disposal application.
    pending_rect: (u32, u32, u32, u32),
    /// Snapshot from before the previous frame (for Restore).
    previous_snapshot: Option<Vec<u8>>,
    frames_yielded: u32,
    done: bool,
}

impl<'a> Frames<'a> {
    /// Opens a GIF for frame iteration.
    pub fn open(bytes: &'a [u8]) -> Result<Self> {
        if crate::format::sniff(bytes) != crate::format::ImageType::Gif {
            return Err(Error::unsupported_format("not a GIF image"));
        }
        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        let decoder = options.read_info(Cursor::new(bytes)).map_err(|err| {
            Error::parse(ErrorSource::GifReader, format!("gif decode failed: {err}"))
        })?;
        let (width, height) = (u32::from(decoder.width()), u32::from(decoder.height()));
        // A zero-size canvas is legal here: the Chrome quirk grows it to the
        // first frame (zero_size_animation.gif). Nonzero canvases obey caps.
        if width != 0 && height != 0 {
            limits::check_dimensions(ErrorSource::GifReader, width, height)?;
        }
        let loop_plays = match decoder.repeat() {
            gif::Repeat::Infinite => 1,
            // The C++ stores the raw repeat value, then the Chrome quirk
            // adds one to report total plays.
            gif::Repeat::Finite(repeats) => u32::from(repeats) + 1,
        };
        let canvas = vec![0; width as usize * height as usize * 4];
        Ok(Frames {
            decoder,
            width,
            height,
            loop_plays,
            canvas,
            restore_snapshot: None,
            pending_disposal: Disposal::Unknown,
            pending_rect: (0, 0, 0, 0),
            previous_snapshot: None,
            frames_yielded: 0,
            done: false,
        })
    }

    /// Canvas header. Note the canvas can still grow under the Chrome quirk
    /// when the first frame exceeds it; check [`AnimatedFrame::canvas`]
    /// dimensions per frame.
    #[must_use]
    pub fn header(&self) -> GifHeader {
        GifHeader {
            width: self.width,
            height: self.height,
            loop_plays: self.loop_plays,
        }
    }

    /// Applies the previous frame's disposal, renders the next frame, and
    /// returns the new full-canvas composite. Returns `Ok(None)` at the
    /// trailer (or on a second call after the trailer).
    pub fn next_frame(&mut self) -> Result<Option<AnimatedFrame>> {
        if self.done {
            return Ok(None);
        }
        // Materialize the owned pixels before compositing: the decoder lends
        // the frame buffer only until the next call.
        struct RawFrame {
            left: u32,
            top: u32,
            width: u32,
            height: u32,
            delay_cs: u16,
            disposal: Disposal,
            pixels: Vec<u8>,
        }
        let raw: Option<RawFrame> = match self.decoder.read_next_frame() {
            Ok(Some(frame)) => {
                let mut pixels = frame.buffer.to_vec();
                // Without a declared transparent index, out-of-range palette
                // indices must decode to opaque black (the C++ expands them
                // to (0,0,0) in alpha-less output). The RGBA converter leaves
                // them as transparent black, so force opacity: nothing else
                // can produce alpha 0 when no transparent index exists.
                if frame.transparent.is_none() {
                    for alpha in pixels.iter_mut().skip(3).step_by(4) {
                        *alpha = crate::frame::ALPHA_OPAQUE;
                    }
                }
                Some(RawFrame {
                    left: u32::from(frame.left),
                    top: u32::from(frame.top),
                    width: u32::from(frame.width),
                    height: u32::from(frame.height),
                    delay_cs: frame.delay,
                    disposal: Disposal::from_gif(frame.dispose),
                    pixels,
                })
            }
            Ok(None) => None,
            Err(err) => {
                return Err(Error::parse(
                    ErrorSource::GifReader,
                    format!("gif frame decode failed: {err}"),
                ));
            }
        };
        let Some(raw) = raw else {
            self.done = true;
            return Ok(None);
        };

        // Chrome quirk: a frame larger than the canvas grows the canvas
        // (and clears the background to transparent, which it already is).
        let need_width = raw.left.saturating_add(raw.width);
        let need_height = raw.top.saturating_add(raw.height);
        if need_width > self.width || need_height > self.height {
            self.width = self.width.max(need_width);
            self.height = self.height.max(need_height);
            limits::check_dimensions(ErrorSource::GifReader, self.width, self.height)?;
            self.canvas = vec![0; self.width as usize * self.height as usize * 4];
            self.previous_snapshot = None;
            self.restore_snapshot = None;
        }
        if self.width == 0 || self.height == 0 {
            return Err(Error::parse(
                ErrorSource::GifReader,
                "GIF has an empty canvas and no resizable frame",
            ));
        }

        // Apply the previous frame's disposal.
        match self.pending_disposal {
            Disposal::Background => {
                clear_rect(
                    &mut self.canvas,
                    (self.width, self.height),
                    self.pending_rect,
                );
            }
            Disposal::Restore => {
                if let Some(snapshot) = self.previous_snapshot.take() {
                    self.canvas = snapshot;
                }
            }
            Disposal::Unknown | Disposal::Keep => {}
        }

        // Snapshot for a future Restore, taken before this frame renders.
        if raw.disposal == Disposal::Restore {
            self.restore_snapshot = Some(self.canvas.clone());
        } else {
            self.restore_snapshot = None;
        }

        // Render the frame rect, skipping fully transparent source pixels so
        // earlier canvas content shows through (the RGBA converter marks the
        // transparent index as alpha 0).
        blit_rect(
            &mut self.canvas,
            (self.width, self.height),
            (raw.left, raw.top, raw.width, raw.height),
            &raw.pixels,
        );

        self.pending_disposal = raw.disposal;
        self.pending_rect = (raw.left, raw.top, raw.width, raw.height);
        self.previous_snapshot = self.restore_snapshot.clone();
        self.frames_yielded += 1;

        let canvas = Frame::from_parts(
            self.width,
            self.height,
            PixelFormat::Rgba8888,
            self.canvas.clone(),
        )?;
        Ok(Some(AnimatedFrame {
            canvas,
            delay_ms: u32::from(raw.delay_cs) * 10,
            disposal: raw.disposal,
        }))
    }
}

/// Renders the first frame composited on a transparent canvas.
///
/// This is the `GifReader` (PNG-interface) contract used for GIF → PNG/JPEG
/// conversion: it renders frame zero even for animated inputs. Callers that
/// need the scanline-adapter contract (animated GIFs rejected) check
/// [`is_animated`] first.
pub fn first_frame(bytes: &[u8]) -> Result<Frame> {
    let mut frames = Frames::open(bytes)?;
    match frames.next_frame()? {
        Some(frame) => Ok(frame.canvas),
        None => Err(Error::parse(ErrorSource::GifReader, "GIF has no frames")),
    }
}

/// True when the GIF carries more than one frame. Reads at most two frames.
pub fn is_animated(bytes: &[u8]) -> Result<bool> {
    let mut frames = Frames::open(bytes)?;
    if frames.next_frame()?.is_none() {
        return Err(Error::parse(ErrorSource::GifReader, "GIF has no frames"));
    }
    Ok(frames.next_frame()?.is_some())
}

/// Canvas dimensions and play count without decoding any frame pixels.
pub fn probe(bytes: &[u8]) -> Result<GifHeader> {
    Ok(Frames::open(bytes)?.header())
}

fn clear_rect(canvas: &mut [u8], canvas_size: (u32, u32), rect: (u32, u32, u32, u32)) {
    let (canvas_width, _) = canvas_size;
    let (left, top, width, height) = rect;
    let right = left.saturating_add(width).min(canvas_width);
    let bottom = top.saturating_add(height).min(canvas_size.1);
    for y in top..bottom {
        let row_start = (y as usize * canvas_width as usize + left as usize) * 4;
        let row_end = (y as usize * canvas_width as usize + right as usize) * 4;
        canvas[row_start..row_end].fill(ALPHA_TRANSPARENT);
    }
}

fn blit_rect(
    canvas: &mut [u8],
    canvas_size: (u32, u32),
    rect: (u32, u32, u32, u32),
    pixels: &[u8],
) {
    let (canvas_width, canvas_height) = canvas_size;
    let (left, top, width, height) = rect;
    let right = left.saturating_add(width).min(canvas_width);
    let bottom = top.saturating_add(height).min(canvas_height);
    for (row, y) in (top..bottom).enumerate() {
        let copy_width = (right - left) as usize;
        let src_row = row * width as usize * 4;
        let dst_row = (y as usize * canvas_width as usize + left as usize) * 4;
        for x in 0..copy_width {
            let src = src_row + x * 4;
            let dst = dst_row + x * 4;
            // Transparent source pixels leave the canvas untouched; anything
            // else replaces it (GIF has no partial alpha... except the RGBA
            // converter only ever emits 0 or 255).
            if pixels.get(src + 3).copied().unwrap_or(0) != ALPHA_TRANSPARENT {
                if let Some(src_px) = pixels.get(src..src + 4) {
                    canvas[dst..dst + 4].copy_from_slice(src_px);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disposal_mapping() {
        assert_eq!(
            Disposal::from_gif(gif::DisposalMethod::Any),
            Disposal::Unknown
        );
        assert_eq!(
            Disposal::from_gif(gif::DisposalMethod::Keep),
            Disposal::Keep
        );
        assert_eq!(
            Disposal::from_gif(gif::DisposalMethod::Background),
            Disposal::Background
        );
        assert_eq!(
            Disposal::from_gif(gif::DisposalMethod::Previous),
            Disposal::Restore
        );
    }

    #[test]
    fn blit_skips_transparent_pixels() {
        let mut canvas = vec![9u8; 4 * 4];
        let pixels = [1, 2, 3, 255, 0, 0, 0, 0];
        blit_rect(&mut canvas, (2, 1), (0, 0, 2, 1), &pixels);
        assert_eq!(&canvas[..4], &[1, 2, 3, 255]);
        assert_eq!(&canvas[4..8], &[9, 9, 9, 9]);
    }
}
