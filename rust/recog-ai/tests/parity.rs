//! Differential tests: the Python modules (through tests/parity/oracle.py) and this crate
//! on the same input, compared byte for byte or as JSON values.
//!
//! The interpreter is `RECOG_AI_PARITY_PYTHON`, else the repository's
//! `venv-gigaam/bin/python`, else `python3` (the modules under test need only the
//! standard library). Every run gets a temporary HOME, data directory and stub `claude` /
//! `codex` executables first on PATH that record their argv, environment, stdin and
//! working directory: no real CLI, model, `~/.recog` or user setting is involved.

mod common;

use std::ffi::OsString;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use recog_ai::AiError;
use recog_ai::defaults::{DEFAULT_MODELS, DEFAULT_TASKS};
use recog_ai::increment::IncrementGenerator;
use recog_ai::install::{LegacyEnv, default_prompts_dir};
use recog_ai::json::{self, Json, Map, object};
use recog_ai::meeting_chat::{self, ChatTurn};
use recog_ai::prompt_registry::{PromptConfig, PromptRegistry, parse_frontmatter};
use recog_ai::provider::{
    CliEnv, CliProviderFactory, build_codex_exec_command, create_provider, mark_ccs_automation, provider_for_settings,
};
use recog_ai::pytext;
use recog_ai::settings::{AiSettingsStore, Fallback, Variables, render_prompt, task_values, validate_models};
use recog_ai::title;

const ORACLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/parity/oracle.py");
const STUB: &str = include_str!("parity/stub_cli.py");

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn python() -> PathBuf {
    if let Some(path) = std::env::var_os("RECOG_AI_PARITY_PYTHON").filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    let venv = repo_root().join("venv-gigaam/bin/python");
    if venv.exists() {
        return venv;
    }
    PathBuf::from("python3")
}

/// A temporary world for one comparison: HOME, stub CLIs, the environment both sides get.
struct Harness {
    dir: tempfile::TempDir,
    bin: PathBuf,
    vars: Vec<(String, String)>,
}

impl Harness {
    fn new() -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let interpreter = which_python();
        for name in ["claude", "codex"] {
            let path = bin.join(name);
            std::fs::write(&path, format!("#!{} -I\n{STUB}", interpreter.display())).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let vars = vec![
            ("PATH".to_owned(), format!("{}:/usr/bin:/bin", bin.display())),
            ("HOME".to_owned(), home.display().to_string()),
            // LC_ALL keeps Python from coercing the locale into LC_CTYPE (PEP 538), and a
            // preset __CF_USER_TEXT_ENCODING from macOS adding it: neither is ours.
            ("LC_ALL".to_owned(), "en_US.UTF-8".to_owned()),
            ("LANG".to_owned(), "en_US.UTF-8".to_owned()),
            ("__CF_USER_TEXT_ENCODING".to_owned(), "0x1F5:0x0:0x2".to_owned()),
            ("RECOG_CONFIG_FILE".to_owned(), dir.path().join("no-config.toml").display().to_string()),
            ("RECOG_DATA_DIR".to_owned(), dir.path().join("no-data").display().to_string()),
        ];
        let harness = Harness { dir, bin, vars };
        harness.answer("claude", "answer\n", "", 0);
        harness.answer("codex", &codex_answer("answer"), "", 0);
        harness
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.dir.path().join(relative)
    }

    fn set(&mut self, key: &str, value: &str) {
        self.vars.retain(|(name, _)| name != key);
        self.vars.push((key.to_owned(), value.to_owned()));
    }

    fn cli_env(&self) -> CliEnv {
        CliEnv::from_pairs(self.vars.iter().cloned())
    }

    fn legacy(&self) -> LegacyEnv {
        LegacyEnv::from_vars(self.vars.iter().cloned())
    }

    fn factory(&self) -> CliProviderFactory {
        CliProviderFactory { env: Some(self.cli_env()) }
    }

    fn answer(&self, name: &str, stdout: &str, stderr: &str, code: i32) {
        let answer =
            object([("stdout", Json::from(stdout)), ("stderr", Json::from(stderr)), ("code", Json::Int(code.into()))]);
        std::fs::write(self.bin.join(format!("{name}.answer.json")), answer.dumps(None)).unwrap();
    }

    fn oracle(&self, request: Json) -> Json {
        let mut child = Command::new(python())
            .arg("-I")
            .arg("-B")
            .arg(ORACLE)
            .arg(repo_root())
            .env_clear()
            .envs(self.vars.iter().cloned())
            .current_dir(std::env::current_dir().unwrap())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("parity interpreter (RECOG_AI_PARITY_PYTHON)");
        child.stdin.take().unwrap().write_all(request.dumps(None).as_bytes()).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "oracle failed: {}", String::from_utf8_lossy(&output.stderr));
        Json::parse(std::str::from_utf8(&output.stdout).unwrap()).unwrap()
    }

    /// The stub calls since the last take, in a stable order, removed.
    fn take_calls(&self) -> Vec<Json> {
        let dir = self.bin.join("calls");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut calls: Vec<Json> = entries
            .map(|entry| {
                let path = entry.unwrap().path();
                let call = Json::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
                std::fs::remove_file(path).unwrap();
                call
            })
            .collect();
        calls.sort_by_key(|call| call.dumps(None));
        calls
    }
}

fn which_python() -> PathBuf {
    let python = python();
    if python.is_absolute() {
        return python;
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join(&python))
        .find(|candidate| candidate.exists())
        .expect("python3 on PATH")
}

fn codex_answer(text: &str) -> String {
    let message = object([
        ("type", Json::from("item.completed")),
        ("item", object([("type", Json::from("agent_message")), ("text", Json::from(text))])),
    ]);
    format!("{}\n{{\"type\":\"turn.completed\"}}\n", message.dumps(None))
}

fn text(value: &str) -> Json {
    Json::from(value)
}

fn path_json(path: &Path) -> Json {
    Json::from(path.display().to_string())
}

/// The Python class of an error, as the oracle reports it.
fn py_class(error: &AiError) -> &'static str {
    match error {
        AiError::Value(_) => "ValueError",
        AiError::Conflict(_) => "SettingsConflictError",
        AiError::SettingsRead(_) => "SettingsReadError",
        AiError::NotFound(_) => "FileNotFoundError",
        AiError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => "FileNotFoundError",
        AiError::Io(_) => "OSError",
        AiError::Runtime(_) => "RuntimeError",
        AiError::Key(_) => "KeyError",
        AiError::Type(message) if message.contains("has no attribute") => "AttributeError",
        AiError::Type(_) => "TypeError",
        AiError::Provider(_) => "Exception",
        AiError::Timeout => "TimeoutError",
    }
}

fn outcome(result: recog_ai::Result<Json>) -> Json {
    match result {
        Ok(value) => object([("ok", value)]),
        Err(error) => object([("error", text(py_class(&error))), ("message", Json::from(error.to_string()))]),
    }
}

/// Every canonical UUID replaced by `<rev-N>`, numbered by first appearance.
fn normalize_revisions(text: &str) -> String {
    let mut seen: Vec<String> = Vec::new();
    let mut out = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        let candidate = rest.get(..36).filter(|window| {
            window.bytes().enumerate().all(|(index, byte)| match index {
                8 | 13 | 18 | 23 => byte == b'-',
                _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
            })
        });
        if let Some(uuid) = candidate {
            let number = seen.iter().position(|known| known == uuid).unwrap_or_else(|| {
                seen.push(uuid.to_owned());
                seen.len() - 1
            });
            out.push_str(&format!("<rev-{number}>"));
            rest = &rest[36..];
        } else {
            let width = rest.chars().next().unwrap().len_utf8();
            out.push_str(&rest[..width]);
            rest = &rest[width..];
        }
    }
    out
}

/// A deterministic generator for the randomized comparisons.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }

    fn text(&mut self, tokens: &[&str], max: usize) -> String {
        (0..self.below(max + 1)).map(|_| self.pick(tokens)).collect()
    }
}

// --- Defaults, JSON text, character classes ---

#[test]
fn shipped_defaults_given_both_implementations_then_identical() {
    let harness = Harness::new();
    let python = harness.oracle(object([("kind", text("defaults"))]));
    for task in DEFAULT_TASKS {
        let theirs = &python["tasks"][task.id];
        assert_eq!(theirs["label"], text(task.label), "{}", task.id);
        assert_eq!(theirs["prompt"], text(task.prompt), "{}", task.id);
        assert_eq!(theirs["multiple"], Json::Bool(task.multiple), "{}", task.id);
        let providers = task
            .providers
            .map_or(Json::Null, |providers| Json::from(providers.iter().map(|p| p.to_string()).collect::<Vec<_>>()));
        assert_eq!(theirs["providers"], providers, "{}", task.id);
    }
    assert_eq!(python["tasks"].as_object().unwrap().len(), DEFAULT_TASKS.len());
    let models = Json::Object(DEFAULT_MODELS.iter().map(|(p, m)| (p.to_string(), text(m))).collect());
    assert_eq!(python["models"].dumps(None), models.dumps(None));
}

