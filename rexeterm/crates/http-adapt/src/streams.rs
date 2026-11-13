use super::*;
use axum::{
    body::Bytes,
    extract::{
        ws::{Message, WebSocket},
        Path, Query, WebSocketUpgrade,
    },
    response::sse::{Event, KeepAlive, Sse},
};
use futures_util::{SinkExt, StreamExt};
use rexeterm_appcore::{
    client::{FileTransferMode, FileTransferOptions},
    protocol::{management::ChannelMetadata, terminal::ClientText},
    transport::Frame,
};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
pub(super) struct StagedTransfer {
    id: String,
    host: String,
    remote: PathBuf,
    upload: bool,
    expected: u64,
    created: Instant,
    metadata: ChannelMetadata,
    overwrite: bool,
    sha256: Option<String>,
    file: tempfile::TempPath,
    _budget: tokio::sync::OwnedSemaphorePermit,
    state: Mutex<Stage>,
    cancel: watch::Sender<bool>,
}
struct Stage {
    job: Option<String>,
    status: String,
    error: Option<String>,
}
impl StagedTransfer {
    async fn view(&self, app: &Application) -> Value {
        let state = self.state.lock().await;
        let mut value = state
            .job
            .as_ref()
            .and_then(|id| {
                app.transfers()
                    .as_array()
                    .and_then(|v| v.iter().find(|v| v["id"] == *id).cloned())
            })
            .unwrap_or(
                json!({"status":state.status,"error":state.error,"bytes":0,"total":self.expected}),
            );
        value["id"] = json!(self.id);
        value["host"] = json!(self.host);
        value["remotePath"] = json!(self.remote);
        value["title"] = json!(self.metadata.title);
        value["description"] = json!(self.metadata.description);
        value["mode"] = json!(if self.upload { "upload" } else { "download" });
        if let Some(value) = value.as_object_mut() {
            value.remove("localPath");
        }
        value
    }
    async fn start(&self, app: &Arc<Application>) -> Result<()> {
        let options = FileTransferOptions {
            metadata: self.metadata.clone(),
            mode: if self.upload {
                FileTransferMode::Upload
            } else {
                FileTransferMode::Download
            },
            local_path: self.file.to_path_buf(),
            target_path: self.remote.clone(),
            overwrite: Some(if self.upload { self.overwrite } else { true }),
            offset: 0,
            sha256: self.sha256.clone(),
        };
        let job = app
            .start_transfer_with_limit(self.host.clone(), options, Some(self.expected))
            .await?;
        let mut state = self.state.lock().await;
        state.job = Some(job);
        state.status = "running".into();
        Ok(())
    }
}
pub(super) async fn reap(state: &ApiState) {
    let expired: Vec<_> = {
        let mut entries = state.staging.lock().await;
        let ids: Vec<_> = entries
            .iter()
            .filter(|(_, entry)| entry.created.elapsed() > Duration::from_secs(600))
            .map(|(k, _)| k.clone())
            .collect();
        ids.into_iter()
            .filter_map(|id| entries.remove(&id))
            .collect()
    };
    for entry in expired {
        entry.cancel.send_replace(true);
        if let Some(job) = &entry.state.lock().await.job {
            let _ = state.app.cancel_transfer(job);
        }
    }
}
pub(super) async fn create_transfer(
    State(s): State<Arc<ApiState>>,
    Path(host): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult {
    once(&s, &headers, &format!("transfer:{host}"), &body, async {
        let direction = field(&body, "mode")?;
        if !matches!(direction, "upload" | "download") {
            return Err(bad("mode must be upload or download"));
        }
        let upload = direction == "upload";
        let path = field(&body, "path")?;
        let expected = if upload {
            body["size"]
                .as_u64()
                .ok_or_else(|| bad("upload size is required"))?
        } else {
            s.app
                .rpc(Some(&host), "stat", json!({"filePath":path}), None)
                .await?["metadata"]["file"]["size"]
                .as_u64()
                .ok_or_else(|| bad("remote path is not a regular file"))?
        };
        if expected > 256 * 1024 * 1024 {
            return Err(ApiError(
                StatusCode::PAYLOAD_TOO_LARGE,
                "file exceeds 256 MiB staging limit".into(),
            ));
        }
        let budget = s
            .staging_bytes
            .clone()
            .try_acquire_many_owned(expected.max(1) as u32)
            .map_err(|_| {
                ApiError(
                    StatusCode::TOO_MANY_REQUESTS,
                    "staging capacity reached".into(),
                )
            })?;
        let metadata = ChannelMetadata {
            title: body["title"].as_str().unwrap_or("未命名").into(),
            description: body["description"].as_str().unwrap_or("").into(),
        };
        metadata.validate().map_err(bad)?;
        let mut entries = s.staging.lock().await;
        if entries.len() >= 64 {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "staged transfer capacity reached".into(),
            ));
        }
        let id = format!("transfer-{:032x}", rand::random::<u128>());
        let (cancel, _) = watch::channel(false);
        let entry = Arc::new(StagedTransfer {
            id: id.clone(),
            host,
            remote: path.into(),
            upload,
            expected,
            created: Instant::now(),
            metadata,
            overwrite: body["overwrite"].as_bool().unwrap_or(false),
            sha256: body["sha256"].as_str().map(str::to_owned),
            file: tempfile::NamedTempFile::new_in(s._staging_directory.path())
                .map_err(anyhow::Error::from)?
                .into_temp_path(),
            _budget: budget,
            state: Mutex::new(Stage {
                job: None,
                status: if upload { "waiting_upload" } else { "pending" }.into(),
                error: None,
            }),
            cancel,
        });
        if !upload {
            // Downloads atomically create this private target. Do not leave an existing
            // Windows temporary-file placeholder at the destination of MoveFileEx.
            std::fs::remove_file(&entry.file).map_err(anyhow::Error::from)?;
            entry.start(&s.app).await?;
        }
        let view = entry.view(&s.app).await;
        entries.insert(id, entry);
        Ok(Json(view))
    })
    .await
}
async fn lookup(s: &ApiState, id: &str) -> std::result::Result<Arc<StagedTransfer>, ApiError> {
    s.staging.lock().await.get(id).cloned().ok_or_else(|| {
        ApiError(
            StatusCode::NOT_FOUND,
            "staged transfer not found or expired".into(),
        )
    })
}
pub(super) async fn transfers(State(s): State<Arc<ApiState>>) -> ApiResult {
    let entries: Vec<_> = s.staging.lock().await.values().cloned().collect();
    let mut views = Vec::new();
    for entry in entries {
        views.push(entry.view(&s.app).await);
    }
    Ok(Json(json!(views)))
}
pub(super) async fn transfer(State(s): State<Arc<ApiState>>, Path(id): Path<String>) -> ApiResult {
    Ok(Json(lookup(&s, &id).await?.view(&s.app).await))
}
pub(super) async fn cancel_transfer(
    State(s): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> ApiResult {
    let entry = lookup(&s, &id).await?;
    entry.cancel.send_replace(true);
    let mut state = entry.state.lock().await;
    if let Some(job) = &state.job {
        let _ = s.app.cancel_transfer(job);
    } else {
        state.status = "cancelled".into();
    }
    Ok(Json(json!({"ok":true})))
}
pub(super) async fn upload(
    State(s): State<Arc<ApiState>>,
    Path(id): Path<String>,
    request: Request,
) -> ApiResult {
    let entry = lookup(&s, &id).await?;
    {
        let mut stage = entry.state.lock().await;
        if !entry.upload || stage.status != "waiting_upload" {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "transfer is not waiting for upload".into(),
            ));
        }
        stage.status = "uploading".into();
    }
    let result=tokio::time::timeout(Duration::from_secs(120),async{
        let mut stream=request.into_body().into_data_stream();let mut file=tokio::fs::OpenOptions::new().write(true).truncate(true).open(&entry.file).await?;let mut bytes=0u64;let mut cancelled=entry.cancel.subscribe();let mut stopped=s.stop.subscribe();
        loop{let next=tokio::select!{biased;_=cancelled.changed()=>anyhow::bail!("upload cancelled"),_=stopped.changed()=>anyhow::bail!("HTTP adapter stopping"),next=stream.next()=>next};let Some(chunk)=next else{break};let chunk=chunk?;bytes+=chunk.len() as u64;ensure!(bytes<=entry.expected,"upload exceeds declared size");file.write_all(&chunk).await?;}
        ensure!(bytes==entry.expected,"upload size mismatch");file.flush().await?;drop(file);entry.start(&s.app).await?;Ok::<_,anyhow::Error>(())
    }).await;
    if let Err(error) = result
        .map_err(|_| anyhow::anyhow!("upload timed out"))
        .and_then(|r| r)
    {
        let mut state = entry.state.lock().await;
        state.status = "error".into();
        state.error = Some(error.to_string());
        return Err(error.into());
    }
    Ok(Json(entry.view(&s.app).await))
}
pub(super) async fn download(
    State(s): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> std::result::Result<Response, ApiError> {
    let entry = lookup(&s, &id).await?;
    let view = entry.view(&s.app).await;
    if entry.upload || view["status"] != "done" {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "download is not ready".into(),
        ));
    }
    let file = tokio::fs::File::open(&entry.file)
        .await
        .map_err(anyhow::Error::from)?;
    let mut stop = s.stop.subscribe();
    let body = async_stream::try_stream! {let _entry=entry;let mut file=file;let mut buffer=vec![0u8;65536];loop{let count=tokio::select!{_=stop.changed()=>Ok(0),result=file.read(&mut buffer)=>result}?;if count==0{break;}yield Bytes::copy_from_slice(&buffer[..count]);}};
    let body = body.map(|item: std::result::Result<Bytes, std::io::Error>| item);
    let mut response = Response::new(Body::from_stream(body));
    response
        .headers_mut()
        .insert("content-type", "application/octet-stream".parse().unwrap());
    if let Some(size) = view["bytes"].as_u64() {
        response.headers_mut().insert("content-length", size.into());
    }
    Ok(response)
}
#[derive(Deserialize)]
pub(super) struct PtyQuery {
    #[serde(default = "default_cols")]
    cols: u16,
    #[serde(default = "default_rows")]
    rows: u16,
}
fn default_cols() -> u16 {
    80
}
fn default_rows() -> u16 {
    24
}
pub(super) async fn pty(
    State(s): State<Arc<ApiState>>,
    Path((host, task)): Path<(String, String)>,
    Query(q): Query<PtyQuery>,
    upgrade: WebSocketUpgrade,
) -> std::result::Result<Response, ApiError> {
    let slot = s.streams.clone().try_acquire_owned().map_err(|_| {
        ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "stream capacity reached".into(),
        )
    })?;
    let connection = s
        .app
        .core()
        .open_terminal(
            Some(&host),
            &task,
            &format!("http-{:016x}", rand::random::<u64>()),
            q.cols.clamp(2, 500),
            q.rows.clamp(2, 200),
        )
        .await?;
    Ok(upgrade
        .max_message_size(65536)
        .max_frame_size(65536)
        .on_upgrade(move |socket| async move {
            let _slot = slot;
            let _ = bridge(socket, connection, s.stop.subscribe()).await;
        })
        .into_response())
}
async fn bridge(
    socket: WebSocket,
    mut terminal: rexeterm_appcore::client::terminal::TerminalConnection,
    mut stopped: watch::Receiver<bool>,
) -> Result<()> {
    if *stopped.borrow() {
        return Ok(());
    }
    let (mut output, mut input) = socket.split();
    let (writer, reader) = terminal.link().halves();
    let receive = async {
        loop {
            let frame = match reader.recv().await? {
                Some(Frame::Binary(b)) => Message::Binary(b.into()),
                Some(Frame::Text(t)) => Message::Text(t.into()),
                _ => break,
            };
            tokio::time::timeout(Duration::from_secs(5), output.send(frame)).await??;
        }
        tokio::time::timeout(Duration::from_secs(5), output.send(Message::Close(None))).await??;
        Ok::<_, anyhow::Error>(())
    };
    let send = async {
        while let Some(message) = input.next().await {
            let frame = match message? {
                Message::Binary(b) => Frame::Binary(b.to_vec()),
                Message::Text(t) => {
                    ensure!(
                        matches!(
                            serde_json::from_str::<ClientText>(&t),
                            Ok(ClientText::Resize { .. } | ClientText::RequestControl)
                        ),
                        "invalid terminal control"
                    );
                    Frame::Text(t.to_string())
                }
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) => continue,
            };
            tokio::time::timeout(Duration::from_secs(5), writer.send(frame)).await??;
        }
        Ok::<_, anyhow::Error>(())
    };
    tokio::select! {_=stopped.changed()=>Ok(()),result=receive=>result,result=send=>result}
}
pub(super) async fn events(
    State(s): State<Arc<ApiState>>,
) -> std::result::Result<Response, ApiError> {
    let slot = s.streams.clone().try_acquire_owned().map_err(|_| {
        ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "stream capacity reached".into(),
        )
    })?;
    let mut stopped = s.stop.subscribe();
    let stream = async_stream::stream! {let _slot=slot;loop{let value=json!({"workspace":s.app.workspace().await.ok(),"tasks":s.app.core().task_history(),"transfers":s.app.transfers()});yield Ok::<_,std::convert::Infallible>(Event::default().event("state").data(value.to_string()));tokio::select!{_=stopped.changed()=>break,_=tokio::time::sleep(Duration::from_secs(1))=>{}}}};
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}
