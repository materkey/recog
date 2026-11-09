//! `recog-copilot-channel`, in place of `python -m recog.copilot_channel`: the same
//! `--bridge-url`, `--mcp-host` and `--mcp-port` arguments, the same `[copilot-channel]`
//! log lines on stderr, the same MCP endpoint and bridge frames. The checkout is
//! `--repo-root`, else the one whose `bin/` holds the binary, else the working
//! directory (the service units set it); it locates `.env`
//! and the installation configuration, which decide the data directory holding
//! `privacy.json`.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use recog_copilot::channel::link::BridgeLink;
use recog_copilot::channel::{self, DEFAULT_BRIDGE_URL, DEFAULT_MCP_HOST, DEFAULT_MCP_PORT};
use recog_copilot::cli;
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

const FLAGS: [&str; 4] = ["--bridge-url", "--mcp-host", "--mcp-port", "--repo-root"];

fn parse(argv: impl Iterator<Item = String>) -> Result<Option<Args>, String> {
    let mut args = Args {
        bridge_url: DEFAULT_BRIDGE_URL.to_owned(),
        mcp_host: DEFAULT_MCP_HOST.to_owned(),
        mcp_port: DEFAULT_MCP_PORT,
        repo_root: None,
    };
    let complete = cli::parse_options(argv, &FLAGS, |flag, value| set(&mut args, flag, value))?;
    Ok(complete.then_some(args))
}

fn set(args: &mut Args, flag: &str, value: String) -> Result<(), String> {
    match flag {
        "--bridge-url" => args.bridge_url = value,
        "--mcp-host" => args.mcp_host = value,
        "--mcp-port" => args.mcp_port = port(&value)?,
        _ => args.repo_root = Some(PathBuf::from(value)),
    }
    Ok(())
}

fn port(raw: &str) -> Result<u16, String> {
    raw.trim().parse().map_err(|_| format!("argument --mcp-port: invalid int value: {}", recog_core::py::repr(raw)))
}

fn main() -> ExitCode {
    match parse(std::env::args().skip(1)) {
        Ok(Some(args)) => run(args).unwrap_or_else(|message| {
            eprintln!("recog-copilot-channel: {message}");
            ExitCode::FAILURE
        }),
        Ok(None) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            let usage: Vec<&str> = USAGE.lines().take(2).collect();
            eprintln!("{}\nrecog-copilot-channel: error: {message}", usage.join("\n"));
            ExitCode::from(2)
        }
    }
}

/// The channel until it stops; an error stops it before it serves.
fn run(args: Args) -> Result<ExitCode, String> {
    let repo_root = cli::repo_root(args.repo_root.clone())?;
    let link = Arc::new(BridgeLink::new(PrivacyStore::new(&data_dir_of(&repo_root)?)));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start the runtime: {e}"))?;
    runtime.block_on(serve_until_signal(args, link))
}

/// What importing recog.privacy does in Python: .env and config.toml are read (a
/// malformed one stops the process), then the data directory is resolved.
fn data_dir_of(repo_root: &Path) -> Result<PathBuf, String> {
    let mut environ: install_config::Environ = std::env::vars().collect();
    let mut warn = |notice: String| eprintln!("{notice}");
    install_config::load(repo_root, &mut environ, &mut warn)?;
    data_dir::get_data_dir(repo_root, &environ, &mut warn).map_err(|e| e.to_string())
}

/// Serves until the endpoint fails, SIGINT or SIGTERM.
async fn serve_until_signal(args: Args, link: Arc<BridgeLink>) -> Result<ExitCode, String> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| format!("cannot watch SIGTERM: {e}"))?;
    Ok(tokio::select! {
        result = channel::serve(&args.bridge_url, link, &args.mcp_host, args.mcp_port) => served(result),
        _ = tokio::signal::ctrl_c() => {
            eprintln!("[copilot-channel] interrupted, exiting");
            ExitCode::SUCCESS
        }
        _ = terminate.recv() => ExitCode::SUCCESS,
    })
}

fn served(result: std::io::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[copilot-channel] task crashed: {e}");
            ExitCode::FAILURE
        }
    }
}
