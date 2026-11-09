//! `recog-desktop`: `python -m recog desktop …` and `python -m recog.desktop.worker`.
//!
//! `recog-desktop [--repo-root DIR] {enable,disable,attach,status} …` takes the
//! arguments, prints the output and exits with the status of `recog desktop …`;
//! `recog-desktop [--repo-root DIR] worker --root ROOT (--target JSON | --recover)` is
//! the inspector worker the observer starts. `--repo-root` names the checkout whose
//! `.env`, legacy data and `browser-extension/dist` count, by default the one this
//! binary was built from.
#![forbid(unsafe_code)]

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use recog_core::install_config::Environ;
use recog_desktop::host::Host;

fn exit(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap_or_else(|e| {
        eprintln!("recog-desktop: cannot start a runtime: {e}");
        std::process::exit(1)
    })
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut repo_root = recog_desktop::default_repo_root();
    if args.first().map(String::as_str) == Some("--repo-root") {
        let Some(dir) = args.get(1).cloned() else {
            eprintln!("recog-desktop: --repo-root needs a directory");
            return ExitCode::from(2);
        };
        repo_root = PathBuf::from(dir);
        args.drain(..2);
    }
    if args.first().map(String::as_str) == Some("worker") {
        let code = runtime().block_on(recog_desktop::worker::main(&Host::system(), &args[1..]));
        return exit(code);
    }
    // Importing recog loads the installation configuration before the arguments are
    // read; the data directory is resolved once they are.
    let mut environ: Environ = std::env::vars().collect();
    let mut warn = |notice: String| eprintln!("{notice}");
    if let Err(message) = recog_core::install_config::load(&repo_root, &mut environ, &mut warn) {
        eprintln!("{message}");
        return ExitCode::FAILURE;
    }
    let command = match recog_desktop::cli::parse(&args) {
        Ok(command) => command,
        Err(argparse) => {
            print!("{}", argparse.stdout);
            eprint!("{}", argparse.stderr);
            return exit(argparse.code);
        }
    };
    let data_dir = match recog_core::data_dir::get_data_dir(&repo_root, &environ, &mut warn) {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let outcome = runtime().block_on(recog_desktop::cli::main(&Host::system(), &command, &data_dir, &repo_root));
    print!("{}", outcome.stdout);
    eprint!("{}", outcome.stderr);
    let _ = std::io::stdout().flush();
    exit(outcome.code)
}
