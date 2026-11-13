//! A pure operations client: it never imports server or terminal runtimes.
pub use crate::protocol::management::default_management_target;
use crate::{
    protocol::management::{ManagementMessage, ManagementSnapshot},
    transport::{Connector, DefaultConnector, Frame, Link},
};
use anyhow::{anyhow, bail, Result};
use std::sync::Arc;
use tokio::time::{timeout, Duration};
pub struct ManagementClient {
    link: Link,
    next_id: u64,
    failed: bool,
}
impl ManagementClient {
    pub async fn connect(target: &str) -> Result<Self> {
        Self::with_connector(Arc::new(DefaultConnector), target).await
    }
    pub async fn with_connector(connector: Arc<dyn Connector>, target: &str) -> Result<Self> {
        let link = timeout(Duration::from_secs(10), connector.connect(target))
            .await
            .map_err(|_| anyhow!("management connection timed out"))??;
        Ok(Self {
            link,
            next_id: 0,
            failed: false,
        })
    }
    pub async fn snapshot(&mut self) -> Result<ManagementSnapshot> {
        if self.failed {
            bail!("management connection must be reopened after an error")
        }
        self.next_id += 1;
        let id = self.next_id;
        // A cancelled request also poisons the link; never match a late reply to a new request.
        self.failed = true;
        let result = timeout(Duration::from_secs(10), async {
            self.link
                .send(Frame::Text(serde_json::to_string(
                    &ManagementMessage::Snapshot { id },
                )?))
                .await?;
            let Some(Frame::Text(text)) = self.link.recv().await? else {
                bail!("management connection closed")
            };
            match serde_json::from_str(&text)? {
                ManagementMessage::Result {
                    id: response_id,
                    snapshot,
                } if response_id == id => Ok(snapshot),
                ManagementMessage::Error { message } => bail!("management: {message}"),
                _ => bail!("unexpected management response"),
            }
        })
        .await
        .map_err(|_| anyhow!("management request timed out"))?;
        if result.is_ok() {
            self.failed = false;
        }
        result
    }
}
