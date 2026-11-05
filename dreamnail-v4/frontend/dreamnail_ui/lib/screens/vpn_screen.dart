import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:provider/provider.dart';
import 'package:window_manager/window_manager.dart';

import '../api.dart';
import '../models/weather.dart';
import '../state/connection_state.dart';
import '../theme.dart';
import '../util/conn_quality.dart';
import '../util/format.dart';
import '../widgets/dreamnail_strike.dart';
import '../widgets/speed_graph.dart';
import '../widgets/terminal_monitor.dart';
import 'settings_sheet.dart';

/// The revealed VPN control screen: the Dreamnail, status + session timer, a
/// credential card, the connect/disconnect button, and a live speed/ping row.
/// All connection state comes from [ConnectionController] (polls GET /status 2s).
/// Connect/Disconnect first play the nail-strike animation, then fire the
/// unchanged backend call once the swing completes.
///
/// Also handles the screen-local lifecycle gestures: a settings gear (Screen 4),
/// the global panic shortcut (double-Esc itself is app-wide: PanicEscape), Ctrl/Cmd+Shift+W
/// (hide to tray), and an inactivity auto-lock with a last-60s countdown.
class VpnScreen extends StatefulWidget {
  final VoidCallback onBack;
  const VpnScreen({super.key, required this.onBack});

  @override
  State<VpnScreen> createState() => _VpnScreenState();
}

class _VpnScreenState extends State<VpnScreen> {
  final _userCtrl = TextEditingController();
  final _passCtrl = TextEditingController();
  bool _remember = false;
  bool _obscure = true;

  // Local plan-expiry reminder: ISO yyyy-mm-dd the customer set from their
  // welcome message (empty = no reminder). Drives the expiry banner.
  String _planExpiry = '';
  // Auto-connect-after-unlock fires at most once per screen mount.
  bool _autoConnectTried = false;

  // After Disconnect is pressed the backend takes up to one /status poll (~2s)
  // to report `connected: false`. Until then the screen still reads "connected",
  // which made the link monitor flash back up to 100% right after the power-down
  // animation. This latches "offline" immediately and clears once the backend
  // confirms the disconnect (see didChangeDependencies).
  bool _optimisticOff = false;

  // Error banner: show the (plain-English) error until auto-dismissed after 9s.
  String? _trackedError;
  String? _dismissedError;
  Timer? _errTimer;

  // The Dreamnail: strike plays before the backend call; block re-entry
  // while a swing is in flight.
  final _monKey = GlobalKey<TerminalMonitorState>();
  bool _striking = false;

  // Lifecycle gestures.
  final _screenFocus = FocusNode();
  Timer? _idleTimer;
  int _timeoutMinutes = 5;
  int _idleSeconds = 300;
  bool _showCountdown = false;

  // The SAVED routing preference ('proxy' | 'tun'), shown as WEB ONLY vs WHOLE
  // SYSTEM before connecting. While connected the live proxy_status is used
  // instead (it reflects a stealth→standard fallback).
  String _routingMode = 'proxy';

  // Session clock: a stable local anchor + a 1s ticker, so the timer counts up
  // smoothly instead of jumping with the 2s /status poll. Re-anchored only on a
  // big drift (a reconnect resets the backend's uptime).
  Timer? _clockTimer;
  DateTime? _connectedAt;
  int _lastPolledUptime = -1;
  // The uptime advances every second while connected. Driving it through a
  // ValueNotifier + a ValueListenableBuilder around JUST the uptime line means
  // the 1s tick rebuilds that one Text — not the whole screen (monitor, stats,
  // banners, buttons) as a bare `setState(() {})` did every second.
  final ValueNotifier<int> _uptimeTick = ValueNotifier<int>(0);

  // Self-update detection (Phase 1/2): null until the backend answers.
  UpdateInfo? _update;

  // Other-VPN/proxy conflicts: checked on open and refreshed periodically. The
  // engine also keeps this fresh in status while connected; this covers the
  // OFFLINE screen (before connecting) and dismissal.
  List<VpnConflict> _conflicts = const [];
  bool _conflictsDismissed = false;
  Timer? _conflictTimer;

  // "Last session" receipt: dismissed by the user, or reset on the next connect.
  bool _receiptDismissed = false;

  @override
  void initState() {
    super.initState();
    _resetIdle();
    _clockTimer = Timer.periodic(const Duration(seconds: 1), (_) {
      // No setState: just bump the notifier so only the uptime line repaints.
      if (_connectedAt != null) _uptimeTick.value = _displayUptime;
    });
    // Warn about another active VPN/proxy: check on open, then every 30s while
    // this screen is up (cheap, off the UI isolate).
    _conflictTimer = Timer.periodic(
      const Duration(seconds: 30),
      (_) => _checkConflicts(),
    );
    WidgetsBinding.instance.addPostFrameCallback((_) {
      _prefill();
      _checkUpdate();
      _checkConflicts();
    });
    // The global boss key (Ctrl+Alt+Esc) is registered app-wide in main.dart;
    // in-window double-Esc is handled app-wide too.
  }

  Future<void> _checkConflicts() async {
    if (!mounted) return;
    try {
      final found = await context.read<ApiClient>().checkConflicts();
      if (mounted) setState(() => _conflicts = found);
    } catch (_) {
      // A failed check must never disrupt the screen — just leave the last result.
    }
  }

  Future<void> _checkUpdate() async {
    if (!mounted) return;
    final api = context.read<ApiClient>();
    final u = await api.updateCheck();
    if (mounted) setState(() => _update = u);
  }

  /// Tappable "update available" banner → the optional in-app updater (Phase 3):
  /// download + verify + install, all from inside the app. Purely opt-in.
  Widget _updateBanner() {
    final v = _update?.latest ?? '';
    return Material(
      color: PxColors.accent.withValues(alpha: 0.15),
      child: InkWell(
        onTap: _showUpdateDialog,
        child: Padding(
          padding: const EdgeInsets.symmetric(vertical: 8, horizontal: 16),
          child: Text(
            v.isEmpty
                ? 'An update is available — tap for details'
                : 'Update available — v$v · tap for details',
            textAlign: TextAlign.center,
            style: const TextStyle(color: PxColors.textPrimary, fontSize: 13),
          ),
        ),
      ),
    );
  }

