import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:hotkey_manager/hotkey_manager.dart';

/// A single OS-registered global shortcut that snaps the app back to the weather
/// disguise from anywhere — even when Dreamnail is not the focused window.
///
/// This is deliberately NOT a keystroke listener. It asks the operating system to
/// notify the app when ONE specific accelerator (Ctrl+Alt+Esc) is pressed; the OS
/// delivers only that event and nothing else. The app never sees any other key,
/// so there is no keylogging surface and nothing for antivirus to flag. The
/// in-focus gesture stays the plain double-Esc handled on the VPN screen.
///
/// Platform reality: the global registration works on Windows, macOS and X11.
/// Wayland won't let apps grab keys, so there the same Ctrl+Alt+Esc is added as
/// a DESKTOP shortcut from Settings (see the engine's plat::shortcut), which
/// pokes a trigger file main.dart watches. The in-focus double-Esc always works
/// regardless. Every call here is best-effort and swallows failures so a desktop
/// that can't register a global shortcut never breaks the app.
class PanicHotkey {
  PanicHotkey._();
  static final PanicHotkey instance = PanicHotkey._();

  // Ctrl+Alt+Esc: a real accelerator (not bare Esc, which cannot be a global
  // shortcut and would collide with every app's Escape). Modifiers make an
  // accidental global trigger very unlikely.
  static final HotKey _key = HotKey(
    key: PhysicalKeyboardKey.escape,
    modifiers: [HotKeyModifier.control, HotKeyModifier.alt],
    scope: HotKeyScope.system,
  );

  bool _initialised = false;
  bool _registered = false;

  /// Register the global boss key. Called once at startup (main.dart) and kept
  /// for the app's lifetime. Safe to call repeatedly.
  Future<void> enable(VoidCallback onPanic) async {
    try {
      if (!_initialised) {
        await hotKeyManager.unregisterAll();
        _initialised = true;
      }
      if (_registered) return;
      await hotKeyManager.register(_key, keyDownHandler: (_) => onPanic());
      _registered = true;
    } catch (e) {
      // A desktop that can't register a system shortcut (e.g. some Wayland
      // sessions) must not break the hidden layer — the in-focus double-Esc
      // still works. Leave _registered false so a later enable() can retry.
      debugPrint('panic hotkey: global registration unavailable ($e)');
    }
  }

  /// Drop the global shortcut (app shutdown).
  Future<void> disable() async {
    if (!_registered) return;
    try {
      await hotKeyManager.unregister(_key);
    } catch (_) {
      // Best-effort; unregisterAll on next enable() is the backstop.
    } finally {
      _registered = false;
    }
  }
}
