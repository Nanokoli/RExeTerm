//! Tool parameter decoding and execution deadlines. No socket or node state.
use crate::{
    context::ToolContext,
    protocol::{is_soft_timeout_method, tool_output, ToolResult},
    tools::{
        exbash::{exbash, ExbashOptions},
        file_action::{file_action, FileActionOptions},
        fs::{glob_paths, read_path, stat_path, GlobOptions, ReadOptions, StatOptions},
        rg::{rg_search, RgOptions},
    },
};
use anyhow::Result;
use serde_json::{Number, Value};
use tokio::time::{timeout, Duration};
const DEFAULT_TOOL_TIMEOUT_MS: u64 = 5_000;
const MAX_TOOL_TIMEOUT_MS: u64 = 600_000;
pub(super) async fn dispatch_with_timeout(
    method: &str,
    params: Value,
    ctx: &ToolContext,
    timeout_ms: u64,
) -> Result<ToolResult> {
    if is_exbash_method(method) || is_soft_timeout_method(method) {
        return dispatch_tool(method, params, ctx).await;
    }
    match timeout(
        Duration::from_millis(timeout_ms),
        dispatch_tool(method, params, ctx),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!(
            "tool {method} timed out after {timeout_ms}ms"
        )),
    }
}
pub(super) fn effective_tool_timeout_ms(requested: Option<u64>) -> u64 {
    requested
        .unwrap_or(DEFAULT_TOOL_TIMEOUT_MS)
        .min(MAX_TOOL_TIMEOUT_MS)
}

fn is_exbash_method(method: &str) -> bool {
    method == "exbash"
}

pub(super) fn apply_soft_timeout_param(
    method: &str,
    mut params: Value,
    requested: Option<u64>,
) -> Value {
    if !is_soft_timeout_method(method) {
        return params;
    }
    let Some(timeout_ms) = requested else {
        return params;
    };
    if let Some(object) = params.as_object_mut() {
        object
            .entry("timeout")
            .or_insert_with(|| Value::Number(Number::from(timeout_ms)));
    }
    params
}
pub async fn dispatch_tool(method: &str, params: Value, ctx: &ToolContext) -> Result<ToolResult> {
    match method {
        "exbash" => exbash(serde_json::from_value::<ExbashOptions>(params)?, ctx).await,
        "read" => read_path(serde_json::from_value::<ReadOptions>(params)?, ctx),
        "stat" => stat_path(serde_json::from_value::<StatOptions>(params)?, ctx),
        "FileAction" => {
            file_action(serde_json::from_value::<FileActionOptions>(params)?, ctx).await
        }
        "glob" => glob_paths(serde_json::from_value::<GlobOptions>(params)?, ctx),
        "rg" => {
            let output = rg_search(serde_json::from_value::<RgOptions>(params)?).await?;
            Ok(ToolResult {
                metadata: serde_json::json!({
                    "matches": output.matches,
                    "filesWalked": output.files_walked,
                    "code": output.code,
                    "timedOut": output.timed_out
                }),
                output: tool_output(output.stdout),
            })
        }
        _ => Err(anyhow::anyhow!("unknown method: {method}")),
    }
}
