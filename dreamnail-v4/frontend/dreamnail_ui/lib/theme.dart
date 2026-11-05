import 'dart:async';

import 'package:flutter/material.dart';

/// Design tokens for Dreamnail v2 — indigo/violet dark system (from the spec).
///
/// Everything is a mood of one night sky: [ink] is the deepest point (used at
/// the bottom of every screen so weather, gate and VPN blend into a single
/// continuous surface), [nightViolet] is the violet-tinted mid, and [accent] /
/// [accentGlow] are the violet "star" the whole product orbits.
class AppColors {
  static const ink = Color(0xFF08080F); // deepest night — shared floor
  static const bg = Color(0xFF0F0F1A);
  static const nightViolet = Color(0xFF1A1530); // violet-tinted surface
  static const surface = Color(0xFF16161F);
  static const card = Color(0xFF1C1C2A);
  static const field = Color(0xFF22222E);
  static const accent = Color(0xFF7C6CFF);
  static const accentGlow = Color(0xFFA78BFA); // lighter violet for glows
  static const success = Color(0xFF2FD98A);
  static const danger = Color(0xFFE63946);
  static const warning = Color(0xFFFFAD47);
  static const textPrimary = Color(0xFFF5F5F7);
  static const textMuted = Color(0xFF9090A0);
}

/// Shared "instrument readout" type — tracked, uppercase micro-labels used for
/// every stat/caption on both faces (weather stats, forecast days, VPN meters).
/// This one treatment is what makes the two faces read as the same product.
class AppText {
  static const label = TextStyle(
    fontSize: 11,
    height: 1.1,
    letterSpacing: 1.4,
    fontWeight: FontWeight.w600,
    color: AppColors.textMuted,
  );

  static const readout = TextStyle(
    fontSize: 17,
    fontWeight: FontWeight.w600,
    color: AppColors.textPrimary,
    letterSpacing: -0.2,
  );
}

/// Border radii.
class AppRadius {
  static const double sm = 8;
  static const double md = 12;
  static const double lg = 20;
}

/// Spacing scale.
class AppSpace {
  static const double xs = 4;
  static const double sm = 8;
  static const double md = 12;
  static const double lg = 16;
  static const double xl = 24;
  static const double xxl = 32;
}

ThemeData buildTheme() {
  const scheme = ColorScheme.dark(
    primary: AppColors.accent,
    secondary: AppColors.accent,
    surface: AppColors.surface,
    error: AppColors.danger,
    onPrimary: AppColors.textPrimary,
    onSurface: AppColors.textPrimary,
  );
  return ThemeData(
    useMaterial3: true,
    colorScheme: scheme,
    scaffoldBackgroundColor: AppColors.bg,
    // Default font: the platform sans-serif. Bundled Inter is added later.
    textTheme: const TextTheme().apply(
      bodyColor: AppColors.textPrimary,
      displayColor: AppColors.textPrimary,
    ),
    snackBarTheme: const SnackBarThemeData(
      backgroundColor: AppColors.card,
      contentTextStyle: TextStyle(color: AppColors.textPrimary),
      behavior: SnackBarBehavior.floating,
    ),
  );
}

// ===========================================================================
// CRT TERMINAL SKIN — VPN side only (password gate + VPN panel + settings).
//
// A separate token set from the weather disguise (AppColors/AppText/
// FrostedCard), which stays byte-identical. The look is an old green-phosphor
// terminal: a near-black green-tinted ground, a scanline wash, monospace
// everything, phosphor-green text with a soft CRT glow on active elements.
// Everything on the VPN side routes through these tokens, so the whole face
// reskins from this one block. Field names deliberately mirror AppColors/
// AppText/AppRadius so the screens read the same on both faces.
// ===========================================================================

/// One monospace family for the whole terminal (JetBrains Mono). [hero] and
/// [body] both point at it; weight is chosen per call with FontWeight.
class PxFont {
  static const String hero = 'JetBrainsMono';
  static const String body = 'JetBrainsMono';
}

/// Desaturated "grey phosphor" palette — a steel-grey CRT with green reserved
/// for the live elements (status, connect action, cursor, focus). Same field
/// names as [AppColors] so the VPN screens re-skin by a prefix swap.
class PxColors {
  static const ink = Color(0xFF08090A); // deepest grey — shared floor
  static const bg = Color(0xFF0C0E0E); // scaffold ground (near-black grey)
  static const nightViolet = Color(0xFF121514);
  static const surface = Color(0xFF141716); // panel body (grey)
  static const card = Color(0xFF121514); // inner card body
  static const field = Color(0xFF0E100F); // sunken field / input track
  static const accent = Color(0xFF56A87B); // muted grey-green (accents)
  static const accentGlow = Color(0xFF86C4A0); // softer green for glow peaks
  static const success = Color(0xFF56A87B); // connected / go
  static const successBright = Color(0xFF86C4A0);
  static const danger = Color(0xFFE0665C); // disconnect / access denied
  static const warning = Color(0xFFD6A24E); // connecting / warning (amber)
  static const textPrimary = Color(0xFFA9B6AC); // grey-green body text
  static const textMuted = Color(0xFF6B776F); // grey
  static const mutedDeep = Color(0xFF454E49); // dim grey — labels/hints
  static const frame = Color(0xFF3A443E); // panel border (grey)
  static const frameInset = Color(0xFF2A322D); // field border (dim grey)
  static const shadow = Color(0xCC000000);
}

