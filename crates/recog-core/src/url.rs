//! `urllib.parse.urlsplit`/`urlparse` and the `ipaddress` checks the ported modules
//! depend on, as CPython 3.12 does them. The allowlists and the meeting ids are
//! decided by how Python splits a URL, oddities included (leading control characters
//! are dropped, tabs and newlines removed anywhere), so the port splits the same way
//! instead of following the URL standard.
//!
//! Known difference: `urlsplit` refuses a non-ASCII netloc whose NFKC normalisation
//! contains `/?#@:`; the port does not normalise and accepts it.

use std::net::Ipv6Addr;

use crate::py;

/// `urlsplit(url)`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SplitResult {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

fn scheme_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')
}

/// `urlsplit(url)`.
///
/// # Errors
/// The `ValueError` text Python raises for a malformed bracketed host.
pub fn urlsplit(url: &str) -> Result<SplitResult, String> {
    let url: String =
        url.trim_start_matches(|c: char| c <= ' ').chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let mut result = SplitResult::default();
    let mut rest = url.as_str();
    if let Some(i) = rest.find(':')
        && i > 0
        && rest.as_bytes()[0].is_ascii_alphabetic()
        && rest[..i].chars().all(scheme_char)
    {
        result.scheme = rest[..i].to_ascii_lowercase();
        rest = &rest[i + 1..];
    }
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        result.netloc = after[..end].to_owned();
        rest = &after[end..];
        let (open, close) = (result.netloc.contains('['), result.netloc.contains(']'));
        if open != close {
            return Err("Invalid IPv6 URL".into());
        }
        if open {
            check_bracketed_netloc(&result.netloc)?;
        }
    }
    let (rest, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    result.path = path.to_owned();
    result.query = query.to_owned();
    result.fragment = fragment.to_owned();
    Ok(result)
}

/// `urlparse(url)`: [`urlsplit`] and the `;params` of the last path segment, which
/// leave the path.
///
/// # Errors
/// What [`urlsplit`] raises.
pub fn urlparse(url: &str) -> Result<(SplitResult, String), String> {
    const USES_PARAMS: [&str; 16] = [
        "", "ftp", "hdl", "prospero", "http", "imap", "https", "shttp", "rtsp", "rtsps", "rtspu", "sip", "sips", "mms",
        "sftp", "tel",
    ];
    let mut split = urlsplit(url)?;
    let mut params = String::new();
    if USES_PARAMS.contains(&split.scheme.as_str()) && split.path.contains(';') {
        let from = split.path.rfind('/').unwrap_or(0);
        if let Some(i) = split.path[from..].find(';').map(|i| i + from) {
            params = split.path[i + 1..].to_owned();
            split.path.truncate(i);
        }
    }
    Ok((split, params))
}

fn partition<'a>(s: &'a str, sep: &str) -> (&'a str, bool, &'a str) {
    match s.split_once(sep) {
        Some((a, b)) => (a, true, b),
        None => (s, false, ""),
    }
}

fn rpartition_tail(s: &str, sep: char) -> &str {
    s.rsplit_once(sep).map_or(s, |(_, tail)| tail)
}

fn check_bracketed_netloc(netloc: &str) -> Result<(), String> {
    check_bracketed_host(bracketed_hostname(netloc)?)
}

/// The host of a netloc with brackets: between them, nothing before the opening one
/// and only a port after the closing one.
fn bracketed_hostname(netloc: &str) -> Result<&str, String> {
    let host_and_port = rpartition_tail(netloc, '@');
    let (before, open, bracketed) = partition(host_and_port, "[");
    if !open {
        return Ok(partition(host_and_port, ":").0);
    }
    if !before.is_empty() {
        return Err("Invalid IPv6 URL".into());
    }
    let (hostname, _, port) = partition(bracketed, "]");
    if !port.is_empty() && !port.starts_with(':') {
        return Err("Invalid IPv6 URL".into());
    }
    Ok(hostname)
}

/// `_check_bracketed_host`: an IPvFuture address or an IPv6 one.
fn check_bracketed_host(hostname: &str) -> Result<(), String> {
    if let Some(future) = hostname.strip_prefix('v') {
        return check_ipvfuture(future);
    }
    if parse_ipv4(hostname).is_ok() {
        return Err("An IPv4 address cannot be in brackets".into());
    }
    if !is_ipv6(hostname) {
        return Err(format!("{} does not appear to be an IPv4 or IPv6 address", py::repr(hostname)));
    }
    Ok(())
}

