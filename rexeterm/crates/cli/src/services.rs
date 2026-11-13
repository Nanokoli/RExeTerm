//! CLI-only host creation, discovery and current-directory binding.
use crate::ipc_client::IpcClient;
use anyhow::{ensure, Context, Result};
use clap::{Args as ClapArgs, Subcommand};
use rexeterm_cli_host::{spawn_detached, Registry, ServiceRecord};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

#[derive(Subcommand, Debug)]
pub enum ServiceCommand {
    /// Create a host: generate a UUID64, start the host role, let it publish the binding.
    Create {
        /// Also enable a local executor inside the new host.
        #[arg(long)]
        local: bool,
    },
    /// List reserved IDs and their actual running state for this OS boot.
    List,
    /// Restart the bound host with its existing UUID64.
    Start,
    /// Stop the daemon serving the bound UUID64; rarely needed, mainly to switch UUID64.
    Stop,
    /// Show the bound UUID64, whether its daemon answers, and the daemon's own info.
    Info,
    /// Ask for a UUID64; the host writes the binding beside the executable when it starts.
    Bind {
        /// Service UUID64 reported by `service create` or `service list`.
        uuid: String,
    },
    /// Internal: run this binary as the host. The CLI starts it and it reads the
    /// UUID file the CLI wrote, then publishes the binding beside the executable.
    #[command(hide = true)]
    Runasdaemon(DaemonArgs),
}
#[derive(Clone, ClapArgs, Debug)]
pub struct DaemonArgs {
    /// Private state directory (lock, log, workspace) for this host.
    #[arg(long, hide = true)]
    pub config_dir: Option<PathBuf>,
    /// Run a local executor inside this host.
    #[arg(long)]
    pub local: bool,
}

fn from_record(record: &ServiceRecord) -> Result<IpcClient> {
    let target = record
        .target
        .clone()
        .context("bound host is stopped; run service start (its UUID64 is still valid)")?;
    let token = record
        .token_file
        .as_ref()
        .map(std::fs::read_to_string)
        .transpose()?
        .map(|s| s.trim().to_owned());
    IpcClient::new(target, token.context("missing host IPC token")?, &record.id)
}
/// Connect to the machine host, starting its host role when the socket is gone.
pub async fn connect(registry: &Registry, id: &str) -> Result<IpcClient> {
    match client(registry, id).await {
        Ok(client) => Ok(client),
        Err(first) => {
            // No usable socket (stopped host, or a socket from the previous boot):
            // the CLI brings the host role back up from the same executable.
            let record = registry.resolve(id)?;
            match start(registry, record).await {
                Ok(_) => client(registry, id).await,
                Err(error) => Err(first.context(format!("host restart failed: {error:#}"))),
            }
        }
    }
}

