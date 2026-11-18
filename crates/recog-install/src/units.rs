//! Render the four launchd or systemd user units from the distribution templates.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq)]
pub enum Backend {
    Launchd,
    Systemd,
}

struct Unit {
    name: &'static str,
    mac: &'static str,
    linux: &'static str,
}

macro_rules! unit {
    ($name:literal) => {
        Unit {
            name: $name,
            mac: include_str!(concat!("../../../deploy/macos/com.recog.", $name, ".plist.in")),
            linux: include_str!(concat!("../../../deploy/linux/recog-", $name, ".service.in")),
        }
    };
}

const UNITS: [Unit; 4] = [unit!("live"), unit!("frontend"), unit!("copilot-channel"), unit!("copilot-supervisor")];

pub struct Layout<'a> {
    pub root: &'a Path,
    pub data: &'a Path,
    pub logs: &'a Path,
    pub home: &'a Path,
    pub search_path: &'a std::ffi::OsStr,
    pub unit_dir: &'a Path,
}

fn locate(name: &str, search_path: &std::ffi::OsStr) -> Option<PathBuf> {
    std::env::split_paths(search_path).find_map(|dir| {
        let file = dir.join(name);
        file.metadata().ok().filter(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).map(|_| dir)
    })
}

fn arguments(name: &str, root: &str, recording_flag: &str) -> Vec<String> {
    match name {
        "live" => {
            [format!("{root}/bin/recog"), "live".into(), "--system-audio".into(), "auto".into(), recording_flag.into()]
                .into()
        }
        "frontend" => [
            format!("{root}/bin/recog-web"),
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            "9080".into(),
            "--repo".into(),
            root.into(),
        ]
        .into(),
        _ => [format!("{root}/bin/recog-{name}"), "--repo-root".into(), root.into()].into(),
    }
}

impl Backend {
    pub fn unit_dir(self, home: &Path) -> PathBuf {
        if let Some(path) = std::env::var_os("SVC_UNIT_DIR").filter(|v| !v.is_empty()) {
            return path.into();
        }
        match self {
            Self::Launchd => home.join("Library/LaunchAgents"),
            Self::Systemd => std::env::var_os("XDG_CONFIG_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"))
                .join("systemd/user"),
        }
    }

    fn recording_flag(self, dir: &Path) -> io::Result<&'static str> {
        let path = dir.join(match self {
            Self::Launchd => "com.recog.live.plist",
            Self::Systemd => "recog-live.service",
        });
        let unit = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok("--conferences-only"),
            Err(e) => return Err(e),
        };
        for flag in ["--ktalk-only", "--conferences-only", "--ktalk"] {
            let found = match self {
                Self::Launchd => unit.contains(&format!("<string>{flag}</string>")),
                Self::Systemd => unit
                    .lines()
                    .filter_map(|line| line.strip_prefix("ExecStart="))
                    .any(|args| args.split_whitespace().any(|arg| arg == flag)),
            };
            if found {
                return Ok(flag);
            }
        }
        Ok("--conferences-only")
    }

    fn escape(self, value: &str) -> String {
        match self {
            Self::Launchd => value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;"),
            Self::Systemd => value.to_owned(),
        }
    }

    fn command(self, args: &[String]) -> String {
        match self {
            Self::Launchd => args
                .iter()
                .map(|arg| format!("<string>{}</string>", self.escape(arg)))
                .collect::<Vec<_>>()
                .join("\n        "),
            Self::Systemd => args
                .iter()
                .enumerate()
                .map(|(i, arg)| {
                    if i == 0
                        || arg.is_empty()
                        || !arg.bytes().all(|b| b.is_ascii_alphanumeric() || b"_./:=-".contains(&b))
                    {
                        format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\""))
                    } else {
                        arg.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
        }
    }

    /// Writes each complete unit atomically; never contacts the service manager.
    pub fn render(self, layout: &Layout<'_>, dest: &Path) -> io::Result<Vec<PathBuf>> {
        let recording_flag = self.recording_flag(layout.unit_dir)?;
        let node = locate("node", layout.search_path).unwrap_or_else(|| "/usr/bin".into());
        let prefix: String = ["claude", "tmux"]
            .iter()
            .filter_map(|n| locate(n, layout.search_path))
            .filter(|dir| dir.is_absolute())
            .map(|dir| format!("{}:", dir.display()))
            .collect();
        let root = layout.root.to_string_lossy();
        let values = [
            ("@REPO_DIR@", root.to_string()),
            ("@DATA_DIR@", layout.data.display().to_string()),
            ("@LOG_DIR@", layout.logs.display().to_string()),
            ("@HOME@", layout.home.display().to_string()),
            ("@NODE_BIN@", node.display().to_string()),
            ("@CLI_PATH_PREFIX@", prefix),
            ("@EXTRA_PATH@", format!("/opt/homebrew/bin:{}/.local/bin", layout.home.display())),
        ];
        fs::create_dir_all(dest)?;
        UNITS
            .iter()
            .map(|unit| {
                let (name, template) = match self {
                    Self::Launchd => (format!("com.recog.{}.plist", unit.name), unit.mac),
                    Self::Systemd => (format!("recog-{}.service", unit.name), unit.linux),
                };
                let command = self.command(&arguments(unit.name, &root, recording_flag));
                let result = substitute(template, |key| {
                    if key == "@COMMAND@" {
                        return Some(command.clone());
                    }
                    values.iter().find(|(name, _)| *name == key).map(|(_, value)| self.escape(value))
                })?;
                let path = dest.join(name);
                let mut file = tempfile::NamedTempFile::new_in(dest)?;
                file.write_all(result.as_bytes())?;
                file.persist(&path)?;
                Ok(path)
            })
            .collect()
    }
}

/// Substitute tokens in the template only. A directory name may itself contain a token.
fn substitute(mut template: &str, value: impl Fn(&str) -> Option<String>) -> io::Result<String> {
    let mut out = String::new();
    while let Some((prefix, rest)) = template.split_once('@') {
        out.push_str(prefix);
        let end = rest.bytes().take_while(|b| b.is_ascii_uppercase() || *b == b'_').count();
        if end > 0 && rest[end..].starts_with('@') {
            let key = &template[prefix.len()..prefix.len() + end + 2];
            let replacement = value(key)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("unresolved placeholder {key}")))?;
            out.push_str(&replacement);
            template = &rest[end + 1..];
        } else {
            out.push('@');
            template = rest;
        }
    }
    out.push_str(template);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_with_unknown_token_when_rendered_then_fails_before_writing() {
        assert!(substitute("x@y @TYPO@", |_| None).is_err());
    }

    #[test]
    fn path_containing_token_when_rendered_then_keeps_literal_directory_name() {
        assert_eq!(substitute("path @ROOT@", |_| Some("/tmp/@HOME@".into())).unwrap(), "path /tmp/@HOME@");
    }
}
