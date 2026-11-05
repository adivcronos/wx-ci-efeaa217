//! Updater — two sources for a newer build, both ending in the SAME trusted
//! install step: a plugged-in USB, or an over-the-air (OTA) download from our
//! own server. Whichever finds a newer build, the bytes are verified against the
//! sha256 the CI manifest published before anything is handed to an installer.
//!
//! USB vs OTA, and the antivirus reality:
//!   The builds are UNSIGNED. An unsigned binary DOWNLOADED from the web gets a
//!   "mark of the web" / quarantine flag, which is exactly what trips Windows
//!   SmartScreen / Defender and macOS Gatekeeper. A file copied from a USB
//!   carries no such flag. So:
//!     - USB  → clean on every OS (preferred where a USB is practical).
//!     - OTA  → clean on Linux (.deb/.AppImage aren't flagged this way); on
//!              Windows/macOS the user still gets an "unknown publisher" warning
//!              until the builds are code-signed. We do NOT strip the web-download
//!              mark or otherwise hide from the scanner — that is malware
//!              behaviour and would put customers at risk. The honest fix for the
//!              Win/mac warning is a code-signing certificate.
//!
//! How either source installs: launch the OS's OWN installer as a VISIBLE wizard
//! that asks for the user's password (UAC on Windows, polkit on Linux, the Finder
//! drag + admin on macOS). A user-authorized GUI install reads as legitimate —
//! far less flagging than a silent background swap, and never a silent one.
//!
//! Trust: neither a USB nor a web server is trusted blindly — a build is only
//! offered if its sha256 matches the hash the CI manifest publishes for it, so
//! the thing handed to an elevated installer is the thing CI actually built. For
//! OTA the manifest itself arrives over HTTPS (TLS-authenticated to our server).
//!
//! Structure stays small + flat: find → verify → apply, plus a version compare.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Diagnostic log line for the update path — off by default, on with
/// `DREAMNAIL_DEBUG=1`. Goes to stderr so the CLI/`dreamnail-cli update` (and a
/// terminal-launched GUI) can show exactly which paths were checked and why an
/// update was or wasn't found. Silent in the shipped GUI (no stderr consumer).
fn ulog(msg: impl AsRef<str>) {
    if std::env::var_os("DREAMNAIL_DEBUG").is_some() {
        eprintln!("[updater] {}", msg.as_ref());
    }
}

/// A build found on removable media.
pub struct UsbBuild {
    pub version: String,
    pub package: PathBuf,
}

/// The manifest key for the running OS (auto-detected at compile time).
pub fn platform_key() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if std::env::var_os("APPIMAGE").is_some() {
        // Running as an AppImage (any distro, incl. Fedora/Nobara where a .deb
        // can't be installed). Pull the AppImage asset and swap it in place —
        // no package manager, no "it just opened the .deb" dead end.
        "linux_appimage"
    } else {
        "linux"
    }
}

/// Directories to look for removable media in, per OS.
fn removable_roots() -> Vec<PathBuf> {
    // Test/QA seam: point the updater at a folder that mimics the USB layout
    // (`<dir>/latest.json` + the package beside it) so the whole find→apply path
    // can be exercised on a dev box with no real USB. Takes precedence when set.
    if let Some(dir) = std::env::var_os("DREAMNAIL_USB_DIR") {
        let p = PathBuf::from(dir);
        ulog(format!("DREAMNAIL_USB_DIR override → {}", p.display()));
        return vec![p];
    }
    if cfg!(target_os = "windows") {
        // Scan drive letters D:..Z: (C: is the system drive).
        (b'D'..=b'Z')
            .map(|c| PathBuf::from(format!("{}:\\", c as char)))
            .collect()
    } else if cfg!(target_os = "macos") {
        children("/Volumes")
    } else {
        // Linux: /run/media/<user>/<label> and /media/[<user>/]<label>.
        let mut roots = Vec::new();
        for base in ["/run/media", "/media"] {
            for child in children(base) {
                roots.extend(children(&child)); // .../<user>/<label>
                roots.push(child); // .../<label>
            }
        }
        roots
    }
}

