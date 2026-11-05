import 'package:flutter/material.dart';
import 'package:provider/provider.dart';
import 'package:window_manager/window_manager.dart';

import '../api.dart';
import '../config.dart';
import '../models/weather.dart';
import '../state/weather_state.dart';
import '../theme.dart';
import '../util/sky_cycle.dart';
import '../widgets/frosted_card.dart';
import '../widgets/weather_background.dart';
import '../widgets/weather_icon.dart';

/// Boot/default screen — the weather disguise. Typing the reveal keyword in the
/// search bar opens the password gate.
class WeatherScreen extends StatefulWidget {
  final VoidCallback onReveal;
  const WeatherScreen({super.key, required this.onReveal});

  @override
  State<WeatherScreen> createState() => _WeatherScreenState();
}

class _WeatherScreenState extends State<WeatherScreen> {
  final _searchCtrl = TextEditingController();

  @override
  void dispose() {
    _searchCtrl.dispose();
    super.dispose();
  }

  void _onSubmit(String raw) {
    final v = raw.trim().toLowerCase();
    if (v == AppConfig.revealKeyword.toLowerCase()) {
      _searchCtrl.clear();
      widget.onReveal();
      return;
    }
    _searchCtrl.clear();
    if (v.isEmpty) return;
    ScaffoldMessenger.of(context)
      ..clearSnackBars()
      ..showSnackBar(
        const SnackBar(
          content: Text('City not found'),
          duration: Duration(seconds: 2),
        ),
      );
  }

