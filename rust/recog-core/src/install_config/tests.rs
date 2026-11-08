//! The cases of tests/test_install_config.py, against the port.

use super::*;

const HOSTS: &str =
    "[conferences]\njitsi = [\"https://meet.example.org\"]\nmatrix = [\"https://element.example.org\"]\n";

fn quiet() -> impl FnMut(String) {
    |_| {}
}

fn parse(text: &str) -> Result<InstallConfig, String> {
    let table: toml::Table = toml::from_str(text).unwrap();
    parse_install_config(&Doc::from(toml::Value::Table(table)))
}

fn environ(pairs: &[(&str, &str)]) -> Environ {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

#[test]
fn no_files_when_loading_then_only_builtin_hosts_remain() {
    // given
    let dir = tempfile::tempdir().unwrap();
    // when
    let config =
        load_install_config(&dir.path().join("config.toml"), &dir.path().join("hosts.json"), &mut quiet()).unwrap();
    // then
    assert_eq!(config, InstallConfig::default());
}

#[test]
fn config_file_when_loading_then_its_settings_are_returned() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        format!("{HOSTS}[sources.aliases]\nold-meet = \"jitsi\"\n[sources.labels]\nimporter = \"Phone Importer\"\n"),
    )
    .unwrap();
    // when
    let config = load_install_config(&path, &dir.path().join("absent.json"), &mut quiet()).unwrap();
    // then
    assert_eq!(config.jitsi, vec!["https://meet.example.org"]);
    assert_eq!(config.matrix, vec!["https://element.example.org"]);
    assert_eq!(config.source_aliases, vec![("old-meet".to_owned(), "jitsi".to_owned())]);
    assert_eq!(config.source_labels, vec![("importer".to_owned(), "Phone Importer".to_owned())]);
    assert_eq!(config.canonical_source(Some("old-meet")), Some("jitsi"));
    assert_eq!(config.canonical_source(Some("ktalk")), Some("ktalk"));
    assert_eq!(config.canonical_source(None), None);
    assert_eq!(
        config.hosts_json(),
        r#"{"jitsi": ["https://meet.example.org"], "matrix": ["https://element.example.org"]}"#
    );
}

#[test]
fn only_legacy_hosts_file_when_loading_then_it_is_read_with_a_warning() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("conference-hosts.json");
    std::fs::write(&legacy, r#"{"jitsi": ["https://meet.example.org"], "source_aliases": {"old-meet": "jitsi"}}"#)
        .unwrap();
    let mut warnings = Vec::new();
    // when
    let config = load_install_config(&dir.path().join("config.toml"), &legacy, &mut |w| warnings.push(w)).unwrap();
    // then
    assert_eq!(config.jitsi, vec!["https://meet.example.org"]);
    assert_eq!(config.source_aliases, vec![("old-meet".to_owned(), "jitsi".to_owned())]);
    assert!(warnings[0].contains("config.toml") && warnings[0].contains("deprecated"));
}

#[test]
fn both_files_when_loading_then_rejected() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (path, legacy) = (dir.path().join("config.toml"), dir.path().join("conference-hosts.json"));
    std::fs::write(&path, HOSTS).unwrap();
    std::fs::write(&legacy, "{}").unwrap();
    // when
    let error = load_install_config(&path, &legacy, &mut quiet()).unwrap_err();
    // then
    assert!(error.ends_with("delete conference-hosts.json"), "{error}");
}

#[test]
fn malformed_toml_when_loading_then_rejected_with_the_path() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[conferences\n").unwrap();
    // when
    let error = load_install_config(&path, &dir.path().join("absent.json"), &mut quiet()).unwrap_err();
    // then
    assert!(error.starts_with(&format!("{}: ", path.display())), "{error}");
    assert!(error.contains("(at line 1, column"), "{error}");
}