  /// Warns that another VPN or system proxy is redirecting traffic and may stop
  /// this connection routing. Amber (warning), dismissible, tap for details.
  /// Deliberately generic wording — no product names — so it fits the terminal
  /// skin and never reads like a VPN advert.
  Widget _conflictBanner(List<VpnConflict> capturing) {
    final hasVpn = capturing.any((c) => c.kind == 'vpn');
    final label = hasVpn
        ? 'Another VPN is active — it may stop this connection routing. Tap for details'
        : 'A system proxy is active — it may stop this connection routing. Tap for details';
    return Material(
      color: PxColors.warning.withValues(alpha: 0.15),
      child: InkWell(
        onTap: () => _showConflictDialog(capturing),
        child: Padding(
          padding: const EdgeInsets.symmetric(vertical: 8, horizontal: 16),
          child: Row(
            children: [
              const Icon(Icons.warning_amber_rounded,
                  color: PxColors.warning, size: 16),
              const SizedBox(width: 8),
              Expanded(
                child: Text(
                  label,
                  style: const TextStyle(color: PxColors.textPrimary, fontSize: 13),
                ),
              ),
              InkWell(
                onTap: () => setState(() => _conflictsDismissed = true),
                child: const Padding(
                  padding: EdgeInsets.all(2),
                  child: Icon(Icons.close, color: PxColors.textMuted, size: 15),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }

  /// Whole days until the locally-set plan expiry (negative = already expired),
  /// or null when unset/unparseable. Compared date-only so "today" reads as 0.
  int? _expiryDaysLeft([String? iso]) {
    final src = (iso != null && iso.isNotEmpty) ? iso : _planExpiry;
    if (src.isEmpty) return null;
    final d = DateTime.tryParse(src);
    if (d == null) return null;
    final today = DateTime.now();
    final a = DateTime(today.year, today.month, today.day);
    final b = DateTime(d.year, d.month, d.day);
    return b.difference(a).inDays;
  }

  /// Operator-required update: the server told us this build is below the
  /// minimum version. Firmer than the optional self-update banner — the
  /// customer should update to keep using it. Tapping opens the download page.
  Widget _forceUpdateBanner(ConnStatus s) {
    final msg = s.updateMessage.isNotEmpty
        ? s.updateMessage
        : 'A newer version is required — please update to keep connecting.';
    return Material(
      color: PxColors.warning.withValues(alpha: 0.15),
      child: InkWell(
        onTap: s.updateUrl.isEmpty
            ? null
            : () => _copy(s.updateUrl, 'Download link copied'),
        child: Padding(
          padding: const EdgeInsets.symmetric(vertical: 8, horizontal: 16),
          child: Row(
            children: [
              const Icon(Icons.system_update_alt, color: PxColors.warning, size: 16),
              const SizedBox(width: 8),
              Expanded(
                child: Text(
                  msg,
                  style: const TextStyle(color: PxColors.warning, fontSize: 12.5),
                ),
              ),
              if (s.updateUrl.isNotEmpty)
                const Text('Copy link', style: TextStyle(color: PxColors.warning, fontWeight: FontWeight.bold, fontSize: 12.5)),
            ],
          ),
        ),
      ),
    );
  }

  /// A gentle renewal nudge shown only as expiry approaches (≤14 days) or once
  /// it has passed. Colour escalates: info → amber (≤7) → red (≤3 / expired).
  /// It's a LOCAL reminder from the customer's welcome message, not a live
  /// server value — deliberately soft wording, no product names.
  Widget _expiryBanner(int days) {
    final Color tint;
    final String label;
    if (days < 0) {
      tint = PxColors.danger;
      label = days == -1
          ? 'Your plan expired yesterday — renew to keep connecting.'
          : 'Your plan expired ${-days} days ago — renew to keep connecting.';
    } else if (days == 0) {
      tint = PxColors.danger;
      label = 'Your plan expires today — renew to avoid losing access.';
    } else if (days <= 3) {
      tint = PxColors.danger;
      label = 'Your plan expires in $days day${days == 1 ? '' : 's'} — renew soon.';
    } else if (days <= 7) {
      tint = PxColors.warning;
      label = 'Your plan expires in $days days — renew soon.';
    } else {
      tint = PxColors.accent;
      label = 'Your plan expires in $days days.';
    }
    return Material(
      color: tint.withValues(alpha: 0.15),
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 8, horizontal: 16),
        child: Row(
          children: [
            Icon(Icons.event_outlined, color: tint, size: 16),
            const SizedBox(width: 8),
            Expanded(
              child: Text(
                label,
                style: const TextStyle(color: PxColors.textPrimary, fontSize: 13),
              ),
            ),
          ],
        ),
      ),
    );
  }

  void _showConflictDialog(List<VpnConflict> capturing) {
    showDialog<void>(
      context: context,
      barrierColor: Colors.black.withValues(alpha: 0.72),
      builder: (ctx) => pixelTypography(
        ctx,
        child: Dialog(
          backgroundColor: PxColors.surface,
          surfaceTintColor: Colors.transparent,
          elevation: 0,
          insetPadding: const EdgeInsets.all(28),
          shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(PxRadius.sm),
            side: const BorderSide(color: PxColors.frame, width: 1),
          ),
          child: Container(
            width: 460,
            padding: const EdgeInsets.all(AppSpace.lg),
            decoration: BoxDecoration(
              borderRadius: BorderRadius.circular(PxRadius.sm),
              boxShadow: crtGlow(PxColors.warning, alpha: 0.12, blur: 18),
            ),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  '> NETWORK CONFLICT',
                  style: TextStyle(
                    fontFamily: PxFont.hero,
                    fontWeight: FontWeight.w700,
                    fontSize: 15,
                    color: PxColors.warning,
                    shadows: glowText(PxColors.warning, alpha: 0.5),
                  ),
                ),
                const SizedBox(height: AppSpace.md),
                Text(
                  capturing.length == 1
                      ? 'This is active and handling your traffic:'
                      : 'These are active and handling your traffic:',
                  style: const TextStyle(
                      color: PxColors.textPrimary, fontSize: 13, height: 1.4),
                ),
                const SizedBox(height: AppSpace.sm),
                for (final c in capturing)
                  Padding(
                    padding: const EdgeInsets.only(bottom: 4),
                    child: Text(
                      '  • ${c.name}  [${c.kind == 'proxy' ? 'system proxy' : 'VPN'}]',
                      style: TextStyle(
                        fontFamily: PxFont.body,
                        color: PxColors.textMuted,
                        fontSize: 13,
                      ),
                    ),
                  ),
                const SizedBox(height: AppSpace.md),
                const Text(
                  'While it is running, your traffic may leave through it instead '
                  'of this connection. Turn it off (or disconnect it) for the most '
                  'reliable results, then connect.',
                  style: TextStyle(
                      color: PxColors.textMuted, fontSize: 12, height: 1.5),
                ),
                const SizedBox(height: AppSpace.lg),
                Align(
                  alignment: Alignment.centerRight,
                  child: PixelButton(
                    label: 'OK',
                    onPressed: () => Navigator.of(ctx).pop(),
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }

  void _showUpdateDialog() {
    final u = _update;
    if (u == null) return;
    final api = context.read<ApiClient>();
    showDialog<void>(
      context: context,
      barrierDismissible: false,
      builder: (ctx) => _UpdateDialog(api: api, info: u),
    );
  }

  /// Keep the clock anchor in step with the backend without re-syncing on every
  /// poll (which is what made it jump): only re-anchor when a NEW poll value
  /// diverges from the smooth local count by more than a few seconds.
  void _syncClock(bool connected, int uptimeSeconds) {
    if (!connected) {
      _connectedAt = null;
      _lastPolledUptime = -1;
      return;
    }
    if (uptimeSeconds == _lastPolledUptime && _connectedAt != null) return;
    _lastPolledUptime = uptimeSeconds;
    final anchor = _connectedAt;
    final localElapsed =
        anchor == null ? -1 : DateTime.now().difference(anchor).inSeconds;
    if (anchor == null || (localElapsed - uptimeSeconds).abs() > 3) {
      _connectedAt =
          DateTime.now().subtract(Duration(seconds: uptimeSeconds));
    }
  }

  int get _displayUptime => _connectedAt == null
      ? _lastPolledUptime.clamp(0, 1 << 31)
      : DateTime.now().difference(_connectedAt!).inSeconds;

  Future<void> _prefill() async {
    final api = context.read<ApiClient>();
    var hasCreds = false;
    try {
      final r = await api.rememberedLogin();
      if (!mounted) return;
      final u = (r['username'] as String?) ?? '';
      final p = (r['password'] as String?) ?? '';
      setState(() {
        _userCtrl.text = u;
        // Pre-fill the saved password so "remember" means one less thing to type.
        if (p.isNotEmpty) _passCtrl.text = p;
        _remember = (r['remember'] as bool?) ?? false;
      });
      hasCreds = u.isNotEmpty && p.isNotEmpty;
    } catch (_) {
      // store may be locked / backend hiccup — leave fields blank.
    }
    var autoConnect = false;
    try {
      final s = await api.getSettings();
      if (!mounted) return;
      _timeoutMinutes = (s['timeout_minutes'] as int?) ?? 5;
      autoConnect = (s['auto_connect'] as bool?) ?? false;
      setState(() {
        _routingMode = (s['routing_mode'] as String?) ?? 'proxy';
        _planExpiry = (s['plan_expiry'] as String?) ?? '';
      });
      _resetIdle();
    } catch (_) {
      // keep the default timeout / routing.
    }
    // Auto-connect after unlock (opt-in): only when we have saved credentials,
    // nothing is already up, and we haven't already tried this session.
    if (autoConnect && hasCreds && !_autoConnectTried && mounted) {
      _autoConnectTried = true;
      final conn = context.read<ConnectionController>();
      if (!conn.status.connected &&
          !conn.status.connecting &&
          !conn.optimisticConnecting) {
        // Let the first frame settle, then start the normal connect flow.
        WidgetsBinding.instance.addPostFrameCallback((_) {
          if (mounted) _connect();
        });
      }
    }
  }

  /// Re-read the saved routing mode (e.g. after the settings sheet changed it).
  Future<void> _refreshRoutingMode() async {
    if (!mounted) return;
    try {
      final s = await context.read<ApiClient>().getSettings();
      if (mounted) {
        setState(() => _routingMode = (s['routing_mode'] as String?) ?? 'proxy');
      }
    } catch (_) {}
  }

  @override
  void dispose() {
    _errTimer?.cancel();
    _idleTimer?.cancel();
    _clockTimer?.cancel();
    _conflictTimer?.cancel();
    _screenFocus.dispose();
    _userCtrl.dispose();
    _passCtrl.dispose();
    _uptimeTick.dispose();
    super.dispose();
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    // Runs whenever the watched ConnectionController notifies, which is where
    // the error-banner timer belongs — build() must not have side effects.
    final conn = context.watch<ConnectionController>();
    // Drop the optimistic-off latch once the backend actually reports the
    // disconnect (plain assignment: build() runs right after this).
    if (_optimisticOff && !conn.status.connected) _optimisticOff = false;
    final raw = conn.actionError ?? conn.status.lastError;
    final err = raw == null ? null : ConnectionController.friendlyError(raw);
    if (err == _trackedError) return;
    _trackedError = err;
    _errTimer?.cancel();
    if (err != null) {
      // Long enough to read a full sentence (messages now say what to do).
      _errTimer = Timer(const Duration(seconds: 9), () {
        if (mounted) setState(() => _dismissedError = err);
      });
    }
  }

  /// Immediate light tap on button press (the heavy impact fires later, on the
  /// nail's slam). No-op on desktops without haptics; fully guarded.
  void _pressHaptic() {
    if (!DreamnailStrike.effectsEnabled) return;
    try {
      unawaited(HapticFeedback.selectionClick());
    } catch (_) {}
  }

  Future<void> _connect() async {
    final user = _userCtrl.text.trim();
    final pass = _passCtrl.text;
    if (user.isEmpty || pass.isEmpty || _striking) return;
    _pressHaptic();
    FocusScope.of(context).unfocus();
    // Let a fresh attempt re-surface even an identical previous error.
    setState(() {
      _dismissedError = null;
      _optimisticOff = false; // a new connect cancels any lingering off-latch
      _receiptDismissed = false; // show a fresh receipt after this session
    });
    final conn = context.read<ConnectionController>();
    await _playTransition(connect: true); // build the link up
    if (!mounted) return; // panicked/locked mid-animation — don't connect
    conn.connect(user, pass, _remember);
  }

  Future<void> _disconnect() async {
    if (_striking) return;
    _pressHaptic();
    final conn = context.read<ConnectionController>();
    // Latch the offline look now so nothing snaps back to 100% during the
    // ~2s the backend takes to report the disconnect.
    setState(() => _optimisticOff = true);
    await _playTransition(connect: false); // power the link down
    if (!mounted) return;
    conn.disconnect();
  }

  Future<void> _playTransition({required bool connect}) async {
    final mon = _monKey.currentState;
    if (mon == null) return;
    setState(() => _striking = true);
    try {
      await mon.play(connect: connect);
    } finally {
      if (mounted) setState(() => _striking = false);
    }
  }

  // --- lifecycle gestures --------------------------------------------------

  /// Restart the inactivity countdown. Called on every key/pointer interaction.
  void _resetIdle() {
    _idleTimer?.cancel();
    _idleSeconds = _timeoutMinutes * 60;
    if (_showCountdown) setState(() => _showCountdown = false);
    _idleTimer = Timer.periodic(const Duration(seconds: 1), (_) {
      _idleSeconds--;
      if (_idleSeconds <= 0) {
        _idleTimer?.cancel();
        if (mounted) {
          // Same as panic: re-lock the store, not just the screen.
          unawaited(context.read<ConnectionController>().lock());
          widget.onBack(); // auto-lock back to the weather disguise
        }
      } else if (_idleSeconds <= 60 && mounted) {
        setState(
          () => _showCountdown = true,
        ); // also ticks the label each second
      }
    });
  }

  KeyEventResult _onKey(FocusNode node, KeyEvent e) {
    if (e is! KeyDownEvent) return KeyEventResult.ignored;
    _resetIdle();
    final kb = HardwareKeyboard.instance;
    // Double-Esc is handled app-wide by PanicEscape (main.dart), independent of
    // keyboard focus; swallow Esc here so it never pops this route on its own.
    if (e.logicalKey == LogicalKeyboardKey.escape) return KeyEventResult.handled;
    if (e.logicalKey == LogicalKeyboardKey.keyW &&
        (kb.isControlPressed || kb.isMetaPressed) &&
        kb.isShiftPressed) {
      windowManager.hide(); // hide to tray
      return KeyEventResult.handled;
    }
    return KeyEventResult.ignored;
  }

  void _openSettings() {
    final api = context.read<ApiClient>();
    showModalBottomSheet(
      context: context,
      backgroundColor: Colors.transparent,
      isScrollControlled: true,
      builder: (_) => SettingsSheet(
        api: api,
        onTimeoutChanged: (m) {
          _timeoutMinutes = m;
          _resetIdle();
        },
      ),
    ).whenComplete(() {
      _resetIdle();
      _refreshRoutingMode(); // the sheet may have toggled stealth
    });
  }

  String _fmtCountdown() =>
      '${_idleSeconds ~/ 60}:${(_idleSeconds % 60).toString().padLeft(2, '0')}';

  @override
  Widget build(BuildContext context) {
    final cs = context.watch<ConnectionController>();
    final s = cs.status;
    // `_optimisticOff` forces the offline look the instant Disconnect is pressed,
    // so the monitor powers down and stays down instead of flashing back to 100%
    // while we wait for the backend to report the disconnect.
    final connected = s.connected && !_optimisticOff;
    final connecting =
        (s.connecting || cs.optimisticConnecting) && !_optimisticOff;
    _syncClock(connected, s.uptimeSeconds);

    // Routing verification, three distinct outcomes while connected:
    //  • green tick — the OS proxy applied AND traffic is confirmed via the
    //    server (trafficVerified);
    //  • red, actionable — the OS never accepted our proxy, so apps bypass the
    //    VPN and the user must set it manually (systemProxyNotApplied);
    //  • amber — proxy applied but the Xray→server path isn't routing
    //    (routingWarning).
    final verified = cs.trafficVerified;
    final proxyNotApplied = cs.systemProxyNotApplied;
    final routingWarn = cs.routingWarning;
    // Stealth (TUN): whether it's the active mode, or was requested but fell back
    // to standard routing (needs admin/root) — the latter is the one to warn on.
    final stealthFellBack = cs.stealthFellBack;
    final stealthActive = cs.stealthActive;

    // Effective routing mode for the WEB ONLY / WHOLE SYSTEM tag. While connected
    // the live status is authoritative (it accounts for a stealth→standard
    // fallback); before connecting we show the saved preference.
    final effectiveTun = connected ? stealthActive : _routingMode == 'tun';

    // Other VPN/proxy conflicts: the engine keeps status fresh while connected;
    // `_conflicts` covers the offline screen. Union by name so neither source
    // misses one. Only ones actually redirecting traffic get a banner.
    final conflictsByName = <String, VpnConflict>{
      for (final c in _conflicts) c.name: c,
      for (final c in s.vpnConflicts) c.name: c,
    };
    final capturing =
        conflictsByName.values.where((c) => c.capturing).toList();

    // Error-banner bookkeeping happens in didChangeDependencies — build() must
    // stay a pure function of state. It used to cancel a timer, start a 5s one
    // and reassign fields from here, under a rebuild storm driven by the
    // once-a-second idle countdown.
    final err = _trackedError;
    final showError = err != null && err != _dismissedError;

    return Focus(
      focusNode: _screenFocus,
      autofocus: true,
      onKeyEvent: _onKey,
      child: Listener(
        onPointerDown: (_) => _resetIdle(),
        child: pixelTypography(
          context,
          child: Scaffold(
            backgroundColor: PxColors.bg,
            body: Stack(
              children: [
                // Near-black green-tinted CRT ground with a faint phosphor
                // bloom toward the centre — no texture, just depth.
                const Positioned.fill(
                  child: DecoratedBox(
                    decoration: BoxDecoration(
                      gradient: RadialGradient(
                        center: Alignment(0, -0.35),
                        radius: 1.3,
                        colors: [
                          Color(0xFF05130A), // faint green pool
                          Color(0xFF030D04),
                          Color(0xFF020C02), // dim edges
                        ],
                        stops: [0.0, 0.5, 1.0],
                      ),
                    ),
                  ),
                ),
              SafeArea(
                child: Column(
                  children: [
                    _topBar(),
                    if (cs.backendDown) _backendDownBanner(),
                    if (_update?.available ?? false) _updateBanner(),
                    if (s.updateRequired) _forceUpdateBanner(s),
                    if (capturing.isNotEmpty && !_conflictsDismissed)
                      _conflictBanner(capturing),
                    if (() {
                      final d = _expiryDaysLeft(s.planExpiry);
                      return d != null && d <= 14;
                    }())
                      _expiryBanner(_expiryDaysLeft(s.planExpiry)!),
                    Expanded(
                      child: SingleChildScrollView(
                        padding: const EdgeInsets.fromLTRB(
                          AppSpace.xl,
                          AppSpace.sm,
                          AppSpace.xl,
                          AppSpace.lg,
                        ),
                        child: Center(
                          child: ConstrainedBox(
                            constraints: const BoxConstraints(maxWidth: 420),
                            child: Column(
                              children: [
                                const SizedBox(height: AppSpace.md),
                                // Terminal "secure link" readout (replaces the
                                // old sigil) — animates on connect/disconnect.
                                TerminalMonitor(
                                  key: _monKey,
                                  connected: connected,
                                  connecting: connecting,
                                ),
                                const SizedBox(height: AppSpace.xl),
                                _terminalStatus(connected, connecting, verified, s),
                                const SizedBox(height: AppSpace.sm),
                                // The single status badge/chip grows + fades in
                                // (and out on disconnect) instead of snapping.
                                AnimatedSwitcher(
                                  duration: const Duration(milliseconds: 260),
                                  switchInCurve: Curves.easeOutCubic,
                                  transitionBuilder: (child, anim) =>
                                      SizeTransition(
                                        sizeFactor: anim,
                                        alignment: Alignment.topLeft,
                                        child: FadeTransition(
                                          opacity: anim,
                                          child: child,
                                        ),
                                      ),
                                  child: _statusBadge(
                                    connected,
                                    proxyNotApplied,
                                    stealthFellBack,
                                    routingWarn,
                                    stealthActive,
                                    s,
                                  ),
                                ),
                                const SizedBox(height: AppSpace.xl),
                                _credentialCard(connected, connecting),
                                const SizedBox(height: AppSpace.md),
                                _routingTag(effectiveTun, connecting),
                                if (s.hasDataCap) ...[
                                  const SizedBox(height: AppSpace.md),
                                  _DataStickyNote(
                                    usedBytes: s.dataUsedBytes,
                                    limitBytes: s.dataLimitBytes,
                                  ),
                                ],
                                if (!connected &&
                                    !connecting &&
                                    !_receiptDismissed &&
                                    s.lastSession != null) ...[
                                  const SizedBox(height: AppSpace.md),
                                  _sessionReceipt(s.lastSession!),
                                ],
                                if (showError) ...[
                                  const SizedBox(height: AppSpace.md),
                                  _errorBanner(err),
                                ],
                                const SizedBox(height: AppSpace.lg),
                                _connectButton(connected, connecting),
                              ],
                            ),
                          ),
                        ),
                      ),
                    ),
                    // Dim (not cut) the live stats on disconnect.
                    AnimatedOpacity(
                      opacity: connected ? 1.0 : 0.45,
                      duration: const Duration(milliseconds: 260),
                      curve: Curves.easeOut,
                      child: _statsRow(s, connected, cs.downloadHistory,
                          cs.trafficVerified ? cs.proxyStatus.verifiedIp : null),
                    ),
                  ],
                ),
              ),
              const Positioned.fill(child: Scanlines(opacity: 0.08)),
            ],
          ),
        ),
        ),
      ),
    );
  }

  // --- pieces --------------------------------------------------------------

  Widget _topBar() => Padding(
    padding: const EdgeInsets.symmetric(horizontal: AppSpace.sm, vertical: 6),
    child: Row(
      children: [
        _TermButton(label: '← BACK', onPressed: widget.onBack),
        const Spacer(),
        if (_showCountdown)
          Flexible(
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: AppSpace.sm),
              child: Text(
                '! auto-lock in ${_fmtCountdown()}',
                overflow: TextOverflow.ellipsis,
                style: const TextStyle(
                  fontFamily: PxFont.body,
                  color: PxColors.warning,
                  fontSize: 11,
                  letterSpacing: 1,
                ),
              ),
            ),
          ),
        _TermButton(label: 'PLAN', onPressed: _showPlan),
        const SizedBox(width: AppSpace.sm),
        _TermButton(label: 'DIAG', onPressed: _showLeakTest),
        const SizedBox(width: AppSpace.sm),
        _TermButton(label: 'LOGS', onPressed: _showLogs),
        const SizedBox(width: AppSpace.sm),
        _TermButton(label: 'CONFIG', onPressed: _openSettings),
      ],
    ),
  );

  void _copy(String text, String msg) {
    Clipboard.setData(ClipboardData(text: text));
    ScaffoldMessenger.maybeOf(context)?.showSnackBar(
      SnackBar(content: Text(msg), duration: const Duration(seconds: 1)),
    );
  }

  /// Human "updated 3m ago" from the last-synced epoch seconds.
  String _syncedAgo(int secs) {
    if (secs <= 0) return 'not synced yet — connect to load your plan';
    final d = DateTime.now()
        .difference(DateTime.fromMillisecondsSinceEpoch(secs * 1000));
    if (d.inMinutes < 1) return 'updated just now';
    if (d.inMinutes < 60) return 'updated ${d.inMinutes}m ago';
    if (d.inHours < 24) return 'updated ${d.inHours}h ago';
    return 'updated ${d.inDays}d ago';
  }

  /// "MY PLAN" — the customer's data cap, usage, expiry, and account status,
  /// synced from the server on connect. Read-only; updates live while open.
  Future<void> _showPlan() async {
    await showDialog<void>(
      context: context,
      barrierColor: Colors.black.withValues(alpha: 0.72),
      builder: (ctx) => pixelTypography(
        ctx,
        child: Dialog(
          backgroundColor: PxColors.surface,
          surfaceTintColor: Colors.transparent,
          elevation: 0,
          insetPadding: const EdgeInsets.all(28),
          shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(PxRadius.sm),
            side: const BorderSide(color: PxColors.frame, width: 1),
          ),
          child: Consumer<ConnectionController>(
            builder: (ctx, cs, _) {
              final s = cs.status;
              final days = _expiryDaysLeft(s.planExpiry);
              final overLimit = s.dataLimitReached;
              String statusLabel;
              Color statusColor;
              if (!s.accountEnabled) {
                statusLabel = 'SUSPENDED';
                statusColor = PxColors.danger;
              } else if (days != null && days < 0) {
                statusLabel = 'EXPIRED';
                statusColor = PxColors.danger;
              } else if (overLimit) {
                statusLabel = 'LIMIT REACHED';
                statusColor = PxColors.danger;
              } else {
                statusLabel = 'ACTIVE';
                statusColor = PxColors.success;
              }
              return Container(
                width: 440,
                padding: const EdgeInsets.all(AppSpace.lg),
                decoration: BoxDecoration(
                  borderRadius: BorderRadius.circular(PxRadius.sm),
                  boxShadow: crtGlow(PxColors.frame, alpha: 0.14, blur: 18),
                ),
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Row(
                      mainAxisAlignment: MainAxisAlignment.spaceBetween,
                      children: [
                        _statLine('> MY PLAN', PxColors.textPrimary, bold: true),
                        GestureDetector(
                          behavior: HitTestBehavior.opaque,
                          onTap: () => Navigator.of(ctx).maybePop(),
                          child: const Padding(
                            padding: EdgeInsets.all(4),
                            child: Text('[ ✕ ]',
                                style: TextStyle(
                                    fontFamily: PxFont.body,
                                    fontWeight: FontWeight.w700,
                                    fontSize: 15,
                                    color: PxColors.textMuted)),
                          ),
                        ),
                      ],
                    ),
                    const SizedBox(height: AppSpace.md),
                    // Status chip.
                    _planStatusChip(statusLabel, statusColor),
                    const SizedBox(height: AppSpace.lg),
                    // DATA.
                    _planLabel('DATA'),
                    const SizedBox(height: 6),
                    if (s.hasDataCap) ...[
                      Text(
                        '${formatBytes(s.dataUsedBytes)} / ${formatBytes(s.dataLimitBytes)}',
                        style: const TextStyle(
                            fontFamily: PxFont.body,
                            fontWeight: FontWeight.w700,
                            fontSize: 20,
                            color: PxColors.textPrimary),
                      ),
                      const SizedBox(height: 8),
                      _planBar(s.dataUsedFraction, overLimit),
                      const SizedBox(height: 6),
                      Text(
                        overLimit
                            ? 'no data left this cycle'
                            : '${formatBytes((s.dataLimitBytes - s.dataUsedBytes).clamp(0, s.dataLimitBytes))} left · ${(s.dataUsedFraction * 100).round()}% used',
                        style: const TextStyle(
                            fontFamily: PxFont.body,
                            fontSize: 12,
                            color: PxColors.textMuted),
                      ),
                    ] else
                      const Text('Unlimited',
                          style: TextStyle(
                              fontFamily: PxFont.body,
                              fontSize: 16,
                              color: PxColors.success)),
                    const SizedBox(height: AppSpace.lg),
                    // EXPIRY.
                    _planLabel('EXPIRY'),
                    const SizedBox(height: 6),
                    Text(
                      s.planExpiry.isEmpty
                          ? 'No expiry'
                          : (days == null
                              ? s.planExpiry
                              : days < 0
                                  ? 'Expired ${-days} day${days == -1 ? '' : 's'} ago (${s.planExpiry})'
                                  : days == 0
                                      ? 'Expires today (${s.planExpiry})'
                                      : 'Expires in $days day${days == 1 ? '' : 's'} (${s.planExpiry})'),
                      style: TextStyle(
                          fontFamily: PxFont.body,
                          fontSize: 16,
                          color: (days != null && days <= 7)
                              ? PxColors.warning
                              : PxColors.textPrimary),
                    ),
                    const SizedBox(height: AppSpace.lg),
                    const Divider(color: PxColors.frame, height: 1),
                    const SizedBox(height: AppSpace.sm),
                    Text(
                      '> synced from your plan · ${_syncedAgo(s.planSyncedAt)}',
                      style: const TextStyle(
                          fontFamily: PxFont.body,
                          fontSize: 11,
                          letterSpacing: 0.5,
                          color: PxColors.mutedDeep),
                    ),
                  ],
                ),
              );
            },
          ),
        ),
      ),
    );
  }

  Widget _planLabel(String text) => Text(
        '> $text',
        style: TextStyle(
            fontFamily: PxFont.body,
            fontSize: 12,
            letterSpacing: 2,
            color: PxColors.textMuted.withValues(alpha: 0.8)),
      );

  Widget _planStatusChip(String label, Color color) => Container(
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
        decoration: BoxDecoration(
          color: color.withValues(alpha: 0.14),
          borderRadius: BorderRadius.circular(PxRadius.sm),
          border: Border.all(color: color.withValues(alpha: 0.5)),
        ),
        child: Text(label,
            style: TextStyle(
                fontFamily: PxFont.body,
                fontWeight: FontWeight.w700,
                fontSize: 12,
                letterSpacing: 1,
                color: color)),
      );

  Widget _planBar(double frac, bool over) {
    final fill = over || frac >= 0.9
        ? PxColors.danger
        : (frac >= 0.75 ? PxColors.warning : PxColors.success);
    return ClipRRect(
      borderRadius: BorderRadius.circular(4),
      child: Container(
        height: 10,
        color: PxColors.frameInset,
        child: Align(
          alignment: Alignment.centerLeft,
          child: FractionallySizedBox(
            widthFactor: frac <= 0 ? 0.02 : frac,
            child: Container(color: fill),
          ),
        ),
      ),
    );
  }

  /// "DIAG" — the one-click leak test, as a terminal readout.
  Future<void> _showLeakTest() async {
    await showDialog<void>(
      context: context,
      barrierColor: Colors.black.withValues(alpha: 0.72),
      builder: (ctx) => pixelTypography(
        ctx,
        child: Dialog(
          backgroundColor: PxColors.surface,
          surfaceTintColor: Colors.transparent,
          elevation: 0,
          insetPadding: const EdgeInsets.all(28),
          shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(PxRadius.sm),
            side: const BorderSide(color: PxColors.frame, width: 1),
          ),
          child: _LeakTestPanel(api: context.read<ApiClient>()),
        ),
      ),
    );
  }

  Future<void> _showLogs() async {
    final api = context.read<ApiClient>();
    Future<List<String>> load() async {
      try {
        return await api.logs();
      } catch (e) {
        return ['Could not load logs: $e'];
      }
    }

    var lines = await load();
    if (!mounted) return;
    await showDialog<void>(
      context: context,
      barrierColor: Colors.black.withValues(alpha: 0.72),
      builder: (ctx) => pixelTypography(
        ctx,
        child: StatefulBuilder(
          builder: (ctx, setLocal) => Dialog(
            backgroundColor: PxColors.surface,
            surfaceTintColor: Colors.transparent,
            elevation: 0,
            insetPadding: const EdgeInsets.all(28),
            shape: RoundedRectangleBorder(
              borderRadius: BorderRadius.circular(PxRadius.sm),
              side: const BorderSide(color: PxColors.frame, width: 1),
            ),
            child: Container(
              width: 500,
              padding: const EdgeInsets.all(AppSpace.lg),
              decoration: BoxDecoration(
                borderRadius: BorderRadius.circular(PxRadius.sm),
                boxShadow: crtGlow(PxColors.frame, alpha: 0.14, blur: 18),
              ),
              child: Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Row(
                    children: [
                      Expanded(
                        child: Text(
                          '> ACTIVITY LOG',
                          style: TextStyle(
                            fontFamily: PxFont.hero,
                            fontWeight: FontWeight.w700,
                            fontSize: 15,
                            letterSpacing: 1.5,
                            color: PxColors.accent,
                            shadows: glowText(PxColors.accent, alpha: 0.3),
                          ),
                        ),
                      ),
                      _TermButton(
                        label: 'REFRESH',
                        onPressed: () async {
                          final l = await load();
                          setLocal(() => lines = l);
                        },
                      ),
                    ],
                  ),
                  const SizedBox(height: AppSpace.sm),
                  Container(height: 1, color: PxColors.frameInset),
                  const SizedBox(height: AppSpace.sm),
                  SizedBox(
                    width: double.infinity,
                    height: 340,
                    child: lines.isEmpty
                        ? const Padding(
                            padding: EdgeInsets.only(top: 8),
                            child: Text(
                              '> no activity logged.',
                              style: TextStyle(
                                fontFamily: PxFont.body,
                                fontSize: 13,
                                color: PxColors.mutedDeep,
                              ),
                            ),
                          )
                        : Scrollbar(
                            child: SingleChildScrollView(
                              reverse: true,
                              child: SelectableText(
                                lines.join('\n'),
                                style: const TextStyle(
                                  fontFamily: PxFont.body,
                                  fontSize: 12.5,
                                  height: 1.5,
                                  color: PxColors.textMuted,
                                ),
                              ),
                            ),
                          ),
                  ),
                  const SizedBox(height: AppSpace.md),
                  Row(
                    mainAxisAlignment: MainAxisAlignment.end,
                    children: [
                      _TermButton(
                        label: 'COPY',
                        onPressed: () => _copy(lines.join('\n'), 'Logs copied'),
                      ),
                      const SizedBox(width: AppSpace.md),
                      _TermButton(
                        label: 'CLOSE',
                        onPressed: () => Navigator.of(ctx).pop(),
                      ),
                    ],
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }

  Widget _backendDownBanner() => Container(
    width: double.infinity,
    color: PxColors.warning.withValues(alpha: 0.12),
    padding: const EdgeInsets.symmetric(horizontal: AppSpace.xl, vertical: 8),
    child: const Text(
      "The app's background engine isn't responding — retrying…",
      style: TextStyle(color: PxColors.warning, fontSize: 13),
    ),
  );

  /// The line under the big status word: session timer when connected, the live
  /// "Step 2/4 …" connect progress while connecting, or the reconnect counter.
  /// Shown when the tunnel is up but the OS never accepted our proxy, so apps
  /// bypass the VPN. Actionable: tells the user the manual proxy to set.
  Widget _systemProxyBanner() => Container(
    padding: const EdgeInsets.symmetric(
      horizontal: AppSpace.md,
      vertical: AppSpace.sm,
    ),
    decoration: BoxDecoration(
      color: PxColors.danger.withValues(alpha: 0.14),
      borderRadius: BorderRadius.circular(PxRadius.sm),
      border: Border.all(color: PxColors.danger.withValues(alpha: 0.4)),
    ),
    child: const Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Icon(Icons.settings_ethernet, color: PxColors.danger, size: 18),
        SizedBox(width: AppSpace.sm),
        Flexible(
          child: Text(
            "Couldn't route your system automatically — set your proxy to "
            'SOCKS 127.0.0.1:11080 (HTTP 127.0.0.1:11081) manually',
            style: TextStyle(color: PxColors.danger, fontSize: 13),
          ),
        ),
      ],
    ),
  );

  /// Shown when the tunnel is up but the through-the-proxy IP check did not see
  /// the server — i.e. the OS isn't actually routing through Xray.
  Widget _routingWarningBanner() => Container(
    padding: const EdgeInsets.symmetric(
      horizontal: AppSpace.md,
      vertical: AppSpace.sm,
    ),
    decoration: BoxDecoration(
      color: PxColors.warning.withValues(alpha: 0.14),
      borderRadius: BorderRadius.circular(PxRadius.sm),
      border: Border.all(color: PxColors.warning.withValues(alpha: 0.4)),
    ),
    child: const Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Icon(Icons.warning_amber_rounded, color: PxColors.warning, size: 18),
        SizedBox(width: AppSpace.sm),
        Flexible(
          child: Text(
            'Connected but traffic may not be routing correctly',
            style: TextStyle(color: PxColors.warning, fontSize: 13),
          ),
        ),
      ],
    ),
  );

  /// Shown when the user turned on stealth but it couldn't start (no admin/root)
  /// so the app fell back to standard routing — otherwise the fallback is silent
  /// and a proxy-rejecting app/game keeps bypassing the VPN with no explanation.
  Widget _stealthFellBackBanner() => Container(
    padding: const EdgeInsets.symmetric(
      horizontal: AppSpace.md,
      vertical: AppSpace.sm,
    ),
    decoration: BoxDecoration(
      color: PxColors.warning.withValues(alpha: 0.14),
      borderRadius: BorderRadius.circular(PxRadius.sm),
      border: Border.all(color: PxColors.warning.withValues(alpha: 0.4)),
    ),
    child: const Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Icon(Icons.shield_outlined, color: PxColors.warning, size: 18),
        SizedBox(width: AppSpace.sm),
        Flexible(
          child: Text(
            'Stealth needs admin — using standard routing. Reconnect and '
            'approve the admin password prompt to enable it.',
            style: TextStyle(color: PxColors.warning, fontSize: 13),
          ),
        ),
      ],
    ),
  );

  // The credential form, styled as a terminal login block: "> user: ____".
  Widget _credentialCard(bool connected, bool connecting) {
    final locked = connected || connecting;
    // The header tracks the session so a live connection never still reads
    // "auth required" above greyed-out fields.
    final (header, headerColor) = connected
        ? ('// SESSION ACTIVE', PxColors.success)
        : connecting
            ? ('// AUTHENTICATING', PxColors.warning)
            : ('// AUTH REQUIRED', PxColors.mutedDeep);
    return AnimatedOpacity(
      opacity: locked ? 0.7 : 1.0,
      duration: const Duration(milliseconds: 260),
      child: _credentialPanel(locked, header, headerColor),
    );
  }

  Widget _credentialPanel(bool locked, String header, Color headerColor) => PixelPanel(
    padding: const EdgeInsets.fromLTRB(
      AppSpace.lg,
      AppSpace.md,
      AppSpace.lg,
      AppSpace.md,
    ),
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          header,
          style: TextStyle(
            fontFamily: PxFont.body,
            fontSize: 12,
            letterSpacing: 2,
            color: headerColor,
          ),
        ),
        const SizedBox(height: AppSpace.md),
        _termField(
          label: 'user',
          controller: _userCtrl,
          locked: locked,
          hint: 'type your login name',
        ),
        // Login-name format help — only useful while you can still type.
        if (!locked) ...[
          const SizedBox(height: 3),
          const Padding(
            padding: EdgeInsets.only(left: 4),
            child: Text(
              '# starts with portkey-, pixelkey- or linuxkey-',
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: TextStyle(
                fontFamily: PxFont.body,
                color: PxColors.mutedDeep,
                fontSize: 11,
              ),
            ),
          ),
        ],
        const SizedBox(height: AppSpace.md),
        _termField(
          label: 'pass',
          controller: _passCtrl,
          locked: locked,
          obscure: _obscure,
          hint: 'type your password',
          onSubmitted: (_) => _connect(),
          suffix: GestureDetector(
            onTap: () => setState(() => _obscure = !_obscure),
            child: Text(
              _obscure ? '[show]' : '[hide]',
              style: const TextStyle(
                fontFamily: PxFont.body,
                fontSize: 12,
                color: PxColors.textMuted,
              ),
            ),
          ),
        ),
        const SizedBox(height: AppSpace.md),
        GestureDetector(
          onTap: locked
              ? null
              : () => setState(() => _remember = !_remember),
          behavior: HitTestBehavior.opaque,
          child: Row(
            children: [
              Text(
                _remember ? '[x]' : '[ ]',
                style: const TextStyle(
                  fontFamily: PxFont.body,
                  fontSize: 14,
                  color: PxColors.success,
                ),
              ),
              const SizedBox(width: 8),
              const Text(
                'remember session',
                style: TextStyle(
                  fontFamily: PxFont.body,
                  fontSize: 13,
                  color: PxColors.textMuted,
                ),
              ),
            ],
          ),
        ),
      ],
    ),
  );

