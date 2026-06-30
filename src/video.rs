//! Synthetic video source and the Motion-JPEG codec glue.
//!
//! The source renders a classic test pattern: a big running timecode
//! (`HH:MM:SS.mmm`), a moving sweep bar so motion is obvious even in ASCII, and
//! a small label. Frames are plain 24-bit RGB and are (de)compressed with a
//! pure-Rust JPEG codec, giving us a real Motion-JPEG stream over the wire.

use anyhow::{anyhow, Result};
use jpeg_encoder::{ColorType, Encoder};
use zune_jpeg::JpegDecoder;

use crate::font;

/// A 24-bit RGB frame (`rgb.len() == width * height * 3`).
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

impl Frame {
    fn new(width: usize, height: usize) -> Self {
        Frame {
            width,
            height,
            rgb: vec![0u8; width * height * 3],
        }
    }

    #[inline]
    fn put(&mut self, x: usize, y: usize, c: [u8; 3]) {
        if x >= self.width || y >= self.height {
            return;
        }
        let i = (y * self.width + x) * 3;
        self.rgb[i] = c[0];
        self.rgb[i + 1] = c[1];
        self.rgb[i + 2] = c[2];
    }

    /// Draws a filled rectangle, clipped to the frame.
    fn fill_rect(&mut self, x0: usize, y0: usize, w: usize, h: usize, c: [u8; 3]) {
        for y in y0..(y0 + h).min(self.height) {
            for x in x0..(x0 + w).min(self.width) {
                let i = (y * self.width + x) * 3;
                self.rgb[i] = c[0];
                self.rgb[i + 1] = c[1];
                self.rgb[i + 2] = c[2];
            }
        }
    }

    /// Draws `text` with the 5x7 font, scaled by `scale`, top-left at (x, y).
    /// `weight` thickens each stroke by that many extra pixels so the glyphs
    /// survive the heavy downsampling the ASCII renderer applies. Returns the x
    /// coordinate just past the drawn text.
    fn draw_text(
        &mut self,
        x: usize,
        y: usize,
        text: &str,
        xscale: usize,
        yscale: usize,
        weight: usize,
        c: [u8; 3],
    ) -> usize {
        let mut cx = x;
        let advance = (font::GLYPH_W + 1) * xscale;
        for ch in text.chars() {
            if let Some(rows) = font::glyph(ch) {
                for (ry, bits) in rows.iter().enumerate() {
                    for col in 0..font::GLYPH_W {
                        // bit 4 is the leftmost column
                        if bits & (1 << (font::GLYPH_W - 1 - col)) != 0 {
                            let px = cx + col * xscale;
                            let py = y + ry * yscale;
                            self.fill_rect(px, py, xscale + weight, yscale + weight, c);
                        }
                    }
                }
            }
            cx += advance;
        }
        cx
    }
}

/// Generates synthetic timecode frames at a fixed resolution.
pub struct TimecodeSource {
    width: usize,
    height: usize,
    frame_no: u64,
    fps: u32,
}

impl TimecodeSource {
    pub fn new(width: usize, height: usize, fps: u32) -> Self {
        TimecodeSource {
            width,
            height,
            frame_no: 0,
            fps,
        }
    }

    /// Renders the frame for a given elapsed time (since stream start).
    pub fn render(&mut self, elapsed_ms: u128) -> Frame {
        let mut f = Frame::new(self.width, self.height);

        // Background: a subtle vertical gradient so the picture isn't flat.
        for y in 0..self.height {
            let v = 16 + (y * 24 / self.height) as u8;
            let row = [v / 3, v / 3, v];
            for x in 0..self.width {
                f.put(x, y, row);
            }
        }

        // Moving sweep bar: a bright vertical band that scrolls left->right once
        // per second. Drawn only in the top and bottom strips so it never crosses
        // the centered timecode in the middle of the frame.
        let period = self.fps.max(1) as u128 * 33; // ~1s worth of ms
        let phase = (elapsed_ms % period) as f64 / period as f64;
        let bar_x = (phase * self.width as f64) as usize;
        let strip = self.height * 22 / 100;
        f.fill_rect(bar_x.saturating_sub(4), 0, 9, strip, [40, 120, 200]);
        f.fill_rect(bar_x.saturating_sub(4), self.height - strip, 9, strip, [40, 120, 200]);

        // Corner crosshair / border so the frame edges are visible.
        f.fill_rect(0, 0, self.width, 3, [80, 80, 80]);
        f.fill_rect(0, self.height - 3, self.width, 3, [80, 80, 80]);
        f.fill_rect(0, 0, 3, self.height, [80, 80, 80]);
        f.fill_rect(self.width - 3, 0, 3, self.height, [80, 80, 80]);

        // Big centered timecode. The horizontal scale fits all 12 characters in
        // ~85% of the width; the vertical scale is stretched ~2.6x taller, since
        // terminals are far shorter than they are wide and the ASCII renderer
        // squeezes the frame's height much harder than its width. Taller digits
        // therefore land on many more terminal rows and read clearly.
        let tc = format_timecode(elapsed_ms);
        let glyph_adv = font::GLYPH_W + 1;
        let xscale = ((self.width * 85 / 100) / (tc.chars().count() * glyph_adv)).max(1);
        let yscale = (xscale * 13 / 5).max(1); // ~2.6x taller
        let weight = xscale / 2;
        let text_w = tc.chars().count() * glyph_adv * xscale;
        let text_h = font::GLYPH_H * yscale;
        let tx = (self.width.saturating_sub(text_w)) / 2;
        let ty = (self.height.saturating_sub(text_h)) / 2;
        // drop shadow then bright text
        f.draw_text(tx + xscale / 3, ty + yscale / 3, &tc, xscale, yscale, weight, [0, 0, 0]);
        f.draw_text(tx, ty, &tc, xscale, yscale, weight, [80, 255, 120]);

        // Small label + frame counter.
        let label = format!("VIROH {}X{} {}FPS", self.width, self.height, self.fps);
        f.draw_text(16, 16, &label, 3, 3, 1, [200, 200, 80]);
        let counter = format!("FRAME {}", self.frame_no);
        f.draw_text(16, self.height - 16 - font::GLYPH_H * 3, &counter, 3, 3, 1, [180, 180, 180]);

        self.frame_no += 1;
        f
    }
}

/// Formats milliseconds as `HH:MM:SS.mmm`.
pub fn format_timecode(ms: u128) -> String {
    let total_s = ms / 1000;
    let h = total_s / 3600;
    let m = (total_s % 3600) / 60;
    let s = total_s % 60;
    let millis = ms % 1000;
    format!("{h:02}:{m:02}:{s:02}.{millis:03}")
}

/// Encodes an RGB frame to JPEG bytes.
pub fn encode_jpeg(frame: &Frame, quality: u8) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let encoder = Encoder::new(&mut out, quality);
    encoder
        .encode(
            &frame.rgb,
            frame.width as u16,
            frame.height as u16,
            ColorType::Rgb,
        )
        .map_err(|e| anyhow!("jpeg encode failed: {e}"))?;
    Ok(out)
}

/// Decodes JPEG bytes into an RGB [`Frame`].
pub fn decode_jpeg(bytes: &[u8]) -> Result<Frame> {
    let mut decoder = JpegDecoder::new(bytes);
    let rgb = decoder
        .decode()
        .map_err(|e| anyhow!("jpeg decode failed: {e:?}"))?;
    let info = decoder
        .info()
        .ok_or_else(|| anyhow!("jpeg decode produced no image info"))?;
    Ok(Frame {
        width: info.width as usize,
        height: info.height as usize,
        rgb,
    })
}
