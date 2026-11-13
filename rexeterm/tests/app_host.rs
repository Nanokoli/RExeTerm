use rexeterm::{
    app::{AppHost, McpHttpOptions},
    client::management::ManagementClient,
    daemon::DaemonOptions,
    protocol::ExecutorRequest,
    server::{management::start_management_listener, start_executor, Executor, ShellManager},
};
use serde_json::json;
use tokio::net::{TcpListener, TcpStream};

#[path = "support/mcp_http.rs"]
mod wire;
use wire::{http, init, tool};

#[tokio::test]
async fn app_can_switch_http_while_keeping_the_same_core_and_re_session() {
    let executor = Executor::local("app-node");
    let manager = ShellManager::default_shell(80, 24);
    let url = start_executor("tcp://127.0.0.1:0", executor.clone(), manager.clone())
        .await
        .unwrap();
    let admin = start_management_listener("tcp://127.0.0.1:0", executor, manager).unwrap();
    let mut management = ManagementClient::connect(admin.address()).await.unwrap();
    let host = AppHost::new(DaemonOptions {
        local_executor: false,
        ..Default::default()
    })
    .await
    .unwrap();
    assert!(host.mcp_http_address().await.is_none());
    let connection = host
        .core()
        .handle(ExecutorRequest {
            id: json!(1),
            method: "connect_to_executor".into(),
            params: json!({"id":"node","url":url,"username":"app-user"}),
            executor: None,
            directory: None,
            tool_timeout_ms: None,
        })
        .await;
    assert!(connection.ok, "{connection:?}");
    let before = management.snapshot().await.unwrap();
    assert_eq!(before.clients.len(), 1);
    let owner = before.clients[0].owner.clone();

    let address = host
        .enable_mcp_http(McpHttpOptions::default())
        .await
        .unwrap();
    let session = init(address).await;
    assert_eq!(
        tool(address, &session, "list_executor", json!({})).await["metadata"]["default"],
        "node"
    );
    tool(
        address,
        &session,
        "set_default_shell",
        json!({"shell":"python"}),
    )
    .await;
    // A UI/API call reads the same preference changed through HTTP.
    let response = host
        .core()
        .handle(ExecutorRequest {
            id: json!(2),
            method: "list_shells".into(),
            params: json!({}),
            executor: None,
            directory: None,
            tool_timeout_ms: None,
        })
        .await;
    assert_eq!(response.result.unwrap()["metadata"]["default"], "python");
    assert_eq!(http(address, "DELETE", Some(&session), "", "").await.0, 204);
    assert_eq!(management.snapshot().await.unwrap().clients[0].owner, owner);
    let session = init(address).await;
    host.disable_mcp_http().await.unwrap();
    assert!(TcpStream::connect(address).await.is_err());
    assert_eq!(host.core().default_executor().await, "node");

    // Restart on the same address: old protocol sessions are gone, AppCore stays.
    host.enable_mcp_http(McpHttpOptions {
        listen: address,
        ..Default::default()
    })
    .await
    .unwrap();
    let ping = json!({"jsonrpc":"2.0","id":3,"method":"ping"}).to_string();
    assert_eq!(
        http(address, "POST", Some(&session), "", &ping).await.0,
        404
    );
    let session = init(address).await;
    assert_eq!(
        tool(address, &session, "list_shells", json!({})).await["metadata"]["default"],
        "python"
    );
    assert_eq!(management.snapshot().await.unwrap().clients[0].owner, owner);
    host.shutdown().await.unwrap();
    assert!(TcpStream::connect(address).await.is_err());
    assert!(management.snapshot().await.unwrap().clients.is_empty());
    assert!(host
        .enable_mcp_http(McpHttpOptions::default())
        .await
        .is_err());
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn app_http_bind_failure_and_concurrent_switches_leave_one_listener() {
    let host = AppHost::new(DaemonOptions {
        local_executor: false,
        ..Default::default()
    })
    .await
    .unwrap();
    let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert!(host
        .enable_mcp_http(McpHttpOptions {
            listen: occupied.local_addr().unwrap(),
            ..Default::default()
        })
        .await
        .is_err());
    assert!(host.mcp_http_address().await.is_none());
    let (a, b) = tokio::join!(
        host.enable_mcp_http(McpHttpOptions::default()),
        host.enable_mcp_http(McpHttpOptions::default())
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let address = host.mcp_http_address().await.unwrap();
    init(address).await;
    let (closed, shutdown) = tokio::join!(host.disable_mcp_http(), host.shutdown());
    closed.unwrap();
    shutdown.unwrap();
    assert!(TcpStream::connect(address).await.is_err());
    assert!(host.mcp_http_address().await.is_none());
    assert!(host
        .enable_mcp_http(McpHttpOptions::default())
        .await
        .is_err());
}
