// v4 ApiClient — the SAME surface the v2 UI expects, but backed by the native
// Rust engine in-process (lib/engine.dart) instead of a localhost HTTP backend.
// Every screen/state controller imports this and is unchanged. Weather and the
// update check are plain public-network calls, so they run directly in Dart.
import 'dart:convert';

import 'package:http/http.dart' as http;

import 'engine.dart';
import 'models/weather.dart';

/// Thrown when a request fails. The UI catches this and shows something
/// graceful — never a raw stack trace.
class BackendException implements Exception {
  final String message;
  BackendException(this.message);
  @override
  String toString() => 'BackendException: $message';
}

/// Result of a self-update check.
class UpdateInfo {
  final bool available;
  final String current;
  final String latest;
  final String url;
  const UpdateInfo({
    required this.available,
    required this.current,
    required this.latest,
    this.url = '',
  });
}

class ApiClient {
  final Engine _engine;
  ApiClient(this._engine);

  bool _ok(Map<String, dynamic> m) => m['success'] == true;

  // WHICH CALLS RUN WHERE
  //
  // health/status/proxy_status are answered by the engine from lock-free atomics
  // WITHOUT touching its actor thread, so they are safe to run on the UI isolate
  // and keep the 2s poll instant.
  //
  // Everything else is serviced BY that single actor thread, which a connect can
  // legitimately occupy for a long time (SSH handshake, Xray start-up, and in
  // stealth mode the admin-password prompt). Running those on the UI isolate froze
  // the whole window — pressing Disconnect or opening LOGS mid-connect was exactly
  // the case that hung. They go to a background isolate instead: the queued request
  // still waits its turn, but the UI keeps painting while it does.

  Future<bool> health() async => _engine.requestSync('health')['ok'] == true;

  Future<ConnStatus> status() async =>
      ConnStatus.fromJson(_engine.requestSync('status'));

  Future<ProxyStatus> proxyStatus() async =>
      ProxyStatus.fromJson(_engine.requestSync('proxy_status'));

  Future<bool> unlock(String password) async =>
      _ok(await _engine.requestAsync('unlock', {'password': password}));

  Future<void> lock() async => _engine.requestAsync('lock', const {});

  Future<bool> connect(String username, String password, bool remember) async {
    final r = await _engine.requestAsync('connect', {
      'username': username,
      'password': password,
      'remember': remember,
    });
    if (r['success'] == true) return true;
    // Surface the engine's SPECIFIC reason (invalid credentials / local service
    // error / not routing) so the UI can show something useful instead of a
    // generic "failed". The caller maps it to friendly, IP-free text.
    throw BackendException((r['error'] ?? 'connection failed').toString());
  }

  Future<bool> disconnect() async =>
      _ok(await _engine.requestAsync('disconnect', const {}));

  /// Run the other-VPN/proxy check now (off the UI isolate; answered outside the
  /// engine actor, so it never waits behind a connect).
  Future<List<VpnConflict>> checkConflicts() async => VpnConflict.listFrom(
    (await _engine.requestAsync('check_conflicts', const {}))['conflicts'],
  );

  Future<bool> resetProxy() async =>
      _ok(await _engine.requestAsync('reset_proxy', const {}));

  /// The network this machine is on (`{id, label, kind}`), or null when offline.
  Future<Map<String, dynamic>?> currentNetwork() async {
    final r = await _engine.requestAsync('current_network', const {});
    return r['id'] == null ? null : r;
  }

  /// One-click leak test. Takes a few seconds (real network probes); answered
  /// off the engine actor on the background isolate.
  Future<Map<String, dynamic>> leakTest() async =>
      _engine.requestAsync('leak_test', const {});

  /// Boss-key status on Wayland desktops: `{needed, desktop, installed, command}`.
  Future<Map<String, dynamic>> bossShortcutStatus() async =>
      _engine.requestAsync('boss_shortcut_status', const {});

  /// Add / remove the desktop's Ctrl+Alt+Esc shortcut. `{success, error}`.
  Future<Map<String, dynamic>> installBossShortcut() async =>
      _engine.requestAsync('install_boss_shortcut', const {});
  Future<Map<String, dynamic>> removeBossShortcut() async =>
      _engine.requestAsync('remove_boss_shortcut', const {});

  /// Zero the data-cap usage counter (new billing cycle). Keeps the limit.
  Future<bool> resetUsage() async =>
      _ok(await _engine.requestAsync('reset_usage', const {}));

