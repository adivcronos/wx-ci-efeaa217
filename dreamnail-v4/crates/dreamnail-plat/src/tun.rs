//! `TunProxy` — transparent, system-wide routing through a TUN device instead of
//! setting the OS system proxy.
//!
//! Traffic is captured at the IP layer and forwarded to the local Xray SOCKS
//! inbound, so apps and games that ignore or actively *reject* a configured
//! system proxy still route through the tunnel — and nothing appears in the OS
//! proxy settings for them to detect.
//!
//! # Why a separate helper process
//!
//! Creating the TUN interface + editing the routing table needs root /
//! Administrator. The old design got that by relaunching the WHOLE GUI as admin,
//! which is broken on every OS (Linux: the GTK image sandbox can't run as root
//! and the app crashes; macOS: the desktop won't draw a root window; Windows: UAC
//! may elevate a different account). So instead the GUI stays as the normal user
//! and `enable()` spawns a small **headless** privileged helper
//! (`dreamnail-tunhelper`, shipped beside the app) through the platform's
//! elevation prompt (pkexec / osascript / UAC). The helper does the tun2proxy
//! work and self-terminates when we drop a stop-file (or when the GUI exits). No
//! GUI in the elevated process → none of the above failure modes.
//!
//! `enable()` is idempotent, so an SSH reconnect (which re-runs bring-up) does
//! NOT re-prompt — the helper spawned on the first connect keeps running. The
//! one exception is the helper itself dying: `alive()` then reports the session
//! dead and forgets it, so the watchdog's reconnect re-launches it (one prompt)
//! rather than leaving traffic to leak direct under a "connected" status.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use dreamnail_core::ports::{EngineError, Proxy, Result};

use crate::netcfg::{EXPECTED_EXIT_IP, SOCKS_HOST, SOCKS_PORT, UDPGW_LOCAL_HOST, UDPGW_LOCAL_PORT};

/// How long PHASE 2 (`enable()`) waits for routing to actually carry traffic,
/// AFTER the helper has signalled it's up, before tearing the half-set state
/// back down. This clock deliberately excludes the elevation-prompt time (that's
/// phase 1) so a slow admin-password entry can't burn the routing budget.
const SETUP_TIMEOUT: Duration = Duration::from_secs(30);
/// How long PHASE 1 waits for the human at the elevation prompt AND the helper to
/// come up (it drops a ready-file). Generous: this is human time, not machine
/// time, so it must not be the thing that fails on a slow prompt.
const AUTH_TIMEOUT: Duration = Duration::from_secs(120);
/// Poll cadence while waiting for the helper / routing to come up.
const PROBE_INTERVAL: Duration = Duration::from_millis(100);
/// Upper bound on reaping the elevation launcher after we've told the helper to
/// stop. Past this we DETACH rather than block — the helper is a separate,
/// stop-file-driven process, so the OS will reap it; we must never let a hung
/// tun2proxy teardown wedge the single engine actor thread forever.
const REAP_TIMEOUT: Duration = Duration::from_secs(5);
/// How long `disable()` waits for the helper to finish unwinding (remove its
/// ready-file) before giving up. Bounded so a wedged teardown can't hang the
/// engine actor, but long enough that a normal wintun/utun/TUN removal completes
/// — so a quick reconnect doesn't collide with the outgoing device.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(6);

