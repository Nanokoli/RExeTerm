use super::{ClientSettings, ClientSettingsStore};
use crate::{protocol::ExecutorRequest, AppCore as Client};
use serde_json::{json, Value};
use std::fs;
use tempfile::tempdir;

fn settings(value: Value) -> ClientSettings {
    serde_json::from_value(value).unwrap()
}
fn request(
    tool: &str,
    executor: Option<&str>,
    directory: Option<&str>,
    params: Value,
) -> ExecutorRequest {
    ExecutorRequest {
        id: json!(tool),
        method: tool.into(),
        params,
        executor: executor.map(str::to_string),
        directory: directory.map(Into::into),
        tool_timeout_ms: None,
    }
}

#[test]
fn preferences_layer_by_executor_and_exact_remote_directory() {
    let store = ClientSettingsStore::memory(settings(json!({
        "shells":{"default":"python"},
        "executors": {
            "a": {"shells":{"default":"node"},"directories":{
                "/remote/project":{"default":"powershell","profiles":{"node":{"candidates":["/remote/node"],"commandArgs":["-e","{command}"]}}},
                "C:\\work":{"default":"bash"}
            }},
            "b":{"shells":{"default":"bash"}}
        }
    }))).unwrap();
    assert_eq!(store.effective("other", None).default, "python");
    assert_eq!(store.effective("a", None).default, "node");
    assert_eq!(
        store.effective("a", Some("/remote/project")).default,
        "powershell"
    );
    assert_eq!(
        store.effective("a", Some("/remote/project/subdir")).default,
        "node"
    );
    assert_eq!(
        store.effective("b", Some("/remote/project")).default,
        "bash"
    );
    assert_eq!(store.effective("a", Some("C:\\work")).default, "bash");
    assert_eq!(
        store.effective("a", Some("/remote/project")).profiles["node"].candidates,
        ["/remote/node"]
    );
    assert_eq!(
        store.effective("a", None).profiles["node"].candidates,
        ["node", "nodejs"]
    );
}

#[tokio::test]
async fn config_tools_work_offline_persist_locally_and_share_only_cloned_stores() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("client.json");
    let store = ClientSettingsStore::load(path.clone()).unwrap();
    let client = Client::default().with_settings_store(store.clone());
    let clone = client.clone();
    let separate = Client::default();
    let set = client
        .handle(request(
            "set_default_shell",
            Some("offline"),
            Some("/not/a/local/path"),
            json!({"shell":"python"}),
        ))
        .await;
    assert!(set.ok, "{:?}", set.error);
    assert_eq!(set.executor.as_deref(), Some("client"));
    assert_eq!(set.result.unwrap()["metadata"]["scope"], "client");
    let list = request(
        "list_shells",
        Some("offline"),
        Some("/not/a/local/path"),
        json!({}),
    );
    assert_eq!(
        clone.handle(list.clone()).await.result.unwrap()["metadata"]["default"],
        "python"
    );
    assert_eq!(
        separate.handle(list).await.result.unwrap()["metadata"]["default"],
        "auto"
    );
    let fresh = ClientSettingsStore::load(path).unwrap();
    assert_eq!(
        fresh
            .effective("offline", Some("/not/a/local/path"))
            .default,
        "python"
    );
    assert_eq!(fresh.effective("offline", None).default, "auto");
    assert!(client.default_executor().await.is_empty());
}

