# Dreamnail patch to tproxy-config 7.0.7

Used via `[patch.crates-io]` in the workspace `Cargo.toml` (pulled in by
tun2proxy's route setup in the stealth/TUN helper).

**Patch:** `run_command` in `src/lib.rs` sets `CREATE_NO_WINDOW` on Windows.

**Why:** on Windows the helper (`dreamnail-tunhelper`) is built with
`windows_subsystem = "windows"`, i.e. it has no console. tproxy-config sets up
and tears down routes by running `route` (~8x), `netsh` and `powershell`; a
console program started from a console-less parent gets a NEW console window, so
every stealth connect/disconnect flashed several terminal windows. The flag runs
them invisibly; output is still captured (`.output()` pipes stdio).

To upgrade: copy the new crate from the cargo registry, re-apply the hunk above.
