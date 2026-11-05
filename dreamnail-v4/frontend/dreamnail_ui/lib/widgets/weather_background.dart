import 'dart:async';
import 'dart:math';

import 'package:flutter/material.dart';

import '../theme.dart';
import '../util/skins.dart';
import '../util/sky_cycle.dart';

/// Full-screen weather atmosphere. Every condition is a mood of the same
/// indigo→violet sky (see [AppColors]) so the disguise, gate and VPN screens
/// read as one continuous surface. On top of the per-condition ambient layer,
/// the whole sky runs a smooth **day/night cycle driven by the real current
/// time in Sydney** ([SkyPalette]): the gradient warms at dawn/dusk, lifts at
/// midday and deepens at night, the sun and moon arc across on their real
/// schedule, and the stars fade in and out with them.
///
/// Ambient layer per condition (kept cheap — runs constantly behind a
/// RepaintBoundary): clear → sun/moon + stars; clouds → drifting parallax
/// clouds; rain → two depths of angled rain; snow → gently swaying snow.
class WeatherBackground extends StatefulWidget {
  final String icon; // clear | clouds | rain | snow
  final bool isDay;
  final Widget child;

  /// Sunrise/sunset as day fractions (0..1), parsed from the weather model when
  /// available; null falls back to sensible Sydney defaults.
  final double? sunrise;
  final double? sunset;

  /// Pins the day/night cycle to a fixed instant instead of the live clock.
  /// Used by the gallery/golden so its render is deterministic (and so its
  /// CLEAR·DAY / CLEAR·NIGHT panels actually show day vs night).
  final DateTime? fixedTimeUtc;

  const WeatherBackground({
    super.key,
    required this.icon,
    required this.isDay,
    required this.child,
    this.sunrise,
    this.sunset,
    this.fixedTimeUtc,
  });

  @override
  State<WeatherBackground> createState() => _WeatherBackgroundState();
}

