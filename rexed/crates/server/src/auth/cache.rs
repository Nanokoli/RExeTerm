//! Positive user cache. A global gate serializes misses and permits at most one lookup per second.
use super::{validate_username, ConfigDirectory, UserConfig};
use crate::transport::tls::{self, rustls};
use anyhow::{ensure, Result};
use argon2::PasswordHash;
use rustls::server::danger::ClientCertVerifier;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;
const CAPACITY: usize = 1024;
const LOOKUP_INTERVAL: Duration = Duration::from_secs(1);

pub(super) struct User {
    pub config: UserConfig,
    pub pins: Vec<Vec<u8>>,
    pub ca: Option<Arc<dyn ClientCertVerifier>>,
    pub identity: String,
}
struct Entry {
    user: Arc<User>,
    used: Instant,
}
pub(super) struct UserCache {
    directory: ConfigDirectory,
    entries: Mutex<HashMap<String, Entry>>,
    next_lookup: tokio::sync::Mutex<Option<Instant>>,
}
impl UserCache {
    pub fn new(directory: ConfigDirectory) -> Self {
        Self {
            directory,
            entries: Mutex::new(HashMap::new()),
            next_lookup: tokio::sync::Mutex::new(None),
        }
    }
    fn cached(&self, name: &str) -> Option<Arc<User>> {
        self.entries.lock().unwrap().get_mut(name).map(|entry| {
            entry.used = Instant::now();
            entry.user.clone()
        })
    }
    pub async fn get(&self, name: &str) -> Result<Arc<User>> {
        validate_username(name)?;
        if let Some(user) = self.cached(name) {
            return Ok(user);
        }
        let mut next = self.next_lookup.lock().await;
        // Coalesce concurrent logins for a user that another lookup just cached.
        if let Some(user) = self.cached(name) {
            return Ok(user);
        }
        if let Some(at) = *next {
            tokio::time::sleep_until(at).await;
        }
        // Reserve before blocking IO, including attempts that fail or are cancelled.
        *next = Some(Instant::now() + LOOKUP_INTERVAL);
        let directory = self.directory.clone();
        let username = name.to_owned();
        let user =
            Arc::new(tokio::task::spawn_blocking(move || load_user(&directory, &username)).await??);
        let mut entries = self.entries.lock().unwrap();
        if entries.len() >= CAPACITY {
            if let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, e)| e.used)
                .map(|(k, _)| k.clone())
            {
                entries.remove(&oldest);
            }
        }
        entries.insert(
            name.to_owned(),
            Entry {
                user: user.clone(),
                used: Instant::now(),
            },
        );
        Ok(user)
    }
}
fn load_user(directory: &ConfigDirectory, name: &str) -> Result<User> {
    let config = directory.load_user(name)?;
    ensure!(!config.disabled, "user disabled");
    if let Some(hash) = &config.password_hash {
        let hash = PasswordHash::new(hash)?;
        ensure!(
            hash.algorithm.as_str() == "argon2id",
            "user password must use Argon2id"
        );
    }
    let path = directory.user_directory(name)?;
    let mut pins = Vec::new();
    for file in &config.certificates {
        let certs = tls::certificates(&path.join(file))?;
        ensure!(
            certs.len() == 1,
            "pinned certificate file must contain exactly one leaf"
        );
        pins.push(certs[0].as_ref().to_vec());
    }
    let ca = if config.certificate_authorities.is_empty() {
        None
    } else {
        let paths: Vec<_> = config
            .certificate_authorities
            .iter()
            .map(|p| path.join(p))
            .collect();
        Some(
            rustls::server::WebPkiClientVerifier::builder_with_provider(
                Arc::new(tls::roots(&paths)?),
                Arc::new(rustls::crypto::ring::default_provider()),
            )
            .build()?,
        )
    };
    let identity = config
        .ca_identity
        .clone()
        .unwrap_or_else(|| format!("urn:rexed:user:{name}"));
    // Leaf validity, exact pin/CA chain and user identity are checked at each login.
    Ok(User {
        config,
        pins,
        ca,
        identity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Permissions;
    fn fixture() -> (tempfile::TempDir, ConfigDirectory, Arc<UserCache>) {
        let root = tempfile::tempdir().unwrap();
        let directory = ConfigDirectory::open(Some(root.path().join("config"))).unwrap();
        let cache = Arc::new(UserCache::new(directory.clone()));
        (root, directory, cache)
    }
    fn save(directory: &ConfigDirectory, name: &str) {
        directory
            .save_user(
                name,
                &UserConfig {
                    permissions: Permissions::all(),
                    ..Default::default()
                },
            )
            .unwrap();
    }
    #[tokio::test(start_paused = true)]
    async fn hits_reuse_memory_and_skip_a_busy_miss_gate() {
        let (_root, directory, cache) = fixture();
        save(&directory, "alice");
        let user = cache.get("alice").await.unwrap();
        std::fs::remove_dir_all(directory.user_directory("alice").unwrap()).unwrap();
        let gate = cache.next_lookup.lock().await;
        let hit = tokio::time::timeout(Duration::from_millis(10), cache.get("alice"))
            .await
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&user, &hit));
        drop(gate);
    }
    #[tokio::test(start_paused = true)]
    async fn failed_lookups_are_globally_limited_and_a_new_user_can_be_loaded_later() {
        let (_root, directory, cache) = fixture();
        assert!(cache.get("missing").await.is_err());
        let start = Instant::now();
        save(&directory, "bob");
        // A different username shares the same limit, including after a failed read.
        assert!(
            tokio::time::timeout(Duration::from_millis(999), cache.get("bob"))
                .await
                .is_err()
        );
        let user = cache.get("bob").await.unwrap();
        assert!(user.config.permissions.files_read);
        assert!(Instant::now().duration_since(start) >= LOOKUP_INTERVAL);
        save(&directory, "missing");
        assert!(
            cache.get("missing").await.is_ok(),
            "failed reads must not be permanently cached"
        );
    }
    #[tokio::test(start_paused = true)]
    async fn concurrent_same_user_misses_share_one_load_and_invalid_users_are_not_cached() {
        let (_root, directory, cache) = fixture();
        directory
            .save_user(
                "alice",
                &UserConfig {
                    disabled: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(cache.get("alice").await.is_err());
        save(&directory, "alice");
        let a = {
            let cache = cache.clone();
            tokio::spawn(async move { cache.get("alice").await.unwrap() })
        };
        let b = {
            let cache = cache.clone();
            tokio::spawn(async move { cache.get("alice").await.unwrap() })
        };
        assert!(Arc::ptr_eq(&a.await.unwrap(), &b.await.unwrap()));
    }
}