pub async fn client(registry: &Registry, id: &str) -> Result<IpcClient> {
    // Every actual IPC request verifies service ID and token; no extra info round trip.
    from_record(&registry.resolve(id)?)
}
async fn running(record: &ServiceRecord) -> bool {
    match from_record(record) {
        Ok(client) => client.verify_service().await.is_ok(),
        Err(_) => false,
    }
}
async fn start(registry: &Registry, record: ServiceRecord) -> Result<ServiceRecord> {
    if running(&record).await {
        return Ok(record);
    }
    // One program, two roles: the host role is this same executable, detached.
    let binary = std::env::current_exe()?;
    let log_path = record.config_dir.join("cli-host.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    rexeterm_cli_host::private_file(&log_path)?;
    registry.write_uuid(&record.id)?;
    let mut command = Command::new(binary);
    command
        .arg("service")
        .arg("runasdaemon")
        .arg("--registry-dir")
        .arg(registry.base_directory());
    if record.local {
        command.arg("--local");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.arg0("rexetermd");
    }
    command
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    let mut child = spawn_detached(&mut command)?;
    let ready = async {
        for attempt in 0..150 {
            if let Some(status) = child.try_wait()? {
                // Another CLI may have won the per-config lock. Return its verified host.
                let latest = registry.resolve(&record.id)?;
                if running(&latest).await {
                    return Ok(latest);
                }
                if attempt >= 10 {
                    anyhow::bail!("CLI host exited ({status}); see {}", log_path.display());
                }
            }
            let latest = registry.resolve(&record.id)?;
            if latest.pid == Some(child.id()) && running(&latest).await {
                return Ok(latest);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        anyhow::bail!("CLI host did not become ready; see {}", log_path.display())
    }
    .await;
    if ready.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    // Keep the detached child alive after the invoking CLI exits.
    ready
}
pub async fn execute(
    registry: &Registry,
    command: ServiceCommand,
    bound: Option<&str>,
    cwd: &Path,
) -> Result<Value> {
    match command {
        ServiceCommand::Create { local } => {
            ensure!(
                bound.is_none(),
                "service create generates/selects its own ID; omit --service-id"
            );
            // The host keeps no preferences, so its private state (lock, log,
            // workspace) sits beside the machine-wide registry, never in a project.
            let config = registry.base_directory().join("host");
            // The machine already has one host: reuse it instead of spawning another.
            let live = registry
                .records()?
                .into_iter()
                .find(|record| record.target.is_some());
            let (record, reused) = match live {
                Some(record) => (record, true),
                None => (
                    start(registry, registry.reserve(&config, local)?).await?,
                    false,
                ),
            };
            registry.write_binding(&record.id)?;
            Ok(
                json!({"serviceId":record.id,"running":true,"reused":reused,"pid":record.pid,"target":record.target,"stateDir":record.config_dir,"bindingFile":registry.binding_path()}),
            )
        }
        ServiceCommand::List => {
            let records = registry.records()?;
            let rows=futures_util::future::join_all(records.into_iter().map(|r|async move {
                let live=running(&r).await;
                json!({"serviceId":r.id,"running":live,"pid":if live{r.pid}else{None},"target":if live{r.target}else{None},"stateDir":r.config_dir})
            })).await;
            Ok(json!(rows))
        }
        ServiceCommand::Bind { uuid } => {
            let id = rexeterm_cli_host::normalize_id(&uuid)?;
            // Not a binding yet: the host publishes it beside the executable when
            // it starts, so an unreachable UUID64 never looks live.
            let record = registry.resolve(&id)?;
            registry.write_uuid(&id)?;
            Ok(json!({
                "serviceId": id,
                "bound": running(&record).await,
                "requested": registry.uuid_path(),
                "bindingFile": registry.binding_path(),
            }))
        }
        ServiceCommand::Info => {
            let planned = registry.read_uuid();
            let bound = registry.read_binding(cwd).ok().or_else(|| planned.clone());
            let Some(id) = bound else {
                return Ok(json!({
                    "serviceId": Value::Null,
                    "bound": false,
                    "running": false,
                    "bindingFile": registry.binding_path(),
                }));
            };
            registry.write_uuid(&id)?;
            let record = registry.resolve(&id).ok();
            let mut live = false;
            if let Some(record) = &record {
                if let Ok(client) = client(registry, &record.id).await {
                    live = client.call("info", json!({})).await.is_ok();
                }
            }
            Ok(json!({
                "serviceId": id,
                "bound": true,
                "running": live,
                "bindingFile": registry.binding_path(),
                "uuidFile": registry.uuid_path(),
                "host": record.clone().map(|record| json!({
                    "pid": record.pid,
                    "target": record.target,
                    "local": record.local,
                    "stateDir": record.config_dir,
                })),
            }))
        }
        ServiceCommand::Runasdaemon(_) => unreachable!("handled before any IPC"),
        ServiceCommand::Start => {
            let id = selected(registry, bound, cwd)?;
            let record = start(registry, registry.resolve(&id)?).await?;
            Ok(
                json!({"serviceId":record.id,"running":true,"pid":record.pid,"target":record.target}),
            )
        }
        ServiceCommand::Stop => {
            let id = selected(registry, bound, cwd)?;
            let record = registry.resolve(&id)?;
            if !running(&record).await {
                return Ok(json!({"serviceId":id,"running":false}));
            }
            from_record(&record)?.call("shutdown", json!({})).await?;
            for _ in 0..100 {
                if registry.resolve(&id)?.target.is_none() {
                    return Ok(json!({"serviceId":id,"running":false}));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            anyhow::bail!("host stop requested; it is still shutting down")
        }
    }
}
/// A missing binding is not an error: the CLI brings the machine host up on
/// demand, so `service create` stays optional.
pub async fn ensure(registry: &Registry, cwd: &Path) -> Result<String> {
    let _ = cwd;
    let record = match registry
        .records()?
        .into_iter()
        .find(|record| record.target.is_some())
    {
        Some(record) => record,
        None => {
            let config = registry.base_directory().join("host");
            start(registry, registry.reserve(&config, true)?).await?
        }
    };
    registry.write_binding(&record.id)?;
    Ok(record.id)
}

pub fn selected(registry: &Registry, explicit: Option<&str>, cwd: &Path) -> Result<String> {
    if let Some(explicit) = explicit {
        return rexeterm_cli_host::normalize_id(explicit);
    }
    registry.read_binding(cwd).or_else(|_| {
        registry.read_uuid().ok_or_else(|| {
            anyhow::anyhow!("no CLI host binding: run service bind <UUID64> or service create")
        })
    })
}
pub async fn bind(registry: &Registry, id: &str, cwd: &Path) -> Result<Value> {
    let id = rexeterm_cli_host::normalize_id(id)?;
    let record = registry.resolve(&id)?;
    // Stopped hosts are still valid bindings until the next OS boot.
    let _ = cwd;
    registry.write_binding(&id)?;
    Ok(
        json!({"serviceId":id,"running":running(&record).await,"bindingFile":registry.binding_path()}),
    )
}