fn children<P: AsRef<Path>>(dir: P) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/// The build file for THIS OS listed in `<root>/<manifest_dir>/latest.json`, if
/// present on disk. `manifest_dir` is "" for the primary build or "v2-fallback"
/// for the rollback build.
fn build_in(root: &Path, manifest_dir: &str) -> Option<UsbBuild> {
    let dir = if manifest_dir.is_empty() {
        root.to_path_buf()
    } else {
        root.join(manifest_dir)
    };
    let manifest = dir.join("latest.json");
    ulog(format!("checking manifest {}", manifest.display()));
    let text = match std::fs::read_to_string(&manifest) {
        Ok(t) => t,
        Err(e) => {
            ulog(format!("  no manifest ({e})"));
            return None;
        }
    };
    let data: serde_json::Value = match serde_json::from_str(&text) {
        Ok(d) => d,
        Err(e) => {
            ulog(format!("  manifest is not valid JSON ({e})"));
            return None;
        }
    };
    let version = data.get("version")?.as_str()?.to_string();
    let plat = match data.get("platforms").and_then(|p| p.get(platform_key())) {
        Some(p) => p,
        None => {
            ulog(format!(
                "  manifest has no build for this OS ({})",
                platform_key()
            ));
            return None;
        }
    };
    let file = match plat.get("file").and_then(|f| f.as_str()) {
        Some(f) => f,
        None => {
            ulog("  manifest entry has no file name");
            return None;
        }
    };
    let package = dir.join(file);
    ulog(format!(
        "  manifest v{version}, package {}",
        package.display()
    ));
    if !package.is_file() {
        // "Just a .json, nothing happens": the manifest advertises a build but the
        // package isn't actually on the media beside it, so there is nothing to
        // install. Say so loudly rather than silently returning "no update".
        ulog("  package MISSING beside the manifest ✗ — treating as no update");
        return None;
    }
    // The package is about to be handed to an ELEVATED installer, and it came off
    // whatever removable media happened to be plugged in — so it must match the
    // hash CI published, or we don't touch it. An absent hash is a refusal too:
    // every manifest our CI writes carries one (build-v4.yml), so a missing field
    // means the manifest isn't ours.
    let want = match plat.get("sha256").and_then(|h| h.as_str()) {
        Some(h) if !h.trim().is_empty() => h.trim().to_ascii_lowercase(),
        _ => {
            ulog("  manifest has NO sha256 for this package ✗ — refusing to install");
            return None;
        }
    };
    match sha256_file(&package) {
        Some(got) if got == want => {
            ulog("  package present, sha256 verified ✓");
            Some(UsbBuild { version, package })
        }
        Some(got) => {
            ulog(format!(
                "  sha256 MISMATCH ✗ — expected {want}, got {got}; refusing to install"
            ));
            None
        }
        None => {
            ulog("  could not read the package to hash it ✗");
            None
        }
    }
}

/// Lowercase hex SHA-256 of a file, streamed so a large package doesn't have to
/// fit in memory. `None` if it can't be read.
fn sha256_file(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut f, &mut hasher).ok()?;
    Some(format!("{:x}", hasher.finalize()))
}

/// Find a build on any plugged-in USB that is NEWER than `current`. First match
/// wins. Returns None when no USB / no newer build is present (the UI then shows
/// nothing — an update check must never disturb the app).
pub fn find_usb_update(current: &str) -> Option<UsbBuild> {
    ulog(format!("looking for an update newer than v{current}"));
    for root in removable_roots() {
        if let Some(b) = build_in(&root, "") {
            if is_newer(&b.version, current) {
                ulog(format!(
                    "update found: v{} at {}",
                    b.version,
                    b.package.display()
                ));
                return Some(b);
            }
            ulog(format!(
                "  v{} is not newer than v{current} — skipping",
                b.version
            ));
        }
    }
    ulog("no newer build found on any removable media");
    None
}

/// The fallback (previous-line) build on a plugged-in USB — the "go back to old
/// weather" rollback source (`<usb>/v2-fallback/`).
pub fn find_usb_fallback() -> Option<UsbBuild> {
    removable_roots()
        .iter()
        .find_map(|root| build_in(root, "v2-fallback"))
}

// --- Over-the-air (internet) update ----------------------------------------
//
// Same contract as the USB path: only ever offer a build whose sha256 matches
// the CI manifest, and install it through the OS's own visible wizard. The only
// difference is the source — HTTPS to our own server instead of a mounted USB —
// and that the payload has to be downloaded to a local file first.

/// The newest version the OTA manifest advertises for THIS OS, without
/// downloading anything. Cheap enough for a background "is there an update?"
/// check; returns None on any network/parse error (a check must never disturb
/// the app) or when the manifest isn't newer than `current`.
pub fn check_net_update(current: &str) -> Option<String> {
    let (version, _url, _sha) = net_manifest_entry()?;
    if is_newer(&version, current) {
        ulog(format!("OTA: manifest offers v{version} (> v{current})"));
        Some(version)
    } else {
        ulog(format!("OTA: manifest v{version} not newer than v{current}"));
        None
    }
}

