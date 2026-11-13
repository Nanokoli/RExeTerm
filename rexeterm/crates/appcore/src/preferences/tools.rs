use super::ClientSettingsStore;
use crate::protocol::{tool_output, ToolResult};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
impl ClientSettingsStore {
    pub(crate) fn handle_tool(
        &self,
        tool: &str,
        params: Value,
        executor: &str,
        directory: Option<&str>,
    ) -> Result<ToolResult> {
        if executor.is_empty() && directory.is_some() {
            return Err(anyhow!(
                "an executor ID is required for directory shell settings"
            ));
        }
        match tool {
            "set_default_shell" => {
                let shell = params
                    .get("shell")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("shell is required"))?;
                self.set_default_shell(executor, directory, shell)?;
            }
            "request_reload" => self.reload()?,
            "list_shells" => {}
            _ => return Err(anyhow!("unknown client settings tool: {tool}")),
        }
        let shells = self.effective(executor, directory);
        let mut metadata = json!({"executor":executor, "directory":directory, "scope":"client", "default":shells.default,
            "interactive":shells.interactive, "profiles":shells.profiles, "settingsPath":self.path(), "settingsError":self.reload_error()});
        if tool == "set_default_shell" {
            metadata["defaultShell"] = metadata["default"].clone();
        }
        if tool == "request_reload" {
            metadata["reloaded"] = json!(true);
        }
        Ok(ToolResult {
            output: tool_output(serde_json::to_string_pretty(&metadata)?),
            metadata,
        })
    }
}
