//! `XrayProcess` — spawns the bundled Xray (VLESS) client, the local end of the
//! data path. We deliberately do NOT reimplement VLESS; we run Xray-core exactly
//! as the Python backend did, generating the same config and pointing the VLESS
//! outbound at the SSH tunnel's local endpoint (127.0.0.1:10809).
//!
//! Also home to the two pure pieces the config needs:
//!   * [`uuid_for_username`] — the per-customer VLESS id, derived from the SSH
//!     username (uuid5). MUST match config.uuid_for_username byte-for-byte, or a
//!     customer presents a different identity than the server registered and the
//!     tunnel is rejected. Pinned by tests against Python-computed goldens.
//!   * [`build_xray_config`] — the Xray client JSON (SOCKS+HTTP inbounds, VLESS
//!     outbound, mux), mirroring config.build_xray_config.

use std::io::Write;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use dreamnail_core::ports::{Credentials, EngineError, Result, Xray};
use uuid::Uuid;

use crate::netcfg;

/// The customer's VLESS UUID, derived from their (normalised) SSH username.
/// Empty username → empty string (the Python side falls back to a baked UUID;
/// v4 has none baked, so an empty username is simply invalid upstream).
#[must_use]
pub fn uuid_for_username(username: &str) -> String {
    let name = username.trim().to_lowercase();
    if name.is_empty() {
        return String::new();
    }
    let ns = Uuid::parse_str(netcfg::UUID_NAMESPACE).expect("namespace is a valid UUID");
    Uuid::new_v5(&ns, name.as_bytes()).to_string()
}

/// Ad/tracker domain list (v2fly geosite `category-ads-all`, ~169k entries,
/// already prefixed `domain:` for Xray), gzip-compressed into the binary.
const ADS_GZ: &[u8] = include_bytes!("../assets/ads.txt.gz");

/// Decompressed ad-domain rules. Empty on any decode error (blocking then simply
/// doesn't happen — it can never break connecting).
fn ad_domains() -> Vec<String> {
    use std::io::Read;
    let mut text = String::new();
    if flate2::read::GzDecoder::new(ADS_GZ).read_to_string(&mut text).is_err() {
        return Vec::new();
    }
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Mux for the DEFAULT (browsing) outbound — the automatic mux switch. ON by
/// default: ordinary pages fire dozens of small requests, and muxing reuses one
/// tunnel connection so they skip the ~200 ms per-request setup (the fix for
/// "pages load slow but the speed test is fast"). Bulk transfers DON'T use this
/// outbound — they're routed to the non-muxed `vless-bulk` outbound (see
/// `BULK_DOMAINS`) and so still stripe at full throughput — which is why muxing
/// browsing onto a single SSH stripe costs nothing. `DREAMNAIL_MUX=0|1` overrides
/// (0 = old behaviour: browsing also non-muxed).
fn browsing_mux() -> bool {
    std::env::var("DREAMNAIL_MUX")
        .map(|v| v != "0")
        .unwrap_or(netcfg::MUX_ENABLED)
}

/// The app's data directory, resolved the SAME way dreamnail-ffi resolves it
/// (`DREAMNAIL_DATA_DIR`, else `%APPDATA%\Dreamnail` on Windows, else
/// `~/.dreamnail`) so we read the SAME `adblock.pref` the UI writes.
pub(crate) fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("DREAMNAIL_DATA_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    #[cfg(windows)]
    {
        PathBuf::from(std::env::var("APPDATA").unwrap_or_else(|_| ".".into())).join("Dreamnail")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".dreamnail")
    }
}

/// Whether the user turned the ad blocker on. Read at Xray start from a plain
/// sibling pref the UI writes (next to `routing.pref`), so it persists while the
/// encrypted store is locked. `DREAMNAIL_ADBLOCK=1|0` overrides (tests).
pub fn adblock_pref() -> bool {
    if let Ok(v) = std::env::var("DREAMNAIL_ADBLOCK") {
        return v == "1";
    }
    std::fs::read_to_string(data_dir().join("adblock.pref"))
        .map(|s| s.trim() == "on")
        .unwrap_or(false)
}