/// `v[a-fA-F0-9]+\..+` after the `v`.
fn check_ipvfuture(future: &str) -> Result<(), String> {
    let (version, dot, address) = partition(future, ".");
    let valid = dot && !version.is_empty() && version.chars().all(|c| c.is_ascii_hexdigit()) && !address.is_empty();
    if !valid {
        return Err("IPvFuture address is invalid".into());
    }
    Ok(())
}

/// `ipaddress.IPv6Address(text)` succeeds: a zone id after `%` is allowed.
fn is_ipv6(text: &str) -> bool {
    let (address, percent, zone) = partition(text, "%");
    if percent && (zone.is_empty() || zone.contains('%')) {
        return false;
    }
    address.parse::<Ipv6Addr>().is_ok()
}

impl SplitResult {
    fn hostinfo(&self) -> (&str, Option<&str>) {
        let hostinfo = rpartition_tail(&self.netloc, '@');
        let (_, open, bracketed) = partition(hostinfo, "[");
        let (hostname, port) = if open {
            let (hostname, _, port) = partition(bracketed, "]");
            (hostname, partition(port, ":").2)
        } else {
            let (hostname, _, port) = partition(hostinfo, ":");
            (hostname, port)
        };
        (hostname, if port.is_empty() { None } else { Some(port) })
    }

    /// `.hostname`: lowercased except an IPv6 zone.
    #[must_use]
    pub fn hostname(&self) -> Option<String> {
        let (hostname, _) = self.hostinfo();
        if hostname.is_empty() {
            return None;
        }
        let (host, percent, zone) = partition(hostname, "%");
        Some(format!("{}{}{zone}", host.to_lowercase(), if percent { "%" } else { "" }))
    }

    /// `.port`.
    ///
    /// # Errors
    /// Python's `ValueError` text for a port that is not a number in range.
    pub fn port(&self) -> Result<Option<u32>, String> {
        let Some(port) = self.hostinfo().1 else { return Ok(None) };
        if !port.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("Port could not be cast to integer value as {}", py::repr(port)));
        }
        match port.trim_start_matches('0').parse::<u64>() {
            Ok(n) if n <= 65535 => Ok(Some(u32::try_from(n).unwrap_or(0))),
            _ if port.trim_start_matches('0').is_empty() => Ok(Some(0)),
            _ => Err("Port out of range 0-65535".into()),
        }
    }

    fn userinfo(&self) -> Option<(&str, Option<&str>)> {
        let (userinfo, _) = self.netloc.rsplit_once('@')?;
        let (username, have_password, password) = partition(userinfo, ":");
        Some((username, have_password.then_some(password)))
    }

    /// `.username`.
    #[must_use]
    pub fn username(&self) -> Option<&str> {
        self.userinfo().map(|(username, _)| username)
    }

    /// `.password`.
    #[must_use]
    pub fn password(&self) -> Option<&str> {
        self.userinfo().and_then(|(_, password)| password)
    }
}

/// `ipaddress.IPv4Address(text)`: the address, or the `AddressValueError` text.
///
/// # Errors
/// Python's message, e.g. `Expected 4 octets in '1.2.3'`.
pub fn parse_ipv4(text: &str) -> Result<[u8; 4], String> {
    if text.is_empty() {
        return Err("Address cannot be empty".into());
    }
    if text.contains('/') {
        return Err(format!("Unexpected '/' in {}", py::repr(text)));
    }
    let octets: Vec<&str> = text.split('.').collect();
    if octets.len() != 4 {
        return Err(format!("Expected 4 octets in {}", py::repr(text)));
    }
    let mut out = [0u8; 4];
    for (slot, octet) in out.iter_mut().zip(&octets) {
        *slot = parse_octet(octet).map_err(|e| format!("{e} in {}", py::repr(text)))?;
    }
    Ok(out)
}

