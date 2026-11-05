//! `ModalProxy` — routes via the [`SystemProxy`](crate::SystemProxy) (standard,
//! default) or the [`TunProxy`](crate::TunProxy) (stealth), chosen at runtime by
//! a stored setting, with **automatic fallback** to the system proxy if TUN can't
//! start (e.g. the app isn't running with admin/root). That way flipping on
//! stealth never leaves the user with no connection — it degrades to standard and
//! records that it did, so the UI can say so.
//!
//! The engine holds one `Box<dyn Proxy>` for its lifetime, so the mode is a
//! process-global selection (an atomic) read at `enable()` time. The FFI sets it
//! from the stored setting just before each connect; changing it while connected
//! takes effect on the next connect.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use dreamnail_core::ports::{Proxy, Result};

use crate::{SystemProxy, TunProxy};

const MODE_SYSTEM: u8 = 0;
const MODE_TUN: u8 = 1;

static ROUTING_MODE: AtomicU8 = AtomicU8::new(MODE_SYSTEM);
static FELL_BACK: AtomicBool = AtomicBool::new(false);

/// Select stealth (TUN) vs standard (system proxy) routing for the next connect.
pub fn set_tun_routing(enabled: bool) {
    ROUTING_MODE.store(
        if enabled { MODE_TUN } else { MODE_SYSTEM },
        Ordering::SeqCst,
    );
}

/// Whether stealth (TUN) routing is currently selected.
pub fn tun_routing_selected() -> bool {
    ROUTING_MODE.load(Ordering::SeqCst) == MODE_TUN
}

/// True if the last `enable()` was asked for stealth but had to fall back to the
/// system proxy (so the UI can tell the user stealth isn't active).
pub fn routing_fell_back() -> bool {
    FELL_BACK.load(Ordering::SeqCst)
}

/// A `Proxy` that dispatches to either the system proxy or the TUN device.
#[derive(Default)]
pub struct ModalProxy {
    system: SystemProxy,
    tun: TunProxy,
    active_is_tun: bool,
}

impl Proxy for ModalProxy {
    fn enable(&mut self) -> Result<()> {
        FELL_BACK.store(false, Ordering::SeqCst);
        if tun_routing_selected() {
            match self.tun.enable() {
                Ok(()) => {
                    self.active_is_tun = true;
                    Ok(())
                }
                Err(e) => {
                    // A user-requested cancel is NOT a stealth failure: falling
                    // back here would set the system proxy on a connect the user
                    // just abandoned, leaving the OS pointed at a proxy that is
                    // about to be torn down. Propagate instead.
                    if crate::cancel::abort_requested() {
                        self.active_is_tun = false;
                        return Err(e);
                    }
                    // Stealth couldn't start (usually missing admin/root). Fall
                    // back to the system proxy so the user still connects, and
                    // flag it so the UI can surface "using standard routing".
                    FELL_BACK.store(true, Ordering::SeqCst);
                    self.active_is_tun = false;
                    self.system.enable()
                }
            }
        } else {
            self.active_is_tun = false;
            self.system.enable()
        }
    }

    fn disable(&mut self) {
        // Clear BOTH backends unconditionally. Disconnect must never leave the
        // system proxy set OR a TUN device/route behind — regardless of which
        // one was active, or whether `active_is_tun` drifted out of sync with
        // reality (a mid-connect fallback, a crash-recovered session, a reconnect
        // that switched modes). Both disables are idempotent no-ops when their
        // backend isn't currently set: `TunProxy::disable` only acts if a helper
        // is live, and `SystemProxy::disable` just re-asserts "no proxy" (gsettings
        // mode none / networksetup off / ProxyEnable 0), which is harmless when
        // no proxy was set. Belt-and-suspenders beats a stranded route/proxy.
        self.tun.disable();
        self.system.disable();
        self.active_is_tun = false;
    }

    fn alive(&mut self) -> bool {
        !self.active_is_tun || self.tun.alive()
    }
}
