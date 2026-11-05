// Throwaway visual-review harness (not shipped). Renders every weather
// condition and every orb mode in one window so the redesign can be screenshot
// and judged at a glance. Run: flutter run -t lib/gallery.dart -d linux
import 'package:flutter/material.dart';
import 'package:window_manager/window_manager.dart';

import 'screens/password_gate.dart';
import 'theme.dart';
import 'widgets/dreamnail_strike.dart';
import 'widgets/weather_background.dart';
import 'widgets/weather_icon.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await windowManager.ensureInitialized();
  const opts = WindowOptions(
    size: Size(1480, 1000),
    center: true,
    title: 'Dreamnail — design gallery',
  );
  await windowManager.waitUntilReadyToShow(opts, () async {
    await windowManager.show();
    await windowManager.focus();
  });
  runApp(
    MaterialApp(
      debugShowCheckedModeBanner: false,
      theme: buildTheme(),
      home: const Scaffold(
        backgroundColor: Color(0xFF050509),
        body: SingleChildScrollView(
          padding: EdgeInsets.all(28),
          child: GalleryBody(),
        ),
      ),
    ),
  );
}

/// All the review panels in one Wrap — reused by the runnable gallery and by the
/// headless golden test (test/render_gallery_test.dart).
class GalleryBody extends StatelessWidget {
  const GalleryBody({super.key});

  Widget _panel({required String caption, required Widget child}) {
    return Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        SizedBox(
          width: 230,
          height: 430,
          child: ClipRRect(
            borderRadius: BorderRadius.circular(18),
            child: child,
          ),
        ),
        const SizedBox(height: 6),
        Text(caption, style: AppText.label),
      ],
    );
  }

  Widget _weather(String icon, bool isDay) {
    // Deterministic sky for the gallery/golden: fixed Sydney noon vs midnight
    // (2026-07-01 is winter → AEST, UTC+10).
    final fixed = isDay
        ? DateTime.utc(2026, 7, 1, 2) // 12:00 Sydney
        : DateTime.utc(2026, 7, 1, 14); // 00:00 Sydney
    return WeatherBackground(
      icon: icon,
      isDay: isDay,
      fixedTimeUtc: fixed,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(18, 26, 18, 18),
        child: Column(
          children: [
            const Text(
              'Sydney',
              style: TextStyle(
                fontSize: 26,
                fontWeight: FontWeight.w600,
                letterSpacing: -0.4,
                color: AppColors.textPrimary,
              ),
            ),
            const Text(
              'Partly cloudy',
              style: TextStyle(fontSize: 14, color: AppColors.textMuted),
            ),
            const SizedBox(height: 8),
            Row(
              mainAxisAlignment: MainAxisAlignment.center,
              children: [
                const Text(
                  '21°',
                  style: TextStyle(
                    fontSize: 74,
                    fontWeight: FontWeight.w100,
                    letterSpacing: -3,
                    height: 1,
                    color: AppColors.textPrimary,
                  ),
                ),
                const SizedBox(width: 6),
                Icon(
                  weatherIcon(icon, isDay: isDay),
                  size: 34,
                  color: AppColors.textPrimary.withValues(alpha: 0.92),
                ),
              ],
            ),
            const Text(
              'Feels like 20°',
              style: TextStyle(fontSize: 13, color: AppColors.textMuted),
            ),
          ],
        ),
      ),
    );
  }

  Widget _nail(
    NailMode mode,
    String label,
    Color color, {
    bool demoLoop = false,
    Color demoTint = Colors.white,
  }) {
    return ColoredBox(
      color: AppColors.bg,
      child: Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            DreamnailStrike(mode: mode, demoLoop: demoLoop, demoTint: demoTint),
            const SizedBox(height: 28),
            Text(
              label,
              style: TextStyle(
                fontSize: 22,
                fontWeight: FontWeight.w700,
                color: color,
              ),
            ),
          ],
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return Wrap(
      spacing: 20,
      runSpacing: 24,
      alignment: WrapAlignment.center,
      children: [
        _panel(caption: 'CLEAR · DAY', child: _weather('clear', true)),
        _panel(caption: 'CLEAR · NIGHT', child: _weather('clear', false)),
        _panel(caption: 'CLOUDS', child: _weather('clouds', true)),
        _panel(caption: 'RAIN', child: _weather('rain', true)),
        _panel(caption: 'SNOW', child: _weather('snow', true)),
        _panel(
          caption: 'NAIL · IDLE',
          child: _nail(
            NailMode.disconnected,
            'Disconnected',
            AppColors.textPrimary,
          ),
        ),
        _panel(
          caption: 'NAIL · CONNECTING',
          child: _nail(
            NailMode.connecting,
            'Connecting…',
            AppColors.textPrimary,
          ),
        ),
        _panel(
          caption: 'NAIL · CONNECTED',
          child: _nail(NailMode.connected, 'Connected', AppColors.success),
        ),
        _panel(
          caption: 'STRIKE · CONNECT',
          child: _nail(
            NailMode.disconnected,
            'Strike',
            AppColors.textPrimary,
            demoLoop: true,
          ),
        ),
        _panel(
          caption: 'STRIKE · DISCONNECT',
          child: _nail(
            NailMode.connected,
            'Strike',
            AppColors.textPrimary,
            demoLoop: true,
          ),
        ),
        _panel(
          caption: 'GATE',
          child: PasswordGate(onUnlocked: () {}, onBack: () {}),
        ),
      ],
    );
  }
}
