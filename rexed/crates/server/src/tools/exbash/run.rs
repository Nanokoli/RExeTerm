//! Command construction, PTY creation and initial output observation.
use super::{
    options::ExbashOptions,
    runs::{clear_exit_code_label, clip, manager, next_id, run_detail, spawn_timeout, RunDetail},
};
use crate::{
    context::ToolContext,
    protocol::{tool_output, tool_output_full, ToolResult},
    shell_manager::ShellManager,
};
use anyhow::{anyhow, Result};
use remote_executor_terminal::session::OutputSubscription;
use remote_executor_terminal::CommandSpec;
use serde_json::json;
use std::time::Duration;
use tokio::time;
const READ_TIMEOUT: u64 = 10_000;
pub(super) struct StartedJob {
    pub(super) manager: ShellManager,
    pub(super) async_id: String,
    pub(super) timeout: Option<i64>,
    output: OutputSubscription,
    observed: super::capture::OutputCapture,
}
pub(super) async fn run_command(options: ExbashOptions, ctx: &ToolContext) -> Result<ToolResult> {
    let read_timeout = options.read_timeout.unwrap_or(READ_TIMEOUT);
    let mut job = start_job(&options, ctx).await?;
    if let Some((detail, output)) = wait_for_stop_with_output(&mut job, read_timeout).await? {
        let mut size = super::terminal_size(&job.manager, &job.async_id)?;
        size["view"] = json!("stream");
        job.manager.remove_pty(&job.async_id);
        clear_exit_code_label(&job.async_id);
        return Ok(ToolResult {
            metadata: json!({
                "asyncID":job.async_id,
                "state":"stopped",
                "startedAt":detail.started_at,
                "endedAt":detail.ended_at,
                "terminalSize":size,
                "output": clip(&output),
                "exitCode": detail.exit_code,
                "command": detail.command,
                "title": detail.title,
                "description": detail.description,
                "cwd": detail.cwd,
                "totalOutput": detail.total_output,
                "outputTruncated": job.observed.truncated,
                "outputLimitBytes": super::capture::CAPTURE_LIMIT,
            }),
            output: tool_output(output),
        });
    }

    let detail = run_detail(&job.manager, &job.async_id, job.timeout)?;
    let snapshot = job.observed.text();
    let mut value = serde_json::to_value(&detail)?;
    value["outputTruncated"] = json!(job.observed.truncated);
    value["outputLimitBytes"] = json!(super::capture::CAPTURE_LIMIT);
    value["detached"] = json!(true);
    value["terminalSize"] = super::terminal_size(&job.manager, &job.async_id)?;
    let message = format!("{} detached", job.async_id);
    Ok(ToolResult {
        metadata: value.clone(),
        output: tool_output_full(message, snapshot, ""),
    })
}
pub(super) async fn start_job(options: &ExbashOptions, ctx: &ToolContext) -> Result<StartedJob> {
    if options.io.as_deref() == Some("stdio") {
        return Err(anyhow!("stdio jobs use their own lifecycle manager"));
    }
    let command = options
        .command
        .clone()
        .ok_or_else(|| anyhow!("command is required"))?;
    let manager = manager(ctx)?;
    let cwd = options
        .workdir
        .as_ref()
        .filter(|path| !path.as_os_str().is_empty())
        .map(|path| ctx.resolve(path))
        .unwrap_or_else(|| ctx.directory.clone());
    let id = next_id();
    let timeout_ms = options.timeout_ms()?;
    let (session, output) = manager.create_owned_pty_with_output(
        id.clone(),
        command_spec(&command, &cwd, options, ctx)?,
        options.cols,
        options.rows,
        ctx.client_owner.clone(),
        options.task_metadata(),
    )?;
    if ctx.cancellation.as_ref().is_some_and(|c| *c.borrow()) {
        let _ = session.kill();
        manager.remove_pty(&id);
        return Err(anyhow!("client lease expired while starting task"));
    }

    if let Some(timeout) = timeout_ms {
        spawn_timeout(manager.clone(), id.clone(), timeout);
    }

    Ok(StartedJob {
        manager,
        async_id: id,
        timeout: options.timeout,
        output,
        observed: Default::default(),
    })
}

pub(super) async fn wait_for_stop_with_output(
    job: &mut StartedJob,
    timeout: u64,
) -> Result<Option<(RunDetail, String)>> {
    let deadline = time::Instant::now() + Duration::from_millis(timeout);

    loop {
        drain_output(&mut job.output, &mut job.observed)?;
        if job.manager.core().try_exit_code(&job.async_id)?.is_some() {
            time::sleep(Duration::from_millis(20)).await;
            drain_output(&mut job.output, &mut job.observed)?;
            let detail = run_detail(&job.manager, &job.async_id, job.timeout)?;
            return Ok(Some((detail, job.observed.text())));
        }

        let now = time::Instant::now();
        if now >= deadline {
            drain_output(&mut job.output, &mut job.observed)?;
            return Ok(None);
        }
        tokio::select! {
            chunk = job.output.recv(), if !job.output.is_closed() => match chunk {
                Ok(bytes) => job.observed.extend(bytes),
                Err(_) if job.output.is_closed() => {},
                Err(error) => return Err(error),
            },
            _ = time::sleep((deadline - now).min(Duration::from_millis(20))) => {},
        }
    }
}
fn drain_output(
    rx: &mut OutputSubscription,
    output: &mut super::capture::OutputCapture,
) -> Result<()> {
    while !rx.is_closed() {
        match rx.try_recv() {
            Ok(Some(chunk)) => output.extend(chunk),
            Ok(None) => break,
            Err(_) if rx.is_closed() => break,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
pub(super) fn command_spec(
    command: &str,
    cwd: &std::path::Path,
    options: &ExbashOptions,
    ctx: &ToolContext,
) -> Result<CommandSpec> {
    if options.shell {
        return shell_command_spec(command, cwd, options, ctx);
    }

    let parts = shell_words::split(command)
        .map_err(|err| anyhow!("failed to parse command arguments: {err}"))?;
    let Some((program, args)) = parts.split_first() else {
        return Err(anyhow!("command is required"));
    };
    Ok(CommandSpec::new(program.clone())
        .args(args.iter().map(String::as_str))
        .cwd(cwd.to_path_buf()))
}

fn shell_command_spec(
    command: &str,
    cwd: &std::path::Path,
    options: &ExbashOptions,
    ctx: &ToolContext,
) -> Result<CommandSpec> {
    if let Some(prepared) = &options.shell_command {
        let program = prepared
            .candidates
            .iter()
            .find_map(|candidate| crate::settings::resolve_program(candidate, cwd))
            .ok_or_else(|| {
                anyhow!(
                    "no candidate found for client shell profile `{}`; checked: {}",
                    prepared.profile,
                    prepared.candidates.join(", ")
                )
            })?;
        return Ok(CommandSpec::new(program)
            .args(prepared.args.iter().map(String::as_str))
            .cwd(cwd.to_path_buf()));
    }
    if let Some(settings) = ctx.settings_store() {
        return settings.command_spec(options.shell_profile.as_deref(), command, cwd);
    }
    Ok(platform_shell_command_spec(command, cwd))
}

fn platform_shell_command_spec(command: &str, cwd: &std::path::Path) -> CommandSpec {
    if cfg!(windows) {
        CommandSpec::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                command,
            ])
            .cwd(cwd.to_path_buf())
    } else {
        CommandSpec::new("sh")
            .args(["-c", command])
            .cwd(cwd.to_path_buf())
    }
}
