//! Shared on-disk layout and private atomic files; no application/transport state.
mod private;
use anyhow::{ensure, Result};
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
pub fn user_directory(root: &Path, user: &str) -> PathBuf {
    root.join("users")
        .join(format!("{:x}", Sha256::digest(user.as_bytes())))
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
/// Import a client connection profile and referenced TLS files into one executor/user directory.
pub fn import_profile(
    directory: &Path,
    profile: &serde_json::Value,
    username: Option<&str>,
    source_directory: &Path,
) -> Result<PathBuf> {
    let mut profile = profile.clone();
    ensure!(profile.is_object(), "connection profile must be an object");
    let username = username
        .or_else(|| profile["username"].as_str())
        .unwrap_or("default")
        .to_owned();
    let users = user_directory(directory, &username);
    private_directory(&users)?;
    for (field, name) in [
        ("caFile", "ca.pem"),
        ("certificateFile", "client-cert.pem"),
        ("privateKeyFile", "client-key.pem"),
    ] {
        if let Some(value) = profile["tls"][field].as_str() {
            let original = PathBuf::from(value);
            let original = if original.is_absolute() {
                original
            } else {
                source_directory.join(original)
            };
            let target = users.join("keys").join(name);
            write_bytes(&target, &std::fs::read(original)?)?;
            profile["tls"][field] = serde_json::json!(std::fs::canonicalize(target)?);
        }
    }
    let path = users.join("authentication.json");
    write_json(&path, &profile)?;
    Ok(std::fs::canonicalize(path)?)
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
