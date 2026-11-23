//! Routing as Starlette routes a FastAPI app: on the percent-decoded path, routes in
//! registration order, the first full match wins, a path that matched with another
//! method answers 405 with that route's `Allow`, and an unmatched path whose
//! trailing-slash twin matches is redirected (307). `HEAD` is not added to `GET`
//! routes (FastAPI's `APIRoute`), only the static mount serves it.
//!
//! # Routes
//!
//! The route list of `recog/web/server.py`, in its order:
//!
//! | Method | Path |
//! |---|---|
//! | GET | `/api/desktop/status` |
//! | GET | `/api/ai-settings` |
//! | PUT | `/api/ai-settings/models` |
//! | PUT | `/api/ai-settings/access` |
//! | PUT | `/api/ai-settings/tasks/{task_id}` |
//! | DELETE | `/api/ai-settings/tasks/{task_id}?revision=` |
//! | GET, PUT | `/api/privacy` |
//! | GET, PUT, DELETE | `/api/user-profile` |
//! | GET | `/api/meetings` |
//! | GET | `/api/search?q=` |
//! | GET, DELETE | `/api/meetings/{meeting_id}` |
//! | GET | `/api/meetings/{meeting_id}/transcript` |
//! | GET | `/api/prompts` |
//! | GET | `/api/meetings/{meeting_id}/increments` |
//! | GET | `/api/meetings/{meeting_id}/increments/{prompt_id}` |
//! | GET | `/api/meetings/{meeting_id}/increments/{prompt_id}/{version}` |
//! | POST | `/api/meetings/{meeting_id}/increments/{prompt_id}/generate?force=&participant_id=` |
//! | POST | `/api/meetings/{meeting_id}/generate-title` |
//! | GET | `/api/live/status` |
//! | GET | `/api/pcm-diagnostics` |
//! | GET | `/api/debug` |
//! | GET, POST, DELETE | `/api/debug/ai-context` (GET: `?include_segments=&limit=`) |
//! | POST | `/api/meetings/{meeting_id}/analyze` |
//! | POST | `/api/meetings/{meeting_id}/analyze-multi` |
//! | GET | `/api/meetings/{meeting_id}/analyze-stream?question=&debug=` (Server-Sent Events) |
//! | GET, DELETE, POST | `/api/meetings/{meeting_id}/chat` |
//! | GET | `/api/meetings/{meeting_id}/audio/{segment_index}?track=` |
//! | GET | `/` |
//! | WebSocket | `/api/meetings/{meeting_id}/watch` |
//! | WebSocket | `/api/live/stream` (Origin allowlist) |
//! | GET, HEAD | `/static/{path}` |
//!
//! FastAPI's generated `/openapi.json`, `/docs`, `/docs/oauth2-redirect` and `/redoc`
//! are not served.

use percent_encoding::percent_decode_str;

/// Every HTTP route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    DesktopStatus,
    Readiness,
    GetAiSettings,
    SaveAiModels,
    SaveAiAccess,
    SaveAiTask,
    ResetAiTask,
    GetPrivacy,
    SavePrivacy,
    GetUserProfile,
    SaveUserProfile,
    ClearUserProfile,
    ListMeetings,
    Search,
    GetMeeting,
    GetTranscript,
    DeleteMeeting,
    ListPrompts,
    ListIncrements,
    ListPromptIncrements,
    GetIncrement,
    GenerateIncrement,
    GenerateTitle,
    LiveStatus,
    PcmDiagnostics,
    Debug,
    GetDebugAiContext,
    SetDebugAiContext,
    ClearDebugAiContext,
    Analyze,
    AnalyzeMulti,
    AnalyzeStream,
    GetChat,
    ClearChat,
    AskChat,
    SegmentAudio,
    Root,
}

/// WebSocket routes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WsRoute {
    WatchMeeting,
    LiveStream,
}

