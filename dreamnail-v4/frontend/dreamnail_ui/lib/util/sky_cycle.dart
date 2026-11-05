import 'dart:math';

import 'package:flutter/material.dart';

/// A smooth day/night cycle for the weather disguise, driven by the *actual*
/// current time in Sydney. Everything stays inside the indigo/violet night-sky
/// system (see AppColors) — daytime just lifts toward a lighter blue-violet and
/// dawn/dusk warm the horizon; it never becomes a bright blue day sky, so the
/// disguise still reads as one continuous surface with the gate and VPN screens.
///
/// [SkyPalette.forSydney] returns the gradient plus where the sun/moon sit on
/// their arc and how bright the stars should be for that moment.
class SkyPalette {
  /// Top → horizon vertical gradient (3 stops).
  final List<Color> gradient;

  /// Sun position on its arc and how present it is (0 below horizon .. 1 noon).
  final Alignment sunPos;
  final double sunStrength;

  /// Moon position and presence (0 day .. 1 deep night).
  final Alignment moonPos;
  final double moonStrength;

  /// Starfield opacity (1 deep night, 0 full day) — dawn/dusk fade between.
  final double starOpacity;

  const SkyPalette({
    required this.gradient,
    required this.sunPos,
    required this.sunStrength,
    required this.moonPos,
    required this.moonStrength,
    required this.starOpacity,
  });

  bool get isNight => sunStrength < 0.04;

  /// How "day" it is right now (0 night .. 1 midday) — callers use it to lift
  /// the non-clear condition gradients toward daylight.
  double get dayness => sunStrength;

  // --- Sydney local time (DST-aware) ---------------------------------------

  /// Current wall-clock time in Sydney. AEST is UTC+10; AEDT (UTC+11) runs from
  /// the first Sunday of October to the first Sunday of April (AU Eastern rule).
  static DateTime sydneyNow([DateTime? nowUtc]) {
    final utc = (nowUtc ?? DateTime.now()).toUtc();
    final offset = _aedt(utc) ? 11 : 10;
    return utc.add(Duration(hours: offset));
  }

  static bool _aedt(DateTime utc) {
    // DST switches happen at 02:00/03:00 local; day granularity is plenty here.
    final firstSunOct = _firstSunday(utc.year, DateTime.october);
    final firstSunApr = _firstSunday(utc.year, DateTime.april);
    // Southern hemisphere: DST spans the new year, so it's "on" when after the
    // October switch OR before the April switch.
    final afterOct = utc.isAfter(
      DateTime.utc(utc.year, DateTime.october, firstSunOct, 16),
    );
    final beforeApr = utc.isBefore(
      DateTime.utc(utc.year, DateTime.april, firstSunApr, 16),
    );
    return afterOct || beforeApr;
  }

  static int _firstSunday(int year, int month) {
    final firstDow = DateTime.utc(year, month, 1).weekday; // Mon=1..Sun=7
    return firstDow == DateTime.sunday ? 1 : (8 - firstDow) + 1;
  }

  /// "HH:MM" (24h) or "h:MM AM/PM" → fraction of the day in [0,1), or null.
  static double? parseClockFraction(String? s) {
    if (s == null) return null;
    final m = RegExp(r'(\d{1,2}):(\d{2})\s*([APap][Mm])?').firstMatch(s.trim());
    if (m == null) return null;
    var h = int.parse(m.group(1)!);
    final min = int.parse(m.group(2)!);
    final ampm = m.group(3)?.toUpperCase();
    if (ampm == 'PM' && h < 12) h += 12;
    if (ampm == 'AM' && h == 12) h = 0;
    if (h > 23 || min > 59) return null;
    return (h * 60 + min) / 1440.0;
  }

  // --- palette computation --------------------------------------------------