  // One "> label: ____" terminal input line (borderless, block cursor).
  Widget _termField({
    required String label,
    required TextEditingController controller,
    required bool locked,
    bool obscure = false,
    String? hint,
    ValueChanged<String>? onSubmitted,
    Widget? suffix,
  }) => Row(
    children: [
      Text(
        '> $label: ',
        style: TextStyle(
          fontFamily: PxFont.body,
          fontSize: 16,
          color: PxColors.textPrimary,
          shadows: glowText(PxColors.accent, alpha: 0.35),
        ),
      ),
      Expanded(
        child: TextField(
          controller: controller,
          enabled: !locked,
          obscureText: obscure,
          obscuringCharacter: '█',
          onChanged: (_) => setState(() {}),
          onSubmitted: onSubmitted,
          cursorColor: PxColors.accent,
          cursorWidth: 10,
          cursorRadius: Radius.zero,
          style: TextStyle(
            fontFamily: PxFont.body,
            fontSize: 16,
            letterSpacing: 1,
            color: PxColors.textPrimary,
            shadows: glowText(PxColors.accent, alpha: 0.4),
          ),
          decoration: InputDecoration(
            isCollapsed: true,
            border: InputBorder.none,
            enabledBorder: InputBorder.none,
            focusedBorder: InputBorder.none,
            disabledBorder: InputBorder.none,
            // A muted in-field placeholder so it's obvious WHERE to type; it
            // disappears as soon as the field has text.
            hintText: hint,
            hintStyle: const TextStyle(
              fontFamily: PxFont.body,
              fontSize: 15,
              letterSpacing: 0.5,
              color: PxColors.mutedDeep,
            ),
          ),
        ),
      ),
      if (suffix != null) ...[const SizedBox(width: 8), suffix],
    ],
  );

