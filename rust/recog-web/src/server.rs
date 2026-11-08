//! The application: its state, the request dispatch and the lifespan (watchers, the
//! meeting catalog, the bridge link) — the port of the module level of
//! `recog/web/server.py`.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock, Weak};

use axum::Router;
use axum::body::Body;
use axum::extract::{FromRequest as _, Request, State, WebSocketUpgrade};
use axum::http::{Method, StatusCode, header};
use axum::response::Response;
use recog_ai::provider::{CliProviderFactory, ProviderFactory};
use recog_ai::settings::AiSettingsStore;
use recog_core::privacy::PrivacyStore;
use recog_core::pyjson::{Map, Value};
use recog_core::user_profile::UserProfileStore;

use crate::ai_provider::AiContext;
use crate::ai_settings::AiSettingsCoordinator;
use crate::bridge_subscriber::{BridgeHandler, BridgeSubscriber, RawSender};
use crate::config::Config;
use crate::live_watcher::LiveWatcher;
use crate::meeting_catalog::{MeetingCatalog, Timings};
use crate::request::{Req, loads_untrusted};
use crate::responses;
use crate::router::{self, Match, WsRoute};
use crate::transcript_watcher::TranscriptWatcher;
use crate::user_profile::ProfileCoordinator;
use crate::util::BoxFuture;
use crate::watch::FsWatcher;
use crate::{json, live_stream, routes, static_files};

/// Frames a browser may send upstream through the live stream.
pub const INBOUND_COPILOT_TYPES: [&str; 3] = ["copilot_chat_message", "copilot_spawn_request", "copilot_stop_request"];
/// Bridge frames kept for tabs that connect later.
pub const COPILOT_DIALOG_TYPES: [&str; 2] = ["copilot_chat_echo", "copilot_reply"];
pub const COPILOT_DIALOG_LIMIT: usize = 200;

/// What a new live-stream client gets after `init`: the last agent-state frame and
/// the recent dialog. Both are lost on a restart, as in Python.
#[derive(Default)]
pub struct CopilotRelay {
    pub primer: Option<String>,
    pub dialog: VecDeque<Value>,
}

/// The debug AI context (`debug_ai_context`).
#[derive(Clone, Debug, Default)]
pub struct DebugAiContext {
    pub segments: Vec<Value>,
    pub user_name: Option<String>,
    pub meeting_id: Option<String>,
    pub meeting_title: Option<Value>,
    pub set_at: Option<String>,
}

pub struct AppState {
    pub config: Arc<Config>,
    pub ai: AiContext,
    pub live_watcher: Arc<LiveWatcher>,
    pub transcript_watcher: Arc<TranscriptWatcher>,
    pub catalog: RwLock<Option<MeetingCatalog>>,
    /// `bridge_subscriber`: what upstream copilot frames are sent through.
    pub subscriber: RwLock<Option<Arc<dyn RawSender>>>,
    pub profile: Arc<ProfileCoordinator>,
    pub ai_settings: Arc<AiSettingsCoordinator>,
    pub privacy: PrivacyStore,
    pub copilot: Mutex<CopilotRelay>,
    pub debug_ai: Mutex<DebugAiContext>,
}

impl AppState {
    /// The state of a server over `config` with the CLI providers.
    #[must_use]
    pub fn new(config: Config) -> Arc<AppState> {
        let factory = Arc::new(CliProviderFactory { env: Some(config.cli_env.clone()) });
        Self::with_factory(config, factory)
    }

