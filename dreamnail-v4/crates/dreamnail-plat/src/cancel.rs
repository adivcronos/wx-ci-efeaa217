//! A process-wide "abandon the current bring-up" flag.
//!
//! The engine runs on ONE actor thread, so a connect that is deep inside a slow
//! step (SSH handshake, waiting for Xray's SOCKS port, or — worst — waiting at the
//! stealth elevation prompt) owns that thread for as long as the step takes. A
//! user pressing Disconnect/Cancel in the meantime would otherwise sit behind it.
//!
//! The FFI raises this flag the instant a disconnect is requested; every bounded
//! wait loop in this crate polls it and gives up early, so the queued disconnect
//! reaches the actor in seconds instead of minutes. It is advisory only: nothing
//! is force-killed, each step still unwinds through its normal teardown path.

use std::sync::atomic::{AtomicBool, Ordering};

static ABORT: AtomicBool = AtomicBool::new(false);

/// Ask any in-flight bring-up to stop waiting and unwind.
pub fn request_abort() {
    ABORT.store(true, Ordering::SeqCst);
}

/// Clear the flag — called at the start of every connect.
pub fn clear_abort() {
    ABORT.store(false, Ordering::SeqCst);
}

/// Whether a bring-up in progress has been asked to give up.
#[must_use]
pub fn abort_requested() -> bool {
    ABORT.load(Ordering::SeqCst)
}
