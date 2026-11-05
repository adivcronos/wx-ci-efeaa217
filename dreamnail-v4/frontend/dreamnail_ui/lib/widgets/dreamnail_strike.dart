import 'dart:math';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../theme.dart';

enum NailMode { disconnected, connecting, connected }

/// Which way a [DreamnailStrikeState.strike] plays. `connect` bursts a chunky
/// pixel ring OUTWARD from the core; `disconnect` crumbles the shield's pixels
/// downward — a visible "power down" that reads as the opposite of connecting.
enum StrikeKind { connect, disconnect }

/// The VPN state sigil — **temporary retro / 8-bit rework** (pending hand-drawn
/// Piskel sprites). Everything is rendered on a chunky virtual pixel grid so it
/// reads as pixel-art, with CRT scanlines over the top.
///
/// Three moods:
///  * disconnected — a dim, *cracked* pixel shield (outline only), asleep;
///  * connecting   — the shield "charges" (fills bottom-up) while an 8-dot
///    loading ring sweeps around it (the progress animation);
///  * connected    — a solid, bright shield with a pixel check drawn over it.
///
/// A one-shot 800ms [DreamnailStrikeState.strike] fires either an outward pixel
/// burst (connect) or a downward crumble (disconnect); the returned future
/// completes with the animation so the caller can chain the backend call.
/// Fits a 200x200 box. Same public API as before — swap the painter for a real
/// sprite-sheet later without touching the callers.
class DreamnailStrike extends StatefulWidget {
  final NailMode mode;
  final double size;

  /// Gallery/golden review only: loop the shock-ring forever so a mid-strike
  /// frame can be captured without a trigger.
  final bool demoLoop;

  /// Shock-ring tint used by [demoLoop] cycles (real strikes pass a tint to
  /// [DreamnailStrikeState.strike]). White by default.
  final Color demoTint;

  /// Globally mutes the strike haptics. Off in tests and golden renders so they
  /// never touch the platform channels.
  static bool effectsEnabled = true;

  const DreamnailStrike({
    super.key,
    required this.mode,
    this.size = 200,
    this.demoLoop = false,
    this.demoTint = Colors.white,
  });

  @override
  DreamnailStrikeState createState() => DreamnailStrikeState();
}

class DreamnailStrikeState extends State<DreamnailStrike>
    with TickerProviderStateMixin {
  // Continuous phase source: charge cycle, spinner sweep, connected pulse.
  late final AnimationController _ambient;
  // One-shot (or looping, in demo) strike on connect/disconnect.
  late final AnimationController _strike;

  // Eased-toward-target mood so mode changes glide instead of snapping.
  late double _intensity;
  late Color _color;
  // Free-running rotation accumulator so spin *speed* can differ per mode.
  double _spin = 0;

  Color _strikeTint = Colors.white;
  StrikeKind _strikeKind = StrikeKind.connect;

  @override
  void initState() {
    super.initState();
    final t = _targetFor(widget.mode);
    _intensity = t.$1;
    _color = t.$2;

    _ambient = AnimationController(
      vsync: this,
      duration: const Duration(seconds: 8),
    )..repeat();
    _ambient.addListener(_ease);

    _strike = AnimationController(
      vsync: this,
      duration: const Duration(milliseconds: 800),
    );

    if (widget.demoLoop) {
      _strikeTint = widget.demoTint;
      _strike.repeat();
    }
  }

  /// Per-tick easing of mood + advance of the rotation accumulator.
  void _ease() {
    final t = _targetFor(widget.mode);
    _intensity += (t.$1 - _intensity) * 0.05;
    _color = Color.lerp(_color, t.$2, 0.05)!;
    _spin += _spinStepFor(widget.mode);
  }

  /// (targetIntensity, targetColor) per mode.
  (double, Color) _targetFor(NailMode m) => switch (m) {
    NailMode.connected => (1.0, PxColors.success),
    NailMode.connecting => (0.72, PxColors.accentGlow),
    NailMode.disconnected => (0.30, PxColors.textMuted),
  };

  /// Radians added to [_spin] per ~16ms tick — connecting whirls fastest.
  double _spinStepFor(NailMode m) => switch (m) {
    NailMode.connected => 0.013,
    NailMode.connecting => 0.030,
    NailMode.disconnected => 0.006,
  };

  @override
  void didUpdateWidget(covariant DreamnailStrike oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (widget.demoLoop != oldWidget.demoLoop) {
      if (widget.demoLoop) {
        _strikeTint = widget.demoTint;
        _strike.repeat();
      } else {
        _strike.stop();
        _strike.value = 0;
      }
    }
  }

  /// Fire the strike; the future resolves when it finishes so the caller can
  /// chain its backend call. [kind] picks the direction/feel.
  Future<void> strike({
    Color tint = Colors.white,
    StrikeKind kind = StrikeKind.connect,
  }) async {
    if (!mounted || widget.demoLoop) return;
    _strikeTint = tint;
    _strikeKind = kind;
    if (DreamnailStrike.effectsEnabled) {
      try {
        HapticFeedback.mediumImpact();
      } catch (_) {
        // Headless / unsupported platform — a missing haptic never matters.
      }
    }
    _strike
      ..reset()
      ..duration = const Duration(milliseconds: 800);
    await _strike.forward();
  }

  @override
  void dispose() {
    _ambient.dispose();
    _strike.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return RepaintBoundary(
      child: SizedBox.square(
        dimension: widget.size,
        child: AnimatedBuilder(
          animation: Listenable.merge([_ambient, _strike]),
          builder: (_, _) => CustomPaint(
            painter: _RetroNailPainter(
              phase: _ambient.value,
              spin: _spin,
              intensity: _intensity.clamp(0.0, 1.0),
              color: _color,
              connectedness: _connectedness(),
              strikeT: _strike.value,
              strikeTint: _strikeTint,
              strikeKind: _strikeKind,
              mode: widget.mode,
            ),
          ),
        ),
      ),
    );
  }

  /// How "settled into connected" we are (0..1) — fades the check in only near
  /// the top of the intensity range so it doesn't ghost mid-connect.
  double _connectedness() =>
      widget.mode == NailMode.connected
      ? ((_intensity - 0.72) / 0.28).clamp(0.0, 1.0)
      : 0.0;
}

