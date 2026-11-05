import 'dart:async' show Timer, unawaited;
import 'dart:io' show Directory, File, Platform, exit;

import 'package:flutter/material.dart';
import 'package:provider/provider.dart';
import 'package:window_manager/window_manager.dart';

import 'api.dart';
import 'config.dart';
import 'engine.dart';
import 'util/skins.dart';
import 'screens/password_gate.dart';
import 'screens/tray_panel.dart';
import 'screens/vpn_screen.dart';
import 'screens/weather_screen.dart';
import 'services/panic_hotkey.dart';
import 'services/tray_panel_controller.dart';
import 'services/tray_window.dart';
import 'services/widget_bridge.dart';
import 'state/connection_state.dart';
import 'state/weather_state.dart';
import 'theme.dart';
import 'util/app_paths.dart';
import 'util/linux_desktop.dart';
import 'util/single_instance.dart';
import 'widgets/panic_escape.dart';

/// Touched by the boss-key desktop shortcut (Wayland) or `--panic`; the running
/// app watches for it. Must match the engine's `shortcut::TRIGGER_FILE`.
const _bossTriggerName = 'panic.req';

Future<void> main(List<String> args) async {
  // `dreamnail_ui --panic`: poke the running instance's boss key and leave —
  // before any window exists, so nothing flashes on screen.
  if (args.contains('--panic')) {
    try {
      Directory(AppPaths.dataDir()).createSync(recursive: true);
      File(AppPaths.file(_bossTriggerName)).writeAsStringSync('1');
    } catch (_) {}
    exit(0);
  }
  WidgetsFlutterBinding.ensureInitialized();
  loadSkin(); // apply the saved weather skin before first paint
  await windowManager.ensureInitialized();
  // A second instance would fight over the engine actor / proxy — refuse to race.
  if (!acquireSingleInstanceLock()) {
    const opts = WindowOptions(
      size: Size(380, 240),
      center: true,
      title: 'Weather',
    );
    await windowManager.waitUntilReadyToShow(opts, () async {
      await windowManager.show();
      await windowManager.focus();
    });
    runApp(const AlreadyRunningApp());
    return;
  }
  await installLinuxDesktopEntry();
  const opts = WindowOptions(
    size: TrayWindowService.fullSize,
    center: true,
    title: 'Weather',
  );
  await windowManager.waitUntilReadyToShow(opts, () async {
    await windowManager.show();
    await windowManager.focus();
  });
  // Closing the window HIDES it (keeping the tunnel alive in the tray), never
  // quits — but prevent-close is enabled ONLY once the tray actually initialises
  // (see TrayWindowService.init). If the tray fails to come up (headless, or a
  // Linux desktop with no appindicator/StatusNotifier), we must NOT hijack the
  // close button, or closing would hide the window with no tray to restore it —
  // leaving the app running and invisible. Real exit is the tray "Quit" (_quit).
  runApp(const DreamnailApp());
}

