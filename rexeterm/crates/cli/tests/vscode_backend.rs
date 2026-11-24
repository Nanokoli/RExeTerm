#![cfg(all(feature = "daemon", feature = "local"))]

use anyhow::{ensure, Context, Result};
use rexeterm_cli::services;
use rexeterm_cli_host::Registry;
use serde_json::{json, Value};
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

fn list(cwd: &Path, registry: &Path) -> Result<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rexeterm-vscode-backend"))
        .arg("--registry-dir")
        .arg(registry)
        .arg("--local")
        .current_dir(cwd)
        .env_remove("PATH")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .context("missing backend stdin")?
        .write_all(br#"{"operation":"list_executor","args":{}}"#)?;
    let output = child.wait_with_output()?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

#[tokio::test]
async fn vscode_backend_starts_without_cli_and_survives_frontend_exit() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let cwd = temp.path().join("client");
    #[cfg(target_os = "linux")]
    let runtime = temp
        .path()
        .join("vscode-profile-with-a-long-user-data-directory")
        .join("User/globalStorage/rexeterm.rexeterm-vscode-ext/runtime");
    #[cfg(not(target_os = "linux"))]
    let runtime = temp.path().join("runtime");
    std::fs::create_dir_all(&cwd)?;
    let result = (|| -> Result<()> {
        let first = list(&cwd, &runtime)?;
        ensure!(first["metadata"]["executors"][0]["id"] == "local");
        let registry = Registry::inspect(Some(runtime.clone()))?;
        let first = registry.records()?;
        ensure!(first.len() == 1);
        let second = list(&cwd, &runtime)?;
        ensure!(second["metadata"]["executors"][0]["id"] == "local");
        let second = registry.records()?;
        ensure!(second.len() == 1);
        ensure!(first[0].id == second[0].id);
        ensure!(first[0].pid.is_some() && first[0].pid == second[0].pid);
        ensure!(!cwd.join("RexETermHostUUID.txt").exists());
        Ok(())
    })();
    let registry = Registry::inspect(Some(runtime))?;
    for record in registry.records()? {
        if record.target.is_some() {
            services::client(&record)?
                .call("shutdown", json!({}))
                .await?;
            for _ in 0..100 {
                if registry.resolve(&record.id)?.target.is_none() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
    result
}