    /// The same with another way to make providers (tests).
    #[must_use]
    pub fn with_factory(config: Config, factory: Arc<dyn ProviderFactory>) -> Arc<AppState> {
        let config = Arc::new(config);
        let storage_config = Arc::clone(&config);
        let live_watcher = Arc::new(LiveWatcher::new(Arc::new(move || storage_config.storage())));
        let ai = AiContext {
            data_dir: config.data_dir.clone(),
            prompts_dir: config.prompts_dir.clone(),
            legacy_env: config.legacy_env.clone(),
            factory,
        };
        let watcher = Arc::clone(&live_watcher);
        let broadcast: crate::user_profile::Broadcast = Arc::new(move |raw: String| watcher.broadcast(&raw));
        let profile = Arc::new(ProfileCoordinator::new(
            Arc::new(UserProfileStore::new(&config.data_dir)),
            &config.mic_speaker_name,
            Arc::clone(&broadcast),
        ));
        let ai_store: AiSettingsStore = ai.store();
        let ai_settings = Arc::new(AiSettingsCoordinator::new(Arc::new(ai_store), broadcast));
        Arc::new(AppState {
            privacy: PrivacyStore::new(&config.data_dir),
            config,
            ai,
            live_watcher,
            transcript_watcher: Arc::new(TranscriptWatcher::new()),
            catalog: RwLock::new(None),
            subscriber: RwLock::new(None),
            profile,
            ai_settings,
            copilot: Mutex::new(CopilotRelay::default()),
            debug_ai: Mutex::new(DebugAiContext::default()),
        })
    }

    /// `_forward_bridge_message`: a frame to every live-stream client.
    pub fn forward_bridge_message(&self, raw: &str) {
        self.live_watcher.broadcast(raw);
    }

    #[must_use]
    pub fn catalog(&self) -> Option<MeetingCatalog> {
        self.catalog.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    #[must_use]
    pub fn subscriber(&self) -> Option<Arc<dyn RawSender>> {
        self.subscriber.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    pub fn set_subscriber(&self, subscriber: Option<Arc<dyn RawSender>>) {
        *self.subscriber.write().unwrap_or_else(std::sync::PoisonError::into_inner) = subscriber;
    }

    fn relay(&self) -> std::sync::MutexGuard<'_, CopilotRelay> {
        self.copilot.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `_remember_dialog_frame`: the frame with a fresh `dialog_id`, as it goes out.
    pub fn remember_dialog_frame(&self, frame: &Map) -> String {
        let mut stamped = frame.clone();
        stamped.insert("dialog_id", Value::from(uuid::Uuid::new_v4().simple().to_string()));
        let stamped = Value::Object(stamped);
        let raw = json::unicode(&stamped);
        let mut relay = self.relay();
        relay.dialog.push_back(stamped);
        while relay.dialog.len() > COPILOT_DIALOG_LIMIT {
            relay.dialog.pop_front();
        }
        raw
    }

    /// `_cache_agent_state_frame`.
    pub fn cache_agent_state_frame(&self, raw: &str) {
        let Ok(Value::Object(data)) = loads_untrusted(raw) else { return };
        if data.get("type").and_then(Value::as_str) == Some("copilot_status")
            && data.get("status").and_then(Value::as_str) == Some("agent_state")
        {
            self.relay().primer = Some(raw.to_owned());
        }
    }

    /// The cached agent-state frame.
    #[must_use]
    pub fn agent_state_primer(&self) -> Option<String> {
        self.relay().primer.clone()
    }

    /// The `copilot_history` frame, when there is a dialog.
    #[must_use]
    pub fn dialog_history(&self) -> Option<String> {
        let relay = self.relay();
        if relay.dialog.is_empty() {
            return None;
        }
        let frames = Value::Array(relay.dialog.iter().cloned().collect());
        Some(json::unicode(&json::object([("type", Value::from("copilot_history")), ("frames", frames)])))
    }

    /// `_on_bridge_frame`: acknowledgements to their coordinators, dialog frames
    /// stamped and kept, the agent-state frame cached, then everything to the clients.
    ///
    /// # Errors
    /// A coordinator could not read its file (the subscriber logs it).
    pub async fn on_bridge_frame(&self, raw: String) -> Result<(), String> {
        let frame = match loads_untrusted(&raw) {
            Ok(Value::Object(map)) => Some(map),
            _ => None,
        };
        let kind = frame.as_ref().and_then(|f| f.get("type")).and_then(Value::as_str).map(str::to_owned);
        let mut raw = raw;
        match (kind.as_deref(), &frame) {
            (Some("user_profile_applied" | "user_profile_error"), Some(frame)) => {
                return self.profile.handle_frame(frame).await.map_err(|e| e.to_string());
            }
            (Some("ai_settings_applied" | "ai_settings_error"), Some(frame)) => {
                return self.ai_settings.handle_frame(frame).await.map_err(|e| e.to_string());
            }
            (Some(kind), Some(frame)) if COPILOT_DIALOG_TYPES.contains(&kind) => {
                raw = self.remember_dialog_frame(frame);
            }
            _ => {}
        }
        self.cache_agent_state_frame(&raw);
        self.forward_bridge_message(&raw);
        Ok(())
    }
}

/// The bridge callbacks of the lifespan.
struct WebBridgeHandler {
    state: Weak<AppState>,
}

impl BridgeHandler for WebBridgeHandler {
    fn forward(&self, raw: String) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let Some(state) = self.state.upgrade() else { return };
            if let Err(e) = state.on_bridge_frame(raw).await {
                eprintln!("[bridge-subscriber] forward callback raised: {e}");
            }
        })
    }

    fn connected(&self, send: Arc<dyn RawSender>) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let Some(state) = self.state.upgrade() else { return };
            if let Err(e) = state.profile.connected(Arc::clone(&send)).await {
                eprintln!("[settings] reconnect reconciliation failed: {e}");
            }
            if let Err(e) = state.ai_settings.connected(send).await {
                eprintln!("[settings] reconnect reconciliation failed: {e}");
            }
        })
    }

    fn disconnected(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let Some(state) = self.state.upgrade() else { return };
            if let Err(e) = state.profile.disconnected().await {
                eprintln!("[settings] disconnect state unavailable: {e}");
            }
            if let Err(e) = state.ai_settings.disconnected().await {
                eprintln!("[settings] disconnect state unavailable: {e}");
            }
        })
    }
}

