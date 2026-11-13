pub(crate) mod attachments;
use attachments::AttachmentState;

use crate::protocol::{
    management::TaskMetadata,
    terminal::{SessionDetail, SessionSummary},
};
use anyhow::{anyhow, Result};
use remote_executor_terminal::session::Session;
use remote_executor_terminal::{
    default_shell, CommandSpec, PtyManager, SessionDetail as CoreSessionDetail,
    SessionSummary as CoreSessionSummary, TermSize,
};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{self, Instant};

pub(crate) enum TerminalEvent {
    Metadata {
        id: String,
        pty: String,
        role: String,
        read_only: bool,
        cols: u16,
        rows: u16,
        exit_code: Option<u32>,
    },
    Closed,
}

#[derive(Clone)]
pub struct ShellManager {
    core: PtyManager,
    pub(crate) stdio: crate::stdio_tasks::StdioTasks,
    pub(crate) events: crate::events::EventLog,
    last_controllers: Arc<Mutex<HashMap<String, Option<String>>>>,
    owners: Arc<Mutex<HashMap<String, crate::protocol::management::SessionOwner>>>,
    attachments: Arc<Mutex<HashMap<String, Arc<Mutex<AttachmentState>>>>>,
    exit_watchers: Arc<Mutex<HashSet<String>>>,
    task_metadata: Arc<Mutex<HashMap<String, TaskMetadata>>>,
}

impl ShellManager {
    pub fn new(default_command: CommandSpec, default_size: TermSize) -> Self {
        Self {
            core: PtyManager::new(default_command, default_size),
            stdio: Default::default(),
            events: Default::default(),
            last_controllers: Default::default(),
            owners: Arc::new(Mutex::new(HashMap::new())),
            attachments: Default::default(),
            exit_watchers: Arc::new(Mutex::new(HashSet::new())),
            task_metadata: Default::default(),
        }
    }

    pub fn default_shell(cols: u16, rows: u16) -> Self {
        Self::new(CommandSpec::new(default_shell()), TermSize { cols, rows })
    }

    pub fn with_event_log(mut self, events: crate::events::EventLog) -> Self {
        self.events = events;
        self
    }
    pub fn core(&self) -> PtyManager {
        self.core.clone()
    }

    pub fn create_pty(
        &self,
        name: impl Into<String>,
        command: CommandSpec,
        cols: Option<u16>,
        rows: Option<u16>,
    ) -> Result<Arc<Session>> {
        self.create_owned_pty(name, command, cols, rows, None)
    }

