import 'dart:async';

import 'package:flutter/material.dart';
import 'package:provider/provider.dart';

import '../api.dart';
import '../config.dart';
import '../state/connection_state.dart';
import '../theme.dart';
import '../util/format.dart';
import '../util/skins.dart';
import '../widgets/connection_coach.dart';

/// Screen 4 — settings, shown as a modal sheet over the VPN screen. Reads/writes
/// the backend's GET/POST /settings (timeout + auto-reconnect + routing mode).
/// Styled to match the CRT-terminal VPN skin.
class SettingsSheet extends StatefulWidget {
  final ApiClient api;
  final ValueChanged<int> onTimeoutChanged;
  const SettingsSheet({
    super.key,
    required this.api,
    required this.onTimeoutChanged,
  });

  @override
  State<SettingsSheet> createState() => _SettingsSheetState();
}

class _SettingsSheetState extends State<SettingsSheet> {
  int _timeout = 5;
  bool _autoReconnect = true;
  bool _autoConnect = false; // connect automatically after unlock
  // Auto-connect on untrusted networks + the networks marked safe, and the
  // network we're on right now (null = offline / unknown).
  bool _untrusted = false;
  List<Map<String, String>> _trusted = [];
  Map<String, dynamic>? _here;
  // Boss-key desktop shortcut (Wayland only): {needed, desktop, installed, command}.
  Map<String, dynamic>? _boss;
  bool _bossBusy = false;
  String? _bossMsg;
  String _planExpiry = ''; // local renewal reminder, ISO yyyy-mm-dd or ''
  double _dataLimitGb = 0; // plan data cap in GB, 0 = no cap
  bool _stealth = false; // routing_mode == 'tun'
  bool _adblock = false; // block ad/tracker domains
  bool _force443 = false; // skip port 22, go straight to the 443 doors
  bool _weaknet = false; // weak-network mode: safe MTU + more stripes
  String _realityDecoy = ''; // chosen decoy SNI for the Reality door
  List<Map<String, String>> _realityDecoys = const []; // [{label, sni}]
  bool _errorReports = true;
  // All-time data usage (sum of every past session), shown as a read-only total.
  int _lifetimeDown = 0;
  int _lifetimeUp = 0;
  // "Remember admin": reflects the real OS state, and is only shown where the OS
  // supports it. `_rememberBusy` guards the in-flight admin prompt; `_rememberMsg`
  // shows why a change didn't take (declined prompt).
  bool _rememberAdmin = false;
  bool _rememberSupported = false;
  bool _rememberBusy = false;
  String? _rememberMsg;
  bool _loading = true;
  Timer? _debounce;

  /// A load or save that didn't reach the backend. Without this the sheet
  /// silently rendered defaults and every later change failed quietly, so the
  /// values on screen looked saved when nothing had been.
  bool _saveFailed = false;

  /// "Fix browser proxy" in-flight + last result message.
  bool _fixingProxy = false;
  String? _fixMsg;

  @override
  void initState() {
    super.initState();
    _load();
    _loadNetwork();
    _loadBoss();
  }

  Future<void> _loadNetwork() async {
    try {
      final n = await widget.api.currentNetwork();
      if (mounted) setState(() => _here = n);
    } catch (_) {}
  }

  Future<void> _loadBoss() async {
    try {
      final b = await widget.api.bossShortcutStatus();
      if (mounted) setState(() => _boss = b);
    } catch (_) {}
  }

  Future<void> _saveUntrusted(bool v) async {
    setState(() => _untrusted = v);
    try {
      await widget.api.setSettings(untrustedAutoconnect: v);
    } catch (_) {
      if (mounted) setState(() => _saveFailed = true);
    }
  }

  Future<void> _saveTrusted(List<Map<String, String>> list) async {
    setState(() => _trusted = list);
    try {
      await widget.api.setSettings(trustedNetworks: list);
    } catch (_) {
      if (mounted) setState(() => _saveFailed = true);
    }
  }

  bool get _hereTrusted =>
      _here != null && _trusted.any((t) => t['id'] == _here!['id']);

  void _trustHere() {
    final h = _here;
    if (h == null || _hereTrusted) return;
    _saveTrusted([
      ..._trusted,
      {'id': '${h['id']}', 'label': '${h['label']}'},
    ]);
  }

  void _untrust(String id) =>
      _saveTrusted(_trusted.where((t) => t['id'] != id).toList());

  Future<void> _toggleBoss() async {
    final installed = _boss?['installed'] == true;
    setState(() {
      _bossBusy = true;
      _bossMsg = null;
    });
    Map<String, dynamic> r;
    try {
      r = installed
          ? await widget.api.removeBossShortcut()
          : await widget.api.installBossShortcut();
    } catch (_) {
      r = {'success': false, 'error': "the app isn't responding"};
    }
    await _loadBoss();
    if (!mounted) return;
    setState(() {
      _bossBusy = false;
      _bossMsg = r['success'] == true
          ? (installed
                ? 'shortcut removed.'
                : 'done — press Ctrl+Alt+Esc anywhere to hide the app.')
          : "couldn't set it up (${r['error']}). you can add it by hand instead.";
    });
  }

  Future<void> _fixProxy() async {
    setState(() {
      _fixingProxy = true;
      _fixMsg = null;
    });
    String msg;
    try {
      final ok = await widget.api.resetProxy();
      msg = ok
          ? 'network proxy reset. if a page was stuck, reload your browser.'
          : 'disconnect first, then run fix again.';
    } catch (_) {
      msg =
          "couldn't do that — the app isn't responding. try restarting dreamnail.";
    }
    if (!mounted) return;
    setState(() {
      _fixingProxy = false;
      _fixMsg = msg;
    });
  }

