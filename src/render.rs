//! Renders an RGB [`Frame`] to colored ASCII art sized to the terminal.

use std::fmt::Write as _;

use crate::video::Frame;

/// Dark-to-bright character ramp.
const RAMP: &[u8] = b" .:-=+*#%@";

/// A terminal character cell is roughly twice as tall as it is wide, so we
/// stretch the horizontal sampling to keep the picture's aspect ratio.
const CHAR_ASPECT: f64 = 2.0;

/// How cells are colored.
///
/// `Truecolor` emits 24-bit ANSI escapes (`\e[38;2;r;g;bm`) — gorgeous, but only
/// on terminals that support it. macOS Terminal does **not**, and misreads those
/// escapes badly, so `Ansi256` falls back to the 256-color palette (`\e[38;5;nm`)
/// it does support. `Mono` emits no color at all (works literally everywhere).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorMode {
    Truecolor,
    Ansi256,
    Mono,
}

/// Maps a 24-bit RGB color to the nearest xterm-256 palette index.
fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    // The 6x6x6 color cube uses these component levels.
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let nearest = |v: u8| -> (usize, u8) {
        let mut best = 0usize;
        let mut best_d = i32::MAX;
        for (i, &lv) in LEVELS.iter().enumerate() {
            let d = (v as i32 - lv as i32).abs();
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        (best, LEVELS[best])
    };
    let (ri, rv) = nearest(r);
    let (gi, gv) = nearest(g);
    let (bi, bv) = nearest(b);

    // Grayscale ramp (indices 232..=255) often matches better for near-gray colors.
    let gray = (r as u32 + g as u32 + b as u32) / 3;
    let gi2 = (((gray as i32 - 8).max(0)) / 10).min(23);
    let gray_v = (8 + 10 * gi2) as i32;

    let sq = |a: i32, b: i32| (a - b) * (a - b);
    let cube_d = sq(r as i32, rv as i32) + sq(g as i32, gv as i32) + sq(b as i32, bv as i32);
    let gray_d = sq(r as i32, gray_v) + sq(g as i32, gray_v) + sq(b as i32, gray_v);

    if gray_d < cube_d {
        232 + gi2 as u8
    } else {
        16 + (36 * ri + 6 * gi + bi) as u8
    }
}

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
pub fn to_ascii(frame: &Frame, cols: usize, rows: usize, mode: ColorMode) -> String {
    let mut out = String::with_capacity(cols * rows * 20);

    for cy in 0..rows {
        // Color escapes are only worth re-emitting when the color changes; reset
        // the tracker each row (we always start a fresh line).
        let mut last: Option<u32> = None;
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

            // Emit a color escape only when it changes from the previous cell.
            match mode {
                ColorMode::Mono => {}
                ColorMode::Truecolor => {
                    let key = (r as u32) << 16 | (g as u32) << 8 | b as u32;
                    if last != Some(key) {
                        let _ = write!(out, "\x1b[38;2;{r};{g};{b}m");
                        last = Some(key);
                    }
                }
                ColorMode::Ansi256 => {
                    let n = rgb_to_ansi256(r, g, b) as u32;
                    if last != Some(n) {
                        let _ = write!(out, "\x1b[38;5;{n}m");
                        last = Some(n);
                    }
                }
            }
            out.push(ch);
        }
        if mode == ColorMode::Mono {
            out.push_str("\r\n");
        } else {
            out.push_str("\x1b[0m\r\n");
        }
    }
    out
}

/// Average RGB of the source block `[x0,x1) x [y0,y1)` (bounds assumed valid).
fn avg_block(frame: &Frame, x0: usize, x1: usize, y0: usize, y1: usize) -> (u8, u8, u8) {
    let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
    for y in y0..y1 {
        let row = y * frame.width;
        for x in x0..x1 {
            let i = (row + x) * 3;
            r += frame.rgb[i] as u64;
            g += frame.rgb[i + 1] as u64;
            b += frame.rgb[i + 2] as u64;
            n += 1;
        }
    }
    let n = n.max(1);
    ((r / n) as u8, (g / n) as u8, (b / n) as u8)
}

/// Renders `frame` using Unicode upper-half-block characters (`▀`).
///
/// Each cell encodes **two** vertical pixels — the foreground color is the top
/// pixel, the background the bottom — which doubles vertical resolution versus
/// [`to_ascii`] and draws solid color blocks instead of sparse characters. This
/// is what makes fine detail (like the timecode) readable in a short terminal.
/// `mode` must be `Truecolor` or `Ansi256` (color is required for half-blocks).
pub fn to_half_blocks(frame: &Frame, cols: usize, rows: usize, mode: ColorMode) -> String {
    let vrows = (rows * 2).max(1); // vertical samples: two per character row
    let mut out = String::with_capacity(cols * rows * 28);
    let band = |k: usize| -> (usize, usize) {
        let a = k * frame.height / vrows;
        let b = ((k + 1) * frame.height / vrows).max(a + 1).min(frame.height);
        (a, b)
    };

    for cy in 0..rows {
        let mut last: Option<(u32, u32)> = None;
        let (ty0, ty1) = band(2 * cy);
        let (by0, by1) = band(2 * cy + 1);
        for cx in 0..cols {
            let sx0 = cx * frame.width / cols;
            let sx1 = ((cx + 1) * frame.width / cols).max(sx0 + 1).min(frame.width);
            let (tr, tg, tb) = avg_block(frame, sx0, sx1, ty0, ty1);
            let (br, bg, bb) = avg_block(frame, sx0, sx1, by0, by1);

            match mode {
                ColorMode::Ansi256 | ColorMode::Mono => {
                    // Mono falls back to 256 here; the receiver routes true mono
                    // to to_ascii, so this is only reached for Ansi256.
                    let tn = rgb_to_ansi256(tr, tg, tb) as u32;
                    let bn = rgb_to_ansi256(br, bg, bb) as u32;
                    if last != Some((tn, bn)) {
                        let _ = write!(out, "\x1b[38;5;{tn};48;5;{bn}m");
                        last = Some((tn, bn));
                    }
                }
                ColorMode::Truecolor => {
                    let tk = (tr as u32) << 16 | (tg as u32) << 8 | tb as u32;
                    let bk = (br as u32) << 16 | (bg as u32) << 8 | bb as u32;
                    if last != Some((tk, bk)) {
                        let _ = write!(out, "\x1b[38;2;{tr};{tg};{tb};48;2;{br};{bg};{bb}m");
                        last = Some((tk, bk));
                    }
                }
            }
            out.push('▀');
        }
        out.push_str("\x1b[0m\r\n");
    }
    out
}
