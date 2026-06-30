//! Shared pieces for the `viroh` synthetic-video sender and ASCII receiver.

pub mod font;
pub mod render;
pub mod video;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// ALPN identifier for the viroh Motion-JPEG stream protocol.
pub const ALPN: &[u8] = b"viroh/mjpeg/1";

/// Stream metadata sent as the very first message on every connection, before
/// any video frames. It is a length-prefixed JSON object (see [`write_meta`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamMeta {
    /// Human-friendly agent name (the sender's `--name`).
    pub name: String,
    /// When the sending agent started, as an RFC 3339 timestamp.
    pub started_at: String,
    /// Stream kind, e.g. `"video only"`.
    pub kind: String,
    pub width: usize,
    pub height: usize,
    pub fps: u32,
}

/// Writes the [`StreamMeta`] handshake as the first length-prefixed message.
pub async fn write_meta<W>(w: &mut W, meta: &StreamMeta) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    let json = serde_json::to_vec(meta)?;
    write_frame(w, &json).await
}

/// Reads the [`StreamMeta`] handshake (the first message on the stream).
pub async fn read_meta<R>(r: &mut R) -> Result<StreamMeta>
where
    R: AsyncReadExt + Unpin,
{
    let msg = read_frame(r)
        .await?
        .ok_or_else(|| anyhow::anyhow!("stream closed before metadata"))?;
    Ok(serde_json::from_slice(&msg)?)
}

/// Default capture resolution.
pub const WIDTH: usize = 640;
pub const HEIGHT: usize = 480;
/// Default frame rate.
pub const FPS: u32 = 30;

/// Reject absurd frame sizes (corruption / wrong protocol) early.
const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;

/// Writes one length-prefixed JPEG frame: `[u32 big-endian len][bytes]`.
pub async fn write_frame<W>(w: &mut W, jpeg: &[u8]) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    let len = jpeg.len() as u32;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(jpeg).await?;
    Ok(())
}

/// Reads one length-prefixed JPEG frame written by [`write_frame`].
///
/// Returns `Ok(None)` on a clean end-of-stream.
pub async fn read_frame<R>(r: &mut R) -> Result<Option<Vec<u8>>>
where
    R: AsyncReadExt + Unpin,
{
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME_BYTES {
        bail!("frame too large: {len} bytes");
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).await?;
    Ok(Some(buf))
}
