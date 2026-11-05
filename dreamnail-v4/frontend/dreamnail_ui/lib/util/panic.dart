// Detects N rapid key presses within a time window — used for the double-Esc
// panic gesture on the VPN screen (double-Esc). Pure + dependency-free so it is unit-testable.
class MultiPressDetector {
  final int count;
  final Duration within;
  final List<DateTime> _hits = [];

  MultiPressDetector({
    this.count = 3,
    this.within = const Duration(seconds: 2),
  });

  /// Records a press at [now] (defaults to `DateTime.now()`). Returns true once
  /// [count] presses have landed inside [within]; the buffer then resets so the
  /// next gesture starts clean.
  bool register([DateTime? now]) {
    final t = now ?? DateTime.now();
    _hits.add(t);
    final cutoff = t.subtract(within);
    _hits.removeWhere((h) => h.isBefore(cutoff));
    if (_hits.length >= count) {
      _hits.clear();
      return true;
    }
    return false;
  }

  void reset() => _hits.clear();
}