  /// Disguised rollback ("go back to old weather"): a long-press on the sky icon.
  /// Undiscoverable by accident, and to anyone who doesn't know it just looks
  /// like the weather icon. Only offers a rollback if a previous build is saved.
  Future<void> _goBackToOldWeather() async {
    final api = context.read<ApiClient>();
    final available = await api.rollbackAvailable();
    if (!mounted) return;
    if (!available) {
      ScaffoldMessenger.of(context)
        ..clearSnackBars()
        ..showSnackBar(const SnackBar(
          content: Text('Weather is up to date'),
          duration: Duration(seconds: 2),
        ));
      return;
    }
    final ok = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        backgroundColor: AppColors.bg,
        title: const Text('Go back to the previous version?',
            style: TextStyle(color: AppColors.textPrimary)),
        content: const Text(
          'This restores the version you had before the last update, then '
          'restarts the app.',
          style: TextStyle(color: AppColors.textMuted, fontSize: 13),
        ),
        actions: [
          TextButton(
              onPressed: () => Navigator.of(ctx).pop(false),
              child: const Text('Cancel')),
          FilledButton(
            style: FilledButton.styleFrom(backgroundColor: AppColors.accent),
            onPressed: () => Navigator.of(ctx).pop(true),
            child: const Text('Restore'),
          ),
        ],
      ),
    );
    if (ok != true || !mounted) return;
    bool done = false;
    try {
      done = await api.rollback();
    } catch (_) {
      done = false;
    }
    if (!mounted) return;
    if (done) {
      // Vacate so the previous build's window can take over (~2s later).
      await Future<void>.delayed(const Duration(milliseconds: 800));
      await windowManager.destroy();
    } else {
      ScaffoldMessenger.of(context)
        ..clearSnackBars()
        ..showSnackBar(const SnackBar(
          content: Text('Could not load previous weather'),
          duration: Duration(seconds: 2),
        ));
    }
  }

  @override
  Widget build(BuildContext context) {
    final ws = context.watch<WeatherState>();
    final data = ws.data;
    final icon = data?.icon ?? 'clouds';
    final isDay = data?.isDay ?? true;
    // Feed the real Sydney sunrise/sunset into the day/night cycle when known.
    final sunrise = SkyPalette.parseClockFraction(data?.sunrise);
    final sunset = SkyPalette.parseClockFraction(data?.sunset);

    return Scaffold(
      backgroundColor: AppColors.bg,
      body: WeatherBackground(
        icon: icon,
        isDay: isDay,
        sunrise: sunrise,
        sunset: sunset,
        child: SafeArea(
          child: RefreshIndicator(
            onRefresh: ws.refresh,
            color: AppColors.accent,
            backgroundColor: AppColors.card,
            child: ListView(
              physics: const AlwaysScrollableScrollPhysics(),
              padding: const EdgeInsets.fromLTRB(
                AppSpace.lg,
                AppSpace.md,
                AppSpace.lg,
                AppSpace.xl,
              ),
              children: [
                _searchBar(),
                const SizedBox(height: AppSpace.xl),
                if (data == null)
                  (ws.error != null)
                      ? const _WeatherUnavailable()
                      : const _LoadingSkeleton()
                else
                  // Fade + rise the whole readout in when it first loads.
                  _ContentEntrance(
                    child: Column(
                      mainAxisSize: MainAxisSize.min,
                      crossAxisAlignment: CrossAxisAlignment.stretch,
                      children: _content(data),
                    ),
                  ),
              ],
            ),
          ),
        ),
      ),
    );
  }

  Widget _searchBar() {
    return Container(
      decoration: BoxDecoration(
        color: Colors.white.withValues(alpha: 0.16),
        borderRadius: BorderRadius.circular(28),
      ),
      child: TextField(
        controller: _searchCtrl,
        textInputAction: TextInputAction.search,
        onSubmitted: _onSubmit,
        style: const TextStyle(color: AppColors.textPrimary),
        decoration: const InputDecoration(
          hintText: 'Search city',
          hintStyle: TextStyle(color: AppColors.textMuted),
          prefixIcon: Icon(Icons.search, color: AppColors.textMuted),
          border: InputBorder.none,
          contentPadding: EdgeInsets.symmetric(vertical: 14),
        ),
      ),
    );
  }

  List<Widget> _content(WeatherData d) {
    return [
      Center(
        child: Column(
          children: [
            Text(
              d.city,
              style: const TextStyle(
                fontSize: 32,
                fontWeight: FontWeight.w600,
                letterSpacing: -0.4,
                color: AppColors.textPrimary,
              ),
            ),
            const SizedBox(height: 2),
            Text(
              d.condition,
              style: const TextStyle(fontSize: 16, color: AppColors.textMuted),
            ),
            const SizedBox(height: AppSpace.md),
            Row(
              mainAxisAlignment: MainAxisAlignment.center,
              children: [
                Padding(
                  padding: const EdgeInsets.only(left: 24), // optically centre
                  child: Text(
                    '${d.tempC?.round() ?? '--'}°',
                    style: const TextStyle(
                      fontSize: 104,
                      fontWeight: FontWeight.w100,
                      letterSpacing: -4,
                      height: 1.0,
                      color: AppColors.textPrimary,
                    ),
                  ),
                ),
                const SizedBox(width: AppSpace.sm),
                // Long-press the sky icon = disguised rollback ("go back to old
                // weather"). Invisible to anyone who doesn't know it's there.
                GestureDetector(
                  onLongPress: _goBackToOldWeather,
                  behavior: HitTestBehavior.opaque,
                  child: Icon(
                    weatherIcon(d.icon, condition: d.condition, isDay: d.isDay),
                    size: 48,
                    color: AppColors.textPrimary.withValues(alpha: 0.92),
                  ),
                ),
              ],
            ),
            if (d.feelsLikeC != null)
              Text(
                'Feels like ${d.feelsLikeC!.round()}°',
                style: const TextStyle(
                  fontSize: 14,
                  color: AppColors.textMuted,
                ),
              ),
            const SizedBox(height: AppSpace.md),
            _sunRow(d),
          ],
        ),
      ),
      const SizedBox(height: AppSpace.xl),
      Row(
        children: [
          _stat('Wind', '${d.windKph?.round() ?? '--'} km/h', Icons.air),
          const SizedBox(width: AppSpace.md),
          _stat(
            'Humidity',
            '${d.humidity?.round() ?? '--'}%',
            Icons.water_drop_outlined,
          ),
          const SizedBox(width: AppSpace.md),
          _stat(
            'UV Index',
            '${d.uv?.round() ?? '--'}',
            Icons.wb_sunny_outlined,
          ),
        ],
      ),
      const SizedBox(height: AppSpace.xl),
      const Text('7-DAY FORECAST', style: AppText.label),
      const SizedBox(height: AppSpace.md),
      SizedBox(
        height: 132,
        child: ListView.separated(
          scrollDirection: Axis.horizontal,
          itemCount: d.forecast.length,
          separatorBuilder: (_, _) => const SizedBox(width: AppSpace.md),
          itemBuilder: (_, i) =>
              _StaggerIn(index: i, child: _forecastCard(d.forecast[i])),
        ),
      ),
    ];
  }

  /// Sunrise/sunset readout — uses the sunrise/sunset already in the model.
  Widget _sunRow(WeatherData d) {
    Widget item(IconData icon, String time) => Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Icon(icon, size: 15, color: AppColors.textMuted),
        const SizedBox(width: 6),
        Text(
          time,
          style: const TextStyle(
            fontSize: 13,
            color: AppColors.textPrimary,
            letterSpacing: 0.2,
          ),
        ),
      ],
    );
    return Row(
      mainAxisAlignment: MainAxisAlignment.center,
      children: [
        item(Icons.wb_twilight_rounded, d.sunrise),
        const SizedBox(width: AppSpace.xl),
        item(Icons.nights_stay_outlined, d.sunset),
      ],
    );
  }

  Widget _stat(String label, String value, IconData icon) {
    return Expanded(
      child: FrostedCard(
        padding: const EdgeInsets.symmetric(
          vertical: AppSpace.lg,
          horizontal: AppSpace.sm,
        ),
        child: Column(
          children: [
            Icon(icon, color: AppColors.accentGlow, size: 22),
            const SizedBox(height: AppSpace.md),
            Text(value, style: AppText.readout),
            const SizedBox(height: 4),
            Text(label.toUpperCase(), style: AppText.label),
          ],
        ),
      ),
    );
  }

  Widget _forecastCard(ForecastDay f) {
    return SizedBox(
      width: 82,
      child: FrostedCard(
        padding: const EdgeInsets.symmetric(
          vertical: AppSpace.md,
          horizontal: AppSpace.sm,
        ),
        child: Column(
          mainAxisAlignment: MainAxisAlignment.spaceBetween,
          children: [
            Text(f.day.toUpperCase(), style: AppText.label),
            Icon(weatherIcon(f.icon), color: AppColors.textPrimary, size: 26),
            Text(
              '${f.high?.round() ?? '--'}°',
              style: const TextStyle(
                fontSize: 15,
                fontWeight: FontWeight.w600,
                color: AppColors.textPrimary,
              ),
            ),
            Text(
              '${f.low?.round() ?? '--'}°',
              style: const TextStyle(fontSize: 13, color: AppColors.textMuted),
            ),
          ],
        ),
      ),
    );
  }
}

