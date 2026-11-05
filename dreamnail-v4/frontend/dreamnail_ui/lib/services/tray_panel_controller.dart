import 'package:flutter/foundation.dart';

/// Whether the macOS menu-bar popover panel is currently showing. The tray
/// service flips this; [DreamnailApp] watches it to overlay the compact panel
/// over the (reused, resized) window. It is never set true on Windows/Linux, so
/// those platforms' behaviour is entirely unchanged.
class TrayPanelController extends ChangeNotifier {
  bool _visible = false;
  bool get visible => _visible;

  void setVisible(bool v) {
    if (_visible == v) return;
    _visible = v;
    notifyListeners();
  }
}
