use rexeterm::appcore::AppCore as Client;
use rexeterm::{
    client::{
        transport::{Connector, DefaultConnector, Link, LinkFuture, Purpose},
        ConnectExecutorOptions,
    },
    protocol::ExecutorRequest,
    server::{start_executor, Executor, ShellManager},
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};
fn options(url: String) -> ConnectExecutorOptions {
    ConnectExecutorOptions {
        authentication: None,
        tls: None,
        username: None,
        client_name: None,
        id: "node".into(),
        url,
        links: vec![],
        channels: Default::default(),
        system: None,
        device: None,
        labels: BTreeMap::new(),
    }
}
fn request(params: Value) -> ExecutorRequest {
    ExecutorRequest {
        id: json!(1),
        method: "forward".into(),
        params,
        directory: None,
        executor: None,
        tool_timeout_ms: None,
    }
}
async fn call(client: &Client, params: Value) -> Value {
    let response = client.handle(request(params)).await;
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap()["metadata"].clone()
}
async fn forward_roundtrip(scheme: &str) {
    let url = start_executor(
        format!("{scheme}://127.0.0.1:0"),
        Executor::local("node"),
        ShellManager::default_shell(80, 24),
    )
    .await
    .unwrap();
    let client = Client::default();
    client.connect_to_executor(options(url)).await.unwrap();
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = target.local_addr().unwrap();
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = target.accept().await else {
                return;
            };
            tokio::spawn(async move {
                // Respond only after receiving EOF; this catches broken half-close.
                let mut bytes = Vec::new();
                stream.read_to_end(&mut bytes).await.unwrap();
                stream.write_all(&bytes).await.unwrap();
                stream.shutdown().await.unwrap();
            });
        }
    });
    let started=call(&client,json!({"mode":"start","target":{"kind":"tcp","address":address.to_string()},"maxConnections":4})).await;
    let listen = started["listen"]
        .as_str()
        .unwrap()
        .strip_prefix("tcp://")
        .unwrap()
        .to_string();
    let id = started["forwardID"].as_str().unwrap().to_string();
    let mut sessions = tokio::task::JoinSet::new();
    for n in 0..8 {
        let listen = listen.clone();
        sessions.spawn(async move {
            let payload: Vec<_> = (0..256 * 1024).map(|i| (i as u8).wrapping_add(n)).collect();
            let mut stream = TcpStream::connect(listen).await.unwrap();
            stream.write_all(&payload).await.unwrap();
            stream.shutdown().await.unwrap();
            let mut response = Vec::new();
            timeout(Duration::from_secs(30), stream.read_to_end(&mut response))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(response, payload);
        });
    }
    while let Some(result) = sessions.join_next().await {
        result.unwrap();
    }
    let listed = call(&client, json!({"mode":"list"})).await;
    assert_eq!(listed["forwards"][0]["forwardID"], id);
    call(&client, json!({"mode":"stop","forwardID":id})).await;
    assert!(TcpStream::connect(&listen).await.is_err());
    assert_eq!(
        call(&client, json!({"mode":"list"})).await["forwards"],
        json!([])
    );
    server.abort();
    let _ = server.await;
}
#[tokio::test]
async fn tcp_forward_preserves_binary_data_half_close_and_concurrent_sessions() {
    forward_roundtrip("tcp").await;
}
#[tokio::test]
async fn ws_forward_uses_the_same_stream_data_protocol() {
    forward_roundtrip("ws").await;
}
#[cfg(feature = "kcp")]
#[tokio::test]
async fn kcp_forward_uses_the_same_stream_data_protocol() {
    forward_roundtrip("kcp").await;
}
struct ObservedConnector {
    targets: Mutex<Vec<(String, String)>>,
}
impl Connector for ObservedConnector {
    fn max_parallel_links(&self, target: &str) -> usize {
        DefaultConnector.max_parallel_links(target)
    }
    fn connect<'a>(&'a self, target: &'a str) -> LinkFuture<'a, Link> {
        DefaultConnector.connect(target)
    }
    fn connect_for<'a>(&'a self, target: &'a str, purpose: Purpose) -> LinkFuture<'a, Link> {
        self.targets
            .lock()
            .unwrap()
            .push((target.to_string(), format!("{purpose:?}")));
        DefaultConnector.connect_for(target, purpose)
    }
}
#[tokio::test]
async fn one_executor_uses_multiple_validated_links_and_separate_query_channels() {
    let executor = Executor::local("actual-node");
    let manager = ShellManager::default_shell(80, 24);
    let one = start_executor("tcp://127.0.0.1:0", executor.clone(), manager.clone())
        .await
        .unwrap();
    let two = start_executor("tcp://127.0.0.1:0", executor, manager)
        .await
        .unwrap();
    let observed = Arc::new(ObservedConnector {
        targets: Default::default(),
    });
    let client = Client::default().with_connector(observed.clone());
    let mut connect = options(one.clone());
    connect.links = vec![two.clone()];
    client.connect_to_executor(connect).await.unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    for n in 0..8 {
        let client = client.clone();
        tasks.spawn(async move {
            let response = client
                .handle(ExecutorRequest {
                    id: json!(n),
                    method: "stat".into(),
                    params: json!({"filePath":"/nonexistent"}),
                    directory: None,
                    executor: None,
                    tool_timeout_ms: None,
                })
                .await;
            assert!(response.ok, "{:?}", response.error);
        });
    }
    while let Some(value) = tasks.join_next().await {
        value.unwrap();
    }
    let targets = observed.targets.lock().unwrap();
    assert!(targets.contains(&(one, "Query".into())));
    assert!(targets.contains(&(two, "Query".into())));
}
#[tokio::test]
async fn extra_link_to_a_different_executor_is_rejected() {
    let one = start_executor(
        "tcp://127.0.0.1:0",
        Executor::local("one"),
        ShellManager::default_shell(80, 24),
    )
    .await
    .unwrap();
    let two = start_executor(
        "tcp://127.0.0.1:0",
        Executor::local("two"),
        ShellManager::default_shell(80, 24),
    )
    .await
    .unwrap();
    let mut connect = options(one);
    connect.links = vec![two];
    assert!(Client::default()
        .connect_to_executor(connect)
        .await
        .unwrap_err()
        .to_string()
        .contains("same executor"));
}
async fn stop_active(scheme: &str) {
    let url = start_executor(
        format!("{scheme}://127.0.0.1:0"),
        Executor::local("node"),
        ShellManager::default_shell(80, 24),
    )
    .await
    .unwrap();
    let client = Client::default();
    client.connect_to_executor(options(url)).await.unwrap();
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let started=call(&client,json!({"mode":"start","target":{"kind":"tcp","address":target.local_addr().unwrap().to_string()}})).await;
    let listen = started["listen"]
        .as_str()
        .unwrap()
        .strip_prefix("tcp://")
        .unwrap();
    let mut stream = TcpStream::connect(listen).await.unwrap();
    let (mut remote, _) = timeout(Duration::from_secs(5), target.accept())
        .await
        .unwrap()
        .unwrap();
    stream.write_all(b"a").await.unwrap();
    let mut byte = [0];
    timeout(Duration::from_secs(5), remote.read_exact(&mut byte))
        .await
        .unwrap()
        .unwrap();
    call(
        &client,
        json!({"mode":"stop","forwardID":started["forwardID"]}),
    )
    .await;
    let read = timeout(Duration::from_secs(3), stream.read(&mut byte))
        .await
        .unwrap();
    assert!(matches!(read, Ok(0) | Err(_)));
    let read = timeout(Duration::from_secs(3), remote.read(&mut byte))
        .await
        .unwrap();
    assert!(matches!(read, Ok(0) | Err(_)));
    assert!(TcpStream::connect(listen).await.is_err());
}
#[tokio::test]
async fn stop_cancels_active_forward_sessions_and_releases_listener() {
    stop_active("tcp").await;
}
#[cfg(feature = "kcp")]
#[tokio::test]
async fn kcp_stop_releases_the_remote_target() {
    stop_active("kcp").await;
}