/// Download + verify the newer build the OTA manifest names for THIS OS, into a
/// temp file. Returns it ready to hand to `apply()`. None if there is no newer
/// build, or the download/verify fails — and a hash mismatch is always a refusal
/// (same rule as the USB path: never hand unverified bytes to an installer).
pub fn fetch_net_update(current: &str) -> Option<UsbBuild> {
    let (version, url, want) = net_manifest_entry()?;
    if !is_newer(&version, current) {
        ulog(format!("OTA: v{version} not newer than v{current} — skipping"));
        return None;
    }
    let want = want.trim().to_ascii_lowercase();
    if want.is_empty() {
        // Mirrors the USB rule: a manifest entry with no hash is not ours.
        ulog("OTA: manifest entry has NO sha256 ✗ — refusing to download");
        return None;
    }
    let file_name = url.rsplit('/').next().unwrap_or("dreamnail-update");
    let dest = std::env::temp_dir().join(format!(
        "dreamnail-ota-{}-{}",
        std::process::id(),
        file_name
    ));
    let _ = std::fs::remove_file(&dest);
    ulog(format!("OTA: downloading {url} → {}", dest.display()));
    if let Err(e) = download_to(&url, &dest) {
        ulog(format!("OTA: download failed ({e}) ✗"));
        let _ = std::fs::remove_file(&dest);
        return None;
    }
    match sha256_file(&dest) {
        Some(got) if got == want => {
            ulog("OTA: download sha256 verified ✓");
            Some(UsbBuild { version, package: dest })
        }
        Some(got) => {
            ulog(format!(
                "OTA: sha256 MISMATCH ✗ — expected {want}, got {got}; deleting download"
            ));
            let _ = std::fs::remove_file(&dest);
            None
        }
        None => {
            ulog("OTA: could not hash the download ✗");
            let _ = std::fs::remove_file(&dest);
            None
        }
    }
}

/// Parse the OTA manifest for THIS OS → (version, full download url, sha256).
/// On Linux the `.deb` entry is preferred (that's what `linux_install` drives);
/// the AppImage is ignored here so OTA and the installer agree. None on any
/// network or shape error.
fn net_manifest_entry() -> Option<(String, String, String)> {
    let url = net_manifest_url();
    ulog(format!("OTA: fetching manifest {url}"));
    let text = match net_client()?.get(&url).send().and_then(|r| r.error_for_status()).and_then(|r| r.text()) {
        Ok(t) => t,
        Err(e) => {
            ulog(format!("OTA: manifest fetch failed ({e})"));
            return None;
        }
    };
    // The manifest is signed by the operator. A network doing TLS interception
    // could otherwise swap it to point OTA at a malicious installer (the sha256
    // check alone trusts whatever the manifest says). Refuse an unsigned/forged
    // manifest — fail safe to "no update". `DREAMNAIL_SKIP_MANIFEST_SIG=1` is a
    // local-QA escape hatch only (e.g. testing against a hand-written manifest).
    if std::env::var_os("DREAMNAIL_SKIP_MANIFEST_SIG").is_none() && !verify_manifest_sig(&text) {
        ulog("OTA: manifest signature missing or invalid ✗ — refusing update");
        return None;
    }
    parse_net_manifest(&text, platform_key())
}

/// Verify the operator's ed25519 signature over the OTA manifest. Rebuilds the
/// exact canonical string `ship_v4.py` signs (version + base_url + each
/// platform's file:sha256, platforms sorted) and checks the top-level `sig`
/// (hex) against the baked-in operator key. `false` on any shape/verify failure.
fn verify_manifest_sig(text: &str) -> bool {
    let Ok(data) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    let Some(sig_hex) = data.get("sig").and_then(|s| s.as_str()) else {
        return false;
    };
    let Some(sig) = crate::netcfg::hex_decode(sig_hex) else {
        return false;
    };
    crate::netcfg::verify_operator_sig(&manifest_canonical(&data), &sig)
}

/// The exact bytes signed/verified for the OTA manifest. MUST stay byte-for-byte
/// in step with `manifest_canonical` in `tools/ship_v4.py`.
fn manifest_canonical(data: &serde_json::Value) -> Vec<u8> {
    let version = data.get("version").and_then(|v| v.as_str()).unwrap_or("");
    let base = data.get("base_url").and_then(|b| b.as_str()).unwrap_or("");
    let mut lines = vec![
        "dn-man-v1".to_string(),
        format!("version={version}"),
        format!("base_url={base}"),
    ];
    if let Some(plats) = data.get("platforms").and_then(|p| p.as_object()) {
        let mut keys: Vec<&String> = plats.keys().collect();
        keys.sort();
        for k in keys {
            let entry = &plats[k];
            let file = entry.get("file").and_then(|f| f.as_str()).unwrap_or("");
            let sha = entry.get("sha256").and_then(|h| h.as_str()).unwrap_or("");
            lines.push(format!("{k}={file}:{sha}"));
        }
    }
    lines.join("\n").into_bytes()
}

/// Pure parse of an OTA `latest.json` for `plat` → (version, full download url,
/// sha256). Separated from the fetch so it can be tested without a network. The
/// url is `base_url` joined with the platform's `file`.
fn parse_net_manifest(text: &str, plat: &str) -> Option<(String, String, String)> {
    let data: serde_json::Value = serde_json::from_str(text).ok()?;
    let version = data.get("version")?.as_str()?.to_string();
    let base = data.get("base_url").and_then(|b| b.as_str()).unwrap_or("");
    let entry = data.get("platforms")?.get(plat)?;
    let file = entry.get("file").and_then(|f| f.as_str())?;
    let sha = entry.get("sha256").and_then(|h| h.as_str()).unwrap_or("");
    // `base_url` ends with a slash in every manifest ship_v4.py writes; join
    // defensively in case it doesn't.
    let full = if base.is_empty() {
        file.to_string()
    } else if base.ends_with('/') {
        format!("{base}{file}")
    } else {
        format!("{base}/{file}")
    };
    Some((version, full, sha.to_string()))
}

