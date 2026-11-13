use rexeterm::{
    daemon::Daemon,
    mcp::{handle_mcp_message, run_mcp_stdio_io_with_core},
};
use serde_json::{json, Value};
use std::fs;
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn mcp_initialize_and_lists_tools() {
    let client_runtime = Daemon::new().await.unwrap();
    let client = client_runtime.core().clone();

    let initialized = handle_mcp_message(
        &client,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    )
    .await
    .unwrap();
    assert_eq!(initialized["result"]["serverInfo"]["name"], "rexeterm-mcp");

    let listed = handle_mcp_message(
        &client,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    )
    .await
    .unwrap();
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|tool| tool["name"] == "read"));
    assert!(tools.iter().any(|tool| tool["name"] == "stat"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "connect_to_executor"));
    let forward = tools.iter().find(|tool| tool["name"] == "forward").unwrap();
    let properties = &forward["inputSchema"]["properties"];
    assert_eq!(properties["target"]["required"], json!(["kind", "address"]));
    assert!(properties.get("targetExecutor").is_some());
    assert!(properties.get("directory").is_none());
    let connect = tools
        .iter()
        .find(|tool| tool["name"] == "connect_to_executor")
        .unwrap();
    assert_eq!(
        connect["inputSchema"]["properties"]["links"]["items"]["type"],
        "string"
    );
    assert_eq!(
        connect["inputSchema"]["properties"]["channels"]["properties"]["forward"]["maximum"],
        1024
    );
    let read = tools.iter().find(|tool| tool["name"] == "read").unwrap();
    let properties = &read["inputSchema"]["properties"];
    assert!(properties.get("targetExecutor").is_some());
    assert!(properties.get("directory").is_some());
    assert!(properties.get("callTimeoutMs").is_none());
    assert!(properties.get("hashCheckMode").is_some());

    let file_action = tools
        .iter()
        .find(|tool| tool["name"] == "FileAction")
        .unwrap();
    let action_properties = &file_action["inputSchema"]["properties"];
    assert!(action_properties.get("mode").is_some());
    assert!(action_properties.get("filePath").is_some());
    assert!(action_properties.get("newFilePath").is_some());
    assert!(action_properties.get("content").is_some());
    assert!(action_properties.get("patchMode").is_some());
    assert!(action_properties.get("hashCode").is_some());
    assert!(!tools.iter().any(|tool| tool["name"] == "apply_patch"));
    assert!(!tools.iter().any(|tool| tool["name"] == "diffy"));

    let list = tools
        .iter()
        .find(|tool| tool["name"] == "list_executor")
        .unwrap();
    let list_properties = &list["inputSchema"]["properties"];
    assert!(list_properties.get("targetExecutor").is_none());
    assert!(list_properties.get("directory").is_none());

    assert!(!tools.iter().any(|tool| tool["name"] == "exbash_shell"));
    assert!(!tools.iter().any(|tool| tool["name"] == "exbash_list"));
    assert!(!tools.iter().any(|tool| tool["name"] == "exbash_attach"));
    assert!(!tools.iter().any(|tool| tool["name"] == "exbash_stop"));
    assert!(!tools.iter().any(|tool| tool["name"] == "exbash_remove"));

    let exbash = tools.iter().find(|tool| tool["name"] == "exbash").unwrap();
    let exbash_properties = &exbash["inputSchema"]["properties"];
    assert!(exbash_properties.get("mode").is_some());
    assert!(exbash_properties.get("read_timeout").is_some());
    assert!(exbash_properties.get("shell").is_some());
    assert!(exbash_properties.get("asyncID").is_some());
    assert!(exbash_properties.get("text").is_some());
    assert!(exbash_properties.get("showRawPretty").is_some());
    assert!(exbash_properties.get("async_timeout").is_none());
    assert!(exbash_properties.get("targetExecutor").is_some());
    assert!(exbash_properties.get("directory").is_some());
    assert!(tools.iter().any(|tool| tool["name"] == "set_default_shell"));
    assert!(tools.iter().any(|tool| tool["name"] == "list_shells"));
    let request_reload = tools
        .iter()
        .find(|tool| tool["name"] == "request_reload")
        .unwrap();
    let reload_properties = &request_reload["inputSchema"]["properties"];
    assert!(reload_properties.get("targetExecutor").is_some());
    assert!(reload_properties.get("directory").is_some());

    let file_transfer = tools
        .iter()
        .find(|tool| tool["name"] == "file_transfer")
        .unwrap();
    let transfer_properties = &file_transfer["inputSchema"]["properties"];
    assert!(transfer_properties.get("mode").is_some());
    assert!(transfer_properties.get("localPath").is_some());
    assert!(transfer_properties.get("targetPath").is_some());
    assert!(transfer_properties.get("filePath").is_none());
    assert!(transfer_properties.get("targetExecutor").is_some());
    assert!(transfer_properties.get("directory").is_none());
}