/// Reap the launcher without risking a permanent block on the engine actor.
/// Polls `try_wait` up to [`REAP_TIMEOUT`], then gives up (the OS reaps the
/// detached process). We wait only to clear the child zombie, never for
/// correctness — the stop-file is what actually tells the helper to unwind.
fn reap_bounded(child: &mut Child) {
    let deadline = Instant::now() + REAP_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return, // exited (reaped) or un-waitable
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            return; // detach — never block the actor on a hung teardown
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Upper bound for the TUN device MTU. Larger = fewer packets for the userspace
/// TCP stack to process (the real throughput bottleneck), and it's safe to go
/// high because the outer transport is TCP. A live MTU sweep on a healthy link
/// peaked at 1460 (~109 Mbps), so auto-sizing never exceeds this. Override the
/// whole decision with `DREAMNAIL_TUN_MTU`.
const DEFAULT_TUN_MTU: u16 = 1460;
/// Header overhead reserved below the MTU when deriving the TCP MSS (IPv4 20 +
/// TCP 20). Override the resulting MSS with `DREAMNAIL_TUN_MSS`.
const MSS_HEADROOM: u16 = 40;

/// The MTU weak-network mode pins. 1280 is the IPv6 minimum — the classic "works
/// on any network" floor — small enough to clear practically every black-holing
/// middlebox (schools, captive Wi-Fi) that silently drops full-size packets.
const WEAKNET_TUN_MTU: u16 = 1280;
/// MTU used when the path probe is inconclusive (ICMP blocked, server
/// unreachable). Conservative enough to clear most black holes, high enough to
/// stay fast — better than blindly using the full 1460 on an unknown link.
const SAFE_FALLBACK_MTU: u16 = 1320;
/// Smallest MTU auto-sizing will derive from a *successful* probe.
const MIN_AUTO_MTU: u16 = 1200;
/// Bytes kept below the measured *path* MTU when deriving the TUN MTU. The outer
/// transport wraps every inner packet (outer IP+TCP 40, SSH/TLS record framing,
/// SOCKS header), so the TUN device must sit this far under what the path can
/// actually carry or large inner packets get silently black-holed anyway.
const PATH_OVERHEAD: u16 = 120;
/// Total (L3) packet sizes the path probe sweeps, largest first. The first size
/// that reaches the server unfragmented is taken as the path MTU. 1400 is the
/// floor of the sweep because anything at/under it minus overhead lands at the
/// 1280 safe value regardless.
const PROBE_SIZES: &[u16] = &[1500, 1460, 1400];

/// The MTU to hand the TUN device.
///
/// `DREAMNAIL_TUN_MTU` forces an exact value (power users / tests) and always
/// wins. Otherwise weak-network mode pins [`WEAKNET_TUN_MTU`]; failing that we
/// *probe the path to the server and size ourselves under it*, so the tunnel
/// self-heals on a black-holing network instead of waiting for a human to notice
/// pages hanging. An inconclusive probe falls back to [`SAFE_FALLBACK_MTU`].
fn tun_mtu() -> u16 {
    if let Some(m) = std::env::var("DREAMNAIL_TUN_MTU")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|&m| m >= 576)
    {
        return m;
    }
    if crate::weaknet_mode() {
        return WEAKNET_TUN_MTU;
    }
    match probe_path_mtu() {
        Some(path) => path
            .saturating_sub(PATH_OVERHEAD)
            .clamp(MIN_AUTO_MTU, DEFAULT_TUN_MTU),
        None => SAFE_FALLBACK_MTU,
    }
}

/// Probe the path MTU to the VPN server with a descending DF-ping sweep. Returns
/// the largest total packet size (bytes) that reaches the server without
/// fragmentation, or `None` if ICMP is blocked / the server is unreachable (the
/// caller then uses a safe fallback). Bounded: at most [`PROBE_SIZES`] pings, and
/// it stops at the first success — on a healthy link that's the very first ping.
fn probe_path_mtu() -> Option<u16> {
    for &total in PROBE_SIZES {
        if df_ping(EXPECTED_EXIT_IP, total) {
            return Some(total);
        }
    }
    None
}

/// One "don't-fragment" ping of a given total (L3) packet size. `true` = a reply
/// came back (the packet fit the path); `false` = too big (fragmentation needed),
/// lost, or ICMP blocked. Payload = total − 28 (IPv4 header 20 + ICMP header 8).
/// Two echoes per size so a single drop on a lossy link isn't read as "too big".
fn df_ping(host: &str, total: u16) -> bool {
    let payload = total.saturating_sub(28).to_string();
    let mut cmd = std::process::Command::new("ping");
    #[cfg(target_os = "linux")]
    cmd.args(["-M", "do", "-c", "2", "-W", "1", "-s", &payload, host]);
    #[cfg(target_os = "macos")]
    cmd.args(["-D", "-c", "2", "-t", "1", "-s", &payload, host]);
    #[cfg(target_os = "windows")]
    cmd.args(["-f", "-n", "2", "-w", "1000", "-l", &payload, host]);
    let out = match cmd.output() {
        Ok(o) => o,
        Err(_) => return false, // no `ping` binary → fall back to safe MTU
    };
    #[cfg(windows)]
    {
        // Windows `ping` exits 0 even when every echo failed ("Packet needs to be
        // fragmented but DF set." / "Request timed out."), so a reply is only real
        // if the output actually reports a TTL.
        let s = String::from_utf8_lossy(&out.stdout).to_ascii_lowercase();
        s.contains("ttl=")
    }
    #[cfg(not(windows))]
    {
        out.status.success()
    }
}

