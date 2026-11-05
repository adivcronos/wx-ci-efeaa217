import 'dart:io';

import 'package:flutter/material.dart';

import 'app_paths.dart';

/// A "skin pack" for the weather disguise. A skin is intentionally LIGHT-TOUCH:
/// it nudges the computed sky gradient toward a hue, leaving all the real
/// day/night + condition logic ([SkyPalette]) untouched. "Default violet" is a
/// no-op (strength 0), so picking it is identical to the original look.
@immutable
class Skin {
  final String id;
  final String label;

  /// The hue the sky is nudged toward.
  final Color tint;

  /// How far, 0..~0.3. 0 = untouched.
  final double strength;

  const Skin(this.id, this.label, this.tint, this.strength);

  /// Nudge every colour of a gradient toward [tint] by [strength].
  List<Color> apply(List<Color> base) {
    if (strength <= 0) return base;
    return [for (final c in base) Color.lerp(c, tint, strength)!];
  }
}

const List<Skin> kSkins = [
  Skin('default', 'Default violet', Color(0xFF7C6CFF), 0.0),
  Skin('sunset', 'Sunset', Color(0xFFFF8A5B), 0.18),
  Skin('ocean', 'Ocean', Color(0xFF3BC9DB), 0.16),
  Skin('forest', 'Forest', Color(0xFF4CAF7D), 0.15),
  Skin('rose', 'Rose', Color(0xFFFF6B9D), 0.15),
  Skin('mono', 'Monochrome', Color(0xFF8A8AA0), 0.26),
];

Skin skinById(String id) =>
    kSkins.firstWhere((s) => s.id == id, orElse: () => kSkins.first);

/// The active skin — listenable so the weather background repaints on change.
final ValueNotifier<Skin> activeSkin = ValueNotifier<Skin>(kSkins.first);

String _skinFile() => AppPaths.file('skin.pref');

/// Load the saved skin at startup (best-effort; defaults when absent).
void loadSkin() {
  try {
    final id = File(_skinFile()).readAsStringSync().trim();
    activeSkin.value = skinById(id);
  } catch (_) {
    // No preference yet — keep the default.
  }
}

/// Pick a skin and persist it (best-effort).
void setSkin(Skin s) {
  activeSkin.value = s;
  try {
    File(_skinFile()).writeAsStringSync(s.id);
  } catch (_) {
    // Persisting is best-effort; the choice still applies this session.
  }
}