    pub(crate) fn create_owned_pty(
        &self,
        name: impl Into<String>,
        command: CommandSpec,
        cols: Option<u16>,
        rows: Option<u16>,
        owner: Option<crate::protocol::management::SessionOwner>,
    ) -> Result<Arc<Session>> {
        Ok(self
            .create_owned_pty_with_output(
                name,
                command,
                cols,
                rows,
                owner,
                TaskMetadata::default(),
            )?
            .0)
    }
    pub(crate) fn task_metadata(&self, id: &str) -> TaskMetadata {
        self.task_metadata
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn create_owned_pty_with_output(
        &self,
        name: impl Into<String>,
        command: CommandSpec,
        cols: Option<u16>,
        rows: Option<u16>,
        owner: Option<crate::protocol::management::SessionOwner>,
        metadata: TaskMetadata,
    ) -> Result<(
        Arc<Session>,
        remote_executor_terminal::session::OutputSubscription,
    )> {
        metadata.validate().map_err(|error| anyhow!(error))?;
        let metadata = metadata.normalized();
        let name = name.into();
        let program = command.program.clone();
        let args = command.args.clone();
        let cwd = command.cwd.clone();
        let (session, output) =
            self.core
                .create_pty_with_output(name.clone(), command, cols, rows)?;
        if let Some(owner) = &owner {
            self.owners
                .lock()
                .unwrap()
                .insert(name.clone(), owner.clone());
        }
        self.task_metadata
            .lock()
            .unwrap()
            .insert(name.clone(), metadata.clone());
        self.events.emit("program.created",owner.as_ref(),serde_json::json!({"programId":name,"pid":session.summary().process_id,"program":program,"args":args,"cwd":cwd,"title":metadata.title,"description":metadata.description}));
        self.start_exit_watcher(&name);
        Ok((session, output))
    }
    pub(crate) fn is_owned_by(
        &self,
        pty: &str,
        owner: &crate::protocol::management::SessionOwner,
    ) -> bool {
        self.owners
            .lock()
            .unwrap()
            .get(pty)
            .is_some_and(|o| o.resource_owner_id() == owner.resource_owner_id())
    }
    pub(crate) fn require_owner(
        &self,
        pty: &str,
        owner: Option<&crate::protocol::management::SessionOwner>,
    ) -> Result<()> {
        if owner.is_some_and(|o| !self.is_owned_by(pty, o)) {
            return Err(anyhow!("task or PTY not found for this client"));
        }
        Ok(())
    }
    /// Only the resource owner can control; a known PTY under the same RE
    /// username can be observed without expanding any inventory/list endpoint.
    pub(crate) fn terminal_read_only(
        &self,
        pty: &str,
        owner: Option<&crate::protocol::management::SessionOwner>,
    ) -> Result<bool> {
        let Some(owner) = owner else {
            return Ok(false);
        };
        let owners = self.owners.lock().unwrap();
        match owners.get(pty) {
            Some(target) if target.resource_owner_id() == owner.resource_owner_id() => Ok(false),
            Some(target) if target.username == owner.username => Ok(true),
            _ => Err(anyhow!("task or PTY not found for this client")),
        }
    }
    pub(crate) fn expire_client(&self, client_id: &str) {
        self.stdio.expire(client_id);
        let ids: Vec<_> = self
            .owners
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, o)| o.resource_owner_id() == client_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.close_pty_clients(&id);
            if let Some(session) = self.core.session(&id) {
                if session.summary().exit_code.is_none() {
                    if let Err(error) = session.kill() {
                        self.events.emit(
                            "program.stop_failed",
                            None,
                            serde_json::json!({"programId":id,"error":error.to_string()}),
                        );
                    }
                }
            }
            self.remove_pty(&id);
        }
    }
    pub fn create_bash(&self, name: impl Into<String>) -> Result<Arc<Session>> {
        self.core.create_bash(name)
    }

    pub(crate) fn program_summaries(&self) -> Vec<crate::protocol::management::ProgramSummary> {
        let mut programs: Vec<_> = self
            .list()
            .into_iter()
            .filter_map(|s| {
                let d = self.detail(&s.pty).ok()?;
                let run = crate::exbash_run_detail(self, &s.pty).ok();
                Some(crate::protocol::management::ProgramSummary {
                    program_id: s.pty.clone(),
                    title: d.title,
                    description: d.description,
                    pid: d.process_id,
                    command: d.command,
                    cwd: d.cwd,
                    created_at_ms: d.created_at,
                    created_by: self.owners.lock().unwrap().get(&s.pty).cloned(),
                    state: run
                        .as_ref()
                        .and_then(|v| v.get("state"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(if d.exit_code.is_some() {
                            "stopped"
                        } else {
                            "running"
                        })
                        .into(),
                    exit_code: run
                        .as_ref()
                        .and_then(|v| v.get("exitCode"))
                        .cloned()
                        .or_else(|| d.exit_code.map(serde_json::Value::from)),
                    controller_session: self.controller_owner(&s.pty),
                    controller: d.controller,
                    attached_clients: self.attached_clients(&s.pty),
                })
            })
            .collect();
        programs.extend(self.stdio.programs());
        programs
    }
    pub fn reap_completed(&self) {
        self.stdio.prune();
        for id in self.core.state().prune_completed(std::time::Instant::now()) {
            self.forget_receipt(&id);
        }
    }
    fn forget_receipt(&self, pty: &str) {
        self.task_metadata.lock().unwrap().remove(pty);
        self.owners.lock().unwrap().remove(pty);
        self.last_controllers.lock().unwrap().remove(pty);
        crate::tools::exbash::forget_run(pty);
    }
    pub fn remove_pty(&self, pty: &str) -> bool {
        self.task_metadata.lock().unwrap().remove(pty);
        let result = self.core.detail(pty).ok();
        let completed = self.core.state().remove_completed(pty);
        crate::tools::exbash::forget_run(pty);
        self.last_controllers.lock().unwrap().remove(pty);
        let owner = self.owners.lock().unwrap().remove(pty);
        self.events.emit(
            "program.removed",
            owner.as_ref(),
            serde_json::json!({"programId":pty,"exitCode":result.as_ref().and_then(|d|d.exit_code),"totalOutput":result.as_ref().map(|d|d.output_total_bytes)}),
        );
        self.remove_pty_runtime(pty) || completed
    }

    pub fn list(&self) -> Vec<SessionSummary> {
        self.core
            .list()
            .into_iter()
            .filter(|summary| summary.exit_code.is_none())
            .map(|summary| self.attach_client_details(summary))
            .collect()
    }

    pub fn detail(&self, pty: &str) -> Result<SessionDetail> {
        Ok(self.attach_client_details_to_detail(self.core.detail(pty)?))
    }

    pub async fn attach_execute(
        &self,
        pty: &str,
        input: impl AsRef<[u8]>,
        collect_for: Duration,
    ) -> Result<Vec<u8>> {
        let mut rx = self.core.subscribe_output(pty)?;
        self.core.send_to_pty(pty, input.as_ref())?;

        let deadline = Instant::now() + collect_for;
        let mut output = Vec::new();
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }

            match time::timeout(deadline - now, rx.recv()).await {
                Ok(Ok(chunk)) => output.extend(chunk),
                Ok(Err(error)) => return Err(error),
                Err(_) => break,
            }
        }
        Ok(output)
    }

    pub fn subscribe_exit_code(&self, pty: &str) -> Result<mpsc::UnboundedReceiver<u32>> {
        self.core.subscribe_exit_code(pty)
    }

    fn attach_client_details(&self, summary: CoreSessionSummary) -> SessionSummary {
        let client_details = self.client_details(&summary.pty);
        let metadata = self.task_metadata(&summary.pty);
        SessionSummary {
            pty: summary.pty.clone(),
            title: metadata.title,
            description: metadata.description,
            command: summary.command,
            controller: self.controller_id(&summary.pty),
            cols: summary.cols,
            rows: summary.rows,
            process_id: summary.process_id,
            created_at: summary.created_at,
            exit_code: summary.exit_code,
            output_history_bytes: summary.output_history_bytes,
            output_history_limit: summary.output_history_limit,
            output_total_bytes: summary.output_total_bytes,
            clients: client_details.iter().map(|c| c.id.clone()).collect(),
            client_details,
        }
    }

    fn attach_client_details_to_detail(&self, detail: CoreSessionDetail) -> SessionDetail {
        let client_details = self.client_details(&detail.pty);
        let metadata = self.task_metadata(&detail.pty);
        SessionDetail {
            pty: detail.pty.clone(),
            title: metadata.title,
            description: metadata.description,
            command: detail.command,
            cwd: detail.cwd,
            env: detail.env,
            process_id: detail.process_id,
            created_at: detail.created_at,
            controller: self.controller_id(&detail.pty),
            cols: detail.cols,
            rows: detail.rows,
            output_history_bytes: detail.output_history_bytes,
            output_history_limit: detail.output_history_limit,
            output_total_bytes: detail.output_total_bytes,
            clients: client_details.iter().map(|c| c.id.clone()).collect(),
            client_details,
            exit_code: detail.exit_code,
        }
    }

    pub(crate) fn broadcast_meta(&self, pty: &str) {
        let Some(session) = self.core.session(pty) else {
            return;
        };
        let summary = self.attach_client_details(session.summary());
        let changed = {
            let mut last = self.last_controllers.lock().unwrap();
            let old = last.insert(pty.into(), summary.controller.clone());
            old.as_ref() != Some(&summary.controller)
        };
        if changed {
            let controller_owner = self.controller_owner(pty);
            self.events.emit(
                "pty.control_changed",
                controller_owner.as_ref(),
                serde_json::json!({"programId":pty,"controller":summary.controller}),
            );
        }
        let clients = self.metadata_clients(pty);

        for (id, read_only, tx) in clients {
            let role = if summary.controller.as_deref() == Some(id.as_str()) {
                "Controller"
            } else {
                "Viewer"
            };
            let event = TerminalEvent::Metadata {
                id: id.clone(),
                pty: summary.pty.clone(),
                role: role.to_string(),
                read_only,
                cols: summary.cols,
                rows: summary.rows,
                exit_code: summary.exit_code,
            };
            let _ = tx.send(event);
        }
    }

    fn start_exit_watcher(&self, pty: &str) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        {
            let mut exit_watchers = self.exit_watchers.lock().unwrap();
            if !exit_watchers.insert(pty.to_string()) {
                return;
            }
        }

        let pty = pty.to_string();
        let manager = self.clone();
        let mut rx = match manager.subscribe_exit_code(&pty) {
            Ok(rx) => rx,
            Err(err) => {
                eprintln!("exit watcher error for {pty}: {err:#}");
                manager.exit_watchers.lock().unwrap().remove(&pty);
                return;
            }
        };
        runtime.spawn(async move {
            if rx.recv().await.is_some() {
                if let Some(session) = manager.core.session(&pty) {
                    // Drain the native tail, bounded when descendants keep the slave open.
                    let _ = time::timeout(Duration::from_secs(1), session.wait_output_eof()).await;
                    manager.broadcast_meta(&pty);
                    let summary=session.summary();
                    let owner=manager.owners.lock().unwrap().get(&pty).cloned();
                    manager.events.emit("program.exited",owner.as_ref(),serde_json::json!({"programId":pty,"exitCode":summary.exit_code,"totalOutput":summary.output_total_bytes}));
                    session.close_output();
                    let removed = manager.core.state().retire_session(
                        &pty,
                        &session,
                        std::time::Instant::now() + Duration::from_secs(30),
                        256,
                    );
                    for id in removed {
                        manager.forget_receipt(&id);
                    }
                    // Existing viewers get a bounded window to drain queued tail bytes.
                    if !manager.attached_clients(&pty).is_empty() {
                        time::sleep(Duration::from_secs(2)).await;
                    }
                    manager.close_pty_clients(&pty);
                    manager.attachments.lock().unwrap().remove(&pty);
                    manager.last_controllers.lock().unwrap().remove(&pty);
                }
            }
            manager.exit_watchers.lock().unwrap().remove(&pty);
        });
    }
}
