// Windows: build as a GUI-subsystem exe so NO console window ever appears,
// however it is launched (UAC prompt, Task Scheduler "remember admin" task…).
// Logging already goes to --log-file on Windows; std's stdout/stderr writes are
// silently ignored when there is no console.
#![cfg_attr(windows, windows_subsystem = "windows")]
//! `dreamnail-tunhelper` — the headless, privileged TUN worker.
//!
//! The GUI (running as the normal user) spawns this elevated, once, when a
//! stealth connection comes up. It creates the TUN device + routes (via
//! tun2proxy) and forwards everything to the app's local SOCKS proxy, then blocks
//! until it's told to stop. Because it has NO GUI, running it as root is safe on
//! every OS — unlike elevating the whole app.
//!
//! Stops (and restores routing/DNS) when EITHER:
//!   * the stop-file appears (the GUI writes it on disconnect), or
//!   * the parent GUI process is gone (crash / quit),
//!
//! whichever comes first. Both are polled on a short cadence.
//!
//! Usage:
//!   dreamnail-tunhelper --proxy 127.0.0.1:11080 --bypass 66.226.144.136 \
//!       --mtu 1460 --mss 1420 --parent-pid 1234 --stop-file /path/tun.stop

use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use dreamnail_plat::{run_tun_blocking, TunCancel, TunRunConfig};

struct Opts {
    proxy: String,
    bypass: String,
    mtu: u16,
    mss: u16,
    udpgw: Option<String>,
    parent_pid: Option<i32>,
    stop_file: Option<PathBuf>,
    ready_file: Option<PathBuf>,
    log_file: Option<PathBuf>,
}

/// Expand a leading `--args-file <path>` into the arguments it holds (one per
/// line, blank lines skipped). Used by the persistent-elevation launchers so a
/// fixed, simple launcher action (`helper --args-file <fixed path>`) can still
/// carry the per-connect parameters — the app writes them to that file first.
/// Any other leading args are kept as-is; only the exact `--args-file <p>` head
/// is expanded, so it can't be smuggled mid-stream.
fn expand_args_file(raw: Vec<String>) -> Result<Vec<String>, String> {
    if raw.len() == 2 && raw[0] == "--args-file" {
        let body = std::fs::read_to_string(&raw[1])
            .map_err(|e| format!("cannot read --args-file {}: {e}", raw[1]))?;
        return Ok(body
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect());
    }
    Ok(raw)
}

fn parse_args() -> Result<Opts, String> {
    parse_from(std::env::args().skip(1).collect())
}

