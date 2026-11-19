//! Attached terminal links share the selected client's RE session.
use crate::{
    protocol::terminal::ClientText,
    transport::{ChannelLease, Frame, Link},
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
        self.open_terminal_link(
            executor,
            ClientText::Hello {
                id: id.into(),
                pty: pty.into(),
                cols,
                rows,
            },
        )
        .await
    }

    /// Consume the live subscription created before the PTY's first byte.
    /// Only valid for an exbash start that returned `ptyOutputStream: true`.
    pub async fn observe_terminal(
        &self,
        executor: Option<&str>,
        pty: &str,
    ) -> Result<TerminalConnection> {
        self.open_terminal_link(executor, ClientText::Observe { pty: pty.into() })
            .await
    }

    async fn open_terminal_link(
        &self,
        executor: Option<&str>,
        hello: ClientText,
    ) -> Result<TerminalConnection> {
        let endpoint = self.state.select(executor).await?;
        let pool = endpoint.stream_pool_for_terminal()?;
        timeout(Duration::from_secs(30), async {
            let (mut link, lease) = pool.connect_terminal().await?;
            if let Some(session) = &endpoint.session {
                session.bind(&mut link).await?;
            }
            link.send(Frame::Text(serde_json::to_string(&hello)?))
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