fn tcp_mss(mtu: u16) -> u16 {
    std::env::var("DREAMNAIL_TUN_MSS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(mtu.saturating_sub(MSS_HEADROOM))
}

#[derive(Default)]
pub struct TunProxy {
    inner: Option<Handle>,
}

/// A live stealth session: the stop-file the helper watches, and the launcher
/// process (pkexec/osascript/powershell) to reap. Dropping the stop-file makes
/// the helper unwind tun2proxy, which restores the routing table + DNS.
struct Handle {
    stop_file: PathBuf,
    ready_file: PathBuf,
    launcher: Child,
}

impl Proxy for TunProxy {
    fn enable(&mut self) -> Result<()> {
        if self.inner.is_some() {
            return Ok(()); // idempotent — a reconnect must not re-prompt
        }

        // A connect that failed partway through a previous tun2proxy setup can
        // leave /etc/resolv.conf as a read-only bind mount; clear that first.
        heal_stale_resolv_conf();

        let helper = helper_path()?;
        let stop_file = stop_file_path();
        let ready_file = ready_file_path();
        let _ = std::fs::remove_file(&stop_file); // clean slate
        let _ = std::fs::remove_file(&ready_file);

        let args = helper_args(&stop_file, &ready_file);
        let mut launcher = spawn_elevated(&helper, &args)?;

        // ── PHASE 1: wait for the human + the helper to actually start. ─────────
        // The helper drops the ready-file the instant it's running, so the seconds
        // spent finding + typing the admin password do NOT count against the
        // routing budget (that mismatch is what timed out and fell back to proxy
        // on a slow prompt). A launcher that exits NON-zero here = the prompt was
        // declined/cancelled. A zero exit is normal on macOS/Windows (the launcher
        // returns once the helper is backgrounded) — keep waiting for the ready-
        // file; if it never appears, the helper died on launch.
        let auth_deadline = Instant::now() + AUTH_TIMEOUT;
        loop {
            if ready_file.exists() {
                break;
            }
            // Disconnect/Cancel pressed while the elevation prompt is up. This is
            // the longest wait in the whole app (AUTH_TIMEOUT is human time), and
            // it runs ON the single engine thread — so without this check the
            // queued disconnect would sit behind it for up to two minutes.
            if crate::cancel::abort_requested() {
                let _ = std::fs::write(&stop_file, "1");
                reap_bounded(&mut launcher);
                return Err(EngineError::Proxy("cancelled".into()));
            }
            if let Ok(Some(status)) = launcher.try_wait() {
                if !status.success() {
                    let _ = std::fs::write(&stop_file, "1");
                    return Err(EngineError::Proxy(
                        "stealth needs admin/root — the elevation prompt was declined, \
                         falling back to standard routing"
                            .into(),
                    ));
                }
            }
            if Instant::now() >= auth_deadline {
                let _ = std::fs::write(&stop_file, "1");
                reap_bounded(&mut launcher);
                return Err(EngineError::Proxy(format!(
                    "stealth helper did not start within {}s{}",
                    AUTH_TIMEOUT.as_secs(),
                    helper_log_hint()
                )));
            }
            std::thread::sleep(PROBE_INTERVAL);
        }

        // ── PHASE 2: helper is up — wait for routing to actually carry traffic. ─
        // A *direct* (un-proxied) request only reaches the server's exit IP if the
        // default route now goes through the tun device, confirming routing is
        // live AND carrying our traffic. The probe is DNS-free on purpose (see
        // `routed_via_server`).
        let route_deadline = Instant::now() + SETUP_TIMEOUT;
        loop {
            if crate::cancel::abort_requested() {
                let _ = std::fs::write(&stop_file, "1"); // tell the helper to unwind
                reap_bounded(&mut launcher);
                let _ = std::fs::remove_file(&ready_file);
                heal_stale_resolv_conf();
                return Err(EngineError::Proxy("cancelled".into()));
            }
            if routed_via_server() {
                self.inner = Some(Handle {
                    stop_file,
                    ready_file,
                    launcher,
                });
                return Ok(());
            }
            if Instant::now() >= route_deadline {
                let _ = std::fs::write(&stop_file, "1"); // tell the helper to unwind
                reap_bounded(&mut launcher);
                let _ = std::fs::remove_file(&ready_file);
                heal_stale_resolv_conf();
                return Err(EngineError::Proxy(format!(
                    "stealth (TUN) routing did not come up within {}s{}",
                    SETUP_TIMEOUT.as_secs(),
                    helper_log_hint()
                )));
            }
            std::thread::sleep(PROBE_INTERVAL);
        }
    }

    fn disable(&mut self) {
        if let Some(mut h) = self.inner.take() {
            // Dropping the stop-file makes the helper unwind tun2proxy (restoring
            // the routing table + DNS) and exit; the launcher then exits too. Reap
            // it with a bound so a hung teardown can't wedge the engine actor.
            let _ = std::fs::write(&h.stop_file, "1");
            // WAIT for the helper to actually finish unwinding before returning.
            // The launcher (pkexec/osascript/powershell) can exit the instant the
            // elevated helper is backgrounded — long BEFORE tun2proxy has removed
            // the TUN device and restored routing. On Windows especially, tearing
            // down the wintun adapter takes a moment, and a connect that starts
            // while the old adapter is still going away collides — the exact
            // "spam connect/disconnect breaks the tunnel" failure. The helper
            // deletes its own ready-file as its last act, so wait (bounded) for
            // that to disappear: it means the device is really gone.
            let deadline = Instant::now() + TEARDOWN_TIMEOUT;
            while h.ready_file.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
            reap_bounded(&mut h.launcher);
            let _ = std::fs::remove_file(&h.stop_file);
            let _ = std::fs::remove_file(&h.ready_file);
            heal_stale_resolv_conf();
        }
    }

    fn alive(&mut self) -> bool {
        let Some(h) = self.inner.as_mut() else {
            return true; // not our routing mode right now — nothing to lose
        };
        // The helper deletes its ready-file on EVERY exit path (clean stop or
        // tun2proxy error). On Linux pkexec also stays as its parent, so a hard
        // kill shows up as the launcher exiting; on macOS/Windows the launcher
        // returns as soon as the helper is backgrounded, so only the file counts.
        let gone = !h.ready_file.exists()
            || (cfg!(target_os = "linux") && matches!(h.launcher.try_wait(), Ok(Some(_))));
        if gone {
            // Forget the dead session so the watchdog's reconnect re-launches the
            // helper (enable() is a no-op while `inner` is set), and heal the
            // resolver it may have left read-only.
            if let Some(mut h) = self.inner.take() {
                let _ = std::fs::write(&h.stop_file, "1");
                reap_bounded(&mut h.launcher);
                let _ = std::fs::remove_file(&h.stop_file);
                let _ = std::fs::remove_file(&h.ready_file);
            }
            heal_stale_resolv_conf();
        }
        !gone
    }
}

/// Tell an ORPHANED helper from a previous session to unwind, and heal anything
/// it left behind.
///
/// The helper watches our pid and stops when we die, but that watchdog can't run
/// after a hard kill (SIGKILL / TerminateProcess / power loss) — which leaves a
/// TUN device, a hijacked default route and a rewritten resolv.conf behind, i.e.
/// no internet at all until a reboot. On startup we drop the stop-file the helper
/// polls, give it a moment to restore routing, then clear the marker files.
/// Idempotent and cheap when there was nothing to clean up.
pub fn stop_stale_helper() {
    let ready = ready_file_path();
    if ready.exists() {
        let stop = stop_file_path();
        let _ = std::fs::write(&stop, "1");
        // The helper polls the stop-file every 500ms and then has to unwind
        // tun2proxy; give it a beat before we clear the markers.
        std::thread::sleep(Duration::from_millis(1500));
        let _ = std::fs::remove_file(&stop);
        let _ = std::fs::remove_file(&ready);
    }
    heal_stale_resolv_conf();
}

/// Absolute path to the privileged helper, shipped next to the app binary.
/// `pub(crate)` so the persistent-elevation module can pin its polkit rule /
/// service to this exact binary.
pub(crate) fn helper_path() -> Result<PathBuf> {
    let exe = std::env::current_exe()
        .map_err(|e| EngineError::Proxy(format!("cannot locate app dir: {e}")))?;
    let dir = exe
        .parent()
        .ok_or_else(|| EngineError::Proxy("app has no parent dir".into()))?;
    let name = if cfg!(windows) {
        "dreamnail-tunhelper.exe"
    } else {
        "dreamnail-tunhelper"
    };
    let p = dir.join(name);
    if p.exists() {
        Ok(p)
    } else {
        Err(EngineError::Proxy(format!(
            "stealth helper not found at {} — reinstall the app",
            p.display()
        )))
    }
}

/// Copy the bundled helper to a stable path that **root** can read, and return
/// it. Needed on Linux because pkexec runs the helper as root: when the app runs
/// from an AppImage, the bundled helper lives inside a FUSE mount created with
/// `user_id=<uid>` and no `allow_other`, so root cannot exec it — the helper
/// never starts and stealth silently falls back to proxy. The AppImage mount
/// path is also random per launch. Staging to `~/.dreamnail/bin` fixes both.
///
/// Idempotent: reuses the staged copy when its size already matches the bundled
/// helper (same build), and replaces atomically (temp + rename) so overwriting
/// never hits ETXTBSY against a helper that is currently running. On a normal
/// (.deb) install this is just a cheap same-box copy; callers fall back to the
/// bundled path if staging fails for any reason.
#[cfg(target_os = "linux")]
fn stage_helper_for_root(bundled: &std::path::Path) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let dir = crate::xray::data_dir().join("bin");
    std::fs::create_dir_all(&dir)?;
    let staged = dir.join("dreamnail-tunhelper");
    let src_len = std::fs::metadata(bundled)?.len();
    if std::fs::metadata(&staged).map(|m| m.len()) .ok() == Some(src_len) {
        return Ok(staged); // already staged, same build — skip the copy
    }
    let tmp = dir.join(".dreamnail-tunhelper.tmp");
    std::fs::copy(bundled, &tmp)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&tmp, &staged)?; // atomic; a running helper keeps the old inode
    Ok(staged)
}

