//! The orchestrator: drives the ports through the connection lifecycle and keeps
//! [`State`] in sync. All the "what order do we bring things up, and how do we
//! recover" logic lives here — over traits, so it's fully testable with fakes.

use crate::ports::{Credentials, EngineError, Proxy, Result, Router, Tunnel, Xray};
use crate::state::{Event, State};

pub struct Engine {
    state: State,
    tunnel: Box<dyn Tunnel>,
    xray: Box<dyn Xray>,
    proxy: Box<dyn Proxy>,
    router: Box<dyn Router>,
    /// The server's public IP; a connection is only "Connected" once traffic is
    /// verified to exit here (the anti-"connected but not routing" guard).
    expected_ip: String,
    /// Kept so the watchdog can silently re-establish after a drop.
    last_creds: Option<Credentials>,
    /// Successful watchdog recoveries so far. `poll` goes Connected →
    /// Reconnecting → Connected inside ONE call, so callers can't see the drop in
    /// `state()`; they watch this counter instead.
    recoveries: u64,
}

impl Engine {
    pub fn new(
        tunnel: Box<dyn Tunnel>,
        xray: Box<dyn Xray>,
        proxy: Box<dyn Proxy>,
        router: Box<dyn Router>,
        expected_ip: impl Into<String>,
    ) -> Self {
        Self {
            state: State::Disconnected,
            tunnel,
            xray,
            proxy,
            router,
            expected_ip: expected_ip.into(),
            last_creds: None,
            recoveries: 0,
        }
    }

    /// How many times the watchdog has silently re-established a dropped link.
    #[must_use]
    pub fn recoveries(&self) -> u64 {
        self.recoveries
    }

    #[must_use]
    pub fn state(&self) -> State {
        self.state
    }

    /// Connect. Idempotent: a request while already active is a no-op. On any
    /// bring-up failure everything is torn back down and we return to
    /// Disconnected, so a failed attempt never leaves a half-open proxy behind.
    pub fn connect(&mut self, creds: Credentials) -> Result<()> {
        if self.state.is_active() {
            return Ok(());
        }
        self.last_creds = Some(creds.clone());
        self.state = self.state.next(Event::ConnectRequested); // -> Connecting
        match self.bring_up(&creds) {
            Ok(()) => {
                self.state = self.state.next(Event::RouteVerified); // -> Connected
                Ok(())
            }
            Err(e) => {
                self.tear_down();
                self.state = self.state.next(Event::Failed); // -> Disconnected
                Err(e)
            }
        }
    }

    /// Disconnect. Best-effort teardown; always lands in Disconnected.
    pub fn disconnect(&mut self) {
        self.tear_down();
        self.state = self.state.next(Event::DisconnectRequested);
        self.last_creds = None;
    }

    /// Watchdog tick — call periodically. If a live link has dropped, moves to
    /// Reconnecting and tries once to re-establish; a failed reconnect returns to
    /// Disconnected (the caller decides on backoff/retry). No-op otherwise.
    pub fn poll(&mut self) -> Result<()> {
        if self.state == State::Connected && !self.link_healthy() {
            self.state = self.state.next(Event::Dropped); // -> Reconnecting
            let creds = match self.last_creds.clone() {
                Some(c) => c,
                None => {
                    self.state = self.state.next(Event::Failed);
                    return Err(EngineError::Tunnel("link dropped, no creds".into()));
                }
            };
            match self.bring_up(&creds) {
                Ok(()) => {
                    self.state = self.state.next(Event::RouteVerified); // -> Connected
                    self.recoveries += 1;
                    Ok(())
                }
                Err(e) => {
                    self.tear_down();
                    self.state = self.state.next(Event::Failed); // -> Disconnected
                    Err(e)
                }
            }
        } else {
            Ok(())
        }
    }

    /// Whether an unexpected drop can be auto-recovered: we still hold the
    /// credentials from the last connect AND we are no longer active. A user
    /// `disconnect()` clears the credentials, so this is false after a deliberate
    /// disconnect — only a watchdog give-up (which leaves the creds in place)
    /// leaves something to retry. The FFI watchdog checks this to decide whether
    /// to arm its backoff loop.
    #[must_use]
    pub fn can_retry(&self) -> bool {
        self.last_creds.is_some() && !self.state.is_active()
    }

