//! Dreamnail v4 FFI — the boundary the Flutter app calls in-process (Phase B).
//!
//! One generic entry point, `dn_request(json) -> json`, backed by a small
//! [`Backend`] (engine + encrypted store) that runs on ONE dedicated actor
//! thread. FFI calls send a `{method, args}` request and get a JSON reply, so
//! the non-Send engine never crosses threads and there are no shared-state
//! locks around it. This replaces the v2 localhost HTTP backend method-for-
//! method, so the Dart `ApiClient` keeps the same surface with no separate
//! process, port, or token.
//!
//! `status`/`proxy_status`/`health` are answered from shared atomics WITHOUT
//! going through the actor, so the UI's 2s status poll stays instant even while a
//! multi-second `connect()` is running on the actor.

use std::collections::VecDeque;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dreamnail_core::ports::EngineError;
use dreamnail_core::{Credentials, Engine, Proxy, State};
use dreamnail_plat::{
    cancel, conflict, diag, elevation, entitlements, netcfg, netid, report, routing_fell_back,
    set_tun_routing, shortcut, traffic, tun_routing_selected,
    updater, EncryptedStore, HttpRouter, ModalProxy, RusshTunnel, SystemProxy, TunnelConfig,
    XrayProcess,
};
use serde_json::{json, Value};

// --- shared, lock-free status (readable without the actor) -----------------
static STATE: AtomicU8 = AtomicU8::new(0); // 0=Disc 1=Connecting 2=Connected 3=Reconnecting
static CONNECTED_AT: AtomicU64 = AtomicU64::new(0); // epoch secs, 0 = not connected
static PING_MS: AtomicU32 = AtomicU32::new(0); // last RTT to the server, 0 = unknown
                                               // Live throughput, sampled from the tunnel's byte counters on each watchdog tick.
static DOWN_BPS: AtomicU64 = AtomicU64::new(0);
static UP_BPS: AtomicU64 = AtomicU64::new(0);
static SESSION_DOWN: AtomicU64 = AtomicU64::new(0);
static SESSION_UP: AtomicU64 = AtomicU64::new(0);
// Watchdog recoveries counted at the start of the current session, so the receipt
// can report drops for THIS session rather than the engine's whole lifetime.
static SESSION_RECOVERIES_BASE: AtomicU64 = AtomicU64::new(0);
// --- persistent auto-reconnect (survives wake / Wi-Fi switch / server blips) -
// The in-engine watchdog recovers a drop in one shot; when THAT gives up (most
// often because the network isn't back yet after a wake), these drive a capped
// backoff retry so the link comes back on its own instead of staying down until
// the user reconnects. AUTO_RETRY_AT is the epoch-secs of the next attempt (0 =
// not retrying); BACKOFF is the current wait; ATTEMPT is surfaced in `status`.
static AUTO_RETRY_AT: AtomicU64 = AtomicU64::new(0);
static AUTO_RETRY_BACKOFF: AtomicU64 = AtomicU64::new(0);
static RECONNECT_ATTEMPT: AtomicU32 = AtomicU32::new(0);
const AUTO_RETRY_FIRST_SECS: u64 = 2;
const AUTO_RETRY_MAX_SECS: u64 = 30;
// --- data cap (operator-set limit + resettable usage this cycle) ------------
// The customer enters their plan's data cap (what the operator set in x-ui) in
// Settings; the client tracks its own measured traffic against it and shows a
// sticky-note + a warning when it's hit. LIMIT 0 = no cap. USED_COMMITTED is the
// bytes from FINISHED sessions this cycle (loaded from the store on unlock); live
// usage adds the current session's bytes on top. Both are total (down + up).
static DATA_LIMIT_BYTES: AtomicU64 = AtomicU64::new(0);
static USED_COMMITTED: AtomicU64 = AtomicU64::new(0);
const BYTES_PER_GB: f64 = 1_073_741_824.0;
// Account status + when the plan was last synced from the server (epoch secs, 0 =
// never). Shown on the "My Plan" panel.
static ACCOUNT_ENABLED: AtomicBool = AtomicBool::new(true);
static PLAN_SYNCED_AT: AtomicU64 = AtomicU64::new(0);
/// A plan fetched in the background after connect, waiting to be persisted to
/// the encrypted store (only the actor thread holds the store, so the fetch
/// thread updates the live atomics itself and parks the plan here).
static PENDING_PLAN: Mutex<Option<entitlements::Entitlements>> = Mutex::new(None);
// A one-line summary of the LAST connection, shown after disconnect and cleared on
// the next connect. None until a session has ended.
static LAST_SESSION: OnceLock<Mutex<Option<Value>>> = OnceLock::new();
static LAST_ERROR: OnceLock<Mutex<Option<String>>> = OnceLock::new();
// Plan expiry (ISO yyyy-mm-dd, "" = none) — synced from the server on connect or
// set manually. Held here so `status` can serve it live (drives the expiry
// banner without the UI re-reading settings). Loaded from the store on unlock.
static PLAN_EXPIRY: OnceLock<Mutex<String>> = OnceLock::new();
// Remote-control (set from /api/me directives). REVOKED: the operator cancelled
// this install — the actor wipes local data + uninstalls on its next tick.
// The update slots drive an "update required" prompt in the UI.
static REVOKED: AtomicBool = AtomicBool::new(false);
// ROLLBACK_REQUESTED: the operator asked this install to revert to the previous
// build. The actor actions it once (guarded by an on-disk marker so it doesn't
// loop every poll); clearing the directive clears the marker, re-arming it.
static ROLLBACK_REQUESTED: AtomicBool = AtomicBool::new(false);
static ROLLBACK_NOTED: AtomicBool = AtomicBool::new(false); // log "can't roll back" once
// AUTO_UPDATE_REQUESTED: the operator asked this install to update itself NOW.
// The actor fetches the latest build WHILE the tunnel is up (so the download
// rides it past an IP-blocking network), then disconnects and installs. Guarded
// by an on-disk marker keyed on the version it updated to, so it fires once per
// new build; clearing the directive clears the marker.
static AUTO_UPDATE_REQUESTED: AtomicBool = AtomicBool::new(false);
static AUTO_UPDATE_NOTED: AtomicBool = AtomicBool::new(false); // log "nothing newer" once
static UPDATE_REQUIRED: AtomicBool = AtomicBool::new(false);
static UPDATE_URL: OnceLock<Mutex<String>> = OnceLock::new();
static UPDATE_MSG: OnceLock<Mutex<String>> = OnceLock::new();
// Guards the periodic plan/directive check-in so only one loop runs at a time
// (re-arming across auto-reconnects is a no-op while it's alive).
static CHECKIN_RUNNING: AtomicBool = AtomicBool::new(false);

// --- auto-connect on untrusted networks --------------------------------------
// The watcher rides the engine's 3s tick but only looks at the network every
// AUTONET_EVERY_TICKS. Its settings + the remembered login are cached here when
// the store is unlocked and deliberately KEPT across a lock: panic/auto-lock must
// hide the UI, but the tray-resident app still has to bring the VPN up by itself
// when the laptop joins a café network. Nothing in here is readable through the
// API — `remembered_login` still answers only while unlocked.
#[derive(Default)]
struct AutoNet {
    enabled: bool,
    /// (id, label) pairs the user marked as safe.
    trusted: Vec<(String, String)>,
    creds: Option<(String, String)>,
    /// The network seen on the last look while DISCONNECTED. Not refreshed while
    /// connected, so a stealth session's hijacked default route can't make the
    /// same network look "new" afterwards.
    last_seen: Option<String>,
    /// The network the user manually disconnected on — don't fight them there.
    suppressed: Option<String>,
    attempts: u32,
    next_try: u64,
    warned_admin: bool,
}
static AUTONET: OnceLock<Mutex<AutoNet>> = OnceLock::new();
const AUTONET_EVERY_TICKS: u32 = 3; // ~9s between looks
const AUTONET_MAX_ATTEMPTS: u32 = 3; // per network join, 30s/60s apart

fn autonet() -> std::sync::MutexGuard<'static, AutoNet> {
    lock_or_recover(AUTONET.get_or_init(Default::default))
}

fn load_trusted(store: &EncryptedStore) -> Vec<(String, String)> {
    store
        .get("trusted_networks")
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.as_array().cloned())
        .map(|a| {
            a.iter()
                .filter_map(|n| {
                    Some((
                        n["id"].as_str()?.to_string(),
                        n["label"].as_str().unwrap_or("").to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn trusted_json(t: &[(String, String)]) -> Value {
    json!(t
        .iter()
        .map(|(id, label)| json!({"id": id, "label": label}))
        .collect::<Vec<_>>())
}

/// Mirror the store into the watcher's cache (on unlock, settings change and
/// connect — whenever the remembered login or the trusted list may change).
fn prime_autonet(store: &EncryptedStore) {
    let creds = (store.get("remember") == Some("true"))
        .then(|| {
            (
                store.get("username").unwrap_or("").to_string(),
                store.get("password").unwrap_or("").to_string(),
            )
        })
        .filter(|(u, p)| !u.is_empty() && !p.is_empty());
    let mut a = autonet();
    a.enabled = store.get("untrusted_autoconnect") == Some("true");
    a.trusted = load_trusted(store);
    a.creds = creds;
}

/// Plain marker that the watcher is on. It holds no secret — it only tells the
/// UI at launch to prime the cache (unlock + immediately re-lock), so the
/// watcher works from a cold start with the app sitting in the tray.
fn netwatch_marker() -> PathBuf {
    data_dir().join("netwatch.pref")
}

/// The boss-key trigger file the desktop shortcut touches (see plat::shortcut).
fn boss_trigger() -> PathBuf {
    data_dir().join(shortcut::TRIGGER_FILE)
}

// --- in-memory activity log (drained by the "logs" command) ----------------
// A small ring buffer of human-readable, timestamped lines describing the
// connection lifecycle. No secrets (never the password) — usernames/mode only.
static LOGS: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
const LOG_CAP: usize = 200;

/// Lock a mutex WITHOUT the usual `.unwrap()`.
///
/// The release profile is `panic = "abort"`, so one poisoned mutex would take the
/// whole app — the weather disguise included — down with it. Poisoning only means
/// some other thread panicked while holding the lock; the data behind it here is a
/// log ring and an Option, both of which are perfectly usable afterwards. So we
/// take the inner value and carry on, which is what "can't-crash by construction"
/// (V4_PLAN principle 2) actually requires.
fn lock_or_recover<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// --- foreign VPN / proxy detection ------------------------------------------
// The latest findings (served in `status`), our own stealth tunnel's interfaces
// (so it's never reported as "another VPN"), and a guard so the periodic check
// never stacks threads if one is slow.
static CONFLICTS: OnceLock<Mutex<Vec<conflict::Finding>>> = OnceLock::new();
static OWN_IFACES: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
/// Tunnel interfaces that already existed before our connect (foreign ones), so
/// a watchdog reconnect that re-creates our TUN — possibly under a new name, e.g.
/// macOS utunN — can re-derive which interfaces are ours.
static TUN_BASELINE: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
static CONFLICT_BUSY: AtomicBool = AtomicBool::new(false);

fn conflicts_slot() -> &'static Mutex<Vec<conflict::Finding>> {
    CONFLICTS.get_or_init(|| Mutex::new(Vec::new()))
}
fn own_ifaces_slot() -> &'static Mutex<Vec<String>> {
    OWN_IFACES.get_or_init(|| Mutex::new(Vec::new()))
}
fn tun_baseline_slot() -> &'static Mutex<Vec<String>> {
    TUN_BASELINE.get_or_init(|| Mutex::new(Vec::new()))
}

/// Record which tunnel interfaces are OURS: whatever exists now that wasn't
/// there before we connected. Empty when stealth isn't the live routing.
fn refresh_own_ifaces() {
    let own: Vec<String> = if tun_routing_selected() && !routing_fell_back() {
        let base = lock_or_recover(tun_baseline_slot()).clone();
        conflict::tunnel_interfaces()
            .into_iter()
            .filter(|i| !base.contains(i))
            .collect()
    } else {
        Vec::new()
    };
    *lock_or_recover(own_ifaces_slot()) = own;
}

fn findings_json(v: &[conflict::Finding]) -> Value {
    Value::Array(
        v.iter()
            .map(|f| json!({"name": f.name, "kind": f.kind.as_str(), "capturing": f.capturing}))
            .collect(),
    )
}

/// Run the detector now (on the CALLING thread — it never touches the engine
/// actor), publish the result, and log what appeared / went away.
fn run_conflict_check() -> Vec<conflict::Finding> {
    let our_tun_active =
        STATE.load(Ordering::SeqCst) == 2 && tun_routing_selected() && !routing_fell_back();
    let own = lock_or_recover(own_ifaces_slot()).clone();
    let found = conflict::detect(&own, our_tun_active);
    let mut slot = lock_or_recover(conflicts_slot());
    for f in found
        .iter()
        .filter(|f| !slot.iter().any(|p| p.name == f.name))
    {
        log_line(format!(
            "> warning: other {} detected — {}{}",
            if f.kind == conflict::Kind::Proxy {
                "proxy"
            } else {
                "VPN"
            },
            f.name,
            if f.capturing {
                " (redirecting traffic)"
            } else {
                ""
            }
        ));
    }
    for p in slot
        .iter()
        .filter(|p| !found.iter().any(|f| f.name == p.name))
    {
        log_line(format!("> cleared: {} no longer detected", p.name));
    }
    *slot = found.clone();
    found
}

/// Fire-and-forget refresh, skipped if one is already running.
fn spawn_conflict_check() {
    if CONFLICT_BUSY
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        std::thread::spawn(|| {
            run_conflict_check();
            CONFLICT_BUSY.store(false, Ordering::SeqCst);
        });
    }
}

fn logs_slot() -> &'static Mutex<VecDeque<String>> {
    LOGS.get_or_init(|| Mutex::new(VecDeque::new()))
}
fn log_line(msg: impl AsRef<str>) {
    let t = now_secs();
    let line = format!(
        "{:02}:{:02}:{:02}  {}",
        (t / 3600) % 24,
        (t / 60) % 60,
        t % 60,
        msg.as_ref()
    );
    let mut q = lock_or_recover(logs_slot());
    if q.len() >= LOG_CAP {
        q.pop_front();
    }
    q.push_back(line);
}
fn logs_snapshot() -> Vec<String> {
    lock_or_recover(logs_slot()).iter().cloned().collect()
}

fn last_session_slot() -> &'static Mutex<Option<Value>> {
    LAST_SESSION.get_or_init(|| Mutex::new(None))
}

