//! Private recovery credentials live beside client settings, separate from preference output.
use anyhow::{Context, Result};
use rexe_client::{
    new_client_credential, protocol::management::ClientCredential, ClientCredentialStore,
};
use std::{collections::BTreeMap, path::PathBuf, sync::Mutex};
pub(crate) struct FileCredentialStore {
    path: PathBuf,
    lock: Mutex<()>,
}
impl FileCredentialStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }
}
impl ClientCredentialStore for FileCredentialStore {
    fn load_or_create(&self, scope: &str) -> Result<ClientCredential> {
        self.load_or_create_for("legacy", scope)
    }
    fn load_or_create_for(&self, executor: &str, scope: &str) -> Result<ClientCredential> {
        let _guard = self.lock.lock().unwrap();
        let root = self
            .path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let directory = crate::config::executor_directory(root, executor);
        crate::config::private_directory(&directory)?;
        let target = directory.join("identities.json");
        let lock_path = directory.join("identities.lock");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        crate::config::private_file(&lock_path)?;
        lock.lock()?;
        let mut entries: BTreeMap<String, ClientCredential> = match std::fs::read(&target) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).context("invalid executor identity store")?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error).context("cannot read executor identities"),
        };
        if let Some(credential) = entries.get(scope) {
            return Ok(credential.clone());
        }
        let legacy: BTreeMap<String, ClientCredential> = match std::fs::read(&self.path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).context("invalid legacy client identity store")?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error).context("cannot read legacy identities"),
        };
        let credential = legacy
            .get(scope)
            .cloned()
            .unwrap_or_else(new_client_credential);
        entries.insert(scope.into(), credential.clone());
        crate::config::write_json(&target, &entries)?;
        Ok(credential)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_credentials_keep_their_identity_inside_executor_storage() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("preferences.identities.json");
        let credential = new_client_credential();
        let scope = "test-scope";
        crate::config::write_json(&path, &BTreeMap::from([(scope, credential.clone())])).unwrap();
        let store = FileCredentialStore::new(path);
        let loaded = store.load_or_create_for("node", scope).unwrap();
        assert_eq!(loaded.client_id, credential.client_id);
        assert_eq!(loaded.secret, credential.secret);
        let target = crate::config::executor_directory(root.path(), "node").join("identities.json");
        assert!(target.is_file());
        assert_eq!(
            store.load_or_create_for("node", scope).unwrap().client_id,
            loaded.client_id
        );
    }
}
