//! `recog-copilot-supervisor`, in place of `python -m recog.copilot_supervisor`: the same
//! `--bridge-url` argument, the same log lines on stderr, the same frames and tmux
//! commands. The checkout is `--repo-root`, else the one whose `bin/` holds the binary,
//! else the working directory (the service units set it).
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;

use recog_copilot::ai_settings::AiSettingsFile;
use recog_copilot::cli;
use recog_copilot::config::{self, DEFAULT_BRIDGE_URL, Env};
use recog_copilot::supervisor::{ProcessRunner, Supervisor, TmuxRunner, Tunables, tmux};

const USAGE: &str = "usage: recog-copilot-supervisor [-h] [--bridge-url BRIDGE_URL] [--repo-root REPO_ROOT]

Auto-spawn the Copilot Claude session per KTalk meeting

options:
  -h, --help            show this help message and exit
  --bridge-url BRIDGE_URL
                        ktalk_bridge WebSocket URL (default: ws://127.0.0.1:8766)
  --repo-root REPO_ROOT
                        the recog checkout (default: the working directory)";

struct Args {
    bridge_url: String,
    repo_root: Option<PathBuf>,
}

fn parse(argv: impl Iterator<Item = String>) -> Result<Option<Args>, String> {
    let mut args = Args { bridge_url: DEFAULT_BRIDGE_URL.to_owned(), repo_root: None };
    let complete = cli::parse_options(argv, &["--bridge-url", "--repo-root"], |flag, value| {
        if flag == "--bridge-url" {
            args.bridge_url = value;
        } else {
            args.repo_root = Some(PathBuf::from(value));
        }
        Ok(())
    })?;
    Ok(complete.then_some(args))
}

fn main() -> ExitCode {
    match parse(std::env::args().skip(1)) {
        Ok(Some(args)) => match run(args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("recog-copilot-supervisor: {message}");
                ExitCode::FAILURE
            }
        },
        Ok(None) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{}\nrecog-copilot-supervisor: error: {message}", USAGE.lines().next().unwrap_or(""));
            ExitCode::from(2)
        }
    }
}

/// The supervisor for its whole life; an error stops it before it starts.
fn run(args: Args) -> Result<(), String> {
    let repo_root = cli::repo_root(args.repo_root)?;
    let settings = config::load(&repo_root, Env::from_process())?;
    let supervisor = build(&args.bridge_url, &repo_root, &settings);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start the runtime: {e}"))?;
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, supervisor.run());
    Ok(())
}

/// The supervisor over the real tmux runner, with the installation's settings.
fn build(bridge_url: &str, repo_root: &Path, settings: &config::Settings) -> Rc<Supervisor<TmuxRunner<ProcessRunner>>> {
    let ai_settings = Rc::new(AiSettingsFile::new(&settings.data_dir, settings.legacy.clone()));
    let process = ProcessRunner { extra_env: settings.from_dotenv.clone() };
    let mut runner = TmuxRunner::new(process, ai_settings.clone(), repo_root, &settings.mcp_url);
    runner.use_systemd_run = tmux::linux_session_manager();
    runner.session_env = settings.session_env.clone();
    let supervisor =
        Supervisor::new(bridge_url, Rc::new(runner), ai_settings, settings.autospawn, Tunables::new(settings.grace_s));
    if !supervisor.autospawn {
        eprintln!(
            "[copilot-supervisor] RECOG_COPILOT_AUTOSPAWN is off — meeting-driven auto-spawn disabled; \
             manual webapp spawn/stop and crash-respawn still work"
        );
    }
    supervisor
}