/// Snapshot the just-ended session as a receipt ({duration, bytes, drops}), if a
/// session was actually up. `drops` is this session's watchdog recoveries.
/// Called from both the manual disconnect and the watchdog's "connection lost".
fn record_session_receipt(drops: u64) {
    let started = CONNECTED_AT.load(Ordering::SeqCst);
    if started == 0 {
        return; // never fully connected — nothing worth showing
    }
    let duration = now_secs().saturating_sub(started);
    let down = SESSION_DOWN.load(Ordering::SeqCst);
    let up = SESSION_UP.load(Ordering::SeqCst);
    *lock_or_recover(last_session_slot()) = Some(json!({
        "duration_seconds": duration,
        "down_bytes": down,
        "up_bytes": up,
        "total_bytes": down.saturating_add(up),
        "drops": drops,
        "ended_at": now_secs(),
    }));
}

fn last_error_slot() -> &'static Mutex<Option<String>> {
    LAST_ERROR.get_or_init(|| Mutex::new(None))
}
fn set_error(e: Option<String>) {
    *lock_or_recover(last_error_slot()) = e;
}
fn get_error() -> Option<String> {
    lock_or_recover(last_error_slot()).clone()
}
fn set_state(s: State) {
    STATE.store(
        match s {
            State::Disconnected => 0,
            State::Connecting => 1,
            State::Connected => 2,
            State::Reconnecting => 3,
        },
        Ordering::SeqCst,
    );
}
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// --- auto-reconnect backoff scheduling --------------------------------------
fn auto_retry_armed() -> bool {
    AUTO_RETRY_AT.load(Ordering::SeqCst) != 0
}
/// Schedule the FIRST retry after an unexpected drop the one-shot recovery
/// couldn't fix.
fn arm_auto_retry() {
    AUTO_RETRY_BACKOFF.store(AUTO_RETRY_FIRST_SECS, Ordering::SeqCst);
    AUTO_RETRY_AT.store(now_secs() + AUTO_RETRY_FIRST_SECS, Ordering::SeqCst);
    RECONNECT_ATTEMPT.store(1, Ordering::SeqCst);
}
/// Schedule the NEXT retry after one failed — double the wait, capped, so an
/// hours-long outage is retried gently (one TCP attempt every 30s) not hammered.
fn bump_auto_retry() {
    let next = (AUTO_RETRY_BACKOFF.load(Ordering::SeqCst) * 2).min(AUTO_RETRY_MAX_SECS);
    AUTO_RETRY_BACKOFF.store(next, Ordering::SeqCst);
    AUTO_RETRY_AT.store(now_secs() + next, Ordering::SeqCst);
    RECONNECT_ATTEMPT.fetch_add(1, Ordering::SeqCst);
}
/// Stop auto-reconnecting (reconnected, user disconnected, or feature off).
fn clear_auto_retry() {
    AUTO_RETRY_AT.store(0, Ordering::SeqCst);
    AUTO_RETRY_BACKOFF.store(0, Ordering::SeqCst);
    RECONNECT_ATTEMPT.store(0, Ordering::SeqCst);
}

/// Load the data cap + committed usage into the lock-free atomics, so `status`
/// (which has no store access) can serve the sticky-note live. Called once the
/// store is readable, i.e. right after a successful unlock.
fn load_data_cap(store: &EncryptedStore) {
    let limit_gb = store
        .get("data_limit_gb")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0)
        .max(0.0);
    DATA_LIMIT_BYTES.store((limit_gb * BYTES_PER_GB) as u64, Ordering::SeqCst);
    let used = store
        .get("used_bytes")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    USED_COMMITTED.store(used, Ordering::SeqCst);
    set_plan_expiry(store.get("plan_expiry").unwrap_or(""));
    ACCOUNT_ENABLED.store(store.get("account_enabled") != Some("false"), Ordering::SeqCst);
    PLAN_SYNCED_AT.store(
        store.get("plan_synced_at").and_then(|v| v.parse().ok()).unwrap_or(0),
        Ordering::SeqCst,
    );
}

fn plan_expiry_slot() -> &'static Mutex<String> {
    PLAN_EXPIRY.get_or_init(|| Mutex::new(String::new()))
}
fn set_plan_expiry(s: &str) {
    *lock_or_recover(plan_expiry_slot()) = s.to_string();
}
fn get_plan_expiry() -> String {
    lock_or_recover(plan_expiry_slot()).clone()
}

fn update_url_slot() -> &'static Mutex<String> {
    UPDATE_URL.get_or_init(|| Mutex::new(String::new()))
}
fn update_msg_slot() -> &'static Mutex<String> {
    UPDATE_MSG.get_or_init(|| Mutex::new(String::new()))
}

/// Dotted-number version compare: is `a` strictly older than `b`? Missing /
/// non-numeric components read as 0 (e.g. "5.4" < "5.4.1"). Pre-release suffixes
/// are ignored (we compare the leading numeric parts only).
fn version_lt(a: &str, b: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.split(|c: char| c == '.' || c == '-' || c == '+')
            .map(|p| p.chars().take_while(|c| c.is_ascii_digit()).collect::<String>())
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect()
    }
    let (pa, pb) = (parts(a), parts(b));
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        if x != y {
            return x < y;
        }
    }
    false
}

/// Remote wipe + uninstall, after the operator revoked this install (cancelled
/// subscription). Deletes the encrypted store and all app data so no credentials
/// remain, then best-effort removes the staged app directory and exits. The
/// caller (the actor) disconnects the tunnel + clears the system proxy first.
fn remote_wipe() -> ! {
    log_line("> this account was turned off by the operator — removing the app and its data");
    // 1) Wipe all local data (the encrypted store with the saved login lives here).
    let dir = data_dir();
    let _ = std::fs::remove_dir_all(&dir);
    // 2) Best-effort: remove the self-extracted app copy (the dir holding the exe).
    //    On Windows a running exe can't delete itself; the creds wipe above is the
    //    part that actually stops reconnection, so a failure here is fine.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
    std::process::exit(0);
}

/// Marker recording that this install already actioned a rollback directive
/// (so it doesn't re-roll on every plan poll). Cleared when the operator clears
/// the directive — see apply_entitlements.
fn rollback_marker_path() -> std::path::PathBuf {
    data_dir().join("updates").join("rolled_back")
}

/// Operator asked this install to roll back to the previous build. Runs the same
/// local rollback as the in-app button (USB v2-fallback build, else the cached
/// previous OTA build), launching the OS installer wizard, then quits so the
/// older build can replace the running one. Guarded so it fires once per
/// directive. The caller (actor) disconnects the tunnel first.
fn remote_rollback() {
    let marker = rollback_marker_path();
    if marker.is_file() {
        return; // already rolled back for this directive; wait for operator to clear it
    }
    let Some(b) = dreamnail_plat::updater::find_usb_fallback()
        .or_else(dreamnail_plat::updater::find_net_fallback)
    else {
        // Nothing to roll back to (no USB fallback, no cached previous OTA build).
        // Say so once rather than every poll.
        if !ROLLBACK_NOTED.swap(true, Ordering::SeqCst) {
            log_line("> operator asked for a rollback, but there's no previous build to roll back to");
        }
        return;
    };
    log_line(format!("> operator rollback — reverting to v{}", b.version));
    if let Some(dir) = marker.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&marker, netcfg::APP_VERSION);
    match dreamnail_plat::updater::apply(&b.package) {
        Ok(()) => {
            log_line("> rollback installer launched — the app will close to finish");
            std::process::exit(0);
        }
        Err(e) => {
            // Launch failed: drop the marker so it can be retried, and report.
            let _ = std::fs::remove_file(&marker);
            log_line(format!("> rollback failed to launch: {e}"));
        }
    }
}

/// Marker recording the version this install auto-updated TO, so a directive
/// left armed doesn't re-download every poll. Cleared when the operator clears
/// the directive (see apply_entitlements).
fn auto_update_marker_path() -> std::path::PathBuf {
    data_dir().join("updates").join("auto_updated")
}

/// Operator asked this install to update itself now. Unlike rollback (which uses
/// a locally-cached build), a forward update must DOWNLOAD the latest build — so
/// the caller runs this only while the tunnel is up, and the OTA client routes
/// the fetch through it (surviving a network that IP-blocks the server). We grab
/// + verify the build first, THEN drop the tunnel and launch the OS installer,
/// quitting so it can replace the running app. Fires once per target version.
fn remote_update(engine: &mut Engine) {
    let marker = auto_update_marker_path();
    // Cheap manifest check first (no download): is a newer build published?
    let Some(ver) = dreamnail_plat::updater::check_net_update(netcfg::APP_VERSION) else {
        if !AUTO_UPDATE_NOTED.swap(true, Ordering::SeqCst) {
            log_line("> operator asked for an update, but no newer build is available yet");
        }
        return;
    };
    // Already attempted this exact version for this directive (survives restart).
    if std::fs::read_to_string(&marker).ok().as_deref() == Some(ver.as_str()) {
        return;
    }
    // Download + sha256-verify the build THROUGH the live tunnel.
    let Some(b) = dreamnail_plat::updater::fetch_net_update(netcfg::APP_VERSION) else {
        log_line("> update: newer build seen but download/verify failed — will retry");
        return;
    };
    log_line(format!("> operator update — installing v{}", b.version));
    if let Some(dir) = marker.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&marker, &b.version);
    // The installer is already local + verified; drop the tunnel (restores the
    // system proxy) and hand it to the OS wizard.
    engine.disconnect();
    match dreamnail_plat::updater::apply(&b.package) {
        Ok(()) => {
            log_line("> update installer launched — the app will close to finish");
            std::process::exit(0);
        }
        Err(e) => {
            // Launch failed: drop the marker so it can be retried, and report.
            let _ = std::fs::remove_file(&marker);
            log_line(format!("> update failed to launch: {e}"));
        }
    }
}