fn parse_from(raw: Vec<String>) -> Result<Opts, String> {
    let mut o = Opts {
        proxy: "127.0.0.1:11080".into(),
        bypass: String::new(),
        mtu: 1460,
        mss: 1420,
        udpgw: None,
        parent_pid: None,
        stop_file: None,
        ready_file: None,
        log_file: None,
    };
    let mut args = expand_args_file(raw)?.into_iter();
    while let Some(flag) = args.next() {
        let mut val = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--proxy" => o.proxy = val()?,
            "--bypass" => o.bypass = val()?,
            "--mtu" => o.mtu = val()?.parse().map_err(|_| "bad --mtu".to_string())?,
            "--mss" => o.mss = val()?.parse().map_err(|_| "bad --mss".to_string())?,
            "--udpgw" => o.udpgw = Some(val()?),
            "--parent-pid" => {
                o.parent_pid = Some(val()?.parse().map_err(|_| "bad --parent-pid".to_string())?)
            }
            "--stop-file" => o.stop_file = Some(PathBuf::from(val()?)),
            "--ready-file" => o.ready_file = Some(PathBuf::from(val()?)),
            "--log-file" => o.log_file = Some(PathBuf::from(val()?)),
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if o.bypass.is_empty() {
        return Err("--bypass <server-ip> is required".into());
    }
    Ok(o)
}

/// Force macOS DNS back to DHCP on every active network service.
///
/// While stealth is up, tun2proxy points the system DNS at the tunnel gateway
/// (10.0.0.1). On disconnect its own restore is unreliable on macOS, so the Mac
/// can keep querying a DNS server that no longer exists — dead internet until the
/// user toggles Wi-Fi. Belt-and-suspenders: after the run unwinds we clear the DNS
/// override on each service (`Empty` = revert to the DHCP-supplied resolver, i.e.
/// whatever the real network hands out). We deliberately do NOT hardcode a public
/// resolver like 1.1.1.1 — restricted networks (the ones stealth exists for) often
/// block those, which would keep DNS broken while disconnected.
#[cfg(target_os = "macos")]
fn reset_macos_dns() {
    use std::process::Command;
    let out = match Command::new("networksetup")
        .arg("-listallnetworkservices")
        .output()
    {
        Ok(o) if o.status.success() => o.stdout,
        _ => return,
    };
    let listing = String::from_utf8_lossy(&out);
    for line in listing.lines().skip(1) {
        // A leading '*' marks a disabled service; skip those and the header.
        let name = line.trim();
        if name.is_empty() || name.starts_with('*') {
            continue;
        }
        let _ = Command::new("networksetup")
            .args(["-setdnsservers", name, "Empty"])
            .status();
    }
    log::info!("reset macOS DNS to DHCP on disconnect");
}

/// Force Windows DNS back to DHCP on every up physical adapter, for the same
/// reason as [`reset_macos_dns`]: while stealth is up tun2proxy points DNS at the
/// tunnel's virtual resolver, and if its own restore is incomplete the box keeps
/// querying a resolver that's gone — dead internet until a reboot. `-Reset` reverts
/// each adapter to "obtain DNS automatically" (DHCP). We touch only physical, up
/// adapters (never the torn-down wintun one). Best-effort; the one case it can't
/// cover is a hard kill. NOTE: like macOS, this assumes DHCP-assigned DNS (true on
/// home/school/corp Wi-Fi); a machine with MANUALLY pinned DNS would revert to DHCP.
#[cfg(windows)]
fn reset_windows_dns() {
    use std::process::Command;
    // One PowerShell call; SilentlyContinue so a locked-down box that forbids it
    // fails quietly rather than noisily.
    let ps = "Get-NetAdapter -Physical | Where-Object { $_.Status -eq 'Up' } | \
              Set-DnsClientServerAddress -ResetServerAddresses -ErrorAction SilentlyContinue";
    // CREATE_NO_WINDOW: the helper runs hidden/elevated, but a console child
    // spawned without this would still pop a fresh terminal window.
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", ps])
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    log::info!("reset Windows DNS to DHCP on disconnect");
}

/// Give Linux the same privileged DNS-restore safety net macOS/Windows have.
///
/// A connect that set up tun2proxy can leave `/etc/resolv.conf` as a read-only
/// bind mount pointing at the tunnel's virtual resolver; if tun2proxy's own Drop
/// doesn't fully restore it, the box keeps querying a resolver that's gone — dead
/// internet until reboot. The unprivileged GUI can't `umount` it (that's why the
/// GUI-side heal is a no-op); the helper runs as root, so it can. Only acts on the
/// broken state (resolv.conf not writable); a healthy one is left alone.
#[cfg(target_os = "linux")]
fn reset_linux_dns() {
    use std::fs::OpenOptions;
    const RC: &str = "/etc/resolv.conf";
    // Up to two mounts can be stacked (tun2proxy's + our pinned one, see
    // `pin_resolv_conf`), so peel until it's writable again.
    for _ in 0..3 {
        if OpenOptions::new().write(true).open(RC).is_ok() {
            break;
        }
        let _ = std::process::Command::new("umount").arg(RC).status();
        log::info!("healed stale /etc/resolv.conf bind mount on disconnect");
    }
    let _ = std::fs::remove_file(PINNED_RESOLV_CONF);
}

/// Where we keep a real, on-disk copy of the tunnel resolv.conf while connected.
#[cfg(target_os = "linux")]
const PINNED_RESOLV_CONF: &str = "/run/dreamnail-resolv.conf";

#[cfg(target_os = "linux")]
const DNS_HEAL_SCRIPT_PATH: &str = "/usr/local/sbin/dreamnail-dns-heal.sh";
#[cfg(target_os = "linux")]
const DNS_HEAL_UNIT_PATH: &str = "/etc/systemd/system/dreamnail-dns-heal.service";

/// The boot-heal script. A stealth session points `/etc/resolv.conf` at the
/// VPN's DNS (via `tun0`); a clean disconnect restores it, but a HARD shutdown
/// can't, leaving DNS dead after reboot (no `tun0`, stale resolver) until the
/// VPN is started again. This runs at every boot and, only when the VPN is NOT
/// up on a systemd-resolved system, restores the canonical resolved symlink.
#[cfg(target_os = "linux")]
const DNS_HEAL_SCRIPT: &str = r#"#!/bin/sh
# Dreamnail DNS boot-heal (installed by the tunnel helper). Safe no-op unless a
# stealth VPN session left /etc/resolv.conf stale after a hard shutdown.
STUB=/run/systemd/resolve/stub-resolv.conf
rm -f /run/dreamnail-resolv.conf 2>/dev/null
if ip link show tun0 >/dev/null 2>&1; then exit 0; fi
[ -e "$STUB" ] || exit 0
systemctl is-active --quiet systemd-resolved || exit 0
if [ -L /etc/resolv.conf ] && [ "$(readlink -f /etc/resolv.conf)" = "$(readlink -f "$STUB")" ]; then
  exit 0
fi
ln -sf ../run/systemd/resolve/stub-resolv.conf /etc/resolv.conf
logger -t dreamnail-dns-heal "restored /etc/resolv.conf -> resolved stub at boot" 2>/dev/null || true
"#;

/// The systemd unit that runs the boot-heal early at each boot.
#[cfg(target_os = "linux")]
const DNS_HEAL_UNIT: &str = r#"[Unit]
Description=Dreamnail DNS boot-heal (restore resolv.conf if a VPN session left it stale)
After=systemd-resolved.service
Wants=systemd-resolved.service
Before=network-online.target nss-lookup.target
DefaultDependencies=no
Conflicts=shutdown.target
Before=shutdown.target

[Service]
Type=oneshot
ExecStart=/usr/local/sbin/dreamnail-dns-heal.sh
RemainAfterExit=no

[Install]
WantedBy=multi-user.target
"#;

/// Install (idempotently) the boot-heal service so a hard shutdown while
/// connected self-heals DNS on the next boot. We run as root here, so this is
/// the right moment. Best-effort: a failure never blocks connecting.
#[cfg(target_os = "linux")]
fn install_dns_heal() {
    use std::os::unix::fs::PermissionsExt;
    let need_script =
        std::fs::read_to_string(DNS_HEAL_SCRIPT_PATH).ok().as_deref() != Some(DNS_HEAL_SCRIPT);
    let need_unit =
        std::fs::read_to_string(DNS_HEAL_UNIT_PATH).ok().as_deref() != Some(DNS_HEAL_UNIT);
    if !need_script && !need_unit {
        return; // already current
    }
    if need_script {
        if let Some(dir) = std::path::Path::new(DNS_HEAL_SCRIPT_PATH).parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(DNS_HEAL_SCRIPT_PATH, DNS_HEAL_SCRIPT) {
            log::warn!("dns-heal: write script failed: {e}");
            return;
        }
        let _ = std::fs::set_permissions(DNS_HEAL_SCRIPT_PATH, std::fs::Permissions::from_mode(0o755));
    }
    if need_unit {
        if let Err(e) = std::fs::write(DNS_HEAL_UNIT_PATH, DNS_HEAL_UNIT) {
            log::warn!("dns-heal: write unit failed: {e}");
            return;
        }
    }
    let _ = std::process::Command::new("systemctl").arg("daemon-reload").status();
    let _ = std::process::Command::new("systemctl")
        .args(["enable", "dreamnail-dns-heal.service"])
        .status();
    log::info!("dns-heal: installed + enabled boot service");
}

/// After a clean disconnect, put `/etc/resolv.conf` back to the canonical
/// systemd-resolved symlink (so it is never left as a stale regular file).
/// Guarded: only on a resolved-managed system with its stub present.
#[cfg(target_os = "linux")]
fn restore_resolved_symlink() {
    const STUB: &str = "/run/systemd/resolve/stub-resolv.conf";
    if !std::path::Path::new(STUB).exists() {
        return;
    }
    let active = std::process::Command::new("systemctl")
        .args(["is-active", "--quiet", "systemd-resolved"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !active {
        return;
    }
    // Already the correct symlink? Leave it.
    if std::fs::symlink_metadata("/etc/resolv.conf")
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
        && std::fs::read_link("/etc/resolv.conf")
            .ok()
            .map(|p| p.ends_with("stub-resolv.conf"))
            .unwrap_or(false)
    {
        return;
    }
    let _ = std::fs::remove_file("/etc/resolv.conf");
    if std::os::unix::fs::symlink("../run/systemd/resolve/stub-resolv.conf", "/etc/resolv.conf").is_ok() {
        log::info!("restored /etc/resolv.conf -> resolved stub on disconnect");
    }
}

/// True while the TOPMOST mount on `/etc/resolv.conf` is a bind of a deleted
/// file (mountinfo lists stacked mounts bottom-to-top, so the last one wins).
///
/// On systemd-resolved systems (Fedora/Nobara default) `/etc/resolv.conf` is a
/// symlink, and mount(2) follows it — so the bind lands on, and mountinfo lists,
/// `/run/systemd/resolve/stub-resolv.conf`, not `/etc/resolv.conf`. Match the
/// resolved path, or the pin never fires there and Steam's sandbox keeps
/// crashing on the deleted source.
#[cfg(target_os = "linux")]
fn resolv_conf_source_deleted() -> bool {
    let target = std::fs::canonicalize("/etc/resolv.conf")
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/etc/resolv.conf".to_string());
    std::fs::read_to_string("/proc/self/mountinfo")
        .map(|m| top_resolv_mount_deleted(&m, &target))
        .unwrap_or(false)
}

#[cfg(any(target_os = "linux", test))]
#[allow(dead_code)]
fn top_resolv_mount_deleted(mountinfo: &str, target: &str) -> bool {
    Some(mountinfo)
        .and_then(|m| {
            m.lines()
                .filter(|l| l.split(' ').nth(4) == Some(target))
                .last()
                .map(|l| {
                    let root = l.split(' ').nth(3).unwrap_or("");
                    root.ends_with("//deleted") || root.contains("\\040(deleted)")
                })
        })
        .unwrap_or(false)
}

/// Re-pin tun2proxy's resolv.conf onto a file that actually exists.
///
/// tun2proxy bind-mounts a temp file over `/etc/resolv.conf` and then deletes it.
/// DNS keeps working, but bubblewrap sandboxes (Steam and its games, Flatpak
/// apps) bind-mount `/etc/resolv.conf` from the host and fail on a deleted
/// source, so they crash while the tunnel is up. For the whole session we watch
/// for that state and, whenever the top mount is a deleted file, copy its
/// contents to a persistent file and swap a read-only bind of it in.
/// Watching (not one-shot) matters: tun2proxy may set DNS up after our first
/// look, or a leftover mount from an earlier run may be there first.
/// tun2proxy's teardown unmounts ours; `reset_linux_dns` cleans up the rest.
#[cfg(target_os = "linux")]
fn pin_resolv_conf(cancel: TunCancel) {
    use std::ffi::CString;
    const RC: &str = "/etc/resolv.conf";
    let src = CString::new(PINNED_RESOLV_CONF).unwrap();
    let dst = CString::new(RC).unwrap();
    while !cancel.is_cancelled() {
        thread::sleep(Duration::from_millis(500));
        if cancel.is_cancelled() || !resolv_conf_source_deleted() {
            continue;
        }
        let body = match std::fs::read(RC) {
            Ok(b) => b,
            Err(e) => {
                log::warn!("pin resolv.conf: read failed: {e}");
                continue;
            }
        };
        if let Err(e) = std::fs::write(PINNED_RESOLV_CONF, &body) {
            log::warn!("pin resolv.conf: write {PINNED_RESOLV_CONF} failed: {e}");
            return;
        }
        // The kernel refuses to mount ON TOP of a deleted-source bind (ENOENT —
        // the very error bwrap hits), so take tun2proxy's mount off first and
        // put ours in its place. tun2proxy's teardown then unmounts ours.
        // Detach (lazy) rather than a plain umount: on a real desktop a plain
        // umount2 + bind still hit ENOENT and left the deleted mount in place,
        // while `umount -l` + bind worked. Peel every deleted layer, and on any
        // failure keep watching instead of giving up — a later try can land.
        // SAFETY: plain umount2/mount(2) calls with valid NUL-terminated paths;
        // null fstype/data are allowed for bind and remount.
        let mut peeled = 0;
        while resolv_conf_source_deleted() && peeled < 8 {
            if unsafe { libc::umount2(dst.as_ptr(), libc::MNT_DETACH) } != 0 {
                log::warn!(
                    "pin resolv.conf: umount failed: {}",
                    std::io::Error::last_os_error()
                );
                break;
            }
            peeled += 1;
        }
        let rc = unsafe {
            libc::mount(src.as_ptr(), dst.as_ptr(), std::ptr::null(), libc::MS_BIND, std::ptr::null())
        };
        if rc != 0 {
            log::warn!(
                "could not pin /etc/resolv.conf (peeled {peeled}): {}; retrying",
                std::io::Error::last_os_error()
            );
            thread::sleep(Duration::from_secs(2));
            continue;
        }
        unsafe {
            libc::mount(
                std::ptr::null(),
                dst.as_ptr(),
                std::ptr::null(),
                libc::MS_REMOUNT | libc::MS_BIND | libc::MS_RDONLY,
                std::ptr::null(),
            );
        }
        log::info!("pinned /etc/resolv.conf to {PINNED_RESOLV_CONF} (sandbox-safe)");
    }
}

/// True if process `pid` is still alive.
#[cfg(unix)]
fn parent_alive(pid: i32) -> bool {
    // SAFETY: kill with signal 0 performs error checking without sending a
    // signal — it only reports whether the pid exists / is signalable.
    unsafe { libc::kill(pid, 0) == 0 }
}
#[cfg(windows)]
fn parent_alive(pid: i32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WAIT_TIMEOUT: u32 = 0x0000_0102;
    // SAFETY: OpenProcess returns a null handle when the parent is gone or
    // un-queryable; a live handle is polled with a 0 timeout, where WAIT_TIMEOUT
    // means the process is still running. The handle is always closed.
    unsafe {
        let h = OpenProcess(SYNCHRONIZE, 0, pid as u32);
        if h.is_null() {
            return false; // parent gone → helper should unwind and exit
        }
        let alive = WaitForSingleObject(h, 0) == WAIT_TIMEOUT;
        CloseHandle(h);
        alive
    }
}
#[cfg(not(any(unix, windows)))]
fn parent_alive(_pid: i32) -> bool {
    true // other OSes rely on the stop-file; parent-death is best-effort.
}

/// Install the logger so tun2proxy's own trace (route/DNS/interface setup, and any
/// failure) — plus the helper's own log:: lines — are captured. `DREAMNAIL_TUN_LOG`
/// overrides the level (default info).
///
/// Sink per OS: unix logs to stderr, which the macOS spawn redirects into
/// /tmp/dreamnail-tunhelper.log. Windows has NO inherited stderr (the UAC
/// Start-Process -Verb RunAs launch can't redirect it), so its output would
/// vanish — instead point env_logger at the file the GUI passed in `--log-file`
/// (in the GUI's temp, which an elevated helper can write and the GUI can read),
/// so `helper_log_hint()` on the GUI side can show WHY stealth failed.
fn init_logger(log_file: Option<&std::path::Path>) {
    let mut builder = env_logger::Builder::from_env(
        env_logger::Env::default().filter_or("DREAMNAIL_TUN_LOG", "info"),
    );
    builder.format_timestamp_millis();
    #[cfg(windows)]
    if let Some(path) = log_file {
        if let Ok(f) = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
        {
            builder.target(env_logger::Target::Pipe(Box::new(f)));
        }
    }
    #[cfg(not(windows))]
    let _ = log_file;
    let _ = builder.try_init();
}

#[cfg(windows)]
/// `--runas-launch <args-file>`: re-launch THIS helper elevated via
/// `ShellExecuteExW("runas")`, owned by the foreground window (the app the user
/// just clicked Connect in), hidden. Windows only shows the UAC prompt in front
/// when the request is tied to a foreground window — the old path (a hidden
/// PowerShell running Start-Process -Verb RunAs) often got the prompt parked as
/// a flashing taskbar icon, so connect looked frozen for up to 2 minutes.
/// Exit codes: 0 = elevated helper started, 1 = prompt declined, 2 = error.
fn runas_launch(args_file: &str) -> i32 {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_CANCELLED};
    use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SW_HIDE};
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return 2,
    };
    let w = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let params = format!("--args-file \"{args_file}\"");
    let (verb, file, par) = (w("runas"), w(&exe.to_string_lossy()), w(&params));
    // SAFETY: SHELLEXECUTEINFOW is plain data (zeroed = valid empty); every
    // pointer we store outlives the call.
    unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.hwnd = GetForegroundWindow();
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = par.as_ptr();
        info.nShow = SW_HIDE;
        if ShellExecuteExW(&mut info) == 0 {
            return if GetLastError() == ERROR_CANCELLED { 1 } else { 2 };
        }
    }
    0
}