const HTTP_ROUTES: &[(&str, &str, Route)] = &[
    ("GET", "/api/desktop/status", Route::DesktopStatus),
    ("GET", "/api/ai-settings", Route::GetAiSettings),
    ("PUT", "/api/ai-settings/models", Route::SaveAiModels),
    ("PUT", "/api/ai-settings/access", Route::SaveAiAccess),
    ("PUT", "/api/ai-settings/tasks/{task_id}", Route::SaveAiTask),
    ("DELETE", "/api/ai-settings/tasks/{task_id}", Route::ResetAiTask),
    ("GET", "/api/privacy", Route::GetPrivacy),
    ("PUT", "/api/privacy", Route::SavePrivacy),
    ("GET", "/api/user-profile", Route::GetUserProfile),
    ("PUT", "/api/user-profile", Route::SaveUserProfile),
    ("DELETE", "/api/user-profile", Route::ClearUserProfile),
    ("GET", "/api/meetings", Route::ListMeetings),
    ("GET", "/api/search", Route::Search),
    ("GET", "/api/meetings/{meeting_id}", Route::GetMeeting),
    ("GET", "/api/meetings/{meeting_id}/transcript", Route::GetTranscript),
    ("DELETE", "/api/meetings/{meeting_id}", Route::DeleteMeeting),
    ("GET", "/api/prompts", Route::ListPrompts),
    ("GET", "/api/meetings/{meeting_id}/increments", Route::ListIncrements),
    ("GET", "/api/meetings/{meeting_id}/increments/{prompt_id}", Route::ListPromptIncrements),
    ("GET", "/api/meetings/{meeting_id}/increments/{prompt_id}/{version}", Route::GetIncrement),
    ("POST", "/api/meetings/{meeting_id}/increments/{prompt_id}/generate", Route::GenerateIncrement),
    ("POST", "/api/meetings/{meeting_id}/generate-title", Route::GenerateTitle),
    ("GET", "/api/live/status", Route::LiveStatus),
    ("GET", "/api/pcm-diagnostics", Route::PcmDiagnostics),
    ("GET", "/api/readiness", Route::Readiness),
    ("GET", "/api/debug", Route::Debug),
    ("GET", "/api/debug/ai-context", Route::GetDebugAiContext),
    ("POST", "/api/debug/ai-context", Route::SetDebugAiContext),
    ("DELETE", "/api/debug/ai-context", Route::ClearDebugAiContext),
    ("POST", "/api/meetings/{meeting_id}/analyze", Route::Analyze),
    ("POST", "/api/meetings/{meeting_id}/analyze-multi", Route::AnalyzeMulti),
    ("GET", "/api/meetings/{meeting_id}/analyze-stream", Route::AnalyzeStream),
    ("GET", "/api/meetings/{meeting_id}/chat", Route::GetChat),
    ("DELETE", "/api/meetings/{meeting_id}/chat", Route::ClearChat),
    ("POST", "/api/meetings/{meeting_id}/chat", Route::AskChat),
    ("GET", "/api/meetings/{meeting_id}/audio/{segment_index}", Route::SegmentAudio),
    ("GET", "/", Route::Root),
];

const WS_ROUTES: &[(&str, WsRoute)] =
    &[("/api/meetings/{meeting_id}/watch", WsRoute::WatchMeeting), ("/api/live/stream", WsRoute::LiveStream)];

/// The path parameters of a match, by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Params(Vec<(String, String)>);

impl Params {
    #[must_use]
    pub fn get(&self, name: &str) -> &str {
        self.0.iter().find(|(k, _)| k == name).map_or("", |(_, v)| v.as_str())
    }
}

/// How an HTTP request is routed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Match {
    Route(Route, Params),
    /// The static mount, with the path under it.
    Static(String),
    /// The path matched a route with other methods.
    MethodNotAllowed(&'static str),
    /// No route; the trailing-slash twin matches.
    Redirect(String),
    NotFound,
}

