//! Attach observation, PTY control ownership and stopped-task responses.
use super::{
    input::input_data,
    options::ExbashOptions,
    runs::{exit_code_display, exit_code_json, manager, merge_json},
};
use crate::{
    context::ToolContext,
    protocol::{tool_output_full, ToolResult},
    shell_manager::ShellManager,
};
use anyhow::{anyhow, Result};
use serde_json::json;
use std::time::Duration;
use tokio::time;
const INPUT_TIMEOUT: u64 = 10_000;
pub(super) async fn attach_input(options: ExbashOptions, ctx: &ToolContext) -> Result<ToolResult> {
    let read_timeout = attach_read_timeout(&options)?;

    let id = options
        .async_id
        .clone()
        .ok_or_else(|| anyhow!("asyncID is required"))?;
    let manager = manager(ctx)?;
    let read_only = manager.terminal_read_only(&id, ctx.client_owner.as_ref())?;
    anyhow::ensure!(
        !read_only || (options.text_input().is_none() && options.file_path_input().is_none()),
        "PTY is read-only for this client"
    );
    let detail = manager.core().detail(&id)?;
    let task = manager.task_metadata(&id);
    if let Some(exit_code) = detail.exit_code {
        let input_failed = options.text_input().is_some() || options.file_path_input().is_some();
        let exit_code_display = exit_code_display(&id, exit_code);
        let message = stopped_attach_message(&exit_code_display, input_failed);
        let exit_code_value =
            exit_code_json(&id, Some(exit_code)).unwrap_or_else(|| json!(exit_code));
        let mut value = json!({
            "terminalSize": super::terminal_size(&manager, &id)?,
            "asyncID": id,
            "title": task.title,
            "description": task.description,
            "wrote": 0,
            "source": requested_input_source(&options),
            "outputBytes": 0,
            "state": "stopped",
            "exitCode": exit_code_value,
        });
        add_raw_pretty(&manager, &id, &mut value, options.show_raw_pretty)?;
        return Ok(ToolResult {
            metadata: value,
            output: tool_output_full(message, String::new(), ""),
        });
    }

    let output = manager.core().subscribe_output(&id)?;
    let (data, source) = input_data(&options, ctx).await?;
    let output_offset = manager.core().session(&id).unwrap().output_total_bytes();
    let controller = (!data.is_empty()).then(|| match &ctx.client_owner {
        Some(owner) => format!("rec:{id}:{}", owner.session_id),
        None => format!("rec:{id}"),
    });
    if !data.is_empty() {
        let controller = controller.as_ref().unwrap();
        manager.events.emit(
            "pty.control_requested",
            ctx.client_owner.as_ref(),
            json!({"programId":id,"controller":controller}),
        );
        manager.claim_and_write(&id, controller, ctx.client_owner.clone(), &data)?;
        manager.broadcast_meta(&id);
    }

    let mut value = json!({
        "terminalSize": super::terminal_size(&manager, &id)?,
        "asyncID": id,
        "title": task.title,
        "description": task.description,
        "wrote": data.len(),
        "source": source,
    });
    let (snapshot, attach_meta) = attach(
        &manager,
        &id,
        output_offset,
        output,
        read_timeout,
        controller.as_deref(),
    )
    .await?;
    merge_json(&mut value, attach_meta);
    let message = take_message(&mut value);
    add_raw_pretty(&manager, &id, &mut value, options.show_raw_pretty)?;
    Ok(ToolResult {
        metadata: value.clone(),
        output: tool_output_full(message, snapshot, ""),
    })
}

pub(super) fn attach_read_timeout(options: &ExbashOptions) -> Result<u64> {
    if let Some(read_timeout) = options.read_timeout {
        return Ok(read_timeout);
    }
    let Some(timeout) = options.timeout else {
        return Ok(INPUT_TIMEOUT);
    };
    if timeout < 0 {
        return Err(anyhow!("timeout must be non-negative for mode attach"));
    }
    Ok((timeout as u64).min(INPUT_TIMEOUT))
}

