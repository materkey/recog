//! The real process tables: psutil's answers on this operating system.

pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;

use std::sync::Arc;

use crate::host::ProcessTable;

/// psutil's `name()` past its 15-character truncation: the first argument's base name
/// when it starts with the truncated name.
#[must_use]
pub fn extend_name(name: String, cmdline: Option<&[String]>) -> String {
    if name.chars().count() < 15 {
        return name;
    }
    let Some(first) = cmdline.and_then(|c| c.first()) else { return name };
    let extended = first.rsplit('/').next().unwrap_or(first);
    if extended.starts_with(&name) { extended.to_owned() } else { name }
}

/// `parse_environ_block(data)`: `NAME=value` entries up to the first empty one,
/// entries without a name skipped, a repeated name keeping its first place and its
/// last value (a dict).
#[must_use]
pub fn parse_environ_block(data: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut pos = 0;
    while let Some(len) = data[pos..].find('\0') {
        if len == 0 {
            break;
        }
        let entry = &data[pos..pos + len];
        if let Some(equal) = entry.find('=')
            && equal > 0
        {
            let (key, value) = (&entry[..equal], &entry[equal + 1..]);
            match out.iter_mut().find(|(k, _)| k == key) {
                Some(entry) => entry.1 = value.to_owned(),
                None => out.push((key.to_owned(), value.to_owned())),
            }
        }
        pos += len + 1;
    }
    out
}

/// This system's table.
#[must_use]
pub fn table() -> Arc<dyn ProcessTable> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(macos::MacTable)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Arc::new(linux::LinuxTable::new("/proc"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environ_block_when_parsed_then_like_psutil() {
        // given
        let block = "A=1\0=hidden\0NOEQUALS\0B=x=y\0A=2\0\0TRAILING=garbage\0";
        // when
        let parsed = parse_environ_block(block);
        // then
        assert_eq!(parsed, [("A".to_owned(), "2".to_owned()), ("B".to_owned(), "x=y".to_owned())]);
    }

    #[test]
    fn truncated_name_when_extended_then_only_from_a_matching_first_argument() {
        // given
        let args = vec!["/usr/bin/gnome-keyring-daemon".to_owned()];
        // when / then
        assert_eq!(extend_name("gnome-keyring-d".into(), Some(&args)), "gnome-keyring-daemon");
        assert_eq!(extend_name("other-long-name".into(), Some(&args)), "other-long-name");
        assert_eq!(extend_name("ktalk".into(), Some(&args)), "ktalk");
    }
}
