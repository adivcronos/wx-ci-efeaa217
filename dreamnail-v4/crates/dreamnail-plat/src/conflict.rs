//! Foreign VPN / proxy detection — warns when ANOTHER VPN or system proxy is
//! active, since it can hijack or break our connection (steal the default route,
//! capture the route to our server, or override the system proxy we set).
//!
//! Read-only and antivirus-quiet by design. We only ask the OS the questions any
//! network-aware app asks: which interfaces exist, which interface internet
//! traffic would leave by (a route LOOKUP — no packet is sent), which VPN
//! connections the OS itself reports, and what system proxy is configured. We
//! deliberately do NOT enumerate running processes looking for VPN/competitor
//! names: scanning for specific software is a classic malware behaviour that
//! behavioural antivirus scores up — doubly bad for an app disguised as weather.
//!
//! Our OWN footprint must never be reported. The stealth TUN has no fixed name
//! (the OS hands out tunN / utunN / a wintun adapter), so the FFI snapshots
//! [`tunnel_interfaces`] just before and just after bringing stealth up and
//! passes the difference in as `own`. When that snapshot is uncertain, the checks
//! that could mistake us for a foreign VPN are skipped — fail quiet, never cry
//! wolf about ourselves.
//!
//! False-positive traps handled per OS: macOS always has system `utun0-3`
//! interfaces (iCloud, etc.) even with no VPN, so interface presence is NOT used
//! there; Windows always has Teredo/6to4/ISATAP "tunnel" adapters; Linux VMs use
//! TAP devices (VPNs use TUN).
//!
//! All output parsing lives in pure functions so the macOS/Windows formats are
//! unit-tested on any host.

use crate::netcfg::{EXPECTED_EXIT_IP, HTTP_HOST, HTTP_PORT, SOCKS_HOST, SOCKS_PORT};

/// A route-lookup destination for "general internet traffic". Only a routing
/// table LOOKUP is done for it — nothing is ever sent.
const INTERNET_PROBE: &str = "1.1.1.1";

/// What was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Vpn,
    Proxy,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Vpn => "vpn",
            Kind::Proxy => "proxy",
        }
    }
}

/// One detected conflict. `capturing` = it is actually redirecting traffic
/// (internet or our server's route goes through it, or it is a system proxy) —
/// the case worth a confirm-before-connect, not just a notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub name: String,
    pub kind: Kind,
    pub capturing: bool,
}

/// Names of every tunnel-type interface present right now (any up/down state).
/// Used by the FFI to work out which interface is OUR stealth tunnel (the diff
/// across bringing it up).
#[must_use]
pub fn tunnel_interfaces() -> Vec<String> {
    os::tunnel_interfaces()
}

/// Detect foreign VPNs / proxies.
///
/// * `own` — interfaces that belong to our own stealth tunnel (never reported).
/// * `our_tun_active` — our stealth TUN is currently up (it owns the default
///   route, so the general-internet route check is skipped).
#[must_use]
pub fn detect(own: &[String], our_tun_active: bool) -> Vec<Finding> {
    let mut out = os::detect_vpns(own, our_tun_active);
    if let Some(p) = os::foreign_proxy() {
        out.push(Finding {
            name: p,
            kind: Kind::Proxy,
            capturing: true,
        });
    }
    dedupe(out)
}

fn dedupe(v: Vec<Finding>) -> Vec<Finding> {
    let mut out: Vec<Finding> = Vec::new();
    for f in v {
        if let Some(existing) = out.iter_mut().find(|e| e.name == f.name) {
            existing.capturing |= f.capturing;
        } else {
            out.push(f);
        }
    }
    out
}

// --------------------------------------------------------------------------
// Pure helpers (compiled everywhere, unit-tested on any host)
// --------------------------------------------------------------------------

/// Is `host:port` one of OUR local inbounds (so a proxy pointing at it is ours)?
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
fn is_our_endpoint(host: &str, port: u32) -> bool {
    let h = host.trim().trim_matches('\'').trim_matches('"');
    let local = h == SOCKS_HOST || h == HTTP_HOST || h == "localhost";
    local && (port == u32::from(SOCKS_PORT) || port == u32::from(HTTP_PORT))
}

/// `ip -o route get <dst>` → the `dev` it would leave by (Linux).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_ip_route_dev(out: &str) -> Option<String> {
    let mut it = out.split_whitespace();
    while let Some(tok) = it.next() {
        if tok == "dev" {
            return it.next().map(str::to_string);
        }
    }
    None
}

