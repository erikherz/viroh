//! viroh sender: synthesizes a 640x480 timecode video and streams it as
//! Motion-JPEG over iroh to any receiver that dials our node id.

use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use iroh::{endpoint::presets, endpoint::Connection, Endpoint, RelayMap, RelayMode};
use tokio::time::sleep;

use viroh::{video, video::TimecodeSource, write_frame, write_meta, StreamMeta, ALPN, FPS, HEIGHT, WIDTH};

#[derive(Parser, Debug, Clone)]
#[command(about = "Stream a synthetic timecode video over iroh")]
struct Args {
    /// Agent name, sent to receivers in the stream metadata.
    #[arg(long, default_value = "viroh")]
    name: String,
    /// Use a custom iroh relay (e.g. https://server.viroh.net) instead of n0's.
    #[arg(long)]
    relay_url: Option<String>,
    /// Frames per second.
    #[arg(long, default_value_t = FPS)]
    fps: u32,
    /// JPEG quality (1-100).
    #[arg(long, default_value_t = 90)]
    quality: u8,
    /// Frame width in pixels.
    #[arg(long, default_value_t = WIDTH)]
    width: usize,
    /// Frame height in pixels.
    #[arg(long, default_value_t = HEIGHT)]
    height: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let args = Args::parse();

    let mut builder = Endpoint::builder(presets::N0).alpns(vec![ALPN.to_vec()]);
    if let Some(url) = &args.relay_url {
        let map = RelayMap::try_from_iter([url.as_str()])
            .map_err(|e| anyhow::anyhow!("invalid --relay-url {url}: {e}"))?;
        builder = builder.relay_mode(RelayMode::Custom(map));
    }
    let ep = builder.bind().await?;

    // Wait until we have a home relay so discovery can publish our address.
    ep.online().await;

    // Metadata advertised to every receiver. `started_at` is fixed at agent
    // startup so all receivers see the same "since" time.
    let meta = std::sync::Arc::new(StreamMeta {
        name: args.name.clone(),
        started_at: chrono::Utc::now().to_rfc3339(),
        kind: "video only".to_string(),
        width: args.width,
        height: args.height,
        fps: args.fps,
    });
    let cfg = FrameCfg {
        fps: args.fps,
        quality: args.quality,
        width: args.width,
        height: args.height,
    };

    let id = ep.id();
    println!("viroh sender ready.");
    println!("  name   : {}", args.name);
    println!("  node id: {id}");
    println!("  source : {}x{} @ {}fps, jpeg q{}", args.width, args.height, args.fps, args.quality);
    match &args.relay_url {
        Some(url) => println!("  relay  : {url} (custom)"),
        None => println!("  relay  : n0 default"),
    }
    println!();
    println!("Start a receiver with:");
    println!("    viroh-receiver {id}");
    println!();
    println!("Waiting for receivers (Ctrl-C to quit)...");

    while let Some(incoming) = ep.accept().await {
        let meta = meta.clone();
        tokio::spawn(async move {
            match incoming.await {
                Ok(conn) => {
                    let peer = conn.remote_id();
                    println!("+ receiver connected: {peer}");
                    if let Err(e) = serve(conn, cfg, meta).await {
                        println!("- receiver {peer} ended: {e}");
                    } else {
                        println!("- receiver {peer} disconnected");
                    }
                }
                Err(e) => eprintln!("incoming connection failed: {e}"),
            }
        });
    }

    Ok(())
}

/// Frame-generation parameters (the `Copy` subset of [`Args`]).
#[derive(Clone, Copy)]
struct FrameCfg {
    fps: u32,
    quality: u8,
    width: usize,
    height: usize,
}

/// Streams metadata then synthetic frames to one receiver until it goes away.
async fn serve(conn: Connection, cfg: FrameCfg, meta: std::sync::Arc<StreamMeta>) -> Result<()> {
    let mut send = conn.open_uni().await?;

    // Metadata handshake first, then the video frames.
    write_meta(&mut send, &meta).await?;

    let mut src = TimecodeSource::new(cfg.width, cfg.height, cfg.fps);

    let start = Instant::now();
    let frame_dur = Duration::from_secs_f64(1.0 / cfg.fps.max(1) as f64);
    let mut next = Instant::now();

    loop {
        let frame = src.render(start.elapsed().as_millis());
        let jpeg = video::encode_jpeg(&frame, cfg.quality)?;

        // A write error means the receiver hung up; that's a normal exit.
        if write_frame(&mut send, &jpeg).await.is_err() {
            break;
        }

        next += frame_dur;
        let now = Instant::now();
        if next > now {
            sleep(next - now).await;
        } else {
            // We fell behind; reset the cadence rather than burst frames.
            next = now;
        }
    }

    Ok(())
}
