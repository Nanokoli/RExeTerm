//! Optional Streamable HTTP adapter over an existing AppCore.
//! HTTP sessions share application state; closing HTTP never shuts down its owner core.
//! RE file bytes still use the RE file lane.
use anyhow::{bail, Result};
use axum::{
    body::to_bytes,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use rexeterm_appcore::AppCore;
use rexeterm_mcp_common::{
    error_response, handle_mcp_message, validate_message, MCP_PROTOCOL_VERSION,
};
use serde_json::Value;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    net::TcpListener,
    sync::{watch, Mutex, OwnedSemaphorePermit, RwLock, Semaphore},
    task::JoinHandle,
};

const SESSION_HEADER: &str = "mcp-session-id";

#[derive(Clone, Debug)]
pub struct McpHttpOptions {
    pub listen: SocketAddr,
    /// Exact browser origins accepted in addition to the actual listening origin.
    pub allowed_origins: Vec<String>,
    pub max_sessions: usize,
    pub max_in_flight: usize,
    pub idle_timeout: Duration,
}
impl Default for McpHttpOptions {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:0".parse().unwrap(),
            allowed_origins: Vec::new(),
            max_sessions: 128,
            max_in_flight: 32,
            idle_timeout: Duration::from_secs(1800),
        }
    }
}

pub struct McpHttpServer {
    address: SocketAddr,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<Result<()>>>,
}
impl McpHttpServer {
    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn target(&self) -> String {
        format!("http://{}/mcp", self.address)
    }
    pub async fn shutdown(mut self) -> Result<()> {
        self.stop.send_replace(true);
        if let Some(task) = self.task.take() {
            task.await??;
        }
        Ok(())
    }
}
impl Drop for McpHttpServer {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

struct Session {
    core: AppCore,
    version: String,
    stop: watch::Sender<bool>,
    active: RwLock<()>,
    _capacity: OwnedSemaphorePermit,
}
impl Session {
    async fn close(&self) {
        self.stop.send_replace(true);
        let _idle = self.active.write().await;
    }
    async fn request(&self, message: Value) -> Option<Value> {
        let _active = self.active.read().await;
        let mut stop = self.stop.subscribe();
        if *stop.borrow() {
            return None;
        }
        tokio::select! {
            biased;
            _ = stop.changed() => None,
            response = handle_mcp_message(&self.core, message) => response,
        }
    }
}
struct Entry {
    session: Arc<Session>,
    touched: Instant,
}
struct HttpState {
    core: AppCore,
    settings: McpHttpOptions,
    sessions: Mutex<HashMap<String, Entry>>,
    capacity: Arc<Semaphore>,
    in_flight: Semaphore,
    stopping: watch::Sender<bool>,
}
impl HttpState {
    async fn close_all(&self) {
        let sessions: Vec<_> = self
            .sessions
            .lock()
            .await
            .drain()
            .map(|(_, e)| e.session)
            .collect();
        // Cancel every request before waiting for any one session.
        for session in &sessions {
            session.stop.send_replace(true);
        }
        let mut tasks = tokio::task::JoinSet::new();
        for session in sessions {
            tasks.spawn(async move { session.close().await });
        }
        while tasks.join_next().await.is_some() {}
    }
    async fn reap(&self) {
        let mut sessions = self.sessions.lock().await;
        let expired: Vec<_> = sessions
            .iter()
            .filter(|(_, e)| {
                e.touched.elapsed() >= self.settings.idle_timeout
                    && e.session.active.try_write().is_ok()
            })
            .map(|(id, _)| id.clone())
            .collect();
        let expired: Vec<_> = expired
            .into_iter()
            .filter_map(|id| sessions.remove(&id))
            .collect();
        drop(sessions);
        for entry in expired {
            entry.session.close().await;
        }
    }
    fn origin_allowed(&self, headers: &HeaderMap) -> bool {
        headers.get("origin").is_none_or(|origin| {
            origin
                .to_str()
                .is_ok_and(|s| self.settings.allowed_origins.iter().any(|o| o == s))
        })
    }
}

pub async fn start_mcp_http(core: AppCore, mut settings: McpHttpOptions) -> Result<McpHttpServer> {
    if settings.max_sessions == 0 || settings.max_in_flight == 0 || settings.idle_timeout.is_zero()
    {
        bail!("MCP HTTP limits must be positive");
    }
    let listener = TcpListener::bind(settings.listen).await?;
    let address = listener.local_addr()?;
    settings.allowed_origins.push(format!("http://{address}"));
    if address.ip().is_loopback() {
        settings
            .allowed_origins
            .push(format!("http://localhost:{}", address.port()));
    }
    let (stop, mut stopping) = watch::channel(false);
    let state = Arc::new(HttpState {
        core,
        capacity: Arc::new(Semaphore::new(settings.max_sessions)),
        in_flight: Semaphore::new(settings.max_in_flight),
        settings,
        sessions: Mutex::new(HashMap::new()),
        stopping: stop.clone(),
    });
    let router = Router::new()
        .route(
            "/mcp",
            post(post_message).get(get_stream).delete(delete_session),
        )
        .with_state(state.clone());
    let task = tokio::spawn(async move {
        let cleanup_state = state.clone();
        let mut reap_stop = stopping.clone();
        let reaper = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! { _ = reap_stop.changed() => break, _ = interval.tick() => cleanup_state.reap().await }
            }
        });
        let cleanup_state = state.clone();
        let result = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                if !*stopping.borrow() {
                    let _ = stopping.changed().await;
                }
                cleanup_state.close_all().await;
            })
            .await;
        state.stopping.send_replace(true);
        let _ = reaper.await;
        state.close_all().await;
        result?;
        Ok(())
    });
    Ok(McpHttpServer {
        address,
        stop,
        task: Some(task),
    })
}

