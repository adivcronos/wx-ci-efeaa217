import 'package:flutter/material.dart';

import '../state/connection_state.dart';
import '../theme.dart';

/// A plain-English "what's going on with my connection" coach.
///
/// It reads the live [ConnectionController] and turns the raw state (proxy
/// applied? traffic verified? stealth fell back? backend down? an error?) into
/// one clear diagnosis, a short explanation anyone can follow, and — where it
/// helps — a button that does the obvious next thing. No jargon, no server IP.
class ConnectionCoach extends StatelessWidget {
  final ConnectionController controller;
  const ConnectionCoach({super.key, required this.controller});

  /// Show the coach as a bottom sheet. Reads the current controller state and
  /// rebuilds as it changes while open.
  static Future<void> show(BuildContext context, ConnectionController c) {
    return showModalBottomSheet<void>(
      context: context,
      backgroundColor: AppColors.surface,
      showDragHandle: true,
      isScrollControlled: true,
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.vertical(top: Radius.circular(20)),
      ),
      builder: (_) => ConnectionCoach(controller: c),
    );
  }

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: controller,
      builder: (ctx, _) {
        final d = _diagnose(controller);
        return SafeArea(
          child: Padding(
            padding: const EdgeInsets.fromLTRB(20, 4, 20, 20),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    Container(
                      padding: const EdgeInsets.all(9),
                      decoration: BoxDecoration(
                        color: d.color.withValues(alpha: 0.18),
                        borderRadius: BorderRadius.circular(11),
                      ),
                      child: Icon(d.icon, color: d.color, size: 22),
                    ),
                    const SizedBox(width: 12),
                    Expanded(
                      child: Text(
                        d.title,
                        style: const TextStyle(
                          fontSize: 17,
                          fontWeight: FontWeight.w600,
                          color: AppColors.textPrimary,
                        ),
                      ),
                    ),
                  ],
                ),
                const SizedBox(height: 14),
                Text(
                  d.body,
                  style: const TextStyle(
                    fontSize: 14,
                    height: 1.5,
                    color: AppColors.textMuted,
                  ),
                ),
                if (d.steps.isNotEmpty) ...[
                  const SizedBox(height: 14),
                  ...d.steps.map(
                    (s) => Padding(
                      padding: const EdgeInsets.only(bottom: 7),
                      child: Row(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          const Text('•  ',
                              style: TextStyle(color: AppColors.accentGlow)),
                          Expanded(
                            child: Text(
                              s,
                              style: const TextStyle(
                                fontSize: 13.5,
                                height: 1.45,
                                color: AppColors.textMuted,
                              ),
                            ),
                          ),
                        ],
                      ),
                    ),
                  ),
                ],
                const SizedBox(height: 18),
                Row(
                  children: [
                    if (d.action != null)
                      Expanded(
                        child: FilledButton(
                          style: FilledButton.styleFrom(
                            backgroundColor: AppColors.accent,
                            padding: const EdgeInsets.symmetric(vertical: 13),
                          ),
                          onPressed: () {
                            d.action!.run();
                            Navigator.of(ctx).maybePop();
                          },
                          child: Text(d.action!.label),
                        ),
                      ),
                    if (d.action != null) const SizedBox(width: 10),
                    Expanded(
                      child: OutlinedButton(
                        style: OutlinedButton.styleFrom(
                          foregroundColor: AppColors.textMuted,
                          side: const BorderSide(color: AppColors.field),
                          padding: const EdgeInsets.symmetric(vertical: 13),
                        ),
                        onPressed: () => Navigator.of(ctx).maybePop(),
                        child: const Text('Close'),
                      ),
                    ),
                  ],
                ),
              ],
            ),
          ),
        );
      },
    );
  }

  _Diagnosis _diagnose(ConnectionController c) {
    // Most actionable / most-wrong first.
    if (c.backendDown) {
      return _Diagnosis(
        Icons.sync_problem,
        AppColors.warning,
        "The app's helper stopped responding",
        'The background service that runs the connection went quiet. The app is '
            'already trying to restart it on its own.',
        steps: const [
          'Give it a few seconds to recover automatically.',
          'If it stays stuck, fully quit and reopen the app.',
        ],
        action: _Action('Re-check now', c.refresh),
      );
    }
    if (c.actionError != null) {
      return _Diagnosis(
        Icons.error_outline,
        AppColors.danger,
        "That didn't work",
        c.actionError!,
        steps: const ['Double-check your username and password, then try again.'],
        action: _Action('Dismiss', c.clearActionError),
      );
    }
    if (c.systemProxyNotApplied) {
      return _Diagnosis(
        Icons.lan_outlined,
        AppColors.danger,
        'Connected, but your apps are bypassing it',
        'The tunnel is up, but your system refused to route traffic through it — '
            'so your browser and apps are still using the normal network.',
        steps: const [
          'Disconnect and connect again — that usually makes it stick.',
          'If it keeps happening, turn on Stealth mode in Settings (it routes '
              'everything without needing the system proxy).',
        ],
        action: _Action('Disconnect', c.disconnect),
      );
    }
    if (c.stealthFellBack) {
      return _Diagnosis(
        Icons.shield_moon_outlined,
        AppColors.warning,
        'Stealth mode fell back to standard',
        'Stealth routing needs administrator permission and it wasn\'t granted, '
            'so the app switched to the standard proxy instead.',
        steps: const [
          'Disconnect, connect again, and approve the admin prompt when it asks.',
        ],
        action: _Action('Disconnect', c.disconnect),
      );
    }
    if (c.routingWarning) {
      return _Diagnosis(
        Icons.traffic_outlined,
        AppColors.warning,
        "Connected, but traffic isn't confirmed",
        'The connection is up and the proxy is set, but the app couldn\'t confirm '
            'your traffic is actually coming out of the server yet.',
        steps: const [
          'Wait a moment — this often clears itself on the next check.',
          'If the warning stays, disconnect and connect again.',
        ],
        action: _Action('Re-check now', c.refresh),
      );
    }
    if (c.optimisticConnecting || c.status.connecting) {
      return _Diagnosis(
        Icons.hourglass_bottom,
        AppColors.accentGlow,
        'Connecting…',
        'The app is trying each way in to the server (it starts with the fastest '
            'and falls back automatically). This usually takes a few seconds.',
        steps: const [],
        action: null,
      );
    }
    if (c.trafficVerified) {
      final stealth = c.stealthActive;
      return _Diagnosis(
        Icons.verified_user,
        AppColors.success,
        "You're protected",
        stealth
            ? 'Stealth mode is on — every app on this device is going through the '
                'tunnel, and traffic is confirmed coming out of the server.'
            : 'Traffic is confirmed going through the tunnel and coming out of the '
                'server. Your apps are protected.',
        steps: const [],
        action: null,
      );
    }
    if (c.status.connected) {
      return _Diagnosis(
        Icons.check_circle_outline,
        AppColors.accentGlow,
        'Connected',
        'The tunnel is up. The app is still confirming your traffic is routing '
            'through it — the shield turns green once that\'s verified.',
        steps: const [],
        action: _Action('Re-check now', c.refresh),
      );
    }
    return _Diagnosis(
      Icons.power_settings_new,
      AppColors.textMuted,
      'Not connected',
      'You\'re on your normal network right now. Enter your login and press '
          'Connect to start the tunnel.',
      steps: const [],
      action: null,
    );
  }
}

class _Action {
  final String label;
  final VoidCallback run;
  _Action(this.label, this.run);
}

class _Diagnosis {
  final IconData icon;
  final Color color;
  final String title;
  final String body;
  final List<String> steps;
  final _Action? action;
  _Diagnosis(
    this.icon,
    this.color,
    this.title,
    this.body, {
    this.steps = const [],
    this.action,
  });
}
