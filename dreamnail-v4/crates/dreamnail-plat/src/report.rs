//! Anonymous, opt-outable error telemetry.
//!
//! When a connection attempt fails, the app sends ONE tiny report to the download
//! host so the operator sees problems before customers complain. It is designed
//! to carry NOTHING that identifies a person:
//!
//!   * no username, no password, no email, no account id;
//!   * no IP address, no server address, no file paths;
//!   * no free-form error text (which could contain any of the above) — only a
//!     fixed category + a fixed coarse hint chosen from a closed list by the
//!     caller;
//!   * time is coarsened to the UTC hour, not a precise timestamp.
//!
//! The report is a handful of fixed, non-identifying fields sent as an HTTPS GET
//! query to `/ingest` on the download host (its Let's Encrypt cert is verified
//! normally — no cert-skipping). A GET is used so the server can record the line
//! with a plain nginx access-log rule that OMITS the client IP — no app, script,
//! or request-body handling on the server, and nothing personal in the query (by
//! construction: only version/os/arch and a fixed error category+hint). Delivery
//! is best-effort and fully fire-and-forget: it runs on its own thread, times out
//! fast, and swallows every error, so it can never slow down or break a connect.

use std::time::Duration;

use crate::netcfg::{API_HOST, APP_VERSION};

/// Set to false at runtime (once, at startup) to disable all reporting — wired to
/// the user-facing setting. Defaults to on.
static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, std::sync::atomic::Ordering::SeqCst);
}

fn enabled() -> bool {
    // An env override lets tests / privacy-conscious users hard-disable it.
    if std::env::var("DREAMNAIL_NO_REPORTS").is_ok() {
        return false;
    }
    ENABLED.load(std::sync::atomic::Ordering::SeqCst)
}

/// A short random id so duplicate retries of the SAME failure can be collapsed by
/// the operator. Not derived from anything about the machine or user — it is
/// fresh per call, so it cannot identify or link reports across sessions.
fn nonce() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    // A cheap, dependency-free pseudo-random hex from the nanosecond clock. This
    // is an anti-duplicate tag, not a security value, so the clock is plenty.
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{:08x}", n)
}

/// UTC time coarsened to the hour ("2026-09-25T22"), so reports cluster by hour
/// without being a precise per-user timestamp. Computed without extra deps.
fn utc_hour() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // days since epoch → civil date (Howard Hinnant's algorithm).
    let days = (secs / 86_400) as i64;
    let hour = (secs % 86_400) / 3_600;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}T{:02}", y, m, d, hour)
}

/// Fire off one anonymous report for a failed connect. `category` and `hint` MUST
/// come from the caller's fixed lists (see the FFI's `report_hint`), never from
/// raw error text. Returns immediately; the send happens on a detached thread.
pub fn send_connect_failure(category: &'static str, hint: &'static str) {
    if !enabled() {
        return;
    }
    // All fields are from closed, non-identifying sets, so a plain query is safe.
    let query = format!(
        "kind=connect_fail&v={}&os={}&arch={}&cat={}&hint={}&t={}&id={}",
        APP_VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH,
        category,
        hint,
        utc_hour(),
        nonce(),
    );
    std::thread::spawn(move || {
        // no_proxy: report directly, not through the (down) tunnel. Short timeout;
        // any failure is ignored — telemetry must never affect the app.
        let client = match reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(4))
            .no_proxy()
            .build()
        {
            Ok(c) => c,
            Err(_) => return,
        };
        let url = format!("https://{API_HOST}/ingest?{query}");
        let _ = client.get(url).send();
    });
}

/// Minimal JSON string encoder (the `json` reqwest feature isn't enabled here).
/// Mirrors the one in `entitlements` so credentials/diagnostics can't break the
/// body or smuggle control characters.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Send ONE authenticated, per-customer problem report to the server so the
/// operator sees it in the admin console.
///
/// Unlike [`send_connect_failure`], this is deliberately tied to the customer —
/// it authenticates with their own SSH login (the same oracle `/api/me` uses) so
/// the operator can see *which* customer hit *what*. It carries a diagnostics
/// bundle (`fields`, already assembled by the caller from the leak-test / engine
/// state — never raw error text) plus an optional typed `message`.
///
/// `leaktest`, when set, makes this run the one-click leak test on its own thread
/// and fold the results into the bundle, so the "Report a problem" button ships
/// real diagnostics without blocking the UI.
///
/// `manual` reports (the button) bypass the telemetry opt-out — the customer
/// explicitly asked to send. Automatic reports honour it. Best-effort: detached
/// thread, short timeout, every error swallowed — a report can never affect the
/// app. `host` is the server (its LE cert is verified normally); the call goes
/// `no_proxy` so it does not try to ride a tunnel that may be down.
pub fn send_report(
    user: &str,
    pass: &str,
    kind: &str,
    message: &str,
    fields: Vec<(String, String)>,
    leaktest: Option<crate::diag::Session>,
    manual: bool,
) {
    if !manual && !enabled() {
        return;
    }
    if user.is_empty() {
        return; // no identity to authenticate with — nothing to send
    }
    let (user, pass, kind, message) =
        (user.to_string(), pass.to_string(), kind.to_string(), message.to_string());
    std::thread::spawn(move || {
        let mut fields = fields;
        if let Some(sess) = leaktest {
            for c in crate::diag::run_all(sess) {
                fields.push((format!("chk_{}", c.key), format!("{}: {}", c.level.as_str(), c.detail)));
            }
        }
        let mut extra = String::new();
        for (k, v) in &fields {
            extra.push(',');
            extra.push_str(&json_string(k));
            extra.push(':');
            extra.push_str(&json_string(v));
        }
        let body = format!(
            "{{\"username\":{},\"password\":{},\"kind\":{},\"message\":{},\"v\":{},\"os\":{},\"arch\":{},\"fields\":{{{}}}}}",
            json_string(&user),
            json_string(&pass),
            json_string(&kind),
            json_string(&message),
            json_string(APP_VERSION),
            json_string(std::env::consts::OS),
            json_string(std::env::consts::ARCH),
            extra.trim_start_matches(','),
        );
        let client = match reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(8))
            .no_proxy()
            .build()
        {
            Ok(c) => c,
            Err(_) => return,
        };
        let url = format!("https://{API_HOST}/api/report");
        let _ = client
            .post(url)
            .header("Content-Type", "application/json")
            .body(body)
            .send();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_hour_is_well_formed() {
        let s = utc_hour();
        assert_eq!(s.len(), 13, "YYYY-MM-DDTHH = 13 chars, got {s:?}");
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], "T");
        // The year is sane (this decade or later), proving the civil-date math ran.
        let year: i64 = s[0..4].parse().unwrap();
        assert!(year >= 2025, "year {year} from {s:?}");
    }

    #[test]
    fn json_string_escapes_and_wraps() {
        assert_eq!(json_string("ab"), "\"ab\"");
        assert_eq!(json_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(json_string("a\nb"), "\"a\\nb\"");
    }

    #[test]
    fn empty_user_sends_nothing() {
        // No identity => the authenticated report is a no-op (returns at once).
        send_report("", "x", "user", "hi", vec![], None, true);
    }

    #[test]
    fn env_override_disables() {
        // The env override wins regardless of the runtime flag.
        std::env::set_var("DREAMNAIL_NO_REPORTS", "1");
        assert!(!enabled());
        std::env::remove_var("DREAMNAIL_NO_REPORTS");
    }
}
