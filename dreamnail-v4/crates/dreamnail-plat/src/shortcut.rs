//! The "boss key" (Ctrl+Alt+Esc) on Linux Wayland desktops.
//!
//! On Windows, macOS and X11 the app registers its global shortcut itself. Wayland
//! deliberately won't let an app grab keys system-wide, so there the shortcut has
//! to live in the DESKTOP's own settings. We add it automatically where that's
//! scriptable (GNOME via gsettings, COSMIC via its shortcuts file); on KDE and the
//! rest we hand the person the one-line command to bind by hand (see `can_auto`).
//! Either way the command only touches a trigger file in the app's data dir; the
//! running app watches for it, snaps back to the weather disguise and hides itself.
//!
//! The shortcut is visible and removable in the desktop's own settings, and is
//! named like the weather app it disguises as ("Weather quick hide").

use std::path::{Path, PathBuf};

/// The file the shortcut touches; the UI deletes it as soon as it reacts.
pub const TRIGGER_FILE: &str = "panic.req";

const NAME: &str = "Weather quick hide";
const GNOME_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
const GNOME_PATH: &str =
    "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/weather-quickhide/";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Desktop {
    Kde,
    Gnome,
    Cosmic,
    Other,
}

impl Desktop {
    pub fn as_str(&self) -> &'static str {
        match self {
            Desktop::Kde => "kde",
            Desktop::Gnome => "gnome",
            Desktop::Cosmic => "cosmic",
            Desktop::Other => "other",
        }
    }
}

/// `XDG_CURRENT_DESKTOP` (colon-separated, e.g. `ubuntu:GNOME`) → which desktop.
pub fn desktop_from(xdg_current_desktop: &str) -> Desktop {
    let parts: Vec<String> = xdg_current_desktop
        .split(':')
        .map(|p| p.trim().to_ascii_uppercase())
        .collect();
    if parts.iter().any(|p| p == "KDE") {
        Desktop::Kde
    } else if parts.iter().any(|p| p == "COSMIC") {
        Desktop::Cosmic
    } else if parts.iter().any(|p| p == "GNOME") {
        Desktop::Gnome
    } else {
        Desktop::Other
    }
}

pub fn desktop() -> Desktop {
    desktop_from(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default())
}

/// True where the app CAN'T register the shortcut itself and needs the desktop
/// to do it: a Linux Wayland session.
pub fn needed() -> bool {
    cfg!(target_os = "linux")
        && (std::env::var("XDG_SESSION_TYPE").map(|v| v == "wayland").unwrap_or(false)
            || std::env::var_os("WAYLAND_DISPLAY").is_some())
}

/// The command a person binds by hand. On Wayland the app can't launch another
/// copy of itself to reach the running instance, so the shortcut pokes the
/// trigger file directly (the running app watches for it) — `touch`.
pub fn manual_command(trigger: &Path) -> String {
    format!("touch {}", sh_quote(&trigger.to_string_lossy()))
}

/// Can we add the shortcut automatically on this desktop? GNOME (gsettings) and
/// COSMIC (its shortcuts file) yes; KDE and the rest need a manual binding — KDE
/// Plasma 6 spreads custom shortcuts across services with no stable scriptable
/// path, so we show clear instructions rather than a button that misfires.
pub fn can_auto() -> bool {
    matches!(desktop(), Desktop::Gnome | Desktop::Cosmic)
}

pub fn installed(trigger: &Path) -> bool {
    match desktop() {
        Desktop::Gnome => gnome::installed(),
        Desktop::Cosmic => cosmic::installed(trigger),
        Desktop::Kde | Desktop::Other => false,
    }
}

pub fn install(trigger: &Path) -> Result<(), String> {
    match desktop() {
        Desktop::Gnome => gnome::install(trigger),
        Desktop::Cosmic => cosmic::install(trigger),
        Desktop::Kde | Desktop::Other => {
            Err("this desktop needs the shortcut added by hand".into())
        }
    }
}