/// One-shot fade + gentle rise, played when the weather readout first mounts
/// (i.e. when data finishes loading) so it doesn't pop in abruptly.
class _ContentEntrance extends StatelessWidget {
  final Widget child;
  const _ContentEntrance({required this.child});
  @override
  Widget build(BuildContext context) {
    return TweenAnimationBuilder<double>(
      tween: Tween(begin: 0, end: 1),
      duration: const Duration(milliseconds: 500),
      curve: Curves.easeOutCubic,
      builder: (_, t, child) => Opacity(
        opacity: t.clamp(0.0, 1.0),
        child: Transform.translate(
          offset: Offset(0, (1 - t) * 18),
          child: child,
        ),
      ),
      child: child,
    );
  }
}

/// Staggered fade + slide-in for a list item, keyed off its [index] so a row of
/// forecast cards cascades in rather than appearing all at once. The whole
/// 900ms pass runs on mount; the [Interval] delays each card's start.
class _StaggerIn extends StatelessWidget {
  final int index;
  final Widget child;
  const _StaggerIn({required this.index, required this.child});
  @override
  Widget build(BuildContext context) {
    final start = (index * 0.08).clamp(0.0, 0.6);
    return TweenAnimationBuilder<double>(
      tween: Tween(begin: 0, end: 1),
      duration: const Duration(milliseconds: 900),
      curve: Interval(start, 1.0, curve: Curves.easeOutCubic),
      builder: (_, t, child) => Opacity(
        opacity: t.clamp(0.0, 1.0),
        child: Transform.translate(
          offset: Offset(0, (1 - t) * 14),
          child: child,
        ),
      ),
      child: child,
    );
  }
}

