//! Renders an RGB [`Frame`] to colored ASCII art sized to the terminal.

use std::fmt::Write as _;

use crate::video::Frame;

/// Dark-to-bright character ramp.
const RAMP: &[u8] = b" .:-=+*#%@";

/// A terminal character cell is roughly twice as tall as it is wide, so we
/// stretch the horizontal sampling to keep the picture's aspect ratio.
const CHAR_ASPECT: f64 = 2.0;

/// Computes the ASCII grid (cols, rows) that best fits `term_cols`x`term_rows`
/// while preserving the image's aspect ratio.
pub fn fit_grid(img_w: usize, img_h: usize, term_cols: usize, term_rows: usize) -> (usize, usize) {
    if img_w == 0 || img_h == 0 || term_cols == 0 || term_rows == 0 {
        return (1, 1);
    }
    let ratio = (img_w as f64 / img_h as f64) * CHAR_ASPECT; // cols per row
    let rows_from_cols = (term_cols as f64 / ratio).floor();
    let (cols, rows) = if rows_from_cols as usize <= term_rows {
        (term_cols, rows_from_cols as usize)
    } else {
        ((term_rows as f64 * ratio).floor() as usize, term_rows)
    };
    (cols.max(1), rows.max(1))
}

/// Renders `frame` into a truecolor ASCII string of `cols`x`rows` cells.
///
/// Each output cell is the average color of the source block it covers; the
/// character is chosen from [`RAMP`] by luminance and colored with a 24-bit
/// ANSI foreground escape. Rows are separated by `\r\n` so the cursor returns
/// to the left margin (we render in raw mode).
pub fn to_ascii(frame: &Frame, cols: usize, rows: usize) -> String {
    let mut out = String::with_capacity(cols * rows * 20);
    let mut last_color: Option<[u8; 3]> = None;

    for cy in 0..rows {
        let sy0 = cy * frame.height / rows;
        let sy1 = ((cy + 1) * frame.height / rows).max(sy0 + 1).min(frame.height);
        for cx in 0..cols {
            let sx0 = cx * frame.width / cols;
            let sx1 = ((cx + 1) * frame.width / cols).max(sx0 + 1).min(frame.width);

            let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
            for sy in sy0..sy1 {
                let row = sy * frame.width;
                for sx in sx0..sx1 {
                    let i = (row + sx) * 3;
                    r += frame.rgb[i] as u64;
                    g += frame.rgb[i + 1] as u64;
                    b += frame.rgb[i + 2] as u64;
                    n += 1;
                }
            }
            let n = n.max(1);
            let (r, g, b) = ((r / n) as u8, (g / n) as u8, (b / n) as u8);

            let lum = (299 * r as u32 + 587 * g as u32 + 114 * b as u32) / 1000;
            let idx = (lum as usize * (RAMP.len() - 1)) / 255;
            let ch = RAMP[idx] as char;

            // Only emit a new color escape when the color actually changes.
            if last_color != Some([r, g, b]) {
                let _ = write!(out, "\x1b[38;2;{r};{g};{b}m");
                last_color = Some([r, g, b]);
            }
            out.push(ch);
        }
        out.push_str("\x1b[0m\r\n");
        last_color = None;
    }
    out
}
