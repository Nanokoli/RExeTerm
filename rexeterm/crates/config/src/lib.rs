//! Shared on-disk layout and private atomic files; no application/transport state.
mod private;
use anyhow::{ensure, Context, Result};
pub use private::{private_directory, private_file};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::{Path, PathBuf},
};
pub const DIRECTORY: &str = ".rexeterm-config";
pub fn beside_executable() -> Result<PathBuf> {
    Ok(std::env::current_exe()?
        .parent()
        .ok_or_else(|| anyhow::anyhow!("executable has no parent directory"))?
        .join(DIRECTORY))
}
pub fn executor_directory(root: &Path, id: &str) -> PathBuf {
    root.join("executors")
        .join(format!("{:x}", Sha256::digest(id.as_bytes())))
}
pub fn credential_directory(root: &Path, id: &str) -> PathBuf {
    root.join("credentials")
        .join(format!("{:x}", Sha256::digest(id.as_bytes())))
}

pub fn portable_name(name: &str) -> bool {
    let first = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(first.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((first.starts_with("COM") || first.starts_with("LPT"))
            && first.len() == 4
            && first.as_bytes()[3].is_ascii_digit());
    !name.is_empty()
        && name.len() <= 100
        && !name.starts_with('.')
        && !name.ends_with('.')
        && !name.contains("..")
        && !reserved
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
}

/// CLI entries and shell preferences share the same directory for a portable ID.
pub fn named_executor_directory(root: &Path, id: &str) -> PathBuf {
    if portable_name(id) {
        root.join("executors").join(id)
    } else {
        executor_directory(root, id)
    }
}
pub fn user_directory(root: &Path, user: &str) -> PathBuf {
    root.join("users")
        .join(format!("{:x}", Sha256::digest(user.as_bytes())))
}
/// Read a selected profile, following a local symlink to find the TLS path base.
/// `connectionProfile` in this file is metadata, not another profile to load.
pub fn load_connection_profile(path: &Path) -> Result<serde_json::Value> {
    let source = std::fs::canonicalize(path)
        .with_context(|| format!("cannot find connection profile {}", path.display()))?;
    ensure!(
        source.is_file(),
        "connection profile is not a file: {}",
        path.display()
    );
    let bytes = std::fs::read(&source)
        .with_context(|| format!("cannot read connection profile {}", path.display()))?;
    let mut profile: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
        anyhow::anyhow!(
            "invalid JSON in connection profile {} at {}:{}",
            path.display(),
            error.line(),
            error.column()
        )
    })?;
    ensure!(
        profile.is_object(),
        "connection profile must be a JSON object: {}",
        path.display()
    );
    resolve_tls_paths(
        &mut profile,
        source
            .parent()
            .context("connection profile needs a parent directory")?,
    )
    .with_context(|| {
        format!(
            "invalid TLS options in connection profile {}",
            path.display()
        )
    })?;
    Ok(profile)
}

/// Resolve PEM paths against the file that supplied the TLS options before IPC.
pub fn resolve_tls_paths(profile: &mut serde_json::Value, directory: &Path) -> Result<()> {
    let Some(tls) = profile.get_mut("tls").filter(|tls| !tls.is_null()) else {
        return Ok(());
    };
    let tls = tls.as_object_mut().context("tls must be a JSON object")?;
    for field in ["caFile", "certificateFile", "privateKeyFile"] {
        if let Some(value) = tls.get_mut(field).filter(|value| !value.is_null()) {
            let path = value
                .as_str()
                .or_else(|| value.get("path").and_then(serde_json::Value::as_str))
                .filter(|s| !s.trim().is_empty())
                .with_context(|| {
                    format!("tls.{field} must be a file path or an object with a path")
                })?;
            *value = serde_json::json!(directory.join(path));
        }
    }
    Ok(())
}
pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    write_bytes(path, &bytes)
}
pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("file has no parent"))?;
    private_directory(parent)?;
    ensure!(!path.is_symlink(), "config file must not be a symlink");
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    private_file(file.path())?;
    file.write_all(bytes)?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn executor_paths_are_case_distinct_bounded_and_cannot_escape() {
        let root = Path::new("config");
        assert_ne!(
            executor_directory(root, "Node"),
            executor_directory(root, "node")
        );
        let path = executor_directory(root, "../../CON/你好");
        assert!(path.starts_with(root.join("executors")));
        assert_eq!(path.file_name().unwrap().len(), 64);
    }
}
