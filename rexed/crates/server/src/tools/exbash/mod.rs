//! Exbash mode routing; PTY lifetime and terminal transport remain separate.
mod attach;
mod capture;
mod input;
mod options;
mod run;
mod runs;
#[cfg(test)]
mod test;

use crate::{
    context::ToolContext,
    protocol::{tool_output, ToolResult},
    shell_manager::ShellManager,
};
use anyhow::{anyhow, Result};
use attach::attach_input;
pub use options::{ExbashOptions, ExbashOutput};
use run::run_command;
use runs::{list_run_details, manager, remove_run, stop_run};
use serde_json::json;
pub async fn exbash(options: ExbashOptions, ctx: &ToolContext) -> Result<ToolResult> {
    options.validate_input_limits()?;
    anyhow::ensure!(
        matches!(options.io.as_deref(), None | Some("pty" | "stdio")),
        "io must be stdio or pty"
    );
    anyhow::ensure!(
        (options.io.as_deref() == Some("stdio")
            || options
                .async_id
                .as_deref()
                .is_some_and(|id| id.starts_with("stdio-")))
            || (options.stdin.is_none() && !options.keep_stdin && !options.close_stdin),
        "stdin/keepStdin/closeStdin require stdio I/O"
    );
    let mut result = exbash_inner(options, ctx).await?;
    // This declares raw terminal data, not a server-rendered screen snapshot.
    if let Some(size) = result.metadata.get("terminalSize").cloned() {
        result.metadata["terminalOutput"] = json!({"format":"raw-pty-v1", "cols":size["cols"], "rows":size["rows"], "view":size["view"]});
        result
            .metadata
            .as_object_mut()
            .unwrap()
            .remove("terminalSize");
    }
    Ok(result)
}
async fn exbash_inner(mut options: ExbashOptions, ctx: &ToolContext) -> Result<ToolResult> {
    let mode = options.mode.as_deref().unwrap_or_default().to_owned();
    let stdio_id = options
        .async_id
        .as_deref()
        .is_some_and(|id| id.starts_with("stdio-"));
    anyhow::ensure!(
        options.io.is_none()
            || options.async_id.is_none()
            || (stdio_id == (options.io.as_deref() == Some("stdio"))),
        "task I/O backend does not match io"
    );
    if matches!(mode.as_str(), "run" | "shell") && options.io.as_deref() == Some("stdio") {
        options.shell = mode == "shell";
        let manager = manager(ctx)?;
        let cwd = options
            .workdir
            .as_ref()
            .map(|path| ctx.resolve(path))
            .unwrap_or_else(|| ctx.directory.clone());
        let command = options
            .command
            .as_deref()
            .ok_or_else(|| anyhow!("command is required"))?;
        let spec = run::command_spec(command, &cwd, &options, ctx)?;
        let id = format!("stdio-{}", runs::next_id());
        return manager
            .stdio
            .start(
                crate::stdio_tasks::Start {
                    id,
                    spec,
                    metadata: options.task_metadata(),
                    timeout: options.timeout_ms()?,
                    wait: options.read_timeout.unwrap_or(1000),
                    stdin: options.stdin.clone(),
                    keep_stdin: options.keep_stdin,
                },
                ctx,
                manager.events.clone(),
            )
            .await;
    }
    if stdio_id {
        anyhow::ensure!(
            matches!(
                mode.as_str(),
                "attach" | "status" | "stop" | "remove" | "list"
            ),
            "invalid stdio task operation"
        );
        let manager = manager(ctx)?;
        let id = options.async_id.as_deref().unwrap();
        if mode == "list" {
            let runs: Vec<_> = manager
                .stdio
                .list(ctx.client_owner.as_ref())
                .into_iter()
                .filter(|v| v["asyncID"] == id)
                .collect();
            return Ok(ToolResult {
                metadata: json!({"runs":runs}),
                output: tool_output(""),
            });
        }
        anyhow::ensure!(
            mode == "attach"
                || (options.text_input().is_none()
                    && options.file_path_input().is_none()
                    && !options.close_stdin),
            "input and closeStdin require attach"
        );
        let (bytes, _) = input::input_data(&options, ctx).await?;
        return manager
            .stdio
            .operate(
                id,
                &mode,
                attach::attach_read_timeout(&options)?,
                (!bytes.is_empty()).then_some(bytes),
                options.close_stdin,
                ctx.client_owner.as_ref(),
            )
            .await;
    }
    match mode.as_str() {
        "run" => run_command(options, ctx).await,
        "shell" => {
            options.shell = true;
            run_command(options, ctx).await
        }
        "attach" => attach_input(options, ctx).await,
        "list" => list(options, ctx).await,
        "status" => {
            let manager = manager(ctx)?;
            let id = options
                .async_id
                .as_deref()
                .ok_or_else(|| anyhow!("asyncID is required"))?;
            manager.require_owner(id, ctx.client_owner.as_ref())?;
            Ok(ToolResult {
                metadata: exbash_run_detail(&manager, id)?,
                output: tool_output(""),
            })
        }
        "stop" => stop(options, ctx).await,
        "remove" => remove(options, ctx).await,
        "" => Err(anyhow!("exbash mode is required")),
        mode => Err(anyhow!("unknown exbash mode: {mode}")),
    }
}

