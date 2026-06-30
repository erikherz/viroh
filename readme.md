# viroh

Peer-to-peer **synthetic video over [iroh](https://www.iroh.computer)**, rendered
as colored ASCII art in your terminal — plus a web app to run a **fleet** of
video agents and **watch any of them as real video in a browser**.

| binary | role |
| --- | --- |
| `viroh-sender` | synthesizes a 640×480 timecode signal, encodes Motion-JPEG, streams it over iroh |
| `viroh-receiver <node-id>` | dials a sender by its iroh node id, decodes, renders colored half-block ASCII |
| `viroh-fleet` | web UI + JSON API to start/stop/monitor many sender agents on one host, plus a browser gateway (`/sources/<id>`) to watch any agent's real video |

Everything is pure Rust — no C codec libraries, no ffmpeg.

---

## Quick start

```sh
cargo build --release
```

**One sender, one receiver:**

```sh
# terminal 1
./target/release/viroh-sender --name "studio-A"
#   ... prints:  node id: bb95f98…84f73

# terminal 2 (same machine or anywhere on the internet)
./target/release/viroh-receiver bb95f98…84f73
```

Quit the receiver with **`q`**, **Esc**, or **Ctrl-C**; it restores your terminal
on exit. Many receivers can watch one sender at once.

**A fleet, via the web app:**

```sh
./target/release/viroh-fleet --bind 127.0.0.1:8080
# open http://127.0.0.1:8080  → launch / stop / monitor agents
```

---

## How it works

```
 sender                                            receiver
 ──────                                            ────────
 TimecodeSource ─ 640×480 RGB frame
       │
 jpeg-encoder ─── Motion-JPEG ─┐
       │                       │   ① JSON metadata  ② … JPEG frames …
 iroh QUIC uni-stream ─────────┼───────────────────────►  iroh QUIC uni-stream
   (ALPN "viroh/mjpeg/1")      │                              │
                                          zune-jpeg ── decode ─ RGB
                                                │
                                          ASCII renderer ─► terminal
```

- **Codec:** [`jpeg-encoder`](https://crates.io/crates/jpeg-encoder) (encode) +
  [`zune-jpeg`](https://crates.io/crates/zune-jpeg) (decode), both pure Rust. Each
  frame is an independent JPEG — i.e. Motion-JPEG.
- **Synthetic source:** drawn in-process (`src/video.rs`) — a running
  `HH:MM:SS.mmm` timecode, a moving sweep bar, and a label. No input file or
  camera needed. The timecode is rendered with an embedded anti-aliased font
  ([`fontdue`](https://crates.io/crates/fontdue) + JetBrains Mono ExtraBold);
  its strokes are dilated and edge-sharpened on a black panel so the digits stay
  legible after the heavy downsample to terminal cells.
- **Wire protocol** on the uni-stream:
  1. **One metadata message** — length-prefixed JSON `StreamMeta`:
     `{ name, started_at (RFC 3339), kind: "video only", width, height, fps }`.
  2. **Then video frames** — each `[u32 big-endian length][JPEG bytes]`.
- **Renderer:** by default uses Unicode upper-half-block characters (`▀`) — each
  cell carries two vertically-stacked pixels (foreground = top, background =
  bottom), doubling vertical resolution. `--ascii` switches to a brightness-ramp
  character set instead. Either way it's sized to your terminal and colored with
  ANSI escapes (24-bit or 256-color).

### Preview the source without networking

```sh
cargo run --release --example preview -- 100 32 3   # cols rows frames
```

---

## How it connects to the iroh network

This is the interesting part: a receiver dials a sender knowing **only its node
id** — no IP address, no port, even across NATs.

**Identity = a public key.** Every endpoint generates an Ed25519 keypair on
startup. The public key *is* the address you dial — iroh calls it the
`EndpointId` (a.k.a. NodeId), the 64-hex-character string the sender prints. The
connection's TLS is authenticated with these keys directly (no certificate
authority), so connecting to an id guarantees you reached exactly that peer.

To establish a connection iroh needs three things: the **EndpointId**, some way
to **find** it (addressing), and an **ALPN** (here `viroh/mjpeg/1`) so both sides
agree on the protocol.

**Finding a peer — discovery.** We use the `presets::N0` defaults, which turn on:

- **Pkarr publishing.** The sender packages its current reachability — its home
  **relay URL** and any directly-reachable IP\:port candidates — into a signed
  [pkarr](https://pkarr.org) record keyed by its EndpointId, and publishes it to a
  DNS-based directory (number 0's `dns.iroh.link` by default).
- **DNS resolution.** The receiver looks up the sender's EndpointId in that same
  directory to learn how to reach it. This is why the receiver only needs the id.

**Establishing the path — relay then hole-punch.** Each endpoint keeps a
connection to its nearest **relay** server. A new connection is first made
*through* the relay (which forwards encrypted packets by EndpointId and can't read
them), so it works even behind NATs immediately. In parallel, the two peers try to
**hole-punch** a direct UDP path; if that succeeds the QUIC connection silently
migrates off the relay and runs peer-to-peer. If it can't, traffic keeps flowing
over the relay as a fallback. Either way it's the same encrypted QUIC stream to
your code.

So the relay and the DNS directory are the two pieces of shared infrastructure.
By default they're number 0's; below, you run your own — and a **live one is
already running at `https://server.viroh.net`**. Point any binary at it with
`--relay-url https://server.viroh.net` (sender, receiver, and fleet all accept it)
to route through your own relay instead of n0's.

---

## Fleet manager (`viroh-fleet`)

A small [axum](https://crates.io/crates/axum) web app that manages `viroh-sender`
processes on the host it runs on.

- **UI** at `/` — launch an agent (name, fps, quality, resolution), watch status,
  tail logs, stop/start/delete. Each running agent's row offers a **watch ▶** link
  (opens the browser player), **copy link** (the shareable `/sources/<id>` URL), a
  **per-sender color** selector, and **copy cmd** (a ready-to-paste
  `viroh-receiver <id>` with that color baked in).
- **JSON API** under `/api`:

  | method + path | action |
  | --- | --- |
  | `GET /api/agents` | list agents with status, node id, pid, recent logs |
  | `POST /api/agents` | launch an agent (JSON body: `name`, `fps`, `quality`, `width`, `height`) |
  | `POST /api/agents/{id}/stop` | stop (kill) the agent |
  | `POST /api/agents/{id}/start` | relaunch a stopped agent |
  | `DELETE /api/agents/{id}` | stop and forget the agent |
  | `GET /api/agents/{id}/logs` | full captured log buffer |

Each agent is a child `viroh-sender`; the fleet captures its stdout/stderr,
sniffs the node id, and kills it cleanly on stop/delete (and on fleet exit).

### Launching a sender — three ways

| how | managed by fleet? | shows in dashboard? | needs token? | best for |
| --- | --- | --- | --- | --- |
| **Web dashboard** | yes | yes | yes (paste in UI) | point-and-click |
| **Fleet API** (`curl`) | yes | yes | yes | CLI ergonomics + admin visibility |
| **Standalone binary** | no | no | no | quick local tests, throwaway senders |

**1. Web dashboard.** Open the fleet UI, fill in name / fps / quality / resolution,
click **+ launch agent**. The agent appears in the table with its node id, a
**watch ▶** link, and stop/start/delete.
*Downside:* you need the browser UI open.

**2. Fleet API from the shell.** The fleet *spawns and tracks* the sender, so it
behaves exactly like a UI-launched one (appears in the dashboard, stop/start
works) — you just trigger it from the command line:

```sh
# on the fleet host
TOKEN=$(sudo systemctl show viroh-fleet -p Environment | grep -o 'VIROH_FLEET_TOKEN=[^ ]*' | cut -d= -f2)
curl -sk -X POST https://localhost:8443/api/agents \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"name":"cli-test","fps":30,"quality":90,"width":640,"height":480}'
```

The node id comes back `null` for a second or two while iroh resolves its
address, then populates in the dashboard.
*Downside:* needs the token, and only manages senders on the fleet's own host.

**3. Standalone binary.** Run `viroh-sender` directly — no fleet, no token,
anywhere:

```sh
viroh-sender --name "cli-test" --relay-url https://server.viroh.net
#   ... prints its node id; stays in the foreground waiting for receivers
```

The fleet has no handle on this process, so it **won't appear in the dashboard**
and has no managed stop/start — read the node id from its own stdout and stop it
with Ctrl-C. It's still fully watchable: the browser gateway and `viroh-receiver`
both just dial the node id. To keep it alive after logout, detach it:

```sh
nohup viroh-sender --name "cli-test" --relay-url https://server.viroh.net > ~/cli-sender.log 2>&1 &
grep 'node id:' ~/cli-sender.log
```

### Watch in a browser (`/sources/<node-id>`)

The fleet runs its own iroh client endpoint and **bridges a sender's stream to
the browser** — no terminal, no transcoding. Since the wire format already is
Motion-JPEG, the gateway just re-emits the frames as
`multipart/x-mixed-replace`, which an `<img>` renders natively as live video.

| method + path | action |
| --- | --- |
| `GET /sources/{node-id}` | player page (auto-reconnects if the sender drops) |
| `GET /sources/{node-id}/stream` | the live MJPEG bridge the page's `<img>` consumes |

These two routes are **public** (no token) by design: anyone with the URL can
watch, matching iroh's "a node id is discoverable" model. The `/api/*` control
plane stays token-gated. Each viewer is an independent iroh connection, so many
browsers (and terminal receivers) can watch one sender at once.

**Options**

```
--bind <addr>        listen address           (default 0.0.0.0:8080)
--sender-bin <path>  path to viroh-sender     (default: next to viroh-fleet)
--tls-cert <pem>     enable HTTPS (with --tls-key)
--tls-key  <pem>
--token <secret>     require Authorization: Bearer <secret> on /api/*
                     (also reads $VIROH_FLEET_TOKEN; the UI has a token field)
--relay-url <url>    custom iroh relay forwarded to every launched agent
```

**Production example (HTTPS + token), e.g. on server.viroh.net:**

```sh
sudo ./target/release/viroh-fleet \
  --bind 0.0.0.0:8443 \
  --tls-cert /etc/letsencrypt/live/server.viroh.net/fullchain.pem \
  --tls-key  /etc/letsencrypt/live/server.viroh.net/privkey.pem \
  --token "$(openssl rand -hex 16)"
```

> The control API can spawn processes, so always set `--token` (and TLS) when it's
> reachable from the internet. The `/` page itself is unauthenticated so you can
> load it and paste the token into the field.

---

## Self-hosting your own iroh directory + relay

You can run the two shared pieces yourself instead of using number 0's, giving you
a fully independent iroh network. You need a public host with a DNS name and TLS
cert — exactly what `server.viroh.net` has.

1. **DNS directory** ([`iroh-dns-server`](https://crates.io/crates/iroh-dns-server)) —
   the pkarr/DNS server that maps EndpointIds → addresses:

   ```sh
   cargo install iroh-dns-server
   iroh-dns-server --help   # configure the http(s) + DNS ports and your zone
   ```

2. **Relay** ([`iroh-relay`](https://crates.io/crates/iroh-relay) built with the
   `server` feature):

   ```sh
   cargo install iroh-relay --features server
   iroh-relay --help        # point it at the Let's Encrypt cert/key
   ```

3. **Point clients at your infra.** See `deploy/` for the systemd units and config
   used on `server.viroh.net`. (Client-side flags to select a custom relay/DNS are
   on the roadmap; today the servers are configured and run here, and clients use
   the `N0` defaults.)

---

## Tests

```sh
cargo test                       # codec, timecode, framing, metadata round-trips
cargo test -- --include-ignored  # also a live in-process iroh transfer
```

The `iroh_loopback_streams_frames` test brings up two endpoints on `127.0.0.1`,
performs the metadata handshake, streams three frames, and verifies they decode.

---

## Platform notes

Pure Rust + [`crossterm`](https://crates.io/crates/crossterm), so the same code
targets **Linux, macOS, and Windows**. Developed on macOS; Linux is the primary
deployment target.

**Color:** the receiver picks a color mode with `--color auto|truecolor|256|mono`
(default `auto`, which uses 24-bit when `$COLORTERM` advertises it, else 256-color).
24-bit terminals (iTerm2, Ghostty, kitty, WezTerm, Windows Terminal) look best;
**macOS Terminal has no 24-bit support and misrenders truecolor escapes**, so use
`--color 256` (or leave it on `auto`) there. The fleet dashboard has a per-sender
color selector that bakes the right `--color` flag into that agent's "copy cmd".
(The browser player at `/sources/<id>` is true RGB video and unaffected by this.)

## Next step: real capture

The synthetic source is isolated behind one type. To stream a real camera, replace
`TimecodeSource` with a capture source that yields the same `video::Frame` (RGB) —
e.g. V4L2 on Linux via the [`v4l`](https://crates.io/crates/v4l) crate — and the
codec, transport, renderer, and fleet manager stay exactly as-is.
```
