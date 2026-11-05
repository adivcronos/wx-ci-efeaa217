/// Weather data as returned by the backend's GET /weather adapter.
class WeatherData {
  final String city;
  final num? tempC;
  final num? feelsLikeC;
  final String condition;
  final String icon; // clear | clouds | rain | snow
  final String emoji;
  final num? windKph;
  final num? humidity;
  final num? uv;
  final bool isDay;
  final String sunrise;
  final String sunset;
  final List<ForecastDay> forecast;

  WeatherData({
    required this.city,
    required this.tempC,
    required this.feelsLikeC,
    required this.condition,
    required this.icon,
    required this.emoji,
    required this.windKph,
    required this.humidity,
    required this.uv,
    required this.isDay,
    required this.sunrise,
    required this.sunset,
    required this.forecast,
  });

  factory WeatherData.fromJson(Map<String, dynamic> j) {
    return WeatherData(
      city: (j['city'] ?? 'Melbourne') as String,
      tempC: j['temp_c'] as num?,
      feelsLikeC: j['feels_like_c'] as num?,
      condition: (j['condition'] ?? '') as String,
      icon: (j['icon'] ?? 'clouds') as String,
      emoji: (j['emoji'] ?? '') as String,
      windKph: j['wind_kph'] as num?,
      humidity: j['humidity'] as num?,
      uv: j['uv'] as num?,
      isDay: (j['is_day'] ?? true) as bool,
      sunrise: (j['sunrise'] ?? '--:--') as String,
      sunset: (j['sunset'] ?? '--:--') as String,
      forecast: ((j['forecast'] ?? []) as List)
          .map((e) => ForecastDay.fromJson(e as Map<String, dynamic>))
          .toList(),
    );
  }
}

class ForecastDay {
  final String day;
  final num? high;
  final num? low;
  final String icon;
  final String emoji;

  ForecastDay({
    required this.day,
    required this.high,
    required this.low,
    required this.icon,
    required this.emoji,
  });

  factory ForecastDay.fromJson(Map<String, dynamic> j) => ForecastDay(
    day: (j['day'] ?? '') as String,
    high: j['high'] as num?,
    low: j['low'] as num?,
    icon: (j['icon'] ?? 'clouds') as String,
    emoji: (j['emoji'] ?? '') as String,
  );
}

/// Connection status as returned by GET /status.
/// Another VPN or system proxy the engine detected (read-only OS checks).
/// [capturing] = it is actually redirecting traffic (internet or our server's
/// route goes through it, or it's a system proxy) — worth a confirm before
/// connecting, not just a notice.
class VpnConflict {
  final String name;
  final String kind; // 'vpn' | 'proxy'
  final bool capturing;

  const VpnConflict({required this.name, required this.kind, required this.capturing});

  factory VpnConflict.fromJson(Map<String, dynamic> j) => VpnConflict(
    name: (j['name'] ?? '') as String,
    kind: (j['kind'] ?? 'vpn') as String,
    capturing: (j['capturing'] ?? false) as bool,
  );

  static List<VpnConflict> listFrom(dynamic v) => v is List
      ? v
          .whereType<Map>()
          .map((m) => VpnConflict.fromJson(Map<String, dynamic>.from(m)))
          .toList()
      : const [];
}

/// A one-line summary of the last connection, shown after disconnect.
class SessionReceipt {
  final int durationSeconds;
  final int downBytes;
  final int upBytes;
  final int totalBytes;
  final int drops;

  const SessionReceipt({
    required this.durationSeconds,
    required this.downBytes,
    required this.upBytes,
    required this.totalBytes,
    required this.drops,
  });

  /// Parses the engine's `last_session` object; null when there is none.
  static SessionReceipt? fromJson(dynamic j) {
    if (j is! Map) return null;
    int i(String k) => (j[k] ?? 0) is int ? (j[k] ?? 0) as int : 0;
    return SessionReceipt(
      durationSeconds: i('duration_seconds'),
      downBytes: i('down_bytes'),
      upBytes: i('up_bytes'),
      totalBytes: i('total_bytes'),
      drops: i('drops'),
    );
  }
}

class ConnStatus {
  final bool connected;
  final bool connecting;
  final int uptimeSeconds;
  final int downloadBps;
  final int uploadBps;
  final int sessionDownBytes;
  final int sessionUpBytes;
  final int? pingMs;
  final String? lastError;
  final int? reconnectAttempt;

  /// Data cap (bytes, 0 = no cap) and usage this cycle (committed past sessions
  /// plus the live session). Drive the data sticky-note on the VPN screen.
  final int dataLimitBytes;
  final int dataUsedBytes;

  /// Plan expiry (ISO yyyy-mm-dd, "" = none), synced from the server on connect
  /// or set manually. Drives the renewal banner.
  final String planExpiry;

  /// Account status + when the plan was last synced from the server (epoch
  /// secs, 0 = never). Shown on the "My Plan" panel.
  final bool accountEnabled;
  final int planSyncedAt;

  /// Remote-control: the operator requires a newer app version → show an
  /// "update required" prompt with [updateUrl] and optional [updateMessage].
  final bool updateRequired;
  final String updateUrl;
  final String updateMessage;

  /// Live connect progress (1..4, with [connectStepLabel]) while [connecting].
  /// Null when not mid-connect or when the backend hasn't reached a step yet.
  final int? connectStep;
  final int? connectStepTotal;
  final String? connectStepLabel;

  /// Other VPNs / system proxies from the engine's latest check.
  final List<VpnConflict> vpnConflicts;

  /// Summary of the last connection, present only after a disconnect.
  final SessionReceipt? lastSession;

