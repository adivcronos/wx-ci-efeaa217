# Dreamnail v4 — Rust rewrite plan

The engine that is today a headless **Python** backend becomes a native **Rust**
library compiled *into* the Flutter app. The Flutter UI (Dart, already AOT-native)
and the bundled Xray binary stay. This document is the reference we build against.

## Why (the one idea)

Every painful thing about the current app traces to one fact: it unpacks a Python
runtime at launch. That single cause gives us the 45 MB bloat, the slow first
launch, Windows Defender killing it, macOS AMFI killing it, and the fragile
per-OS packaging. Rust's payoff is: **one native binary, nothing unpacked at
runtime, ever.**

North star: **one process, one signed binary, no runtime unpacking.**

## Principles

1. **In-process, not two-process.** The Rust engine links into Flutter via
   `flutter_rust_bridge` — no localhost:8765, no auth token, no "backend won't
   start" fail screen, no single-instance fights. Those bug classes cannot exist.
2. **Can't-crash by construction.** Every fallible call returns `Result`; the
   engine forbids panics (`panic = "abort"`, no `unwrap` in engine code). The
   disguise never dies with a stack trace.
3. **Testable with no server.** The core depends only on TRAITS (ports); real
   I/O is injected. CI tests the whole engine offline with fakes. (Already true:
   12 tests pass with zero dependencies and no network.)
4. **The disguise is sacred.** Named "Weather", no console flashes, no stray
   files, no odd network noise.

## Decisions (locked with the operator)

