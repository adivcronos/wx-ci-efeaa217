// Low-level in-process bridge to the native Rust engine (dreamnail-ffi) via
// dart:ffi. Everything goes through one call — dn_request(json) -> json — so the
// higher-level ApiClient (api.dart) keeps the exact v2 method surface with no
// separate process, no localhost HTTP, no port 8765, no auth token.
//
// requestSync runs on the calling isolate (fine for the fast methods, and for
// status/proxy_status which the native side answers lock-free). requestAsync
// runs the call on a background isolate so a multi-second connect() never janks
// the UI — the Rust engine's global actor is process-wide, so the isolate's call
// reaches the same engine.
import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'dart:isolate';

import 'package:ffi/ffi.dart';

typedef _VoidNative = Void Function();
typedef _VoidDart = void Function();
typedef _ReqNative = Pointer<Utf8> Function(Pointer<Utf8>);
typedef _ReqDart = Pointer<Utf8> Function(Pointer<Utf8>);
typedef _FreeNative = Void Function(Pointer<Utf8>);
typedef _FreeDart = void Function(Pointer<Utf8>);

String _resolveLibPath() {
  final env = Platform.environment['DN_LIB'];
  if (env != null && env.isNotEmpty) return env;
  // Windows: dart:ffi finds a bare-name DLL in the exe's own directory (that's
  // where CMake bundles it), so a bare name is enough.
  if (Platform.isWindows) return 'dreamnail_ffi.dll';
  // macOS: dlopen does NOT search the .app bundle for a bare name, so resolve the
  // dylib we ship in Contents/Frameworks, relative to Contents/MacOS/<exe>.
  if (Platform.isMacOS) {
    final macosDir = File(Platform.resolvedExecutable).parent.path;
    return '$macosDir/../Frameworks/libdreamnail_ffi.dylib';
  }
  // Linux: the exe's $ORIGIN/lib rpath covers the bundled lib/ dir.
  return 'libdreamnail_ffi.so';
}

/// Open the engine dylib, logging the resolved path and any loader error. A
/// silent load miss (wrong path, unresolved dep, macOS library-validation block)
/// otherwise surfaces up the stack as a generic "connect failed" with no clue
/// where it broke — so we log the exact path + error and rethrow a descriptive
/// message the UI can recognise as a local-service (not a network) failure.
DynamicLibrary _openLib(String libPath) {
  try {
    return DynamicLibrary.open(libPath);
  } catch (e) {
    stderr.writeln('[engine] FFI load FAILED for "$libPath": $e');
    throw Exception('local service unavailable: could not load the engine '
        'library at "$libPath" ($e)');
  }
}

/// Run one dn_request against a freshly-resolved library handle. Used both on the
/// main isolate and (via Isolate.run) on a background isolate.
Map<String, dynamic> _callOnce(String libPath, String method, Map<String, dynamic> args) {
  final lib = _openLib(libPath);
  lib.lookupFunction<_VoidNative, _VoidDart>('dn_init')();
  final req = lib.lookupFunction<_ReqNative, _ReqDart>('dn_request');
  final free = lib.lookupFunction<_FreeNative, _FreeDart>('dn_free_string');
  final input = jsonEncode({'method': method, 'args': args}).toNativeUtf8();
  final outPtr = req(input);
  calloc.free(input);
  if (outPtr == nullptr) return const {};
  final out = outPtr.toDartString();
  free(outPtr);
  final decoded = jsonDecode(out);
  return decoded is Map ? Map<String, dynamic>.from(decoded) : const {};
}

class Engine {
  final String _libPath;
  Engine._(this._libPath);

  factory Engine.open() {
    final path = _resolveLibPath();
    stderr.writeln('[engine] resolving engine library at "$path"');
    // Load + start the actor once up front (logs the path + any load error).
    _openLib(path).lookupFunction<_VoidNative, _VoidDart>('dn_init')();
    return Engine._(path);
  }

  /// Fast/lock-free calls (status, settings, unlock, …).
  Map<String, dynamic> requestSync(String method, [Map<String, dynamic> args = const {}]) =>
      _callOnce(_libPath, method, args);

  /// Slow calls (connect) — run off the UI isolate.
  Future<Map<String, dynamic>> requestAsync(String method, Map<String, dynamic> args) {
    final path = _libPath;
    return Isolate.run(() => _callOnce(path, method, args));
  }
}
