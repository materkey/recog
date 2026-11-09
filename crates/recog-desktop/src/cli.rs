//! `recog/desktop/cli.py`: `recog desktop {enable,disable,attach,status}`, local-only
//! configuration (no HTTP endpoint can select an inspector target).
//!
//! The arguments are read as argparse reads them for this parser (abbreviated long
//! options, `--opt=value`, unknown arguments reported by the top-level `recog`
//! parser), with its help, usage and error texts and its exit status 2.

use std::path::Path;

use recog_core::pyjson::Value;

use crate::host::Host;
use crate::py::{self, PyErr};
use crate::{processes, service, storage, worker};

/// A parsed `desktop` command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Enable { app: String },
    Disable,
    Attach,
    Status { json: bool },
}

/// What argparse prints and exits with instead of running a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exit {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

const TOP_USAGE: &str = "usage: recog [-h] {live,meetings,desktop} ...\n";
const DESKTOP_USAGE: &str = "usage: recog desktop [-h] {enable,disable,attach,status} ...\n";
const DESKTOP_HELP: &str = "usage: recog desktop [-h] {enable,disable,attach,status} ...

positional arguments:
  {enable,disable,attach,status}
    enable              Включить автоподключение в web-сервере
    disable             Выключить автоподключение новых процессов
    attach              Подключить работающий Толк вручную
    status              Состояние автоподключения

options:
  -h, --help            show this help message and exit
";
const ENABLE_HELP: &str = "usage: recog desktop enable [-h] --app APP

options:
  -h, --help  show this help message and exit
  --app APP   Путь к официальному AppImage или .app
";
const STATUS_HELP: &str = "usage: recog desktop status [-h] [--json]

options:
  -h, --help  show this help message and exit
  --json
";

fn plain_help(name: &str) -> String {
    format!("usage: recog desktop {name} [-h]\n\noptions:\n  -h, --help  show this help message and exit\n")
}

fn usage_of(help: &str) -> String {
    format!("{}\n", help.lines().next().unwrap_or_default())
}

fn error(usage: &str, prog: &str, message: &str) -> Exit {
    Exit { code: 2, stdout: String::new(), stderr: format!("{usage}{prog}: error: {message}\n") }
}

fn help(text: &str) -> Exit {
    Exit { code: 0, stdout: text.to_owned(), stderr: String::new() }
}

/// argparse's classification of an argument: an option string, or a value.
fn looks_like_option(arg: &str) -> bool {
    if !arg.starts_with('-') || arg == "-" {
        return false;
    }
    let negative = arg[1..].parse::<f64>().is_ok() && arg[1..].chars().all(|c| c.is_ascii_digit() || c == '.');
    !negative && !(arg.contains(' ') && !arg.starts_with("--"))
}

