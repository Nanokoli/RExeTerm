//! Credential persistence is supplied by the application, never by SDK preference files.
use crate::protocol::management::ClientCredential;
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Mutex};
/// Implementations must return one stable credential per executor scope.
/// Keys contain public endpoint/user labels; values contain secrets and must not be logged.
pub trait ClientCredentialStore: Send + Sync {
    fn load_or_create(&self, scope: &str) -> Result<ClientCredential>;
    /// Optional application grouping by the selected local executor ID.
    fn load_or_create_for(&self, _executor: &str, scope: &str) -> Result<ClientCredential> {
        self.load_or_create(scope)
    }
}
pub fn new_client_credential() -> ClientCredential {
    let secret = format!(
        "{:032x}{:032x}",
        rand::random::<u128>(),
        rand::random::<u128>()
    );
    let client_id = format!("client-{:x}", Sha256::digest(secret.as_bytes()));
    ClientCredential { client_id, secret }
}
#[derive(Default)]
pub(crate) struct MemoryCredentialStore(Mutex<BTreeMap<String, ClientCredential>>);
impl ClientCredentialStore for MemoryCredentialStore {
    fn load_or_create(&self, scope: &str) -> Result<ClientCredential> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .entry(scope.into())
            .or_insert_with(new_client_credential)
            .clone())
    }
}