/// The stop-file both sides agree on. In the shared temp dir so the (user) GUI
/// can create it and the (root) helper can read it; per-uid so two users on one
/// box don't collide.
fn stop_file_path() -> PathBuf {
    #[cfg(unix)]
    // SAFETY: getuid only reads the caller's uid and cannot fail.
    let uid = unsafe { libc::getuid() };
    #[cfg(not(unix))]
    let uid: u32 = 0;
    std::env::temp_dir().join(format!("dreamnail-tun-{uid}.stop"))
}

/// The ready-file the helper touches the moment it's running. Same shared temp
/// dir + per-uid scheme as the stop-file; the GUI computes the path and passes it
/// in, and the (root) helper writes it back — so both sides agree even though
/// they run as different users.
fn ready_file_path() -> PathBuf {
    #[cfg(unix)]
    // SAFETY: getuid only reads the caller's uid and cannot fail.
    let uid = unsafe { libc::getuid() };
    #[cfg(not(unix))]
    let uid: u32 = 0;
    std::env::temp_dir().join(format!("dreamnail-tun-{uid}.ready"))
}

/// Where the helper writes its log. On macOS the elevation shell redirects the
/// helper's stderr here; on Windows the helper itself opens this path (the elevated
/// process has no inherited stderr) — it's under the GUI's temp so the (admin)
/// helper can write it and the (user) GUI can read it back for `helper_log_hint`.
/// A fixed name (not per-uid) matches the macOS shell redirect target.
fn log_file_path() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::temp_dir().join("dreamnail-tunhelper.log")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/tmp/dreamnail-tunhelper.log")
    }
}

