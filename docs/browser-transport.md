# Browser-native transport & decode — design exploration

> Status: **exploration / parked.** No code written yet. This captures the design
> space for getting viroh video into a browser *without* today's HTTP-multipart
> bridge, and without MoQ. Written 2026-07-01.

## Why this exists

Today the browser watches a sender via a Rust **gateway** (`src/bin/fleet.rs`,
`source_stream`) that dials the sender over iroh, reads the Motion-JPEG frames,
and re-emits them as HTTP `multipart/x-mixed-replace` into a plain `<img>`. That
works everywhere with zero client code, but it's not browser-native transport,
there's no hardware decode, and each frame is a full JPEG (~10 Mbps).

This doc explores two better paths, and the decode/framing choices that apply to
both.

## Two independent axes

1. **The pipe** — how bytes reach the browser: HTTP multipart (today),
   **WebTransport**, or **actual iroh compiled to WASM**.
2. **Decode/render** — how the browser turns bytes into pixels: **WebCodecs**
   (`VideoDecoder`) or `ImageDecoder`, painted to `<canvas>`.

You can mix and match. **MoQ** is just one opinionated app-protocol on top of
WebTransport; we skip it and keep viroh's own framing.

## Hard truth: "iroh in browser JavaScript"

- You **cannot** hand-reimplement iroh in JS. It's a large Rust codebase (QUIC via
  `quinn`, relay protocol, pkarr/DNS discovery, raw-key TLS, hole-punching).
- "iroh in the browser" realistically means **iroh compiled to WASM**
  (`wasm32-unknown-unknown`) + a thin `wasm-bindgen` JS wrapper. n0 has been
  building this.
- Two constraints shape it:
  1. **No UDP / no hole-punching in browsers** → a browser iroh node reaches
     peers **only through a relay** (which we self-host at `server.viroh.net`).
  2. That relay transport in-browser is a **WebSocket** (and increasingly
     **WebTransport**) to the relay.
- Upshot: a browser CAN be a real iroh endpoint that dials the sender's node id,
  negotiates ALPN `viroh/mjpeg/1`, `accept_uni()`, and reads our exact
  `[u32 len][payload]` frames — end-to-end encrypted, relayed through our infra.
  Same logic as the CLI receiver, compiled to WASM.

## Path A — WebTransport gateway + WebCodecs (pragmatic, ship-first)

Evolve the gateway: keep speaking iroh to the sender, but replace HTTP-multipart
with **WebTransport** (HTTP/3 / QUIC, native in Chrome). The wire framing stays
identical, so the browser parser is a near-1:1 port of the CLI receiver.

```
sender ──iroh/QUIC (viroh/mjpeg/1)──▶ gateway (Rust: iroh client + WebTransport server)
                                         │  reads meta + frames
                                         ▼
browser ◀── WebTransport uni-stream, SAME [u32 len][payload] framing
         └─▶ WebCodecs / ImageDecoder ─▶ <canvas>
```

### Browser client (JS sketch)

```js
const wt = new WebTransport('https://server.viroh.net:4443/watch/' + nodeId);
await wt.ready;

const reader = wt.incomingUnidirectionalStreams.getReader();
const { value: stream } = await reader.read();
const bytes = new ByteReader(stream.getReader());   // buffers arbitrary chunks, slices exact lengths

const metaLen = await bytes.u32be();
const meta = JSON.parse(new TextDecoder().decode(await bytes.exact(metaLen)));

const ctx = document.querySelector('canvas').getContext('2d');
for (;;) {
  const len = await bytes.u32be();
  const jpeg = await bytes.exact(len);
  const bmp = await createImageBitmap(new Blob([jpeg], { type: 'image/jpeg' }));
  ctx.drawImage(bmp, 0, 0, meta.width, meta.height);
  bmp.close();
}
```

`ByteReader` is the JS equivalent of Rust's `read_exact`.

### Gateway (Rust sketch)

Use `web-transport-quinn` (from `moq-dev/web-transport`) or the `wtransport`
crate. Its `SendStream` implements `tokio::io::AsyncWrite`, so `write_meta` /
`write_frame` from `src/lib.rs` are reusable verbatim:

```rust
let session = incoming.await?.accept().await?;   // WebTransport session from browser
let mut send = session.open_uni().await?.await?; // uni stream browser reads
write_meta(&mut send, &meta).await?;
while let Some(jpeg) = read_frame(&mut iroh_recv).await? {
    write_frame(&mut send, &jpeg).await?;        // identical framing, new pipe
}
```

**Costs / gotchas:** needs a UDP port (e.g. 4443) opened in the security group and
a valid TLS cert (our Let's Encrypt chain works; no `serverCertificateHashes`
hack needed). Still a gateway, not P2P — but browser-native and a clean path to
WebCodecs.

## Path B — iroh-WASM in the browser + WebCodecs (the "real iroh in browser")

No bespoke gateway. Compile a Rust receiver to WASM with iroh's browser support;
the browser is a real iroh node reaching the sender **through our relay**.

```
sender ──iroh/QUIC──▶ relay (server.viroh.net) ◀──WS/WebTransport── browser (iroh-WASM)
                                                                       │ accept_uni, read_frame
                                                                       ▼ JPEG bytes → JS
                                                             WebCodecs/ImageDecoder → <canvas>
```

```rust
#[wasm_bindgen]
pub async fn watch(node_id: String, on_frame: js_sys::Function) -> Result<(), JsValue> {
    let ep = Endpoint::builder(presets::N0)
        .relay_mode(RelayMode::Custom(relay_map("https://server.viroh.net")))
        .bind().await?;                              // browser: relay transport, no UDP
    let conn = ep.connect(EndpointId::from_str(&node_id)?, ALPN).await?;
    let mut recv = conn.accept_uni().await?;
    let _meta = read_meta(&mut recv).await?;         // same wire protocol!
    while let Some(jpeg) = read_frame(&mut recv).await? {
        on_frame.call1(&JsValue::NULL, &js_sys::Uint8Array::from(&jpeg[..])).ok();
    }
    Ok(())
}
```

JS glue just decodes + paints (same WebCodecs code as Path A). Discovery works
too: the browser can resolve a node id against `directory.viroh.net` over
DNS-over-HTTPS (pkarr is HTTP-friendly).

