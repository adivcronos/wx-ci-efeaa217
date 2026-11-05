import 'dart:async';

import 'package:flutter/foundation.dart';

import '../api.dart';
import '../models/weather.dart';

/// Polls GET /weather every 10 minutes (plus on-demand pull-to-refresh).
class WeatherState extends ChangeNotifier {
  final ApiClient api;
  WeatherData? data;
  bool loading = false;
  String? error;
  Timer? _timer;

  WeatherState(this.api);

  void start() {
    refresh();
    _timer?.cancel();
    _timer = Timer.periodic(const Duration(minutes: 10), (_) => refresh());
  }

  /// Stop polling while the window is hidden to the tray.
  void pause() {
    _timer?.cancel();
    _timer = null;
  }

  /// Resume, refreshing immediately so a long hide doesn't show stale weather.
  void resume() {
    if (_timer != null) return;
    start();
  }

  Future<void> refresh() async {
    loading = true;
    _safeNotify();
    try {
      data = await api.weather();
      error = null;
    } catch (e) {
      error = 'Could not load weather';
    } finally {
      loading = false;
      _safeNotify();
    }
  }

  bool _disposed = false;

  /// A fetch in flight when the controller goes away would otherwise notify a
  /// disposed notifier, which throws.
  void _safeNotify() {
    if (_disposed) return;
    notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _timer?.cancel();
    _timer = null;
    super.dispose();
  }
}
