import 'package:flutter/material.dart';
import 'package:provider/provider.dart';

import '../models/weather.dart';
import '../state/connection_state.dart';
import '../state/weather_state.dart';
import '../theme.dart';
import '../util/conn_quality.dart';
import '../util/format.dart';

/// The compact macOS menu-bar popover (320x200), shown under the tray icon.
/// A quick-glance surface driven by the same live [ConnectionController] as the
/// full app: status pill, and when connected a 2x2 grid of uptime / download /
/// upload / ping; when disconnected, a single Connect button that expands into
/// the full app on the VPN screen. Fades itself in over 150ms on mount.
class TrayPanel extends StatelessWidget {
  final VoidCallback onClose;
  final VoidCallback onConnect;
  const TrayPanel({super.key, required this.onClose, required this.onConnect});

  static const double width = 320;
  static const double height = 200;

  @override
  Widget build(BuildContext context) {
    final cs = context.watch<ConnectionController>();
    final s = cs.status;
    final connected = s.connected;
    // Falls back to the backend's own default until the first weather poll.
    final city = context.watch<WeatherState>().data?.city ?? 'Melbourne';

    return TweenAnimationBuilder<double>(
      tween: Tween(begin: 0, end: 1),
      duration: const Duration(milliseconds: 150),
      curve: Curves.easeOut,
      builder: (_, t, child) => Opacity(
        opacity: t.clamp(0.0, 1.0),
        child: Transform.scale(scale: 0.98 + 0.02 * t, child: child),
      ),
      child: Center(
        child: SizedBox(
          width: width,
          height: height,
          child: Container(
            decoration: BoxDecoration(
              color: AppColors.bg, // was a hand-copied 0xFF0F0F1A
              borderRadius: BorderRadius.circular(16),
              border: Border.all(color: Colors.white.withValues(alpha: 0.10)),
              boxShadow: [
                BoxShadow(
                  color: Colors.black.withValues(alpha: 0.45),
                  blurRadius: 24,
                  offset: const Offset(0, 8),
                ),
              ],
            ),
            padding: const EdgeInsets.fromLTRB(16, 10, 16, 10),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                _header(),
                const SizedBox(height: AppSpace.sm),
                Center(child: _statusPill(connected)),
                const SizedBox(height: AppSpace.sm),
                Expanded(child: connected ? _statGrid(s) : _connectBody()),
                _footer(city),
              ],
            ),
          ),
        ),
      ),
    );
  }

  Widget _header() => Row(
    children: [
      const Icon(Icons.nightlight_round, color: AppColors.accent, size: 16),
      const SizedBox(width: 6),
      const Text(
        'Dreamnail',
        style: TextStyle(
          color: AppColors.textPrimary,
          fontSize: 14,
          fontWeight: FontWeight.w600,
          letterSpacing: 0.2,
        ),
      ),
      const Spacer(),
      _iconButton(Icons.close, onClose, 'Close'),
    ],
  );

  Widget _iconButton(IconData icon, VoidCallback onTap, String tip) => Tooltip(
    message: tip,
    child: InkWell(
      onTap: onTap,
      borderRadius: BorderRadius.circular(6),
      child: Padding(
        padding: const EdgeInsets.all(3),
        child: Icon(icon, color: AppColors.textMuted, size: 18),
      ),
    ),
  );

  Widget _statusPill(bool connected) {
    final color = connected ? AppColors.success : AppColors.textMuted;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 5),
      decoration: BoxDecoration(
        color: color.withValues(alpha: 0.14),
        borderRadius: BorderRadius.circular(999),
        border: Border.all(color: color.withValues(alpha: 0.4)),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Container(
            width: 8,
            height: 8,
            decoration: BoxDecoration(color: color, shape: BoxShape.circle),
          ),
          const SizedBox(width: 7),
          Text(
            connected ? 'Connected' : 'Disconnected',
            style: TextStyle(
              color: color,
              fontSize: 12.5,
              fontWeight: FontWeight.w600,
              letterSpacing: 0.2,
            ),
          ),
        ],
      ),
    );
  }

  Widget _statGrid(ConnStatus s) {
    final q = QualityInfo.fromPing(s.pingMs);
    return Column(
      children: [
        Expanded(
          child: Row(
            children: [
              Expanded(
                child: _statCard(
                  Icons.timer_outlined,
                  'UPTIME',
                  formatDuration(s.uptimeSeconds),
                  AppColors.textPrimary,
                ),
              ),
              const SizedBox(width: AppSpace.sm),
              Expanded(
                child: _statCard(
                  Icons.network_ping,
                  'PING',
                  s.pingMs == null ? '—' : '${s.pingMs} ms',
                  q.color,
                ),
              ),
            ],
          ),
        ),
        const SizedBox(height: 6),
        Expanded(
          child: Row(
            children: [
              Expanded(
                child: _statCard(
                  Icons.south,
                  'DOWNLOAD',
                  formatSpeed(s.downloadBps),
                  AppColors.accent,
                ),
              ),
              const SizedBox(width: AppSpace.sm),
              Expanded(
                child: _statCard(
                  Icons.north,
                  'UPLOAD',
                  formatSpeed(s.uploadBps),
                  AppColors.success,
                ),
              ),
            ],
          ),
        ),
      ],
    );
  }

  Widget _statCard(IconData icon, String label, String value, Color color) =>
      Container(
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 4),
        decoration: BoxDecoration(
          color: Colors.white.withValues(alpha: 0.04),
          borderRadius: BorderRadius.circular(AppRadius.sm),
          border: Border.all(color: Colors.white.withValues(alpha: 0.06)),
        ),
        child: Row(
          children: [
            Icon(icon, size: 16, color: color),
            const SizedBox(width: 8),
            Expanded(
              child: Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(label, style: AppText.label),
                  const SizedBox(height: 1),
                  Text(
                    value,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: const TextStyle(
                      color: AppColors.textPrimary,
                      fontSize: 14,
                      fontWeight: FontWeight.w600,
                      letterSpacing: -0.2,
                    ),
                  ),
                ],
              ),
            ),
          ],
        ),
      );

  Widget _connectBody() => Center(
    child: SizedBox(
      width: double.infinity,
      child: FilledButton.icon(
        style: FilledButton.styleFrom(
          backgroundColor: AppColors.accent,
          padding: const EdgeInsets.symmetric(vertical: 12),
          shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(999),
          ),
        ),
        onPressed: onConnect,
        icon: const Icon(Icons.lock_open, size: 18),
        label: const Text(
          'Connect',
          style: TextStyle(fontSize: 14, fontWeight: FontWeight.w600),
        ),
      ),
    ),
  );

  /// The city comes from the live weather data. It was hardcoded to
  /// "Melbourne AU" while every weather path in the app is Sydney, so the
  /// popover confidently displayed the wrong city.
  Widget _footer(String city) => Row(
    mainAxisAlignment: MainAxisAlignment.center,
    children: [
      const Icon(Icons.public, size: 12, color: AppColors.textMuted),
      const SizedBox(width: 5),
      Text(city, style: AppText.label.copyWith(letterSpacing: 0.6)),
    ],
  );
}
