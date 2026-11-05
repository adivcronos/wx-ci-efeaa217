//! Real-world adapters for the core's ports (the v4 answer to the Python
//! backend's platform code).
//!
//! - [`RusshTunnel`]  — SSH local forward (russh).                [verified live to auth]
//! - [`XrayProcess`]  — spawn Xray-core with the generated config. [Linux-testable]
//! - [`SystemProxy`]  — per-OS system proxy set/clear.            [Linux verified]
//! - [`HttpRouter`]   — route verification through the local proxy.
//! - [`traffic`]      — byte counters behind the live throughput readout.
//! - [`cancel`]       — "the user pressed Disconnect" flag the slow waits poll.
//!
//! Also exposes the pure protocol helpers [`uuid_for_username`] and
//! [`build_xray_config`] (mirrors of config.py), pinned against Python goldens.

pub mod cancel;
pub mod diag;
pub mod elevation;
pub mod entitlements;
pub mod conflict;
pub mod netcfg;
pub mod netid;
pub mod report;
pub mod shortcut;
pub mod traffic;
pub mod updater;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod modal;
mod proxy;
mod router;
mod storage;
mod tunnel;
#[cfg(windows)]
mod winproxy;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod tun;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub mod tun_run;
mod xray;

pub use proxy::SystemProxy;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub use modal::{routing_fell_back, set_tun_routing, tun_routing_selected, ModalProxy};
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub use tun::{stop_stale_helper as stop_stale_tun, TunProxy};
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub use tun_run::{run_blocking as run_tun_blocking, TunCancel, TunRunConfig};
pub use router::HttpRouter;
pub use storage::EncryptedStore;
pub use tunnel::{
    active_port as tunnel_port, force_443, reality_decoy_sni, set_force_443, set_reality_decoy,
    set_weaknet_mode, weaknet_mode, RusshTunnel, TunnelConfig, Via,
};
pub use xray::{build_xray_config, uuid_for_username, XrayProcess};

/// Stop Windows flashing a console window when the GUI app spawns a console
/// subprocess (schtasks, powershell, reg, xray…). `CREATE_NO_WINDOW` makes the
/// child run with no console at all. MUST be applied before spawn on EVERY
/// command the app runs on Windows, or a background/elevation/proxy call pops a
/// black terminal. No-op off Windows.
#[cfg(windows)]
pub(crate) fn hide_console(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}
#[cfg(not(windows))]
#[allow(dead_code)] // only called from #[cfg(windows)] spawn sites
pub(crate) fn hide_console(_cmd: &mut std::process::Command) {}
