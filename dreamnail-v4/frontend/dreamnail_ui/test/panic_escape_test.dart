// Double-Esc must snap back to the weather disguise from ANYWHERE in the hidden
// layer, regardless of keyboard focus. Before the app-wide PanicEscape it only
// worked while focus was inside the VPN screen: nothing on the access-key gate,
// and an open popup (CONFIG, LOGS, dialogs) swallowed the first Esc.
//
// Runs against the real engine library: DN_LIB=<path to libdreamnail_ffi.so>
// (with DREAMNAIL_DATA_DIR pointed at a scratch folder).
import 'dart:io';

import 'package:dreamnail_ui/api.dart';
import 'package:dreamnail_ui/engine.dart';
import 'package:dreamnail_ui/models/weather.dart';
import 'package:dreamnail_ui/screens/password_gate.dart';
import 'package:dreamnail_ui/screens/vpn_screen.dart';
import 'package:dreamnail_ui/state/connection_state.dart';
import 'package:dreamnail_ui/widgets/panic_escape.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:provider/provider.dart';

void main() {
  final lib = Platform.environment['DN_LIB'];
  final noEngine = lib == null || lib.isEmpty;

  // The global-shortcut plugin has no native side under `flutter test`; stub its
  // channels so its setup is a quiet no-op (the app already treats it as optional).
  setUp(() {
    final m = TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    m.setMockMethodCallHandler(
      const MethodChannel('dev.leanflutter.plugins/hotkey_manager'),
      (_) async => null,
    );
    m.setMockStreamHandler(
      const EventChannel('dev.leanflutter.plugins/hotkey_manager_event'),
      MockStreamHandler.inline(onListen: (_, _) {}),
    );
  });

  /// The app's shape: weather home + PanicEscape around the navigator.
  Future<(GlobalKey<NavigatorState>, ConnectionController, List<int>)> app(
    WidgetTester tester,
  ) async {
    tester.view.physicalSize = const Size(900, 1000);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    // The test font draws every glyph as a full-width block, so long terminal
    // lines "overflow" here only; that is not what these tests check.
    final onError = FlutterError.onError;
    FlutterError.onError = (d) {
      if (d.exceptionAsString().contains('overflowed')) return;
      onError?.call(d);
    };
    addTearDown(() => FlutterError.onError = onError);

    final api = ApiClient(Engine.open());
    final conn = ConnectionController(api);
    final nav = GlobalKey<NavigatorState>();
    final panics = <int>[];
    await tester.pumpWidget(
      MultiProvider(
        providers: [
          Provider<ApiClient>.value(value: api),
          ChangeNotifierProvider<ConnectionController>.value(value: conn),
        ],
        child: PanicEscape(
          navigatorKey: nav,
          onPanic: () {
            panics.add(1);
            nav.currentState!.popUntil((r) => r.isFirst);
          },
          child: MaterialApp(
            navigatorKey: nav,
            home: const Scaffold(body: Text('weather')),
          ),
        ),
      ),
    );
    return (nav, conn, panics);
  }

  Future<void> doubleEsc(WidgetTester tester) async {
    await tester.sendKeyEvent(LogicalKeyboardKey.escape);
    await tester.pump(const Duration(milliseconds: 50));
    await tester.sendKeyEvent(LogicalKeyboardKey.escape);
    await tester.pumpAndSettle(const Duration(milliseconds: 50));
  }

  Future<void> teardown(WidgetTester tester) async {
    await tester.pumpWidget(const SizedBox());
    await tester.pump(const Duration(seconds: 1));
  }

  Future<void> openVpn(WidgetTester tester, GlobalKey<NavigatorState> nav) async {
    nav.currentState!.push(
      MaterialPageRoute<void>(builder: (_) => VpnScreen(onBack: () {})),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 500));
  }

  testWidgets('double-Esc on the access-key gate goes back to weather',
      (tester) async {
    final (nav, _, panics) = await app(tester);
    nav.currentState!.push(
      MaterialPageRoute<void>(
        builder: (_) => PasswordGate(onUnlocked: () {}, onBack: () {}),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 500));

    await doubleEsc(tester);
    expect(panics, hasLength(1));
    expect(find.text('weather'), findsOneWidget);
    await teardown(tester);
  }, skip: noEngine);

  testWidgets('double-Esc with a popup open over the VPN screen still panics',
      (tester) async {
    final (nav, _, panics) = await app(tester);
    await openVpn(tester, nav);
    // Stand-in for the CONFIG sheet / LOGS dialog.
    showModalBottomSheet<void>(
      context: nav.currentContext!,
      builder: (_) => const SizedBox(height: 200, child: Text('config')),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 500));

    await doubleEsc(tester);
    expect(panics, hasLength(1));
    expect(find.text('weather'), findsOneWidget);
    expect(find.byType(VpnScreen), findsNothing);
    await teardown(tester);
  }, skip: noEngine);

  testWidgets('double-Esc still panics after connecting locks the fields',
      (tester) async {
    final (nav, conn, panics) = await app(tester);
    await openVpn(tester, nav);
    final fields = find.byType(TextField);
    await tester.tap(fields.at(1));
    await tester.enterText(fields.at(1), 'secret');
    conn.status = ConnStatus.fromJson({'connected': true, 'uptime_seconds': 5});
    conn.notifyListeners();
    await tester.pump(const Duration(milliseconds: 300));

    await doubleEsc(tester);
    expect(panics, hasLength(1));
    await teardown(tester);
  }, skip: noEngine);

  testWidgets('on the weather screen Esc stays ordinary (no panic)',
      (tester) async {
    final (_, _, panics) = await app(tester);
    await doubleEsc(tester);
    await doubleEsc(tester);
    expect(panics, isEmpty);
    await teardown(tester);
  }, skip: noEngine);
}
