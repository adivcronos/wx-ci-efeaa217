// v4 app config. Per-customer values are baked at build time (as in v2); these
// are the dev defaults. Unlike v2 there is no localhost backend — the engine is
// linked in-process (lib/engine.dart), so the api host/port/backendSha fields
// are gone.
class AppConfig {
  /// Launch password checked at the gate (instant client-side reject) and used
  /// to unlock the engine's encrypted store.
  static const String launchPassword = '__LAUNCH_PASSWORD__';

  /// Keyword typed in the weather search bar to reveal the password gate.
  static const String revealKeyword = '__REVEAL_KEYWORD__';

  static const String supportContact = '';
  static const String appVersion = '5.6.2';
}
