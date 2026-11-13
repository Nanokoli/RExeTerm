use futures_util::{SinkExt, StreamExt};
use rexeterm::{
    appcore::AppCore,
    http::{start_http, Application, Host, HttpOptions, Workspace},
    server::{start_executor, Executor, ShellManager},
};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::{sleep, timeout},
};
const TOKEN: &str = "test-token-012345678901234567890123456789";
async fn request(
    client: &reqwest::Client,
    base: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> reqwest::Response {
    let mut request = client
        .request(method.parse().unwrap(), format!("{base}/api/v1{path}"))
        .bearer_auth(TOKEN);
    if let Some(body) = body {
        request = request.json(&body);
    }
    if let Some(key) = key {
        request = request.header("Idempotency-Key", key);
    }
    request.send().await.unwrap()
}
async fn json_response(response: reqwest::Response) -> Value {
    let status = response.status();
    let body = response.text().await.unwrap();
    assert!(status.is_success(), "{status}: {body}");
    serde_json::from_str(&body).unwrap()
}
async fn transfer_done(client: &reqwest::Client, base: &str, id: &str) -> Value {
    timeout(Duration::from_secs(10), async {
        loop {
            let value = json_response(
                request(client, base, "GET", &format!("/transfers/{id}"), None, None).await,
            )
            .await;
            if value["status"] == "done" {
                return value;
            }
            assert_ne!(value["status"], "error", "{value}");
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}
async fn setup() -> (tempfile::TempDir, Arc<Application>, ShellManager) {
    let root = tempfile::tempdir().unwrap();
    let manager = ShellManager::default_shell(80, 24);
    let url = start_executor(
        "tcp://127.0.0.1:0",
        Executor::local("http-test"),
        manager.clone(),
    )
    .await
    .unwrap();
    let core = AppCore::new_with_options(rexeterm::appcore::AppCoreOptions {
        client_settings_path: Some(root.path().join("preferences.json")),
        ..Default::default()
    })
    .await
    .unwrap();
    let app = Application::new(
        core,
        root.path().join("workspace.json"),
        Workspace {
            version: 1,
            hosts: vec![Host {
                id: "node".into(),
                name: "Test".into(),
                url,
                root: root.path().display().to_string(),
                username: Some("alice".into()),
                connection_profile: None,
                management: None,
            }],
            last_host: None,
        },
    )
    .await
    .unwrap();
    (root, app, manager)
}
#[tokio::test]
async fn rest_streams_and_local_history_share_one_application_identity_and_http_stop_preserves_it()
{
    let (root, app, manager) = setup().await;
    let server = start_http(
        app.clone(),
        HttpOptions {
            token: Some(TOKEN.into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let base = server.target().to_string();
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .get(format!("{base}/api/v1/hosts"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .get(format!("{base}/api/v1/hosts"))
            .bearer_auth(TOKEN)
            .header("Origin", "https://evil.invalid")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    json_response(request(&client, &base, "PUT", "/hosts/node/connection", None, None).await).await;
    assert_eq!(
        request(
            &client,
            &base,
            "POST",
            "/requests",
            Some(json!({"tool":"stat","params":{}})),
            None
        )
        .await
        .status(),
        400
    );
    let initial = app.workspace().await.unwrap();
    let owner = initial["runtime"]["executors"][0]["clientSession"]["clientId"].clone();
    assert!(owner.is_string());
    let command = if cfg!(windows) {
        "Start-Sleep -Milliseconds 200; exit 7"
    } else {
        "sleep 0.2; exit 7"
    };
    let body = json!({"command":command,"cwd":root.path(),"title":"一次性任务","description":"结束后保留在客户端"});
    let (a, b) = tokio::join!(
        request(
            &client,
            &base,
            "POST",
            "/hosts/node/tasks",
            Some(body.clone()),
            Some("once")
        ),
        request(
            &client,
            &base,
            "POST",
            "/hosts/node/tasks",
            Some(body),
            Some("once")
        )
    );
    let a = json_response(a).await;
    let b = json_response(b).await;
    assert_eq!(a["asyncID"], b["asyncID"]);
    assert_eq!(a["title"], "一次性任务");
    assert_eq!(a["description"], "结束后保留在客户端");
    let id = a["asyncID"].as_str().unwrap();
    assert_eq!(
        request(
            &client,
            &base,
            "POST",
            "/hosts/node/tasks",
            Some(json!({"command":"different"})),
            Some("once")
        )
        .await
        .status(),
        409
    );
    timeout(Duration::from_secs(8), async {
        loop {
            let history = app.core().task_history();
            if history["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["asyncID"] == id && t["state"] == "stopped")
            {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let task = json_response(
        request(
            &client,
            &base,
            "GET",
            &format!("/hosts/node/tasks/{id}"),
            None,
            None,
        )
        .await,
    )
    .await;
    assert_eq!(task["exitCode"], 7);
    assert_eq!(task["title"], "一次性任务");
    assert_eq!(task["description"], "结束后保留在客户端");
    timeout(Duration::from_secs(3), async {
        while manager.core().detail(id).is_ok() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    json_response(
        request(
            &client,
            &base,
            "DELETE",
            &format!("/hosts/node/tasks/{id}"),
            None,
            None,
        )
        .await,
    )
    .await;
    let payload: Vec<_> = (0..1024 * 1024 + 17).map(|i| (i % 251) as u8).collect();
    let remote = root.path().join("roundtrip.bin");
    let upload = json_response(
        request(
            &client,
            &base,
            "POST",
            "/hosts/node/transfers",
            Some(json!({"mode":"upload","path":remote,"size":payload.len()})),
            Some("upload-once"),
        )
        .await,
    )
    .await;
    let upload_id = upload["id"].as_str().unwrap();
    json_response(
        client
            .put(format!("{base}/api/v1/transfers/{upload_id}/content"))
            .bearer_auth(TOKEN)
            .body(payload.clone())
            .send()
            .await
            .unwrap(),
    )
    .await;
    let completed = transfer_done(&client, &base, upload_id).await;
    assert!(completed["sha256"].as_str().unwrap().starts_with("sha256:"));
    assert_eq!(std::fs::read(&remote).unwrap(), payload);
    let download = json_response(
        request(
            &client,
            &base,
            "POST",
            "/hosts/node/transfers",
            Some(json!({"mode":"download","path":remote})),
            Some("download-once"),
        )
        .await,
    )
    .await;
    let download_id = download["id"].as_str().unwrap();
    transfer_done(&client, &base, download_id).await;
    let bytes = client
        .get(format!("{base}/api/v1/transfers/{download_id}/content"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(&bytes[..], &payload[..]);
    assert_eq!(
        request(
            &client,
            &base,
            "POST",
            "/hosts/node/transfers",
            Some(json!({"mode":"upload","path":remote,"size":268435457u64})),
            Some("too-large")
        )
        .await
        .status(),
        413
    );
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = sink.local_addr().unwrap();
    let echo = tokio::spawn(async move {
        let (mut socket, _) = sink.accept().await.unwrap();
        let mut buf = [0; 4];
        socket.read_exact(&mut buf).await.unwrap();
        socket.write_all(&buf).await.unwrap();
    });
    let forward=json_response(request(&client,&base,"POST","/hosts/node/forwards",Some(json!({"listen":"127.0.0.1:0","target":{"kind":"tcp","address":address.to_string()},"title":"Test forward","description":"shared instance"})),Some("forward-once")).await).await;
    let mut socket = tokio::net::TcpStream::connect(
        forward["listen"]
            .as_str()
            .unwrap()
            .strip_prefix("tcp://")
            .unwrap(),
    )
    .await
    .unwrap();
    socket.write_all(b"ping").await.unwrap();
    let mut reply = [0; 4];
    socket.read_exact(&mut reply).await.unwrap();
    assert_eq!(&reply, b"ping");
    echo.await.unwrap();
    let forward_id = forward["forwardID"].as_str().unwrap();
    assert_eq!(
        request(
            &client,
            &base,
            "DELETE",
            &format!("/hosts/wrong/forwards/{forward_id}"),
            None,
            None
        )
        .await
        .status(),
        404
    );
    json_response(
        request(
            &client,
            &base,
            "DELETE",
            &format!("/hosts/node/forwards/{forward_id}"),
            None,
            None,
        )
        .await,
    )
    .await;
    let mut events = client
        .get(format!("{base}/api/v1/events"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert!(
        String::from_utf8_lossy(&events.chunk().await.unwrap().unwrap()).contains("event: state")
    );
    timeout(Duration::from_secs(3), server.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        app.workspace().await.unwrap()["runtime"]["executors"][0]["clientSession"]["clientId"],
        owner
    );
    let json = std::fs::read_to_string(root.path().join("preferences.task-history.json")).unwrap();
    assert!(!json.contains(id));
    app.shutdown().await.unwrap();
    app.core().shutdown().await;
}
#[cfg(unix)]
#[tokio::test]
async fn pty_binary_websocket_and_unix_http_ipc_keep_terminal_and_core_lifetimes_separate() {
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
    let (_root, app, _manager) = setup().await;
    app.connect_host("node", false).await.unwrap();
    let server = start_http(
        app.clone(),
        HttpOptions {
            token: Some(TOKEN.into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let base = server.target().to_string();
    let client = reqwest::Client::new();
    let task = json_response(
        request(
            &client,
            &base,
            "POST",
            "/hosts/node/tasks",
            Some(json!({"mode":"run","command":"sh -c 'stty -echo; cat'"})),
            Some("cat-once"),
        )
        .await,
    )
    .await;
    let id = task["asyncID"].as_str().unwrap();
    let mut req = format!(
        "{}/api/v1/hosts/node/tasks/{id}/pty",
        base.replacen("http://", "ws://", 1)
    )
    .into_client_request()
    .unwrap();
    req.headers_mut()
        .insert("Authorization", format!("Bearer {TOKEN}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    timeout(Duration::from_secs(5), async {
        loop {
            if let Message::Text(t) = ws.next().await.unwrap().unwrap() {
                if serde_json::from_str::<Value>(&t).unwrap()["type"] == "meta" {
                    break;
                }
            }
        }
    })
    .await
    .unwrap();
    sleep(Duration::from_millis(100)).await;
    ws.send(Message::Binary(b"raw-pty-check\n".to_vec().into()))
        .await
        .unwrap();
    let mut bytes = Vec::new();
    timeout(Duration::from_secs(5), async {
        while !bytes.ends_with(b"raw-pty-check\r\n") {
            if let Message::Binary(chunk) = ws.next().await.unwrap().unwrap() {
                bytes.extend_from_slice(&chunk);
            }
        }
    })
    .await
    .unwrap();
    ws.close(None).await.unwrap();
    assert!(app
        .rpc(
            Some("node"),
            "exbash",
            json!({"mode":"status","asyncID":id}),
            None
        )
        .await
        .is_ok());
    let socket_dir = tempfile::tempdir().unwrap();
    let socket = socket_dir.path().join("api.sock");
    let ipc = start_http(
        app.clone(),
        HttpOptions {
            unix_socket: Some(socket.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let local = reqwest::Client::builder()
        .unix_socket(socket.as_path())
        .build()
        .unwrap();
    let hosts: Value = local
        .get("http://localhost/api/v1/hosts")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hosts[0]["id"], "node");
    ipc.shutdown().await.unwrap();
    assert!(!socket.exists());
    server.shutdown().await.unwrap();
    app.rpc(
        Some("node"),
        "exbash",
        json!({"mode":"remove","asyncID":id}),
        None,
    )
    .await
    .unwrap();
    app.shutdown().await.unwrap();
    app.core().shutdown().await;
}

#[tokio::test]
async fn cli_host_identity_rejects_misrouted_writes_and_shutdown_belongs_to_owner() {
    let (_root, app, _manager) = setup().await;
    let (shutdown, mut stopped) = tokio::sync::watch::channel(false);
    let id = "1234567890abcdef";
    let ordinary = start_http(
        app.clone(),
        HttpOptions {
            token: Some(TOKEN.into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let rejected = reqwest::Client::new()
        .get(format!("{}/api/v1/info", ordinary.target()))
        .bearer_auth(TOKEN)
        .header("x-rexeterm-service-id", id)
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), 409);
    ordinary.shutdown().await.unwrap();
    let server = start_http(
        app.clone(),
        HttpOptions {
            token: Some(TOKEN.into()),
            service_id: Some(id.into()),
            host_shutdown: Some(shutdown),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let client = reqwest::Client::new();
    let target = server.target();
    let before = app.workspace().await.unwrap();
    let wrong = client
        .post(format!("{target}/api/v1/hosts"))
        .bearer_auth(TOKEN)
        .header("x-rexeterm-service-id", "fedcba0987654321")
        .json(&json!({"id":"wrong-host","name":"Wrong","url":"tcp://127.0.0.1:1","root":"/"}))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 409);
    assert_eq!(app.workspace().await.unwrap()["saved"], before["saved"]);
    let missing = client
        .post(format!("{target}/api/v1/cli-host/stop"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 409);
    assert!(!*stopped.borrow());
    let ok = client
        .post(format!("{target}/api/v1/cli-host/stop"))
        .bearer_auth(TOKEN)
        .header("x-rexeterm-service-id", id)
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    tokio::time::timeout(std::time::Duration::from_secs(2), stopped.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(*stopped.borrow());
    // The adapter only asks its owner to stop. It must not shut down the shared application.
    assert_eq!(app.workspace().await.unwrap()["saved"], before["saved"]);
    server.shutdown().await.unwrap();
    app.shutdown().await.unwrap();
}
