//! Client management and file-transfer input contracts.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Debug, Deserialize)]
pub struct ConnectExecutorOptions {
    #[serde(default)]
    pub authentication: Option<crate::auth::AuthenticationOptions>,
    #[serde(default)]
    pub tls: Option<crate::transport::TlsClientOptions>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default, rename = "clientName")]
    pub client_name: Option<String>,
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub links: Vec<String>,
    #[serde(default)]
    pub channels: crate::transport::ChannelLimits,
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SetDefaultExecutorOptions {
    pub id: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FileTransferOptions {
    #[serde(flatten)]
    pub metadata: crate::protocol::management::ChannelMetadata,
    pub mode: FileTransferMode,
    #[serde(rename = "localPath")]
    pub local_path: PathBuf,
    #[serde(rename = "targetPath")]
    pub target_path: PathBuf,
    #[serde(default)]
    pub overwrite: Option<bool>,
    #[serde(default)]
    pub offset: u64,
    #[serde(default)]
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FileTransferMode {
    Download,
    Upload,
}

/// Selected executor identity and the shell-command preparation capability.
#[derive(Clone, Debug)]
pub struct ExecutorSelection {
    pub info: crate::protocol::ExecutorInfo,
    pub client_shell_command: bool,
}
