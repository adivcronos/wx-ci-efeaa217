import 'package:flutter/foundation.dart';

/// A [ChangeNotifier] whose notification is public, so a per-frame simulation
/// can drive a [CustomPainter]'s `repaint:` without rebuilding any widgets.
/// (Calling `notifyListeners()` directly on a bare ChangeNotifier trips the
/// `invalid_use_of_protected_member` lint — this exposes it intentionally.)
class RepaintNotifier extends ChangeNotifier {
  void tick() => notifyListeners();
}