/// Civil date (yyyy-mm-dd) from a Unix epoch in ms, or None for 0/negative.
/// Hinnant's days→civil algorithm — no date crate needed.
fn ymd_from_epoch_ms(ms: i64) -> Option<String> {
    if ms <= 0 {
        return None;
    }
    let days = ms / 86_400_000; // whole days since 1970-01-01
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// Mirror a freshly-fetched plan into the client: the data cap + usage atomics
/// (so the sticky-note is live) and the expiry (so the banner is live), and
/// persist them so they survive to the next launch and show before connecting.
fn apply_entitlements(store: Option<&mut EncryptedStore>, ent: &entitlements::Entitlements) {
    DATA_LIMIT_BYTES.store(ent.limit_bytes, Ordering::SeqCst);
    USED_COMMITTED.store(ent.used_bytes, Ordering::SeqCst);
    ACCOUNT_ENABLED.store(ent.enabled, Ordering::SeqCst);
    let synced = now_secs();
    PLAN_SYNCED_AT.store(synced, Ordering::SeqCst);
    let expiry_iso = ymd_from_epoch_ms(ent.expiry_ms).unwrap_or_default();
    set_plan_expiry(&expiry_iso);
    // Remote-control directives. Revoke is latched for the actor to action on
    // its next tick (it holds the engine, so it can disconnect before wiping).
    if ent.revoked {
        REVOKED.store(true, Ordering::SeqCst);
    }
    // Rollback directive: latch it for the actor. When the operator CLEARS it,
    // drop the on-disk "already rolled back" marker so a future rollback can fire
    // again.
    ROLLBACK_REQUESTED.store(ent.rollback, Ordering::SeqCst);
    if !ent.rollback {
        let _ = std::fs::remove_file(rollback_marker_path());
        ROLLBACK_NOTED.store(false, Ordering::SeqCst);
    }
    // Auto-update directive: latch it for the actor (it must run while the tunnel
    // is up so the download rides it). Clearing the directive re-arms it.
    AUTO_UPDATE_REQUESTED.store(ent.auto_update, Ordering::SeqCst);
    if !ent.auto_update {
        let _ = std::fs::remove_file(auto_update_marker_path());
        AUTO_UPDATE_NOTED.store(false, Ordering::SeqCst);
    }
    let needs_update = !ent.min_version.is_empty()
        && version_lt(netcfg::APP_VERSION, &ent.min_version);
    UPDATE_REQUIRED.store(needs_update, Ordering::SeqCst);
    *lock_or_recover(update_url_slot()) = ent.update_url.clone();
    *lock_or_recover(update_msg_slot()) = ent.message.clone();
    if let Some(s) = store {
        let gb = ent.limit_bytes as f64 / BYTES_PER_GB;
        s.set("data_limit_gb", format!("{gb}"));
        s.set("used_bytes", ent.used_bytes.to_string());
        s.set("plan_expiry", expiry_iso);
        s.set("account_enabled", if ent.enabled { "true" } else { "false" });
        s.set("plan_synced_at", synced.to_string());
        let _ = s.save();
    }
}

/// How often (seconds) the client re-polls `/api/me` while connected for its plan
/// and the operator's remote-control directives. Overridable for QA with
/// `DREAMNAIL_CHECKIN_SECS`; floored at 60s so a bad value can't hammer the
/// server. Default 20 min.
fn checkin_interval_secs() -> u64 {
    std::env::var("DREAMNAIL_CHECKIN_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|n| n.max(60))
        .unwrap_or(1200)
}

/// Start the periodic plan + directive check-in, unless one is already running.
///
/// The app used to poll `/api/me` only once, right after connecting, so an
/// operator revoke / push-update / rollback / required-version only reached a
/// customer when they next reconnected. This does an immediate sync and then
/// re-syncs every [`checkin_interval_secs`] while the tunnel stays up, so those
/// controls land within one interval. Safe to do periodically now that the
/// directives are ed25519-SIGNED (see [`entitlements::fetch`]) — a network doing
/// TLS interception still can't forge one. Best-effort and self-terminating: it
/// skips a tick while the engine is mid-reconnect (no route) and stops once the
/// engine goes fully Disconnected. `apply_entitlements` latches the directives
/// for the actor to action on its next tick; `PENDING_PLAN` lets the actor
/// persist the refreshed plan.
fn spawn_checkin(user: String, pass: String) {
    if CHECKIN_RUNNING.swap(true, Ordering::SeqCst) {
        return; // one already running
    }
    std::thread::spawn(move || {
        let mut logged_first = false;
        loop {
            match STATE.load(Ordering::SeqCst) {
                0 => break,         // fully disconnected — stop checking in
                2 => {              // Connected: a route is up, sync now
                    if let Some(ent) = entitlements::fetch(netcfg::API_HOST, &user, &pass) {
                        apply_entitlements(None, &ent);
                        *lock_or_recover(&PENDING_PLAN) = Some(ent);
                        if !logged_first {
                            log_line("> plan synced");
                            logged_first = true;
                        }
                    }
                }
                _ => {}             // Connecting/Reconnecting: skip, don't exit
            }
            // Sleep the interval in short slices so a disconnect ends the loop
            // promptly instead of after a whole interval.
            let interval = checkin_interval_secs();
            let mut slept = 0;
            while slept < interval {
                if STATE.load(Ordering::SeqCst) == 0 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(5));
                slept += 5;
            }
        }
        CHECKIN_RUNNING.store(false, Ordering::SeqCst);
    });
}

/// Whether a connect error looks like a TRANSIENT network hiccup worth retrying
/// (the server resetting the SSH session right after a previous one closed, a
/// brief handshake EOF/timeout, a dropped pipe) rather than a real failure like
/// bad credentials or a route mismatch (which must NOT be retried).
///
/// We key on signals that are locale- AND OS-INDEPENDENT:
///   1. Rust's own `io::ErrorKind` names (from the `{:?}` Debug the tunnel embeds:
///      `kind: ConnectionReset`, `TimedOut`, `BrokenPipe`, …). Rust maps every
///      platform's native error to the SAME kind, so this is the reliable
///      cross-OS signal — Windows included.
///   2. The unambiguous Windows WSA numeric codes, as a backup.
///   3. English prose, as a last-resort fallback (Linux/macOS Display text).
///
/// Why not the message text: Windows phrases these failures completely
/// differently ("An existing connection was forcibly closed by the remote host")
/// and localises them, so the old substring match ("reset", "timed out", …) never
/// fired on Windows — the retry was silently Linux/macOS-only.
fn is_transient_conn_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    const KINDS: &[&str] = &[
        "connectionreset",
        "connectionaborted",
        "timedout",
        "brokenpipe",
        "unexpectedeof",
    ];
    if KINDS.iter().any(|k| m.contains(k)) {
        return true;
    }
    // WSAECONNRESET / WSAECONNABORTED / WSAETIMEDOUT / WSAESHUTDOWN — 5-digit,
    // so no risk of matching a substring of an unrelated code.
    const WSA: &[&str] = &["10054", "10053", "10060", "10058"];
    if WSA.iter().any(|c| m.contains(c)) {
        return true;
    }
    m.contains("reset")
        || m.contains("timed out")
        || m.contains("timeout")
        || m.contains("broken pipe")
        || m.contains("eof")
}

/// Classify a connect failure into a fixed (category, hint) pair for anonymous
/// telemetry. BOTH halves are compile-time constants chosen from closed sets —
/// never raw error text — so a report can carry no address, path, or credential.
/// The hint mirrors the branches of `friendly_error` so the operator's report
/// lines up with what the user was told.
fn report_hint(e: &EngineError) -> (&'static str, &'static str) {
    let raw = e.to_string().to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| raw.contains(n));
    match e {
        EngineError::Auth => ("auth", "bad_credentials"),
        EngineError::RouteMismatch { .. } => ("route", "exit_mismatch"),
        EngineError::Unsupported(_) => ("other", "unsupported_os"),
        EngineError::Tunnel(_) => (
            "tunnel",
            if has(&["unknownkey", "wrongserversig", "pinned host key"]) {
                "host_key_mismatch"
            } else if has(&["refused", "code: 61,", "10061"]) {
                "refused"
            } else if has(&[
                "networkunreachable", "hostunreachable", "network is unreachable",
                "no route to host", "code: 101,", "code: 113,", "code: 51,",
                "code: 65,", "10051", "10065", "addrnotavailable",
            ]) {
                "no_internet"
            } else if has(&["bind ", "addrinuse", "address already in use", "10048"]) {
                "port_in_use"
            } else if has(&["timed out", "timeout", "elapsed", "10060"]) {
                "server_timeout"
            } else if has(&["disconnect", "hup", "reset", "aborted", "eof", "broken pipe"]) {
                "dropped"
            } else {
                "unreachable"
            },
        ),
        EngineError::Xray(_) => (
            "xray",
            if has(&["spawn", "missing file", "not found", "permission denied", "os error 13"]) {
                "binary_missing_or_blocked"
            } else {
                "not_ready"
            },
        ),
        EngineError::Proxy(_) => (
            "proxy",
            if raw.ends_with("cancelled") {
                "cancelled"
            } else if has(&["route probe", "no ip in route", "http client"]) {
                "verify_failed"
            } else if has(&["no supported desktop proxy", "system proxy unsupported"]) {
                "unsupported_desktop"
            } else {
                "apply_failed"
            },
        ),
    }
}

/// Turn an engine error into one plain-English sentence for the UI.
///
/// The raw text is for the activity log only: it's jargon ("tunnel error:
/// IO(Os { code: 111 … })"), differs per OS, and some variants embed the server
/// IP — which must never appear on screen. Every string here is a constant, so
/// nothing from the raw error (addresses, paths) can leak through. Matching is on
/// the Debug/Display text because the adapters wrap foreign errors as strings;
/// OS-specific codes are listed per platform (Linux / macOS / Windows WSA).
fn friendly_error(e: &EngineError) -> &'static str {
    let raw = e.to_string().to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| raw.contains(n));
    match e {
        EngineError::Auth => "Wrong username or password.",
        EngineError::RouteMismatch { .. } => {
            "Connected, but traffic isn't going through the VPN. Please try again."
        }
        EngineError::Unsupported(_) => "This isn't supported on this computer yet.",
        EngineError::Tunnel(_) => {
            if has(&["unknownkey", "wrongserversig", "pinned host key"]) {
                // The pinned SSH host key didn't match: possible interception.
                "The server's identity couldn't be verified, so the connection was \
                 stopped for your safety. Please contact support."
            } else if has(&["connectionrefused", "refused", "code: 61,", "10061"]) {
                "The server refused the connection. It may be down — try again in a minute."
            } else if has(&[
                "networkunreachable",
                "hostunreachable",
                "network is unreachable",
                "no route to host",
                "code: 101,",
                "code: 113,",
                "code: 51,",
                "code: 65,",
                "10051",
                "10065",
                "addrnotavailable",
            ]) {
                "No internet connection. Check your Wi-Fi or network cable."
            } else if has(&["bind ", "addrinuse", "address already in use", "10048"]) {
                "Another app is using a network port Dreamnail needs. Close other VPN \
                 apps or restart your computer, then try again."
            } else if has(&["timed out", "timeout", "elapsed", "10060"]) {
                "The server isn't responding. Check your internet connection and try again."
            } else if has(&["disconnect", "hup", "reset", "aborted", "eof", "broken pipe"]) {
                "The connection to the server dropped. Please try again."
            } else {
                "Couldn't reach the server. Check your internet connection and try again."
            }
        }
        EngineError::Xray(_) => {
            if has(&["spawn", "missing file", "not found", "permission denied", "os error 13"]) {
                "Part of Dreamnail is missing or was blocked. Reinstall the app, and \
                 check your antivirus hasn't quarantined it."
            } else {
                "Couldn't start the secure connection on this computer. Please try again."
            }
        }
        EngineError::Proxy(_) => {
            if raw.ends_with("cancelled") {
                "Connection cancelled."
            } else if has(&["route probe", "no ip in route", "http client"]) {
                "Connected to the server, but couldn't confirm your traffic is protected. \
                 Please try again."
            } else if has(&["no supported desktop proxy", "system proxy unsupported"]) {
                "Web-only routing isn't supported on this desktop. Switch routing to \
                 WHOLE SYSTEM in settings."
            } else {
                "Couldn't change this computer's network settings. Try again, or switch \
                 routing in settings."
            }
        }
    }
}

/// macOS silently clears the `networksetup` system proxy on Wi-Fi flaps,
/// sleep/wake, or when another VPN app runs — WHILE our SSH tunnel stays alive,
/// so the watchdog's `alive()` check never trips and traffic quietly stops with
/// no recovery. Re-assert the system proxy periodically while connected in
/// STANDARD (non-TUN) routing. No-op in stealth/TUN mode (routing is via the
/// device, not the proxy) and a no-op on Linux/Windows, whose global proxy
/// settings don't drift this way. Mirrors the Python client's 30s keepalive.
#[cfg(target_os = "macos")]
fn reassert_system_proxy_macos() {
    let effective_proxy = !tun_routing_selected() || routing_fell_back();
    if effective_proxy {
        let mut p = SystemProxy;
        let _ = p.enable();
    }
}
#[cfg(not(target_os = "macos"))]
fn reassert_system_proxy_macos() {}

