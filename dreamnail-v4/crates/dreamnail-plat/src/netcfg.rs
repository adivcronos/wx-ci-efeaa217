//! Network constants — the Rust mirror of the Python backend's config.py values.
//!
//! Kept in one place so the tunnel, Xray config, proxy and route-verifier all
//! agree on the same ports/hosts, exactly as the Python side did. If any of
//! these drift from config.py, connections break in subtle ways.

/// Local SOCKS inbound the OS proxy points at.
pub const SOCKS_HOST: &str = "127.0.0.1";
pub const SOCKS_PORT: u16 = 11080;
/// Local HTTP inbound (for HTTP-only apps and the route probe).
pub const HTTP_HOST: &str = "127.0.0.1";
pub const HTTP_PORT: u16 = 11081;
/// Local end of the SSH tunnel — Xray's VLESS outbound dials this.
pub const TUNNEL_LOCAL_HOST: &str = "127.0.0.1";
pub const TUNNEL_LOCAL_PORT: u16 = 10809;
/// UDP gateway (udpgw): tunnels UDP over TCP so apps/games that need UDP work
/// even where SOCKS UDP-associate is broken (notably macOS, os error 56
/// EISCONN). The server runs tun2proxy's `udpgw-server` bound to loopback; the
/// TUN helper points tun2proxy at UDPGW_LOCAL, reached via a SECOND SSH forward
/// (local UDPGW_LOCAL_PORT → server UDPGW_REMOTE_PORT). Local port is distinct
/// from the remote (mirrors the 10809/10808 split) so it can't clash on the box.
pub const UDPGW_LOCAL_HOST: &str = "127.0.0.1";
pub const UDPGW_LOCAL_PORT: u16 = 10730;
pub const UDPGW_REMOTE_HOST: &str = "127.0.0.1";
pub const UDPGW_REMOTE_PORT: u16 = 7300;
/// WebSocket path for the "looks like a website" backup door (Via::Ws): the app
/// connects over TLS on 443 and sends `GET WS_PATH` with an Upgrade header, so a
/// filter that inspects HTTPS sees an ordinary long-lived web request. nginx
/// routes this one path to the WebSocket terminator (server/dreamnail-ws.py),
/// which unwraps the frames into sshd. Not a secret (it ships in the client);
/// the real auth is still the SSH login. MUST match the nginx `location`.
pub const WS_PATH: &str = "/live/ts/v1";
/// Host for the WebSocket door: the Cloudflare-proxied domain. The door now
/// connects to THIS name (which resolves to Cloudflare's anycast IPs, not the
/// origin) with a matching TLS SNI, so Cloudflare forwards the WebSocket to the
/// server. That makes this door survive a network that IP-blocks the origin
/// (it looks like ordinary HTTPS to a huge shared Cloudflare address), while the
/// SSH (:22) and Reality (:443) doors still hit the origin IP directly.
pub const WS_HOST: &str = "penguinwalker.dev";
/// VLESS + REALITY "looks like a real site" door (the Reality fallback, tried
/// when every SSH door is unreachable). The client's TLS handshake borrows the
/// decoy site below, so a DPI box that inspects 443 sees that real site, not a
/// VPN. The PUBLIC key is safe to ship — the matching PRIVATE key never leaves
/// the server (/etc/dreamnail/reality.json). Decoy must support TLS1.3+X25519
/// and match the server's reality `target`/serverNames.
/// NOTE: this cannot pass an allow-list proxy that forwards a name to the REAL
/// site (it never reaches our server then); it defeats DPI that still routes to
/// the dialled IP. `DREAMNAIL_REALITY=0` disables it; `DREAMNAIL_DOOR=reality`
/// forces it.
pub const REALITY_PORT: u16 = 443;
/// The decoy sites the Reality door can impersonate, as (label, SNI). The client
/// presents one site's name; the server routes that name (nginx SNI map) to a
/// Reality inbound whose `dest` is that same real site, so the borrowed cert
/// always matches. Pick the one a given network allows (e.g. a school that
/// permits Compass/Teams). All must support TLS1.3 + X25519. Keep this list in
/// sync with the server's reality inbounds + nginx map.
pub const REALITY_DECOYS: &[(&str, &str)] = &[
    ("Desmos", "www.desmos.com"),
    ("Microsoft Teams", "teams.microsoft.com"),
    ("Compass", "compass.education"),
    ("Outlook", "outlook.office365.com"),
];
/// Default decoy SNI (first in the list) when the user hasn't picked one.
pub const REALITY_SNI: &str = REALITY_DECOYS[0].1;
/// Whether `sni` is one of our known decoys (guards the saved preference).
#[must_use]
pub fn is_known_decoy(sni: &str) -> bool {
    REALITY_DECOYS.iter().any(|(_, s)| *s == sni)
}
// One Reality keypair + shortId is shared across every decoy inbound (the key
// only authenticates the client; reusing it across decoys is fine). The PUBLIC
// key ships; the PRIVATE key stays server-only.
pub const REALITY_PUBKEY: &str = "K1tVqQDRb2eGp6rUsvAPR5tHcF0Z1K_3CNVGgA4iNSQ";
pub const REALITY_SHORTID: &str = "a1b2c3d4e5f60718";
pub const REALITY_FINGERPRINT: &str = "chrome";
/// Namespace for deriving a customer's VLESS UUID from their username.
/// MUST stay byte-identical to config.UUID_NAMESPACE / admin/uuid_gen.py.
pub const UUID_NAMESPACE: &str = "__UUID_NAMESPACE__";
/// Where the route verifier asks "what's my public IP" (through the proxy).
// The /ip path returns the bare IP as plain text to ANY client; the root path
// serves a full HTML page to non-curl user-agents (reqwest included), which then
// fails the exact-match exit-IP compare even when routing is correct.
pub const PROXY_VERIFY_URL: &str = "https://ifconfig.me/ip";
/// Fast route check on OUR server (nginx `location = /ip`, plain HTTP): it only
/// answers requests arriving from the server itself — i.e. through the VPN —
/// with the bare IP, and 404s everyone else. ~12 ms vs ~300 ms for ifconfig.me
/// (no TLS, no third party). The verifier falls back to `PROXY_VERIFY_URL` if
/// this doesn't yield an IP (older server, or routing really is broken).
pub const FAST_VERIFY_URL: &str = "http://66.226.144.136/ip";
/// The server's IP — a connection is only "routing" if traffic exits here.
/// Stays the raw IP: the egress check compares the observed exit IP to this.
pub const EXPECTED_EXIT_IP: &str = "66.226.144.136";
/// Host for the client's HTTPS calls to the server — plan sync (`/api/me`),
/// problem reports (`/api/report`), and anonymous telemetry (`/ingest`). The
/// Cloudflare-proxied domain, so these calls go through Cloudflare's shared
/// IPs and keep working even where the origin IP is blocked (they run outside
/// the tunnel). Distinct from EXPECTED_EXIT_IP, which must stay the raw IP for
/// the egress check.
pub const API_HOST: &str = "penguinwalker.dev";
/// Over-the-air (internet) update manifest — the SAME `latest.json` that
/// `ship_v4.py` writes beside the installers on the download site. The OTA
/// updater fetches it over HTTPS, and downloads whichever installer it names
/// from `base_url` (also in the manifest). This is the one place the OTA source
/// lives; point it at a real domain here if the server ever gets one.
/// Honest note: a web-DOWNLOADED installer is unsigned, so Windows SmartScreen /
/// macOS Gatekeeper still warn until the builds are code-signed. Linux is clean.
pub const UPDATE_MANIFEST_URL: &str = "https://penguinwalker.dev/downloads/v4/latest.json";
/// Running app version — the USB updater compares this to a build on the USB.
///
/// Read at COMPILE TIME from the Flutter `config.dart` by this crate's build.rs,
/// which is the same file CI reads to name every artifact. There is nothing to
/// keep in step by hand: bump `AppConfig.appVersion` and this follows.
pub const APP_VERSION: &str = env!("DREAMNAIL_APP_VERSION");