/// The OTA manifest URL — overridable for tests/QA with `DREAMNAIL_UPDATE_URL`
/// (point it at a local file:// or a staging host); the shipped default is the
/// download site's `latest.json`.
fn net_manifest_url() -> String {
    std::env::var("DREAMNAIL_UPDATE_URL")
        .unwrap_or_else(|_| crate::netcfg::UPDATE_MANIFEST_URL.to_string())
}

/// The local SOCKS proxy the running tunnel exposes. When this is listening the
/// VPN is up, so we can send the OTA fetch THROUGH it: the request then exits
/// from our server, not the client's (possibly censored) network. `socks5h` so
/// name resolution also happens server-side.
fn tunnel_proxy_url() -> String {
    format!("socks5h://{}:{}", crate::netcfg::SOCKS_HOST, crate::netcfg::SOCKS_PORT)
}

/// Is the tunnel's local SOCKS proxy accepting connections right now? A quick
/// probe (same cheap check the Xray readiness path uses) — true ≈ VPN connected.
fn tunnel_proxy_up() -> bool {
    use std::net::{SocketAddr, TcpStream};
    let addr: SocketAddr = match format!("{}:{}", crate::netcfg::SOCKS_HOST, crate::netcfg::SOCKS_PORT).parse() {
        Ok(a) => a,
        Err(_) => return false,
    };
    TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(300)).is_ok()
}

/// Whether to route the OTA fetch through the tunnel. Yes whenever the VPN is up,
/// so updates survive a network that blocks our server IP directly (e.g. a
/// Zscaler/corporate filter that categorises it as a proxy) — the fetch rides the
/// tunnel and exits from the server. `DREAMNAIL_OTA_DIRECT=1` forces a direct
/// fetch (QA / bootstrapping before the first connect).
fn ota_via_tunnel() -> bool {
    if std::env::var("DREAMNAIL_OTA_DIRECT").map(|v| v == "1").unwrap_or(false) {
        return false;
    }
    tunnel_proxy_up()
}

/// A blocking HTTP client with sane timeouts for the update path. When the tunnel
/// is up the client is pointed at the local SOCKS proxy, so both the manifest
/// fetch and the installer download go through the VPN (censorship-resistant);
/// otherwise it fetches directly. None if the client can't be built (shouldn't
/// happen with the features we ship).
fn net_client() -> Option<reqwest::blocking::Client> {
    let mut builder = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(300)) // installers are tens of MB
        .user_agent("Dreamnail-Updater");
    if ota_via_tunnel() {
        match reqwest::Proxy::all(tunnel_proxy_url()) {
            Ok(proxy) => {
                ulog("OTA: routing through the VPN tunnel (SOCKS) — bypasses IP-level blocks");
                builder = builder.proxy(proxy);
            }
            // Fall back to a direct fetch rather than failing the update entirely.
            Err(e) => ulog(format!("OTA: tunnel proxy unusable ({e}) — fetching directly")),
        }
    } else {
        ulog("OTA: tunnel down — fetching directly");
    }
    builder
        .build()
        .map_err(|e| ulog(format!("OTA: client build failed ({e})")))
        .ok()
}

/// Stream an HTTPS download straight to `dest` (not held in memory — installers
/// are tens of MB). The TLS layer authenticates the server, and the caller
/// verifies the sha256 before the file is ever handed to an installer.
fn download_to(url: &str, dest: &Path) -> std::io::Result<()> {
    let client = net_client().ok_or_else(|| std::io::Error::other("no http client"))?;
    let mut resp = client
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(std::io::Error::other)?;
    let mut out = std::fs::File::create(dest)?;
    std::io::copy(&mut resp, &mut out)?;
    out.sync_all()?;
    Ok(())
}

// --- OTA rollback ("go back to the old app" after a bad update) -------------
//
// The USB path already has a rollback (the `v2-fallback/` build on the stick).
// OTA needs its own, because a customer who updated over the internet may not
// have a USB. So we keep the INSTALLER locally in a tiny two-slot cache under
// the app data dir:
//     <data>/updates/current/   the build we last installed via OTA (+version)
//     <data>/updates/previous/  the one before it  ← the rollback target
// On each OTA install we rotate current → previous and drop the new installer
// into current. "Roll back" then just re-runs the `previous` installer through
// the same visible wizard. One level deep, which covers the real case: "that
// last update broke it, put the old one back."

