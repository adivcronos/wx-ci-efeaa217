//! One-click leak test: plain-English checks that traffic really goes where the
//! VPN says it does.
//!
//! Each check pairs a small probe (an HTTP "what's my IP" request, or reading
//! the OS's DNS settings) with a PURE evaluator that turns the probe result into
//! a verdict — the evaluators are unit-tested; the probes just gather facts.
//!
//! Privacy: the only outside services contacted are plain IP-echo endpoints
//! (the same kind the connect-time route check already uses). The user's real
//! IP is never shown in full — it's masked in every message — and the VPN
//! server's address is never printed at all.

use std::net::IpAddr;
use std::time::Duration;

use crate::netcfg;
use crate::netid::run;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Pass,
    Warn,
    Fail,
    Info,
    Skip,
}

impl Level {
    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Pass => "pass",
            Level::Warn => "warn",
            Level::Fail => "fail",
            Level::Info => "info",
            Level::Skip => "skip",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Check {
    pub key: &'static str,
    pub title: &'static str,
    pub level: Level,
    pub detail: String,
}

/// What the engine knows about the current session.
#[derive(Debug, Clone, Copy)]
pub struct Session {
    pub connected: bool,
    /// Stealth (TUN) routing is active, vs the standard system proxy.
    pub tun: bool,
    /// Standard mode: the OS accepted our system proxy.
    pub proxy_applied: bool,
}

const V4_ECHO: [&str; 2] = ["https://api.ipify.org", "https://ipv4.icanhazip.com"];
const V6_ECHO: &str = "https://api6.ipify.org";

/// Run every check (probes in parallel, so the whole test takes about as long
/// as the slowest single request).
pub fn run_all(s: Session) -> Vec<Check> {
    let server = netcfg::EXPECTED_EXIT_IP;
    let (via_tunnel, direct, v6_direct, v6_tunnel, dns) = std::thread::scope(|sc| {
        let a = sc.spawn(|| if s.connected { echo_v4(true) } else { None });
        let b = sc.spawn(|| echo_v4(false));
        let c = sc.spawn(|| echo(V6_ECHO, false));
        let d = sc.spawn(|| if s.connected { echo(V6_ECHO, true) } else { None });
        let e = sc.spawn(dns_servers);
        (
            a.join().unwrap_or(None),
            b.join().unwrap_or(None),
            c.join().unwrap_or(None),
            d.join().unwrap_or(None),
            e.join().unwrap_or_default(),
        )
    });
    vec![
        eval_tunnel(s, via_tunnel.as_deref(), server),
        eval_direct(s, direct.as_deref(), server),
        eval_proxy(s),
        eval_dns(s, &dns),
        eval_ipv6(s, v6_direct.as_deref(), v6_tunnel.as_deref()),
        eval_webrtc(s),
    ]
}

// --------------------------------------------------------------------------
// Pure evaluators
// --------------------------------------------------------------------------

pub fn eval_tunnel(s: Session, via: Option<&str>, server: &str) -> Check {
    let (level, detail) = if !s.connected {
        (Level::Skip, "connect first to test the tunnel.".to_string())
    } else {
        match via {
            Some(ip) if ip == server => (
                Level::Pass,
                "traffic sent into the tunnel comes out at the VPN server.".to_string(),
            ),
            Some(ip) => (
                Level::Fail,
                format!("traffic sent into the tunnel came out at {} — not the VPN server.", mask_ip(ip)),
            ),
            None => (
                Level::Fail,
                "couldn't reach the internet through the tunnel.".to_string(),
            ),
        }
    };
    Check { key: "tunnel", title: "tunnel exit", level, detail }
}

pub fn eval_direct(s: Session, direct: Option<&str>, server: &str) -> Check {
    let (level, detail) = match (s.connected, s.tun, direct) {
        (_, _, None) => (
            Level::Info,
            "a plain request (like an app that ignores proxy settings) couldn't get out — \
             nothing to leak."
                .to_string(),
        ),
        (false, _, Some(ip)) => (
            Level::Info,
            format!("not connected: apps currently show your real address ({}).", mask_ip(ip)),
        ),
        (true, true, Some(ip)) if ip == server => (
            Level::Pass,
            "every app — even ones that ignore proxy settings — goes through the VPN.".to_string(),
        ),
        (true, true, Some(ip)) => (
            Level::Fail,
            format!(
                "apps that ignore proxy settings are going around the VPN — they show your real \
                 address ({}).",
                mask_ip(ip)
            ),
        ),
        (true, false, Some(ip)) if ip == server => (
            Level::Pass,
            "even plain requests are going through the VPN.".to_string(),
        ),
        (true, false, Some(ip)) => (
            Level::Warn,
            format!(
                "standard mode only covers apps that use the system proxy (browsers do). apps \
                 that ignore it show your real address ({}). turn on stealth routing to cover \
                 everything.",
                mask_ip(ip)
            ),
        ),
    };
    Check { key: "direct", title: "apps that skip the proxy", level, detail }
}

pub fn eval_proxy(s: Session) -> Check {
    let (level, detail) = match (s.connected, s.tun, s.proxy_applied) {
        (false, _, _) => (Level::Skip, "connect first.".to_string()),
        (true, true, _) => (
            Level::Info,
            "stealth routing doesn't need a system proxy — everything is routed instead.".to_string(),
        ),
        (true, false, true) => (
            Level::Pass,
            "your system proxy points at the VPN, so browsers use it.".to_string(),
        ),
        (true, false, false) => (
            Level::Fail,
            "your computer didn't accept the VPN's proxy setting — apps are going around it. \
             try reconnecting, or turn on stealth routing."
                .to_string(),
        ),
    };
    Check { key: "proxy", title: "system proxy", level, detail }
}

/// Where a DNS server lives, in words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsKind {
    /// tun2proxy's virtual resolver (198.18.0.0/15) — answered inside the tunnel.
    Tunnel,
    /// 127.x / ::1 — a local stub (its upstreams are listed separately).
    Loopback,
    /// LAN / router / carrier-grade NAT range.
    Local,
    /// A public resolver or your ISP's.
    Public,
}

