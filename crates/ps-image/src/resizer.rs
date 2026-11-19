//! Area-method image shrinking, ported from `image_resizer.cc`.
//!
//! The floating-point operation order matches the C++ exactly (same table
//! construction in f64, same f32 accumulation order, same truncation), so
//! resized pixels are bit-identical to the C++ `ScanlineResizer`. Like the
//! C++, only shrinking is supported: requested dimensions above the input
//! are truncated, and [`PRESERVE_ASPECT_RATIO`] (0) keeps the ratio.
//!
//! One deliberate simplification: the C++ instantiates separate uint8/float
//! vertical-resizer paths, but uint8 samples convert to f32 exactly, so this
//! port always accumulates in f32 with identical results and half the code.

use crate::error::{Error, ErrorSource, Result};
use crate::frame::Frame;

/// Pass for either dimension to preserve the aspect ratio (`kPreserveAspectRatio`).
pub const PRESERVE_ASPECT_RATIO: u32 = 0;

/// Closeness threshold for snapping grid positions to integers (1.0E-10).
const CLOSE_TO_INTEGER_THRESHOLD: f64 = 1.0E-10;

/// One output row/column of the area table: blend input
/// `[first_index, last_index]` with edge weights and unit interior weights.
#[derive(Debug, Clone, Copy)]
struct TableEntry {
    first_index: usize,
    last_index: usize,
    first_weight: f32,
    last_weight: f32,
}

/// Round-half-to-even, matching `lrintf` (NOT `f64::round`, which rounds
/// half away from zero).
fn round_half_even(value: f64) -> f64 {
    value.round_ties_even()
}

fn is_approximately_integer(value: f64) -> bool {
    (value - round_half_even(value)).abs() <= CLOSE_TO_INTEGER_THRESHOLD
}

/// Interpolation coefficients for the area method (`CreateTableForAreaMethod`).
fn create_table(in_size: usize, out_size: usize, ratio: f64) -> Result<Vec<TableEntry>> {
    if in_size == 0 || out_size == 0 || ratio <= 0.0 {
        return Err(Error::invocation(
            ErrorSource::Resizer,
            "resize table inputs must be positive",
        ));
    }
    let mut table: Vec<TableEntry> = Vec::with_capacity(out_size);
    let mut end_pos = 0.0f64;
    for _ in 0..out_size {
        let start_pos = end_pos;
        let start_pos_floor = start_pos.floor();
        let first_index = start_pos_floor as usize;
        let first_weight = (1.0 + start_pos_floor - start_pos) as f32;

        end_pos = (table.len() + 1) as f64 * ratio;
        if is_approximately_integer(end_pos) {
            end_pos = round_half_even(end_pos);
        }
        let mut last_index = if is_approximately_integer(end_pos) {
            end_pos as usize - 1
        } else {
            end_pos as usize
        };
        // The aspect-preserving size rounding can push the last index past
        // the input; clamp it, like the C++.
        if last_index >= in_size {
            last_index = in_size - 1;
        }

        let last_weight = if first_index < last_index {
            (end_pos - last_index as f64) as f32
        } else {
            (ratio - f64::from(first_weight)) as f32
        };

        if let Some(previous) = table.last() {
            if previous.first_index >= first_index {
                return Err(Error::internal(
                    ErrorSource::Resizer,
                    "significant rounding error accumulated in resize table",
                ));
            }
        }
        table.push(TableEntry {
            first_index,
            last_index,
            first_weight,
            last_weight,
        });
    }
    Ok(table)
}

/// Output size and ratios (`ComputeResizedSizeRatio`).
fn resized_size_ratio(
    input_width: usize,
    input_height: usize,
    output_width: usize,
    output_height: usize,
) -> Result<(usize, usize, f64, f64)> {
    let original_width = input_width as f64;
    let original_height = input_height as f64;
    let mut resized_width = output_width as f64;
    let mut resized_height = output_height as f64;
    let (ratio_x, ratio_y);
    if resized_width > 0.0 && resized_height > 0.0 {
        ratio_x = original_width / resized_width;
        ratio_y = original_height / resized_height;
    } else if resized_width > 0.0 {
        ratio_x = original_width / resized_width;
        ratio_y = ratio_x;
        resized_height = round_half_even(original_height / ratio_y);
    } else if resized_height > 0.0 {
        ratio_x = original_height / resized_height;
        ratio_y = ratio_x;
        resized_width = round_half_even(original_width / ratio_x);
    } else {
        return Err(Error::invocation(
            ErrorSource::Resizer,
            "output width and height cannot both preserve aspect ratio",
        ));
    }
    Ok((
        resized_width as usize,
        resized_height as usize,
        ratio_x,
        ratio_y,
    ))
}

