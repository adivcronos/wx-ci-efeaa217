import 'package:flutter_test/flutter_test.dart';
import 'package:dreamnail_ui/util/conn_quality.dart';

void main() {
  group('QualityInfo.fromPing', () {
    test('null ping = No signal (the disconnected/unknown state)', () {
      final q = QualityInfo.fromPing(null);
      expect(q.level, ConnQuality.unknown);
      expect(q.label, 'No signal');
      expect(q.bars, 0);
    });

    test('sub-80ms = Excellent (3 bars)', () {
      final q = QualityInfo.fromPing(24);
      expect(q.level, ConnQuality.excellent);
      expect(q.label, 'Excellent');
      expect(q.bars, 3);
    });

    test('80–179ms = Good (2 bars)', () {
      expect(QualityInfo.fromPing(80).level, ConnQuality.good);
      expect(QualityInfo.fromPing(179).label, 'Good');
      expect(QualityInfo.fromPing(120).bars, 2);
    });

    test('180ms+ = Poor (1 bar)', () {
      final q = QualityInfo.fromPing(180);
      expect(q.level, ConnQuality.poor);
      expect(q.label, 'Poor');
      expect(q.bars, 1);
    });
  });
}