/// Near-sharp terminal corners with a hair of modern softness. Same field names
/// as [AppRadius].
class PxRadius {
  static const double sm = 2;
  static const double md = 3;
  static const double lg = 4;
}

/// Monospace type. Same field names as [AppText]; [label] the dim caption,
/// [readout] the bright value line.
class PxText {
  static const label = TextStyle(
    fontFamily: PxFont.body,
    fontSize: 12,
    height: 1.3,
    letterSpacing: 1.0,
    color: PxColors.textMuted,
  );

  static const readout = TextStyle(
    fontFamily: PxFont.body,
    fontSize: 18,
    height: 1.3,
    letterSpacing: 0.5,
    color: PxColors.textPrimary,
  );
}

/// A soft phosphor bloom for active elements — two stacked blurs read like CRT
/// glow. Drop into a BoxShadow list.
List<BoxShadow> crtGlow(
  Color c, {
  double alpha = 0.24,
  double blur = 12,
  double spread = 0.5,
}) => [
  BoxShadow(color: c.withValues(alpha: alpha), blurRadius: blur, spreadRadius: spread),
  BoxShadow(
    color: c.withValues(alpha: alpha * 0.4),
    blurRadius: blur * 2.2,
    spreadRadius: spread,
  ),
];

/// Soft green text glow — apply to a [Text]'s `shadows`.
List<Shadow> glowText(Color c, {double alpha = 0.3, double blur = 7}) => [
  Shadow(color: c.withValues(alpha: alpha), blurRadius: blur),
  Shadow(color: c.withValues(alpha: alpha * 0.5), blurRadius: blur * 2),
];

/// A blinking block cursor (█) that toggles on/off every [period]. Reusable
/// after any "live" line of terminal text in either screen.
class BlinkingCursor extends StatefulWidget {
  final Color color;
  final double fontSize;
  final String glyph;
  final Duration period;
  const BlinkingCursor({
    super.key,
    this.color = PxColors.textPrimary,
    this.fontSize = 16,
    this.glyph = '█',
    this.period = const Duration(milliseconds: 500),
  });
  @override
  State<BlinkingCursor> createState() => _BlinkingCursorState();
}

class _BlinkingCursorState extends State<BlinkingCursor> {
  late final Timer _t;
  bool _on = true;
  @override
  void initState() {
    super.initState();
    _t = Timer.periodic(widget.period, (_) {
      if (mounted) setState(() => _on = !_on);
    });
  }

  @override
  void dispose() {
    _t.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Opacity(
    opacity: _on ? 1 : 0,
    child: Text(
      widget.glyph,
      style: TextStyle(
        fontFamily: PxFont.body,
        fontSize: widget.fontSize,
        height: 1.0,
        color: widget.color,
        shadows: glowText(widget.color),
      ),
    ),
  );
}

/// A bordered terminal panel: near-black body, thin dim-green edge and a soft
/// green glow. Same constructor shape as before so it's a drop-in on the VPN
/// side. [shadow] is kept for API compatibility (the panel uses a CRT glow now).
class PixelPanel extends StatelessWidget {
  final Widget child;
  final EdgeInsetsGeometry padding;
  final Color? color;
  final Color? border;
  final Offset shadow;
  const PixelPanel({
    super.key,
    required this.child,
    this.padding = const EdgeInsets.all(AppSpace.lg),
    this.color,
    this.border,
    this.shadow = const Offset(5, 5),
  });

  @override
  Widget build(BuildContext context) {
    final b = border ?? PxColors.frame;
    return Container(
      padding: padding,
      decoration: BoxDecoration(
        color: color ?? PxColors.surface,
        borderRadius: BorderRadius.circular(PxRadius.sm),
        border: Border.all(color: b, width: 1),
        boxShadow: crtGlow(b, alpha: 0.16, blur: 14, spread: 0),
      ),
      child: child,
    );
  }
}

/// A sunken inner surface — fields, meter tracks. Near-black with a dim edge.
class PixelInset extends StatelessWidget {
  final Widget child;
  final EdgeInsetsGeometry padding;
  final Color? color;
  final Color? border;
  const PixelInset({
    super.key,
    required this.child,
    this.padding = const EdgeInsets.all(AppSpace.md),
    this.color,
    this.border,
  });

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: padding,
      decoration: BoxDecoration(
        color: color ?? PxColors.field,
        borderRadius: BorderRadius.circular(PxRadius.sm),
        border: Border.all(color: border ?? PxColors.frameInset, width: 1),
      ),
      child: child,
    );
  }
}