  Future<void> _load() async {
    try {
      final s = await widget.api.getSettings();
      if (!mounted) return;
      setState(() {
        _timeout = (s['timeout_minutes'] as int?) ?? 5;
        _autoReconnect = (s['auto_reconnect_enabled'] as bool?) ?? true;
        _autoConnect = (s['auto_connect'] as bool?) ?? false;
        _untrusted = (s['untrusted_autoconnect'] as bool?) ?? false;
        _trusted = ((s['trusted_networks'] as List?) ?? const [])
            .whereType<Map>()
            .map((m) => {
                  'id': '${m['id']}',
                  'label': '${m['label']}',
                })
            .toList();
        _planExpiry = (s['plan_expiry'] as String?) ?? '';
        _dataLimitGb = (s['data_limit_gb'] as num?)?.toDouble() ?? 0;
        _stealth = (s['routing_mode'] as String?) == 'tun';
        _adblock = (s['adblock'] as bool?) ?? false;
        _force443 = (s['force_443'] as bool?) ?? false;
        _weaknet = (s['weaknet'] as bool?) ?? false;
        _realityDecoy = (s['reality_decoy'] as String?) ?? '';
        _realityDecoys = ((s['reality_decoys'] as List?) ?? const [])
            .whereType<Map>()
            .map((m) => {
                  'label': (m['label'] as String?) ?? '',
                  'sni': (m['sni'] as String?) ?? '',
                })
            .where((m) => (m['sni'] ?? '').isNotEmpty)
            .toList();
        _errorReports = (s['error_reports'] as bool?) ?? true;
        _rememberAdmin = (s['remember_admin'] as bool?) ?? false;
        _rememberSupported = (s['remember_admin_supported'] as bool?) ?? false;
        _lifetimeDown = (s['lifetime_down_bytes'] as num?)?.toInt() ?? 0;
        _lifetimeUp = (s['lifetime_up_bytes'] as num?)?.toInt() ?? 0;
        _loading = false;
      });
    } catch (_) {
      if (mounted) {
        setState(() {
          _loading = false;
          _saveFailed = true;
        });
      }
    }
  }

  void _saveTimeout(int v) {
    widget.onTimeoutChanged(v);
    _debounce?.cancel();
    _debounce = Timer(const Duration(milliseconds: 300), () async {
      try {
        await widget.api.setSettings(timeoutMinutes: v);
      } catch (_) {
        if (mounted) setState(() => _saveFailed = true);
      }
    });
  }

  Future<void> _saveAutoReconnect(bool v) async {
    setState(() => _autoReconnect = v);
    try {
      await widget.api.setSettings(autoReconnectEnabled: v);
    } catch (_) {
      if (mounted) setState(() => _saveFailed = true);
    }
  }

  Future<void> _saveAutoConnect(bool v) async {
    setState(() => _autoConnect = v);
    try {
      await widget.api.setSettings(autoConnect: v);
    } catch (_) {
      if (mounted) setState(() => _saveFailed = true);
    }
  }

  /// Pick (or clear) the local plan-expiry reminder date. A date-only picker;
  /// stored as ISO yyyy-mm-dd. Clearing hides the reminder banner.
  Future<void> _pickExpiry() async {
    final now = DateTime.now();
    final current = DateTime.tryParse(_planExpiry);
    final picked = await showDatePicker(
      context: context,
      initialDate: current ?? now.add(const Duration(days: 30)),
      firstDate: DateTime(now.year - 1),
      lastDate: DateTime(now.year + 6),
      helpText: 'When does your plan end?',
      builder: (ctx, child) => Theme(
        data: Theme.of(ctx).copyWith(
          colorScheme: const ColorScheme.dark(
            primary: PxColors.accent,
            surface: PxColors.surface,
            onSurface: PxColors.textPrimary,
          ),
        ),
        child: child!,
      ),
    );
    if (picked == null) return;
    final iso =
        '${picked.year.toString().padLeft(4, '0')}-${picked.month.toString().padLeft(2, '0')}-${picked.day.toString().padLeft(2, '0')}';
    setState(() => _planExpiry = iso);
    try {
      await widget.api.setSettings(planExpiry: iso);
    } catch (_) {
      if (mounted) setState(() => _saveFailed = true);
    }
  }

  Future<void> _clearExpiry() async {
    setState(() => _planExpiry = '');
    try {
      await widget.api.setSettings(planExpiry: '');
    } catch (_) {
      if (mounted) setState(() => _saveFailed = true);
    }
  }