**Strongest fit for viroh:** reuses our exact protocol + infra, true e2e
encryption browser↔sender, genuinely "iroh in the browser." **Costs:**
relayed-only (browsers can't hole-punch), larger WASM bundle, and it rides iroh's
browser support — **verify current iroh WASM APIs & relay-transport status
against upstream before committing** (this area moves fast; API names above are
illustrative).

## Decode axis — WebCodecs & the MJPEG problem

**WebCodecs `VideoDecoder` has no "mjpeg" codec.** Registered codecs: `avc1.*`
(H.264), `vp8`, `vp09.*`, `av01.*`, `hev1.*`. So with today's per-frame JPEGs:

1. **Keep MJPEG → decode with `ImageDecoder` / `createImageBitmap`.** Zero sender
   change. Simple, works, yields `VideoFrame`/`ImageBitmap` for `drawImage`. But
   still per-frame JPEG (~10 Mbps) and not the hardware `VideoDecoder` fast path.
2. **Switch to a real codec → unlock `VideoDecoder` (the big win):**
   hardware-accelerated, sub-frame latency, **10–50× less bandwidth.**

```js
const dec = new VideoDecoder({
  output: frame => { ctx.drawImage(frame, 0, 0); frame.close(); },
  error:  e => console.error(e),
});
dec.configure({ codec: 'avc1.42E01E', optimizeForLatency: true });  // H.264 baseline
dec.decode(new EncodedVideoChunk({ type: isKey ? 'key' : 'delta', timestamp, data }));
```

To feed that, the **sender** swaps `jpeg-encoder` for a Rust encoder —
[`openh264`](https://crates.io/crates/openh264) (pragmatic, low-latency H.264) or
[`rav1e`](https://crates.io/crates/rav1e) (pure-Rust AV1, slower). Protocol
changes: metadata gains a `codec` string + decoder `description` (H.264 SPS/PPS);
each frame gains a tiny header (key/delta flag + timestamp). Keep
`viroh/mjpeg/1`; add `viroh/h264/1` or `viroh/av01/1`, negotiated by ALPN.

## Upstream building blocks (evaluated 2026-07-01)

Both are **100% Rust, no WASM/JS** — neither puts iroh in the browser by itself.

### [`n0-computer/web-transport-iroh`](https://github.com/n0-computer/web-transport-iroh)
- WebTransport *semantics* over an iroh connection, **Rust-to-Rust**. Does NOT let
  a W3C-`WebTransport` browser dial iroh.
- **Archived (2026-03), moved into [`moq-dev/web-transport`](https://github.com/moq-dev/web-transport).**
  Signal: n0's browser-media bet is the `web-transport` trait family + MoQ on top.
- **Value to us:** that monorepo has sibling backends — a native server
  (`web-transport-quinn`, reachable from a browser via W3C WebTransport) and a
  browser WASM client (`web-transport-wasm`). Use `web-transport-quinn` for
  Path A's gateway instead of hand-rolling HTTP/3. Usable **without** MoQ, but the
  ecosystem assumes MoQ above it. *(Verify exact crate names in the monorepo.)*

### [`n0-computer/iroh-roq`](https://github.com/n0-computer/iroh-roq) — the useful one
- **RoQ = RTP-over-QUIC** ([draft-ietf-avtcore-rtp-over-quic](https://datatracker.ietf.org/doc/draft-ietf-avtcore-rtp-over-quic/)),
  implemented as an iroh protocol. **RoQ ≠ MoQ** — this is the lightweight,
  packet-level, WebRTC-adjacent media path, i.e. exactly the "not MoQ" option.
- Gives us, over our hand-rolled `[u32 len]` framing: RTP headers per packet —
  **sequence numbers, timestamps, marker bit (frame boundary), payload type
  (codec)**, SSRC; sessions/flows; datagram-vs-stream media handling. Maps 1:1
  onto WebCodecs (RTP timestamp → chunk timestamp, marker bit → frame boundary,
  payload type → `configure({codec})`).
- **Catch:** Rust-only; browsers don't speak RoQ natively. Terminate RoQ at a
  gateway and hand WebCodecs chunks to the browser, OR run `iroh-roq` inside
  iroh-WASM (gated on iroh WASM maturity + `iroh-roq` building for `wasm32`).

### How they slot in

| | Pipe to browser | Media framing (Rust) | Browser decode |
|---|---|---|---|
| **Path A (gateway)** | `web-transport-quinn` (W3C WebTransport) | **`iroh-roq`** on the iroh link (or keep JPEG framing) | WebCodecs `VideoDecoder` |
| **Path B (iroh-WASM)** | iroh's own browser transport (relay) | `iroh-roq` in WASM (if it builds) | WebCodecs `VideoDecoder` |

## Recommended sequencing

| Phase | Pipe | Decode | Sender change | Gets you |
|---|---|---|---|---|
| **0 (today)** | HTTP multipart | `<img>` | none | works everywhere, no JS |
| **1** | **WebTransport** gateway | `ImageDecoder` (MJPEG) | none | Chrome-native transport + canvas; proves the JS framing parser |
| **2** | WebTransport gateway | **`VideoDecoder`** | add `openh264`/`rav1e` + codec metadata (or adopt `iroh-roq`) | real HW decode, ~20× less bandwidth |
| **3** | **iroh-WASM** (relay) | `VideoDecoder` | new ALPN only | true iroh-in-browser, drop the bespoke gateway |

Phase 1 is the cheapest high-signal step: validates the WebTransport pipe and the
JS `read_frame` port against the existing MJPEG sender with **no sender changes**.
Phase 3 is the north star but depends on iroh's browser-WASM maturity.

## Steal-this-now (free, no browser work)

Adopt **RoQ's RTP framing model** as viroh's media wire format even before any
browser work — timestamps + marker bit + payload type. Strict upgrade over
`[u32 len]`, it's the non-MoQ path we want, and it makes the eventual WebCodecs
hop trivial. `iroh-roq` provides it off the shelf on the Rust side today
(CLI sender ↔ receiver), no browser required.

## To verify before building
1. Current **iroh WASM / browser-transport API** — WebSocket-to-relay vs
   WebTransport-to-relay, and the `Endpoint`/relay builder surface in the WASM
   target. Check upstream iroh examples.
2. **`moq-dev/web-transport`** crate names/backends (quinn server, wasm client),
   and that server-initiated uni-streams behave as the sketch assumes.
3. **Cert / UDP-port** setup for HTTP/3 (LE cert over the chosen UDP port; open it
   in the security group).
