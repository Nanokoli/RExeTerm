//! File operations use the same framed links as tools and terminals.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

pub const FILE_PROTOCOL: &str = "re.file.v2";

/// Accept user input with or without the prefix and use the wire format internally.
pub fn normalize_sha256(value: &str) -> Result<String, &'static str> {
    let digest = value.strip_prefix("sha256:").unwrap_or(value);
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("expected 64 hexadecimal SHA-256 digits, optionally prefixed with sha256:");
    }
    Ok(format!("sha256:{}", digest.to_ascii_lowercase()))
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum FileRequest {
    Download {
        path: PathBuf,
        #[serde(default)]
        directory: Option<PathBuf>,
        #[serde(default)]
        offset: u64,
    },
    Upload {
        path: PathBuf,
        #[serde(default)]
        directory: Option<PathBuf>,
        size: u64,
        #[serde(default)]
        overwrite: bool,
        #[serde(default)]
        sha256: Option<String>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FileTransferResult {
    pub bytes: u64,
    pub sha256: String,
    #[serde(default)]
    pub file: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum FileMessage {
    #[serde(rename = "re.file.v2.open")]
    Open { request: FileRequest },
    #[serde(rename = "re.file.v2.ready")]
    Ready { bytes: u64, offset: u64 },
    #[serde(rename = "re.file.v2.end")]
    End { bytes: u64, sha256: String },
    #[serde(rename = "re.file.v2.done")]
    Done { result: FileTransferResult },
    #[serde(rename = "re.file.v2.error")]
    Error { message: String },
}
