use rexeterm::{
    client::ConnectExecutorOptions,
    daemon::Daemon,
    protocol::{ExecutorInfo, ExecutorRequest, ExecutorResponse},
    server::{start_shared_executor_ws, Executor, ShellManager},
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, path::Path, time::Duration};
use tempfile::tempdir;
use tokio::time::timeout;

fn executor(id: &str) -> Executor {
    Executor::new(ExecutorInfo {
        id: id.to_string(),
        system: None,
        device: None,
        labels: BTreeMap::new(),
    })
}

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

fn result(response: ExecutorResponse) -> Value {
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap()
}

#[tokio::test]
async fn client_clones_share_registry_but_separate_clients_do_not() {
    let directory = tempdir().unwrap();
    let original_runtime = Daemon::new().await.unwrap();
    let original = original_runtime.core().clone();
    let cloned = original.clone();
    let other_runtime = Daemon::new().await.unwrap();
    let other = other_runtime.core().clone();
    let address = start_shared_executor_ws(
        "127.0.0.1:0",
        executor("remote"),
        ShellManager::default_shell(80, 24),
    )
    .unwrap();
    cloned
        .connect_to_executor(ConnectExecutorOptions {
            authentication: None,
            tls: None,
            username: None,
            client_name: None,
            links: Vec::new(),
            channels: Default::default(),
            id: "remote".to_string(),
            url: format!("ws://{address}"),
            system: None,
            device: None,
            labels: BTreeMap::new(),
        })
        .await
        .unwrap();
    cloned.set_default_executor("remote").await.unwrap();
    assert_eq!(original.default_executor().await, "remote");
    assert_eq!(other.default_executor().await, "local");
    assert!(other.set_default_executor("remote").await.is_err());

    fs::write(directory.path().join("shared.txt"), "remote contents").unwrap();
    let response = original
        .handle(request(
            "read",
            directory.path(),
            json!({"filePath": "shared.txt"}),
        ))
        .await;
    assert_eq!(response.executor.as_deref(), Some("remote"));
    assert!(result(response)["output"]["text"]
        .as_str()
        .unwrap()
        .contains("remote contents"));
}

#[tokio::test]
async fn framed_upload_serializes_tool_writes_while_reads_continue() {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("readable.txt"), "available").unwrap();
    let original = executor("shared-lock");
    let address = start_shared_executor_ws(
        "127.0.0.1:0",
        original.clone(),
        ShellManager::default_shell(80, 24),
    )
    .unwrap();
    use rexeterm::{
        client::{
            data::file::{receive_message, send_message},
            transport::Frame,
        },
        protocol::file::{FileMessage, FileRequest},
    };
    #[path = "support/re_session.rs"]
    mod session;
    let mut upload = session::bound_link(&format!("ws://{address}")).await;
    send_message(
        &mut upload,
        &FileMessage::Open {
            request: FileRequest::Upload {
                path: "uploaded.bin".into(),
                directory: Some(directory.path().to_owned()),
                size: 8,
                overwrite: false,
                sha256: None,
            },
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        receive_message(&mut upload).await.unwrap(),
        FileMessage::Ready { .. }
    ));
    upload.send(Frame::Binary(b"abcd".to_vec())).await.unwrap();

    let write = original.handle(request(
        "FileAction",
        directory.path(),
        json!({
            "mode": "create", "filePath": "created.txt", "content": "after upload",
        }),
    ));
    tokio::pin!(write);
    assert!(timeout(Duration::from_millis(75), &mut write)
        .await
        .is_err());
    let read = timeout(
        Duration::from_secs(2),
        original.handle(request(
            "read",
            directory.path(),
            json!({"filePath": "readable.txt"}),
        )),
    )
    .await
    .unwrap();
    assert!(result(read)["output"]["text"]
        .as_str()
        .unwrap()
        .contains("available"));

    upload.send(Frame::Binary(b"efgh".to_vec())).await.unwrap();
    send_message(
        &mut upload,
        &FileMessage::End {
            bytes: 8,
            sha256: rexeterm::server::hash_bytes(b"abcdefgh"),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        receive_message(&mut upload).await.unwrap(),
        FileMessage::Done { .. }
    ));
    result(timeout(Duration::from_secs(2), &mut write).await.unwrap());
    assert_eq!(
        fs::read(directory.path().join("uploaded.bin")).unwrap(),
        b"abcdefgh"
    );
    assert_eq!(
        fs::read_to_string(directory.path().join("created.txt")).unwrap(),
        "after upload"
    );
}
