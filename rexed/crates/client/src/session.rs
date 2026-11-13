//! Shared identity lease across data links; a heartbeat uses the same channel pool.
use crate::{
    protocol::management::*,
    transport::{ChannelPool, Frame, Link, Purpose},
};
use anyhow::{anyhow, bail, Result};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::{
    task::JoinHandle,
    time::{timeout, Duration},
};
pub(super) struct ClientSession {
    pub owner: SessionOwner,
    credential: SessionCredential,
    pool: ChannelPool,
    heartbeat: JoinHandle<()>,
    closed: AtomicBool,
    channel_metadata: bool,
}
async fn exchange(link: &mut Link, message: &SessionMessage) -> Result<SessionMessage> {
    link.send(Frame::Text(serde_json::to_string(message)?))
        .await?;
    let Some(Frame::Text(text)) = link.recv().await? else {
        bail!("session handshake closed")
    };
    match serde_json::from_str(&text)? {
        SessionMessage::Error { message } => bail!("RE session: {message}"),
        message => Ok(message),
    }
}
impl ClientSession {
    pub(super) async fn open(
        pool: ChannelPool,
        client: ClientHello,
        identity: Option<ClientCredential>,
        channel_metadata: bool,
    ) -> Result<Arc<Self>> {
        client.validate().map_err(|e| anyhow!(e))?;
        let (owner, credential) = timeout(Duration::from_secs(30), async {
            let (mut link, _lease) = pool.connect(Purpose::Control).await?;
            let message = match identity {
                Some(credential) => SessionMessage::OpenClient { client, credential },
                None => SessionMessage::Open { client },
            };
            let response = exchange(&mut link, &message).await?;
            let (owner, credential) = match response {
                SessionMessage::Opened { owner, credential }
                | SessionMessage::ClientOpened { owner, credential } => (owner, credential),
                _ => bail!("expected RE session opened"),
            };
            let _ = link.close().await;
            Ok::<_, anyhow::Error>((owner, credential))
        })
        .await
        .map_err(|_| anyhow!("session open timed out"))??;
        let task_pool = pool.clone();
        let task_credential = credential.clone();
        let heartbeat = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                let _ = timeout(Duration::from_secs(15), async {
                    let (mut link, _lease) = task_pool.connect(Purpose::Control).await?;
                    let reply = exchange(
                        &mut link,
                        &SessionMessage::Ping {
                            credential: task_credential.clone(),
                        },
                    )
                    .await?;
                    if !matches!(reply, SessionMessage::Ok) {
                        bail!("invalid heartbeat reply")
                    }
                    link.close().await
                })
                .await;
            }
        });
        Ok(Arc::new(Self {
            owner,
            credential,
            pool,
            heartbeat,
            closed: AtomicBool::new(false),
            channel_metadata,
        }))
    }
    pub(super) async fn close(&self) -> Result<()> {
        self.heartbeat.abort();
        if self.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        timeout(Duration::from_secs(5), async {
            let (mut link, _lease) = self.pool.connect(Purpose::Control).await?;
            let reply = exchange(
                &mut link,
                &SessionMessage::Close {
                    credential: self.credential.clone(),
                },
            )
            .await?;
            if !matches!(reply, SessionMessage::Ok) {
                bail!("invalid session close reply")
            }
            self.closed.store(true, Ordering::Release);
            let _ = link.close().await;
            Ok(())
        })
        .await
        .map_err(|_| anyhow!("session close timed out"))?
    }
    pub(super) async fn bind(&self, link: &mut Link) -> Result<()> {
        self.bind_named(link, None).await
    }
    pub(super) async fn bind_named(
        &self,
        link: &mut Link,
        metadata: Option<ChannelMetadata>,
    ) -> Result<()> {
        if let Some(metadata) = &metadata {
            metadata.validate().map_err(anyhow::Error::msg)?;
        }
        if metadata.is_some() && !self.channel_metadata {
            bail!("executor does not support channelMetadata; upgrade rexed");
        }
        let reply = timeout(
            Duration::from_secs(10),
            exchange(
                link,
                &SessionMessage::Bind {
                    credential: self.credential.clone(),
                    metadata,
                },
            ),
        )
        .await
        .map_err(|_| anyhow!("session binding timed out"))??;
        if !matches!(reply, SessionMessage::Ok) {
            bail!("invalid session binding reply")
        }
        Ok(())
    }
}
impl Drop for ClientSession {
    fn drop(&mut self) {
        // A lost handle / reconnect enters the server's grace period. Only explicit
        // shutdown sends Close; replacing an endpoint must preserve its active streams.
        self.heartbeat.abort();
    }
}