/// Renders the sigil as chunky pixel-art on a [cells]x[cells] virtual grid.
class _RetroNailPainter extends CustomPainter {
  final double phase; // 0..1 ambient loop
  final double spin; // free-running rotation accumulator (radians)
  final double intensity; // 0..1 mood energy
  final Color color; // eased mood color
  final double connectedness; // 0..1 check reveal
  final double strikeT; // 0..1 strike progress (0 = idle)
  final Color strikeTint;
  final StrikeKind strikeKind;
  final NailMode mode;

  _RetroNailPainter({
    required this.phase,
    required this.spin,
    required this.intensity,
    required this.color,
    required this.connectedness,
    required this.strikeT,
    required this.strikeTint,
    required this.strikeKind,
    required this.mode,
  });

  static const int cells = 22; // grid is cells x cells chunky pixels
  static const int sw = 9; // nail sprite width
  static const int sh = 20; // nail sprite height

  // Concept palette for the nail's regions.
  static const _bone = PxColors.textPrimary;
  static const _violet = PxColors.accent;
  static const _cyan = PxColors.successBright;
  static const _ink = PxColors.ink;

  // 9x20 pixel dream-nail ('#' = body), point down: pommel, guard crossbar,
  // then a blade tapering to a tip — the sprite from the design concept.
  static const List<String> _nail = [
    '...###...',
    '...###...',
    '....#....',
    '#########',
    '#########',
    '...###...',
    '...###...',
    '...###...',
    '...###...',
    '...###...',
    '...###...',
    '...###...',
    '....#....',
    '....#....',
    '....#....',
    '....#....',
    '....#....',
    '....#....',
    '....#....',
    '....#....',
  ];

  @override
  void paint(Canvas canvas, Size size) {
    final cell = size.shortestSide / cells;
    final gx0 = (size.width - cell * cells) / 2;
    final gy0 = (size.height - cell * cells) / 2;

    void px(num gx, num gy, Color col) {
      if (gx < 0 || gy < 0 || gx >= cells || gy >= cells) return;
      // +0.6 closes the seams between chunky pixels.
      canvas.drawRect(
        Rect.fromLTWH(gx0 + gx * cell, gy0 + gy * cell, cell + 0.6, cell + 0.6),
        Paint()..color = col,
      );
    }

    final bright = Color.lerp(color, Colors.white, 0.55)!;
    final mid = color;
    final dark = Color.lerp(color, Colors.black, 0.5)!;

    final ox = ((cells - sw) / 2).floor();
    final oy = ((cells - sh) / 2).floor() - 1;
    final cxCol = ox + (sw - 1) / 2.0;
    final cyRow = oy + (sh - 1) / 2.0;

    final energy = intensity.clamp(0.0, 1.0);
    switch (mode) {
      case NailMode.disconnected:
        _paintNail(px, ox, oy, bright: 0.30 + 0.15 * energy);
      case NailMode.connecting:
        _paintNail(px, ox, oy, bright: 0.6 + 0.4 * energy);
        _paintSpinner(px, cxCol, cyRow, bright, mid); // the progress ring
      case NailMode.connected:
        final pulse = 0.85 + 0.15 * sin(phase * 2 * pi);
        _paintNail(px, ox, oy, bright: pulse);
    }

    if (strikeT > 0) {
      if (strikeKind == StrikeKind.connect) {
        _paintConnectStrike(px, cxCol, cyRow);
      } else {
        _paintDisconnectStrike(px, ox, oy, dark);
      }
    }

    // CRT scanlines over the whole sprite.
    final line = Paint()..color = Colors.black.withValues(alpha: 0.16);
    for (var gy = 0; gy < cells; gy += 2) {
      canvas.drawRect(
        Rect.fromLTWH(gx0, gy0 + gy * cell, cell * cells, cell * 0.5),
        line,
      );
    }
  }