  /// Prompt for the plan data cap in GB (blank / 0 = no cap), then save it.
  Future<void> _editDataLimit() async {
    final controller = TextEditingController(
      text: _dataLimitGb > 0 ? _trimGb(_dataLimitGb) : '',
    );
    final result = await showDialog<double>(
      context: context,
      builder: (ctx) => AlertDialog(
        backgroundColor: PxColors.surface,
        title: const Text(
          'Data limit',
          style: TextStyle(fontFamily: PxFont.body, color: PxColors.textPrimary),
        ),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const Text(
              'Enter your plan\'s data cap in GB (leave blank for no cap).',
              style: TextStyle(
                fontFamily: PxFont.body,
                fontSize: 12,
                color: PxColors.mutedDeep,
              ),
            ),
            const SizedBox(height: AppSpace.md),
            TextField(
              controller: controller,
              autofocus: true,
              keyboardType: const TextInputType.numberWithOptions(decimal: true),
              style: const TextStyle(
                fontFamily: PxFont.body,
                color: PxColors.textPrimary,
              ),
              decoration: const InputDecoration(
                suffixText: 'GB',
                suffixStyle: TextStyle(
                  fontFamily: PxFont.body,
                  color: PxColors.mutedDeep,
                ),
              ),
              onSubmitted: (v) =>
                  Navigator.pop(ctx, double.tryParse(v.trim()) ?? 0),
            ),
          ],
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx),
            child: const Text('Cancel',
                style: TextStyle(fontFamily: PxFont.body, color: PxColors.textMuted)),
          ),
          TextButton(
            onPressed: () => Navigator.pop(
                ctx, double.tryParse(controller.text.trim()) ?? 0),
            child: const Text('Save',
                style: TextStyle(fontFamily: PxFont.body, color: PxColors.accent)),
          ),
        ],
      ),
    );
    if (result == null) return; // cancelled
    final gb = result < 0 ? 0.0 : result;
    setState(() => _dataLimitGb = gb);
    try {
      await widget.api.setSettings(dataLimitGb: gb);
    } catch (_) {
      if (mounted) setState(() => _saveFailed = true);
    }
  }

  Future<void> _clearDataLimit() async {
    setState(() => _dataLimitGb = 0);
    try {
      await widget.api.setSettings(dataLimitGb: 0);
    } catch (_) {
      if (mounted) setState(() => _saveFailed = true);
    }
  }

  Future<void> _resetUsage() async {
    try {
      await widget.api.resetUsage();
    } catch (_) {
      if (mounted) setState(() => _saveFailed = true);
    }
  }

  /// "50" not "50.0", "1.5" kept — tidy GB display.
  static String _trimGb(double gb) {
    final s = gb.toStringAsFixed(2);
    return s.replaceFirst(RegExp(r'\.?0+$'), '');
  }

  Future<void> _saveStealth(bool v) async {
    setState(() => _stealth = v);
    try {
      await widget.api.setSettings(routingMode: v ? 'tun' : 'proxy');
    } catch (_) {
      if (mounted) setState(() => _saveFailed = true);
    }
  }

  Future<void> _saveAdblock(bool v) async {
    setState(() => _adblock = v);
    try {
      await widget.api.setSettings(adblock: v);
    } catch (_) {
      if (mounted) setState(() => _saveFailed = true);
    }
  }

  Future<void> _saveForce443(bool v) async {
    setState(() => _force443 = v);
    try {
      await widget.api.setSettings(force443: v);
    } catch (_) {
      if (mounted) setState(() => _saveFailed = true);
    }
  }

  Future<void> _saveWeaknet(bool v) async {
    setState(() => _weaknet = v);
    try {
      await widget.api.setSettings(weaknet: v);
    } catch (_) {
      if (mounted) setState(() => _saveFailed = true);
    }
  }

  Future<void> _saveRealityDecoy(String sni) async {
    setState(() => _realityDecoy = sni);
    try {
      await widget.api.setSettings(realityDecoy: sni);
    } catch (_) {
      if (mounted) setState(() => _saveFailed = true);
    }
  }

  Future<void> _saveErrorReports(bool v) async {
    setState(() => _errorReports = v);
    try {
      await widget.api.setSettings(errorReports: v);
    } catch (_) {
      if (mounted) setState(() => _saveFailed = true);
    }
  }

  /// "Report a problem" — a small CRT-styled dialog that collects a note and
  /// sends it (with a diagnostics bundle) to support via the backend.
  Future<void> _reportProblem() async {
    final controller = TextEditingController();
    bool sending = false;
    String? error;
    await showDialog<void>(
      context: context,
      barrierColor: Colors.black54,
      builder: (ctx) => StatefulBuilder(
        builder: (ctx, setLocal) => Dialog(
          backgroundColor: PxColors.surface,
          shape: RoundedRectangleBorder(
            side: const BorderSide(color: PxColors.frame),
            borderRadius: BorderRadius.circular(8),
          ),
          child: Padding(
            padding: const EdgeInsets.all(18),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                _heading('REPORT A PROBLEM'),
                const SizedBox(height: 8),
                const Text(
                  'what went wrong? a quick diagnostic is attached automatically '
                  '(no browsing, no passwords).',
                  style: _note,
                ),
                const SizedBox(height: 12),
                TextField(
                  controller: controller,
                  maxLines: 5,
                  maxLength: 1000,
                  autofocus: true,
                  style: const TextStyle(
                    fontFamily: PxFont.body,
                    fontSize: 13,
                    color: PxColors.textPrimary,
                  ),
                  decoration: InputDecoration(
                    hintText: 'e.g. it won\'t connect on my home wifi…',
                    hintStyle: const TextStyle(color: PxColors.mutedDeep, fontSize: 13),
                    filled: true,
                    fillColor: PxColors.field,
                    enabledBorder: OutlineInputBorder(
                      borderSide: const BorderSide(color: PxColors.frameInset),
                      borderRadius: BorderRadius.circular(6),
                    ),
                    focusedBorder: OutlineInputBorder(
                      borderSide: const BorderSide(color: PxColors.accent),
                      borderRadius: BorderRadius.circular(6),
                    ),
                  ),
                ),
                if (error != null) ...[
                  const SizedBox(height: 6),
                  Text(error!, style: const TextStyle(color: PxColors.danger, fontSize: 12)),
                ],
                const SizedBox(height: 10),
                Row(
                  mainAxisAlignment: MainAxisAlignment.end,
                  children: [
                    _link('cancel', () => Navigator.of(ctx).pop()),
                    const SizedBox(width: 18),
                    _link(
                      sending ? 'sending…' : 'send',
                      () async {
                        if (sending) return;
                        final msg = controller.text.trim();
                        if (msg.isEmpty) {
                          setLocal(() => error = 'please type a short note first.');
                          return;
                        }
                        setLocal(() {
                          sending = true;
                          error = null;
                        });
                        Map<String, dynamic> r;
                        try {
                          r = await widget.api.reportProblem(msg);
                        } catch (_) {
                          r = {'success': false, 'error': 'could not send right now.'};
                        }
                        if (r['success'] == true) {
                          if (ctx.mounted) Navigator.of(ctx).pop();
                          if (mounted) {
                            ScaffoldMessenger.of(context).showSnackBar(
                              const SnackBar(content: Text('Report sent — thank you.')),
                            );
                          }
                        } else {
                          setLocal(() {
                            sending = false;
                            error = (r['error'] as String?) ?? 'could not send right now.';
                          });
                        }
                      },
                      color: PxColors.accent,
                    ),
                  ],
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }

  /// Toggle passwordless stealth. This pops a REAL admin prompt, so we don't
  /// assume it worked: we await the result and set the switch to the state the OS
  /// actually ended in (declining leaves it where it was), with a short note.
  Future<void> _saveRememberAdmin(bool v) async {
    if (_rememberBusy) return;
    setState(() {
      _rememberBusy = true;
      _rememberMsg = null;
    });
    try {
      final r = await widget.api.setRememberAdmin(v);
      if (!mounted) return;
      final enabled = (r['enabled'] as bool?) ?? _rememberAdmin;
      setState(() {
        _rememberAdmin = enabled;
        _rememberBusy = false;
        if (r['success'] != true) {
          _rememberMsg = (r['error'] as String?) ??
              'that needs a one-time admin approval.';
        } else {
          _rememberMsg = enabled
              ? "done — stealth won't ask for admin again on this computer."
              : 'turned off — stealth will ask for admin again.';
        }
      });
    } catch (_) {
      if (mounted) {
        setState(() {
          _rememberBusy = false;
          _rememberMsg = "couldn't reach the app — try restarting dreamnail.";
        });
      }
    }
  }

  @override
  void dispose() {
    _debounce?.cancel();
    super.dispose();
  }

  // A "> label" heading line.
  Widget _heading(String text) => Text(
    text,
    style: TextStyle(
      fontFamily: PxFont.hero,
      fontWeight: FontWeight.w700,
      fontSize: 15,
      letterSpacing: 1.5,
      color: PxColors.accent,
      shadows: glowText(PxColors.accent, alpha: 0.3),
    ),
  );

  Widget _rule() => Container(height: 1, color: PxColors.frameInset);

  // A small "> action" text link.
  Widget _link(String label, VoidCallback onTap, {Color color = PxColors.textMuted}) =>
      GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: onTap,
        child: Text(
          '> $label',
          style: TextStyle(fontFamily: PxFont.body, fontSize: 12, color: color),
        ),
      );

  static const _note = TextStyle(
    fontFamily: PxFont.body,
    fontSize: 12,
    height: 1.4,
    color: PxColors.mutedDeep,
  );

  /// The trusted-networks block shown under "auto-connect on untrusted networks".
  Widget _networksBlock() {
    final here = _here;
    return Padding(
      padding: const EdgeInsets.only(left: 22, top: AppSpace.sm),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          if (here == null)
            const Text('> this network: not connected to one right now', style: _note)
          else
            Row(
              children: [
                Expanded(
                  child: Text(
                    '> this network: ${here['label']}'
                    '${_hereTrusted ? '  (trusted)' : ''}',
                    overflow: TextOverflow.ellipsis,
                    style: const TextStyle(
                      fontFamily: PxFont.body,
                      fontSize: 13,
                      color: PxColors.textPrimary,
                    ),
                  ),
                ),
                const SizedBox(width: AppSpace.md),
                _hereTrusted
                    ? _link('untrust', () => _untrust('${here['id']}'))
                    : _link('trust it', _trustHere, color: PxColors.accent),
              ],
            ),
          const SizedBox(height: AppSpace.sm),
          if (_trusted.isEmpty)
            const Text(
              'no trusted networks yet — mark home or work as trusted so it '
              'only connects by itself everywhere else.',
              style: _note,
            )
          else ...[
            const Text('trusted (never auto-connects on these):', style: _note),
            const SizedBox(height: 4),
            for (final t in _trusted)
              Padding(
                padding: const EdgeInsets.only(bottom: 4),
                child: Row(
                  children: [
                    Expanded(
                      child: Text(
                        '  · ${t['label']}',
                        overflow: TextOverflow.ellipsis,
                        style: const TextStyle(
                          fontFamily: PxFont.body,
                          fontSize: 12,
                          color: PxColors.textMuted,
                        ),
                      ),
                    ),
                    _link('remove', () => _untrust('${t['id']}')),
                  ],
                ),
              ),
          ],
        ],
      ),
    );
  }

  /// The quick-hide shortcut row — only on desktops (Linux Wayland) where the
  /// app can't register Ctrl+Alt+Esc itself.
  Widget _bossBlock() {
    final b = _boss!;
    final installed = b['installed'] == true;
    final supported = b['can_auto'] == true;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          crossAxisAlignment: CrossAxisAlignment.center,
          children: [
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  const Text(
                    '> quick-hide shortcut (ctrl+alt+esc)',
                    style: TextStyle(
                      fontFamily: PxFont.body,
                      fontSize: 14,
                      color: PxColors.textPrimary,
                    ),
                  ),
                  const SizedBox(height: 3),
                  Text(
                    installed
                        ? 'on — ctrl+alt+esc snaps back to the weather and hides '
                            'the window, from any app.'
                        : 'your desktop doesn\'t let apps grab keys, so add '
                            'ctrl+alt+esc to its own shortcuts: it then snaps back to '
                            'the weather and hides the window, from any app.',
                    style: _note,
                  ),
                ],
              ),
            ),
            if (supported) ...[
              const SizedBox(width: AppSpace.md),
              SizedBox(
                width: 110,
                child: PixelButton(
                  label: installed ? 'Remove' : 'Set up',
                  color: PxColors.accent,
                  border: PxColors.accentGlow,
                  onPressed: _bossBusy ? null : _toggleBoss,
                  child: _bossBusy
                      ? const SizedBox(
                          width: 15,
                          height: 15,
                          child: CircularProgressIndicator(
                            strokeWidth: 2,
                            color: PxColors.accentGlow,
                          ),
                        )
                      : null,
                ),
              ),
            ],
          ],
        ),
        if (!supported || (_bossMsg?.startsWith("couldn't") ?? false)) ...[
          const SizedBox(height: AppSpace.sm),
          const Text(
            'to add it by hand, make a custom shortcut for ctrl+alt+esc in your '
            'desktop\'s keyboard settings that runs:',
            style: _note,
          ),
          const SizedBox(height: 4),
          SelectableText(
            '${b['command']}',
            style: const TextStyle(
              fontFamily: PxFont.body,
              fontSize: 12,
              color: PxColors.textMuted,
            ),
          ),
        ],
        if (_bossMsg != null)
          Padding(
            padding: const EdgeInsets.only(top: AppSpace.sm),
            child: Text(
              '> $_bossMsg',
              style: const TextStyle(
                fontFamily: PxFont.body,
                fontSize: 12,
                height: 1.4,
                color: PxColors.textMuted,
              ),
            ),
          ),
      ],
    );
  }

  // A terminal "[x] title" toggle with a description line.
  Widget _toggle(
    String title,
    String desc,
    bool value,
    ValueChanged<bool> onChanged,
  ) => GestureDetector(
    behavior: HitTestBehavior.opaque,
    onTap: () => onChanged(!value),
    child: Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Padding(
          padding: const EdgeInsets.only(top: 1),
          child: Text(
            value ? '[x]' : '[ ]',
            style: TextStyle(
              fontFamily: PxFont.body,
              fontWeight: FontWeight.w700,
              fontSize: 15,
              color: value ? PxColors.success : PxColors.mutedDeep,
              shadows: value ? glowText(PxColors.success, alpha: 0.3) : null,
            ),
          ),
        ),
        const SizedBox(width: 10),
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                '> $title',
                style: const TextStyle(
                  fontFamily: PxFont.body,
                  fontSize: 14,
                  color: PxColors.textPrimary,
                ),
              ),
              const SizedBox(height: 3),
              Text(
                desc,
                style: const TextStyle(
                  fontFamily: PxFont.body,
                  fontSize: 12,
                  height: 1.4,
                  color: PxColors.mutedDeep,
                ),
              ),
            ],
          ),
        ),
      ],
    ),
  );

  /// Read-only all-time data usage (every past session added up). The live
  /// per-session counter lives on the VPN screen; this is the running total.
  Widget _usageReadout() {
    final total = _lifetimeDown + _lifetimeUp;
    return Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        const Padding(
          padding: EdgeInsets.only(top: 1),
          child: Text(
            '[=]',
            style: TextStyle(
              fontFamily: PxFont.body,
              fontWeight: FontWeight.w700,
              fontSize: 15,
              color: PxColors.accent,
            ),
          ),
        ),
        const SizedBox(width: 10),
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              const Text(
                '> data used (all time)',
                style: TextStyle(
                  fontFamily: PxFont.body,
                  fontSize: 14,
                  color: PxColors.textPrimary,
                ),
              ),
              const SizedBox(height: 3),
              Text(
                total == 0
                    ? 'nothing yet — totals start after your first session.'
                    : '↓ ${formatBytes(_lifetimeDown)}   '
                        '↑ ${formatBytes(_lifetimeUp)}   '
                        '· ${formatBytes(total)} total',
                style: const TextStyle(
                  fontFamily: PxFont.body,
                  fontSize: 12,
                  height: 1.4,
                  color: PxColors.mutedDeep,
                ),
              ),
            ],
          ),
        ),
      ],
    );
  }

  @override
  Widget build(BuildContext context) {
    return pixelTypography(
      context,
      child: Container(
        decoration: BoxDecoration(
          color: PxColors.surface,
          border: const Border(
            top: BorderSide(color: PxColors.frame, width: 1),
          ),
          boxShadow: crtGlow(PxColors.frame, alpha: 0.12, blur: 16),
        ),
        padding: const EdgeInsets.fromLTRB(
          AppSpace.xl,
          AppSpace.md,
          AppSpace.xl,
          AppSpace.xl,
        ),
        child: SafeArea(
          top: false,
          child: ConstrainedBox(
            // Cap the sheet so it never fills the whole window — that leaves the
            // dim area above it tappable to dismiss, and the body scrolls instead
            // of overflowing when there are lots of options.
            constraints: BoxConstraints(
              maxHeight: MediaQuery.of(context).size.height * 0.85,
            ),
            child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Center(
                child: Container(
                  width: 40,
                  height: 3,
                  margin: const EdgeInsets.only(bottom: AppSpace.lg),
                  color: PxColors.frameInset,
                ),
              ),
              // Title + an always-visible close button (the sheet used to rely
              // only on tap-outside / drag-down, which left no obvious exit).
              Row(
                mainAxisAlignment: MainAxisAlignment.spaceBetween,
                crossAxisAlignment: CrossAxisAlignment.center,
                children: [
                  _heading('> SETTINGS'),
                  GestureDetector(
                    behavior: HitTestBehavior.opaque,
                    onTap: () => Navigator.of(context).maybePop(),
                    child: const Padding(
                      padding: EdgeInsets.all(4),
                      child: Text(
                        '[ ✕ ]',
                        style: TextStyle(
                          fontFamily: PxFont.body,
                          fontWeight: FontWeight.w700,
                          fontSize: 15,
                          color: PxColors.textMuted,
                        ),
                      ),
                    ),
                  ),
                ],
              ),
              const SizedBox(height: AppSpace.sm),
              _rule(),
              const SizedBox(height: AppSpace.md),
              Flexible(
                child: SingleChildScrollView(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
              if (_saveFailed)
                Padding(
                  padding: const EdgeInsets.only(bottom: AppSpace.md),
                  child: Text(
                    "! not saved — the app isn't responding. try again, or restart dreamnail.",
                    style: const TextStyle(
                      fontFamily: PxFont.body,
                      fontSize: 12,
                      letterSpacing: 0.3,
                      color: PxColors.warning,
                    ),
                  ),
                ),
              if (_loading)
                const Padding(
                  padding: EdgeInsets.symmetric(vertical: AppSpace.xl),
                  child: Center(
                    child: CircularProgressIndicator(color: PxColors.accent),
                  ),
                )
              else ...[
                Row(
                  mainAxisAlignment: MainAxisAlignment.spaceBetween,
                  children: [
                    const Text(
                      '> auto-lock',
                      style: TextStyle(
                        fontFamily: PxFont.body,
                        fontSize: 14,
                        color: PxColors.textPrimary,
                      ),
                    ),
                    Text(
                      '${_timeout}min',
                      style: TextStyle(
                        fontFamily: PxFont.body,
                        fontSize: 14,
                        color: PxColors.accent,
                        shadows: glowText(PxColors.accent, alpha: 0.3),
                      ),
                    ),
                  ],
                ),
                SliderTheme(
                  data: SliderThemeData(
                    trackHeight: 2,
                    activeTrackColor: PxColors.accent,
                    inactiveTrackColor: PxColors.frameInset,
                    thumbColor: PxColors.accentGlow,
                    overlayColor: PxColors.accent.withValues(alpha: 0.12),
                    valueIndicatorColor: PxColors.surface,
                    valueIndicatorTextStyle: const TextStyle(
                      fontFamily: PxFont.body,
                      color: PxColors.textPrimary,
                    ),
                  ),
                  child: Slider(
                    value: _timeout.toDouble(),
                    min: 1,
                    max: 30,
                    divisions: 29,
                    label: '${_timeout}min',
                    onChanged: (v) => setState(() => _timeout = v.round()),
                    onChangeEnd: (v) => _saveTimeout(v.round()),
                  ),
                ),
                const SizedBox(height: AppSpace.md),
                _toggle(
                  'auto-reconnect',
                  'keep bringing the VPN back by itself after a drop, a laptop '
                      'wake, or a Wi-Fi switch — it retries until it reconnects '
                      'or you disconnect.',
                  _autoReconnect,
                  _saveAutoReconnect,
                ),
                const SizedBox(height: AppSpace.lg),
                _usageReadout(),
                const SizedBox(height: AppSpace.lg),
                _toggle(
                  'auto-connect',
                  'connect automatically as soon as you unlock — needs "remember '
                      'session" ticked so your login is saved.',
                  _autoConnect,
                  _saveAutoConnect,
                ),
                const SizedBox(height: AppSpace.lg),
                _toggle(
                  'auto-connect on untrusted networks',
                  'turns the VPN on by itself whenever you join a network you '
                      'haven\'t marked as trusted — café, airport, hotel wi-fi — '
                      'even with the app in the tray. needs "remember session" so '
                      'your login is saved. if you disconnect by hand, it leaves '
                      'that network alone until you move to another.',
                  _untrusted,
                  _saveUntrusted,
                ),
                if (_untrusted) _networksBlock(),
                const SizedBox(height: AppSpace.lg),
                _toggle(
                  'stealth routing',
                  'routes every app through the tunnel with nothing set in your '
                      'system proxy — so apps and games that refuse a proxy still '
                      'work. asks for your admin password once on connect; declines '
                      'fall back to standard routing. takes effect next connect.',
                  _stealth,
                  _saveStealth,
                ),
                // Passwordless stealth — only where the OS supports it. Nested
                // under stealth routing since it only matters when stealth is used.
                if (_rememberSupported) ...[
                  const SizedBox(height: AppSpace.md),
                  Padding(
                    padding: const EdgeInsets.only(left: 22),
                    child: _toggle(
                      _rememberBusy ? 'remember admin — waiting…' : 'remember admin',
                      "ask for the admin password ONCE, then never again for "
                          "stealth on this computer. installs a small, visible "
                          "system rule (it can be seen and removed); turning this "
                          "off removes it.",
                      _rememberAdmin,
                      _rememberBusy ? (_) {} : _saveRememberAdmin,
                    ),
                  ),
                  if (_rememberMsg != null)
                    Padding(
                      padding: const EdgeInsets.only(left: 44, top: 4),
                      child: Text(
                        _rememberMsg!,
                        style: const TextStyle(
                          fontFamily: PxFont.body,
                          fontSize: 11.5,
                          height: 1.4,
                          color: PxColors.textMuted,
                        ),
                      ),
                    ),
                ],
                const SizedBox(height: AppSpace.lg),
                _toggle(
                  'ad blocker',
                  'blocks ads and trackers (about 169,000 known ad sites) '
                      'for every app while connected, so pages load lighter. '
                      'can\'t block youtube ads. if a site misbehaves, turn it '
                      'off. takes effect on your next connect.',
                  _adblock,
                  _saveAdblock,
                ),
                const SizedBox(height: AppSpace.lg),
                _toggle(
                  'force 443',
                  'skip the normal port and connect only over 443 (the port '
                      'normal websites use). helps on networks that block the '
                      'usual port. takes effect on your next connect.',
                  _force443,
                  _saveForce443,
                ),
                const SizedBox(height: AppSpace.lg),
                _toggle(
                  'weak-network mode',
                  'for slow or flaky wi-fi (like at school). sends smaller '
                      'packets so pages stop hanging, and spreads your traffic '
                      'over more connections so one dropout doesn\'t freeze '
                      'everything. leave it off at home. takes effect on your '
                      'next connect.',
                  _weaknet,
                  _saveWeaknet,
                ),
                if (_realityDecoys.isNotEmpty) ...[
                  const SizedBox(height: AppSpace.lg),
                  Text(
                    'disguise site',
                    style: const TextStyle(
                      fontSize: 13.5,
                      fontWeight: FontWeight.w600,
                      color: PxColors.textPrimary,
                    ),
                  ),
                  const SizedBox(height: 4),
                  Text(
                    'on the strongest door, the app can look like a visit to one '
                    'of these real sites. pick the one your network allows. only '
                    'used if the other doors are blocked. next connect.',
                    style: const TextStyle(
                      fontSize: 11.5,
                      height: 1.4,
                      color: PxColors.textMuted,
                    ),
                  ),
                  const SizedBox(height: 8),
                  Container(
                    padding: const EdgeInsets.symmetric(horizontal: 12),
                    decoration: BoxDecoration(
                      color: PxColors.surface,
                      borderRadius: BorderRadius.circular(8),
                      border: Border.all(color: PxColors.frameInset),
                    ),
                    child: DropdownButtonHideUnderline(
                      child: DropdownButton<String>(
                        isExpanded: true,
                        dropdownColor: PxColors.surface,
                        value: _realityDecoys.any((m) => m['sni'] == _realityDecoy)
                            ? _realityDecoy
                            : _realityDecoys.first['sni'],
                        style: const TextStyle(
                          fontSize: 13,
                          color: PxColors.textPrimary,
                        ),
                        items: [
                          for (final m in _realityDecoys)
                            DropdownMenuItem<String>(
                              value: m['sni'],
                              child: Text('${m['label']}  (${m['sni']})'),
                            ),
                        ],
                        onChanged: (v) {
                          if (v != null) _saveRealityDecoy(v);
                        },
                      ),
                    ),
                  ),
                ],
                const SizedBox(height: AppSpace.lg),
                _toggle(
                  'anonymous problem reports',
                  'if a connection fails, send an anonymous note (app version, '
                      'your operating system, and the type of error — no username, '
                      'no password, no IP address) so problems can be fixed faster.',
                  _errorReports,
                  _saveErrorReports,
                ),
                const SizedBox(height: AppSpace.lg),
                Row(
                  crossAxisAlignment: CrossAxisAlignment.center,
                  children: [
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          const Text(
                            '> report a problem',
                            style: TextStyle(
                              fontFamily: PxFont.body,
                              fontSize: 14,
                              color: PxColors.textPrimary,
                            ),
                          ),
                          const SizedBox(height: 3),
                          const Text(
                            'tell support what went wrong. sends your note plus a '
                            'quick diagnostic (no browsing, no passwords) so it can '
                            'be looked into. connect once first so it can be sent.',
                            style: _note,
                          ),
                        ],
                      ),
                    ),
                    const SizedBox(width: AppSpace.md),
                    _link('send…', _reportProblem, color: PxColors.accent),
                  ],
                ),
                const SizedBox(height: AppSpace.md),
                Row(
                  crossAxisAlignment: CrossAxisAlignment.center,
                  children: [
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          const Text(
                            '> connection help',
                            style: TextStyle(
                              fontFamily: PxFont.body,
                              fontSize: 14,
                              color: PxColors.textPrimary,
                            ),
                          ),
                          const SizedBox(height: 3),
                          const Text(
                            'a plain-english read on what your connection is doing '
                            "right now — and the one thing to try if something's off.",
                            style: _note,
                          ),
                        ],
                      ),
                    ),
                    const SizedBox(width: AppSpace.md),
                    _link(
                      'explain…',
                      () => ConnectionCoach.show(
                          context, context.read<ConnectionController>()),
                      color: PxColors.accent,
                    ),
                  ],
                ),
                const SizedBox(height: AppSpace.md),
                const Text(
                  '> weather skin',
                  style: TextStyle(
                    fontFamily: PxFont.body,
                    fontSize: 14,
                    color: PxColors.textPrimary,
                  ),
                ),
                const SizedBox(height: 3),
                const Text(
                  'recolour the sky. purely cosmetic — the weather itself is '
                  'unchanged.',
                  style: _note,
                ),
                const SizedBox(height: AppSpace.sm),
                Wrap(
                  spacing: 8,
                  runSpacing: 8,
                  children: [
                    for (final s in kSkins)
                      GestureDetector(
                        onTap: () {
                          setSkin(s);
                          setState(() {});
                        },
                        child: Container(
                          padding: const EdgeInsets.symmetric(
                              horizontal: 10, vertical: 7),
                          decoration: BoxDecoration(
                            color: activeSkin.value.id == s.id
                                ? PxColors.accent.withValues(alpha: 0.18)
                                : Colors.white.withValues(alpha: 0.04),
                            borderRadius: BorderRadius.circular(8),
                            border: Border.all(
                              color: activeSkin.value.id == s.id
                                  ? PxColors.accent
                                  : PxColors.textMuted.withValues(alpha: 0.25),
                            ),
                          ),
                          child: Row(
                            mainAxisSize: MainAxisSize.min,
                            children: [
                              Container(
                                width: 12,
                                height: 12,
                                decoration: BoxDecoration(
                                  shape: BoxShape.circle,
                                  color: s.tint,
                                ),
                              ),
                              const SizedBox(width: 7),
                              Text(
                                s.label,
                                style: TextStyle(
                                  fontFamily: PxFont.body,
                                  fontSize: 12.5,
                                  color: activeSkin.value.id == s.id
                                      ? PxColors.textPrimary
                                      : PxColors.textMuted,
                                ),
                              ),
                            ],
                          ),
                        ),
                      ),
                  ],
                ),
                const SizedBox(height: AppSpace.lg),
                _rule(),
                const SizedBox(height: AppSpace.md),
                // Local plan-expiry reminder — set from the welcome message.
                Row(
                  crossAxisAlignment: CrossAxisAlignment.center,
                  children: [
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          const Text(
                            '> plan expiry reminder',
                            style: TextStyle(
                              fontFamily: PxFont.body,
                              fontSize: 14,
                              color: PxColors.textPrimary,
                            ),
                          ),
                          const SizedBox(height: 3),
                          Text(
                            _planExpiry.isEmpty
                                ? 'syncs from your plan when you connect. you can '
                                    'also set a date here for a renewal reminder.'
                                : 'reminding you as $_planExpiry approaches '
                                    '(syncs from your plan on connect).',
                            style: const TextStyle(
                              fontFamily: PxFont.body,
                              fontSize: 12,
                              height: 1.4,
                              color: PxColors.mutedDeep,
                            ),
                          ),
                        ],
                      ),
                    ),
                    const SizedBox(width: AppSpace.md),
                    SizedBox(
                      width: 110,
                      child: PixelButton(
                        label: _planExpiry.isEmpty ? 'Set date' : 'Change',
                        color: PxColors.accent,
                        border: PxColors.accentGlow,
                        onPressed: _pickExpiry,
                      ),
                    ),
                  ],
                ),
                if (_planExpiry.isNotEmpty)
                  Padding(
                    padding: const EdgeInsets.only(top: AppSpace.sm),
                    child: GestureDetector(
                      onTap: _clearExpiry,
                      child: const Text(
                        '> clear reminder',
                        style: TextStyle(
                          fontFamily: PxFont.body,
                          fontSize: 12,
                          color: PxColors.textMuted,
                        ),
                      ),
                    ),
                  ),
                const SizedBox(height: AppSpace.lg),
                _rule(),
                const SizedBox(height: AppSpace.md),
                // Plan data cap — mirrors what the operator set; the sticky-note
                // on the VPN screen tracks usage against it.
                Row(
                  crossAxisAlignment: CrossAxisAlignment.center,
                  children: [
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          const Text(
                            '> data limit',
                            style: TextStyle(
                              fontFamily: PxFont.body,
                              fontSize: 14,
                              color: PxColors.textPrimary,
                            ),
                          ),
                          const SizedBox(height: 3),
                          Text(
                            _dataLimitGb <= 0
                                ? 'syncs from your plan when you connect. you can '
                                    'also set it here to see a usage sticky-note.'
                                : 'tracking usage against ${_trimGb(_dataLimitGb)} GB '
                                    '(syncs from your plan on connect).',
                            style: const TextStyle(
                              fontFamily: PxFont.body,
                              fontSize: 12,
                              height: 1.4,
                              color: PxColors.mutedDeep,
                            ),
                          ),
                        ],
                      ),
                    ),
                    const SizedBox(width: AppSpace.md),
                    SizedBox(
                      width: 110,
                      child: PixelButton(
                        label: _dataLimitGb <= 0 ? 'Set cap' : 'Change',
                        color: PxColors.accent,
                        border: PxColors.accentGlow,
                        onPressed: _editDataLimit,
                      ),
                    ),
                  ],
                ),
                if (_dataLimitGb > 0)
                  Padding(
                    padding: const EdgeInsets.only(top: AppSpace.sm),
                    child: Row(
                      children: [
                        GestureDetector(
                          onTap: _resetUsage,
                          child: const Text(
                            '> reset usage',
                            style: TextStyle(
                              fontFamily: PxFont.body,
                              fontSize: 12,
                              color: PxColors.textMuted,
                            ),
                          ),
                        ),
                        const SizedBox(width: AppSpace.lg),
                        GestureDetector(
                          onTap: _clearDataLimit,
                          child: const Text(
                            '> remove cap',
                            style: TextStyle(
                              fontFamily: PxFont.body,
                              fontSize: 12,
                              color: PxColors.textMuted,
                            ),
                          ),
                        ),
                      ],
                    ),
                  ),
                const SizedBox(height: AppSpace.lg),
                _rule(),
                const SizedBox(height: AppSpace.md),
                Row(
                  crossAxisAlignment: CrossAxisAlignment.center,
                  children: [
                    const Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(
                            '> fix browser proxy',
                            style: TextStyle(
                              fontFamily: PxFont.body,
                              fontSize: 14,
                              color: PxColors.textPrimary,
                            ),
                          ),
                          SizedBox(height: 3),
                          Text(
                            'resets your network proxy if the internet stops '
                            'working after disconnecting.',
                            style: TextStyle(
                              fontFamily: PxFont.body,
                              fontSize: 12,
                              height: 1.4,
                              color: PxColors.mutedDeep,
                            ),
                          ),
                        ],
                      ),
                    ),
                    const SizedBox(width: AppSpace.md),
                    SizedBox(
                      width: 110,
                      child: PixelButton(
                        label: 'Fix',
                        color: PxColors.accent,
                        border: PxColors.accentGlow,
                        onPressed: _fixingProxy ? null : _fixProxy,
                        child: _fixingProxy
                            ? const SizedBox(
                                width: 15,
                                height: 15,
                                child: CircularProgressIndicator(
                                  strokeWidth: 2,
                                  color: PxColors.accentGlow,
                                ),
                              )
                            : null,
                      ),
                    ),
                  ],
                ),
                if (_fixMsg != null)
                  Padding(
                    padding: const EdgeInsets.only(top: AppSpace.sm),
                    child: Text(
                      '> $_fixMsg',
                      style: const TextStyle(
                        fontFamily: PxFont.body,
                        fontSize: 12,
                        height: 1.4,
                        color: PxColors.textMuted,
                      ),
                    ),
                  ),
                if (_boss?['needed'] == true) ...[
                  const SizedBox(height: AppSpace.lg),
                  _rule(),
                  const SizedBox(height: AppSpace.md),
                  _bossBlock(),
                ],
                const SizedBox(height: AppSpace.lg),
                _rule(),
                const SizedBox(height: AppSpace.md),
                Center(
                  child: Text(
                    '> dreamnail v${AppConfig.appVersion}',
                    style: const TextStyle(
                      fontFamily: PxFont.body,
                      fontSize: 12,
                      letterSpacing: 1,
                      color: PxColors.mutedDeep,
                    ),
                  ),
                ),
              ],
                    ],
                  ),
                ),
              ),
            ],
          ),
          ),
        ),
      ),
    );
  }
}