#[tokio::test]
async fn reload_is_local_explicit_and_keeps_last_good_snapshot() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("client.json");
    fs::write(&path, r#"{"shells":{"default":"python"}}"#).unwrap();
    let store = ClientSettingsStore::load(path.clone()).unwrap();
    let client = Client::default().with_settings_store(store.clone());
    let clone = client.clone();
    for invalid in ["{ broken", r#"{"shells":{"default":"missing"}}"#] {
        fs::write(&path, invalid).unwrap();
        assert_eq!(store.effective("a", None).default, "python");
        let reload = clone
            .handle(request("request_reload", Some("a"), None, json!({})))
            .await;
        assert!(!reload.ok);
        assert_eq!(store.effective("a", None).default, "python");
        let list = client
            .handle(request("list_shells", Some("a"), None, json!({})))
            .await
            .result
            .unwrap();
        assert!(list["metadata"]["settingsError"].is_string());
    }
    fs::write(&path, r#"{"shells":{"default":"node"}}"#).unwrap();
    let reload = client
        .handle(request("request_reload", Some("a"), None, json!({})))
        .await;
    assert!(reload.ok, "{:?}", reload.error);
    assert_eq!(reload.result.unwrap()["metadata"]["reloaded"], true);
    assert_eq!(store.effective("a", None).default, "node");
    assert!(store.reload_error().is_none());
}

#[test]
fn failed_save_and_invalid_profile_do_not_change_snapshot() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("client.json");
    let store = ClientSettingsStore::load(path.clone()).unwrap();
    assert!(store.set_default_shell("a", None, "missing").is_err());
    assert!(!path.exists());
    fs::create_dir(&path).unwrap();
    assert!(store.set_default_shell("a", None, "python").is_err());
    assert_eq!(store.effective("a", None).default, "auto");
    assert!(store.settings().executors.is_empty());
}

#[test]
fn shell_spec_uses_remote_os_and_does_not_resolve_remote_candidates_locally() {
    let store = ClientSettingsStore::memory(settings(json!({"shells":{"profiles":{
        "remote":{"candidates":["/only/on/executor/python"],"commandArgs":["-c","prefix; {command}"]},
        "append":{"candidates":["remote-shell"],"commandArgs":["-c"]}
    }}}))).unwrap();
    assert_eq!(
        store
            .shell_command("a", None, None, Some("windows"), "echo hi")
            .unwrap()
            .profile,
        "powershell"
    );
    assert_eq!(
        store
            .shell_command("a", None, None, Some("linux"), "echo hi")
            .unwrap()
            .profile,
        "bash"
    );
    assert!(store
        .shell_command("a", None, None, None, "echo hi")
        .is_err());
    let command = store
        .shell_command("a", None, Some(" remote "), None, "print(1)")
        .unwrap();
    assert_eq!(command.candidates, ["/only/on/executor/python"]);
    assert_eq!(command.args, ["-c", "prefix; print(1)"]);
    assert_eq!(
        store
            .shell_command("a", None, Some("append"), None, "hello")
            .unwrap()
            .args,
        ["-c", "hello"]
    );
}

#[tokio::test]
async fn no_selected_executor_uses_global_defaults_and_directory_requires_an_id() {
    let client = Client::default();
    let set = client
        .handle(request(
            "set_default_shell",
            None,
            None,
            json!({"shell":"python"}),
        ))
        .await;
    assert!(set.ok, "{:?}", set.error);
    let listed = client
        .handle(request(
            "list_shells",
            Some("future-executor"),
            None,
            json!({}),
        ))
        .await;
    assert_eq!(listed.result.unwrap()["metadata"]["default"], "python");
    assert!(
        !client
            .handle(request(
                "set_default_shell",
                None,
                Some("/remote"),
                json!({"shell":"node"})
            ))
            .await
            .ok
    );
}
#[test]
fn legacy_preferences_migrate_to_executor_directories_without_losing_overrides() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("preferences.json");
    fs::write(&path,serde_json::to_vec(&json!({"shells":{"default":"bash"},"executors":{"node":{"shells":{"default":"python"},"directories":{"/work":{"default":"sh"}}}}})).unwrap()).unwrap();
    let store = ClientSettingsStore::load(path.clone()).unwrap();
    assert_eq!(store.effective("node", None).default, "python");
    assert_eq!(store.effective("node", Some("/work")).default, "sh");
    let root: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(root["executors"], json!({}));
    let scoped =
        crate::config::executor_directory(directory.path(), "node").join("preferences.json");
    assert!(scoped.is_file());
    store.set_default_shell("Node", None, "node").unwrap();
    let restored = ClientSettingsStore::load(path).unwrap();
    assert_eq!(restored.effective("node", None).default, "python");
    assert_eq!(restored.effective("Node", None).default, "node");
}
