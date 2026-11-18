//! The command line of `recog/cli.py` (argparse): `live` with every flag and the hidden
//! aliases, `meetings`, and the Rust-only `replay` and installation helpers (`check`,
//! `config`, `launcher`, `prefetch`). Options take `--name value` or `--name=value`
//! and, as argparse allows, any unambiguous prefix of their name.

use std::fmt::Write as _;

/// `recog live`'s arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveArgs {
    pub mic_device: String,
    pub system_audio: Option<String>,
    pub system_device: Option<String>,
    pub chunk_duration: f64,
    pub output: Option<String>,
    pub list_devices: bool,
    pub ktalk: bool,
    pub ktalk_only: bool,
    pub zoom_app: bool,
    pub screencapture: bool,
    pub mic_speaker_name: Option<String>,
    pub no_save_audio: bool,
}

impl Default for LiveArgs {
    fn default() -> Self {
        Self {
            mic_device: "auto".into(),
            system_audio: None,
            system_device: None,
            chunk_duration: 3.0,
            output: None,
            list_devices: false,
            ktalk: false,
            ktalk_only: false,
            zoom_app: false,
            screencapture: false,
            mic_speaker_name: None,
            no_save_audio: false,
        }
    }
}

/// `recog meetings`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MeetingsArgs {
    pub meeting_id: Option<String>,
    pub json: bool,
    pub refresh_cache: bool,
}

/// `recog replay`'s arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayArgs {
    pub session: String,
    pub out: String,
    pub profile: Option<String>,
    pub tail: f64,
}

/// A value `recog config` prints for the shell scripts and the extension build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigQuery {
    /// The data directory (`scripts/data-dir.sh`).
    DataDir,
    /// The installation configuration's path (`scripts/config-path.sh`).
    ConfigPath,
    /// The configured conference hosts as JSON (`python -m recog.install_config`).
    Conferences,
    /// `[install] runtime` as the parser reads it.
    InstallRuntime,
    /// The Copilot's Claude model and effort (`./recog-service.sh claude`).
    CopilotLaunch,
}

/// The names `recog config` takes, in the order of its help.
pub const CONFIG_QUERIES: [(&str, ConfigQuery); 5] = [
    ("data-dir", ConfigQuery::DataDir),
    ("config-path", ConfigQuery::ConfigPath),
    ("conferences", ConfigQuery::Conferences),
    ("install-runtime", ConfigQuery::InstallRuntime),
    ("copilot-launch", ConfigQuery::CopilotLaunch),
];

/// What to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Live(LiveArgs),
    Meetings(MeetingsArgs),
    Replay(ReplayArgs),
    /// The KTalk desktop subcommands with their arguments, run by `recog-desktop`.
    Desktop(Vec<String>),
    ServiceRender(String),
    /// Whether the settings can serve: what `scripts/deps.sh` asked Python.
    Check,
    /// One value for the shell scripts.
    Config(ConfigQuery),
    /// Build `Recog.app` when needed (`python -m recog.macos_launcher`).
    Launcher,
    /// Download the ASR model into the Hugging Face cache.
    Prefetch,
    /// No command: the help, exit 1.
    None,
}

/// A parse that ends the process: the text and the exit code (0 for `--help`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exit {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

pub const MAIN_USAGE: &str =
    "usage: recog [-h] {live,meetings,replay,desktop,check,config,launcher,prefetch,service} ...";

const CHECK_USAGE: &str = "usage: recog check [-h]";

const CONFIG_USAGE: &str = "usage: recog config [-h] {data-dir,config-path,conferences,install-runtime,copilot-launch}";

const LAUNCHER_USAGE: &str = "usage: recog launcher [-h]";

const PREFETCH_USAGE: &str = "usage: recog prefetch [-h]";

const LIVE_USAGE: &str = "usage: recog live [-h] [--mic-device MIC_DEVICE] [--system-audio BACKEND]
                  [--chunk-duration CHUNK_DURATION] [--output OUTPUT]
                  [--list-devices] [--ktalk] [--ktalk-only] [--zoom-app]
                  [--mic-speaker-name MIC_SPEAKER_NAME] [--no-save-audio]";

