//! Headless harness for the v4 engine — the Rust successor to test_connect.py.
//!
//! `dreamnail-cli connect <user> <pass> [hold_secs]` brings up the real tunnel +
//! Xray + system proxy, verifies the exit IP, then holds the connection for
//! `hold_secs` (default 60) so you can browse-test, running the watchdog, before
//! a clean disconnect. This is how a real connect is smoke-tested per OS, no UI.
//!
//! Needs a real Xray binary — set `DREAMNAIL_XRAY=/path/to/xray` (or drop `xray`
//! next to this binary / on PATH). It sets the SYSTEM proxy while connected and
//! restores it on exit; a hard kill mid-hold leaves the proxy set (same caveat
//! as the real app after a crash).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;

use dreamnail_core::ports::{Proxy, Result as PortResult};
use dreamnail_core::{Credentials, Engine, State};
use dreamnail_plat::{HttpRouter, RusshTunnel, SystemProxy, TunnelConfig, XrayProcess};

const EXPECTED_EXIT_IP: &str = dreamnail_plat::netcfg::EXPECTED_EXIT_IP;

/// A Proxy that does nothing — for `DREAMNAIL_NO_PROXY=1` safe testing on a live
/// desktop. The route-verify still works (HttpRouter fetches the exit IP through
/// the LOCAL SOCKS proxy, not the system one), so this proves tunnel + Xray +
/// real routing end-to-end WITHOUT ever touching the machine's proxy settings.
struct NullProxy;
impl Proxy for NullProxy {
    fn enable(&mut self) -> PortResult<()> {
        Ok(())
    }
    fn disable(&mut self) {}
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // `dreamnail-cli update [apply]` — exercise the USB updater headlessly. Point
    // it at a folder that mimics the USB with DREAMNAIL_USB_DIR=/path, and set
    // DREAMNAIL_DEBUG=1 to see every path it checks.
    // `dreamnail-cli conflicts [own-iface…]` — run the foreign VPN / proxy
    // detector once and print what it sees (read-only; nothing is changed). Any
    // interface names given are treated as OUR active stealth tunnel, to test
    // that we never report ourselves.
    if args.get(1).map(String::as_str) == Some("conflicts") {
        use dreamnail_plat::conflict;
        let own: Vec<String> = args[2..].to_vec();
        println!("tunnel interfaces present: {:?}", conflict::tunnel_interfaces());
        let found = conflict::detect(&own, !own.is_empty());
        if found.is_empty() {
            println!("no other VPN or system proxy detected");
        }
        for f in found {
            println!(
                "{} [{}]{}",
                f.name,
                f.kind.as_str(),
                if f.capturing { " — REDIRECTING TRAFFIC" } else { "" }
            );
        }
        return;
    }

    // `dreamnail-cli network` — which network are we on (the id "auto-connect on
    // untrusted networks" keys on). Read-only.
    if args.get(1).map(String::as_str) == Some("network") {
        match dreamnail_plat::netid::current() {
            Some(n) => println!("{} [{}] id={}", n.label, n.kind, n.id),
            None => println!("no network detected"),
        }
        return;
    }

    // `dreamnail-cli leaktest [tun]` — run the leak test against whatever is up
    // right now (pass `tun` if a stealth session is active). Read-only.
    if args.get(1).map(String::as_str) == Some("leaktest") {
        use dreamnail_plat::diag;
        let tun = args.get(2).map(String::as_str) == Some("tun");
        let connected = std::net::TcpStream::connect_timeout(
            &"127.0.0.1:11080".parse().unwrap(),
            Duration::from_millis(300),
        )
        .is_ok();
        println!("connected={connected} tun={tun}");
        println!("dns servers: {:?}", diag::dns_servers());
        for c in diag::run_all(diag::Session { connected, tun, proxy_applied: connected && !tun }) {
            println!("[{:4}] {:<24} {}", c.level.as_str(), c.title, c.detail);
        }
        return;
    }