pub(crate) fn forget_run(id: &str) {
    runs::clear_exit_code_label(id);
}
pub fn exbash_run_detail(manager: &ShellManager, async_id: &str) -> Result<serde_json::Value> {
    Ok(serde_json::to_value(runs::run_detail(
        manager, async_id, None,
    )?)?)
}
async fn list(options: ExbashOptions, ctx: &ToolContext) -> Result<ToolResult> {
    let manager = manager(ctx)?;
    if let Some(id) = options.async_id.as_deref() {
        manager.require_owner(id, ctx.client_owner.as_ref())?;
    }
    let runs = list_run_details(
        &manager,
        options.async_id.as_deref(),
        ctx.client_owner.as_ref(),
    )?;
    let formatted = runs::format_run_details(&runs);
    let mut runs: Vec<_> = runs
        .iter()
        .map(|run| serde_json::to_value(run).unwrap())
        .collect();
    runs.extend(manager.stdio.list(ctx.client_owner.as_ref()));
    let value = json!({ "runs": runs });
    Ok(ToolResult {
        metadata: value.clone(),
        output: tool_output(formatted),
    })
}

async fn stop(options: ExbashOptions, ctx: &ToolContext) -> Result<ToolResult> {
    let id = options
        .async_id
        .clone()
        .ok_or_else(|| anyhow!("asyncID is required"))?;
    let manager = manager(ctx)?;
    manager.require_owner(&id, ctx.client_owner.as_ref())?;
    let detail = stop_run(&manager, &id).await?;
    let output = String::new();
    let mut metadata = serde_json::to_value(&detail)?;
    metadata["terminalSize"] = terminal_size(&manager, &id)?;
    Ok(ToolResult {
        metadata,
        output: tool_output(output),
    })
}

fn terminal_size(manager: &ShellManager, id: &str) -> Result<serde_json::Value> {
    let detail = manager.core().detail(id)?;
    Ok(json!({"cols":detail.cols,"rows":detail.rows}))
}
async fn remove(options: ExbashOptions, ctx: &ToolContext) -> Result<ToolResult> {
    let id = options
        .async_id
        .clone()
        .ok_or_else(|| anyhow!("asyncID is required"))?;
    let manager = manager(ctx)?;
    manager.require_owner(&id, ctx.client_owner.as_ref())?;
    remove_run(&manager, &id).await?;
    Ok(ToolResult {
        metadata: json!({ "ok": true }),
        output: tool_output("ok"),
    })
}
