use remote_executor::{client::transport::Frame, protocol::terminal::ServerText};
use remote_executor::{
    client::{
        management::ManagementClient, Client, ConnectExecutorOptions, FileTransferMode,
        FileTransferOptions,
    },
    protocol::{management::*, ExecutorRequest},
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, time::Duration};
use tokio::time::{sleep, timeout};
fn options(url: String, username: &str, client: &str) -> ConnectExecutorOptions {
    ConnectExecutorOptions {
        authentication: None,
        tls: None,
        username: Some(username.into()),
        client_name: Some(client.into()),
        id: "node".into(),
        url,
        links: vec![],
        channels: Default::default(),
        system: None,
        device: None,
        labels: BTreeMap::new(),
    }
}
async fn call(client: &Client, tool: &str, params: Value) -> Value {
    let r = client
        .handle(ExecutorRequest {
            id: json!(1),
            method: tool.into(),
            params,
            executor: None,
            directory: None,
            tool_timeout_ms: None,
        })
        .await;
    assert!(r.ok, "{:?}", r.error);
    r.result.unwrap()
}
async fn until(
    client: &mut ManagementClient,
    condition: impl Fn(&ManagementSnapshot) -> bool,
) -> ManagementSnapshot {
    timeout(Duration::from_secs(5), async {
        loop {
            let s = client.snapshot().await.unwrap();
            if condition(&s) {
                return s;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn server_binary_is_named_rexed() {
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_rexed"))
        .arg("--help")
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("Usage: rexed"));
}
#[cfg(unix)]
#[tokio::test]
async fn standalone_rexed_defaults_to_a_unix_operations_socket() {
    let root = tempfile::tempdir().unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_rexed"))
        .args(["--id", "test-default", "--listen", "tcp://127.0.0.1:0"])
        .arg("--insecure-no-auth")
        .arg("--config-dir")
        .arg(root.path().join("config"))
        .env("XDG_RUNTIME_DIR", root.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let path = root.path().join("rexed/test-default.admin.sock");
    timeout(Duration::from_secs(5), async {
        while !path.exists() {
            assert!(child.try_wait().unwrap().is_none(), "rexed exited early");
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut client = ManagementClient::connect(&format!("unix://{}", path.display()))
        .await
        .unwrap();
    let s = client.snapshot().await.unwrap();
    assert_eq!(s.executor_id, "test-default");
    assert!(s.programs.is_empty());
    let pid = child.id().unwrap();
    assert!(std::process::Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .status()
        .unwrap()
        .success());
    assert!(timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
    timeout(Duration::from_secs(3), async {
        while path.exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn standalone_stdout_is_connections_and_file_audit_covers_operations_and_results() {
    use remote_executor::server::auth::ConfigDirectory;
    use tokio::io::{AsyncBufReadExt, BufReader};
    let root = tempfile::tempdir().unwrap();
    let directory = ConfigDirectory::open(Some(root.path().join("config"))).unwrap();
    directory.initialize(Vec::new()).unwrap();
    let config_path = directory.path().join("server.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    let logs = root.path().join("logs");
    config["logging"]["directory"] = json!(logs);
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_rexed"))
        .args([
            "--id",
            "event-test",
            "--listen",
            "tcp://127.0.0.1:0",
            "--insecure-no-auth",
        ])
        .arg("--config-dir")
        .arg(directory.path())
        .env("XDG_RUNTIME_DIR", root.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
    let reader = tokio::spawn(async move {
        let mut rows = Vec::<Value>::new();
        while let Some(line) = stdout.next_line().await.unwrap() {
            rows.push(serde_json::from_str(&line).unwrap());
        }
        rows
    });
    let mut stderr = BufReader::new(child.stderr.take().unwrap()).lines();
    let url = timeout(Duration::from_secs(5), async {
        loop {
            let line = stderr.next_line().await.unwrap().expect("rexed exited");
            if let Some((url, _)) = line.split_once(" pty=") {
                break url.to_owned();
            }
        }
    })
    .await
    .unwrap();
    let mut admin = ManagementClient::connect(&format!(
        "unix://{}/rexed/event-test.admin.sock",
        root.path().display()
    ))
    .await
    .unwrap();
    let mut clients = Vec::new();
    for (username, name) in [
        ("alice", "editor-a"),
        ("alice", "editor-b"),
        ("bob", "agent"),
    ] {
        let client = Client::default();
        client
            .connect_to_executor(options(url.clone(), username, name))
            .await
            .unwrap();
        clients.push(client);
    }
    let client = &clients[0];
    let owner = admin
        .snapshot()
        .await
        .unwrap()
        .clients
        .iter()
        .find(|c| c.owner.client_name == "editor-a")
        .unwrap()
        .owner
        .clone();
    let file = root.path().join("example.txt");
    call(client,"FileAction",json!({"mode":"create","filePath":file,"content":"needle-special file-body-must-not-be-logged"})).await;
    call(client, "read", json!({"filePath":file})).await;
    call(
        client,
        "rg",
        json!({"pattern":"needle-special","path":file}),
    )
    .await;
    let missing = client
        .handle(ExecutorRequest {
            id: json!("missing"),
            method: "read".into(),
            params: json!({"filePath":root.path().join("missing")}),
            executor: None,
            directory: None,
            tool_timeout_ms: None,
        })
        .await;
    assert!(!missing.ok);
    let result=call(client,"exbash",json!({"mode":"shell","command":"sleep 30 # audit-command-marker","read_timeout":1,"timeout":60000})).await;
    let id = result["metadata"]["asyncID"].as_str().unwrap();
    let mut terminal = client
        .open_terminal(None, id, "controller", 80, 24)
        .await
        .unwrap();
    receive_meta(&mut terminal, "Controller").await;
    let source = root.path().join("source");
    std::fs::write(&source, b"file-body-must-not-be-logged").unwrap();
    client
        .transfer_file(
            None,
            FileTransferOptions {
                metadata: Default::default(),
                mode: FileTransferMode::Upload,
                local_path: source,
                target_path: root.path().join("destination"),
                overwrite: Some(false),
                offset: 0,
                sha256: None,
            },
            None,
            None,
        )
        .await
        .unwrap();
    drop(terminal);
    until(&mut admin, |s| s.channels.is_empty()).await;
    call(client, "exbash", json!({"mode":"remove","asyncID":id})).await;
    for client in &clients {
        client.shutdown().await;
    }
    until(&mut admin, |s| s.clients.is_empty()).await;
    let pid = child.id().unwrap();
    assert!(std::process::Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .status()
        .unwrap()
        .success());
    assert!(timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
    let console = reader.await.unwrap();
    assert_eq!(console.len(), 6);
    assert!(console.iter().all(|r| matches!(
        r["event"].as_str(),
        Some("client.connected" | "client.disconnected")
    ) && r["ip"] == "127.0.0.1"));
    assert_eq!(
        console
            .iter()
            .filter(|r| r["event"] == "client.connected" && r["username"] == "alice")
            .count(),
        2
    );
    let log_path = std::fs::read_dir(logs)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("audit.jsonl");
    let text = std::fs::read_to_string(&log_path).unwrap();
    assert!(text.contains("audit-command-marker") && text.contains("needle-special"));
    assert!(!text.contains("file-body-must-not-be-logged") && !text.contains("\"secret\""));
    let records: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    for event in [
        "client.connected",
        "client.disconnected",
        "request.started",
        "request.finished",
        "program.created",
        "channel.opened",
        "channel.closed",
        "file.started",
        "file.finished",
        "pty.control_changed",
    ] {
        assert!(
            records
                .iter()
                .any(|r| r["event"] == event && r["owner"]["sessionId"] == owner.session_id),
            "missing {event}"
        );
    }
    let final_record = records
        .iter()
        .find(|r| {
            r["event"] == "client.disconnected" && r["owner"]["sessionId"] == owner.session_id
        })
        .unwrap();
    assert_eq!(final_record["counts"]["fileReadRequests"], 2);
    assert_eq!(final_record["counts"]["fileWriteRequests"], 1);
    assert_eq!(final_record["counts"]["searches"], 1);
    assert_eq!(final_record["counts"]["commands"], 1);
    assert_eq!(final_record["counts"]["failedRequests"], 1);
    assert_eq!(final_record["counts"]["uploads"], 1);
    assert!(records.iter().any(|r| r["event"] == "file.finished"
        && r["details"]["outcome"] == "ok"
        && r["details"]["bytes"] == 28));
}

async fn receive_meta(
    connection: &mut remote_executor::client::terminal::TerminalConnection,
    role: &str,
) {
    timeout(Duration::from_secs(5),async {loop {
        if let Some(Frame::Text(text))=connection.link().recv().await.unwrap() {
            if matches!(serde_json::from_str::<ServerText>(&text),Ok(ServerText::Meta {role:r,..}) if r == role) {break}
        }
    }}).await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn standalone_operations_listener_uses_config_with_cli_precedence() {
    use remote_executor::server::auth::ConfigDirectory;
    let root = tempfile::tempdir().unwrap();
    let directory = ConfigDirectory::open(Some(root.path().join("config"))).unwrap();
    directory.initialize(Vec::new()).unwrap();
    let path = directory.path().join("server.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let configured = root.path().join("configured.sock");
    config["adminListen"] = json!(format!("unix://{}", configured.display()));
    let bytes = serde_json::to_vec_pretty(&config).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    for override_cli in [false, true] {
        let overridden = root.path().join("overridden.sock");
        let target = if override_cli {
            &overridden
        } else {
            &configured
        };
        let unused = if override_cli {
            &configured
        } else {
            &overridden
        };
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_rexed"));
        command
            .args(["--id", "config-test", "--listen", "tcp://127.0.0.1:0"])
            .arg("--insecure-no-auth")
            .arg("--config-dir")
            .arg(directory.path())
            .env("XDG_RUNTIME_DIR", root.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if override_cli {
            command
                .arg("--admin-listen")
                .arg(format!("unix://{}", target.display()));
        }
        let mut child = command.spawn().unwrap();
        timeout(Duration::from_secs(5), async {
            while !target.exists() {
                assert!(child.try_wait().unwrap().is_none(), "rexed exited early");
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!unused.exists());
        assert!(!root.path().join("rexed/config-test.admin.sock").exists());
        let mut admin = ManagementClient::connect(&format!("unix://{}", target.display()))
            .await
            .unwrap();
        assert_eq!(admin.snapshot().await.unwrap().executor_id, "config-test");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bytes,
            "CLI must not rewrite config"
        );
        let pid = child.id().unwrap();
        assert!(std::process::Command::new("kill")
            .args(["-INT", &pid.to_string()])
            .status()
            .unwrap()
            .success());
        assert!(timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success());
        assert!(
            !target.exists(),
            "graceful shutdown must clean up its socket"
        );
    }
}

#[tokio::test]
async fn standalone_operations_config_rejects_invalid_or_insecure_listeners() {
    use remote_executor::server::auth::ConfigDirectory;
    let root = tempfile::tempdir().unwrap();
    let directory = ConfigDirectory::open(Some(root.path().join("config"))).unwrap();
    directory.initialize(Vec::new()).unwrap();
    let path = directory.path().join("server.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for (value, expected) in [
        (json!(42), "invalid type"),
        (json!(""), "management target needs"),
        (
            json!("kcp://127.0.0.1:0"),
            "unsupported management transport",
        ),
        (
            json!("tcp://127.0.0.1:0"),
            "network management requires tls:// or wss://",
        ),
    ] {
        config["adminListen"] = value;
        std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        let output = timeout(
            Duration::from_secs(10),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_rexed"))
                .args(["--id", "invalid-config", "--listen", "tls://127.0.0.1:0"])
                .arg("--config-dir")
                .arg(directory.path())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(expected), "expected {expected:?}: {error}");
    }
}

#[tokio::test]
async fn standalone_operations_listener_accepts_authenticated_tls_and_wss_from_config() {
    use remote_executor::client::{
        auth::{AuthenticatedConnector, AuthenticationOptions},
        transport::{TlsClientOptions, TlsConnector},
    };
    use remote_executor::server::auth::{hash_password, ConfigDirectory, Permissions, UserConfig};
    use std::sync::Arc;
    use tokio::io::{AsyncBufReadExt, BufReader};
    let root = tempfile::tempdir().unwrap();
    let directory = ConfigDirectory::open(Some(root.path().join("config"))).unwrap();
    directory.initialize(Vec::new()).unwrap();
    directory
        .save_user(
            "admin",
            &UserConfig {
                password_hash: Some(hash_password("test-password").unwrap()),
                permissions: Permissions {
                    management: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
    let path = directory.path().join("server.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let logs = root.path().join("tls-logs");
    config["logging"]["directory"] = json!(logs);
    for listener in ["tls://127.0.0.1:0", "wss://127.0.0.1:0/ops"] {
        config["adminListen"] = json!(listener);
        std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_rexed"))
            .args(["--id", "secure-config", "--listen", "tls://127.0.0.1:0"])
            .arg("--config-dir")
            .arg(directory.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
        let target = timeout(Duration::from_secs(5), async {
            loop {
                let line = lines
                    .next_line()
                    .await
                    .unwrap()
                    .expect("rexed exited before listening");
                if let Some(target) = line.strip_prefix("management=") {
                    break target.to_owned();
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(target.split("://").next(), listener.split("://").next());
        assert_eq!(target.ends_with("/ops"), listener.ends_with("/ops"));
        let tls = TlsConnector::new(TlsClientOptions {
            ca_file: Some(directory.path().join("server-cert.pem")),
            ..Default::default()
        })
        .unwrap();
        let denied_tls = TlsConnector::new(TlsClientOptions {
            ca_file: Some(directory.path().join("server-cert.pem")),
            ..Default::default()
        })
        .unwrap();
        let denied = AuthenticatedConnector::new(
            Arc::new(denied_tls),
            "admin".into(),
            AuthenticationOptions::Password {
                password: "wrong-password-must-not-be-logged".into(),
            },
        );
        assert!(ManagementClient::with_connector(Arc::new(denied), &target)
            .await
            .is_err());
        let connector = AuthenticatedConnector::new(
            Arc::new(tls),
            "admin".into(),
            AuthenticationOptions::Password {
                password: "test-password".into(),
            },
        );
        let mut admin = ManagementClient::with_connector(Arc::new(connector), &target)
            .await
            .unwrap();
        assert_eq!(admin.snapshot().await.unwrap().executor_id, "secure-config");
        let log_path = std::fs::read_dir(&logs)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("audit.jsonl");
        timeout(Duration::from_secs(5), async {
            loop {
                let text = std::fs::read_to_string(&log_path).unwrap();
                if text.contains("authentication.succeeded")
                    && text.contains("authentication.failed")
                {
                    assert!(
                        !text.contains("test-password")
                            && !text.contains("wrong-password-must-not-be-logged")
                    );
                    break;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        child.kill().await.unwrap();
    }
}