/// The Xray client config as JSON, mirroring config.build_xray_config.
///
/// `adblock` adds a blackhole outbound + one routing rule sending every
/// ad/tracker domain to it. Off = the config is byte-for-byte the original.
/// Works in both modes: proxy clients and the stealth TUN (Fake-IP) both hand
/// Xray the hostname, so domain rules match without sniffing.
#[must_use]
pub fn build_xray_config(vless_uuid: &str, adblock: bool) -> serde_json::Value {
    // The primary outbound. Normally VLESS to the local SSH forward
    // (127.0.0.1:10809). In Reality mode (SSH blocked → the "looks like a real
    // site" door) it's VLESS+Reality straight to the server:443, so there is no
    // SSH layer and Xray itself does the disguised TLS.
    let reality = crate::tunnel::reality_mode();
    let primary_out = if reality {
        // The decoy site the user picked (defaults to the first in the list).
        let decoy_sni = crate::tunnel::reality_decoy_sni();
        serde_json::json!({
            "tag": "vless-out",
            "protocol": "vless",
            "settings": {
                "vnext": [
                    {
                        "address": netcfg::EXPECTED_EXIT_IP,
                        "port": netcfg::REALITY_PORT,
                        "users": [ { "id": vless_uuid, "encryption": "none" } ]
                    }
                ]
            },
            "streamSettings": {
                "network": "tcp",
                "security": "reality",
                "sockopt": { "mark": 255 },
                "realitySettings": {
                    "serverName": decoy_sni,
                    "publicKey": netcfg::REALITY_PUBKEY,
                    "shortId": netcfg::REALITY_SHORTID,
                    "fingerprint": netcfg::REALITY_FINGERPRINT
                }
            }
        })
    } else {
        serde_json::json!({
            "tag": "vless-out",
            "protocol": "vless",
            "settings": {
                "vnext": [
                    {
                        "address": netcfg::TUNNEL_LOCAL_HOST,
                        "port": netcfg::TUNNEL_LOCAL_PORT,
                        "users": [ { "id": vless_uuid, "encryption": "none" } ]
                    }
                ]
            },
            "streamSettings": {
                "network": "tcp",
                "sockopt": { "tcpFastOpen": true, "mark": 255 }
            },
            "mux": {
                "enabled": browsing_mux(),
                "concurrency": 8,
                "xudpConcurrency": 8,
                "xudpProxyUDP443": "reject"
            }
        })
    };
    let mut cfg = serde_json::json!({
        "log": { "loglevel": "warning" },
        "inbounds": [
            {
                "tag": "socks-in",
                "listen": netcfg::SOCKS_HOST,
                "port": netcfg::SOCKS_PORT,
                "protocol": "socks",
                "settings": { "auth": "noauth", "udp": true },
                "sniffing": { "enabled": false }
            },
            {
                "tag": "http-in",
                "listen": netcfg::HTTP_HOST,
                "port": netcfg::HTTP_PORT,
                "protocol": "http",
                "settings": {},
                "sniffing": { "enabled": false }
            }
        ],
        "outbounds": [ primary_out ]
    });

    // Routing rules, in priority order. Ad-blocking wins over everything; the
    // bulk rule (the automatic mux switch) comes next; anything unmatched falls
    // through to outbounds[0] (the muxed browsing outbound).
    let mut rules: Vec<serde_json::Value> = Vec::new();

    if adblock {
        let domains = ad_domains();
        if !domains.is_empty() {
            if let Some(obs) = cfg["outbounds"].as_array_mut() {
                obs.push(serde_json::json!({ "tag": "adblock", "protocol": "blackhole" }));
            }
            rules.push(serde_json::json!(
                { "type": "field", "domain": domains, "outboundTag": "adblock" }
            ));
        }
    }

    // Automatic mux switch: a second, NON-muxed VLESS outbound for bulk
    // destinations (speed tests + big download/stream CDNs). Each of their
    // connections then spreads across the SSH stripes for full throughput, while
    // browsing stays on the muxed outbound for snappy page loads. Reality mode
    // has no SSH stripes to spread across (it's a direct TLS door), so there's
    // nothing to switch and we keep its single outbound.
    if !reality {
        if let Some(obs) = cfg["outbounds"].as_array_mut() {
            obs.push(serde_json::json!({
                "tag": "vless-bulk",
                "protocol": "vless",
                "settings": {
                    "vnext": [
                        {
                            "address": netcfg::TUNNEL_LOCAL_HOST,
                            "port": netcfg::TUNNEL_LOCAL_PORT,
                            "users": [ { "id": vless_uuid, "encryption": "none" } ]
                        }
                    ]
                },
                "streamSettings": {
                    "network": "tcp",
                    "sockopt": { "tcpFastOpen": true, "mark": 255 }
                },
                "mux": { "enabled": false }
            }));
        }
        rules.push(serde_json::json!(
            { "type": "field", "domain": netcfg::BULK_DOMAINS, "outboundTag": "vless-bulk" }
        ));
    }

    if !rules.is_empty() {
        cfg["routing"] = serde_json::json!({ "domainStrategy": "AsIs", "rules": rules });
    }
    cfg
}

