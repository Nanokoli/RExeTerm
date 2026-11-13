//! User authentication and policy, independent of resource/session credentials.
mod cache;
mod store;
mod verifier;
use crate::{
    protocol::auth::{AuthCredential, AuthMessage},
    transport::{
        tls::{self, rustls},
        Frame, Link,
    },
};
use anyhow::{anyhow, bail, ensure, Result};
use argon2::{Argon2, PasswordHash, PasswordVerifier};
use rustls::pki_types::{CertificateDer, UnixTime};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
pub use store::{
    hash_password, validate_username, ConfigDirectory, Permissions, ServerConfig, UserConfig,
};
pub(crate) use store::{private_directory, private_file};
use tokio::time::timeout;

struct Ticket {
    username: String,
    expires: Instant,
}
pub struct Authentication {
    users: cache::UserCache,
    tls: Arc<rustls::ServerConfig>,
    passwords: Arc<tokio::sync::Semaphore>,
    tickets: Mutex<HashMap<String, Ticket>>,
    dummy_hash: String,
    pub(crate) connections: Arc<tokio::sync::Semaphore>,
}
#[derive(Clone)]
pub(crate) struct Principal {
    pub username: String,
    pub permissions: Permissions,
}
impl Authentication {
    pub fn load(directory: &ConfigDirectory) -> Result<Arc<Self>> {
        let server = directory.server()?;
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_client_cert_verifier(Arc::new(verifier::CertificateVerifier))
        .with_single_cert(
            tls::certificates(&server.certificate)?,
            tls::private_key(&server.private_key)?,
        )?;
        Ok(Arc::new(Self {
            users: cache::UserCache::new(directory.clone()),
            tls: Arc::new(tls),
            passwords: Arc::new(tokio::sync::Semaphore::new(2)),
            tickets: Mutex::new(HashMap::new()),
            dummy_hash: hash_password(&format!("{:032x}", rand::random::<u128>()))?,
            connections: Arc::new(tokio::sync::Semaphore::new(128)),
        }))
    }
    pub fn tls_config(&self) -> Arc<rustls::ServerConfig> {
        self.tls.clone()
    }
    pub(crate) async fn permissions(&self, username: &str) -> Result<Permissions> {
        Ok(self.users.get(username).await?.config.permissions)
    }
    /// Called before *any* info, control, session bind or data message.
    pub(crate) async fn authenticate(
        &self,
        link: &mut Link,
        events: &crate::events::EventLog,
        peer: &str,
        transport: &str,
    ) -> Result<Principal> {
        let mut attempted_username = None;
        let mut method = None;
        let result = timeout(Duration::from_secs(10), async {
            ensure!(
                link.security().encrypted,
                "authentication requires a TLS link"
            );
            let Some(Frame::Text(text)) = link.recv().await? else {
                bail!("authentication failed");
            };
            ensure!(text.len() <= 8192, "authentication failed");
            let AuthMessage::Login {
                username,
                credential,
            } = serde_json::from_str(&text).map_err(|_| anyhow!("authentication failed"))?
            else {
                bail!("authentication failed");
            };
            attempted_username = Some(username.clone());
            method = Some(match &credential {
                AuthCredential::Password { .. } => "password",
                AuthCredential::Certificate => "certificate",
                AuthCredential::Ca => "ca",
                AuthCredential::Ticket { .. } => "ticket",
            });
            validate_username(&username).map_err(|_| anyhow!("authentication failed"))?;
            let user = self.users.get(&username).await.ok();
            let mut existing_ticket = None;
            let valid = match credential {
                AuthCredential::Password { password } => {
                    ensure!(
                        !password.is_empty() && password.len() <= 1024,
                        "authentication failed"
                    );
                    let slot = self
                        .passwords
                        .clone()
                        .try_acquire_owned()
                        .map_err(|_| anyhow!("authentication busy; retry later"))?;
                    let hash = user
                        .as_ref()
                        .and_then(|u| u.config.password_hash.clone())
                        .unwrap_or_else(|| self.dummy_hash.clone());
                    let verified = tokio::task::spawn_blocking(move || {
                        let _slot = slot;
                        PasswordHash::new(&hash).is_ok_and(|h| {
                            Argon2::default()
                                .verify_password(password.as_bytes(), &h)
                                .is_ok()
                        })
                    })
                    .await?;
                    verified
                        && user
                            .as_ref()
                            .is_some_and(|u| u.config.password_hash.is_some())
                }
                AuthCredential::Certificate => user.as_ref().is_some_and(|u| {
                    link.security()
                        .peer_certificates
                        .first()
                        .is_some_and(|leaf| {
                            u.pins.contains(leaf) && verifier::valid_leaf(leaf, UnixTime::now())
                        })
                }),
                AuthCredential::Ca => user.as_ref().is_some_and(|u| {
                    let certs: Vec<_> = link
                        .security()
                        .peer_certificates
                        .iter()
                        .map(|c| CertificateDer::from(c.as_slice()))
                        .collect();
                    certs.first().is_some_and(|leaf| {
                        verifier::has_identity(leaf.as_ref(), &u.identity)
                            && u.ca.as_ref().is_some_and(|ca| {
                                ca.verify_client_cert(leaf, &certs[1..], UnixTime::now())
                                    .is_ok()
                            })
                    })
                }),
                AuthCredential::Ticket { ticket } => {
                    let mut tickets = self.tickets.lock().unwrap();
                    tickets.retain(|_, t| t.expires > Instant::now());
                    let valid = tickets.get(&ticket).is_some_and(|t| t.username == username);
                    if valid {
                        existing_ticket = Some(ticket);
                    }
                    valid
                }
            };
            ensure!(valid, "authentication failed");
            let permissions = user
                .ok_or_else(|| anyhow!("authentication failed"))?
                .config
                .permissions;
            let ticket = if let Some(ticket) = existing_ticket {
                ticket
            } else {
                let mut tickets = self.tickets.lock().unwrap();
                tickets.retain(|_, t| t.expires > Instant::now());
                // The bound prevents login churn from growing service memory.
                if tickets.len() >= 1024 {
                    if let Some(oldest) = tickets
                        .iter()
                        .min_by_key(|(_, t)| t.expires)
                        .map(|(k, _)| k.clone())
                    {
                        tickets.remove(&oldest);
                    }
                }
                let ticket = format!(
                    "{:032x}{:032x}",
                    rand::random::<u128>(),
                    rand::random::<u128>()
                );
                tickets.insert(
                    ticket.clone(),
                    Ticket {
                        username: username.clone(),
                        expires: Instant::now() + Duration::from_secs(600),
                    },
                );
                ticket
            };
            link.send(Frame::Text(serde_json::to_string(
                &AuthMessage::Authenticated {
                    username: username.clone(),
                    ticket,
                },
            )?))
            .await?;
            Ok(Principal {
                username,
                permissions,
            })
        })
        .await
        .unwrap_or_else(|_| Err(anyhow!("authentication timed out")));
        events.emit(if result.is_ok() { "authentication.succeeded" } else { "authentication.failed" },None,serde_json::json!({"username":attempted_username,"method":method,"peer":peer,"transport":transport,"error":result.as_ref().err().map(|e|e.to_string())}));
        if result.is_err() {
            let _ = timeout(
                Duration::from_secs(1),
                link.send(Frame::Text(serde_json::to_string(&AuthMessage::Error {
                    message: "authentication failed".into(),
                })?)),
            )
            .await;
        }
        result
    }
}
