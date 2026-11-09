//! The in-process harness of the ported Python tests: an application over a temporary
//! data directory (no lifespan, so no watcher, catalog or bridge link unless a test
//! starts them), requests through the router, a real listener for WebSockets, fake AI
//! providers and a fake bridge sender. The repository's `.env` is never read.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use recog_ai::AiError;
use recog_ai::provider::{AiProvider, BoxFuture, ProviderFactory};
use recog_ai::settings::ResolvedTask;
use recog_core::pyjson::{self, Value};
use recog_web::bridge_subscriber::RawSender;
use recog_web::config::Config;
use recog_web::server::{self, AppState};
use tower::ServiceExt as _;

pub fn repo_root() -> PathBuf {
    std::fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap()
}

pub fn obj(text: &str) -> Value {
    pyjson::loads(text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

pub fn get<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.as_object().unwrap_or_else(|| panic!("not an object: {value:?}")).get(key).unwrap_or(&Value::Null)
}

pub fn text(value: &Value, key: &str) -> String {
    get(value, key).py_str()
}

/// A configuration over `dir`: data in `dir/data`, HOME in `dir/home`, no
/// configuration file, the bridge on a dead port, the repository's UI and prompts.
pub fn config(dir: &Path) -> Config {
    let fake_repo = dir.join("repo");
    std::fs::create_dir_all(&fake_repo).unwrap();
    std::fs::create_dir_all(dir.join("home")).unwrap();
    let environ: HashMap<String, String> = [
        ("HOME", dir.join("home").to_string_lossy().into_owned()),
        ("RECOG_DATA_DIR", dir.join("data").to_string_lossy().into_owned()),
        ("RECOG_CONFIG_FILE", dir.join("config.toml").to_string_lossy().into_owned()),
        ("RECOG_BRIDGE_WS_URL", "ws://127.0.0.1:1".to_owned()),
        ("PATH", dir.join("bin").to_string_lossy().into_owned()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    let mut config = Config::from_environ(&fake_repo, &environ, "127.0.0.1", 0).unwrap();
    config.static_dir = repo_root().join("recog/web/static");
    config.prompts_dir = repo_root().join("recog/prompts");
    config
}

/// What a fake provider answers.
#[derive(Clone)]
pub enum Answer {
    Text(String),
    Fail(String),
}

/// What runs before a fake answer.
pub type Hook = Box<dyn Fn() + Send + Sync>;

/// Records the prompts every provider of a [`FakeFactory`] got, and the settings it
/// was made from.
#[derive(Clone)]
pub struct FakeFactory {
    pub answer: Arc<Mutex<Answer>>,
    pub prompts: Arc<Mutex<Vec<String>>>,
    pub settings: Arc<Mutex<Vec<ResolvedTask>>>,
    /// Runs before each answer (e.g. deleting the meeting meanwhile).
    pub before_answer: Arc<Mutex<Option<Hook>>>,
    /// Providers whose CLI is "not installed": creating them fails with `NotFound`.
    pub missing: Arc<Mutex<Vec<String>>>,
}

impl FakeFactory {
    pub fn new(answer: &str) -> Self {
        Self {
            answer: Arc::new(Mutex::new(Answer::Text(answer.to_owned()))),
            prompts: Arc::new(Mutex::new(Vec::new())),
            settings: Arc::new(Mutex::new(Vec::new())),
            before_answer: Arc::new(Mutex::new(None)),
            missing: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn answer(&self, answer: Answer) {
        *self.answer.lock().unwrap() = answer;
    }

    pub fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }
}

struct FakeProvider {
    key: String,
    model: String,
    factory: FakeFactory,
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
        None
    }
    fn complete<'a>(&'a self, prompt: &'a str) -> BoxFuture<'a, Result<String, AiError>> {
        self.factory.prompts.lock().unwrap().push(prompt.to_owned());
        if let Some(hook) = self.factory.before_answer.lock().unwrap().as_ref() {
            hook();
        }
        let answer = self.factory.answer.lock().unwrap().clone();
        Box::pin(async move {
            match answer {
                Answer::Text(text) => Ok(text),
                Answer::Fail(message) => Err(AiError::Provider(message)),
            }
        })
    }
}

impl ProviderFactory for FakeFactory {
    fn create(
        &self,
        settings: &ResolvedTask,
        _task: &str,
        provider: Option<&str>,
    ) -> Result<Box<dyn AiProvider>, AiError> {
        self.settings.lock().unwrap().push(settings.clone());
        let key = provider.unwrap_or(settings.provider()).to_owned();
        if self.missing.lock().unwrap().contains(&key) {
            return Err(AiError::NotFound(format!("{key} CLI not found")));
        }
        let model =
            if key == "claude" { "model-x".to_owned() } else { settings.models.get(&key).cloned().unwrap_or_default() };
        Ok(Box::new(FakeProvider { key, model, factory: self.clone() }))
    }
}

/// An application over a temporary directory.
pub struct App {
    pub dir: tempfile::TempDir,
    pub state: Arc<AppState>,
    pub factory: FakeFactory,
}

impl App {
    pub fn new() -> App {
        Self::with(|_| {})
    }

    pub fn with(change: impl FnOnce(&mut Config)) -> App {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config(dir.path());
        change(&mut config);
        let factory = FakeFactory::new("Ответ");
        let state = AppState::with_factory(config, Arc::new(factory.clone()));
        App { dir, state, factory }
    }

    pub fn data(&self) -> PathBuf {
        self.state.config.data_dir.clone()
    }

    pub fn meetings(&self) -> PathBuf {
        self.state.config.meetings_dir.clone()
    }

    /// One request through the router: status and body.
    pub async fn call(&self, method: &str, path: &str, body: Option<&str>) -> (StatusCode, Vec<u8>) {
        let mut builder = Request::builder().method(method).uri(path);
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        let request = builder.body(body.map_or_else(Body::empty, |b| Body::from(b.to_owned()))).unwrap();
        let response = server::app(Arc::clone(&self.state)).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes().to_vec();
        (status, bytes)
    }

    /// One request; the body as JSON.
    pub async fn json(&self, method: &str, path: &str, body: Option<&str>) -> (StatusCode, Value) {
        let (status, bytes) = self.call(method, path, body).await;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        (status, pyjson::loads(&text).unwrap_or(Value::from(text)))
    }

    /// The full response of a request with headers.
    pub async fn response(&self, method: &str, path: &str, headers: &[(&str, &str)]) -> axum::response::Response {
        let mut builder = Request::builder().method(method).uri(path);
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        server::app(Arc::clone(&self.state)).oneshot(builder.body(Body::empty()).unwrap()).await.unwrap()
    }

    /// The application on a real listener (for WebSockets); its port.
    pub async fn serve(&self) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = server::app(Arc::clone(&self.state));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        port
    }
}

/// A bridge link that records what is sent and answers a fixed result.
pub struct FakeBridge {
    pub tx: tokio::sync::mpsc::UnboundedSender<Value>,
    pub result: bool,
}

impl RawSender for FakeBridge {
    fn send(&self, raw: String) -> BoxFuture<'_, bool> {
        let _ = self.tx.send(obj(&raw));
        let result = self.result;
        Box::pin(async move { result })
    }
}

/// A fake bridge link and the frames it receives.
pub fn fake_bridge(result: bool) -> (Arc<dyn RawSender>, tokio::sync::mpsc::UnboundedReceiver<Value>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    (Arc::new(FakeBridge { tx, result }), rx)
}

/// The next frame within two seconds.
pub async fn next(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Value>) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await.expect("a frame within 2 s").expect("open")
}

/// Records broadcast frames.
pub fn recorder() -> (recog_web::user_profile::Broadcast, Arc<Mutex<Vec<Value>>>) {
    let frames = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&frames);
    (Arc::new(move |raw: String| sink.lock().unwrap().push(obj(&raw))), frames)
}

pub fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}
