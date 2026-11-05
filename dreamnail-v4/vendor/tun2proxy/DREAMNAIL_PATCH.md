# Dreamnail patch to tun2proxy 0.8.3

Vendored from crates.io tun2proxy 0.8.3; wired in via `[patch.crates-io]` in
the workspace `Cargo.toml`. Only `src/lib.rs::handle_udp_gateway_session` (and
the new helper `recv_owned`) differ from upstream.

**Bug:** the UDP-gateway session loop re-created its receive future on every
`tokio::select!` iteration. When an outgoing packet won the race while an
incoming gateway frame was half-read, the partial frame was discarded and the
TCP stream desynced → the session died with a recv error and the next one left
from a new server-side source port. Constant two-way traffic (games) hit this
within seconds; request/response UDP (DNS) never did. Roblox: joins stalled or
kicked.

**Fix:** keep one receive future alive across iterations (it owns the reader and
returns it with each frame); never pool a stream whose receive was in flight;
ignore stray keepalive replies instead of tearing the session down.

## Patch 2 — "game lane" (v5.3.9)

When the udpgw address is loopback (the app's SSH local-forward), the
gateway TCP stream is dialled directly rather than through the SOCKS proxy.
Game UDP then bypasses Xray + mux and travels on its own dedicated SSH
connection (see dreamnail-plat tunnel.rs), so bulk downloads can't queue in
front of it. `DREAMNAIL_UDPGW_VIA_PROXY=1` restores upstream behaviour.
