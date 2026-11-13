//! REST and local HTTP IPC over an existing application instance.
mod routes;
mod streams;
use anyhow::{ensure, Result};
use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    Json, Router,
};
pub use rexeterm_application::{Application, Host, Workspace};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    sync::{watch, Mutex, Semaphore},
    task::JoinHandle,
};

#[derive(Clone)]
pub struct HttpOptions {
    /// Only set by the CLI-owned host entrypoint; GUI/MCP keep their own lifetimes.
    pub service_id: Option<String>,
    pub host_shutdown: Option<watch::Sender<bool>>,
    pub listen: SocketAddr,
    pub unix_socket: Option<PathBuf>,
    /// Required for TCP. Keep the token out of URLs and command-line arguments.
    pub token: Option<String>,
    pub allowed_origins: Vec<String>,
}
impl Default for HttpOptions {
    fn default() -> Self {
        Self {
            service_id: None,
            host_shutdown: None,
            listen: "127.0.0.1:0".parse().unwrap(),
            unix_socket: None,
            token: None,
            allowed_origins: Vec::new(),
        }
    }
}
pub struct HttpServer {
    target: String,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<Result<()>>>,
}
impl HttpServer {
    pub fn target(&self) -> &str {
        &self.target
    }
    pub async fn shutdown(mut self) -> Result<()> {
        self.stop.send_replace(true);
        if let Some(task) = self.task.take() {
            task.await??;
        }
        Ok(())
    }
}
impl Drop for HttpServer {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}
struct ApiState {
    service_id: Option<String>,
    host_shutdown: Option<watch::Sender<bool>>,
    app: Arc<Application>,
    token: Option<String>,
    origins: Vec<String>,
    stop: watch::Sender<bool>,
    in_flight: Arc<Semaphore>,
    streams: Arc<Semaphore>,
    idempotency: Mutex<HashMap<String, Idempotent>>,
    staging: Mutex<HashMap<String, Arc<streams::StagedTransfer>>>,
    staging_bytes: Arc<Semaphore>,
    _staging_directory: tempfile::TempDir,
}
struct Idempotent {
    fingerprint: String,
    created: Instant,
    result: Option<Value>,
    error: Option<(StatusCode, String)>,
}
#[derive(Debug)]
struct ApiError(StatusCode, String);
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self(StatusCode::BAD_GATEWAY, e.to_string())
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(json!({"error":{"code":self.0.as_u16(),"message":self.1}})),
        )
            .into_response()
    }
}
type ApiResult = std::result::Result<Json<Value>, ApiError>;
fn bad(message: &str) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, message.into())
}
fn field<'a>(v: &'a Value, name: &str) -> std::result::Result<&'a str, ApiError> {
    v[name]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| bad(&format!("missing {name}")))
}

