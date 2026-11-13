#![cfg(feature = "server")]
use rexeterm::{
    appcore::{AppCore, AppCoreOptions},
    client::ConnectExecutorOptions,
    protocol::ExecutorRequest,
    server::{start_executor, Executor, ShellManager},
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
fn endpoint(url: String) -> ConnectExecutorOptions {
    ConnectExecutorOptions {
        authentication: None,
        tls: None,
        username: Some("alice".into()),
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
async fn call(core: &AppCore, params: Value) -> Value {
    let response = core
        .handle(ExecutorRequest {
            id: json!(1),
            method: "exbash".into(),
            params: params.clone(),
            executor: None,
            directory: None,
            tool_timeout_ms: None,
        })
        .await;
    assert!(response.ok, "params={params} response={response:?}");
    response.result.unwrap()["metadata"].clone()
}
#[tokio::test]
async fn a_new_core_with_the_same_private_identity_store_recovers_server_task_index() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("preferences.json");
    let url = start_executor(
        "tcp://127.0.0.1:0",
        Executor::local("recovery"),
        ShellManager::default_shell(80, 24),
    )
    .await
    .unwrap();
    let core = AppCore::new_with_options(AppCoreOptions {
        client_settings_path: Some(path.clone()),
        ..Default::default()
    })
    .await
    .unwrap();
    core.connect_to_executor(endpoint(url.clone()))
        .await
        .unwrap();
    let command = if cfg!(windows) {
        "powershell.exe -NoProfile -Command 'Start-Sleep -Seconds 30'"
    } else {
        "sleep 30"
    };
    let task = call(
        &core,
        json!({"mode":"run","command":command,"read_timeout":0}),
    )
    .await;
    let id = task["asyncID"].clone();
    assert!(id.is_string());
    // No explicit shutdown: simulate loss of the old client process during grace.
    drop(core);
    let restored = AppCore::new_with_options(AppCoreOptions {
        client_settings_path: Some(path),
        ..Default::default()
    })
    .await
    .unwrap();
    restored
        .connect_to_executor(endpoint(url.clone()))
        .await
        .unwrap();
    let list = call(&restored, json!({"mode":"list"})).await;
    assert_eq!(list["runs"][0]["asyncID"], id);
    let other = AppCore::default();
    other.connect_to_executor(endpoint(url)).await.unwrap();
    assert_eq!(
        call(&other, json!({"mode":"list"})).await["runs"],
        json!([])
    );
    let identity_file =
        rexeterm::appcore::config::executor_directory(root.path(), "node").join("identities.json");
    assert!(identity_file.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(identity_file)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    call(&restored, json!({"mode":"remove","asyncID":id})).await;
    restored.shutdown().await;
    other.shutdown().await;
}
#[cfg(feature = "mcp-http")]
#[tokio::test]
async fn mcp_transparently_passes_task_file_and_forward_titles_and_descriptions() {
    use rexeterm::app::{AppHost, McpHttpOptions};
    use rexeterm::daemon::DaemonOptions;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        time::{timeout, Duration},
    };
    #[path = "support/mcp_http.rs"]
    mod wire;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let target = root.path().join("target");
    std::fs::write(&source, b"named transfer").unwrap();
    let url = start_executor(
        "tcp://127.0.0.1:0",
        Executor::local("mcp-labels"),
        ShellManager::default_shell(80, 24),
    )
    .await
    .unwrap();
    let app = AppHost::new(DaemonOptions {
        local_executor: false,
        ..Default::default()
    })
    .await
    .unwrap();
    app.core()
        .connect_to_executor(endpoint(url.clone()))
        .await
        .unwrap();
    let address = app
        .enable_mcp_http(McpHttpOptions::default())
        .await
        .unwrap();
    let session = wire::init(address).await;
    let command = if cfg!(windows) {
        "powershell.exe -NoProfile -Command 'Start-Sleep -Seconds 30'"
    } else {
        "sleep 30"
    };
    for io in ["stdio", "pty"] {
        let task = wire::tool(address, &session, "exbash", json!({"mode":"run","io":io,"command":command,"read_timeout":0,"title":"任务标题","description":"任务描述"})).await;
        assert_eq!(task["metadata"]["title"], "任务标题", "{task}");
        assert_eq!(task["metadata"]["description"], "任务描述");
        let id = &task["metadata"]["asyncID"];
        let inventory = wire::tool(address, &session, "client_resources", json!({})).await;
        assert!(
            inventory["metadata"]["programs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["programId"] == *id
                    && p["title"] == "任务标题"
                    && p["description"] == "任务描述"),
            "{inventory}"
        );
        let stopped = wire::tool(
            address,
            &session,
            "exbash",
            json!({"mode":"stop","asyncID":id}),
        )
        .await;
        assert_eq!(stopped["metadata"]["title"], "任务标题", "{stopped}");
        assert_eq!(stopped["metadata"]["description"], "任务描述");
    }
    let upload=wire::tool(address,&session,"file_transfer",json!({"mode":"upload","localPath":source,"targetPath":target,"title":"文件标题","description":"文件描述"})).await;
    assert!(upload["metadata"]["bytes"].is_number(), "{upload}");
    assert_eq!(std::fs::read(target).unwrap(), b"named transfer");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let result=wire::tool(address,&session,"forward",json!({"mode":"start","target":{"kind":"tcp","address":listener.local_addr().unwrap().to_string()},"title":"数据库","description":"测试转发"})).await;
    assert_eq!(result["metadata"]["title"], "数据库");
    assert_eq!(result["metadata"]["description"], "测试转发");
    app.core().connect_to_executor(endpoint(url)).await.unwrap();
    let mut local = TcpStream::connect(
        result["metadata"]["listen"]
            .as_str()
            .unwrap()
            .strip_prefix("tcp://")
            .unwrap(),
    )
    .await
    .unwrap();
    local.write_all(b"x").await.unwrap();
    let (mut remote, _) = listener.accept().await.unwrap();
    let mut b = [0];
    remote.read_exact(&mut b).await.unwrap();
    let inventory = wire::tool(address, &session, "client_resources", json!({})).await;
    assert!(inventory["metadata"]["channels"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["metadata"]["title"] == "数据库" && c["metadata"]["description"] == "测试转发"));
    let (_, _, invalid)=wire::http(address,"POST",Some(&session),"",&json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"forward","arguments":{"mode":"start","target":{"kind":"tcp","address":"127.0.0.1:1"},"title":"中".repeat(41)}}}).to_string()).await;
    assert_eq!(invalid["result"]["isError"], true);
    assert!(invalid.to_string().contains("40"), "{invalid}");
    app.shutdown().await.unwrap();
    assert_eq!(
        timeout(Duration::from_secs(2), local.read(&mut b))
            .await
            .unwrap()
            .unwrap(),
        0
    );
}