/// CLI flags handed to the helper (see `dreamnail-tunhelper`).
fn helper_args(stop_file: &std::path::Path, ready_file: &std::path::Path) -> Vec<String> {
    let mtu = tun_mtu();
    vec![
        "--proxy".into(),
        format!("{SOCKS_HOST}:{SOCKS_PORT}"),
        "--bypass".into(),
        EXPECTED_EXIT_IP.to_string(),
        "--mtu".into(),
        mtu.to_string(),
        "--mss".into(),
        tcp_mss(mtu).to_string(),
        "--udpgw".into(),
        format!("{UDPGW_LOCAL_HOST}:{UDPGW_LOCAL_PORT}"),
        "--parent-pid".into(),
        std::process::id().to_string(),
        "--stop-file".into(),
        stop_file.to_string_lossy().into_owned(),
        "--ready-file".into(),
        ready_file.to_string_lossy().into_owned(),
        "--log-file".into(),
        log_file_path().to_string_lossy().into_owned(),
    ]
}

/// Last few lines of the helper's log, appended to an `enable()` error so the app
/// can SHOW why stealth failed (tun2proxy error, DNS, a Gatekeeper kill…) instead
/// of a blind "didn't come up". macOS redirects the helper's stderr to this path;
/// Windows has the helper write it directly (see `log_file_path`); absent/empty
/// (e.g. Linux under pkexec) → nothing is appended.
fn helper_log_hint() -> String {
    match std::fs::read_to_string(log_file_path()) {
        Ok(s) if !s.trim().is_empty() => {
            let mut tail: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
            let tail = tail.split_off(tail.len().saturating_sub(3));
            format!(" — helper said: {}", tail.join(" | "))
        }
        _ => String::new(),
    }
}