/// Single source of truth for the app's per-user data directory (the encrypted
/// store, the routing preference, and — on the Dart side — the UI single-instance
/// lock). This MUST agree with the Flutter `AppPaths.dataDir()`
/// (frontend/dreamnail_ui/lib/util/app_paths.dart); change the rule in both.
///
/// Resolution order:
///   1. `DREAMNAIL_DATA_DIR` if set — how an ELEVATED relaunch is handed the SAME
///      directory even when it runs under a different HOME/%APPDATA%, so the
///      elevated copy still finds the stealth preference + saved login.
///   2. The per-OS default: `%APPDATA%\Dreamnail` (Windows) / `~/.dreamnail`.
fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("DREAMNAIL_DATA_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    if cfg!(windows) {
        // Prefer %APPDATA%; if it's ever missing, derive it from the user profile
        // rather than ".", which would be a per-launch relative dir the saved
        // login can't survive in.
        std::env::var("APPDATA")
            .map(PathBuf::from)
            .or_else(|_| {
                std::env::var("USERPROFILE")
                    .map(|p| PathBuf::from(p).join("AppData").join("Roaming"))
            })
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("Dreamnail")
    } else {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".dreamnail")
    }
}

fn store_path() -> PathBuf {
    data_dir().join("config.enc")
}

// The stealth (routing) toggle is a device preference, NOT a secret, so it must
// persist even when the encrypted store is locked. Storing it only in the
// encrypted store meant a save while locked was silently dropped and the switch
// reverted to off. Keep it in a plain sibling file that never needs unlocking.
fn routing_pref_path() -> PathBuf {
    store_path().with_file_name("routing.pref")
}

fn read_routing_pref() -> Option<String> {
    std::fs::read_to_string(routing_pref_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn write_routing_pref(v: &str) {
    let p = routing_pref_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(p, v);
}

// Ad blocker toggle. A device preference (not a secret) that must survive while
// the store is locked AND be readable by the plat layer at Xray start, so it
// lives in a plain sibling file (adblock.pref) that xray::adblock_pref() reads.
// "on" = block ad/tracker domains; absent = off. Takes effect on next connect.
fn adblock_pref_path() -> PathBuf {
    store_path().with_file_name("adblock.pref")
}

fn read_adblock_pref() -> bool {
    std::fs::read_to_string(adblock_pref_path())
        .map(|s| s.trim() == "on")
        .unwrap_or(false)
}

fn write_adblock_pref(on: bool) {
    let p = adblock_pref_path();
    if on {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, "on");
    } else {
        let _ = std::fs::remove_file(p);
    }
}

// --- stranded-proxy crash recovery -----------------------------------------
// A clean disconnect tears the system proxy down, but a CRASH or kill while
// connected (e.g. the app dying mid-session) can't run teardown — leaving the OS
// pointed at a dead local proxy with no internet. We drop a marker file while
// connected and clear it on disconnect; on the next startup, a leftover marker
// means the last session ended dirty, so we reset the proxy once.
fn proxy_flag_path() -> PathBuf {
    store_path().with_file_name("proxy.active")
}

fn mark_proxy_active() {
    let p = proxy_flag_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(p, "1");
}

fn clear_proxy_flag() {
    let _ = std::fs::remove_file(proxy_flag_path());
}

/// If a previous session left the proxy marker (it crashed/was killed while
/// connected), clear the stranded system proxy so the user isn't left offline.
/// Best-effort and safe: disabling an already-clear proxy is a no-op.
fn recover_stranded_proxy() {
    if proxy_flag_path().exists() {
        SystemProxy.disable();
        // In STEALTH mode the stranded state isn't a proxy setting at all — it's a
        // TUN device, a default route pointing into it and a rewritten resolver,
        // left by a helper whose parent-death watchdog never got to run (hard kill
        // / power loss). Clearing only the system proxy left those machines with
        // no internet until a reboot.
        stop_stale_tun();
        clear_proxy_flag();
    }
}

/// Unwind an orphaned stealth helper, where the platform supports stealth at all.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn stop_stale_tun() {
    dreamnail_plat::stop_stale_tun();
}
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn stop_stale_tun() {}

// --- the backend that lives on the actor thread ----------------------------
struct Backend {
    engine: Engine,
    store: Option<EncryptedStore>,
    /// The login we last connected with, held in memory only (never persisted
    /// here — "remember" handles persistence). Lets the "Report a problem" button
    /// authenticate a report even after the password field is cleared.
    creds: Option<(String, String)>,
}

impl Backend {
    /// The persistent-auto-reconnect preference (default ON). Read live each tick
    /// so toggling it in Settings takes effect without a reconnect.
    fn auto_reconnect_enabled(&self) -> bool {
        self.store
            .as_ref()
            .and_then(|s| s.get("auto_reconnect_enabled"))
            .map(|v| v != "false")
            .unwrap_or(true)
    }

    /// Fold a finished session's bytes into the persistent totals: the all-time
    /// usage (Settings readout) AND the resettable cap counter (sticky-note).
    /// Best-effort — skipped if the store is absent/locked, keeping the atomic in
    /// step with what actually got saved.
    fn commit_session_usage(&mut self, down: u64, up: u64) {
        let total = down.saturating_add(up);
        if total == 0 {
            return;
        }
        if let Some(s) = self.store.as_mut() {
            let cur_d = s
                .get("lifetime_down_bytes")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            let cur_u = s
                .get("lifetime_up_bytes")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            let used = s
                .get("used_bytes")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            let new_used = used.saturating_add(total);
            s.set("lifetime_down_bytes", cur_d.saturating_add(down).to_string());
            s.set("lifetime_up_bytes", cur_u.saturating_add(up).to_string());
            s.set("used_bytes", new_used.to_string());
            let _ = s.save();
            USED_COMMITTED.store(new_used, Ordering::SeqCst);
        }
    }

    /// Auto-connect on untrusted networks: one look at the current network.
    /// Connects only from a settled idle state, on a network that is neither
    /// trusted nor the one the user just disconnected on, after it has been seen
    /// on two consecutive looks (so DHCP/DNS have settled), with a small retry
    /// budget per network join.
    fn autonet_tick(&mut self) {
        let (enabled, creds) = {
            let a = autonet();
            (a.enabled, a.creds.clone())
        };
        let Some((user, pass)) = creds.filter(|_| enabled) else {
            return;
        };
        if self.engine.state() != State::Disconnected || auto_retry_armed() {
            return;
        }
        let net = netid::current();
        let mut a = autonet();
        let Some(net) = net else {
            // Offline: forget, so rejoining any network counts as arriving.
            a.last_seen = None;
            return;
        };
        if a.last_seen.as_deref() != Some(net.id.as_str()) {
            // Just arrived (or first look): wait one more look for the network to
            // settle, and start a fresh attempt budget for it.
            if a.suppressed.as_deref() != Some(net.id.as_str()) {
                a.suppressed = None;
            }
            a.last_seen = Some(net.id);
            a.attempts = 0;
            a.next_try = 0;
            return;
        }
        if a.trusted.iter().any(|(id, _)| *id == net.id)
            || a.suppressed.as_deref() == Some(net.id.as_str())
            || a.attempts >= AUTONET_MAX_ATTEMPTS
            || now_secs() < a.next_try
        {
            return;
        }
        // Stealth without "remember admin" would pop an admin-password prompt
        // out of nowhere — skip, and say why once.
        let tun = read_routing_pref().map(|v| v == "tun").unwrap_or(false);
        if tun && !elevation::installed() {
            let first = !a.warned_admin;
            a.warned_admin = true;
            drop(a);
            if first {
                log_line(
                    "> auto-connect skipped: stealth needs \"remember admin\" to connect unattended",
                );
            }
            return;
        }
        a.attempts += 1;
        a.next_try = now_secs() + 30 * u64::from(a.attempts);
        drop(a);
        log_line(format!(
            "> untrusted network \"{}\" — connecting automatically",
            net.label
        ));
        let r = self.handle(
            "connect",
            &json!({"username": user, "password": pass, "remember": true}),
        );
        if r["success"] == true {
            autonet().attempts = 0;
        }
    }

