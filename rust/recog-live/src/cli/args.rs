//! The command line of `recog/cli.py` (argparse): `live` with every flag and the hidden
//! aliases, `meetings`, and the Rust-only `replay`. Options take `--name value` or
//! `--name=value` and, as argparse allows, any unambiguous prefix of their name.

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

/// What to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Live(LiveArgs),
    Meetings(MeetingsArgs),
    Replay(ReplayArgs),
    /// The KTalk desktop subcommands, which stay in Python.
    Desktop,
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

pub const MAIN_USAGE: &str = "usage: recog [-h] {live,meetings,replay,desktop} ...";

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
  {{live,meetings,replay,desktop}}
                        Доступные команды
    live                Live транскрипция с микрофона и/или системного звука
    meetings            Просмотр сохранённых митингов
    replay              Воспроизвести golden-сессию в live-процессе
    desktop             KTalk desktop (python -m recog desktop)

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
        if only_positionals || !arg.starts_with('-') || arg == "-" {
            positionals.push(arg.clone());
            continue;
        }
        if arg == "--" {
            only_positionals = true;
            continue;
        }
        if arg == "-h" || arg == "--help" {
            help = true;
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value.to_owned())),
            _ => (arg.as_str(), None),
        };
        match find_option(specs, name) {
            Ok(spec) if spec.takes_value => {
                let value = match inline {
                    Some(v) => v,
                    None => match args.get(i) {
                        Some(next) if !next.starts_with('-') || next.parse::<f64>().is_ok() => {
                            i += 1;
                            next.clone()
                        }
                        _ => {
                            return Err(error(
                                usage,
                                prog,
                                &format!("argument {}: expected one argument", spec_label(spec)),
                            ));
                        }
                    },
                };
                options.push((spec.name, Some(value)));
            }
            Ok(spec) => {
                if inline.is_some() {
                    return Err(error(
                        usage,
                        prog,
                        &format!(
                            "argument {}: ignored explicit argument {}",
                            spec.name,
                            py_repr(inline.as_deref().unwrap_or(""))
                        ),
                    ));
                }
                options.push((spec.name, None));
            }
            Err(candidates) if candidates.len() > 1 => {
                let mut message = format!("ambiguous option: {name} could match ");
                let _ = write!(message, "{}", candidates.join(", "));
                return Err(error(usage, prog, &message));
            }
            Err(_) => unrecognized.push(arg.clone()),
        }
    }
    if !unrecognized.is_empty() {
        return Err(error(MAIN_USAGE, "recog", &format!("unrecognized arguments: {}", unrecognized.join(" "))));
    }
    Ok((options, positionals, help))
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
        let value = value.unwrap_or_default();
        match name {
            "--mic-device" => live.mic_device = value,
            "--system-audio" => live.system_audio = Some(value),
            "--system-device" => live.system_device = Some(value),
            "--chunk-duration" => {
                live.chunk_duration = recog_core::py::parse_float(&value).map_err(|_| {
                    error(
                        LIVE_USAGE,
                        "recog live",
                        &format!("argument --chunk-duration: invalid float value: {}", py_repr(&value)),
                    )
                })?;
            }
            "--output" => live.output = Some(value),
            "--list-devices" => live.list_devices = true,
            "--ktalk" => live.ktalk = true,
            "--ktalk-only" => live.ktalk_only = true,
            "--zoom-app" => live.zoom_app = true,
            "--screencapture" => live.screencapture = true,
            "--mic-speaker-name" => live.mic_speaker_name = Some(value),
            "--no-save-audio" => live.no_save_audio = true,
            _ => {}
        }
    }
    Ok(Command::Live(live))
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
    let [session, out] = positionals.as_slice() else {
        let missing = if positionals.is_empty() { "session, out" } else { "out" };
        if positionals.len() > 2 {
            return Err(error(MAIN_USAGE, "recog", &format!("unrecognized arguments: {}", positionals[2..].join(" "))));
        }
        return Err(error(REPLAY_USAGE, "recog replay", &format!("the following arguments are required: {missing}")));
    };
    let mut replay =
        ReplayArgs { session: session.clone(), out: out.clone(), profile: None, tail: crate::live::replay::TAIL_S };
    for (name, value) in options {
        let value = value.unwrap_or_default();
        match name {
            "--profile" => replay.profile = Some(value),
            "--tail" => {
                replay.tail = recog_core::py::parse_float(&value).map_err(|_| {
                    error(
                        REPLAY_USAGE,
                        "recog replay",
                        &format!("argument --tail: invalid float value: {}", py_repr(&value)),
                    )
                })?;
            }
            _ => {}
        }
    }
    Ok(Command::Replay(replay))
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
        "desktop" => Ok(Command::Desktop),
        other if other.starts_with('-') => Err(error(MAIN_USAGE, "recog", &format!("unrecognized arguments: {other}"))),
        other => Err(error(
            MAIN_USAGE,
            "recog",
            &format!(
                "argument command: invalid choice: {} (choose from 'live', 'meetings', 'replay', 'desktop')",
                py_repr(other)
            ),
        )),
    }
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
}