fn main() {
    #[cfg(windows)]
    {
        let a: Vec<String> = std::env::args().skip(1).collect();
        if a.len() == 2 && a[0] == "--runas-launch" {
            std::process::exit(runas_launch(&a[1]));
        }
    }
    // Detach from whatever launched us before doing anything else. The macOS
    // elevation trampoline (`do shell script … with administrator privileges`)
    // SIGHUPs its session when it exits, which would kill us the instant it
    // returns. Ignore HUP and start our own session so a hangup can't reach us.
    // This is the belt to the shell's `trap '' HUP` suspenders (see plat/tun.rs);
    // macOS ships no `setsid(1)` and `nohup` fails there with ENOTTY, so we do it
    // ourselves. Best-effort — a failing setsid (e.g. already a group leader) is
    // harmless because ignoring HUP already covers the kill.
    // SAFETY: signal/setsid only alter this process's own signal disposition and
    // session; neither dereferences memory nor can corrupt state.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::setsid();
    }

    // Parse args BEFORE the logger — on Windows the log sink is a file whose path
    // the GUI passes in `--log-file`, so we need the parsed opts to install it.
    let opts = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("dreamnail-tunhelper: {e}");
            std::process::exit(2);
        }
    };

    init_logger(opts.log_file.as_deref());

    // Clean slate: a leftover stop-file from a previous run must not make us exit
    // immediately.
    if let Some(sf) = &opts.stop_file {
        let _ = std::fs::remove_file(sf);
    }

    let cfg = match TunRunConfig::parse(
        &opts.proxy,
        &opts.bypass,
        opts.mtu,
        opts.mss,
        opts.udpgw.as_deref(),
    ) {
        Ok(c) => c,
        Err(e) => {
            log::error!("{e}");
            std::process::exit(2);
        }
    };

    // Watchdog: trip the cancel token when told to stop, so tun2proxy unwinds and
    // restores routing on the main thread.
    let cancel = TunCancel::new();
    let watch = cancel.clone();
    let stop_file = opts.stop_file.clone();
    let parent_pid = opts.parent_pid;
    thread::spawn(move || loop {
        thread::sleep(Duration::from_millis(500));
        let stop = stop_file.as_ref().map(|p| p.exists()).unwrap_or(false);
        let orphaned = parent_pid.map(|p| !parent_alive(p)).unwrap_or(false);
        if stop || orphaned {
            watch.cancel();
            break;
        }
    });

    log::info!(
        "up (proxy={}, bypass={}, mtu={})",
        cfg.proxy_addr,
        cfg.bypass_ip,
        cfg.mtu
    );
    // Signal the GUI that we've launched (elevation succeeded, tun2proxy is about
    // to come up). The GUI's phase-1 wait watches for this, so the time the user
    // spent at the admin-password prompt doesn't count against the routing budget.
    if let Some(rf) = &opts.ready_file {
        let _ = std::fs::write(rf, "1");
    }
    #[cfg(target_os = "linux")]
    {
        // Install the boot-heal now (we're root) so a hard shutdown while
        // connected self-heals DNS on the next boot.
        install_dns_heal();
        let c = cancel.clone();
        thread::spawn(move || pin_resolv_conf(c));
    }
    let code = match run_tun_blocking(cfg, cancel) {
        Ok(()) => 0,
        Err(e) => {
            log::error!("exited with error: {e}");
            1
        }
    };
    // macOS/Windows: guarantee DNS is usable again after disconnect (see fn docs).
    // Runs on both clean stop and error exit — the one path we can't cover is a
    // hard SIGKILL / TerminateProcess, which no in-process cleanup can.
    #[cfg(target_os = "macos")]
    reset_macos_dns();
    #[cfg(windows)]
    reset_windows_dns();
    #[cfg(target_os = "linux")]
    {
        reset_linux_dns();
        // Leave resolv.conf as the canonical resolved symlink, never a stale
        // regular file — so even if something later goes wrong, DNS is healthy.
        restore_resolved_symlink();
    }
    // Best-effort: clear the stop-file + ready-file so the next run starts clean.
    if let Some(sf) = &opts.stop_file {
        let _ = std::fs::remove_file(sf);
    }
    if let Some(rf) = &opts.ready_file {
        let _ = std::fs::remove_file(rf);
    }
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::{expand_args_file, parse_from};

    #[test]
    fn args_file_expands_to_its_lines() {
        let dir = std::env::temp_dir().join(format!("dn-argsfile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.args");
        std::fs::write(&f, "--proxy\n127.0.0.1:11080\n\n--bypass\n66.226.144.136\n").unwrap();
        let out = expand_args_file(vec!["--args-file".into(), f.to_string_lossy().into()]).unwrap();
        assert_eq!(out, vec!["--proxy", "127.0.0.1:11080", "--bypass", "66.226.144.136"]);
        // And it parses through to a valid Opts (bypass is required).
        let o = parse_from(vec!["--args-file".into(), f.to_string_lossy().into()]).unwrap();
        assert_eq!(o.bypass, "66.226.144.136");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_deleted_top_resolv_mount() {
        use super::top_resolv_mount_deleted as f;
        let deleted = "251 44 0:42 /.tmpSL94//deleted /etc/resolv.conf ro,noatime shared:73 - tmpfs tmpfs rw\n";
        let pinned = "300 251 0:30 /dreamnail-resolv.conf /etc/resolv.conf ro shared:9 - tmpfs tmpfs rw\n";
        let other = "1022 54 0:46 / /run/credentials/systemd-resolved.service ro - tmpfs tmpfs rw\n";
        let rc = "/etc/resolv.conf";
        assert!(f(&format!("{other}{deleted}"), rc));
        assert!(!f(&format!("{deleted}{pinned}"), rc), "our pin on top = healthy");
        assert!(!f(other, rc));
    }

    #[test]
    fn detects_deleted_mount_on_resolved_stub() {
        // /etc/resolv.conf -> stub-resolv.conf symlink: the bind lands on the stub.
        use super::top_resolv_mount_deleted as f;
        let stub = "/run/systemd/resolve/stub-resolv.conf";
        let deleted = "779 54 0:47 /.tmp5R0j//deleted /run/systemd/resolve/stub-resolv.conf ro,noatime shared:115 - tmpfs tmpfs rw\n";
        let pinned = "800 779 0:30 /dreamnail-resolv.conf /run/systemd/resolve/stub-resolv.conf ro shared:9 - tmpfs tmpfs rw\n";
        assert!(f(deleted, stub));
        assert!(!f(deleted, "/etc/resolv.conf"), "old exact-path match missed this");
        assert!(!f(&format!("{deleted}{pinned}"), stub));
    }

    #[test]
    fn plain_args_are_untouched() {
        let out = expand_args_file(vec!["--bypass".into(), "1.2.3.4".into()]).unwrap();
        assert_eq!(out, vec!["--bypass", "1.2.3.4"]);
        // --args-file only expands as the exact 2-token head, never mid-stream.
        let mixed = vec!["--proxy".into(), "x".into(), "--args-file".into(), "y".into()];
        assert_eq!(expand_args_file(mixed.clone()).unwrap(), mixed);
    }
}