/// The option `arg` names among `options`, accepting unique prefixes of long ones,
/// with its `=value` part.
fn option<'a>(arg: &str, options: &[&'a str]) -> Result<Option<(&'a str, Option<String>)>, String> {
    let (name, inline) = match arg.split_once('=') {
        Some((n, v)) if arg.starts_with("--") => (n, Some(v.to_owned())),
        _ => (arg, None),
    };
    if let Some(exact) = options.iter().find(|o| **o == name) {
        return Ok(Some((exact, inline)));
    }
    if !name.starts_with("--") {
        return Ok(None);
    }
    let candidates: Vec<&&str> = options.iter().filter(|o| o.starts_with("--") && o.starts_with(name)).collect();
    match candidates.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some((one, inline))),
        many => Err(format!(
            "ambiguous option: {name} could match {}",
            many.iter().map(|o| o.to_string()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// Parses the arguments after `recog desktop`.
///
/// # Errors
/// The help or the error argparse prints, with its exit status.
pub fn parse(args: &[String]) -> Result<Command, Exit> {
    let (subcommand, mut extras, rest) = split_subcommand(args)?;
    let (usage_help, options) = subcommand_options(&subcommand)?;
    let usage = usage_of(&usage_help);
    let prog = format!("recog desktop {subcommand}");
    let fail = |message: &str| error(&usage, &prog, message);
    let read = read_options(&rest, options, &usage_help, &fail, &mut extras)?;
    let command = command_of(&subcommand, read).map_err(|m| fail(&m))?;
    if !extras.is_empty() {
        return Err(error(TOP_USAGE, "recog", &format!("unrecognized arguments: {}", extras.join(" "))));
    }
    Ok(command)
}

/// The subcommand, the unrecognized options before it and the arguments after it.
fn split_subcommand(args: &[String]) -> Result<(String, Vec<String>, Vec<String>), Exit> {
    let mut extras = Vec::new();
    for (at, arg) in args.iter().enumerate() {
        if !looks_like_option(arg) {
            return Ok((arg.clone(), extras, args[at + 1..].to_vec()));
        }
        if option(arg, &["-h", "--help"]).map_err(|m| error(DESKTOP_USAGE, "recog desktop", &m))?.is_some() {
            return Err(help(DESKTOP_HELP));
        }
        extras.push(arg.clone());
    }
    Err(error(DESKTOP_USAGE, "recog desktop", "the following arguments are required: desktop_command"))
}

/// The subcommand's help and its options.
fn subcommand_options(subcommand: &str) -> Result<(String, &'static [&'static str]), Exit> {
    match subcommand {
        "enable" => Ok((ENABLE_HELP.to_owned(), &["-h", "--help", "--app"])),
        "status" => Ok((STATUS_HELP.to_owned(), &["-h", "--help", "--json"])),
        "disable" | "attach" => Ok((plain_help(subcommand), &["-h", "--help"])),
        other => Err(error(
            DESKTOP_USAGE,
            "recog desktop",
            &format!(
                "argument desktop_command: invalid choice: {} (choose from enable, disable, attach, status)",
                recog_core::py::repr(other)
            ),
        )),
    }
}

/// What the options after the subcommand set.
#[derive(Default)]
struct Options {
    app: Option<String>,
    json: bool,
}

/// One argument after the subcommand.
enum Arg {
    Extra,
    Help,
    Json(Option<String>),
    App(Option<String>),
}

fn classify(arg: &str, options: &[&str]) -> Result<Arg, String> {
    if !looks_like_option(arg) {
        return Ok(Arg::Extra);
    }
    Ok(match option(arg, options)? {
        Some(("-h" | "--help", _)) => Arg::Help,
        Some(("--json", inline)) => Arg::Json(inline),
        Some(("--app", inline)) => Arg::App(inline),
        _ => Arg::Extra,
    })
}

/// The options after the subcommand; what it does not know goes to `extras`.
fn read_options(
    rest: &[String],
    options: &[&str],
    usage_help: &str,
    fail: &dyn Fn(&str) -> Exit,
    extras: &mut Vec<String>,
) -> Result<Options, Exit> {
    let mut read = Options::default();
    let mut at = 0;
    while at < rest.len() {
        let arg = &rest[at];
        at += 1;
        match classify(arg, options).map_err(|m| fail(&m))? {
            Arg::Extra => extras.push(arg.clone()),
            Arg::Help => return Err(help(usage_help)),
            Arg::Json(inline) => read.json = json_flag(inline).map_err(|m| fail(&m))?,
            Arg::App(inline) => read.app = Some(app_value(inline, rest, &mut at).map_err(|m| fail(&m))?),
        }
    }
    Ok(read)
}

/// `--json`, which takes no value.
fn json_flag(inline: Option<String>) -> Result<bool, String> {
    match inline {
        Some(value) => Err(format!("argument --json: ignored explicit argument {}", recog_core::py::repr(&value))),
        None => Ok(true),
    }
}

/// `--app`'s value: `--app=VALUE`, or the next argument unless it is an option.
fn app_value(inline: Option<String>, rest: &[String], at: &mut usize) -> Result<String, String> {
    if let Some(value) = inline {
        return Ok(value);
    }
    match rest.get(*at) {
        Some(value) if !looks_like_option(value) => {
            *at += 1;
            Ok(value.clone())
        }
        _ => Err("argument --app: expected one argument".to_owned()),
    }
}

fn command_of(subcommand: &str, read: Options) -> Result<Command, String> {
    match subcommand {
        "enable" => read
            .app
            .map(|app| Command::Enable { app })
            .ok_or_else(|| "the following arguments are required: --app".to_owned()),
        "status" => Ok(Command::Status { json: read.json }),
        "disable" => Ok(Command::Disable),
        _ => Ok(Command::Attach),
    }
}

/// `result["state"] + (": " + result["reason"] if result.get("reason") else "")`.
fn human_status(result: &Value) -> Result<String, PyErr> {
    let state = py::index(result, "state")?;
    let reason = py::get(result, "reason")?.filter(|r| r.truthy());
    let suffix = match reason {
        None => String::new(),
        Some(Value::Str(reason)) => format!(": {reason}"),
        Some(other) => {
            return Err(PyErr::type_error(format!("can only concatenate str (not \"{}\") to str", other.type_name())));
        }
    };
    match state {
        Value::Str(state) => Ok(format!("{state}{suffix}")),
        Value::Array(_) => Err(PyErr::type_error("can only concatenate list (not \"str\") to list")),
        other => {
            Err(PyErr::type_error(format!("unsupported operand type(s) for +: '{}' and 'str'", other.type_name())))
        }
    }
}

/// What a command printed and its exit status.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

async fn execute(
    host: &Host,
    command: &Command,
    root: &Path,
    repo_root: &Path,
    out: &mut String,
) -> Result<i32, PyErr> {
    match command {
        Command::Enable { app } => enable(host, app, root, repo_root, out),
        Command::Disable => disable(root, out),
        Command::Attach => attach(host, root, out).await,
        Command::Status { json } => status(host, root, *json, out),
    }
}

/// `enable --app APP`: the application checked, the extension snapshotted, both
/// configured.
fn enable(host: &Host, app: &str, root: &Path, repo_root: &Path, out: &mut String) -> Result<i32, PyErr> {
    let app = processes::application_path(host, app)?;
    let source = repo_root.join("browser-extension/dist");
    let (digest, _) = storage::prepare_bundle(&source, root)?;
    storage::write_json(
        &root.join("config.json"),
        &py::obj([
            ("enabled", Value::Bool(true)),
            ("app", Value::from(app.to_string_lossy().as_ref())),
            ("bundle", Value::from(digest)),
        ]),
    )?;
    out.push_str("Автоподключение включено. Оно работает, пока запущен web-сервер Recog.\n");
    Ok(0)
}

/// `disable`: the rest of the configuration kept.
fn disable(root: &Path, out: &mut String) -> Result<i32, PyErr> {
    let config = storage::read(&root.join("config.json"))?;
    storage::write_json(&root.join("config.json"), &py::spread(&config, &py::obj([("enabled", Value::Bool(false))]))?)?;
    out.push_str("Автоподключение выключено. Уже загруженное расширение действует до выхода из Толка.\n");
    Ok(0)
}

/// `attach`: one worker run on the one main process, 0 when it attached.
async fn attach(host: &Host, root: &Path, out: &mut String) -> Result<i32, PyErr> {
    let config = storage::read(&root.join("config.json"))?;
    let raw = worker::path_arg(py::index(&config, "app")?)?;
    let app = processes::application_path(host, raw)?;
    let targets = processes::find_targets(host, &app)?;
    if targets.len() != 1 {
        return Err(PyErr::value("Нужен ровно один работающий основной процесс выбранного Толка"));
    }
    let result = worker::run(host, root, Some(&targets[0]), false).await?;
    out.push_str(&py::dumps(&result));
    out.push('\n');
    let attached = py::index(&result, "state")?.as_str() == Some("attached");
    Ok(i32::from(!attached))
}

/// `status [--json]`.
fn status(host: &Host, root: &Path, json: bool, out: &mut String) -> Result<i32, PyErr> {
    let result = service::status_at(root, host.clock.time());
    if json {
        out.push_str(&py::dumps(&result));
    } else {
        out.push_str(&human_status(&result)?);
    }
    out.push('\n');
    Ok(0)
}

/// `main(args, data_dir)`: runs a command for the data directory, the extension
/// taken from `repo_root/browser-extension/dist`.
pub async fn main(host: &Host, command: &Command, data_dir: &Path, repo_root: &Path) -> Outcome {
    let root = data_dir.join("desktop");
    let mut stdout = String::new();
    match execute(host, command, &root, repo_root, &mut stdout).await {
        Ok(code) => Outcome { code, stdout, stderr: String::new() },
        Err(e) if e.is_os() || e.is_value() || e.is("KeyError") || e.is("TypeError") => {
            stdout.push_str(&format!("Desktop integration: {}\n", e.message));
            Outcome { code: 1, stdout, stderr: String::new() }
        }
        // Python prints the traceback, whose last line this is.
        Err(e) => Outcome { code: 1, stdout, stderr: format!("{}\n", e.described()) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn malformed_invocations_when_parsed_then_argparse_texts_and_status_2() {
        // given / when / then
        let missing = parse(&args("")).unwrap_err();
        assert_eq!(missing.code, 2);
        assert_eq!(
            missing.stderr,
            "usage: recog desktop [-h] {enable,disable,attach,status} ...\nrecog desktop: error: the following arguments are required: desktop_command\n"
        );
        assert_eq!(
            parse(&args("enable")).unwrap_err().stderr,
            "usage: recog desktop enable [-h] --app APP\nrecog desktop enable: error: the following arguments are required: --app\n"
        );
        assert_eq!(
            parse(&args("status --js --bogus x")).unwrap_err().stderr,
            "usage: recog [-h] {live,meetings,desktop} ...\nrecog: error: unrecognized arguments: --bogus x\n"
        );
        assert!(parse(&args("bogus")).unwrap_err().stderr.contains("invalid choice: 'bogus' (choose from enable"));
        assert_eq!(parse(&args("status -h")).unwrap_err().stdout, STATUS_HELP);
    }

    #[test]
    fn valid_invocations_when_parsed_then_commands() {
        // given / when / then
        assert_eq!(parse(&args("enable --ap=/x")).unwrap(), Command::Enable { app: "/x".into() });
        assert_eq!(parse(&args("enable --app /y")).unwrap(), Command::Enable { app: "/y".into() });
        assert_eq!(parse(&args("status --j")).unwrap(), Command::Status { json: true });
        assert_eq!(parse(&args("attach")).unwrap(), Command::Attach);
    }

    #[test]
    fn shared_prefix_when_option_looked_up_then_ambiguous_like_argparse() {
        // given
        let options = ["-h", "--json", "--jump"];
        // when / then
        assert_eq!(option("--j", &options), Err("ambiguous option: --j could match --json, --jump".to_owned()));
        assert_eq!(option("--ju=1", &options), Ok(Some(("--jump", Some("1".to_owned())))));
        assert_eq!(option("-x", &options), Ok(None));
    }

    #[test]
    fn reason_present_or_not_when_status_rendered_then_human_line() {
        // given
        let with = py::obj([("state", Value::from("waiting")), ("reason", Value::from("cleanup_failed"))]);
        let without = py::obj([("state", Value::from("waiting")), ("reason", Value::Null)]);
        // when / then
        assert_eq!(human_status(&with).unwrap(), "waiting: cleanup_failed");
        assert_eq!(human_status(&without).unwrap(), "waiting");
    }
}
