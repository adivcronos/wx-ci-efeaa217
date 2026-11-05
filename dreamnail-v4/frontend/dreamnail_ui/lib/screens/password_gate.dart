import 'dart:async';
import 'dart:math';

import 'package:flutter/material.dart';
import 'package:provider/provider.dart';

import '../api.dart';
import '../config.dart';
import '../theme.dart';

/// Revealed by typing the keyword in the weather search bar. A deniable launch
/// gate: a wrong key flickers the screen and prints "> ACCESS DENIED" with no
/// hint; the correct key prints "> ACCESS GRANTED" then wipes to the VPN screen.
///
/// CRT-terminal skin (VPN side only): green phosphor on near-black, scanlines,
/// monospace everything, a fake secure-terminal prompt. The weather disguise is
/// untouched.
class PasswordGate extends StatefulWidget {
  final VoidCallback onUnlocked;
  final VoidCallback onBack;
  const PasswordGate({
    super.key,
    required this.onUnlocked,
    required this.onBack,
  });

  @override
  State<PasswordGate> createState() => _PasswordGateState();
}

class _PasswordGateState extends State<PasswordGate>
    with TickerProviderStateMixin {
  final _ctrl = TextEditingController();
  final _focus = FocusNode();
  late final AnimationController _glitch; // wrong-key screen flicker
  late final AnimationController _wipe; // unlock -> VPN top-to-bottom wipe
  late final AnimationController _marquee; // bottom stats scroll
  bool _denied = false;
  bool _granted = false;
  bool _busy = false;

  static const _stats =
      'CPU: 2%   |   MEM: 847MB   |   NET: ENCRYPTED   |   STATUS: STANDBY';

  @override
  void initState() {
    super.initState();
    _glitch = AnimationController(
      vsync: this,
      duration: const Duration(milliseconds: 300),
    );
    _wipe = AnimationController(
      vsync: this,
      duration: const Duration(milliseconds: 520),
    );
    _marquee = AnimationController(
      vsync: this,
      duration: const Duration(seconds: 14),
    )..repeat();
  }

  @override
  void dispose() {
    _ctrl.dispose();
    _focus.dispose();
    _glitch.dispose();
    _wipe.dispose();
    _marquee.dispose();
    super.dispose();
  }

  Future<void> _submit() async {
    if (_busy) return;
    final pw = _ctrl.text;
    if (pw == AppConfig.launchPassword) {
      setState(() {
        _busy = true;
        _granted = true;
        _denied = false;
      });
      // Best-effort: hydrate the backend's encrypted store. Navigation does not
      // hinge on this — the gate already matched the baked password.
      try {
        await context.read<ApiClient>().unlock(pw);
      } catch (_) {}
      await Future.delayed(const Duration(milliseconds: 500));
      if (!mounted) return;
      await _wipe.forward(from: 0); // sweep the screen away top -> bottom
      if (mounted) widget.onUnlocked();
      return;
    }
    // Wrong: flicker + red "ACCESS DENIED", then reset. No hint.
    _ctrl.clear();
    setState(() => _denied = true);
    _glitch.forward(from: 0);
    Future.delayed(const Duration(milliseconds: 1000), () {
      if (mounted) setState(() => _denied = false);
    });
  }

  @override
  Widget build(BuildContext context) {
    return pixelTypography(
      context,
      child: Scaffold(
        backgroundColor: PxColors.bg,
        body: Stack(
          children: [
            // Everything below flickers as one unit on a wrong key.
            AnimatedBuilder(
              animation: _glitch,
              builder: (_, child) {
                double op = 1;
                if (_glitch.isAnimating) {
                  op = sin(_glitch.value * pi * 6) > 0 ? 1 : 0;
                }
                return Opacity(opacity: op, child: child);
              },
              child: SafeArea(
                child: Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 28),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    children: [
                      const SizedBox(height: 6),
                      _header(),
                      Expanded(child: Center(child: _prompt())),
                      _statsBar(),
                      const SizedBox(height: 4),
                    ],
                  ),
                ),
              ),
            ),
            // CRT scanline wash over the whole screen.
            const Positioned.fill(child: Scanlines(opacity: 0.08)),
            // Unlock wipe: a near-black cover led by a bright green line, growing
            // from the top down until it fills the screen, then we navigate.
            Positioned.fill(
              child: IgnorePointer(
                child: AnimatedBuilder(
                  animation: _wipe,
                  builder: (_, _) {
                    if (_wipe.value == 0) return const SizedBox.shrink();
                    return Align(
                      alignment: Alignment.topCenter,
                      child: FractionallySizedBox(
                        heightFactor: _wipe.value,
                        widthFactor: 1,
                        child: DecoratedBox(
                          decoration: BoxDecoration(
                            color: PxColors.ink,
                            border: const Border(
                              bottom: BorderSide(
                                color: PxColors.accentGlow,
                                width: 2,
                              ),
                            ),
                            boxShadow: crtGlow(PxColors.accent, alpha: 0.6, blur: 16),
                          ),
                        ),
                      ),
                    );
                  },
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }

  // Top row: OS tag left, blinking "secure terminal" indicator right.
  Widget _header() => DefaultTextStyle(
    style: const TextStyle(
      fontFamily: PxFont.body,
      fontSize: 10,
      letterSpacing: 1.5,
      color: PxColors.mutedDeep,
    ),
    child: Row(
      children: [
        IconButton(
          padding: EdgeInsets.zero,
          constraints: const BoxConstraints(minWidth: 32, minHeight: 32),
          icon: const Icon(Icons.chevron_left, color: PxColors.mutedDeep, size: 18),
          onPressed: widget.onBack,
        ),
        const SizedBox(width: 2),
        const Text('DREAMNAIL OS v2.1'),
        const Spacer(),
        const _BlinkDot(),
        const SizedBox(width: 6),
        const Text('SECURE TERMINAL'),
      ],
    ),
  );

  // Center: a fake terminal boot + auth prompt with the block-cursor input.
  Widget _prompt() {
    final line = _denied
        ? PxColors.danger
        : (_granted ? PxColors.success : PxColors.textMuted);
    return ConstrainedBox(
      constraints: const BoxConstraints(maxWidth: 560),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          _boot('DREAMNAIL SECURE TERMINAL  [v2.1]', color: PxColors.textMuted),
          const SizedBox(height: 2),
          _boot(
            '=' * 40,
            color: PxColors.mutedDeep,
          ),
          const SizedBox(height: 12),
          _boot('> initializing secure channel ... OK', color: PxColors.mutedDeep),
          _boot('> authentication required', color: PxColors.textMuted),
          const SizedBox(height: 14),
          // The live input line: "> access key: ██████▌"
          Row(
            crossAxisAlignment: CrossAxisAlignment.center,
            children: [
              Text(
                '> access key: ',
                style: TextStyle(
                  fontFamily: PxFont.body,
                  fontSize: 18,
                  color: PxColors.textPrimary,
                  shadows: glowText(PxColors.accent, alpha: 0.4),
                ),
              ),
              Expanded(
                child: TextField(
                  controller: _ctrl,
                  focusNode: _focus,
                  autofocus: true,
                  obscureText: true,
                  obscuringCharacter: '█',
                  enabled: !_busy,
                  onSubmitted: (_) => _submit(),
                  cursorColor: PxColors.accent,
                  cursorWidth: 11,
                  cursorRadius: Radius.zero,
                  showCursor: true,
                  style: TextStyle(
                    fontFamily: PxFont.body,
                    fontSize: 18,
                    letterSpacing: 1,
                    color: PxColors.textPrimary,
                    shadows: glowText(PxColors.accent, alpha: 0.5),
                  ),
                  decoration: InputDecoration(
                    isCollapsed: true,
                    border: InputBorder.none,
                    enabledBorder: InputBorder.none,
                    focusedBorder: InputBorder.none,
                    // A faint cue for where to type (clears on first keypress).
                    hintText: 'type here',
                    hintStyle: const TextStyle(
                      fontFamily: PxFont.body,
                      fontSize: 16,
                      letterSpacing: 1,
                      color: PxColors.mutedDeep,
                    ),
                  ),
                ),
              ),
            ],
          ),
          const SizedBox(height: 18),
          // Reserved status line so layout doesn't jump.
          SizedBox(
            height: 24,
            child: (_denied || _granted)
                ? Row(
                    children: [
                      Text(
                        _granted ? '> ACCESS GRANTED' : '> ACCESS DENIED',
                        style: TextStyle(
                          fontFamily: PxFont.hero,
                          fontWeight: FontWeight.w700,
                          fontSize: 18,
                          letterSpacing: 1.5,
                          color: line,
                          shadows: glowText(line, alpha: 0.7),
                        ),
                      ),
                      const SizedBox(width: 6),
                      BlinkingCursor(color: line, fontSize: 18),
                    ],
                  )
                : null,
          ),
        ],
      ),
    );
  }

  Widget _boot(String s, {required Color color}) => Padding(
    padding: const EdgeInsets.symmetric(vertical: 1),
    child: Text(
      s,
      maxLines: 1,
      overflow: TextOverflow.clip,
      style: TextStyle(
        fontFamily: PxFont.body,
        fontSize: 14,
        height: 1.3,
        letterSpacing: 0.5,
        color: color,
      ),
    ),
  );

  // Slowly scrolling fake system stats along the bottom.
  Widget _statsBar() {
    const gap = 80.0;
    final approxWidth = _stats.length * 8.4 + gap;
    return SizedBox(
      height: 16,
      child: ClipRect(
        child: AnimatedBuilder(
          animation: _marquee,
          builder: (_, _) {
            final dx = -_marquee.value * approxWidth;
            return Transform.translate(
              offset: Offset(dx, 0),
              child: OverflowBox(
                alignment: Alignment.centerLeft,
                maxWidth: approxWidth * 2,
                child: Row(
                  children: [
                    _statsText(),
                    const SizedBox(width: gap),
                    _statsText(),
                  ],
                ),
              ),
            );
          },
        ),
      ),
    );
  }

  Widget _statsText() => Text(
    _stats,
    maxLines: 1,
    style: const TextStyle(
      fontFamily: PxFont.body,
      fontSize: 11,
      letterSpacing: 1.2,
      color: PxColors.mutedDeep,
    ),
  );
}

/// Small blinking green status dot for the terminal header.
class _BlinkDot extends StatefulWidget {
  const _BlinkDot();
  @override
  State<_BlinkDot> createState() => _BlinkDotState();
}

class _BlinkDotState extends State<_BlinkDot> {
  bool _on = true;
  late final Timer? _t;
  @override
  void initState() {
    super.initState();
    _t = Timer.periodic(const Duration(milliseconds: 600), (_) {
      if (mounted) setState(() => _on = !_on);
    });
  }

  @override
  void dispose() {
    _t?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AnimatedOpacity(
    opacity: _on ? 1 : 0.15,
    duration: const Duration(milliseconds: 200),
    child: Container(
      width: 7,
      height: 7,
      decoration: BoxDecoration(
        shape: BoxShape.circle,
        color: PxColors.success,
        boxShadow: crtGlow(PxColors.success, alpha: 0.7, blur: 6),
      ),
    ),
  );
}
