use rexeterm::{
    client::ConnectExecutorOptions,
    daemon::{Daemon, DaemonOptions},
    protocol::{ExecutorInfo, ExecutorRequest},
    server::{start_shared_executor_ws, Executor, SettingsStore, ShellManager},
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, path::Path, process::Stdio, time::Duration};
use tempfile::tempdir;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    time::timeout,
};

fn request(tool: &str, directory: &Path, params: Value) -> ExecutorRequest {
    ExecutorRequest {
        id: json!(tool),
        method: tool.to_string(),
        params,
        directory: Some(directory.to_path_buf()),
        executor: None,
        tool_timeout_ms: None,
    }
}

fn connection(id: &str, url: &str) -> ConnectExecutorOptions {
    ConnectExecutorOptions {
        authentication: None,
        tls: None,
        username: None,
        client_name: None,
        links: Vec::new(),
        channels: Default::default(),
        id: id.to_string(),
        url: url.to_string(),
        system: None,
        device: None,
        labels: BTreeMap::new(),
    }
}

fn remote(id: &str, settings_path: &Path) -> String {
    let executor = Executor::new(ExecutorInfo {
        id: id.to_string(),
        system: None,
        device: None,
        labels: BTreeMap::new(),
    })
    .with_settings_store(SettingsStore::load(Some(settings_path.to_path_buf())).unwrap());
    let address =
        start_shared_executor_ws("127.0.0.1:0", executor, ShellManager::default_shell(80, 24))
            .unwrap();
    format!("ws://{address}")
}

#[tokio::test]
async fn disabled_local_skips_settings_and_reports_missing_local_capabilities() {
    let directory = tempdir().unwrap();
    let invalid_settings = directory.path().join("invalid.json");
    fs::write(&invalid_settings, "this is not JSON").unwrap();
    let runtime = Daemon::new_with_options(DaemonOptions {
        identity: Default::default(),
        settings_path: Some(invalid_settings.clone()),
        client_settings_path: None,
        local_executor: false,
    })
    .await
    .unwrap();
    let client = runtime.core().clone();
    assert!(Daemon::new_with_settings(Some(invalid_settings))
        .await
        .is_err());

    let listed = client
        .handle(request("list_executor", directory.path(), json!({})))
        .await;
    assert!(listed.ok);
    let metadata = listed.result.unwrap()["metadata"].clone();
    assert_eq!(metadata["executors"], json!([]));
    assert_eq!(metadata["default"], Value::Null);
    assert_eq!(client.default_executor().await, "");
    assert!(runtime.local_executor().is_none());
    let response = client
        .handle(request(
            "read",
            directory.path(),
            json!({"filePath": "file.txt"}),
        ))
        .await;
    assert!(!response.ok);
    assert!(response.error.unwrap().contains("no executor selected"));
    let mut explicit_local = request("read", directory.path(), json!({"filePath": "file.txt"}));
    explicit_local.executor = Some("local".to_string());
    let response = client.handle(explicit_local).await;
    assert!(!response.ok);
    assert_eq!(response.error.as_deref(), Some("executor not found: local"));
}

#[tokio::test]
async fn first_successful_remote_becomes_default_and_routing_remains_selectable() {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("remote.txt"), "remote bytes").unwrap();
    let runtime = Daemon::new_with_options(DaemonOptions {
        identity: Default::default(),
        local_executor: false,
        ..DaemonOptions::default()
    })
    .await
    .unwrap();
    let client = runtime.core().clone();
    assert!(client
        .connect_to_executor(connection("broken", "ws://["))
        .await
        .is_err());
    assert!(client.default_executor().await.is_empty());
    let first_url = remote("first", &directory.path().join("first.json"));
    let second_url = remote("second", &directory.path().join("second.json"));
    let clone = client.clone();
    clone
        .connect_to_executor(connection("first", &first_url))
        .await
        .unwrap();
    assert_eq!(client.default_executor().await, "first");
    client
        .connect_to_executor(connection("second", &second_url))
        .await
        .unwrap();
    assert_eq!(client.default_executor().await, "first");
    let read = || request("read", directory.path(), json!({"filePath": "remote.txt"}));
    let response = client.handle(read()).await;
    assert!(response.ok, "{:?}", response.error);
    assert_eq!(response.executor.as_deref(), Some("first"));
    client.set_default_executor("second").await.unwrap();
    let response = clone.handle(read()).await;
    assert!(response.ok, "{:?}", response.error);
    assert_eq!(response.executor.as_deref(), Some("second"));
    let mut explicit = read();
    explicit.executor = Some("first".to_string());
    let response = client.handle(explicit).await;
    assert!(response.ok, "{:?}", response.error);
    assert_eq!(response.executor.as_deref(), Some("first"));
}

