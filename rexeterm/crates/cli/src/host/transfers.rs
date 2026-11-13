use anyhow::{anyhow, ensure, Result};
use rexeterm_appcore::{
    client::{FileTransferMode, FileTransferOptions},
    protocol::management::ChannelMetadata,
};
use rexeterm_application::Application;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{watch, Mutex, Semaphore},
};
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

pub(super) struct Transfers {
    entries: Mutex<HashMap<String, Arc<StagedTransfer>>>,
    budget: Arc<Semaphore>,
    directory: tempfile::TempDir,
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key].as_str().ok_or_else(|| anyhow!("missing {key}"))
}
fn options(body: &Value, local: PathBuf) -> Result<FileTransferOptions> {
    let metadata = ChannelMetadata {
        title: body["title"].as_str().unwrap_or("未命名").into(),
        description: body["description"].as_str().unwrap_or("").into(),
    };
    metadata.validate().map_err(anyhow::Error::msg)?;
    let mode = match field(body, "mode")? {
        "upload" => FileTransferMode::Upload,
        "download" => FileTransferMode::Download,
        _ => anyhow::bail!("mode must be upload or download"),
    };
    Ok(FileTransferOptions {
        metadata,
        mode,
        local_path: local,
        target_path: field(body, "path")?.into(),
        overwrite: Some(body["overwrite"].as_bool().unwrap_or(false)),
        offset: 0,
        sha256: body["sha256"].as_str().map(str::to_owned),
    })
}
impl Transfers {
    pub fn new(directory: &Path, id: &str) -> Result<Self> {
        let prefix = format!("transfers-{id}-");
        // The config process lock is held: only this service's abandoned staging is reclaimed.
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(&prefix))
                && entry.file_type()?.is_dir()
            {
                std::fs::remove_dir_all(entry.path())?;
            }
        }
        Ok(Self {
            entries: Mutex::new(HashMap::new()),
            budget: Arc::new(Semaphore::new(512 * 1024 * 1024)),
            directory: tempfile::Builder::new()
                .prefix(&prefix)
                .tempdir_in(directory)?,
        })
    }
    async fn lookup(&self, id: &str) -> Result<Arc<StagedTransfer>> {
        self.entries
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("staged transfer not found or expired"))
    }
    pub async fn reap(&self, app: &Application) {
        let expired = {
            let mut entries = self.entries.lock().await;
            let ids = entries
                .iter()
                .filter(|(_, v)| v.created.elapsed() > Duration::from_secs(600))
                .map(|(k, _)| k.clone())
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|id| entries.remove(&id))
                .collect::<Vec<_>>()
        };
        for entry in expired {
            entry.cancel.send_replace(true);
            if let Some(job) = &entry.state.lock().await.job {
                let _ = app.cancel_transfer(job);
            }
        }
    }
    pub async fn dispatch(
        &self,
        app: &Arc<Application>,
        operation: &str,
        args: Value,
    ) -> Result<Value> {
        match operation {
            "transfers.local" => {
                let body = &args["options"];
                let local = PathBuf::from(field(body, "localPath")?);
                ensure!(local.is_absolute(), "localPath must be absolute");
                if body["mode"] == "upload" {
                    let metadata = tokio::fs::metadata(&local).await?;
                    ensure!(
                        metadata.is_file() && metadata.len() <= 256 * 1024 * 1024,
                        "upload must be a regular file of at most 256 MiB"
                    );
                }
                let id = app
                    .start_transfer_with_limit(
                        field(&args, "host")?.into(),
                        options(body, local)?,
                        Some(256 * 1024 * 1024),
                    )
                    .await?;
                Ok(json!({"id":id}))
            }
            "transfers.create" => {
                let host = field(&args, "host")?;
                let body = &args["options"];
                let opts = options(body, PathBuf::new())?;
                let upload = body["mode"] == "upload";
                let expected = if upload {
                    body["size"]
                        .as_u64()
                        .ok_or_else(|| anyhow!("upload size is required"))?
                } else {
                    let stat = app
                        .rpc(
                            Some(host),
                            "stat",
                            json!({"filePath":field(body,"path")?}),
                            None,
                        )
                        .await?;
                    ensure!(
                        stat["metadata"]["file"]["kind"] == "file",
                        "remote path is not a regular file"
                    );
                    stat["metadata"]["file"]["size"]
                        .as_u64()
                        .ok_or_else(|| anyhow!("remote file has no size"))?
                };
                ensure!(
                    expected <= 256 * 1024 * 1024,
                    "file exceeds 256 MiB staging limit"
                );
                let budget = self
                    .budget
                    .clone()
                    .try_acquire_many_owned(expected.max(1) as u32)
                    .map_err(|_| anyhow!("staging capacity reached"))?;
                let mut entries = self.entries.lock().await;
                ensure!(entries.len() < 64, "staged transfer capacity reached");
                let id = format!("stage-{:032x}", rand::random::<u128>());
                let (cancel, _) = watch::channel(false);
                let entry = Arc::new(StagedTransfer {
                    id: id.clone(),
                    host: host.into(),
                    remote: opts.target_path,
                    upload,
                    expected,
                    created: Instant::now(),
                    metadata: opts.metadata,
                    overwrite: opts.overwrite.unwrap_or(false),
                    sha256: opts.sha256,
                    file: tempfile::NamedTempFile::new_in(self.directory.path())?.into_temp_path(),
                    _budget: budget,
                    state: Mutex::new(Stage {
                        job: None,
                        status: if upload { "waiting_upload" } else { "pending" }.into(),
                        error: None,
                    }),
                    cancel,
                });
                if !upload {
                    std::fs::remove_file(&entry.file)?;
                    entry.start(app).await?;
                }
                let value = entry.view(app).await;
                entries.insert(id, entry);
                Ok(value)
            }
            "transfers.show" => {
                let id = field(&args, "id")?;
                if let Ok(entry) = self.lookup(id).await {
                    return Ok(entry.view(app).await);
                }
                app.transfers()
                    .as_array()
                    .and_then(|v| v.iter().find(|v| v["id"] == id).cloned())
                    .ok_or_else(|| anyhow!("transfer not found or expired"))
            }
            "transfers.list" => {
                let entries = self
                    .entries
                    .lock()
                    .await
                    .values()
                    .cloned()
                    .collect::<Vec<_>>();
                let mut views = Vec::new();
                let mut jobs = Vec::new();
                for entry in entries {
                    views.push(entry.view(app).await);
                    if let Some(id) = &entry.state.lock().await.job {
                        jobs.push(id.clone());
                    }
                }
                if let Some(all) = app.transfers().as_array() {
                    views.extend(
                        all.iter()
                            .filter(|v| !jobs.iter().any(|id| v["id"] == *id))
                            .cloned(),
                    );
                }
                Ok(json!(views))
            }
            "transfers.cancel" => {
                let id = field(&args, "id")?;
                if let Ok(entry) = self.lookup(id).await {
                    entry.cancel.send_replace(true);
                    let mut state = entry.state.lock().await;
                    if let Some(job) = &state.job {
                        let _ = app.cancel_transfer(job);
                    } else {
                        state.status = "cancelled".into();
                    }
                } else {
                    app.cancel_transfer(id)?;
                }
                Ok(json!({"ok":true}))
            }
            "transfers.put" => {
                let entry = self.lookup(field(&args, "id")?).await?;
                {
                    let mut state = entry.state.lock().await;
                    ensure!(
                        entry.upload && state.status == "waiting_upload",
                        "transfer is not waiting for upload"
                    );
                    state.status = "uploading".into();
                }
                let mut cancelled = entry.cancel.subscribe();
                let result=tokio::time::timeout(Duration::from_secs(120),async{
                    let input=tokio::fs::File::open(field(&args,"localPath")?).await?;
                    ensure!(input.metadata().await?.is_file(),"upload source must be a regular file");
                    let mut input=input.take(entry.expected+1);
                    let mut output=tokio::fs::OpenOptions::new().write(true).truncate(true).open(&entry.file).await?;
                    let bytes=tokio::select! {biased;_=cancelled.changed()=>anyhow::bail!("upload cancelled"),bytes=tokio::io::copy(&mut input,&mut output)=>bytes?};
                    ensure!(bytes==entry.expected,"upload size mismatch");output.flush().await?;output.sync_all().await?;drop(output);
                    ensure!(!*cancelled.borrow(),"upload cancelled");entry.start(app).await?;Ok::<_,anyhow::Error>(())
                }).await.map_err(|_|anyhow!("upload timed out")).and_then(|r|r);
                if let Err(error) = result {
                    let mut state = entry.state.lock().await;
                    state.status = "error".into();
                    state.error = Some(error.to_string());
                    return Err(error);
                }
                Ok(entry.view(app).await)
            }
            "transfers.content" => {
                let entry = self.lookup(field(&args, "id")?).await?;
                let view = entry.view(app).await;
                ensure!(
                    !entry.upload && view["status"] == "done",
                    "download is not ready"
                );
                Ok(json!({"localPath":entry.file.to_path_buf(),"bytes":view["bytes"]}))
            }
            _ => anyhow::bail!("unsupported transfer operation: {operation}"),
        }
    }
}