fn take_message(value: &mut serde_json::Value) -> String {
    let Some(object) = value.as_object_mut() else {
        return String::new();
    };
    object
        .remove("message")
        .and_then(|message| message.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn add_raw_pretty(
    manager: &ShellManager,
    id: &str,
    value: &mut serde_json::Value,
    show: bool,
) -> Result<()> {
    if !show {
        return Ok(());
    }
    let _ = (manager, id);
    let formatted: Vec<u8> = Vec::new();
    merge_json(
        value,
        json!({
            "rawPretty": String::from_utf8_lossy(&formatted),
        }),
    );
    Ok(())
}

fn requested_input_source(options: &ExbashOptions) -> &'static str {
    match (
        options.text_input().is_some(),
        options.file_path_input().is_some(),
    ) {
        (true, false) => "text",
        (false, true) => "file",
        (true, true) => "input",
        (false, false) => "attach",
    }
}

fn stopped_attach_message(exit_code: &str, input_failed: bool) -> String {
    let message = format!("task already exited with code {exit_code}");
    if input_failed {
        format!("input failed: {message}")
    } else {
        message
    }
}
pub(super) async fn attach(
    manager: &ShellManager,
    async_id: &str,
    output_offset: u64,
    mut output: remote_executor_terminal::session::OutputSubscription,
    timeout: u64,
    controller: Option<&str>,
) -> Result<(String, serde_json::Value)> {
    let bytes = wait_attach_timeout(manager, async_id, timeout, controller, &mut output).await?;
    let text = bytes.text();
    let detail = manager.core().detail(async_id)?;
    let output_bytes = detail.output_total_bytes.saturating_sub(output_offset);
    let mut metadata = json!({ "outputBytes": output_bytes, "outputTruncated": bytes.truncated, "outputLimitBytes": super::capture::CAPTURE_LIMIT });
    if let Some(exit_code) = detail.exit_code {
        let exit_code_value =
            exit_code_json(async_id, Some(exit_code)).unwrap_or_else(|| json!(exit_code));
        let exit_code_display = exit_code_display(async_id, exit_code);
        metadata["state"] = json!("stopped");
        metadata["exitCode"] = exit_code_value;
        metadata["message"] = json!(format!("task exited with code {exit_code_display}"));
    }
    Ok((text, metadata))
}

async fn wait_attach_timeout(
    manager: &ShellManager,
    async_id: &str,
    timeout: u64,
    controller: Option<&str>,
    output: &mut remote_executor_terminal::session::OutputSubscription,
) -> Result<super::capture::OutputCapture> {
    let mut bytes = super::capture::OutputCapture::default();
    let deadline = time::Instant::now() + Duration::from_millis(timeout);
    loop {
        while !output.is_closed() {
            match output.try_recv() {
                Ok(Some(chunk)) => bytes.extend(chunk),
                Ok(None) => break,
                Err(_) if output.is_closed() => break,
                Err(error) => return Err(error),
            }
        }
        if let Some(controller) = controller {
            let current = manager.controller_id(async_id);
            if current.as_deref() != Some(controller) {
                let attached_by = current.unwrap_or_else(|| "unknown".to_string());
                return Err(anyhow!("control lost: someone attached: {attached_by}"));
            }
        }

        if manager.core().detail(async_id)?.exit_code.is_some() {
            return Ok(bytes);
        }
        let now = time::Instant::now();
        if now >= deadline {
            return Ok(bytes);
        }
        tokio::select! {
            chunk = output.recv(), if !output.is_closed() => match chunk { Ok(chunk) => bytes.extend(chunk), Err(_) if output.is_closed() => {}, Err(error) => return Err(error) },
            _ = time::sleep((deadline - now).min(Duration::from_millis(20))) => {},
        }
    }
}
