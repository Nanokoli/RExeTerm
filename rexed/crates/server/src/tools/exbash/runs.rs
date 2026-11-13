//! Run identity, task metadata, lifecycle and timeout labels.
use crate::{
    context::ToolContext, protocol::management::TaskMetadata, shell_manager::ShellManager,
};
use anyhow::{anyhow, Result};
use remote_executor_terminal::SessionDetail;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        LazyLock, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time;
const OUTPUT_LIMIT: usize = 30_000;
const EXBASH_PREFIX: &str = "rex-";

#[derive(Clone, Debug, Serialize)]
pub(super) struct RunDetail {
    #[serde(rename = "asyncID")]
    pub(super) async_id: String,
    pub(super) pid: Option<u32>,
    pub(super) state: String,
    #[serde(rename = "exitCode", skip_serializing_if = "Option::is_none")]
    pub(super) exit_code: Option<Value>,
    #[serde(rename = "totalOutput")]
    pub(super) total_output: u64,
    pub(super) command: String,
    pub(super) title: String,
    pub(super) description: String,
    pub(super) cwd: String,
    pub(super) timeout: Option<i64>,
    #[serde(rename = "startedAt")]
    pub(super) started_at: u128,
    #[serde(rename = "endedAt", skip_serializing_if = "Option::is_none")]
    pub(super) ended_at: Option<u128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<String>,
}
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static EXIT_CODE_LABELS: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn manager(ctx: &ToolContext) -> Result<ShellManager> {
    ctx.shell_manager()
        .ok_or_else(|| anyhow!("exbash requires a PTY-backed ShellManager"))
}
pub(super) fn run_detail(
    manager: &ShellManager,
    async_id: &str,
    timeout: Option<i64>,
) -> Result<RunDetail> {
    let detail = manager.core().detail(async_id)?;
    Ok(run_detail_from_session(
        detail,
        manager.task_metadata(async_id),
        timeout,
    ))
}

pub(super) fn list_run_details(
    manager: &ShellManager,
    filter: Option<&str>,
    owner: Option<&crate::protocol::management::SessionOwner>,
) -> Result<Vec<RunDetail>> {
    let mut runs = Vec::new();
    for summary in manager.core().list() {
        if summary.exit_code.is_some() || !summary.pty.starts_with(EXBASH_PREFIX) {
            continue;
        }
        if owner.is_some_and(|o| !manager.is_owned_by(&summary.pty, o)) {
            continue;
        }
        if filter.is_some_and(|id| id != summary.pty) {
            continue;
        }
        runs.push(run_detail(manager, &summary.pty, None)?);
    }
    Ok(runs)
}

