// Phase-B FFI smoke test: proves Dart calls the Rust engine IN-PROCESS via
// dart:ffi (the same boundary the Flutter app uses). Everything goes through the
// generic dn_request(json)->json entry point.
//
//   cargo build -p dreamnail-ffi
//   cd crates/dreamnail-ffi/smoke && dart pub get
//   DN_LIB=../../../target/debug/libdreamnail_ffi.so dart run bin/main.dart
import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'package:ffi/ffi.dart';

typedef _VoidC = Void Function();
typedef _Void = void Function();
typedef _ReqC = Pointer<Utf8> Function(Pointer<Utf8>);
typedef _Req = Pointer<Utf8> Function(Pointer<Utf8>);
typedef _FreeC = Void Function(Pointer<Utf8>);
typedef _Free = void Function(Pointer<Utf8>);

void main() {
  final libPath = Platform.environment['DN_LIB'] ??
      '../../../target/debug/libdreamnail_ffi.so';
  final lib = DynamicLibrary.open(libPath);
  lib.lookupFunction<_VoidC, _Void>('dn_init')();
  final req = lib.lookupFunction<_ReqC, _Req>('dn_request');
  final free = lib.lookupFunction<_FreeC, _Free>('dn_free_string');

  Map<String, dynamic> call(String method, [Map<String, dynamic> args = const {}]) {
    final input = jsonEncode({'method': method, 'args': args}).toNativeUtf8();
    final out = req(input);
    calloc.free(input);
    final s = out.toDartString();
    free(out);
    return jsonDecode(s) as Map<String, dynamic>;
  }

  print('[dart] health : ${call('health')}');
  print('[dart] status : ${call('status')}');
  final c = call('connect', {'username': 'v4probe', 'password': 'notarealpw', 'remember': false});
  print('[dart] connect(dummy) : $c');
  print('[dart] status : ${call('status')}');
  print('[dart] FFI round-trip OK');
}
