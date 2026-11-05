import 'package:flutter/material.dart';

import '../theme.dart';

/// Connection quality derived from the live ping, for the color-coded indicator
/// on the VPN screen. Thresholds are tuned for an SSH-tunnelled hop (see
/// core.py's ping sampling): sub-80ms feels instant, under ~180ms is usable,
/// beyond that is laggy.
enum ConnQuality { excellent, good, poor, unknown }

class QualityInfo {
  final ConnQuality level;
  final String label;
  final Color color;
  final int bars; // filled signal bars, 0..3

  const QualityInfo(this.level, this.label, this.color, this.bars);

  static QualityInfo fromPing(int? ms) {
    if (ms == null) {
      return const QualityInfo(
        ConnQuality.unknown,
        'No signal',
        AppColors.textMuted,
        0,
      );
    }
    if (ms < 80) {
      return const QualityInfo(
        ConnQuality.excellent,
        'Excellent',
        AppColors.success,
        3,
      );
    }
    if (ms < 180) {
      return const QualityInfo(ConnQuality.good, 'Good', AppColors.warning, 2);
    }
    return const QualityInfo(ConnQuality.poor, 'Poor', AppColors.danger, 1);
  }
}
