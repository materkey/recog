//! `recog-copilot-supervisor`, in place of `python -m recog.copilot_supervisor`: the same
//! `--bridge-url` argument, the same log lines on stderr, the same frames and tmux
//! commands. The checkout is the working directory (the service units set it), or
//! `--repo-root`.
#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;

use recog_copilot::ai_settings::AiSettingsFile;
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

fn parse(mut argv: impl Iterator<Item = String>) -> Result<Option<Args>, String> {
    let mut args = Args { bridge_url: DEFAULT_BRIDGE_URL.to_owned(), repo_root: None };
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
            eprintln!("{}\nrecog-copilot-supervisor: error: {message}", USAGE.lines().next().unwrap_or(""));
            return ExitCode::from(2);
        }
    };
    let repo_root = match args.repo_root.map_or_else(std::env::current_dir, Ok) {
        Ok(root) => root,
        Err(e) => {
            eprintln!("recog-copilot-supervisor: no working directory: {e}");
            return ExitCode::FAILURE;
        }
    };
    let settings = match config::load(&repo_root, Env::from_process()) {
        Ok(settings) => settings,
        Err(message) => {
            eprintln!("recog-copilot-supervisor: {message}");
            return ExitCode::FAILURE;
        }
    };

    let ai_settings = Rc::new(AiSettingsFile::new(&settings.data_dir, settings.legacy.clone()));
    let process = ProcessRunner { extra_env: settings.from_dotenv.clone() };
    let mut runner = TmuxRunner::new(process, ai_settings.clone(), &repo_root, &settings.mcp_url);
    runner.use_systemd_run = tmux::linux_session_manager();
    runner.session_env = settings.session_env.clone();
    let supervisor = Supervisor::new(
        &args.bridge_url,
        Rc::new(runner),
        ai_settings,
        settings.autospawn,
        Tunables::new(settings.grace_s),
    );
    if !supervisor.autospawn {
        eprintln!(
            "[copilot-supervisor] RECOG_COPILOT_AUTOSPAWN is off — meeting-driven auto-spawn disabled; \
             manual webapp spawn/stop and crash-respawn still work"
        );
    }

    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("recog-copilot-supervisor: cannot start the runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, supervisor.run());
    ExitCode::SUCCESS
}