pub fn classify_dns(ip: &str) -> Option<DnsKind> {
    let ip: IpAddr = ip.trim().parse().ok()?;
    Some(match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            if v.is_loopback() {
                DnsKind::Loopback
            } else if o[0] == 198 && (o[1] == 18 || o[1] == 19) {
                DnsKind::Tunnel
            } else if v.is_private() || v.is_link_local() || (o[0] == 100 && (64..128).contains(&o[1])) {
                DnsKind::Local
            } else {
                DnsKind::Public
            }
        }
        IpAddr::V6(v) => {
            let seg0 = v.segments()[0];
            if v.is_loopback() {
                DnsKind::Loopback
            } else if (seg0 & 0xfe00) == 0xfc00 || (seg0 & 0xffc0) == 0xfe80 {
                DnsKind::Local
            } else {
                DnsKind::Public
            }
        }
    })
}

pub fn eval_dns(s: Session, servers: &[String]) -> Check {
    let outside: Vec<&String> = servers
        .iter()
        .filter(|ip| matches!(classify_dns(ip), Some(DnsKind::Local | DnsKind::Public)))
        .collect();
    let in_tunnel = servers
        .iter()
        .any(|ip| classify_dns(ip) == Some(DnsKind::Tunnel));
    let where_ = describe_dns(&outside);
    let (level, detail) = if servers.is_empty() {
        (Level::Info, "couldn't read this computer's DNS settings.".to_string())
    } else if !s.connected {
        (Level::Info, format!("not connected: website lookups go to {where_}."))
    } else if s.tun && outside.is_empty() {
        (
            Level::Pass,
            "website lookups are answered inside the tunnel.".to_string(),
        )
    } else if s.tun {
        (
            Level::Warn,
            format!(
                "some apps may still ask {where_} for website names, outside the tunnel{} — \
                 that network can see which sites you look up.",
                if in_tunnel { " (others use the tunnel)" } else { "" }
            ),
        )
    } else {
        (
            Level::Warn,
            format!(
                "your browser looks sites up through the VPN, but other apps ask {where_} \
                 directly. turn on stealth routing to cover them."
            ),
        )
    };
    Check { key: "dns", title: "DNS lookups", level, detail }
}

fn describe_dns(outside: &[&String]) -> String {
    let local = outside
        .iter()
        .any(|ip| classify_dns(ip) == Some(DnsKind::Local));
    let public = outside
        .iter()
        .any(|ip| classify_dns(ip) == Some(DnsKind::Public));
    match (local, public) {
        (true, true) => "your router and a public DNS server".into(),
        (true, false) => "your router / local network".into(),
        (false, true) => "a public or internet-provider DNS server".into(),
        (false, false) => "the tunnel".into(),
    }
}