const MEETINGS_USAGE: &str = "usage: recog meetings [-h] [--json] [--refresh-cache] [meeting_id]";

const REPLAY_USAGE: &str = "usage: recog replay [-h] [--profile PROFILE] [--tail TAIL] session out";

#[must_use]
pub fn main_help() -> String {
    format!(
        "{MAIN_USAGE}

CLI-утилита для live-транскрибирования и управления митингами

positional arguments:
  {{live,meetings,replay,desktop,check,config,launcher,prefetch,service}}
                        Доступные команды
    live                Live транскрипция с микрофона и/или системного звука
    meetings            Просмотр сохранённых митингов
    replay              Воспроизвести golden-сессию в live-процессе
    desktop             KTalk desktop (recog-desktop)
    check               Проверить настройки установки (для scripts/deps.sh)
    config              Напечатать значение для скриптов установки
    launcher            Собрать Recog.app, если её исходник изменился (macOS)
    prefetch            Скачать модель ASR в кэш Hugging Face
    service render DIR  Записать файлы служб в каталог DIR

options:
  -h, --help            show this help message and exit
"
    )
}

fn live_help(mic_default: &str) -> String {
    format!(
        "{LIVE_USAGE}

options:
  -h, --help            show this help message and exit
  --mic-device MIC_DEVICE
                        ID устройства микрофона, 'auto' — устройство ввода по
                        умолчанию, 'none' — без микрофона (по умолчанию: auto)
  --system-audio BACKEND
                        Источник системного звука: auto (по ОС) |
                        screencapture (macOS, ScreenCaptureKit) | pulse
                        (Linux, PulseAudio/PipeWire) | device:<id>
                        (устройство ввода, device:auto — найти BlackHole) |
                        none (по умолчанию: none)
  --chunk-duration CHUNK_DURATION
                        Длительность чанка для ASR (сек, по умолчанию: 3.0)
  --output OUTPUT       Сохранять транскрипт в файл
  --list-devices        Показать доступные аудио устройства
  --ktalk               Использовать KTalk Chrome extension для определения
                        спикеров
  --ktalk-only          Захватывать звук только во время встречи KTalk
                        (включает --ktalk)
  --zoom-app            Определять спикеров в нативном приложении Zoom (macOS,
                        нужен доступ Accessibility)
  --mic-speaker-name MIC_SPEAKER_NAME
                        Имя спикера для этого запуска (по умолчанию: профиль,
                        затем {mic_default})
  --no-save-audio       Не записывать аудио встреч (playback сегментов будет
                        недоступен; по умолчанию аудио пишется и хранится 24
                        часа)
"
    )
}

fn meetings_help() -> String {
    format!(
        "{MEETINGS_USAGE}

positional arguments:
  meeting_id       ID митинга для просмотра (без аргумента - список всех)

options:
  -h, --help       show this help message and exit
  --json           Вывести в JSON формате
  --refresh-cache  Пересканировать внешние директории (обновить кэш)
"
    )
}

fn replay_help() -> String {
    format!(
        "{REPLAY_USAGE}

positional arguments:
  session            the golden session's directory
  out                where the replay's data and replay.json go

options:
  -h, --help         show this help message and exit
  --profile PROFILE  user-profile.json to replay with
  --tail TAIL        seconds the live process runs on after the last event
"
    )
}

fn error(usage: &str, prog: &str, message: &str) -> Exit {
    Exit { stdout: String::new(), stderr: format!("{usage}\n{prog}: error: {message}\n"), code: 2 }
}

struct Spec {
    name: &'static str,
    takes_value: bool,
}

/// The option `arg` names among `specs`: exact, or the one option it is a prefix of.
fn find_option<'a>(specs: &'a [Spec], name: &str) -> Result<&'a Spec, Vec<&'a str>> {
    if let Some(spec) = specs.iter().find(|s| s.name == name) {
        return Ok(spec);
    }
    let matches: Vec<&Spec> = specs.iter().filter(|s| s.name.starts_with(name)).collect();
    match matches.as_slice() {
        [one] => Ok(one),
        _ => Err(matches.iter().map(|s| s.name).collect()),
    }
}

