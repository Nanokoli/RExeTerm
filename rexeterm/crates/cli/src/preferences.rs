//! Project preferences belong to the CLI: it reads and writes
//! `<cwd>/.rexeterm/config-dir/preferences.json` itself and sends the resolved
//! shell with every task, so the host never needs to know this file exists.
use crate::{executors::Inventory, ipc_client::IpcClient};
use anyhow::{Context, Result};
use rexeterm_appcore::preferences::ClientSettingsStore;
use serde_json::{json, Value};
use std::path::PathBuf;

/// Executor facts the CLI needs to resolve a shell command on its own.
#[derive(Clone, Debug, Default)]
pub struct ExecutorFacts {
    pub system: Option<String>,
    pub client_shell_command: bool,
}

pub fn directory() -> Result<PathBuf> {
    Ok(std::env::current_dir()
        .context("the CLI has no working directory")?
        .join(rexeterm_cli_host::PROJECT_DIR)
        .join("config-dir"))
}

pub fn store() -> Result<ClientSettingsStore> {
    ClientSettingsStore::load_cli(directory()?.join("preferences.json"))
}

/// Ask the bound host what the executor runs on; `auto` needs it.
pub async fn executor_facts(client: &IpcClient, host: &str) -> Result<ExecutorFacts> {
    let inventory = Inventory::live(client).await?;
    let entry = inventory.find(host).with_context(|| {
        format!("executor {host} is not connected to this service; check info and executor list")
    })?;
    Ok(ExecutorFacts {
        system: entry["system"].as_str().map(str::to_owned),
        client_shell_command: entry["clientShellCommand"].as_bool().unwrap_or(false),
    })
}

/// Stored layers plus what they resolve to: global, executor, directory.
pub fn layers(executor: &str, directory: Option<&str>) -> Result<Value> {
    let store = store()?;
    let stored = serde_json::to_value(store.settings())?;
    let layer = |name: &str, value: Option<Value>| json!({"layer": name, "shells": value.unwrap_or(Value::Null)});
    Ok(json!({
        "layers": [
            layer("global", stored.get("shells").cloned()),
            layer(
                "executor",
                stored["executors"]
                    .get(executor)
                    .map(|entry| entry["shells"].clone())
            ),
            layer(
                "directory",
                directory.and_then(|cwd| stored["executors"][executor]["directories"].get(cwd).cloned())
            ),
        ],
        "resolved": serde_json::to_value(store.effective(executor, directory))?,
    }))
}

pub fn shells(executor: &str, directory: Option<&str>) -> Result<Value> {
    Ok(serde_json::to_value(
        store()?.effective(executor, directory),
    )?)
}

pub fn set_default(executor: &str, directory: Option<&str>, name: &str) -> Result<Value> {
    let store = store()?;
    store.set_default_shell(executor, directory, name)?;
    Ok(serde_json::to_value(store.effective(executor, directory))?)
}

#[allow(clippy::too_many_arguments)]
pub fn set_profile(
    executor: &str,
    directory: Option<&str>,
    name: &str,
    program: &str,
    mut command_args: Vec<String>,
    interactive_args: Vec<String>,
    make_default: bool,
) -> Result<Value> {
    if command_args.is_empty() {
        command_args = vec!["-c".into(), "{command}".into()];
    }
    let store = store()?;
    store.set_shell_profile(
        executor,
        directory,
        name,
        program,
        command_args,
        interactive_args,
    )?;
    if make_default {
        store.set_default_shell(executor, directory, name)?;
    }
    Ok(serde_json::to_value(store.effective(executor, directory))?)
}
