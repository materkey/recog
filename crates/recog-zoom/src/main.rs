//! `recog-zoom-app`: the Zoom application source on its own, as
//! `python -m recog.zoom_app` runs it, for a check while the live service is up.
//!
//! Exits 0 on Ctrl-C and 1 when the source fails (the helper could not be built or
//! launched, or it went away with the permission granted). `--repo-root` names the
//! checkout that holds the Swift source and `RecogZoomHelper.app`; by default it is the
//! one this binary was built from.
#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: recog-zoom-app [--repo-root DIR]";

fn parse(mut args: impl Iterator<Item = String>) -> Result<Option<PathBuf>, String> {
    let mut repo_root = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--repo-root" => repo_root = Some(PathBuf::from(args.next().ok_or("--repo-root needs a directory")?)),
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(repo_root)
}

fn main() -> ExitCode {
    let repo_root = match parse(std::env::args().skip(1)) {
        Ok(root) => root,
        Err(message) => {
            if !message.is_empty() {
                eprintln!("recog-zoom-app: {message}");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(if message.is_empty() { 0 } else { 2 });
        }
    };
    start(repo_root.unwrap_or_else(recog_zoom::run::default_repo_root))
}

#[cfg(not(target_os = "macos"))]
fn start(_repo_root: PathBuf) -> ExitCode {
    eprintln!("recog-zoom-app supports macOS only");
    ExitCode::FAILURE
}

#[cfg(target_os = "macos")]
fn start(repo_root: PathBuf) -> ExitCode {
    use recog_zoom::log::Logger;
    use recog_zoom::run::{Options, run, run_until_interrupted};

    let options = Options::new(repo_root);
    let logger = Logger::stderr();
    run_until_interrupted(run(&options, &logger), tokio::signal::ctrl_c(), &logger)
}
