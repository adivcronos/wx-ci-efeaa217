import 'dart:async';

import 'package:flutter/foundation.dart' show ChangeNotifier, visibleForTesting;

import '../api.dart';
import '../models/weather.dart';

/// Polls GET /status every 2s and exposes connect/disconnect.
/// Tracks consecutive poll failures so the UI can show a "reconnecting to local
/// service" banner, and — once the backend looks down — actually respawns it
/// via [onRespawn] so a crashed/killed backend self-heals instead of leaving
/// connect permanently stuck on "retrying…".
class ConnectionController extends ChangeNotifier {
  ApiClient api;

  /// Restarts the local backend and repoints [api] at it; returns true on
  /// success. Null in tests that don't exercise recovery.
  final Future<bool> Function()? onRespawn;

  ConnStatus status = ConnStatus.disconnected();

  /// Last GET /proxy_status — drives the green tick vs the "traffic may not be
  /// routing correctly" warning. Only refreshed while a session is up.
  ProxyStatus proxyStatus = ProxyStatus.unknown();

  bool optimisticConnecting = false;

  /// True once a POST /connect has been accepted by the backend. The engine
  /// moves to CONNECTING synchronously inside that call, so from here on the
  /// backend's own status is authoritative — a poll that comes back
  /// *disconnected* means the attempt already settled (e.g. invalid
  /// credentials) and the optimistic spinner must be dropped. Without this the
  /// UI hung on "Connecting…" forever whenever auth failed faster than the 2s
  /// poll could catch the transient CONNECTING state.
  bool _connectAcked = false;

  int consecutiveFailures = 0;
  bool _respawning = false;
  Timer? _timer;

  /// Rolling download-rate history (bytes/sec), oldest first, one sample per
  /// status poll (~2s) capped to the last ~30s — feeds the stats-bar graph.
  static const int _historyLen = 16;
  final List<int> downloadHistory = [];

  ConnectionController(this.api, {this.onRespawn});

  bool get backendDown => consecutiveFailures >= 3;

  /// Connected, the OS proxy is actually applied, AND traffic is confirmed
  /// egressing through the server — the green tick needs all three, since the
  /// through-Xray probe can pass even when the OS proxy never took.
  bool get trafficVerified =>
      status.connected && proxyStatus.proxyApplied && proxyStatus.verified;

  /// Connected but the OS never accepted our proxy (unsupported desktop, blocked
  /// write, no active service) — apps bypass the VPN. The actionable failure:
  /// the user must set the proxy manually. Takes precedence over [routingWarning].
  bool get systemProxyNotApplied =>
      status.connected && proxyStatus.proxySet && !proxyStatus.proxyApplied;

  /// The OS proxy IS applied, but the through-the-proxy IP check did NOT see the
  /// server — the Xray→server path is the problem. "May not be routing" warning.
  bool get routingWarning =>
      status.connected && proxyStatus.proxyApplied && !proxyStatus.verified;

  /// Stealth (TUN) routing is the active mode — every app is routed through the
  /// tunnel with nothing set in the system proxy.
  bool get stealthActive =>
      status.connected && proxyStatus.routingMode == 'tun';

  /// The user asked for stealth but it couldn't start (needs admin/root) and the
  /// app fell back to standard proxy. Surfaced so a user who enabled stealth for
  /// a proxy-rejecting app/game learns WHY it still bypasses the VPN — instead of
  /// seeing the green tick and assuming stealth is on. Takes precedence over the
  /// positive [stealthActive] indicator.
  bool get stealthFellBack => status.connected && proxyStatus.stealthFellBack;

  void _recordDownload(int bps) {
    downloadHistory.add(bps < 0 ? 0 : bps);
    while (downloadHistory.length > _historyLen) {
      downloadHistory.removeAt(0);
    }
  }

  void start({int intervalSeconds = 2}) {
    _timer?.cancel();
    _timer = Timer.periodic(Duration(seconds: intervalSeconds), (_) => _poll());
    _poll();
  }

  /// Stop polling while the window is hidden to the tray.
  ///
  /// The 2s poll otherwise ran forever — including from the weather disguise,
  /// where nothing consumes it — waking the backend, and on macOS shelling out
  /// to networksetup for the proxy-drift check every few seconds.
  void pause() {
    _timer?.cancel();
    _timer = null;
  }

  void resume() {
    if (_timer != null) return; // already polling
    start();
  }

  /// Slow poll to 5s when the window is not focused — keeps reconnect
  /// detection alive but reduces idle wakeups.
  void slowPoll() {
    _timer?.cancel();
    _timer = Timer.periodic(const Duration(seconds: 5), (_) => _poll());
  }

  /// Test hook: run a single status poll (and any recovery it triggers)
  /// without spinning up the periodic timer.
  @visibleForTesting
  Future<void> pollOnceForTest() => _poll();

  /// Pull fresh status now (e.g. right after an on-demand check updated
  /// something the next timed poll would otherwise take a few seconds to show).
  Future<void> refresh() => _poll();

