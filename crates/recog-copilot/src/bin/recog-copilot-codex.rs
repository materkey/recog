//! `recog-copilot-codex`: the Copilot on Codex, run by the supervisor in the Copilot's
//! tmux session in place of Claude Code (docs/plans/2026-11-23-codex-copilot.md). The
//! checkout is `--repo-root`, else the one whose `bin/` holds the binary, else the
//! working directory.
#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

use recog_ai::provider::{CliEnv, resolve_cli};
use recog_copilot::ai_settings::{AiSettingsFile, CopilotSettings as _};
use recog_copilot::cli;
use recog_copilot::codex::instructions::developer_instructions;
use recog_copilot::codex::session::{self, Options, Pane, prepare_home};
use recog_copilot::config::{self, Env};

const USAGE: &str = "usage: recog-copilot-codex [-h] [--repo-root REPO_ROOT]

Run the meeting Copilot on Codex in this terminal

options:
  -h, --help            show this help message and exit
  --repo-root REPO_ROOT
                        the recog checkout (default: the working directory)";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut repo_root = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--repo-root" => repo_root = args.next().map(PathBuf::from),
            other => {
                eprintln!(
                    "{}\nrecog-copilot-codex: error: unrecognized arguments: {other}",
                    USAGE.lines().next().unwrap_or("")
                );
                return ExitCode::from(2);
            }
        }
    }
    match run(repo_root) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("recog-copilot-codex: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(repo_root: Option<PathBuf>) -> Result<(), String> {
    let repo_root = cli::repo_root(repo_root)?;
    let settings = config::load(&repo_root, Env::from_process())?;
    let task = AiSettingsFile::new(&settings.data_dir, settings.legacy).resolve()?;
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("HOME is not set")?;
    let user_home = std::env::var_os("CODEX_HOME").map_or_else(|| home.join(".codex"), PathBuf::from);
    let base = settings.data_dir.join("copilot");
    let codex_home = base.join("codex-home");
    let cwd = base.join("codex-cwd");
    let made = |e: std::io::Error| format!("{}: {e}", base.display());
    prepare_home(&codex_home, &user_home).map_err(made)?;
    std::fs::create_dir_all(&cwd).map_err(made)?;
    let logs = settings.data_dir.join("logs");
    let options = Options {
        codex: resolve_cli("codex", &CliEnv::from_process()).map_err(|e| e.to_string())?,
        codex_home,
        cwd,
        mcp_url: settings.mcp_url,
        instructions: developer_instructions(&repo_root)?,
        model: task.codex_model,
        reasoning_effort: task.reasoning_effort,
        stderr: std::fs::create_dir_all(&logs).ok().map(|()| logs.join("copilot-codex.stderr.log")),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start the runtime: {e}"))?;
    runtime.block_on(session::run(&options, &mut Pane::new(std::io::stdout())))
}
