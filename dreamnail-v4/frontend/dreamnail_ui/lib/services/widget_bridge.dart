import 'dart:io';

import 'package:flutter/services.dart';

import '../models/weather.dart';

/// Pushes connection status to the macOS WidgetKit widget.
///
/// It talks to native code over a [MethodChannel]; the native side (added by
/// macos/tool/add_dreamnail_widget.rb) writes the values into the shared App
/// Group and asks WidgetCenter to reload the timeline. On every other platform
/// — and on a macOS build where the widget target hasn't been wired yet — the
/// call is a guarded no-op, so this is always safe to invoke.
class WidgetBridge {
  static const MethodChannel _channel = MethodChannel('com.dreamnail/widget');

  static bool get isSupported => Platform.isMacOS;

  /// Mirror [s] into the widget. Never throws.
  static Future<void> update(ConnStatus s) async {
    if (!isSupported) return;
    try {
      await _channel.invokeMethod<void>('updateStatus', <String, dynamic>{
        'connected': s.connected,
        'connecting': s.connecting,
        'uptimeSeconds': s.uptimeSeconds,
        'downloadBps': s.downloadBps,
        'uploadBps': s.uploadBps,
        'pingMs': s.pingMs ?? -1,
      });
    } on MissingPluginException {
      // Native handler not present (widget target not wired on this build).
    } catch (_) {
      // Widget updates are strictly best-effort — never disturb the app.
    }
  }

  /// Reset the widget to the disconnected state (e.g. on quit).
  static Future<void> clear() => update(ConnStatus.disconnected());
}