- Engine as an **in-process FFI library** — yes.
- **Keep the Flutter UI** (it's Dart/AOT-native, never was Python) — yes.
- **Keep spawning the Xray binary** (do not reimplement VLESS) — yes.
- **Incremental migration** (never a big-bang) — yes.
- Native TLS-on-:443 stealth ("looks like HTTPS") — Phase C, not a launch goal.

## What we reuse vs rewrite

| Reuse as-is | Rewrite in Rust |
|---|---|
| Xray binary (VLESS crypto) | SSH tunnel (`russh`) |
| Flutter UI (Dart) | Per-OS system proxy |
| Server + admin app (Python) | Encrypted storage (`age`/`aes-gcm`+`argon2`) |
| UUID-from-username derivation (port byte-identical) | Weather client (`reqwest`) |
| | Watchdog / reconnect, update, orchestration |

## Structure (this workspace)

```
dreamnail-v4/
  crates/
    dreamnail-core/   pure engine: state machine + orchestration over TRAITS.
                      Zero deps. 100% unit-tested with fakes.  [Phase A: DONE]
    dreamnail-plat/   real per-OS adapters for those traits.   [stubs; filling in]
    dreamnail-cli/    headless harness (successor to test_connect.py). [wired]
    dreamnail-ffi/    flutter_rust_bridge glue.                 [Phase B]
```

Core ports (traits): `Tunnel`, `Xray`, `Proxy`, `Router` (route-verify). The
engine names no concrete type, so swapping russh / a proxy backend / a mock is
free.

## Migration — incremental, always shippable

- **Phase A — headless Rust engine.** Core state machine + ports + CLI harness,
  then fill the adapters (russh tunnel, spawn Xray, per-OS proxy, route verify)
  until `dreamnail-cli connect <user> <pass>` brings up a real tunnel and prints
  the verified exit IP. The Python app keeps shipping untouched throughout.
- **Phase B — link into Flutter via FFI.** Replace the Python backend +
  localhost HTTP with in-process calls. Delete port 8765, the token, the fail
  screen, and the backend single-instance logic. This is where the bug classes
  vanish.
  - [x] `dreamnail-ffi` crate (cdylib/staticlib): engine on a dedicated actor
        thread, FFI sends commands over a channel (no Send bounds, no shared-state
        locks). C ABI (`dn_init/dn_connect/dn_disconnect/dn_state/dn_free_string`)
        + plain-Rust `init/connect/disconnect/state` for flutter_rust_bridge to
        wrap. Watchdog ticked on idle.
  - [x] **In-process round-trip proven + debugged** via `dart:ffi`: init → state
        → `connect(dummy)` (→ real server host-key check + auth → "invalid
        credentials") → state → disconnect. Reproducible smoke test in
        `crates/dreamnail-ffi/smoke/`.
  - [x] Dart engine client (`frontend/dreamnail_ui/lib/engine.dart`): dart:ffi
        over the C ABI; `connect` on a background isolate (UI never janks),
        `state`/`disconnect` instant. (flutter_rust_bridge 2.13 is installed to
        layer a generated async API later; the hand-written client is the small,
        debuggable boundary for now.)
  - [x] Minimal v4 Flutter app wired to it (user/pass → Connect → live state →
        Disconnect). `flutter analyze` clean; `flutter build linux --debug`
        succeeds; the cdylib is bundled into `lib/` with `$ORIGIN` rpath, so the
        app is self-contained (no Python, no port 8765, no token, no fail screen).
  - [x] Ported the full v2 weather/VPN UI onto the engine client (screens,
        widgets, models, state, services, theme, assets). api.dart keeps the v2
        ApiClient surface, backed by the FFI. main.dart builds the engine
        in-process (no backend spawn / port / token / fail screen); tray, panic,
        inactivity, weather→gate→vpn nav all preserved. **flutter build linux
        succeeds and the app LAUNCHES** (GTK + tray + engine load, no crash).
  - [x] Weather: direct Open-Meteo call in Dart (public API, Melbourne).
  - [x] Linux cdylib bundling; Windows too (CMake, code-only). macOS bundling
        (Xcode copy-phase) still TODO — needs a Mac to verify.
  - [x] Self-updater — PURE DART (`lib/util/updater.dart`), since the manifest +
        package are public HTTPS (no engine needed): checks /downloads/v4/
        latest.json, streams the per-OS package with sha256 verify, installs via
        the same handoff as v2 Phase 3 (pkexec dpkg / hdiutil ditto / Inno
        silent), keeps `previous` for rollback. `api.dart` delegates to it; the
        already-ported UI (banner/dialog, long-press rollback) now works. 7 tests.
  - [x] build-v4 CI (`.github/workflows/build-v4.yml`) — LINUX shipping. gate
        (cargo test + flutter analyze/test) → prepare (resolve 4.0.0, reset
        `v4-latest`) → build (cargo build --release the cdylib, flutter build
        linux, drop `xray` next to the exe, .deb via scripts/make_linux_deb.sh) →
        deploy to /downloads/v4/ + a manifest of only the platforms that built.
        **v4.0.0 live on the server (2026-09-06), green on first run.** Fixed a
        CMake foot-gun: linux/ + windows/ installed release THEN debug cdylib to
        the same name, so a stale debug lib clobbered release — now debug first,
        release last.
  - [x] Windows (.exe) + macOS (.dmg) legs — DONE, both green on the first CI run
        (2026-09-06). **v4.0.0 now ships on all three OSes** at /downloads/v4/.
        Windows: `scripts/win_installer.iss` (Inno; installs the whole Flutter
        Release folder — no launcher — with MSYS_NO_PATHCONV for ISCC), xray.exe
        dropped next to the exe, DLL bundled by CMake. macOS: engine dylib →
        Contents/Frameworks + xray → Contents/MacOS, signed inside-out (never
        --deep) with the app sealed last under `scripts/macos_app.entitlements`
        (disable-library-validation), drag-to-Applications .dmg; `engine.dart`
        resolves the dylib's full path on macOS (dlopen won't find a bare name in
        a .app). Signing/notarization secret-gated → ad-hoc for now (Windows
        SmartScreen warning; macOS one-time `xattr -cr` — the in-person-USB model
        avoids quarantine anyway). Deploy is resilient (if: always + present-only
        manifest).
  - [x] LIVE CONNECT proven on Linux (2026-09-06) — `dreamnail-cli connect`
        against the real server brought up the russh tunnel + SSH auth → Xray →
        traffic verified exiting via 66.226.144.136 → clean disconnect. Added a
        `DREAMNAIL_NO_PROXY=1` mode (NullProxy) so a live desktop can be tested
        without touching its system proxy. **Fixed a real route-verify bug the
        live test exposed:** `netcfg::PROXY_VERIFY_URL` was `https://ifconfig.me`
        (serves HTML to reqwest) → the exact-match exit-IP compare always failed;
        now `/ip`, and `HttpRouter` extracts the IPv4 token defensively (4 tests).
  - [ ] GUI run (full app, not just the CLI) on a real machine; macOS bundle.
- **Phase C — the upgrades Rust makes easy.** Native VLESS-over-TLS:443 stealth,
  smaller/self-verifying updates, tiny footprint.

## Phase A task list

- [x] Workspace + zero-dep core: state machine, ports, engine, 12 tests green.
- [x] CLI harness wired to the real adapters.
- [x] `RusshTunnel` (russh 0.63): SSH connect + password auth + local
      `10809→10808` forward via per-connection direct-tcpip channels; `alive()`
      = `Handle::is_closed` (keepalives flip it on a dead link). **Verified live**
      against `66.226.144.136:22` up to auth (dummy creds → correct `Auth`
      error). Not yet verified: the forward carrying real VLESS traffic (needs
      valid creds). TODO: pin the server host key (`check_server_key` currently
      accepts any).
- [x] `XrayProcess`: spawns Xray-core with the generated config, polls the SOCKS
      inbound for readiness, `CREATE_NO_WINDOW` on Windows, kills + cleans up the
      temp config on stop. **Verified live** on Linux (real xray → SOCKS :11080
      opened, then freed on stop).
- [x] `SystemProxy`: Linux (GNOME gsettings + KDE kwriteconfig, both set),
      macOS (networksetup on all services), Windows (WinINET registry + refresh),
      each `#[cfg]`-gated. **Linux verified live** (enable → mode=manual/:11080 →
      disable → none). macOS/Windows code-verified only.
- [x] `HttpRouter`: fetches the public IP through the local SOCKS proxy (reqwest,
      no curl/console-flash) for the engine to compare. Not yet live-verified
      end-to-end (needs the full tunnel up).
- [x] UUID-from-username derivation (`uuid_for_username`) + `build_xray_config`,
      **pinned byte-identical** to config.py via Python-computed goldens (tests).
- [x] Encrypted storage (`EncryptedStore`): AES-256-GCM + PBKDF2-HMAC-SHA256
      (200k), password-locked, atomic save. Tests: round-trip, wrong-password
      rejected, tamper detected, no plaintext on disk.
- [x] Host-key pinning in `RusshTunnel::check_server_key` (SHA-256 fingerprint
      vs the pinned server ed25519 key). **Verified live**: with pinning ON the
      connection still reaches auth against the real server (pin matches).
- [ ] `dreamnail-cli connect` proves a real end-to-end tunnel on each OS (needs a
      valid customer credential + reachable server — the one thing the sandbox
      can't do). Everything up to that point is verified.

**Phase A is COMPLETE.** ── Phase B below.

## Honest limits

- Rust does **not** remove Apple's $99 notarization for a stranger to
  double-click on Mac (that's Apple's download check, separate from unpacking).
  It only kills the AMFI/Defender/first-launch problems. The in-person-USB
  distribution model already sidesteps notarization.
- `flutter_rust_bridge` adds some CI build complexity — worth it.
- This is weeks of work — which is exactly why v2.3.0 (Python) was stabilized
  first: it stays the shipping version until v4 is proven.