pub fn eval_ipv6(s: Session, direct: Option<&str>, via_tunnel: Option<&str>) -> Check {
    let (level, detail) = match (s.connected, direct) {
        (_, None) => (
            Level::Pass,
            "this network has no IPv6 route, so nothing can slip out over it.".to_string(),
        ),
        (false, Some(ip)) => (
            Level::Info,
            format!("your network has IPv6 ({}).", mask_ip(ip)),
        ),
        (true, Some(ip)) if via_tunnel == Some(ip) => (
            Level::Pass,
            "IPv6 traffic goes through the VPN too.".to_string(),
        ),
        (true, Some(ip)) => (
            Level::Fail,
            format!(
                "IPv6 traffic skips the VPN and shows your real address ({}). IPv6-capable \
                 sites can see who you are.",
                mask_ip(ip)
            ),
        ),
    };
    Check { key: "ipv6", title: "IPv6 leak", level, detail }
}

pub fn eval_webrtc(s: Session) -> Check {
    let (level, detail) = match (s.connected, s.tun) {
        (false, _) => (Level::Skip, "connect first.".to_string()),
        (true, true) => (
            Level::Pass,
            "browser calls and video chat (WebRTC) are routed through the tunnel too.".to_string(),
        ),
        (true, false) => (
            Level::Warn,
            "in standard mode, browser calls and video chat (WebRTC) can reveal your real \
             address to websites. turn on stealth routing, or disable WebRTC in your browser."
                .to_string(),
        ),
    };
    Check { key: "webrtc", title: "WebRTC (browser calls)", level, detail }
}

/// Show enough of an address to recognise it, not enough to identify anyone:
/// `203.0.113.7` → `203.0.x.x`, `2001:db8:1:2::5` → `2001:db8:…`.
pub fn mask_ip(ip: &str) -> String {
    match ip.trim().parse::<IpAddr>() {
        Ok(IpAddr::V4(v)) => {
            let o = v.octets();
            format!("{}.{}.x.x", o[0], o[1])
        }
        Ok(IpAddr::V6(v)) => {
            let s = v.segments();
            format!("{:x}:{:x}:…", s[0], s[1])
        }
        Err(_) => "unknown".to_string(),
    }
}

// --------------------------------------------------------------------------
// Probes
// --------------------------------------------------------------------------

fn echo_v4(through_tunnel: bool) -> Option<String> {
    V4_ECHO
        .iter()
        .find_map(|u| echo(u, through_tunnel).filter(|ip| ip.parse::<std::net::Ipv4Addr>().is_ok()))
}

/// GET an IP-echo URL, either straight out (`.no_proxy()`, like an app that
/// ignores proxy settings) or through the local SOCKS inbound (the tunnel).
fn echo(url: &str, through_tunnel: bool) -> Option<String> {
    let mut b = reqwest::blocking::Client::builder().timeout(Duration::from_secs(7));
    b = if through_tunnel {
        let p = format!("socks5h://{}:{}", netcfg::SOCKS_HOST, netcfg::SOCKS_PORT);
        b.proxy(reqwest::Proxy::all(&p).ok()?)
    } else {
        b.no_proxy()
    };
    let body = b.build().ok()?.get(url).send().ok()?.text().ok()?;
    let ip = body.trim();
    ip.parse::<IpAddr>().is_ok().then(|| ip.to_string())
}

/// The DNS servers this computer will actually ask, best-effort.
pub fn dns_servers() -> Vec<String> {
    let mut v = os_dns_servers();
    let mut seen = std::collections::HashSet::new();
    v.retain(|ip| seen.insert(ip.clone()));
    v
}

/// `/etc/resolv.conf` → `nameserver` entries.
pub fn parse_resolv_conf(s: &str) -> Vec<String> {
    s.lines()
        .filter_map(|l| l.trim().strip_prefix("nameserver"))
        .map(|v| v.trim().to_string())
        .filter(|v| v.parse::<IpAddr>().is_ok())
        .collect()
}