class _WeatherBackgroundState extends State<WeatherBackground>
    with SingleTickerProviderStateMixin, WidgetsBindingObserver {
  late final AnimationController _ctrl;
  late SkyPalette _sky;
  Timer? _clock;

  @override
  void initState() {
    super.initState();
    // One slow master clock (0..1 every 12s) for ambient motion (rain multiplies it).
    _ctrl = AnimationController(
      vsync: this,
      duration: const Duration(seconds: 12),
    )..repeat();
    WidgetsBinding.instance.addObserver(this);
    activeSkin.addListener(_onSkinChanged);
    _sky = _computeSky();
    // The sun/moon arc slowly; recompute the palette from real Sydney time every
    // 30s (imperceptible steps) rather than every frame. A fixed-time render
    // (gallery/golden) is static, so it needs no clock.
    if (widget.fixedTimeUtc == null) {
      _clock = Timer.periodic(const Duration(seconds: 30), (_) {
        if (mounted) setState(() => _sky = _computeSky());
      });
    }
  }

  SkyPalette _computeSky() => SkyPalette.forSydney(
    nowUtc: widget.fixedTimeUtc,
    sunrise: widget.sunrise,
    sunset: widget.sunset,
  );

  @override
  void didUpdateWidget(covariant WeatherBackground old) {
    super.didUpdateWidget(old);
    if (old.sunrise != widget.sunrise || old.sunset != widget.sunset) {
      _sky = _computeSky();
    }
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    // Don't animate the sky (and its soft-blob repaints) while the app is hidden
    // to the tray / minimised — stop the clock when not resumed, resume on return.
    final visible = state == AppLifecycleState.resumed;
    if (visible && !_ctrl.isAnimating) {
      _ctrl.repeat();
    } else if (!visible && _ctrl.isAnimating) {
      _ctrl.stop();
    }
  }

  /// Repaint when the user picks a different skin.
  void _onSkinChanged() {
    if (mounted) setState(() {});
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    activeSkin.removeListener(_onSkinChanged);
    _clock?.cancel();
    _ctrl.dispose();
    super.dispose();
  }

  bool get _clear => widget.icon == 'clear';

  /// Sky gradient. Clear sky uses the day/night palette directly; the other
  /// conditions keep their themed gradient but lift toward daylight by `dayness`.
  List<Color> get _gradient {
    if (_clear) return activeSkin.value.apply(_sky.gradient);
    final base = switch (widget.icon) {
      'rain' => const [Color(0xFF101024), Color(0xFF1B1836), Color(0xFF242142)],
      'snow' => const [Color(0xFF20223C), Color(0xFF313356), Color(0xFF42456E)],
      _ => const [Color(0xFF1A1A2B), Color(0xFF2C2B44), Color(0xFF3A3652)],
    };
    final lift = _sky.dayness * 0.16; // brighter, cooler by day
    const day = Color(0xFF8891C4);
    // The chosen skin (default = no-op) gently nudges the final gradient's hue.
    return activeSkin.value.apply([for (final c in base) Color.lerp(c, day, lift)!]);
  }

  @override
  Widget build(BuildContext context) {
    final icon = widget.icon;
    return Stack(
      fit: StackFit.expand,
      children: [
        DecoratedBox(
          decoration: BoxDecoration(
            gradient: LinearGradient(
              begin: Alignment.topCenter,
              end: Alignment.bottomCenter,
              colors: _gradient,
            ),
          ),
        ),

        // Ambient animated layer — one per condition.
        Positioned.fill(
          child: RepaintBoundary(
            child: AnimatedBuilder(
              animation: _ctrl,
              builder: (_, _) {
                final t = _ctrl.value;
                CustomPainter? painter;
                if (_clear) {
                  painter = _StarfieldPainter(t, _sky.starOpacity);
                } else if (icon == 'clouds') {
                  painter = _CloudsPainter(t);
                } else if (icon == 'rain') {
                  painter = _RainPainter(t);
                } else if (icon == 'snow') {
                  painter = _SnowPainter(t);
                }
                return painter == null
                    ? const SizedBox.shrink()
                    : CustomPaint(painter: painter);
              },
            ),
          ),
        ),

        // Top scrim over the ambient layer so header text keeps contrast when a
        // bright cloud mass or dense rain drifts behind it. Clear skies skip it
        // so the sun/moon glow stays untouched.
        if (icon != 'clear')
          const Positioned.fill(
            child: DecoratedBox(
              decoration: BoxDecoration(
                gradient: LinearGradient(
                  begin: Alignment.topCenter,
                  end: Alignment(0, -0.1),
                  colors: [Color(0x6608080F), Color(0x0008080F)],
                ),
              ),
            ),
          ),

        // Celestial bodies (clear sky), each faded by how present it is right
        // now, so they cross-fade smoothly through dawn and dusk. The sun gets a
        // gentle breath from the master clock.
        if (_clear && _sky.sunStrength > 0.02)
          Positioned.fill(
            child: AnimatedBuilder(
              animation: _ctrl,
              builder: (_, _) => _SunGlow(
                pos: _sky.sunPos,
                strength: _sky.sunStrength,
                breath: 0.5 - 0.5 * cos(_ctrl.value * 2 * pi),
              ),
            ),
          ),
        if (_clear && _sky.moonStrength > 0.02)
          Positioned.fill(
            child: _MoonGlow(pos: _sky.moonPos, strength: _sky.moonStrength),
          ),

        // Bottom scrim → ink, so cards and text always read over any sky.
        const Positioned.fill(
          child: DecoratedBox(
            decoration: BoxDecoration(
              gradient: LinearGradient(
                begin: Alignment.center,
                end: Alignment.bottomCenter,
                colors: [Color(0x00000000), Color(0x8C08080F)],
              ),
            ),
          ),
        ),

        widget.child,
      ],
    );
  }
}