  // Terminal-output status block under the sigil.
  Widget _terminalStatus(
    bool connected,
    bool connecting,
    bool verified,
    ConnStatus s,
  ) {
    final children = <Widget>[];
    if (connected) {
      // The SYNC bar in the monitor above already shows 100% — no second bar.
      children.add(_statLine('> STATUS: ONLINE', PxColors.success, bold: true));
      children.add(const SizedBox(height: 4));
      // Only this line rebuilds on the 1s tick (see `_uptimeTick`).
      children.add(
        ValueListenableBuilder<int>(
          valueListenable: _uptimeTick,
          builder: (_, _, _) =>
              _liveLine('> UPTIME: ${_fmtUptime(_displayUptime)}', PxColors.textMuted),
        ),
      );
      if (verified) {
        children.add(const SizedBox(height: 4));
        children.add(_statLine('> TUNNEL: VERIFIED', PxColors.textMuted));
      }
    } else if (connecting) {
      // An auto-reconnect in progress (after a drop / wake / Wi-Fi switch) reads
      // as RECONNECTING with its attempt number, so the user knows the app is
      // bringing the link back on its own rather than starting a fresh connect.
      final reattempt = s.reconnectAttempt;
      children.add(
        Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            _statLine(
              reattempt != null ? '> RECONNECTING' : '> ESTABLISHING TUNNEL',
              PxColors.warning,
              bold: true,
            ),
            const _CyclingDots(color: PxColors.warning),
          ],
        ),
      );
      if (reattempt != null) {
        children.add(const SizedBox(height: 4));
        children.add(
          _statLine(
            '> link dropped — auto-reconnect (try $reattempt)',
            PxColors.textMuted,
          ),
        );
      } else {
        // Which of the 4 connect steps we're on, so a slow connect visibly moves.
        final label = s.connectStepLabel;
        if (s.connectStep != null && label != null) {
          children.add(const SizedBox(height: 4));
          children.add(
            _statLine(
              '> step ${s.connectStep}/${s.connectStepTotal ?? 4}: '
              '${label.replaceAll('…', '').toLowerCase()}',
              PxColors.textMuted,
            ),
          );
        }
      }
    } else {
      children.add(_liveLine('> STATUS: OFFLINE', PxColors.textPrimary, bold: true));
    }
    return Column(mainAxisSize: MainAxisSize.min, children: children);
  }

  Widget _statLine(String text, Color color, {bool bold = false}) => Text(
    text,
    textAlign: TextAlign.center,
    style: TextStyle(
      fontFamily: PxFont.body,
      fontWeight: bold ? FontWeight.w700 : FontWeight.w400,
      fontSize: 16,
      letterSpacing: 1,
      color: color,
      shadows: glowText(color, alpha: 0.5),
    ),
  );

  // A status line with a trailing blinking block cursor.
  Widget _liveLine(String text, Color color, {bool bold = false}) => Row(
    mainAxisSize: MainAxisSize.min,
    children: [
      _statLine(text, color, bold: bold),
      const SizedBox(width: 2),
      BlinkingCursor(color: color, fontSize: 16),
    ],
  );

  static String _fmtUptime(int sec) {
    final h = sec ~/ 3600;
    final m = (sec % 3600) ~/ 60;
    final s = sec % 60;
    String two(int n) => n.toString().padLeft(2, '0');
    return '${two(h)}:${two(m)}:${two(s)}';
  }

  /// Human "2h 14m" / "3m 20s" / "45s" for the session receipt.
  static String _humanDuration(int seconds) {
    if (seconds < 0) seconds = 0;
    final h = seconds ~/ 3600;
    final m = (seconds % 3600) ~/ 60;
    final s = seconds % 60;
    if (h > 0) return '${h}h ${m}m';
    if (m > 0) return '${m}m ${s}s';
    return '${s}s';
  }

  /// A calm one-line summary of the last connection, shown after disconnect.
  /// Terminal styled to match the routing tag; dismissible.
  Widget _sessionReceipt(SessionReceipt r) {
    final drops = r.drops == 1 ? '1 drop' : '${r.drops} drops';
    final summary =
        '${_humanDuration(r.durationSeconds)} · '
        '${formatBytes(r.totalBytes)} · $drops';
    return Row(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.center,
      children: [
        Flexible(
          child: Text(
            '> LAST SESSION: $summary',
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            textAlign: TextAlign.center,
            style: TextStyle(
              fontFamily: PxFont.body,
              fontSize: 13,
              letterSpacing: 0.5,
              color: PxColors.textMuted,
              shadows: glowText(PxColors.textMuted, alpha: 0.3),
            ),
          ),
        ),
        const SizedBox(width: 6),
        InkWell(
          onTap: () => setState(() => _receiptDismissed = true),
          child: const Padding(
            padding: EdgeInsets.all(2),
            child: Icon(Icons.close, color: PxColors.mutedDeep, size: 14),
          ),
        ),
      ],
    );
  }

  /// Routing-scope indicator: WHOLE SYSTEM (stealth/TUN — every app + games) vs
  /// WEB ONLY (standard/system proxy — browsers & proxy-aware apps). Terminal
  /// styled to match the status block; tap opens CONFIG to change it.
  Widget _routingTag(bool wholeSystem, bool connecting) {
    final valColor = wholeSystem ? PxColors.accentGlow : PxColors.textPrimary;
    final value = wholeSystem ? 'WHOLE SYSTEM' : 'WEB ONLY';
    final caption =
        wholeSystem ? 'all apps & games' : 'browsers & proxy-aware apps';
    // Don't invite a mode change mid-connect (it only applies on the next connect).
    final tappable = !connecting;
    final row = Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        Row(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.center,
          children: [
            Text(
              '> ROUTING: ',
              style: TextStyle(
                fontFamily: PxFont.body,
                fontSize: 14,
                letterSpacing: 1,
                color: PxColors.textMuted,
                shadows: glowText(PxColors.textMuted, alpha: 0.3),
              ),
            ),
            Text(
              value,
              style: TextStyle(
                fontFamily: PxFont.body,
                fontWeight: FontWeight.w700,
                fontSize: 14,
                letterSpacing: 1,
                color: valColor,
                shadows: glowText(valColor, alpha: 0.5),
              ),
            ),
            if (tappable) ...[
              const SizedBox(width: 8),
              Text(
                '[change]',
                style: TextStyle(
                  fontFamily: PxFont.body,
                  fontSize: 12,
                  letterSpacing: 1,
                  color: PxColors.mutedDeep,
                ),
              ),
            ],
          ],
        ),
        const SizedBox(height: 2),
        Text(
          caption,
          style: TextStyle(
            fontFamily: PxFont.body,
            fontSize: 11,
            letterSpacing: 0.5,
            color: PxColors.textMuted,
          ),
        ),
      ],
    );
    if (!tappable) return row;
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: () {
          _resetIdle();
          _openSettings();
        },
        child: Padding(
          padding: const EdgeInsets.symmetric(vertical: 2),
          child: row,
        ),
      ),
    );
  }

  Widget _errorBanner(String message) => Container(
    width: double.infinity,
    padding: const EdgeInsets.symmetric(
      horizontal: AppSpace.md,
      vertical: AppSpace.sm,
    ),
    decoration: BoxDecoration(
      color: PxColors.danger.withValues(alpha: 0.14),
      borderRadius: BorderRadius.circular(PxRadius.sm),
      border: Border.all(color: PxColors.danger.withValues(alpha: 0.4)),
    ),
    child: Row(
      children: [
        const Icon(Icons.error_outline, color: PxColors.danger, size: 18),
        const SizedBox(width: AppSpace.sm),
        Expanded(
          child: Text(
            message,
            style: const TextStyle(color: PxColors.danger, fontSize: 13),
          ),
        ),
      ],
    ),
  );

  /// The one status badge/banner under the status word while connected — priority
  /// proxy-not-applied > stealth-fell-back > routing-warning > stealth/quality
  /// chip — or an empty box when disconnected. Keyed so the surrounding
  /// [AnimatedSwitcher] can cross-fade/size between states.
  Widget _statusBadge(
    bool connected,
    bool proxyNotApplied,
    bool stealthFellBack,
    bool routingWarn,
    bool stealthActive,
    ConnStatus s,
  ) {
    final String key;
    final Widget inner;
    if (connected && proxyNotApplied) {
      key = 'proxy';
      inner = _systemProxyBanner();
    } else if (connected && stealthFellBack) {
      key = 'fellback';
      inner = _stealthFellBackBanner();
    } else if (connected && routingWarn) {
      key = 'routing';
      inner = _routingWarningBanner();
    } else if (connected && !stealthActive) {
      // Standard routing: show the ping/quality chip. (The old "Stealth on —
      // all apps routed" chip was removed — stealth needs no callout.)
      key = 'quality';
      inner = _qualityChip(s.pingMs);
    } else {
      return const SizedBox(key: ValueKey('none'), width: double.infinity);
    }
    return Padding(
      key: ValueKey(key),
      padding: const EdgeInsets.only(top: AppSpace.sm),
      child: inner,
    );
  }

  Widget _connectButton(bool connected, bool connecting) {
    final canConnect =
        _userCtrl.text.trim().isNotEmpty && _passCtrl.text.isNotEmpty;
    if (connecting) {
      // Same bracketed terminal label as CONNECT/DISCONNECT (the round Material
      // spinner looked foreign here); the status line above already animates.
      return const PixelButton(label: 'Connecting…', onPressed: null);
    }
    final VoidCallback? onPressed = connected
        ? (_striking ? null : _disconnect)
        : ((canConnect && !_striking) ? _connect : null);
    // Dip the button while the strike plays, so the press feels physical.
    return AnimatedScale(
      scale: _striking ? 0.97 : 1.0,
      duration: const Duration(milliseconds: 120),
      curve: Curves.easeOut,
      child: PixelButton(
        label: connected ? 'Disconnect' : 'Connect',
        color: connected ? PxColors.danger : PxColors.success,
        border: connected ? const Color(0xFFF3A69E) : PxColors.successBright,
        fg: connected ? const Color(0xFFFFE3E6) : PxColors.ink,
        onPressed: onPressed,
      ),
    );
  }

  Widget _statsRow(ConnStatus s, bool connected, List<int> history,
      String? exitIp) {
    final down = connected ? s.downloadBps : 0;
    final up = connected ? s.uploadBps : 0;
    final ping = connected ? s.pingMs : null;
    return Padding(
      padding: const EdgeInsets.fromLTRB(
        AppSpace.lg,
        0,
        AppSpace.lg,
        AppSpace.lg,
      ),
      child: PixelPanel(
        padding: const EdgeInsets.fromLTRB(
          AppSpace.md,
          AppSpace.sm,
          AppSpace.md,
          AppSpace.md,
        ),
        child: Column(
          children: [
            // Smooth animated download-rate graph over the last ~30s.
            Padding(
              padding: const EdgeInsets.only(bottom: AppSpace.sm),
              child: SpeedGraph(
                samples: history,
                color: connected ? PxColors.success : PxColors.accent,
              ),
            ),
            Row(
              children: [
                _statCol(
                  Icons.south,
                  'Download',
                  formatSpeed(down),
                  PxColors.accent,
                ),
                _statCol(
                  Icons.north,
                  'Upload',
                  formatSpeed(up),
                  PxColors.success,
                ),
                _statCol(
                  Icons.network_ping,
                  'Ping',
                  ping == null ? '—' : '$ping ms',
                  PxColors.textPrimary,
                ),
              ],
            ),
            // One terminal-style footer line: session totals, plus PROTECTED
            // once the route is verified. That badge confirms traffic is routing
            // WITHOUT revealing the server IP (a disguise/security leak) — exitIp
            // is non-null only when verified, so it just gates the badge; the
            // address itself is never displayed or copied.
            if (connected) ...[
              const SizedBox(height: AppSpace.sm),
              Row(
                mainAxisAlignment: MainAxisAlignment.center,
                children: [
                  Flexible(
                    child: Text(
                      'SESSION  ↓ ${formatBytes(s.sessionDownBytes)}'
                      '  ↑ ${formatBytes(s.sessionUpBytes)}',
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: PxText.label,
                    ),
                  ),
                  if (exitIp != null && exitIp.isNotEmpty) ...[
                    Text('  ·  ', style: PxText.label),
                    const Icon(Icons.verified_user_outlined,
                        size: 12, color: PxColors.success),
                    const SizedBox(width: 4),
                    Text(
                      'PROTECTED',
                      style: PxText.label.copyWith(color: PxColors.success),
                    ),
                  ],
                ],
              ),
            ],
          ],
        ),
      ),
    );
  }

  /// Color-coded connection-quality pill (signal bars + label) derived from the
  /// live ping. Shown only while connected.
  Widget _qualityChip(int? pingMs) {
    final q = QualityInfo.fromPing(pingMs);
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: AppSpace.md, vertical: 5),
      decoration: BoxDecoration(
        color: q.color.withValues(alpha: 0.12),
        border: Border.all(color: q.color.withValues(alpha: 0.5), width: 2),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          _signalBars(q.bars, q.color),
          const SizedBox(width: AppSpace.sm),
          Text(
            q.label,
            style: TextStyle(
              color: q.color,
              fontSize: 12,
              fontWeight: FontWeight.w600,
              letterSpacing: 0.3,
            ),
          ),
        ],
      ),
    );
  }

  Widget _signalBars(int filled, Color color) {
    return Row(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.end,
      children: List.generate(3, (i) {
        final on = i < filled;
        return Padding(
          padding: EdgeInsets.only(right: i < 2 ? 2 : 0),
          child: Container(
            width: 3.5,
            height: 6.0 + i * 3.0,
            decoration: BoxDecoration(
              color: on ? color : color.withValues(alpha: 0.22),
              borderRadius: BorderRadius.circular(1),
            ),
          ),
        );
      }),
    );
  }

  Widget _statCol(IconData icon, String label, String value, Color color) =>
      Expanded(
        child: Column(
          children: [
            Icon(icon, color: color, size: 18),
            const SizedBox(height: AppSpace.sm),
            Text(
              value,
              style: TextStyle(
                color: color,
                fontSize: 16,
                fontWeight: FontWeight.w600,
                letterSpacing: -0.2,
              ),
            ),
            const SizedBox(height: 3),
            Text(label.toUpperCase(), style: PxText.label),
          ],
        ),
      );
}

