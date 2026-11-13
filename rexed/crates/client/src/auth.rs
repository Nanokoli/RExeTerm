//! Per-endpoint login wrapper. Every purpose and every additional link uses it.
use crate::{
    protocol::auth::{AuthCredential, AuthMessage},
    transport::{Connector, Frame, Link, LinkFuture, Purpose},
};
use anyhow::{anyhow, bail, ensure, Result};
use serde::Deserialize;
use std::sync::Arc;
use tokio::{
    sync::Mutex,
    time::{timeout, Duration},
};
#[derive(Clone, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthenticationOptions {
    Password { password: String },
    Certificate,
    Ca,
}
impl std::fmt::Debug for AuthenticationOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Password { .. } => "Password([REDACTED])",
            Self::Certificate => "Certificate",
            Self::Ca => "Ca",
        })
    }
}
pub struct AuthenticatedConnector {
    inner: Arc<dyn Connector>,
    username: String,
    authentication: AuthenticationOptions,
    // Serialize initial password login to avoid spending Argon2 work on each lane.
    ticket: Mutex<Option<String>>,
}
impl AuthenticatedConnector {
    pub fn new(
        inner: Arc<dyn Connector>,
        username: String,
        authentication: AuthenticationOptions,
    ) -> Self {
        Self {
            inner,
            username,
            authentication,
            ticket: Mutex::new(None),
        }
    }
    async fn login(&self, mut link: Link, credential: AuthCredential) -> Result<(Link, String)> {
        ensure!(
            link.security().encrypted,
            "credentials require tls:// or wss:// with verified peer identity"
        );
        timeout(Duration::from_secs(10), async {
            link.send(Frame::Text(serde_json::to_string(&AuthMessage::Login {
                username: self.username.clone(),
                credential,
            })?))
            .await?;
            let Some(Frame::Text(text)) = link.recv().await? else {
                bail!("authentication connection closed");
            };
            ensure!(text.len() <= 8192, "authentication reply too large");
            match serde_json::from_str(&text)? {
                AuthMessage::Authenticated { username, ticket } if username == self.username => {
                    Ok((link, ticket))
                }
                _ => bail!("authentication failed"),
            }
        })
        .await
        .map_err(|_| anyhow!("authentication timed out"))?
    }
    async fn open(&self, target: &str, purpose: Purpose) -> Result<Link> {
        let cached = self.ticket.lock().await.clone();
        if let Some(ticket) = cached {
            let link = self.inner.connect_for(target, purpose).await?;
            match self
                .login(
                    link,
                    AuthCredential::Ticket {
                        ticket: ticket.clone(),
                    },
                )
                .await
            {
                Ok((link, _)) => return Ok(link),
                Err(_) => {
                    let mut cache = self.ticket.lock().await;
                    if cache.as_ref() == Some(&ticket) {
                        *cache = None;
                    }
                }
            }
        }
        // Only initial/re-login is serialized. Cached parallel lanes dial independently.
        let mut cache = self.ticket.lock().await;
        if let Some(ticket) = cache.clone() {
            let link = self.inner.connect_for(target, purpose).await?;
            if let Ok((link, _)) = self.login(link, AuthCredential::Ticket { ticket }).await {
                return Ok(link);
            }
            *cache = None;
        }
        let link = self.inner.connect_for(target, purpose).await?;
        let credential = match &self.authentication {
            AuthenticationOptions::Password { password } => AuthCredential::Password {
                password: password.clone(),
            },
            AuthenticationOptions::Certificate => AuthCredential::Certificate,
            AuthenticationOptions::Ca => AuthCredential::Ca,
        };
        let (link, ticket) = self.login(link, credential).await?;
        *cache = Some(ticket);
        Ok(link)
    }
}
impl Connector for AuthenticatedConnector {
    fn max_parallel_links(&self, target: &str) -> usize {
        self.inner.max_parallel_links(target)
    }
    fn connect<'a>(&'a self, target: &'a str) -> LinkFuture<'a, Link> {
        Box::pin(self.open(target, Purpose::Query))
    }
    fn connect_for<'a>(&'a self, target: &'a str, purpose: Purpose) -> LinkFuture<'a, Link> {
        Box::pin(self.open(target, purpose))
    }
}
