//! Microphone capture process of recog, in place of `recog/mic_worker.py` (selected by
//! `[live] mic_worker = "rust"`): the same arguments, frames and exit codes.
//!
//! stdout carries only frames (`frame.rs`); everything else goes to stderr. Exit codes:
//! 3 when the stream stops capturing (an error from CoreAudio, or ten seconds without a
//! callback), 4 when the named device is absent, 0 when the parent closed the pipe.
#![forbid(unsafe_code)]

mod args;
#[cfg(target_os = "macos")]
mod capture;
// Built and tested everywhere; only the macOS capture calls them.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod devices;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod frame;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod framer;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod physical;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod resample;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args = match args::parse(std::env::args().skip(1)) {
        Ok(args::Parsed::Run(args)) => args,
        Ok(args::Parsed::Help) => {
            eprintln!("{}", args::USAGE);
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("{}\nrecog-mic: error: {message}", args::USAGE);
            return ExitCode::from(2);
        }
    };
    ExitCode::from(run(&args) as u8)
}

#[cfg(target_os = "macos")]
fn run(args: &args::Args) -> i32 {
    // Keep the pipe to ourselves: whatever a library prints lands on stderr.
    let pipe = match nix::unistd::dup(std::io::stdout()) {
        Ok(fd) => std::fs::File::from(fd),
        Err(e) => {
            eprintln!("Cannot duplicate stdout: {e}");
            return 1;
        }
    };
    if let Err(e) = nix::unistd::dup2_stdout(std::io::stderr()) {
        eprintln!("Cannot point stdout at stderr: {e}");
        return 1;
    }
    capture::run(args, pipe)
}

#[cfg(not(target_os = "macos"))]
fn run(_args: &args::Args) -> i32 {
    // The microphone is captured in the live process on other systems.
    eprintln!("recog-mic captures on macOS only");
    1
}
