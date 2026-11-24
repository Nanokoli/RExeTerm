//! Machine interface bundled in the RexE Term VS Code extension.
//! The native host is shared across VS Code windows in the same profile.
use anyhow::{ensure, Context, Result};
use clap::{Parser, Subcommand};
use rexeterm_cli::{directory, executors, host, services};
use rexeterm_cli_host::{ipc, Registry};
use serde_json::{json, Value};
use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

#[derive(Parser)]
#[command(name = "rexeterm-vscode-backend", version)]
struct Args {
    #[arg(long, global = true)]
    registry_dir: Option<PathBuf>,
    /// Expose the same-user built-in executor in this VS Code profile.
    #[arg(long)]
    local: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    #[command(hide = true)]
    Service {
        #[command(subcommand)]
        command: Service,
    },
}

#[derive(Subcommand)]
enum Service {
    Runasdaemon {
        #[arg(long)]
        service_id: String,
    },
}

fn read_request() -> Result<Value> {
    let mut bytes = Vec::new();
    io::stdin()
        .take((ipc::MAX_REQUEST + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= ipc::MAX_REQUEST, "RexE request is too large");
    Ok(serde_json::from_slice(&bytes)?)
}

async fn dispatch(registry_dir: PathBuf, local: bool, request: Value) -> Result<Value> {
    let operation = request["operation"]
        .as_str()
        .context("missing RexE operation")?;
    let args = &request["args"];
    ensure!(args.is_object(), "RexE arguments must be an object");
    let required = |key: &str| {
        args[key]
            .as_str()
            .filter(|value| !value.is_empty())
            .with_context(|| format!("missing {key}"))
    };
    // Local inventory edits don't need a daemon or a network connection.
    match operation {
        "executor.add" => {
            let id = required("id")?;
            executors::validate_alias(id)?;
            let url = required("url")?;
            ensure!(
                !url.contains(['@', '?', '#']) && !url.chars().any(char::is_whitespace),
                "expected a RexE endpoint",
            );
            return executors::save(
                id,
                json!({"id":id,"name":id,"url":url,"root":required("root")?}),
            );
        }
        "credential.link" => {
            let profile = Path::new(required("profile")?);
            ensure!(profile.is_absolute(), "profile path must be absolute");
            return executors::link_credential(required("name")?, profile);
        }
        _ => {}
    }

    let (call, payload) = match operation {
        "list_executor" => ("rpc", json!({"tool":"list_executor","params":{}})),
        "executor.connect" => {
            let id = required("id")?;
            let params = executors::connection(id, args["credential"].as_str())?;
            (
                "rpc",
                json!({"executor":id,"tool":"connect_to_executor","params":params}),
            )
        }
        // Keep the bridge surface limited to operations the extension uses.
        "rpc" => {
            ensure!(
                matches!(
                    required("tool")?,
                    "stat" | "read" | "file_transfer" | "_executor_info" | "exbash"
                ),
                "unsupported RexE tool"
            );
            (operation, args.clone())
        }
        "hosts.disconnect" | "tasks.list" | "tasks.read" | "tasks.stop" | "transfers.local"
        | "transfers.list" | "transfers.show" | "transfers.cancel" | "forwards.list"
        | "forwards.start" | "forwards.stop" => (operation, args.clone()),
        _ => anyhow::bail!("unsupported RexE operation"),
    };
    let registry = Registry::open(Some(registry_dir))?;
    let state = registry
        .base_directory()
        .join(if local { "vscode-local" } else { "vscode" });
    let record = registry.reserve(&state, local)?;
    let client = services::connect(&registry, &record.id).await?;
    client.call(call, payload).await
}

async fn run(args: Args) -> Result<()> {
    let registry_dir = args.registry_dir.context("missing --registry-dir")?;
    ensure!(registry_dir.is_absolute(), "registry path must be absolute");
    directory::check(&std::env::current_dir()?)?;
    if let Some(Command::Service {
        command: Service::Runasdaemon { service_id },
    }) = args.command
    {
        host::run((
            services::DaemonArgs {
                config_dir: None,
                local: false,
            },
            Some(service_id),
            Some(registry_dir),
        ))
        .await?;
    } else {
        let request = read_request()?;
        let result = dispatch(registry_dir, args.local, request).await?;
        let bytes = serde_json::to_vec(&result)?;
        ensure!(
            bytes.len() <= ipc::MAX_RESPONSE,
            "RexE response is too large"
        );
        io::stdout().lock().write_all(&bytes)?;
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run(Args::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