/// `resolvectl dns` → every server on every link (and Global).
/// Lines look like `Global: 1.1.1.1` / `Link 3 (wlo1): 192.168.4.1 fe80::1%3`.
pub fn parse_resolvectl_dns(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in s.lines() {
        let rest = if let Some(r) = line.trim().strip_prefix("Global:") {
            r
        } else if let Some((_, r)) = line.split_once("):") {
            r
        } else {
            continue;
        };
        for tok in rest.split_whitespace() {
            // Drop a `%iface` zone suffix and any `#servername` SNI tail.
            let ip = tok.split(['%', '#']).next().unwrap_or("");
            if ip.parse::<IpAddr>().is_ok() {
                out.push(ip.to_string());
            }
        }
    }
    out
}

/// glibc NSS uses systemd-resolved directly (bypassing /etc/resolv.conf)
/// when `hosts:` lists the `resolve` module.
pub fn nss_uses_resolved(nsswitch: &str) -> bool {
    nsswitch
        .lines()
        .find_map(|l| l.trim().strip_prefix("hosts:"))
        .map(|h| h.split_whitespace().any(|m| m == "resolve"))
        .unwrap_or(false)
}

/// macOS `scutil --dns` → nameservers of the default resolver (#1).
pub fn parse_scutil_dns(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_first = false;
    for line in s.lines() {
        let t = line.trim();
        if t.starts_with("resolver #") {
            if in_first {
                break;
            }
            in_first = t == "resolver #1";
            continue;
        }
        if in_first && t.starts_with("nameserver[") {
            if let Some((_, v)) = t.split_once(':') {
                let v = v.trim();
                if v.parse::<IpAddr>().is_ok() {
                    out.push(v.to_string());
                }
            }
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn os_dns_servers() -> Vec<String> {
    let rc = std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default();
    let mut servers = parse_resolv_conf(&rc);
    let stub = servers.iter().any(|s| s == "127.0.0.53");
    let nss = std::fs::read_to_string("/etc/nsswitch.conf").unwrap_or_default();
    if stub || nss_uses_resolved(&nss) {
        if let Some(out) = run("resolvectl", &["dns"]) {
            servers.extend(parse_resolvectl_dns(&out));
        }
    }
    // The stub itself isn't a destination — its upstreams (added above) are.
    servers.retain(|s| s != "127.0.0.53");
    servers
}

#[cfg(target_os = "macos")]
fn os_dns_servers() -> Vec<String> {
    run("scutil", &["--dns"]).map(|o| parse_scutil_dns(&o)).unwrap_or_default()
}

#[cfg(target_os = "windows")]
fn os_dns_servers() -> Vec<String> {
    run_timeout_ps(
        "Get-DnsClientServerAddress | Where-Object { $_.ServerAddresses } | \
         ForEach-Object { $_.ServerAddresses }",
    )
    .map(|o| {
        o.lines()
            .map(|l| l.trim().to_string())
            .filter(|l| l.parse::<IpAddr>().is_ok())
            .collect()
    })
    .unwrap_or_default()
}

#[cfg(target_os = "windows")]
fn run_timeout_ps(script: &str) -> Option<String> {
    crate::netid::run_timeout(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", script],
        Duration::from_secs(8),
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn os_dns_servers() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRV: &str = "66.226.144.136";
    fn sess(connected: bool, tun: bool, proxy_applied: bool) -> Session {
        Session { connected, tun, proxy_applied }
    }

    #[test]
    fn tunnel_exit() {
        assert_eq!(eval_tunnel(sess(true, false, true), Some(SRV), SRV).level, Level::Pass);
        let bad = eval_tunnel(sess(true, false, true), Some("203.0.113.7"), SRV);
        assert_eq!(bad.level, Level::Fail);
        assert!(bad.detail.contains("203.0.x.x") && !bad.detail.contains("113.7"));
        assert_eq!(eval_tunnel(sess(true, true, false), None, SRV).level, Level::Fail);
        assert_eq!(eval_tunnel(sess(false, false, false), None, SRV).level, Level::Skip);
    }

    #[test]
    fn direct_requests_by_mode() {
        let real = Some("203.0.113.7");
        assert_eq!(eval_direct(sess(true, true, false), Some(SRV), SRV).level, Level::Pass);
        assert_eq!(eval_direct(sess(true, true, false), real, SRV).level, Level::Fail);
        assert_eq!(eval_direct(sess(true, false, true), real, SRV).level, Level::Warn);
        assert_eq!(eval_direct(sess(false, false, false), real, SRV).level, Level::Info);
        // The server address itself is never printed.
        for c in [
            eval_direct(sess(true, true, false), Some(SRV), SRV),
            eval_tunnel(sess(true, true, false), Some(SRV), SRV),
        ] {
            assert!(!c.detail.contains(SRV));
        }
    }

    #[test]
    fn proxy_and_webrtc() {
        assert_eq!(eval_proxy(sess(true, false, true)).level, Level::Pass);
        assert_eq!(eval_proxy(sess(true, false, false)).level, Level::Fail);
        assert_eq!(eval_proxy(sess(true, true, false)).level, Level::Info);
        assert_eq!(eval_webrtc(sess(true, false, true)).level, Level::Warn);
        assert_eq!(eval_webrtc(sess(true, true, false)).level, Level::Pass);
        assert_eq!(eval_webrtc(sess(false, true, false)).level, Level::Skip);
    }

    #[test]
    fn dns_classification_and_verdicts() {
        assert_eq!(classify_dns("198.18.0.1"), Some(DnsKind::Tunnel));
        assert_eq!(classify_dns("192.168.4.1"), Some(DnsKind::Local));
        assert_eq!(classify_dns("100.100.100.100"), Some(DnsKind::Local));
        assert_eq!(classify_dns("1.1.1.1"), Some(DnsKind::Public));
        assert_eq!(classify_dns("fe80::1"), Some(DnsKind::Local));
        assert_eq!(classify_dns("127.0.0.53"), Some(DnsKind::Loopback));

        let tunnel_only = vec!["198.18.0.1".to_string()];
        let leaky = vec!["198.18.0.1".to_string(), "192.168.4.1".to_string()];
        assert_eq!(eval_dns(sess(true, true, false), &tunnel_only).level, Level::Pass);
        let w = eval_dns(sess(true, true, false), &leaky);
        assert_eq!(w.level, Level::Warn);
        assert!(w.detail.contains("router") && !w.detail.contains("192.168"));
        assert_eq!(eval_dns(sess(true, false, true), &leaky).level, Level::Warn);
        assert_eq!(eval_dns(sess(false, false, false), &leaky).level, Level::Info);
        assert_eq!(eval_dns(sess(true, true, false), &[]).level, Level::Info);
    }

    #[test]
    fn ipv6() {
        let v6 = Some("2001:db8:1:2::5");
        assert_eq!(eval_ipv6(sess(true, true, false), None, None).level, Level::Pass);
        assert_eq!(eval_ipv6(sess(true, true, false), v6, None).level, Level::Fail);
        assert_eq!(eval_ipv6(sess(true, true, false), v6, v6).level, Level::Pass);
        let info = eval_ipv6(sess(false, false, false), v6, None);
        assert_eq!(info.level, Level::Info);
        assert!(info.detail.contains("2001:db8:…") && !info.detail.contains("::5"));
    }

    #[test]
    fn dns_parsers() {
        let rc = "# generated\nnameserver 127.0.0.53\nnameserver 8.8.8.8\noptions edns0\n";
        assert_eq!(parse_resolv_conf(rc), vec!["127.0.0.53", "8.8.8.8"]);
        let rv = "Global:\nLink 2 (enp3s0):\nLink 3 (wlo1): 192.168.4.1 fe80::1%3\nLink 9 (tun0): 198.18.0.1#dns\n";
        assert_eq!(parse_resolvectl_dns(rv), vec!["192.168.4.1", "fe80::1", "198.18.0.1"]);
        assert!(nss_uses_resolved("hosts:      files myhostname resolve [!UNAVAIL=return] dns\n"));
        assert!(!nss_uses_resolved("hosts: files dns\n"));
        let sc = "DNS configuration\n\nresolver #1\n  nameserver[0] : 192.168.1.1\n  nameserver[1] : 1.1.1.1\n\nresolver #2\n  nameserver[0] : 10.0.0.1\n";
        assert_eq!(parse_scutil_dns(sc), vec!["192.168.1.1", "1.1.1.1"]);
    }

    #[test]
    fn masking() {
        assert_eq!(mask_ip("203.0.113.7"), "203.0.x.x");
        assert_eq!(mask_ip("2001:db8::1"), "2001:db8:…");
        assert_eq!(mask_ip("nonsense"), "unknown");
    }
}
