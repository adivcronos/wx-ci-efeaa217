import 'package:flutter/material.dart';

import '../theme.dart';

/// A terminal-native replacement for the old nail sigil: a monospace "secure
/// link" readout that fits the CRT theme (no pasted-in graphic) and animates on
/// both connect and disconnect.
///
///   [ SECURE LINK ]
///   LOCAL ●════════▶··● NODE
///   SYNC  [███████░░░] 72%
///
/// - disconnected → broken link (grey), bar empty.
/// - connecting   → link scans / packets flow (amber).
/// - connected    → solid link with a packet running along it (green).
///
/// [play] runs the one-shot transition the button awaits: `connect` builds the
/// link up left→right; `!connect` powers it down right→left.
class TerminalMonitor extends StatefulWidget {
  final bool connected;
  final bool connecting;
  const TerminalMonitor({
    super.key,
    required this.connected,
    required this.connecting,
  });

  @override
  State<TerminalMonitor> createState() => TerminalMonitorState();
}

class TerminalMonitorState extends State<TerminalMonitor>
    with TickerProviderStateMixin {
  static const int _n = 12; // link segments

  late final AnimationController _p; // link/bar fill 0..1
  late final AnimationController _flow; // running packet / scan
  bool _busy = false; // a one-shot transition is playing

  @override
  void initState() {
    super.initState();
    _p = AnimationController(
      vsync: this,
      duration: const Duration(milliseconds: 700),
      value: widget.connected ? 1 : 0,
    );
    _flow = AnimationController(
      vsync: this,
      duration: const Duration(milliseconds: 1100),
    );
    _applyFlow();
  }

  @override
  void didUpdateWidget(TerminalMonitor oldWidget) {
    super.didUpdateWidget(oldWidget);
    // Settle the static fill to match the live state (unless a one-shot
    // transition is mid-flight — it drives _p itself).
    if (!_busy) {
      if (widget.connected && _p.value < 1) {
        _p.animateTo(1, curve: Curves.easeOut);
      } else if (!widget.connected && !widget.connecting && _p.value > 0) {
        _p.animateTo(0, curve: Curves.easeIn);
      }
    }
    _applyFlow();
  }

  void _applyFlow() {
    final live = widget.connected || widget.connecting || _busy;
    if (live && !_flow.isAnimating) {
      _flow.repeat();
    } else if (!live && _flow.isAnimating) {
      _flow.stop();
    }
  }

  /// One-shot connect/disconnect animation the button awaits.
  Future<void> play({required bool connect}) async {
    setState(() => _busy = true);
    _applyFlow();
    try {
      if (connect) {
        await _p.animateTo(1, curve: Curves.easeOutCubic);
      } else {
        await _p.animateBack(0, curve: Curves.easeInCubic);
      }
    } finally {
      if (mounted) {
        setState(() => _busy = false);
        _applyFlow();
      }
    }
  }

  @override
  void dispose() {
    _p.dispose();
    _flow.dispose();
    super.dispose();
  }

  Color get _color {
    if (widget.connected && !_busy) return PxColors.success;
    if (widget.connecting || _busy) return PxColors.warning;
    return PxColors.mutedDeep;
  }

  String _linkLine(double p, double flow) {
    final filled = (p * _n).round().clamp(0, _n);
    final String seg;
    if (filled == 0) {
      seg = ' ─ ─ ✕ ─ ─  ';
    } else {
      final cells = List<String>.filled(_n, '·');
      for (var i = 0; i < filled; i++) {
        cells[i] = '═';
      }
      final fp = (flow * _n).floor().clamp(0, _n - 1);
      if (fp < filled) cells[fp] = '▶';
      seg = cells.join();
    }
    return 'LOCAL ●$seg● NODE';
  }

  String _barLine(double p) {
    final on = (p * 10).round().clamp(0, 10);
    final bar = '█' * on + '░' * (10 - on);
    final pct = (p * 100).round().toString().padLeft(3);
    return 'SYNC  [$bar] $pct%';
  }

  String get _title {
    if (widget.connected && !_busy) return '[ SECURE LINK ]';
    if (_busy) return '[ NEGOTIATING ]';
    if (widget.connecting) return '[ HANDSHAKE ]';
    return '[ LINK DOWN ]';
  }

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: Listenable.merge([_p, _flow]),
      builder: (_, _) {
        final c = _color;
        final p = _p.value;
        final line = TextStyle(
          fontFamily: PxFont.body,
          fontSize: 16,
          height: 1.6,
          letterSpacing: 0.5,
          color: c,
          shadows: glowText(c, alpha: 0.28),
        );
        return Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(
              _title,
              style: TextStyle(
                fontFamily: PxFont.body,
                fontWeight: FontWeight.w700,
                fontSize: 13,
                letterSpacing: 2,
                color: c,
                shadows: glowText(c, alpha: 0.3),
              ),
            ),
            const SizedBox(height: 8),
            Text(_linkLine(p, _flow.value), style: line),
            Text(_barLine(p), style: line),
          ],
        );
      },
    );
  }
}