/// Launch the helper elevated. If the user enabled "remember admin", a persistent
/// passwordless launcher (polkit rule / sudoers / Scheduled Task) is used and no
/// prompt appears; otherwise we fall back to the normal one-prompt-per-launch
/// path below. A reconnect never reaches here anyway (enable() is idempotent), so
/// the prompt path is at most once per app launch even without "remember admin".
fn spawn_elevated(helper: &std::path::Path, args: &[String]) -> Result<Child> {
    if let Some(res) = crate::elevation::spawn_passwordless(helper, args) {
        return res;
    }
    spawn_elevated_prompt(helper, args)
}

/// Launch the helper via the platform's elevation PROMPT. The launcher returns
/// once the helper is up (macOS/Windows) or keeps running for the helper's
/// lifetime (Linux/pkexec) — either way `enable()` polls for routing and reaps it.
#[cfg(target_os = "linux")]
fn spawn_elevated_prompt(helper: &std::path::Path, args: &[String]) -> Result<Child> {
    // pkexec shows the polkit password dialog (via the session's auth agent) and
    // runs the headless helper as root. It inherits our session env, so the
    // prompt appears; the helper has no GUI, so running it as root is safe.
    //
    // Stage the helper to a root-readable path first: from an AppImage the
    // bundled helper sits inside a user-only FUSE mount that root can't exec, so
    // without this pkexec fails and stealth falls back to proxy (silent on
    // AppImage installs). Harmless on a .deb install. Falls back to the bundled
    // path if staging fails.
    let staged = stage_helper_for_root(helper).unwrap_or_else(|_| helper.to_path_buf());
    Command::new("pkexec")
        .arg(&staged)
        .args(args)
        .spawn()
        .map_err(|e| EngineError::Proxy(format!("could not start pkexec: {e}")))
}

#[cfg(target_os = "macos")]
fn spawn_elevated_prompt(helper: &std::path::Path, args: &[String]) -> Result<Child> {
    // Run the helper as root via the native auth dialog, backgrounded so
    // osascript returns as soon as it is up. Everything is single-quoted for the
    // shell inside the AppleScript string.
    let mut cmd = sh_single_quote(&helper.to_string_lossy());
    for a in args {
        cmd.push(' ');
        cmd.push_str(&sh_single_quote(a));
    }
    // Two macOS gotchas handled in the one privileged shell:
    //
    // 1. GATEKEEPER QUARANTINE. A helper unzipped from a downloaded .dmg carries
    //    com.apple.quarantine; right-click→Open clears it only for the app's MAIN
    //    binary, not this sibling, so Gatekeeper silently kills the helper the
    //    instant we exec it → the TUN never comes up. We already strip the bundle
    //    at startup as the user, but if the app was copied to a root-owned
    //    location that fails — so strip the helper again here, as root, right
    //    before launch. (`xattr` is a system binary, never itself quarantined.)
    //
    // 2. DETACH. `do shell script … with administrator privileges` runs under a
    //    short-lived privileged trampoline that SIGHUPs anything left in its
    //    session when it exits — which was killing the backgrounded helper the
    //    moment osascript returned. The obvious `nohup` does NOT work here: that
    //    privileged context has no controlling terminal, so macOS `nohup` aborts
    //    with "can't detach from console: Inappropriate ioctl for device" and the
    //    helper never execs (the tun2proxy log showed exactly this). Instead run
    //    the helper in a subshell that IGNORES SIGHUP (`trap '' HUP`); the
    //    backgrounded child inherits that disposition, so the trampoline's hangup
    //    can't reach it. (The helper ALSO ignores HUP + `setsid`s itself at
    //    startup as a belt-and-suspenders — macOS ships no `setsid(1)`.) Its own
    //    watchdog still stops it on the stop-file or when the GUI dies.
    let helper_q = sh_single_quote(&helper.to_string_lossy());
    let script = format!(
        "do shell script \"xattr -d com.apple.quarantine {helper_q} 2>/dev/null; \
         ( trap '' HUP; {cmd} </dev/null >/tmp/dreamnail-tunhelper.log 2>&1 & )\" \
         with administrator privileges"
    );
    Command::new("osascript")
        .args(["-e", &script])
        .spawn()
        .map_err(|e| EngineError::Proxy(format!("could not start osascript: {e}")))
}