pub fn remove(trigger: &Path) -> Result<(), String> {
    match desktop() {
        Desktop::Gnome => gnome::remove(),
        Desktop::Cosmic => cosmic::remove(trigger),
        Desktop::Kde | Desktop::Other => Ok(()),
    }
}

// --------------------------------------------------------------------------
// Quoting helpers (pure)
// --------------------------------------------------------------------------

/// POSIX single-quote a string for `sh -c`.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// GVariant text for a string (single-quoted, `\` and `'` escaped).
pub fn gvariant_str(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// Parse `gsettings get … custom-keybindings` output (`['/a/', '/b/']` or `@as []`).
pub fn parse_gvariant_str_list(s: &str) -> Vec<String> {
    let s = s.trim().trim_start_matches("@as").trim();
    let inner = s.trim_start_matches('[').trim_end_matches(']');
    inner
        .split(',')
        .map(|p| p.trim().trim_matches('\'').to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

pub fn format_gvariant_str_list(items: &[String]) -> String {
    if items.is_empty() {
        return "@as []".into();
    }
    format!(
        "[{}]",
        items.iter().map(|i| gvariant_str(i)).collect::<Vec<_>>().join(", ")
    )
}

/// RON-escaped string literal.
fn ron_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Add our Ctrl+Alt+Esc entry to COSMIC's custom-shortcuts RON map.
pub fn cosmic_insert(existing: &str, command: &str) -> String {
    let entry = format!(
        "    (\n        modifiers: [\n            Ctrl,\n            Alt,\n        ],\n        key: \"Escape\",\n    ): Spawn({}),\n",
        ron_str(command)
    );
    let t = existing.trim_end();
    match t.rfind('}') {
        Some(close) if t.trim_start().starts_with('{') => {
            let head = t[..close].trim_end();
            let sep = if head.ends_with('{') || head.ends_with(',') { "" } else { "," };
            format!("{head}{sep}\n{entry}}}\n")
        }
        _ => format!("{{\n{entry}}}\n"),
    }
}

/// Remove the entry whose `Spawn(...)` mentions `marker` (our trigger path).
pub fn cosmic_remove(existing: &str, marker: &str) -> String {
    let Some(spawn) = existing.find(marker) else {
        return existing.to_string();
    };
    // The entry starts at the `(` that opens its key tuple: the last line that is
    // exactly "(" (indented) before the Spawn.
    let start = existing[..spawn]
        .rfind("\n    (\n")
        .map(|i| i + 1)
        .unwrap_or(0);
    let end = existing[spawn..]
        .find("),\n")
        .map(|i| spawn + i + 3)
        .unwrap_or(existing.len());
    format!("{}{}", &existing[..start], &existing[end..])
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

fn run_ok(cmd: &str, args: &[&str]) -> Result<String, String> {
    crate::netid::run_timeout(cmd, args, std::time::Duration::from_secs(8))
        .ok_or_else(|| format!("{cmd} failed"))
}

// --------------------------------------------------------------------------
// GNOME: a custom keybinding under settings-daemon's media-keys.
// --------------------------------------------------------------------------
mod gnome {
    use super::*;

    fn list() -> Vec<String> {
        run_ok("gsettings", &["get", GNOME_SCHEMA, "custom-keybindings"])
            .map(|o| parse_gvariant_str_list(&o))
            .unwrap_or_default()
    }

    fn set_list(items: &[String]) -> Result<(), String> {
        run_ok(
            "gsettings",
            &["set", GNOME_SCHEMA, "custom-keybindings", &format_gvariant_str_list(items)],
        )
        .map(|_| ())
    }

    fn set_key(key: &str, value: &str) -> Result<(), String> {
        let schema = format!("{GNOME_SCHEMA}.custom-keybinding:{GNOME_PATH}");
        run_ok("gsettings", &["set", &schema, key, &gvariant_str(value)]).map(|_| ())
    }

    pub fn installed() -> bool {
        list().iter().any(|p| p == GNOME_PATH)
    }

    pub fn install(trigger: &Path) -> Result<(), String> {
        set_key("name", NAME)?;
        set_key("command", &manual_command(trigger))?;
        set_key("binding", "<Control><Alt>Escape")?;
        let mut l = list();
        if !l.iter().any(|p| p == GNOME_PATH) {
            l.push(GNOME_PATH.to_string());
            set_list(&l)?;
        }
        Ok(())
    }

    pub fn remove() -> Result<(), String> {
        let l: Vec<String> = list().into_iter().filter(|p| p != GNOME_PATH).collect();
        set_list(&l)
    }
}

// --------------------------------------------------------------------------
// COSMIC: an entry in the custom-shortcuts RON file (cosmic-settings-daemon
// watches it and applies changes live).
// --------------------------------------------------------------------------
mod cosmic {
    use super::*;

    fn path() -> PathBuf {
        home().join(".config/cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom")
    }

    pub fn installed(trigger: &Path) -> bool {
        std::fs::read_to_string(path())
            .map(|s| s.contains(&*trigger.to_string_lossy()))
            .unwrap_or(false)
    }

    pub fn install(trigger: &Path) -> Result<(), String> {
        if installed(trigger) {
            return Ok(());
        }
        let p = path();
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let cur = std::fs::read_to_string(&p).unwrap_or_default();
        std::fs::write(&p, cosmic_insert(&cur, &manual_command(trigger))).map_err(|e| e.to_string())
    }

    pub fn remove(trigger: &Path) -> Result<(), String> {
        let p = path();
        let Ok(cur) = std::fs::read_to_string(&p) else {
            return Ok(());
        };
        std::fs::write(&p, cosmic_remove(&cur, &trigger.to_string_lossy())).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_desktops() {
        assert_eq!(desktop_from("KDE"), Desktop::Kde);
        assert_eq!(desktop_from("ubuntu:GNOME"), Desktop::Gnome);
        assert_eq!(desktop_from("COSMIC"), Desktop::Cosmic);
        assert_eq!(desktop_from("XFCE"), Desktop::Other);
        assert_eq!(desktop_from(""), Desktop::Other);
    }

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("/home/a b/x"), "'/home/a b/x'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(gvariant_str("touch '/x'"), r"'touch \'/x\''");
    }

    #[test]
    fn gnome_list_round_trip() {
        assert!(parse_gvariant_str_list("@as []").is_empty());
        let l = parse_gvariant_str_list("['/a/', '/b/']\n");
        assert_eq!(l, vec!["/a/", "/b/"]);
        assert_eq!(format_gvariant_str_list(&l), "['/a/', '/b/']");
        assert_eq!(format_gvariant_str_list(&[]), "@as []");
    }

    #[test]
    fn cosmic_insert_and_remove() {
        let existing = "{\n    (\n        modifiers: [\n            Super,\n        ],\n    ): System(AppLibrary),\n}";
        let cmd = "touch '/home/u/.dreamnail/panic.req'";
        let added = cosmic_insert(existing, cmd);
        assert!(added.starts_with("{\n    (\n        modifiers: [\n            Super,"));
        assert!(added.contains("key: \"Escape\""));
        assert!(added.contains("Spawn(\"touch '/home/u/.dreamnail/panic.req'\")"));
        assert!(added.trim_end().ends_with('}'));
        // Removing restores the original entries exactly.
        let back = cosmic_remove(&added, "/home/u/.dreamnail/panic.req");
        assert_eq!(back.trim(), existing.trim());
        // Missing/empty file → a fresh map.
        let fresh = cosmic_insert("", cmd);
        assert!(fresh.starts_with("{\n    (") && fresh.trim_end().ends_with('}'));
        assert_eq!(cosmic_remove(&fresh, "/home/u/.dreamnail/panic.req").trim(), "{\n}");
    }
}