    fn handle(&mut self, method: &str, args: &Value) -> Value {
        // Persist a plan the post-connect background sync fetched since the
        // last command.
        if let Some(ent) = lock_or_recover(&PENDING_PLAN).take() {
            apply_entitlements(self.store.as_mut(), &ent);
        }
        // Operator revoked this install → tear the tunnel down (restores the
        // system proxy), then wipe local data + uninstall. Does not return.
        if REVOKED.load(Ordering::SeqCst) {
            self.engine.disconnect();
            remote_wipe();
        }
        // Operator asked for a rollback → disconnect, then launch the previous
        // build's installer (fires once per directive; may not return).
        if ROLLBACK_REQUESTED.load(Ordering::SeqCst) {
            self.engine.disconnect();
            remote_rollback();
        }
        // Operator asked for an update → fetch the latest build WHILE connected
        // (so the download rides the tunnel past an IP-blocking network), then
        // disconnect and install. Only while fully connected; otherwise wait for
        // the next poll once the tunnel is up. remote_update disconnects itself.
        if AUTO_UPDATE_REQUESTED.load(Ordering::SeqCst)
            && self.engine.state() == State::Connected
        {
            remote_update(&mut self.engine);
        }
        match method {
            "unlock" => {
                let pw = args["password"].as_str().unwrap_or("");
                match EncryptedStore::open(store_path(), pw) {
                    Ok(s) => {
                        // One-time breadcrumb: where the store lives. If "remember"
                        // ever misbehaves we can see the actual path it persists to.
                        log_line(format!("> store: {}", store_path().display()));
                        // Apply the saved telemetry preference now that the store is
                        // readable (defaults to on when unset).
                        report::set_enabled(s.get("error_reports") != Some("false"));
                        // Seed the data-cap atomics so the sticky-note is live
                        // from the first status poll after unlock.
                        load_data_cap(&s);
                        // Prime the untrusted-network watcher (kept across lock).
                        prime_autonet(&s);
                        self.store = Some(s);
                        json!({"success": true, "error": null})
                    }
                    Err(_) => json!({"success": false, "error": "wrong password"}),
                }
            }
            "lock" => {
                self.store = None;
                json!({"success": true})
            }
            // "Report a problem" button: send an authenticated, per-customer
            // report (typed message + a fresh leak-test diagnostics bundle) to
            // the server for the operator to read in the admin console. Needs a
            // login we've connected with at least once this run (held in memory).
            "report_problem" => {
                let message = args["message"].as_str().unwrap_or("").to_string();
                match self.creds.clone() {
                    Some((user, pass)) if !user.is_empty() => {
                        let connected = self.engine.state() == State::Connected;
                        let tun = tun_routing_selected() && !routing_fell_back();
                        report::send_report(
                            &user,
                            &pass,
                            "user",
                            &message,
                            vec![("state".into(), format!("{:?}", self.engine.state()))],
                            Some(diag::Session {
                                connected,
                                tun,
                                proxy_applied: connected && !tun,
                            }),
                            true, // manual: always send, ignoring the telemetry opt-out
                        );
                        log_line("> problem report sent to support — thank you");
                        json!({"success": true, "error": null})
                    }
                    _ => json!({
                        "success": false,
                        "error": "Please connect once first so your report can reach support."
                    }),
                }
            }
            "reset_usage" => {
                // Start a fresh data-cap cycle (e.g. at the customer's billing
                // reset). Zeroes the committed counter; the current session, if
                // any, keeps counting toward the new cycle.
                USED_COMMITTED.store(0, Ordering::SeqCst);
                if let Some(s) = self.store.as_mut() {
                    s.set("used_bytes", "0");
                    let _ = s.save();
                }
                log_line("> data usage reset");
                json!({"success": true})
            }
            "connect" => {
                let user = args["username"].as_str().unwrap_or("").to_string();
                let pass = args["password"].as_str().unwrap_or("").to_string();
                let remember = args["remember"].as_bool().unwrap_or(false);
                // Apply the routing mode (stealth/TUN vs standard/proxy) before
                // connecting so ModalProxy picks the right backend. Read the plain
                // preference (works locked); fall back to the store for older data.
                let tun = read_routing_pref()
                    .or_else(|| {
                        self.store
                            .as_ref()
                            .and_then(|s| s.get("routing_mode").map(String::from))
                    })
                    .map(|v| v == "tun")
                    .unwrap_or(false);
                set_tun_routing(tun);
                // Fresh attempt: clear any abort left by a previous cancel, and
                // zero the byte counters so "this session" totals mean what they
                // say.
                cancel::clear_abort();
                // The user is driving — cancel any pending auto-reconnect so it
                // can't race this manual attempt.
                clear_auto_retry();
                traffic::reset();
                DOWN_BPS.store(0, Ordering::SeqCst);
                UP_BPS.store(0, Ordering::SeqCst);
                SESSION_DOWN.store(0, Ordering::SeqCst);
                SESSION_UP.store(0, Ordering::SeqCst);
                set_state(State::Connecting);
                set_error(None);
                log_line(format!(
                    "> connect requested [{}]{}",
                    if tun { "stealth/tun" } else { "standard" },
                    if user.is_empty() {
                        String::new()
                    } else {
                        format!(" user={user}")
                    }
                ));
                // Connecting can hit a transient failure — most often the server
                // resetting the SSH connection right after a previous session
                // closed (ECONNRESET), or a brief handshake EOF/timeout. Those
                // succeed on a quick retry, so retry up to 3 attempts before
                // surfacing the error. Transience is detected cross-OS (see
                // `is_transient_conn_error`) so the retry works on Windows/macOS
                // too, not just Linux.
                // Snapshot tunnel interfaces BEFORE stealth comes up: whatever is
                // new afterwards is our own TUN (it has no fixed name), so the
                // foreign-VPN detector never reports us.
                let pre_tun = if tun {
                    conflict::tunnel_interfaces()
                } else {
                    Vec::new()
                };
                let mut attempt = 1u32;
                let res = loop {
                    match self.engine.connect(Credentials {
                        username: user.clone(),
                        password: pass.clone(),
                    }) {
                        Ok(()) => break Ok(()),
                        Err(e) => {
                            // The user asked to stop — don't burn another attempt.
                            if cancel::abort_requested() {
                                break Err(e);
                            }
                            let transient = is_transient_conn_error(&e.to_string());
                            attempt += 1;
                            if transient && attempt <= 3 {
                                log_line(format!(
                                    "> transient tunnel error — retrying ({attempt}/3)"
                                ));
                                std::thread::sleep(Duration::from_millis(900));
                                continue;
                            }
                            break Err(e);
                        }
                    }
                };
                match res {
                    Ok(()) => {
                        log_line("> tunnel established");
                        log_line(format!("> exit verified: {}", netcfg::EXPECTED_EXIT_IP));
                        // Cache the working login so the "Report a problem" button
                        // can authenticate a report to the server even after the
                        // password field is cleared (it lives only in memory here).
                        self.creds = Some((user.clone(), pass.clone()));
                        CONNECTED_AT.store(now_secs(), Ordering::SeqCst);
                        // Baseline this session's drop count, and clear the previous
                        // receipt now that a new session has begun.
                        SESSION_RECOVERIES_BASE
                            .store(self.engine.recoveries(), Ordering::SeqCst);
                        *lock_or_recover(last_session_slot()) = None;
                        // Mark the session live so a crash/kill before a clean
                        // disconnect can be recovered on the next startup.
                        mark_proxy_active();
                        // Seed the signal indicator immediately (off-thread) so it
                        // isn't "No signal" until the next 3s watchdog tick.
                        std::thread::spawn(|| PING_MS.store(measure_ping(), Ordering::SeqCst));
                        *lock_or_recover(tun_baseline_slot()) = pre_tun;
                        refresh_own_ifaces();
                        if let Some(s) = self.store.as_mut() {
                            s.set("username", if remember { user.as_str() } else { "" });
                            // Persist the password too (encrypted at rest, unlocked
                            // by the launch password) so "remember" can pre-fill it
                            // and the auto-connect setting can reconnect on unlock.
                            // Cleared when the box is unticked.
                            s.set("password", if remember { pass.as_str() } else { "" });
                            s.set("remember", if remember { "true" } else { "false" });
                            // Surface a failed save (it was silently dropped, which
                            // on Windows looked like "remember never works" with no
                            // clue why). The login lives only in this store.
                            if let Err(e) = s.save() {
                                log_line(format!("> could not save your login (it won't be remembered): {e}"));
                            }
                            prime_autonet(s);
                        }
                        set_state(self.engine.state());
                        // Sync this customer's plan (data cap + usage + expiry)
                        // from the server now that we have their creds and a live
                        // route. Off the connect path (it's a separate HTTPS round
                        // trip the button used to wait for): the thread seeds the
                        // cap atomics so the UI updates on the next status poll,
                        // and the actor persists it on its next command.
                        // Best-effort: on any failure we keep whatever the
                        // sticky-note/banner already showed.
                        // Periodic check-in: an immediate plan+directive sync,
                        // then a re-sync every interval while connected, so an
                        // operator revoke / push-update / rollback lands without
                        // waiting for the customer to reconnect. Directives are
                        // signed, so periodic polling can't be abused by a MITM.
                        spawn_checkin(user.clone(), pass.clone());
                        // Re-check now that we're up (a VPN that grabbed the route
                        // mid-connect, or our own footprint now excluded).
                        spawn_conflict_check();
                        json!({"success": true, "error": null})
                    }
                    Err(e) => {
                        // Raw detail → log (for debugging); plain English → UI.
                        log_line(format!("> connect failed: {e}"));
                        let msg = friendly_error(&e);
                        set_error(Some(msg.to_string()));
                        set_state(self.engine.state());
                        // Telemetry (opt-outable). Skip a user-cancelled attempt —
                        // that isn't a failure worth reporting.
                        let (cat, hint) = report_hint(&e);
                        if !cancel::abort_requested() && hint != "cancelled" {
                            if user.is_empty() {
                                // No login to authenticate with → the anonymous,
                                // IP-less report (fixed category+hint only).
                                report::send_connect_failure(cat, hint);
                            } else {
                                // We have the customer's login → send the richer,
                                // per-customer report the admin console shows. No
                                // leak test (the tunnel is down); just the coarse
                                // category/hint + mode/attempts. Honours opt-out.
                                report::send_report(
                                    &user,
                                    &pass,
                                    "connect_fail",
                                    "",
                                    vec![
                                        ("cat".into(), cat.to_string()),
                                        ("hint".into(), hint.to_string()),
                                        ("mode".into(), (if tun { "stealth" } else { "standard" }).to_string()),
                                        ("attempts".into(), attempt.to_string()),
                                    ],
                                    None,
                                    false,
                                );
                            }
                        }
                        json!({"success": false, "error": msg})
                    }
                }
            }
            "disconnect" => {
                self.engine.disconnect();
                // Teardown is done; the flag has served its purpose (it only ever
                // shortens waits), so clear it or the NEXT connect would start out
                // already cancelled.
                cancel::clear_abort();
                // A deliberate disconnect must never auto-reconnect.
                clear_auto_retry();
                log_line("> disconnect — link closed");
                // Session summary (before the counters are zeroed).
                let drops = self
                    .engine
                    .recoveries()
                    .saturating_sub(SESSION_RECOVERIES_BASE.load(Ordering::SeqCst));
                record_session_receipt(drops);
                self.commit_session_usage(
                    SESSION_DOWN.load(Ordering::SeqCst),
                    SESSION_UP.load(Ordering::SeqCst),
                );
                CONNECTED_AT.store(0, Ordering::SeqCst);
                DOWN_BPS.store(0, Ordering::SeqCst);
                UP_BPS.store(0, Ordering::SeqCst);
                clear_proxy_flag(); // clean teardown — no recovery needed next launch
                set_state(self.engine.state());
                // A deliberate disconnect on an untrusted network must not be
                // undone by the watcher seconds later: remember this network
                // until the user moves to another one. Looked up AFTER teardown,
                // so stealth's hijacked default route is already gone.
                if autonet().enabled {
                    let here = netid::current().map(|n| n.id);
                    let mut a = autonet();
                    a.last_seen = here.clone();
                    a.suppressed = here;
                    a.attempts = 0;
                }
                // Our tunnel is gone; refresh so the offline screen shows only
                // genuinely foreign VPNs/proxies.
                lock_or_recover(own_ifaces_slot()).clear();
                spawn_conflict_check();
                json!({"success": true})
            }
            "reset_proxy" => {
                if self.engine.state() == State::Connected {
                    json!({"success": false, "error": "disconnect first"})
                } else {
                    SystemProxy.disable();
                    // Stealth leaves a TUN device, a hijacked default route and a
                    // rewritten resolver behind, none of which the proxy reset
                    // touches — so unwind an orphaned helper too.
                    stop_stale_tun();
                    log_line("> system proxy reset");
                    json!({"success": true, "error": null})
                }
            }
            "remembered_login" => {
                let (u, p, r) = self
                    .store
                    .as_ref()
                    .map(|s| {
                        (
                            s.get("username").unwrap_or("").to_string(),
                            s.get("password").unwrap_or("").to_string(),
                            s.get("remember") == Some("true"),
                        )
                    })
                    .unwrap_or_default();
                // Password is only returned in-process to the just-unlocked UI so
                // it can pre-fill the field and auto-connect; it never leaves the
                // machine and is empty unless "remember" is on.
                json!({"username": u, "password": p, "remember": r})
            }
            "settings" => {
                let get = |k: &str| {
                    self.store
                        .as_ref()
                        .and_then(|s| s.get(k).map(str::to_string))
                };
                let timeout = get("timeout_minutes")
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(5);
                let auto = get("auto_reconnect_enabled")
                    .map(|v| v == "true")
                    .unwrap_or(true);
                let theme = get("theme").unwrap_or_else(|| "dark".into());
                // "proxy" (standard, default) or "tun" (stealth, routes all apps).
                // Read the plain pref (persists while locked); fall back to the
                // store for data written by older builds.
                let routing = read_routing_pref()
                    .or_else(|| get("routing_mode"))
                    .unwrap_or_else(|| "proxy".into());
                let error_reports = get("error_reports").map(|v| v != "false").unwrap_or(true);
                // Auto-connect after unlock (opt-in, default off). Only acts when
                // "remember" saved a username+password to connect with.
                let auto_connect = get("auto_connect").map(|v| v == "true").unwrap_or(false);
                // Auto-connect whenever the machine joins a network not marked as
                // safe (opt-in). Same remembered-login requirement as above.
                let untrusted_autoconnect = get("untrusted_autoconnect")
                    .map(|v| v == "true")
                    .unwrap_or(false);
                let trusted = self.store.as_ref().map(load_trusted).unwrap_or_default();
                // Local plan-expiry reminder (ISO yyyy-mm-dd, or "" if unset). The
                // customer sets it from their welcome message; it's a renewal nudge,
                // not a live server value.
                let plan_expiry = get("plan_expiry").unwrap_or_default();
                // The customer's plan data cap in GB (0 = no cap), mirrored from
                // what the operator set. Drives the sticky-note on the VPN screen.
                let data_limit_gb = get("data_limit_gb")
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(0.0);
                // All-time data usage (sum of every past session), for the usage
                // readout in Settings. 0 until the first session ends.
                let lifetime_down = get("lifetime_down_bytes")
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0);
                let lifetime_up = get("lifetime_up_bytes")
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0);
                json!({
                    "timeout_minutes": timeout,
                    "auto_reconnect_enabled": auto,
                    "lifetime_down_bytes": lifetime_down,
                    "lifetime_up_bytes": lifetime_up,
                    "auto_connect": auto_connect,
                    "untrusted_autoconnect": untrusted_autoconnect,
                    "trusted_networks": trusted_json(&trusted),
                    "plan_expiry": plan_expiry,
                    "data_limit_gb": data_limit_gb,
                    "theme": theme,
                    "routing_mode": routing,
                    "adblock": read_adblock_pref(),
                    // Weak-network mode: safe 1280 MTU + max SSH stripes for
                    // flaky / locked-down networks (school Wi-Fi, captive portals).
                    "weaknet": dreamnail_plat::weaknet_mode(),
                    // Reality door: force-443 (skip port 22) + the chosen decoy
                    // site, plus the list of decoys for the UI dropdown.
                    "force_443": dreamnail_plat::force_443(),
                    "reality_decoy": dreamnail_plat::reality_decoy_sni(),
                    "reality_decoys": netcfg::REALITY_DECOYS
                        .iter()
                        .map(|(label, sni)| json!({ "label": label, "sni": sni }))
                        .collect::<Vec<_>>(),
                    "error_reports": error_reports,
                    // "Remember admin" reflects real OS state (is the passwordless
                    // rule installed?), not a stored flag — so it stays honest even
                    // if the rule was removed outside the app. `supported` lets the
                    // UI hide the toggle on an OS without this yet.
                    "remember_admin": elevation::installed(),
                    "remember_admin_supported": elevation::supported(),
                })
            }
            "set_settings" => {
                // Routing mode (stealth) is a device preference, not a secret, so
                // persist it OUTSIDE the encrypted store — it must stick even when
                // the store is locked (otherwise the switch silently reverts).
                if let Some(rm) = args["routing_mode"].as_str() {
                    write_routing_pref(rm);
                    set_tun_routing(rm == "tun");
                    log_line(format!(
                        "> routing mode: {}",
                        if rm == "tun" {
                            "stealth/tun"
                        } else {
                            "standard"
                        }
                    ));
                }
                if let Some(on) = args["adblock"].as_bool() {
                    write_adblock_pref(on);
                    log_line(if on { "> ad blocker: on (next connect)" } else { "> ad blocker: off (next connect)" });
                }
                if let Some(on) = args["force_443"].as_bool() {
                    dreamnail_plat::set_force_443(on);
                    log_line(if on {
                        "> force 443: on (skips port 22; next connect)"
                    } else {
                        "> force 443: off (next connect)"
                    });
                }
                if let Some(on) = args["weaknet"].as_bool() {
                    dreamnail_plat::set_weaknet_mode(on);
                    log_line(if on {
                        "> weak-network mode: on (smaller packets + more connections; next connect)"
                    } else {
                        "> weak-network mode: off (next connect)"
                    });
                }
                if let Some(sni) = args["reality_decoy"].as_str() {
                    dreamnail_plat::set_reality_decoy(sni);
                    log_line(format!("> disguise site: {sni} (next connect)"));
                }
                if let Some(s) = self.store.as_mut() {
                    if let Some(t) = args["timeout_minutes"].as_i64() {
                        s.set("timeout_minutes", t.to_string());
                    }
                    if let Some(a) = args["auto_reconnect_enabled"].as_bool() {
                        s.set("auto_reconnect_enabled", if a { "true" } else { "false" });
                    }
                    if let Some(th) = args["theme"].as_str() {
                        s.set("theme", th);
                    }
                    if let Some(er) = args["error_reports"].as_bool() {
                        s.set("error_reports", if er { "true" } else { "false" });
                        report::set_enabled(er);
                    }
                    if let Some(ac) = args["auto_connect"].as_bool() {
                        s.set("auto_connect", if ac { "true" } else { "false" });
                    }
                    if let Some(on) = args["untrusted_autoconnect"].as_bool() {
                        s.set("untrusted_autoconnect", if on { "true" } else { "false" });
                        if on {
                            let _ = std::fs::write(netwatch_marker(), "1");
                        } else {
                            let _ = std::fs::remove_file(netwatch_marker());
                        }
                        log_line(if on {
                            "> auto-connect on untrusted networks: on"
                        } else {
                            "> auto-connect on untrusted networks: off"
                        });
                    }
                    if let Some(list) = args["trusted_networks"].as_array() {
                        let clean: Vec<(String, String)> = list
                            .iter()
                            .filter_map(|n| {
                                Some((
                                    n["id"].as_str()?.to_string(),
                                    n["label"].as_str().unwrap_or("").to_string(),
                                ))
                            })
                            .collect();
                        s.set("trusted_networks", trusted_json(&clean).to_string());
                    }
                    if let Some(pe) = args["plan_expiry"].as_str() {
                        s.set("plan_expiry", pe);
                        set_plan_expiry(pe);
                    }
                    if let Some(dl) = args["data_limit_gb"].as_f64() {
                        let dl = dl.max(0.0);
                        s.set("data_limit_gb", format!("{dl}"));
                        DATA_LIMIT_BYTES.store((dl * BYTES_PER_GB) as u64, Ordering::SeqCst);
                    }
                    // Mirror into the store too (harmless) for older read paths.
                    if let Some(rm) = args["routing_mode"].as_str() {
                        s.set("routing_mode", rm);
                    }
                    let _ = s.save();
                    prime_autonet(s);
                }
                json!({"success": true})
            }
            // "Remember admin" on/off. Separate from set_settings because it
            // triggers a REAL admin prompt and can be declined — the UI needs the
            // true outcome to leave its toggle where it really ended up, rather
            // than assuming success. Never touches the encrypted store.
            "set_remember_admin" => {
                let enable = args["enabled"].as_bool().unwrap_or(false);
                let res = if enable {
                    elevation::install()
                } else {
                    elevation::uninstall()
                };
                match res {
                    Ok(()) => {
                        log_line(if enable {
                            "> remembered admin: enabled (passwordless stealth)"
                        } else {
                            "> remembered admin: disabled"
                        });
                        // Report the resulting real state so the UI can sync.
                        json!({"success": true, "enabled": elevation::installed()})
                    }
                    Err(e) => {
                        json!({"success": false, "error": e.to_string(), "enabled": elevation::installed()})
                    }
                }
            }
            "logs" => json!({"lines": logs_snapshot()}),
            "set_focus" => json!({"success": true}),
            // Update sources (see dreamnail_plat::updater): a plugged-in USB
            // (cleanest — no AV flag on any OS) is tried FIRST; if none, fall
            // back to an over-the-air (OTA) check against our own server. USB
            // check is instant; the OTA check is one small HTTPS request.
            "update_check" => {
                if let Some(b) = updater::find_usb_update(netcfg::APP_VERSION) {
                    json!({
                        "available": true, "current": netcfg::APP_VERSION,
                        "latest": b.version, "url": b.package.to_string_lossy(),
                        "source": "usb",
                    })
                } else if let Some(v) = updater::check_net_update(netcfg::APP_VERSION) {
                    json!({
                        "available": true, "current": netcfg::APP_VERSION,
                        "latest": v, "url": "", "source": "ota",
                    })
                } else {
                    json!({
                        "available": false, "current": netcfg::APP_VERSION,
                        "latest": "", "url": "", "source": "",
                    })
                }
            }
            // Apply: USB first. Else OTA — download + verify, SAVE A ROLLBACK
            // POINT (so a bad update can be reverted), then launch the installer.
            "update_apply" => {
                if let Some(b) = updater::find_usb_update(netcfg::APP_VERSION) {
                    log_line(format!("> applying update (USB) -> v{}", b.version));
                    match updater::apply(&b.package) {
                        Ok(()) => json!({"ok": true, "error": null}),
                        Err(e) => {
                            log_line(format!("> update failed: {e}"));
                            json!({"ok": false, "error": e.to_string()})
                        }
                    }
                } else if let Some(b) = updater::fetch_net_update(netcfg::APP_VERSION) {
                    log_line(format!("> downloaded update (OTA) -> v{}", b.version));
                    // Keep the rollback safety net; never let it block the update.
                    if let Err(e) = updater::save_rollback_point(&b.package, &b.version) {
                        log_line(format!("> (rollback point not saved: {e})"));
                    }
                    match updater::apply(&b.package) {
                        Ok(()) => json!({"ok": true, "error": null}),
                        Err(e) => {
                            log_line(format!("> update failed: {e}"));
                            json!({"ok": false, "error": e.to_string()})
                        }
                    }
                } else {
                    json!({"ok": false, "error": "no update found (USB or server)"})
                }
            }
            // Rollback is available from either source: the USB's v2-fallback
            // build, or the locally-cached previous OTA build.
            "rollback_status" => {
                let usb = updater::find_usb_fallback();
                let ota = updater::find_net_fallback();
                let version = usb.as_ref().or(ota.as_ref()).map(|b| b.version.clone());
                json!({"available": usb.is_some() || ota.is_some(), "version": version})
            }
            "rollback" => match updater::find_usb_fallback().or_else(updater::find_net_fallback) {
                Some(b) => {
                    log_line(format!("> rolling back -> v{}", b.version));
                    match updater::apply(&b.package) {
                        Ok(()) => json!({"ok": true, "error": null}),
                        Err(e) => json!({"ok": false, "error": e.to_string()}),
                    }
                }
                None => json!({"ok": false, "error": "no previous build to roll back to"}),
            },
            other => json!({"error": format!("unknown method: {other}")}),
        }
    }
}

