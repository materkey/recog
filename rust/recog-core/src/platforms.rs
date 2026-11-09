//! The conference platforms and their built-in hosts: the one definition the
//! installation settings, the meeting storage and the WebSocket origin allowlist
//! share. The hosts mirror `browser-extension/manifest.json`; a test in `ws_origin`
//! holds them equal.

/// Every platform the extension and the bridge know.
pub const PLATFORMS: [&str; 6] = ["ktalk", "vas3k", "jitsi", "matrix", "gmeet", "zoom"];

/// The exact hosts of the manifest's built-in conference pages.
pub const BUILTIN_HOSTS: [&str; 2] = ["meet.vas3k.cloud", "meet.google.com"];

/// The domains the manifest matches as `*.<domain>`.
pub const BUILTIN_WILDCARD_DOMAINS: [&str; 2] = ["ktalk.ru", "zoom.us"];

/// Whether `hostname` is one of [`BUILTIN_WILDCARD_DOMAINS`] or a subdomain of one.
#[must_use]
pub fn in_builtin_wildcard_domain(hostname: &str) -> bool {
    BUILTIN_WILDCARD_DOMAINS.iter().any(|d| hostname == *d || hostname.ends_with(&format!(".{d}")))
}
