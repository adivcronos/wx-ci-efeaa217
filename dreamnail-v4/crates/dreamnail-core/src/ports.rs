//! Ports — the traits the engine depends on instead of touching the OS/network.
//!
//! Each is one seam of the old Python backend: the SSH tunnel, the Xray process,
//! the per-OS system proxy, and the route verifier. `dreamnail-plat` provides the
//! real implementations; tests provide fakes. The engine never names a concrete
//! type, so swapping russh, a different proxy backend, or a mock costs nothing.

use core::fmt;

/// Engine-level error. Deliberately simple + owned (no external error crate) so
/// the core stays dependency-free; adapters map their own errors into this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// Credentials rejected by the server.
    Auth,
    /// Could not reach / open the SSH tunnel.
    Tunnel(String),
    /// Local Xray failed to start or become ready.
    Xray(String),
    /// Could not set the OS proxy.
    Proxy(String),
    /// Connected, but traffic is NOT exiting via the server (route mismatch).
    RouteMismatch { expected: String, got: String },
    /// An adapter isn't implemented yet (Phase-A stubs).
    Unsupported(&'static str),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::Auth => write!(f, "invalid credentials"),
            EngineError::Tunnel(m) => write!(f, "tunnel error: {m}"),
            EngineError::Xray(m) => write!(f, "local service error: {m}"),
            EngineError::Proxy(m) => write!(f, "proxy error: {m}"),
            EngineError::RouteMismatch { expected, got } => {
                write!(f, "traffic not routing: expected {expected}, got {got}")
            }
            EngineError::Unsupported(what) => write!(f, "not implemented: {what}"),
        }
    }
}

impl std::error::Error for EngineError {}

pub type Result<T> = core::result::Result<T, EngineError>;

/// What the customer types in the Connect view (username + password). The VLESS
/// UUID is DERIVED from the username server-side compatibly — see V4_PLAN.md; the
/// derivation itself is ported in a later task and must stay byte-identical.
#[derive(Debug, Clone)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// The SSH tunnel (local port → server VLESS inbound). Real impl: the `russh`
/// crate. `alive` is what the watchdog polls to detect a dropped link.
pub trait Tunnel {
    fn open(&mut self, creds: &Credentials) -> Result<()>;
    fn close(&mut self);
    fn alive(&self) -> bool;
}

/// The local Xray (VLESS) client process. Real impl: spawn the bundled Xray
/// binary and poll its SOCKS inbound for readiness (we do NOT reimplement VLESS).
///
/// `start` takes the credentials because the VLESS outbound id is DERIVED from
/// the username (uuid5); the adapter does that derivation so the core never
/// needs to know the protocol detail.
pub trait Xray {
    fn start(&mut self, creds: &Credentials) -> Result<()>;
    fn stop(&mut self);
    /// Whether the local Xray is still running. The watchdog polls this exactly
    /// like `Tunnel::alive`: Xray dying (crashed, killed, port stolen) leaves the
    /// SSH tunnel perfectly healthy, so without this check the engine reports a
    /// green connection while nothing routes. Defaults to `true` for adapters
    /// that can't tell (fakes, stubs); the real process adapter reaps the child.
    fn alive(&mut self) -> bool {
        true
    }
}

/// The OS system proxy. Real impls are per-OS in `dreamnail-plat` (Windows
/// registry / macOS networksetup / Linux gsettings+kwriteconfig).
pub trait Proxy {
    fn enable(&mut self) -> Result<()>;
    fn disable(&mut self);
    /// Whether the routing set by `enable` is still in place. The system proxy
    /// can't die on its own (defaults to `true`), but the stealth TUN helper is a
    /// separate privileged process: if it exits mid-session the TUN device and
    /// its routes vanish and traffic silently goes direct while the tunnel +
    /// Xray still look healthy. The watchdog polls this like the other halves.
    fn alive(&mut self) -> bool {
        true
    }
}

/// Proves traffic actually exits via the server: fetches our public IP THROUGH
/// the local proxy and returns it, so the engine can compare it to the expected
/// server IP (the v2 `verify_proxy` idea, kept as a first-class port).
pub trait Router {
    fn exit_ip(&self) -> Result<String>;
}
