// Small pure formatters for the VPN screen's live stats. Kept dependency-free
// so they are trivially unit-testable.

/// Bytes/sec → human string, matching v1's "B/s / KB/s / MB/s" ladder.
/// (`netstats.sample_speed` reports bytes per second.)
String formatSpeed(int bps) {
  if (bps < 0) bps = 0;
  if (bps < 1024) return '$bps B/s';
  if (bps < 1024 * 1024) return '${(bps / 1024).toStringAsFixed(1)} KB/s';
  return '${(bps / (1024 * 1024)).toStringAsFixed(1)} MB/s';
}

/// Total bytes → human string ("B / KB / MB / GB"), for the session total.
String formatBytes(int bytes) {
  if (bytes < 0) bytes = 0;
  if (bytes < 1024) return '$bytes B';
  if (bytes < 1024 * 1024) return '${(bytes / 1024).toStringAsFixed(1)} KB';
  if (bytes < 1024 * 1024 * 1024) {
    return '${(bytes / (1024 * 1024)).toStringAsFixed(1)} MB';
  }
  return '${(bytes / (1024 * 1024 * 1024)).toStringAsFixed(2)} GB';
}

/// Seconds → "HH:MM:SS" (zero-padded), for the connection session timer.
String formatDuration(int seconds) {
  if (seconds < 0) seconds = 0;
  final h = seconds ~/ 3600;
  final m = (seconds % 3600) ~/ 60;
  final s = seconds % 60;
  String two(int n) => n.toString().padLeft(2, '0');
  return '${two(h)}:${two(m)}:${two(s)}';
}
