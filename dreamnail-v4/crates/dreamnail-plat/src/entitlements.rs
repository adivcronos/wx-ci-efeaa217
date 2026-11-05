//! Plan sync — fetch THIS customer's data cap / usage / expiry from the server.
//!
//! The admin app sets each customer's cap + expiry server-side in x-ui; the
//! client can't read them directly (it carries no panel credentials — see the
//! security notes in CLAUDE.md), so the server exposes a tiny endpoint,
//! `POST https://<server>/api/me`, that verifies the customer's OWN SSH login and
//! returns only their row. We call it right after a successful connect (when we
//! have the creds and a working route) and mirror the result into the client so
//! the data sticky-note and the expiry reminder populate themselves.
//!
//! Best-effort: any failure (offline, endpoint down, bad creds) returns `None`
//! and the caller keeps whatever was there, so sync never blocks connecting.

use std::time::Duration;

/// One customer's plan, as returned by `/api/me`.
#[derive(Debug, Clone, Default)]
pub struct Entitlements {
    /// Data cap in bytes (0 = unlimited / no cap).
    pub limit_bytes: u64,
    /// Bytes used this cycle (up + down), server-authoritative.
    pub used_bytes: u64,
    /// Plan expiry as a Unix epoch in MILLISECONDS (0 = no expiry).
    pub expiry_ms: i64,
    /// Whether the account is currently enabled on the server.
    pub enabled: bool,
    /// Remote-control: operator revoked this install — the client must wipe its
    /// stored credentials + data and uninstall itself (subscription ended /
    /// account cancelled). Set from the server's directives file via /api/me.
    pub revoked: bool,
    /// Remote-control: minimum app version the operator requires (empty = none).
    /// A client older than this should prompt the customer to update.
    pub min_version: String,
    /// Where to download the update (shown with the update prompt).
    pub update_url: String,
    /// Optional operator message shown with the update prompt.
    pub message: String,
    /// Remote-control: operator asked this install to roll back to the previous
    /// build (a bad update went out). The client runs its local rollback (the
    /// USB v2-fallback build, or the cached previous OTA build) once, then the
    /// operator clears the flag. Set from the server's directives file.
    pub rollback: bool,
    /// Remote-control: operator asked this install to update itself NOW — fetch
    /// the latest build (through the tunnel, so it survives an IP-blocking
    /// network) and apply it, instead of only showing the update banner. Fires
    /// once per directive; the operator clears the flag afterwards. Set from the
    /// server's directives file.
    pub auto_update: bool,
}

/// Ask the server for `user`'s plan. `host` is the server IP/hostname (the same
/// one the tunnel connects to); the cert there is a valid LE cert, so normal TLS
/// verification applies. Proxy env is ignored so the call goes straight to the
/// server regardless of any system proxy the connection just set.
#[must_use]
pub fn fetch(host: &str, user: &str, pass: &str) -> Option<Entitlements> {
    let url = format!("https://{host}/api/me");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(6))
        .no_proxy()
        .build()
        .ok()?;
    // Built by hand (reqwest's `json` feature isn't enabled here); the password
    // is JSON-escaped so odd characters can't break the body.
    let body = format!(
        "{{\"username\":{},\"password\":{}}}",
        json_string(user),
        json_string(pass)
    );
    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .ok()?;
    if !resp.status().is_success() {
        return None; // 401 (bad creds) / 5xx / rate-limited — leave values as-is
    }
    let text = resp.text().ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    if v.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    let get_str = |k: &str| {
        v.get(k)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let mut e = Entitlements {
        limit_bytes: v.get("limit_bytes").and_then(serde_json::Value::as_u64).unwrap_or(0),
        used_bytes: v.get("used_bytes").and_then(serde_json::Value::as_u64).unwrap_or(0),
        expiry_ms: v.get("expiry_ms").and_then(serde_json::Value::as_i64).unwrap_or(0),
        enabled: v.get("enabled").and_then(serde_json::Value::as_bool).unwrap_or(true),
        revoked: v.get("revoked").and_then(serde_json::Value::as_bool).unwrap_or(false),
        min_version: get_str("min_version"),
        update_url: get_str("update_url"),
        message: get_str("message"),
        rollback: v.get("rollback").and_then(serde_json::Value::as_bool).unwrap_or(false),
        auto_update: v.get("auto_update").and_then(serde_json::Value::as_bool).unwrap_or(false),
    };
    // Verify the operator's signature over the DANGEROUS directive fields. A
    // network doing TLS interception (its own root cert on a managed device)
    // could otherwise forge this reply and trigger a wipe / rollback / forced
    // update. The server signs those fields with a key whose public half is
    // baked in; if the signature is missing or doesn't verify we DROP every
    // dangerous directive (fail safe) and keep only the harmless plan data.
    let sig_ok = v
        .get("sig")
        .and_then(serde_json::Value::as_str)
        .and_then(crate::netcfg::hex_decode)
        .is_some_and(|sig| crate::netcfg::verify_operator_sig(&directive_canonical(user, &e), &sig));
    if !sig_ok {
        e.revoked = false;
        e.rollback = false;
        e.auto_update = false;
        e.min_version.clear();
        e.update_url.clear();
    }
    Some(e)
}

/// Rebuild the exact bytes the server signed for this customer's directives (see
/// `directive_canonical` in the entitlements server — the two MUST match byte for
/// byte). Covers only the fields that make the client act, bound to the username.
fn directive_canonical(user: &str, e: &Entitlements) -> Vec<u8> {
    let b = |x: bool| if x { "1" } else { "0" };
    format!(
        "dn-dir-v1\nuser={}\nrevoked={}\nrollback={}\nauto_update={}\nmin_version={}\nupdate_url={}",
        user,
        b(e.revoked),
        b(e.rollback),
        b(e.auto_update),
        e.min_version,
        e.update_url,
    )
    .into_bytes()
}

/// Minimal JSON string encoder for the two fields we send.
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

#[cfg(test)]
mod tests {
    use super::{directive_canonical, json_string, Entitlements};

    #[test]
    fn escapes_json_specials() {
        assert_eq!(json_string("ab"), "\"ab\"");
        assert_eq!(json_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(json_string("a\nb"), "\"a\\nb\"");
    }

    // Interop vector: signed by the operator's PRIVATE key with Python's
    // `cryptography` over the server's `directive_canonical`. Proves the Rust
    // canonical string + ring verification match the server byte for byte.
    #[test]
    fn verifies_server_signed_directive() {
        let e = Entitlements {
            auto_update: true,
            min_version: "5.5.7".into(),
            update_url: "https://penguinwalker.dev/".into(),
            ..Default::default()
        };
        let msg = directive_canonical("e2etest1", &e);
        let sig = crate::netcfg::hex_decode(
            "f1cdf57ba05d7b3332779e3de0fe90631d876d8b941a383a2be4d4626d8dee97\
             a54eecd893426d326dcc039ab844454bd077469b7ac2a1cef65b470a122c9a0f",
        )
        .unwrap();
        assert!(crate::netcfg::verify_operator_sig(&msg, &sig), "valid sig must verify");
        // Flip one field => the signature must no longer match (fail safe).
        let tampered = Entitlements { revoked: true, ..e };
        assert!(!crate::netcfg::verify_operator_sig(&directive_canonical("e2etest1", &tampered), &sig));
        // Same directive, different user => bound signature must not verify.
        assert!(!crate::netcfg::verify_operator_sig(&directive_canonical("someone-else", &tampered), &sig));
    }
}
