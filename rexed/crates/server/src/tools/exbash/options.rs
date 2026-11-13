//! Exbash input contract and byte/timeout validation.
use crate::protocol::shell::ShellCommand;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
const INPUT_BYTES_LIMIT: usize = 4096;
const ASYNC_ID_BYTES_LIMIT: usize = 50;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExbashOptions {
    #[serde(default)]
    pub mode: Option<String>,
    /// I/O backend. Existing callers retain PTY by default.
    #[serde(default)]
    pub io: Option<String>,
    #[serde(default)]
    pub stdin: Option<String>,
    #[serde(default, rename = "keepStdin")]
    pub keep_stdin: bool,
    #[serde(default, rename = "closeStdin")]
    pub close_stdin: bool,
    #[serde(skip)]
    pub shell: bool,
    #[serde(default, rename = "shell")]
    pub shell_profile: Option<String>,
    #[serde(default, rename = "shellCommand")]
    pub shell_command: Option<ShellCommand>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub timeout: Option<i64>,
    #[serde(default, rename = "read_timeout")]
    pub read_timeout: Option<u64>,
    /// Terminal width for `io: pty`; omitted keeps the host default.
    #[serde(default)]
    pub cols: Option<u16>,
    /// Terminal height for `io: pty`.
    #[serde(default)]
    pub rows: Option<u16>,
    #[serde(default, rename = "asyncID")]
    pub async_id: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default, rename = "filePath")]
    pub file_path: Option<PathBuf>,
    #[serde(default)]
    pub workdir: Option<PathBuf>,
    #[serde(default, rename = "showRawPretty")]
    pub show_raw_pretty: bool,
}

fn validate_optional_bytes(name: &str, value: Option<&str>, limit: usize) -> Result<()> {
    if let Some(value) = value {
        validate_bytes(name, value, limit)?;
    }
    Ok(())
}

fn validate_bytes(name: &str, value: &str, limit: usize) -> Result<()> {
    let len = value.len();
    if len > limit {
        return Err(anyhow!("{name} exceeds {limit} bytes ({len} bytes)"));
    }
    Ok(())
}

impl ExbashOptions {
    pub(crate) const INPUT_BYTES_LIMIT: usize = INPUT_BYTES_LIMIT;

    pub(crate) fn validate_input_limits(&self) -> Result<()> {
        validate_optional_bytes("stdin", self.stdin.as_deref(), INPUT_BYTES_LIMIT)?;
        validate_optional_bytes("command", self.command.as_deref(), INPUT_BYTES_LIMIT)?;
        self.task_metadata()
            .validate()
            .map_err(|error| anyhow!(error))?;
        validate_optional_bytes("asyncID", self.async_id.as_deref(), ASYNC_ID_BYTES_LIMIT)?;
        validate_optional_bytes("text", self.text.as_deref(), INPUT_BYTES_LIMIT)?;
        validate_optional_bytes("shell", self.shell_profile.as_deref(), INPUT_BYTES_LIMIT)?;
        if let Some(path) = self.file_path_input() {
            let value = path.to_string_lossy();
            validate_bytes("filePath", &value, INPUT_BYTES_LIMIT)?;
        }
        if let Some(path) = self
            .workdir
            .as_ref()
            .filter(|path| !path.as_os_str().is_empty())
        {
            let value = path.to_string_lossy();
            validate_bytes("workdir", &value, INPUT_BYTES_LIMIT)?;
        }
        Ok(())
    }

    pub(crate) fn task_metadata(&self) -> crate::protocol::management::TaskMetadata {
        crate::protocol::management::TaskMetadata {
            title: self.title.clone().unwrap_or_default(),
            description: self.description.clone().unwrap_or_default(),
        }
    }

    pub(crate) fn timeout_ms(&self) -> Result<Option<u64>> {
        match self.timeout {
            None | Some(-1 | 0) => Ok(None),
            Some(timeout) if timeout < -1 => Err(anyhow!("timeout must be -1 or non-negative")),
            Some(timeout) => Ok(Some(timeout as u64)),
        }
    }

    pub(crate) fn text_input(&self) -> Option<&str> {
        self.text.as_deref().filter(|text| !text.is_empty())
    }

    pub(crate) fn file_path_input(&self) -> Option<&PathBuf> {
        self.file_path
            .as_ref()
            .filter(|path| !path.as_os_str().is_empty())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ExbashOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}
