import 'dart:math';

import 'package:flutter/material.dart';
import 'package:flutter/scheduler.dart';

import '../util/repaint_notifier.dart';

/// A smooth, animated area graph of download speed over the last ~30 seconds.
///
/// [samples] is the recent download rate in bytes/sec, oldest first, newest
/// last (fed from [ConnectionController.downloadHistory], one point per ~2s
/// status poll). The status feed is coarse, so the graph *eases* toward each
/// new sample every frame instead of stepping — the line glides and the vertical
/// scale re-normalises smoothly — which is what makes 15 discrete points read as
/// one flowing curve. Newest sample is pinned to the right edge.
class SpeedGraph extends StatefulWidget {
  final List<int> samples;
  final Color color;
  final int windowPoints; // how many points span the width (~30s at 2s each)
  final double height;

  const SpeedGraph({
    super.key,
    required this.samples,
    required this.color,
    this.windowPoints = 16,
    this.height = 34,
  });

  @override
  State<SpeedGraph> createState() => _SpeedGraphState();
}

class _SpeedGraphState extends State<SpeedGraph>
    with SingleTickerProviderStateMixin {
  late final Ticker _ticker;
  final _repaint = RepaintNotifier();
  late List<double>
  _shown; // eased normalised values [0,1], length windowPoints
  double _peak = 1; // eased vertical scale (bytes/sec)
  Duration _last = Duration.zero;

  @override
  void initState() {
    super.initState();
    _shown = List.filled(widget.windowPoints, 0);
    _ticker = createTicker(_onTick)..start();
  }

  /// Right-aligned target array: newest sample at the far right, front-padded
  /// with zeros until enough history has accumulated.
  List<double> get _targetRaw {
    final n = widget.windowPoints;
    final out = List<double>.filled(n, 0);
    final s = widget.samples;
    for (var i = 0; i < s.length && i < n; i++) {
      out[n - 1 - i] = s[s.length - 1 - i].toDouble().clamp(0, double.infinity);
    }
    return out;
  }

  void _onTick(Duration now) {
    final dt = _last == Duration.zero
        ? 0.016
        : (now - _last).inMicroseconds / 1e6;
    _last = now;
    final k = (dt / 0.35).clamp(0.0, 1.0); // ~0.35s ease

    final raw = _targetRaw;
    final targetPeak = max(1.0, raw.fold<double>(0, max));
    _peak += (targetPeak - _peak) * k;
    for (var i = 0; i < _shown.length; i++) {
      final target = (raw[i] / _peak).clamp(0.0, 1.0);
      _shown[i] += (target - _shown[i]) * k;
    }
    _repaint.tick();
  }

  @override
  void dispose() {
    _ticker.dispose();
    _repaint.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      height: widget.height,
      width: double.infinity,
      child: RepaintBoundary(
        child: CustomPaint(
          painter: _GraphPainter(
            values: _shown,
            color: widget.color,
            repaint: _repaint,
          ),
          size: Size.infinite,
        ),
      ),
    );
  }
}

class _GraphPainter extends CustomPainter {
  final List<double> values; // normalised [0,1], left→right
  final Color color;
  _GraphPainter({
    required this.values,
    required this.color,
    required Listenable repaint,
  }) : super(repaint: repaint);

  @override
  void paint(Canvas canvas, Size size) {
    final n = values.length;
    if (n < 2) return;
    final w = size.width, h = size.height;
    const pad = 3.0; // keep the stroke off the top edge
    double x(int i) => i / (n - 1) * w;
    double y(int i) => h - pad - values[i].clamp(0.0, 1.0) * (h - pad);

    // Stepped, blocky path — a retro bar-graph readout, no smoothing.
    final line = Path()..moveTo(x(0), y(0));
    for (var i = 1; i < n; i++) {
      line.lineTo(x(i), y(i - 1));
      line.lineTo(x(i), y(i));
    }

    // Flat fill under the steps (no gradient, no blur — crisp pixels).
    final area = Path.from(line)
      ..lineTo(w, h)
      ..lineTo(0, h)
      ..close();
    canvas.drawPath(area, Paint()..color = color.withValues(alpha: 0.18));

    // Crisp stepped stroke.
    canvas.drawPath(
      line,
      Paint()
        ..style = PaintingStyle.stroke
        ..strokeWidth = 2
        ..strokeJoin = StrokeJoin.miter
        ..color = color,
    );

    // Square leading marker at the newest (right-most) point.
    final head = Offset(x(n - 1), y(n - 1));
    canvas.drawRect(
      Rect.fromCenter(center: head, width: 5, height: 5),
      Paint()..color = color,
    );
  }

  @override
  bool shouldRepaint(_GraphPainter old) => old.color != color;
}
