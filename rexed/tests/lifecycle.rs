#![cfg(unix)]
use remote_executor::{
    client::Client,
    protocol::ExecutorRequest,
    server::{start_executor, Executor, ShellManager},
};
use serde_json::{json, Value};
use std::time::Duration;
async fn call(client: &Client, params: Value) -> Value {
    let response = client
        .handle(ExecutorRequest {
            id: json!(1),
            method: "exbash".into(),
            params,
            directory: None,
            executor: Some("node".into()),
            tool_timeout_ms: None,
        })
        .await;
    assert!(response.ok, "{response:?}");
    response.result.unwrap()
}
#[tokio::test]
async fn stopped_tasks_release_native_resources_and_only_keep_an_acknowledgeable_final_receipt() {
    let manager = ShellManager::default_shell(80, 24);
    let url = start_executor(
        "tcp://127.0.0.1:0",
        Executor::local("lifecycle"),
        manager.clone(),
    )
    .await
    .unwrap();
    let client = Client::default();
    client
        .connect_to_executor(
            serde_json::from_value(json!({"id":"node","url":url,"username":"alice"})).unwrap(),
        )
        .await
        .unwrap();
    let started=call(&client,json!({"mode":"run","command":"sh -c 'sleep 0.1; printf FINAL-OUTPUT; exit 7'","read_timeout":0,"timeout":600000})).await;
    let id = started["metadata"]["asyncID"].as_str().unwrap();
    let weak = std::sync::Arc::downgrade(&manager.core().session(id).unwrap());
    tokio::time::timeout(Duration::from_secs(5), async {
        while weak.upgrade().is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(manager.list().is_empty());
    let receipt = call(&client, json!({"mode":"status","asyncID":id})).await;
    assert_eq!(receipt["metadata"]["exitCode"], 7);
    let ended_at = receipt["metadata"]["endedAt"].clone();
    assert!(ended_at.is_u64());
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        call(&client, json!({"mode":"status","asyncID":id})).await["metadata"]["endedAt"],
        ended_at
    );
    assert!(manager.core().detail(id).unwrap().env.is_empty());
    call(&client, json!({"mode":"remove","asyncID":id})).await;
    assert!(manager.core().detail(id).is_err());
    client.shutdown().await;
}
#[tokio::test]
async fn command_replies_have_a_memory_budget_without_limiting_raw_output_production() {
    let manager = ShellManager::default_shell(80, 24);
    let url = start_executor("tcp://127.0.0.1:0", Executor::local("capture"), manager)
        .await
        .unwrap();
    let client = Client::default();
    client
        .connect_to_executor(serde_json::from_value(json!({"id":"node","url":url})).unwrap())
        .await
        .unwrap();
    let result = call(
        &client,
        json!({"mode":"run","command":"head -c 2097152 /dev/zero","read_timeout":5000}),
    )
    .await;
    assert_eq!(result["metadata"]["totalOutput"], 2 * 1024 * 1024);
    assert_eq!(result["metadata"]["outputTruncated"], true);
    assert_eq!(
        result["output"]["text"].as_str().unwrap().len(),
        1024 * 1024
    );
    client.shutdown().await;
}