/// `route -n get <dst>` → the `interface:` line (macOS).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_route_get_iface(out: &str) -> Option<String> {
    out.lines()
        .find_map(|l| l.trim().strip_prefix("interface:"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// `scutil --nc list` → names of VPN services the OS reports as Connected (macOS).
/// Line shape: `* (Connected)   <UUID> PPP --> L2TP   "Name"   [PPP:L2TP]`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_scutil_nc_connected(out: &str) -> Vec<String> {
    out.lines()
        .filter(|l| l.contains("(Connected)"))
        .filter_map(|l| {
            let start = l.find('"')? + 1;
            let end = start + l[start..].find('"')?;
            let name = l[start..end].trim();
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

/// `scutil --proxy` (macOS) → a description of a FOREIGN system proxy, if any.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_scutil_proxy(out: &str) -> Option<String> {
    let get = |k: &str| -> Option<String> {
        out.lines().find_map(|l| {
            let (key, val) = l.split_once(" : ")?;
            (key.trim() == k).then(|| val.trim().to_string())
        })
    };
    let on = |k: &str| get(k).as_deref() == Some("1");
    if on("ProxyAutoConfigEnable") {
        return Some("auto-config (PAC) proxy".into());
    }
    for (en, host, port) in [
        ("SOCKSEnable", "SOCKSProxy", "SOCKSPort"),
        ("HTTPEnable", "HTTPProxy", "HTTPPort"),
        ("HTTPSEnable", "HTTPSProxy", "HTTPSPort"),
    ] {
        if on(en) {
            let h = get(host).unwrap_or_default();
            let p = get(port).and_then(|p| p.parse().ok()).unwrap_or(0);
            if !h.is_empty() && !is_our_endpoint(&h, p) {
                return Some(format!("system proxy {h}:{p}"));
            }
        }
    }
    None
}

/// `reg query "HKCU\...\Internet Settings"` output (Windows) → a description of a
/// FOREIGN WinINET proxy, if any. Value lines look like
/// `    ProxyEnable    REG_DWORD    0x1` — the type/value tokens are never
/// localised, so this parses on any Windows language.
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_reg_internet_settings(out: &str) -> Option<String> {
    let mut enabled = false;
    let mut server = String::new();
    let mut pac = String::new();
    for l in out.lines() {
        let toks: Vec<&str> = l.split_whitespace().collect();
        if toks.len() < 3 || !toks[1].starts_with("REG_") {
            continue;
        }
        let data = toks[2..].join(" ");
        match toks[0] {
            "ProxyEnable" => {
                enabled = u32::from_str_radix(data.trim_start_matches("0x"), 16).unwrap_or(0) == 1
            }
            "ProxyServer" => server = data,
            "AutoConfigURL" => pac = data,
            _ => {}
        }
    }
    if !pac.trim().is_empty() {
        return Some("auto-config (PAC) proxy".into());
    }
    if enabled && !server.trim().is_empty() {
        let ours_s = format!("{SOCKS_HOST}:{SOCKS_PORT}");
        let ours_h = format!("{HTTP_HOST}:{HTTP_PORT}");
        if !(server.contains(&ours_s) || server.contains(&ours_h)) {
            return Some(format!("system proxy {}", server.trim()));
        }
    }
    None
}

/// Windows adapter classification: is this a VPN-style adapter? `if_type` is the
/// IANA ifType (23 PPP, 53 prop-virtual [wintun / WireGuard-NT], 131 tunnel);
/// Ethernet-typed VPN adapters (TAP-Windows, AnyConnect, GlobalProtect,
/// Fortinet…) are caught by description. OS pseudo-tunnels (Teredo, 6to4,
/// ISATAP, IP-HTTPS) and VM adapters are excluded — they're always present.
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_is_vpn_adapter(if_type: u32, description: &str) -> bool {
    let d = description.to_lowercase();
    const NOT_VPN: &[&str] = &[
        "teredo", "6to4", "isatap", "ip-https", "pseudo-interface", "kernel debug",
        "loopback", "hyper-v", "virtualbox", "vmware", "wi-fi direct", "bluetooth",
    ];
    if NOT_VPN.iter().any(|k| d.contains(k)) {
        return false;
    }
    const VPN_WORDS: &[&str] = &[
        "vpn", "tap-windows", "wireguard", "wintun", "openvpn", "tailscale", "nordlynx",
        "anyconnect", "pangp", "globalprotect", "fortinet", "forticlient", "cloudflare",
        "zscaler", "mullvad", "proton", "juniper", "pulse secure", "hamachi", "zerotier",
        "surfshark", "windscribe", "expressvpn", "private internet access",
    ];
    matches!(if_type, 23 | 53 | 131) || VPN_WORDS.iter().any(|k| d.contains(k))
}

/// A friendlier label for a Linux VPN interface name.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn linux_friendly(name: &str) -> String {
    let n = name.to_lowercase();
    let label = if n.starts_with("tailscale") {
        "Tailscale"
    } else if n.starts_with("nordlynx") || n.starts_with("nordtun") {
        "NordVPN"
    } else if n.starts_with("proton") || n.starts_with("pvpn") {
        "Proton VPN"
    } else if n.contains("cloudflarewarp") {
        "Cloudflare WARP"
    } else if n.starts_with("zcctun") {
        "Zscaler"
    } else if n.contains("mullvad") {
        "Mullvad"
    } else if n.starts_with("wg") {
        return format!("WireGuard ({name})");
    } else if n.starts_with("ppp") {
        return format!("PPP VPN ({name})");
    } else {
        return format!("VPN tunnel ({name})");
    };
    format!("{label} ({name})")
}

/// Linux sysfs `tun_flags` (hex, e.g. "0x1001") → is it an L3 TUN (not a VM TAP)?
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn linux_is_l3_tun(tun_flags: &str) -> bool {
    const IFF_TUN: u32 = 0x0001;
    u32::from_str_radix(tun_flags.trim().trim_start_matches("0x"), 16)
        .map(|f| f & IFF_TUN != 0)
        .unwrap_or(false)
}

/// Strip gsettings value quoting: `'127.0.0.1'` → `127.0.0.1`, `uint32 80` → `80`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn gs_clean(v: &str) -> String {
    v.trim()
        .trim_start_matches("uint32 ")
        .trim_start_matches("int32 ")
        .trim_matches('\'')
        .to_string()
}

/// KDE `kioslaverc` proxy value (e.g. `socks://127.0.0.1 11080`,
/// `http://10.0.0.1:8080`) → (host, port).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_kde_proxy(v: &str) -> Option<(String, u32)> {
    let v = v.trim();
    if v.is_empty() {
        return None;
    }
    let rest = v.split_once("://").map_or(v, |(_, r)| r);
    let (host, port) = if let Some((h, p)) = rest.split_once(' ') {
        (h, p)
    } else if let Some((h, p)) = rest.rsplit_once(':') {
        (h, p)
    } else {
        (rest, "0")
    };
    Some((host.trim().to_string(), port.trim().parse().unwrap_or(0)))
}

