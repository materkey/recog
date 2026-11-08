//! `recog-copilot-channel`, in place of `python -m recog.copilot_channel`: the same
//! `--bridge-url`, `--mcp-host` and `--mcp-port` arguments, the same `[copilot-channel]`
//! log lines on stderr, the same MCP endpoint and bridge frames. The checkout is
//! `--repo-root`, else the one whose `bin/` holds the binary, else the working
//! directory (the service units set it); it locates `.env`
//! and the installation configuration, which decide the data directory holding
//! `privacy.json`.
#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use recog_copilot::channel::link::BridgeLink;
use recog_copilot::channel::{self, DEFAULT_BRIDGE_URL, DEFAULT_MCP_HOST, DEFAULT_MCP_PORT};
use recog_core::privacy::PrivacyStore;
use recog_core::{data_dir, install_config};

const USAGE: &str = "usage: recog-copilot-channel [-h] [--bridge-url BRIDGE_URL] [--mcp-host MCP_HOST]
                             [--mcp-port MCP_PORT] [--repo-root REPO_ROOT]

KTalk Copilot channel server (MCP Streamable HTTP <-> bridge WS).

options:
  -h, --help            show this help message and exit
  --bridge-url BRIDGE_URL
                        WebSocket URL of the KTalk bridge (default: ws://127.0.0.1:8766)
  --mcp-host MCP_HOST   Host to bind the MCP HTTP server to (default: 127.0.0.1)
  --mcp-port MCP_PORT   Port to bind the MCP HTTP server to (default: 8767)
  --repo-root REPO_ROOT
                        the recog checkout (default: the working directory)";

struct Args {
    bridge_url: String,
    mcp_host: String,
    mcp_port: u16,
    repo_root: Option<PathBuf>,
}

fn parse(mut argv: impl Iterator<Item = String>) -> Result<Option<Args>, String> {
    let mut args = Args {
        bridge_url: DEFAULT_BRIDGE_URL.to_owned(),
        mcp_host: DEFAULT_MCP_HOST.to_owned(),
        mcp_port: DEFAULT_MCP_PORT,
        repo_root: None,
    };
    while let Some(arg) = argv.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag.to_owned(), Some(value.to_owned())),
            _ => (arg.clone(), None),
        };
        let mut value = |name: &str| {
            inline.clone().or_else(|| argv.next()).ok_or_else(|| format!("argument {name}: expected one argument"))
        };
        match flag.as_str() {
            "-h" | "--help" => return Ok(None),
            "--bridge-url" => args.bridge_url = value("--bridge-url")?,
            "--mcp-host" => args.mcp_host = value("--mcp-host")?,
            "--mcp-port" => {
                let raw = value("--mcp-port")?;
                args.mcp_port = raw
                    .trim()
                    .parse()
                    .map_err(|_| format!("argument --mcp-port: invalid int value: {}", recog_core::py::repr(&raw)))?;
            }
            "--repo-root" => args.repo_root = Some(PathBuf::from(value("--repo-root")?)),
            _ => return Err(format!("unrecognized arguments: {arg}")),
        }
    }
    Ok(Some(args))
}

fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1)) {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            let usage: Vec<&str> = USAGE.lines().take(2).collect();
            eprintln!("{}\nrecog-copilot-channel: error: {message}", usage.join("\n"));
            return ExitCode::from(2);
        }
    };
    // The checkout: named, else the one this binary is installed in, else the working
    // directory (the service units set it).
    let repo_root =
        match args.repo_root.or_else(recog_core::install::installed_repo).map_or_else(std::env::current_dir, Ok) {
            Ok(root) => root,
            Err(e) => {
                eprintln!("recog-copilot-channel: no working directory: {e}");
                return ExitCode::FAILURE;
            }
        };
    // What importing recog.privacy does in Python: .env and config.toml are read (a
    // malformed one stops the process), then the data directory is resolved.
    let mut environ: install_config::Environ = std::env::vars().collect();
    let mut warn = |notice: String| eprintln!("{notice}");
    if let Err(message) = install_config::load(&repo_root, &mut environ, &mut warn) {
        eprintln!("recog-copilot-channel: {message}");
        return ExitCode::FAILURE;
    }
    let data_dir = match data_dir::get_data_dir(&repo_root, &environ, &mut warn) {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("recog-copilot-channel: {e}");
            return ExitCode::FAILURE;
        }
    };
    let link = Arc::new(BridgeLink::new(PrivacyStore::new(&data_dir)));

    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("recog-copilot-channel: cannot start the runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async move {
        let mut terminate = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(e) => {
                eprintln!("recog-copilot-channel: cannot watch SIGTERM: {e}");
                return ExitCode::FAILURE;
            }
        };
        tokio::select! {
            result = channel::serve(&args.bridge_url, link, &args.mcp_host, args.mcp_port) => match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("[copilot-channel] task crashed: {e}");
                    ExitCode::FAILURE
                }
            },
            _ = tokio::signal::ctrl_c() => {
                eprintln!("[copilot-channel] interrupted, exiting");
                ExitCode::SUCCESS
            }
            _ = terminate.recv() => ExitCode::SUCCESS,
        }
    })
}