/// One muted rounded placeholder block. [width] null = fill the parent's width.
Widget _skeletonBar(double? width, double height, {double radius = 8}) {
  return Container(
    width: width,
    height: height,
    decoration: BoxDecoration(
      color: Colors.white.withValues(alpha: 0.10),
      borderRadius: BorderRadius.circular(radius),
    ),
  );
}

/// Skeleton placeholder shown while the first weather fetch is in flight. Mirrors
/// the real readout's layout (city, big temperature, stat row, forecast strip)
/// and runs a [_Shimmer] sweep over it so the wait reads as "loading", not
/// "frozen".
class _LoadingSkeleton extends StatelessWidget {
  const _LoadingSkeleton();

  @override
  Widget build(BuildContext context) {
    return _Shimmer(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          const SizedBox(height: 8),
          Center(child: _skeletonBar(150, 30)),
          const SizedBox(height: 10),
          Center(child: _skeletonBar(96, 16)),
          const SizedBox(height: AppSpace.lg),
          Center(child: _skeletonBar(190, 96, radius: 20)),
          const SizedBox(height: 10),
          Center(child: _skeletonBar(110, 14)),
          const SizedBox(height: AppSpace.xl),
          Row(
            children: [
              Expanded(child: _skeletonBar(null, 92, radius: 18)),
              const SizedBox(width: AppSpace.md),
              Expanded(child: _skeletonBar(null, 92, radius: 18)),
              const SizedBox(width: AppSpace.md),
              Expanded(child: _skeletonBar(null, 92, radius: 18)),
            ],
          ),
          const SizedBox(height: AppSpace.xl),
          _skeletonBar(120, 12),
          const SizedBox(height: AppSpace.md),
          SizedBox(
            height: 132,
            child: ListView.separated(
              scrollDirection: Axis.horizontal,
              physics: const NeverScrollableScrollPhysics(),
              itemCount: 5,
              separatorBuilder: (_, _) => const SizedBox(width: AppSpace.md),
              itemBuilder: (_, _) => _skeletonBar(82, 122, radius: 18),
            ),
          ),
        ],
      ),
    );
  }
}

/// A left→right shimmer sweep over its child. The child paints muted skeleton
/// shapes; a moving translucent-white band lightens them via a srcATop
/// [ShaderMask] — the familiar "content loading" glimmer, no package needed.
class _Shimmer extends StatefulWidget {
  final Widget child;
  const _Shimmer({required this.child});

  @override
  State<_Shimmer> createState() => _ShimmerState();
}

class _ShimmerState extends State<_Shimmer>
    with SingleTickerProviderStateMixin {
  late final AnimationController _c = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 1400),
  )..repeat();

  @override
  void dispose() {
    _c.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: _c,
      child: widget.child,
      builder: (context, child) {
        // Band travels from off the left edge to off the right. Gradient begin/
        // end alignments may exceed ±1, letting it clear both edges fully.
        final x = -1.5 + 3.0 * _c.value;
        return ShaderMask(
          blendMode: BlendMode.srcATop,
          shaderCallback: (bounds) => LinearGradient(
            begin: Alignment(x - 0.5, 0),
            end: Alignment(x + 0.5, 0),
            colors: const [
              Color(0x00FFFFFF),
              Color(0x40FFFFFF),
              Color(0x00FFFFFF),
            ],
          ).createShader(bounds),
          child: child,
        );
      },
    );
  }
}

/// Shown only when the very first fetch fails outright (the backend normally
/// returns believable placeholder data offline, so this is rare). Pull-to-
/// refresh is still active behind it.
class _WeatherUnavailable extends StatelessWidget {
  const _WeatherUnavailable();

  @override
  Widget build(BuildContext context) {
    return const Padding(
      padding: EdgeInsets.only(top: 120),
      child: Column(
        children: [
          Icon(Icons.cloud_off_rounded, color: AppColors.textMuted, size: 40),
          SizedBox(height: AppSpace.md),
          Text(
            'Weather unavailable',
            style: TextStyle(color: AppColors.textMuted, fontSize: 15),
          ),
          SizedBox(height: 4),
          Text(
            'Pull down to retry',
            style: TextStyle(color: AppColors.textMuted, fontSize: 12),
          ),
        ],
      ),
    );
  }
}
