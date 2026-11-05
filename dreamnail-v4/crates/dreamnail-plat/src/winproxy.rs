//! Windows system proxy via the official WinINET per-connection API.
//!
//! Replaces the old `reg.exe add` approach. Why:
//! * one API call updates BOTH the registry and the `DefaultConnectionSettings`
//!   blob Windows actually uses, and notifies running apps — no child processes
//!   (faster, and nothing for behavioural antivirus to score);
//! * an auto-config script (PAC) or "automatically detect settings" (WPAD)
//!   OVERRIDES a manual proxy in WinINET — common on school/work networks and
//!   with some apps — which left users "connected but not routing". While
//!   connected we switch those off; on disconnect we put them back;
//! * local/LAN addresses bypass the proxy, so the router page, printers,
//!   casting and localhost dev servers keep working;
//! * the user's PREVIOUS proxy settings are saved to disk before we touch them
//!   and restored exactly on disconnect — or on the next launch after a crash.
use std::path::PathBuf;
use windows_sys::Win32::Foundation::GlobalFree;
use windows_sys::Win32::Networking::WinInet::{
    InternetQueryOptionW, InternetSetOptionW, INTERNET_OPTION_PER_CONNECTION_OPTION,
    INTERNET_OPTION_PROXY_SETTINGS_CHANGED, INTERNET_OPTION_REFRESH,
    INTERNET_OPTION_SETTINGS_CHANGED, INTERNET_PER_CONN_AUTOCONFIG_URL, INTERNET_PER_CONN_FLAGS,
    INTERNET_PER_CONN_OPTIONW, INTERNET_PER_CONN_OPTION_LISTW, INTERNET_PER_CONN_PROXY_BYPASS,
    INTERNET_PER_CONN_PROXY_SERVER, PROXY_TYPE_DIRECT, PROXY_TYPE_PROXY,
};

/// Addresses that must never go through the tunnel proxy. `<local>` = any
/// dotless hostname (intranet names, "router", printers).
pub const BYPASS: &str = "<local>;localhost;127.*;10.*;192.168.*;169.254.*;\
172.16.*;172.17.*;172.18.*;172.19.*;172.20.*;172.21.*;172.22.*;172.23.*;\
172.24.*;172.25.*;172.26.*;172.27.*;172.28.*;172.29.*;172.30.*;172.31.*;*.local";

/// One snapshot of the per-connection proxy settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub flags: u32,
    pub server: String,
    pub bypass: String,
    pub pac: String,
}

