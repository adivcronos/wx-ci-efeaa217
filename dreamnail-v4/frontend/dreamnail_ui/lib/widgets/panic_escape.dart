import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import '../util/panic.dart';

/// Double-Esc anywhere in the hidden layer snaps back to the weather disguise.
///
/// Listens at the keyboard level (HardwareKeyboard) rather than through a
/// screen's focus node, so it no longer depends on where keyboard focus happens
/// to sit. The old per-screen listener only heard Esc while focus was inside the
/// VPN screen, so the gesture did nothing on the access-key gate, and a popup
/// (CONFIG sheet, LOGS, a dialog) would take the first Esc for itself.
///
/// It only sees keys delivered to this app's own window: normal in-app key
/// handling, not a system-wide hook. "Hidden layer" = anything pushed on top of
/// the weather home route, i.e. the navigator can pop.
class PanicEscape extends StatefulWidget {
  final GlobalKey<NavigatorState> navigatorKey;
  final VoidCallback onPanic;
  final Widget child;

  const PanicEscape({
    super.key,
    required this.navigatorKey,
    required this.onPanic,
    required this.child,
  });

  @override
  State<PanicEscape> createState() => _PanicEscapeState();
}

class _PanicEscapeState extends State<PanicEscape> {
  final _presses = MultiPressDetector(count: 2); // within 2s

  @override
  void initState() {
    super.initState();
    HardwareKeyboard.instance.addHandler(_onKey);
  }

  @override
  void dispose() {
    HardwareKeyboard.instance.removeHandler(_onKey);
    super.dispose();
  }

  bool _onKey(KeyEvent e) {
    if (e is! KeyDownEvent || e.logicalKey != LogicalKeyboardKey.escape) {
      return false;
    }
    // Only inside the hidden layer; on the weather screen Esc stays ordinary.
    if (!(widget.navigatorKey.currentState?.canPop() ?? false)) {
      _presses.reset();
      return false;
    }
    if (_presses.register()) {
      widget.onPanic();
      return true; // the panic press is consumed
    }
    // The first press still does its normal job (e.g. closes a popup).
    return false;
  }

  @override
  Widget build(BuildContext context) => widget.child;
}