#[tokio::test]
async fn last_client_drop_releases_forward_listener_and_errors_are_local() {
    let url = start_executor(
        "tcp://127.0.0.1:0",
        Executor::local("node"),
        ShellManager::default_shell(80, 24),
    )
    .await
    .unwrap();
    let client = Client::default();
    client.connect_to_executor(options(url)).await.unwrap();
    for params in [
        json!({"mode":"start"}),
        json!({"mode":"stop","forwardID":"missing"}),
        json!({"mode":"unknown"}),
        json!({"mode":"start","maxConnections":0,"target":{"kind":"tcp","address":"127.0.0.1:1"}}),
    ] {
        assert!(!client.handle(request(params)).await.ok);
    }
    let started = call(
        &client,
        json!({"mode":"start","target":{"kind":"tcp","address":"127.0.0.1:1"}}),
    )
    .await;
    let listen = started["listen"]
        .as_str()
        .unwrap()
        .strip_prefix("tcp://")
        .unwrap();
    let clone = client.clone();
    drop(client);
    assert_eq!(
        call(&clone, json!({"mode":"list"})).await["forwards"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    drop(clone);
    timeout(Duration::from_secs(2), async {
        loop {
            if TcpStream::connect(listen).await.is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

struct DeviceOpener(Mutex<Option<tokio::io::DuplexStream>>);
impl rexeterm::server::data::StreamOpener for DeviceOpener {
    fn open<'a>(
        &'a self,
        target: &'a rexeterm::protocol::forward::StreamTarget,
    ) -> LinkFuture<'a, Box<dyn rexeterm::server::data::ByteStream>> {
        Box::pin(async move {
            assert_eq!(target.kind, "device");
            assert_eq!(target.address, "mock-serial");
            Ok(Box::new(self.0.lock().unwrap().take().unwrap())
                as Box<dyn rexeterm::server::data::ByteStream>)
        })
    }
}
#[tokio::test]
async fn custom_device_target_uses_the_same_data_bridge() {
    let (stream, mut device) = tokio::io::duplex(4096);
    let executor = Executor::local("node")
        .with_stream_opener(Arc::new(DeviceOpener(Mutex::new(Some(stream)))));
    let url = start_executor(
        "tcp://127.0.0.1:0",
        executor,
        ShellManager::default_shell(80, 24),
    )
    .await
    .unwrap();
    let client = Client::default();
    client.connect_to_executor(options(url)).await.unwrap();
    let started = call(
        &client,
        json!({"mode":"start","target":{"kind":"device","address":"mock-serial"}}),
    )
    .await;
    let mut local = TcpStream::connect(
        started["listen"]
            .as_str()
            .unwrap()
            .strip_prefix("tcp://")
            .unwrap(),
    )
    .await
    .unwrap();
    local.write_all(b"device input").await.unwrap();
    local.shutdown().await.unwrap();
    let mut input = Vec::new();
    timeout(Duration::from_secs(3), device.read_to_end(&mut input))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(input, b"device input");
    device.write_all(b"device output").await.unwrap();
    device.shutdown().await.unwrap();
    let mut output = Vec::new();
    timeout(Duration::from_secs(3), local.read_to_end(&mut output))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output, b"device output");
    call(
        &client,
        json!({"mode":"stop","forwardID":started["forwardID"]}),
    )
    .await;
}
#[tokio::test]
async fn unsupported_datagram_target_returns_a_handshake_error() {
    #[path = "support/re_session.rs"]
    mod session;
    let url = start_executor(
        "tcp://127.0.0.1:0",
        Executor::local("node"),
        ShellManager::default_shell(80, 24),
    )
    .await
    .unwrap();
    let mut link = session::bound_link(&url).await;
    let error = rexeterm::client::data::open_stream(
        &mut link,
        rexeterm::protocol::forward::StreamTarget {
            kind: "udp".into(),
            address: "127.0.0.1:53".into(),
        },
    )
    .await
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("unsupported stream target `udp`"));
}