#[test]
fn default_location_when_resolving_then_xdg_config_home_is_used() {
    // given
    let repo = Path::new("/repo");
    // when / then
    assert_eq!(
        config_path(repo, &environ(&[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/home/u")])),
        PathBuf::from("/xdg/recog/config.toml")
    );
    assert_eq!(config_path(repo, &environ(&[("HOME", "/home/u")])), PathBuf::from("/home/u/.config/recog/config.toml"));
    assert_eq!(
        config_path(repo, &environ(&[("XDG_CONFIG_HOME", ""), ("HOME", "/home/u")])),
        PathBuf::from("/home/u/.config/recog/config.toml")
    );
}

#[test]
fn explicit_location_when_resolving_then_it_wins() {
    // given
    let repo = Path::new("/repo");
    // when / then
    assert_eq!(
        config_path(repo, &environ(&[("RECOG_CONFIG_FILE", "~/elsewhere.toml"), ("HOME", "/home/u")])),
        PathBuf::from("/home/u/elsewhere.toml")
    );
    assert_eq!(
        config_path(repo, &environ(&[("RECOG_CONFIG_FILE", "relative.toml")])),
        PathBuf::from("/repo/relative.toml")
    );
    assert_eq!(legacy_hosts_path(repo, &environ(&[])), PathBuf::from("/repo/.local/conference-hosts.json"));
}

#[test]
fn settings_when_parsing_then_sections_hold_typed_values() {
    // given / when
    let config = parse(
        "[asr]\nbackend = \"remote\"\nremote_timeout_s = 20\ncircuit_fails = 4\n[live]\ndedup_max_gap_s = 2.5\nzoom_app = true\n\
         [copilot]\nautospawn = false\n[web]\nexternal_meetings_paths = [\"/a/meetings:a\", \"/b/meetings\"]\n",
    )
    .unwrap();
    // then
    assert_eq!(config.asr.backend, "remote");
    assert!((config.asr.remote_timeout_s - 20.0).abs() < f64::EPSILON);
    assert_eq!(config.asr.circuit_fails, 4);
    assert_eq!(config.asr.remote_model, "gigaam");
    assert!((config.live.dedup_max_gap_s - 2.5).abs() < f64::EPSILON);
    assert!(config.live.zoom_app);
    assert!(!config.copilot.autospawn);
    assert_eq!(config.web.external_meetings_paths, vec!["/a/meetings:a", "/b/meetings"]);
}

#[test]
fn setting_keys_when_naming_a_variable_then_its_table_and_key_are_given() {
    // given / when / then
    assert_eq!(setting_key("RECOG_COPILOT_AUTOSPAWN").as_deref(), Some("[copilot] autospawn"));
    assert_eq!(setting_key("ASR_PENDING_BUFFER_CAP_S").as_deref(), Some("[asr] pending_buffer_cap_s"));
    assert_eq!(setting_key("NOT_A_SETTING"), None);
    assert_eq!(all_settings().count(), 24);
}

#[test]
fn environment_when_overlaid_then_it_wins_and_conflicts_with_the_file_are_named() {
    // given
    let config = parse("[asr]\nbackend = \"remote\"\nremote_url = \"https://a\"\nremote_timeout_s = 20\n").unwrap();
    let environ = environ(&[
        ("RECOG_ASR_BACKEND", "local"),
        ("RECOG_ASR_REMOTE_URL", " https://a "),
        ("RECOG_ASR_REMOTE_MODEL", "big"),
        ("RECOG_ZOOM_APP", "yes"),
        ("RECOG_EXTRA_WS_ORIGINS", " https://a.test , ,https://b.test"),
        ("RECOG_TORCH_THREADS", ""),
    ]);
    // when
    let (resolved, overridden) = with_environment(&config, &environ).unwrap();
    // then
    assert_eq!(resolved.asr.backend, "local");
    assert_eq!(resolved.asr.remote_url, "https://a");
    assert_eq!(resolved.asr.remote_model, "big");
    assert!((resolved.asr.remote_timeout_s - 20.0).abs() < f64::EPSILON);
    assert_eq!(resolved.asr.torch_threads, None);
    assert!(resolved.live.zoom_app);
    assert_eq!(resolved.web.extra_ws_origins, vec!["https://a.test", "https://b.test"]);
    // The same value as the file is no conflict; a variable the file left unset neither.
    assert_eq!(overridden, vec!["RECOG_ASR_BACKEND"]);
    assert_eq!(config.asr.backend, "remote");
}

#[test]
fn malformed_environment_value_when_overlaid_then_rejected_by_name() {
    for (variable, raw, message) in [
        (
            "RECOG_ZOOM_APP",
            "maybe",
            "RECOG_ZOOM_APP: RECOG_ZOOM_APP='maybe' is not one of 1/0, true/false, yes/no, on/off",
        ),
        ("RECOG_TORCH_THREADS", "many", "RECOG_TORCH_THREADS: invalid literal for int() with base 10: 'many'"),
        ("RECOG_TORCH_THREADS", "0", "RECOG_TORCH_THREADS: RECOG_TORCH_THREADS must be a positive integer"),
        ("RECOG_ASR_REMOTE_TIMEOUT_S", "soon", "RECOG_ASR_REMOTE_TIMEOUT_S: could not convert string to float: 'soon'"),
    ] {
        // given / when
        let error = with_environment(&InstallConfig::default(), &environ(&[(variable, raw)])).unwrap_err();
        // then
        assert_eq!(error, message);
    }
}

#[test]
fn invalid_config_when_parsing_then_rejected() {
    for text in [
        "[other]\n",
        "[conferences]\nother = []\n",
        "[conferences]\njitsi = \"https://meet.example.org\"\n",
        "[conferences]\njitsi = [false]\n",
        "[conferences]\njitsi = [\"http://meet.example.org\"]\n",
        "[conferences]\nmatrix = [\"https://element.example.org:443\"]\n",
        "[conferences]\nmatrix = [\"https://MEET.example.org\"]\n",
        "[conferences]\nmatrix = [\"https://127.1\"]\n",
        "[conferences]\njitsi = [\"https://app.zoom.us\"]\n",
        "[conferences]\njitsi = [\"https://same.example.org\"]\nmatrix = [\"https://same.example.org\"]\n",
        "[sources.aliases]\nktalk = \"matrix\"\n",
        "[sources.labels]\nimporter = \"\"\n",
        "[asr]\nremote_timeout_s = true\n",
        "[asr]\ncircuit_fails = 3.5\n",
        "[asr]\ntorch_threads = 0\n",
        "[live]\nzoom_app = \"1\"\n",
        "[web]\nextra_ws_origins = [1]\n",
    ] {
        assert!(parse(text).is_err(), "{text}");
    }
}

#[test]
fn canonical_hostnames_and_ipv4_when_parsing_then_accepted_as_written() {
    // given / when
    let config = parse(
        "[conferences]\njitsi = [\"https://evilzoom.us\", \"https://evilktalk.ru\", \"https://meet-1.xn--e1afmkfd.xn--p1ai\", \"https://127.0.0.1\"]\n",
    )
    .unwrap();
    // then
    assert_eq!(config.jitsi.len(), 4);
    assert_eq!(config.jitsi[3], "https://127.0.0.1");
}

#[test]
fn shipped_example_when_every_setting_is_uncommented_then_it_parses_and_covers_all() {
    // given
    let example =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/config.example.toml"))
            .unwrap();
    let uncommented: Vec<&str> =
        example
            .lines()
            .map(|line| {
                if line.starts_with("# ") && line.contains(" = ") && !line.contains("RECOG") {
                    &line[2..]
                } else {
                    line
                }
            })
            .collect();
    // when
    let config = parse(&uncommented.join("\n")).unwrap();
    // then
    let all: BTreeSet<String> = all_settings().map(|s| s.env.to_owned()).collect();
    assert_eq!(config.from_file, all);
    assert!(!config.jitsi.is_empty() && !config.matrix.is_empty());
    assert!(!config.source_aliases.is_empty() && !config.source_labels.is_empty());
}

#[test]
fn env_file_when_loaded_then_unset_variables_are_filled_without_overriding() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let env_file = dir.path().join(".env");
    std::fs::write(
        &env_file,
        "# comment\n\nRECOG_TEST_PLAIN=one\nRECOG_TEST_QUOTED=\"two words\"\nRECOG_TEST_SINGLE='three'\n\
         RECOG_TEST_KEPT=from-file\nnot a setting\n=orphan\n",
    )
    .unwrap();
    let mut environ = environ(&[("RECOG_TEST_KEPT", "from-process")]);
    // when
    load_dotenv(&env_file, &mut environ);
    load_dotenv(&dir.path().join("absent.env"), &mut environ);
    // then
    assert_eq!(
        environ,
        self::environ(&[
            ("RECOG_TEST_KEPT", "from-process"),
            ("RECOG_TEST_PLAIN", "one"),
            ("RECOG_TEST_QUOTED", "two words"),
            ("RECOG_TEST_SINGLE", "three"),
        ])
    );
}

#[test]
fn a_file_value_the_environment_changes_when_loading_then_a_notice_names_both() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[live]\ndedup_max_gap_s = 3.5\n[asr]\nremote_model = \"big\"\n").unwrap();
    let mut environ = environ(&[
        ("RECOG_CONFIG_FILE", path.to_str().unwrap()),
        ("RECOG_CONFERENCE_HOSTS_FILE", "/nonexistent/conference-hosts.json"),
        ("RECOG_ASR_REMOTE_MODEL", "small"),
    ]);
    let mut warnings = Vec::new();
    // when
    let config = load(dir.path(), &mut environ, &mut |w| warnings.push(w)).unwrap();
    // then
    assert!((config.live.dedup_max_gap_s - 3.5).abs() < f64::EPSILON);
    assert_eq!(config.asr.remote_model, "small");
    assert_eq!(
        warnings,
        vec![
            "recog: RECOG_ASR_REMOTE_MODEL from the environment or .env overrides [asr] remote_model of config.toml; keep only one of them"
        ]
    );
}