// --- actor -----------------------------------------------------------------
struct Request {
    method: String,
    args: Value,
    reply: SyncSender<String>,
}
static ENGINE: OnceLock<Mutex<Option<SyncSender<Request>>>> = OnceLock::new();

/// The channel to the engine thread, or `None` if that thread could not be
/// started. Failing to spawn is survivable — the disguise still runs and every
/// request answers with a clean error — so it must not be an `.expect()` that
/// aborts the process.
fn actor() -> Option<SyncSender<Request>> {
    let slot = ENGINE.get_or_init(|| {
        let (tx, rx) = sync_channel::<Request>(8);
        match std::thread::Builder::new()
            .name("dreamnail-engine".into())
            .spawn(move || engine_loop(rx))
        {
            Ok(_) => Mutex::new(Some(tx)),
            Err(_) => Mutex::new(None),
        }
    });
    lock_or_recover(slot).clone()
}

/// Strip `com.apple.quarantine` from our own `.app` bundle so the bundled xray
/// and tun-helper aren't Gatekeeper-blocked when spawned. No-op off macOS.
#[cfg(target_os = "macos")]
fn clear_own_quarantine() {
    if let Ok(exe) = std::env::current_exe() {
        // exe = <App>.app/Contents/MacOS/<bin>; the bundle root is 3 levels up.
        if let Some(bundle) = exe.ancestors().nth(3) {
            let _ = std::process::Command::new("/usr/bin/xattr")
                .args(["-dr", "com.apple.quarantine"])
                .arg(bundle)
                .status();
        }
    }
}
#[cfg(not(target_os = "macos"))]
fn clear_own_quarantine() {}