/// What the lifespan started, to stop it again.
pub struct Services {
    catalog: MeetingCatalog,
    subscriber: Arc<BridgeSubscriber>,
    _watcher: Option<FsWatcher>,
}

/// The lifespan's startup: the meetings directory, the file watchers, the catalog and
/// the bridge link.
pub async fn start_services(state: &Arc<AppState>) -> Services {
    let config = &state.config;
    let _ = std::fs::create_dir_all(&config.meetings_dir);
    let catalog =
        MeetingCatalog::new(&config.meetings_dir, &config.external_paths, &config.install, Timings::default());
    let watcher = FsWatcher::start(state, &catalog);
    catalog.start().await;
    *state.catalog.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(catalog.clone());
    let handler = Arc::new(WebBridgeHandler { state: Arc::downgrade(state) });
    let subscriber = Arc::new(BridgeSubscriber::new(&config.bridge_ws_url, handler));
    subscriber.start();
    state.set_subscriber(Some(subscriber.sender()));
    Services { catalog, subscriber, _watcher: watcher }
}

/// The lifespan's shutdown.
pub async fn stop_services(state: &Arc<AppState>, services: Services) {
    services.subscriber.stop().await;
    state.set_subscriber(None);
    services.catalog.stop().await;
    *state.catalog.write().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

/// The axum application.
pub fn app(state: Arc<AppState>) -> Router {
    Router::new().fallback(dispatch).with_state(state)
}

fn quote_path(path: &str) -> String {
    const SAFE: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'_')
        .remove(b'.')
        .remove(b'-')
        .remove(b'~')
        .remove(b':')
        .remove(b'/')
        .remove(b'%')
        .remove(b'#')
        .remove(b'?')
        .remove(b'=')
        .remove(b'@')
        .remove(b'[')
        .remove(b']')
        .remove(b'!')
        .remove(b'$')
        .remove(b'&')
        .remove(b'\'')
        .remove(b'(')
        .remove(b')')
        .remove(b'*')
        .remove(b'+')
        .remove(b',')
        .remove(b';');
    percent_encoding::utf8_percent_encode(path, SAFE).to_string()
}

