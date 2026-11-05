import 'dart:io';

/// Single source of truth for where the app keeps its per-user data: the
/// encrypted store (`config.enc`), the plain routing preference (`routing.pref`),
/// and the UI's single-instance lock (`ui.lock`).
///
/// This MUST agree with the Rust engine's `data_dir()` (crates/dreamnail-ffi) —
/// both resolve the same directory the same way, so change the rule in these two
/// places together and nowhere else.
///
/// Resolution order:
///   1. `DREAMNAIL_DATA_DIR` env var, if set — an override knob (kept in step
///      with the Rust engine's data_dir()). Normally unset.
///   2. The per-OS default: `%APPDATA%\Dreamnail` (Windows) / `~/.dreamnail`.
class AppPaths {
  /// The resolved app data directory (no trailing separator).
  static String dataDir() {
    final override = Platform.environment['DREAMNAIL_DATA_DIR'];
    if (override != null && override.isNotEmpty) return override;
    final sep = Platform.pathSeparator;
    if (Platform.isWindows) {
      final appData = Platform.environment['APPDATA'] ?? '.';
      return '$appData${sep}Dreamnail';
    }
    final home = Platform.environment['HOME'] ?? '.';
    return '$home$sep.dreamnail';
  }

  /// A file inside [dataDir].
  static String file(String name) =>
      '${dataDir()}${Platform.pathSeparator}$name';
}
