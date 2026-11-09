//! worker.rs on the inputs tests/differential.rs gave both implementations, with
//! the answers Python gave stated: the inspector URLs a target list may carry, and
//! the worker's command line and exit status.
#![forbid(unsafe_code)]

mod common;

use common::*;
use recog_core::pyjson::Value;
use recog_desktop::inspector::Failure;
use recog_desktop::worker;

fn url_list(url: &str) -> Value {
    Value::Array(vec![recog_desktop::py::obj([("webSocketDebuggerUrl", Value::from(url))])])
}

fn other(message: &str) -> Result<String, Failure> {
    Err(Failure::Other(message.to_owned()))
}

#[test]
fn inspector_urls_when_validated_then_python_answers() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let host = host(&World::new(dir.path()), 9229, free_port(), dir.path());
    let invalid = || Err(Failure::attach("invalid_inspector_endpoint"));
    let ipv6 = || other("ValueError: Invalid IPv6 URL");
    let in_brackets = || other("ValueError: An IPv4 address cannot be in brackets");
    let accepted = |url: &str| Ok(url.to_owned());
    let cases: Vec<(&str, Result<String, Failure>)> = vec![
        ("ws://127.0.0.1:9229/x", accepted("ws://127.0.0.1:9229/x")),
        ("ws://127.0.0.1:9229", accepted("ws://127.0.0.1:9229")),
        ("WS://127.0.0.1:9229/x", accepted("WS://127.0.0.1:9229/x")),
        ("ws://127.0.0.1:09229/x", accepted("ws://127.0.0.1:09229/x")),
        ("ws://127.0.0.1:99999/x", other("ValueError: Port out of range 0-65535")),
        ("ws://127.0.0.1:92a9/x", other("ValueError: Port could not be cast to integer value as '92a9'")),
        ("ws://127.0.0.1:/x", invalid()),
        ("ws://@127.0.0.1:9229/x", accepted("ws://@127.0.0.1:9229/x")),
        ("ws://u:@127.0.0.1:9229/x", invalid()),
        ("ws://[::1]:9229/x", invalid()),
        ("ws://[fe80::1%25eth0]:9229/x", invalid()),
        ("ws://[127.0.0.1]:9229/x", in_brackets()),
        ("ws://[abc]:9229/x", other("ValueError: 'abc' does not appear to be an IPv4 or IPv6 address")),
        ("ws://[::1%]:9229/x", other("ValueError: '::1%' does not appear to be an IPv4 or IPv6 address")),
        ("ws://[v1.x]:9229/x", invalid()),
        ("ws://[v1.]:9229/x", other("ValueError: IPvFuture address is invalid")),
        ("ws://[v.x]:9229/x", other("ValueError: IPvFuture address is invalid")),
        ("ws://x[::1]:9229/x", ipv6()),
        ("ws://[::1]x:9229/x", ipv6()),
        ("ws://[::1:9229/x", ipv6()),
        ("ws://::1]:9229/x", ipv6()),
        ("ws://a[b]@127.0.0.1:9229/x", in_brackets()),
        ("ws://a[b]@127.0.0.1/x", in_brackets()),
        ("  \tws://127.0.0.1:9229/x", accepted("  \tws://127.0.0.1:9229/x")),
        ("ws://127.0.0.1:9229/x\n", accepted("ws://127.0.0.1:9229/x\n")),
        ("ws://127.0.0.1:9229/x?", accepted("ws://127.0.0.1:9229/x?")),
        ("ws://127.0.0.1:9229/x#", accepted("ws://127.0.0.1:9229/x#")),
        ("ws://127.0.0.1:9229/x?a#b", invalid()),
        ("ws:127.0.0.1:9229/x", invalid()),
        ("1ws://127.0.0.1:9229/x", invalid()),
        ("ws://LOCALHOST:9229/x", invalid()),
        ("", invalid()),
    ];
    for (url, expected) in cases {
        // when
        let result = worker::inspector_url(&host, &url_list(url));
        // then
        assert_eq!(result, expected, "{url:?}");
    }
    let lists: Vec<(&str, Result<String, Failure>)> = vec![
        ("[{}]", invalid()),
        (r#"[{"webSocketDebuggerUrl": 5}]"#, other("cannot split a int")),
        ("[5]", other("AttributeError: 'int' object has no attribute 'get'")),
    ];
    for (list, expected) in lists {
        assert_eq!(worker::inspector_url(&host, &obj(list)), expected, "{list}");
    }
}

fn args(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

#[test]
fn worker_command_lines_when_parsed_then_its_argparse_errors() {
    // given
    let cases = [
        ("--root", "argument --root: expected one argument"),
        ("--root /r --target", "argument --target: expected one argument"),
        ("--root /r --target {x", "argument --target: invalid loads value: '{x'"),
        ("--root /r --bogus", "unrecognized arguments: --bogus"),
        ("--recover", "the following arguments are required: --root"),
    ];
    for (line, message) in cases {
        // when
        let error = worker::parse_args(&args(line)).err();
        // then
        assert_eq!(error.as_deref(), Some(message), "{line}");
    }
    let parsed = worker::parse_args(&args("--root=/r --target null")).unwrap();
    assert_eq!(parsed.root, std::path::PathBuf::from("/r"));
    assert_eq!(parsed.target, Some(Value::Null));
    assert!(!parsed.recover);
}

#[tokio::test]
async fn worker_invocations_when_run_then_exit_status_of_the_outcome() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let host = host(&World::new(&dir.path().join("world")), free_port(), free_port(), &home);
    let root = dir.path().join("root");
    let file = dir.path().join("file");
    std::fs::write(&file, "").unwrap();
    let root_arg = root.to_string_lossy().into_owned();
    let file_arg = file.to_string_lossy().into_owned();
    let target = r#"{"pid": 42, "started": 1.0, "executable": "/ktalk"}"#;
    let cases: Vec<(Vec<String>, i32)> = vec![
        (args("--bogus"), 2),
        (vec!["--root".into(), root_arg.clone(), "--target".into(), "[1]".into()], 1),
        (vec!["--root".into(), root_arg.clone(), "--recover".into()], 0),
        (vec!["--root".into(), root_arg.clone(), "--target".into(), "null".into()], 1),
        (vec!["--root".into(), file_arg, "--target".into(), target.into()], 1),
    ];
    for (line, code) in cases {
        // when
        let status = worker::main(&host, &line).await;
        // then
        assert_eq!(status, code, "{line:?}");
    }
    // only the run without a target wrote a status, as an invalid installation
    let status = recog_desktop::storage::read(&root.join("status.json")).unwrap();
    assert_eq!(text(&status, "reason").as_deref(), Some("invalid_installation"));
    assert_eq!(field(&status, "target"), &Value::Null);
}
