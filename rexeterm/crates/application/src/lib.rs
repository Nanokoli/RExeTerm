//! Shared application services: hosts, browsing, preview, transfer jobs and task requests.
mod storage;
mod terminal;
use anyhow::{anyhow, bail, ensure, Context, Result};
use rexeterm_appcore::AppCore;
use rexeterm_appcore::{
    client::FileTransferOptions, data::file::TransferObserver, protocol::ExecutorRequest,
    transport::LinkFuture,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{watch, Mutex as AsyncMutex},
    task::JoinSet,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Host {
    pub id: String,
    pub name: String,
    pub url: String,
    pub root: String,
    #[serde(default)]
    pub username: Option<String>,
    /// Private client connection settings. Only its path is saved in the workspace.
    #[serde(default)]
    pub connection_profile: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub management: Option<ManagementSettings>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagementSettings {
    #[serde(default)]
    pub target: Option<String>,
    /// Separate private credentials; omission reuses the Host business profile.
    #[serde(default)]
    pub connection_profile: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub version: u32,
    pub hosts: Vec<Host>,
    pub last_host: Option<String>,
}

pub struct Application {
    core: AppCore,
    connections: AsyncMutex<()>,
    workspace: Mutex<Workspace>,
    path: PathBuf,
    jobs: Arc<Mutex<Vec<TransferJob>>>,
    tasks: AsyncMutex<JoinSet<()>>,
    terminal: terminal::TerminalPreview,
    next: AtomicU64,
    stopping: AtomicBool,
}

struct TransferJob {
    view: Value,
    cancel: watch::Sender<bool>,
}

impl Application {
    pub async fn new(core: AppCore, path: PathBuf, defaults: Workspace) -> Result<Arc<Self>> {
        let mut workspace = storage::read(&path, defaults)?;
        ensure!(workspace.version == 1, "不支持的工作区版本");
        let mut ids = std::collections::HashSet::new();
        for host in &mut workspace.hosts {
            validate_host(host)?;
            *host = storage::prepare(&path, host.clone())?;
            ensure!(ids.insert(host.id.clone()), "Host ID 重复");
        }
        if path.exists() || !workspace.hosts.is_empty() {
            storage::write(&path, &workspace)?;
        }
        Ok(Arc::new(Self {
            core,
            connections: Default::default(),
            workspace: Mutex::new(workspace),
            path,
            jobs: Default::default(),
            tasks: Default::default(),
            terminal: Default::default(),
            next: AtomicU64::new(1),
            stopping: AtomicBool::new(false),
        }))
    }

    pub fn core(&self) -> &AppCore {
        &self.core
    }

    /// Binary PTY batch for the native bridge; never serialize this Vec as JSON.
    pub async fn terminal_poll(&self, id: &str) -> Result<Vec<u8>> {
        self.terminal.poll(id).await
    }

    /// The CLI read this task's output once; `task list` uses it for ordering.
    pub fn mark_task_read(&self, executor: &str, id: &str) {
        self.core().mark_task_read(executor, id);
    }

    pub async fn workspace(&self) -> Result<Value> {
        let saved = self.workspace.lock().unwrap().clone();
        let runtime = self.rpc(None, "list_executor", json!({}), None).await?;
        Ok(json!({"saved":saved,"runtime":runtime["metadata"]}))
    }

    pub fn host(&self, id: &str) -> Result<Host> {
        self.workspace
            .lock()
            .unwrap()
            .hosts
            .iter()
            .find(|h| h.id == id)
            .cloned()
            .ok_or_else(|| anyhow!("Host 不存在"))
    }

    pub async fn set_management(&self, id: &str, settings: ManagementSettings) -> Result<()> {
        ensure!(!self.stopping.load(Ordering::SeqCst), "应用正在退出");
        let _guard = self.connections.lock().await;
        self.host(id)?;
        let settings = storage::prepare_management(&self.path, id, settings)?;
        let mut state = self.workspace.lock().unwrap();
        let mut next = state.clone();
        let host = next
            .hosts
            .iter_mut()
            .find(|h| h.id == id)
            .ok_or_else(|| anyhow!("Host 不存在"))?;
        host.management = Some(settings);
        self.persist(&next)?;
        *state = next;
        Ok(())
    }

    pub fn save_host(&self, host: Host) -> Result<()> {
        validate_host(&host)?;
        let host = storage::prepare(&self.path, host)?;
        let mut state = self.workspace.lock().unwrap();
        ensure!(
            !state.hosts.iter().any(|h| h.id == host.id),
            "Host ID 已存在"
        );
        let mut next = state.clone();
        next.hosts.push(host);
        self.persist(&next)?;
        *state = next;
        Ok(())
    }

    pub async fn edit_host(&self, id: &str, replacement: Option<Host>) -> Result<()> {
        // Host mutation is serialized with connect, including runtime lookup.
        let _guard = self.connections.lock().await;
        let runtime = self.rpc(None, "list_executor", json!({}), None).await?;
        ensure!(
            !runtime["metadata"]["executors"]
                .as_array()
                .is_some_and(|v| v.iter().any(|n| n["id"] == id)),
            "Host 已连接，请先断开"
        );
        let mut state = self.workspace.lock().unwrap();
        let mut next = state.clone();
        let index = next
            .hosts
            .iter()
            .position(|h| h.id == id)
            .ok_or_else(|| anyhow!("Host 不存在"))?;
        if let Some(host) = replacement {
            ensure!(host.id == id, "Host ID 不可修改");
            validate_host(&host)?;
            next.hosts[index] = storage::prepare(&self.path, host)?;
        } else {
            next.hosts.remove(index);
            if next.last_host.as_deref() == Some(id) {
                next.last_host = None;
            }
        }
        self.persist(&next)?;
        *state = next;
        Ok(())
    }
    fn persist(&self, state: &Workspace) -> Result<()> {
        storage::write(&self.path, state)
    }

    pub async fn connect(&self, id: &str) -> Result<Value> {
        self.connect_host(id, true).await
    }
    pub async fn connect_host(&self, id: &str, select: bool) -> Result<Value> {
        let _guard = self.connections.lock().await;
        let host = self
            .workspace
            .lock()
            .unwrap()
            .hosts
            .iter()
            .find(|h| h.id == id)
            .cloned()
            .ok_or_else(|| anyhow!("Host 不存在"))?;
        let runtime = self.rpc(None, "list_executor", json!({}), None).await?;
        let connected = runtime["metadata"]["executors"]
            .as_array()
            .is_some_and(|nodes| nodes.iter().any(|node| node["id"] == id));
        if !connected {
            let mut params = match &host.connection_profile {
                Some(path) => serde_json::from_slice::<Value>(
                    &std::fs::read(path).context("无法读取连接认证配置")?,
                )?,
                None => json!({}),
            };
            ensure!(params.is_object(), "连接配置必须是 JSON 对象");
            params["id"] = json!(id);
            params["url"] = json!(host.url);
            if let Some(username) = &host.username {
                params["username"] = json!(username);
            }
            self.rpc(None, "connect_to_executor", params, None).await?;
        }
        if select {
            self.rpc(None, "set_default_executor", json!({"id":id}), None)
                .await?;
        }
        {
            let mut state = self.workspace.lock().unwrap();
            let mut next = state.clone();
            next.last_host = Some(id.into());
            self.persist(&next)?;
            *state = next;
        }
        self.workspace().await
    }

    pub async fn rpc(
        &self,
        host: Option<&str>,
        method: &str,
        params: Value,
        directory: Option<&str>,
    ) -> Result<Value> {
        ensure!(!self.stopping.load(Ordering::SeqCst), "应用正在退出");
        let response = self
            .core()
            .handle(ExecutorRequest {
                id: json!(self.next.fetch_add(1, Ordering::Relaxed)),
                method: method.into(),
                params,
                directory: directory.map(PathBuf::from),
                executor: host.map(str::to_owned),
                tool_timeout_ms: Some(12000),
            })
            .await;
        ensure!(response.ok, "{}", response.error.unwrap_or_default());
        Ok(response.result.unwrap_or(Value::Null))
    }

    pub async fn browse(&self, host: &str, path: &str, offset: usize) -> Result<Value> {
        ensure!(!path.trim().is_empty(), "请输入远端目录");
        let result = self
            .rpc(
                Some(host),
                "read",
                json!({"filePath":path,"offset":offset.max(1),"limit":500}),
                None,
            )
            .await?;
        ensure!(
            result["metadata"]["file"]["kind"] == "directory",
            "目标不是文件夹"
        );
        let canonical = result["metadata"]["file"]["canonicalPath"]
            .as_str()
            .unwrap_or(path);
        let canonical = friendly_path(canonical);
        let text = output(&result);
        let mut entries = text
            .lines()
            .filter(|s| !s.is_empty())
            .map(|line| {
                let directory = line.ends_with('/');
                let name = line.trim_end_matches('/');
                json!({"name":name,"path":join_remote(&canonical,name),"directory":directory})
            })
            .collect::<Vec<_>>();
        entries.sort_by(|a, b| {
            b["directory"]
                .as_bool()
                .cmp(&a["directory"].as_bool())
                .then_with(|| {
                    a["name"]
                        .as_str()
                        .unwrap_or("")
                        .to_lowercase()
                        .cmp(&b["name"].as_str().unwrap_or("").to_lowercase())
                })
        });
        let more = result["output"]["info"]
            .as_str()
            .unwrap_or("")
            .contains("Use offset=");
        Ok(
            json!({"path":canonical,"nextOffset":if more {Some(offset.max(1)+entries.len())} else {None},"entries":entries,"file":result["metadata"]["file"]}),
        )
    }

    pub async fn preview(&self, host: &str, path: &str) -> Result<Value> {
        let stat = self
            .rpc(Some(host), "stat", json!({"filePath":path}), None)
            .await?;
        let file = &stat["metadata"]["file"];
        ensure!(file["kind"] == "file", "请选择普通文件");
        if file["size"].as_u64().unwrap_or_default() > 2 * 1024 * 1024 {
            return Ok(
                json!({"file":file,"mode":"large","content":"文件超过 2 MB，请下载后打开。"}),
            );
        }
        let result = match self
            .rpc(
                Some(host),
                "read",
                json!({"filePath":path,"limit":300}),
                None,
            )
            .await
        {
            Ok(result) => (result, "text"),
            Err(error) if error.to_string().contains("binary") => (
                self.rpc(
                    Some(host),
                    "read",
                    json!({"filePath":path,"mode":"binary","limit":128}),
                    None,
                )
                .await?,
                "binary",
            ),
            Err(error) => return Err(error),
        };
        Ok(
            json!({"file":file,"mode":result.1,"content":output(&result.0),"info":result.0["output"]["info"]}),
        )
    }

    pub async fn search(
        &self,
        host: &str,
        path: &str,
        query: &str,
        content: bool,
    ) -> Result<Value> {
        ensure!(!query.trim().is_empty(), "请输入搜索内容");
        ensure!(query.len() <= 1024, "搜索内容过长");
        if content {
            let result = self.rpc(Some(host), "rg", json!({"pattern":query,"path":path,"max_count":100,"case_sensitive":false,"timeout":6000}), None).await?;
            let expression = regex::Regex::new(r"^(.+?):([0-9]+):([0-9]+):(.*)$")?;
            let text = output(&result);
            let entries = text.lines().filter_map(|line| expression.captures(line)).map(|c|
                json!({"path":friendly_path(&c[1]),"line":c[2].parse::<u64>().unwrap_or_default(),"text":c[4].to_string()})).collect::<Vec<_>>();
            Ok(json!({"entries":entries,"info":result["metadata"],"content":true}))
        } else {
            let escaped = query
                .chars()
                .map(|c| match c {
                    '*' => "[*]".into(),
                    '?' => "[?]".into(),
                    '[' => "[[]".into(),
                    ']' => "[]]".into(),
                    c => c.to_string(),
                })
                .collect::<String>();
            let result = self
                .rpc(
                    Some(host),
                    "glob",
                    json!({"pattern":format!("**/*{escaped}*"),"path":path,"timeout":6000}),
                    None,
                )
                .await?;
            let count = result["metadata"]["count"].as_u64().unwrap_or_default() as usize;
            let text = output(&result);
            let entries = text
                .lines()
                .take(count)
                .map(|s| json!({"path":friendly_path(s)}))
                .collect::<Vec<_>>();
            Ok(json!({"entries":entries,"info":result["metadata"],"content":false}))
        }
    }

    pub fn transfers(&self) -> Value {
        json!(self
            .jobs
            .lock()
            .unwrap()
            .iter()
            .map(|job| job.view.clone())
            .collect::<Vec<_>>())
    }

    pub async fn start_transfer(
        self: &Arc<Self>,
        host: String,
        options: FileTransferOptions,
    ) -> Result<String> {
        self.start_transfer_with_limit(host, options, None).await
    }
    pub async fn start_transfer_with_limit(
        self: &Arc<Self>,
        host: String,
        options: FileTransferOptions,
        limit: Option<u64>,
    ) -> Result<String> {
        let mut tasks = self.tasks.lock().await;
        ensure!(!self.stopping.load(Ordering::SeqCst), "应用正在退出");
        let id = format!("transfer-{}", self.next.fetch_add(1, Ordering::Relaxed));
        let (cancel, mut cancelled) = watch::channel(false);
        {
            let mut jobs = self.jobs.lock().unwrap();
            ensure!(
                jobs.iter()
                    .filter(|j| j.view["status"] == "running")
                    .count()
                    < 8,
                "最多同时传输 8 个文件"
            );
            if jobs.len() >= 200 {
                jobs.retain(|j| j.view["status"] == "running");
            }
            jobs.push(TransferJob {
                view: json!({"id":id,"host":host,"mode":options.mode,
                "localPath":options.local_path,"remotePath":options.target_path,"status":"running",
                "title":options.metadata.title,"description":options.metadata.description,
                "bytes":0,"total":0,"rate":0,"startedAt":now_ms()}),
                cancel,
            });
        }
        let service = self.clone();
        let task_id = id.clone();
        while tasks.try_join_next().is_some() {}
        tasks.spawn(async move {
            let observer = Observer { id: task_id.clone(), jobs: service.jobs.clone(), start: Instant::now(), limit };
            let result = tokio::select! {
                biased;
                _ = cancelled.changed() => Err(anyhow!("已取消")),
                result = service.core().transfer_file(Some(&host), options, None, Some(&observer)) => result,
            };
            if let Some(job) = service.jobs.lock().unwrap().iter_mut().find(|j| j.view["id"] == task_id) {
                job.view["status"] = json!(if result.is_ok() {"done"} else if *cancelled.borrow() {"cancelled"} else {"error"});
                job.view["endedAt"] = json!(now_ms());
                let elapsed = observer.start.elapsed();
                job.view["durationMs"] = json!(elapsed.as_millis() as u64);
                job.view["rate"] = json!(job.view["bytes"].as_u64().unwrap_or_default() as f64 / elapsed.as_secs_f64().max(0.001));
                match result { Ok(result) => {job.view["sha256"]=json!(result.sha256);job.view["bytes"]=json!(result.bytes);},Err(error) => { job.view["error"] = json!(format!("{error:#}")); } }
            }
        });
        Ok(id)
    }

    pub fn cancel_transfer(&self, id: &str) -> Result<()> {
        let jobs = self.jobs.lock().unwrap();
        let job = jobs
            .iter()
            .find(|j| j.view["id"] == id)
            .ok_or_else(|| anyhow!("传输任务不存在"))?;
        ensure!(job.view["status"] == "running", "传输已结束");
        job.cancel.send_replace(true);
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.stopping.store(true, Ordering::SeqCst);
        self.terminal.shutdown().await;
        for job in self.jobs.lock().unwrap().iter() {
            job.cancel.send_replace(true);
        }
        let mut tasks = self.tasks.lock().await;
        while tasks.join_next().await.is_some() {}
        Ok(())
    }
}

struct Observer {
    id: String,
    jobs: Arc<Mutex<Vec<TransferJob>>>,
    start: Instant,
    limit: Option<u64>,
}
impl TransferObserver for Observer {
    fn progress(&self, bytes: u64, total: u64) -> LinkFuture<'_, ()> {
        Box::pin(async move {
            ensure!(
                self.limit
                    .is_none_or(|limit| total <= limit && bytes <= limit),
                "文件超过暂存容量"
            );
            if let Some(job) = self
                .jobs
                .lock()
                .unwrap()
                .iter_mut()
                .find(|j| j.view["id"] == self.id)
            {
                job.view["bytes"] = json!(bytes);
                job.view["total"] = json!(total);
                job.view["rate"] =
                    json!(bytes as f64 / self.start.elapsed().as_secs_f64().max(0.001));
            }
            Ok(())
        })
    }
}

fn validate_host(host: &Host) -> Result<()> {
    ensure!(
        !host.id.trim().is_empty() && host.id.len() <= 128,
        "Host ID 无效"
    );
    ensure!(
        !host.name.trim().is_empty() && !host.root.trim().is_empty(),
        "请填写名称和起始目录"
    );
    ensure!(
        host.url.starts_with("tls://")
            || host.url.starts_with("ws://")
            || host.url.starts_with("wss://")
            || host.url.starts_with("tcp://")
            || host.url.starts_with("unix://"),
        "不支持的 RE 地址"
    );
    Ok(())
}
fn output(value: &Value) -> String {
    value["output"]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}
pub fn friendly_path(path: &str) -> String {
    if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(path).to_owned()
    }
}
pub fn join_remote(parent: &str, name: &str) -> String {
    let separator = if parent.contains('\\') || parent.as_bytes().get(1) == Some(&b':') {
        '\\'
    } else {
        '/'
    };
    format!("{}{separator}{name}", parent.trim_end_matches(['/', '\\']))
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Whitelisted desktop request dispatcher, shared by native commands and integration tests.
pub async fn dispatch(service: &Arc<Application>, action: &str, args: Value) -> Result<Value> {
    ensure!(!service.stopping.load(Ordering::SeqCst), "应用正在退出");
    let get = |name: &str| {
        args[name]
            .as_str()
            .ok_or_else(|| anyhow!("缺少参数 {name}"))
    };
    match action {
        "workspace" => service.workspace().await,
        "save_host" => { service.save_host(serde_json::from_value(args)?)?; service.workspace().await }
        "connect" => service.connect(get("host")?).await,
        "browse" => service.browse(get("host")?, get("path")?, args["offset"].as_u64().unwrap_or(1) as usize).await,
        "stat" => service.rpc(Some(get("host")?),"stat",json!({"filePath":get("path")?}),None).await,
        "terminal_open" => service.terminal.open(service.core().re_client(),get("host")?,get("pty")?,args["cols"].as_u64().unwrap_or(100).clamp(2,500) as u16,args["rows"].as_u64().unwrap_or(30).clamp(2,200) as u16).await,
        "terminal_close" => {service.terminal.close(Some(get("id")?)).await;Ok(json!({"ok":true}))}
        "terminal_input" => {let data=get("data")?;ensure!(data.len()<=65536,"终端输入过长");service.terminal.send(get("id")?,rexeterm_appcore::transport::Frame::Binary(data.as_bytes().to_vec())).await?;Ok(json!({"ok":true}))}
        "terminal_resize" => {service.terminal.resize(get("id")?,args["cols"].as_u64().unwrap_or(100).clamp(2,500) as u16,args["rows"].as_u64().unwrap_or(30).clamp(2,200) as u16).await?;Ok(json!({"ok":true}))}
        "terminal_control" => {service.terminal.send(get("id")?,rexeterm_appcore::transport::Frame::Text(serde_json::to_string(&rexeterm_appcore::protocol::terminal::ClientText::RequestControl)?)).await?;Ok(json!({"ok":true}))}
        "preview" => service.preview(get("host")?,get("path")?).await,
        "search" => service.search(get("host")?,get("path")?,get("query")?,args["content"].as_bool().unwrap_or(false)).await,
        "tasks" => {
            let host=get("host")?;
            let mut value=service.rpc(Some(host),"exbash",json!({"mode":"list"}),None).await?;
            if let Some(runs)=value["metadata"]["runs"].as_array_mut(){
                if let Some(history)=service.core().task_history()["tasks"].as_array(){
                    for task in history.iter().filter(|t|t["host"]==host && t["state"]!="running"){
                        if !runs.iter().any(|r|r["asyncID"]==task["asyncID"]){runs.push(task.clone());}
                    }
                }
            }
            Ok(value)
        },
        "run" => service.rpc(Some(get("host")?),"exbash",json!({"mode":"shell","command":get("command")?,"read_timeout":100,"title":args["title"].as_str().unwrap_or("未命名"),"description":args["description"].as_str().unwrap_or("")}),Some(get("path")?)).await,
        "stop_task" => service.rpc(Some(get("host")?),"exbash",json!({"mode":"stop","asyncID":get("id")?}),None).await,
        "forwards" => service.rpc(None,"forward",json!({"mode":"list"}),None).await,
        "start_forward" => service.rpc(Some(get("host")?),"forward",json!({"mode":"start","listen":get("listen")?,"target":{"kind":"tcp","address":get("target")?},"title":args["title"].as_str().unwrap_or("未命名"),"description":args["description"].as_str().unwrap_or("")}),None).await,
        "resources" => service.rpc(Some(get("host")?),"client_resources",json!({}),None).await,
        "stop_forward" => service.rpc(None,"forward",json!({"mode":"stop","forwardID":get("id")?}),None).await,
        "transfers" => Ok(service.transfers()),
        "start_transfer" => Ok(json!({"id":service.start_transfer(get("host")?.into(),serde_json::from_value(args["options"].clone())?).await?})),
        "cancel_transfer" => { service.cancel_transfer(get("id")?)?; Ok(json!({"ok":true})) }
        _ => bail!("不支持的桌面操作：{action}"),
    }
}