/// Horizontal area pass over one row (`ResizeRowArea*`, in f32 element units).
fn resize_row(table: &[TableEntry], channels: usize, input: &[u8], output: &mut [f32]) {
    for (out_x, entry) in table.iter().enumerate() {
        let first = entry.first_index * channels;
        let last = entry.last_index * channels;
        for channel in 0..channels {
            let mut acc = f32::from(input[first + channel]) * entry.first_weight;
            let mut index = first + channels;
            while index < last {
                acc += f32::from(input[index + channel]);
                index += channels;
            }
            acc += f32::from(input[last + channel]) * entry.last_weight;
            output[out_x * channels + channel] = acc;
        }
    }
}

/// Vertical area pass (`ResizeColArea::Resize` state machine, over f32 rows).
///
/// `rows` holds every horizontally resized row; `out_row` selects the output.
/// The accumulation order matches the C++ exactly, including the shared-row
/// carry-over between adjacent outputs.
struct ColumnResizer {
    table: Vec<TableEntry>,
    buffer: Vec<f32>,
    inv_grid_area: f32,
    half_grid_area: f32,
    only_scale_outputs: bool,
}

impl ColumnResizer {
    fn new(
        in_size: usize,
        out_size: usize,
        ratio_x: f64,
        ratio_y: f64,
        elements_per_row: usize,
    ) -> Result<Self> {
        let table = create_table(in_size, out_size, ratio_y)?;
        let only_scale_outputs = ratio_y == 1.0;
        let buffer = if only_scale_outputs {
            Vec::new()
        } else {
            vec![0.0; elements_per_row]
        };
        let grid_area = (ratio_x * ratio_y) as f32;
        Ok(ColumnResizer {
            table,
            buffer,
            inv_grid_area: 1.0 / grid_area,
            half_grid_area: 0.5 * grid_area,
            only_scale_outputs,
        })
    }

    /// Computes output row `out_row` from the resized input `rows`.
    fn resize_row_into(&mut self, rows: &[Vec<f32>], out_row: usize, output: &mut [u8]) {
        if self.only_scale_outputs {
            Self::compute_output(
                &rows[out_row],
                output,
                self.half_grid_area,
                self.inv_grid_area,
            );
            return;
        }
        let entry = self.table[out_row];
        // First row: overwrite the accumulator (AppendFirstRow).
        append_first(
            &rows[entry.first_index],
            entry.first_weight,
            &mut self.buffer,
        );
        // Middle rows: accumulate (AppendMiddleRow).
        for row in &rows[entry.first_index + 1..entry.last_index] {
            append_middle(row, &mut self.buffer);
        }
        // Last row (AppendLastRow), unless its weight is zero — in which
        // case the row contributes nothing and is skipped, like the C++.
        // Note the C++ still CONSUMES the row (in_row_ advances); since this
        // port indexes rows directly, consumption is implicit.
        if entry.last_weight > 0.0 {
            append_last(&rows[entry.last_index], entry.last_weight, &mut self.buffer);
        }
        // The C++ carry-over (re-AppendFirstRow for the next output) is an
        // optimization only: every output recomputed from scratch below
        // performs the identical arithmetic.
        let buffer = std::mem::take(&mut self.buffer);
        Self::compute_output(&buffer, output, self.half_grid_area, self.inv_grid_area);
        self.buffer = buffer;
    }

    /// Normalize by the grid area and truncate to bytes (`ComputeOutput`).
    fn compute_output(input: &[f32], output: &mut [u8], half_grid_area: f32, inv_grid_area: f32) {
        for (index, out) in output.iter_mut().enumerate().take(input.len()) {
            *out = ((input[index] + half_grid_area) * inv_grid_area) as u8;
        }
    }
}

fn append_first(input: &[f32], weight: f32, buffer: &mut [f32]) {
    for (slot, sample) in buffer.iter_mut().zip(input.iter()) {
        *slot = weight * *sample;
    }
}

fn append_middle(input: &[f32], buffer: &mut [f32]) {
    for (slot, sample) in buffer.iter_mut().zip(input.iter()) {
        *slot += *sample;
    }
}

fn append_last(input: &[f32], weight: f32, buffer: &mut [f32]) {
    for (slot, sample) in buffer.iter_mut().zip(input.iter()) {
        *slot += weight * *sample;
    }
}

