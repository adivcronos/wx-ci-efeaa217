import 'package:dreamnail_ui/state/connection_state.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  final f = ConnectionController.friendlyError;

  test('plain-English engine messages pass through untouched', () {
    const msg =
        "The server isn't responding. Check your internet connection and try again.";
    expect(f(msg), msg);
    expect(f('Wrong username or password.'), 'Wrong username or password.');
  });

  test('raw engine text is replaced, never shown', () {
    expect(f('invalid credentials'), 'Wrong username or password.');
    expect(
      f('traffic not routing: expected 66.226.144.136, got 9.9.9.9'),
      "Connected, but traffic isn't going through the VPN. Please try again.",
    );
    expect(
      f('tunnel error: IO(Os { code: 111, kind: ConnectionRefused })'),
      'Connection failed. Please try again.',
    );
  });

  test('an IP address or OS error code can never reach the screen', () {
    for (final raw in [
      'something odd happened talking to 66.226.144.136',
      'bind failed (os error 98)',
    ]) {
      final out = f(raw);
      expect(out.contains('66.226'), isFalse);
      expect(out.toLowerCase().contains('os error'), isFalse);
    }
  });
}
