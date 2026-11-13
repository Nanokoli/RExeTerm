//! The host role: it links rexed to the CLI and owns no preferences. It runs
//! from the same binary as the CLI (`rexeterm --daemon`), as a separate process.
mod transfers;
use anyhow::{anyhow, ensure, Result};
use rexeterm_appcore::protocol::management::ClientHello;
use rexeterm_application::{Application, Workspace};
use rexeterm_cli::{directory, services::DaemonArgs};
use rexeterm_cli_host::{
    ipc::{self, Request, Response},
    Registry,
};
use rexeterm_daemon::{Daemon, DaemonOptions};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::{watch, Mutex, Semaphore};
struct State {
    app: Arc<Application>,
    id: String,
    token: String,
    stop: watch::Sender<bool>,
    stages: transfers::Transfers,
    cache: Mutex<HashMap<String, Cached>>,
    outputs: OutputStore,
    pumps: Arc<Mutex<HashMap<(String, String), Arc<AtomicBool>>>>,
}

struct Cached {
    signature: String,
    created: Instant,
    outcome: Option<std::result::Result<Value, String>>,
}
/// Client-side replay buffer: rexeterm owns task output history, rexed only streams it.
const OUTPUT_BUFFER_BYTES: usize = 256 * 1024;
#[derive(Default)]
struct OutputBuffer {
    data: Vec<u8>,
    written: u64,
    dropped: u64,
    cursor: u64,
    state: String,
    exit_code: Value,
    terminal: Value,
}

/// Streamed text of one exbash reply: pty fills `text`, stdio fills stdout and stderr.
fn reply_text(value: &Value) -> String {
    let mut text = String::new();
    for key in ["text", "stdout", "stderr"] {
        if let Some(chunk) = value["output"][key].as_str() {
            text.push_str(chunk);
        }
    }
    for key in ["stdout", "stderr"] {
        if let Some(chunk) = value["metadata"][key].as_str() {
            text.push_str(chunk);
        }
    }
    text
}

/// Shared buffer map: IPC handlers and per-task pumps both write here.
#[derive(Clone, Default)]
struct OutputStore(Arc<Mutex<HashMap<(String, String), OutputBuffer>>>);

impl OutputStore {
    async fn append(&self, key: &(String, String), value: &Value) {
        let mut outputs = self.0.lock().await;
        let entry = outputs.entry(key.clone()).or_default();
        let text = reply_text(value);
        if !text.is_empty() {
            entry.data.extend_from_slice(text.as_bytes());
            entry.written += text.len() as u64;
            if entry.data.len() > OUTPUT_BUFFER_BYTES {
                let overflow = entry.data.len() - OUTPUT_BUFFER_BYTES;
                entry.data.drain(..overflow);
                entry.dropped += overflow as u64;
                entry.cursor = entry.cursor.max(entry.dropped);
            }
        }
        if let Some(state) = value["metadata"]["state"].as_str() {
            entry.state = state.to_string();
        }
        if let Some(code) = value["metadata"].get("exitCode") {
            entry.exit_code = code.clone();
        }
        if let Some(size) = value["metadata"].get("terminalSize") {
            entry.terminal = size.clone();
        }
    }

    async fn read(
        &self,
        key: &(String, String),
        from_start: bool,
        since: Option<u64>,
    ) -> Option<Value> {
        let mut outputs = self.0.lock().await;
        let entry = outputs.get_mut(key)?;
        let start = entry.dropped;
        let mut from = if from_start {
            start
        } else if let Some(since) = since {
            since.max(start)
        } else {
            entry.cursor.max(start)
        };
        from = from.min(entry.written);
        let offset = (from - start) as usize;
        let text = String::from_utf8_lossy(&entry.data[offset..]).into_owned();
        entry.cursor = entry.written;
        Some(json!({
            "metadata": {
                "asyncID": key.1,
                "host": key.0,
                "state": entry.state,
                "exitCode": entry.exit_code,
                "terminalSize": entry.terminal,
                "from": from,
                "next_offset": entry.written,
                "buffered_bytes": entry.data.len(),
                "dropped_bytes": entry.dropped,
            },
            "output": {"text": text},
        }))
    }

    async fn clear(&self, key: &(String, String)) {
        self.0.lock().await.remove(key);
    }
}