#[cfg(target_os = "windows")]
fn spawn_elevated_prompt(helper: &std::path::Path, args: &[String]) -> Result<Child> {
    // The helper elevates ITSELF: `helper --runas-launch <args-file>` (a hidden,
    // non-elevated launch) calls ShellExecuteExW("runas") owned by the foreground
    // window, so the UAC prompt comes up IN FRONT. The old hidden-PowerShell
    // Start-Process path often got the prompt parked as a flashing taskbar icon
    // and connect looked frozen for up to AUTH_TIMEOUT. Args travel via a file
    // (one per line), which also sidesteps all command-line quoting issues with
    // spaces in the profile path. Launcher exit: 0 started, 1 declined, 2 error.
    let args_file = std::env::temp_dir().join("dreamnail-tun-prompt.args");
    std::fs::write(&args_file, args.join("\n"))
        .map_err(|e| EngineError::Proxy(format!("could not stage helper args: {e}")))?;
    let mut c = Command::new(helper);
    c.arg("--runas-launch").arg(&args_file);
    crate::hide_console(&mut c);
    c.spawn()
        .map_err(|e| EngineError::Proxy(format!("could not start the stealth helper: {e}")))
}

/// Quote one argument the way `CommandLineToArgvW` (and Rust's `std::env::args`)
/// parse it back, so a value with spaces/quotes survives the round-trip through
/// Start-Process into the helper. Only quotes when needed. Mirrors the canonical
/// MSVC argv-quoting algorithm. Not `#[cfg(windows)]` so its logic can be unit
/// tested on any host (it's the correctness-critical part of the Windows path).
#[allow(dead_code)] // kept (tested) for any future argv-style Windows launch
fn win_quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{0B}', '"']) {
        return arg.to_string();
    }
    let mut s = String::from('"');
    let chars: Vec<char> = arg.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let mut backslashes = 0;
        while i < chars.len() && chars[i] == '\\' {
            backslashes += 1;
            i += 1;
        }
        if i == chars.len() {
            // Trailing backslashes precede the closing quote → double them.
            for _ in 0..backslashes * 2 {
                s.push('\\');
            }
        } else if chars[i] == '"' {
            // Double the backslashes, then escape the embedded quote.
            for _ in 0..backslashes * 2 + 1 {
                s.push('\\');
            }
            s.push('"');
            i += 1;
        } else {
            for _ in 0..backslashes {
                s.push('\\');
            }
            s.push(chars[i]);
            i += 1;
        }
    }
    s.push('"');
    s
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn spawn_elevated_prompt(_helper: &std::path::Path, _args: &[String]) -> Result<Child> {
    Err(EngineError::Proxy("stealth unsupported on this OS".into()))
}

/// Wrap `s` in single quotes for a POSIX shell, escaping embedded single quotes.
#[cfg(target_os = "macos")]
fn sh_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Clear a leftover read-only `/etc/resolv.conf` bind mount from a previously
/// failed tun2proxy setup. Only acts when resolv.conf is currently NOT writable
/// (the exact broken state); a healthy resolv.conf is left alone.
#[cfg(target_os = "linux")]
fn heal_stale_resolv_conf() {
    use std::fs::OpenOptions;
    const RC: &str = "/etc/resolv.conf";
    if OpenOptions::new().write(true).open(RC).is_ok() {
        return;
    }
    // Best-effort: unmount an overlay/bind that tun2proxy left behind. Needs
    // root; a no-op (and harmless) when we're the unprivileged GUI.
    let _ = Command::new("umount").arg(RC).status();
}
#[cfg(not(target_os = "linux"))]
fn heal_stale_resolv_conf() {}

