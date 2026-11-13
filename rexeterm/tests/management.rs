use rexeterm::appcore::AppCore as Client;
use rexeterm::{
    client::{
        management::ManagementClient,
        transport::{Connector, DefaultConnector, Frame, LinkFuture},
        ConnectExecutorOptions, FileTransferMode, FileTransferOptions,
    },
    protocol::{management::*, terminal::ServerText, ExecutorRequest},
    server::{
        management::{resolve_management_target, start_management_listener},
        start_executor, Executor, ShellManager,
    },
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Notify,
    time::{sleep, timeout},
};
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
async fn setup() -> (
    String,
    ManagementClient,
    rexeterm::server::management::ManagementListener,
    ShellManager,
) {
    let executor = Executor::local("node");
    let manager = ShellManager::default_shell(80, 24);
    let url = start_executor("tcp://127.0.0.1:0", executor.clone(), manager.clone())
        .await
        .unwrap();
    let admin = start_management_listener("tcp://127.0.0.1:0", executor, manager.clone()).unwrap();
    let client = ManagementClient::connect(admin.address()).await.unwrap();
    (url, client, admin, manager)
}
async fn call(client: &Client, tool: &str, params: Value) -> Value {
    let response = client
        .handle(ExecutorRequest {
            id: json!(1),
            method: tool.into(),
            params,
            executor: None,
            directory: None,
            tool_timeout_ms: None,
        })
        .await;
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap()
}
async fn until(
    client: &mut ManagementClient,
    condition: impl Fn(&ManagementSnapshot) -> bool,
) -> ManagementSnapshot {
    timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = client.snapshot().await.unwrap();
            if condition(&snapshot) {
                return snapshot;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
async fn receive_meta(connection: &mut rexeterm::client::terminal::TerminalConnection, role: &str) {
    timeout(Duration::from_secs(5),async {loop {
        if let Some(Frame::Text(text))=connection.link().recv().await.unwrap() {
            if matches!(serde_json::from_str::<ServerText>(&text),Ok(ServerText::Meta {role:r,..}) if r == role) {break}
        }
    }}).await.unwrap();
}
#[test]
fn independent_addresses_and_network_port_default() {
    assert_eq!(
        resolve_management_target(Some("tcp://127.0.0.1"), "x", "ws://0.0.0.0:9001/re").unwrap(),
        "tcp://127.0.0.1:9002"
    );
    assert_eq!(
        resolve_management_target(Some("ws://[::1]/ops"), "x", "tcp://[::]:9001").unwrap(),
        "ws://[::1]:9002/ops"
    );
    assert_eq!(
        resolve_management_target(Some("tcp://localhost:1234"), "x", "unix:///tmp/re.sock")
            .unwrap(),
        "tcp://localhost:1234"
    );
    assert!(
        resolve_management_target(Some("tcp://localhost"), "x", "tcp://127.0.0.1:65535").is_err()
    );
    assert!(
        resolve_management_target(Some("tcp://localhost"), "x", "unix:///tmp/re.sock").is_err()
    );
    assert!(resolve_management_target(
        Some("tcp://localhost:1234/path"),
        "x",
        "tcp://localhost:1234"
    )
    .is_err());
    assert!(resolve_management_target(
        Some("ws://localhost:1234/ops?token=x"),
        "x",
        "tcp://localhost:1234"
    )
    .is_err());
}
#[tokio::test]
async fn same_user_has_distinct_clients_and_queries_are_not_channel_rows() {
    let (url, mut admin, _guard, _manager) = setup().await;
    let a = Client::default();
    let b = Client::default();
    a.connect_to_executor(options(url.clone(), "alice", "mcp"))
        .await
        .unwrap();
    b.connect_to_executor(options(url, "alice", "terminal"))
        .await
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("query");
    std::fs::write(&path, b"hello").unwrap();
    for _ in 0..5 {
        call(&a, "read", json!({"filePath":path})).await;
    }
    let snapshot = admin.snapshot().await.unwrap();
    assert_eq!(snapshot.clients.len(), 2);
    assert!(snapshot.channels.is_empty());
    assert!(snapshot
        .clients
        .iter()
        .all(|c| c.owner.username == "alice" && c.transport == "tcp"));
    assert_ne!(
        snapshot.clients[0].owner.session_id,
        snapshot.clients[1].owner.session_id
    );
    let view = rexeterm::admin_view::render(&snapshot, rexeterm::admin_view::Page::Clients);
    assert!(view.contains("2 clients / 1 users") && view.contains("├─") && view.contains("└─"));
    assert!(view.contains("MB/s") && view.contains("TOTAL"));
    a.shutdown().await;
    drop(a);
    let snapshot = until(&mut admin, |s| s.clients.len() == 1).await;
    assert_eq!(snapshot.clients[0].owner.client_name, "terminal");
}
#[tokio::test]
async fn client_traffic_sums_parallel_links_including_short_queries_and_framing() {
    let (url, mut admin, _guard, _manager) = setup().await;
    let mut open = DefaultConnector.connect(&url).await.unwrap();
    open.send(Frame::Text(
        serde_json::to_string(&SessionMessage::Open {
            client: ClientHello::default(),
        })
        .unwrap(),
    ))
    .await
    .unwrap();
    let Some(Frame::Text(text)) = open.recv().await.unwrap() else {
        panic!("missing open")
    };
    let SessionMessage::Opened { credential, .. } = serde_json::from_str(&text).unwrap() else {
        panic!("missing credential")
    };
    assert_eq!(open.recv().await.unwrap(), None);
    let baseline = until(&mut admin, |s| {
        s.clients.len() == 1 && s.clients[0].active_links == 0
    })
    .await
    .clients
    .remove(0)
    .traffic;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("query");
    std::fs::write(&path, "query traffic").unwrap();
    let mut links = vec![];
    let mut sent = 0;
    let mut received = 0;
    let mut payload_bytes = 0;
    for id in 1..=2 {
        let mut link = DefaultConnector.connect(&url).await.unwrap();
        let bind = serde_json::to_string(&SessionMessage::Bind {
            credential: credential.clone(),
            metadata: None,
        })
        .unwrap();
        payload_bytes += bind.len() as u64;
        link.send(Frame::Text(bind)).await.unwrap();
        assert!(matches!(link.recv().await.unwrap(), Some(Frame::Text(_))));
        let query = json!({"id":id,"tool":"stat","params":{"filePath":path}}).to_string();
        payload_bytes += query.len() as u64;
        link.send(Frame::Text(query)).await.unwrap();
        assert!(matches!(link.recv().await.unwrap(), Some(Frame::Text(_))));
        let wire = link.traffic().unwrap().snapshot();
        sent += wire.sent_bytes;
        received += wire.received_bytes;
        links.push(link);
    }
    let snapshot = admin.snapshot().await.unwrap();
    let client = &snapshot.clients[0];
    assert_eq!(client.active_links, 2);
    assert!(client.traffic_complete);
    assert!(snapshot.channels.is_empty());
    assert_eq!(
        client.traffic.received_bytes - baseline.received_bytes,
        sent
    );
    assert_eq!(client.traffic.sent_bytes - baseline.sent_bytes, received);
    assert_eq!(
        sent - payload_bytes,
        4 * 9,
        "two bind and two query REX1 headers"
    );
    assert!(
        client.traffic.receive_bytes_per_second > 0 && client.traffic.send_bytes_per_second > 0
    );
    for link in &mut links {
        link.close().await.unwrap();
    }
    let snapshot = until(&mut admin, |s| s.clients[0].active_links == 0).await;
    assert_eq!(
        snapshot.clients[0].traffic.received_bytes,
        baseline.received_bytes + sent + 2 * 9
    );
    assert!(snapshot.clients[0].traffic.sent_bytes >= baseline.sent_bytes + received);
}
#[tokio::test]
async fn program_control_is_owned_by_the_creator_client_and_shutdown_stops_it() {
    let (url, mut admin, _guard, _manager) = setup().await;
    let a = Client::default();
    let b = Client::default();
    a.connect_to_executor(options(url.clone(), "alice", "mcp"))
        .await
        .unwrap();
    b.connect_to_executor(options(url, "alice", "terminal"))
        .await
        .unwrap();
    let command = if cfg!(windows) {
        "Start-Sleep -Seconds 30"
    } else {
        "sleep 30"
    };
    let result = call(
        &a,
        "exbash",
        json!({"mode":"shell","command":command,"read_timeout":1,"timeout":60000}),
    )
    .await;
    let id = result["metadata"]["asyncID"].as_str().unwrap();
    let mut denied = b.open_terminal(None, id, "foreign", 80, 24).await.unwrap();
    let Some(Frame::Text(text)) = denied.link().recv().await.unwrap() else {
        panic!("missing read-only metadata");
    };
    assert!(matches!(
        serde_json::from_str(&text).unwrap(),
        ServerText::Meta {
            read_only: true,
            ..
        }
    ));
    denied.link().close().await.unwrap();
    let mut terminal = a.open_terminal(None, id, "own", 80, 24).await.unwrap();
    receive_meta(&mut terminal, "Controller").await;
    let s = until(&mut admin, |s| {
        s.programs
            .iter()
            .any(|p| p.program_id == id && p.controller_session.is_some())
    })
    .await;
    let p = s.programs.iter().find(|p| p.program_id == id).unwrap();
    assert_eq!(
        p.created_by.as_ref().unwrap().client_id,
        p.controller_session.as_ref().unwrap().client_id
    );
    assert_eq!(p.attached_clients.len(), 1);
    a.shutdown().await;
    let s = until(&mut admin, |s| {
        s.channels.is_empty() && !s.programs.iter().any(|p| p.program_id == id)
    })
    .await;
    assert_eq!(s.clients.len(), 1);
    assert!(_manager.core().session(id).is_none());
    b.shutdown().await;
}

struct Pause {
    reached: Notify,
    resume: Notify,
}
impl rexeterm::client::data::file::TransferObserver for Pause {
    fn progress(&self, bytes: u64, _: u64) -> LinkFuture<'_, ()> {
        Box::pin(async move {
            if bytes == 0 {
                self.reached.notify_one();
                self.resume.notified().await;
            }
            Ok(())
        })
    }
}
#[tokio::test]
async fn file_channels_have_owners_and_cancel_cleanup_does_not_block_queries() {
    let (url, mut admin, _guard, _manager) = setup().await;
    let client = Client::default();
    client
        .connect_to_executor(options(url, "alice", "files"))
        .await
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let dest = root.path().join("dest");
    std::fs::write(&source, vec![5; 256 * 1024]).unwrap();
    let pause = Arc::new(Pause {
        reached: Notify::new(),
        resume: Notify::new(),
    });
    let worker_client = client.clone();
    let observer = pause.clone();
    let target = dest.clone();
    let worker = tokio::spawn(async move {
        worker_client
            .transfer_file(
                None,
                FileTransferOptions {
                    metadata: Default::default(),
                    mode: FileTransferMode::Upload,
                    local_path: source,
                    target_path: target,
                    overwrite: Some(false),
                    offset: 0,
                    sha256: None,
                },
                None,
                Some(observer.as_ref()),
            )
            .await
    });
    timeout(Duration::from_secs(5), pause.reached.notified())
        .await
        .unwrap();
    let s = admin.snapshot().await.unwrap();
    assert_eq!(s.channels.len(), 1);
    assert_eq!(s.channels[0].kind, ChannelKind::File);
    assert_eq!(s.channels[0].owner.as_ref().unwrap().client_name, "files");
    call(&client, "stat", json!({"filePath":root.path()})).await;
    assert_eq!(admin.snapshot().await.unwrap().channels.len(), 1);
    worker.abort();
    let _ = worker.await;
    until(&mut admin, |s| s.channels.is_empty()).await;
    assert!(!dest.exists());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}
#[tokio::test]
async fn forwarding_streams_have_the_originating_session() {
    let (url, mut admin, _guard, _manager) = setup().await;
    let client = Client::default();
    client
        .connect_to_executor(options(url, "alice", "forwarder"))
        .await
        .unwrap();
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let started=call(&client,"forward",json!({"mode":"start","target":{"kind":"tcp","address":target.local_addr().unwrap().to_string()}})).await;
    let listen = started["metadata"]["listen"]
        .as_str()
        .unwrap()
        .strip_prefix("tcp://")
        .unwrap();
    let mut incoming = TcpStream::connect(listen).await.unwrap();
    incoming.write_all(b"ping").await.unwrap();
    let (mut outgoing, _) = timeout(Duration::from_secs(5), target.accept())
        .await
        .unwrap()
        .unwrap();
    let mut bytes = [0; 4];
    outgoing.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"ping");
    let s = admin.snapshot().await.unwrap();
    assert_eq!(s.channels.len(), 1);
    assert_eq!(s.channels[0].kind, ChannelKind::Forward);
    assert_eq!(
        s.channels[0].owner.as_ref().unwrap().client_name,
        "forwarder"
    );
    drop(incoming);
    drop(outgoing);
    call(
        &client,
        "forward",
        json!({"mode":"stop","forwardID":started["metadata"]["forwardID"]}),
    )
    .await;
    until(&mut admin, |s| s.channels.is_empty()).await;
}
#[tokio::test]
async fn management_and_business_protocols_and_ws_paths_stay_separate() {
    let executor = Executor::local("node");
    let manager = ShellManager::default_shell(80, 24);
    let business = start_executor("ws://127.0.0.1:0/re", executor.clone(), manager.clone())
        .await
        .unwrap();
    let admin = start_management_listener("ws://127.0.0.1:0/ops", executor, manager).unwrap();
    let mut management = ManagementClient::connect(admin.address()).await.unwrap();
    assert_eq!(management.snapshot().await.unwrap().executor_id, "node");
    let wrong = admin.address().replace("/ops", "/wrong");
    assert!(ManagementClient::connect(&wrong).await.is_err());
    let mut wrong_client = ManagementClient::connect(&business).await.unwrap();
    assert!(wrong_client.snapshot().await.is_err());
    let mut link = DefaultConnector.connect(admin.address()).await.unwrap();
    link.send(Frame::Text(
        json!({"id":1,"tool":"exbash","params":{"mode":"shell","command":"echo wrong"}})
            .to_string(),
    ))
    .await
    .unwrap();
    let Some(Frame::Text(text)) = link.recv().await.unwrap() else {
        panic!("missing rejection")
    };
    assert!(matches!(
        serde_json::from_str(&text).unwrap(),
        ManagementMessage::Error { .. }
    ));
    let client = Client::default();
    client
        .connect_to_executor(options(business, "alice", "ws"))
        .await
        .unwrap();
    assert_eq!(
        management.snapshot().await.unwrap().clients[0].transport,
        "ws"
    );
}
#[tokio::test]
async fn a_forged_session_binding_cannot_claim_another_clients_channels() {
    let (url, mut admin, _guard, _manager) = setup().await;
    let client = Client::default();
    client
        .connect_to_executor(options(url.clone(), "alice", "owner"))
        .await
        .unwrap();
    let id = admin.snapshot().await.unwrap().clients[0]
        .owner
        .session_id
        .clone();
    let mut link = DefaultConnector.connect(&url).await.unwrap();
    link.send(Frame::Text(
        serde_json::to_string(&SessionMessage::Bind {
            credential: SessionCredential {
                session_id: id,
                secret: "wrong-secret".into(),
            },
            metadata: None,
        })
        .unwrap(),
    ))
    .await
    .unwrap();
    let Some(Frame::Text(text)) = link.recv().await.unwrap() else {
        panic!("missing rejection")
    };
    assert!(matches!(
        serde_json::from_str(&text).unwrap(),
        SessionMessage::Error { .. }
    ));
    assert!(admin.snapshot().await.unwrap().channels.is_empty());
}
#[cfg(unix)]
#[tokio::test]
async fn unix_management_is_private_cleans_its_socket_and_rexctl_reads_it() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("admin.sock");
    let target = format!("unix://{}", path.display());
    let executor = Executor::local("node");
    let manager = ShellManager::default_shell(80, 24);
    let guard = start_management_listener(&target, executor.clone(), manager.clone()).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(start_management_listener(&target, executor, manager).is_err());
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_rexctl"))
        .args(["--target", &target, "--json"])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["executorId"], "node");
    let mut client = ManagementClient::connect(&target).await.unwrap();
    assert!(client.snapshot().await.unwrap().clients.is_empty());
    drop(guard);
    timeout(Duration::from_secs(3), async {
        while path.exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(client.snapshot().await.is_err());
}
#[tokio::test]
async fn client_shutdown_clears_shared_routes_and_active_forwards() {
    let (url, mut admin, _guard, _manager) = setup().await;
    let client = Client::default();
    let shared = client.clone();
    client
        .connect_to_executor(options(url, "alice", "shutdown"))
        .await
        .unwrap();
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let started=call(&client,"forward",json!({"mode":"start","target":{"kind":"tcp","address":target.local_addr().unwrap().to_string()}})).await;
    let address = started["metadata"]["listen"]
        .as_str()
        .unwrap()
        .strip_prefix("tcp://")
        .unwrap();
    let mut local = TcpStream::connect(address).await.unwrap();
    local.write_all(b"x").await.unwrap();
    let (mut remote, _) = target.accept().await.unwrap();
    let mut byte = [0];
    remote.read_exact(&mut byte).await.unwrap();
    until(&mut admin, |s| s.channels.len() == 1).await;
    client.shutdown().await;
    until(&mut admin, |s| {
        s.clients.is_empty() && s.channels.is_empty()
    })
    .await;
    assert_eq!(
        timeout(Duration::from_secs(2), local.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert!(TcpStream::connect(address).await.is_err());
    let listed = call(&shared, "list_executor", json!({})).await;
    assert!(listed["metadata"]["default"].is_null());
    assert_eq!(listed["metadata"]["executors"], json!([]));
    shared.shutdown().await;
}

#[cfg(all(feature = "stdio", feature = "mcp-stdio"))]
#[tokio::test]
async fn business_cli_eof_closes_the_session_before_runtime_exit() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let (url, mut admin, _guard, _manager) = setup().await;
    let root = tempfile::tempdir().unwrap();
    for (binary, mcp) in [(env!("CARGO_BIN_EXE_rexeterm-mcp"), true)] {
        let mut process = tokio::process::Command::new(binary)
            .args([
                "--no-local",
                "--re-user",
                "alice",
                "--client-name",
                "eof-test",
                "--client-settings",
            ])
            .arg(
                root.path()
                    .join(if mcp { "mcp.json" } else { "stdio.json" }),
            )
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let request = if mcp {
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"connect_to_executor","arguments":{"id":"node","url":url}}})
        } else {
            json!({"id":1,"tool":"connect_to_executor","params":{"id":"node","url":url}})
        };
        process
            .stdin
            .as_mut()
            .unwrap()
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        let mut lines = BufReader::new(process.stdout.take().unwrap()).lines();
        let line = timeout(Duration::from_secs(5), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        if mcp {
            assert_eq!(response["result"]["isError"], false);
        } else {
            assert_eq!(response["ok"], true);
        }
        assert_eq!(admin.snapshot().await.unwrap().clients.len(), 1);
        drop(process.stdin.take());
        let output = timeout(Duration::from_secs(8), process.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        timeout(
            Duration::from_secs(2),
            until(&mut admin, |s| s.clients.is_empty()),
        )
        .await
        .unwrap();
    }
}
