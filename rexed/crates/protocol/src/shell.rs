//! Settings data model and built-in profiles.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
const COMMAND_PLACEHOLDER: &str = "{command}";
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReSettings {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(
        default = "default_disconnect_timeout",
        rename = "clientDisconnectTimeoutSeconds"
    )]
    pub client_disconnect_timeout_seconds: std::num::NonZeroU32,
    #[serde(default)]
    pub shells: ShellSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShellSettings {
    #[serde(default = "auto_string")]
    pub default: String,
    #[serde(default = "auto_string")]
    pub interactive: String,
    #[serde(default)]
    pub profiles: BTreeMap<String, ShellProfile>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShellProfile {
    #[serde(default)]
    pub candidates: Vec<String>,
    #[serde(default, rename = "commandArgs")]
    pub command_args: Vec<String>,
    #[serde(default, rename = "interactiveArgs")]
    pub interactive_args: Vec<String>,
}

impl Default for ReSettings {
    fn default() -> Self {
        Self {
            version: default_version(),
            client_disconnect_timeout_seconds: default_disconnect_timeout(),
            shells: ShellSettings::default(),
        }
    }
}

impl Default for ShellSettings {
    fn default() -> Self {
        Self {
            default: auto_string(),
            interactive: auto_string(),
            profiles: default_profiles(),
        }
    }
}
pub fn default_profiles() -> BTreeMap<String, ShellProfile> {
    BTreeMap::from([
        (
            "sh".to_string(),
            ShellProfile {
                candidates: vec!["sh".to_string(), "/bin/sh".to_string()],
                command_args: vec!["-c".to_string(), COMMAND_PLACEHOLDER.to_string()],
                interactive_args: Vec::new(),
            },
        ),
        (
            "bash".to_string(),
            ShellProfile {
                candidates: vec![
                    ".venv/bin/bash".to_string(),
                    "bash".to_string(),
                    "/bin/bash".to_string(),
                    "sh".to_string(),
                ],
                command_args: vec!["-lc".to_string(), COMMAND_PLACEHOLDER.to_string()],
                interactive_args: vec!["-l".to_string()],
            },
        ),
        (
            "python".to_string(),
            ShellProfile {
                candidates: vec![
                    ".venv/bin/python".to_string(),
                    ".venv/Scripts/python.exe".to_string(),
                    "venv/bin/python".to_string(),
                    "venv/Scripts/python.exe".to_string(),
                    "python3".to_string(),
                    "python".to_string(),
                ],
                command_args: vec!["-c".to_string(), COMMAND_PLACEHOLDER.to_string()],
                interactive_args: Vec::new(),
            },
        ),
        (
            "node".to_string(),
            ShellProfile {
                candidates: vec!["node".to_string(), "nodejs".to_string()],
                command_args: vec!["-e".to_string(), COMMAND_PLACEHOLDER.to_string()],
                interactive_args: Vec::new(),
            },
        ),
        (
            "powershell".to_string(),
            ShellProfile {
                candidates: vec!["pwsh".to_string(), "powershell.exe".to_string()],
                command_args: vec![
                    "-NoLogo".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    COMMAND_PLACEHOLDER.to_string(),
                ],
                interactive_args: vec!["-NoLogo".to_string()],
            },
        ),
    ])
}

fn default_disconnect_timeout() -> std::num::NonZeroU32 {
    std::num::NonZeroU32::new(60).unwrap()
}
fn default_version() -> u32 {
    1
}

fn auto_string() -> String {
    "auto".to_string()
}

/// Fully expanded command arguments chosen by the client. Candidates are
/// resolved on the executor against its working directory and PATH.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellCommand {
    pub profile: String,
    pub candidates: Vec<String>,
    pub args: Vec<String>,
}
