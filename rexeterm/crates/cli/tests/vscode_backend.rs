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

fn request(
    cwd: &Path,
    registry: &Path,
    operation: &str,
    args: Value,
    local: bool,
) -> Result<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rexeterm-vscode-backend"))
        .arg("--registry-dir")
        .arg(registry)
        .args(local.then_some("--local"))
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
        .write_all(&serde_json::to_vec(
            &json!({"operation":operation,"args":args}),
        )?)?;
    let output = child.wait_with_output()?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn list(cwd: &Path, registry: &Path) -> Result<Value> {
    request(cwd, registry, "list_executor", json!({}), true)
}

#[test]
fn configs_keep_public_hosts_credentials_and_jump_routes_separate() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let cwd = temp.path();
    let registry = cwd.join("runtime");
    let run = |operation, args| request(cwd, &registry, operation, args, false);
    let base = cwd.join(".rexeterm/config-dir");
    let source = cwd.join("source.json");
    std::fs::write(
        &source,
        serde_json::to_vec(&json!({
            "id":"wrong", "url":"tls://not-the-host:9999", "root":"/not-the-start-path",
            "username":"tester", "authentication":{"method":"password","password":"fixture-only-password"},
            "tls":{"caFile":"cert.pem"}
        }))?,
    )?;
    run(
        "credential.import",
        json!({"name":"test-key","profile":source}),
    )?;
    let saved: Value =
        serde_json::from_slice(&std::fs::read(base.join("credentials/test-key.json"))?)?;
    assert_eq!(saved["username"], "tester");
    assert_eq!(
        saved["tls"]["caFile"],
        json!(std::fs::canonicalize(cwd)?.join("cert.pem"))
    );
    for key in ["id", "url", "root"] {
        assert!(saved.get(key).is_none());
    }
    for id in ["entry", "bridge", "container"] {
        run(
            "executor.add",
            json!({"id":id,"url":format!("tls://{id}:45957")}),
        )?;
    }
    let legacy = base.join("executors/old.json");
    std::fs::write(
        legacy,
        br#"{"id":"old","url":"tls://old:45957","root":"/existing"}"#,
    )?;
    for (id, host, start, via) in [
        ("first", "entry", "/", None),
        ("p90", "bridge", "/home", Some("first")),
        ("build", "container", "/workspace", Some("p90")),
        ("logs", "container", "C:/Logs", Some("p90")),
    ] {
        run(
            "config.add",
            json!({"id":id,"executor":host,"credential":"test-key","startPath":start,"via":via}),
        )?;
    }
    let anonymous = run(
        "config.add",
        json!({"id":"anonymous","executor":"entry","startPath":"/"}),
    )?;
    assert!(anonymous.get("credential").is_none());
    assert!(run("config.add", json!({
        "id":"build","executor":"container","credential":"test-key","startPath":"/changed","via":"first"
    })).is_err(), "a duplicate config silently replaced an existing one");
    assert!(run("config.add", json!({
        "id":"bad","executor":"container","credential":"missing","startPath":"/workspace","via":"p90"
    })).is_err());

    let external = base.join("configs/cycle.json");
    std::fs::write(external, br#"{"id":"cycle","executor":"entry","credential":"test-key","startPath":"/","via":"cycle"}"#)?;
    let error = run("config.connect", json!({"id":"cycle"})).unwrap_err();
    assert!(error.to_string().contains("cycle"));
    assert!(
        !registry.exists(),
        "invalid configs or inventory should not start a backend host"
    );
    let inventory = run("inventory", json!({}))?;
    assert!(!inventory.to_string().contains("fixture-only-password"));
    let hosts = inventory["executors"].as_array().context("executors")?;
    assert!(hosts
        .iter()
        .all(|host| host.get("root").is_none() && host.get("tls").is_none()));
    let rows = inventory["configs"].as_array().context("configs")?;
    let get = |id| rows.iter().find(|row| row["id"] == id).unwrap();
    assert_eq!(get("build")["route"], json!(["first", "p90", "build"]));
    assert_eq!(get("build")["startPath"], "/workspace");
    assert_eq!(get("build")["executor"], get("logs")["executor"]);
    assert_eq!(get("logs")["startPath"], "C:/Logs");
    assert_eq!(get("old")["legacy"], true);
    assert_eq!(get("old")["startPath"], "/existing");
    assert!(get("cycle")["error"].as_str().unwrap().contains("cycle"));
    Ok(())
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
