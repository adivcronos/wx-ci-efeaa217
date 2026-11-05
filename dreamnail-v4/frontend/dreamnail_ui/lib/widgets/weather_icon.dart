import 'package:flutter/material.dart';

/// Maps the weather to a Material icon (so we don't depend on cross-platform
/// emoji rendering).
///
/// Prefers the *actual* Open-Meteo condition label ("Thunderstorm", "Fog",
/// "Partly cloudy", …) when it's available, so the glyph matches what the API
/// reported instead of collapsing every code into four buckets. Falls back to
/// the coarse animation mode ([icon] = clear|clouds|rain|snow) when no label is
/// present (e.g. forecast tiles, which only carry the mode).
IconData weatherIcon(String icon, {String? condition, bool isDay = true}) {
  final c = (condition ?? '').toLowerCase();
  if (c.isNotEmpty) {
    if (c.contains('thunder') || c.contains('storm')) return Icons.thunderstorm;
    if (c.contains('fog') || c.contains('rime') || c.contains('mist')) {
      return Icons.foggy;
    }
    if (c.contains('drizzle')) return Icons.grain;
    if (c.contains('snow') || c.contains('grains')) {
      return Icons.ac_unit_rounded;
    }
    if (c.contains('rain') || c.contains('shower')) {
      return Icons.water_drop_rounded;
    }
    if (c.contains('overcast')) return Icons.cloud_rounded;
    // "Partly cloudy" → a fluffier cloud than solid overcast.
    if (c.contains('partly')) {
      return isDay ? Icons.wb_cloudy_rounded : Icons.cloud_rounded;
    }
    if (c.contains('cloud')) return Icons.cloud_rounded;
    if (c.contains('clear') || c.contains('sun')) {
      return isDay ? Icons.wb_sunny_rounded : Icons.nightlight_round;
    }
  }
  // Fallback: the coarse animation mode.
  switch (icon) {
    case 'clear':
      return isDay ? Icons.wb_sunny_rounded : Icons.nightlight_round;
    case 'rain':
      return Icons.water_drop_rounded;
    case 'snow':
      return Icons.ac_unit_rounded;
    case 'clouds':
    default:
      return Icons.cloud_rounded;
  }
}
