//! Which network is this machine on? — a stable id for "auto-connect on
//! untrusted networks".
//!
//! The preferred id is the Wi-Fi name (SSID). When the OS won't say (wired
//! Ethernet, or macOS/Windows hiding the SSID from apps without location
//! permission), we fall back to the default gateway's hardware (MAC) address:
//! every router has its own, so it pins down "this network" just as well and
//! needs no permissions.
//!
//! All OS-output parsing is pure and unit-tested; the per-OS code only runs the
//! commands. Every command is best-effort with a short timeout — a missing tool
//! or a wedged network manager just means "unknown network", never a hang.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkId {
    /// Stable key: `wifi:<ssid>` or `gw:<mac>`.
    pub id: String,
    /// What to show a person: the SSID, or "wired network (router …ee:ff)".
    pub label: String,
    /// `"wifi"` or `"gateway"`.
    pub kind: &'static str,
}

impl NetworkId {
    pub fn wifi(ssid: &str) -> Self {
        NetworkId {
            id: format!("wifi:{ssid}"),
            label: ssid.to_string(),
            kind: "wifi",
        }
    }

    pub fn gateway(mac: &str) -> Self {
        let mac = normalize_mac(mac);
        let tail = mac.get(mac.len().saturating_sub(5)..).unwrap_or(&mac);
        NetworkId {
            id: format!("gw:{mac}"),
            label: format!("wired network (router …{tail})"),
            kind: "gateway",
        }
    }
}

use std::sync::Mutex;

/// Short-lived cache of the last resolved network. Resolving shells out to the
/// OS (on Windows `netsh wlan show interfaces` alone is ~1–2 s), and `current()`
/// sits on the connect hot path (per-network door memory) and is hit again on
/// every auto-reconnect and for each door tried in the ladder. Without this
/// cache each of those re-ran the probe, which is what made connects feel much
/// slower on Windows and Linux after per-network memory landed. A short TTL
/// still notices a network change within seconds.
static NET_CACHE: Mutex<Option<(Instant, Option<NetworkId>)>> = Mutex::new(None);
const NET_TTL: Duration = Duration::from_secs(45);

/// The network this machine is on right now, or None when offline/unknown.
/// Cached for `NET_TTL` so repeated callers (connect, reconnect, door ladder)
/// share one OS probe instead of each spawning their own.
pub fn current() -> Option<NetworkId> {
    if let Ok(guard) = NET_CACHE.lock() {
        if let Some((at, val)) = guard.as_ref() {
            if at.elapsed() < NET_TTL {
                return val.clone();
            }
        }
    }
    let fresh = os::current();
    if let Ok(mut guard) = NET_CACHE.lock() {
        *guard = Some((Instant::now(), fresh.clone()));
    }
    fresh
}

// --------------------------------------------------------------------------
// Pure parsers (unit-tested)
// --------------------------------------------------------------------------

/// `nmcli -t -f ACTIVE,SSID dev wifi list --rescan no` → the active SSID.
/// Terse mode escapes `:` and `\` inside values as `\:` and `\\`.
pub fn parse_nmcli_active_ssid(out: &str) -> Option<String> {
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("yes:") {
            let ssid = unescape_nmcli(rest);
            if !ssid.is_empty() {
                return Some(ssid);
            }
        }
    }
    None
}

