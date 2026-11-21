//! CLI-only host creation, discovery and current-directory binding.
use crate::ipc_client::IpcClient;
use anyhow::{ensure, Context, Result};
use clap::{Args as ClapArgs, Subcommand};
use rexeterm_cli_host::{
    read_binding, spawn_detached, write_binding, Registry, ServiceRecord, BINDING_FILE,
};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

#[derive(Subcommand, Debug)]
pub enum ServiceCommand {
    /// Start a background daemon for this state directory and bind the current directory.
    Create(DaemonArgs),
    /// List the services registered for this OS boot, including stopped ones.
    List,
    /// Start the selected service with its existing UUID64 and state directory.
    Start,
    /// Stop the selected service and its live connections.
    Stop,
    /// Show the current directory, binding, registry, and live executor without starting a daemon.
    Info,
    /// Bind the current directory to a service ID from `service list`.
    Bind {
        /// Service UUID64 reported by `service create` or `service list`.
        uuid: String,
    },
    /// Internal detached daemon entrypoint; the service ID is passed explicitly.
    #[command(hide = true)]
    Runasdaemon(DaemonArgs),
}
#[derive(Clone, ClapArgs, Debug)]
pub struct DaemonArgs {
    /// State directory for this host (default: .rexeterm-config in the current directory).
    #[arg(long, value_name = "DIR")]
    pub config_dir: Option<PathBuf>,
    /// Enable the built-in `local` executor, if compiled in.
    #[arg(long)]
    pub local: bool,
}

pub fn state_directory(cwd: &Path) -> PathBuf {
    cwd.join(rexeterm_appcore::config::DIRECTORY)
}

pub fn client(record: &ServiceRecord) -> Result<IpcClient> {
    let target = record
        .target
        .clone()
        .context("selected service is stopped; run service start")?;
    let token_file = record
        .token_file
        .as_ref()
        .context("missing host IPC token")?;
    let token = std::fs::read_to_string(token_file)
        .context("cannot read host IPC token; check service info and service start")?;
    IpcClient::new(target, token.trim().to_owned(), &record.id)
}

/// Reuse a verified host, or restart a reserved service before a business call.
pub async fn connect(registry: &Registry, id: &str) -> Result<IpcClient> {
    client(&start(registry, registry.resolve(id)?).await?)
}

async fn running(record: &ServiceRecord) -> bool {
    match client(record) {
        Ok(client) => client.verify_service().await.is_ok(),
        Err(_) => false,
    }
}

async fn start(registry: &Registry, record: ServiceRecord) -> Result<ServiceRecord> {
    if running(&record).await {
        return Ok(record);
    }
    crate::directory::check_state(&record.config_dir)?;
    // The child inherits the actual cwd and receives the ID, so concurrent starts
    // never hand off identity through a shared file.
    let binary = std::env::current_exe()?;
    let log_path = record.config_dir.join("cli-host.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    rexeterm_cli_host::private_file(&log_path)?;
    let mut command = Command::new(binary);
    command
        .arg("service")
        .arg("runasdaemon")
        .arg("--registry-dir")
        .arg(registry.base_directory())
        .arg("--service-id")
        .arg(&record.id)
        .env_remove("REXETERM_CWD");
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
                // Another CLI may have won the state directory lock.
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
        anyhow::bail!("CLI host did not become ready; see {}", log_path.display());
    }
    .await;
    if ready.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    ready
}