    /// Re-establish after an unexpected drop, using the remembered credentials.
    ///
    /// Unlike [`poll`] (one-shot, run while Connected), this is the caller's
    /// backoff-retry entry point: the FFI watchdog calls it on an increasing
    /// delay while auto-reconnect is enabled, so a laptop wake or a Wi-Fi switch
    /// — where the first attempt usually fails because the network isn't up yet —
    /// keeps retrying instead of giving up after a single try.
    ///
    /// Returns `Ok(true)` when it reconnected, `Ok(false)` when there was nothing
    /// to do (already active, or the user disconnected so no creds remain), and
    /// `Err` when this attempt failed (the caller schedules the next one).
    pub fn retry(&mut self) -> Result<bool> {
        if self.state.is_active() {
            return Ok(false);
        }
        let creds = match self.last_creds.clone() {
            Some(c) => c,
            None => return Ok(false), // user disconnected — nothing to retry
        };
        // Enter Reconnecting directly: the pure machine has no (Disconnected,
        // event) edge into it, and conceptually this is a continuation of the
        // dropped session rather than a brand-new connect.
        self.state = State::Reconnecting;
        match self.bring_up(&creds) {
            Ok(()) => {
                self.state = self.state.next(Event::RouteVerified); // -> Connected
                self.recoveries += 1;
                Ok(true)
            }
            Err(e) => {
                self.tear_down();
                self.state = self.state.next(Event::Failed); // Reconnecting -> Disconnected
                Err(e)
            }
        }
    }

    /// Is the live path still intact? EVERY part must be: the SSH tunnel can
    /// die without Xray noticing, Xray can die without the tunnel noticing, and
    /// the stealth TUN helper can die with both still up — and any one alone
    /// means traffic has silently stopped routing (or is leaking direct).
    fn link_healthy(&mut self) -> bool {
        self.tunnel.alive() && self.xray.alive() && self.proxy.alive()
    }

    /// Open tunnel → start Xray → set proxy → verify the exit IP. Ordered so the
    /// proxy is only pointed at Xray once Xray is up, and only declared good once
    /// traffic is proven to exit via the server.
    fn bring_up(&mut self, creds: &Credentials) -> Result<()> {
        // DREAMNAIL_TIMING=1 prints how long each bring-up step took (stderr).
        let timing = std::env::var_os("DREAMNAIL_TIMING").is_some();
        let mut t = std::time::Instant::now();
        let mut lap = |step: &str| {
            if timing {
                eprintln!("[timing] {step}: {} ms", t.elapsed().as_millis());
            }
            t = std::time::Instant::now();
        };
        self.tunnel.open(creds)?;
        lap("tunnel");
        self.xray.start(creds)?;
        lap("xray");
        self.proxy.enable()?;
        lap("proxy");
        let ip = self.router.exit_ip()?;
        lap("verify");
        if ip != self.expected_ip {
            return Err(EngineError::RouteMismatch {
                expected: self.expected_ip.clone(),
                got: ip,
            });
        }
        Ok(())
    }

