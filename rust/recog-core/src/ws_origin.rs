//! The WebSocket Origin allowlist of the bridge and the live-stream endpoint — the
//! port of `recog/ws_origin.py`.
//!
//! WebSockets bypass CORS, so both servers check the Origin header at accept time: the
//! extension's conference hosts (mirroring `browser-extension/manifest.json`), the web
//! UI itself, and the installation's exact extra origins. An absent header (the
//! Python daemons send none) passes; the literal `null` does not.

use crate::install_config::InstallConfig;
use crate::platforms::{BUILTIN_HOSTS, in_builtin_wildcard_domain};
use crate::py;
use crate::url::urlsplit;

/// The web UI on port 9080, admitted beside the built-in conference hosts.
pub const WEBAPP_ORIGINS: [&str; 2] = ["http://127.0.0.1:9080", "http://localhost:9080"];

/// Exact-match origins: `https://` on each built-in conference host, and the web UI.
fn is_allowed_exact(origin: &str) -> bool {
    WEBAPP_ORIGINS.contains(&origin) || origin.strip_prefix("https://").is_some_and(|h| BUILTIN_HOSTS.contains(&h))
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
    if is_allowed_exact(origin) || extra.iter().any(|e| e == origin) {
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
    hostname.split('.').all(label_ok) && in_builtin_wildcard_domain(&hostname)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::*;
    use crate::platforms::BUILTIN_WILDCARD_DOMAINS;

    fn allowed(origin: &str) -> bool {
        origin_allowed(Some(origin), &[])
    }

    /// The hosts of the manifest's page patterns (`content_scripts[].matches` and
    /// `host_permissions`), split into exact hosts and `*.` wildcard domains.
    fn manifest_hosts() -> (BTreeSet<String>, BTreeSet<String>) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../browser-extension/manifest.json");
        let manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let content_scripts = manifest["content_scripts"].as_array().unwrap();
        let matches = content_scripts.iter().flat_map(|entry| entry["matches"].as_array().unwrap());
        let permissions = manifest["host_permissions"].as_array().unwrap();
        let (mut exact, mut wildcard) = (BTreeSet::new(), BTreeSet::new());
        for pattern in matches.chain(permissions) {
            // Pattern shape: <scheme>://<host>/<path>, e.g. https://*.ktalk.ru/*.
            let pattern = pattern.as_str().unwrap();
            let (scheme, rest) = pattern.split_once("://").unwrap();
            assert_eq!(scheme, "https", "unexpected scheme in {pattern:?}");
            let host = rest.split('/').next().unwrap();
            match host.strip_prefix("*.") {
                Some(domain) => wildcard.insert(domain.to_owned()),
                None => exact.insert(host.to_owned()),
            };
        }
        (exact, wildcard)
    }

    #[test]
    fn given_extension_manifest_when_compared_then_builtin_hosts_and_allowlist_match_both_ways() {
        // given: the manifest's page patterns, the source of truth the hosts mirror
        let (exact, wildcard) = manifest_hosts();
        // when
        let builtin_exact: BTreeSet<String> = BUILTIN_HOSTS.iter().map(|h| (*h).to_owned()).collect();
        let builtin_wildcard: BTreeSet<String> = BUILTIN_WILDCARD_DOMAINS.iter().map(|d| (*d).to_owned()).collect();
        // then: a host added on one side only fails here
        assert_eq!(exact, builtin_exact);
        assert_eq!(wildcard, builtin_wildcard);
        assert_eq!(WEBAPP_ORIGINS, ["http://127.0.0.1:9080", "http://localhost:9080"]);
        for host in &exact {
            assert!(allowed(&format!("https://{host}")), "{host}");
        }
        for domain in &wildcard {
            assert!(allowed(&format!("https://{domain}")), "{domain}");
            assert!(allowed(&format!("https://sub.{domain}")), "{domain}");
        }
    }

    #[test]
    fn given_ktalk_and_zoom_https_origins_when_checked_then_allowed() {
        for origin in [
            "https://ktalk.ru",
            "https://example.ktalk.ru",
            "https://a.b.ktalk.ru",
            "https://app.zoom.us",
            "https://us05web.zoom.us:443",
            "https://zoom.us",
            "https://ktalk.ru:443",
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
            "http://app.zoom.us",
            "https://evil-zoom.us",
            "https://zoom.us.evil.example",
            "https://evil.example",
            "https://",
            "not a url",
            "https://[invalid",
            "https://ktalk.ru:444",
            "https://ktalk.ru:99999",
            "https://.ktalk.ru",
            "https://evil%00.ktalk.ru",
            "https://evil-.ktalk.ru",
        ] {
            assert!(!allowed(origin), "{origin:?}");
        }
    }

    #[test]
    fn given_exact_webapp_and_absent_origins_when_checked_then_allowed() {
        for host in BUILTIN_HOSTS {
            assert!(allowed(&format!("https://{host}")));
            assert!(!allowed(&format!("http://{host}")), "{host}");
        }
        for origin in WEBAPP_ORIGINS {
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
        assert!(!origin_allowed(Some("https://meet.example.org"), &[]));
        assert!(!origin_allowed(Some("null"), &extra[..1]));
    }

    #[test]
    fn given_configured_extra_and_conference_origins_when_collected_then_each_is_admitted() {
        // given
        let mut config = InstallConfig { jitsi: vec!["https://meet.example.org".to_owned()], ..Default::default() };
        config.matrix = vec!["https://element.example.org".to_owned()];
        config.web.extra_ws_origins = vec!["https://other.example.org".to_owned()];
        // when
        let extra = extra_origins(&config);
        // then
        for origin in ["https://meet.example.org", "https://element.example.org", "https://other.example.org"] {
            assert!(origin_allowed(Some(origin), &extra), "{origin}");
            assert!(!allowed(origin), "{origin}");
        }
        assert!(!origin_allowed(Some("https://evil.example.org"), &extra));
    }

    #[test]
    fn given_leading_control_characters_when_checked_then_python_split_still_admits_the_host() {
        // Python's urlsplit drops them before splitting; the port keeps that.
        assert!(allowed("\u{0}https://ktalk.ru"));
    }
}