fn updates_cache() -> PathBuf {
    crate::xray::data_dir().join("updates")
}

fn slot_build(slot: &str) -> Option<UsbBuild> {
    let dir = updates_cache().join(slot);
    let version = std::fs::read_to_string(dir.join("version.txt")).ok()?;
    let version = version.trim().to_string();
    if version.is_empty() {
        return None;
    }
    // The installer is the one non-metadata file in the slot.
    let package = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_file() && p.file_name().is_some_and(|n| n != "version.txt"))?;
    Some(UsbBuild { version, package })
}

/// Record the build we are about to install via OTA as the new rollback point:
/// rotate the previous `current` into `previous`, then stash `installer` (the
/// just-downloaded, hash-verified package) as the new `current`. Best-effort —
/// a failure here must never block the actual update, only forgo the safety net,
/// so the caller logs the error and proceeds. Call it right before `apply()`.
pub fn save_rollback_point(installer: &Path, new_version: &str) -> std::io::Result<()> {
    let base = updates_cache();
    let current = base.join("current");
    let previous = base.join("previous");
    // Rotate current → previous (drop the older previous first).
    if current.join("version.txt").is_file() {
        let _ = std::fs::remove_dir_all(&previous);
        std::fs::create_dir_all(&base)?;
        // rename is atomic within the same dir; fall back to copy across FSes.
        if std::fs::rename(&current, &previous).is_err() {
            copy_dir(&current, &previous)?;
            let _ = std::fs::remove_dir_all(&current);
        }
    }
    // Write the new current slot.
    std::fs::create_dir_all(&current)?;
    let name = installer
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("installer"));
    std::fs::copy(installer, current.join(&name))?;
    std::fs::write(current.join("version.txt"), new_version.trim())?;
    ulog(format!(
        "OTA: rollback point saved (current=v{new_version}); previous slot now holds the prior build"
    ));
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)?.flatten() {
        let p = entry.path();
        if p.is_file() {
            std::fs::copy(&p, to.join(entry.file_name()))?;
        }
    }
    Ok(())
}

/// The locally-cached previous OTA build, if any — the "go back to the old app"
/// target for a bad OTA update. None until at least two OTA installs have
/// happened (the baseline build the customer first installed by USB/website
/// isn't in the cache; that case still has the USB rollback).
pub fn find_net_fallback() -> Option<UsbBuild> {
    slot_build("previous")
}

/// Launch the OS's own installer as a visible wizard that asks for the user's
/// password. Returns Ok once the wizard is confirmed launched; the caller should
/// then quit so the install can replace the running app.
///
/// We do NOT fire-and-forget: an opener that starts but immediately fails (a
/// missing/unhandleable package, no GUI session, an unmountable .dmg) must be
/// reported as an error, not swallowed — otherwise the app quits on a phantom
/// "success" and the user sees nothing happen. So we spawn the launcher and
/// briefly watch it: a fast non-zero exit is a real failure; a launcher still
/// running after the handoff window has handed off to the wizard = success.
pub fn apply(package: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        // Inno installer WITHOUT /SILENT → the wizard shows; PrivilegesRequired=
        // admin → Windows prompts (UAC). CloseApplications in the .iss shuts the
        // running app so files can be replaced. The package IS the wizard, so it
        // stays running — the watch below only catches an immediate failed start.
        let mut cmd = Command::new(package);
        launch_and_confirm("installer", &mut cmd)
    }
    #[cfg(target_os = "macos")]
    {
        // Opens the .dmg; the user drags Weather to Applications (admin password
        // when replacing). The Finder is the wizard here. `open` returns 0 fast on
        // success, non-zero fast if it cannot open the file — which is exactly the
        // failure that used to read as "nothing happens".
        let mut cmd = Command::new("/usr/bin/open");
        cmd.arg(package);
        launch_and_confirm("open", &mut cmd)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        // Running as an AppImage → swap the file in place and relaunch (no
        // installer at all). Otherwise it's a .deb install via a GUI installer.
        if std::env::var_os("APPIMAGE").is_some() {
            return appimage_swap(package);
        }
        linux_install(package)
    }
}

/// Update an AppImage install: replace the running AppImage with the freshly
/// downloaded one and relaunch. Done by writing a sibling temp then renaming
/// over the target — `rename` is atomic and leaves the *running* process on its
/// original (now-unlinked) inode, so swapping the file underneath it is safe.
/// The caller exits right after, so the relaunched build takes over.
#[cfg(all(unix, not(target_os = "macos")))]
fn appimage_swap(new_pkg: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let cur = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "APPIMAGE not set"))?;
    let dir = cur.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(".dreamnail-update.AppImage.tmp");
    std::fs::copy(new_pkg, &tmp)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&tmp, &cur)?;
    ulog(format!("OTA: AppImage swapped in place → {}", cur.display()));
    // Relaunch detached so the new build survives our imminent exit.
    Command::new(&cur).spawn()?;
    Ok(())
}

