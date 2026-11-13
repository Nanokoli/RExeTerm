use rexeterm::appcore::AppCore as Client;
use rexeterm::{
    client::{
        data::file::{receive_message, send_message, TransferObserver},
        transport::{Connector, DefaultConnector, Frame, Link, LinkFuture, Purpose},
        ConnectExecutorOptions, FileTransferMode, FileTransferOptions,
    },
    protocol::{
        file::{FileMessage, FileRequest},
        ExecutorRequest,
    },
    server::{hash_bytes, serve_executor_link, start_executor, Executor, ShellManager},
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tempfile::tempdir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::{sleep, timeout},
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
fn transfer(
    mode: FileTransferMode,
    local: &std::path::Path,
    target: &std::path::Path,
) -> FileTransferOptions {
    FileTransferOptions {
        metadata: Default::default(),
        mode,
        local_path: local.to_owned(),
        target_path: target.to_owned(),
        overwrite: Some(false),
        offset: 0,
        sha256: None,
    }
}
struct Observed {
    routes: Mutex<Vec<String>>,
}
impl Connector for Observed {
    fn max_parallel_links(&self, target: &str) -> usize {
        DefaultConnector.max_parallel_links(target)
    }
    fn connect<'a>(&'a self, target: &'a str) -> LinkFuture<'a, Link> {
        DefaultConnector.connect(target)
    }
    fn connect_for<'a>(&'a self, target: &'a str, purpose: Purpose) -> LinkFuture<'a, Link> {
        self.routes.lock().unwrap().push(format!("{purpose:?}"));
        DefaultConnector.connect_for(target, purpose)
    }
}
struct Progress(AtomicU64);
impl TransferObserver for Progress {
    fn progress(&self, bytes: u64, _: u64) -> LinkFuture<'_, ()> {
        self.0.store(bytes, Ordering::Relaxed);
        Box::pin(async { Ok(()) })
    }
}
async fn roundtrip(scheme: &str, size: usize) {
    let root = tempdir().unwrap();
    let url = start_executor(
        format!("{scheme}://127.0.0.1:0"),
        Executor::local("node"),
        ShellManager::default_shell(80, 24),
    )
    .await
    .unwrap();
    let observed = Arc::new(Observed {
        routes: Mutex::new(vec![]),
    });
    let client = Client::default().with_connector(observed.clone());
    client.connect_to_executor(options(url)).await.unwrap();
    let source = root.path().join("local.bin");
    let remote = root.path().join("remote.bin");
    let copy = root.path().join("copy.bin");
    let payload: Vec<u8> = (0..size).map(|n| (n % 251) as u8).collect();
    std::fs::write(&source, &payload).unwrap();
    let progress = Progress(AtomicU64::new(0));
    let result = client
        .transfer_file(
            None,
            transfer(FileTransferMode::Upload, &source, &remote),
            None,
            Some(&progress),
        )
        .await
        .unwrap();
    assert_eq!(result.bytes, size as u64);
    assert_eq!(result.sha256, hash_bytes(&payload));
    assert_eq!(progress.0.load(Ordering::Relaxed), size as u64);
    assert_eq!(std::fs::read(&remote).unwrap(), payload);
    let result = client
        .transfer_file(
            None,
            transfer(FileTransferMode::Download, &copy, &remote),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.bytes, size as u64);
    assert_eq!(std::fs::read(&copy).unwrap(), payload);
    assert_eq!(
        observed
            .routes
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.as_str() == "File")
            .count(),
        2
    );
    assert!(observed
        .routes
        .lock()
        .unwrap()
        .iter()
        .all(|p| matches!(p.as_str(), "File" | "Control")));
    std::fs::remove_file(&copy).unwrap();
    let mut request = transfer(FileTransferMode::Download, &copy, &remote);
    request.offset = size.min(7) as u64;
    let result = client
        .transfer_file(None, request, None, None)
        .await
        .unwrap();
    let suffix = &payload[size.min(7)..];
    assert_eq!(std::fs::read(copy).unwrap(), suffix);
    assert_eq!(result.sha256, hash_bytes(suffix));
}
#[tokio::test]
async fn websocket_file_transfer_uses_the_file_lane() {
    roundtrip("ws", 512 * 1024).await;
}
#[tokio::test]
async fn tcp_file_transfer_exceeds_the_frame_limit_without_large_frames() {
    roundtrip("tcp", 17 * 1024 * 1024).await;
}
#[tokio::test]
async fn empty_files_complete_and_verify() {
    roundtrip("tcp", 0).await;
}
#[cfg(feature = "kcp")]
#[tokio::test]
async fn kcp_file_transfer_uses_the_same_data_protocol() {
    roundtrip("kcp", 256 * 1024).await;
}
struct DeviceConnector {
    executor: Executor,
    manager: ShellManager,
}
impl Connector for DeviceConnector {
    fn connect<'a>(&'a self, _: &'a str) -> LinkFuture<'a, Link> {
        Box::pin(async move {
            let executor = self.executor.clone();
            let manager = self.manager.clone();
            let (client, server) = tokio::io::duplex(4096);
            tokio::spawn(async move {
                let _ = serve_executor_link(
                    rexeterm::client::transport::from_stream(server),
                    "device".into(),
                    executor,
                    manager,
                )
                .await;
            });
            Ok(rexeterm::client::transport::from_stream(client))
        })
    }
}
#[tokio::test]
async fn injected_device_link_transfers_without_network_sockets() {
    let root = tempdir().unwrap();
    let source = root.path().join("source");
    let target = root.path().join("target");
    std::fs::write(&source, b"custom link").unwrap();
    let client = Client::default().with_connector(Arc::new(DeviceConnector {
        executor: Executor::local("node"),
        manager: ShellManager::default_shell(80, 24),
    }));
    client
        .connect_to_executor(options("device://mock".into()))
        .await
        .unwrap();
    client
        .transfer_file(
            None,
            transfer(FileTransferMode::Upload, &source, &target),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read(target).unwrap(), b"custom link");
}
#[cfg(unix)]
#[tokio::test]
async fn unix_file_transfer_uses_the_same_data_protocol() {
    let root = tempdir().unwrap();
    let url = start_executor(
        format!("unix://{}", root.path().join("rex.sock").display()),
        Executor::local("node"),
        ShellManager::default_shell(80, 24),
    )
    .await
    .unwrap();
    let client = Client::default();
    client.connect_to_executor(options(url)).await.unwrap();
    let source = root.path().join("source");
    let target = root.path().join("target");
    std::fs::write(&source, b"unix link").unwrap();
    client
        .transfer_file(
            None,
            transfer(FileTransferMode::Upload, &source, &target),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read(target).unwrap(), b"unix link");
}
fn no_staging(root: &std::path::Path) -> bool {
    std::fs::read_dir(root).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".re-transfer-")
    })
}
#[tokio::test]
async fn hash_failure_and_overwrite_rejection_preserve_original_file() {
    let root = tempdir().unwrap();
    let url = start_executor(
        "tcp://127.0.0.1:0",
        Executor::local("node"),
        ShellManager::default_shell(80, 24),
    )
    .await
    .unwrap();
    let client = Client::default();
    client.connect_to_executor(options(url)).await.unwrap();
    let source = root.path().join("source");
    let target = root.path().join("target");
    std::fs::write(&source, b"new content").unwrap();
    std::fs::write(&target, b"original").unwrap();
    assert!(client
        .transfer_file(
            None,
            transfer(FileTransferMode::Upload, &source, &target),
            None,
            None
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("exists"));
    let mut request = transfer(FileTransferMode::Upload, &source, &target);
    request.overwrite = Some(true);
    request.sha256 = Some(hash_bytes(b"wrong"));
    assert!(client
        .transfer_file(None, request, None, None)
        .await
        .unwrap_err()
        .to_string()
        .contains("sha256"));
    assert_eq!(std::fs::read(&target).unwrap(), b"original");
    assert!(no_staging(root.path()));
    let mut request = transfer(FileTransferMode::Upload, &source, &target);
    request.overwrite = Some(true);
    client
        .transfer_file(None, request, None, None)
        .await
        .unwrap();
    assert_eq!(std::fs::read(target).unwrap(), b"new content");
}
#[tokio::test]
async fn aborted_upload_and_invalid_end_never_replace_the_target() {
    #[path = "support/re_session.rs"]
    mod session;
    for invalid_end in [false, true] {
        let root = tempdir().unwrap();
        let target = root.path().join("target");
        std::fs::write(&target, b"original").unwrap();
        let url = start_executor(
            "tcp://127.0.0.1:0",
            Executor::local("node"),
            ShellManager::default_shell(80, 24),
        )
        .await
        .unwrap();
        let mut link = session::bound_link(&url).await;
        send_message(
            &mut link,
            &FileMessage::Open {
                request: FileRequest::Upload {
                    path: target.clone(),
                    directory: None,
                    size: 8,
                    overwrite: true,
                    sha256: None,
                },
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            receive_message(&mut link).await.unwrap(),
            FileMessage::Ready { .. }
        ));
        link.send(Frame::Binary(b"abcd".to_vec())).await.unwrap();
        if invalid_end {
            send_message(
                &mut link,
                &FileMessage::End {
                    bytes: 4,
                    sha256: hash_bytes(b"abcd"),
                },
            )
            .await
            .unwrap();
            assert!(receive_message(&mut link)
                .await
                .unwrap_err()
                .to_string()
                .contains("size mismatch"));
        } else {
            link.close().await.unwrap();
        }
        drop(link);
        timeout(Duration::from_secs(2), async {
            while !no_staging(root.path()) {
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(std::fs::read(target).unwrap(), b"original");
    }
}
#[tokio::test]
async fn http_file_routes_are_removed() {
    let root = tempdir().unwrap();
    let source = root.path().join("source");
    let target = root.path().join("target");
    std::fs::write(&source, b"private file bytes").unwrap();
    let url = start_executor(
        "ws://127.0.0.1:0",
        Executor::local("node"),
        ShellManager::default_shell(80, 24),
    )
    .await
    .unwrap();
    for method in ["GET", "PUT"] {
        let mut stream = TcpStream::connect(url.strip_prefix("ws://").unwrap())
            .await
            .unwrap();
        stream.write_all(format!("{method} /re-file/v1 HTTP/1.1\r\nHost: localhost\r\nX-RE-Path: {}\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx",if method=="GET"{&source}else{&target}.display()).as_bytes()).await.unwrap();
        let mut response = Vec::new();
        timeout(Duration::from_secs(2), stream.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(!response.starts_with(b"HTTP/1.1 200"));
        assert!(!response.windows(18).any(|w| w == b"private file bytes"));
    }
    assert!(!target.exists());
}
#[tokio::test]
async fn file_transfer_uses_the_client_tool_contract() {
    let root = tempdir().unwrap();
    let source = root.path().join("source");
    let target = root.path().join("target");
    std::fs::write(&source, b"tool payload").unwrap();
    let runtime = rexeterm::daemon::Daemon::new().await.unwrap();
    let response = runtime
        .core()
        .handle(ExecutorRequest {
            id: json!(1),
            method: "file_transfer".into(),
            params: json!({"mode":"upload","localPath":source,"targetPath":target}),
            directory: None,
            executor: None,
            tool_timeout_ms: None,
        })
        .await;
    assert!(response.ok, "{:?}", response.error);
    assert_eq!(std::fs::read(target).unwrap(), b"tool payload");
}
struct Paused {
    ready: tokio::sync::Notify,
}
impl TransferObserver for Paused {
    fn progress(&self, _: u64, _: u64) -> LinkFuture<'_, ()> {
        self.ready.notify_one();
        Box::pin(std::future::pending())
    }
}
#[tokio::test]
async fn cancelling_download_cleans_staging_and_file_lane_does_not_block_queries() {
    let root = tempdir().unwrap();
    let source = root.path().join("source");
    let target = root.path().join("target");
    std::fs::write(&source, vec![9; 128 * 1024]).unwrap();
    std::fs::write(&target, b"original").unwrap();
    let mut config = options(
        start_executor(
            "tcp://127.0.0.1:0",
            Executor::local("node"),
            ShellManager::default_shell(80, 24),
        )
        .await
        .unwrap(),
    );
    config.channels.file = 1;
    let client = Client::default();
    client.connect_to_executor(config).await.unwrap();
    let observer = Arc::new(Paused {
        ready: tokio::sync::Notify::new(),
    });
    let task = {
        let client = client.clone();
        let observer = observer.clone();
        let mut request = transfer(FileTransferMode::Download, &target, &source);
        request.overwrite = Some(true);
        tokio::spawn(async move {
            client
                .transfer_file(None, request, None, Some(observer.as_ref()))
                .await
        })
    };
    timeout(Duration::from_secs(2), observer.ready.notified())
        .await
        .unwrap();
    let query = timeout(
        Duration::from_secs(2),
        client.handle(ExecutorRequest {
            id: json!(1),
            method: "stat".into(),
            params: json!({"filePath":source}),
            directory: None,
            executor: None,
            tool_timeout_ms: None,
        }),
    )
    .await
    .unwrap();
    assert!(query.ok);
    task.abort();
    let _ = task.await;
    assert!(no_staging(root.path()));
    assert_eq!(std::fs::read(&target).unwrap(), b"original");
    let mut request = transfer(FileTransferMode::Download, &target, &source);
    request.overwrite = Some(true);
    timeout(
        Duration::from_secs(3),
        client.transfer_file(None, request, None, None),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        std::fs::read(target).unwrap(),
        std::fs::read(source).unwrap()
    );
}
struct CorruptWriter(rexeterm::client::transport::Sender);
impl rexeterm::client::transport::FrameWriter for CorruptWriter {
    fn send(&mut self, frame: Frame) -> LinkFuture<'_, ()> {
        Box::pin(async move {
            let frame = match frame {
                Frame::Text(text) if text.contains("re.file.v2.end") => Frame::Text(
                    serde_json::to_string(&FileMessage::End {
                        bytes: 7,
                        sha256: hash_bytes(b"corrupt"),
                    })
                    .unwrap(),
                ),
                other => other,
            };
            self.0.send(frame).await
        })
    }
}
struct TestReader(rexeterm::client::transport::Receiver);
impl rexeterm::client::transport::FrameReader for TestReader {
    fn recv(&mut self) -> LinkFuture<'_, Option<Frame>> {
        Box::pin(self.0.recv())
    }
}
struct CorruptConnector {
    executor: Executor,
    manager: ShellManager,
}
impl Connector for CorruptConnector {
    fn connect<'a>(&'a self, _: &'a str) -> LinkFuture<'a, Link> {
        Box::pin(async move {
            let executor = self.executor.clone();
            let manager = self.manager.clone();
            let (client, server) = tokio::io::duplex(4096);
            let (sender, receiver) = rexeterm::client::transport::from_stream(server).split();
            tokio::spawn(async move {
                let _ = serve_executor_link(
                    Link::new(CorruptWriter(sender), TestReader(receiver)),
                    "mock".into(),
                    executor,
                    manager,
                )
                .await;
            });
            Ok(rexeterm::client::transport::from_stream(client))
        })
    }
}
#[tokio::test]
async fn corrupt_download_hash_keeps_the_existing_local_file() {
    let root = tempdir().unwrap();
    let source = root.path().join("source");
    let target = root.path().join("target");
    std::fs::write(&source, b"payload").unwrap();
    std::fs::write(&target, b"original").unwrap();
    let client = Client::default().with_connector(Arc::new(CorruptConnector {
        executor: Executor::local("node"),
        manager: ShellManager::default_shell(80, 24),
    }));
    client
        .connect_to_executor(options("mock://corrupt".into()))
        .await
        .unwrap();
    let mut request = transfer(FileTransferMode::Download, &target, &source);
    request.overwrite = Some(true);
    assert!(client
        .transfer_file(None, request, None, None)
        .await
        .unwrap_err()
        .to_string()
        .contains("sha256"));
    assert!(no_staging(root.path()));
    assert_eq!(std::fs::read(target).unwrap(), b"original");
}
#[tokio::test]
async fn stdio_file_transfer_completes_before_reporting_success() {
    use tokio::io::BufReader;
    let root = tempdir().unwrap();
    let source = root.path().join("source");
    let target = root.path().join("target");
    std::fs::write(&source, b"stdio payload").unwrap();
    let runtime = rexeterm::daemon::Daemon::new().await.unwrap();
    let (mut tx, rx) = tokio::io::duplex(4096);
    let (output, mut reader) = tokio::io::duplex(4096);
    let client = runtime.core().clone();
    let runner = tokio::spawn(async move {
        rexeterm::stdio::run_stdio_io_with_core(client, BufReader::new(rx), output)
            .await
            .unwrap()
    });
    tx.write_all(format!("{}\n",json!({"id":1,"tool":"file_transfer","params":{"mode":"download","localPath":target,"targetPath":source}})).as_bytes()).await.unwrap();
    tx.shutdown().await.unwrap();
    let mut text = String::new();
    reader.read_to_string(&mut text).await.unwrap();
    runner.await.unwrap();
    let response: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(response["result"]["metadata"]["bytes"], 13);
    assert_eq!(std::fs::read(target).unwrap(), b"stdio payload");
}