/// The options given (name, value), the positionals, and whether help was asked for.
type Split = (Vec<(&'static str, Option<String>)>, Vec<String>, bool);

/// What one argument is before the option table is consulted.
enum ArgKind {
    Positional,
    /// `--`: everything after it is positional.
    EndOfOptions,
    Help,
    Option,
}

fn arg_kind(arg: &str, only_positionals: bool) -> ArgKind {
    if only_positionals || !arg.starts_with('-') || arg == "-" {
        ArgKind::Positional
    } else if arg == "--" {
        ArgKind::EndOfOptions
    } else if arg == "-h" || arg == "--help" {
        ArgKind::Help
    } else {
        ArgKind::Option
    }
}

/// Options and positionals of one subcommand.
fn split(args: &[String], specs: &[Spec], usage: &str, prog: &str) -> Result<Split, Exit> {
    let mut options = Vec::new();
    let mut positionals = Vec::new();
    let mut unrecognized = Vec::new();
    let mut help = false;
    let mut i = 0;
    let mut only_positionals = false;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        match arg_kind(arg, only_positionals) {
            ArgKind::Positional => positionals.push(arg.clone()),
            ArgKind::EndOfOptions => only_positionals = true,
            ArgKind::Help => help = true,
            ArgKind::Option => {
                if let Some((name, value, used)) = option(arg, args.get(i), specs, usage, prog)? {
                    i += used;
                    options.push((name, value));
                } else {
                    unrecognized.push(arg.clone());
                }
            }
        }
    }
    if !unrecognized.is_empty() {
        return Err(error(MAIN_USAGE, "recog", &format!("unrecognized arguments: {}", unrecognized.join(" "))));
    }
    Ok((options, positionals, help))
}

/// An option's name, its value, and how many of the following arguments it took.
type Taken = (&'static str, Option<String>, usize);

/// An option argument (`next` the argument after it) and what it took; None when no
/// option has that name.
fn option(arg: &str, next: Option<&String>, specs: &[Spec], usage: &str, prog: &str) -> Result<Option<Taken>, Exit> {
    let (name, inline) = match arg.split_once('=') {
        Some((name, value)) if name.starts_with("--") => (name, Some(value.to_owned())),
        _ => (arg, None),
    };
    match find_option(specs, name) {
        Ok(spec) if spec.takes_value => {
            option_value(spec, inline, next, usage, prog).map(|(value, used)| Some((spec.name, Some(value), used)))
        }
        Ok(spec) => flag(spec, inline.as_deref(), usage, prog).map(|()| Some((spec.name, None, 0))),
        Err(candidates) if candidates.len() > 1 => {
            let mut message = format!("ambiguous option: {name} could match ");
            let _ = write!(message, "{}", candidates.join(", "));
            Err(error(usage, prog, &message))
        }
        Err(_) => Ok(None),
    }
}

/// The value of an option that takes one: after `=`, else the next argument unless
/// it is another option (a negative number is a value).
fn option_value(
    spec: &Spec,
    inline: Option<String>,
    next: Option<&String>,
    usage: &str,
    prog: &str,
) -> Result<(String, usize), Exit> {
    if let Some(value) = inline {
        return Ok((value, 0));
    }
    match next {
        Some(next) if !next.starts_with('-') || next.parse::<f64>().is_ok() => Ok((next.clone(), 1)),
        _ => Err(error(usage, prog, &format!("argument {}: expected one argument", spec_label(spec)))),
    }
}

/// A flag takes no `=value`.
fn flag(spec: &Spec, inline: Option<&str>, usage: &str, prog: &str) -> Result<(), Exit> {
    match inline {
        Some(value) => {
            Err(error(usage, prog, &format!("argument {}: ignored explicit argument {}", spec.name, py_repr(value))))
        }
        None => Ok(()),
    }
}

fn spec_label(spec: &Spec) -> &'static str {
    spec.name
}

fn py_repr(s: &str) -> String {
    recog_core::py::repr(s)
}

fn parse_live(args: &[String], mic_default: &str) -> Result<Command, Exit> {
    let specs = [
        Spec { name: "--mic-device", takes_value: true },
        Spec { name: "--system-audio", takes_value: true },
        Spec { name: "--system-device", takes_value: true },
        Spec { name: "--chunk-duration", takes_value: true },
        Spec { name: "--output", takes_value: true },
        Spec { name: "--list-devices", takes_value: false },
        Spec { name: "--ktalk", takes_value: false },
        Spec { name: "--ktalk-only", takes_value: false },
        Spec { name: "--zoom-app", takes_value: false },
        Spec { name: "--screencapture", takes_value: false },
        Spec { name: "--mic-speaker-name", takes_value: true },
        Spec { name: "--no-save-audio", takes_value: false },
    ];
    let (options, positionals, help) = split(args, &specs, LIVE_USAGE, "recog live")?;
    if help {
        return Err(Exit { stdout: live_help(mic_default), stderr: String::new(), code: 0 });
    }
    if !positionals.is_empty() {
        return Err(error(MAIN_USAGE, "recog", &format!("unrecognized arguments: {}", positionals.join(" "))));
    }
    let mut live = LiveArgs::default();
    for (name, value) in options {
        match value {
            Some(value) => set_live_value(&mut live, name, value)?,
            None => set_live_flag(&mut live, name),
        }
    }
    Ok(Command::Live(live))
}

/// An option of `live` that takes a value.
fn set_live_value(live: &mut LiveArgs, name: &str, value: String) -> Result<(), Exit> {
    match name {
        "--mic-device" => live.mic_device = value,
        "--system-audio" => live.system_audio = Some(value),
        "--system-device" => live.system_device = Some(value),
        "--chunk-duration" => live.chunk_duration = float_value(&value, "--chunk-duration", LIVE_USAGE, "recog live")?,
        "--output" => live.output = Some(value),
        "--mic-speaker-name" => live.mic_speaker_name = Some(value),
        _ => {}
    }
    Ok(())
}

/// A flag of `live`.
fn set_live_flag(live: &mut LiveArgs, name: &str) {
    match name {
        "--list-devices" => live.list_devices = true,
        "--ktalk" => live.ktalk = true,
        "--ktalk-only" => live.ktalk_only = true,
        "--zoom-app" => live.zoom_app = true,
        "--screencapture" => live.screencapture = true,
        "--no-save-audio" => live.no_save_audio = true,
        _ => {}
    }
}

/// A float option's value as argparse's `type=float` reads it.
fn float_value(value: &str, option: &str, usage: &str, prog: &str) -> Result<f64, Exit> {
    recog_core::py::parse_float(value)
        .map_err(|_| error(usage, prog, &format!("argument {option}: invalid float value: {}", py_repr(value))))
}

fn config_help() -> String {
    format!(
        "{CONFIG_USAGE}

positional arguments:
  {{data-dir,config-path,conferences,install-runtime,copilot-launch}}
                        data-dir — каталог данных; config-path — путь
                        config.toml; conferences — хосты конференций в JSON;
                        install-runtime — [install] runtime; copilot-launch —
                        модель и effort Claude для Copilot

options:
  -h, --help            show this help message and exit
"
    )
}

fn plain_help(usage: &str, description: &str) -> String {
    format!("{usage}\n\n{description}\n\noptions:\n  -h, --help            show this help message and exit\n")
}

/// A command without arguments: `--help`, or the command.
fn parse_bare(args: &[String], usage: &str, description: &str, command: Command) -> Result<Command, Exit> {
    match args.first().map(String::as_str) {
        None => Ok(command),
        Some("-h" | "--help") => Err(Exit { stdout: plain_help(usage, description), stderr: String::new(), code: 0 }),
        Some(_) => Err(error(MAIN_USAGE, "recog", &format!("unrecognized arguments: {}", args.join(" ")))),
    }
}

fn parse_config(args: &[String]) -> Result<Command, Exit> {
    let choices = CONFIG_QUERIES.iter().map(|(name, _)| format!("'{name}'")).collect::<Vec<_>>().join(", ");
    match args {
        [] => Err(error(CONFIG_USAGE, "recog config", "the following arguments are required: query")),
        [flag, ..] if flag == "-h" || flag == "--help" => {
            Err(Exit { stdout: config_help(), stderr: String::new(), code: 0 })
        }
        [name] => {
            CONFIG_QUERIES.iter().find(|(choice, _)| choice == name).map(|(_, q)| Command::Config(*q)).ok_or_else(
                || {
                    error(
                        CONFIG_USAGE,
                        "recog config",
                        &format!("argument query: invalid choice: {} (choose from {choices})", py_repr(name)),
                    )
                },
            )
        }
        [_, rest @ ..] => Err(error(MAIN_USAGE, "recog", &format!("unrecognized arguments: {}", rest.join(" ")))),
    }
}

fn parse_meetings(args: &[String]) -> Result<Command, Exit> {
    let specs = [Spec { name: "--json", takes_value: false }, Spec { name: "--refresh-cache", takes_value: false }];
    let (options, positionals, help) = split(args, &specs, MEETINGS_USAGE, "recog meetings")?;
    if help {
        return Err(Exit { stdout: meetings_help(), stderr: String::new(), code: 0 });
    }
    if positionals.len() > 1 {
        return Err(error(MAIN_USAGE, "recog", &format!("unrecognized arguments: {}", positionals[1..].join(" "))));
    }
    let mut meetings = MeetingsArgs { meeting_id: positionals.first().cloned(), ..MeetingsArgs::default() };
    for (name, _) in options {
        match name {
            "--json" => meetings.json = true,
            "--refresh-cache" => meetings.refresh_cache = true,
            _ => {}
        }
    }
    Ok(Command::Meetings(meetings))
}

fn parse_replay(args: &[String]) -> Result<Command, Exit> {
    let specs = [Spec { name: "--profile", takes_value: true }, Spec { name: "--tail", takes_value: true }];
    let (options, positionals, help) = split(args, &specs, REPLAY_USAGE, "recog replay")?;
    if help {
        return Err(Exit { stdout: replay_help(), stderr: String::new(), code: 0 });
    }
    let (session, out) = replay_positionals(&positionals)?;
    let mut replay = ReplayArgs { session, out, profile: None, tail: crate::live::replay::TAIL_S };
    for (name, value) in options {
        let value = value.unwrap_or_default();
        match name {
            "--profile" => replay.profile = Some(value),
            "--tail" => replay.tail = float_value(&value, "--tail", REPLAY_USAGE, "recog replay")?,
            _ => {}
        }
    }
    Ok(Command::Replay(replay))
}

/// `session` and `out`, or what argparse says of missing or extra positionals.
fn replay_positionals(positionals: &[String]) -> Result<(String, String), Exit> {
    let [session, out] = positionals else {
        let missing = if positionals.is_empty() { "session, out" } else { "out" };
        if positionals.len() > 2 {
            return Err(error(MAIN_USAGE, "recog", &format!("unrecognized arguments: {}", positionals[2..].join(" "))));
        }
        return Err(error(REPLAY_USAGE, "recog replay", &format!("the following arguments are required: {missing}")));
    };
    Ok((session.clone(), out.clone()))
}

/// Parses `argv` without the program name; `mic_default` is `MIC_SPEAKER_NAME` for the
/// help text.
///
/// # Errors
/// `--help`, a usage error, or an unknown command: what to print and the exit code.
pub fn parse(argv: &[String], mic_default: &str) -> Result<Command, Exit> {
    let Some(first) = argv.first() else { return Ok(Command::None) };
    let rest = &argv[1..];
    match first.as_str() {
        "-h" | "--help" => Err(Exit { stdout: main_help(), stderr: String::new(), code: 0 }),
        "live" => parse_live(rest, mic_default),
        "meetings" => parse_meetings(rest),
        "replay" => parse_replay(rest),
        "desktop" => Ok(Command::Desktop(rest.to_vec())),
        "service" => match rest {
            [action, dir] if action == "render" && !dir.starts_with('-') => Ok(Command::ServiceRender(dir.clone())),
            [action, flag] if action == "render" && matches!(flag.as_str(), "-h" | "--help") => {
                Err(Exit { stdout: "usage: recog service render DIR\n".into(), stderr: String::new(), code: 0 })
            }
            [flag] if matches!(flag.as_str(), "-h" | "--help") => {
                Err(Exit { stdout: "usage: recog service render DIR\n".into(), stderr: String::new(), code: 0 })
            }
            _ => Err(error("usage: recog service render DIR", "recog service", "expected render DIR")),
        },
        "config" => parse_config(rest),
        "check" | "launcher" | "prefetch" => parse_helper(first, rest),
        other => Err(unknown_command(other)),
    }
}

/// An installation helper without arguments.
fn parse_helper(name: &str, rest: &[String]) -> Result<Command, Exit> {
    match name {
        "check" => parse_bare(rest, CHECK_USAGE, "Проверить настройки установки", Command::Check),
        "launcher" => parse_bare(rest, LAUNCHER_USAGE, "Собрать Recog.app (macOS)", Command::Launcher),
        _ => parse_bare(rest, PREFETCH_USAGE, "Скачать модель ASR", Command::Prefetch),
    }
}

fn unknown_command(other: &str) -> Exit {
    if other.starts_with('-') {
        return error(MAIN_USAGE, "recog", &format!("unrecognized arguments: {other}"));
    }
    error(
        MAIN_USAGE,
        "recog",
        &format!(
            "argument command: invalid choice: {} (choose from 'live', 'meetings', 'replay', 'desktop', \
             'check', 'config', 'launcher', 'prefetch', 'service')",
            py_repr(other)
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    fn live(text: &str) -> LiveArgs {
        match parse(&args(text), "Me") {
            Ok(Command::Live(live)) => live,
            other => panic!("not live: {other:?}"),
        }
    }

    #[test]
    fn live_without_flags_parse_gives_the_defaults() {
        assert_eq!(live("live"), LiveArgs::default());
    }

    #[test]
    fn every_live_flag_parse_reads_it() {
        let parsed = live(
            "live --mic-device 3 --system-audio pulse --chunk-duration 2.5 --output t.md --list-devices \
             --ktalk --ktalk-only --zoom-app --mic-speaker-name Вася --no-save-audio",
        );
        assert_eq!(parsed.mic_device, "3");
        assert_eq!(parsed.system_audio.as_deref(), Some("pulse"));
        assert!((parsed.chunk_duration - 2.5).abs() < 1e-12);
        assert_eq!(parsed.output.as_deref(), Some("t.md"));
        assert!(parsed.list_devices && parsed.ktalk && parsed.ktalk_only && parsed.zoom_app && parsed.no_save_audio);
        assert_eq!(parsed.mic_speaker_name.as_deref(), Some("Вася"));
    }

    #[test]
    fn hidden_aliases_parse_accepts_them() {
        let parsed = live("live --screencapture --system-device 5");
        assert!(parsed.screencapture);
        assert_eq!(parsed.system_device.as_deref(), Some("5"));
    }

    #[test]
    fn an_equals_sign_or_a_prefix_parse_reads_them_as_argparse_does() {
        assert_eq!(live("live --system-audio=device:2").system_audio.as_deref(), Some("device:2"));
        assert!(live("live --no-save").no_save_audio);
        assert_eq!(live("live --mic-dev none").mic_device, "none");
    }

    #[test]
    fn an_ambiguous_prefix_parse_fails_with_exit_2() {
        let err = parse(&args("live --ktal"), "Me").unwrap_err();
        assert_eq!(err.code, 2);
        assert!(err.stderr.contains("ambiguous option: --ktal could match --ktalk, --ktalk-only"), "{}", err.stderr);
    }

    #[test]
    fn an_unknown_option_parse_fails_like_argparse() {
        let err = parse(&args("live --nope"), "Me").unwrap_err();
        assert_eq!(err.code, 2);
        assert!(err.stderr.ends_with("recog: error: unrecognized arguments: --nope\n"), "{}", err.stderr);
    }

    #[test]
    fn a_bad_float_parse_names_the_argument() {
        let err = parse(&args("live --chunk-duration x"), "Me").unwrap_err();
        assert!(err.stderr.contains("argument --chunk-duration: invalid float value: 'x'"));
    }

    #[test]
    fn a_missing_value_parse_says_it_expects_one() {
        let err = parse(&args("live --output"), "Me").unwrap_err();
        assert!(err.stderr.contains("argument --output: expected one argument"));
    }

    #[test]
    fn help_parse_prints_the_flags_with_the_default_name() {
        let exit = parse(&args("live --help"), "Тест").unwrap_err();
        assert_eq!(exit.code, 0);
        assert!(exit.stdout.contains("--system-audio BACKEND"));
        assert!(exit.stdout.contains("затем Тест)"));
        assert!(!exit.stdout.contains("--screencapture"));
        assert!(!exit.stdout.contains("--system-device"));
    }

    #[test]
    fn meetings_parse_reads_the_id_and_flags() {
        assert_eq!(
            parse(&args("meetings 2026-01-01_x --json --refresh-cache"), "Me").unwrap(),
            Command::Meetings(MeetingsArgs {
                meeting_id: Some("2026-01-01_x".into()),
                json: true,
                refresh_cache: true
            })
        );
        assert_eq!(parse(&args("meetings"), "Me").unwrap(), Command::Meetings(MeetingsArgs::default()));
    }

    #[test]
    fn no_command_or_an_unknown_one_parse_tells_them_apart() {
        assert_eq!(parse(&[], "Me").unwrap(), Command::None);
        let err = parse(&args("bogus"), "Me").unwrap_err();
        assert_eq!(err.code, 2);
        assert!(err.stderr.contains("invalid choice: 'bogus'"));
    }

    #[test]
    fn replay_parse_needs_a_session_and_an_out() {
        let Command::Replay(r) = parse(&args("replay s o --tail 3"), "Me").unwrap() else { panic!() };
        assert_eq!((r.session.as_str(), r.out.as_str()), ("s", "o"));
        assert!((r.tail - 3.0).abs() < 1e-12);
        let err = parse(&args("replay s"), "Me").unwrap_err();
        assert!(err.stderr.contains("the following arguments are required: out"));
    }

    #[test]
    fn a_negative_number_as_a_value_parse_takes_it() {
        assert!((live("live --chunk-duration -1").chunk_duration + 1.0).abs() < 1e-12);
    }

    #[test]
    fn installation_helpers_when_parsing_then_each_names_its_command() {
        // given / when / then
        assert_eq!(parse(&args("check"), "Me").unwrap(), Command::Check);
        assert_eq!(parse(&args("launcher"), "Me").unwrap(), Command::Launcher);
        assert_eq!(parse(&args("prefetch"), "Me").unwrap(), Command::Prefetch);
        for (name, query) in CONFIG_QUERIES {
            assert_eq!(parse(&args(&format!("config {name}")), "Me").unwrap(), Command::Config(query));
        }
        assert_eq!(
            parse(&args("desktop status --json"), "Me").unwrap(),
            Command::Desktop(vec!["status".into(), "--json".into()])
        );
    }

    #[test]
    fn installation_helpers_with_wrong_arguments_when_parsing_then_usage_errors() {
        // given / when / then
        assert!(
            parse(&args("config"), "Me").unwrap_err().stderr.contains("the following arguments are required: query")
        );
        let unknown = parse(&args("config bogus"), "Me").unwrap_err();
        assert_eq!(unknown.code, 2);
        assert!(unknown.stderr.contains("invalid choice: 'bogus'"));
        assert!(parse(&args("check now"), "Me").unwrap_err().stderr.contains("unrecognized arguments: now"));
        assert!(parse(&args("config data-dir x"), "Me").unwrap_err().stderr.contains("unrecognized arguments: x"));
        assert_eq!(parse(&args("prefetch --help"), "Me").unwrap_err().code, 0);
        assert!(parse(&args("config -h"), "Me").unwrap_err().stdout.contains("install-runtime"));
    }

    #[test]
    fn a_double_dash_or_a_lone_dash_when_parsing_then_positionals_live_does_not_take() {
        // given / when
        let after_dashes = parse(&args("live -- --ktalk"), "Me").unwrap_err();
        let dash = parse(&args("live -"), "Me").unwrap_err();
        // then
        assert_eq!(after_dashes.stderr, format!("{MAIN_USAGE}\nrecog: error: unrecognized arguments: --ktalk\n"));
        assert_eq!(dash.stderr, format!("{MAIN_USAGE}\nrecog: error: unrecognized arguments: -\n"));
    }

    #[test]
    fn a_flag_given_a_value_when_parsing_then_argparse_says_it_ignores_it() {
        // given / when
        let err = parse(&args("live --ktalk=yes"), "Me").unwrap_err();
        // then
        assert_eq!((err.code, err.stdout.as_str()), (2, ""));
        assert_eq!(
            err.stderr,
            format!("{LIVE_USAGE}\nrecog live: error: argument --ktalk: ignored explicit argument 'yes'\n")
        );
    }

    #[test]
    fn a_value_option_given_with_an_equals_sign_when_parsing_then_the_whole_value_after_it() {
        // given / when
        let parsed = live("live --mic-speaker-name=a=b --chunk-duration=1.5");
        // then
        assert_eq!(parsed.mic_speaker_name.as_deref(), Some("a=b"));
        assert!((parsed.chunk_duration - 1.5).abs() < 1e-12);
    }

    #[test]
    fn the_main_help_or_an_unknown_top_level_option_when_parsing_then_help_or_usage_error() {
        // given / when
        let help = parse(&args("--help"), "Me").unwrap_err();
        let option = parse(&args("--bogus live"), "Me").unwrap_err();
        // then
        assert_eq!((help.code, help.stdout), (0, main_help()));
        assert_eq!(
            (option.code, option.stderr),
            (2, format!("{MAIN_USAGE}\nrecog: error: unrecognized arguments: --bogus\n"))
        );
    }

    #[test]
    fn meetings_with_help_or_two_ids_when_parsing_then_help_or_the_extra_id_is_refused() {
        // given / when
        let help = parse(&args("meetings -h"), "Me").unwrap_err();
        let two = parse(&args("meetings a b c"), "Me").unwrap_err();
        // then
        assert_eq!((help.code, help.stdout), (0, meetings_help()));
        assert_eq!(two.stderr, format!("{MAIN_USAGE}\nrecog: error: unrecognized arguments: b c\n"));
    }

    #[test]
    fn replay_with_its_options_or_wrong_positionals_when_parsing_then_read_or_refused() {
        // given / when
        let Command::Replay(read) = parse(&args("replay --profile p.json s o"), "Me").unwrap() else { panic!() };
        let help = parse(&args("replay --help"), "Me").unwrap_err();
        let none = parse(&args("replay"), "Me").unwrap_err();
        let three = parse(&args("replay s o x"), "Me").unwrap_err();
        let tail = parse(&args("replay s o --tail soon"), "Me").unwrap_err();
        // then
        assert_eq!(
            read,
            ReplayArgs {
                session: "s".into(),
                out: "o".into(),
                profile: Some("p.json".into()),
                tail: crate::live::replay::TAIL_S
            }
        );
        assert_eq!((help.code, help.stdout), (0, replay_help()));
        assert_eq!(
            none.stderr,
            format!("{REPLAY_USAGE}\nrecog replay: error: the following arguments are required: session, out\n")
        );
        assert_eq!(three.stderr, format!("{MAIN_USAGE}\nrecog: error: unrecognized arguments: x\n"));
        assert_eq!(
            tail.stderr,
            format!("{REPLAY_USAGE}\nrecog replay: error: argument --tail: invalid float value: 'soon'\n")
        );
    }
}