    // `dreamnail-cli bosskey [install|remove]` — the Wayland desktop shortcut for
    // the boss key. With no action it just reports status. The trigger path
    // follows DREAMNAIL_DATA_DIR, so point that at a scratch dir to test.
    if args.get(1).map(String::as_str) == Some("bosskey") {
        use dreamnail_plat::shortcut;
        let dir = std::env::var("DREAMNAIL_DATA_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".dreamnail")
            });
        let trigger = dir.join(shortcut::TRIGGER_FILE);
        match args.get(2).map(String::as_str) {
            Some("install") => println!("install: {:?}", shortcut::install(&trigger)),
            Some("remove") => println!("remove: {:?}", shortcut::remove(&trigger)),
            _ => {}
        }
        println!(
            "desktop={} needed={} installed={} trigger={}",
            shortcut::desktop().as_str(),
            shortcut::needed(),
            shortcut::installed(&trigger),
            trigger.display()
        );
        return;
    }

    if args.get(1).map(String::as_str) == Some("update") {
        use dreamnail_plat::{netcfg, updater};
        println!("current app version: v{}", netcfg::APP_VERSION);
        let apply = args.get(2).map(String::as_str) == Some("apply");
        // USB first (cleanest install, no AV flag), then over-the-air.
        if let Some(b) = updater::find_usb_update(netcfg::APP_VERSION) {
            println!("UPDATE AVAILABLE (USB): v{} → {}", b.version, b.package.display());
            if apply {
                match updater::apply(&b.package) {
                    Ok(()) => println!("installer launched ✓ (see the wizard window)"),
                    Err(e) => {
                        println!("installer FAILED to launch: {e}");
                        std::process::exit(3);
                    }
                }
            }
        } else if let Some(v) = updater::check_net_update(netcfg::APP_VERSION) {
            println!("UPDATE AVAILABLE (over-the-air): v{v}");
            if apply {
                match updater::fetch_net_update(netcfg::APP_VERSION) {
                    Some(b) => {
                        if let Err(e) = updater::save_rollback_point(&b.package, &b.version) {
                            println!("(rollback point not saved: {e})");
                        }
                        match updater::apply(&b.package) {
                            Ok(()) => println!("installer launched ✓ (see the wizard window)"),
                            Err(e) => {
                                println!("installer FAILED to launch: {e}");
                                std::process::exit(3);
                            }
                        }
                    }
                    None => {
                        println!("download/verify FAILED (see DREAMNAIL_DEBUG=1)");
                        std::process::exit(3);
                    }
                }
            }
        } else {
            println!("no update found (see DREAMNAIL_DEBUG=1 output for why)");
        }
        return;
    }

    let tunnel = match RusshTunnel::new(TunnelConfig::default()) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("failed to init tunnel: {e}");
            std::process::exit(1);
        }
    };
    let no_proxy = std::env::var("DREAMNAIL_NO_PROXY").is_ok();
    let tun_mode = std::env::var("DREAMNAIL_TUN").is_ok();
    let proxy: Box<dyn Proxy> = if no_proxy {
        println!("(DREAMNAIL_NO_PROXY set — NOT touching the system proxy)");
        Box::new(NullProxy)
    } else if tun_mode {
        println!("(DREAMNAIL_TUN set — routing ALL traffic via a TUN device, no system proxy; needs admin/root)");
        Box::new(dreamnail_plat::TunProxy::default())
    } else {
        Box::new(SystemProxy)
    };
    let mut engine = Engine::new(
        Box::new(tunnel),
        Box::new(XrayProcess::default()),
        proxy,
        Box::new(HttpRouter),
        EXPECTED_EXIT_IP,
    );

    println!("dreamnail-cli v{}", env!("CARGO_PKG_VERSION"));
    println!("initial state: {:?}", engine.state());

    if args.len() < 3 || args[1] != "connect" {
        println!("usage: dreamnail-cli connect <username> <password> [hold_secs]");
        return;
    }

    let creds = Credentials {
        username: args[2].clone(),
        password: args.get(3).cloned().unwrap_or_default(),
    };
    // hold_secs: a number = hold that long; 0 or omitted = hold until Ctrl-C.
    let hold_secs: u64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
    let forever = hold_secs == 0;

    println!("connecting as {} …", creds.username);
    match engine.connect(creds) {
        Ok(()) => println!("CONNECTED — traffic verified via {EXPECTED_EXIT_IP}"),
        Err(e) => {
            println!("connect failed: {e}");
            std::process::exit(2);
        }
    }

    // Trap Ctrl-C so an interrupt still tears down cleanly (restores routing/
    // proxy) instead of leaving the machine pointed at a dead TUN/proxy.
    let running = Arc::new(AtomicBool::new(true));
    {
        let r = running.clone();
        if let Err(e) = ctrlc::set_handler(move || r.store(false, Ordering::SeqCst)) {
            eprintln!("warning: could not install Ctrl-C handler: {e}");
        }
    }

    // Hold the link up, running the watchdog, so it can be browse-tested.
    if forever {
        println!("holding until Ctrl-C (proxy is live)…");
    } else {
        println!("holding for {hold_secs}s (Ctrl-C to drop; proxy is live)…");
    }
    let mut left = hold_secs;
    while running.load(Ordering::SeqCst)
        && engine.state() != State::Disconnected
        && (forever || left > 0)
    {
        // Sleep in 1s steps so Ctrl-C is picked up promptly.
        let step = if forever { 5 } else { left.min(5) };
        for _ in 0..step {
            if !running.load(Ordering::SeqCst) {
                break;
            }
            sleep(Duration::from_secs(1));
        }
        if !forever {
            left = left.saturating_sub(step);
        }
        if !running.load(Ordering::SeqCst) {
            break;
        }
        if let Err(e) = engine.poll() {
            println!("watchdog: link lost and could not recover: {e}");
            break;
        }
        if forever {
            println!("  … {:?} (Ctrl-C to stop)", engine.state());
        } else {
            println!("  … {:?} ({left}s left)", engine.state());
        }
    }

    println!("\ndisconnecting…");
    engine.disconnect();
    println!("state: {:?}", engine.state());
}
