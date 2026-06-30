//! Offline preview of the synthetic video source as ASCII — no networking.
//!
//! Renders a few timecode frames straight to stdout so you can eyeball the
//! source and the ASCII renderer without running a sender/receiver pair.
//!
//! Usage: `cargo run --example preview [cols] [rows] [frames]`

use viroh::{render, video, FPS, HEIGHT, WIDTH};

fn main() {
    let mut args = std::env::args().skip(1);
    let cols: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(100);
    let rows: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(32);
    let frames: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(3);

    let mut src = video::TimecodeSource::new(WIDTH, HEIGHT, FPS);
    let (gc, gr) = render::fit_grid(WIDTH, HEIGHT, cols, rows);

    for i in 0..frames {
        // Sample a few moments across one second.
        let frame = src.render((i as u128) * 1000 / frames.max(1) as u128);
        let art = render::to_ascii(&frame, gc, gr);
        // `to_ascii` ends each row with CRLF (for raw mode); LF is fine here too.
        print!("{art}");
        println!("\x1b[0m--- frame {i} ---\n");
    }
}
