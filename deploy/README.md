# Deploying viroh on server.viroh.net

These are the units/configs used to run viroh on the production host
(`server.viroh.net`, Ubuntu, 1 vCPU / 2 GB — add swap before building).

## 0. Build

```sh
git clone https://github.com/erikherz/viroh.git ~/viroh
cd ~/viroh
# 1 vCPU / 2 GB: cap parallelism so the linker doesn't OOM
CARGO_BUILD_JOBS=1 cargo build --release \
  --bin viroh-sender --bin viroh-receiver --bin viroh-fleet
```

## 1. Fleet manager (web UI + API)

Serves HTTPS with the existing Let's Encrypt cert and requires a bearer token.

```sh
sudo cp deploy/viroh-fleet.service /etc/systemd/system/
# set a real token:
sudo sed -i "s/CHANGE_ME/$(openssl rand -hex 16)/" /etc/systemd/system/viroh-fleet.service
sudo systemctl daemon-reload
sudo systemctl enable --now viroh-fleet
sudo systemctl status viroh-fleet --no-pager
```

Open `https://server.viroh.net:8443`, paste the token into the field, and launch
agents. Find the token again with:
`grep VIROH_FLEET_TOKEN /etc/systemd/system/viroh-fleet.service`.

Open the firewall/security-group for **TCP 8443**.

## 2. Self-hosted relay (optional)

Lets your nodes hole-punch / relay through your own server instead of number 0's.
No DNS delegation required — clients just need the relay URL.

```sh
sudo cargo install iroh-relay --features server   # or build from source
sudo cp deploy/iroh-relay.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now iroh-relay
```

Config: `deploy/iroh-relay.config.toml` (binds `:443` HTTPS with the Let's Encrypt
cert, `:80` for plain HTTP services). Open **TCP/UDP 443** and **TCP 80**.

> To make viroh clients *use* this relay you point them at
> `RelayMode::Custom(https://server.viroh.net)` instead of the `N0` preset — a
> `--relay-url` client flag is the planned hook for this.

## 3. Self-hosted DNS directory (optional, needs registrar changes)

The pkarr/DNS discovery directory (`iroh-dns-server`) is what maps an EndpointId
to its addresses so peers can dial by id alone.

```sh
cargo install iroh-dns-server
iroh-dns-server --help     # configure HTTP(S) + DNS ports and the served zone
```

**Important:** a discovery directory is an *authoritative DNS server* for a zone,
so it only works once you delegate a subdomain to this host at your DNS registrar
(an `NS` record pointing the iroh zone at `server.viroh.net`, plus the matching
`A`/`glue`). Until that delegation exists, keep using the `N0` discovery defaults;
the relay in step 2 works without it.