    /// Reverse order of bring-up, each step best-effort — teardown must never
    /// itself fail (a stranded proxy is worse than a noisy log).
    fn tear_down(&mut self) {
        self.proxy.disable();
        self.xray.stop();
        self.tunnel.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{Proxy, Router, Tunnel, Xray};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A shared fake "world" all the fake ports read/write, so a test can set up
    /// failures and inspect the resulting side effects.
    #[derive(Default)]
    struct World {
        open_fail: Option<EngineError>,
        xray_fail: Option<EngineError>,
        exit_ip: String,
        tunnel_alive: bool,
        tunnel_open: bool,
        xray_on: bool,
        proxy_on: bool,
    }
    type Shared = Rc<RefCell<World>>;

    struct FTun(Shared);
    impl Tunnel for FTun {
        fn open(&mut self, _c: &Credentials) -> Result<()> {
            if let Some(e) = self.0.borrow().open_fail.clone() {
                return Err(e);
            }
            let mut w = self.0.borrow_mut();
            w.tunnel_open = true;
            w.tunnel_alive = true;
            Ok(())
        }
        fn close(&mut self) {
            let mut w = self.0.borrow_mut();
            w.tunnel_open = false;
            w.tunnel_alive = false;
        }
        fn alive(&self) -> bool {
            self.0.borrow().tunnel_alive
        }
    }

    struct FXray(Shared);
    impl Xray for FXray {
        fn start(&mut self, _creds: &Credentials) -> Result<()> {
            if let Some(e) = self.0.borrow().xray_fail.clone() {
                return Err(e);
            }
            self.0.borrow_mut().xray_on = true;
            Ok(())
        }
        fn stop(&mut self) {
            self.0.borrow_mut().xray_on = false;
        }
        fn alive(&mut self) -> bool {
            self.0.borrow().xray_on
        }
    }

    struct FProxy(Shared);
    impl Proxy for FProxy {
        fn enable(&mut self) -> Result<()> {
            self.0.borrow_mut().proxy_on = true;
            Ok(())
        }
        fn disable(&mut self) {
            self.0.borrow_mut().proxy_on = false;
        }
        fn alive(&mut self) -> bool {
            self.0.borrow().proxy_on
        }
    }

    struct FRouter(Shared);
    impl Router for FRouter {
        fn exit_ip(&self) -> Result<String> {
            Ok(self.0.borrow().exit_ip.clone())
        }
    }

    fn engine_with(world: &Shared, expected: &str) -> Engine {
        Engine::new(
            Box::new(FTun(world.clone())),
            Box::new(FXray(world.clone())),
            Box::new(FProxy(world.clone())),
            Box::new(FRouter(world.clone())),
            expected,
        )
    }

    fn creds() -> Credentials {
        Credentials {
            username: "ad".into(),
            password: "pw".into(),
        }
    }

    #[test]
    fn connect_success_sets_up_everything() {
        let w: Shared = Rc::new(RefCell::new(World {
            exit_ip: "66.226.144.136".into(),
            ..Default::default()
        }));
        let mut e = engine_with(&w, "66.226.144.136");
        assert!(e.connect(creds()).is_ok());
        assert_eq!(e.state(), State::Connected);
        let w = w.borrow();
        assert!(w.tunnel_open && w.xray_on && w.proxy_on);
    }

    #[test]
    fn auth_failure_tears_down_and_disconnects() {
        let w: Shared = Rc::new(RefCell::new(World {
            open_fail: Some(EngineError::Auth),
            ..Default::default()
        }));
        let mut e = engine_with(&w, "66.226.144.136");
        assert_eq!(e.connect(creds()), Err(EngineError::Auth));
        assert_eq!(e.state(), State::Disconnected);
        let w = w.borrow();
        assert!(!w.proxy_on && !w.xray_on);
    }

    #[test]
    fn route_mismatch_is_rejected_and_cleaned_up() {
        let w: Shared = Rc::new(RefCell::new(World {
            exit_ip: "9.9.9.9".into(), // NOT the server
            ..Default::default()
        }));
        let mut e = engine_with(&w, "66.226.144.136");
        let r = e.connect(creds());
        assert!(matches!(r, Err(EngineError::RouteMismatch { .. })));
        assert_eq!(e.state(), State::Disconnected);
        assert!(
            !w.borrow().proxy_on,
            "proxy must be cleared on route mismatch"
        );
    }

    #[test]
    fn disconnect_clears_the_proxy() {
        let w: Shared = Rc::new(RefCell::new(World {
            exit_ip: "66.226.144.136".into(),
            ..Default::default()
        }));
        let mut e = engine_with(&w, "66.226.144.136");
        e.connect(creds()).unwrap();
        e.disconnect();
        assert_eq!(e.state(), State::Disconnected);
        assert!(!w.borrow().proxy_on);
    }

    #[test]
    fn watchdog_reconnects_after_a_drop() {
        let w: Shared = Rc::new(RefCell::new(World {
            exit_ip: "66.226.144.136".into(),
            ..Default::default()
        }));
        let mut e = engine_with(&w, "66.226.144.136");
        e.connect(creds()).unwrap();
        w.borrow_mut().tunnel_alive = false; // the link drops
        assert!(e.poll().is_ok());
        assert_eq!(e.state(), State::Connected); // silently recovered
    }

    #[test]
    fn watchdog_gives_up_when_reconnect_fails() {
        let w: Shared = Rc::new(RefCell::new(World {
            exit_ip: "66.226.144.136".into(),
            ..Default::default()
        }));
        let mut e = engine_with(&w, "66.226.144.136");
        e.connect(creds()).unwrap();
        {
            let mut b = w.borrow_mut();
            b.tunnel_alive = false;
            b.open_fail = Some(EngineError::Tunnel("server down".into()));
        }
        assert!(e.poll().is_err());
        assert_eq!(e.state(), State::Disconnected);
        assert!(!w.borrow().proxy_on);
    }

    #[test]
    fn watchdog_reconnects_when_xray_dies_but_the_tunnel_lives() {
        // The silent-failure case: Xray exits, the SSH session stays perfectly
        // healthy, so `tunnel.alive()` alone would report a green connection
        // while nothing routes.
        let w: Shared = Rc::new(RefCell::new(World {
            exit_ip: "66.226.144.136".into(),
            ..Default::default()
        }));
        let mut e = engine_with(&w, "66.226.144.136");
        e.connect(creds()).unwrap();
        w.borrow_mut().xray_on = false; // Xray dies; the tunnel is untouched
        assert!(w.borrow().tunnel_alive);
        assert!(e.poll().is_ok());
        assert_eq!(e.state(), State::Connected); // restarted and recovered
        assert!(w.borrow().xray_on);
    }

    #[test]
    fn watchdog_reconnects_when_the_routing_dies_but_the_link_lives() {
        // The stealth-helper case: the TUN helper exits, the tunnel + Xray stay
        // up, and traffic would leak direct under a green "connected".
        let w: Shared = Rc::new(RefCell::new(World {
            exit_ip: "66.226.144.136".into(),
            ..Default::default()
        }));
        let mut e = engine_with(&w, "66.226.144.136");
        e.connect(creds()).unwrap();
        assert_eq!(e.recoveries(), 0);
        w.borrow_mut().proxy_on = false; // routing gone; tunnel + Xray untouched
        assert!(e.poll().is_ok());
        assert_eq!(e.state(), State::Connected);
        assert!(w.borrow().proxy_on, "routing must be re-applied");
        assert_eq!(e.recoveries(), 1);
        assert!(e.poll().is_ok()); // healthy tick: no phantom recovery
        assert_eq!(e.recoveries(), 1);
    }

    #[test]
    fn retry_reconnects_after_the_watchdog_gave_up() {
        // Simulate the wake/network-change case: the link drops, the one-shot
        // poll recovery fails (network not up yet), so the engine lands in
        // Disconnected but KEEPS the creds — then a later retry succeeds.
        let w: Shared = Rc::new(RefCell::new(World {
            exit_ip: "66.226.144.136".into(),
            ..Default::default()
        }));
        let mut e = engine_with(&w, "66.226.144.136");
        e.connect(creds()).unwrap();
        {
            let mut b = w.borrow_mut();
            b.tunnel_alive = false;
            b.open_fail = Some(EngineError::Tunnel("network down".into()));
        }
        assert!(e.poll().is_err()); // one-shot recovery gives up
        assert_eq!(e.state(), State::Disconnected);
        assert!(e.can_retry(), "creds are kept, so a retry is possible");

        // Network is back; the next retry reconnects.
        w.borrow_mut().open_fail = None;
        assert_eq!(e.retry(), Ok(true));
        assert_eq!(e.state(), State::Connected);
        assert!(!e.can_retry(), "no retry needed while connected");
    }

    #[test]
    fn retry_is_a_noop_after_a_user_disconnect() {
        let w: Shared = Rc::new(RefCell::new(World {
            exit_ip: "66.226.144.136".into(),
            ..Default::default()
        }));
        let mut e = engine_with(&w, "66.226.144.136");
        e.connect(creds()).unwrap();
        e.disconnect(); // clears the creds — must NOT auto-reconnect
        assert!(!e.can_retry());
        assert_eq!(e.retry(), Ok(false));
        assert_eq!(e.state(), State::Disconnected);
        assert!(!w.borrow().proxy_on);
    }

    #[test]
    fn retry_keeps_the_creds_when_an_attempt_fails() {
        let w: Shared = Rc::new(RefCell::new(World {
            exit_ip: "66.226.144.136".into(),
            open_fail: Some(EngineError::Tunnel("still down".into())),
            ..Default::default()
        }));
        let mut e = engine_with(&w, "66.226.144.136");
        // Prime creds via a failed connect (connect keeps last_creds).
        assert!(e.connect(creds()).is_err());
        assert!(e.can_retry());
        // A failed retry must leave us able to try again (backoff loop).
        assert!(e.retry().is_err());
        assert_eq!(e.state(), State::Disconnected);
        assert!(e.can_retry());
    }

    #[test]
    fn connect_is_idempotent_while_active() {
        let w: Shared = Rc::new(RefCell::new(World {
            exit_ip: "66.226.144.136".into(),
            ..Default::default()
        }));
        let mut e = engine_with(&w, "66.226.144.136");
        e.connect(creds()).unwrap();
        assert!(e.connect(creds()).is_ok());
        assert_eq!(e.state(), State::Connected);
    }
}