fn parse_octet(octet: &str) -> Result<u8, String> {
    if octet.is_empty() {
        return Err("Empty octet not permitted".into());
    }
    // Python's str.isdigit() is wider than ASCII, but isascii() comes first.
    if !(octet.is_ascii() && octet.bytes().all(|b| b.is_ascii_digit())) {
        return Err(format!("Only decimal digits permitted in {}", py::repr(octet)));
    }
    if octet.len() > 3 {
        return Err(format!("At most 3 characters permitted in {}", py::repr(octet)));
    }
    if octet != "0" && octet.starts_with('0') {
        return Err(format!("Leading zeros are not permitted in {}", py::repr(octet)));
    }
    let value: u32 = octet.parse().unwrap_or(0);
    u8::try_from(value).map_err(|_| format!("Octet {value} (> 255) not permitted"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_origin_splits_into_scheme_host_and_port_like_python() {
        // given
        let split = urlsplit("HTTPS://User:pw@Meet.Example.org:443/a?b#c").unwrap();
        // then
        assert_eq!(split.scheme, "https");
        assert_eq!(split.netloc, "User:pw@Meet.Example.org:443");
        assert_eq!(split.hostname().as_deref(), Some("meet.example.org"));
        assert_eq!(split.port(), Ok(Some(443)));
        assert_eq!((split.username(), split.password()), (Some("User"), Some("pw")));
        assert_eq!((split.path.as_str(), split.query.as_str(), split.fragment.as_str()), ("/a", "b", "c"));
    }

    #[test]
    fn leading_control_characters_and_embedded_tabs_are_dropped_before_splitting() {
        // given / when
        let split = urlsplit("\u{0}\u{1} https://kt\talk.ru").unwrap();
        // then
        assert_eq!((split.scheme.as_str(), split.netloc.as_str()), ("https", "ktalk.ru"));
    }

    #[test]
    fn malformed_brackets_and_ports_fail_with_python_messages() {
        // given / when / then
        assert_eq!(urlsplit("https://[::1").unwrap_err(), "Invalid IPv6 URL");
        assert_eq!(urlsplit("https://[1.2.3.4]").unwrap_err(), "An IPv4 address cannot be in brackets");
        assert_eq!(urlsplit("https://[zz]").unwrap_err(), "'zz' does not appear to be an IPv4 or IPv6 address");
        assert_eq!(urlsplit("https://[::1]x").unwrap_err(), "Invalid IPv6 URL");
        assert!(urlsplit("https://[fe80::1%en0]:8080").is_ok());
        assert_eq!(
            urlsplit("https://a:x").unwrap().port(),
            Err("Port could not be cast to integer value as 'x'".into())
        );
        assert_eq!(urlsplit("https://a:70000").unwrap().port(), Err("Port out of range 0-65535".into()));
        assert_eq!(urlsplit("https://a:").unwrap().port(), Ok(None));
    }

    #[test]
    fn bracketed_hosts_when_split_then_ipvfuture_and_misplaced_brackets_checked_as_python() {
        // given / when / then
        assert!(urlsplit("https://[v1.x]/r").is_ok());
        assert!(urlsplit("https://user@[vA.b:c]:80").is_ok());
        for (url, message) in [
            ("https://[v1]", "IPvFuture address is invalid"),
            ("https://[vz.x]", "IPvFuture address is invalid"),
            ("https://[v.x]", "IPvFuture address is invalid"),
            ("https://[v1.]", "IPvFuture address is invalid"),
            ("https://a[::1]", "Invalid IPv6 URL"),
            ("https://[x@h]", "'h]' does not appear to be an IPv4 or IPv6 address"),
            ("https://[]", "'' does not appear to be an IPv4 or IPv6 address"),
            ("https://[::1%]", "'::1%' does not appear to be an IPv4 or IPv6 address"),
        ] {
            assert_eq!(urlsplit(url).unwrap_err(), message, "{url}");
        }
    }

    #[test]
    fn urlparse_moves_the_last_segment_params_out_of_the_path() {
        // given / when
        let (split, params) = urlparse("https://h/a;x/b;y;z").unwrap();
        // then
        assert_eq!((split.path.as_str(), params.as_str()), ("/a;x/b", "y;z"));
    }

    #[test]
    fn ipv4_parsing_reports_like_ipaddress() {
        // given / when / then
        assert_eq!(parse_ipv4("127.0.0.1"), Ok([127, 0, 0, 1]));
        assert_eq!(parse_ipv4("127.1").unwrap_err(), "Expected 4 octets in '127.1'");
        assert_eq!(parse_ipv4("1.2.3.256").unwrap_err(), "Octet 256 (> 255) not permitted in '1.2.3.256'");
        assert_eq!(parse_ipv4("a.0x1.2.3").unwrap_err(), "Only decimal digits permitted in 'a' in 'a.0x1.2.3'");
        assert_eq!(parse_ipv4("01.2.3.4").unwrap_err(), "Leading zeros are not permitted in '01' in '01.2.3.4'");
    }
}