/// Shrinks a frame to the requested size with the area method.
///
/// Either dimension may be [`PRESERVE_ASPECT_RATIO`] to keep the ratio;
/// requests above the input size truncate (never enlarge). The pixel format
/// is preserved.
pub fn resize(frame: &Frame, request_width: u32, request_height: u32) -> Result<Frame> {
    let input_width = frame.width() as usize;
    let input_height = frame.height() as usize;
    if input_width == 0 || input_height == 0 {
        return Err(Error::invocation(
            ErrorSource::Resizer,
            "cannot resize an empty image",
        ));
    }
    if request_width == PRESERVE_ASPECT_RATIO && request_height == PRESERVE_ASPECT_RATIO {
        return Err(Error::invocation(
            ErrorSource::Resizer,
            "output width and height cannot both preserve aspect ratio",
        ));
    }
    let channels = frame.format().channels();
    if channels == 0 {
        return Err(Error::invocation(
            ErrorSource::Resizer,
            "cannot resize an unsupported pixel format",
        ));
    }

    // Truncate enlargement requests, like ScanlineResizer::Initialize.
    let output_width = (request_width as usize).min(input_width);
    let output_height = (request_height as usize).min(input_height);
    let (resized_width, resized_height, ratio_x, ratio_y) =
        resized_size_ratio(input_width, input_height, output_width, output_height)?;

    #[allow(clippy::float_cmp)]
    let need_resize_x = ratio_x != 1.0;

    // Horizontal pass (or a u8->f32 lift when x needs no resizing).
    let table_x = if need_resize_x {
        Some(create_table(input_width, resized_width, ratio_x)?)
    } else {
        None
    };
    let mut rows: Vec<Vec<f32>> = Vec::with_capacity(input_height);
    for y in 0..input_height {
        let row_start = y * input_width * channels;
        let input_row = &frame.pixels()[row_start..row_start + input_width * channels];
        let mut out_row = vec![0.0f32; resized_width * channels];
        if let Some(table) = &table_x {
            resize_row(table, channels, input_row, &mut out_row);
        } else {
            for (slot, sample) in out_row.iter_mut().zip(input_row.iter()) {
                *slot = f32::from(*sample);
            }
        }
        rows.push(out_row);
    }

    // Vertical pass.
    let mut resizer_y = ColumnResizer::new(
        input_height,
        resized_height,
        ratio_x,
        ratio_y,
        resized_width * channels,
    )?;
    let mut pixels = vec![0u8; resized_width * resized_height * channels];
    for out_y in 0..resized_height {
        let row_start = out_y * resized_width * channels;
        let output_row = &mut pixels[row_start..row_start + resized_width * channels];
        resizer_y.resize_row_into(&rows, out_y, output_row);
    }

    Frame::from_parts(
        resized_width as u32,
        resized_height as u32,
        frame.format(),
        pixels,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::PixelFormat;

    #[test]
    fn round_matches_lrintf() {
        assert_eq!(round_half_even(2.5), 2.0);
        assert_eq!(round_half_even(3.5), 4.0);
        assert_eq!(round_half_even(16.4848), 16.0);
    }

    #[test]
    fn table_endpoints_match_cpp() {
        // 2x shrink of 32: each output takes exactly 2 inputs, unit weights.
        let table = create_table(32, 16, 2.0).unwrap();
        assert_eq!(table.len(), 16);
        assert_eq!(table[0].first_index, 0);
        assert_eq!(table[0].last_index, 1);
        assert_eq!(table[0].first_weight, 1.0);
        assert_eq!(table[0].last_weight, 1.0);
        assert_eq!(table[15].first_index, 30);
        assert_eq!(table[15].last_index, 31);
    }

    #[test]
    fn ratios_preserve_sides() {
        assert_eq!(
            resized_size_ratio(32, 32, 16, 0).unwrap(),
            (16, 16, 2.0, 2.0)
        );
        assert_eq!(resized_size_ratio(33, 34, 16, 0).unwrap().0, 16);
        assert_eq!(resized_size_ratio(33, 34, 16, 0).unwrap().1, 16);
        assert!(resized_size_ratio(32, 32, 0, 0).is_err());
    }

    #[test]
    fn solid_shrink_stays_solid() {
        let solid = Frame::solid(32, 32, PixelFormat::Rgb888, &[10, 20, 30]).unwrap();
        let small = resize(&solid, 3, 3).unwrap();
        assert_eq!((small.width(), small.height()), (3, 3));
        assert!(small
            .pixels()
            .as_chunks::<3>()
            .0
            .iter()
            .all(|px| *px == [10, 20, 30]));
    }

    #[test]
    fn upscale_requests_truncate() {
        let solid = Frame::solid(8, 8, PixelFormat::Gray8, &[7]).unwrap();
        let same = resize(&solid, 32, 32).unwrap();
        assert_eq!((same.width(), same.height()), (8, 8));
    }
}