  static SkyPalette forSydney({
    DateTime? nowUtc,
    double? sunrise,
    double? sunset,
  }) {
    final now = sydneyNow(nowUtc);
    final t = (now.hour * 60 + now.minute + now.second / 60.0) / 1440.0;
    final sr = sunrise ?? 0.29; // ~7:00 default (Sydney winter-ish)
    final ss = sunset ?? 0.75; //  ~18:00 default

    // Sun arc: only meaningful between sunrise and sunset.
    double dayProgress = 0, altitude = 0;
    final isDay = t >= sr && t <= ss;
    if (isDay) {
      dayProgress = (t - sr) / (ss - sr);
      altitude = sin(pi * dayProgress); // 0 horizon .. 1 noon
    }
    // Strong near noon, fading to 0 through the ~40min around each horizon crossing.
    final sunStrength = isDay ? (altitude * 2.4).clamp(0.0, 1.0) : 0.0;

    // Moon arc across the night (sunset → next sunrise), wrapping midnight.
    final nightLen = (sr + 1) - ss;
    final nightProgress = isDay
        ? 0.0
        : (((t - ss + 1) % 1) / nightLen).clamp(0.0, 1.0);
    final moonAlt = isDay ? 0.0 : sin(pi * nightProgress);
    final moonStrength = isDay ? 0.0 : (0.35 + 0.65 * moonAlt);

    // Day/night crossfade tied to the REAL sunrise/sunset, ramping over a 30-min
    // window centred on each — shared by the gradient and the star fade so the
    // sky, stars and horizon all turn together instead of on fixed clock times.
    final dayFactor = _dayFactor(t, sr, ss);
    final starOpacity = (1.0 - dayFactor).clamp(0.0, 1.0);

    return SkyPalette(
      gradient: _gradientForSun(t, sr, ss),
      sunPos: Alignment(
        _lerp(-0.95, 0.95, dayProgress),
        -0.14 - 0.72 * altitude,
      ),
      sunStrength: sunStrength,
      moonPos: Alignment(
        _lerp(-0.9, 0.9, nightProgress),
        -0.12 - 0.66 * moonAlt,
      ),
      moonStrength: moonStrength,
      starOpacity: starOpacity,
    );
  }

  // Anchor gradients (top → horizon), all violet-tinted so the disguise reads as
  // one continuous surface. The sky blends between these based on the REAL
  // sunrise/sunset rather than fixed clock keyframes, so dawn/dusk land at the
  // actual Sydney twilight.
  static const List<Color> _nightSky = [
    Color(0xFF06060D),
    Color(0xFF0E0B1F),
    Color(0xFF181233),
  ];
  static const List<Color> _daySky = [
    Color(0xFF2E4C86),
    Color(0xFF48589C),
    Color(0xFF7E82BC),
  ];
  static const List<Color> _warmSky = [
    Color(0xFF34294E),
    Color(0xFF6B4A6E),
    Color(0xFFB6725E),
  ];

  /// 0 at night → 1 by day, ramping across sunrise and back down across sunset
  /// over a 30-minute window centred on each (±15 min).
  static double _dayFactor(double t, double sr, double ss) {
    const h = 15 / 1440.0; // ±15 min ⇒ a 30-minute transition
    return (_smoothstep(sr - h, sr + h, t) - _smoothstep(ss - h, ss + h, t))
        .clamp(0.0, 1.0);
  }

  /// Sky gradient for [t], anchored to the real [sr]/[ss]: a night↔day crossfade
  /// plus a warm dawn/dusk glow that peaks at the horizon crossing and fades
  /// within ~40 minutes either side.
  static List<Color> _gradientForSun(double t, double sr, double ss) {
    final day = _dayFactor(t, sr, ss);
    const warmHalf = 40 / 1440.0;
    final warm = (_bump(t, sr, warmHalf) + _bump(t, ss, warmHalf)).clamp(
      0.0,
      1.0,
    );
    return [
      for (var i = 0; i < 3; i++)
        Color.lerp(
          Color.lerp(_nightSky[i], _daySky[i], day)!,
          _warmSky[i],
          warm,
        )!,
    ];
  }

  /// Smoothstep: 0 at [a], 1 at [b], eased in between.
  static double _smoothstep(double a, double b, double x) {
    if (a >= b) return x >= b ? 1.0 : 0.0;
    final u = ((x - a) / (b - a)).clamp(0.0, 1.0);
    return u * u * (3 - 2 * u);
  }

  /// Triangular bump: 1 at [center], 0 by ±[halfWidth]; wraps around midnight so
  /// a late sunset / early sunrise glow doesn't clip at 00:00.
  static double _bump(double t, double center, double halfWidth) {
    var d = (t - center).abs();
    d = min(d, 1 - d); // shortest distance around the 24h circle
    return (1 - d / halfWidth).clamp(0.0, 1.0);
  }

  static double _lerp(double a, double b, double t) => a + (b - a) * t;
}
