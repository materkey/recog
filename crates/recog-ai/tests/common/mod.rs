//! Test doubles shared by the integration tests: a fake provider and factory, and stub
//! `claude`/`codex` executables. Nothing here runs a real CLI or touches `~/.recog`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use recog_ai::error::{AiError, Result};
use recog_ai::provider::{AiProvider, BoxFuture, CliEnv, ProviderFactory};
use recog_ai::settings::ResolvedTask;

/// What a fake provider answers.
#[derive(Clone)]
pub enum Answer {
    Text(String),
    Fail(String),
}

/// A provider that records prompts and answers fixed text.
pub struct FakeProvider {
    pub key: String,
    pub model: String,
    pub effort: Option<String>,
    pub answer: Answer,
    pub prompts: Arc<Mutex<Vec<String>>>,
}

impl AiProvider for FakeProvider {
    fn key(&self) -> &str {
        &self.key
    }

    fn name(&self) -> String {
        format!("Fake ({})", self.model)
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn reasoning_effort(&self) -> Option<&str> {
        self.effort.as_deref()
    }

    fn complete<'a>(&'a self, prompt: &'a str) -> BoxFuture<'a, Result<String>> {
        self.prompts.lock().unwrap().push(prompt.to_owned());
        let answer = self.answer.clone();
        Box::pin(async move {
            match answer {
                Answer::Text(text) => Ok(text),
                Answer::Fail(message) => Err(AiError::Provider(message)),
            }
        })
    }
}

/// A factory of [`FakeProvider`]s that records each call's settings and task.
pub struct FakeFactory {
    pub key: String,
    pub model: String,
    pub effort: Option<String>,
    pub answer: Mutex<Answer>,
    pub prompts: Arc<Mutex<Vec<String>>>,
    pub calls: Mutex<Vec<(ResolvedTask, String)>>,
}

impl FakeFactory {
    pub fn new(key: &str, model: &str, effort: Option<&str>, answer: &str) -> Arc<FakeFactory> {
        Arc::new(FakeFactory {
            key: key.to_owned(),
            model: model.to_owned(),
            effort: effort.map(str::to_owned),
            answer: Mutex::new(Answer::Text(answer.to_owned())),
            prompts: Arc::default(),
            calls: Mutex::default(),
        })
    }

    pub fn failing(key: &str, model: &str, message: &str) -> Arc<FakeFactory> {
        let factory = FakeFactory::new(key, model, Some("xhigh"), "");
        *factory.answer.lock().unwrap() = Answer::Fail(message.to_owned());
        factory
    }

    pub fn set_answer(&self, answer: &str) {
        *self.answer.lock().unwrap() = Answer::Text(answer.to_owned());
    }

    pub fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }

    pub fn calls(&self) -> Vec<(ResolvedTask, String)> {
        self.calls.lock().unwrap().clone()
    }
}

impl ProviderFactory for FakeFactory {
    fn create(&self, settings: &ResolvedTask, task: &str, _provider: Option<&str>) -> Result<Box<dyn AiProvider>> {
        self.calls.lock().unwrap().push((settings.clone(), task.to_owned()));
        Ok(Box::new(FakeProvider {
            key: self.key.clone(),
            model: self.model.clone(),
            effort: self.effort.clone(),
            answer: self.answer.lock().unwrap().clone(),
            prompts: self.prompts.clone(),
        }))
    }
}

/// A factory whose providers answer by key, like several installed CLIs: a key without
/// an answer is a CLI that is not installed. Records the keys asked, in order.
pub struct KeyedFactory {
    pub answers: Vec<(&'static str, Answer)>,
    pub asked: Mutex<Vec<String>>,
}

impl KeyedFactory {
    pub fn new(answers: Vec<(&'static str, Answer)>) -> KeyedFactory {
        KeyedFactory { answers, asked: Mutex::default() }
    }

    pub fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl ProviderFactory for KeyedFactory {
    fn create(&self, settings: &ResolvedTask, _task: &str, provider: Option<&str>) -> Result<Box<dyn AiProvider>> {
        let key = provider.unwrap_or(settings.provider()).to_owned();
        self.asked.lock().unwrap().push(key.clone());
        let (_, answer) =
            self.answers.iter().find(|(name, _)| *name == key).ok_or_else(|| AiError::NotFound(key.clone()))?;
        Ok(Box::new(FakeProvider {
            key: key.clone(),
            model: format!("{key}-model"),
            effort: None,
            answer: answer.clone(),
            prompts: Arc::default(),
        }))
    }
}

/// A stub CLI in `dir`: records its argv (one per line) and stdin next to itself,
/// prints `stdout` and `stderr`, exits with `code`.
pub fn shell_stub(dir: &Path, name: &str, stdout: &str, stderr: &str, code: i32) -> PathBuf {
    let data = dir.join(format!("{name}.data"));
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("stdout"), stdout).unwrap();
    std::fs::write(data.join("stderr"), stderr).unwrap();
    let script = format!(
        "#!/bin/sh\nd='{}'\nprintf '%s\\n' \"$@\" > \"$d/argv\"\ncat > \"$d/stdin\"\ncat \"$d/stdout\"\ncat \"$d/stderr\" >&2\nexit {code}\n",
        data.display()
    );
    let path = dir.join(name);
    write_executable(&path, &script);
    path
}

/// Writes an executable through a child `sh`, so the test process never holds it open
/// for writing: on Linux a test forking in another thread at that moment would inherit
/// the descriptor and make running the file fail with ETXTBSY ("Text file busy").
fn write_executable(path: &Path, content: &str) {
    use std::io::Write as _;
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(content.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success(), "could not write {}", path.display());
}

/// The argv a [`shell_stub`] recorded, without argv[0].
pub fn stub_argv(dir: &Path, name: &str) -> Vec<String> {
    let text = std::fs::read_to_string(dir.join(format!("{name}.data/argv"))).unwrap();
    text.lines().map(str::to_owned).collect()
}

pub fn stub_stdin(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(format!("{name}.data/stdin"))).unwrap()
}

/// Whether `resolve_cli`'s fixed fallbacks hold a real CLI of this name: a test of a
/// missing CLI must not run then, or it would start the real one.
pub fn system_cli_exists(name: &str) -> bool {
    ["/opt/homebrew/bin", "/usr/local/bin"].iter().any(|dir| Path::new(dir).join(name).exists())
}

/// An environment with only the stub directory on PATH and a temporary HOME.
pub fn stub_env(bin: &Path, home: &Path) -> CliEnv {
    CliEnv::from_pairs([("PATH", format!("{}:/usr/bin:/bin", bin.display())), ("HOME", home.display().to_string())])
}
