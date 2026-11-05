//! Single source of truth for the app version.
//!
//! The version lived in three places (config.dart, netcfg::APP_VERSION and the
//! Cargo manifest) and had already drifted once — while drifted, the USB update
//! check compares against the wrong number. The Flutter `config.dart` is what CI
//! reads to name every artifact (build-v4.yml, "Resolve app version"), so it wins:
//! this script parses it and hands the value to the crate as an env var, and
//! `netcfg::APP_VERSION` is that env var. Nothing to keep in step by hand.
//!
//! If config.dart isn't reachable (a crate-only build), we fall back to the Cargo
//! package version rather than failing the build.

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into()),
    );
    let dart = manifest.join("../../frontend/dreamnail_ui/lib/config.dart");
    println!("cargo:rerun-if-changed={}", dart.display());
    println!("cargo:rerun-if-changed=build.rs");

    let fallback = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let version = std::fs::read_to_string(&dart)
        .ok()
        .as_deref()
        .and_then(parse_app_version)
        .unwrap_or(fallback);

    println!("cargo:rustc-env=DREAMNAIL_APP_VERSION={version}");
}

/// Pull `4.3.9` out of `static const String appVersion = '4.3.9';`.
fn parse_app_version(src: &str) -> Option<String> {
    let line = src.lines().find(|l| l.contains("appVersion"))?;
    let rest = line.split_once('\'')?.1;
    let v = rest.split_once('\'')?.0.trim().to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}