#[tokio::test]
async fn mcp_calls_client_tool_over_stdio_shape() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("file.txt"), "hello mcp\n").unwrap();
    let client_runtime = Daemon::new().await.unwrap();
    let client = client_runtime.core().clone();
    let dir_text = dir.path().to_string_lossy().to_string();

    let response = handle_mcp_message(
        &client,
        json!({
            "jsonrpc":"2.0",
            "id":3,
            "method":"tools/call",
            "params":{
                "name":"read",
                "arguments":{
                    "filePath":"file.txt",
                    "directory":dir_text
                }
            }
        }),
    )
    .await
    .unwrap();

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["result"]["isError"], Value::Bool(false));
    assert!(response["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("hello mcp"));
}

#[tokio::test]
async fn mcp_transfers_file_bytes_through_the_executor_link() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("source.bin");
    let destination = directory.path().join("copy.bin");
    fs::write(&source, b"binary MCP data\0").unwrap();
    let runtime = Daemon::new().await.unwrap();
    let response=handle_mcp_message(runtime.core(),json!({"jsonrpc":"2.0","id":30,"method":"tools/call","params":{"name":"file_transfer","arguments":{"mode":"download","localPath":destination,"targetPath":source}}})).await.unwrap();
    assert_eq!(response["result"]["isError"], false);
    let metadata = &response["result"]["structuredContent"]["metadata"];
    assert_eq!(metadata["protocol"], "re.file.v2");
    assert!(metadata.get("url").is_none());
    assert!(metadata.get("headers").is_none());
    assert_eq!(fs::read(destination).unwrap(), fs::read(source).unwrap());
}

#[tokio::test]
async fn mcp_notifications_do_not_return_response() {
    let client_runtime = Daemon::new().await.unwrap();
    let client = client_runtime.core().clone();
    let response = handle_mcp_message(
        &client,
        json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
    )
    .await;
    assert!(response.is_none());
}

#[tokio::test]
async fn mcp_allows_concurrent_exbash_controls() {
    let client_runtime = Daemon::new().await.unwrap();
    let client = client_runtime.core().clone();
    let (mut input_tx, input_rx) = tokio::io::duplex(4096);
    let (output_tx, mut output_rx) = tokio::io::duplex(8192);

    let runner = tokio::spawn(async move {
        run_mcp_stdio_io_with_core(client, BufReader::new(input_rx), output_tx)
            .await
            .unwrap();
    });

    input_tx
        .write_all(
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"exbash","arguments":{"mode":"run","command":"bash -lc 'sleep 0.2; echo first'","read_timeout":1000}}}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"exbash","arguments":{"mode":"run","command":"echo second","read_timeout":1000}}}
"#,
        )
        .await
        .unwrap();
    input_tx.shutdown().await.unwrap();

    let mut output = String::new();
    output_rx.read_to_string(&mut output).await.unwrap();
    runner.await.unwrap();

    let responses = output
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 2);
    assert_eq!(
        responses
            .iter()
            .filter(|response| response["result"]["isError"] == false)
            .count(),
        2
    );
    assert!(!responses
        .iter()
        .any(|response| response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("write operation")));
}

#[tokio::test]
async fn mcp_manages_client_owned_forward_listeners() {
    let runtime = Daemon::new().await.unwrap();
    let call = |args| {
        handle_mcp_message(
            runtime.core(),
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"forward","arguments":args}}),
        )
    };
    let started = call(json!({"mode":"start","target":{"kind":"tcp","address":"127.0.0.1:1"}}))
        .await
        .unwrap();
    assert_eq!(started["result"]["isError"], false);
    let id = &started["result"]["structuredContent"]["metadata"]["forwardID"];
    let listed = call(json!({"mode":"list"})).await.unwrap();
    assert_eq!(
        &listed["result"]["structuredContent"]["metadata"]["forwards"][0]["forwardID"],
        id
    );
    let stopped = call(json!({"mode":"stop","forwardID":id})).await.unwrap();
    assert_eq!(stopped["result"]["isError"], false);
}

#[tokio::test]
async fn mcp_validates_envelopes_and_negotiates_versions() {
    let client = rexeterm::appcore::AppCore::default();
    for message in [
        json!({"id":1,"method":"ping"}),
        json!([]),
        json!({"jsonrpc":"2.0","id":null,"method":"ping"}),
    ] {
        assert_eq!(
            handle_mcp_message(&client, message).await.unwrap()["error"]["code"],
            -32600
        );
    }
    let bad = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_executor","arguments":[]}});
    assert_eq!(
        handle_mcp_message(&client, bad).await.unwrap()["error"]["code"],
        -32602
    );
    assert!(
        handle_mcp_message(&client, json!({"jsonrpc":"2.0","id":1,"result":{}}))
            .await
            .is_none()
    );
    for version in ["2024-11-05", "2025-03-26", "2025-06-18"] {
        let response = handle_mcp_message(&client, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":version}})).await.unwrap();
        assert_eq!(response["result"]["protocolVersion"], version);
    }
}