/// `scope["path"]`: the request path percent-decoded, invalid UTF-8 replaced.
#[must_use]
pub fn decode_path(raw: &str) -> String {
    percent_decode_str(raw).decode_utf8_lossy().into_owned()
}

/// A path against a template: `{name}` takes one non-empty segment.
fn match_template(template: &str, path: &str) -> Option<Params> {
    let mut params = Vec::new();
    let mut rest = path;
    let mut parts = template.split('{');
    let first = parts.next().unwrap_or_default();
    rest = rest.strip_prefix(first)?;
    for part in parts {
        let (name, literal) = part.split_once('}')?;
        let end = rest.find('/').unwrap_or(rest.len());
        let value = &rest[..end];
        if value.is_empty() {
            return None;
        }
        params.push((name.to_owned(), value.to_owned()));
        rest = rest[end..].strip_prefix(literal)?;
    }
    rest.is_empty().then_some(Params(params))
}

fn match_static(path: &str) -> Option<String> {
    path.strip_prefix("/static/").map(str::to_owned)
}

/// Which route serves `method` on the decoded `path`.
#[must_use]
pub fn route(method: &str, path: &str) -> Match {
    let mut partial = None;
    for (route_method, template, route) in HTTP_ROUTES {
        if let Some(params) = match_template(template, path) {
            if *route_method == method {
                return Match::Route(*route, params);
            }
            partial.get_or_insert(*route_method);
        }
    }
    if let Some(rest) = match_static(path)
        && partial.is_none()
    {
        return Match::Static(rest);
    }
    if let Some(allow) = partial {
        return Match::MethodNotAllowed(allow);
    }
    redirect_twin(path).map_or(Match::NotFound, Match::Redirect)
}

/// The path with its trailing slash added or removed, when a route serves that
/// twin; never for `/`.
fn redirect_twin(path: &str) -> Option<String> {
    if path == "/" {
        return None;
    }
    let twin = if path.ends_with('/') { path.trim_end_matches('/').to_owned() } else { format!("{path}/") };
    let served = HTTP_ROUTES.iter().any(|(_, template, _)| match_template(template, &twin).is_some())
        || match_static(&twin).is_some();
    served.then_some(twin)
}

/// Which WebSocket route serves the decoded `path`.
#[must_use]
pub fn ws_route(path: &str) -> Option<(WsRoute, Params)> {
    WS_ROUTES.iter().find_map(|(template, route)| match_template(template, path).map(|p| (*route, p)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_given_starlette_rules_when_routed_then_first_match_method_check_and_slash_redirect() {
        // given / when / then
        assert!(matches!(route("GET", "/api/meetings"), Match::Route(Route::ListMeetings, _)));
        assert_eq!(route("HEAD", "/api/meetings"), Match::MethodNotAllowed("GET"));
        assert_eq!(route("POST", "/api/user-profile"), Match::MethodNotAllowed("GET"));
        match route("POST", "/api/meetings/m1/increments/p1/generate") {
            Match::Route(Route::GenerateIncrement, params) => {
                assert_eq!((params.get("meeting_id"), params.get("prompt_id")), ("m1", "p1"));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            route("GET", "/api/meetings/m1/increments/p1/generate"),
            Match::Route(Route::GetIncrement, _)
        ));
        assert_eq!(route("GET", "/api/meetings/"), Match::Redirect("/api/meetings".into()));
        assert_eq!(route("GET", "/static"), Match::Redirect("/static/".into()));
        assert_eq!(route("GET", "/static/app.js"), Match::Static("app.js".into()));
        assert_eq!(route("GET", "/api/meetings/a/b"), Match::NotFound);
        assert_eq!(route("GET", &decode_path("/api/meetings/a%2Fb")), Match::NotFound);
        assert_eq!(route("GET", "/api/live/stream"), Match::NotFound);
        assert!(matches!(ws_route("/api/live/stream"), Some((WsRoute::LiveStream, _))));
    }
}