  // Nail: pommel + blade in bone, guard in violet, tip in cyan (the concept
  // palette). Edges take the pure region colour; interiors are nudged toward ink
  // for a chunky outline. [bright] (0..1) scales alpha so mood reads as glow.
  void _paintNail(
    void Function(num, num, Color) px,
    int ox,
    int oy, {
    required double bright,
  }) {
    final a = (0.28 + 0.72 * bright.clamp(0.0, 1.0)).clamp(0.0, 1.0);
    for (var r = 0; r < sh; r++) {
      final row = _nail[r];
      for (var c = 0; c < sw; c++) {
        if (row[c] != '#') continue;
        final Color base = r <= 2
            ? _bone
            : r <= 4
                ? _violet
                : r >= sh - 3
                    ? _cyan
                    : _bone;
        final col = _isEdge(r, c) ? base : Color.lerp(base, _ink, 0.3)!;
        px(ox + c, oy + r, col.withValues(alpha: a));
      }
    }
  }

  bool _isEdge(int r, int c) {
    for (final d in const [
      [0, 1],
      [0, -1],
      [1, 0],
      [-1, 0],
    ]) {
      final rr = r + d[0], cc = c + d[1];
      if (rr < 0 || rr >= sh || cc < 0 || cc >= sw || _nail[rr][cc] != '#') {
        return true;
      }
    }
    return false;
  }

  // 8-dot loading ring; the active dot (+ short trail) sweeps around — the
  // "progress" indicator while connecting.
  void _paintSpinner(
    void Function(num, num, Color) px,
    double cxCol,
    double cyRow,
    Color bright,
    Color mid,
  ) {
    const n = 8;
    final rad = (sh / 2) - 1.0;
    final head = (spin * 1.5).floor();
    for (var i = 0; i < n; i++) {
      final ang = i * (2 * pi / n) - pi / 2;
      final gx = (cxCol + cos(ang) * rad).round();
      final gy = (cyRow + sin(ang) * rad).round();
      final rel = ((head - i) % n + n) % n; // 0 = active, grows behind
      final Color col;
      if (rel == 0) {
        col = bright;
      } else if (rel <= 2) {
        col = mid.withValues(alpha: 0.7 - rel * 0.2);
      } else {
        col = mid.withValues(alpha: 0.14);
      }
      px(gx, gy, col);
    }
  }

  // Connect: a chunky pixel ring bursts OUTWARD from the core.
  void _paintConnectStrike(
    void Function(num, num, Color) px,
    double cxCol,
    double cyRow,
  ) {
    final e = 1 - pow(1 - strikeT, 2).toDouble();
    final rad = 1 + e * (cells / 2 - 1);
    final a = (1 - strikeT).clamp(0.0, 1.0);
    final steps = (2 * pi * rad).ceil().clamp(8, 96);
    for (var i = 0; i < steps; i++) {
      final ang = i * (2 * pi / steps);
      px(
        (cxCol + cos(ang) * rad).round(),
        (cyRow + sin(ang) * rad).round(),
        strikeTint.withValues(alpha: a),
      );
    }
  }

  // Disconnect: the shield's pixels drop away + fade — a pixel "power-down".
  void _paintDisconnectStrike(
    void Function(num, num, Color) px,
    int ox,
    int oy,
    Color dark,
  ) {
    final drop = (strikeT * 10).round();
    final a = (1 - strikeT).clamp(0.0, 1.0);
    for (var r = 0; r < sh; r++) {
      for (var c = 0; c < sw; c++) {
        if (_nail[r][c] != '#') continue;
        final d = drop + (c % 3); // stagger the fall by column
        px(ox + c, oy + r + d, dark.withValues(alpha: 0.6 * a));
      }
    }
  }

  @override
  bool shouldRepaint(covariant _RetroNailPainter old) =>
      old.phase != phase ||
      old.spin != spin ||
      old.intensity != intensity ||
      old.color != color ||
      old.connectedness != connectedness ||
      old.strikeT != strikeT ||
      old.strikeTint != strikeTint ||
      old.strikeKind != strikeKind ||
      old.mode != mode;
}