fn engine_loop(rx: Receiver<Request>) {
    let tunnel = match RusshTunnel::new(TunnelConfig::default()) {
        Ok(t) => t,
        Err(_) => {
            for req in rx.iter() {
                let _ = req
                    .reply
                    .send(json!({"error": "engine unavailable"}).to_string());
            }
            return;
        }
    };
    let engine = Engine::new(
        Box::new(tunnel),
        Box::new(XrayProcess::default()),
        // ModalProxy picks stealth (TUN) or standard (system proxy) per the
        // stored setting, falling back to standard if stealth can't start.
        Box::new(ModalProxy::default()),
        Box::new(HttpRouter),
        netcfg::EXPECTED_EXIT_IP,
    );
    let mut backend = Backend {
        engine,
        store: None,
        creds: None,
    };

    // macOS Gatekeeper: a bundle unzipped from a downloaded .dmg carries
    // com.apple.quarantine on EVERY binary inside it. Right-click→Open clears it
    // only for the app's main executable, not the sibling xray / tun-helper — so
    // those get silently killed when we spawn them (stealth's TUN never comes up).
    // Strip it from our whole bundle, once, at startup. Best-effort + user-level.
    clear_own_quarantine();

    // If the last session crashed/was killed while connected, the OS proxy is
    // stranded — clear it before servicing requests so the user isn't offline.
    recover_stranded_proxy();
    log_line(format!(
        "> engine ready — dreamnail v{}",
        netcfg::APP_VERSION
    ));

    // Counts 3s watchdog ticks so periodic upkeep (the macOS proxy keepalive) can
    // run on a coarser cadence than the tick itself.
    let mut tick: u32 = 0;
    // Last throughput sample: (uploaded, downloaded, when). The counters are
    // monotonic and zeroed at connect, so a saturating delta over the real elapsed
    // time gives the rate without any special-casing around a reset.
    let mut last_bytes = (0u64, 0u64);
    let mut last_at = std::time::Instant::now();
    // Separate tick counter for the untrusted-network watcher (runs whatever the
    // connection state; `tick` above only advances while connected).
    let mut net_tick: u32 = 0;
    loop {
        match rx.recv_timeout(Duration::from_secs(3)) {
            Ok(req) => {
                let out = backend.handle(&req.method, &req.args);
                let _ = req.reply.send(out.to_string());
            }
            Err(RecvTimeoutError::Timeout) => {
                // Watchdog: reconnect a dropped link, keep shared status fresh.
                let prev = STATE.load(Ordering::SeqCst);
                let recovered_before = backend.engine.recoveries();
                if let Err(e) = backend.engine.poll() {
                    // The link dropped and the one-shot recovery failed. Say WHY
                    // on screen — before, the UI just flipped to disconnected.
                    log_line(format!("> reconnect failed: {e}"));
                    set_error(Some(format!(
                        "Connection lost and couldn't reconnect. {}",
                        friendly_error(&e)
                    )));
                }
                set_state(backend.engine.state());
                if backend.engine.recoveries() != recovered_before {
                    // A drop + successful recovery happens inside ONE poll, so the
                    // state never visibly leaves Connected — log it here instead.
                    // Stealth may have been re-launched (new TUN name) or fallen
                    // back to the system proxy, so re-derive our own interfaces.
                    log_line("> link dropped — reconnected");
                    if tun_routing_selected() && routing_fell_back() {
                        log_line("> stealth could not restart — using standard routing");
                    }
                    refresh_own_ifaces();
                    spawn_conflict_check();
                }
                let now = STATE.load(Ordering::SeqCst);
                if now != prev {
                    // Only log watchdog-driven transitions (manual connect/
                    // disconnect are logged in their handlers). 2=Connected,
                    // 3=Reconnecting, 0=Disconnected.
                    match now {
                        3 => log_line("> link dropped — reconnecting"),
                        2 if prev == 3 => log_line("> reconnected"),
                        0 if prev != 0 => log_line("> connection lost"),
                        _ => {}
                    }
                }
                // Link lost for good (watchdog gave up): leave a receipt, exactly
                // like a manual disconnect, before the counters are zeroed.
                if now == 0 && prev != 0 {
                    let drops = backend
                        .engine
                        .recoveries()
                        .saturating_sub(SESSION_RECOVERIES_BASE.load(Ordering::SeqCst));
                    record_session_receipt(drops);
                    backend.commit_session_usage(
                        SESSION_DOWN.load(Ordering::SeqCst),
                        SESSION_UP.load(Ordering::SeqCst),
                    );
                    // The one-shot recovery just gave up on an UNEXPECTED drop
                    // (the engine still holds the creds). Arm the backoff loop so
                    // a wake / Wi-Fi switch / server blip comes back on its own,
                    // and suppress the "couldn't reconnect" error while it tries.
                    if backend.auto_reconnect_enabled() && backend.engine.can_retry() {
                        arm_auto_retry();
                        set_error(None);
                        log_line("> connection dropped — reconnecting automatically…");
                    }
                }
                if backend.engine.state() != State::Connected {
                    CONNECTED_AT.store(0, Ordering::SeqCst);
                    PING_MS.store(0, Ordering::SeqCst);
                    DOWN_BPS.store(0, Ordering::SeqCst);
                    UP_BPS.store(0, Ordering::SeqCst);
                    last_bytes = (0, 0);
                    last_at = std::time::Instant::now();
                } else {
                    // Healthy and connected — nothing to auto-reconnect.
                    clear_auto_retry();
                    // Live throughput: what actually crossed the tunnel since the
                    // last tick. These used to be hardcoded zeros, so the speed
                    // graph drew a flat line no matter what was happening.
                    let (up, down) = traffic::totals();
                    let secs = last_at.elapsed().as_secs_f64().max(0.001);
                    UP_BPS.store(
                        (up.saturating_sub(last_bytes.0) as f64 / secs) as u64,
                        Ordering::SeqCst,
                    );
                    DOWN_BPS.store(
                        (down.saturating_sub(last_bytes.1) as f64 / secs) as u64,
                        Ordering::SeqCst,
                    );
                    SESSION_UP.store(up, Ordering::SeqCst);
                    SESSION_DOWN.store(down, Ordering::SeqCst);
                    last_bytes = (up, down);
                    last_at = std::time::Instant::now();
                    // Refresh the ping OFF this thread so a slow probe never
                    // blocks the actor (which also services connect/disconnect).
                    std::thread::spawn(|| PING_MS.store(measure_ping(), Ordering::SeqCst));
                    // ~every 30s, re-assert the macOS system proxy in case it
                    // drifted off while the tunnel stayed up (no-op elsewhere).
                    tick = tick.wrapping_add(1);
                    if tick.is_multiple_of(10) {
                        reassert_system_proxy_macos();
                        // Live warning: notice a VPN/proxy that comes up WHILE
                        // we're connected (off-thread; skipped if one's running).
                        spawn_conflict_check();
                    }
                }
                // Persistent auto-reconnect: while a retry is armed, attempt it on
                // the backoff schedule until the link is back or the user takes
                // over. This is what turns a wake / Wi-Fi switch (where the first
                // attempt fails because the network isn't ready yet) into a
                // self-healing reconnection instead of a dead session.
                if auto_retry_armed() {
                    if !backend.auto_reconnect_enabled() || !backend.engine.can_retry() {
                        // Toggled off mid-wait, or the user disconnected — stop.
                        clear_auto_retry();
                    } else if now_secs() >= AUTO_RETRY_AT.load(Ordering::SeqCst) {
                        match backend.engine.retry() {
                            Ok(true) => {
                                clear_auto_retry();
                                // Re-run the "session established" bookkeeping the
                                // connect handler does, so totals/signal/receipt
                                // behave exactly like a fresh connect.
                                CONNECTED_AT.store(now_secs(), Ordering::SeqCst);
                                SESSION_RECOVERIES_BASE
                                    .store(backend.engine.recoveries(), Ordering::SeqCst);
                                *lock_or_recover(last_session_slot()) = None;
                                last_bytes = (0, 0);
                                last_at = std::time::Instant::now();
                                mark_proxy_active();
                                refresh_own_ifaces();
                                set_state(backend.engine.state());
                                std::thread::spawn(|| {
                                    PING_MS.store(measure_ping(), Ordering::SeqCst)
                                });
                                log_line("> auto-reconnected");
                                spawn_conflict_check();
                                // Re-arm the check-in in case the drop had fully
                                // stopped it (no-op if it's still running).
                                if let Some((u, p)) = backend.creds.clone() {
                                    spawn_checkin(u, p);
                                }
                            }
                            Ok(false) => clear_auto_retry(),
                            Err(e) => {
                                bump_auto_retry();
                                let n = RECONNECT_ATTEMPT.load(Ordering::SeqCst);
                                let wait = AUTO_RETRY_BACKOFF.load(Ordering::SeqCst);
                                log_line(format!(
                                    "> auto-reconnect attempt {n} failed ({}) — retrying in {wait}s",
                                    friendly_error(&e)
                                ));
                            }
                        }
                    }
                }
                net_tick = net_tick.wrapping_add(1);
                if net_tick.is_multiple_of(AUTONET_EVERY_TICKS) {
                    backend.autonet_tick();
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

// --- request dispatch (fast-path for status/proxy_status/health) -----------
fn status_json() -> Value {
    let st = STATE.load(Ordering::SeqCst);
    let connected = st == 2;
    let uptime = if connected {
        now_secs().saturating_sub(CONNECTED_AT.load(Ordering::SeqCst))
    } else {
        0
    };
    let ping = PING_MS.load(Ordering::SeqCst);
    // Between backoff attempts the engine sits in Disconnected, but from the
    // user's view we're still "Reconnecting…" — so a pending auto-retry counts as
    // connecting and carries its attempt number.
    let retry_attempt = RECONNECT_ATTEMPT.load(Ordering::SeqCst);
    let retrying = retry_attempt > 0;
    json!({
        "connected": connected,
        "connecting": st == 1 || st == 3 || retrying,
        "uptime_seconds": uptime,
        "download_bps": if connected { DOWN_BPS.load(Ordering::SeqCst) } else { 0 },
        "upload_bps": if connected { UP_BPS.load(Ordering::SeqCst) } else { 0 },
        "session_down_bytes": if connected { SESSION_DOWN.load(Ordering::SeqCst) } else { 0 },
        "session_up_bytes": if connected { SESSION_UP.load(Ordering::SeqCst) } else { 0 },
        // Data cap (0 = no cap) + usage this cycle = committed past sessions plus
        // the live session's bytes. Drives the sticky-note on the VPN screen.
        "data_limit_bytes": DATA_LIMIT_BYTES.load(Ordering::SeqCst),
        "data_used_bytes": USED_COMMITTED.load(Ordering::SeqCst)
            + if connected {
                SESSION_DOWN.load(Ordering::SeqCst) + SESSION_UP.load(Ordering::SeqCst)
            } else {
                0
            },
        // Plan expiry (ISO yyyy-mm-dd, "" = none), synced from the server or set
        // manually — drives the renewal banner live.
        "plan_expiry": get_plan_expiry(),
        // For the "My Plan" panel: account status + when the plan was last
        // synced from the server (epoch secs, 0 = never synced on this device).
        "account_enabled": ACCOUNT_ENABLED.load(Ordering::SeqCst),
        "plan_synced_at": PLAN_SYNCED_AT.load(Ordering::SeqCst),
        // Remote-control: the client is older than the operator's required
        // minimum version → the UI shows an "update required" prompt with a
        // download link + optional message. (revoke needs no UI: the app wipes
        // itself and exits on the actor's next tick.)
        "update_required": UPDATE_REQUIRED.load(Ordering::SeqCst),
        "update_url": lock_or_recover(update_url_slot()).clone(),
        "update_message": lock_or_recover(update_msg_slot()).clone(),
        // Real RTT to the server while connected (drives the signal/quality
        // indicator); null until the first probe lands or when disconnected.
        "ping_ms": if connected && ping > 0 { Some(ping) } else { None::<u32> },
        "last_error": get_error(),
        // Receipt for the last connection, shown after disconnect; null while
        // connected or before any session has ended.
        "last_session": if connected {
            Value::Null
        } else {
            lock_or_recover(last_session_slot()).clone().unwrap_or(Value::Null)
        },
        "reconnect_attempt": if retrying { Some(retry_attempt) } else { None::<u32> },
        // Other VPNs / system proxies that may conflict (latest check).
        "vpn_conflicts": findings_json(&lock_or_recover(conflicts_slot())),
    })
}

/// TCP round-trip to the server as a latency signal — on the port the tunnel
/// actually uses (22, or 443 when the backup door is in use because this
/// network blocks 22). Returns 0 on failure/timeout. Cheap and root-free.
fn measure_ping() -> u32 {
    use std::net::{TcpStream, ToSocketAddrs};
    let addr = format!("{}:{}", netcfg::EXPECTED_EXIT_IP, dreamnail_plat::tunnel_port());
    let sa = match addr.to_socket_addrs().ok().and_then(|mut it| it.next()) {
        Some(sa) => sa,
        None => return 0,
    };
    let start = std::time::Instant::now();
    match TcpStream::connect_timeout(&sa, Duration::from_secs(3)) {
        Ok(_) => start.elapsed().as_millis().min(u32::MAX as u128).max(1) as u32,
        Err(_) => 0,
    }
}

fn proxy_status_json() -> Value {
    let connected = STATE.load(Ordering::SeqCst) == 2;
    // stealth requested, but fell back to the system proxy (no admin/root)?
    let fell_back = tun_routing_selected() && routing_fell_back();
    let routing_mode = if tun_routing_selected() && !fell_back {
        "tun"
    } else {
        "proxy"
    };
    json!({
        "proxy_set": connected,
        "proxy_applied": connected,
        "proxy_method": routing_mode,
        "verified_ip": if connected { Some(netcfg::EXPECTED_EXIT_IP) } else { None },
        "expected_ip": netcfg::EXPECTED_EXIT_IP,
        "verified": connected,
        // stealth surfacing for the UI:
        "routing_mode": routing_mode,
        "stealth_fell_back": fell_back,
    })
}

fn leak_test_json() -> Value {
    let connected = STATE.load(Ordering::SeqCst) == 2;
    let tun = connected && tun_routing_selected() && !routing_fell_back();
    // Standard mode: our proxy is in effect unless the conflict detector sees a
    // DIFFERENT system proxy configured (it never reports our own endpoint).
    let foreign_proxy = lock_or_recover(conflicts_slot())
        .iter()
        .any(|f| f.kind.as_str() == "proxy");
    let checks = diag::run_all(diag::Session {
        connected,
        tun,
        proxy_applied: connected && !tun && !foreign_proxy,
    });
    log_line("> leak test run");
    json!({
        "connected": connected,
        "tun": tun,
        "checks": checks
            .iter()
            .map(|c| json!({
                "key": c.key,
                "title": c.title,
                "level": c.level.as_str(),
                "detail": c.detail,
            }))
            .collect::<Vec<_>>(),
    })
}

/// Dispatch a `{method, args}` request to JSON. Public so flutter_rust_bridge or
/// tests can call it directly.
pub fn request(method: &str, args: Value) -> String {
    match method {
        "health" => json!({"ok": true}).to_string(),
        "status" => status_json().to_string(),
        "proxy_status" => proxy_status_json().to_string(),
        // Runs on the caller's (background-isolate) thread, never queued behind
        // a long connect on the engine actor.
        "check_conflicts" => json!({"conflicts": findings_json(&run_conflict_check())}).to_string(),
        // The network we're on, for "trust this network" in Settings.
        "current_network" => match netid::current() {
            Some(n) => json!({"id": n.id, "label": n.label, "kind": n.kind}),
            None => json!({"id": null}),
        }
        .to_string(),
        // One-click leak test (several seconds of network probes — always called
        // from the background isolate, never queued behind a connect).
        "leak_test" => leak_test_json().to_string(),
        // Boss key on Wayland desktops (see plat::shortcut).
        "boss_shortcut_status" => {
            let trigger = boss_trigger();
            let needed = shortcut::needed();
            json!({
                "needed": needed,
                "desktop": shortcut::desktop().as_str(),
                "can_auto": needed && shortcut::can_auto(),
                "installed": needed && shortcut::installed(&trigger),
                "command": shortcut::manual_command(&trigger),
            })
            .to_string()
        }
        "install_boss_shortcut" => match shortcut::install(&boss_trigger()) {
            Ok(()) => {
                log_line("> quick-hide shortcut added to the desktop (Ctrl+Alt+Esc)");
                json!({"success": true, "error": null})
            }
            Err(e) => json!({"success": false, "error": e}),
        }
        .to_string(),
        "remove_boss_shortcut" => match shortcut::remove(&boss_trigger()) {
            Ok(()) => json!({"success": true, "error": null}),
            Err(e) => json!({"success": false, "error": e}),
        }
        .to_string(),
        _ => {
            // A disconnect must not queue politely behind the connect it is
            // cancelling: raise the abort flag FIRST, so every slow wait inside
            // the in-flight bring-up (SSH handshake, Xray readiness, and above all
            // the stealth elevation prompt) gives up and frees the engine thread.
            if method == "disconnect" {
                cancel::request_abort();
            }
            let sender = match actor() {
                Some(s) => s,
                None => return json!({"error": "engine unavailable"}).to_string(),
            };
            let (tx, rx) = sync_channel(1);
            let req = Request {
                method: method.to_string(),
                args,
                reply: tx,
            };
            if sender.send(req).is_err() {
                return json!({"error": "engine gone"}).to_string();
            }
            // Bounded, so a wedged step can never hang the caller forever. The
            // budget is generous because a legitimate stealth connect includes
            // human time at the elevation prompt.
            match rx.recv_timeout(Duration::from_secs(180)) {
                Ok(reply) => reply,
                Err(_) => json!({"error": "the local service is busy — try again"}).to_string(),
            }
        }
    }
}

/// Start the engine actor (idempotent).
pub fn init() {
    let _ = actor();
}

// --- C ABI -----------------------------------------------------------------
fn cstr_in(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}
fn cstr_out(s: String) -> *mut c_char {
    CString::new(s).unwrap_or_default().into_raw()
}

/// Start the engine actor.
#[no_mangle]
pub extern "C" fn dn_init() {
    init();
}

/// Handle a request. Input JSON: `{"method": "...", "args": {...}}`. Returns an
/// owned JSON string (free it with dn_free_string).
///
/// # Safety
/// `input` must be NULL or a valid NUL-terminated UTF-8 pointer.
#[no_mangle]
pub unsafe extern "C" fn dn_request(input: *const c_char) -> *mut c_char {
    let raw = cstr_in(input);
    let parsed: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let method = parsed["method"].as_str().unwrap_or("");
    let args = parsed.get("args").cloned().unwrap_or(Value::Null);
    cstr_out(request(method, args))
}

/// Free a string returned by this library.
///
/// # Safety
/// `p` must be NULL or a pointer previously returned by this library, not freed.
#[no_mangle]
pub unsafe extern "C" fn dn_free_string(p: *mut c_char) {
    if !p.is_null() {
        let _ = CString::from_raw(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_is_disconnected_initially() {
        let s = request("status", Value::Null);
        assert!(s.contains("\"connected\":false"));
    }

    #[test]
    fn health_ok() {
        assert!(request("health", Value::Null).contains("\"ok\":true"));
    }

    #[test]
    fn conflict_check_shape_and_status_field() {
        // The UI depends on both: an explicit check, and the latest result
        // riding along in every status poll.
        let c: Value = serde_json::from_str(&request("check_conflicts", Value::Null)).unwrap();
        assert!(c["conflicts"].is_array());
        let s: Value = serde_json::from_str(&request("status", Value::Null)).unwrap();
        assert!(s["vpn_conflicts"].is_array());
    }

    #[test]
    fn unknown_method_errors() {
        assert!(request("nope", json!({})).contains("unknown method"));
    }

    #[test]
    fn settings_defaults_when_locked() {
        let s = request("settings", Value::Null);
        assert!(s.contains("\"timeout_minutes\":5"));
        assert!(s.contains("\"auto_reconnect_enabled\":true"));
    }

    #[test]
    fn transient_matches_reset_and_timeout_across_os() {
        // Linux reset — the exact shape our earlier engine log showed.
        assert!(is_transient_conn_error(
            "tunnel error: IO(Os { code: 104, kind: ConnectionReset, message: \"Connection reset by peer\" })"
        ));
        // macOS reset (os error 54) via Display prose.
        assert!(is_transient_conn_error(
            "tunnel error: connection reset by peer (os error 54)"
        ));
        // Windows reset — localised prose, but kind + WSA code are stable. This is
        // the case the old substring match missed entirely.
        assert!(is_transient_conn_error(
            "tunnel error: IO(Os { code: 10054, kind: ConnectionReset, \
             message: \"An existing connection was forcibly closed by the remote host.\" })"
        ));
        // Windows timeout.
        assert!(is_transient_conn_error(
            "tunnel error: Os { code: 10060, kind: TimedOut, message: \"...\" }"
        ));
        // Our own connect-timeout message + a handshake EOF.
        assert!(is_transient_conn_error("tunnel error: connect timed out"));
        assert!(is_transient_conn_error("tunnel error: kind: UnexpectedEof"));
    }

    #[test]
    fn transient_ignores_real_failures() {
        // Bad credentials must NEVER be retried (would waste time / lock accounts).
        assert!(!is_transient_conn_error("invalid credentials"));
        // A route mismatch is a real, non-transient failure.
        assert!(!is_transient_conn_error(
            "traffic not routing: expected 66.226.144.136, got 9.9.9.9"
        ));
        assert!(!is_transient_conn_error(
            "local service error: empty username → no VLESS id"
        ));
    }
    #[test]
    fn friendly_errors_are_plain_and_never_leak_the_server() {
        use EngineError as E;
        let t = |s: &str| E::Tunnel(s.into());
        let cases: Vec<(EngineError, &str)> = vec![
            (E::Auth, "Wrong username or password"),
            (E::RouteMismatch { expected: "66.226.144.136".into(), got: "9.9.9.9".into() },
                "isn't going through the VPN"),
            // russh Debug text as the adapter wraps it, per OS.
            (t(r#"IO(Os { code: 111, kind: ConnectionRefused, message: "Connection refused" })"#),
                "refused the connection"),
            (t(r#"IO(Os { code: 61, kind: ConnectionRefused, message: "Connection refused" })"#),
                "refused the connection"),
            (t(r#"IO(Os { code: 10061, kind: ConnectionRefused, message: "No connection could be made" })"#),
                "refused the connection"),
            (t(r#"IO(Os { code: 101, kind: NetworkUnreachable, message: "Network is unreachable" })"#),
                "No internet connection"),
            (t(r#"IO(Os { code: 65, kind: HostUnreachable, message: "No route to host" })"#),
                "No internet connection"),
            (t(r#"IO(Os { code: 10051, kind: NetworkUnreachable, message: "unreachable" })"#),
                "No internet connection"),
            (t("connect timed out"), "isn't responding"),
            (t("UnknownKey"), "identity couldn't be verified"),
            (t("Disconnect"), "dropped"),
            (t(r#"IO(Kind(ConnectionReset))"#), "dropped"),
            (t("bind 127.0.0.1:10809: Address already in use (os error 98)"), "network port"),
            (t("something novel"), "Couldn't reach the server"),
            (E::Xray("spawn /opt/Dreamnail/xray: No such file or directory (os error 2)".into()),
                "missing or was blocked"),
            (E::Xray("local service did not become ready (SOCKS never opened)".into()),
                "Couldn't start the secure connection"),
            (E::Proxy("cancelled".into()), "cancelled"),
            (E::Proxy("route probe failed: error sending request".into()),
                "couldn't confirm your traffic"),
            (E::Proxy("no supported desktop proxy backend (need GNOME gsettings or KDE kwriteconfig)".into()),
                "WHOLE SYSTEM"),
            (E::Proxy("failed to write WinINET registry".into()), "network settings"),
        ];
        for (err, want) in cases {
            let got = friendly_error(&err);
            assert!(got.contains(want), "{err:?} → {got:?}, wanted {want:?}");
            // Nothing technical or identifying may reach the screen.
            for bad in ["66.226", "127.0.0.1", "os error", "Os {", "/opt", "error:"] {
                assert!(!got.contains(bad), "{err:?} leaked {bad:?} in {got:?}");
            }
        }
    }

    #[test]
    fn session_receipt_summarises_a_real_session_only() {
        // No receipt for a session that never connected (CONNECTED_AT == 0).
        CONNECTED_AT.store(0, Ordering::SeqCst);
        *lock_or_recover(last_session_slot()) = None;
        record_session_receipt(0);
        assert!(lock_or_recover(last_session_slot()).is_none());

        // A real session: totals add up and drops carry through.
        CONNECTED_AT.store(now_secs().saturating_sub(120), Ordering::SeqCst);
        SESSION_DOWN.store(3_000, Ordering::SeqCst);
        SESSION_UP.store(1_000, Ordering::SeqCst);
        record_session_receipt(2);
        let r = lock_or_recover(last_session_slot()).clone().unwrap();
        assert!(r["duration_seconds"].as_u64().unwrap() >= 120);
        assert_eq!(r["down_bytes"], 3_000);
        assert_eq!(r["up_bytes"], 1_000);
        assert_eq!(r["total_bytes"], 4_000);
        assert_eq!(r["drops"], 2);

        // Cleared for the next connect.
        CONNECTED_AT.store(0, Ordering::SeqCst);
        *lock_or_recover(last_session_slot()) = None;
    }

    #[test]
    fn report_hint_is_a_closed_set_and_never_leaks() {
        use EngineError as E;
        // Every category/hint must come from these fixed lists — proof that no
        // raw error text (which could hold an IP / path / credential) escapes.
        const CATS: &[&str] = &["auth", "tunnel", "xray", "proxy", "route", "other"];
        const HINTS: &[&str] = &[
            "bad_credentials", "exit_mismatch", "unsupported_os", "host_key_mismatch",
            "refused", "no_internet", "port_in_use", "server_timeout", "dropped",
            "unreachable", "binary_missing_or_blocked", "not_ready", "cancelled",
            "verify_failed", "unsupported_desktop", "apply_failed",
        ];
        let samples = [
            E::Auth,
            E::RouteMismatch { expected: "66.226.144.136".into(), got: "9.9.9.9".into() },
            E::Unsupported("x"),
            E::Tunnel("UnknownKey".into()),
            E::Tunnel(r#"IO(Os { code: 111, kind: ConnectionRefused })"#.into()),
            E::Tunnel(r#"IO(Os { code: 101, kind: NetworkUnreachable })"#.into()),
            E::Tunnel("bind 127.0.0.1:10809: Address already in use".into()),
            E::Tunnel("connect timed out".into()),
            E::Tunnel("Disconnect".into()),
            E::Tunnel("mystery".into()),
            E::Xray("spawn /opt/x: No such file".into()),
            E::Xray("not ready".into()),
            E::Proxy("cancelled".into()),
            E::Proxy("route probe failed".into()),
            E::Proxy("no supported desktop proxy backend".into()),
            E::Proxy("failed to write WinINET registry".into()),
        ];
        for e in samples {
            let (c, h) = report_hint(&e);
            assert!(CATS.contains(&c), "category {c:?} not in the closed set");
            assert!(HINTS.contains(&h), "hint {h:?} not in the closed set");
        }
    }
}