pub async fn start_http(app: Arc<Application>, options: HttpOptions) -> Result<HttpServer> {
    let (stop, _) = watch::channel(false);
    if options.unix_socket.is_none() {
        ensure!(options.token.is_some(), "TCP HTTP requires a token");
    }
    if let Some(token) = &options.token {
        ensure!(
            token.len() >= 32
                && token.len() <= 256
                && token
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c)),
            "token must contain 32..256 ASCII letters, digits, '-' or '_'"
        );
    }
    let listener = if options.unix_socket.is_none() {
        Some(tokio::net::TcpListener::bind(options.listen).await?)
    } else {
        None
    };
    let mut origins = options.allowed_origins;
    if let Some(listener) = &listener {
        origins.push(format!("http://{}", listener.local_addr()?));
    }
    let state = Arc::new(ApiState {
        service_id: options.service_id,
        host_shutdown: options.host_shutdown,
        app,
        token: options.token,
        origins,
        stop: stop.clone(),
        in_flight: Arc::new(Semaphore::new(32)),
        streams: Arc::new(Semaphore::new(16)),
        idempotency: Mutex::new(HashMap::new()),
        staging: Mutex::new(HashMap::new()),
        staging_bytes: Arc::new(Semaphore::new(512 * 1024 * 1024)),
        _staging_directory: tempfile::tempdir()?,
    });
    let router = routes::router()
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .with_state(state.clone());
    let mut stopped = stop.subscribe();
    let reap_state = Arc::downgrade(&state);
    let mut reap_stop = stop.subscribe();
    let reaper = tokio::spawn(async move {
        loop {
            tokio::select! {_ = reap_stop.changed()=>break,_=tokio::time::sleep(Duration::from_secs(10))=>{let Some(state)=reap_state.upgrade()else{break};streams::reap(&state).await;}}
        }
    });
    let reaper = AbortOnDrop(reaper.abort_handle());
    #[cfg(unix)]
    if let Some(path) = options.unix_socket {
        use std::os::unix::fs::PermissionsExt;
        let listener = tokio::net::UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let target = format!("unix://{}", std::path::absolute(&path)?.display());
        let task = tokio::spawn(async move {
            let _reaper = reaper;
            let _cleanup = SocketCleanup(path);
            let mut tasks = tokio::task::JoinSet::new();
            let slots = Arc::new(Semaphore::new(128));
            loop {
                tokio::select! {biased;_=stopped.changed()=>break,accepted=listener.accept()=>{
                    let (stream,_)=accepted?;while tasks.try_join_next().is_some(){}let Ok(slot)=slots.clone().try_acquire_owned()else{continue};let router=router.clone();let mut connection_stop=state.stop.subscribe();
                    tasks.spawn(async move{let _slot=slot;let service=hyper_util::service::TowerToHyperService::new(router);let io=hyper_util::rt::TokioIo::new(stream);let connection=hyper::server::conn::http1::Builder::new().serve_connection(io,service).with_upgrades();tokio::select!{_=connection_stop.changed()=>{},_=connection=>{}}});
                }}
            }
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
            Ok(())
        });
        return Ok(HttpServer {
            target,
            stop,
            task: Some(task),
        });
    }
    #[cfg(not(unix))]
    ensure!(
        options.unix_socket.is_none(),
        "Unix HTTP IPC is unavailable on this platform; use loopback HTTP with a token"
    );
    let listener = listener.expect("TCP listener bound");
    let target = format!("http://{}", listener.local_addr()?);
    let task = tokio::spawn(async move {
        let _reaper = reaper;
        let mut tasks = tokio::task::JoinSet::new();
        let slots = Arc::new(Semaphore::new(128));
        loop {
            tokio::select! {biased;_=stopped.changed()=>break,accepted=listener.accept()=>{
                let(stream,_)=accepted?;while tasks.try_join_next().is_some(){}let Ok(slot)=slots.clone().try_acquire_owned()else{continue};stream.set_nodelay(true)?;
                let router=router.clone();let mut connection_stop=state.stop.subscribe();tasks.spawn(async move{
                    let _slot=slot;let service=hyper_util::service::TowerToHyperService::new(router);let io=hyper_util::rt::TokioIo::new(stream);
                    let mut builder=hyper::server::conn::http1::Builder::new();builder.timer(hyper_util::rt::TokioTimer::new()).header_read_timeout(Duration::from_secs(10)).max_buf_size(65536);
                    let connection=builder.serve_connection(io,service).with_upgrades();tokio::select!{_=connection_stop.changed()=>{},_=connection=>{}}
                });
            }}
        }
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        Ok(())
    });
    Ok(HttpServer {
        target,
        stop,
        task: Some(task),
    })
}
struct AbortOnDrop(tokio::task::AbortHandle);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
#[cfg(unix)]
struct SocketCleanup(PathBuf);
#[cfg(unix)]
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
async fn authenticate(
    State(state): State<Arc<ApiState>>,
    request: Request,
    next: Next,
) -> Response {
    if *state.stop.borrow() {
        return ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "HTTP adapter stopping".into(),
        )
        .into_response();
    }
    if let Some(actual) = request.headers().get("x-rexeterm-service-id") {
        if state.service_id.is_none() || actual.to_str().ok() != state.service_id.as_deref() {
            return ApiError(StatusCode::CONFLICT, "CLI host service ID mismatch".into())
                .into_response();
        }
    }
    let origin = request
        .headers()
        .get("origin")
        .and_then(|h| h.to_str().ok());
    if origin.is_some_and(|o| !state.origins.iter().any(|allowed| allowed == o)) {
        return ApiError(StatusCode::FORBIDDEN, "origin rejected".into()).into_response();
    }
    if let Some(token) = &state.token {
        let bearer = request
            .headers()
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "));
        let cookie = request
            .headers()
            .get("cookie")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| {
                s.split(';')
                    .map(str::trim)
                    .find_map(|s| s.strip_prefix("rexeterm_session="))
            });
        let bearer_ok = bearer.is_some_and(|v| constant_equal(v.as_bytes(), token.as_bytes()));
        let cookie_ok = cookie.is_some_and(|v| constant_equal(v.as_bytes(), token.as_bytes()));
        if !bearer_ok && (!cookie_ok || origin.is_none()) {
            return ApiError(StatusCode::UNAUTHORIZED, "authentication required".into())
                .into_response();
        }
    }
    let Ok(slot) = state.in_flight.clone().try_acquire_owned() else {
        return ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "request capacity reached".into(),
        )
        .into_response();
    };
    let mut stopped = state.stop.subscribe();
    let response = tokio::select! {biased;_=stopped.changed()=>ApiError(StatusCode::SERVICE_UNAVAILABLE,"HTTP adapter stopping".into()).into_response(),response=next.run(request)=>response};
    drop(slot);
    response
}
fn constant_equal(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for (i, byte) in b.iter().enumerate() {
        diff |= usize::from(*byte ^ a.get(i).copied().unwrap_or_default());
    }
    diff == 0
}
async fn once(
    state: &ApiState,
    headers: &HeaderMap,
    scope: &str,
    body: &Value,
    operation: impl std::future::Future<Output = ApiResult>,
) -> ApiResult {
    use sha2::{Digest, Sha256};
    let key = headers
        .get("idempotency-key")
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| bad("Idempotency-Key is required"))?;
    if key.is_empty() || key.len() > 128 || !key.bytes().all(|c| c.is_ascii_graphic()) {
        return Err(bad("invalid Idempotency-Key"));
    }
    let fingerprint = format!("{:x}", Sha256::digest(format!("{scope}\n{body}")));
    let mut entries = state.idempotency.lock().await;
    entries.retain(|_, v| v.created.elapsed() < Duration::from_secs(600));
    if let Some(entry) = entries.get(key) {
        return if entry.fingerprint != fingerprint {
            Err(ApiError(
                StatusCode::CONFLICT,
                "Idempotency-Key was used for another request".into(),
            ))
        } else if let Some(result) = &entry.result {
            Ok(Json(result.clone()))
        } else if let Some((status, error)) = &entry.error {
            Err(ApiError(*status, error.clone()))
        } else {
            Err(ApiError(
                StatusCode::CONFLICT,
                "request outcome is unknown; inspect resources before retrying".into(),
            ))
        };
    }
    if entries.len() >= 256 {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "idempotency capacity reached".into(),
        ));
    }
    // Reserve before dispatch: cancelling an HTTP request must never make a retry run it twice.
    entries.insert(
        key.into(),
        Idempotent {
            fingerprint,
            created: Instant::now(),
            result: None,
            error: None,
        },
    );
    match operation.await {
        Ok(result) => {
            entries.get_mut(key).unwrap().result = Some(result.0.clone());
            Ok(result)
        }
        Err(error) => {
            entries.get_mut(key).unwrap().error = Some((error.0, error.1.clone()));
            Err(error)
        }
    }
}
