use rexeterm::{
    appcore::AppCore,
    client::management::ManagementClient,
    mcp_http::*,
    server::{management::start_management_listener, start_executor, Executor, ShellManager},
};
use serde_json::json;
use std::time::Duration;

#[path = "support/mcp_http.rs"]
mod wire;
use wire::{http, init, tool};

#[tokio::test]
async fn http_sessions_share_app_state_and_delete_does_not_close_re_connections() {
    let executor = Executor::local("http-node");
    let manager = ShellManager::default_shell(80, 24);
    let url = start_executor("tcp://127.0.0.1:0", executor.clone(), manager.clone())
        .await
        .unwrap();
    let admin = start_management_listener("tcp://127.0.0.1:0", executor, manager).unwrap();
    let mut management = ManagementClient::connect(admin.address()).await.unwrap();
    let core = AppCore::new().await.unwrap();
    let server = start_mcp_http(core.clone(), McpHttpOptions::default())
        .await
        .unwrap();
    let address = server.address();
    let a = init(address).await;
    let b = init(address).await;
    assert_ne!(a, b);
    tool(
        address,
        &a,
        "connect_to_executor",
        json!({"id":"node","url":url,"username":"alice"}),
    )
    .await;
    let list = tool(address, &b, "list_executor", json!({})).await;
    assert_eq!(list["metadata"]["default"], "node");
    tool(address, &a, "set_default_shell", json!({"shell":"python"})).await;
    assert_eq!(
        tool(address, &b, "list_shells", json!({})).await["metadata"]["default"],
        "python"
    );
    let snapshot = management.snapshot().await.unwrap();
    assert_eq!(snapshot.clients.len(), 1);
    let session_id = snapshot.clients[0].owner.session_id.clone();
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let copy = root.path().join("copy");
    std::fs::write(&source, b"HTTP MCP uses RE file data\0").unwrap();
    let result = tool(
        address,
        &a,
        "file_transfer",
        json!({"mode":"download","localPath":copy,"targetPath":source}),
    )
    .await;
    assert_eq!(result["metadata"]["protocol"], "re.file.v2");
    assert_eq!(std::fs::read(copy).unwrap(), std::fs::read(source).unwrap());
    assert_eq!(http(address, "DELETE", Some(&a), "", "").await.0, 204);
    assert_eq!(management.snapshot().await.unwrap().clients.len(), 1);
    assert_eq!(
        tool(address, &b, "list_executor", json!({})).await["metadata"]["default"],
        "node"
    );
    let c = init(address).await;
    assert_eq!(
        tool(address, &c, "list_executor", json!({})).await["metadata"]["default"],
        "node"
    );
    server.shutdown().await.unwrap();
    let snapshot = management.snapshot().await.unwrap();
    assert_eq!(snapshot.clients.len(), 1);
    assert_eq!(snapshot.clients[0].owner.session_id, session_id);
    assert_eq!(core.default_executor().await, "node");
    core.shutdown().await;
    assert!(management.snapshot().await.unwrap().clients.is_empty());
}
#[tokio::test]
async fn http_protocol_errors_notifications_limits_and_expiry() {
    let server = start_mcp_http(
        AppCore::new().await.unwrap(),
        McpHttpOptions {
            max_sessions: 1,
            idle_timeout: Duration::from_millis(100),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let address = server.address();
    assert_eq!(http(address, "GET", None, "", "").await.0, 405);
    assert_eq!(
        http(
            address,
            "POST",
            None,
            "Origin: http://evil.invalid\r\n",
            "{}"
        )
        .await
        .0,
        403
    );
    assert_eq!(http(address, "POST", None, "", "[]").await.0, 400);
    let ping = json!({"jsonrpc":"2.0","id":2,"method":"ping"}).to_string();
    assert_eq!(http(address, "POST", None, "", &ping).await.0, 400);
    assert_eq!(
        http(address, "POST", Some("unknown"), "", &ping).await.0,
        404
    );
    let session = init(address).await;
    let init_body = json!({"jsonrpc":"2.0","id":1,"method":"initialize"}).to_string();
    assert_eq!(http(address, "POST", None, "", &init_body).await.0, 503);
    let notification = json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string();
    assert_eq!(
        http(address, "POST", Some(&session), "", &notification)
            .await
            .0,
        202
    );
    let too_big = " ".repeat(16 * 1024 * 1024 + 1);
    assert_eq!(
        http(address, "POST", Some(&session), "", &too_big).await.0,
        413
    );
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(
        http(address, "POST", Some(&session), "", &ping).await.0,
        404
    );
    init(address).await;
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn delete_cancels_in_flight_work_and_http_applies_backpressure() {
    let server = start_mcp_http(
        AppCore::new().await.unwrap(),
        McpHttpOptions {
            max_in_flight: 1,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let address = server.address();
    let session = init(address).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("tcp://{}", listener.local_addr().unwrap());
    let request = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"connect_to_executor","arguments":{"id":"slow","url":url}}}).to_string();
    let pending_session = session.clone();
    let pending =
        tokio::spawn(
            async move { http(address, "POST", Some(&pending_session), "", &request).await },
        );
    let (_peer, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let ping = json!({"jsonrpc":"2.0","id":3,"method":"ping"}).to_string();
    assert_eq!(
        http(address, "POST", Some(&session), "", &ping).await.0,
        429
    );
    assert_eq!(http(address, "DELETE", Some(&session), "", "").await.0, 204);
    assert_eq!(pending.await.unwrap().0, 410);
    server.shutdown().await.unwrap();
}
