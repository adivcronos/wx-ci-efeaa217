//! `HttpRouter` — proves traffic actually exits via the server.
//!
//! Mirrors the Python `exit_ip`: an in-process HTTP GET to an "what's my IP"
//! service THROUGH the local SOCKS proxy (127.0.0.1:11080, the same VLESS
//! outbound the whole app uses). The engine compares the returned IP to the
//! expected server IP; a mismatch means "connected but not routing" and the
//! connection is rejected. Using the library (reqwest) rather than shelling out
//! to curl means no console-window flash on Windows and no curl dependency.

use std::time::Duration;

use dreamnail_core::ports::{EngineError, Result, Router};

use crate::netcfg;

#[derive(Default)]
pub struct HttpRouter;

impl Router for HttpRouter {
    fn exit_ip(&self) -> Result<String> {
        let proxy_url = format!("socks5h://{}:{}", netcfg::SOCKS_HOST, netcfg::SOCKS_PORT);
        let client = reqwest::blocking::Client::builder()
            .proxy(
                reqwest::Proxy::all(&proxy_url)
                    .map_err(|e| EngineError::Proxy(format!("bad proxy url: {e}")))?,
            )
            .timeout(Duration::from_secs(12))
            .build()
            .map_err(|e| EngineError::Proxy(format!("http client: {e}")))?;

        // Our own server first (fast). Only a request that really left through
        // the VPN gets an IP back; anything else (404, error) falls through to
        // the third-party check, which then reports the true exit IP.
        if let Some(ip) = client
            .get(netcfg::FAST_VERIFY_URL)
            .timeout(Duration::from_secs(4))
            .send()
            .ok()
            .filter(|r| r.status().is_success())
            .and_then(|r| r.text().ok())
            .and_then(|b| first_ipv4(&b))
        {
            return Ok(ip);
        }

        let body = client
            .get(netcfg::PROXY_VERIFY_URL)
            .send()
            .map_err(|e| EngineError::Proxy(format!("route probe failed: {e}")))?
            .text()
            .map_err(|e| EngineError::Proxy(format!("route probe read: {e}")))?;

        // Pull the dotted-quad out of the body rather than trusting it verbatim,
        // so a trailing newline or an unexpectedly wrapped response can't fail an
        // otherwise-correct route check.
        first_ipv4(&body)
            .ok_or_else(|| EngineError::Proxy(format!("no IP in route probe: {}", short(&body))))
    }
}

/// The first IPv4 dotted-quad in `s`, or None.
fn first_ipv4(s: &str) -> Option<String> {
    for token in s.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() == 4
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u8>().is_ok())
        {
            return Some(token.to_string());
        }
    }
    None
}

fn short(s: &str) -> String {
    let t = s.trim();
    if t.len() <= 60 {
        t.to_string()
    } else {
        format!("{}…", &t[..60])
    }
}

#[cfg(test)]
mod tests {
    use super::first_ipv4;

    #[test]
    fn extracts_plain_ip() {
        assert_eq!(first_ipv4("66.226.144.136\n").as_deref(), Some("66.226.144.136"));
    }

    #[test]
    fn finds_ip_amid_text() {
        assert_eq!(first_ipv4("your ip is 1.2.3.4 today").as_deref(), Some("1.2.3.4"));
    }

    #[test]
    fn none_when_no_ip() {
        assert_eq!(first_ipv4("<!DOCTYPE html><html>no ip here</html>"), None);
    }

    #[test]
    fn rejects_out_of_range_octet() {
        // 999 is not a u8 → not a valid dotted-quad.
        assert_eq!(first_ipv4("999.1.1.1"), None);
    }
}
