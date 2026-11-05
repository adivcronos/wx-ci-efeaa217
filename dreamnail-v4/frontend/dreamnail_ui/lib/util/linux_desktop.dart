import 'dart:io';

import 'package:flutter/services.dart' show rootBundle;

/// Installs (and refreshes) a freedesktop `.desktop` entry + hicolor icon so
/// Dreamnail shows up in the GNOME/KDE app launcher and gets a proper dock icon
/// on Linux. Called once at startup; safe to run every launch (idempotent, and
/// it re-points `Exec` at the current single-file path so a moved/updated
/// download self-heals).
///
/// Everything is best-effort and guarded — a headless box, a read-only home, or
/// missing tools must never stop the app from opening.
Future<void> installLinuxDesktopEntry() async {
  if (!Platform.isLinux) return;
  try {
    final env = Platform.environment;
    final home = env['HOME'];
    if (home == null || home.isEmpty) return;
    final dataHome = _nonEmpty(env['XDG_DATA_HOME']) ?? '$home/.local/share';

    // The self-extracting stub exports the path of the file the user actually
    // launched (DREAMNAIL_LAUNCHER); fall back to the running executable when
    // run some other way (dev, or launched from the staged copy directly).
    final target =
        _nonEmpty(env['DREAMNAIL_LAUNCHER']) ?? Platform.resolvedExecutable;

    // --- install the icon into the hicolor theme (scalable svg + 512 png) ---
    final iconsBase = '$dataHome/icons/hicolor';
    await _writeAsset(
      'assets/icons/logo.svg',
      '$iconsBase/scalable/apps/weather.svg',
    );
    await _writeAsset(
      'assets/icons/logo.png',
      '$iconsBase/512x512/apps/weather.png',
    );

    // --- write the .desktop entry ---
    final appsDir = Directory('$dataHome/applications');
    await appsDir.create(recursive: true);
    final desktop = File('${appsDir.path}/weather.desktop');
    final content = _desktopEntry(target);
    // Only rewrite when something changed (keeps mtime stable, avoids needless
    // desktop-db churn on every launch).
    if (!await _fileHas(desktop, content)) {
      await desktop.writeAsString(content, flush: true);
      await Process.run('chmod', ['644', desktop.path]);
      // Best-effort cache refresh so the entry/icon appear without a re-login.
      await _run('update-desktop-database', [appsDir.path]);
      await _run('gtk-update-icon-cache', ['-f', '-t', iconsBase]);
    }
  } catch (_) {
    // Never let launcher integration break app launch.
  }
}

String _desktopEntry(String exec) {
  // Exec is quoted per the freedesktop spec (double quotes, backslash-escaped).
  final quoted = '"${exec.replaceAll(r'\', r'\\').replaceAll('"', r'\"')}"';
  return '[Desktop Entry]\n'
      'Type=Application\n'
      'Name=Weather\n'
      'GenericName=Weather\n'
      'Comment=Weather and forecasts\n'
      'Exec=$quoted\n'
      'Icon=weather\n'
      'Terminal=false\n'
      'Categories=Utility;\n'
      // Ties the running window to this entry so the dock shows our icon.
      'StartupWMClass=com.dreamnail.dreamnail_ui\n'
      'StartupNotify=true\n';
}

/// Copy a bundled asset to [dest] (creating parents) only if it differs, so we
/// don't rewrite the icon on every launch.
Future<void> _writeAsset(String asset, String dest) async {
  final data = await rootBundle.load(asset);
  final bytes = data.buffer.asUint8List(data.offsetInBytes, data.lengthInBytes);
  final f = File(dest);
  if (await f.exists() && await f.length() == bytes.length) return;
  await f.parent.create(recursive: true);
  await f.writeAsBytes(bytes, flush: true);
}

Future<bool> _fileHas(File f, String content) async {
  try {
    return await f.exists() && await f.readAsString() == content;
  } catch (_) {
    return false;
  }
}

Future<void> _run(String exe, List<String> args) async {
  try {
    await Process.run(exe, args);
  } catch (_) {
    // tool not installed — the loose files still work without a cache refresh.
  }
}

String? _nonEmpty(String? s) => (s == null || s.isEmpty) ? null : s;