  Future<void> _poll() async {
    try {
      status = await api.status();
      consecutiveFailures = 0;
      _recordDownload(status.connected ? status.downloadBps : 0);
      // Clear the optimistic flag once the backend reports a real state.
      if (status.connected || status.connecting) {
        optimisticConnecting = false;
        _connectAcked = false;
      } else if (_connectAcked) {
        // Connect was accepted but the backend has already settled back to
        // disconnected before a poll caught CONNECTING — the attempt failed
        // (invalid credentials, unreachable server). Drop the spinner so the
        // UI shows the error instead of hanging on "Connecting…".
        optimisticConnecting = false;
        _connectAcked = false;
      }
      // Routing verification is only meaningful while a session is up/coming
      // up; when idle, reset it so a stale tick can't linger.
      if (status.connected || status.connecting) {
        try {
          proxyStatus = await api.proxyStatus();
        } catch (_) {
          // A transient miss shouldn't flip the tick — keep the last reading.
        }
      } else {
        proxyStatus = ProxyStatus.unknown();
      }
    } catch (_) {
      consecutiveFailures++;
      // Backend has gone quiet — bring it back up. The next poll will succeed
      // against the respawned process (same [api], fresh token).
      //
      // But NEVER respawn while a connect is in flight: the backend accepted
      // POST /connect and is establishing the tunnel on a worker thread — that
      // paramiko/Xray work can briefly starve the status poll (TCP accept
      // delayed past the 5s connect timeout) and trip `backendDown`. Respawning
      // here kills the healthy backend mid-connect, so the tunnel dies and the
      // attempt fails — the classic "sometimes connects, sometimes doesn't". A
      // genuinely dead backend still recovers: the connect call itself fails,
      // clears these flags, and the next poll resumes recovery.
      if (backendDown && !optimisticConnecting && !_connectAcked) {
        unawaited(_recoverBackend());
      }
    }
    _safeNotify();
  }

  /// Respawn the backend at most once at a time; leave [consecutiveFailures]
  /// intact on failure so the banner stays up and the next poll retries.
  Future<void> _recoverBackend() async {
    if (_respawning || onRespawn == null) return;
    _respawning = true;
    try {
      if (await onRespawn!()) consecutiveFailures = 0;
    } catch (_) {
      // Still down; a later poll re-attempts recovery.
    } finally {
      _respawning = false;
      _safeNotify();
    }
  }

  /// Set when connect/disconnect itself fails, so the screen can say so.
  ///
  /// These used to be swallowed: the only feedback was `status.lastError`
  /// arriving on a later poll, which requires the backend to still be alive —
  /// exactly what is in doubt when the call has just failed. Cleared on the
  /// next attempt, and by [clearActionError] once shown.
  String? actionError;

  void clearActionError() {
    if (actionError == null) return;
    actionError = null;
    _safeNotify();
  }

  Future<void> connect(String user, String pass, bool remember) async {
    optimisticConnecting = true;
    _connectAcked = false;
    actionError = null;
    _safeNotify();
    try {
      final accepted = await api.connect(user, pass, remember);
      if (!accepted) {
        optimisticConnecting = false;
        actionError = "Couldn't start connecting. Please try again.";
        _safeNotify();
      } else {
        // Backend has entered CONNECTING synchronously; let the next poll own
        // the state so a fast auth failure can clear the spinner.
        _connectAcked = true;
      }
    } catch (e) {
      optimisticConnecting = false;
      _connectAcked = false;
      actionError = e is BackendException
          ? friendlyError(e.message)
          : "Couldn't start connecting. Please try again.";
      _safeNotify();
    }
  }

  /// Last line of defence before an error reaches the screen.
  ///
  /// The engine (v4.4.4+) already sends one plain-English sentence, which is
  /// passed through untouched. This only catches raw engine text — the old
  /// "tunnel error: …" style, or anything carrying an IP address or OS error
  /// code — and swaps it for a generic line, so jargon or the server IP can
  /// never be shown even if a future engine path forgets to translate.
  static String friendlyError(String raw) {
    final m = raw.toLowerCase();
    final looksRaw =
        RegExp(r'\b\d{1,3}(\.\d{1,3}){3}\b').hasMatch(raw) ||
        m.contains('os error') ||
        m.startsWith('tunnel error') ||
        m.startsWith('local service error') ||
        m.startsWith('proxy error') ||
        m.startsWith('traffic not routing') ||
        m.startsWith('not implemented') ||
        m == 'invalid credentials';
    if (!looksRaw) return raw;
    if (m == 'invalid credentials') return 'Wrong username or password.';
    if (m.startsWith('traffic not routing')) {
      return "Connected, but traffic isn't going through the VPN. Please try again.";
    }
    return 'Connection failed. Please try again.';
  }

  Future<void> disconnect() async {
    optimisticConnecting = false;
    actionError = null;
    try {
      await api.disconnect();
    } catch (e) {
      actionError =
          "Couldn't disconnect because the app isn't responding. Restart "
          'Dreamnail to get your normal internet back.';
    }
    _safeNotify();
  }

  /// Re-lock the backend store (panic / inactivity). Best-effort.
  Future<void> lock() => api.lock();

  bool _disposed = false;

  /// notifyListeners() after dispose throws. A poll or respawn already in
  /// flight when the controller goes away would otherwise land on a disposed
  /// notifier — reachable in the real app whenever the window closes while a
  /// request is outstanding.
  void _safeNotify() {
    if (_disposed) return;
    notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _timer?.cancel();
    _timer = null;
    super.dispose();
  }
}