impl Settings {
    fn to_text(&self) -> String {
        format!("v1\n{}\n{}\n{}\n{}\n", self.flags, self.server, self.bypass, self.pac)
    }
    fn from_text(t: &str) -> Option<Self> {
        let mut l = t.split('\n');
        if l.next()? != "v1" {
            return None;
        }
        Some(Self {
            flags: l.next()?.trim().parse().ok()?,
            server: l.next()?.to_string(),
            bypass: l.next()?.to_string(),
            pac: l.next()?.to_string(),
        })
    }
    /// True if these settings point at OUR local proxy (i.e. a leftover from a
    /// crash) — never save those as "the user's previous settings".
    pub fn is_ours(&self, our_server: &str) -> bool {
        self.flags & PROXY_TYPE_PROXY != 0 && self.server == our_server
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Read a WinINET-allocated wide string and free it.
unsafe fn take_pwstr(p: *mut u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    while *p.add(n) != 0 {
        n += 1;
    }
    let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
    GlobalFree(p as _);
    s
}

/// Current default-connection proxy settings, or None if the query failed.
pub fn query() -> Option<Settings> {
    let mut opts: [INTERNET_PER_CONN_OPTIONW; 4] = unsafe { std::mem::zeroed() };
    opts[0].dwOption = INTERNET_PER_CONN_FLAGS;
    opts[1].dwOption = INTERNET_PER_CONN_PROXY_SERVER;
    opts[2].dwOption = INTERNET_PER_CONN_PROXY_BYPASS;
    opts[3].dwOption = INTERNET_PER_CONN_AUTOCONFIG_URL;
    let mut list: INTERNET_PER_CONN_OPTION_LISTW = unsafe { std::mem::zeroed() };
    list.dwSize = std::mem::size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32;
    list.pszConnection = std::ptr::null_mut(); // default / LAN connection
    list.dwOptionCount = opts.len() as u32;
    list.pOptions = opts.as_mut_ptr();
    let mut size = list.dwSize;
    let ok = unsafe {
        InternetQueryOptionW(
            std::ptr::null(),
            INTERNET_OPTION_PER_CONNECTION_OPTION,
            &mut list as *mut _ as *mut core::ffi::c_void,
            &mut size,
        )
    };
    if ok == 0 {
        return None;
    }
    unsafe {
        Some(Settings {
            flags: opts[0].Value.dwValue,
            server: take_pwstr(opts[1].Value.pszValue),
            bypass: take_pwstr(opts[2].Value.pszValue),
            pac: take_pwstr(opts[3].Value.pszValue),
        })
    }
}

/// Apply settings to the default connection and notify running apps.
pub fn apply(s: &Settings) -> bool {
    let (mut server, mut bypass, mut pac) = (wide(&s.server), wide(&s.bypass), wide(&s.pac));
    let mut opts: [INTERNET_PER_CONN_OPTIONW; 4] = unsafe { std::mem::zeroed() };
    opts[0].dwOption = INTERNET_PER_CONN_FLAGS;
    opts[0].Value.dwValue = s.flags;
    opts[1].dwOption = INTERNET_PER_CONN_PROXY_SERVER;
    opts[1].Value.pszValue = server.as_mut_ptr();
    opts[2].dwOption = INTERNET_PER_CONN_PROXY_BYPASS;
    opts[2].Value.pszValue = bypass.as_mut_ptr();
    opts[3].dwOption = INTERNET_PER_CONN_AUTOCONFIG_URL;
    opts[3].Value.pszValue = pac.as_mut_ptr();
    let mut list: INTERNET_PER_CONN_OPTION_LISTW = unsafe { std::mem::zeroed() };
    list.dwSize = std::mem::size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32;
    list.pszConnection = std::ptr::null_mut();
    list.dwOptionCount = opts.len() as u32;
    list.pOptions = opts.as_mut_ptr();
    let ok = unsafe {
        InternetSetOptionW(
            std::ptr::null(),
            INTERNET_OPTION_PER_CONNECTION_OPTION,
            &list as *const _ as *const core::ffi::c_void,
            list.dwSize,
        )
    } != 0;
    unsafe {
        InternetSetOptionW(std::ptr::null(), INTERNET_OPTION_PROXY_SETTINGS_CHANGED, std::ptr::null(), 0);
        InternetSetOptionW(std::ptr::null(), INTERNET_OPTION_SETTINGS_CHANGED, std::ptr::null(), 0);
        InternetSetOptionW(std::ptr::null(), INTERNET_OPTION_REFRESH, std::ptr::null(), 0);
    }
    ok
}

/// Where the user's pre-VPN settings are parked while connected.
fn saved_path(dir: &std::path::Path) -> PathBuf {
    dir.join("proxy-prev.txt")
}

/// Turn the tunnel proxy on: save the user's current settings (unless they are
/// already ours, i.e. left over from a crash), then point WinINET at us with
/// PAC/auto-detect off and local addresses bypassed.
pub fn enable(dir: &std::path::Path, our_server: &str) -> bool {
    let p = saved_path(dir);
    if !p.exists() {
        if let Some(cur) = query() {
            if !cur.is_ours(our_server) {
                let _ = std::fs::create_dir_all(dir);
                let _ = std::fs::write(&p, cur.to_text());
            }
        }
    }
    apply(&Settings {
        flags: PROXY_TYPE_DIRECT | PROXY_TYPE_PROXY,
        server: our_server.to_string(),
        bypass: BYPASS.to_string(),
        pac: String::new(),
    })
}

/// Turn the tunnel proxy off: restore the saved pre-VPN settings exactly (PAC,
/// auto-detect and any previous proxy come back); with nothing saved, go
/// direct — but only if the current settings are ours, so we never clobber a
/// proxy the user set themselves.
pub fn disable(dir: &std::path::Path, our_server: &str) -> bool {
    let p = saved_path(dir);
    if let Some(prev) = std::fs::read_to_string(&p).ok().and_then(|t| Settings::from_text(&t)) {
        let ok = apply(&prev);
        if ok {
            let _ = std::fs::remove_file(&p);
        }
        return ok;
    }
    let _ = std::fs::remove_file(&p);
    match query() {
        Some(cur) if !cur.is_ours(our_server) => true, // not ours — leave it alone
        _ => apply(&Settings {
            flags: PROXY_TYPE_DIRECT,
            server: String::new(),
            bypass: String::new(),
            pac: String::new(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_roundtrip_and_ours() {
        let s = Settings { flags: 5, server: "a:1".into(), bypass: "<local>".into(), pac: "http://x/p.pac".into() };
        assert_eq!(Settings::from_text(&s.to_text()), Some(s.clone()));
        assert!(!s.is_ours("a:1")); // PROXY bit not set
        let o = Settings { flags: 3, server: "a:1".into(), bypass: String::new(), pac: String::new() };
        assert!(o.is_ours("a:1"));
        assert_eq!(Settings::from_text("garbage"), None);
    }
}