/// Install a `.deb` on Linux via a graphical package installer that shows an
/// Install wizard and prompts for the password (polkit). We do NOT rely on
/// `xdg-open` alone: on many desktops a `.deb` is registered to Archive Manager
/// (or nothing), so `xdg-open` opens it as an archive and silently does NOT
/// install. Try the real GUI installers in order, then fall back to `xdg-open`.
#[cfg(all(unix, not(target_os = "macos")))]
fn linux_install(package: &Path) -> std::io::Result<()> {
    let pkg = package.to_string_lossy().into_owned();
    let candidates: [(&str, Vec<String>); 3] = [
        ("gdebi-gtk", vec![pkg.clone()]),
        ("gnome-software", vec![format!("--local-filename={pkg}")]),
        ("plasma-discover", vec![pkg.clone()]),
    ];
    for (bin, args) in &candidates {
        if which(bin) {
            let mut cmd = Command::new(bin);
            cmd.args(args);
            return launch_and_confirm(bin, &mut cmd);
        }
    }
    let mut cmd = Command::new("xdg-open");
    cmd.arg(package);
    launch_and_confirm("xdg-open", &mut cmd)
}

/// Whether `bin` is on PATH (POSIX `command -v`).
#[cfg(all(unix, not(target_os = "macos")))]
fn which(bin: &str) -> bool {
    use std::process::Stdio;
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {bin}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Spawn an installer launcher and confirm it did not immediately fail. Waits up
/// to ~1.5s for a fast error exit (the "nothing happens" cases fail within
/// milliseconds); a launcher still alive after that has handed off to the visible
/// wizard and is treated as success. `label` names the command in the logs.
fn launch_and_confirm(label: &str, cmd: &mut Command) -> std::io::Result<()> {
    use std::time::{Duration, Instant};
    ulog(format!("launching {label} for the update…"));
    let mut child = cmd.spawn().map_err(|e| {
        ulog(format!("{label} failed to start: {e}"));
        e
    })?;
    let deadline = Instant::now() + Duration::from_millis(1500);
    loop {
        match child.try_wait()? {
            Some(status) if status.success() => {
                ulog(format!("{label} exited 0 — installer handed off ✓"));
                return Ok(());
            }
            Some(status) => {
                ulog(format!(
                    "{label} exited with {status} — install did NOT launch ✗"
                ));
                return Err(std::io::Error::other(format!(
                    "{label} could not open the installer (exit {status})"
                )));
            }
            None => {
                if Instant::now() >= deadline {
                    ulog(format!("{label} still running — wizard is up ✓"));
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// '4.10.1' -> (4,10,1); leading digits per part, missing parts pad 0, junk 0.
fn parse_version(v: &str) -> (u32, u32, u32) {
    let mut it = v.split('.').map(|p| {
        p.chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse::<u32>()
            .unwrap_or(0)
    });
    (
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
    )
}

fn is_newer(latest: &str, current: &str) -> bool {
    parse_version(latest) > parse_version(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Interop vector: manifest signed by the operator PRIVATE key with Python's
    // `cryptography` over `manifest_canonical`. Proves the Rust canonical (incl.
    // platform sorting) + ring verification match ship_v4.py byte for byte.
    #[test]
    fn verifies_operator_signed_manifest() {
        // `windows` listed before `linux` on purpose — canonical sorts keys.
        let good = r#"{"version":"5.5.7","base_url":"https://penguinwalker.dev/downloads/v4/","platforms":{"windows":{"file":"b.exe","sha256":"bb"},"linux":{"file":"a.deb","sha256":"aa"}},"sig":"54b3113be11a1274ef2df8eb1b7efee3e328ab5b3590edda450d5489c17f7ba103498d1548de781138f085dfd09e4cd7d2de8327842212a00ad3249a69175c0f"}"#;
        assert!(verify_manifest_sig(good), "operator-signed manifest must verify");
        // Tamper with a sha => signature no longer matches => refuse (fail safe).
        let tampered = good.replace("\"aa\"", "\"deadbeef\"");
        assert!(!verify_manifest_sig(&tampered));
        // No signature at all => refuse.
        let unsigned = r#"{"version":"5.5.7","base_url":"x","platforms":{}}"#;
        assert!(!verify_manifest_sig(unsigned));
    }

    #[test]
    fn ota_proxy_points_at_the_local_tunnel_socks() {
        // socks5h (remote DNS) at the tunnel's own SOCKS inbound, and reqwest
        // must accept it as a proxy URL — so when the VPN is up the OTA fetch
        // exits from the server, not the client's network.
        let url = tunnel_proxy_url();
        assert_eq!(
            url,
            format!("socks5h://{}:{}", crate::netcfg::SOCKS_HOST, crate::netcfg::SOCKS_PORT)
        );
        assert!(reqwest::Proxy::all(&url).is_ok(), "reqwest rejected {url}");
    }

    #[test]
    fn ota_direct_env_forces_a_direct_fetch() {
        // With the override set we never route through the tunnel, regardless of
        // whether a SOCKS proxy happens to be listening.
        std::env::set_var("DREAMNAIL_OTA_DIRECT", "1");
        assert!(!ota_via_tunnel());
        std::env::remove_var("DREAMNAIL_OTA_DIRECT");
    }

    #[test]
    fn newer_compares_numerically_not_lexically() {
        assert!(is_newer("4.10.0", "4.9.0"));
        assert!(is_newer("5.0.0", "4.9.9"));
        assert!(is_newer("4.0.1", "4.0.0"));
        assert!(!is_newer("4.0.0", "4.0.0"));
        assert!(!is_newer("3.9.9", "4.0.0"));
    }

    #[test]
    fn parse_is_total_never_panics() {
        assert_eq!(parse_version("4.0.0-rc1"), (4, 0, 0));
        assert_eq!(parse_version("x.y.z"), (0, 0, 0));
        assert_eq!(parse_version(""), (0, 0, 0));
        assert_eq!(parse_version("4.2"), (4, 2, 0));
    }

    #[test]
    fn platform_key_is_one_of_three() {
        assert!(["windows", "macos", "linux", "linux_appimage"].contains(&platform_key()));
    }

    // --- build_in: parse the CI manifest + verify the package sits beside it ---
    fn scratch_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dn-updtest-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_manifest(dir: &Path, version: &str, file: &str, sha: &str) {
        // Exactly the shape the CI deploy step writes.
        let json = format!(
            r#"{{"version":"{version}","base_url":"https://x/","platforms":{{"linux":{{"file":"{file}","sha256":"{sha}"}},"macos":{{"file":"{file}","sha256":"{sha}"}},"windows":{{"file":"{file}","sha256":"{sha}"}}}}}}"#
        );
        std::fs::write(dir.join("latest.json"), json).unwrap();
    }

    /// Write a package and return its real sha256 — what CI would publish.
    fn write_package(dir: &Path, file: &str, body: &[u8]) -> String {
        std::fs::write(dir.join(file), body).unwrap();
        sha256_file(&dir.join(file)).unwrap()
    }

    #[test]
    fn build_in_finds_manifest_with_package_beside_it() {
        let dir = scratch_dir("with-pkg");
        let file = format!("Dreamnail-4.0.2-{}.pkg", platform_key());
        let sha = write_package(&dir, &file, b"pkg");
        write_manifest(&dir, "4.0.2", &file, &sha);
        let b = build_in(&dir, "").expect("manifest + matching package → Some");
        assert_eq!(b.version, "4.0.2");
        assert_eq!(b.package, dir.join(&file));
    }

    #[test]
    fn build_in_rejects_manifest_without_the_package() {
        // "Just a .json, nothing happens": manifest present, package absent → None.
        let dir = scratch_dir("json-only");
        let file = format!("Dreamnail-4.0.2-{}.pkg", platform_key());
        write_manifest(&dir, "4.0.2", &file, &"0".repeat(64));
        assert!(
            build_in(&dir, "").is_none(),
            "no package beside json → no update"
        );
    }

    #[test]
    fn build_in_rejects_a_package_whose_hash_does_not_match() {
        // The security case: anyone can drop a latest.json + payload on a USB. If
        // the bytes aren't the ones CI published, we must not hand them to an
        // elevated installer.
        let dir = scratch_dir("bad-hash");
        let file = format!("Dreamnail-4.0.2-{}.pkg", platform_key());
        write_package(&dir, &file, b"malicious payload");
        write_manifest(&dir, "4.0.2", &file, &"0".repeat(64)); // not its hash
        assert!(build_in(&dir, "").is_none(), "hash mismatch → refuse");
    }

    #[test]
    fn build_in_rejects_a_manifest_with_no_hash_at_all() {
        let dir = scratch_dir("no-hash");
        let file = format!("Dreamnail-4.0.2-{}.pkg", platform_key());
        write_package(&dir, &file, b"pkg");
        let json = format!(
            r#"{{"version":"4.0.2","platforms":{{"{}":{{"file":"{file}"}}}}}}"#,
            platform_key()
        );
        std::fs::write(dir.join("latest.json"), json).unwrap();
        assert!(build_in(&dir, "").is_none(), "no sha256 → refuse");
    }

    #[test]
    fn sha256_matches_a_known_vector() {
        let dir = scratch_dir("sha");
        let got = write_package(&dir, "abc.bin", b"abc");
        assert_eq!(
            got,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    // --- launch_and_confirm: a failed launch is an error, not a phantom success -
    #[test]
    fn launch_confirm_reports_a_fast_failure() {
        // A launcher that exits non-zero immediately (the macOS `open <bad>` case)
        // must be a real error — this is the bug that read as "nothing happens".
        let mut c = Command::new("sh");
        c.arg("-c").arg("exit 7");
        assert!(launch_and_confirm("false-launcher", &mut c).is_err());
    }

    #[test]
    fn launch_confirm_accepts_a_clean_exit() {
        let mut c = Command::new("sh");
        c.arg("-c").arg("exit 0");
        assert!(launch_and_confirm("ok-launcher", &mut c).is_ok());
    }

    #[test]
    fn launch_confirm_accepts_a_still_running_wizard() {
        // A launcher that stays attached to the wizard (doesn't exit fast) is a
        // success — we must not block on the install finishing.
        let mut c = Command::new("sh");
        c.arg("-c").arg("sleep 5");
        assert!(launch_and_confirm("wizard", &mut c).is_ok());
    }

    // --- OTA: manifest parse (no network) ---
    #[test]
    fn parse_net_manifest_joins_base_url_and_picks_platform() {
        let text = r#"{"version":"5.4.6","base_url":"https://host/dl/v4/",
            "platforms":{
              "linux":{"file":"D-5.4.6-linux.deb","sha256":"aa"},
              "windows":{"file":"D-5.4.6-win.exe","sha256":"bb"},
              "macos":{"file":"D-5.4.6-mac.dmg","sha256":"cc"}}}"#;
        let (v, url, sha) = parse_net_manifest(text, "windows").unwrap();
        assert_eq!(v, "5.4.6");
        assert_eq!(url, "https://host/dl/v4/D-5.4.6-win.exe");
        assert_eq!(sha, "bb");
        // Missing-slash base_url still joins correctly.
        let text2 = text.replace("v4/\"", "v4\"");
        let (_, url2, _) = parse_net_manifest(&text2, "linux").unwrap();
        assert_eq!(url2, "https://host/dl/v4/D-5.4.6-linux.deb");
    }

    #[test]
    fn parse_net_manifest_none_for_unknown_platform_or_junk() {
        let text = r#"{"version":"5.4.6","base_url":"https://h/","platforms":{"linux":{"file":"x","sha256":"a"}}}"#;
        assert!(parse_net_manifest(text, "windows").is_none());
        assert!(parse_net_manifest("not json", "linux").is_none());
    }

    // --- OTA: two-slot rollback cache ---
    #[test]
    fn rollback_rotates_current_into_previous() {
        // Isolate the data dir so this doesn't touch a real install.
        let data = scratch_dir("ota-rollback");
        // SAFETY: single-threaded test; we set a process env for this crate's
        // data_dir() and clear it at the end.
        unsafe { std::env::set_var("DREAMNAIL_DATA_DIR", &data) };

        // Pretend we have two downloaded installers.
        let src = scratch_dir("ota-src");
        let a = src.join("Dreamnail-5.4.6-linux.deb");
        let b = src.join("Dreamnail-5.4.7-linux.deb");
        std::fs::write(&a, b"old").unwrap();
        std::fs::write(&b, b"new").unwrap();

        // No history yet → no fallback.
        assert!(find_net_fallback().is_none());

        // Install A via OTA: current=A, previous empty (first OTA install).
        save_rollback_point(&a, "5.4.6").unwrap();
        assert!(find_net_fallback().is_none(), "first install has no predecessor");

        // Install B: current=B, previous=A → rollback target is A (5.4.6).
        save_rollback_point(&b, "5.4.7").unwrap();
        let fb = find_net_fallback().expect("previous slot now holds A");
        assert_eq!(fb.version, "5.4.6");
        assert_eq!(std::fs::read(&fb.package).unwrap(), b"old");

        unsafe { std::env::remove_var("DREAMNAIL_DATA_DIR") };
    }

    #[test]
    #[ignore = "hits the live download server over the network"]
    fn ota_downloads_and_verifies_from_live_server() {
        let data = scratch_dir("ota-live-data");
        unsafe { std::env::set_var("DREAMNAIL_DATA_DIR", &data) };
        // current = 0.0.0 so the live manifest is always "newer" → forces a real
        // download + sha256 verify against whatever the server advertises.
        let b = fetch_net_update("0.0.0").expect("live server should offer a build");
        println!("OTA downloaded v{} → {}", b.version, b.package.display());
        assert!(b.package.is_file());
        assert!(std::fs::metadata(&b.package).unwrap().len() > 1_000_000);
        // And the rollback cache round-trips the downloaded installer.
        save_rollback_point(&b.package, &b.version).unwrap();
        save_rollback_point(&b.package, &b.version).unwrap(); // second install → previous populated
        assert_eq!(find_net_fallback().unwrap().version, b.version);
        unsafe { std::env::remove_var("DREAMNAIL_DATA_DIR") };
    }

    #[test]
    #[ignore = "needs a USB with a Dreamnail build plugged in"]
    fn finds_real_usb_build() {
        let b = find_usb_update("0.0.0").expect("should find a USB build");
        println!("found v{} at {}", b.version, b.package.display());
        assert!(b.package.exists());
    }
}