/// Shown when another instance holds the lock — worded for the weather disguise.
class AlreadyRunningApp extends StatelessWidget {
  const AlreadyRunningApp({super.key});
  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      debugShowCheckedModeBanner: false,
      theme: buildTheme(),
      home: Scaffold(
        backgroundColor: AppColors.bg,
        body: Center(
          child: Padding(
            padding: const EdgeInsets.all(AppSpace.xl),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                const Icon(
                  Icons.nightlight_round,
                  color: AppColors.accent,
                  size: 44,
                ),
                const SizedBox(height: AppSpace.lg),
                const Text(
                  'Weather is already open',
                  style: TextStyle(color: AppColors.textPrimary, fontSize: 16),
                ),
                const SizedBox(height: AppSpace.xs),
                const Text(
                  'Check your system tray or taskbar.',
                  style: TextStyle(color: AppColors.textMuted, fontSize: 13),
                ),
                const SizedBox(height: AppSpace.lg),
                FilledButton(
                  style: FilledButton.styleFrom(
                    backgroundColor: AppColors.accent,
                  ),
                  onPressed: () => windowManager.destroy(),
                  child: const Text('OK'),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// The reveal transition from the weather disguise to the hidden layer: two soft
/// cloud banks part down the middle to uncover the page beneath, which fades and
/// gently zooms in. Purely cosmetic, driven by the route's own animation.
class _CloudPartReveal extends StatelessWidget {
  final Animation<double> animation;
  final Widget child;
  const _CloudPartReveal({required this.animation, required this.child});

  @override
  Widget build(BuildContext context) {
    final t = CurvedAnimation(parent: animation, curve: Curves.easeInOutCubic);
    return AnimatedBuilder(
      animation: t,
      builder: (ctx, _) {
        final v = t.value; // 0 → 1
        final w = MediaQuery.of(ctx).size.width;
        final part = Curves.easeInOut.transform(v); // how far the clouds opened
        return Stack(
          fit: StackFit.expand,
          children: [
            // The revealed hidden layer: fade + a gentle zoom-in.
            Opacity(
              opacity: Curves.easeIn.transform(v).clamp(0.0, 1.0),
              child: Transform.scale(scale: 0.95 + 0.05 * v, child: child),
            ),
            // Two cloud banks sliding apart and thinning out.
            IgnorePointer(
              child: Opacity(
                opacity: (1 - part).clamp(0.0, 1.0),
                child: Stack(
                  fit: StackFit.expand,
                  children: [
                    Transform.translate(
                      offset: Offset(-w * 0.6 * part, 0),
                      child: const Align(
                        alignment: Alignment.centerLeft,
                        child: FractionallySizedBox(
                          widthFactor: 0.6,
                          heightFactor: 1,
                          child: _CloudBank(fromLeft: true),
                        ),
                      ),
                    ),
                    Transform.translate(
                      offset: Offset(w * 0.6 * part, 0),
                      child: const Align(
                        alignment: Alignment.centerRight,
                        child: FractionallySizedBox(
                          widthFactor: 0.6,
                          heightFactor: 1,
                          child: _CloudBank(fromLeft: false),
                        ),
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ],
        );
      },
    );
  }
}

/// One half of the parting clouds — a soft lilac-white haze that's dense at the
/// centre seam and fades to the outer edge, with puffy blobs along the seam for a
/// cloud silhouette. Colours sit in the app's violet family.
class _CloudBank extends StatelessWidget {
  final bool fromLeft;
  const _CloudBank({required this.fromLeft});

  @override
  Widget build(BuildContext context) {
    return Stack(
      fit: StackFit.expand,
      children: [
        DecoratedBox(
          decoration: BoxDecoration(
            gradient: LinearGradient(
              begin: fromLeft ? Alignment.centerRight : Alignment.centerLeft,
              end: fromLeft ? Alignment.centerLeft : Alignment.centerRight,
              colors: const [
                Color(0xF2F3F1FF),
                Color(0xCCEDEBFF),
                Color(0x00EDEBFF),
              ],
              stops: const [0.0, 0.5, 1.0],
            ),
          ),
        ),
        ..._puffs(),
      ],
    );
  }

  List<Widget> _puffs() {
    const n = 7;
    return List.generate(n, (i) {
      final frac = (i + 0.5) / n;
      final size = 120.0 + (i.isEven ? 60.0 : 0.0);
      return Align(
        alignment: Alignment(fromLeft ? 1.0 : -1.0, frac * 2 - 1),
        child: Container(
          width: size,
          height: size,
          decoration: const BoxDecoration(
            shape: BoxShape.circle,
            gradient: RadialGradient(
              colors: [Color(0xFFF4F2FF), Color(0x00F4F2FF)],
              stops: [0.55, 1.0],
            ),
          ),
        ),
      );
    });
  }
}

class DreamnailApp extends StatefulWidget {
  const DreamnailApp({super.key});
  @override
  State<DreamnailApp> createState() => _DreamnailAppState();
}

class _DreamnailAppState extends State<DreamnailApp>
    with WidgetsBindingObserver, WindowListener {
  final _navKey = GlobalKey<NavigatorState>();
  // The engine is in-process now — no async backend boot, no fail screen.
  late final ApiClient _api = ApiClient(Engine.open());
  late final WeatherState _weather = WeatherState(_api)..start();
  late final ConnectionController _conn = ConnectionController(_api)..start();
  TrayWindowService? _tray;
  final TrayPanelController _panel = TrayPanelController();
  bool _visible = true;
  Timer? _bossWatch;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    windowManager.addListener(this);
    if (Platform.isMacOS) {
      _conn.addListener(_syncWidget);
      _syncWidget();
    }
    _initTray();
    _initBossKey();
    unawaited(_primeNetworkWatcher());
  }

  // --- boss key (Ctrl+Alt+Esc) ---------------------------------------------
  // Two routes to the same action, always on:
  //  * the OS-level global shortcut (Windows, macOS, X11), and
  //  * on Wayland, a desktop shortcut (set up from Settings) or `--panic` that
  //    touches a trigger file, which we poll for here — a stat every 350ms.
  void _initBossKey() {
    final trigger = File(AppPaths.file(_bossTriggerName));
    // A press from before we started must not fire later.
    try {
      if (trigger.existsSync()) trigger.deleteSync();
    } catch (_) {}
    unawaited(PanicHotkey.instance.enable(_bossKey));
    _bossWatch = Timer.periodic(const Duration(milliseconds: 350), (_) {
      try {
        if (!trigger.existsSync()) return;
        trigger.deleteSync();
      } catch (_) {
        return;
      }
      _bossKey();
    });
  }

  /// Snap back to the weather disguise (re-locking, like panic) and get the
  /// window off the screen: hidden to the tray when there is one, else minimised.
  Future<void> _bossKey() async {
    _panicToWeather();
    if (_tray != null) {
      await windowManager.hide();
    } else {
      await windowManager.minimize();
    }
  }

  /// "Auto-connect on untrusted networks" needs the remembered login cached in
  /// the engine even if nobody opens the VPN screen this session (the app often
  /// just sits in the tray). When the feature is on, unlock once at launch —
  /// which primes the engine's watcher — and lock straight away again, so the
  /// hidden layer stays locked exactly as before.
  Future<void> _primeNetworkWatcher() async {
    try {
      if (!File(AppPaths.file('netwatch.pref')).existsSync()) return;
      await _api.unlock(AppConfig.launchPassword);
      await _api.lock();
    } catch (_) {}
  }

  // Tray + hide-to-tray are best-effort: a headless/unsupported environment must
  // not block the app from coming up.
  Future<void> _initTray() async {
    try {
      final tray = TrayWindowService(onQuit: _quit, panel: _panel);
      await tray.init();
      _tray = tray;
      _conn.addListener(_syncTray);
    } catch (_) {
      _tray = null;
    }
  }

  void _syncTray() => _tray?.setConnected(_conn.status.connected);

  String? _lastWidgetKey;
  void _syncWidget() {
    final s = _conn.status;
    final key =
        '${s.connected}|${s.connecting}|${s.uptimeSeconds}|'
        '${s.downloadBps}|${s.uploadBps}|${s.pingMs}';
    if (key == _lastWidgetKey) return;
    _lastWidgetKey = key;
    WidgetBridge.update(s);
  }

  /// Real exit (tray "Quit"): drop the tunnel, then close for good.
  Future<void> _quit() async {
    if (Platform.isMacOS) await WidgetBridge.clear();
    try {
      await _api.disconnect(); // restore the system proxy before quitting
    } catch (_) {}
    await _tray?.dispose();
    await windowManager.destroy();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    // Never tear down on lifecycle changes (macOS reports `detached` when hidden
    // to the menu bar); just pause on-screen polling and keep the tunnel up.
    _setVisible(
      state == AppLifecycleState.resumed || state == AppLifecycleState.inactive,
    );
  }

  void _setVisible(bool visible) {
    if (visible == _visible) return;
    setState(() => _visible = visible);
    if (visible) {
      _conn.resume();
      _weather.resume();
    } else {
      _conn.pause();
      _weather.pause();
    }
  }

  void _onFocusChanged(bool focused) {
    if (focused) {
      _api.setFocus(true);
      _conn.resume();
    } else {
      _api.setFocus(false);
      _conn.slowPoll();
    }
  }

  @override
  void onWindowFocus() => _onFocusChanged(true);
  @override
  void onWindowBlur() => _onFocusChanged(false);

  @override
  void onWindowClose() async {
    // Red close → hide to tray, keep the VPN running; real exit is tray "Quit".
    if (await windowManager.isPreventClose()) {
      await windowManager.hide();
    }
  }

  @override
  void dispose() {
    _bossWatch?.cancel();
    unawaited(PanicHotkey.instance.disable());
    WidgetsBinding.instance.removeObserver(this);
    windowManager.removeListener(this);
    _conn.removeListener(_syncTray);
    _conn.removeListener(_syncWidget);
    _conn.dispose();
    _weather.dispose();
    _tray?.dispose();
    _panel.dispose();
    super.dispose();
  }

  // --- navigation (weather → gate → vpn) ---------------------------------
  void _pushGate() {
    _navKey.currentState?.push(
      PageRouteBuilder(
        // A touch longer so the "clouds part" reveal has room to breathe.
        transitionDuration: const Duration(milliseconds: 650),
        pageBuilder: (_, _, _) => PasswordGate(
          onUnlocked: _pushVpn,
          onBack: () => _navKey.currentState?.pop(),
        ),
        transitionsBuilder: (_, anim, _, child) =>
            _CloudPartReveal(animation: anim, child: child),
      ),
    );
  }

  void _pushVpn() {
    _navKey.currentState?.pushReplacement(
      PageRouteBuilder(
        transitionDuration: const Duration(milliseconds: 350),
        pageBuilder: (_, _, _) => VpnScreen(onBack: _toWeather),
        transitionsBuilder: (_, anim, _, child) => FadeTransition(
          opacity: CurvedAnimation(parent: anim, curve: Curves.easeInOutCubic),
          child: child,
        ),
      ),
    );
  }

  /// Back to the weather disguise from anywhere in the hidden layer, closing any
  /// popup (CONFIG, LOGS, dialogs) on the way — a plain pop() only closed the
  /// topmost one, so the global shortcut or auto-lock could leave you on the
  /// VPN screen. The tunnel keeps running.
  void _toWeather() => _navKey.currentState?.popUntil((r) => r.isFirst);

  /// Double-Esc panic (and the boss key): re-lock the store and go back to the
  /// weather disguise, exactly as the VPN screen's own panic does.
  void _panicToWeather() {
    unawaited(_conn.lock());
    _toWeather();
  }

  Future<void> _panelConnect() async {
    await _tray?.openFull();
    _pushGate();
  }

  @override
  Widget build(BuildContext context) {
    return MultiProvider(
      providers: [
        Provider<ApiClient>.value(value: _api),
        ChangeNotifierProvider.value(value: _weather),
        ChangeNotifierProvider.value(value: _conn),
      ],
      child: PanicEscape(
        navigatorKey: _navKey,
        onPanic: _panicToWeather,
        child: MaterialApp(
          debugShowCheckedModeBanner: false,
          theme: buildTheme(),
          navigatorKey: _navKey,
          home: WeatherScreen(onReveal: _pushGate),
          builder: (context, child) => TickerMode(
            enabled: _visible,
            child: AnimatedBuilder(
              animation: _panel,
              builder: (context, _) => Stack(
                children: [
                  ?child,
                  if (_panel.visible)
                    Positioned.fill(
                      child: Material(
                        color: Colors.transparent,
                        child: TrayPanel(
                          onClose: () => _tray?.hidePanel(),
                          onConnect: _panelConnect,
                        ),
                      ),
                    ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}