/// Locate the Xray binary. Order: `$DREAMNAIL_XRAY`, then a bundled binary next
/// to the running exe, then `xray` on PATH. (Phase A / CLI: the env var is the
/// easy way to point at a real Xray for live testing; the bundled path is what
/// the shipped app will use.)
fn locate_xray() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("DREAMNAIL_XRAY") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Ok(pb);
        }
        return Err(EngineError::Xray(format!(
            "DREAMNAIL_XRAY points at a missing file: {}",
            pb.display()
        )));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let name = if cfg!(windows) { "xray.exe" } else { "xray" };
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    // Fall back to PATH (Command resolves a bare name against PATH).
    Ok(PathBuf::from(if cfg!(windows) { "xray.exe" } else { "xray" }))
}

pub struct XrayProcess {
    ready_timeout: Duration,
    child: Option<Child>,
    cfg_path: Option<PathBuf>,
}

impl Default for XrayProcess {
    fn default() -> Self {
        Self {
            ready_timeout: Duration::from_secs(10),
            child: None,
            cfg_path: None,
        }
    }
}

impl XrayProcess {
    #[must_use]
    pub fn new(ready_timeout: Duration) -> Self {
        Self {
            ready_timeout,
            child: None,
            cfg_path: None,
        }
    }

    /// Poll the SOCKS inbound until it accepts a connection or we time out —
    /// Xray is "ready" once it's listening, which is what the proxy will use.
    fn wait_socks_ready(&self) -> bool {
        let addr = match (netcfg::SOCKS_HOST, netcfg::SOCKS_PORT).to_socket_addrs() {
            Ok(mut a) => match a.next() {
                Some(a) => a,
                None => return false,
            },
            Err(_) => return false,
        };
        let deadline = Instant::now() + self.ready_timeout;
        while Instant::now() < deadline {
            // The user pressed Disconnect/Cancel — stop waiting so the queued
            // request isn't stuck behind the rest of this timeout.
            if crate::cancel::abort_requested() {
                return false;
            }
            // Short timeout on purpose: a loopback connect to a LISTENING port
            // completes in well under a millisecond, but on Windows a connect to
            // a not-yet-listening one isn't refused at once — the stack retries
            // the SYN for ~1-2 s. With a 1 s timeout every early probe cost a
            // full second there (the slow Windows connect button).
            if TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }
}

impl Xray for XrayProcess {
    fn start(&mut self, creds: &Credentials) -> Result<()> {
        // A child HANDLE isn't the same as a running process: Xray can exit on its
        // own (crash, port stolen, bad config) and the handle stays Some forever.
        // Reap it and respawn rather than reporting a false success.
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(None) => return Ok(()), // genuinely still running
                _ => self.stop(),          // exited (or un-waitable) → clean slate
            }
        }
        let vless_uuid = uuid_for_username(&creds.username);
        if vless_uuid.is_empty() {
            return Err(EngineError::Xray("empty username → no VLESS id".into()));
        }
        let binary = locate_xray()?;
        ensure_executable(&binary);
        let config = build_xray_config(&vless_uuid, adblock_pref());

        // Write the config to a temp file Xray reads with `run -c`.
        let mut path = std::env::temp_dir();
        path.push(format!("dn-v4-{}.json", std::process::id()));
        {
            let mut f = std::fs::File::create(&path)
                .map_err(|e| EngineError::Xray(format!("write config: {e}")))?;
            let bytes = serde_json::to_vec(&config)
                .map_err(|e| EngineError::Xray(format!("serialize config: {e}")))?;
            f.write_all(&bytes)
                .map_err(|e| EngineError::Xray(format!("write config: {e}")))?;
        }

