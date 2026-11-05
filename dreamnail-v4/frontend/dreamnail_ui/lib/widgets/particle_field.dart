import 'dart:math';

import 'package:flutter/material.dart';
import 'package:flutter/scheduler.dart';

import '../theme.dart';
import '../util/repaint_notifier.dart';
import 'dreamnail_strike.dart' show NailMode;

/// A subtle drifting particle field that sits behind the Dream Nail and reacts
/// to the connection state:
///   * disconnected — slow, faint violet-white motes drifting at rest;
///   * connecting   — the field quickens and brightens as the nail is roused;
///   * connected    — motes turn success-green and settle to a calm-but-alive rise.
///
/// Speed and colour ease toward their per-mode targets rather than snapping, so
/// a state change reads as the field accelerating / shifting hue. A real physics
/// tick (positions integrated from velocity) means changing speed never makes
/// particles jump. Coordinates are normalised [0,1] so it's resolution-free.
class ParticleField extends StatefulWidget {
  final NailMode mode;
  final int count;
  const ParticleField({super.key, required this.mode, this.count = 30});

  @override
  State<ParticleField> createState() => _ParticleFieldState();
}

class _ParticleFieldState extends State<ParticleField>
    with SingleTickerProviderStateMixin, WidgetsBindingObserver {
  late final Ticker _ticker;
  final _repaint = RepaintNotifier();
  final _rnd = Random(7);
  late final List<_Particle> _particles;
  Duration _last = Duration.zero;

  // Eased state — lerps toward the per-mode targets each tick.
  double _speed = 1.0;
  double _bright = 1.0;
  Color _color = AppColors.accentGlow;

  @override
  void initState() {
    super.initState();
    _particles = List.generate(widget.count, (_) => _spawn(seedY: null));
    _ticker = createTicker(_onTick)..start();
    WidgetsBinding.instance.addObserver(this);
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    // Stop burning GPU animating a field nobody can see: when the app is hidden
    // to the tray / minimised (paused or inactive), halt the ticker; resume on
    // return. `_last` is reset so the physics step doesn't jump on resume.
    final visible = state == AppLifecycleState.resumed;
    if (visible && !_ticker.isActive) {
      _last = Duration.zero;
      _ticker.start();
    } else if (!visible && _ticker.isActive) {
      _ticker.stop();
    }
  }

  _Particle _spawn({double? seedY}) {
    final ang = _rnd.nextDouble() * 2 * pi;
    // Gentle base drift with a slight upward bias (essence rising).
    final sp = 0.008 + _rnd.nextDouble() * 0.018;
    return _Particle(
      pos: Offset(_rnd.nextDouble(), seedY ?? _rnd.nextDouble()),
      vel: Offset(cos(ang) * sp * 0.7, -sp.abs() * (0.4 + _rnd.nextDouble())),
      radius: 0.8 + _rnd.nextDouble() * 2.2,
      phase: _rnd.nextDouble() * 2 * pi,
      twFreq: 0.6 + _rnd.nextDouble() * 1.4,
    );
  }

  (double, double, Color) _targets(NailMode m) => switch (m) {
    NailMode.connected => (1.5, 1.15, AppColors.success),
    NailMode.connecting => (2.9, 1.35, AppColors.accentGlow),
    NailMode.disconnected => (1.0, 0.85, AppColors.accentGlow),
  };

  void _onTick(Duration now) {
    final dt = _last == Duration.zero
        ? 0.016
        : (now - _last).inMicroseconds / 1e6;
    _last = now;
    final clampedDt = dt.clamp(0.0, 0.05); // guard against tab-stall jumps

    final (tSpeed, tBright, tColor) = _targets(widget.mode);
    // Exponential ease toward targets (~0.3s time constant).
    final k = (clampedDt / 0.3).clamp(0.0, 1.0);
    _speed += (tSpeed - _speed) * k;
    _bright += (tBright - _bright) * k;
    _color = Color.lerp(_color, tColor, k)!;

    for (final p in _particles) {
      p.pos += p.vel * _speed * clampedDt * 60;
      p.phase += clampedDt * p.twFreq;
      // Wrap around all edges so the field is seamless.
      var x = p.pos.dx, y = p.pos.dy;
      if (x < -0.05) x += 1.1;
      if (x > 1.05) x -= 1.1;
      if (y < -0.05) y += 1.1;
      if (y > 1.05) y -= 1.1;
      p.pos = Offset(x, y);
    }
    _repaint.tick();
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _ticker.dispose();
    _repaint.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return RepaintBoundary(
      child: CustomPaint(
        painter: _ParticlePainter(
          particles: _particles,
          color: _color,
          brightness: _bright,
          repaint: _repaint,
        ),
        size: Size.infinite,
      ),
    );
  }
}

class _Particle {
  Offset pos; // normalised [0,1]
  Offset vel; // normalised units / (1/60 s)
  final double radius; // px
  double phase;
  final double twFreq;
  _Particle({
    required this.pos,
    required this.vel,
    required this.radius,
    required this.phase,
    required this.twFreq,
  });
}

class _ParticlePainter extends CustomPainter {
  final List<_Particle> particles;
  final Color color;
  final double brightness;
  _ParticlePainter({
    required this.particles,
    required this.color,
    required this.brightness,
    required Listenable repaint,
  }) : super(repaint: repaint);

  @override
  void paint(Canvas canvas, Size size) {
    // Reused Paint objects — allocating two Paints per particle per frame was
    // needless churn at 60fps. A soft mote is now a halo + core drawn with plain
    // alpha, NOT MaskFilter.blur: the blur ran a full GPU blur pass per particle
    // per frame (46× every frame), which was the bulk of this widget's GPU cost.
    final halo = Paint();
    final core = Paint();
    for (final p in particles) {
      final tw = 0.5 - 0.5 * cos(p.phase); // 0..1 twinkle
      final a = ((0.12 + 0.32 * tw) * brightness).clamp(0.0, 0.6);
      final center = Offset(p.pos.dx * size.width, p.pos.dy * size.height);
      // Halo: a larger, very translucent disc fakes the glow far more cheaply
      // than a blur filter and reads the same at this size/opacity.
      halo.color = color.withValues(alpha: a * 0.22);
      canvas.drawCircle(center, p.radius * 2.6, halo);
      core.color = color.withValues(alpha: a);
      canvas.drawCircle(center, p.radius, core);
    }
  }

  @override
  bool shouldRepaint(_ParticlePainter old) =>
      old.color != color || old.brightness != brightness;
}
