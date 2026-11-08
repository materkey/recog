//! The WebSocket Origin allowlist of the bridge and the live-stream endpoint — the
//! port of `recog/ws_origin.py`.
//!
//! WebSockets bypass CORS, so both servers check the Origin header at accept time: the
//! extension's conference hosts (mirroring `browser-extension/manifest.json`), the web
//! UI itself, and the installation's exact extra origins. An absent header (the
//! Python daemons send none) passes; the literal `null` does not.

use crate::install_config::InstallConfig;
use crate::py;
use crate::url::urlsplit;

/// Exact-match origins: the built-in conference hosts and the web UI on port 9080.
pub const ALLOWED_ORIGINS: [&str; 4] =
    ["https://meet.vas3k.cloud", "https://meet.google.com", "http://127.0.0.1:9080", "http://localhost:9080"];

/// Each admits `https://<domain>` and any `https://*.<domain>`.
pub const WILDCARD_DOMAINS: [&str; 2] = ["ktalk.ru", "zoom.us"];

fn in_wildcard_domain(hostname: &str) -> bool {
    WILDCARD_DOMAINS.iter().any(|d| hostname == *d || hostname.ends_with(&format!(".{d}")))
}

/// `EXTRA_ORIGINS`: `[web] extra_ws_origins` and the configured conference hosts.
#[must_use]
pub fn extra_origins(config: &InstallConfig) -> Vec<String> {
    config.web.extra_ws_origins.iter().chain(&config.jitsi).chain(&config.matrix).cloned().collect()
}

/// `origin_allowed(origin, extra)`: whether a WS connection with this Origin header
/// may proceed. `extra` are exact origins; they never widen the wildcard rule.
#[must_use]
pub fn origin_allowed(origin: Option<&str>, extra: &[String]) -> bool {
    let Some(origin) = origin else { return true };
    if ALLOWED_ORIGINS.contains(&origin) || extra.iter().any(|e| e == origin) {
        return true;
    }
    if origin.chars().any(py::is_space) {
        return false;
    }
    // Wildcard domains: parse the hostname — a raw suffix check on the origin string
    // would admit https://evil-ktalk.ru.
    let Ok(parts) = urlsplit(origin) else { return false };
    let Ok(port) = parts.port() else { return false };
    let hostname = parts.hostname();
    if parts.scheme != "https" {
        return false;
    }
    if parts.username().is_some()
        || parts.password().is_some()
        || !parts.path.is_empty()
        || !parts.query.is_empty()
        || !parts.fragment.is_empty()
        || !matches!(port, None | Some(443))
    {
        return false;
    }
    let Some(hostname) = hostname.filter(|h| !h.is_empty() && h.chars().count() <= 253) else { return false };
    let label_ok = |label: &str| {
        let b = label.as_bytes();
        !b.is_empty()
            && label.chars().count() <= 63
            && label.is_ascii()
            && b[0].is_ascii_alphanumeric()
            && b[b.len() - 1].is_ascii_alphanumeric()
            && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
    };
    hostname.split('.').all(label_ok) && in_wildcard_domain(&hostname)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(origin: &str) -> bool {
        origin_allowed(Some(origin), &[])
    }

    #[test]
    fn given_ktalk_and_zoom_https_origins_when_checked_then_allowed() {
        for origin in [
            "https://ktalk.ru",
            "https://example.ktalk.ru",
            "https://a.b.ktalk.ru",
            "https://app.zoom.us",
            "https://us05web.zoom.us:443",
        ] {
            assert!(allowed(origin), "{origin}");
        }
    }

    #[test]
    fn given_lookalike_or_insecure_origins_when_checked_then_rejected() {
        for origin in [
            "http://example.ktalk.ru",
            "https://evil-ktalk.ru",
            "https://ktalk.ru.evil.test",
            "https://evilzoom.us",
            "https://app.zoom.us:8443",
            "https://user@app.zoom.us",
            "https://app.zoom.us/",
            "https://app.zoom.us?x",
            "https://app.zoom.us#x",
            "https://-a.ktalk.ru",
            "https://a..ktalk.ru",
            "https://\u{0430}.ktalk.ru",
            "https://a b.ktalk.ru",
            "https://[::1]",
            "https://app.zoom.us:x",
            "null",
            "",
        ] {
            assert!(!allowed(origin), "{origin:?}");
        }
    }

    #[test]
    fn given_exact_webapp_and_absent_origins_when_checked_then_allowed() {
        for origin in ALLOWED_ORIGINS {
            assert!(allowed(origin));
        }
        assert!(origin_allowed(None, &[]));
    }

    #[test]
    fn given_extra_origin_when_checked_then_only_that_exact_origin_is_added() {
        // given
        let extra = vec!["https://meet.example.org".to_owned(), "null".to_owned()];
        // when / then
        assert!(origin_allowed(Some("https://meet.example.org"), &extra));
        assert!(origin_allowed(Some("null"), &extra));
        assert!(!origin_allowed(Some("https://other.example.org"), &extra));
        assert!(!origin_allowed(Some("https://x.meet.example.org"), &extra));
    }

    #[test]
    fn given_leading_control_characters_when_checked_then_python_split_still_admits_the_host() {
        // Python's urlsplit drops them before splitting; the port keeps that.
        assert!(allowed("\u{0}https://ktalk.ru"));
    }
}
