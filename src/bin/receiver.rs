//! viroh receiver: dials a sender's node id, reads the Motion-JPEG stream, and
//! renders it as colored ASCII art in the terminal.

use std::io::{self, Write};
use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{cursor, event, execute, terminal};
use iroh::{endpoint::presets, Endpoint, EndpointId, RelayMap, RelayMode};

use viroh::{read_frame, read_meta, render, render::ColorMode, video, ALPN};

#[derive(Parser, Debug)]
#[command(about = "Render a viroh sender's video as terminal ASCII art")]
struct Args {
    /// The sender's node id (printed by viroh-sender on startup).
    node_id: String,
    /// Use a custom iroh relay (e.g. https://server.viroh.net) instead of n0's.
    #[arg(long)]
    relay_url: Option<String>,
    /// Color output mode. `auto` uses 24-bit if $COLORTERM advertises it, else
    /// 256-color (correct on macOS Terminal, which lacks 24-bit support).
    #[arg(long, value_enum, default_value_t = ColorArg::Auto)]
    color: ColorArg,
}

#[derive(Copy, Clone, Debug, clap::ValueEnum)]
enum ColorArg {
    /// Detect from $COLORTERM; fall back to 256-color.
    Auto,
    /// 24-bit truecolor (iTerm2, Ghostty, kitty, WezTerm).
    Truecolor,
    /// 256-color palette (works in macOS Terminal).
    #[value(name = "256")]
    C256,
    /// No color, characters only (works anywhere).
    Mono,
}

impl ColorArg {
    /// Resolves `Auto` against the environment into a concrete render mode.
    fn resolve(self) -> ColorMode {
        match self {
            ColorArg::Truecolor => ColorMode::Truecolor,
            ColorArg::C256 => ColorMode::Ansi256,
            ColorArg::Mono => ColorMode::Mono,
            ColorArg::Auto => match std::env::var("COLORTERM").as_deref() {
                Ok("truecolor") | Ok("24bit") => ColorMode::Truecolor,
                _ => ColorMode::Ansi256,
            },
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let color_mode = args.color.resolve();
    let id = EndpointId::from_str(args.node_id.trim()).context("invalid node id")?;

    let mut builder = Endpoint::builder(presets::N0);
    if let Some(url) = &args.relay_url {
        let map = RelayMap::try_from_iter([url.as_str()])
            .map_err(|e| anyhow::anyhow!("invalid --relay-url {url}: {e}"))?;
        builder = builder.relay_mode(RelayMode::Custom(map));
    }
    let ep = builder.bind().await?;
    eprintln!("connecting to {id} ...");
    let conn = ep.connect(id, ALPN).await?;
    let mut recv = conn.accept_uni().await?;

    // Read the metadata handshake before any frames.
    let meta = read_meta(&mut recv).await.context("reading stream metadata")?;
    eprintln!(
        "stream from \"{}\" ({}), started {}",
        meta.name, meta.kind, meta.started_at
    );

    // From here on the terminal is in raw / alternate-screen mode; the guard
    // restores it on any exit path (including panics).
    let _guard = TerminalGuard::enter()?;

    let mut stdout = io::stdout();
    let mut frames: u64 = 0;
    let mut bytes_total: u64 = 0;
    let mut last_dims = (0u16, 0u16);
    let started = Instant::now();
    let mut fps_window_start = Instant::now();
    let mut fps_window_frames: u64 = 0;
    let mut shown_fps = 0.0f64;

    loop {
        if should_quit()? {
            break;
        }

        let jpeg = match read_frame(&mut recv).await? {
            Some(j) => j,
            None => break, // sender closed the stream
        };
        bytes_total += jpeg.len() as u64;
        frames += 1;
        fps_window_frames += 1;

        let frame = match video::decode_jpeg(&jpeg) {
            Ok(f) => f,
            Err(_) => continue, // skip a corrupt frame rather than dying
        };

        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        if (cols, rows) != last_dims {
            execute!(stdout, terminal::Clear(terminal::ClearType::All))?;
            last_dims = (cols, rows);
        }
        // Reserve the bottom row for a status line.
        let (gc, gr) = render::fit_grid(frame.width, frame.height, cols as usize, rows.saturating_sub(1) as usize);
        let art = render::to_ascii(&frame, gc, gr, color_mode);

        // Update the rolling FPS estimate roughly twice a second.
        if fps_window_start.elapsed() >= Duration::from_millis(500) {
            shown_fps = fps_window_frames as f64 / fps_window_start.elapsed().as_secs_f64();
            fps_window_start = Instant::now();
            fps_window_frames = 0;
        }

        let mbps = (bytes_total as f64 * 8.0 / 1_000_000.0) / started.elapsed().as_secs_f64().max(0.001);
        let status = format!(
            "{} ({})  {gc}x{gr}  {:.1} fps  {} frames  {:.1} KiB/f  {:.2} Mbps  {:.0}s  [q]",
            meta.name,
            meta.kind,
            shown_fps,
            frames,
            jpeg.len() as f64 / 1024.0,
            mbps,
            started.elapsed().as_secs_f64(),
        );

        // Home the cursor, paint the frame, then the status line.
        let _ = write!(stdout, "\x1b[H{art}");
        execute!(stdout, cursor::MoveTo(0, rows.saturating_sub(1)))?;
        let _ = write!(stdout, "\x1b[7m{:width$}\x1b[0m", status, width = cols as usize);
        stdout.flush()?;
    }

    Ok(())
}

/// Non-blocking check for a quit keypress (`q`, `Esc`, or `Ctrl-C`).
fn should_quit() -> Result<bool> {
    if event::poll(Duration::from_millis(0))? {
        if let event::Event::Key(k) = event::read()? {
            use event::{KeyCode, KeyModifiers};
            let ctrl_c = k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL);
            if ctrl_c || matches!(k.code, KeyCode::Char('q') | KeyCode::Esc) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Puts the terminal into raw mode on the alternate screen, restoring it on drop.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        terminal::enable_raw_mode()?;
        execute!(io::stdout(), terminal::EnterAlternateScreen, cursor::Hide)?;
        Ok(TerminalGuard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), cursor::Show, terminal::LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}
