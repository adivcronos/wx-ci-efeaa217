//! `SystemProxy` — sets/clears the OS system proxy per platform, ported from the
//! Python `platform_utils.SystemProxy`. Only the current target's code compiles
//! (each impl is `#[cfg]`-gated), so the Linux build here never touches the
//! Windows/macOS paths.
//!
//! Linux is verified on this box; macOS (`networksetup`) and Windows (registry +
//! WinINET refresh) mirror the Python logic but are code-verified only.

use dreamnail_core::ports::{Proxy, Result};

#[allow(unused_imports)]
use dreamnail_core::ports::EngineError;

#[derive(Default)]
pub struct SystemProxy;

impl Proxy for SystemProxy {
    fn enable(&mut self) -> Result<()> {
        platform_enable()
    }
    fn disable(&mut self) {
        platform_disable();
    }
}

// --------------------------------------------------------------------------
// Linux — GNOME gsettings + KDE kwriteconfig. We set BOTH when present, because
// Chromium/Brave/Firefox read the GNOME (dconf) proxy even under KDE Plasma.
// --------------------------------------------------------------------------
#[cfg(target_os = "linux")]
fn platform_enable() -> Result<()> {
    use crate::netcfg::{HTTP_HOST, HTTP_PORT, SOCKS_HOST, SOCKS_PORT};

    // Test hook: skip touching any system proxy (for headless end-to-end runs
    // where the SOCKS inbound is used directly). Never set in shipped builds.
    if std::env::var("DREAMNAIL_NO_SYSPROXY").as_deref() == Ok("1") {
        return Ok(());
    }

    // GNOME and KDE are independent stores, so write them at the same time —
    // each is a chain of short-lived processes and this is on the connect path.
    let gnome = std::thread::spawn(|| {
        if !have("gsettings") {
            return false;
        }
        let sp = SOCKS_PORT.to_string();
        let hp = HTTP_PORT.to_string();
        run("gsettings", &["set", "org.gnome.system.proxy", "mode", "manual"])
            && run("gsettings", &["set", "org.gnome.system.proxy.socks", "host", SOCKS_HOST])
            && run("gsettings", &["set", "org.gnome.system.proxy.socks", "port", &sp])
            && run("gsettings", &["set", "org.gnome.system.proxy.http", "host", HTTP_HOST])
            && run("gsettings", &["set", "org.gnome.system.proxy.http", "port", &hp])
            && run("gsettings", &["set", "org.gnome.system.proxy.https", "host", HTTP_HOST])
            && run("gsettings", &["set", "org.gnome.system.proxy.https", "port", &hp])
    });

    let kde = std::thread::spawn(|| {
        let Some(tool) = kde_tool() else { return false };
        let socks = format!("socks://{SOCKS_HOST} {SOCKS_PORT}");
        let http = format!("http://{HTTP_HOST} {HTTP_PORT}");
        let g = ["--file", "kioslaverc", "--group", "Proxy Settings"];
        let ok = run(&tool, &[g[0], g[1], g[2], g[3], "--key", "ProxyType", "1"])
            && run(&tool, &[g[0], g[1], g[2], g[3], "--key", "socksProxy", &socks]);
        run(&tool, &[g[0], g[1], g[2], g[3], "--key", "httpProxy", &http]);
        run(&tool, &[g[0], g[1], g[2], g[3], "--key", "httpsProxy", &http]);
        run(&tool, &[g[0], g[1], g[2], g[3], "--key", "NoProxyFor", "localhost,127.0.0.1"]);
        kde_notify();
        ok
    });

    let applied = gnome.join().unwrap_or(false) | kde.join().unwrap_or(false);
    if applied {
        Ok(())
    } else {
        Err(EngineError::Proxy(
            "no supported desktop proxy backend (need GNOME gsettings or KDE kwriteconfig)".into(),
        ))
    }
}

#[cfg(target_os = "linux")]
fn platform_disable() {
    if have("gsettings") {
        run("gsettings", &["set", "org.gnome.system.proxy", "mode", "none"]);
    }
    if let Some(tool) = kde_tool() {
        let g = ["--file", "kioslaverc", "--group", "Proxy Settings"];
        run(&tool, &[g[0], g[1], g[2], g[3], "--key", "ProxyType", "0"]);
        kde_notify();
    }
}