fn unescape_nmcli(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// `ip -4 route show default` → the gateway IP of the first default route.
/// A default route with no `via` (e.g. one pointing straight at a VPN's TUN
/// device) has no router to identify, so it yields None.
pub fn parse_ip_route_default(out: &str) -> Option<String> {
    for line in out.lines() {
        let toks: Vec<&str> = line.split_whitespace().collect();
        if toks.first() != Some(&"default") {
            continue;
        }
        if let Some(i) = toks.iter().position(|t| *t == "via") {
            if let Some(gw) = toks.get(i + 1) {
                return Some(gw.to_string());
            }
        }
    }
    None
}

/// `ip neigh show <gw>` → the `lladdr` MAC.
pub fn parse_ip_neigh_mac(out: &str) -> Option<String> {
    for line in out.lines() {
        let toks: Vec<&str> = line.split_whitespace().collect();
        if let Some(i) = toks.iter().position(|t| *t == "lladdr") {
            if let Some(mac) = toks.get(i + 1) {
                if is_mac(mac) {
                    return Some(normalize_mac(mac));
                }
            }
        }
    }
    None
}

/// macOS `ipconfig getsummary en0` → `SSID : <name>`. Newer macOS prints
/// `<redacted>` to apps without location permission; treat that as unknown.
pub fn parse_macos_summary_ssid(out: &str) -> Option<String> {
    for line in out.lines() {
        let t = line.trim();
        if let Some(v) = t.strip_prefix("SSID : ") {
            let v = v.trim();
            if !v.is_empty() && v != "<redacted>" {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// macOS `networksetup -getairportnetwork en0` → `Current Wi-Fi Network: <name>`.
pub fn parse_networksetup_ssid(out: &str) -> Option<String> {
    out.lines()
        .find_map(|l| l.trim().strip_prefix("Current Wi-Fi Network: "))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// macOS `route -n get default` → `gateway: <ip>`.
pub fn parse_route_get_gateway(out: &str) -> Option<String> {
    out.lines()
        .find_map(|l| l.trim().strip_prefix("gateway:"))
        .map(|v| v.trim().to_string())
        .filter(|v| v.parse::<std::net::Ipv4Addr>().is_ok())
}

/// macOS `arp -n <ip>` → `? (<ip>) at a:b:c:d:e:f on en0 …`. macOS drops
/// leading zeros in each octet; [`normalize_mac`] puts them back.
pub fn parse_macos_arp_mac(out: &str) -> Option<String> {
    for line in out.lines() {
        let toks: Vec<&str> = line.split_whitespace().collect();
        if let Some(i) = toks.iter().position(|t| *t == "at") {
            if let Some(mac) = toks.get(i + 1) {
                if is_mac(mac) {
                    return Some(normalize_mac(mac));
                }
            }
        }
    }
    None
}

/// Windows `netsh wlan show interfaces` → the `SSID : <name>` line (not BSSID).
pub fn parse_netsh_ssid(out: &str) -> Option<String> {
    for line in out.lines() {
        let t = line.trim();
        if !t.starts_with("SSID") {
            continue;
        }
        if let Some((_, v)) = t.split_once(':') {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Windows `route print -4 0.0.0.0` → gateway column of the default route.
pub fn parse_route_print_gateway(out: &str) -> Option<String> {
    for line in out.lines() {
        let toks: Vec<&str> = line.split_whitespace().collect();
        if toks.len() >= 3 && toks[0] == "0.0.0.0" && toks[1] == "0.0.0.0" {
            if toks[2].parse::<std::net::Ipv4Addr>().is_ok() {
                return Some(toks[2].to_string());
            }
        }
    }
    None
}

/// Windows `arp -a <ip>` → the physical address on the row for `<ip>`.
pub fn parse_windows_arp_mac(out: &str, ip: &str) -> Option<String> {
    for line in out.lines() {
        let toks: Vec<&str> = line.split_whitespace().collect();
        if toks.len() >= 2 && toks[0] == ip && is_mac(toks[1]) {
            return Some(normalize_mac(toks[1]));
        }
    }
    None
}

fn is_mac(s: &str) -> bool {
    let parts: Vec<&str> = s.split(|c| c == ':' || c == '-').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 2 && u8::from_str_radix(p, 16).is_ok())
        // An all-zero / incomplete entry identifies nothing.
        && parts.iter().any(|p| u8::from_str_radix(p, 16).unwrap_or(0) != 0)
}

/// Lower-case, colon-separated, two digits per octet.
pub fn normalize_mac(s: &str) -> String {
    s.split(|c| c == ':' || c == '-')
        .map(|p| format!("{:02x}", u8::from_str_radix(p, 16).unwrap_or(0)))
        .collect::<Vec<_>>()
        .join(":")
}

// --------------------------------------------------------------------------
// Command runner (bounded, quiet, no console window on Windows)
// --------------------------------------------------------------------------

pub(crate) fn run(cmd: &str, args: &[&str]) -> Option<String> {
    run_timeout(cmd, args, Duration::from_secs(3))
}

pub(crate) fn run_timeout(cmd: &str, args: &[&str], limit: Duration) -> Option<String> {
    let mut c = Command::new(cmd);
    c.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = c.spawn().ok()?;
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                if let Some(mut so) = child.stdout.take() {
                    use std::io::Read;
                    let _ = so.read_to_string(&mut out);
                }
                return status.success().then_some(out);
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(15));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

// --------------------------------------------------------------------------
// Per-OS
// --------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod os {
    use super::*;

    pub fn current() -> Option<NetworkId> {
        // Try the cheap, instant `iwgetid -r` first; only fall back to the
        // heavier `nmcli … dev wifi list` (which enumerates every visible AP)
        // when iwgetid is missing or says nothing.
        if let Some(ssid) = run("iwgetid", &["-r"])
            .map(|o| o.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                run("nmcli", &["-t", "-f", "ACTIVE,SSID", "dev", "wifi", "list", "--rescan", "no"])
                    .and_then(|o| parse_nmcli_active_ssid(&o))
            })
        {
            return Some(NetworkId::wifi(&ssid));
        }
        let gw = run("ip", &["-4", "route", "show", "default"]).and_then(|o| parse_ip_route_default(&o))?;
        let mac = run("ip", &["neigh", "show", &gw]).and_then(|o| parse_ip_neigh_mac(&o))?;
        Some(NetworkId::gateway(&mac))
    }
}

#[cfg(target_os = "macos")]
mod os {
    use super::*;

    pub fn current() -> Option<NetworkId> {
        for iface in ["en0", "en1"] {
            let ssid = run("ipconfig", &["getsummary", iface])
                .and_then(|o| parse_macos_summary_ssid(&o))
                .or_else(|| {
                    run("networksetup", &["-getairportnetwork", iface])
                        .and_then(|o| parse_networksetup_ssid(&o))
                });
            if let Some(ssid) = ssid {
                return Some(NetworkId::wifi(&ssid));
            }
        }
        let gw = run("route", &["-n", "get", "default"]).and_then(|o| parse_route_get_gateway(&o))?;
        let mac = run("arp", &["-n", &gw]).and_then(|o| parse_macos_arp_mac(&o))?;
        Some(NetworkId::gateway(&mac))
    }
}

#[cfg(target_os = "windows")]
mod os {
    use super::*;

    pub fn current() -> Option<NetworkId> {
        if let Some(ssid) = run("netsh", &["wlan", "show", "interfaces"]).and_then(|o| parse_netsh_ssid(&o)) {
            return Some(NetworkId::wifi(&ssid));
        }
        let gw = run("route", &["print", "-4", "0.0.0.0"]).and_then(|o| parse_route_print_gateway(&o))?;
        let mac = run("arp", &["-a", &gw]).and_then(|o| parse_windows_arp_mac(&o, &gw))?;
        Some(NetworkId::gateway(&mac))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod os {
    use super::*;
    pub fn current() -> Option<NetworkId> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nmcli_picks_the_active_ssid_and_unescapes() {
        let out = "no:Neighbours\nyes:Cafe\\: Free WiFi\nno:Other\n";
        assert_eq!(parse_nmcli_active_ssid(out).as_deref(), Some("Cafe: Free WiFi"));
        assert_eq!(parse_nmcli_active_ssid("no:A\nno:B\n"), None);
        // An active row with a hidden (empty) SSID identifies nothing.
        assert_eq!(parse_nmcli_active_ssid("yes:\n"), None);
    }

    #[test]
    fn ip_route_default_reads_the_gateway() {
        let out = "default via 192.168.4.1 dev wlo1 proto dhcp src 192.168.4.20 metric 600\n";
        assert_eq!(parse_ip_route_default(out).as_deref(), Some("192.168.4.1"));
        // A VPN-style default route straight at a device has no router.
        assert_eq!(parse_ip_route_default("default dev tun0 scope link\n"), None);
        assert_eq!(parse_ip_route_default(""), None);
    }

    #[test]
    fn ip_neigh_reads_the_mac() {
        let out = "192.168.4.1 dev wlo1 lladdr AA:bb:0c:dd:ee:ff REACHABLE\n";
        assert_eq!(parse_ip_neigh_mac(out).as_deref(), Some("aa:bb:0c:dd:ee:ff"));
        assert_eq!(parse_ip_neigh_mac("192.168.4.1 dev wlo1 FAILED\n"), None);
    }

    #[test]
    fn macos_ssid_sources() {
        let summary = "<dictionary> {\n  BSSID : 11:22:33:44:55:66\n  SSID : Home Net\n}\n";
        assert_eq!(parse_macos_summary_ssid(summary).as_deref(), Some("Home Net"));
        assert_eq!(parse_macos_summary_ssid("  SSID : <redacted>\n"), None);
        assert_eq!(
            parse_networksetup_ssid("Current Wi-Fi Network: Office\n").as_deref(),
            Some("Office")
        );
        assert_eq!(
            parse_networksetup_ssid("You are not associated with an AirPort network.\n"),
            None
        );
    }

    #[test]
    fn macos_gateway_and_arp() {
        let route = "   route to: default\ndestination: default\n    gateway: 10.0.0.1\n  interface: en0\n";
        assert_eq!(parse_route_get_gateway(route).as_deref(), Some("10.0.0.1"));
        let arp = "? (10.0.0.1) at a:b:c:d:e:f on en0 ifscope [ethernet]\n";
        assert_eq!(parse_macos_arp_mac(arp).as_deref(), Some("0a:0b:0c:0d:0e:0f"));
        assert_eq!(parse_macos_arp_mac("? (10.0.0.1) at (incomplete) on en0\n"), None);
    }

    #[test]
    fn windows_parsers() {
        let netsh = "    Name                   : Wi-Fi\n    BSSID                  : 11:22:33:44:55:66\n    SSID                   : Airport_Free\n";
        assert_eq!(parse_netsh_ssid(netsh).as_deref(), Some("Airport_Free"));
        let route = "Network Destination        Netmask          Gateway       Interface  Metric\n          0.0.0.0          0.0.0.0      192.168.1.254    192.168.1.20     35\n";
        assert_eq!(parse_route_print_gateway(route).as_deref(), Some("192.168.1.254"));
        let arp = "\nInterface: 192.168.1.20 --- 0x5\n  Internet Address      Physical Address      Type\n  192.168.1.254         a0-b1-c2-d3-e4-f5     dynamic\n";
        assert_eq!(
            parse_windows_arp_mac(arp, "192.168.1.254").as_deref(),
            Some("a0:b1:c2:d3:e4:f5")
        );
        assert_eq!(parse_windows_arp_mac(arp, "192.168.1.1"), None);
    }

    #[test]
    fn ids_are_stable_and_labels_are_friendly() {
        assert_eq!(NetworkId::wifi("Home").id, "wifi:Home");
        let g = NetworkId::gateway("AA-BB-CC-DD-EE-FF");
        assert_eq!(g.id, "gw:aa:bb:cc:dd:ee:ff");
        assert_eq!(g.label, "wired network (router …ee:ff)");
        assert!(!is_mac("00:00:00:00:00:00"));
    }
}
