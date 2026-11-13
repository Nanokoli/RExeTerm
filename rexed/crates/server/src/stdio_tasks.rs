//! Non-terminal Exbash tasks. The registry owns lifecycle, not output history.
mod text;
mod tree;
use crate::{
    context::ToolContext,
    events::EventLog,
    protocol::{
        management::{ProgramSummary, SessionOwner, TaskMetadata},
        tool_output, ToolResult,
    },
};
use anyhow::{anyhow, ensure, Result};
use remote_executor_terminal::CommandSpec;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    sync::{broadcast, watch, Mutex as AsyncMutex},
};
const LIMIT: usize = 1024 * 1024;
#[derive(Clone)]
struct Chunk {
    stderr: bool,
    bytes: Arc<[u8]>,
}
struct Task {
    owner: Option<SessionOwner>,
    status: Mutex<Value>,
    stdin: AsyncMutex<Option<ChildStdin>>,
    output: broadcast::Sender<Chunk>,
    cancel: watch::Sender<bool>,
    finished: watch::Sender<bool>,
    ended: Mutex<Option<Instant>>,
}
impl Task {
    fn owned(&self, owner: Option<&SessionOwner>) -> bool {
        owner.is_none_or(|o| {
            self.owner
                .as_ref()
                .is_some_and(|actual| actual.resource_owner_id() == o.resource_owner_id())
        })
    }
}
#[derive(Default)]
struct State(Mutex<BTreeMap<String, Arc<Task>>>);
impl Drop for State {
    fn drop(&mut self) {
        for task in self.0.get_mut().unwrap().values() {
            task.cancel.send_replace(true);
        }
    }
}
#[derive(Clone, Default)]
pub(crate) struct StdioTasks(Arc<State>);
pub(crate) struct Start {
    pub id: String,
    pub spec: CommandSpec,
    pub metadata: TaskMetadata,
    pub timeout: Option<u64>,
    pub wait: u64,
    pub stdin: Option<String>,
    pub keep_stdin: bool,
}
impl StdioTasks {
    pub(crate) fn prune(&self) {
        let mut tasks = self.0 .0.lock().unwrap();
        tasks.retain(|_, task| {
            task.ended
                .lock()
                .unwrap()
                .is_none_or(|ended| ended.elapsed() < Duration::from_secs(30))
        });
        while tasks
            .values()
            .filter(|t| t.ended.lock().unwrap().is_some())
            .count()
            > 256
        {
            let oldest = tasks
                .iter()
                .filter_map(|(id, t)| t.ended.lock().unwrap().map(|at| (id.clone(), at)))
                .min_by_key(|(_, at)| *at)
                .map(|(id, _)| id);
            if let Some(id) = oldest {
                tasks.remove(&id);
            } else {
                break;
            }
        }
    }
    fn get(&self, id: &str, owner: Option<&SessionOwner>) -> Result<Arc<Task>> {
        self.prune();
        self.0
             .0
            .lock()
            .unwrap()
            .get(id)
            .filter(|task| task.owned(owner))
            .cloned()
            .ok_or_else(|| anyhow!("stdio task not found for this client"))
    }
    pub(crate) fn list(&self, owner: Option<&SessionOwner>) -> Vec<Value> {
        self.prune();
        self.0
             .0
            .lock()
            .unwrap()
            .values()
            .filter(|task| task.owned(owner) && !*task.finished.borrow())
            .map(|task| task.status.lock().unwrap().clone())
            .collect()
    }
    pub(crate) fn programs(&self) -> Vec<ProgramSummary> {
        self.0
             .0
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, t)| !*t.finished.borrow())
            .map(|(id, task)| {
                let status = task.status.lock().unwrap();
                ProgramSummary {
                    program_id: id.clone(),
                    title: status["title"].as_str().unwrap_or("未命名").into(),
                    description: status["description"].as_str().unwrap_or_default().into(),
                    pid: status["pid"].as_u64().map(|p| p as u32),
                    command: status["argv"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect(),
                    cwd: status["cwd"].as_str().map(str::to_owned),
                    created_at_ms: status["startedAt"].as_u64().unwrap_or_default(),
                    created_by: task.owner.clone(),
                    state: "running".into(),
                    exit_code: None,
                    controller: None,
                    controller_session: None,
                    attached_clients: Vec::new(),
                }
            })
            .collect()
    }
    pub(crate) fn expire(&self, client_id: &str) {
        let mut tasks = self.0 .0.lock().unwrap();
        tasks.retain(|_, task| {
            if task
                .owner
                .as_ref()
                .is_some_and(|o| o.resource_owner_id() == client_id)
            {
                task.cancel.send_replace(true);
                false
            } else {
                true
            }
        });
    }
    pub(crate) async fn start(
        &self,
        options: Start,
        ctx: &ToolContext,
        events: EventLog,
    ) -> Result<ToolResult> {
        let Start {
            id,
            spec,
            metadata,
            timeout,
            wait,
            stdin,
            keep_stdin,
        } = options;
        metadata.validate().map_err(|error| anyhow!(error))?;
        let metadata = metadata.normalized();
        self.prune();
        ensure!(
            !ctx.cancellation.as_ref().is_some_and(|c| *c.borrow()),
            "client lease expired"
        );
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .envs(&spec.env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }
        #[cfg(unix)]
        command.process_group(0);
        // Reserve capacity and insert the task under one lock, including spawn.
        let (task, child, tree, receiver) = {
            let mut tasks = self.0 .0.lock().unwrap();
            ensure!(
                tasks
                    .values()
                    .filter(|task| !*task.finished.borrow())
                    .count()
                    < 256,
                "stdio task capacity reached"
            );
            let mut child = command.spawn()?;
            let tree =
                tree::ProcessTree::new(child.id().ok_or_else(|| anyhow!("missing child pid"))?)?;
            let (output, receiver) = broadcast::channel(32);
            let (cancel, _) = watch::channel(false);
            let (finished, _) = watch::channel(false);
            let task = Arc::new(Task {
                owner: ctx.client_owner.clone(),
                status: Mutex::new(
                    json!({"asyncID":id,"io":"stdio","state":"running","pid":child.id(),"command":spec.argv().join(" "),"argv":spec.argv(),"title":metadata.title,"description":metadata.description,"cwd":spec.cwd,"startedAt":now_ms(),"totalOutput":0}),
                ),
                stdin: AsyncMutex::new(child.stdin.take()),
                output,
                cancel,
                finished,
                ended: Mutex::new(None),
            });
            tasks.insert(id.clone(), task.clone());
            (task, child, tree, receiver)
        };
        events.emit("program.created",ctx.client_owner.as_ref(),json!({"programId":id,"io":"stdio","pid":task.status.lock().unwrap()["pid"],"program":spec.program,"args":spec.args,"cwd":spec.cwd,"title":metadata.title,"description":metadata.description}));
        let running = task.clone();
        let cancellation = ctx.cancellation.clone();
        tokio::spawn(async move {
            drive(child, tree, running, timeout, cancellation, events).await;
        });
        if let Some(input) = stdin {
            let write_result = async {
                let mut writer = task.stdin.lock().await;
                writer
                    .as_mut()
                    .ok_or_else(|| anyhow!("stdin is closed"))?
                    .write_all(input.as_bytes())
                    .await
                    .map_err(anyhow::Error::from)
            };
            if let Err(error) = tokio::time::timeout(Duration::from_secs(1), write_result)
                .await
                .map_err(anyhow::Error::from)
                .and_then(|r| r)
            {
                task.cancel.send_replace(true);
                return Err(error);
            }
        }
        if !keep_stdin {
            task.stdin.lock().await.take();
        }
        observe(&task, receiver, wait).await
    }
    pub(crate) async fn operate(
        &self,
        id: &str,
        mode: &str,
        wait: u64,
        input: Option<Vec<u8>>,
        close_stdin: bool,
        owner: Option<&SessionOwner>,
    ) -> Result<ToolResult> {
        let task = self.get(id, owner)?;
        let receiver = task.output.subscribe();
        if let Some(bytes) = input {
            ensure!(!*task.finished.borrow(), "task already exited");
            let mut stdin = task.stdin.lock().await;
            tokio::time::timeout(
                Duration::from_secs(1),
                stdin
                    .as_mut()
                    .ok_or_else(|| anyhow!("stdin is closed"))?
                    .write_all(&bytes),
            )
            .await??;
        }
        if close_stdin {
            task.stdin.lock().await.take();
        }
        if matches!(mode, "stop" | "remove") {
            task.cancel.send_replace(true);
            let mut finished = task.finished.subscribe();
            if !*finished.borrow_and_update() {
                tokio::time::timeout(Duration::from_secs(5), finished.changed()).await??;
            }
        }
        if mode == "remove" {
            self.0 .0.lock().unwrap().remove(id);
        }
        if mode == "attach" {
            observe(&task, receiver, wait).await
        } else {
            let mut status = task.status.lock().unwrap().clone();
            if mode == "remove" {
                status["removed"] = json!(true);
            }
            Ok(ToolResult {
                metadata: status,
                output: tool_output(""),
            })
        }
    }
}
async fn pump<R: AsyncRead + Unpin>(
    mut reader: R,
    stderr: bool,
    task: Arc<Task>,
) -> std::io::Result<()> {
    let mut buffer = vec![0; 16384];
    loop {
        let size = reader.read(&mut buffer).await?;
        if size == 0 {
            return Ok(());
        }
        {
            let mut status = task.status.lock().unwrap();
            let total = status["totalOutput"].as_u64().unwrap_or_default();
            status["totalOutput"] = json!(total + size as u64);
        }
        let _ = task.output.send(Chunk {
            stderr,
            bytes: Arc::from(&buffer[..size]),
        });
    }
}
async fn drive(
    mut child: Child,
    tree: tree::ProcessTree,
    task: Arc<Task>,
    timeout: Option<u64>,
    mut lease: Option<watch::Receiver<bool>>,
    events: EventLog,
) {
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let out = tokio::spawn(pump(stdout, false, task.clone()));
    let err = tokio::spawn(pump(stderr, true, task.clone()));
    let mut cancel = task.cancel.subscribe();
    let deadline = async {
        if let Some(ms) = timeout {
            tokio::time::sleep(Duration::from_millis(ms)).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    let lease_expired = async {
        if let Some(lease) = lease.as_mut() {
            if !*lease.borrow_and_update() {
                let _ = lease.changed().await;
            }
        } else {
            std::future::pending::<()>().await;
        }
    };
    let mut timed_out = false;
    let mut stopped = false;
    let status = tokio::select! {
        result=child.wait()=>result,
        _=async {if !*cancel.borrow_and_update() {let _=cancel.changed().await;}}=>{stopped=true;tree.kill();let _=child.start_kill();child.wait().await},
        _=lease_expired=>{stopped=true;tree.kill();let _=child.start_kill();child.wait().await},
        _=deadline=>{timed_out=true;tree.kill();let _=child.start_kill();child.wait().await},
    };
    // Descendants cannot retain stdio pipes or outlive their owning task.
    tree.kill();
    task.stdin.lock().await.take();
    let out_abort = out.abort_handle();
    let err_abort = err.abort_handle();
    let drains =
        tokio::time::timeout(Duration::from_secs(1), async { tokio::join!(out, err) }).await;
    let drain_error = match drains {
        Ok((Ok(Ok(())), Ok(Ok(())))) => None,
        Ok(other) => Some(format!("stdio drain failed: {other:?}")),
        Err(_) => {
            out_abort.abort();
            err_abort.abort();
            Some("stdio drain timed out".into())
        }
    };
    let mut summary = task.status.lock().unwrap();
    summary["state"] = json!("stopped");
    summary["endedAt"] = json!(now_ms());
    summary["timedOut"] = json!(timed_out);
    summary["stopped"] = json!(stopped);
    match status {
        Ok(status) => {
            let code = status.code();
            #[cfg(unix)]
            let code = {
                use std::os::unix::process::ExitStatusExt;
                code.or_else(|| status.signal().map(|signal| 128 + signal))
            };
            summary["exitCode"] = json!(code);
        }
        Err(error) => summary["error"] = json!(error.to_string()),
    }
    if let Some(error) = drain_error {
        summary["outputError"] = json!(error);
    }
    *task.ended.lock().unwrap() = Some(Instant::now());
    events.emit("program.exited", task.owner.as_ref(), summary.clone());
    drop(summary);
    task.finished.send_replace(true);
}
async fn observe(
    task: &Task,
    mut receiver: broadcast::Receiver<Chunk>,
    wait: u64,
) -> Result<ToolResult> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(wait.min(10000));
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut dropped = 0u64;
    let mut truncated = false;
    loop {
        drain(
            &mut receiver,
            &mut stdout,
            &mut stderr,
            &mut truncated,
            &mut dropped,
        );
        if *task.finished.borrow() {
            // The producer may have completed between the first drain and this check.
            drain(
                &mut receiver,
                &mut stdout,
                &mut stderr,
                &mut truncated,
                &mut dropped,
            );
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        let mut finished = task.finished.subscribe();
        if *finished.borrow_and_update() {
            continue;
        }
        tokio::select! {
            biased;
            chunk=receiver.recv()=>match chunk {Ok(chunk)=>capture(chunk,&mut stdout,&mut stderr,&mut truncated),Err(broadcast::error::RecvError::Lagged(n))=>{dropped+=n;truncated=true},Err(_)=>break},
            _=finished.changed()=>{},
            _=tokio::time::sleep_until(deadline)=>break,
        }
    }
    let mut metadata = task.status.lock().unwrap().clone();
    metadata["outputTruncated"] = json!(truncated);
    metadata["droppedChunks"] = json!(dropped);
    metadata["outputLimitBytes"] = json!(LIMIT);
    metadata["detached"] = json!(!*task.finished.borrow());
    metadata["stdout"] = json!(text::decode(&stdout));
    metadata["stderr"] = json!(text::decode(&stderr));
    metadata["stdoutBytes"] = json!(stdout.len());
    metadata["stderrBytes"] = json!(stderr.len());
    metadata["outputObservation"] = json!("Live output during this call only; no replay buffer.");
    Ok(ToolResult {
        metadata,
        output: tool_output(""),
    })
}
fn drain(
    receiver: &mut broadcast::Receiver<Chunk>,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
    truncated: &mut bool,
    dropped: &mut u64,
) {
    for _ in 0..64 {
        match receiver.try_recv() {
            Ok(chunk) => capture(chunk, stdout, stderr, truncated),
            Err(broadcast::error::TryRecvError::Lagged(n)) => {
                *dropped += n;
                *truncated = true;
            }
            Err(_) => break,
        }
    }
}
fn capture(chunk: Chunk, stdout: &mut Vec<u8>, stderr: &mut Vec<u8>, truncated: &mut bool) {
    let keep = chunk
        .bytes
        .len()
        .min(LIMIT.saturating_sub(stdout.len() + stderr.len()));
    *truncated |= keep < chunk.bytes.len();
    let target = if chunk.stderr { stderr } else { stdout };
    target.extend_from_slice(&chunk.bytes[..keep]);
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
#[cfg(test)]
mod tests;
