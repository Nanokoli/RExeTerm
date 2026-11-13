//! Physical identity and content hashes, shared by reads and file mutations.
use anyhow::{Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
#[derive(Clone, Debug, Serialize)]
pub struct FileStamp {
    #[serde(rename = "fileKey")]
    pub file_key: String,
    #[serde(rename = "canonicalPath")]
    pub canonical_path: String,
    pub kind: FileKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(rename = "mtimeMs", skip_serializing_if = "Option::is_none")]
    pub mtime_ms: Option<u128>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    File,
    Directory,
    Missing,
    Other,
}
pub fn file_hash_code(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(hash_bytes(&bytes))
}
pub fn hash_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("sha256:{}", hex_lower(&digest))
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn file_stamp(path: &Path) -> Result<FileStamp> {
    match fs::metadata(path) {
        Ok(metadata) => file_stamp_for_metadata(path, &metadata),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(FileStamp {
            file_key: format!("missing:{}", stable_path(path)?.display()),
            canonical_path: stable_path(path)?.to_string_lossy().into_owned(),
            kind: FileKind::Missing,
            size: None,
            mtime_ms: None,
        }),
        Err(err) => Err(err).with_context(|| format!("failed to stat {}", path.display())),
    }
}
pub(super) fn file_stamp_for_metadata(path: &Path, metadata: &fs::Metadata) -> Result<FileStamp> {
    let kind = if metadata.is_file() {
        FileKind::File
    } else if metadata.is_dir() {
        FileKind::Directory
    } else {
        FileKind::Other
    };
    Ok(FileStamp {
        file_key: physical_file_key(path, metadata)?,
        canonical_path: stable_path(path)?.to_string_lossy().into_owned(),
        kind,
        size: metadata.is_file().then_some(metadata.len()),
        mtime_ms: metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis()),
    })
}

fn stable_path(path: &Path) -> Result<PathBuf> {
    Ok(path.canonicalize().unwrap_or_else(|_| path.to_path_buf()))
}

fn physical_file_key(path: &Path, metadata: &fs::Metadata) -> Result<String> {
    match file_id::get_file_id(path) {
        Ok(id) => Ok(format!("file-id:{id:?}")),
        Err(_) => Ok(format!(
            "path:{}:{}:{}",
            stable_path(path)?.display(),
            metadata.len(),
            metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis())
                .unwrap_or(0)
        )),
    }
}