        // An Xray left behind by an earlier app instance that crashed or was
        // killed still listens on the SOCKS port; with SO_REUSEPORT the new one
        // binds too, the readiness probe passes, and connections land on the
        // stale process — "connection to server dropped". Clear them first.
        reap_orphaned_xrays();

        let mut cmd = Command::new(&binary);
        die_with_parent(&mut cmd);
        cmd.arg("run")
            .arg("-c")
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        no_console_window(&mut cmd);

        let child = cmd.spawn().map_err(|e| {
            let _ = std::fs::remove_file(&path);
            EngineError::Xray(format!("spawn {}: {e}", binary.display()))
        })?;

        #[cfg(windows)]
        kill_with_app(&child);

        self.child = Some(child);
        self.cfg_path = Some(path);

        if !self.wait_socks_ready() {
            self.stop();
            return Err(EngineError::Xray(
                "local service did not become ready (SOCKS never opened)".into(),
            ));
        }
        Ok(())
    }

    fn alive(&mut self) -> bool {
        match self.child.as_mut() {
            // `Ok(None)` is the only "still running" answer; an exit status (or an
            // un-waitable handle) means the local service is gone and the link is
            // dead even though the SSH tunnel above it is perfectly healthy.
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => false,
        }
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(path) = self.cfg_path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Drop for XrayProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

/// On Windows, keep the child from flashing a console window — the frozen app is
/// windowed, and a console child is the conspicuous flash the v2 engine fixed.
#[cfg(windows)]
fn no_console_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_console_window(_cmd: &mut Command) {}

#[cfg(windows)]
/// Windows: tie Xray's lifetime to the app's. Every Xray we spawn joins one
/// process-wide Job Object created with KILL_ON_JOB_CLOSE; the app holds the
/// only handle, so when the app exits for ANY reason (crash, Task Manager kill,
/// logoff) Windows closes the handle and terminates Xray with it. This is the
/// Windows counterpart of Linux's PR_SET_PDEATHSIG — without it a crashed app
/// left xray.exe running, still holding the local proxy ports, and the next
/// connect misbehaved until a reboot. Best-effort: any failure just means the
/// old behaviour.
fn kill_with_app(child: &std::process::Child) {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    // Stored as usize so the static is Send + Sync; it is a HANDLE we never close.
    static JOB: OnceLock<usize> = OnceLock::new();
    let job = *JOB.get_or_init(|| unsafe {
        let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if h.is_null() {
            return 0;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            h,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        h as usize
    });
    if job != 0 {
        // SAFETY: both handles are valid for the duration of the call.
        unsafe {
            AssignProcessToJobObject(job as _, child.as_raw_handle() as _);
        }
    }
}


/// Linux: have the kernel SIGTERM Xray if the app dies, so a crash or kill
/// can't orphan it. (The spawning thread is the long-lived engine actor.)
#[cfg(target_os = "linux")]
fn die_with_parent(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: prctl is async-signal-safe and only sets this child's own
    // parent-death signal between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
}
#[cfg(not(target_os = "linux"))]
fn die_with_parent(_cmd: &mut Command) {}

/// Kill Xray processes started by app instances that no longer exist. Each one
/// runs `xray run -c <tmp>/dn-v4-<app pid>.json`, so the owner is in its args.
#[cfg(target_os = "linux")]
fn reap_orphaned_xrays() {
    let me = std::process::id();
    let Ok(dir) = std::fs::read_dir("/proc") else { return };
    for ent in dir.flatten() {
        let Some(pid) = ent.file_name().to_str().and_then(|n| n.parse::<i32>().ok()) else {
            continue;
        };
        let Ok(raw) = std::fs::read(ent.path().join("cmdline")) else { continue };
        let args: Vec<String> = raw
            .split(|b| *b == 0)
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        if let Some(owner) = orphan_owner(&args) {
            // SAFETY: kill(owner, 0) only probes existence; SIGTERM targets a
            // process we verified is one of our own Xray instances.
            let owner_alive = owner == me || unsafe { libc::kill(owner as i32, 0) } == 0;
            if !owner_alive {
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
                if let Some(cfg) = args.get(3) {
                    let _ = std::fs::remove_file(cfg);
                }
            }
        }
    }
}
#[cfg(not(target_os = "linux"))]
fn reap_orphaned_xrays() {}

/// For `xray run -c …/dn-v4-<pid>.json`, the owning app pid.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn orphan_owner(args: &[String]) -> Option<u32> {
    if !args.first()?.ends_with("xray") || args.get(1)? != "run" || args.get(2)? != "-c" {
        return None;
    }
    let name = std::path::Path::new(args.get(3)?).file_name()?.to_str()?;
    name.strip_prefix("dn-v4-")?.strip_suffix(".json")?.parse().ok()
}

/// Best-effort: make sure the bundled Xray binary is executable before we spawn
/// it. A binary extracted from a downloaded .dmg/.zip can lose its mode bits, and
/// Xray then fails to launch with a bare "permission denied" — which the readiness
/// wait later reports as a misleading "SOCKS never opened". Only touches a file
/// that isn't already executable, so a bare PATH name (no such path) is a no-op.
/// (macOS Gatekeeper quarantine is stripped bundle-wide at startup by the FFI
/// layer's `clear_own_quarantine`.) No-op on Windows.
#[cfg(unix)]
fn ensure_executable(p: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(p) {
        let mode = meta.permissions().mode();
        if mode & 0o111 == 0 {
            let mut perms = meta.permissions();
            perms.set_mode(mode | 0o755);
            let _ = std::fs::set_permissions(p, perms);
        }
    }
}
#[cfg(not(unix))]
fn ensure_executable(_p: &std::path::Path) {}

#[cfg(test)]
mod tests {

    #[test]
    fn orphan_owner_parses_only_our_xray() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            super::orphan_owner(&v(&["/x/dreamnail-v4/xray", "run", "-c", "/tmp/dn-v4-30594.json", ""])),
            Some(30594)
        );
        assert_eq!(super::orphan_owner(&v(&["/usr/local/bin/xray", "run", "-config", "/etc/x.json"])), None);
        assert_eq!(super::orphan_owner(&v(&["/bin/cat", "run", "-c", "/tmp/dn-v4-1.json"])), None);
    }

    use super::*;

    // Goldens computed from the Python config.uuid_for_username — these MUST
    // match or customers can't connect. (namespace injected at build time)
    #[test]
    fn uuid_matches_python_goldens() {
        assert_eq!(uuid_for_username("ad"), "e81973f3-db23-5d8b-9381-5b2cb5090f31");
        assert_eq!(
            uuid_for_username("portkey-bob"),
            "555f2965-4b91-5262-a249-4a7a689236df"
        );
        assert_eq!(
            uuid_for_username("pixelkey-bob"),
            "6ad23823-1b28-5ccd-9bb0-e65753cdd73f"
        );
        assert_eq!(
            uuid_for_username("linuxkey-bob"),
            "487134c2-6c22-587e-8850-3592bf8433be"
        );
    }

    #[test]
    fn uuid_normalises_case_and_whitespace() {
        // Python does .strip().lower(); "  Ad  " and "E2ETest1" prove it.
        assert_eq!(uuid_for_username("  Ad  "), uuid_for_username("ad"));
        assert_eq!(
            uuid_for_username("E2ETest1"),
            "27bd01ed-4a5a-5d0f-a3c1-48ab37b77f02"
        );
    }

    #[test]
    fn empty_username_is_empty() {
        assert_eq!(uuid_for_username("   "), "");
    }

    #[test]
    fn alive_is_false_before_start() {
        let mut x = XrayProcess::default();
        assert!(!x.alive(), "nothing spawned yet → not alive");
    }

    #[test]
    fn a_dead_child_is_reaped_so_start_can_respawn() {
        // Stand in for Xray with a process that exits immediately: after it dies,
        // `alive()` must report false and `start()` must NOT short-circuit on the
        // stale handle (the bug that hid a crashed Xray behind a green UI).
        let mut x = XrayProcess::default();
        x.child = Some(
            Command::new("sh")
                .arg("-c")
                .arg("exit 0")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn test child"),
        );
        // Give it a moment to actually exit.
        for _ in 0..50 {
            if !x.alive() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!x.alive(), "an exited child must not report alive");
        x.stop();
        assert!(x.child.is_none());
    }

    #[test]
    fn adblock_adds_blackhole_rule_and_off_is_unchanged() {
        let uuid = uuid_for_username("ad");
        // Ad-block OFF: no blackhole outbound, and the first routing rule (if any)
        // is NOT an ad rule — it's the bulk-switch rule, checked separately below.
        let off = build_xray_config(&uuid, false);
        let off_obs = off["outbounds"].as_array().unwrap();
        assert!(
            off_obs.iter().all(|o| o["protocol"] != serde_json::json!("blackhole")),
            "ad-block off must not add a blackhole outbound"
        );
        // Ad-block ON: adds the blackhole outbound (right after the default) and
        // an ad rule as the FIRST (highest-priority) routing rule.
        let on = build_xray_config(&uuid, true);
        let obs = on["outbounds"].as_array().unwrap();
        assert_eq!(obs[0]["tag"], serde_json::json!("vless-out")); // default stays first
        assert_eq!(obs[1]["protocol"], serde_json::json!("blackhole"));
        let doms = on["routing"]["rules"][0]["domain"].as_array().unwrap();
        assert!(doms.len() > 100_000, "ad list too small: {}", doms.len());
        assert!(doms.contains(&serde_json::json!("domain:doubleclick.net")));
        assert_eq!(on["routing"]["rules"][0]["outboundTag"], serde_json::json!("adblock"));
    }

    #[test]
    fn auto_mux_switch_browsing_muxed_bulk_not() {
        // Only meaningful on the SSH path (Reality has no stripes to switch to).
        if crate::tunnel::reality_mode() {
            return;
        }
        let uuid = uuid_for_username("ad");
        let c = build_xray_config(&uuid, false);
        let obs = c["outbounds"].as_array().unwrap();
        // Default (browsing) outbound: muxed per browsing_mux() (ON by default).
        assert_eq!(obs[0]["tag"], serde_json::json!("vless-out"));
        assert_eq!(obs[0]["mux"]["enabled"], serde_json::json!(browsing_mux()));
        // A second, NON-muxed outbound exists for bulk transfers.
        let bulk = obs
            .iter()
            .find(|o| o["tag"] == serde_json::json!("vless-bulk"))
            .expect("bulk outbound must exist on the SSH path");
        assert_eq!(bulk["mux"]["enabled"], serde_json::json!(false));
        // It dials the same tunnel local end with the same id.
        assert_eq!(bulk["settings"]["vnext"][0]["port"], serde_json::json!(netcfg::TUNNEL_LOCAL_PORT));
        assert_eq!(bulk["settings"]["vnext"][0]["users"][0]["id"], serde_json::json!(uuid));
        // A routing rule sends bulk destinations (e.g. speed tests) to it.
        let bulk_rule = c["routing"]["rules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["outboundTag"] == serde_json::json!("vless-bulk"))
            .expect("a rule must route bulk domains to vless-bulk");
        let bdoms = bulk_rule["domain"].as_array().unwrap();
        assert!(bdoms.contains(&serde_json::json!("domain:speedtest.net")));
    }

    #[test]
    fn xray_config_shape_and_uuid() {
        let uuid = uuid_for_username("ad");
        let c = build_xray_config(&uuid, false);
        // The derived id reaches the VLESS outbound.
        assert_eq!(
            c["outbounds"][0]["settings"]["vnext"][0]["users"][0]["id"],
            serde_json::json!(uuid)
        );
        // Outbound dials the tunnel's local end.
        assert_eq!(
            c["outbounds"][0]["settings"]["vnext"][0]["port"],
            serde_json::json!(netcfg::TUNNEL_LOCAL_PORT)
        );
        // Inbounds on the expected ports.
        assert_eq!(c["inbounds"][0]["port"], serde_json::json!(netcfg::SOCKS_PORT));
        assert_eq!(c["inbounds"][1]["port"], serde_json::json!(netcfg::HTTP_PORT));
        // The browsing outbound is muxed by default (the automatic mux switch).
        assert_eq!(
            c["outbounds"][0]["mux"]["enabled"],
            serde_json::json!(browsing_mux())
        );
        // No DNS section. On the SSH path there's a second (bulk) outbound and a
        // routing section for the mux switch; Reality has neither.
        assert!(c["dns"].is_null());
        if crate::tunnel::reality_mode() {
            assert!(c["routing"].is_null());
            assert_eq!(c["outbounds"].as_array().unwrap().len(), 1);
        } else {
            assert!(c["routing"].is_object());
            assert_eq!(c["outbounds"].as_array().unwrap().len(), 2);
        }
    }
}