pub(super) fn format_run_details(runs: &[RunDetail]) -> String {
    if runs.is_empty() {
        return "No async runs".to_string();
    }
    runs.iter()
        .map(|run| {
            let exit = run
                .exit_code
                .as_ref()
                .map(|code| format!(" exit={}", exit_code_value_text(code)))
                .unwrap_or_default();
            format!(
                "{} {}{} totalOutput={} title={} description={} command={}",
                run.async_id,
                run.state,
                exit,
                run.total_output,
                single_line_list_text(&run.title),
                single_line_list_text(&run.description),
                clipped_list_command(&run.command)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn clipped_list_command(command: &str) -> String {
    single_line_list_text(command).chars().take(30).collect()
}

fn single_line_list_text(text: &str) -> String {
    text.replace("\r\n", "\\n").replace(['\n', '\r'], "\\n")
}

pub(super) async fn stop_run(manager: &ShellManager, async_id: &str) -> Result<RunDetail> {
    if let Ok(detail) = manager.core().detail(async_id) {
        if detail.exit_code.is_some() {
            return Ok(run_detail_from_session(
                detail,
                manager.task_metadata(async_id),
                None,
            ));
        }
    }
    let session = manager
        .core()
        .session(async_id)
        .ok_or_else(|| anyhow!("Async run not found: {async_id}"))?;
    set_exit_code_label(async_id, "stopped");
    let kill_error = session.kill().err();
    let wait_result = manager
        .core()
        .wait_exit_code_timeout(async_id, Duration::from_millis(500))
        .await;

    if let Ok(detail) = manager.core().detail(async_id) {
        if detail.exit_code.is_some() {
            return Ok(run_detail_from_session(
                detail,
                manager.task_metadata(async_id),
                None,
            ));
        }
    }

    if let Some(error) = kill_error {
        clear_exit_code_label(async_id);
        return Err(error);
    }
    wait_result?;
    run_detail(manager, async_id, None)
}

pub(super) async fn remove_run(
    manager: &ShellManager,
    async_id: &str,
) -> Result<serde_json::Value> {
    let detail = manager.core().detail(async_id)?;
    let mut stopped = false;
    let exit_code = if let Some(code) = detail.exit_code {
        Some(code)
    } else {
        // Confirm the cached exit before treating a native kill error as failure,
        // just like explicit stop (ConPTY can exit while termination is requested).
        stop_run(manager, async_id).await?;
        stopped = true;
        manager.core().try_exit_code(async_id)?
    };

    let mut value = json!({
        "asyncID": async_id,
        "removed": true,
        "stopped": stopped,
    });
    if let Some(code) = exit_code {
        value["exitCode"] = json!(code);
    }
    manager.remove_pty(async_id);
    clear_exit_code_label(async_id);
    Ok(value)
}

pub(super) fn clear_exit_code_label(async_id: &str) {
    EXIT_CODE_LABELS.lock().unwrap().remove(async_id);
}

pub(super) fn exit_code_display(async_id: &str, code: u32) -> String {
    exit_code_label(async_id).unwrap_or_else(|| code.to_string())
}

pub(super) fn exit_code_json(async_id: &str, code: Option<u32>) -> Option<Value> {
    code.map(|code| {
        exit_code_label(async_id)
            .map(Value::String)
            .unwrap_or_else(|| json!(code))
    })
}

pub(super) fn clip(text: &str) -> String {
    if text.len() <= OUTPUT_LIMIT {
        text.to_string()
    } else {
        format!("{}\n\n...", &text[..OUTPUT_LIMIT])
    }
}

pub(super) fn merge_json(target: &mut serde_json::Value, source: serde_json::Value) {
    if let (Some(target), Some(source)) = (target.as_object_mut(), source.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
}

fn run_detail_from_session(
    detail: SessionDetail,
    metadata: TaskMetadata,
    timeout: Option<i64>,
) -> RunDetail {
    let async_id = detail.pty;
    let command = detail.command.join(" ");
    let raw_exit_code = detail.exit_code;
    let exit_code = exit_code_json(&async_id, raw_exit_code);
    let state = if exit_code.is_some() {
        "stopped"
    } else {
        "running"
    }
    .to_string();
    let ended_at = detail.ended_at.map(u128::from);
    RunDetail {
        async_id,
        pid: detail.process_id,
        state,
        exit_code,
        total_output: detail.output_total_bytes,
        command: command.clone(),
        title: metadata.title,
        description: metadata.description,
        cwd: detail.cwd.unwrap_or_default(),
        timeout,
        started_at: u128::from(detail.created_at),
        ended_at,
        error: None,
    }
}

fn exit_code_label(async_id: &str) -> Option<String> {
    EXIT_CODE_LABELS.lock().unwrap().get(async_id).cloned()
}

fn exit_code_value_text(value: &Value) -> String {
    value
        .as_str()
        .map(ToString::to_string)
        .unwrap_or_else(|| value.to_string())
}

fn set_exit_code_label(async_id: &str, label: &str) {
    EXIT_CODE_LABELS
        .lock()
        .unwrap()
        .insert(async_id.to_string(), label.to_string());
}

pub(super) fn spawn_timeout(manager: ShellManager, async_id: String, timeout: u64) {
    let Some(session) = manager.core().session(&async_id) else {
        return;
    };
    let mut exited = session.subscribe_exit_code();
    tokio::spawn(async move {
        tokio::select! {
            _ = exited.recv() => {},
            _ = time::sleep(Duration::from_millis(timeout)) => {
                if session.try_exit_code().ok().flatten().is_none() {
                    set_exit_code_label(&async_id, "timeout");
                    let _ = session.kill();
                }
            }
        }
    });
}
pub(super) fn next_id() -> String {
    format!(
        "{}{}-{}",
        EXBASH_PREFIX,
        now_ms(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}
