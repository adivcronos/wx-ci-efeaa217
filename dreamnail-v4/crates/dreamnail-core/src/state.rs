//! The connection state machine — a pure function of (state, event).
//!
//! Mirrors the v1/v2 engine's lifecycle (DISCONNECTED → CONNECTING → CONNECTED
//! → RECONNECTING) but as a total, side-effect-free transition table, so every
//! path is trivially testable and the UI can render `State` directly.

/// Where the connection is right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
}

/// Things that happen TO the connection. The engine emits these as it drives the
/// ports; `State::next` decides the resulting state. Keeping events separate from
/// orchestration is what makes the machine unit-testable in isolation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The user asked to connect.
    ConnectRequested,
    /// Tunnel + Xray + proxy are up AND the exit IP was verified.
    RouteVerified,
    /// A step of bring-up failed (auth, tunnel, xray, or route mismatch).
    Failed,
    /// A previously-good link dropped (watchdog saw the tunnel die).
    Dropped,
    /// The user asked to disconnect.
    DisconnectRequested,
}

impl State {
    /// Total transition function — never panics; unknown (state, event) pairs
    /// leave the state unchanged, so a stray event can't corrupt the machine.
    #[must_use]
    pub fn next(self, event: Event) -> State {
        use Event::*;
        use State::*;
        match (self, event) {
            // A disconnect request always wins, from anywhere.
            (_, DisconnectRequested) => Disconnected,

            (Disconnected, ConnectRequested) => Connecting,
            (Connecting, RouteVerified) => Connected,
            (Connecting, Failed) => Disconnected,

            (Connected, Dropped) => Reconnecting,
            (Reconnecting, RouteVerified) => Connected,
            // Give up after a failed reconnect (the caller may retry with backoff).
            (Reconnecting, Failed) => Disconnected,

            // Anything else is a no-op transition.
            (s, _) => s,
        }
    }

    #[must_use]
    pub fn is_active(self) -> bool {
        matches!(self, State::Connecting | State::Connected | State::Reconnecting)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path() {
        let s = State::Disconnected;
        let s = s.next(Event::ConnectRequested);
        assert_eq!(s, State::Connecting);
        let s = s.next(Event::RouteVerified);
        assert_eq!(s, State::Connected);
        let s = s.next(Event::DisconnectRequested);
        assert_eq!(s, State::Disconnected);
    }

    #[test]
    fn failed_bringup_returns_to_disconnected() {
        assert_eq!(
            State::Connecting.next(Event::Failed),
            State::Disconnected
        );
    }

    #[test]
    fn drop_then_recover() {
        assert_eq!(State::Connected.next(Event::Dropped), State::Reconnecting);
        assert_eq!(
            State::Reconnecting.next(Event::RouteVerified),
            State::Connected
        );
    }

    #[test]
    fn disconnect_wins_from_any_state() {
        for s in [
            State::Disconnected,
            State::Connecting,
            State::Connected,
            State::Reconnecting,
        ] {
            assert_eq!(s.next(Event::DisconnectRequested), State::Disconnected);
        }
    }

    #[test]
    fn stray_events_are_noops() {
        assert_eq!(State::Disconnected.next(Event::Dropped), State::Disconnected);
        assert_eq!(State::Connected.next(Event::ConnectRequested), State::Connected);
    }
}