fn status(code: StatusCode, message: &str) -> Response {
    (code, message.to_owned()).into_response()
}
async fn get_stream(State(state): State<Arc<HttpState>>, headers: HeaderMap) -> Response {
    if !state.origin_allowed(&headers) {
        return status(StatusCode::FORBIDDEN, "origin rejected");
    }
    let mut response = status(StatusCode::METHOD_NOT_ALLOWED, "server SSE is not offered");
    response
        .headers_mut()
        .insert("allow", HeaderValue::from_static("POST, DELETE"));
    response
}
async fn find_session(
    state: &HttpState,
    headers: &HeaderMap,
) -> std::result::Result<Arc<Session>, (StatusCode, &'static str)> {
    let Some(id) = headers.get(SESSION_HEADER).and_then(|s| s.to_str().ok()) else {
        return Err((StatusCode::BAD_REQUEST, "Mcp-Session-Id required"));
    };
    let mut sessions = state.sessions.lock().await;
    let Some(entry) = sessions.get_mut(id) else {
        return Err((StatusCode::NOT_FOUND, "unknown MCP session"));
    };
    let version = headers
        .get("mcp-protocol-version")
        .and_then(|s| s.to_str().ok())
        .unwrap_or("2025-03-26");
    if version != entry.session.version {
        return Err((StatusCode::BAD_REQUEST, "MCP protocol version mismatch"));
    }
    entry.touched = Instant::now();
    Ok(entry.session.clone())
}
async fn delete_session(State(state): State<Arc<HttpState>>, headers: HeaderMap) -> Response {
    if !state.origin_allowed(&headers) {
        return status(StatusCode::FORBIDDEN, "origin rejected");
    }
    if let Err((code, message)) = find_session(&state, &headers).await {
        return status(code, message);
    }
    let id = headers[SESSION_HEADER].to_str().unwrap();
    let entry = state.sessions.lock().await.remove(id);
    if let Some(entry) = entry {
        entry.session.close().await;
    }
    StatusCode::NO_CONTENT.into_response()
}
async fn post_message(State(state): State<Arc<HttpState>>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let headers = parts.headers;
    if !state.origin_allowed(&headers) {
        return status(StatusCode::FORBIDDEN, "origin rejected");
    }
    if *state.stopping.borrow() {
        return status(StatusCode::SERVICE_UNAVAILABLE, "MCP shutting down");
    }
    let Ok(_permit) = state.in_flight.try_acquire() else {
        return status(StatusCode::TOO_MANY_REQUESTS, "too many requests");
    };
    if headers
        .get("content-type")
        .and_then(|h| h.to_str().ok())
        .is_none_or(|s| s.split(';').next().unwrap_or("").trim() != "application/json")
    {
        return status(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "application/json required",
        );
    }
    let accept = headers
        .get("accept")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let accepts = |kind| {
        accept
            .split(',')
            .any(|part| part.split(';').next().is_some_and(|s| s.trim() == kind))
    };
    if !accepts("application/json") || !accepts("text/event-stream") {
        return status(
            StatusCode::NOT_ACCEPTABLE,
            "Accept must include application/json and text/event-stream",
        );
    }
    let body = match tokio::time::timeout(
        Duration::from_secs(30),
        to_bytes(body, rexeterm_adapter_common::MAX_MESSAGE_BYTES),
    )
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(_)) => return status(StatusCode::PAYLOAD_TOO_LARGE, "body limit exceeded"),
        Err(_) => return status(StatusCode::REQUEST_TIMEOUT, "body read timed out"),
    };
    let message: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(error_response(Value::Null, -32700, error.to_string())),
            )
                .into_response()
        }
    };
    if let Err(response) = validate_message(&message) {
        return (StatusCode::BAD_REQUEST, Json(response)).into_response();
    }
    if message["method"] == "initialize" && message.get("id").is_some() {
        if headers.contains_key(SESSION_HEADER) {
            return status(StatusCode::BAD_REQUEST, "initialize requires a new session");
        }
        let Ok(capacity) = state.capacity.clone().try_acquire_owned() else {
            return status(StatusCode::SERVICE_UNAVAILABLE, "session capacity reached");
        };
        let requested = message
            .pointer("/params/protocolVersion")
            .and_then(Value::as_str);
        // HTTP supports the two Streamable HTTP versions, never the legacy SSE transport.
        let version = if requested == Some("2025-03-26") {
            "2025-03-26"
        } else {
            MCP_PROTOCOL_VERSION
        };
        let (stop, _) = watch::channel(false);
        let session = Arc::new(Session {
            core: state.core.clone(),
            version: version.to_owned(),
            stop,
            active: RwLock::new(()),
            _capacity: capacity,
        });
        let mut message = message;
        message["params"]["protocolVersion"] = Value::String(version.to_owned());
        let response = session.request(message).await;
        let id = format!(
            "{:032x}{:032x}",
            rand::random::<u128>(),
            rand::random::<u128>()
        );
        let mut sessions = state.sessions.lock().await;
        if *state.stopping.borrow() {
            drop(sessions);
            session.close().await;
            return status(StatusCode::SERVICE_UNAVAILABLE, "MCP shutting down");
        }
        sessions.insert(
            id.clone(),
            Entry {
                session,
                touched: Instant::now(),
            },
        );
        let mut response = Json(response).into_response();
        response
            .headers_mut()
            .insert(SESSION_HEADER, HeaderValue::from_str(&id).unwrap());
        return response;
    }
    let session = match find_session(&state, &headers).await {
        Ok(session) => session,
        Err((code, message)) => return status(code, message),
    };
    let is_request = message.get("id").is_some() && message.get("method").is_some();
    match session.request(message).await {
        Some(response) => Json(response).into_response(),
        None if is_request => status(StatusCode::GONE, "session closed during request"),
        None => StatusCode::ACCEPTED.into_response(),
    }
}
