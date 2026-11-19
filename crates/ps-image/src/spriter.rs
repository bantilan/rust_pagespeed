//! Image spriting (`net/instaweb/spriter`).
//!
//! Vertical-strip layout plus RGBA canvas compositing ("over"
//! blending). Only PNG and GIF inputs sprite (GIFs contribute
//! their first frame); the canvas always encodes as PNG.
#![forbid(unsafe_code)]

use crate::error::{Error, ErrorSource, Result};
use crate::frame::{Frame, PixelFormat};

/// Placed clip rectangle (`spriter::Rect`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacedRect {
    /// Left edge in the canvas.
    pub x: i32,
    /// Top edge in the canvas.
    pub y: i32,
    /// Image width.
    pub width: i32,
    /// Image height.
    pub height: i32,
}

/// Pastes a sprite onto a canvas (`DrawImage`) with "over" alpha
/// compositing. Both frames must be RGBA; out-of-canvas pixels clip.
pub fn draw_image(canvas: &mut Frame, sprite: &Frame, x: u32, y: u32) -> Result<()> {
    if canvas.format() != PixelFormat::Rgba8888 || sprite.format() != PixelFormat::Rgba8888 {
        return Err(Error::invocation(
            ErrorSource::Util,
            "sprite compositing needs RGBA frames",
        ));
    }
    let (canvas_width, canvas_height) = (canvas.width(), canvas.height());
    let (sprite_width, sprite_height) = (sprite.width(), sprite.height());
    for row in 0..sprite_height {
        let canvas_y = y + row;
        if canvas_y >= canvas_height {
            break;
        }
        for col in 0..sprite_width {
            let canvas_x = x + col;
            if canvas_x >= canvas_width {
                break;
            }
            let src = &sprite.pixels()
                [(row * sprite_width + col) as usize * 4..][..4];
            let dst = &mut canvas.pixels_mut()
                [(canvas_y * canvas_width + canvas_x) as usize * 4..][..4];
            let alpha = u32::from(src[3]);
            if alpha == 255 {
                dst.copy_from_slice(src);
            } else if alpha != 0 {
                // Porter-Duff "over" onto the (opaque or transparent)
                // canvas pixel.
                for channel in 0..3 {
                    let blended = (u32::from(src[channel]) * alpha
                        + u32::from(dst[channel]) * (255 - alpha)
                        + 127)
                        / 255;
                    dst[channel] = blended.min(255) as u8;
                }
                let dst_alpha = u32::from(dst[3]);
                dst[3] = (alpha + dst_alpha * (255 - alpha) / 255).min(255) as u8;
            }
        }
    }
    Ok(())
}

/// Lays images out in a vertical strip and composites them
/// (`DrawImagesInVerticalStrip`): each image at x=0, stacked
/// top-to-bottom; the canvas is max(width) by sum(heights).
/// Returns the canvas plus one clip rect per input, in order.
pub fn compose_vertical_strip(images: &[Frame]) -> Result<(Frame, Vec<PlacedRect>)> {
    let mut max_width = 0;
    let mut total_height = 0;
    let mut rects = Vec::with_capacity(images.len());
    for image in images {
        let (width, height) = (image.width(), image.height());
        rects.push(PlacedRect {
            x: 0,
            y: total_height as i32,
            width: width as i32,
            height: height as i32,
        });
        max_width = max_width.max(width);
        total_height += height;
    }
    let transparent = [0, 0, 0, 0];
    let mut canvas = Frame::solid(max_width, total_height, PixelFormat::Rgba8888, &transparent)?;
    for (image, rect) in images.iter().zip(rects.iter()) {
        let rgba = image.expand(PixelFormat::Rgba8888)?;
        draw_image(&mut canvas, &rgba, rect.x as u32, rect.y as u32)?;
    }
    Ok((canvas, rects))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_frame(width: u32, height: u32, pixel: [u8; 4]) -> Frame {
        Frame::solid(width, height, PixelFormat::Rgba8888, &pixel).unwrap()
    }

    #[test]
    fn vertical_strip_layout_and_paste() {
        let red = solid_frame(10, 20, [255, 0, 0, 255]);
        let green = solid_frame(6, 8, [0, 255, 0, 255]);
        let (canvas, rects) = compose_vertical_strip(&[red, green]).unwrap();
        assert_eq!((canvas.width(), canvas.height()), (10, 28));
        assert_eq!(
            rects,
            vec![
                PlacedRect { x: 0, y: 0, width: 10, height: 20 },
                PlacedRect { x: 0, y: 20, width: 6, height: 8 },
            ]
        );
        // Top image paints the top rows; second image paints below it.
        assert_eq!(&canvas.row(0)[0..4], &[255, 0, 0, 255]);
        assert_eq!(&canvas.row(19)[0..4], &[255, 0, 0, 255]);
        assert_eq!(&canvas.row(20)[0..4], &[0, 255, 0, 255]);
        assert_eq!(&canvas.row(27)[0..4], &[0, 255, 0, 255]);
        // Unpainted canvas stays transparent.
        assert_eq!(&canvas.row(20)[8 * 4..9 * 4], &[0, 0, 0, 0]);
    }

    #[test]
    fn paste_blends_alpha() {
        let mut canvas = solid_frame(2, 1, [0, 0, 255, 255]);
        let half_red = solid_frame(1, 1, [255, 0, 0, 128]);
        draw_image(&mut canvas, &half_red, 0, 0).unwrap();
        let pixel = &canvas.row(0)[0..4];
        assert_eq!(pixel[3], 255);
        assert!(pixel[0] > 100 && pixel[2] > 100, "{pixel:?}");
        // Untouched pixel stays blue.
        assert_eq!(&canvas.row(0)[4..8], &[0, 0, 255, 255]);
    }

    #[test]
    fn paste_rejects_non_rgba() {
        let mut canvas = solid_frame(2, 2, [0, 0, 0, 255]);
        let rgb = Frame::solid(1, 1, PixelFormat::Rgb888, &[1, 2, 3]).unwrap();
        assert!(draw_image(&mut canvas, &rgb, 0, 0).is_err());
    }
}