/// Warm sun with a soft, slow breath. Arcs across the sky via [pos]; low on the
/// horizon (dawn/dusk) it reads warm amber, high at noon it turns pale gold.
/// Alpha scales with [strength] so it cross-fades in and out through twilight.
class _SunGlow extends StatelessWidget {
  final Alignment pos;
  final double strength; // 0..1
  final double breath; // 0..1
  const _SunGlow({
    required this.pos,
    required this.strength,
    required this.breath,
  });
  @override
  Widget build(BuildContext context) {
    final s = strength.clamp(0.0, 1.0);
    // Warmer core when the sun sits low (small strength), paler when high.
    final core = Color.lerp(
      const Color(0xFFFFC98A),
      const Color(0xFFFFF6DC),
      s,
    )!;
    final halo = Color.lerp(
      const Color(0xFFFFB06A),
      const Color(0xFFFFD98A),
      s,
    )!;
    return Stack(
      fit: StackFit.expand,
      children: [
        DecoratedBox(
          decoration: BoxDecoration(
            gradient: RadialGradient(
              center: pos,
              radius: 0.6 + 0.05 * breath,
              colors: [
                halo.withValues(alpha: (0.5 * s).clamp(0.0, 1.0)),
                halo.withValues(alpha: (0.15 * s).clamp(0.0, 1.0)),
                halo.withValues(alpha: 0.0),
              ],
              stops: const [0.0, 0.4, 1.0],
            ),
          ),
        ),
        DecoratedBox(
          decoration: BoxDecoration(
            gradient: RadialGradient(
              center: pos,
              radius: 0.13 + 0.012 * breath,
              colors: [
                core.withValues(alpha: (0.94 * s).clamp(0.0, 1.0)),
                core.withValues(alpha: 0.0),
              ],
              stops: const [0.0, 1.0],
            ),
          ),
        ),
      ],
    );
  }
}

/// Soft violet-white moon with a wide halo. Arcs via [pos]; alpha scales with
/// [strength] so it fades in after sunset and out before sunrise.
class _MoonGlow extends StatelessWidget {
  final Alignment pos;
  final double strength; // 0..1
  const _MoonGlow({required this.pos, required this.strength});
  @override
  Widget build(BuildContext context) {
    final s = strength.clamp(0.0, 1.0);
    return Stack(
      fit: StackFit.expand,
      children: [
        DecoratedBox(
          decoration: BoxDecoration(
            gradient: RadialGradient(
              center: pos,
              radius: 0.7,
              colors: [
                const Color(0xFFA78BFA).withValues(alpha: 0.3 * s),
                const Color(0xFF7C6CFF).withValues(alpha: 0.0),
              ],
              stops: const [0.0, 1.0],
            ),
          ),
        ),
        DecoratedBox(
          decoration: BoxDecoration(
            gradient: RadialGradient(
              center: pos,
              radius: 0.16,
              colors: [
                const Color(0xFFF3EEFF).withValues(alpha: 0.9 * s),
                const Color(0xFFE9E4FF).withValues(alpha: 0.0),
              ],
              stops: const [0.0, 1.0],
            ),
          ),
        ),
      ],
    );
  }
}

/// Twinkling starfield (fixed seeded positions; alpha breathes per star). A few
/// brighter stars get a soft glow halo. [globalOpacity] fades the whole field
/// with the day/night cycle so stars come out at dusk and vanish by day.
class _StarfieldPainter extends CustomPainter {
  final double t;
  final double globalOpacity;
  static const int _count = 90;
  _StarfieldPainter(this.t, this.globalOpacity);

  @override
  void paint(Canvas canvas, Size size) {
    if (globalOpacity <= 0.01) return;
    final rnd = Random(42); // stable field
    for (var i = 0; i < _count; i++) {
      final x = rnd.nextDouble() * size.width;
      // Keep stars mostly in the upper 70% (horizon glow washes out low stars).
      final y = rnd.nextDouble() * size.height * 0.72;
      final baseR = 0.5 + rnd.nextDouble() * 1.3;
      final phase = rnd.nextDouble();
      final freq = 0.5 + rnd.nextDouble() * 1.5;
      final tw = 0.5 - 0.5 * cos(2 * pi * (t * freq + phase));
      final alpha = ((0.25 + 0.6 * tw) * globalOpacity).clamp(0.0, 1.0);
      final bright = i % 18 == 0; // ~5 hero stars
      final color = Color.lerp(
        const Color(0xFFCBD2FF),
        const Color(0xFFFFFFFF),
        rnd.nextDouble(),
      )!.withValues(alpha: alpha);

      if (bright) {
        // Cheap halo (translucent disc) instead of a per-frame blur pass.
        canvas.drawCircle(
          Offset(x, y),
          baseR * 3.2,
          Paint()
            ..color = AppColors.accentGlow.withValues(
              alpha: (alpha * 0.18).clamp(0.0, 1.0),
            ),
        );
      }
      canvas.drawCircle(
        Offset(x, y),
        baseR * (bright ? 1.5 : 1.0),
        Paint()..color = color,
      );
    }
  }

  @override
  bool shouldRepaint(_StarfieldPainter old) =>
      old.t != t || old.globalOpacity != globalOpacity;
}