/// Optional in-app updater dialog. Walks the backend's download → verify →
/// install state machine, showing a progress bar, and quits the app once the
/// new build has been handed off (so its window can come up in our place).
class _UpdateDialog extends StatefulWidget {
  final ApiClient api;
  final UpdateInfo info;
  const _UpdateDialog({required this.api, required this.info});

  @override
  State<_UpdateDialog> createState() => _UpdateDialogState();
}

class _UpdateDialogState extends State<_UpdateDialog> {
  bool _busy = false; // installer launch in flight
  String? _error;

  Future<void> _install() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    bool ok = false;
    try {
      ok = await widget.api.applyUpdate();
    } catch (_) {
      ok = false;
    }
    if (!mounted) return;
    if (ok) {
      // The OS installer wizard is now open; quit so it can replace the app.
      await Future<void>.delayed(const Duration(milliseconds: 700));
      await windowManager.destroy();
    } else {
      setState(() {
        _busy = false;
        _error =
            "Couldn't open the installer. If you're updating from the USB, "
            'check it is still plugged in, then try again.';
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final u = widget.info;
    return pixelTypography(
      context,
      child: Dialog(
        backgroundColor: PxColors.surface,
        surfaceTintColor: Colors.transparent,
        elevation: 0,
        insetPadding: const EdgeInsets.all(28),
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(PxRadius.sm),
          side: const BorderSide(color: PxColors.frame, width: 1),
        ),
        child: Container(
          width: 460,
          padding: const EdgeInsets.all(AppSpace.lg),
          decoration: BoxDecoration(
            borderRadius: BorderRadius.circular(PxRadius.sm),
            boxShadow: crtGlow(PxColors.frame, alpha: 0.14, blur: 18),
          ),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                '> UPDATE AVAILABLE  v${u.latest}',
                style: TextStyle(
                  fontFamily: PxFont.hero,
                  fontWeight: FontWeight.w700,
                  fontSize: 15,
                  letterSpacing: 1.5,
                  color: PxColors.accent,
                  shadows: glowText(PxColors.accent, alpha: 0.3),
                ),
              ),
              const SizedBox(height: AppSpace.sm),
              Container(height: 1, color: PxColors.frameInset),
              const SizedBox(height: AppSpace.md),
              Text(
                _busy
                    ? "> opening installer…\n> follow the prompts (you'll be asked for your "
                        'password), then reopen.'
                    : '> current:   v${u.current}\n> available: v${u.latest}\n\n'
                        '> install from USB? this opens the installer and asks for '
                        'your password.',
                style: const TextStyle(
                  fontFamily: PxFont.body,
                  fontSize: 13,
                  height: 1.5,
                  color: PxColors.textMuted,
                ),
              ),
              if (_error != null) ...[
                const SizedBox(height: AppSpace.sm),
                Text(
                  '> error: ${_error!}',
                  style: const TextStyle(
                    fontFamily: PxFont.body,
                    fontSize: 12,
                    color: PxColors.warning,
                  ),
                ),
              ],
              const SizedBox(height: AppSpace.lg),
              Row(
                mainAxisAlignment: MainAxisAlignment.end,
                children: [
                  if (!_busy) ...[
                    _TermButton(
                      label: 'LATER',
                      onPressed: () => Navigator.of(context).pop(),
                    ),
                    const SizedBox(width: AppSpace.lg),
                  ],
                  SizedBox(
                    width: 170,
                    child: PixelButton(
                      label: 'Install',
                      onPressed: _busy ? null : _install,
                      child: _busy
                          ? Row(
                              mainAxisAlignment: MainAxisAlignment.center,
                              children: [
                                const SizedBox(
                                  width: 15,
                                  height: 15,
                                  child: CircularProgressIndicator(
                                    strokeWidth: 2,
                                    color: PxColors.successBright,
                                  ),
                                ),
                                const SizedBox(width: AppSpace.sm),
                                Text(
                                  'INSTALLING',
                                  style: TextStyle(
                                    fontFamily: PxFont.hero,
                                    fontWeight: FontWeight.w700,
                                    fontSize: 13,
                                    letterSpacing: 1,
                                    color: PxColors.successBright,
                                  ),
                                ),
                              ],
                            )
                          : null,
                    ),
                  ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// A terminal-style text button ("[← BACK]") that dims when idle and glows
/// bright green on hover — used for the VPN top bar commands.
/// The DIAG dialog body: runs the engine's leak test on open (a few seconds of
/// real network probes, off the UI isolate) and prints one line per check.
class _LeakTestPanel extends StatefulWidget {
  final ApiClient api;
  const _LeakTestPanel({required this.api});
  @override
  State<_LeakTestPanel> createState() => _LeakTestPanelState();
}

class _LeakTestPanelState extends State<_LeakTestPanel> {
  bool _running = false;
  bool _failed = false;
  bool _connected = false;
  List<Map<String, dynamic>> _checks = const [];

  @override
  void initState() {
    super.initState();
    _run();
  }

  Future<void> _run() async {
    setState(() {
      _running = true;
      _failed = false;
    });
    try {
      final r = await widget.api.leakTest();
      if (!mounted) return;
      setState(() {
        _connected = r['connected'] == true;
        _checks = ((r['checks'] as List?) ?? const [])
            .whereType<Map>()
            .map((m) => Map<String, dynamic>.from(m))
            .toList();
        _running = false;
      });
    } catch (_) {
      if (mounted) {
        setState(() {
          _running = false;
          _failed = true;
        });
      }
    }
  }

  static (String, Color) _tag(String level) => switch (level) {
    'pass' => ('PASS', PxColors.success),
    'warn' => ('WARN', PxColors.warning),
    'fail' => ('FAIL', PxColors.danger),
    'info' => ('INFO', PxColors.textMuted),
    _ => (' -- ', PxColors.mutedDeep),
  };

  (String, Color) get _verdict {
    final fails = _checks.where((c) => c['level'] == 'fail').length;
    final warns = _checks.where((c) => c['level'] == 'warn').length;
    if (fails > 0) {
      return ('LEAK FOUND — $fails problem${fails == 1 ? '' : 's'}', PxColors.danger);
    }
    if (warns > 0) {
      return ('$warns WARNING${warns == 1 ? '' : 'S'} — see below', PxColors.warning);
    }
    if (!_connected) {
      return ('NOT CONNECTED — connect to test your protection', PxColors.textMuted);
    }
    return ('ALL CLEAR — no leaks found', PxColors.success);
  }

  @override
  Widget build(BuildContext context) {
    const body = TextStyle(fontFamily: PxFont.body, fontSize: 12, height: 1.4);
    return Container(
      width: 560,
      constraints: BoxConstraints(maxHeight: MediaQuery.of(context).size.height * 0.8),
      padding: const EdgeInsets.all(AppSpace.lg),
      decoration: BoxDecoration(
        borderRadius: BorderRadius.circular(PxRadius.sm),
        boxShadow: crtGlow(PxColors.frame, alpha: 0.14, blur: 18),
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            '> LEAK TEST',
            style: TextStyle(
              fontFamily: PxFont.hero,
              fontWeight: FontWeight.w700,
              fontSize: 15,
              letterSpacing: 1.5,
              color: PxColors.accent,
              shadows: glowText(PxColors.accent, alpha: 0.3),
            ),
          ),
          const SizedBox(height: AppSpace.sm),
          Container(height: 1, color: PxColors.frameInset),
          const SizedBox(height: AppSpace.md),
          if (_running)
            Row(
              children: [
                Text(
                  '> probing tunnel, DNS, IPv6',
                  style: body.copyWith(fontSize: 14, color: PxColors.textPrimary),
                ),
                const SizedBox(width: 6),
                const BlinkingCursor(color: PxColors.accent, fontSize: 14),
              ],
            )
          else if (_failed)
            Text(
              "> couldn't run the test — the app isn't responding. try again.",
              style: body.copyWith(color: PxColors.warning),
            )
          else ...[
            Builder(builder: (_) {
              final (text, color) = _verdict;
              return Text(
                '> $text',
                style: TextStyle(
                  fontFamily: PxFont.hero,
                  fontWeight: FontWeight.w700,
                  fontSize: 14,
                  letterSpacing: 1,
                  color: color,
                  shadows: glowText(color, alpha: 0.45),
                ),
              );
            }),
            const SizedBox(height: AppSpace.md),
            Flexible(
              child: SingleChildScrollView(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    for (final c in _checks)
                      Padding(
                        padding: const EdgeInsets.only(bottom: AppSpace.md),
                        child: Builder(builder: (_) {
                          final (tag, color) = _tag('${c['level']}');
                          return Row(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            children: [
                              Text(
                                '[$tag]',
                                style: body.copyWith(
                                  fontWeight: FontWeight.w700,
                                  fontSize: 13,
                                  color: color,
                                ),
                              ),
                              const SizedBox(width: 10),
                              Expanded(
                                child: Column(
                                  crossAxisAlignment: CrossAxisAlignment.start,
                                  children: [
                                    Text(
                                      '${c['title']}',
                                      style: body.copyWith(
                                        fontSize: 13,
                                        color: PxColors.textPrimary,
                                      ),
                                    ),
                                    const SizedBox(height: 2),
                                    Text(
                                      '${c['detail']}',
                                      style: body.copyWith(color: PxColors.mutedDeep),
                                    ),
                                  ],
                                ),
                              ),
                            ],
                          );
                        }),
                      ),
                  ],
                ),
              ),
            ),
          ],
          const SizedBox(height: AppSpace.sm),
          Row(
            mainAxisAlignment: MainAxisAlignment.end,
            children: [
              if (!_running) _TermButton(label: 'RUN AGAIN', onPressed: _run),
              const SizedBox(width: AppSpace.sm),
              _TermButton(label: 'CLOSE', onPressed: () => Navigator.of(context).pop()),
            ],
          ),
        ],
      ),
    );
  }
}

class _TermButton extends StatefulWidget {
  final String label;
  final VoidCallback onPressed;
  const _TermButton({required this.label, required this.onPressed});
  @override
  State<_TermButton> createState() => _TermButtonState();
}

class _TermButtonState extends State<_TermButton> {
  bool _hover = false;
  @override
  Widget build(BuildContext context) {
    final c = _hover ? PxColors.accentGlow : PxColors.textMuted;
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      child: GestureDetector(
        onTap: widget.onPressed,
        behavior: HitTestBehavior.opaque,
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 4),
          child: Text(
            '[${widget.label}]',
            style: TextStyle(
              fontFamily: PxFont.body,
              fontWeight: FontWeight.w700,
              fontSize: 13,
              letterSpacing: 1,
              color: c,
              shadows: _hover ? glowText(PxColors.accent, alpha: 0.6) : null,
            ),
          ),
        ),
      ),
    );
  }
}

/// Cycling "...", ".", "..", "..." dots for the CONNECTING line.
class _CyclingDots extends StatefulWidget {
  final Color color;
  const _CyclingDots({required this.color});
  @override
  State<_CyclingDots> createState() => _CyclingDotsState();
}

class _CyclingDotsState extends State<_CyclingDots> {
  Timer? _t;
  int _n = 0;
  @override
  void initState() {
    super.initState();
    _t = Timer.periodic(const Duration(milliseconds: 400), (_) {
      if (mounted) setState(() => _n = (_n + 1) % 4);
    });
  }