/// A terminal action button: a bracketed monospace label, a green edge with a
/// soft glow, that presses in on tap. Defaults to terminal green; callers pass
/// red/amber for other actions.
class PixelButton extends StatefulWidget {
  final String? label;
  final Widget? child;
  final VoidCallback? onPressed;
  final Color color;
  final Color border;
  final Color fg;
  final double fontSize;
  const PixelButton({
    super.key,
    this.label,
    this.child,
    required this.onPressed,
    this.color = PxColors.success,
    this.border = PxColors.successBright,
    this.fg = PxColors.ink,
    this.fontSize = 11,
  });

  @override
  State<PixelButton> createState() => _PixelButtonState();
}

class _PixelButtonState extends State<PixelButton> {
  bool _down = false;
  bool _hover = false;
  @override
  Widget build(BuildContext context) {
    final enabled = widget.onPressed != null;
    void set(bool v) {
      if (enabled && _down != v) setState(() => _down = v);
    }

    final base = enabled ? widget.color : widget.color.withValues(alpha: 0.4);
    final fill = base.withValues(alpha: _hover && enabled ? 0.20 : 0.10);
    final edge = enabled ? widget.border : widget.border.withValues(alpha: 0.4);
    // JetBrains Mono reads a touch small at the old arcade sizes; nudge up.
    final fs = widget.fontSize <= 12 ? 15.0 : widget.fontSize;

    return MouseRegion(
      cursor: enabled ? SystemMouseCursors.click : MouseCursor.defer,
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      child: GestureDetector(
        onTapDown: (_) => set(true),
        onTapUp: (_) => set(false),
        onTapCancel: () => set(false),
        onTap: widget.onPressed,
        child: Transform.translate(
          offset: _down ? const Offset(0, 2) : Offset.zero,
          child: AnimatedContainer(
            duration: const Duration(milliseconds: 90),
            curve: Curves.easeOut,
            width: double.infinity,
            padding: const EdgeInsets.symmetric(vertical: 15, horizontal: 12),
            decoration: BoxDecoration(
              color: fill,
              borderRadius: BorderRadius.circular(PxRadius.sm),
              border: Border.all(color: edge, width: 1),
              boxShadow: (_down || !enabled)
                  ? const <BoxShadow>[]
                  : crtGlow(widget.color, alpha: _hover ? 0.4 : 0.22, blur: 14),
            ),
            child: Center(
              child:
                  widget.child ??
                  Text(
                    '[ ${widget.label!.toUpperCase()} ]',
                    textAlign: TextAlign.center,
                    style: TextStyle(
                      fontFamily: PxFont.hero,
                      fontWeight: FontWeight.w700,
                      fontSize: fs,
                      height: 1.2,
                      letterSpacing: 2.0,
                      color: enabled ? widget.border : widget.border.withValues(alpha: 0.6),
                      shadows: enabled ? glowText(widget.border, alpha: 0.5) : null,
                    ),
                  ),
            ),
          ),
        ),
      ),
    );
  }
}

/// Full-bleed CRT scanline wash — 1px dark lines every 3px, low opacity. Drop
/// into a [Stack] via `Positioned.fill`.
class Scanlines extends StatelessWidget {
  final double opacity;
  const Scanlines({super.key, this.opacity = 0.08});
  @override
  Widget build(BuildContext context) {
    return IgnorePointer(
      child: CustomPaint(size: Size.infinite, painter: _ScanlinePainter(opacity)),
    );
  }
}

class _ScanlinePainter extends CustomPainter {
  final double opacity;
  _ScanlinePainter(this.opacity);
  @override
  void paint(Canvas canvas, Size size) {
    final p = Paint()..color = Colors.black.withValues(alpha: opacity);
    for (double y = 0; y < size.height; y += 3) {
      canvas.drawRect(Rect.fromLTWH(0, y, size.width, 1), p);
    }
  }

  @override
  bool shouldRepaint(_ScanlinePainter old) => old.opacity != opacity;
}

/// Wrap a VPN-side [Scaffold] so all its text defaults to the monospace
/// terminal family. Kept local so the weather disguise's typography is
/// unaffected.
Widget pixelTypography(BuildContext context, {required Widget child}) {
  final base = Theme.of(context);
  return Theme(
    data: base.copyWith(
      textTheme: base.textTheme.apply(
        fontFamily: PxFont.body,
        bodyColor: PxColors.textPrimary,
        displayColor: PxColors.textPrimary,
      ),
    ),
    child: child,
  );
}
