//! Parity with Python for the installation settings, the data directory and the
//! WebSocket origin allowlist: the same fixtures go through
//! `recog/install_config.py`, `recog/config.py` and `recog/ws_origin.py` (via
//! tests/parity/harness.py) and through the port, and the answers must be equal —
//! values as JSON, floats by their repr, errors by message, notices line by line.
#![forbid(unsafe_code)]

mod common;

use std::fs;
use std::path::Path;

use common::{assert_none, assert_same, difference, python, run_python};
use recog_core::install_config::{self, Environ, InstallConfig, SettingValue};
use recog_core::{data_dir, py, ws_origin};
use serde_json::{Map as JsonMap, Value as Json, json};

const HOSTS: &str =
    "[conferences]\njitsi = [\"https://meet.example.org\"]\nmatrix = [\"https://element.example.org\"]\n";

/// One fixture: files under the case directory and the environment, `{dir}` standing
/// for the case directory. `RECOG_CONFIG_FILE` defaults to `{dir}/config.toml` and
/// the legacy file to `{dir}/conference-hosts.json`, HOME to `{dir}/home`.
struct Case {
    name: &'static str,
    files: Vec<(&'static str, Vec<u8>)>,
    env: Vec<(&'static str, String)>,
    unset: Vec<&'static str>,
    /// A TOML syntax error: tomllib and the toml crate word it differently, so only
    /// the failure and the path before the message are compared.
    syntax_error: bool,
}

fn case(name: &'static str, config: &str) -> Case {
    Case {
        name,
        files: vec![("config.toml", config.as_bytes().to_vec())],
        env: vec![],
        unset: vec![],
        syntax_error: false,
    }
}

fn with_env(mut c: Case, env: &[(&'static str, &str)]) -> Case {
    c.env.extend(env.iter().map(|(k, v)| (*k, (*v).to_owned())));
    c
}

fn shipped_example() -> String {
    let text = fs::read_to_string(common::repo_root().join("deploy/config.example.toml")).unwrap();
    text.lines()
        .map(
            |line| {
                if line.starts_with("# ") && line.contains(" = ") && !line.contains("RECOG") {
                    &line[2..]
                } else {
                    line
                }
            },
        )
        .collect::<Vec<_>>()
        .join("\n")
}

fn malformed_documents() -> Vec<(&'static str, String)> {
    let long_label = format!("[conferences]\nmatrix = [\"https://{}.example.org\"]\n", "a".repeat(64));
    let mut documents: Vec<(&'static str, String)> = [
        ("unknown table", "[other]\n"),
        ("unknown platform", "[conferences]\nother = []\n"),
        ("hosts not an array", "[conferences]\njitsi = \"https://meet.example.org\"\n"),
        ("host not a string", "[conferences]\njitsi = [false]\n"),
        ("http origin", "[conferences]\njitsi = [\"http://meet.example.org\"]\n"),
        ("trailing slash", "[conferences]\nmatrix = [\"https://element.example.org/\"]\n"),
        ("explicit port", "[conferences]\nmatrix = [\"https://element.example.org:443\"]\n"),
        ("path", "[conferences]\nmatrix = [\"https://element.example.org/path\"]\n"),
        ("query", "[conferences]\nmatrix = [\"https://element.example.org?query\"]\n"),
        ("fragment", "[conferences]\nmatrix = [\"https://element.example.org#room\"]\n"),
        ("userinfo", "[conferences]\nmatrix = [\"https://user@element.example.org\"]\n"),
        ("wildcard", "[conferences]\nmatrix = [\"https://*.example.org\"]\n"),
        ("uppercase", "[conferences]\nmatrix = [\"https://MEET.example.org\"]\n"),
        ("cyrillic e", "[conferences]\nmatrix = [\"https://m\u{435}et.example.org\"]\n"),
        ("empty label", "[conferences]\nmatrix = [\"https://meet..example.org\"]\n"),
        ("trailing dot", "[conferences]\nmatrix = [\"https://meet.example.org.\"]\n"),
        ("leading hyphen", "[conferences]\nmatrix = [\"https://-meet.example.org\"]\n"),
        ("ipv6", "[conferences]\nmatrix = [\"https://[::1]\"]\n"),
        ("unclosed bracket", "[conferences]\nmatrix = [\"https://[::1\"]\n"),
        ("bracketed ipv4", "[conferences]\nmatrix = [\"https://[1.2.3.4]\"]\n"),
        ("bracketed junk", "[conferences]\nmatrix = [\"https://[zz]\"]\n"),
        ("ipv4 shorthand", "[conferences]\nmatrix = [\"https://127.1\"]\n"),
        ("bare number", "[conferences]\nmatrix = [\"https://123\"]\n"),
        ("numeric last label", "[conferences]\nmatrix = [\"https://foo.123\"]\n"),
        ("octet too large", "[conferences]\nmatrix = [\"https://1.2.3.256\"]\n"),
        ("hex last label", "[conferences]\nmatrix = [\"https://a.0x1f\"]\n"),
        ("leading zero octet", "[conferences]\nmatrix = [\"https://01.2.3.4\"]\n"),
        ("ktalk", "[conferences]\nmatrix = [\"https://ktalk.ru\"]\n"),
        ("vas3k", "[conferences]\njitsi = [\"https://meet.vas3k.cloud\"]\n"),
        ("google meet", "[conferences]\njitsi = [\"https://meet.google.com\"]\n"),
        ("zoom", "[conferences]\njitsi = [\"https://zoom.us\"]\n"),
        ("zoom subdomain", "[conferences]\njitsi = [\"https://app.zoom.us\"]\n"),
        ("same host twice", "[conferences]\njitsi = [\"https://a.example.org\", \"https://a.example.org\"]\n"),
        (
            "same host on two platforms",
            "[conferences]\njitsi = [\"https://same.example.org\"]\nmatrix = [\"https://same.example.org\"]\n",
        ),
        ("unknown sources key", "[sources.other]\n"),
        ("alias to unknown", "[sources.aliases]\nold = \"unknown\"\n"),
        ("alias of a platform", "[sources.aliases]\nktalk = \"matrix\"\n"),
        ("alias of gmeet", "[sources.aliases]\ngmeet = \"jitsi\"\n"),
        ("alias of zoom", "[sources.aliases]\nzoom = \"jitsi\"\n"),
        ("alias to a number", "[sources.aliases]\nold = 1\n"),
        ("empty alias name", "[sources.aliases]\n\"\" = \"jitsi\"\n"),
        ("label a number", "[sources.labels]\nimporter = 1\n"),
        ("label empty", "[sources.labels]\nimporter = \"\"\n"),
        ("label of nothing", "[sources.labels]\n\"\" = \"Label\"\n"),
        ("labels an array", "[sources]\nlabels = [\"importer\"]\n"),
        ("unknown asr key", "[asr]\nother = \"x\"\n"),
        ("backend a number", "[asr]\nbackend = 1\n"),
        ("timeout a string", "[asr]\nremote_timeout_s = \"15\"\n"),
        ("timeout a flag", "[asr]\nremote_timeout_s = true\n"),
        ("fails a float", "[asr]\ncircuit_fails = 3.5\n"),
        ("threads zero", "[asr]\ntorch_threads = 0\n"),
        ("threads negative", "[asr]\ntorch_threads = -2\n"),
        ("flag a string", "[live]\nzoom_app = \"1\"\n"),
        ("flag a number", "[live]\nzoom_app = 1\n"),
        ("list a string", "[web]\nextra_ws_origins = \"https://a.example.org\"\n"),
        ("list of numbers", "[web]\nextra_ws_origins = [1]\n"),
        ("a date", "[asr]\nbackend = 2026-11-08\n"),
        ("a section not a table", "asr = 5\n"),
        ("two errors, first section wins", "[web]\nextra_ws_origins = 1\n[asr]\nbackend = 1\n"),
        ("section error before hosts", "[conferences]\njitsi = [\"x\"]\n[macos]\ncodesign_identity = 1\n"),
        ("aliases before labels", "[sources.aliases]\nx = 1\n[sources.labels]\ny = 1\n"),
    ]
    .iter()
    .map(|(name, text)| (*name, (*text).to_owned()))
    .collect();
    documents.push(("64-character label", long_label));
    documents
}

fn cases() -> Vec<Case> {
    let mut cases = vec![
        Case { name: "no files", files: vec![], env: vec![], unset: vec![], syntax_error: false },
        case("shipped example uncommented", &shipped_example()),
        case(
            "hosts, aliases and labels",
            &format!("{HOSTS}[sources.aliases]\nold-meet = \"jitsi\"\nlegacy = \"matrix\"\n[sources.labels]\nimporter = \"Phone Importer\"\n"),
        ),
        case(
            "typed sections",
            "[asr]\nbackend = \"remote\"\nremote_timeout_s = 20\ncircuit_fails = 4\ncircuit_open_s = 1e-5\n\
             [live]\ndedup_max_gap_s = 2.5\nzoom_app = true\n[copilot]\nautospawn = false\ngrace_s = 1e16\n\
             [web]\nexternal_meetings_paths = [\"/a/meetings:a\", \"/b/meetings\"]\n",
        ),
        case("canonical hosts", "[conferences]\njitsi = [\"https://evilzoom.us\", \"https://evilktalk.ru\", \"https://meet-1.xn--e1afmkfd.xn--p1ai\"]\nmatrix = [\"https://127.0.0.1\"]\n"),
        with_env(
            case("environment over the file", "[asr]\nbackend = \"remote\"\nremote_url = \"https://a\"\nremote_timeout_s = 20\n[live]\ndedup_max_gap_s = 2\n"),
            &[
                ("RECOG_ASR_BACKEND", "local"),
                ("RECOG_ASR_REMOTE_URL", " https://a "),
                ("RECOG_ASR_REMOTE_MODEL", "big"),
                ("RECOG_ZOOM_APP", "yes"),
                ("RECOG_EXTRA_WS_ORIGINS", " https://a.test , ,https://b.test"),
                ("RECOG_TORCH_THREADS", ""),
                ("RECOG_ASR_REMOTE_TIMEOUT_S", "20.0"),
                ("DEDUP_MAX_GAP_S", "2.5"),
            ],
        ),
        with_env(
            Case { name: "number and flag spellings", files: vec![], env: vec![], unset: vec![], syntax_error: false },
            &[
                ("RECOG_ASR_REMOTE_TIMEOUT_S", "1_5"),
                ("RECOG_ASR_LIVE_TIMEOUT_S", " 2.5e1 "),
                ("RECOG_ASR_CIRCUIT_OPEN_S", "inf"),
                ("ASR_PENDING_BUFFER_CAP_S", "-0"),
                ("DEDUP_MAX_GAP_S", ".5"),
                ("RECOG_COPILOT_GRACE_S", "1.e2"),
                ("RECOG_TORCH_THREADS", "+0_7"),
                ("RECOG_ASR_CIRCUIT_FAILS", "\u{a0}12\u{3000}"),
                ("RECOG_ZOOM_APP", "TRUE"),
                ("RECOG_ECHO_CANCEL", " On "),
                ("RECOG_GOLDEN_RECORD", "no"),
                ("RECOG_COPILOT_AUTOSPAWN", "0"),
                ("RECOG_CODESIGN_IDENTITY", "Developer ID"),
                ("EXTERNAL_MEETINGS_PATHS", "/x:y,/z"),
            ],
        ),
        with_env(
            Case { name: "nan from the environment", files: vec![], env: vec![], unset: vec![], syntax_error: false },
            &[("RECOG_ASR_REMOTE_TIMEOUT_S", "NaN")],
        ),
        with_env(case("nan conflicts with itself", "[asr]\nremote_timeout_s = nan\n"), &[("RECOG_ASR_REMOTE_TIMEOUT_S", "nan")]),
        Case {
            name: ".env fills unset variables only",
            files: vec![
                ("config.toml", b"[asr]\ndevice = \"cuda\"\n".to_vec()),
                (
                    "repo/.env",
                    b"# comment\n\nRECOG_ASR_DEVICE=cpu\nRECOG_ASR_REMOTE_MODEL=\"two words\"\r\nRECOG_TEST_SINGLE='three'\n\
                      RECOG_ZOOM_APP = on\nnot a setting\n=orphan\nRECOG_ASR_BACKEND=remote\n"
                        .to_vec(),
                ),
            ],
            env: vec![("RECOG_ASR_BACKEND", "local".into())],
            unset: vec![],
            syntax_error: false,
        },
        Case {
            name: "legacy hosts only",
            files: vec![(
                "conference-hosts.json",
                br#"{"jitsi": ["https://meet.example.org"], "source_aliases": {"old-meet": "jitsi"}}"#.to_vec(),
            )],
            env: vec![],
            unset: vec![],
            syntax_error: false,
        },
        Case {
            name: "legacy hosts in the default place",
            files: vec![("repo/.local/conference-hosts.json", br#"{"matrix": ["https://element.example.org"]}"#.to_vec())],
            env: vec![],
            unset: vec!["RECOG_CONFERENCE_HOSTS_FILE"],
            syntax_error: false,
        },
        Case {
            name: "both files",
            files: vec![("config.toml", HOSTS.as_bytes().to_vec()), ("conference-hosts.json", b"{}".to_vec())],
            env: vec![],
            unset: vec![],
            syntax_error: false,
        },
        Case {
            name: "legacy hosts malformed json",
            files: vec![("conference-hosts.json", b"{\"jitsi\": [1,]}".to_vec())],
            env: vec![],
            unset: vec![],
            syntax_error: false,
        },
        Case {
            name: "legacy hosts unknown key",
            files: vec![("conference-hosts.json", b"{\"other\": 1}".to_vec())],
            env: vec![],
            unset: vec![],
            syntax_error: false,
        },
        Case {
            name: "legacy hosts not a list",
            files: vec![("conference-hosts.json", b"{\"jitsi\": null}".to_vec())],
            env: vec![],
            unset: vec![],
            syntax_error: false,
        },
        Case {
            name: "legacy aliases not a table",
            files: vec![("conference-hosts.json", b"{\"source_aliases\": [\"x\"]}".to_vec())],
            env: vec![],
            unset: vec![],
            syntax_error: false,
        },
        Case {
            name: "xdg config home",
            files: vec![("xdg/recog/config.toml", b"[asr]\ndevice = \"mps\"\n".to_vec())],
            env: vec![("XDG_CONFIG_HOME", "{dir}/xdg".into())],
            unset: vec!["RECOG_CONFIG_FILE"],
            syntax_error: false,
        },
        Case {
            name: "default under home",
            files: vec![("home/.config/recog/config.toml", b"[asr]\ndevice = \"cpu\"\n".to_vec())],
            env: vec![("XDG_CONFIG_HOME", String::new())],
            unset: vec!["RECOG_CONFIG_FILE"],
            syntax_error: false,
        },
        Case {
            name: "tilde config file",
            files: vec![("home/elsewhere.toml", b"[asr]\nbackend = 3\n".to_vec())],
            env: vec![("RECOG_CONFIG_FILE", "~/elsewhere.toml".into())],
            unset: vec![],
            syntax_error: false,
        },
        Case {
            name: "relative config file",
            files: vec![("repo/rel/config.toml", b"[live]\nzoom_app = 2\n".to_vec())],
            env: vec![("RECOG_CONFIG_FILE", "rel//./config.toml".into())],
            unset: vec![],
            syntax_error: false,
        },
        Case {
            name: "config path is a directory",
            files: vec![("config.toml/keep", b"".to_vec())],
            env: vec![],
            unset: vec![],
            syntax_error: false,
        },
        Case {
            name: "config not utf-8",
            files: vec![("config.toml", b"[asr]\nbackend = \"\xff\"\n".to_vec())],
            env: vec![],
            unset: vec![],
            syntax_error: false,
        },
        Case {
            name: "crlf file",
            files: vec![("config.toml", b"[asr]\r\nbackend = \"remote\"\r\nremote_url = \"\"\"\r\nx\"\"\"\r\n".to_vec())],
            env: vec![],
            unset: vec![],
            syntax_error: false,
        },
        Case {
            name: "toml syntax error",
            files: vec![("config.toml", b"[conferences\n".to_vec())],
            env: vec![],
            unset: vec![],
            syntax_error: true,
        },
    ];
    for (variable, raw) in [
        ("RECOG_ZOOM_APP", "maybe"),
        ("RECOG_ZOOM_APP", "it's"),
        ("RECOG_TORCH_THREADS", "many"),
        ("RECOG_TORCH_THREADS", "0"),
        ("RECOG_TORCH_THREADS", "-3"),
        ("RECOG_TORCH_THREADS", "0x10"),
        ("RECOG_TORCH_THREADS", "1__0"),
        ("RECOG_TORCH_THREADS", "3.0"),
        ("RECOG_ASR_REMOTE_TIMEOUT_S", "soon"),
        ("RECOG_ASR_REMOTE_TIMEOUT_S", "1_"),
        ("RECOG_ASR_REMOTE_TIMEOUT_S", "e5"),
        ("RECOG_ASR_REMOTE_TIMEOUT_S", "\u{1}5"),
        ("RECOG_ASR_CIRCUIT_FAILS", "\u{661}"),
    ] {
        let name: &'static str = Box::leak(format!("malformed {variable}={raw:?}").into_boxed_str());
        cases.push(with_env(
            Case { name, files: vec![], env: vec![], unset: vec![], syntax_error: false },
            &[(variable, raw)],
        ));
    }
    for (name, text) in malformed_documents() {
        cases.push(case(name, &text));
    }
    cases
}

fn number(x: f64) -> Json {
    Json::from(format!("float:{}", py::float_repr(x)))
}

fn dump_value(value: &SettingValue) -> Json {
    match value {
        SettingValue::Flag(b) => json!(b),
        SettingValue::Integer(n) => json!(n),
        SettingValue::Number(x) => number(*x),
        SettingValue::Text(s) => json!(s),
        SettingValue::List(items) => json!(items),
    }
}

fn dump_config(config: &InstallConfig) -> Json {
    let pairs = |items: &[(String, String)]| -> Json {
        Json::Object(items.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<JsonMap<_, _>>())
    };
    let mut out = json!({
        "jitsi": config.jitsi,
        "matrix": config.matrix,
        "source_aliases": pairs(&config.source_aliases),
        "source_labels": pairs(&config.source_labels),
        "from_file": config.from_file.iter().collect::<Vec<_>>(),
    });
    for section in install_config::SECTIONS {
        let fields: JsonMap<String, Json> = install_config::all_settings()
            .filter(|s| s.table == section)
            .map(|s| (s.key.to_owned(), dump_value(&config.get(s.table, s.key))))
            .collect();
        out[section] = Json::Object(fields);
    }
    out
}

fn case_env(c: &Case, dir: &Path) -> Environ {
    let d = dir.to_string_lossy();
    let mut env: Environ = [
        ("HOME".to_owned(), format!("{d}/home")),
        ("RECOG_CONFIG_FILE".to_owned(), format!("{d}/config.toml")),
        ("RECOG_CONFERENCE_HOSTS_FILE".to_owned(), format!("{d}/conference-hosts.json")),
    ]
    .into();
    for key in &c.unset {
        env.remove(*key);
    }
    for (k, v) in &c.env {
        env.insert((*k).to_owned(), v.replace("{dir}", &d));
    }
    env
}

fn rust_answer(repo: &Path, mut environ: Environ) -> Json {
    let mut warnings = Vec::new();
    let mut answer = match install_config::load(repo, &mut environ, &mut |w| warnings.push(w)) {
        Ok(config) => json!({"config": dump_config(&config), "hosts_json": config.hosts_json()}),
        Err(message) => json!({"error": message}),
    };
    answer["stderr"] = json!(warnings);
    answer["environ"] = json!(environ);
    answer
}

#[test]
fn install_config_fixtures_load_the_same_settings_errors_and_notices_as_python() {
    // given
    let Some(python) = python() else { return };
    let root = tempfile::tempdir().unwrap();
    let cases = cases();
    let mut requests = Vec::new();
    for (i, c) in cases.iter().enumerate() {
        let dir = root.path().join(i.to_string());
        fs::create_dir_all(dir.join("repo")).unwrap();
        fs::create_dir_all(dir.join("home")).unwrap();
        for (rel, bytes) in &c.files {
            let path = dir.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        requests.push(json!({"repo": dir.join("repo"), "env": case_env(c, &dir)}));
    }
    // when
    let answer =
        run_python(&python, "install_config", &json!({"cases": requests}), &root.path().join("sandbox"), "UTC");
    // then
    let mut failures = Vec::new();
    for (i, c) in cases.iter().enumerate() {
        let dir = root.path().join(i.to_string());
        let mut expected = answer["results"][i].clone();
        if let Some(error) = expected.get("error").cloned() {
            expected["error"] = error[1].clone();
        }
        let mut got = rust_answer(&dir.join("repo"), case_env(c, &dir));
        if c.syntax_error {
            let prefix = |v: &Json| v["error"].as_str().map(|m| m.split(": ").next().unwrap_or("").to_owned());
            assert!(
                prefix(&expected).is_some() && prefix(&expected) == prefix(&got),
                "{}: {expected} vs {got}",
                c.name
            );
            expected["error"] = Json::Null;
            got["error"] = Json::Null;
        }
        failures.extend(difference(c.name, &expected, &got));
    }
    assert_none(&failures);
    // Every malformed document and environment value is refused by Python: the
    // fixtures test the errors, not only the defaults.
    let errors = answer["results"].as_array().unwrap().iter().filter(|r| r.get("error").is_some()).count();
    assert!(errors >= malformed_documents().len() + 13, "only {errors} fixtures failed in Python");
    for (variable, key) in answer["setting_keys"].as_object().unwrap() {
        assert_eq!(install_config::setting_key(variable).as_deref(), key.as_str(), "{variable}");
    }
    assert_eq!(answer["setting_keys"].as_object().unwrap().len(), install_config::all_settings().count());
}

/// A data-dir fixture: its name, the files (a trailing `/` makes a directory) and the
/// environment over HOME.
type Layout = (&'static str, Vec<&'static str>, Vec<(&'static str, &'static str)>);

#[test]
fn data_dir_cases_choose_the_same_root_warn_and_fail_as_python() {
    // given
    let Some(python) = python() else { return };
    let root = tempfile::tempdir().unwrap();
    let base = root.path();
    let layouts: Vec<Layout> = vec![
        ("fresh checkout", vec![], vec![]),
        ("override", vec!["repo/meetings/"], vec![("RECOG_DATA_DIR", " ~/data ")]),
        ("relative override", vec![], vec![("RECOG_DATA_DIR", "data")]),
        ("blank override", vec![], vec![("RECOG_DATA_DIR", "  ")]),
        ("legacy meetings", vec!["repo/meetings/"], vec![]),
        ("legacy speakers", vec!["repo/speakers_team.json", "repo/speakers.json"], vec![]),
        ("legacy profile", vec!["repo/user-profile.json"], vec![]),
        ("both roots", vec!["repo/logs/", "home/.recog/privacy.json"], vec![]),
        ("settings lock is not data", vec!["repo/ai-settings.json.lock", "repo/Speakers.json"], vec![]),
        ("home through dot-dot", vec!["repo/meetings/", "home/.recog/logs/"], vec![("HOME", "{dir}/repo/../home/")]),
    ];
    let mut requests = Vec::new();
    for (i, (_, files, env)) in layouts.iter().enumerate() {
        let dir = base.join(i.to_string());
        fs::create_dir_all(dir.join("repo")).unwrap();
        fs::create_dir_all(dir.join("home")).unwrap();
        for file in files {
            let path = dir.join(file);
            if file.ends_with('/') {
                fs::create_dir_all(&path).unwrap();
            } else {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, "{}").unwrap();
            }
        }
        let mut environ: Environ = [("HOME".to_owned(), dir.join("home").to_string_lossy().into_owned())].into();
        for (k, v) in env {
            environ.insert((*k).to_owned(), v.replace("{dir}", &dir.to_string_lossy()));
        }
        requests.push(json!({"repo": dir.join("repo"), "env": environ}));
    }
    // when
    let answer = run_python(&python, "data_dir", &json!({"cases": requests}), &base.join("sandbox"), "UTC");
    // then
    for (i, (name, _, _)) in layouts.iter().enumerate() {
        let request = &requests[i];
        let environ: Environ = serde_json::from_value(request["env"].clone()).unwrap();
        let mut warnings = Vec::new();
        let repo = Path::new(request["repo"].as_str().unwrap());
        let mut got = match data_dir::get_data_dir(repo, &environ, &mut |w| warnings.push(w)) {
            Ok(path) => json!({"path": path}),
            Err(data_dir::DataDirError::Relative(m)) => json!({"error": ["Value", m]}),
            Err(data_dir::DataDirError::Conflict(m)) => json!({"error": ["Runtime", m]}),
        };
        got["stderr"] = json!(warnings);
        assert_same(name, &answer["results"][i], &got);
    }
}

#[test]
fn ws_origins_are_allowed_and_refused_exactly_as_python_decides() {
    // given
    let Some(python) = python() else { return };
    let extra = vec!["https://meet.example.org".to_owned(), "null".to_owned()];
    let origins: Vec<Option<&str>> = vec![
        None,
        Some("https://ktalk.ru"),
        Some("https://example.ktalk.ru"),
        Some("https://a.b.ktalk.ru"),
        Some("https://ktalk.ru:443"),
        Some("https://ktalk.ru:"),
        Some("https://ktalk.ru:0443"),
        Some("https://ktalk.ru:8443"),
        Some("https://ktalk.ru:x"),
        Some("https://ktalk.ru:99999"),
        Some("http://example.ktalk.ru"),
        Some("HTTPS://EXAMPLE.KTALK.RU"),
        Some("https://evil-ktalk.ru"),
        Some("https://ktalk.ru.evil.test"),
        Some("https://app.zoom.us"),
        Some("https://evilzoom.us"),
        Some("https://user@app.zoom.us"),
        Some("https://:pw@app.zoom.us"),
        Some("https://app.zoom.us/"),
        Some("https://app.zoom.us?"),
        Some("https://app.zoom.us#"),
        Some("https://-a.ktalk.ru"),
        Some("https://a-.ktalk.ru"),
        Some("https://a..ktalk.ru"),
        Some("https://.ktalk.ru"),
        Some("https://\u{430}.ktalk.ru"),
        Some("https://a_b.ktalk.ru"),
        Some("https://a b.ktalk.ru"),
        Some("https://a\u{a0}b.ktalk.ru"),
        Some("https://a\u{1c}.ktalk.ru"),
        Some("\u{0}https://ktalk.ru"),
        Some("https://kt\u{7f}alk.ru"),
        Some("https://[::1]"),
        Some("https://[::1"),
        Some("https://[v1.x]"),
        Some("https://\u{212a}talk.ru"),
        Some("https://meet.vas3k.cloud"),
        Some("https://meet.google.com"),
        Some("http://127.0.0.1:9080"),
        Some("http://localhost:9080"),
        Some("http://localhost:9081"),
        Some("https://meet.example.org"),
        Some("https://x.meet.example.org"),
        Some("null"),
        Some(""),
        Some("https://"),
        Some("https:ktalk.ru"),
        Some("//ktalk.ru"),
    ];
    let mut requests = Vec::new();
    for origin in &origins {
        requests.push(json!([origin, extra]));
        requests.push(json!([origin, []]));
    }
    // when
    let answer =
        run_python(&python, "ws_origin", &json!({"cases": requests}), tempfile::tempdir().unwrap().path(), "UTC");
    // then
    let mut i = 0;
    for origin in &origins {
        for list in [extra.clone(), vec![]] {
            assert_eq!(
                answer["results"][i],
                json!(ws_origin::origin_allowed(*origin, &list)),
                "{origin:?} with {list:?}"
            );
            i += 1;
        }
    }
}
