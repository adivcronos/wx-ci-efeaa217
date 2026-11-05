import 'dart:async';
import 'dart:io' show Platform;

import 'package:flutter/material.dart';
import 'package:screen_retriever/screen_retriever.dart';
import 'package:tray_manager/tray_manager.dart';
import 'package:window_manager/window_manager.dart';

import '../screens/tray_panel.dart';
import '../theme.dart';
import 'tray_panel_controller.dart';

/// Owns the system tray icon/menu and the hide-to-tray window behaviour.
/// The icon swaps abstractly (sun ↔ green dot) with connection state; the tray
/// is deliberately not VPN-branded. Closing the window hides to tray instead of
/// quitting; real exit goes through the "Quit" menu item ([onQuit]).
///
/// macOS only: a left-click on the tray icon shows a compact NSPopover-style
/// panel ([TrayPanel]) under the icon instead of opening the full window, and a
/// click outside dismisses it. This reuses the single app window (Flutter
/// desktop is single-window) — it shrinks to a frameless 320x200 panel, then
/// restores to the full window when the user opens the app. Right-click still
/// shows the Open/Quit menu. Windows and Linux keep the original behaviour: a
/// left-click just opens the full window.
class TrayWindowService with TrayListener, WindowListener {
  final Future<void> Function() onQuit;

  /// macOS popover state. Null on Windows/Linux (panel never used there).
  final TrayPanelController? panel;

  bool _transitioning = false; // guards overlapping show/hide/open animations

  TrayWindowService({required this.onQuit, this.panel});

  bool get _isMac => Platform.isMacOS;
  bool get _panelEnabled => _isMac && panel != null;

  // A single, constant weather-style glyph. The tray icon must NEVER change with
  // connection state — a menu-bar icon that flips to a "connected" indicator
  // reveals that this "weather app" is really a VPN. Disguise beats status here.
  static const _iconWeather = 'assets/tray/disconnected.png';

  static const _panelSize = Size(TrayPanel.width, TrayPanel.height);

  /// The app's normal window size, and the single source of truth for it —
  /// main.dart's WindowOptions uses this too. Kept in step because
  /// [openFull] restores to exactly this size after the macOS popover shrinks
  /// the window; if the two drifted, expanding the popover would resize the
  /// app to something other than how it launched.
  static const fullSize = Size(420, 760);

  Future<void> init() async {
    trayManager.addListener(this);
    windowManager.addListener(this);
    await trayManager.setIcon(_iconWeather, isTemplate: false);
    await trayManager.setToolTip('Weather');
    await _buildMenu();
    // Enable hide-to-tray (X button → hide, not quit) LAST — only after the tray
    // is fully up. If any step above throws (no tray support), prevent-close is
    // never turned on, so closing the window quits normally instead of hiding it
    // with no tray to bring it back.
    await windowManager.setPreventClose(true);
  }

  Future<void> _buildMenu() => trayManager.setContextMenu(
    Menu(
      items: [
        MenuItem(key: 'open', label: 'Open'),
        MenuItem.separator(),
        MenuItem(key: 'quit', label: 'Quit'),
      ],
    ),
  );

  /// Kept for the caller's API (main.dart mirrors connection state here), but the
  /// tray icon deliberately does NOT change — it stays the constant weather glyph
  /// so the menu bar never betrays the VPN. State is tracked only.
  Future<void> setConnected(bool connected) async {
    // No-op on purpose. Kept so main.dart's connection listener has something to
    // call, but the tray icon must stay the constant weather glyph.
  }

  Future<void> dispose() async {
    trayManager.removeListener(this);
    windowManager.removeListener(this);
  }

  Future<void> _show() async {
    await windowManager.show();
    await windowManager.focus();
  }

  // --- macOS popover panel -------------------------------------------------

  Future<void> _togglePanel() async {
    if (_transitioning) return;
    if (panel!.visible) {
      await hidePanel();
    } else {
      await showPanel();
    }
  }

