use rexeterm::appcore::AppCore as Client;
use rexeterm::{
    client::{
        transport::{self, Connector, DefaultConnector, Frame, Link, LinkFuture},
        ConnectExecutorOptions,
    },
    protocol::ExecutorRequest,
    server::{serve_executor_link, start_executor_listener, Executor, ShellManager},
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, sync::Arc};
use tempfile::tempdir;
use tokio::{
    io::duplex,
    time::{timeout, Duration},
};

fn options(url: String) -> ConnectExecutorOptions {
    ConnectExecutorOptions {
        authentication: None,
        tls: None,
        username: None,
        client_name: None,
        links: Vec::new(),
        channels: Default::default(),
        id: "target".into(),
        url,
        system: None,
        device: None,
        labels: BTreeMap::new(),
    }
}
fn request(tool: &str, params: Value) -> ExecutorRequest {
    ExecutorRequest {
        id: json!(tool),
        method: tool.into(),
        params,
        directory: None,
        executor: None,
        tool_timeout_ms: None,
    }
}

#[tokio::test]
async fn tcp_and_ws_share_tool_and_terminal_data_handlers() {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("hello.txt"), "link payload").unwrap();
    for kind in ["tcp", "ws"] {
        let url = start_executor_listener(
            format!("{kind}://127.0.0.1:0"),
            Executor::local("target"),
            ShellManager::default_shell(80, 24),
        )
        .unwrap();
        let client = Client::default();
        client
            .connect_to_executor(options(url.clone()))
            .await
            .unwrap();
        let mut read = request("read", json!({"filePath":"hello.txt"}));
        read.directory = Some(directory.path().into());
        let response = client.handle(read).await;
        assert!(response.ok, "{:?}", response.error);
        assert!(response.result.unwrap()["output"]["text"]
            .as_str()
            .unwrap()
            .contains("link payload"));
        let command = if cfg!(windows) {
            "Write-Output transport-shell"
        } else {
            "echo transport-shell"
        };
        let response = client
            .handle(request(
                "exbash",
                json!({"mode":"shell","command":command,"read_timeout":2000}),
            ))
            .await;
        assert!(response.ok, "{:?}", response.error);
        assert!(response.result.unwrap()["output"]["text"]
            .as_str()
            .unwrap()
            .contains("transport-shell"));
        let listed = client
            .handle(request("list_executor", json!({})))
            .await
            .result
            .unwrap();
        assert_eq!(listed["metadata"]["executors"][0]["fileTransfer"], true);
        let transfer = client
            .handle(request(
                "file_transfer",
                json!({"mode":"download","localPath":"local.bin","targetPath":"remote.bin"}),
            ))
            .await;
        assert!(!transfer.ok); // The requested remote file does not exist on any transport.
        let mut terminal = DefaultConnector.connect(&url).await.unwrap();
        terminal
            .send(Frame::Text(
                serde_json::to_string(&rexeterm::protocol::terminal::AdminText::List).unwrap(),
            ))
            .await
            .unwrap();
        let Some(Frame::Text(text)) = terminal.recv().await.unwrap() else {
            panic!("terminal response");
        };
        let result: rexeterm::protocol::terminal::ServerText = serde_json::from_str(&text).unwrap();
        assert!(matches!(
            result,
            rexeterm::protocol::terminal::ServerText::Error { .. }
        ));
        terminal.close().await.unwrap();
    }
}

struct HardwareConnector {
    executor: Executor,
    manager: ShellManager,
}
impl Connector for HardwareConnector {
    fn connect<'a>(&'a self, target: &'a str) -> LinkFuture<'a, Link> {
        Box::pin(async move {
            assert_eq!(target, "hardware://device-1");
            let (client, server) = duplex(256);
            let executor = self.executor.clone();
            let manager = self.manager.clone();
            tokio::spawn(async move {
                serve_executor_link(
                    transport::from_stream(server),
                    "hardware:device-1".into(),
                    executor,
                    manager,
                )
                .await
                .unwrap();
            });
            Ok(transport::from_stream(client))
        })
    }
}

#[tokio::test]
async fn custom_byte_stream_connector_uses_the_same_rpc_data_layer() {
    let client = Client::default().with_connector(Arc::new(HardwareConnector {
        executor: Executor::local("target"),
        manager: ShellManager::default_shell(80, 24),
    }));
    client
        .connect_to_executor(options("hardware://device-1".into()))
        .await
        .unwrap();
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("data.txt"), "hardware-link").unwrap();
    let mut read = request("read", json!({"filePath":"data.txt"}));
    read.directory = Some(directory.path().into());
    let response = timeout(Duration::from_secs(2), client.handle(read))
        .await
        .unwrap();
    assert!(response.ok, "{:?}", response.error);
    assert!(response.result.unwrap()["output"]["text"]
        .as_str()
        .unwrap()
        .contains("hardware-link"));
}

#[cfg(unix)]
#[tokio::test]
async fn unix_socket_routes_requests_and_preserves_existing_socket_on_bind_failure() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("rexed.sock");
    let target = format!("unix://{}", path.display());
    let url = start_executor_listener(
        &target,
        Executor::local("target"),
        ShellManager::default_shell(80, 24),
    )
    .unwrap();
    assert!(start_executor_listener(
        &target,
        Executor::local("other"),
        ShellManager::default_shell(80, 24)
    )
    .is_err());
    let client = Client::default();
    client.connect_to_executor(options(url)).await.unwrap();
    let mut stat = request("stat", json!({"filePath":"rexed.sock"}));
    stat.directory = Some(directory.path().into());
    let response = client.handle(stat).await;
    assert!(response.ok, "{:?}", response.error);
}

#[cfg(unix)]
#[tokio::test]
async fn tcp_terminal_carries_control_and_binary_pty_input_and_output() {
    use rexeterm::protocol::terminal::ClientText;
    let manager = ShellManager::default_shell(80, 24);
    let url = start_executor_listener(
        "tcp://127.0.0.1:0",
        Executor::local("target"),
        manager.clone(),
    )
    .unwrap();
    let client = Client::default();
    client.connect_to_executor(options(url)).await.unwrap();
    let response = client.handle(request("exbash",json!({"mode":"run","command":"sh -c 'read line; printf \"echo:%s\\n\" \"$line\"; sleep 1'","read_timeout":0}))).await;
    assert!(response.ok, "{response:?}");
    let id = response.result.unwrap()["metadata"]["asyncID"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut terminal = client
        .open_terminal(None, &id, "test", 80, 24)
        .await
        .unwrap();
    let link = terminal.link();
    link.send(Frame::Text(
        serde_json::to_string(&ClientText::RequestControl).unwrap(),
    ))
    .await
    .unwrap();
    link.send(Frame::Binary(b"tcp-terminal\n".to_vec()))
        .await
        .unwrap();
    timeout(Duration::from_secs(3), async {
        let mut bytes = Vec::new();
        while let Some(frame) = link.recv().await.unwrap() {
            if let Frame::Binary(data) = frame {
                bytes.extend(data);
                if String::from_utf8_lossy(&bytes).contains("echo:tcp-terminal") {
                    return;
                }
            }
        }
        panic!("PTY output missing");
    })
    .await
    .unwrap();
    let _ = link.close().await;
    manager.remove_pty(&id);
    client.shutdown().await;
}