/// Run a command, returning stdout on success (quietly `None` otherwise).
#[cfg(unix)]
fn cmd_out(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

// --------------------------------------------------------------------------
// Linux
// --------------------------------------------------------------------------
#[cfg(target_os = "linux")]
mod os {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn read(p: &Path) -> String {
        fs::read_to_string(p).unwrap_or_default()
    }

    /// Tunnel-type interfaces: WireGuard, L3 TUN (not VM TAP), PPP.
    fn is_tunnel(dir: &Path) -> bool {
        if read(&dir.join("uevent")).contains("DEVTYPE=wireguard") {
            return true;
        }
        let tf = dir.join("tun_flags");
        if tf.exists() {
            return linux_is_l3_tun(&read(&tf));
        }
        read(&dir.join("type")).trim() == "512" // ARPHRD_PPP
    }

    fn is_up(dir: &Path) -> bool {
        let flags = u32::from_str_radix(read(&dir.join("flags")).trim().trim_start_matches("0x"), 16)
            .unwrap_or(0);
        flags & 0x1 != 0 && read(&dir.join("operstate")).trim() != "down"
    }

    fn scan() -> Vec<(String, bool)> {
        let mut v = Vec::new();
        if let Ok(rd) = fs::read_dir("/sys/class/net") {
            for e in rd.flatten() {
                let dir = e.path();
                if is_tunnel(&dir) {
                    v.push((e.file_name().to_string_lossy().into_owned(), is_up(&dir)));
                }
            }
        }
        v
    }

    pub fn tunnel_interfaces() -> Vec<String> {
        scan().into_iter().map(|(n, _)| n).collect()
    }

    fn route_dev(dst: &str) -> Option<String> {
        // `ip route get` honours policy routing (wg-quick / Tailscale exit nodes
        // route via fwmark rules, which the main table alone would miss).
        cmd_out("ip", &["-o", "route", "get", dst]).and_then(|o| parse_ip_route_dev(&o))
    }

    pub fn detect_vpns(own: &[String], our_tun_active: bool) -> Vec<Finding> {
        let tunnels = scan();
        let is_foreign_tunnel = |dev: &str| {
            !own.iter().any(|o| o == dev) && tunnels.iter().any(|(n, _)| n == dev)
        };
        // If our stealth TUN is up but we couldn't pin down which interface is
        // ours, interface presence would flag US — skip it rather than cry wolf.
        let trust_presence = !(our_tun_active && own.is_empty());
        let mut out = Vec::new();
        if trust_presence {
            for (n, up) in &tunnels {
                if *up && !own.iter().any(|o| o == n) {
                    out.push(Finding { name: linux_friendly(n), kind: Kind::Vpn, capturing: false });
                }
            }
        }
        let mut mark = |dev: String| {
            let name = linux_friendly(&dev);
            match out.iter_mut().find(|f| f.name == name) {
                Some(f) => f.capturing = true,
                None => out.push(Finding { name, kind: Kind::Vpn, capturing: true }),
            }
        };
        // Our own TUN owns the default route in stealth, so only check internet
        // capture when it isn't up. The server route is ALWAYS checked: stealth
        // bypasses it to the physical gateway, so a tunnel there is foreign.
        if !our_tun_active {
            if let Some(dev) = route_dev(INTERNET_PROBE).filter(|d| is_foreign_tunnel(d)) {
                mark(dev);
            }
        }
        if let Some(dev) = route_dev(EXPECTED_EXIT_IP).filter(|d| is_foreign_tunnel(d)) {
            mark(dev);
        }
        out
    }

    fn have(cmd: &str) -> bool {
        cmd_out("sh", &["-c", &format!("command -v {cmd}")]).is_some()
    }

    fn gsettings(schema: &str, key: &str) -> String {
        cmd_out("gsettings", &["get", schema, key]).map(|s| gs_clean(&s)).unwrap_or_default()
    }

    pub fn foreign_proxy() -> Option<String> {
        if have("gsettings") {
            match gsettings("org.gnome.system.proxy", "mode").as_str() {
                "auto" => return Some("auto-config (PAC) proxy".into()),
                "manual" => {
                    for s in ["socks", "http", "https"] {
                        let schema = format!("org.gnome.system.proxy.{s}");
                        let h = gsettings(&schema, "host");
                        let p = gsettings(&schema, "port").parse().unwrap_or(0);
                        if !h.is_empty() && p > 0 && !is_our_endpoint(&h, p) {
                            return Some(format!("system proxy {h}:{p}"));
                        }
                    }
                }
                _ => {}
            }
        }
        for tool in ["kreadconfig6", "kreadconfig5"] {
            if !have(tool) {
                continue;
            }
            let kv = |key: &str| {
                cmd_out(tool, &["--file", "kioslaverc", "--group", "Proxy Settings", "--key", key])
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            };
            match kv("ProxyType").as_str() {
                "2" | "3" => return Some("auto-config (PAC) proxy".into()),
                "1" => {
                    for key in ["socksProxy", "httpProxy", "httpsProxy"] {
                        if let Some((h, p)) = parse_kde_proxy(&kv(key)) {
                            if !h.is_empty() && p > 0 && !is_our_endpoint(&h, p) {
                                return Some(format!("system proxy {h}:{p}"));
                            }
                        }
                    }
                }
                _ => {}
            }
            break;
        }
        None
    }
}

// --------------------------------------------------------------------------
// macOS
// --------------------------------------------------------------------------
#[cfg(target_os = "macos")]
mod os {
    use super::*;

    fn is_tunnel_name(n: &str) -> bool {
        ["utun", "ipsec", "ppp", "tun", "wg"].iter().any(|p| n.starts_with(p))
    }

    pub fn tunnel_interfaces() -> Vec<String> {
        cmd_out("ifconfig", &["-l"])
            .map(|o| o.split_whitespace().filter(|n| is_tunnel_name(n)).map(str::to_string).collect())
            .unwrap_or_default()
    }

    fn route_iface(dst: &str) -> Option<String> {
        cmd_out("route", &["-n", "get", dst]).and_then(|o| parse_route_get_iface(&o))
    }

    pub fn detect_vpns(own: &[String], our_tun_active: bool) -> Vec<Finding> {
        // The OS's own list of connected VPN services (system IKEv2/L2TP and every
        // Network-Extension VPN app register here). Our stealth TUN does not.
        let mut out: Vec<Finding> = cmd_out("scutil", &["--nc", "list"])
            .map(|o| parse_scutil_nc_connected(&o))
            .unwrap_or_default()
            .into_iter()
            .map(|name| Finding { name, kind: Kind::Vpn, capturing: false })
            .collect();
        // Route-based: catches VPNs that DON'T register with the OS (Tunnelblick,
        // wireguard-go, openvpn CLI). utun presence alone is meaningless on macOS
        // (system utun0-3 always exist), so only a route THROUGH one counts.
        let foreign = |i: &str| is_tunnel_name(i) && !own.iter().any(|o| o == i);
        // With our TUN up but its utun unidentified, a route via ANY utun could be
        // ours — skip rather than cry wolf.
        let check_internet = !our_tun_active;
        let mut captured: Option<String> = None;
        if check_internet {
            captured = route_iface(INTERNET_PROBE).filter(|i| foreign(i));
        }
        if captured.is_none() && !(our_tun_active && own.is_empty()) {
            captured = route_iface(EXPECTED_EXIT_IP).filter(|i| foreign(i));
        }
        if let Some(iface) = captured {
            if out.is_empty() {
                out.push(Finding { name: format!("VPN tunnel ({iface})"), kind: Kind::Vpn, capturing: true });
            } else {
                // The OS-reported VPN is almost certainly the one holding the route.
                for f in &mut out {
                    f.capturing = true;
                }
            }
        }
        out
    }

    pub fn foreign_proxy() -> Option<String> {
        cmd_out("scutil", &["--proxy"]).and_then(|o| parse_scutil_proxy(&o))
    }
}

/// A minimal, OS-independent view of one network adapter — exactly the inputs
/// the Windows VPN classifier reasons about. Lifted out of the `#[cfg(windows)]`
/// module so the DECISION (where the connect/disconnect-spam self-detection bug
/// lived) can be unit-tested on any platform, not only when compiled for Windows.
#[derive(Debug, Clone)]
pub struct AdapterView {
    pub index: u32,
    pub friendly: String,
    pub description: String,
    pub vpn: bool,
    pub up: bool,
}

/// Pure decision behind the Windows `detect_vpns`: from the live adapters, our
/// own tunnel's known names, whether our stealth TUN is up, and the interface
/// indices that currently carry the server / internet routes, decide which
/// adapters are FOREIGN VPNs and whether each is capturing traffic.
///
/// The self-detection fix lives here. When our TUN is up it owns the routes to
/// the server and to the internet, so whichever interface INDEX carries those is
/// OURS and is excluded — independent of the `own` NAME list, which goes stale
/// when the wintun adapter is torn down and recreated under a new name during a
/// fast reconnect (the exact connect/disconnect-spam symptom). The route lookups
/// are Windows-only, so their results are passed in and this stays testable.
pub fn windows_classify(
    adapters: &[AdapterView],
    own: &[String],
    our_tun_active: bool,
    server_route_idx: Option<u32>,
    internet_route_idx: Option<u32>,
) -> Vec<Finding> {
    // If our TUN is up but we know none of our own names, presence alone would
    // flag US — trust presence only when that can't misfire.
    let trust_presence = !(our_tun_active && own.is_empty());
    // Indices that are OURS: the routes our stealth TUN owns while it is up.
    let mut our_idx: Vec<u32> = Vec::new();
    if our_tun_active {
        our_idx.extend(server_route_idx);
        our_idx.extend(internet_route_idx);
    }
    // Route-based "capturing" only makes sense when OUR TUN is NOT up; when it is,
    // those routes are ours (excluded above), so a foreign adapter is flagged by
    // presence alone.
    let mut capture_idx: Vec<u32> = Vec::new();
    if !our_tun_active {
        capture_idx.extend(internet_route_idx);
        capture_idx.extend(server_route_idx);
    }
    let label = |a: &AdapterView| {
        if a.description.is_empty() { a.friendly.clone() } else { a.description.clone() }
    };
    adapters
        .iter()
        .filter(|a| {
            a.vpn
                && a.up
                && !own.iter().any(|o| o == &a.friendly)
                && !our_idx.contains(&a.index)
        })
        .filter_map(|a| {
            let capturing = capture_idx.contains(&a.index);
            (capturing || trust_presence)
                .then(|| Finding { name: label(a), kind: Kind::Vpn, capturing })
        })
        .collect()
}

// --------------------------------------------------------------------------
// Windows — native IP Helper API (no PowerShell/process spawns for adapters)
// --------------------------------------------------------------------------
#[cfg(windows)]
mod os {
    use super::*;
    use std::net::Ipv4Addr;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, GetBestInterface, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
        GAA_FLAG_SKIP_MULTICAST, GAA_FLAG_SKIP_UNICAST, IP_ADAPTER_ADDRESSES_LH,
    };

    const ERROR_BUFFER_OVERFLOW: u32 = 111;
    const AF_UNSPEC: u32 = 0;
    const OPER_UP: i32 = 1; // IfOperStatusUp

    struct Adapter {
        index: u32,
        friendly: String,
        description: String,
        vpn: bool,
        up: bool,
    }

    /// NUL-terminated UTF-16 → String.
    ///
    /// # Safety
    /// `p` must be null or point at a NUL-terminated UTF-16 string.
    unsafe fn wide(p: *const u16) -> String {
        if p.is_null() {
            return String::new();
        }
        let mut n = 0usize;
        // SAFETY: caller guarantees NUL termination.
        while unsafe { *p.add(n) } != 0 {
            n += 1;
        }
        // SAFETY: the n units before the NUL are initialised.
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) })
    }

    fn adapters() -> Vec<Adapter> {
        let flags = GAA_FLAG_SKIP_UNICAST
            | GAA_FLAG_SKIP_ANYCAST
            | GAA_FLAG_SKIP_MULTICAST
            | GAA_FLAG_SKIP_DNS_SERVER;
        let mut size: u32 = 16 * 1024;
        for _ in 0..3 {
            // u64-backed so the struct array is suitably aligned.
            let mut buf = vec![0u64; (size as usize).div_ceil(8)];
            let head = buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
            // SAFETY: buffer is `size` bytes, aligned; the API fills it or tells
            // us the size it needs.
            let rc = unsafe {
                GetAdaptersAddresses(AF_UNSPEC, flags, std::ptr::null(), head, &mut size)
            };
            if rc == ERROR_BUFFER_OVERFLOW {
                continue; // `size` now holds the required length — retry
            }
            if rc != 0 {
                return Vec::new();
            }
            let mut out = Vec::new();
            let mut p = head as *const IP_ADAPTER_ADDRESSES_LH;
            while !p.is_null() {
                // SAFETY: p walks the API-built linked list inside `buf`.
                let a = unsafe { &*p };
                // SAFETY: the string fields are NUL-terminated per the API.
                let friendly = unsafe { wide(a.FriendlyName) };
                let description = unsafe { wide(a.Description) };
                // SAFETY: the IfIndex arm of this union is the one the API fills.
                let index = unsafe { a.Anonymous1.Anonymous.IfIndex };
                out.push(Adapter {
                    index,
                    vpn: windows_is_vpn_adapter(a.IfType, &description),
                    up: a.OperStatus == OPER_UP,
                    friendly,
                    description,
                });
                p = a.Next;
            }
            return out;
        }
        Vec::new()
    }

    pub fn tunnel_interfaces() -> Vec<String> {
        adapters().into_iter().filter(|a| a.vpn).map(|a| a.friendly).collect()
    }

    fn best_if(dst: &str) -> Option<u32> {
        let ip: Ipv4Addr = dst.parse().ok()?;
        let mut idx = 0u32;
        // IPAddr is network byte order: the octets as laid out in memory.
        // SAFETY: plain out-parameter.
        let rc = unsafe { GetBestInterface(u32::from_ne_bytes(ip.octets()), &mut idx) };
        (rc == 0).then_some(idx)
    }

    pub fn detect_vpns(own: &[String], our_tun_active: bool) -> Vec<Finding> {
        // Gather the live adapters into the OS-independent view, look up which
        // interface carries the server / internet routes (Windows-only calls),
        // then hand the whole decision to the pure, unit-tested classifier. The
        // self-detection fix (exclude the interface INDEX that is ours while our
        // TUN is up, rather than trust the rename-prone name list) lives there.
        let views: Vec<AdapterView> = adapters()
            .into_iter()
            .map(|a| AdapterView {
                index: a.index,
                friendly: a.friendly,
                description: a.description,
                vpn: a.vpn,
                up: a.up,
            })
            .collect();
        windows_classify(
            &views,
            own,
            our_tun_active,
            best_if(EXPECTED_EXIT_IP),
            best_if(INTERNET_PROBE),
        )
    }

    pub fn foreign_proxy() -> Option<String> {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let out = std::process::Command::new("reg")
            .args(["query", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        parse_reg_internet_settings(&String::from_utf8_lossy(&out.stdout))
    }
}

// --------------------------------------------------------------------------
// Anything else: nothing to detect.
// --------------------------------------------------------------------------
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod os {
    use super::*;
    pub fn tunnel_interfaces() -> Vec<String> {
        Vec::new()
    }
    pub fn detect_vpns(_own: &[String], _our_tun_active: bool) -> Vec<Finding> {
        Vec::new()
    }
    pub fn foreign_proxy() -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_route_dev_parses() {
        let o = "1.1.1.1 via 192.168.4.1 dev wlo1 src 192.168.7.216 uid 1000 \\    cache";
        assert_eq!(parse_ip_route_dev(o).as_deref(), Some("wlo1"));
        let wg = "1.1.1.1 dev wg0 table 51820 src 10.64.0.2 uid 1000 \\    cache";
        assert_eq!(parse_ip_route_dev(wg).as_deref(), Some("wg0"));
        assert_eq!(parse_ip_route_dev("RTNETLINK answers: Network is unreachable"), None);
    }

    #[test]
    fn linux_tun_vs_vm_tap() {
        assert!(linux_is_l3_tun("0x1001")); // IFF_TUN | IFF_NO_PI
        assert!(!linux_is_l3_tun("0x1002")); // IFF_TAP — a VM NIC, not a VPN
        assert!(!linux_is_l3_tun("garbage"));
    }

    #[test]
    fn linux_friendly_names() {
        assert_eq!(linux_friendly("tailscale0"), "Tailscale (tailscale0)");
        assert_eq!(linux_friendly("nordlynx"), "NordVPN (nordlynx)");
        assert_eq!(linux_friendly("wg0"), "WireGuard (wg0)");
        assert_eq!(linux_friendly("tun0"), "VPN tunnel (tun0)");
        assert_eq!(linux_friendly("zcctun0"), "Zscaler (zcctun0)");
    }

    #[test]
    fn our_own_proxy_is_not_foreign() {
        assert!(is_our_endpoint("127.0.0.1", 11080));
        assert!(is_our_endpoint("'127.0.0.1'", 11081));
        assert!(!is_our_endpoint("127.0.0.1", 8080));
        assert!(!is_our_endpoint("10.0.0.5", 11080));
    }

    #[test]
    fn gsettings_and_kde_values() {
        assert_eq!(gs_clean("'manual'\n"), "manual");
        assert_eq!(gs_clean("uint32 11080"), "11080");
        assert_eq!(parse_kde_proxy("socks://127.0.0.1 11080"), Some(("127.0.0.1".into(), 11080)));
        assert_eq!(parse_kde_proxy("http://10.0.0.1:8080"), Some(("10.0.0.1".into(), 8080)));
        assert_eq!(parse_kde_proxy(""), None);
    }

    #[test]
    fn macos_route_and_nc_list() {
        let r = "   route to: 1.1.1.1\ndestination: default\n    gateway: 10.8.0.1\n  interface: utun6\n      flags: <UP,GATEWAY,DONE,STATIC,PRCLONING>\n";
        assert_eq!(parse_route_get_iface(r).as_deref(), Some("utun6"));
        let nc = "Available network connection services in the current set (*=enabled):\n\
* (Connected)      2E1D3A4B-0000-0000-0000-000000000001 VPN (com.nordvpn.macos) \"NordVPN NordLynx\"   [VPN:com.nordvpn.macos]\n\
* (Disconnected)   2E1D3A4B-0000-0000-0000-000000000002 IPSec              \"Work VPN\"          [IPSec]\n\
  (Connected)      2E1D3A4B-0000-0000-0000-000000000003 PPP --> L2TP       \"Home L2TP\"         [PPP:L2TP]\n";
        assert_eq!(parse_scutil_nc_connected(nc), vec!["NordVPN NordLynx", "Home L2TP"]);
        assert!(parse_scutil_nc_connected("Available network connection services...\n").is_empty());
    }

    #[test]
    fn macos_proxy_foreign_vs_ours() {
        let ours = "<dictionary> {\n  HTTPEnable : 1\n  HTTPPort : 11081\n  HTTPProxy : 127.0.0.1\n  SOCKSEnable : 1\n  SOCKSPort : 11080\n  SOCKSProxy : 127.0.0.1\n}\n";
        assert_eq!(parse_scutil_proxy(ours), None);
        let corp = "<dictionary> {\n  HTTPEnable : 1\n  HTTPPort : 3128\n  HTTPProxy : proxy.corp.local\n}\n";
        assert_eq!(parse_scutil_proxy(corp).as_deref(), Some("system proxy proxy.corp.local:3128"));
        let pac = "<dictionary> {\n  ProxyAutoConfigEnable : 1\n  ProxyAutoConfigURLString : http://wpad/wpad.dat\n}\n";
        assert_eq!(parse_scutil_proxy(pac).as_deref(), Some("auto-config (PAC) proxy"));
        assert_eq!(parse_scutil_proxy("<dictionary> {\n  HTTPEnable : 0\n}\n"), None);
    }

    #[test]
    fn windows_registry_proxy() {
        let ours = "\r\nHKEY_CURRENT_USER\\Software\\...\\Internet Settings\r\n    ProxyEnable    REG_DWORD    0x1\r\n    ProxyServer    REG_SZ    socks=127.0.0.1:11080;http=127.0.0.1:11081;https=127.0.0.1:11081\r\n";
        assert_eq!(parse_reg_internet_settings(ours), None);
        let off = "    ProxyEnable    REG_DWORD    0x0\r\n    ProxyServer    REG_SZ    10.1.1.1:8080\r\n";
        assert_eq!(parse_reg_internet_settings(off), None, "disabled proxy is not a conflict");
        let foreign = "    ProxyEnable    REG_DWORD    0x1\r\n    ProxyServer    REG_SZ    10.1.1.1:8080\r\n";
        assert_eq!(parse_reg_internet_settings(foreign).as_deref(), Some("system proxy 10.1.1.1:8080"));
        let pac = "    ProxyEnable    REG_DWORD    0x0\r\n    AutoConfigURL    REG_SZ    http://wpad/proxy.pac\r\n";
        assert_eq!(parse_reg_internet_settings(pac).as_deref(), Some("auto-config (PAC) proxy"));
    }

    #[test]
    fn windows_adapter_classification() {
        assert!(windows_is_vpn_adapter(53, "Wintun Userspace Tunnel"));
        assert!(windows_is_vpn_adapter(53, "WireGuard Tunnel"));
        assert!(windows_is_vpn_adapter(6, "TAP-Windows Adapter V9"));
        assert!(windows_is_vpn_adapter(6, "PANGP Virtual Ethernet Adapter"));
        assert!(windows_is_vpn_adapter(23, "WAN Miniport (PPP)"));
        // Always-present OS pseudo-tunnels and VM NICs must NOT count.
        assert!(!windows_is_vpn_adapter(131, "Teredo Tunneling Pseudo-Interface"));
        assert!(!windows_is_vpn_adapter(131, "Microsoft 6to4 Adapter"));
        assert!(!windows_is_vpn_adapter(6, "Hyper-V Virtual Ethernet Adapter"));
        assert!(!windows_is_vpn_adapter(6, "Intel(R) Wi-Fi 6 AX201 160MHz"));
    }

    // ---- Windows VPN self-detection (the connect/disconnect-spam bug) --------
    // These drive the real Windows decision (`windows_classify`) with synthetic
    // adapters, so the fix is proven on this Linux box — no Windows needed.

    fn av(index: u32, friendly: &str, vpn: bool, up: bool) -> AdapterView {
        AdapterView {
            index,
            friendly: friendly.into(),
            description: String::new(),
            vpn,
            up,
        }
    }

    /// THE BUG: our stealth TUN is up and carrying the routes, but the wintun
    /// adapter came back under a NEW name after a fast reconnect, so the `own`
    /// name list is stale and no longer matches. Before the fix this reported our
    /// OWN tunnel as "another VPN (capturing)". The fix excludes it by the route's
    /// interface INDEX, so it must now be found NOT to be a conflict.
    #[test]
    fn our_own_tunnel_is_not_flagged_when_the_name_list_is_stale() {
        // idx 12 = our live wintun (route to server AND internet go through it);
        // `own` still says "Dreamnail" but the live adapter is now "wintun2".
        let adapters = vec![av(12, "wintun2", true, true)];
        let own = vec!["Dreamnail".to_string()];
        let findings = windows_classify(&adapters, &own, true, Some(12), Some(12));
        assert!(findings.is_empty(), "own tunnel must not self-report: {findings:?}");
    }

    /// Even with an EMPTY own-name list (worst-case stale), an up VPN adapter that
    /// carries our routes while our TUN is active is still excluded by index.
    #[test]
    fn empty_own_list_still_excludes_our_route_bearing_adapter() {
        let adapters = vec![av(7, "wintun", true, true)];
        let findings = windows_classify(&adapters, &[], true, Some(7), Some(7));
        assert!(findings.is_empty(), "index exclusion must survive an empty name list: {findings:?}");
    }

    /// A genuine third-party VPN (different interface index from our routes) must
    /// STILL be detected while our TUN is up — the fix must not blind us.
    #[test]
    fn a_real_foreign_vpn_is_still_detected_while_we_are_connected() {
        // idx 12 = ours (carries the routes); idx 30 = a foreign VPN that is up.
        let adapters = vec![av(12, "Dreamnail", true, true), av(30, "NordLynx", true, true)];
        let own = vec!["Dreamnail".to_string()];
        let findings = windows_classify(&adapters, &own, true, Some(12), Some(12));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].name, "NordLynx");
        // Our TUN owns the routes, so a foreign VPN is flagged by presence, not as
        // "capturing" (it isn't the one holding the default route).
        assert!(!findings[0].capturing);
    }

    /// When our TUN is DOWN, a foreign VPN sitting on the internet route is
    /// correctly reported as capturing.
    #[test]
    fn foreign_vpn_capturing_is_flagged_when_we_are_disconnected() {
        let adapters = vec![av(30, "ProtonVPN", true, true)];
        let findings = windows_classify(&adapters, &[], false, None, Some(30));
        assert_eq!(findings.len(), 1);
        assert!(findings[0].capturing, "route-carrying foreign VPN must be capturing");
    }

    #[test]
    fn dedupe_merges_capturing() {
        let v = vec![
            Finding { name: "wg0".into(), kind: Kind::Vpn, capturing: false },
            Finding { name: "wg0".into(), kind: Kind::Vpn, capturing: true },
        ];
        let d = dedupe(v);
        assert_eq!(d.len(), 1);
        assert!(d[0].capturing);
    }
}
