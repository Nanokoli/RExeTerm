//! Attached terminal links share the selected client's RE session.
use crate::{
    protocol::terminal::ClientText,
    transport::{ChannelLease, Frame, Link, Purpose},
    Client,
};
use anyhow::{anyhow, Result};
use tokio::time::{timeout, Duration};
pub struct TerminalConnection {
    link: Link,
    pub(crate) _lease: ChannelLease,
    pub(crate) _session: Option<std::sync::Arc<super::session::ClientSession>>,
}
impl TerminalConnection {
    /// Access the link while retaining the channel lease and client session.
    pub fn link(&mut self) -> &mut Link {
        &mut self.link
    }
}
impl Client {
    pub async fn open_terminal(
        &self,
        executor: Option<&str>,
        pty: &str,
        id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalConnection> {
        let endpoint = self.state.select(executor).await?;
        let pool = endpoint.stream_pool_for_terminal()?;
        timeout(Duration::from_secs(30), async {
            let (mut link, lease) = pool.connect(Purpose::Pty).await?;
            if let Some(session) = &endpoint.session {
                session.bind(&mut link).await?;
            }
            link.send(Frame::Text(serde_json::to_string(&ClientText::Hello {
                id: id.into(),
                pty: pty.into(),
                cols,
                rows,
            })?))
            .await?;
            Ok(TerminalConnection {
                link,
                _lease: lease,
                _session: endpoint.session,
            })
        })
        .await
        .map_err(|_| anyhow!("terminal channel open timed out"))?
    }
}
