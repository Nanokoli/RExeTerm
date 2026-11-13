use futures_util::{SinkExt, StreamExt};
use rexeterm::appcore::AppCore as Client;
#[cfg(not(feature = "local"))]
use rexeterm::daemon::{Daemon, DaemonOptions};
use rexeterm::{
    client::ConnectExecutorOptions,
    protocol::{ExecutorRequest, ExecutorResponse},
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, process::Stdio, time::Duration};
use tempfile::tempdir;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    process::Command,
    time::timeout,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

// The test peer speaks the wire contract without linking the server crate.
async fn test_peer() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut ws = accept_async(stream).await.unwrap();
                let Some(Ok(Message::Text(text))) = ws.next().await else {
                    return;
                };
                let request: ExecutorRequest = serde_json::from_str(&text).unwrap();
                let result = match request.method.as_str() {
                    "_executor_info" => json!({"metadata": {
                        "id":"peer", "protocol":"remote-executor", "version":"0.2.13",
                        "capabilities":{"fileTransfer":true}, "fileTransferPath":"/re-file/v1"
                    }}),
                    "read" => json!({"metadata":{}, "output":{"text":"remote payload"}}),
                    tool => panic!("unexpected tool: {tool}"),
                };
                let response = ExecutorResponse::ok(request.id, Some("peer".to_string()), result);
                ws.send(Message::Text(
                    serde_json::to_string(&response).unwrap().into(),
                ))
                .await
                .unwrap();
                let _ = ws.close(None).await;
            });
        }
    });
    (url, task)
}

#[tokio::test]
async fn pure_client_routes_using_only_the_wire_contract() {
    let instance = rexeterm::appcore::AppCore::new_with_options(Default::default())
        .await
        .unwrap();
    let client = instance.re_client();
    assert!(client.default_executor().await.is_empty());
    let (url, peer) = test_peer().await;
    client
        .connect_to_executor(ConnectExecutorOptions {
            authentication: None,
            tls: None,
            username: None,
            client_name: None,
            links: Vec::new(),
            channels: Default::default(),
            id: "peer".to_string(),
            url,
            system: None,
            device: None,
            labels: BTreeMap::new(),
        })
        .await
        .unwrap();
    let response = client
        .handle(ExecutorRequest {
            id: json!(1),
            method: "read".to_string(),
            params: json!({"filePath":"remote.txt"}),
            directory: None,
            executor: None,
            tool_timeout_ms: None,
        })
        .await;
    assert!(response.ok, "{:?}", response.error);
    assert_eq!(response.executor.as_deref(), Some("peer"));
    assert_eq!(response.result.unwrap()["output"]["text"], "remote payload");
    let clone = instance.clone();
    instance.shutdown().await;
    assert!(clone.re_client().default_executor().await.is_empty());
    peer.abort();
}