  ConnStatus({
    required this.connected,
    required this.connecting,
    required this.uptimeSeconds,
    required this.downloadBps,
    required this.uploadBps,
    this.sessionDownBytes = 0,
    this.sessionUpBytes = 0,
    required this.pingMs,
    required this.lastError,
    required this.reconnectAttempt,
    this.dataLimitBytes = 0,
    this.dataUsedBytes = 0,
    this.planExpiry = '',
    this.accountEnabled = true,
    this.planSyncedAt = 0,
    this.updateRequired = false,
    this.updateUrl = '',
    this.updateMessage = '',
    this.connectStep,
    this.connectStepTotal,
    this.connectStepLabel,
    this.vpnConflicts = const [],
    this.lastSession,
  });

  factory ConnStatus.disconnected() => ConnStatus(
    connected: false,
    connecting: false,
    uptimeSeconds: 0,
    downloadBps: 0,
    uploadBps: 0,
    pingMs: null,
    lastError: null,
    reconnectAttempt: null,
  );

  factory ConnStatus.fromJson(Map<String, dynamic> j) => ConnStatus(
    connected: (j['connected'] ?? false) as bool,
    connecting: (j['connecting'] ?? false) as bool,
    uptimeSeconds: (j['uptime_seconds'] ?? 0) as int,
    downloadBps: (j['download_bps'] ?? 0) as int,
    uploadBps: (j['upload_bps'] ?? 0) as int,
    sessionDownBytes: (j['session_down_bytes'] ?? 0) as int,
    sessionUpBytes: (j['session_up_bytes'] ?? 0) as int,
    pingMs: j['ping_ms'] as int?,
    lastError: j['last_error'] as String?,
    reconnectAttempt: j['reconnect_attempt'] as int?,
    dataLimitBytes: (j['data_limit_bytes'] as num?)?.toInt() ?? 0,
    dataUsedBytes: (j['data_used_bytes'] as num?)?.toInt() ?? 0,
    planExpiry: (j['plan_expiry'] as String?) ?? '',
    accountEnabled: (j['account_enabled'] as bool?) ?? true,
    planSyncedAt: (j['plan_synced_at'] as num?)?.toInt() ?? 0,
    updateRequired: (j['update_required'] as bool?) ?? false,
    updateUrl: (j['update_url'] as String?) ?? '',
    updateMessage: (j['update_message'] as String?) ?? '',
    connectStep: j['connect_step'] as int?,
    connectStepTotal: j['connect_step_total'] as int?,
    connectStepLabel: j['connect_step_label'] as String?,
    vpnConflicts: VpnConflict.listFrom(j['vpn_conflicts']),
    lastSession: SessionReceipt.fromJson(j['last_session']),
  );

  /// Human "Step 2/4 Starting local service…", or null when no step is active.
  String? get stepText {
    final label = connectStepLabel;
    if (connectStep == null || label == null) return null;
    final total = connectStepTotal ?? 4;
    return 'Step $connectStep/$total $label';
  }

  /// Whether the customer set a data cap (so the sticky-note should show).
  bool get hasDataCap => dataLimitBytes > 0;

  /// Fraction of the cap used, 0..1 (clamped). 0 when there's no cap.
  double get dataUsedFraction {
    if (dataLimitBytes <= 0) return 0;
    final f = dataUsedBytes / dataLimitBytes;
    return f.isNaN ? 0 : f.clamp(0.0, 1.0);
  }

  /// Usage has reached or passed the cap.
  bool get dataLimitReached => hasDataCap && dataUsedBytes >= dataLimitBytes;
}

/// Proxy routing status as returned by GET /proxy_status.
///
/// Two independent facts drive the UI, so the two failure modes stay distinct:
/// [proxyApplied] is whether the OS actually accepted our proxy (false = the
/// system was never pointed at the VPN, so apps bypass it — the real "connected
/// but not routing" cause), and [verified] is whether traffic checked through
/// Xray reaches the server. The green tick needs BOTH.
class ProxyStatus {
  final bool proxySet;
  final bool proxyApplied;
  final String? proxyMethod;
  final String? verifiedIp;
  final String expectedIp;
  final bool verified;

  /// "tun" (stealth) or "proxy" (standard) — how traffic is being routed.
  final String routingMode;

  /// True when stealth (TUN) was requested but had to fall back to the standard
  /// system proxy (e.g. the app isn't running with admin/root).
  final bool stealthFellBack;

  ProxyStatus({
    required this.proxySet,
    required this.proxyApplied,
    required this.proxyMethod,
    required this.verifiedIp,
    required this.expectedIp,
    required this.verified,
    this.routingMode = 'proxy',
    this.stealthFellBack = false,
  });

  factory ProxyStatus.unknown() => ProxyStatus(
    proxySet: false,
    proxyApplied: false,
    proxyMethod: null,
    verifiedIp: null,
    expectedIp: '',
    verified: false,
  );

  factory ProxyStatus.fromJson(Map<String, dynamic> j) => ProxyStatus(
    proxySet: (j['proxy_set'] ?? false) as bool,
    // Older backends don't send proxy_applied; assume applied when set so we
    // don't nag against a backend that predates this field.
    proxyApplied: (j['proxy_applied'] ?? j['proxy_set'] ?? false) as bool,
    proxyMethod: j['proxy_method'] as String?,
    verifiedIp: j['verified_ip'] as String?,
    expectedIp: (j['expected_ip'] ?? '') as String,
    verified: (j['verified'] ?? false) as bool,
    routingMode: (j['routing_mode'] ?? 'proxy') as String,
    stealthFellBack: (j['stealth_fell_back'] ?? false) as bool,
  );
}