async fn dispatch(State(state): State<Arc<AppState>>, request: Request) -> Response {
    let raw_path = request.uri().path().to_owned();
    let path = router::decode_path(&raw_path);
    let is_upgrade = request
        .headers()
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    if is_upgrade {
        let Some((route, params)) = router::ws_route(&path) else {
            return responses::plain(StatusCode::FORBIDDEN, "", &[]);
        };
        let headers = request.headers().clone();
        if route == WsRoute::LiveStream
            && let Some(reason) = live_stream::origin_rejection(&headers, &state.config.extra_origins)
        {
            crate::util::log_rate_limited(
                "frontend-ws-origin-reject",
                &format!("[Frontend] Rejected /api/live/stream connection from {reason}"),
            );
            return responses::plain(StatusCode::FORBIDDEN, "", &[]);
        }
        let upgrade = match WebSocketUpgrade::from_request(request, &state).await {
            Ok(upgrade) => upgrade,
            Err(rejection) => return axum::response::IntoResponse::into_response(rejection),
        };
        let meeting_id = params.get("meeting_id").to_owned();
        return match route {
            WsRoute::LiveStream => upgrade.on_upgrade(move |socket| live_stream::live_stream(state, socket)),
            WsRoute::WatchMeeting => {
                upgrade.on_upgrade(move |socket| live_stream::watch_meeting(state, meeting_id, socket))
            }
        };
    }
    let method = request.method().clone();
    let matched = router::route(method.as_str(), &path);
    let query: Vec<(String, String)> =
        request.uri().query().map(|q| form_urlencoded::parse(q.as_bytes()).into_owned().collect()).unwrap_or_default();
    match matched {
        Match::NotFound => responses::status_detail(StatusCode::NOT_FOUND),
        Match::MethodNotAllowed(allow) => responses::method_not_allowed(allow),
        Match::Redirect(twin) => {
            let host = request.headers().get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or_default();
            let query = request.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
            responses::redirect(&format!("http://{host}{}{query}", quote_path(&twin)))
        }
        Match::Static(rest) => {
            if method != Method::GET && method != Method::HEAD {
                return responses::status_detail(StatusCode::METHOD_NOT_ALLOWED);
            }
            let headers = request.headers().clone();
            let how = static_files::Serve { head: method == Method::HEAD, conditional: true };
            static_files::lookup(&state.config.static_dir, &rest)
                .and_then(|file| static_files::serve(&file, &headers, how))
                .unwrap_or_else(|| responses::status_detail(StatusCode::NOT_FOUND))
        }
        Match::Route(route, params) => {
            let (parts, body) = request.into_parts();
            let Ok(body) = axum::body::to_bytes(body, usize::MAX).await else {
                return responses::detail(StatusCode::BAD_REQUEST, "There was an error parsing the body");
            };
            let req = Req { method: method.to_string(), path, params, query, headers: parts.headers, body };
            routes::handle(&state, route, req).await.unwrap_or_else(|error| error)
        }
    }
}

/// Serves the application on a bound listener until `shutdown` resolves, with the
/// lifespan around it.
///
/// # Errors
/// The listener fails.
pub async fn serve(
    state: Arc<AppState>,
    listener: tokio::net::TcpListener,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let services = start_services(&state).await;
    println!("Server started at http://{}:{}", state.config.host, state.config.port);
    let result = axum::serve(listener, app(Arc::clone(&state))).with_graceful_shutdown(shutdown).await;
    stop_services(&state, services).await;
    result
}

/// `index.html` for `/`.
#[must_use]
pub fn root_response(static_dir: &Path, request: &axum::http::HeaderMap) -> Response {
    static_files::serve(
        &static_dir.join("index.html"),
        request,
        static_files::Serve { head: false, conditional: false },
    )
    .unwrap_or_else(responses::internal_error)
}

/// An empty body response, for handlers that only set a status.
#[must_use]
pub fn empty(status: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}