/// The operator's ed25519 PUBLIC key (hex) for verifying signed remote-control
/// directives (`/api/me`) and the OTA update manifest. Safe to ship — the
/// matching PRIVATE key never leaves the server / operator build box. A client
/// only acts on a directive (wipe / rollback / forced update) or installs an OTA
/// build if the signature over it verifies against THIS key, so a network doing
/// TLS interception with its own root cert can rewrite the JSON but cannot forge
/// an action. If a signature is missing or bad the client fails safe: it ignores
/// the dangerous directive / refuses the update — never the reverse.
pub const SIGN_PUBKEY_HEX: &str =
    "9e28f8030b4fe434efb3b65832a6f1b441fc4ebad9896163213b43513f807421";

/// Decode a hex string to bytes (no external dep). `None` on odd length or a
/// non-hex digit. Used for the signing key + signatures.
#[must_use]
pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if b.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(b.len() / 2);
    let mut i = 0;
    while i < b.len() {
        let hi = (b[i] as char).to_digit(16)?;
        let lo = (b[i + 1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
        i += 2;
    }
    Some(out)
}

/// Verify an ed25519 `signature` over `msg` against the baked-in operator public
/// key. `false` on any decode/verify failure (so callers fail safe).
#[must_use]
pub fn verify_operator_sig(msg: &[u8], signature: &[u8]) -> bool {
    let Some(pk) = hex_decode(SIGN_PUBKEY_HEX) else {
        return false;
    };
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &pk)
        .verify(msg, signature)
        .is_ok()
}
/// Default for the BROWSING outbound's multiplexing (the automatic mux switch,
/// see xray::build_xray_config). ON = ordinary pages reuse one tunnel connection
/// for their dozens of small requests, skipping the ~200 ms per-request setup —
/// the fix for "pages load slow but the speed test is fast". Known-good with this
/// server. Bulk transfers use a separate NON-muxed outbound, so browsing being
/// muxed onto one SSH stripe costs them nothing.
pub const MUX_ENABLED: bool = true;

/// Destinations treated as BULK / sustained transfers: speed tests and the big
/// download & streaming CDNs. The automatic mux switch routes these to a
/// non-muxed VLESS outbound, so each of their connections spreads across the SSH
/// stripes for full throughput, while everything else (ordinary browsing) rides
/// the muxed outbound. Chosen per destination — no toggle, no mid-transfer
/// restart. `domain:` matches the host and all its subdomains.
pub const BULK_DOMAINS: &[&str] = &[
    // Speed tests (the "is my VPN fast" check)
    "domain:speedtest.net",
    "domain:ooklaserver.net",
    "domain:speed.cloudflare.com",
    "domain:fast.com",
    // Big streaming / download CDNs (sustained, throughput-bound)
    "domain:googlevideo.com",      // YouTube video
    "domain:nflxvideo.net",        // Netflix video
    "domain:steamcontent.com",     // Steam game downloads
    "domain:steamstatic.com",      // Steam CDN
    "domain:windowsupdate.com",    // Windows Update
    "domain:dl.delivery.mp.microsoft.com", // Windows/Store delivery
];
