//! Live, environment-dependent integration tests. `#[ignore]`d so normal
//! `cargo test` (and CI) skip them; run explicitly with:
//!     cargo test -p dreamnail-plat --test live_linux -- --ignored
//!
//! They touch the real machine: spawn Xray and bind :11080, and flip the system
//! proxy. The proxy test only runs a safe enable→disable cycle.

use std::net::TcpStream;
use std::time::Duration;

use dreamnail_core::ports::{Credentials, Proxy, Xray};
use dreamnail_plat::{SystemProxy, XrayProcess};

/// XrayProcess spawns the real Xray and its SOCKS inbound opens (readiness),
/// even without a live tunnel (the inbound binds regardless of the outbound).
/// Set DREAMNAIL_XRAY to a real xray binary before running.
#[test]
#[ignore = "spawns real Xray + binds :11080"]
fn xray_starts_and_opens_socks() {
    assert!(
        std::env::var("DREAMNAIL_XRAY").is_ok(),
        "set DREAMNAIL_XRAY=/path/to/xray"
    );
    let mut x = XrayProcess::new(Duration::from_secs(8));
    let creds = Credentials {
        username: "livetest".into(),
        password: String::new(),
    };
    x.start(&creds).expect("xray should start and open SOCKS");
    // Prove the inbound is actually accepting.
    TcpStream::connect_timeout(
        &"127.0.0.1:11080".parse().unwrap(),
        Duration::from_secs(2),
    )
    .expect("SOCKS 11080 should accept connections while Xray runs");
    x.stop();
    // After stop, the port should be free again shortly.
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        TcpStream::connect_timeout(&"127.0.0.1:11080".parse().unwrap(), Duration::from_millis(300))
            .is_err(),
        "SOCKS should be gone after stop()"
    );
}

/// SystemProxy enable sets the GNOME proxy to manual→our ports; disable clears
/// it. Only safe to assert-restore when the pre-state was 'none'.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "flips the real system proxy"]
fn system_proxy_enable_then_disable() {
    fn gmode() -> String {
        let o = std::process::Command::new("gsettings")
            .args(["get", "org.gnome.system.proxy", "mode"])
            .output()
            .expect("gsettings");
        String::from_utf8_lossy(&o.stdout).trim().trim_matches('\'').to_string()
    }
    assert_eq!(gmode(), "none", "test only safe when starting from 'none'");

    let mut p = SystemProxy;
    p.enable().expect("enable should apply a backend");
    assert_eq!(gmode(), "manual", "GNOME mode should be manual after enable");

    let port = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.system.proxy.socks", "port"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    assert_eq!(port, "11080", "SOCKS port should be our inbound");

    p.disable();
    assert_eq!(gmode(), "none", "GNOME mode should be back to none after disable");
}