/// True when a DIRECT (un-proxied) request egresses via the server — i.e. the TUN
/// is up and carrying our traffic.
///
/// DNS-FREE ON PURPOSE. We hit an IP LITERAL (Cloudflare's 1.1.1.1 trace), never a
/// hostname: a freshly-raised TUN's DNS redirection is exactly what's flaky on
/// macOS (it ignores the /etc/resolv.conf that tun2proxy rewrites), so a hostname
/// probe would report "down" and trigger a fallback even while packets are really
/// flowing. Plain `http` so a skewed clock / missing cert store can't fail it
/// either. The trace's single `ip=<addr>` line is our egress IP as the internet
/// sees it — it equals the server only when traffic is going through the tunnel.
fn routed_via_server() -> bool {
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(4))
        .no_proxy()
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let body = match client
        .get("http://1.1.1.1/cdn-cgi/trace")
        .send()
        .and_then(|r| r.text())
    {
        Ok(b) => b,
        Err(_) => return false,
    };
    body.lines()
        .find_map(|l| l.strip_prefix("ip="))
        .map(str::trim)
        == Some(EXPECTED_EXIT_IP)
}

#[cfg(test)]
mod tests {
    use super::{helper_args, ready_file_path, stop_file_path};

    #[test]
    fn helper_args_carry_proxy_bypass_and_stop_file() {
        let stop = stop_file_path();
        let ready = ready_file_path();
        let a = helper_args(&stop, &ready);
        // proxy + bypass + stop-file must all be present so the elevated helper
        // forwards to the right SOCKS and can be told to stop.
        assert!(a.iter().any(|x| x == "--proxy"));
        assert!(a.iter().any(|x| x == "--bypass"));
        assert!(a.iter().any(|x| x == "66.226.144.136"));
        assert!(a.iter().any(|x| x.contains("dreamnail-tun-")));
    }

    #[test]
    fn win_quote_leaves_simple_args_untouched() {
        use super::win_quote;
        assert_eq!(win_quote("--proxy"), "--proxy");
        assert_eq!(win_quote("127.0.0.1:11080"), "127.0.0.1:11080");
        assert_eq!(win_quote("1460"), "1460");
    }

    #[test]
    fn win_quote_wraps_paths_with_spaces() {
        use super::win_quote;
        // The exact bug: a stop-file under a profile dir with a space MUST come
        // back as a SINGLE arg, so it has to be quoted.
        let p = r"C:\Users\First Last\AppData\Local\Temp\dreamnail-tun-0.stop";
        assert_eq!(
            win_quote(p),
            r#""C:\Users\First Last\AppData\Local\Temp\dreamnail-tun-0.stop""#
        );
        // Trailing backslash before the closing quote must be doubled.
        assert_eq!(win_quote(r"C:\dir with space\"), r#""C:\dir with space\\""#);
    }

    #[test]
    fn win_quote_escapes_embedded_quotes() {
        use super::win_quote;
        assert_eq!(win_quote(r#"a "b" c"#), r#""a \"b\" c""#);
    }
    /// A stealth session whose helper exits mid-session must read as DEAD (so the
    /// watchdog reconnects instead of leaking traffic under a green "connected"),
    /// and must be forgotten so the reconnect can re-launch it.
    #[cfg(unix)]
    #[test]
    fn alive_notices_the_helper_exiting() {
        use super::{Handle, TunProxy};
        use dreamnail_core::ports::Proxy;
        let dir = std::env::temp_dir().join(format!("dn-tun-alive-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ready_file = dir.join("ready");
        std::fs::write(&ready_file, "1").unwrap();
        let launcher = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let mut p = TunProxy {
            inner: Some(Handle {
                stop_file: dir.join("stop"),
                ready_file: ready_file.clone(),
                launcher,
            }),
        };
        assert!(p.alive(), "helper running + ready-file present = alive");
        std::fs::remove_file(&ready_file).unwrap(); // helper's exit path deletes it
        assert!(!p.alive(), "ready-file gone = helper exited");
        assert!(
            p.inner.is_none(),
            "dead session must be dropped so enable() relaunches"
        );
        assert!(p.alive(), "no session = nothing to lose");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