#[cfg(target_os = "linux")]
fn kde_tool() -> Option<String> {
    for t in ["kwriteconfig6", "kwriteconfig5"] {
        if have(t) {
            return Some(t.to_string());
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn kde_notify() {
    // Tell KIO to reload proxy config in already-running apps.
    run(
        "dbus-send",
        &[
            "--session",
            "--type=signal",
            "/KIO/Scheduler",
            "org.kde.KIO.Scheduler.reparseSlaveConfiguration",
            "string:",
        ],
    );
}

/// Is `cmd` an executable on PATH? Looked up directly instead of spawning a
/// shell for `command -v` (several of those used to run on every connect).
#[cfg(target_os = "linux")]
fn have(cmd: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            std::fs::metadata(dir.join(cmd))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}

#[cfg(target_os = "linux")]
fn run(cmd: &str, args: &[&str]) -> bool {
    let ok = std::process::Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    // A disable() failure is otherwise invisible (the trait returns nothing and we
    // suppress output), so a stranded proxy is undebuggable. Surface it under
    // DREAMNAIL_DEBUG without adding noise to the shipped GUI (no stderr consumer).
    if !ok && std::env::var_os("DREAMNAIL_DEBUG").is_some() {
        eprintln!("[proxy] command failed: {cmd} {}", args.join(" "));
    }
    ok
}

// --------------------------------------------------------------------------
// macOS — networksetup on every active service (SOCKS + web + secure).
// --------------------------------------------------------------------------
#[cfg(target_os = "macos")]
fn platform_enable() -> Result<()> {
    use crate::netcfg::{HTTP_HOST, HTTP_PORT, SOCKS_HOST, SOCKS_PORT};
    let services = macos_services();
    if services.is_empty() {
        return Err(EngineError::Proxy("no network services found".into()));
    }
    let sp = SOCKS_PORT.to_string();
    let hp = HTTP_PORT.to_string();
    let mut applied = false;
    for svc in &services {
        let ok = run("networksetup", &["-setsocksfirewallproxy", svc, SOCKS_HOST, &sp])
            && run("networksetup", &["-setsocksfirewallproxystate", svc, "on"])
            && run("networksetup", &["-setwebproxy", svc, HTTP_HOST, &hp])
            && run("networksetup", &["-setwebproxystate", svc, "on"])
            && run("networksetup", &["-setsecurewebproxy", svc, HTTP_HOST, &hp])
            && run("networksetup", &["-setsecurewebproxystate", svc, "on"]);
        applied |= ok;
    }
    if applied {
        Ok(())
    } else {
        Err(EngineError::Proxy("networksetup rejected all services".into()))
    }
}

#[cfg(target_os = "macos")]
fn platform_disable() {
    for svc in macos_services() {
        for state in [
            "-setsocksfirewallproxystate",
            "-setwebproxystate",
            "-setsecurewebproxystate",
        ] {
            run("networksetup", &[state, &svc, "off"]);
        }
    }
}

#[cfg(target_os = "macos")]
fn macos_services() -> Vec<String> {
    let out = match std::process::Command::new("networksetup")
        .arg("-listallnetworkservices")
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .skip(1) // first line is an explanatory header
        .filter(|l| !l.starts_with('*')) // '*' = a disabled service
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

#[cfg(target_os = "macos")]
fn run(cmd: &str, args: &[&str]) -> bool {
    let ok = std::process::Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok && std::env::var_os("DREAMNAIL_DEBUG").is_some() {
        eprintln!("[proxy] command failed: {cmd} {}", args.join(" "));
    }
    ok
}

// --------------------------------------------------------------------------
// Windows — HKCU WinINET registry + a WinINET "settings changed" refresh.
// --------------------------------------------------------------------------
#[cfg(windows)]
fn our_server() -> String {
    use crate::netcfg::{HTTP_HOST, HTTP_PORT, SOCKS_HOST, SOCKS_PORT};
    format!("socks={SOCKS_HOST}:{SOCKS_PORT};http={HTTP_HOST}:{HTTP_PORT};https={HTTP_HOST}:{HTTP_PORT}")
}

#[cfg(windows)]
fn platform_enable() -> Result<()> {
    // Preferred: the official per-connection API (saves + later restores the
    // user's settings, bypasses LAN, switches off PAC/auto-detect that would
    // otherwise override us). See winproxy.rs.
    if crate::winproxy::enable(&crate::xray::data_dir(), &our_server()) {
        return Ok(());
    }
    // Fallback: the old direct registry write.
    let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    let ok = reg_add(key, "ProxyEnable", "REG_DWORD", "1")
        && reg_add(key, "ProxyServer", "REG_SZ", &our_server())
        && reg_add(key, "ProxyOverride", "REG_SZ", crate::winproxy::BYPASS);
    wininet_refresh();
    if ok {
        Ok(())
    } else {
        Err(EngineError::Proxy("failed to set the Windows proxy".into()))
    }
}

#[cfg(windows)]
fn platform_disable() {
    if crate::winproxy::disable(&crate::xray::data_dir(), &our_server()) {
        return;
    }
    let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    reg_add(key, "ProxyEnable", "REG_DWORD", "0");
    wininet_refresh();
}

#[cfg(windows)]
fn reg_add(key: &str, name: &str, ty: &str, data: &str) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("reg")
        .args(["add", key, "/v", name, "/t", ty, "/d", data, "/f"])
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn wininet_refresh() {
    // Tell WinINET the proxy settings changed so live apps pick it up without a
    // restart (mirrors the Python InternetSetOption calls).
    use windows_sys::Win32::Networking::WinInet::{
        InternetSetOptionW, INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED,
    };
    unsafe {
        InternetSetOptionW(std::ptr::null_mut(), INTERNET_OPTION_SETTINGS_CHANGED, std::ptr::null(), 0);
        InternetSetOptionW(std::ptr::null_mut(), INTERNET_OPTION_REFRESH, std::ptr::null(), 0);
    }
}

// --------------------------------------------------------------------------
// Fallback for any other target so the crate always compiles.
// --------------------------------------------------------------------------
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn platform_enable() -> Result<()> {
    Err(EngineError::Proxy("system proxy unsupported on this OS".into()))
}
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn platform_disable() {}