impl State {
    async fn record_output(&self, host: Option<&str>, value: &Value) {
        if let Some(host) = host {
            if let Some(id) = value["metadata"]["asyncID"].as_str() {
                self.outputs
                    .append(&(host.to_string(), id.to_string()), value)
                    .await;
            }
        }
    }

    /// Hold the task link: keep attaching in the background and record every chunk.
    async fn start_pump(&self, host: Option<&str>, value: &Value) {
        let (Some(host), Some(id)) = (host, value["metadata"]["asyncID"].as_str()) else {

            return;
        };

        if value["metadata"]["state"].as_str() == Some("stopped") {
            return;
        }
        let key = (host.to_string(), id.to_string());
        let stop = {
            let mut pumps = self.pumps.lock().await;
            if pumps.contains_key(&key) {
                return;
            }
            let flag = Arc::new(AtomicBool::new(false));
            pumps.insert(key.clone(), Arc::clone(&flag));
            flag
        };
        let app = Arc::clone(&self.app);
        let outputs = self.outputs.clone();
        let pumps = Arc::clone(&self.pumps);
        tokio::spawn(async move {
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let reply = app
                    .rpc(
                        Some(key.0.as_str()),
                        "exbash",
                        json!({"mode":"attach","asyncID":key.1,"read_timeout":1000}),
                        None,
                    )
                    .await;
                let Ok(reply) = reply else {
                    break;
                };
                let empty = reply_text(&reply).is_empty();

                outputs.append(&key, &reply).await;
                if reply["metadata"]["state"].as_str() == Some("stopped") {
                    break;
                }
                if empty {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
            pumps.lock().await.remove(&key);
        });
    }

    async fn stop_pump(&self, key: &(String, String)) {
        if let Some(flag) = self.pumps.lock().await.get(key).cloned() {
            flag.store(true, Ordering::Relaxed);
        }
    }

    async fn read_output(
        &self,
        key: &(String, String),
        from_start: bool,
        since: Option<u64>,
    ) -> Option<Value> {
        self.outputs.read(key, from_start, since).await
    }
}

pub async fn run(
    (options, id, registry_dir): (DaemonArgs, Option<String>, Option<PathBuf>),
) -> Result<Value> {
    let cwd = std::env::current_dir()?;
    directory::check(&cwd)?;
    let registry = Registry::open(registry_dir)?;
    // The CLI wrote the UUID it wants; the host publishes it as the binding.
    let explicit = id.is_some();
    let requested = match id {
        Some(id) => Some(rexeterm_cli_host::normalize_id(&id)?),
        None => registry.read_uuid(),
    };
    // A host started without an explicit ID is the one that owns the binding.
    let bind_default = !explicit;
    let record = if let Some(id) = requested {
        registry.resolve(&id)?
    } else {
        registry.reserve(
            &options
                .config_dir
                .clone()
                .unwrap_or_else(|| cwd.join(".rexeterm")),
            options.local,
        )?
    };
    if let Some(path) = options.config_dir {
        ensure!(
            std::fs::canonicalize(path)? == record.config_dir,
            "daemon config differs from the selected service"
        );
    }
    ensure!(
        !options.local || record.local,
        "local executor differs from the selected service"
    );
    let mut registration = registry.start(&record.id)?;
    rexeterm_cli_host::set_daemon_name()?;
    let token = format!(
        "{:032x}{:032x}",
        rand::random::<u128>(),
        rand::random::<u128>()
    );
    let token_path = registration.token_path();
    rexeterm_cli_host::write_atomic(&token_path, token.as_bytes())?;
    let daemon = Daemon::new_with_options(DaemonOptions {
        local_executor: record.local,
        // The host owns no preferences: the CLI resolves shells and sends them.
        client_settings_path: None,
        identity: ClientHello {
            client_name: "rexetermd".into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .await?;
    let app = Application::new(
        daemon.core().clone(),
        record.config_dir.join("workspace.json"),
        Workspace {
            version: 1,
            hosts: Vec::new(),
            last_host: None,
        },
    )
    .await?;
    let (stop, mut stopped) = watch::channel(false);
    let state = Arc::new(State {
        app: app.clone(),
        id: record.id.clone(),
        token,
        stop,
        stages: transfers::Transfers::new(registry.directory(), &record.id)?,
        cache: Mutex::new(HashMap::new()),
        outputs: OutputStore::default(),
        pumps: Arc::new(Mutex::new(HashMap::new())),
    });
    #[cfg(unix)]
    let listener = {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};
        let path = registration.socket_path()?;
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            ensure!(
                meta.file_type().is_socket(),
                "CLI socket path is occupied by a non-socket"
            );
            std::fs::remove_file(&path)?;
        }
        let (address, _directory) = ipc::unix_address(&path)?;
        let listener = tokio::net::UnixListener::bind(address)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        registration.publish(format!("unix://{}", path.display()), Some(token_path))?;
        (listener, SocketCleanup(path))
    };
    #[cfg(windows)]
    let listener = {
        let name = registration.pipe_name();
        let path = ipc::pipe_path(&name)?;
        let pipe = tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&path)?;
        registration.publish(format!("pipe://{name}"), Some(token_path))?;
        (pipe, path)
    };
    if bind_default {
        registry.write_binding(&record.id)?;
        registry.clear_uuid();
    }
    eprintln!(
        "role=rexetermd pid={} service={} ipc={}",
        std::process::id(),
        record.id,
        registration.record().target.as_deref().unwrap_or_default()
    );
    let result = tokio::select! {
        result=serve(listener,state.clone())=>result,
        result=tokio::signal::ctrl_c()=>result.map_err(Into::into),
        _=stopped.changed()=>Ok(()),
    };
    state.stop.send_replace(true);
    let shutdown = app.shutdown().await;
    daemon.shutdown().await;
    result?;
    shutdown?;
    Ok(json!({"serviceId":record.id,"running":false}))
}
#[cfg(unix)]
struct SocketCleanup(PathBuf);
#[cfg(unix)]
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
#[cfg(unix)]
type Listener = (tokio::net::UnixListener, SocketCleanup);
#[cfg(windows)]
type Listener = (tokio::net::windows::named_pipe::NamedPipeServer, String);
async fn serve(listener: Listener, state: Arc<State>) -> Result<()> {
    #[cfg(windows)]
    let mut listener = listener;
    let slots = Arc::new(Semaphore::new(32));
    let mut tasks = tokio::task::JoinSet::new();
    let mut reap = tokio::time::interval(Duration::from_secs(10));
    loop {
        #[cfg(unix)]
        let accepted = async {
            let (stream, _) = listener.0.accept().await?;
            Ok::<ipc::LocalStream, std::io::Error>(Box::new(stream))
        };
        #[cfg(windows)]
        let accepted = async {
            loop {
                let result = listener.0.connect().await;
                // Keep an instance alive before closing an aborted client or dispatching it.
                let next = tokio::net::windows::named_pipe::ServerOptions::new()
                    .reject_remote_clients(true)
                    .create(&listener.1)?;
                let connected = std::mem::replace(&mut listener.0, next);
                match result {
                    Ok(()) => return Ok::<ipc::LocalStream, std::io::Error>(Box::new(connected)),
                    Err(error) if matches!(error.raw_os_error(), Some(109 | 232 | 233)) => continue,
                    Err(error) => return Err(error),
                }
            }
        };
        tokio::select! {
            accepted=accepted=>{
                let io=accepted?;while tasks.try_join_next().is_some(){}
                let Ok(slot)=slots.clone().try_acquire_owned()else{continue};
                let state=state.clone();tasks.spawn(async move{let _slot=slot;let _=handle(io,state).await;});
            }
            _=reap.tick()=>state.stages.reap(&state.app).await,
        }
    }
}
async fn handle(mut io: ipc::LocalStream, state: Arc<State>) -> Result<()> {
    let request: Request =
        tokio::time::timeout(Duration::from_secs(5), ipc::read(&mut io, ipc::MAX_REQUEST))
            .await??;
    let authenticated =
        request.service_id == state.id && equal(request.token.as_bytes(), state.token.as_bytes());
    let shutdown = request.operation == "shutdown";
    let result = if authenticated {
        state.execute(&request).await
    } else {
        Err(anyhow!("CLI host identity/authentication mismatch"))
    };
    let stopping = shutdown && result.is_ok();
    let result = result.and_then(|value| {
        ensure!(
            serde_json::to_vec(&value)?.len() <= ipc::MAX_RESPONSE - 1024,
            "IPC response exceeds 8 MiB; narrow the query"
        );
        Ok(value)
    });
    let (result, error) = match result {
        Ok(value) => (Some(value), None),
        Err(e) => (None, Some(format!("{e:#}"))),
    };
    let reply = Response {
        service_id: state.id.clone(),
        result,
        error,
    };
    let sent = tokio::time::timeout(
        Duration::from_secs(5),
        ipc::write(&mut io, &reply, ipc::MAX_RESPONSE),
    )
    .await?;
    if stopping {
        state.stop.send_replace(true);
    }
    sent
}
fn equal(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for (i, x) in b.iter().enumerate() {
        diff |= usize::from(*x ^ a.get(i).copied().unwrap_or_default());
    }
    diff == 0
}
impl State {
    async fn execute(&self, request: &Request) -> Result<Value> {
        let Some(key) = &request.key else {
            return self
                .dispatch(&request.operation, request.args.clone())
                .await;
        };
        ensure!(
            !key.is_empty() && key.len() <= 128 && key.bytes().all(|b| b.is_ascii_graphic()),
            "invalid idempotency key"
        );
        let signature = serde_json::to_string(&(&request.operation, &request.args))?;
        {
            let mut cache = self.cache.lock().await;
            cache.retain(|_, v| v.created.elapsed() < Duration::from_secs(600));
            if let Some(existing) = cache.get(key) {
                ensure!(
                    existing.signature == signature,
                    "idempotency key was used for another operation"
                );
                return match &existing.outcome {
                    Some(Ok(v)) => Ok(v.clone()),
                    Some(Err(e)) => Err(anyhow!("{e}")),
                    None => Err(anyhow!(
                        "request outcome is unknown; inspect resources before retrying"
                    )),
                };
            }
            ensure!(cache.len() < 128, "idempotency capacity reached");
            cache.insert(
                key.clone(),
                Cached {
                    signature,
                    created: Instant::now(),
                    outcome: None,
                },
            );
        }
        let result = self
            .dispatch(&request.operation, request.args.clone())
            .await;
        let cached = match &result {
            Ok(v) if serde_json::to_vec(v)?.len() <= 65536 => Ok(v.clone()),
            Ok(_) => Err("result exceeds retry cache budget; inspect resources".into()),
            Err(e) => Err(format!("{e:#}")),
        };
        if let Some(entry) = self.cache.lock().await.get_mut(key) {
            entry.outcome = Some(cached);
        }
        result
    }
    async fn dispatch(&self, operation: &str, args: Value) -> Result<Value> {
        let app = &self.app;
        let field = |key: &str| args[key].as_str().ok_or_else(|| anyhow!("missing {key}"));
        let field_opt = |key: &str| {
            args[key]
                .as_str()
                .map(str::to_string)
                .filter(|value| !value.is_empty())
        };
        let field_list = |key: &str| -> Vec<String> {
            args[key]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        match operation {
            "info" => Ok(
                json!({"name":"RexETerm","role":"rexetermd","version":env!("CARGO_PKG_VERSION"),"ipcVersion":1,"serviceId":self.id}),
            ),
            "capabilities" => Ok(
                json!({"nativeIpc":true,"http":false,"exbashIo":["stdio","pty"],"fileProtocol":"re.file.v2","localFiles":"shared-filesystem","maxFileBytes":268435456,"stagingBytes":536870912,"maxRequests":32,"taskHistory":"application-owned","idempotencySeconds":600}),
            ),
            "shutdown" => Ok(json!({"stopping":true,"serviceId":self.id})),
            "rpc" => {
                let tool = field("tool")?;
                ensure!(
                    matches!(
                        tool,
                        "list_executor"
                            | "list_shells"
                            | "request_reload"
                            | "set_default_shell"
                            | "task_history"
                    ) || args["executor"].is_string(),
                    "executor is required"
                );
                let executor = args["executor"].as_str().map(str::to_string);
                let value = app
                    .rpc(
                        executor.as_deref(),
                        tool,
                        args.get("params").cloned().unwrap_or(json!({})),
                        args["directory"].as_str(),
                    )
                    .await?;
                self.record_output(executor.as_deref(), &value).await;
                self.start_pump(executor.as_deref(), &value).await;
                Ok(value)
            }
            "hosts.list" => Ok(app.workspace().await?["saved"]["hosts"].clone()),
            "hosts.show" => Ok(serde_json::to_value(app.host(field("host")?)?)?),
            "hosts.save" => {
                app.save_host(serde_json::from_value(args)?)?;
                Ok(json!({"ok":true}))
            }
            "hosts.edit" => {
                let host = field("host")?;
                let mut value = serde_json::to_value(app.host(host)?)?;
                for (k, v) in args["options"]
                    .as_object()
                    .ok_or_else(|| anyhow!("host patch must be an object"))?
                {
                    value[k] = v.clone();
                }
                app.edit_host(host, Some(serde_json::from_value(value)?))
                    .await?;
                Ok(json!({"ok":true}))
            }
            "hosts.remove" => {
                app.edit_host(field("host")?, None).await?;
                Ok(json!({"ok":true}))
            }
            "hosts.connect" => {
                Ok(app.connect_host(field("host")?, false).await?["runtime"].clone())
            }
            "hosts.disconnect" => {
                app.core().disconnect_executor(field("host")?).await?;
                Ok(json!({"ok":true}))
            }
            "hosts.status" => {
                let host = field("host")?;
                Ok(app.workspace().await?["runtime"]["executors"]
                    .as_array()
                    .and_then(|a| a.iter().find(|v| v["id"] == host))
                    .cloned()
                    .unwrap_or(json!({"id":host,"connected":false})))
            }
            "tasks.list" => {
                let host = field("host")?;
                let live = app
                    .rpc(Some(host), "exbash", json!({"mode":"list"}), None)
                    .await?;
                let history = app.core().task_history()["tasks"]
                    .as_array()
                    .map(|v| {
                        v.iter()
                            .filter(|v| v["host"] == host && v["state"] != "running")
                            .cloned()
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                Ok(json!({"active":live["metadata"]["runs"],"completed":history}))
            }
            "tasks.status" => {
                let host = field("host")?;
                let id = field("id")?;
                let history = app.core().task_history();
                if let Some(task) = history["tasks"].as_array().and_then(|v| {
                    v.iter().find(|v| {
                        v["host"] == host && v["asyncID"] == id && v["state"] != "running"
                    })
                }) {
                    return Ok(task.clone());
                }
                Ok(app
                    .rpc(
                        Some(host),
                        "exbash",
                        json!({"mode":"status","asyncID":id}),
                        None,
                    )
                    .await?["metadata"]
                    .clone())
            }
            "tasks.read" => {
                let host = field("host")?;
                let id = field("id")?;
                let wait_ms = args["wait_ms"].as_u64().unwrap_or(1000).min(10_000);
                let from_start = args["from_start"].as_bool().unwrap_or(false);
                let since = args["since"].as_u64();
                let key = (host.to_string(), id.to_string());

                if let Some(buffered) = self.read_output(&key, from_start, since).await {

                    return Ok(buffered);
                }
                self.start_pump(Some(host), &json!({"metadata":{"asyncID":id,"state":"running"}}))
                    .await;
                let value = app
                    .rpc(
                        Some(host),
                        "exbash",
                        json!({"mode":"attach","asyncID":id,"read_timeout":wait_ms}),
                        None,
                    )
                    .await?;
                self.record_output(Some(host), &value).await;
                Ok(self.read_output(&key, from_start, since).await.unwrap_or_else(|| {
                    json!({
                        "metadata": {"asyncID": id, "host": host, "from": 0, "next_offset": 0,
                                     "buffered_bytes": 0, "dropped_bytes": 0},
                        "output": {"text": ""},
                    })
                }))
            }
            "tasks.stop" => {
                let host = field("host")?;
                let id = field("id")?;
                self.stop_pump(&(host.to_string(), id.to_string())).await;
                Ok(app
                    .rpc(
                        Some(host),
                        "exbash",
                        json!({"mode":"stop","asyncID":id}),
                        None,
                    )
                    .await?["metadata"]
                    .clone())
            }
            "tasks.forget" => {
                let host = field("host")?;
                let id = field("id")?;
                app.core().remove_task_history(host, id)?;
                let key = (host.to_string(), id.to_string());
                self.stop_pump(&key).await;
                self.outputs.clear(&key).await;
                Ok(json!({"ok":true}))
            }
            "resources" => Ok(app
                .rpc(Some(field("host")?), "client_resources", json!({}), None)
                .await?["metadata"]
                .clone()),
            "forwards.list" => {
                let host = field("host")?;
                let all = app
                    .rpc(None, "forward", json!({"mode":"list"}), None)
                    .await?;
                Ok(json!(all["metadata"]["forwards"]
                    .as_array()
                    .map(|v| v
                        .iter()
                        .filter(|v| v["executor"] == host)
                        .cloned()
                        .collect::<Vec<_>>())
                    .unwrap_or_default()))
            }
            "forwards.start" => {
                let mut body = args["options"].clone();
                body["mode"] = json!("start");
                Ok(app.rpc(Some(field("host")?), "forward", body, None).await?["metadata"].clone())
            }
            "forwards.stop" => {
                let host = field("host")?;
                let id = field("id")?;
                let all = app
                    .rpc(None, "forward", json!({"mode":"list"}), None)
                    .await?;
                ensure!(
                    all["metadata"]["forwards"].as_array().is_some_and(|v| v
                        .iter()
                        .any(|v| v["executor"] == host && v["forwardID"] == id)),
                    "forward not found for host"
                );
                Ok(app
                    .rpc(
                        Some(host),
                        "forward",
                        json!({"mode":"stop","forwardID":id}),
                        None,
                    )
                    .await?["metadata"]
                    .clone())
            }
            "preferences.show" => Ok(serde_json::to_value(app.core().preferences().settings())?),
            "preferences.reload" => app.rpc(None, "request_reload", json!({}), None).await,
            "preferences.shells" => {
                let executor = field_opt("host").unwrap_or_default();
                let directory = field_opt("cwd");
                let executor = match (executor, directory.is_some()) {
                    (executor, true) if executor.is_empty() => app.core().default_executor().await,
                    (executor, _) => executor,
                };
                Ok(serde_json::to_value(
                    app.core()
                        .preferences()
                        .effective(&executor, directory.as_deref()),
                )?)
            }
            "preferences.layers" => {
                let executor = field_opt("host").unwrap_or_default();
                let directory = field_opt("cwd");
                let executor = match (executor, directory.is_some()) {
                    (executor, true) if executor.is_empty() => app.core().default_executor().await,
                    (executor, _) => executor,
                };
                let preferences = app.core().preferences();
                let settings = preferences.settings();
                let stored = serde_json::to_value(&settings)?;
                let layer = |name: &str, value: Option<serde_json::Value>| {
                    json!({"layer": name, "shells": value.unwrap_or(Value::Null)})
                };
                // Fixed order: global, then executor, then the directory overlay.
                Ok(json!({
                    "layers": [
                        layer("global", stored.get("shells").cloned()),
                        layer(
                            "executor",
                            stored["executors"]
                                .get(&executor)
                                .map(|entry| entry["shells"].clone())
                        ),
                        layer(
                            "directory",
                            directory.as_deref().and_then(|cwd| {
                                stored["executors"][&executor]["directories"].get(cwd).cloned()
                            })
                        ),
                    ],
                    // `resolved` sorts after `layers`, so the stored layers read first.
                    "resolved": serde_json::to_value(preferences.effective(&executor, directory.as_deref()))?,
                }))
            }
            "preferences.setDefault" => {
                let executor = field_opt("host").unwrap_or_default();
                let directory = field_opt("cwd");
                let executor = match (executor, directory.is_some()) {
                    (executor, true) if executor.is_empty() => app.core().default_executor().await,
                    (executor, _) => executor,
                };
                let preferences = app.core().preferences();
                preferences.set_default_shell(&executor, directory.as_deref(), field("name")?)?;
                Ok(serde_json::to_value(
                    preferences.effective(&executor, directory.as_deref()),
                )?)
            }
            "preferences.setShell" => {
                let executor = field_opt("host").unwrap_or_default();
                let directory = field_opt("cwd");
                let executor = match (executor, directory.is_some()) {
                    (executor, true) if executor.is_empty() => app.core().default_executor().await,
                    (executor, _) => executor,
                };
                let name = field("name")?;
                let program = field("program")?;
                let mut command_args: Vec<String> = field_list("commandArgs");
                if command_args.is_empty() {
                    command_args = vec!["-c".into(), "{command}".into()];
                }
                let preferences = app.core().preferences();
                preferences.set_shell_profile(
                    &executor,
                    directory.as_deref(),
                    name,
                    program,
                    command_args,
                    field_list("interactiveArgs"),
                )?;
                if args["default"].as_bool() == Some(true) {
                    preferences.set_default_shell(&executor, directory.as_deref(), name)?;
                }
                Ok(serde_json::to_value(
                    preferences.effective(&executor, directory.as_deref()),
                )?)
            }
            _ if operation.starts_with("transfers.") => {
                self.stages.dispatch(app, operation, args).await
            }
            _ => anyhow::bail!("unsupported IPC operation: {operation}"),
        }
    }
}
