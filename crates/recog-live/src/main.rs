//! `recog`: the live process of recog in Rust (`python -m recog live` with the same
//! flags), `meetings`, and `replay` of a golden session. `[live] runtime = "rust"`
//! makes the live service run it (recog/cli.py hands over to it).
#![forbid(unsafe_code)]

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let code = match argv.as_slice() {
        [cmd, dir] if cmd == "__package-macos-apps" => {
            recog_live::cli::exit_code(recog_live::cli::install::package_macos_apps(dir.as_ref()))
        }
        #[cfg(target_os = "macos")]
        [cmd, action] if cmd == "__macos-permission" => {
            recog_live::cli::exit_code(recog_live::cli::macos_permissions::command(action))
        }
        _ => recog_live::cli::main(&argv),
    };
    // Python's live pipeline ends with os._exit: threads still blocked on a pipe or a
    // socket must not hold the process.
    std::process::exit(code);
}
