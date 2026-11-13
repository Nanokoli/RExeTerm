use super::*;
use axum::{
    extract::{Path, Query},
    http::header,
    routing::{get, post},
};
use serde::Deserialize;
pub(super) fn router() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/api/v1/info", get(info))
        .route("/api/v1/cli-host/stop", post(stop_cli_host))
        .route("/api/v1/capabilities", get(capabilities))
        .route("/api/v1/session", post(session))
        .route("/api/v1/hosts", get(hosts).post(save_host))
        .route(
            "/api/v1/hosts/{host}",
            get(host).patch(edit_host).delete(delete_host),
        )
        .route(
            "/api/v1/hosts/{host}/connection",
            get(connection).put(connect).delete(disconnect),
        )
        .route("/api/v1/hosts/{host}/tasks", get(tasks).post(create_task))
        .route(
            "/api/v1/hosts/{host}/tasks/{task}",
            get(task).delete(delete_task),
        )
        .route("/api/v1/hosts/{host}/tasks/{task}/stop", post(stop_task))
        .route("/api/v1/hosts/{host}/tasks/{task}/pty", get(streams::pty))
        .route("/api/v1/hosts/{host}/resources", get(resources))
        .route("/api/v1/hosts/{host}/fs/entries", get(entries))
        .route("/api/v1/hosts/{host}/fs/stat", get(stat))
        .route("/api/v1/hosts/{host}/fs/preview", get(preview))
        .route("/api/v1/hosts/{host}/searches", post(search))
        .route(
            "/api/v1/hosts/{host}/transfers",
            post(streams::create_transfer),
        )
        .route("/api/v1/transfers", get(streams::transfers))
        .route("/api/v1/transfers/{transfer}", get(streams::transfer))
        .route(
            "/api/v1/transfers/{transfer}/cancel",
            post(streams::cancel_transfer),
        )
        .route(
            "/api/v1/transfers/{transfer}/content",
            get(streams::download)
                .put(streams::upload)
                .layer(axum::extract::DefaultBodyLimit::disable()),
        )
        .route(
            "/api/v1/hosts/{host}/forwards",
            get(forwards).post(start_forward),
        )
        .route(
            "/api/v1/hosts/{host}/forwards/{forward}",
            axum::routing::delete(stop_forward),
        )
        .route(
            "/api/v1/preferences",
            get(preferences).patch(set_preferences),
        )
        .route("/api/v1/preferences/reload", post(reload_preferences))
        .route("/api/v1/events", get(streams::events))
        .route("/api/v1/requests", post(raw_request))
        .route("/api/v1/openapi.json", get(openapi))
}
async fn info(State(s): State<Arc<ApiState>>) -> Json<Value> {
    Json(
        json!({"name":"RexETerm","version":env!("CARGO_PKG_VERSION"),"apiVersion":"v1","serviceId":s.service_id}),
    )
}
async fn stop_cli_host(State(s): State<Arc<ApiState>>, headers: HeaderMap) -> ApiResult {
    let id = s
        .service_id
        .as_deref()
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "not a CLI-owned host".into()))?;
    if headers
        .get("x-rexeterm-service-id")
        .and_then(|v| v.to_str().ok())
        != Some(id)
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "bind the CLI host service ID before stopping it".into(),
        ));
    }
    let shutdown = s
        .host_shutdown
        .clone()
        .ok_or_else(|| bad("host shutdown is unavailable"))?;
    // Give the response a chance to flush; the owner performs final application shutdown.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        shutdown.send_replace(true);
    });
    Ok(Json(json!({"stopping":true,"serviceId":id})))
}
async fn capabilities() -> Json<Value> {
    Json(
        json!({"rest":true,"exbashIo":["stdio","pty"],"pty":"binary-websocket","events":"sse-snapshots","fileProtocol":"re.file.v2","maxFileBytes":268435456,"stagingBytes":536870912,"maxRequests":32,"maxStreams":16,"taskHistory":"application-owned","idempotencySeconds":600}),
    )
}
async fn session(State(state): State<Arc<ApiState>>) -> Response {
    let mut response = Json(json!({"ok":true})).into_response();
    if let Some(token) = &state.token {
        if let Ok(cookie) =
            format!("rexeterm_session={token}; HttpOnly; SameSite=Strict; Path=/api/v1").parse()
        {
            response.headers_mut().insert(header::SET_COOKIE, cookie);
        }
    }
    response
}
async fn hosts(State(s): State<Arc<ApiState>>) -> ApiResult {
    Ok(Json(s.app.workspace().await?["saved"]["hosts"].clone()))
}
async fn save_host(State(s): State<Arc<ApiState>>, Json(body): Json<Value>) -> ApiResult {
    s.app
        .save_host(serde_json::from_value(body).map_err(|_| bad("invalid host"))?)?;
    Ok(Json(json!({"ok":true})))
}
async fn host(State(s): State<Arc<ApiState>>, Path(id): Path<String>) -> ApiResult {
    let workspace = s.app.workspace().await?;
    let host = workspace["saved"]["hosts"]
        .as_array()
        .and_then(|v| v.iter().find(|v| v["id"] == id))
        .cloned()
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "host not found".into()))?;
    Ok(Json(host))
}
async fn edit_host(
    State(s): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    let Json(mut value) = host(State(s.clone()), Path(id.clone())).await?;
    let updates = body
        .as_object()
        .ok_or_else(|| bad("host patch must be an object"))?;
    for (k, v) in updates {
        value[k] = v.clone();
    }
    s.app
        .edit_host(
            &id,
            Some(serde_json::from_value(value).map_err(|_| bad("invalid host"))?),
        )
        .await
        .map_err(|e| ApiError(StatusCode::CONFLICT, e.to_string()))?;
    Ok(Json(json!({"ok":true})))
}
async fn delete_host(State(s): State<Arc<ApiState>>, Path(id): Path<String>) -> ApiResult {
    s.app
        .edit_host(&id, None)
        .await
        .map_err(|e| ApiError(StatusCode::CONFLICT, e.to_string()))?;
    Ok(Json(json!({"ok":true})))
}
async fn connect(State(s): State<Arc<ApiState>>, Path(id): Path<String>) -> ApiResult {
    let value = s.app.connect_host(&id, false).await?;
    Ok(Json(value["runtime"].clone()))
}
async fn connection(State(s): State<Arc<ApiState>>, Path(id): Path<String>) -> ApiResult {
    let value = s.app.workspace().await?;
    Ok(Json(
        value["runtime"]["executors"]
            .as_array()
            .and_then(|v| v.iter().find(|v| v["id"] == id))
            .cloned()
            .unwrap_or(json!({"id":id,"connected":false})),
    ))
}
async fn disconnect(State(s): State<Arc<ApiState>>, Path(id): Path<String>) -> ApiResult {
    s.app.core().disconnect_executor(&id).await?;
    Ok(Json(json!({"ok":true})))
}
async fn resources(State(s): State<Arc<ApiState>>, Path(id): Path<String>) -> ApiResult {
    Ok(Json(
        s.app
            .rpc(Some(&id), "client_resources", json!({}), None)
            .await?["metadata"]
            .clone(),
    ))
}
async fn tasks(State(s): State<Arc<ApiState>>, Path(id): Path<String>) -> ApiResult {
    let live = s
        .app
        .rpc(Some(&id), "exbash", json!({"mode":"list"}), None)
        .await?;
    let history = s.app.core().task_history()["tasks"]
        .as_array()
        .map(|v| {
            v.iter()
                .filter(|v| v["host"] == id && v["state"] != "running")
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(Json(
        json!({"active":live["metadata"]["runs"],"completed":history}),
    ))
}
async fn create_task(
    State(s): State<Arc<ApiState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    once(&s, &headers, &format!("task:{id}"), &body, async {
        let mode = body["mode"].as_str().unwrap_or("shell");
        if !matches!(mode, "shell" | "run") {
            return Err(bad("mode must be shell or run"));
        }
        let command = field(&body, "command")?;
        let mut params = json!({"mode":mode,"command":command,"read_timeout":0});
        for key in ["io", "stdin", "keepStdin", "title"] {
            if let Some(value) = body.get(key) {
                params[key] = value.clone();
            }
        }
        if let Some(value) = body.get("readTimeoutMs") {
            params["read_timeout"] = value.clone();
        }
        if let Some(value) = body.get("timeoutMs") {
            params["timeout"] = value.clone();
        }
        if let Some(value) = body.get("description") {
            params["description"] = value.clone();
        }
        if let Some(value) = body.get("shell") {
            params["shell"] = value.clone();
        }
        let value = s
            .app
            .rpc(Some(&id), "exbash", params, body["cwd"].as_str())
            .await?;
        Ok(Json(value["metadata"].clone()))
    })
    .await
}
async fn task(
    State(s): State<Arc<ApiState>>,
    Path((host, id)): Path<(String, String)>,
) -> ApiResult {
    let history = s.app.core().task_history();
    if let Some(task) = history["tasks"].as_array().and_then(|v| {
        v.iter()
            .find(|v| v["host"] == host && v["asyncID"] == id && v["state"] != "running")
    }) {
        return Ok(Json(task.clone()));
    }
    Ok(Json(
        s.app
            .rpc(
                Some(&host),
                "exbash",
                json!({"mode":"status","asyncID":id}),
                None,
            )
            .await?["metadata"]
            .clone(),
    ))
}
async fn stop_task(
    State(s): State<Arc<ApiState>>,
    Path((host, id)): Path<(String, String)>,
) -> ApiResult {
    Ok(Json(
        s.app
            .rpc(
                Some(&host),
                "exbash",
                json!({"mode":"stop","asyncID":id}),
                None,
            )
            .await?["metadata"]
            .clone(),
    ))
}
async fn delete_task(
    State(s): State<Arc<ApiState>>,
    Path((host, id)): Path<(String, String)>,
) -> ApiResult {
    s.app
        .core()
        .remove_task_history(&host, &id)
        .map_err(|e| ApiError(StatusCode::CONFLICT, e.to_string()))?;
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize)]
pub(super) struct FileQuery {
    path: String,
    #[serde(default = "one")]
    offset: usize,
}
fn one() -> usize {
    1
}
async fn entries(
    State(s): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(q): Query<FileQuery>,
) -> ApiResult {
    Ok(Json(s.app.browse(&id, &q.path, q.offset).await?))
}
async fn stat(
    State(s): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(q): Query<FileQuery>,
) -> ApiResult {
    Ok(Json(
        s.app
            .rpc(Some(&id), "stat", json!({"filePath":q.path}), None)
            .await?["metadata"]
            .clone(),
    ))
}
async fn preview(
    State(s): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(q): Query<FileQuery>,
) -> ApiResult {
    Ok(Json(s.app.preview(&id, &q.path).await?))
}
async fn search(
    State(s): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    Ok(Json(
        s.app
            .search(
                &id,
                field(&body, "path")?,
                field(&body, "query")?,
                body["content"].as_bool().unwrap_or(false),
            )
            .await?,
    ))
}
async fn forwards(State(s): State<Arc<ApiState>>, Path(id): Path<String>) -> ApiResult {
    let all = s
        .app
        .rpc(None, "forward", json!({"mode":"list"}), None)
        .await?;
    let entries = all["metadata"]["forwards"]
        .as_array()
        .map(|v| {
            v.iter()
                .filter(|v| v["executor"] == id)
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(Json(json!(entries)))
}
async fn start_forward(
    State(s): State<Arc<ApiState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> ApiResult {
    let original = body.clone();
    body["mode"] = json!("start");
    once(&s, &headers, &format!("forward:{id}"), &original, async {
        Ok(Json(
            s.app.rpc(Some(&id), "forward", body, None).await?["metadata"].clone(),
        ))
    })
    .await
}
async fn stop_forward(
    State(s): State<Arc<ApiState>>,
    Path((host, id)): Path<(String, String)>,
) -> ApiResult {
    let all = s
        .app
        .rpc(None, "forward", json!({"mode":"list"}), None)
        .await?;
    if !all["metadata"]["forwards"].as_array().is_some_and(|v| {
        v.iter()
            .any(|v| v["executor"] == host && v["forwardID"] == id)
    }) {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "forward not found for host".into(),
        ));
    }
    Ok(Json(
        s.app
            .rpc(
                Some(&host),
                "forward",
                json!({"mode":"stop","forwardID":id}),
                None,
            )
            .await?["metadata"]
            .clone(),
    ))
}
async fn preferences(State(s): State<Arc<ApiState>>) -> ApiResult {
    Ok(Json(
        serde_json::to_value(s.app.core().preferences().settings()).map_err(anyhow::Error::from)?,
    ))
}
async fn set_preferences(State(s): State<Arc<ApiState>>, Json(body): Json<Value>) -> ApiResult {
    Ok(Json(
        s.app
            .rpc(
                body["host"].as_str(),
                "set_default_shell",
                body["options"].clone(),
                body["directory"].as_str(),
            )
            .await?,
    ))
}
async fn reload_preferences(State(s): State<Arc<ApiState>>) -> ApiResult {
    Ok(Json(
        s.app.rpc(None, "request_reload", json!({}), None).await?,
    ))
}
async fn raw_request(State(s): State<Arc<ApiState>>, Json(body): Json<Value>) -> ApiResult {
    // Explicit targets are mandatory for business operations in concurrent HTTP clients.
    let tool = field(&body, "tool")?;
    if !matches!(
        tool,
        "list_executor" | "list_shells" | "request_reload" | "set_default_shell" | "task_history"
    ) && body["executor"].as_str().is_none()
    {
        return Err(bad("executor is required"));
    }
    Ok(Json(
        s.app
            .rpc(
                body["executor"].as_str(),
                tool,
                body.get("params").cloned().unwrap_or(json!({})),
                body["directory"].as_str(),
            )
            .await?,
    ))
}
async fn openapi() -> Json<Value> {
    Json(serde_json::from_str(include_str!("../openapi.json")).expect("bundled OpenAPI is valid"))
}
