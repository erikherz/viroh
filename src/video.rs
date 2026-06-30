//! Synthetic video source and the Motion-JPEG codec glue.
//!
//! The source renders a classic test pattern: a big running timecode
//! (`HH:MM:SS.mmm`), a moving sweep bar so motion is obvious even in ASCII, and
//! a small label. Frames are plain 24-bit RGB and are (de)compressed with a
//! pure-Rust JPEG codec, giving us a real Motion-JPEG stream over the wire.

use anyhow::{anyhow, Result};
use fontdue::Font;
use jpeg_encoder::{ColorType, Encoder};
use zune_jpeg::JpegDecoder;

/// JetBrains Mono ExtraBold (SIL OFL 1.1), embedded for portable text rendering.
const FONT_BYTES: &[u8] = include_bytes!("../assets/JetBrainsMono-ExtraBold.ttf");

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

    /// Alpha-composites a grayscale coverage bitmap (`cov`, `cw`x`ch`) into the
    /// destination rectangle, box-averaging so it scales smoothly, tinted `color`.
    fn blit_coverage(
        &mut self,
        cov: &[u8],
        cw: usize,
        ch: usize,
        dx: usize,
        dy: usize,
        dw: usize,
        dh: usize,
        color: [u8; 3],
    ) {
        if cw == 0 || ch == 0 || dw == 0 || dh == 0 {
            return;
        }
        for j in 0..dh {
            let py = dy + j;
            if py >= self.height {
                break;
            }
            let sy0 = j * ch / dh;
            let sy1 = ((j + 1) * ch / dh).max(sy0 + 1).min(ch);
            for i in 0..dw {
                let px = dx + i;
                if px >= self.width {
                    break;
                }
                let sx0 = i * cw / dw;
                let sx1 = ((i + 1) * cw / dw).max(sx0 + 1).min(cw);
                let (mut sum, mut n) = (0u32, 0u32);
                for sy in sy0..sy1 {
                    let row = sy * cw;
                    for sx in sx0..sx1 {
                        sum += cov[row + sx] as u32;
                        n += 1;
                    }
                }
                let a = sum / n.max(1);
                if a == 0 {
                    continue;
                }
                let idx = (py * self.width + px) * 3;
                for k in 0..3 {
                    let e = self.rgb[idx + k] as u32;
                    self.rgb[idx + k] = ((e * (255 - a) + color[k] as u32 * a) / 255) as u8;
                }
            }
        }
    }
}

/// Rasterizes `text` to a tightly-cropped grayscale coverage canvas at `px`
/// pixels tall, laying glyphs out with the font's metrics. Returns
/// `(coverage, width, height)`.
fn rasterize_text(font: &Font, text: &str, px: f32) -> (Vec<u8>, usize, usize) {
    let mut pen = 0f32;
    let mut max_top = 0i32;
    let mut min_bot = 0i32;
    let mut glyphs = Vec::new();
    for ch in text.chars() {
        let (m, bmp) = font.rasterize(ch, px);
        max_top = max_top.max(m.ymin + m.height as i32);
        min_bot = min_bot.min(m.ymin);
        glyphs.push((pen + m.xmin as f32, m, bmp));
        pen += m.advance_width;
    }
    let width = pen.ceil().max(1.0) as usize;
    let height = (max_top - min_bot).max(1) as usize;
    let baseline = max_top;
    let mut canvas = vec![0u8; width * height];
    for (gx, m, bmp) in glyphs {
        let ox = gx.round() as i32;
        let oy = baseline - (m.ymin + m.height as i32);
        for j in 0..m.height {
            for i in 0..m.width {
                let cx = ox + i as i32;
                let cy = oy + j as i32;
                if cx >= 0 && (cx as usize) < width && cy >= 0 && (cy as usize) < height {
                    let v = bmp[j * m.width + i];
                    let idx = cy as usize * width + cx as usize;
                    if v > canvas[idx] {
                        canvas[idx] = v;
                    }
                }
            }
        }
    }
    (canvas, width, height)
}

