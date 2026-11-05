//! Dreamnail v4 engine core — pure, platform-agnostic, dependency-free.
//!
//! This is the Rust replacement for the Python backend's connection engine. It
//! owns the connection STATE MACHINE and the ORCHESTRATION (open tunnel → start
//! Xray → set proxy → verify route → watch/reconnect), but performs no real I/O
//! itself: every side effect goes through a trait in [`ports`] that the caller
//! injects. That is what lets the whole engine be unit-tested with fakes, with
//! no server, no network, and no OS access — exactly the "testable without a
//! server" principle from V4_PLAN.md.
//!
//! Real implementations of those traits live in `dreamnail-plat`; the headless
//! `dreamnail-cli` and (Phase B) the Flutter FFI layer both drive this engine.

pub mod engine;
pub mod ports;
pub mod state;

pub use engine::Engine;
pub use ports::{Credentials, EngineError, Proxy, Result, Router, Tunnel, Xray};
pub use state::{Event, State};
