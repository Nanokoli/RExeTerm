use rexeterm::appcore::preferences::{ClientSettings, ClientSettingsStore};
use rexeterm::appcore::AppCore as Client;
use rexeterm::{
    client::ConnectExecutorOptions,
    daemon::{Daemon, DaemonOptions},
    protocol::ExecutorRequest,
    server::{start_shared_executor_ws, Executor, SettingsStore, ShellManager},
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs};
use tempfile::tempdir;

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
async fn local_composition_installs_client_preferences() {
    let dir = tempdir().unwrap();
    let client_path = dir.path().join("client.json");
    fs::write(&client_path, r#"{"shells":{"default":"python"}}"#).unwrap();
    let runtime = Daemon::new_with_options(DaemonOptions {
        identity: Default::default(),
        settings_path: Some(dir.path().join("server.json")),
        client_settings_path: Some(client_path),
        local_executor: true,
    })
    .await
    .unwrap();
    let listed = runtime
        .core()
        .handle(request("list_shells", json!({})))
        .await;
    assert!(listed.ok, "{:?}", listed.error);
    assert_eq!(listed.result.unwrap()["metadata"]["default"], "python");
}

#[cfg(unix)]
#[tokio::test]
async fn client_prepares_profile_and_executor_resolves_program_without_workspace_config() {
    use std::os::unix::fs::PermissionsExt;
    let remote = tempdir().unwrap();
    let server_path = remote.path().join("startup.json");
    fs::write(&server_path, r#"{"shells":{"default":"node"}}"#).unwrap();
    let server_bytes = fs::read(&server_path).unwrap();
    // A malformed workspace file must never participate in client-prepared execution.
    fs::write(remote.path().join(".re-setting.json"), "{ invalid").unwrap();
    let program = remote.path().join("remote-shell");
    fs::write(
        &program,
        "#!/bin/sh\nprintf 'remote-profile-marker\\n'\nexec /bin/sh \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    let addr = start_shared_executor_ws(
        "127.0.0.1:0",
        Executor::local("remote")
            .with_settings_store(SettingsStore::load(Some(server_path.clone())).unwrap()),
        ShellManager::default_shell(80, 24),
    )
    .unwrap();
    let directory = remote.path().to_string_lossy().into_owned();
    let client: ClientSettings = serde_json::from_value(json!({
        "executors":{"remote":{"directories":{(directory.clone()):{
            "default":"remote-only", "profiles":{"remote-only":{"candidates":["./missing-shell", "./remote-shell"], "commandArgs":["-c","{command}"]}}
        }}}}
    })).unwrap();
    let store = ClientSettingsStore::memory(client).unwrap();
    let client = Client::default().with_settings_store(store);
    client
        .connect_to_executor(ConnectExecutorOptions {
            authentication: None,
            tls: None,
            username: None,
            client_name: None,
            links: Vec::new(),
            channels: Default::default(),
            id: "remote".into(),
            url: format!("ws://{addr}"),
            system: None,
            device: None,
            labels: BTreeMap::new(),
        })
        .await
        .unwrap();
    let mut run = request(
        "exbash",
        json!({"mode":"shell","command":"echo remote-command","read_timeout":2000}),
    );
    run.directory = Some(remote.path().into());
    let response = client.handle(run).await;
    assert!(response.ok, "{:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["metadata"]["exitCode"], 0);
    assert!(result["output"]["text"]
        .as_str()
        .unwrap()
        .contains("remote-profile-marker"));
    assert!(result["output"]["text"]
        .as_str()
        .unwrap()
        .contains("remote-command"));
    assert_eq!(fs::read(server_path).unwrap(), server_bytes);
}

#[tokio::test]
async fn preferences_are_owned_by_appcore_while_the_re_client_only_calls_re() {
    let runtime = Daemon::new().await.unwrap();
    let core = runtime.core();
    let low_level = core.re_client();
    let direct = low_level
        .handle(request("set_default_shell", json!({"shell":"python"})))
        .await;
    assert!(
        !direct.ok,
        "the RE client must not consume application preference tools"
    );
    let configured = core
        .handle(request("set_default_shell", json!({"shell":"python"})))
        .await;
    assert!(configured.ok, "{:?}", configured.error);
    let listed = core.handle(request("list_shells", json!({}))).await;
    assert_eq!(listed.result.unwrap()["metadata"]["default"], "python");
    runtime.core().shutdown().await;
}