  /// Send a problem report (customer's message + a diagnostics bundle) to
  /// support. Needs a login we've connected with this run; returns the raw
  /// result so the UI can show the `error` hint when `success` is false.
  Future<Map<String, dynamic>> reportProblem(String message) async =>
      _engine.requestAsync('report_problem', {'message': message});

  Future<Map<String, dynamic>> rememberedLogin() async =>
      _engine.requestAsync('remembered_login', const {});

  Future<List<String>> logs() async {
    final lines = (await _engine.requestAsync('logs', const {}))['lines'];
    return (lines is List) ? lines.map((e) => e.toString()).toList() : const [];
  }

  Future<Map<String, dynamic>> getSettings() async =>
      _engine.requestAsync('settings', const {});

  Future<bool> setSettings({
    int? timeoutMinutes,
    bool? autoReconnectEnabled,
    bool? autoConnect,
    bool? untrustedAutoconnect,
    List<Map<String, String>>? trustedNetworks, // [{id, label}], full list
    String? planExpiry, // ISO yyyy-mm-dd, or '' to clear the reminder
    double? dataLimitGb, // plan data cap in GB, 0 = no cap
    String? theme,
    String? routingMode, // 'proxy' (standard) or 'tun' (stealth)
    bool? adblock, // block ad/tracker domains (next connect)
    bool? force443, // skip port 22, go straight to the 443 doors (next connect)
    bool? weaknet, // weak-network mode: safe MTU + more stripes (next connect)
    String? realityDecoy, // decoy site SNI for the Reality door (next connect)
    bool? errorReports,
  }) async {
    final args = <String, dynamic>{};
    if (timeoutMinutes != null) args['timeout_minutes'] = timeoutMinutes;
    if (autoReconnectEnabled != null) {
      args['auto_reconnect_enabled'] = autoReconnectEnabled;
    }
    if (autoConnect != null) args['auto_connect'] = autoConnect;
    if (untrustedAutoconnect != null) {
      args['untrusted_autoconnect'] = untrustedAutoconnect;
    }
    if (trustedNetworks != null) args['trusted_networks'] = trustedNetworks;
    if (planExpiry != null) args['plan_expiry'] = planExpiry;
    if (dataLimitGb != null) args['data_limit_gb'] = dataLimitGb;
    if (theme != null) args['theme'] = theme;
    if (routingMode != null) args['routing_mode'] = routingMode;
    if (adblock != null) args['adblock'] = adblock;
    if (force443 != null) args['force_443'] = force443;
    if (weaknet != null) args['weaknet'] = weaknet;
    if (realityDecoy != null) args['reality_decoy'] = realityDecoy;
    if (errorReports != null) args['error_reports'] = errorReports;
    return _ok(await _engine.requestAsync('set_settings', args));
  }

  /// Turn "remember admin" (passwordless stealth) on/off. This triggers a REAL
  /// one-time admin prompt, so it can be declined — the result carries the true
  /// resulting state (`enabled`) and an `error` message when it didn't take, and
  /// the caller should sync its toggle to `enabled` rather than to what it asked
  /// for. Runs on the background isolate (the prompt can sit for a while).
  Future<Map<String, dynamic>> setRememberAdmin(bool enabled) async =>
      _engine.requestAsync('set_remember_admin', {'enabled': enabled});

  Future<void> setFocus(bool focused) async =>
      _engine.requestAsync('set_focus', {'focused': focused});

  // --- Weather: a plain public API call, done directly in Dart. -------------
  Future<WeatherData> weather() async {
    try {
      // Melbourne AU (matches the v2 disguise).
      final uri = Uri.parse(
          'https://api.open-meteo.com/v1/forecast?latitude=-37.8136&longitude=144.9631'
          '&current=temperature_2m,apparent_temperature,relative_humidity_2m,is_day,'
          'weather_code,wind_speed_10m'
          '&daily=weather_code,temperature_2m_max,temperature_2m_min,sunrise,sunset,'
          'uv_index_max&timezone=Australia%2FMelbourne&forecast_days=7');
      final resp = await http.get(uri).timeout(const Duration(seconds: 15));
      if (resp.statusCode != 200) {
        throw BackendException('weather HTTP ${resp.statusCode}');
      }
      final j = jsonDecode(resp.body) as Map<String, dynamic>;
      final cur = (j['current'] ?? {}) as Map<String, dynamic>;
      final daily = (j['daily'] ?? {}) as Map<String, dynamic>;
      final code = (cur['weather_code'] as num?)?.toInt() ?? 3;
      final desc = _describe(code);
      return WeatherData.fromJson({
        'city': 'Melbourne',
        'temp_c': cur['temperature_2m'],
        'feels_like_c': cur['apparent_temperature'],
        'condition': desc.condition,
        'icon': desc.icon,
        'emoji': desc.emoji,
        'wind_kph': cur['wind_speed_10m'],
        'humidity': cur['relative_humidity_2m'],
        'uv': (daily['uv_index_max'] as List?)?.firstOrNull,
        'is_day': (cur['is_day'] as num?)?.toInt() == 1,
        'sunrise': _clock((daily['sunrise'] as List?)?.firstOrNull),
        'sunset': _clock((daily['sunset'] as List?)?.firstOrNull),
        'forecast': _forecast(daily),
      });
    } on BackendException {
      rethrow;
    } catch (e) {
      throw BackendException('weather failed: $e');
    }
  }