#[test]
fn json_values_when_dumped_then_python_text() {
    let harness = Harness::new();
    let texts = [
        r#"{"a": [], "b": {}, "c": [1, -2, 3.5, 1e16, 1.5e-7, 0.1, 100.0, -0.0, 12345678901234567], "d": null}"#,
        r#"{"esc": "quote \" backslash \\ tab \t nl \n cr \r bs \b ff \f nul \u0000 us \u001f del \u007f", "uni": "Пётр 😀    "}"#,
        r#"[true, false, {"nested": {"deep": [[], [{}]]}}, "x", 0, 9007199254740993]"#,
        r#"{"dup": 1, "other": 2, "dup": 3}"#,
        r#""just a string""#,
        "[1.7976931348623157e308, 5e-324, 2.5, 1e22, 123456.789]",
    ];
    let mut cases = Vec::new();
    for value in texts {
        for indent in [Json::Null, Json::Int(2)] {
            cases.push(Json::Array(vec![text(value), indent]));
        }
    }
    let python = harness.oracle(object([("kind", text("dumps")), ("cases", Json::Array(cases.clone()))]));
    for (case, expected) in cases.iter().zip(python.as_array().unwrap()) {
        let indent = case.as_array().unwrap()[1].as_int().map(|indent| indent as usize);
        let ours = Json::parse(case.as_array().unwrap()[0].as_str().unwrap()).unwrap().dumps(indent);
        assert_eq!(ours, expected.as_str().unwrap(), "{case:?}");
    }
}

fn ranges(predicate: impl Fn(char) -> bool) -> Json {
    let mut found = Vec::new();
    let mut start: Option<u32> = None;
    for code in 0..=0x11_0000u32 {
        let inside = char::from_u32(code).is_some_and(&predicate);
        match (inside, start) {
            (true, None) => start = Some(code),
            (false, Some(first)) => {
                found.push(Json::Array(vec![Json::Int(first.into()), Json::Int((code - 1).into())]));
                start = None;
            }
            _ => {}
        }
    }
    Json::Array(found)
}

#[test]
fn code_point_classes_given_unicode_then_same_as_python() {
    let harness = Harness::new();
    let python = harness.oracle(object([("kind", text("classes"))]));
    assert_eq!(python["unidata"], text("15.0.0"), "the crate's tables are Unicode 15.0");
    assert_eq!(python["space"], ranges(pytext::is_space), "str.isspace");
    assert_eq!(python["alnum"], ranges(pytext::is_alnum), "str.isalnum");
    assert_eq!(python["printable"], ranges(pytext::is_printable), "str.isprintable");
    assert_eq!(python["other"], ranges(pytext::is_other), "category C");
    assert_eq!(python["word"], ranges(pytext::is_word), "\\w of the saved ID pattern");
}

// --- Prompt templates ---