pub async fn execute(
    registry: &Registry,
    command: ServiceCommand,
    explicit: Option<&str>,
    cwd: &Path,
) -> Result<Value> {
    match command {
        ServiceCommand::Create(options) => {
            ensure!(
                explicit.is_none(),
                "service create selects its own ID; omit --service-id"
            );
            let config = options.config_dir.unwrap_or_else(|| state_directory(cwd));
            let reserved = registry.reserve(&config, options.local)?;
            let record = start(registry, reserved).await?;
            write_binding(cwd, &record.id)?;
            Ok(
                json!({"serviceId":record.id,"running":true,"pid":record.pid,"target":record.target,"stateDir":record.config_dir,"bindingFile":cwd.join(BINDING_FILE)}),
            )
        }
        ServiceCommand::List => {
            let records = registry.records()?;
            let rows = futures_util::future::join_all(records.into_iter().map(|record| async move {
                let live = running(&record).await;
                json!({"serviceId":record.id,"running":live,"pid":record.pid.filter(|_| live),"target":record.target.filter(|_| live),"stateDir":record.config_dir,"local":record.local})
            })).await;
            Ok(json!({"registryDir":registry.base_directory(),"services":rows}))
        }
        ServiceCommand::Bind { uuid } => {
            ensure!(
                explicit.is_none(),
                "service bind takes a positional ID; omit --service-id"
            );
            let id = rexeterm_cli_host::normalize_id(&uuid)?;
            let record = registry.resolve(&id)?;
            // Stopped hosts are still valid bindings until the next OS boot.
            write_binding(cwd, &id)?;
            Ok(
                json!({"serviceId":id,"bound":true,"running":running(&record).await,"bindingFile":cwd.join(BINDING_FILE)}),
            )
        }
        ServiceCommand::Info | ServiceCommand::Runasdaemon(_) => {
            unreachable!("handled by CLI dispatch")
        }
        ServiceCommand::Start => {
            let record = start(registry, registry.resolve(&required_id(cwd, explicit)?)?).await?;
            Ok(
                json!({"serviceId":record.id,"running":true,"pid":record.pid,"target":record.target}),
            )
        }
        ServiceCommand::Stop => {
            let id = required_id(cwd, explicit)?;
            let record = registry.resolve(&id)?;
            if !running(&record).await {
                return Ok(json!({"serviceId":id,"running":false}));
            }
            client(&record)?.call("shutdown", json!({})).await?;
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

pub struct Selection {
    pub id: String,
    pub source: &'static str,
    pub binding_file: Option<PathBuf>,
}

pub fn select(cwd: &Path, explicit: Option<&str>) -> Result<Option<Selection>> {
    if let Some(explicit) = explicit {
        return Ok(Some(Selection {
            id: rexeterm_cli_host::normalize_id(explicit)?,
            source: "explicit",
            binding_file: None,
        }));
    }
    Ok(read_binding(cwd)?.map(|(id, path)| Selection {
        id,
        source: if path.ends_with(BINDING_FILE) {
            "cwd"
        } else {
            "legacy-cwd"
        },
        binding_file: Some(path),
    }))
}

pub fn required_id(cwd: &Path, explicit: Option<&str>) -> Result<String> {
    select(cwd, explicit)?.map(|selection| selection.id).with_context(|| {
        format!("no CLI host binding at {}; run service create, service bind <UUID64>, or pass --service-id", cwd.join(BINDING_FILE).display())
    })
}

/// Inspect selection and, if reachable, return its authenticated IPC client.
pub async fn inspect(
    registry: &Registry,
    cwd: &Path,
    explicit: Option<&str>,
) -> Result<(Value, Option<IpcClient>)> {
    let selected = select(cwd, explicit)?;
    let mut info = json!({
        "cwd": cwd,
        "registryDir": registry.base_directory(),
        "bootRegistryDir": registry.directory(),
        "bindingSource": selected.as_ref().map_or("none", |s| s.source),
        "serviceId": selected.as_ref().map(|s| &s.id),
        "bound": selected.as_ref().is_some_and(|s| s.binding_file.is_some()),
        "running": false,
        "bindingFile": selected.as_ref().map_or_else(|| Some(cwd.join(BINDING_FILE)), |s| s.binding_file.clone()),
    });
    let Some(selection) = selected else {
        return Ok((info, None));
    };
    let record = match registry.resolve(&selection.id) {
        Ok(record) => record,
        Err(error) => {
            info["serviceError"] = json!(error.to_string());
            return Ok((info, None));
        }
    };
    info["stateDir"] = json!(record.config_dir);
    info["local"] = json!(record.local);
    let client = match client(&record) {
        Ok(client) => client,
        Err(error) => {
            info["serviceError"] = json!(error.to_string());
            return Ok((info, None));
        }
    };
    match tokio::time::timeout(Duration::from_secs(1), client.call("info", json!({}))).await {
        Ok(Ok(host)) => {
            info["host"] = host;
            info["running"] = json!(true);
            info["pid"] = json!(record.pid);
            info["target"] = json!(record.target);
            Ok((info, Some(client)))
        }
        Ok(Err(error)) => {
            info["serviceError"] = json!(error.to_string());
            Ok((info, None))
        }
        Err(_) => {
            info["serviceError"] =
                json!("selected service did not answer; check service list or run service start");
            Ok((info, None))
        }
    }
}
