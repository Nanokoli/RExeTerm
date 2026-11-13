//! MCP messages and tool schemas shared by independent transport adapters.
use rexeterm_appcore::{
    protocol::{ExecutorRequest, ExecutorResponse},
    AppCore as Client,
};
use serde_json::{json, Map, Value};
use std::path::PathBuf;
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

pub async fn handle_mcp_message(client: &Client, message: Value) -> Option<Value> {
    if let Err(response) = validate_message(&message) {
        return Some(response);
    }
    let id = message.get("id").cloned()?;
    let method = message.get("method").and_then(Value::as_str)?;
    match method {
        "initialize" => {
            let requested = message
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str);
            let version = match requested {
                Some("2024-11-05") => "2024-11-05",
                Some("2025-03-26") => "2025-03-26",
                _ => MCP_PROTOCOL_VERSION,
            };
            Some(success_response(id, initialize_result(version)))
        }
        "ping" => Some(success_response(id, json!({}))),
        "tools/list" => Some(success_response(id, json!({ "tools": tools() }))),
        "tools/call" => {
            if !message
                .pointer("/params/name")
                .is_some_and(Value::is_string)
                || message
                    .pointer("/params/arguments")
                    .is_some_and(|v| !v.is_object())
            {
                return Some(error_response(
                    id,
                    -32602,
                    "tools/call requires name and object arguments",
                ));
            }
            let result = call_tool(client, &id, message).await;
            Some(success_response(id, result))
        }
        _ => Some(error_response(
            id,
            -32601,
            format!("method not found: {method}"),
        )),
    }
}

pub fn validate_message(message: &Value) -> std::result::Result<(), Value> {
    let id = message.get("id");
    let valid_id = |v: &Value| v.is_string() || v.is_i64() || v.is_u64();
    let invalid = || error_response(Value::Null, -32600, "invalid JSON-RPC 2.0 message");
    if !message.is_object() || message["jsonrpc"] != "2.0" || id.is_some_and(|v| !valid_id(v)) {
        return Err(invalid());
    }
    if let Some(method) = message.get("method") {
        if !method.is_string() || message.get("result").is_some() || message.get("error").is_some()
        {
            return Err(invalid());
        }
        if message.get("params").is_some_and(|v| !v.is_object()) {
            return Err(error_response(
                id.cloned().unwrap_or(Value::Null),
                -32602,
                "params must be an object",
            ));
        }
    } else if id.is_none()
        || message.get("result").is_some() == message.get("error").is_some()
        || message
            .get("error")
            .is_some_and(|e| !e["code"].is_i64() || !e["message"].is_string())
    {
        return Err(invalid());
    }
    Ok(())
}

async fn call_tool(client: &Client, id: &Value, message: Value) -> Value {
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return tool_error("tools/call params.name is required");
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let request = request_from_tool_call(id.clone(), name, arguments);
    response_to_tool_result(client.handle(request).await)
}

fn request_from_tool_call(id: Value, name: &str, arguments: Value) -> ExecutorRequest {
    let mut params = match arguments {
        Value::Object(map) => map,
        _ => Map::new(),
    };

    let executor = take_string(&mut params, "targetExecutor");
    let tool_timeout_ms = take_u64(&mut params, "toolTimeoutMs");
    let directory = take_path(&mut params, "directory");

    ExecutorRequest {
        id,
        method: name.to_string(),
        params: Value::Object(params),
        directory,
        executor,
        tool_timeout_ms,
    }
}

fn take_string(params: &mut Map<String, Value>, key: &str) -> Option<String> {
    params
        .remove(key)
        .and_then(|value| value.as_str().map(str::to_string))
}

fn take_path(params: &mut Map<String, Value>, key: &str) -> Option<PathBuf> {
    take_string(params, key).map(PathBuf::from)
}

fn take_u64(params: &mut Map<String, Value>, key: &str) -> Option<u64> {
    params.remove(key).and_then(|value| value.as_u64())
}

fn response_to_tool_result(response: ExecutorResponse) -> Value {
    if !response.ok {
        return tool_error(
            response
                .error
                .unwrap_or_else(|| "tool call failed".to_string()),
        );
    }

    let result = response.result.unwrap_or(Value::Null);
    let output = result.get("output").unwrap_or(&Value::Null);
    let text = output
        .as_str()
        .or_else(|| output.get("text").and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| {
            serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string())
        });

    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": result,
        "isError": false,
    })
}