fn cli_request(mcp: bool, id: u64, tool: &str, params: Value) -> Value {
    if mcp {
        json!({"jsonrpc":"2.0", "id":id, "method":"tools/call", "params":{"name":tool, "arguments":params}})
    } else {
        json!({"id":id, "tool":tool, "params":params})
    }
}

fn cli_result(mcp: bool, response: &Value) -> &Value {
    if mcp {
        assert_eq!(response["result"]["isError"], false, "{response}");
        &response["result"]["structuredContent"]
    } else {
        assert_eq!(response["ok"], true, "{response}");
        &response["result"]
    }
}

#[cfg(target_os = "linux")]
fn assert_no_local_resources(pid: u32) {
    assert!(
        fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
            .unwrap()
            .trim()
            .is_empty()
    );
    let mut listeners = std::collections::HashSet::new();
    for table in ["tcp", "tcp6"] {
        let entries = fs::read_to_string(format!("/proc/{pid}/net/{table}")).unwrap();
        for row in entries.lines().skip(1) {
            let fields: Vec<_> = row.split_whitespace().collect();
            if fields[3] == "0A" {
                listeners.insert(format!("socket:[{}]", fields[9]));
            }
        }
    }
    for entry in fs::read_dir(format!("/proc/{pid}/fd")).unwrap() {
        let Ok(target) = fs::read_link(entry.unwrap().path()) else {
            continue;
        };
        let target = target.to_string_lossy();
        assert!(
            !listeners.contains(target.as_ref()),
            "unexpected TCP listener: {target}"
        );
        assert!(
            !target.starts_with("/dev/pts/") && target != "/dev/ptmx",
            "unexpected PTY: {target}"
        );
    }
}

#[tokio::test]
async fn both_cli_adapters_can_run_without_shells_or_listeners_and_call_a_remote() {
    let directory = tempdir().unwrap();
    let invalid_settings = directory.path().join("invalid.json");
    fs::write(&invalid_settings, "invalid settings").unwrap();
    fs::write(directory.path().join("remote.txt"), "remote bytes").unwrap();
    let url = remote("remote", &directory.path().join("server.json"));
    for (mcp, binary) in [(true, env!("CARGO_BIN_EXE_rexeterm-mcp"))] {
        let mut child = Command::new(binary)
            .arg("--no-local")
            .arg("--settings")
            .arg(&invalid_settings)
            .env("PATH", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let requests = [
            cli_request(mcp, 1, "list_executor", json!({})),
            cli_request(
                mcp,
                2,
                "connect_to_executor",
                json!({"id":"remote", "url":url}),
            ),
            cli_request(
                mcp,
                3,
                "read",
                json!({"filePath": directory.path().join("remote.txt")}),
            ),
        ];
        for (index, request) in requests.into_iter().enumerate() {
            input
                .write_all(format!("{request}\n").as_bytes())
                .await
                .unwrap();
            input.flush().await.unwrap();
            let line = timeout(Duration::from_secs(5), lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .expect("client exited without a response");
            let response: Value = serde_json::from_str(&line).unwrap();
            let result = cli_result(mcp, &response);
            match index {
                0 => {
                    assert_eq!(result["metadata"]["executors"], json!([]));
                    assert_eq!(result["metadata"]["default"], Value::Null);
                    #[cfg(target_os = "linux")]
                    assert_no_local_resources(child.id().unwrap());
                }
                1 => assert_eq!(result["metadata"]["default"], "remote"),
                _ => assert!(result["output"]["text"]
                    .as_str()
                    .unwrap()
                    .contains("remote bytes")),
            }
        }
        drop(input);
        assert!(timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success());
    }
}

#[tokio::test]
async fn invalid_identity_is_rejected_before_loading_local_executor_settings() {
    let options = rexeterm::daemon::DaemonOptions {
        identity: rexeterm::protocol::management::ClientHello {
            username: "".into(),
            ..Default::default()
        },
        settings_path: Some("/does-not-exist/executor-settings.json".into()),
        local_executor: true,
        ..Default::default()
    };
    let error = rexeterm::daemon::Daemon::new_with_options(options)
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("client identity"), "{error}");
}