  @override
  void dispose() {
    _t?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => SizedBox(
    // Fixed width + padded to a constant 3 chars + no wrap, so the dots never
    // reflow the line (which used to bump the whole screen down on the 3rd dot).
    width: 38,
    child: Text(
      ('.' * _n).padRight(3),
      maxLines: 1,
      softWrap: false,
      overflow: TextOverflow.visible,
      style: TextStyle(
        fontFamily: PxFont.body,
        fontWeight: FontWeight.w700,
        fontSize: 16,
        letterSpacing: 1,
        color: widget.color,
        shadows: glowText(widget.color, alpha: 0.4),
      ),
    ),
  );
}

/// A playful paper sticky-note that shows data usage against the plan cap: a
/// usage bar (green → amber → red), the figures, and a clear "limit reached"
/// state. It gently sways; when the cap is hit it switches to an urgent red
/// nudge so the customer actually notices before they get cut off.
class _DataStickyNote extends StatefulWidget {
  final int usedBytes;
  final int limitBytes;
  const _DataStickyNote({required this.usedBytes, required this.limitBytes});

  @override
  State<_DataStickyNote> createState() => _DataStickyNoteState();
}

class _DataStickyNoteState extends State<_DataStickyNote>
    with SingleTickerProviderStateMixin {
  late final AnimationController _sway;

  // Sticky-note paper + ink (deliberately light "physical note" colours that
  // pop against the dark CRT backdrop).
  static const _paper = Color(0xFFF4E27A);
  static const _paperWarn = Color(0xFFF2C14E);
  static const _paperOver = Color(0xFFEBA39A);
  static const _ink = Color(0xFF2A2410);
  static const _green = Color(0xFF2E9E5B);
  static const _amber = Color(0xFFB9770E);

  @override
  void initState() {
    super.initState();
    _sway = AnimationController(
      vsync: this,
      duration: const Duration(seconds: 4),
    )..repeat(reverse: true);
  }

  @override
  void dispose() {
    _sway.dispose();
    super.dispose();
  }

  double get _fraction {
    if (widget.limitBytes <= 0) return 0;
    final f = widget.usedBytes / widget.limitBytes;
    return f.isNaN ? 0 : f.clamp(0.0, 1.0);
  }

  @override
  Widget build(BuildContext context) {
    final frac = _fraction;
    final reached = widget.usedBytes >= widget.limitBytes;
    final near = frac >= 0.9;
    const red = Color(0xFFB53020);
    final fillColor = reached || near ? red : (frac >= 0.75 ? _amber : _green);
    final paper = reached ? _paperOver : (near ? _paperWarn : _paper);
    final int remaining =
        (widget.limitBytes - widget.usedBytes).clamp(0, widget.limitBytes).toInt();

    return AnimatedBuilder(
      animation: _sway,
      builder: (context, child) {
        // Gentle idle sway; a slightly stronger, faster rock once the cap is hit
        // so the note visibly "nags".
        final t = _sway.value * 2 * math.pi;
        final angle =
            reached ? math.sin(t * 2) * 0.035 : math.sin(t) * 0.016;
        return Transform.rotate(angle: angle, child: child);
      },
      child: Stack(
        clipBehavior: Clip.none,
        children: [
          Container(
            width: double.infinity,
            padding: const EdgeInsets.fromLTRB(16, 18, 16, 14),
            decoration: BoxDecoration(
              color: paper,
              borderRadius: BorderRadius.circular(3),
              border: reached
                  ? Border.all(color: red, width: 2)
                  : Border.all(color: Colors.black.withValues(alpha: 0.08)),
              boxShadow: [
                BoxShadow(
                  color: Colors.black.withValues(alpha: 0.45),
                  blurRadius: 14,
                  offset: const Offset(3, 6),
                ),
                if (reached)
                  BoxShadow(
                    color: red.withValues(alpha: 0.35),
                    blurRadius: 18,
                    spreadRadius: 1,
                  ),
              ],
            ),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              mainAxisSize: MainAxisSize.min,
              children: [
                Row(
                  mainAxisAlignment: MainAxisAlignment.spaceBetween,
                  children: [
                    Text(
                      'DATA PLAN',
                      style: TextStyle(
                        fontFamily: PxFont.body,
                        fontWeight: FontWeight.w700,
                        fontSize: 12,
                        letterSpacing: 2,
                        color: _ink.withValues(alpha: 0.7),
                      ),
                    ),
                    Text(
                      '${(frac * 100).round()}%',
                      style: TextStyle(
                        fontFamily: PxFont.body,
                        fontWeight: FontWeight.w700,
                        fontSize: 13,
                        color: reached || near ? red : _ink,
                      ),
                    ),
                  ],
                ),
                const SizedBox(height: 6),
                Text(
                  '${formatBytes(widget.usedBytes)} / ${formatBytes(widget.limitBytes)}',
                  style: const TextStyle(
                    fontFamily: PxFont.body,
                    fontWeight: FontWeight.w700,
                    fontSize: 20,
                    color: _ink,
                  ),
                ),
                const SizedBox(height: 8),
                // Usage bar.
                ClipRRect(
                  borderRadius: BorderRadius.circular(4),
                  child: Container(
                    height: 9,
                    color: Colors.black.withValues(alpha: 0.14),
                    child: Align(
                      alignment: Alignment.centerLeft,
                      child: FractionallySizedBox(
                        widthFactor: frac == 0 ? 0.02 : frac,
                        child: Container(color: fillColor),
                      ),
                    ),
                  ),
                ),
                const SizedBox(height: 8),
                Text(
                  reached
                      ? '⚠ LIMIT REACHED — you may be cut off until it resets.'
                      : near
                          ? 'Heads up — only ${formatBytes(remaining)} left.'
                          : '${formatBytes(remaining)} left this cycle.',
                  style: TextStyle(
                    fontFamily: PxFont.body,
                    fontWeight: reached ? FontWeight.w700 : FontWeight.w500,
                    fontSize: 12,
                    height: 1.3,
                    color: reached || near ? red : _ink.withValues(alpha: 0.75),
                  ),
                ),
              ],
            ),
          ),
          // A strip of "tape" across the top to sell the sticky-note look.
          Positioned(
            top: -7,
            left: 0,
            right: 0,
            child: Center(
              child: Transform.rotate(
                angle: -0.05,
                child: Container(
                  width: 74,
                  height: 16,
                  color: Colors.white.withValues(alpha: 0.28),
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }
}