fn tool_error(message: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": message.into() }],
        "isError": true,
    })
}

fn initialize_result(version: &str) -> Value {
    json!({
        "protocolVersion": version,
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "serverInfo": {
            "name": "rexeterm-mcp",
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

fn success_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn error_response(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message.into() }
    })
}

fn tools() -> Vec<Value> {
    vec![
        executor_tool(
            "read",
            "Read a file or directory",
            schema(
                &["filePath"],
                &[
                    prop("filePath", "string"),
                    prop("mode", "string"),
                    prop("offset", "number"),
                    prop("limit", "number"),
                    prop("hashCheckMode", "boolean"),
                ],
            ),
            true,
        ),
        executor_tool(
            "glob",
            "Find files by glob pattern",
            schema(
                &["pattern"],
                &[prop("pattern", "string"), prop("path", "string")],
            ),
            true,
        ),
        executor_tool(
            "stat",
            "Return a FileStamp for a file path",
            schema(&["filePath"], &[prop("filePath", "string")]),
            true,
        ),
        executor_tool(
            "FileAction",
            "Create, delete, rename, or patch a single file",
            schema(
                &["mode", "filePath"],
                &[
                    prop("mode", "string"),
                    prop("filePath", "string"),
                    prop("newFilePath", "string"),
                    prop("patchText", "string"),
                    prop("content", "string"),
                    prop("patchMode", "string"),
                    prop("hashCheckMode", "boolean"),
                    prop("hashCode", "string"),
                ],
            ),
            true,
        ),
        executor_tool(
            "set_default_shell",
            "Save a client-owned shell preference for targetExecutor and optional exact directory; no remote configuration is changed",
            schema(&["shell"], &[prop("shell", "string")]),
            true,
        ),
        executor_tool(
            "list_shells",
            "List client-configured shell profiles for targetExecutor and optional exact directory; does not probe installed programs",
            schema(&[], &[]),
            true,
        ),
        executor_tool(
            "request_reload",
            "Reload the local client settings file; targetExecutor and directory select the returned effective view",
            schema(&[], &[]),
            true,
        ),
        executor_tool(
            "file_transfer",
            "Upload or download file bytes through the selected Executor link, with size and SHA-256 validation",
            schema(
                &["mode", "localPath", "targetPath"],
                &[
                    prop("mode", "string"),
                    prop("localPath", "string"),
                    prop("targetPath", "string"),
                    prop("overwrite", "boolean"),
                    prop("offset", "integer"),
                    prop("sha256", "string"),
                    json!({"name":"title","type":"string","maxLength":40,"default":"未命名"}),
                    json!({"name":"description","type":"string","maxLength":200,"default":""}),
                ],
            ),
            false,
        ),
        executor_tool(
            "exbash",
            "Background tasks with stdio or PTY I/O. Prefer shell mode for ordinary commands because it uses the configured shell and normal program lookup. Use run only when the first token is an explicit executable program; run splits command into executable + args without shell parsing.",
            schema(
                &["mode"],
                &[
                    prop_desc(
                        "mode",
                        "string",
                        "Operation selector. Prefer shell for normal commands; run requires an explicit executable program as the first token and does not use shell syntax.",
                    ),
                    prop_desc(
                        "command",
                        "string",
                        "Command input must be at most 4KB. In shell mode it is sent to the configured shell; in run mode it is split into executable + argv without shell parsing.",
                    ),
                    json!({"name":"io","type":"string","enum":["stdio","pty"],"default":"pty","description":"Task I/O backend; stdio uses pipes and separate stdout/stderr without terminal emulation"}),
                    prop("stdin", "string"),
                    prop("keepStdin", "boolean"),
                    prop("closeStdin", "boolean"),
                    prop("shell", "string"),
                    json!({"name":"title","type":"string","maxLength":40,"default":"未命名"}),
                    json!({"name":"description","type":"string","maxLength":200,"default":""}),
                    prop_desc(
                        "timeout",
                        "number",
                        "Total lifetime timeout in ms for run/shell. For attach, timeout is accepted as a legacy read wait when read_timeout is absent, capped at 10000 ms.",
                    ),
                    prop_desc(
                        "read_timeout",
                        "number",
                        "Read wait in ms. Attach uses this field; if timeout is also present, read_timeout wins.",
                    ),
                    prop("asyncID", "string"),
                    prop_desc(
                        "text",
                        "string",
                        "Text input for attach mode, at most 4KB after escape parsing. Interactive programs may require control bytes; use \\n for Enter/newline and \\u0003 for Ctrl-C/control bytes.",
                    ),
                    prop("filePath", "string"),
                    prop("showRawPretty", "boolean"),
                ],
            ),
            true,
        ),
        executor_tool(
            "rg",
            "Ripgrep-style search",
            schema(
                &["pattern"],
                &[
                    prop("pattern", "string"),
                    prop("root", "string"),
                    prop("path", "string"),
                    prop("globs", "array"),
                    prop("case_sensitive", "boolean"),
                    prop("max_count", "number"),
                    prop_desc(
                        "timeout",
                        "number",
                        "Soft search timeout in milliseconds. Default 10000. Use -1 for no timeout.",
                    ),
                ],
            ),
            false,
        ),
        executor_tool(
            "forward",
            "Start, list or stop local TCP port forwarding through an Executor; each TCP connection gets an independent ordered stream",
            schema(&["mode"], &[
                json!({"name":"title","type":"string","maxLength":40,"default":"未命名"}),
                json!({"name":"description","type":"string","maxLength":200,"default":""}),
                prop("mode","string"), prop("listen","string"), prop("forwardID","string"), prop("maxConnections","integer"),
                json!({"name":"target","type":"object","properties":{"kind":{"type":"string"},"address":{"type":"string"}},"required":["kind","address"],"additionalProperties":false}),
            ]),
            false,
        ),
        executor_tool("client_resources", "List this persistent client's server-maintained tasks and active PTY/file/forward channels; never includes other clients", schema(&[], &[]), true),
        tool(
            "list_executor",
            "List connected executors",
            schema(&[], &[]),
        ),
        tool(
            "connect_to_executor",
            "Connect an Executor over TLS/WSS with password, certificate or CA authentication, or a configured trusted transport",
            schema(
                &["id", "url"],
                &[
                    prop("id", "string"),
                    prop("url", "string"),
                    json!({"name":"username","type":"string"}),
                    json!({"name":"authentication","type":"object","oneOf":[{"properties":{"method":{"const":"password"},"password":{"type":"string","writeOnly":true}},"required":["method","password"],"additionalProperties":false},{"properties":{"method":{"enum":["certificate","ca"]}},"required":["method"],"additionalProperties":false}]}),
                    json!({"name":"tls","type":"object","properties":{"caFile":{"type":"string"},"certificateFile":{"type":"string"},"privateKeyFile":{"type":"string"},"serverName":{"type":"string"}},"additionalProperties":false}),
                    json!({"name":"clientName","type":"string"}),
                    json!({"name":"links","type":"array","items":{"type":"string"}}),
                    json!({"name":"channels","type":"object","properties":{"control":{"type":"integer","minimum":1,"maximum":1024},"query":{"type":"integer","minimum":1,"maximum":1024},"pty":{"type":"integer","minimum":1,"maximum":1024},"file":{"type":"integer","minimum":1,"maximum":1024},"forward":{"type":"integer","minimum":1,"maximum":1024}},"additionalProperties":false}),
                    prop("system", "string"),
                    prop("device", "string"),
                    prop("labels", "object"),
                ],
            ),
        ),
        tool(
            "set_default_executor",
            "Set the default Executor",
            schema(&["id"], &[prop("id", "string")]),
        ),
    ]
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
    })
}

fn executor_tool(name: &str, description: &str, input_schema: Value, directory: bool) -> Value {
    tool(
        name,
        description,
        add_executor_routing(input_schema, directory),
    )
}

fn schema(required: &[&str], props: &[Value]) -> Value {
    let mut properties = Map::new();
    for prop in props {
        if let Some(name) = prop.get("name").and_then(Value::as_str) {
            let mut prop = prop.clone();
            prop.as_object_mut().unwrap().remove("name");
            properties.insert(name.to_string(), prop);
        }
    }
    json!({ "type": "object", "properties": properties, "required": required })
}

fn prop(name: &str, kind: &str) -> Value {
    json!({ "name": name, "type": kind })
}

fn prop_desc(name: &str, kind: &str, description: &str) -> Value {
    json!({ "name": name, "type": kind, "description": description })
}

fn add_executor_routing(mut schema: Value, directory: bool) -> Value {
    let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return schema;
    };
    properties.insert("targetExecutor".to_string(), json!({ "type": "string" }));
    if directory {
        properties.insert("directory".to_string(), json!({ "type": "string" }));
    }
    schema
}
