# Receiver access / auth options for viroh senders

**Status: design notes only — none of this is implemented yet (on hold).**

Today a `viroh-sender` streams to **anyone who connects with its node id**. The node
id is a public key (not secret), so being discoverable and being watchable are the
same thing. This doc captures the options for adding *access control on a sender*
— i.e. restricting **who may watch a given stream** — for when we decide to build it.

## Key fact this all rests on

Every iroh connection is **mutually authenticated** with the node keypairs (TLS, no
CA). So when a sender accepts a connection, **before it sends a single frame**, it
already knows the **cryptographically verified node id of the receiver** via
`conn.remote_id()` — and it can't be forged. Every option below hangs off that hook:
make an authorization decision at accept time, and `conn.close(...)` (or refuse to
open the video stream) for anyone who fails.

> Access control is **orthogonal to discovery**. A sender can be listed in the public
> n0 directory (or our own `directory.viroh.net`) *and* still gate who may watch. The
> directory is the phonebook; the access check is the bouncer at the sender's door.

---

## Option A — Allow-list by receiver node id (recommended for controlled viewers)

Authorization **by identity**. The sender holds a set of approved receiver node ids;
on accept it checks `conn.remote_id()` and closes the connection if it isn't in the
set, before streaming anything.

- **Pros:** strongest. No shared secret to leak — authorization is a public key, and
  only the holder of the matching private key can present it. Revoking one viewer
  (drop their id) doesn't affect others. This is the "iroh-native" approach —
  effectively SSH `authorized_keys` for *who may watch*.
- **Cons / what it needs:** each receiver needs a **stable identity**. Today the
  receiver generates a throwaway key per run (its id changes each launch), so we'd
  add key persistence (e.g. `--identity ~/.viroh/key`, generated once). Then the
  sender takes `--allow <receiver-id>` (repeatable) or an allow-list file.
- **Fits:** viewers you control (your machines, teammates), a fleet you administer.

## Option B — Shared viewer token / password (simplest to distribute)

Authorization **by secret**. After connecting, the receiver must present a token
before the sender streams. Small protocol tweak: receiver opens a stream and sends
the token first; sender validates, then either starts sending frames or closes.

- **Pros:** dead simple to hand out — one password to whoever should watch
  (`viroh-sender --viewer-token hunter2`, `viroh-receiver --token hunter2 <id>`).
- **Cons:** it's a shared secret — it spreads, and anyone who has it can watch;
  revoking means rotating it for everyone. Mitigate with per-viewer tokens or
  rotation.
- **Note:** this is fine as a *separate* secret from the fleet/admin token — it only
  grants "can watch this stream," never "can control the fleet." Do **not** reuse the
  admin token here.
- **Fits:** ad-hoc "here's the password" sharing.

## Option C — Signed capability tickets (scalable, advanced)

Authorization **by grant**. The sender (or an issuer you run) hands out short-lived,
**signed** tickets — e.g. "this bearer may watch agent X until 5pm" — and receivers
present the ticket; the sender verifies the signature. No per-viewer list to
maintain, and grants expire on their own (think signed URLs / JWT).

- **Pros:** scales to many viewers; time-boxed access; no central allow-list to edit.
- **Cons:** most to build (issuer, signing keys, verification, expiry/clock).
- **Fits:** handing out access at scale, or time-limited share links. Overkill until
  there's a real need.

## Option D — Secret ALPN (don't rely on it)

Make the ALPN string a secret so the QUIC handshake fails for anyone who doesn't know
it. **Weak:** ALPN isn't really confidential and it's all-or-nothing. Treat it as a
thin obscurity layer at most, never as real access control.

---

## Recommendation matrix

| Goal | Use |
| --- | --- |
| Viewers you control (fleet, teammates) | **A** — node-id allow-list |
| Quick "here's the password" sharing | **B** — viewer token |
| Many viewers / time-limited links | **C** — signed tickets |
| Anything | not **D** alone |

A and B **compose** (allow-list known agents, accept a token for guests), and all of
them work whether discovery is via n0's directory or our own.

## Caveat (true for every option)

Because the node id is public, randoms can still *attempt* a connection — they just
get rejected before any video flows (a cheap early `close()`). That's connection-setup
noise, not a data leak; if it ever matters, add rate-limiting on the reject path.

---

*Captured 2026-06-30. Decision: hold off on implementing any of these for now.*