  /// Reconfigure the window into the frameless popover, position it under the
  /// tray icon and fade it in (150ms). macOS only.
  Future<void> showPanel() async {
    if (!_panelEnabled) return;
    _transitioning = true;
    try {
      await windowManager.setOpacity(0);
      await windowManager.setAlwaysOnTop(true);
      await windowManager.setSkipTaskbar(true);
      await windowManager.setTitleBarStyle(
        TitleBarStyle.hidden,
        windowButtonVisibility: false,
      );
      await windowManager.setBackgroundColor(Colors.transparent);
      await windowManager.setSize(_panelSize);
      await windowManager.setPosition(await _panelPosition());
      panel!.setVisible(true);
      await windowManager.show();
      await windowManager.focus();
      await _fade(0, 1);
    } finally {
      _transitioning = false;
    }
  }

  /// Fade the popover out (150ms) and hide the window. macOS only.
  Future<void> hidePanel() async {
    if (!_panelEnabled) return;
    _transitioning = true;
    try {
      await _fade(1, 0);
      await windowManager.hide();
      panel!.setVisible(false);
      await windowManager.setOpacity(1);
    } finally {
      _transitioning = false;
    }
  }

  /// Leave the popover (if showing) and restore the full app window. Callers do
  /// any navigation (e.g. push the gate) after this resolves.
  Future<void> openFull() async {
    _transitioning = true;
    try {
      panel?.setVisible(false);
      if (_isMac) {
        await windowManager.setAlwaysOnTop(false);
        await windowManager.setSkipTaskbar(false);
        await windowManager.setTitleBarStyle(
          TitleBarStyle.normal,
          windowButtonVisibility: true,
        );
        await windowManager.setBackgroundColor(AppColors.bg);
        await windowManager.setSize(fullSize);
        await windowManager.center();
        await windowManager.setOpacity(1);
      }
      await windowManager.show();
      await windowManager.focus();
    } finally {
      _transitioning = false;
    }
  }

  /// Screen position for the popover: centred under the tray icon (from
  /// [TrayManager.getBounds]), clamped to the primary display; falls back to the
  /// top-right corner under the menu bar if the icon bounds are unavailable.
  Future<Offset> _panelPosition() async {
    const w = TrayPanel.width;
    Display? display;
    try {
      display = await screenRetriever.getPrimaryDisplay();
    } catch (_) {}
    try {
      final b = await trayManager.getBounds();
      if (b != null) {
        var x = b.left + b.width / 2 - w / 2;
        final y = b.top + b.height + 6; // just below the menu-bar icon
        final sw = display?.size.width;
        if (sw != null) {
          if (x + w > sw - 8) x = sw - w - 8;
          if (x < 8) x = 8;
        }
        return Offset(x, y);
      }
    } catch (_) {}
    final sw = display?.size.width ?? (w + 24);
    return Offset(sw - w - 12, 32);
  }

  /// Step the window opacity from [from] to [to] over ~150ms.
  Future<void> _fade(double from, double to) async {
    const steps = 8;
    const stepMs = 150 ~/ steps;
    for (var i = 1; i <= steps; i++) {
      final v = from + (to - from) * (i / steps);
      await windowManager.setOpacity(v.clamp(0.0, 1.0));
      await Future<void>.delayed(const Duration(milliseconds: stepMs));
    }
  }

  // --- TrayListener --------------------------------------------------------
  @override
  void onTrayIconMouseDown() {
    if (_panelEnabled) {
      _togglePanel();
    } else {
      _show();
    }
  }

  @override
  void onTrayIconRightMouseDown() {
    trayManager.popUpContextMenu();
  }

  @override
  void onTrayMenuItemClick(MenuItem menuItem) {
    switch (menuItem.key) {
      case 'open':
        _panelEnabled ? openFull() : _show();
        break;
      case 'quit':
        onQuit();
        break;
    }
  }

  // --- WindowListener ------------------------------------------------------
  @override
  void onWindowClose() async {
    if (await windowManager.isPreventClose()) {
      await windowManager.hide(); // keep running in the tray
    }
  }

  @override
  void onWindowBlur() {
    // Click-outside dismiss for the popover (macOS). Guarded so it never
    // touches the full window, and never fires mid-transition.
    if (_panelEnabled && panel!.visible && !_transitioning) {
      hidePanel();
    }
  }
}