fn fixture_variables() -> Variables {
    let transcript = [
        "[11:30:14] Peter: Я за синюю кнопку. {user_name} {\"json\": {\"x\": 1}} {{double}}",
        "[11:31:00] Мария: Понял — завтра {transcript}, до 18:00.",
        "[11:32:10] Иван: Ещё: \"кавычки\", 'апострофы', табуляция\tи \\ обратная черта.",
    ]
    .join("\n");
    [
        ("transcript", transcript.as_str()),
        ("user_name", "Мария"),
        ("participants", "Peter, Мария, Иван"),
        ("meeting_title", "Обзор {дизайна}"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect()
}

fn variables_json(variables: &Variables) -> Json {
    Json::Object(variables.iter().map(|(key, value)| (key.clone(), text(value))).collect())
}

fn render_outcome(template: &str, variables: &Variables) -> Json {
    outcome(render_prompt(template, variables).map(Json::from))
}

#[test]
fn prompt_files_and_default_prompts_when_rendered_on_fixture_transcript_then_byte_identical() {
    let harness = Harness::new();
    let variables = fixture_variables();
    let mut templates: Vec<String> =
        PromptRegistry::new(default_prompts_dir()).load_all().into_iter().map(|p| p.template).collect();
    assert!(!templates.is_empty(), "recog/prompts has prompts");
    templates.extend(DEFAULT_TASKS.iter().map(|task| task.prompt.to_owned()));
    let cases: Vec<Json> = templates
        .iter()
        .map(|template| object([("template", text(template)), ("variables", variables_json(&variables))]))
        .collect();
    let python = harness.oracle(object([("kind", text("render")), ("cases", Json::Array(cases))]));
    for (template, expected) in templates.iter().zip(python.as_array().unwrap()) {
        let ours = render_outcome(template, &variables);
        assert_eq!(ours.dumps(None), expected.dumps(None), "{template}");
        assert!(expected.get("ok").is_some(), "{expected:?}");
    }
}

#[test]
fn random_templates_when_rendered_then_same_text_or_same_error() {
    let harness = Harness::new();
    let tokens = [
        "{",
        "}",
        "{{",
        "}}",
        "transcript",
        "user_name",
        "participants",
        "meeting_title",
        "x",
        "_a1",
        "!r",
        ".attr",
        ":>10",
        "[0]",
        "\n",
        " ",
        "\"",
        "й",
        "{transcript}",
        "{user_name}",
        "{unknown}",
        "{1}",
        "{}",
        "😀",
    ];
    let mut rng = Rng(0x5eed_1234_abcd_ef01);
    let mut cases = Vec::new();
    let mut ours = Vec::new();
    for _ in 0..4000 {
        let template = rng.text(&tokens, 12);
        let mut variables = Variables::new();
        for name in ["transcript", "user_name", "participants", "meeting_title"] {
            if rng.below(3) > 0 {
                variables.insert(name.to_owned(), rng.text(&["{", "}", "a", "{user_name}", " "], 4));
            }
        }
        ours.push(render_outcome(&template, &variables));
        cases.push(object([("template", text(&template)), ("variables", variables_json(&variables))]));
    }
    let python = harness.oracle(object([("kind", text("render")), ("cases", Json::Array(cases.clone()))]));
    for ((case, expected), ours) in cases.iter().zip(python.as_array().unwrap()).zip(&ours) {
        assert_eq!(ours.dumps(None), expected.dumps(None), "{}", case.dumps(None));
    }
}

// --- Prompt registry ---

#[test]
fn frontmatter_texts_when_parsed_then_identical() {
    let harness = Harness::new();
    let mut texts: Vec<String> = [
        "---\nname: My increment\nfilter: null\n---\n\nPrompt body here.",
        "---\nname: \"Team sync\"\nfilter: \"Sync|синк\"\n---\n\nBody",
        "Just a plain prompt.",
        "---\na: null\nb: none\nc: ~\nd:\ne: NONE\nf: \"\"\ng: '\nh: 'x'\n---\n\nBody",
        "---\nname: x\n----\nbody",
        "---\nname: x\n---   \n\n  body  \n",
        "------\nname: x\n---\nbody",
        "---name: x\n---\nbody",
        "---\n---\n",
        "---",
        "---\nname: a: b\n  key :  value  \n:novalue\nnocolon\n---\nbody\n---\nmore",
        "---\nname: x\n--- \u{a0}x\n---\u{1c}\nbody",
    ]
    .iter()
    .map(|text| text.to_string())
    .collect();
    let mut rng = Rng(0x0fed_cba9_8765_4321);
    let tokens = ["---", "\n", " ", "name", ":", " null", "\"", "'", "x", "\t", "\u{a0}", "----", "~", "й", "\u{2028}"];
    for _ in 0..3000 {
        texts.push(rng.text(&tokens, 14));
    }
    let request = object([("kind", text("frontmatter")), ("texts", Json::from(texts.clone()))]);
    let python = harness.oracle(request);
    for (source, expected) in texts.iter().zip(python.as_array().unwrap()) {
        let (fields, body) = parse_frontmatter(source);
        let fields = Json::Object(fields.into_iter().map(|(key, value)| (key, Json::from(value))).collect());
        let ours = Json::Array(vec![fields, Json::from(body)]);
        assert_eq!(ours.dumps(None), expected.dumps(None), "{source:?}");
    }
}

fn config_json(config: &PromptConfig) -> Json {
    object([
        ("id", text(&config.id)),
        ("name", text(&config.name)),
        ("filter", Json::from(config.filter.clone())),
        ("template", text(&config.template)),
    ])
}

fn registry_view(dir: &Path, get: &[&str], titles: &[&str]) -> Json {
    let registry = PromptRegistry::new(dir);
    object([
        ("load_all", Json::Array(registry.load_all().iter().map(config_json).collect())),
        (
            "get",
            Json::Object(
                get.iter()
                    .map(|name| (name.to_string(), registry.get(name).as_ref().map_or(Json::Null, config_json)))
                    .collect(),
            ),
        ),
        (
            "matching",
            Json::Object(
                titles
                    .iter()
                    .map(|title| {
                        (
                            title.to_string(),
                            Json::from(registry.matching(title).into_iter().map(|p| p.id).collect::<Vec<_>>()),
                        )
                    })
                    .collect(),
            ),
        ),
    ])
}

#[test]
fn registry_directory_when_loaded_and_matched_then_identical() {
    let harness = Harness::new();
    let dir = harness.path("prompts");
    std::fs::create_dir_all(&dir).unwrap();
    let files: [(&str, &[u8]); 11] = [
        ("alpha.md", b"---\nname: Alpha\nfilter: null\n---\n\nAlpha {transcript}"),
        (".hidden.md", b"---\r\nname: Hidden\r\nfilter: 'retro'\r\n---\r\nHidden\r\nbody\r\n"),
        ("no-name.md", b"---\nfilter: null\n---\nNo name"),
        ("bad-regex.md", b"---\nname: Bad\nfilter: \"[invalid\"\n---\nBad"),
        ("upper.MD", b"---\nname: Upper\n---\nignored"),
        ("lookahead.md", b"---\nname: Look\nfilter: \"^(?!.*retro).*sync\"\n---\nLook"),
        (
            "cyrillic.md",
            b"---\nname: \xd0\xa1\xd0\xb8\xd0\xbd\xd0\xba\nfilter: \xd1\x81\xd0\xb8\xd0\xbd\xd0\xba\n---\nC",
        ),
        ("binary.md", b"---\nname: Bin\n---\n\xff\xfe"),
        ("plain.md", b"No frontmatter at all"),
        ("Zeta.md", b"---\nname: Zeta\nfilter: Daily|weekly\n---\nZ"),
        ("empty-name.md", b"---\nname: \"\"\n---\nE"),
    ];
    for (name, content) in files {
        std::fs::write(dir.join(name), content).unwrap();
    }
    std::fs::create_dir(dir.join("folder.md")).unwrap();
    let get = ["alpha", ".hidden", "no-name", "binary", "folder", "missing", "upper"];
    let titles = ["Daily sync", "sync retro", "Еженедельный СИНК", "", "Weekly"];
    let request = object([
        ("kind", text("registry")),
        ("dir", path_json(&dir)),
        ("get", Json::from(get.iter().map(|s| s.to_string()).collect::<Vec<_>>())),
        ("titles", Json::from(titles.iter().map(|s| s.to_string()).collect::<Vec<_>>())),
    ]);
    let python = harness.oracle(request);
    assert_eq!(registry_view(&dir, &get, &titles).dumps(Some(1)), python.dumps(Some(1)));

    let shipped = default_prompts_dir();
    let request = object([
        ("kind", text("registry")),
        ("dir", path_json(&shipped)),
        ("get", Json::from(vec!["my-increment".to_owned()])),
        ("titles", Json::from(vec!["Daily sync".to_owned()])),
    ]);
    assert_eq!(
        registry_view(&shipped, &["my-increment"], &["Daily sync"]).dumps(None),
        harness.oracle(request).dumps(None)
    );
}

// --- AI settings ---

#[test]
fn model_and_task_values_when_validated_then_same_outcomes() {
    let harness = Harness::new();
    let long = "x".repeat(161);
    let exact = "y".repeat(160);
    let models = [
        r#"{"codex": "gpt-5.4", "claude": "opus"}"#.to_owned(),
        r#"{"codex": " "}"#.to_owned(),
        r#"{"codex": ""}"#.to_owned(),
        r#"{"codex": null}"#.to_owned(),
        r#"{"codex": 5}"#.to_owned(),
        r#"{"codex": "two words"}"#.to_owned(),
        r#"{"codex": "--option"}"#.to_owned(),
        r#"{"codex": "a\u200bb"}"#.to_owned(),
        r#"{"codex": "a\u00adb"}"#.to_owned(),
        r#"{"codex": "a\u0378b"}"#.to_owned(),
        r#"{"codex": "a\ue000b"}"#.to_owned(),
        r#"{"codex": "a\u00a0b"}"#.to_owned(),
        r#"{"codex": "a\u001cb"}"#.to_owned(),
        r#"{"codex": "модель-ё"}"#.to_owned(),
        format!(r#"{{"codex": "{long}"}}"#),
        format!(r#"{{"codex": "{exact}"}}"#),
        r#"{"api_key": "secret", "openrouter": "x"}"#.to_owned(),
        r#"[]"#.to_owned(),
        r#""claude""#.to_owned(),
    ];
    let cases: Vec<Json> = models.iter().map(|text| Json::parse(text).unwrap()).collect();
    let python = harness.oracle(object([("kind", text("models")), ("cases", Json::Array(cases.clone()))]));
    for (case, expected) in cases.iter().zip(python.as_array().unwrap()) {
        let ours = outcome(validate_models(case).map(|models| json::string_map(&models)));
        assert_eq!(ours.dumps(None), expected.dumps(None), "{case:?}");
    }

    let long_prompt = format!("{}{{transcript}}", "x".repeat(31_988));
    let tasks: Vec<(&str, String)> = vec![
        ("analysis.increment", r#"{"unknown": true}"#.into()),
        ("analysis.increment", r#"{"prompt": ""}"#.into()),
        ("analysis.increment", r#"{"prompt": "   "}"#.into()),
        ("analysis.increment", r#"{"prompt": "No transcript"}"#.into()),
        ("analysis.increment", r#"{"prompt": "{transcript} {unknown}"}"#.into()),
        ("analysis.increment", r#"{"prompt": "{transcript!r}"}"#.into()),
        ("analysis.increment", r#"{"prompt": "{transcript.__class__}"}"#.into()),
        ("analysis.increment", r#"{"prompt": "a\u0000 {transcript}"}"#.into()),
        ("analysis.increment", format!(r#"{{"prompt": "{long_prompt}"}}"#)),
        ("analysis.increment", format!(r#"{{"prompt": "x{long_prompt}"}}"#)),
        ("analysis.increment", r#"{"providers": []}"#.into()),
        ("analysis.increment", r#"{"providers": ["claude", "claude"]}"#.into()),
        ("analysis.increment", r#"{"providers": ["codex", "claude"]}"#.into()),
        ("analysis.increment", r#"{"providers": "claude"}"#.into()),
        ("analysis.increment", r#"{"providers": [null]}"#.into()),
        ("analysis.increment", r#"{"models": {"codex": "  "}}"#.into()),
        ("analysis.increment", r#"{"reasoning_effort": "max"}"#.into()),
        ("analysis.increment", r#"{"reasoning_effort": ""}"#.into()),
        ("analysis.increment", r#"{"claude_effort": "max", "reasoning_effort": "low", "prompt": "{transcript}", "models": {"claude": "m"}, "providers": ["claude"]}"#.into()),
        ("analysis.increment", r#"{"enabled": true}"#.into()),
        ("title", r#"{"providers": ["claude", "codex"]}"#.into()),
        ("title", r#"{"providers": ["claude"]}"#.into()),
        ("copilot", r#"{"providers": ["codex"]}"#.into()),
        ("copilot", r#"{"models": {"codex": "model"}}"#.into()),
        ("copilot", r#"{"reasoning_effort": "high"}"#.into()),
        ("copilot", r#"{"reasoning_effort": "bogus"}"#.into()),
        ("copilot", r#"{"enabled": 1}"#.into()),
        ("copilot", r#"{"enabled": false, "prompt": "{unknown} {\"json\": true}"}"#.into()),
        ("copilot", r#"{"claude_effort": "--max"}"#.into()),
        ("missing", r#"{"prompt": "{transcript}"}"#.into()),
        ("saved:", r#"{}"#.into()),
        ("saved:ok-id.v2", r#"{"prompt": "{transcript}"}"#.into()),
        ("saved:Заметки_1", r#"{}"#.into()),
        ("saved:-dash", r#"{}"#.into()),
        ("saved:../outside", r#"{}"#.into()),
        ("saved:a b", r#"{}"#.into()),
        ("analysis.increment", r#"[]"#.into()),
    ];
    let cases: Vec<Json> = tasks
        .iter()
        .map(|(task_id, values)| object([("task_id", text(task_id)), ("values", Json::parse(values).unwrap())]))
        .collect();
    let python = harness.oracle(object([("kind", text("validate")), ("cases", Json::Array(cases.clone()))]));
    for ((task_id, values), expected) in tasks.iter().zip(python.as_array().unwrap()) {
        let ours = outcome(task_values(task_id, &Json::parse(values).unwrap()).map(Json::Object));
        assert_eq!(ours.dumps(None), expected.dumps(None), "{task_id} {values}");
    }
}

fn rust_settings_op(store: &AiSettingsStore, op: &Json) -> Json {
    let revision = match op["revision"].as_str() {
        Some("$current") => store.read().ok().and_then(|snapshot| snapshot["revision"].as_str().map(str::to_owned)),
        other => other.map(str::to_owned),
    };
    let revision = revision.as_deref();
    let task_id = op["task_id"].as_str().unwrap_or_default();
    outcome(match op["op"].as_str().unwrap() {
        "read" => store.read().map(Json::Object),
        "view" => store.view().map(|view| view.to_json()),
        "resolve" => {
            let fallback = Fallback { prompt: op["fallback_prompt"].as_str(), label: op["fallback_label"].as_str() };
            store.resolve_task(task_id, None, fallback).map(|task| task.to_json())
        }
        "write_models" => store.write_models(&op["models"], revision).map(|view| view.to_json()),
        "write_task" => store.write_task(task_id, &op["values"], revision).map(|view| view.to_json()),
        "reset_task" => store.reset_task(task_id, revision).map(|view| view.to_json()),
        other => panic!("unknown op {other}"),
    })
}

/// Runs the same settings operations in two data directories and compares every
/// result and the file left behind (revisions numbered by first appearance).
fn compare_settings(harness: &Harness, name: &str, prompts: &Path, initial: Option<&[u8]>, ops: &[&str]) {
    let ops: Vec<Json> = ops.iter().map(|op| Json::parse(op).unwrap()).collect();
    let (python_dir, rust_dir) = (harness.path(&format!("{name}/python")), harness.path(&format!("{name}/rust")));
    for dir in [&python_dir, &rust_dir] {
        std::fs::create_dir_all(dir).unwrap();
        if let Some(initial) = initial {
            std::fs::write(dir.join("ai-settings.json"), initial).unwrap();
        }
    }
    let request = object([
        ("kind", text("settings")),
        ("data_dir", path_json(&python_dir)),
        ("prompts_dir", path_json(prompts)),
        ("ops", Json::Array(ops.clone())),
    ]);
    let python = harness.oracle(request);
    let store = AiSettingsStore::new(&rust_dir, Some(prompts.to_path_buf())).with_legacy_env(harness.legacy());
    let ours = Json::Array(ops.iter().map(|op| rust_settings_op(&store, op)).collect());
    for (index, (theirs, ours)) in python.as_array().unwrap().iter().zip(ours.as_array().unwrap()).enumerate() {
        assert_eq!(
            normalize_revisions(&ours.dumps(Some(1))),
            normalize_revisions(&theirs.dumps(Some(1))),
            "{name}: op {index} {}",
            ops[index].dumps(None)
        );
    }
    let file = |dir: &Path| {
        std::fs::read(dir.join("ai-settings.json"))
            .ok()
            .map(|bytes| normalize_revisions(&String::from_utf8_lossy(&bytes)))
    };
    assert_eq!(file(&rust_dir), file(&python_dir), "{name}: ai-settings.json");
    let entries = |dir: &Path| {
        let mut names: Vec<String> =
            std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    };
    assert_eq!(entries(&rust_dir), entries(&python_dir), "{name}: files");
}

#[test]
fn settings_operation_sequences_when_applied_then_same_results_and_file_bytes() {
    let mut harness = Harness::new();
    let prompts = harness.path("prompts");
    std::fs::create_dir_all(&prompts).unwrap();
    std::fs::write(prompts.join("my-increment.md"), "---\nname: Мой инкремент\n---\nShared {transcript}").unwrap();
    std::fs::write(
        prompts.join("notes.md"),
        "---\nname: Notes\nfilter: sync\n---\nSummarize {transcript} for {user_name}",
    )
    .unwrap();
    std::fs::write(prompts.join("broken.md"), "---\nfilter: x\n---\nNo name").unwrap();

    compare_settings(
        &harness,
        "fresh",
        &prompts,
        None,
        &[
            r#"{"op": "read"}"#,
            r#"{"op": "view"}"#,
            r#"{"op": "resolve", "task_id": "copilot"}"#,
            r#"{"op": "resolve", "task_id": "saved:notes"}"#,
            r#"{"op": "resolve", "task_id": "saved:missing"}"#,
            r#"{"op": "resolve", "task_id": "saved:generated", "fallback_prompt": "Fallback {transcript}", "fallback_label": "Generated"}"#,
            r#"{"op": "resolve", "task_id": "saved:generated", "fallback_prompt": "no placeholder", "fallback_label": ""}"#,
            r#"{"op": "resolve", "task_id": "saved:../outside"}"#,
            r#"{"op": "write_models", "models": {"claude": "global-claude", "codex": "global-codex"}, "revision": "defaults"}"#,
            r#"{"op": "write_task", "task_id": "title", "values": {"models": {"codex": "title-model"}, "prompt": "Название {transcript} {\"json\": {}}"}, "revision": "$current"}"#,
            r#"{"op": "write_task", "task_id": "title", "values": {"providers": ["claude"]}, "revision": "defaults"}"#,
            r#"{"op": "write_task", "task_id": "saved:my-increment", "values": {"prompt": "Edited {transcript}", "providers": ["claude", "codex"], "claude_effort": "max"}}"#,
            r#"{"op": "resolve", "task_id": "saved:my-increment"}"#,
            r#"{"op": "resolve", "task_id": "analysis.increment"}"#,
            r#"{"op": "write_task", "task_id": "copilot", "values": {"enabled": false}}"#,
            r#"{"op": "write_task", "task_id": "copilot", "values": {"prompt": "Policy {literal}", "models": {"claude": "c"}}}"#,
            r#"{"op": "reset_task", "task_id": "copilot"}"#,
            r#"{"op": "write_task", "task_id": "saved:notes", "values": {"reasoning_effort": ""}}"#,
            r#"{"op": "write_models", "models": {"codex": ""}}"#,
            r#"{"op": "write_models", "models": {"openrouter": "x"}}"#,
            r#"{"op": "write_task", "task_id": "saved:missing", "values": {"prompt": "{transcript}"}}"#,
            r#"{"op": "write_task", "task_id": "title", "values": {"prompt": "no placeholder"}}"#,
            r#"{"op": "reset_task", "task_id": "analysis.increment"}"#,
            r#"{"op": "view"}"#,
        ],
    );

    let retired = r#"{"version": 1, "revision": "0f8fad5b-d9cb-469f-a165-70867728950e", "models": {"openrouter": "old", "codex": "kept"}, "tasks": {
        "anchor": {"prompt": "Removed {transcript}"}, "analysis.summary": {"prompt": "Retired {transcript}"},
        "title": {"providers": ["openrouter"], "models": {"openrouter": "old"}, "prompt": "Keep {transcript}"},
        "saved:my-increment": {"prompt": "Legacy {transcript}", "claude_effort": "max"}}}"#;
    compare_settings(
        &harness,
        "retired",
        &prompts,
        Some(retired.as_bytes()),
        &[
            r#"{"op": "read"}"#,
            r#"{"op": "view"}"#,
            r#"{"op": "resolve", "task_id": "analysis.summary"}"#,
            r#"{"op": "write_task", "task_id": "analysis.increment", "values": {"providers": ["codex"]}, "revision": "0f8fad5b-d9cb-469f-a165-70867728950e"}"#,
            r#"{"op": "write_models", "models": {"claude": "after"}, "revision": "0f8fad5b-d9cb-469f-a165-70867728950e"}"#,
            r#"{"op": "read"}"#,
        ],
    );

    for (name, bytes) in [
        ("not-json", b"not json".to_vec()),
        ("not-utf8", b"\xff".to_vec()),
        ("list", b"[]".to_vec()),
        ("empty-object", b"{}".to_vec()),
        ("bom", b"\xef\xbb\xbf{\"version\": 1, \"revision\": \"0f8fad5b-d9cb-469f-a165-70867728950e\", \"models\": {}, \"tasks\": {}}".to_vec()),
        ("utf16", "{\"version\": 1, \"revision\": \"0f8fad5b-d9cb-469f-a165-70867728950e\", \"models\": {}, \"tasks\": {}}".encode_utf16().flat_map(u16::to_le_bytes).collect()),
        ("uppercase-revision", br#"{"version": 1, "revision": "0F8FAD5B-D9CB-469F-A165-70867728950E", "models": {}, "tasks": {}}"#.to_vec()),
        ("float-version", br#"{"version": 1.0, "revision": "0f8fad5b-d9cb-469f-a165-70867728950e", "models": {}, "tasks": {}}"#.to_vec()),
        ("noncanonical-model", br#"{"version": 1, "revision": "0f8fad5b-d9cb-469f-a165-70867728950e", "models": {"codex": " "}, "tasks": {}}"#.to_vec()),
        ("noncanonical-task", br#"{"version": 1, "revision": "0f8fad5b-d9cb-469f-a165-70867728950e", "models": {}, "tasks": {"title": {"models": {}}}}"#.to_vec()),
        ("unknown-placeholder", br#"{"version": 1, "revision": "0f8fad5b-d9cb-469f-a165-70867728950e", "models": {}, "tasks": {"title": {"prompt": "{unknown}"}}}"#.to_vec()),
        ("secret", br#"{"version": 1, "revision": "0f8fad5b-d9cb-469f-a165-70867728950e", "models": {}, "tasks": {}, "secret": 1}"#.to_vec()),
        ("too-large", vec![b' '; 256 * 1024 + 1]),
    ] {
        compare_settings(&harness, name, &prompts, Some(&bytes), &[
            r#"{"op": "read"}"#,
            r#"{"op": "view"}"#,
            r#"{"op": "write_models", "models": {"codex": "new"}}"#,
            r#"{"op": "reset_task", "task_id": "title"}"#,
        ]);
    }

    harness.set("CLAUDE_MODEL", "legacy-claude");
    harness.set("CODEX_MODEL", "legacy-codex");
    harness.set("CODEX_REASONING_EFFORT", "high");
    compare_settings(
        &harness,
        "legacy",
        &prompts,
        None,
        &[
            r#"{"op": "view"}"#,
            r#"{"op": "write_models", "models": {"claude": "saved"}}"#,
            r#"{"op": "resolve", "task_id": "title"}"#,
            r#"{"op": "write_models", "models": {"claude": ""}}"#,
            r#"{"op": "resolve", "task_id": "analysis.increment"}"#,
        ],
    );
    harness.set("CODEX_MODEL", "invalid legacy value");
    harness.set("CODEX_REASONING_EFFORT", "turbo");
    compare_settings(
        &harness,
        "invalid-legacy",
        &prompts,
        None,
        &[
            r#"{"op": "view"}"#,
            r#"{"op": "resolve", "task_id": "title"}"#,
            r#"{"op": "write_models", "models": {"codex": "saved"}}"#,
            r#"{"op": "resolve", "task_id": "copilot"}"#,
        ],
    );
}

// --- Providers through stub CLIs ---

fn rust_provider_call(harness: &Harness, request: &Json) -> Json {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let created = create_provider(
            request["provider"].as_str().unwrap(),
            request["task"].as_str().unwrap(),
            request["model"].as_str(),
            request["effort"].as_str(),
            Some(harness.cli_env()),
        );
        let provider = match created {
            Ok(provider) => provider,
            Err(error) => return outcome(Err(error)),
        };
        let mut described = Map::new();
        described.insert("key".into(), text(provider.key()));
        described.insert("name".into(), Json::from(provider.name()));
        described.insert("model".into(), text(provider.model()));
        described.insert("reasoning_effort".into(), provider.reasoning_effort().map_or(Json::Null, text));
        let prompt = request["prompt"].as_str().unwrap();
        if request["mode"].as_str() == Some("complete") {
            match provider.complete(prompt).await {
                Ok(answer) => described.insert("answer".into(), Json::from(answer)),
                Err(error) => return outcome(Err(error)),
            };
        } else {
            let mut chunks = Vec::new();
            let result = provider
                .stream(prompt, &mut |text, done| chunks.push(Json::Array(vec![Json::from(text), Json::Bool(done)])))
                .await;
            described.insert("chunks".into(), Json::Array(chunks));
            if let Err(error) = result {
                described.insert("error".into(), Json::from(error.to_string()));
            }
        }
        outcome(Ok(Json::Object(described)))
    })
}

/// One provider call on both sides: outcome and the stub's records must be equal.
fn compare_provider(harness: &Harness, label: &str, request: Json) {
    let python = harness.oracle(request.clone());
    let python_calls = harness.take_calls();
    let ours = rust_provider_call(harness, &request);
    let our_calls = harness.take_calls();
    assert_eq!(ours.dumps(Some(1)), python.dumps(Some(1)), "{label}: outcome");
    // Not vacuous: the CLI ran once unless no provider could be created at all.
    let expected = if matches!(label, "missing cli" | "unknown provider") { 0 } else { 1 };
    assert_eq!(python_calls.len(), expected, "{label}: Python's CLI calls");
    assert_eq!(our_calls.len(), python_calls.len(), "{label}: number of CLI calls");
    for (ours, theirs) in our_calls.iter().zip(&python_calls) {
        for key in ["argv", "stdin", "cwd", "env"] {
            assert_eq!(ours[key].dumps(Some(1)), theirs[key].dumps(Some(1)), "{label}: {key}");
        }
    }
}

fn provider_request(
    provider: &str,
    mode: &str,
    model: Option<&str>,
    effort: Option<&str>,
    task: &str,
    prompt: &str,
) -> Json {
    object([
        ("kind", text("provider")),
        ("provider", text(provider)),
        ("mode", text(mode)),
        ("model", model.map_or(Json::Null, text)),
        ("effort", effort.map_or(Json::Null, text)),
        ("task", text(task)),
        ("prompt", text(prompt)),
    ])
}

#[test]
fn providers_given_stub_cli_when_called_then_same_argv_env_stdin_and_outcome() {
    let mut harness = Harness::new();
    let prompt = "Проанализируй {транскрипт}\n\"кавычки\" и\u{2028}разделитель";
    let agent = |text: &str| {
        object([
            ("type", Json::from("item.completed")),
            ("item", object([("type", Json::from("agent_message")), ("text", Json::from(text))])),
        ])
        .dumps(None)
    };
    let codex_outputs: Vec<(&str, String, &str, i32)> = vec![
        ("answer", codex_answer("Ответ"), "", 0),
        (
            "reconnect",
            format!(
                "{{\"type\":\"error\",\"message\":\"Reconnecting... 2/5\"}}\n{}\n{{\"type\":\"turn.completed\"}}\n",
                agent("a")
            ),
            "",
            0,
        ),
        ("two messages", format!("{}\ngarbage\n{}\n", agent("one"), agent("two")), "", 0),
        ("turn failed", "{\"type\":\"turn.failed\",\"error\":{\"message\":\"stream disconnected\"}}\n".into(), "", 0),
        ("turn failed bare", "{\"type\":\"turn.failed\"}\n".into(), "", 0),
        (
            "no message",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"reasoning\",\"text\":\"x\"}}\nnot json\n".into(),
            "",
            0,
        ),
        ("exit with stderr", String::new(), "auth expired\n", 1),
        ("exit silent", String::new(), "", 2),
        ("exit blank stderr", String::new(), "  \n", 1),
        ("raw separator", format!("{}\n", agent("до\u{2028}после")), "", 0),
        (
            "message after done",
            format!("{}\n{{\"type\":\"turn.completed\"}}\n{}\n", agent("first"), agent("late")),
            "",
            0,
        ),
        ("null line", "null\n".into(), "", 0),
        ("no trailing newline", agent("tail"), "", 0),
    ];
    for (label, stdout, stderr, code) in &codex_outputs {
        harness.answer("codex", stdout, stderr, *code);
        for mode in ["complete", "stream"] {
            compare_provider(
                &harness,
                &format!("codex {label} {mode}"),
                provider_request("codex", mode, Some("gpt-5.4"), Some("high"), "increment", prompt),
            );
        }
    }
    harness.answer("codex", &codex_answer("ok"), "", 0);
    for (model, effort) in [(Some("o4-mini-spark"), Some("xhigh")), (Some("gpt-6-astra"), Some("")), (None, None)] {
        compare_provider(
            &harness,
            &format!("codex {model:?} {effort:?}"),
            provider_request("codex", "complete", model, effort, "Title Task", prompt),
        );
    }

    let claude_outputs: Vec<(&str, &str, &str, i32)> = vec![
        ("text", "  Ответ модели \n", "", 0),
        (
            "stream events",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"part\"},{\"type\":\"tool_use\"}]}}\n\n{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"part two\"}]}}\n{\"type\":\"result\",\"result\":\"final\"}\n",
            "",
            0,
        ),
        (
            "no result",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"only\"}]}}\nnot json\n",
            "",
            0,
        ),
        ("result without text", "{\"type\":\"result\"}\n", "", 0),
        ("exit", "", "rate limited\n", 1),
        ("exit silent", "", "", 1),
        ("message not object", "{\"type\":\"assistant\",\"message\":\"x\"}\n", "", 0),
        ("content null", "{\"type\":\"assistant\",\"message\":{\"content\":null}}\n", "", 0),
    ];
    for (label, stdout, stderr, code) in &claude_outputs {
        harness.answer("claude", stdout, stderr, *code);
        for mode in ["complete", "stream"] {
            for effort in [None, Some(""), Some("max")] {
                compare_provider(
                    &harness,
                    &format!("claude {label} {mode} {effort:?}"),
                    provider_request("claude", mode, Some("claude-x"), effort, "meeting_chat", prompt),
                );
            }
        }
    }
    harness.answer("claude", "ok", "", 0);
    compare_provider(
        &harness,
        "claude default model",
        provider_request("claude", "complete", None, None, "", "  <!-- ccs-automation:recog task=x -->\nalready"),
    );
    harness.set("CLAUDE_MODEL", "legacy-claude");
    harness.set("CODEX_MODEL", "legacy-codex");
    harness.set("CODEX_REASONING_EFFORT", "medium");
    compare_provider(
        &harness,
        "claude legacy model",
        provider_request("claude", "complete", None, None, "analysis", "p"),
    );
    compare_provider(
        &harness,
        "codex legacy model",
        provider_request("codex", "complete", None, None, "analysis", "p"),
    );
    compare_provider(
        &harness,
        "unknown provider",
        provider_request("openrouter", "complete", None, None, "analysis", "p"),
    );

    // A service manager's PATH without node: both prepend the newest nvm node.
    let nvm = harness.path("home/.nvm/versions/node");
    for version in ["v18.19.0", "v20.11.1", "v9.0.0", "system"] {
        let node = nvm.join(version).join("bin/node");
        std::fs::create_dir_all(node.parent().unwrap()).unwrap();
        std::fs::write(&node, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    if !Path::new("/usr/bin/node").exists() && !Path::new("/bin/node").exists() {
        compare_provider(
            &harness,
            "nvm node",
            provider_request("claude", "complete", Some("m"), None, "analysis", "p"),
        );
        let calls_env = {
            let request = provider_request("claude", "complete", Some("m"), None, "analysis", "p");
            harness.oracle(request);
            harness.take_calls()
        };
        let path = calls_env[0]["env"]["PATH"].as_str().unwrap().to_owned();
        assert!(path.starts_with(&nvm.join("v20.11.1/bin").display().to_string()), "{path}");
    }

    // A missing CLI, unless a real one sits where resolve_cli falls back to: that one
    // would run.
    if common::system_cli_exists("codex") {
        eprintln!("skipped the missing-CLI case: a real codex is installed");
        return;
    }
    let empty = harness.path("empty");
    std::fs::create_dir_all(&empty).unwrap();
    harness.set("PATH", &empty.display().to_string());
    compare_provider(
        &harness,
        "missing cli",
        provider_request("codex", "complete", Some("m"), Some("high"), "analysis", "p"),
    );
}

#[test]
fn provider_for_settings_when_called_then_scenario_model_and_effort_reach_the_cli() {
    let harness = Harness::new();
    harness.answer("claude", "claude answer", "", 0);
    harness.answer("codex", &codex_answer("codex answer"), "", 0);
    let settings = br#"{"version": 1, "revision": "0f8fad5b-d9cb-469f-a165-70867728950e", "models": {"codex": "global"}, "tasks": {"analysis.increment": {"providers": ["claude", "codex"], "models": {"claude": "scenario"}, "reasoning_effort": "low", "claude_effort": "high"}}}"#;
    let data = harness.path("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("ai-settings.json"), settings).unwrap();
    for (task_id, provider) in [
        ("analysis.increment", None),
        ("analysis.increment", Some("codex")),
        ("title", None),
        ("meeting_chat", None),
        ("saved:my-increment", None),
    ] {
        let request = object([
            ("kind", text("provider_for_settings")),
            ("data_dir", path_json(&data)),
            ("task_id", text(task_id)),
            ("task", text("analysis")),
            ("provider", provider.map_or(Json::Null, text)),
            ("prompt", text("вопрос")),
        ]);
        let python = harness.oracle(request);
        let python_calls = harness.take_calls();
        let store = AiSettingsStore::new(&data, None).with_legacy_env(harness.legacy());
        let resolved = store.resolve_task(task_id, None, Fallback::default()).unwrap();
        let provider = provider_for_settings(&resolved, "analysis", provider, Some(harness.cli_env())).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let answer = runtime.block_on(provider.complete("вопрос")).unwrap();
        let ours = object([("ok", object([("name", Json::from(provider.name())), ("answer", Json::from(answer))]))]);
        assert_eq!(ours.dumps(None), python.dumps(None), "{task_id}");
        let our_calls = harness.take_calls();
        assert_eq!(our_calls[0]["argv"], python_calls[0]["argv"], "{task_id}");
        assert_eq!(our_calls[0]["stdin"], python_calls[0]["stdin"], "{task_id}");
    }
}

#[test]
fn ccs_marker_and_codex_command_when_built_then_identical() {
    let harness = Harness::new();
    let marks = [
        ("hello", "Increment Prompt"),
        ("hello", ""),
        ("hello", "  "),
        ("hello", "--Мой Сценарий: v2.0--"),
        ("hello", "ΣΑΣ Straße İ"),
        ("hello", "a/b\\c d\te"),
        ("hello", "---"),
        ("  \n<!-- ccs-automation:recog task=x -->\nbody", "title"),
        ("<!-- ccs-automation:recogX", "title"),
        ("", "copilot"),
    ];
    let cases: Vec<Json> =
        marks.iter().map(|(prompt, task)| object([("prompt", text(prompt)), ("task", text(task))])).collect();
    let python = harness.oracle(object([("kind", text("mark")), ("cases", Json::Array(cases))]));
    for ((prompt, task), expected) in marks.iter().zip(python.as_array().unwrap()) {
        assert_eq!(mark_ccs_automation(prompt, task), expected.as_str().unwrap(), "{task:?}");
    }

    let commands = [
        ("/usr/bin/codex", "gpt-5.4", Some("xhigh"), true, false),
        ("/usr/bin/codex", "gpt-5.4", Some("xhigh"), true, true),
        ("/usr/bin/codex", "gpt-5.4-spark", Some("xhigh"), true, true),
        ("codex", "m", Some(""), false, true),
        ("codex", "m", None, false, false),
    ];
    let cases: Vec<Json> = commands
        .iter()
        .map(|(cli, model, effort, ephemeral, isolated)| {
            object([
                ("cli", text(cli)),
                ("model", text(model)),
                ("effort", effort.map_or(Json::Null, text)),
                ("ephemeral", Json::Bool(*ephemeral)),
                ("isolated", Json::Bool(*isolated)),
            ])
        })
        .collect();
    let python = harness.oracle(object([("kind", text("codex_command")), ("cases", Json::Array(cases))]));
    for ((cli, model, effort, ephemeral, isolated), expected) in commands.iter().zip(python.as_array().unwrap()) {
        let ours: Vec<String> = build_codex_exec_command(cli.as_ref(), model, *effort, *ephemeral, *isolated)
            .into_iter()
            .map(|arg: OsString| arg.into_string().unwrap())
            .collect();
        assert_eq!(Json::from(ours), *expected, "{model} {effort:?}");
    }
}

// --- Increments ---

const MEETING_META: &str = r#"{"id": "2026-03-21_test", "title": "Daily sync \"core\"\\team", "created_at": "2026-03-21T14:30:00+00:00",
"participants": {"speaker_1": {"name": "Вася", "is_local": true}, "speaker_2": {"name": "Петя \"П\"", "is_local": false}, "speaker_3": {"is_local": 1}}}"#;

fn transcript_rows(range: std::ops::RangeInclusive<i64>) -> String {
    range
        .map(|id| match id % 4 {
            0 => format!(r#"{{"id": {id}, "event_type": "gap", "text": "marker {id}"}}"#),
            1 => format!(r#"{{"id": {id}, "ts_display": "14:30:{id:02}", "speaker": "Вася", "text": "Реплика {id} {{user_name}} {{\"j\": 1}}"}}"#),
            2 => format!(r#"{{"id": {id}, "ts_display": "14:31:{id:02}", "speaker": "Петя \"П\"", "text": "Ответ {id} с разделителем"}}"#),
            _ => format!(r#"{{"id": {id}, "speaker": null, "text": "Без времени {id}"}}"#),
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

struct IncrementCase {
    label: &'static str,
    prompt: Option<PromptConfig>,
    force: bool,
    participant: Option<&'static str>,
}

/// The files of a meetings tree, metadata with time stamps and durations neutralized.
fn tree(base: &Path) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let relative = path.strip_prefix(base).unwrap().display().to_string();
            let mut content = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
            if relative.ends_with("_meta.json")
                && let Ok(mut meta) = Json::parse(&content)
            {
                // Whether the file is exactly `json.dumps(indent=2)` plus a newline is
                // compared too, so neutralizing two values hides no formatting.
                let canonical = content == meta.dumps(Some(2)) + "\n";
                if let Some(map) = meta.as_object_mut() {
                    for key in ["generated_at", "duration_seconds"] {
                        if map.contains_key(key)
                            && !(key == "generated_at" && map["generated_at"] == text("2026-03-21T14:45:00+00:00"))
                        {
                            map.insert(key.into(), text("<varies>"));
                        }
                    }
                }
                content = format!("canonical={canonical}\n{}", meta.dumps(Some(2)));
            }
            files.push((relative, normalize_revisions(&content)));
        }
    }
    files.sort();
    files
}

fn prepare_meeting(root: &Path, rows: &str, extra: &[(&str, &str)]) -> PathBuf {
    let meeting = root.join("meetings/2026-03-21_test");
    std::fs::create_dir_all(&meeting).unwrap();
    std::fs::write(meeting.join("meta.json"), MEETING_META).unwrap();
    std::fs::write(meeting.join("transcript.jsonl"), rows).unwrap();
    for (relative, content) in extra {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    meeting
}

fn prompt_json(prompt: &PromptConfig) -> Json {
    config_json(prompt)
}

/// An outcome as text, the JSON parser's own wording after "Malformed JSONL in … line
/// N:" removed: serde_json and Python's json word the same error differently (a known
/// difference; the line number is ours and compared).
fn parser_wording_cut(outcome: &Json) -> String {
    let mut outcome = outcome.clone();
    if let Some(map) = outcome.as_object_mut()
        && let Some(Json::Str(message)) = map.get("message")
        && message.starts_with("Malformed JSONL")
        && let Some(line) = message.find(" line ")
        && let Some(colon) = message[line..].find(':')
    {
        let cut = message[..line + colon + 1].to_owned();
        map.insert("message".into(), Json::from(cut));
    }
    outcome.dumps(None)
}

/// Runs the same increment steps on two copies of a meeting, compares the trees and
/// the CLI calls. Returns Python's outcomes, its number of CLI calls and the file names,
/// so that a scenario can check that the comparison was not vacuous.
fn compare_increments(
    harness: &Harness,
    name: &str,
    prompts: &Path,
    rows: &str,
    extra: &[(&str, &str)],
    steps: &[IncrementCase],
) -> (Vec<String>, usize, Vec<String>) {
    let roots = [harness.path(&format!("{name}/python")), harness.path(&format!("{name}/rust"))];
    let meetings: Vec<PathBuf> = roots.iter().map(|root| prepare_meeting(root, rows, extra)).collect();
    let mut python_calls = Vec::new();
    let mut python_results = Vec::new();
    for step in steps {
        let mut request = vec![
            ("kind", text("increment")),
            ("meetings_base", path_json(&roots[0].join("meetings"))),
            ("prompts_dir", path_json(prompts)),
            ("meeting_id", text("2026-03-21_test")),
            ("meeting_path", path_json(&meetings[0])),
            ("force", Json::Bool(step.force)),
            ("participant_id", step.participant.map_or(Json::Null, text)),
        ];
        match &step.prompt {
            Some(prompt) => request.push(("prompt", prompt_json(prompt))),
            None => request.push(("all", Json::Bool(true))),
        }
        python_results.push(harness.oracle(object(request)));
        python_calls.extend(harness.take_calls());
    }

    let generator = IncrementGenerator::new(roots[1].join("meetings"), Some(prompts.to_path_buf()))
        .with_factory(Arc::new(harness.factory()))
        .with_legacy_env(harness.legacy());
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut our_calls = Vec::new();
    for (step, python) in steps.iter().zip(&python_results) {
        let result = runtime.block_on(async {
            match &step.prompt {
                Some(prompt) => generator
                    .generate("2026-03-21_test", &meetings[1], prompt, step.force, step.participant)
                    .await
                    .map(|version| version.map_or(Json::Null, Json::from)),
                None => generator.generate_all("2026-03-21_test", &meetings[1]).await.map(|()| Json::Null),
            }
        });
        let ours = outcome(result);
        assert_eq!(parser_wording_cut(&ours), parser_wording_cut(python), "{name}: {}", step.label);
        our_calls.extend(harness.take_calls());
    }
    let files = tree(&roots[0].join("meetings"));
    assert_eq!(tree(&roots[1].join("meetings")), files, "{name}: files");
    assert_eq!(our_calls.len(), python_calls.len(), "{name}: CLI calls");
    for (ours, theirs) in our_calls.iter().zip(&python_calls) {
        for key in ["argv", "stdin", "env", "cwd"] {
            assert_eq!(ours[key].dumps(Some(1)), theirs[key].dumps(Some(1)), "{name}: {key}");
        }
    }
    let outcomes = python_results.iter().map(parser_wording_cut).collect();
    (outcomes, python_calls.len(), files.into_iter().map(|(name, _)| name).collect())
}

fn has(files: &[String], name: &str) -> bool {
    files.iter().any(|file| file == name)
}

#[test]
fn increments_given_stub_cli_when_generated_then_same_files_and_calls() {
    let harness = Harness::new();
    harness.answer("codex", &codex_answer("## Задачи\n- Сделать ревью \"срочно\"\n"), "", 0);
    harness.answer("claude", "## Итог\nГотово", "", 0);
    let shipped = default_prompts_dir();
    let my_increment = PromptRegistry::new(&shipped).get("my-increment").unwrap();
    let custom = PromptConfig {
        id: "team-facts".into(),
        name: "Факты команды".into(),
        filter: None,
        template:
            "Title: {meeting_title}\nParticipants: {participants}\nUser: {user_name}\n{transcript} {\"json\": {}}"
                .into(),
    };
    let step = |label, prompt: &PromptConfig, force, participant| IncrementCase {
        label,
        prompt: Some(prompt.clone()),
        force,
        participant,
    };

    let (outcomes, calls, files) = compare_increments(
        &harness,
        "delta",
        &shipped,
        &transcript_rows(1..=6),
        &[],
        &[
            step("v1", &my_increment, false, None),
            step("zero delta", &my_increment, false, None),
            step("other participant", &my_increment, false, Some("speaker_2")),
            step("unknown participant", &my_increment, false, Some("nobody")),
            step("custom prompt", &custom, false, None),
            step("force", &my_increment, true, None),
        ],
    );
    assert_eq!(outcomes[..3], [r#"{"ok": 1}"#, r#"{"ok": null}"#, r#"{"ok": 2}"#]);
    assert_eq!(outcomes[3], r#"{"error": "ValueError", "message": "Participant 'nobody' not found"}"#);
    assert_eq!(outcomes[4..], [r#"{"ok": 1}"#, r#"{"ok": 3}"#]);
    assert_eq!(calls, 4);
    for file in ["my-increment/v1.md", "my-increment/v2_meta.json", "my-increment/v3.md", "team-facts/v1.md"] {
        assert!(has(&files, &format!("2026-03-21_test/increments/{file}")), "{file}: {files:?}");
    }

    // A completed v1, a stale v2 placeholder and an unreadable v3: the stale one is
    // marked failed and its segment retried, the next version is v4.
    let completed = r#"{"version": 1, "participant_id": null, "status": "completed", "generated_at": "2026-03-21T14:45:00+00:00", "segment_range": [1, 5]}"#;
    let stale = r#"{"version": 2, "participant_id": null, "status": "generating", "generated_at": "2026-03-21T14:45:00+00:00", "segment_range": [6, 6]}"#;
    let (outcomes, calls, _) = compare_increments(
        &harness,
        "appended",
        &shipped,
        &transcript_rows(1..=9),
        &[
            ("meetings/2026-03-21_test/increments/my-increment/v1_meta.json", completed),
            ("meetings/2026-03-21_test/increments/my-increment/v2_meta.json", stale),
            ("meetings/2026-03-21_test/increments/my-increment/v3_meta.json", "{broken"),
        ],
        &[step("after a stale and a broken version", &my_increment, false, None)],
    );
    assert_eq!((outcomes, calls), (vec![r#"{"ok": 4}"#.to_owned()], 1));
    let stale_after = std::fs::read_to_string(
        harness.path("appended/rust/meetings/2026-03-21_test/increments/my-increment/v2_meta.json"),
    );
    assert!(stale_after.unwrap().contains("process crash recovery"));

    // Saved settings select Claude with its own model and effort for the saved alias.
    let settings = r#"{"version": 1, "revision": "0f8fad5b-d9cb-469f-a165-70867728950e", "models": {}, "tasks": {"analysis.increment": {"prompt": "Custom {user_name}: {transcript} / {participants}", "providers": ["claude"], "models": {"claude": "custom-claude"}, "claude_effort": "max"}}}"#;
    let (outcomes, calls, _) = compare_increments(
        &harness,
        "saved-settings",
        &shipped,
        &transcript_rows(1..=3),
        &[("ai-settings.json", settings)],
        &[step("claude override", &my_increment, false, None)],
    );
    assert_eq!((outcomes, calls), (vec![r#"{"ok": 1}"#.to_owned()], 1));
    let meta = std::fs::read_to_string(
        harness.path("saved-settings/rust/meetings/2026-03-21_test/increments/my-increment/v1_meta.json"),
    );
    let meta = Json::parse(&meta.unwrap()).unwrap();
    assert_eq!(
        (meta["provider"].clone(), meta["model"].clone(), meta["reasoning_effort"].clone()),
        (text("claude"), text("custom-claude"), text("max"))
    );

    // A failing CLI is recorded as a failed version; a delta with events only is skipped.
    harness.answer("codex", "", "auth expired\n", 1);
    let rows = format!("{}{}", transcript_rows(1..=2), r#"{"id": 4, "event_type": "gap", "text": "x"}"#);
    let (outcomes, calls, files) = compare_increments(
        &harness,
        "failure",
        &shipped,
        &rows,
        &[],
        &[
            step("cli failure", &my_increment, false, None),
            step("only events", &PromptConfig { id: "events".into(), ..custom.clone() }, false, None),
        ],
    );
    assert_eq!((outcomes, calls), (vec![r#"{"ok": null}"#.to_owned(), r#"{"ok": null}"#.to_owned()], 2));
    assert!(
        has(&files, "2026-03-21_test/increments/my-increment/v1_meta.json")
            && !has(&files, "2026-03-21_test/increments/my-increment/v1.md")
    );
    let failed = std::fs::read_to_string(
        harness.path("failure/rust/meetings/2026-03-21_test/increments/my-increment/v1_meta.json"),
    )
    .unwrap();
    assert!(failed.contains("Codex CLI error: auth expired"), "{failed}");

    // generate_all: flat migration, filters, several prompts at once.
    harness.answer("codex", &codex_answer("Всё"), "", 0);
    let prompts = harness.path("all-prompts");
    std::fs::create_dir_all(&prompts).unwrap();
    std::fs::write(
        prompts.join("my-increment.md"),
        "---\nname: My increment\nfilter: null\n---\n\nUser: {user_name}\n{transcript}",
    )
    .unwrap();
    std::fs::write(
        prompts.join("sync-only.md"),
        "---\nname: Sync only\nfilter: SYNC\n---\n\nSync report for {user_name}\n{transcript}",
    )
    .unwrap();
    std::fs::write(prompts.join("retro-only.md"), "---\nname: Retro\nfilter: retro\n---\n\n{transcript}").unwrap();
    let (_, calls, files) = compare_increments(
        &harness,
        "all",
        &prompts,
        &transcript_rows(1..=5),
        &[
            ("meetings/2026-03-21_test/increments/v1.md", "# Old"),
            (
                "meetings/2026-03-21_test/increments/v1_meta.json",
                r#"{"version": 1, "status": "completed", "segment_range": [1, 2]}"#,
            ),
        ],
        &[IncrementCase { label: "generate_all", prompt: None, force: false, participant: None }],
    );
    assert_eq!(calls, 2);
    for file in ["my-increment/v1.md", "my-increment/v2.md", "sync-only/v1.md"] {
        assert!(has(&files, &format!("2026-03-21_test/increments/{file}")), "{file}: {files:?}");
    }
    assert!(!files.iter().any(|file| file.contains("retro-only")));

    let (outcomes, calls, _) = compare_increments(
        &harness,
        "malformed",
        &shipped,
        "{\"id\": 1, \"text\": \"a\"}\n\n{broken\n",
        &[],
        &[step("malformed transcript", &my_increment, false, None)],
    );
    assert!(outcomes[0].contains("Malformed JSONL in transcript.jsonl line 3:") && calls == 0);
}

// --- Titles ---

#[test]
fn titles_given_stub_cli_when_generated_then_same_title_and_call() {
    let harness = Harness::new();
    let data = harness.path("data");
    std::fs::create_dir_all(&data).unwrap();
    let long: Vec<String> = (0..300)
        .map(|index| format!("[{index:04}] Спикер {}: {}", index % 3, "слово ".repeat(index % 17 + 1)))
        .collect();
    let long = long.join("\n");
    let transcripts = [
        "short",
        "x".repeat(49).as_str(),
        &format!("   {}   ", "y".repeat(48)),
        &"обсуждаем {user_name} {{x}} ".repeat(4),
        &long,
    ]
    .map(str::to_owned);
    let answers = [
        codex_answer("  \"Архитектура системы\"  "),
        codex_answer(&"Очень длинное название ".repeat(10)),
        codex_answer("'\"'"),
        "{\"type\":\"turn.failed\"}\n".to_owned(),
    ];
    let variables: Variables = [("user_name", "Мария"), ("meeting_title", "Обзор"), ("transcript", "ignored")]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    let (mut titled, mut untitled) = (0, 0);
    for answer in &answers {
        harness.answer("codex", answer, "", 0);
        for transcript in &transcripts {
            for model in [None, Some("one-call")] {
                let request = object([
                    ("kind", text("title")),
                    ("data_dir", path_json(&data)),
                    ("transcript", text(transcript)),
                    ("model", model.map_or(Json::Null, text)),
                    ("variables", variables_json(&variables)),
                ]);
                let python = harness.oracle(request);
                let python_calls = harness.take_calls();
                let settings = AiSettingsStore::new(&data, None)
                    .with_legacy_env(harness.legacy())
                    .resolve_task("title", None, Fallback::default())
                    .unwrap();
                let ours =
                    title::generate_title_blocking(transcript, model, Some(&settings), &variables, &harness.factory());
                assert_eq!(Json::from(ours), python, "{} {model:?}", pytext::prefix_chars(transcript, 20));
                titled += usize::from(python != Json::Null);
                untitled += usize::from(python == Json::Null);
                let our_calls = harness.take_calls();
                assert_eq!(our_calls.len(), python_calls.len());
                for (ours, theirs) in our_calls.iter().zip(&python_calls) {
                    assert_eq!(ours["argv"], theirs["argv"]);
                    assert_eq!(ours["stdin"], theirs["stdin"]);
                    assert_eq!(ours["env"], theirs["env"]);
                }
            }
        }
    }

    // Not vacuous: two long transcripts, two models, two usable answers got titles;
    // short transcripts, a failed turn and an answer of quotes only did not.
    assert_eq!((titled, untitled), (8, 32));

    let mut rng = Rng(0x7171_7171_0000_0001);
    for _ in 0..40 {
        let transcript = rng.text(&["\n", "  ", "[00:00] A: ", "слово ", &"длинная строка ".repeat(30), "x"], 400);
        let python = harness.oracle(object([("kind", text("sample")), ("transcript", text(&transcript))]));
        assert_eq!(text(&title::sample_transcript(&transcript, 6000, 6)), python);
    }
}

#[test]
fn meeting_title_given_stub_cli_when_generated_then_same_update() {
    let harness = Harness::new();
    harness.answer("claude", "Архитектура \"системы\"", "", 0);
    harness.answer("codex", &codex_answer("Название"), "", 0);
    let settings = r#"{"version": 1, "revision": "0f8fad5b-d9cb-469f-a165-70867728950e", "models": {}, "tasks": {"title": {"providers": ["claude"], "models": {"claude": "meeting-model"}}}}"#;
    let cases: [(&str, Option<&str>, &str, &str); 4] = [
        (
            "saved claude",
            Some(settings),
            r#"{"title": null, "participants": {"a": {"name": "User", "is_local": true}, "b": {"name": "Пётр"}, "c": {}}}"#,
            "{\"id\": 1, \"ts_display\": \"12:00:00\", \"speaker\": \"User\", \"speaker_id\": \"a\", \"text\": \"Обсуждаем архитектуру системы и все необходимые изменения.\"}\n{\"speaker_id\": \"__system__\", \"text\": \"m\"}\n  \n{\"speaker\": null, \"text\": \"Без говорящего, но достаточно длинная реплика для названия\"}\nbroken\n",
        ),
        (
            "defaults",
            None,
            r#"{"title": "Старое", "participants": {}}"#,
            "{\"text\": \"Обсуждаем архитектуру системы и все необходимые изменения сегодня.\"}\n",
        ),
        ("empty transcript", None, r#"{}"#, "\n"),
        (
            "broken settings",
            Some("broken"),
            r#"{}"#,
            "{\"text\": \"Обсуждаем архитектуру системы и все необходимые изменения сегодня.\"}\n",
        ),
    ];
    for (label, settings, meta, rows) in cases {
        let mut results = Vec::new();
        for side in ["python", "rust"] {
            let data = harness.path(&format!("{label}/{side}"));
            let meeting = data.join("meetings/m1");
            std::fs::create_dir_all(&meeting).unwrap();
            if let Some(settings) = settings {
                std::fs::write(data.join("ai-settings.json"), settings).unwrap();
            }
            std::fs::write(meeting.join("meta.json"), meta).unwrap();
            std::fs::write(meeting.join("transcript.jsonl"), rows).unwrap();
            if side == "python" {
                let request = object([
                    ("kind", text("meeting_title")),
                    ("data_dir", path_json(&data)),
                    ("meeting_path", path_json(&meeting)),
                ]);
                results.push(harness.oracle(request));
            } else {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                let update = runtime.block_on(title::meeting_title_update(&data, &meeting, &harness.factory()));
                results.push(update.map_or(Json::Null, |update| update.to_json()));
            }
            results.push(Json::Array(harness.take_calls().iter().map(|call| call["stdin"].clone()).collect()));
        }
        assert_eq!(results[2].dumps(None), results[0].dumps(None), "{label}: update");
        assert_eq!(results[3], results[1], "{label}: prompt");
        let expected_title = match label {
            "saved claude" => Some("Архитектура \"системы"),
            "defaults" => Some("Название"),
            _ => None,
        };
        assert_eq!(results[0]["title"].as_str(), expected_title, "{label}");
    }
}

// --- Questions about a meeting ---

#[test]
fn chat_dialog_given_same_operations_then_same_file_and_prompts() {
    let harness = Harness::new();
    let turns = [
        (
            "2026-11-02T08:40:00.123456+00:00",
            "Что сказал Пётр?",
            "Пётр за синюю кнопку [11:30:14].",
            "Claude (claude-opus-5-5 max)",
            "defaults",
            false,
        ),
        (
            "2026-11-02T08:41:00+00:00",
            "Цитата \"в кавычках\" и \\ черта\tтаб",
            "Ответ\nв две строки",
            "Codex (gpt-6-astra xhigh)",
            "0f8fad5b-d9cb-469f-a165-70867728950e",
            true,
        ),
        ("t", "Разделитель\u{2028}внутри", "теряется при чтении", "p", "r", false),
        ("t", "Последний", "😀", "p", "r", false),
    ];
    let mut ops: Vec<Json> = turns
        .iter()
        .map(|(asked, question, answer, provider, revision, truncated)| {
            object([
                ("op", text("append")),
                (
                    "turn",
                    object([
                        ("asked_at", text(asked)),
                        ("question", text(question)),
                        ("answer", text(answer)),
                        ("provider", text(provider)),
                        ("settings_revision", text(revision)),
                        ("transcript_truncated", Json::Bool(*truncated)),
                    ]),
                ),
            ])
        })
        .collect();
    ops.push(object([("op", text("read"))]));
    ops.push(object([
        ("op", text("build")),
        ("instructions", text("Инструкции\n\nТранскрипт:\n[11:30:14] Peter: Я за синюю кнопку.\n  \n")),
        ("question", text("А он уверен?")),
    ]));
    let more: Vec<Json> = (0..10)
        .map(|number| {
            object([
                ("op", text("append")),
                (
                    "turn",
                    object([
                        ("asked_at", text("t")),
                        ("question", Json::from(format!("Вопрос номер {number}."))),
                        ("answer", text("Ответ")),
                        ("provider", text("p")),
                        ("settings_revision", text("r")),
                        ("transcript_truncated", Json::Bool(false)),
                    ]),
                ),
            ])
        })
        .collect();
    ops.extend(more);
    ops.push(object([("op", text("build")), ("instructions", text("I")), ("question", text("Последний"))]));
    ops.push(object([("op", text("clear"))]));
    ops.push(object([("op", text("clear"))]));
    ops.push(object([("op", text("read"))]));

    let damaged = "{\"question\": \"оборв\n[1, 2]\n\u{ff}\n{\"asked_at\": 1, \"question\": \"q\", \"answer\": \"a\", \"provider\": null, \"settings_revision\": [1], \"transcript_truncated\": \"yes\"}\n{\"asked_at\": \"t\", \"question\": null, \"answer\": \"a\", \"provider\": \"p\", \"settings_revision\": \"r\", \"transcript_truncated\": false}\r\n";
    let python_dir = harness.path("chat/python");
    let rust_dir = harness.path("chat/rust");
    let mut results = Vec::new();
    for dir in [&python_dir, &rust_dir] {
        std::fs::create_dir_all(dir).unwrap();
    }
    results.push(harness.oracle(object([
        ("kind", text("chat")),
        ("meeting_path", path_json(&python_dir)),
        ("ops", Json::Array(ops.clone())),
    ])));
    results.push(Json::Array(ops.iter().map(|op| rust_chat_op(&rust_dir, op)).collect()));
    assert_eq!(results[1].dumps(Some(1)), results[0].dumps(Some(1)), "operations");

    // A file damaged by hand, then more appends; and an append into a deleted meeting.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(damaged.as_bytes());
    bytes.extend_from_slice(b"\xff\xfe broken bytes\n");
    for dir in [&python_dir, &rust_dir] {
        std::fs::write(dir.join("chat.jsonl"), &bytes).unwrap();
    }
    let ops = vec![ops[0].clone(), object([("op", text("read"))])];
    let python = harness.oracle(object([
        ("kind", text("chat")),
        ("meeting_path", path_json(&python_dir)),
        ("ops", Json::Array(ops.clone())),
    ]));
    let ours = Json::Array(ops.iter().map(|op| rust_chat_op(&rust_dir, op)).collect());
    assert_eq!(ours.dumps(Some(1)), python.dumps(Some(1)), "damaged file");
    assert_eq!(
        std::fs::read(rust_dir.join("chat.jsonl")).unwrap(),
        std::fs::read(python_dir.join("chat.jsonl")).unwrap(),
        "chat.jsonl"
    );

    let gone_python = harness.path("chat/gone-python");
    let gone_rust = harness.path("chat/gone-rust");
    let python = harness.oracle(object([
        ("kind", text("chat")),
        ("meeting_path", path_json(&gone_python)),
        ("ops", Json::Array(vec![ops[0].clone()])),
    ]));
    let ours = rust_chat_op(&gone_rust, &ops[0]);
    assert_eq!(ours["error"], python.as_array().unwrap()[0]["error"]);
    assert!(!gone_rust.exists() && !gone_python.exists());
}

fn rust_chat_op(path: &Path, op: &Json) -> Json {
    let turn_json = |turn: &ChatTurn| turn.to_json();
    outcome(match op["op"].as_str().unwrap() {
        "append" => {
            let turn = &op["turn"];
            let turn = ChatTurn {
                asked_at: turn["asked_at"].clone(),
                question: turn["question"].as_str().unwrap().to_owned(),
                answer: turn["answer"].as_str().unwrap().to_owned(),
                provider: turn["provider"].clone(),
                settings_revision: turn["settings_revision"].clone(),
                transcript_truncated: turn["transcript_truncated"].clone(),
            };
            meeting_chat::append_turn(path, &turn).map(|()| Json::Null)
        }
        "read" => meeting_chat::read_turns(path).map(|turns| Json::Array(turns.iter().map(turn_json).collect())),
        "clear" => meeting_chat::clear_turns(path).map(|()| Json::Null),
        "build" => meeting_chat::read_turns(path).map(|turns| {
            Json::from(meeting_chat::build_chat_prompt(
                op["instructions"].as_str().unwrap(),
                &turns,
                op["question"].as_str().unwrap(),
            ))
        }),
        other => panic!("unknown op {other}"),
    })
}