/// In-place morphological dilation (separable max filter, radius `r`), thickening
/// glyph strokes so each survives the heavy downsample as several solid cells.
fn dilate(cov: &mut [u8], w: usize, h: usize, r: usize) {
    if r == 0 || w == 0 || h == 0 {
        return;
    }
    let mut tmp = vec![0u8; w * h];
    for y in 0..h {
        let row = y * w;
        for x in 0..w {
            let x0 = x.saturating_sub(r);
            let x1 = (x + r + 1).min(w);
            let mut m = 0u8;
            for xx in x0..x1 {
                m = m.max(cov[row + xx]);
            }
            tmp[row + x] = m;
        }
    }
    for y in 0..h {
        let y0 = y.saturating_sub(r);
        let y1 = (y + r + 1).min(h);
        for x in 0..w {
            let mut m = 0u8;
            for yy in y0..y1 {
                m = m.max(tmp[yy * w + x]);
            }
            cov[y * w + x] = m;
        }
    }
}

/// In-place contrast curve that pushes anti-aliased mid-grays toward 0/255,
/// restoring crisp edges. The box-average downsample re-introduces exactly the
/// smoothing we need, so sharp source edges read better than soft ones.
fn sharpen(cov: &mut [u8]) {
    const K: f32 = 4.5;
    for v in cov.iter_mut() {
        let n = (*v as f32 / 255.0 - 0.5) * K + 0.5;
        *v = (n.clamp(0.0, 1.0) * 255.0) as u8;
    }
}

/// Generates synthetic timecode frames at a fixed resolution.
pub struct TimecodeSource {
    width: usize,
    height: usize,
    frame_no: u64,
    fps: u32,
    font: Font,
}

impl TimecodeSource {
    pub fn new(width: usize, height: usize, fps: u32) -> Self {
        let font = Font::from_bytes(FONT_BYTES, fontdue::FontSettings::default())
            .expect("embedded font is valid");
        TimecodeSource {
            width,
            height,
            frame_no: 0,
            fps,
            font,
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

        // Big centered timecode. The terminal output is only ~120x80 effective
        // pixels, so each cell averages a ~5x6 source block — thin strokes wash
        // out to gray mush. To survive that we (1) fatten the strokes by dilation
        // so each is several solid cells wide, (2) sharpen the anti-aliased edges
        // back toward crisp (the downsample re-adds the smoothing), and (3) draw
        // white digits on a pure-black panel for maximum luminance separation
        // through the 256-color quantization. Stretched ~1.8x taller to buy a few
        // more terminal rows without cramping the digits horizontally.
        let tc = format_timecode(elapsed_ms);
        let (mut cov, cw, ch) = rasterize_text(&self.font, &tc, 160.0);
        dilate(&mut cov, cw, ch, 5);
        sharpen(&mut cov);
        let dst_w = self.width * 88 / 100;
        let scale_x = dst_w as f32 / cw as f32;
        let dst_h = (ch as f32 * scale_x * 1.8) as usize;
        let tx = (self.width.saturating_sub(dst_w)) / 2;
        let ty = (self.height.saturating_sub(dst_h)) / 2;
        let pad = 14usize;
        f.fill_rect(
            tx.saturating_sub(pad),
            ty.saturating_sub(pad),
            dst_w + 2 * pad,
            dst_h + 2 * pad,
            [0, 0, 0],
        );
        f.blit_coverage(&cov, cw, ch, tx, ty, dst_w, dst_h, [255, 255, 255]);

        // Small label + frame counter, rendered 1:1 (no stretch) up top/bottom.
        let label = format!("VIROH {}x{} {}FPS", self.width, self.height, self.fps);
        let (lc, lcw, lch) = rasterize_text(&self.font, &label, 22.0);
        f.blit_coverage(&lc, lcw, lch, 16, 16, lcw, lch, [200, 200, 80]);
        let counter = format!("FRAME {}", self.frame_no);
        let (cc, ccw, cch) = rasterize_text(&self.font, &counter, 22.0);
        f.blit_coverage(&cc, ccw, cch, 16, self.height - 16 - cch, ccw, cch, [180, 180, 180]);

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