/// Two parallax layers of soft, blurred cloud blobs drifting sideways.
class _CloudsPainter extends CustomPainter {
  final double t;
  _CloudsPainter(this.t);

  void _layer(
    Canvas canvas,
    Size size,
    int seed,
    int count,
    double speed,
    double yBand,
    double scale,
    double alpha,
  ) {
    final rnd = Random(seed);
    // Soft blobs via a radial gradient (opaque centre → transparent edge) rather
    // than MaskFilter.blur: the blur ran a full gaussian pass per oval per frame
    // (~11 large blurs every frame) — the sky's biggest GPU cost. A gradient
    // shader gives the same soft edge in one cheap fill.
    final base = const Color(0xFFCFCBEA).withValues(alpha: alpha);
    for (var i = 0; i < count; i++) {
      final baseX = rnd.nextDouble();
      final y = (yBand + rnd.nextDouble() * 0.20) * size.height;
      final w = (150 + rnd.nextDouble() * 130) * scale;
      final x = ((baseX + t * speed) % 1.25 - 0.12) * size.width;
      final rect =
          Rect.fromCenter(center: Offset(x, y), width: w, height: w * 0.55);
      final paint = Paint()
        ..shader = RadialGradient(
          colors: [base, base.withValues(alpha: 0.0)],
          stops: const [0.0, 1.0],
        ).createShader(rect);
      canvas.drawOval(rect, paint);
    }
  }

  @override
  void paint(Canvas canvas, Size size) {
    _layer(canvas, size, 3, 5, 0.05, 0.08, 1.30, 0.11); // far, big, soft
    _layer(
      canvas,
      size,
      9,
      6,
      0.12,
      0.22,
      0.90,
      0.18,
    ); // near, smaller, brighter
  }

  @override
  bool shouldRepaint(_CloudsPainter old) => old.t != t;
}

/// Layered rain — a faint fast far layer and a brighter near layer, slight slant.
class _RainPainter extends CustomPainter {
  final double t;
  _RainPainter(this.t);

  void _layer(
    Canvas canvas,
    Size size,
    int seed,
    int count,
    double speed,
    double alpha,
    double len,
    double slant,
    double width,
  ) {
    final paint = Paint()
      ..color = const Color(0xFFAEC6FF).withValues(alpha: alpha)
      ..strokeWidth = width
      ..strokeCap = StrokeCap.round;
    final rnd = Random(seed);
    for (var i = 0; i < count; i++) {
      final x = rnd.nextDouble() * size.width;
      final sp = speed * (0.8 + rnd.nextDouble() * 0.5);
      final phase = (t * sp + rnd.nextDouble()) % 1.0;
      final y = phase * (size.height + 40) - 20;
      canvas.drawLine(Offset(x, y), Offset(x - slant, y + len), paint);
    }
  }

  @override
  void paint(Canvas canvas, Size size) {
    _layer(canvas, size, 11, 40, 3.0, 0.12, 11, 2.0, 1.0); // far
    _layer(canvas, size, 5, 34, 4.2, 0.28, 16, 3.5, 1.5); // near
  }

  @override
  bool shouldRepaint(_RainPainter old) => old.t != t;
}

/// Gentle falling snow with a slow horizontal sway.
class _SnowPainter extends CustomPainter {
  final double t;
  static const int _count = 55;
  _SnowPainter(this.t);

  @override
  void paint(Canvas canvas, Size size) {
    final rnd = Random(23);
    for (var i = 0; i < _count; i++) {
      final baseX = rnd.nextDouble();
      final r = 1.0 + rnd.nextDouble() * 2.2;
      final sp = 0.35 + rnd.nextDouble() * 0.45;
      final phase = (t * sp + rnd.nextDouble()) % 1.0;
      final y = phase * (size.height + 20) - 10;
      final sway =
          sin(2 * pi * (t + baseX) * (0.6 + rnd.nextDouble())) * 14 * (r / 3.2);
      final x = baseX * size.width + sway;
      canvas.drawCircle(
        Offset(x, y),
        r,
        Paint()
          ..color = Colors.white.withValues(alpha: 0.35 + 0.35 * (r / 3.2)),
      );
    }
  }

  @override
  bool shouldRepaint(_SnowPainter old) => old.t != t;
}