#[tokio::test]
async fn both_adapters_route_without_linking_the_execution_engine() {
    let directory = tempdir().unwrap();
    let bad_settings = directory.path().join("invalid.json");
    fs::write(&bad_settings, "invalid local settings").unwrap();
    let (url, peer) = test_peer().await;
    for (mcp, binary) in [(true, env!("CARGO_BIN_EXE_rexeterm-mcp"))] {
        let mut command = Command::new(binary);
        if cfg!(feature = "local") {
            command.arg("--no-local");
        }
        let mut child = command
            .arg("--settings")
            .arg(&bad_settings)
            .arg("--client-settings")
            .arg(
                directory
                    .path()
                    .join(if mcp { "mcp.json" } else { "stdio.json" }),
            )
            .env("PATH", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        for (id, tool, params) in [
            (1, "list_executor", json!({})),
            (2, "connect_to_executor", json!({"id":"peer", "url":url})),
            (3, "read", json!({"filePath":"remote.txt"})),
            (4, "set_default_shell", json!({"shell":"python"})),
            (5, "list_shells", json!({})),
            (6, "request_reload", json!({})),
        ] {
            let directory_key = (id >= 4).then_some("/remote/project");
            let request = if mcp {
                let mut params = params;
                if let Some(directory) = directory_key {
                    params["targetExecutor"] = json!("peer");
                    params["directory"] = json!(directory);
                }
                json!({"jsonrpc":"2.0", "id":id, "method":"tools/call", "params":{"name":tool,"arguments":params}})
            } else {
                json!({"id":id,"tool":tool,"params":params,"executor":directory_key.map(|_| "peer"),"directory":directory_key})
            };
            input
                .write_all(format!("{request}\n").as_bytes())
                .await
                .unwrap();
            input.flush().await.unwrap();
            let line = timeout(Duration::from_secs(5), lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let response: Value = serde_json::from_str(&line).unwrap();
            let result = if mcp {
                assert_eq!(response["result"]["isError"], false, "{response}");
                &response["result"]["structuredContent"]
            } else {
                assert_eq!(response["ok"], true, "{response}");
                &response["result"]
            };
            match id {
                1 => assert_eq!(result["metadata"]["executors"], json!([])),
                2 => assert_eq!(result["metadata"]["default"], "peer"),
                3 => assert_eq!(result["output"]["text"], "remote payload"),
                _ => {
                    assert_eq!(result["metadata"]["default"], "python");
                    assert_eq!(result["metadata"]["executor"], "peer");
                    assert_eq!(result["metadata"]["directory"], "/remote/project");
                    assert_eq!(result["metadata"]["scope"], "client");
                }
            }
        }
        drop(input);
        assert!(timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success());
        let scoped = rexeterm::appcore::config::executor_directory(directory.path(), "peer")
            .join("preferences.json");
        let settings: Value = serde_json::from_slice(&fs::read(scoped).unwrap()).unwrap();
        assert_eq!(
            settings["settings"]["directories"]["/remote/project"]["default"],
            "python"
        );
    }
    peer.abort();
}

#[cfg(not(feature = "local"))]
#[tokio::test]
async fn unavailable_local_support_reports_a_build_configuration_error() {
    assert!(!DaemonOptions::default().local_executor);
    let result = Daemon::new_with_options(DaemonOptions {
        identity: Default::default(),
        local_executor: true,
        ..DaemonOptions::default()
    })
    .await;
    let error = result
        .err()
        .expect("local execution must require its build feature");
    assert!(error.to_string().contains("not compiled in"));
}

#[tokio::test]
async fn older_executor_reports_shell_capability_error_before_sending_command() {
    let client = Client::default();
    let (url, peer) = test_peer().await;
    client
        .connect_to_executor(ConnectExecutorOptions {
            authentication: None,
            tls: None,
            username: None,
            client_name: None,
            links: Vec::new(),
            channels: Default::default(),
            id: "peer".into(),
            url,
            system: Some("linux".into()),
            device: None,
            labels: BTreeMap::new(),
        })
        .await
        .unwrap();
    let response = client
        .handle(ExecutorRequest {
            id: json!(1),
            method: "exbash".into(),
            params: json!({"mode":"shell","command":"echo hi"}),
            directory: None,
            executor: None,
            tool_timeout_ms: None,
        })
        .await;
    assert!(!response.ok);
    assert!(response
        .error
        .unwrap()
        .contains("does not support clientShellCommand"));
    peer.abort();
}

#[tokio::test]
async fn legacy_http_file_capability_requires_upgrade_without_a_fallback() {
    let (url, peer) = test_peer().await;
    let client = Client::default();
    client
        .connect_to_executor(ConnectExecutorOptions {
            authentication: None,
            tls: None,
            username: None,
            client_name: None,
            id: "peer".into(),
            url,
            links: vec![],
            channels: Default::default(),
            system: None,
            device: None,
            labels: BTreeMap::new(),
        })
        .await
        .unwrap();
    let root = tempdir().unwrap();
    let destination = root.path().join("copy.bin");
    let response = client
        .handle(ExecutorRequest {
            id: json!(1),
            method: "file_transfer".into(),
            params: json!({"mode":"download","localPath":destination,"targetPath":"remote.bin"}),
            directory: None,
            executor: None,
            tool_timeout_ms: None,
        })
        .await;
    assert!(!response.ok);
    assert!(response.error.unwrap().contains("re.file.v2; upgrade"));
    assert!(!destination.exists());
    peer.abort();
    let _ = peer.await;
}

#[tokio::test]
async fn jsonl_adapters_backpressure_before_spawning_more_than_32_calls() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::sync::watch;
    for mcp in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let (release, gate) = watch::channel(false);
        let peer = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let seen = seen.clone();
                let mut gate = gate.clone();
                tokio::spawn(async move {
                    let mut ws = accept_async(stream).await.unwrap();
                    let Some(Ok(Message::Text(text))) = ws.next().await else {
                        return;
                    };
                    let request: ExecutorRequest = serde_json::from_str(&text).unwrap();
                    let result = if request.method == "_executor_info" {
                        json!({"metadata":{"id":"peer","version":"0.2.13","capabilities":{}}})
                    } else {
                        assert_eq!(request.method, "read");
                        seen.fetch_add(1, Ordering::SeqCst);
                        if !*gate.borrow() {
                            let _ = gate.changed().await;
                        }
                        json!({"output":{"text":"done"}})
                    };
                    let response = ExecutorResponse::ok(request.id, Some("peer".into()), result);
                    ws.send(Message::Text(
                        serde_json::to_string(&response).unwrap().into(),
                    ))
                    .await
                    .unwrap();
                });
            }
        });
        let client = Client::default();
        client
            .connect_to_executor(
                serde_json::from_value(json!({"id":"peer","url":url,"channels":{"query":64}}))
                    .unwrap(),
            )
            .await
            .unwrap();
        let input = (0..64).map(|id| {
            let value = if mcp { json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"read","arguments":{"filePath":"file"}}}) }
                else { json!({"id":id,"tool":"read","params":{"filePath":"file"}}) };
            format!("{value}\n")
        }).collect::<String>();
        let runner = tokio::spawn(async move {
            let reader = BufReader::new(std::io::Cursor::new(input.into_bytes()));
            if mcp {
                rexeterm::mcp::run_mcp_stdio_io_with_core(client, reader, tokio::io::sink())
                    .await
                    .unwrap();
            } else {
                rexeterm::stdio::run_stdio_io_with_core(client, reader, tokio::io::sink())
                    .await
                    .unwrap();
            }
        });
        timeout(Duration::from_secs(5), async {
            while count.load(Ordering::SeqCst) < 32 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(75)).await;
        assert_eq!(count.load(Ordering::SeqCst), 32);
        release.send_replace(true);
        timeout(Duration::from_secs(5), runner)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 64);
        peer.abort();
    }
}
