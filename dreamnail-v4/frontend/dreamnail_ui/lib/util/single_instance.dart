import 'dart:io';

import 'app_paths.dart';

RandomAccessFile? _lockHandle; // held (open, locked) for the app's lifetime

/// How long [acquireSingleInstanceLock] keeps retrying before giving up. A prior
/// instance that just exited may still hold the OS lock for a brief
/// moment; retrying avoids a spurious "already running".
const _acquireRetryWindow = Duration(seconds: 2);

/// Cross-platform single-instance guard: an exclusive advisory lock on
/// `ui.lock` in the app data dir ([AppPaths.dataDir]). The OS releases the lock
/// when the process dies, so a crash can never wedge future launches.
///
/// Why this exists: the whole engine (SSH tunnel, Xray child, proxy/TUN routing)
/// runs IN-PROCESS via the FFI. A second instance would fight the first over the
/// engine actor and the system proxy / TUN device, leaving both broken. So only
/// one may run at a time.
///
/// Returns false when another instance already holds the lock (after retrying
/// for [_acquireRetryWindow]). If the lock file can't even be created (exotic
/// permissions), it fails open — a broken guard must never stop the app itself
/// from starting.
bool acquireSingleInstanceLock() {
  final RandomAccessFile raf;
  try {
    Directory(AppPaths.dataDir()).createSync(recursive: true);
    raf = File(AppPaths.file('ui.lock')).openSync(mode: FileMode.append);
  } catch (_) {
    return true; // can't check → fail open
  }
  final deadline = DateTime.now().add(_acquireRetryWindow);
  while (true) {
    try {
      // FileLock.exclusive is non-blocking: it throws if another process holds
      // the lock, rather than waiting.
      raf.lockSync(FileLock.exclusive);
      _lockHandle = raf;
      return true;
    } on FileSystemException {
      if (DateTime.now().isAfter(deadline)) {
        try {
          raf.closeSync();
        } catch (_) {}
        return false;
      }
      sleep(const Duration(milliseconds: 100));
    }
  }
}

/// Release the single-instance lock so another instance (e.g. an elevated
/// relaunch) can take it. Safe to call when the lock isn't held.
void releaseSingleInstanceLock() {
  try {
    _lockHandle?.unlockSync();
    _lockHandle?.closeSync();
  } catch (_) {}
  _lockHandle = null;
}

/// Test hook alias — kept for existing tests.
void releaseSingleInstanceLockForTest() => releaseSingleInstanceLock();