  List<Map<String, dynamic>> _forecast(Map<String, dynamic> daily) {
    final codes = (daily['weather_code'] as List?) ?? const [];
    final highs = (daily['temperature_2m_max'] as List?) ?? const [];
    final lows = (daily['temperature_2m_min'] as List?) ?? const [];
    final times = (daily['time'] as List?) ?? const [];
    const days = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
    final out = <Map<String, dynamic>>[];
    for (var i = 0; i < codes.length && i < 7; i++) {
      final d = _describe((codes[i] as num).toInt());
      String label = '';
      if (i < times.length) {
        final dt = DateTime.tryParse(times[i].toString());
        if (dt != null) label = days[dt.weekday % 7];
      }
      out.add({
        'day': label,
        'high': i < highs.length ? highs[i] : null,
        'low': i < lows.length ? lows[i] : null,
        'icon': d.icon,
        'emoji': d.emoji,
      });
    }
    return out;
  }

  String _clock(dynamic iso) {
    if (iso == null) return '--:--';
    final dt = DateTime.tryParse(iso.toString());
    if (dt == null) return '--:--';
    String two(int n) => n.toString().padLeft(2, '0');
    return '${two(dt.hour)}:${two(dt.minute)}';
  }

  _Desc _describe(int code) {
    if (code == 0) return const _Desc('Clear', 'clear', '☀️');
    if (code <= 2) return const _Desc('Partly cloudy', 'clear', '🌤️');
    if (code == 3) return const _Desc('Cloudy', 'clouds', '☁️');
    if (code == 45 || code == 48) return const _Desc('Fog', 'clouds', '🌫️');
    if (code >= 51 && code <= 57) return const _Desc('Drizzle', 'rain', '🌦️');
    if (code >= 61 && code <= 67) return const _Desc('Rain', 'rain', '🌧️');
    if (code >= 71 && code <= 77) return const _Desc('Snow', 'snow', '❄️');
    if (code >= 80 && code <= 82) return const _Desc('Showers', 'rain', '🌦️');
    if (code >= 85 && code <= 86) return const _Desc('Snow showers', 'snow', '🌨️');
    if (code >= 95) return const _Desc('Thunderstorm', 'rain', '⛈️');
    return const _Desc('Cloudy', 'clouds', '☁️');
  }

  // --- Self-update: USB-based, handled by the Rust engine (no internet). ------
  // The engine scans a plugged-in USB for a newer build for THIS OS and, on
  // apply, launches the OS's own installer WIZARD (which asks for the password) —
  // a user-authorized install avoids the antivirus flags an unsigned web
  // download would trip. See crates/dreamnail-plat/src/updater.rs.
  Future<UpdateInfo> updateCheck() async {
    final m = await _engine.requestAsync('update_check', const {});
    return UpdateInfo(
      available: m['available'] == true,
      current: (m['current'] ?? '').toString(),
      latest: (m['latest'] ?? '').toString(),
      url: (m['url'] ?? '').toString(),
    );
  }

  /// Launch the OS installer wizard for the USB build. On success the UI should
  /// quit so the installer can replace the running app. Runs off the UI isolate:
  /// apply() now waits briefly to confirm the launcher didn't fail immediately,
  /// so it must not block the UI thread.
  Future<bool> applyUpdate() async =>
      (await _engine.requestAsync('update_apply', const {}))['ok'] == true;

  Future<bool> rollbackAvailable() async =>
      (await _engine.requestAsync('rollback_status', const {}))['available'] == true;

  Future<bool> rollback() async =>
      (await _engine.requestAsync('rollback', const {}))['ok'] == true;
}

class _Desc {
  final String condition;
  final String icon;
  final String emoji;
  const _Desc(this.condition, this.icon, this.emoji);
}
